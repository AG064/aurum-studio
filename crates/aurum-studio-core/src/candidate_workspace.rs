//! Recognized private workspaces with crash-released leases and bounded admission.
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) const WORKSPACE_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const SLOTS: usize = 2;
const MAX_POOL_ENTRIES: usize = 32;
const MAX_TREE_ENTRIES: usize = 50_000;
const POOL_NAME: &str = "aurum-candidates-v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u8,
    id: String,
    reserved_bytes: u64,
}

fn open_lock(path: &Path, create: bool) -> Result<File, String> {
    crate::diagnostics::checked_path(path).map_err(|error| error.to_string())?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .open(path)
        .map_err(|error| error.to_string())
}
fn manager(root: &Path) -> Result<File, String> {
    crate::diagnostics::checked_path(root).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let lock = open_lock(&root.join("pool.lock"), true)?;
    lock.try_lock().map_err(|_| {
        "candidate storage is busy; retry after the current allocation or cleanup".to_string()
    })?;
    Ok(lock)
}
fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn recognized(root: &Path, path: &Path) -> Result<bool, String> {
    crate::diagnostics::checked_path(path).map_err(|error| error.to_string())?;
    if path.parent() != Some(root) {
        return Ok(false);
    }
    let Some(id) = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("candidate-"))
    else {
        return Ok(false);
    };
    if !valid_id(id) {
        return Ok(false);
    }
    let marker = path.join("workspace.json");
    crate::diagnostics::checked_path(&marker).map_err(|error| error.to_string())?;
    let Ok(metadata) = std::fs::metadata(&marker) else {
        return Ok(false);
    };
    if !metadata.is_file() || metadata.len() > 1024 {
        return Ok(false);
    }
    let bytes = std::fs::read(marker).map_err(|error| error.to_string())?;
    if bytes.len() > 1024 {
        return Ok(false);
    }
    let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) else {
        return Ok(false);
    };
    Ok(manifest.version == 1 && manifest.id == id && manifest.reserved_bytes == WORKSPACE_BYTES)
}
fn remove(root: &Path, path: &Path) -> Result<(), String> {
    if !recognized(root, path)?
        || crate::project::clean_path(path.canonicalize().map_err(|error| error.to_string())?)
            != path
    {
        return Err("candidate cleanup refused an unrecognized private workspace".into());
    }
    // std removes nested links themselves. Never follow them or clean legacy
    // flat temporary directories, unmarked directories, or project revisions.
    std::fs::remove_dir_all(path).map_err(|error| error.to_string())
}

pub(crate) struct Workspace {
    pub(crate) path: PathBuf,
    pool: PathBuf,
    lease: Option<File>,
    pub(crate) recovered: usize,
}
impl Workspace {
    pub(crate) fn create() -> Result<Self, String> {
        Self::in_pool(&std::env::temp_dir().join(POOL_NAME))
    }
    #[cfg(windows)]
    pub(crate) fn create_native() -> Result<Self, String> {
        Self::in_pool_with_slots(&std::env::temp_dir().join("aurum-native-previews-v1"), 4)
    }
    #[cfg(test)]
    pub(crate) fn for_test_project(project: &Path) -> Result<Self, String> {
        Self::in_pool(&std::env::temp_dir().join(format!(
            "aurum-candidate-fixture-{}",
            crate::sha256_hex(project.to_string_lossy().as_bytes())
        )))
    }
    fn in_pool(pool: &Path) -> Result<Self, String> {
        Self::in_pool_with_slots(pool, SLOTS)
    }
    fn in_pool_with_slots(pool: &Path, slots: usize) -> Result<Self, String> {
        let _manager = manager(pool)?;
        let pool =
            crate::project::clean_path(pool.canonicalize().map_err(|error| error.to_string())?);
        let mut active = 0;
        let mut recovered = 0;
        for (index, entry) in std::fs::read_dir(&pool)
            .map_err(|error| error.to_string())?
            .enumerate()
        {
            if index >= MAX_POOL_ENTRIES {
                return Err(
                    "candidate pool has too many entries; inspect private storage before retrying"
                        .into(),
                );
            }
            let entry = entry.map_err(|error| error.to_string())?;
            if entry.file_name() == "pool.lock" {
                continue;
            }
            let path = entry.path();
            if !recognized(&pool, &path)? {
                return Err(format!("candidate pool contains unrecognized state; preserved for explicit inspection: {}", pool.display()));
            }
            let lease = open_lock(&path.join("workspace.lock"), false)?;
            match lease.try_lock() {
                Ok(()) => {
                    remove(&pool, &path)?;
                    recovered += 1;
                }
                Err(std::fs::TryLockError::WouldBlock) => active += 1,
                Err(error) => return Err(error.to_string()),
            }
        }
        if active >= slots {
            return Err(format!("private storage reservations are full ({slots} slots of 1 GiB); retry after an owned operation completes"));
        }
        let id = crate::random::session_id();
        let path = pool.join(format!("candidate-{id}"));
        std::fs::create_dir(&path).map_err(|error| error.to_string())?;
        // The manager lease prevents another allocator observing partial setup.
        // A crash before the marker completes leaves unrecognized state, which
        // is preserved rather than guessed to be safe to delete.
        let lease = open_lock(&path.join("workspace.lock"), true)?;
        lease.try_lock().map_err(|error| error.to_string())?;
        crate::files::write_atomic(
            &path.join("workspace.json"),
            &serde_json::to_vec(&Manifest {
                version: 1,
                id,
                reserved_bytes: WORKSPACE_BYTES,
            })
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            path,
            pool,
            lease: Some(lease),
            recovered,
        })
    }
    pub(crate) fn clean(&mut self) -> Result<(), String> {
        if self.lease.is_none() {
            return Ok(());
        }
        let _manager = manager(&self.pool)?;
        remove(&self.pool, &self.path)?;
        self.lease.take();
        Ok(())
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        if self.lease.is_some() && self.clean().is_err() {
            eprintln!("aurum candidates: private workspace retained for next-allocation cleanup; source was not removed");
        }
    }
}

/// Observed-byte safeguard, not a hard filesystem quota. Bounded traversal
/// refuses links, special files and oversized/complex trees without reading
/// content. The process runner checks it periodically and before acceptance.
pub(crate) fn usage(root: &Path, limit: u64) -> Result<u64, String> {
    let started = Instant::now();
    crate::diagnostics::checked_path(root).map_err(|error| error.to_string())?;
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut count = 0;
    let mut bytes = 0u64;
    while let Some((directory, depth)) = pending.pop() {
        if depth > 32 {
            return Err("candidate storage nesting exceeds its inspection budget".into());
        }
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        for entry in entries {
            count += 1;
            if count > MAX_TREE_ENTRIES || started.elapsed() > Duration::from_millis(250) {
                return Err("candidate storage exceeds its bounded inspection budget".into());
            }
            let entry = entry.map_err(|error| error.to_string())?;
            let metadata = match std::fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            };
            if metadata.file_type().is_symlink() {
                return Err("candidate storage contains an unsupported link".into());
            }
            if metadata.is_dir() {
                pending.push((entry.path(), depth + 1));
            } else if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
                if bytes > limit {
                    return Err("candidate workspace exceeds its 1 GiB observed-byte budget".into());
                }
            } else {
                return Err("candidate storage contains an unsupported file kind".into());
            }
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Pool(PathBuf);
    impl Pool {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("aurum-pool-test-{}", crate::random::session_id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Pool {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn pool_lease_child() {
        let Some(pool) = std::env::var_os("AURUM_POOL_LEASE_FIXTURE") else {
            return;
        };
        let pool = PathBuf::from(pool);
        let workspace = Workspace::in_pool(&pool).unwrap();
        std::fs::write(
            pool.with_extension("ready"),
            workspace.path.to_string_lossy().as_bytes(),
        )
        .unwrap();
        std::thread::sleep(Duration::from_secs(30));
    }
    #[test]
    fn a_real_host_crash_releases_the_lease_for_safe_reclamation() {
        let pool = Pool::new();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command
            .args([
                "--exact",
                "candidate_workspace::tests::pool_lease_child",
                "--nocapture",
            ])
            .env("AURUM_POOL_LEASE_FIXTURE", &pool.0)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let ready = pool.0.with_extension("ready");
        let started = Instant::now();
        while !ready.exists() {
            if started.elapsed() > Duration::from_secs(5) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("pool lease child did not become ready");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let abandoned = PathBuf::from(std::fs::read_to_string(&ready).unwrap());
        let active = Workspace::in_pool(&pool.0).unwrap();
        assert_eq!(active.recovered, 0);
        assert!(abandoned.exists());
        assert!(Workspace::in_pool(&pool.0).is_err());
        child.kill().unwrap();
        child.wait().unwrap();
        let replacement = Workspace::in_pool(&pool.0).unwrap();
        assert_eq!(replacement.recovered, 1);
        assert!(!abandoned.exists());
        assert!(active.path.exists());
        std::fs::remove_file(ready).unwrap();
    }
    #[test]
    fn corrupted_markers_never_authorize_cleanup() {
        let pool = Pool::new();
        let mut old = Workspace::in_pool(&pool.0).unwrap();
        std::fs::write(old.path.join("workspace.json"), "{}\n").unwrap();
        drop(old.lease.take());
        assert!(Workspace::in_pool(&pool.0).is_err());
        assert!(old.path.exists());
    }
    #[test]
    fn storage_inspection_rejects_excessive_nesting() {
        let pool = Pool::new();
        let workspace = Workspace::in_pool(&pool.0).unwrap();
        let mut directory = workspace.path.clone();
        for _ in 0..34 {
            directory.push("d");
        }
        std::fs::create_dir_all(directory).unwrap();
        assert!(usage(&workspace.path, WORKSPACE_BYTES)
            .unwrap_err()
            .contains("nesting"));
    }
    #[test]
    fn active_reservations_are_never_swept_and_capacity_is_bounded() {
        let pool = Pool::new();
        let first = Workspace::in_pool(&pool.0).unwrap();
        let second = Workspace::in_pool(&pool.0).unwrap();
        assert!(Workspace::in_pool(&pool.0)
            .err()
            .unwrap()
            .contains("reservations are full"));
        assert!(first.path.exists() && second.path.exists());
        drop(first);
        let third = Workspace::in_pool(&pool.0).unwrap();
        assert!(third.path.exists() && second.path.exists());
    }
    #[test]
    fn abandoned_recognized_copies_are_removed_on_next_allocation() {
        let pool = Pool::new();
        let mut old = Workspace::in_pool(&pool.0).unwrap();
        let path = old.path.clone();
        drop(old.lease.take()); // Simulate OS releasing handles after host death.
        let current = Workspace::in_pool(&pool.0).unwrap();
        assert_eq!(current.recovered, 1);
        assert!(!path.exists());
    }
    #[test]
    fn unknown_state_is_preserved_and_admission_fails_closed() {
        let pool = Pool::new();
        let path = pool.0.join("candidate-unrecognized");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("keep"), "untouched").unwrap();
        assert!(Workspace::in_pool(&pool.0).is_err());
        assert_eq!(
            std::fs::read_to_string(path.join("keep")).unwrap(),
            "untouched"
        );
    }
    #[test]
    fn observed_bytes_include_hidden_cache_and_userdata() {
        let pool = Pool::new();
        let workspace = Workspace::in_pool(&pool.0).unwrap();
        std::fs::create_dir(workspace.path.join(".godot")).unwrap();
        std::fs::write(workspace.path.join(".godot/cache"), vec![0; 2048]).unwrap();
        assert!(usage(&workspace.path, 1024).is_err());
        assert!(usage(&workspace.path, 4096).unwrap() >= 2048);
    }
}
