//! Recoverable per-project native save generations. No implicit pruning.
use crate::{files, project::clean_path};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::Read;
#[cfg(any(windows, test))]
use std::io::Write;
use std::path::{Path, PathBuf};

pub const RUN_BYTES: u64 = 512 * 1024 * 1024;
pub const TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const RUNS: usize = 16;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u8,
    id: String,
    parent: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Current {
    version: u8,
    id: String,
}
fn valid_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn base(root: &Path) -> Result<PathBuf, String> {
    let path = files::confined(root, ".aurum/native/userdata")?;
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    Ok(path)
}
fn bounded_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    crate::diagnostics::checked_path(path).map_err(|error| error.to_string())?;
    let meta = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if !meta.is_file() || meta.len() > 4096 {
        return Err("native userdata metadata exceeds its bound".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| error.to_string())?
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 4096 {
        return Err("native userdata metadata exceeds its bound".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "invalid native userdata metadata".into())
}
fn current(directory: &Path) -> Result<Option<String>, String> {
    let path = directory.join("current.json");
    if !path.exists() {
        return Ok(None);
    }
    let value: Current = bounded_json(&path)?;
    if value.version != 1 || !valid_id(&value.id) {
        return Err("invalid native userdata pointer".into());
    }
    Ok(Some(value.id))
}
struct Lease(File);
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
fn root_lock(directory: &Path) -> Result<Lease, String> {
    crate::diagnostics::checked_path(directory).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let path = directory.join("store.lock");
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.try_lock()
        .map_err(|_| "native userdata store is busy; retry after its current operation")?;
    Ok(Lease(file))
}
fn data_path(directory: &Path, id: &str) -> Result<PathBuf, String> {
    if !valid_id(id) {
        return Err("native userdata ID must be 32 lowercase hexadecimal characters".into());
    }
    let path = directory.join("runs").join(id);
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    let marker: Marker = bounded_json(&path.join("run.json"))?;
    if marker.version != 1
        || marker.id != id
        || marker.parent.as_deref().is_some_and(|id| !valid_id(id))
    {
        return Err("native userdata run identity is invalid".into());
    }
    Ok(path.join("data"))
}
fn inventory(root: &Path) -> Result<Vec<(PathBuf, u64)>, String> {
    tree(root).map(|(files, _)| files)
}
type Tree = (Vec<(PathBuf, u64)>, Vec<PathBuf>);
fn tree(root: &Path) -> Result<Tree, String> {
    crate::diagnostics::checked_path(root).map_err(|error| error.to_string())?;
    let mut queue = vec![(root.to_path_buf(), 0usize)];
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut count = 0usize;
    let mut bytes = 0u64;
    while let Some((directory, depth)) = queue.pop() {
        if depth > 24 {
            return Err("native userdata nesting exceeds its bound".into());
        }
        for entry in std::fs::read_dir(&directory).map_err(|error| error.to_string())? {
            count += 1;
            if count > 20000 {
                return Err("native userdata exceeds its entry bound".into());
            }
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err("native userdata links are not followed".into());
            }
            if metadata.is_dir() {
                directories.push(
                    path.strip_prefix(root)
                        .map_err(|error| error.to_string())?
                        .to_path_buf(),
                );
                queue.push((path, depth + 1));
            } else if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
                if bytes > RUN_BYTES {
                    return Err("native userdata exceeds its 512 MiB observed budget".into());
                }
                files.push((
                    path.strip_prefix(root)
                        .map_err(|error| error.to_string())?
                        .to_path_buf(),
                    metadata.len(),
                ));
            } else {
                return Err("native userdata contains an unsupported file kind".into());
            }
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    directories.sort();
    Ok((files, directories))
}
#[cfg(any(windows, test))]
fn fingerprint(root: &Path) -> Result<String, String> {
    let mut hash = crate::hash::Sha256::new();
    let (files, directories) = tree(root)?;
    for directory in directories {
        let name = directory
            .to_str()
            .ok_or("native userdata filenames must be valid text")?;
        hash.update(b"d");
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
    }
    for (path, size) in files {
        let name = path
            .to_str()
            .ok_or("native userdata filenames must be valid text")?;
        hash.update(b"f");
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(&size.to_le_bytes());
        let mut file = File::open(root.join(path)).map_err(|error| error.to_string())?;
        let mut buffer = [0u8; 8192];
        let mut total = 0u64;
        loop {
            let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }
            total += count as u64;
            if total > size {
                return Err("native userdata changed while being read".into());
            }
            hash.update(&buffer[..count]);
        }
        if total != size {
            return Err("native userdata changed while being read".into());
        }
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
#[cfg(any(windows, test))]
fn copy(source: &Path, target: &Path) -> Result<String, String> {
    let before = fingerprint(source)?;
    let (files, directories) = tree(source)?;
    for directory in directories {
        std::fs::create_dir_all(target.join(directory)).map_err(|error| error.to_string())?;
    }
    for (relative, size) in files {
        let destination = target.join(&relative);
        std::fs::create_dir_all(destination.parent().ok_or("missing userdata parent")?)
            .map_err(|error| error.to_string())?;
        let mut input = File::open(source.join(&relative)).map_err(|error| error.to_string())?;
        let mut output = File::create(&destination).map_err(|error| error.to_string())?;
        let mut remaining = size;
        let mut buffer = [0u8; 8192];
        while remaining > 0 {
            let count = input
                .read(&mut buffer[..remaining.min(8192) as usize])
                .map_err(|error| error.to_string())?;
            if count == 0 {
                return Err("native userdata changed during copying".into());
            }
            output
                .write_all(&buffer[..count])
                .map_err(|error| error.to_string())?;
            remaining -= count as u64;
        }
        output.sync_all().map_err(|error| error.to_string())?;
    }
    if fingerprint(source)? != before || fingerprint(target)? != before {
        return Err("native userdata changed during copying; the original was retained".into());
    }
    Ok(before)
}

#[cfg(any(windows, test))]
pub(crate) struct UserdataRun {
    store: PathBuf,
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    parent: Option<String>,
    seed: Option<(PathBuf, String)>,
    _lease: Lease,
}
#[cfg(any(windows, test))]
impl UserdataRun {
    pub(crate) fn stage(root: &Path, parent: Option<&UserdataRun>) -> Result<Self, String> {
        let store = base(root)?;
        let _store = root_lock(&store)?;
        let selected = current(&store)?;
        if let Some(parent) = parent {
            if selected.as_deref() != Some(&parent.id) {
                return Err("current native userdata changed; replacement refused".into());
            }
        }
        let runs = store.join("runs");
        std::fs::create_dir_all(&runs).map_err(|error| error.to_string())?;
        let entries = std::fs::read_dir(&runs)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        if entries.len() >= RUNS {
            return Err("native userdata retains 16 generations; explicitly forget an old non-current generation".into());
        }
        let mut total = 0u64;
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let data = data_path(&store, &name)?;
            total += inventory(&data)?
                .iter()
                .map(|(_, bytes)| bytes)
                .sum::<u64>();
        }
        let seed_path = if let Some(parent) = parent {
            Some(parent.path.clone())
        } else {
            selected
                .as_ref()
                .map(|id| data_path(&store, id))
                .transpose()?
        };
        let seed_size = seed_path
            .as_ref()
            .map(|path| {
                inventory(path).map(|files| files.iter().map(|(_, bytes)| bytes).sum::<u64>())
            })
            .transpose()?
            .unwrap_or(0);
        if total.saturating_add(seed_size) > TOTAL_BYTES {
            return Err(
                "native userdata reached its 2 GiB retained budget; explicit cleanup is required"
                    .into(),
            );
        }
        let id = crate::random::session_id();
        let directory = runs.join(&id);
        std::fs::create_dir(&directory).map_err(|error| error.to_string())?;
        struct UnstartedRun(PathBuf, bool);
        impl Drop for UnstartedRun {
            fn drop(&mut self) {
                if !self.1 {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
        }
        // Nothing has launched or written save data yet. Only this freshly
        // created, unpublished directory can be cleaned if setup/copy fails.
        let mut unstarted = UnstartedRun(directory.clone(), false);
        files::write_atomic(
            &directory.join("run.json"),
            &serde_json::to_vec(&Marker {
                version: 1,
                id: id.clone(),
                parent: selected.clone(),
            })
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(directory.join("run.lock"))
            .map_err(|error| error.to_string())?;
        lease.try_lock().map_err(|error| error.to_string())?;
        let lease = Lease(lease);
        let path = directory.join("data");
        std::fs::create_dir(&path).map_err(|error| error.to_string())?;
        let seed = seed_path
            .map(|source| copy(&source, &path).map(|hash| (source, hash)))
            .transpose()?;
        unstarted.1 = true;
        Ok(Self {
            store,
            id,
            path,
            parent: selected,
            seed,
            _lease: lease,
        })
    }
    pub(crate) fn seed_unchanged(&self) -> Result<bool, String> {
        self.seed
            .as_ref()
            .map(|(path, hash)| fingerprint(path).map(|current| &current == hash))
            .transpose()
            .map(|value| value.unwrap_or(true))
    }
    pub(crate) fn publish(&self) -> Result<(), String> {
        self.observed_ok()?;
        let _store = root_lock(&self.store)?;
        let existing = current(&self.store)?;
        if existing.as_deref() == Some(&self.id) {
            return Ok(());
        }
        if existing != self.parent {
            return Err("native userdata current generation changed; publication refused".into());
        }
        if !self.seed_unchanged()? {
            return Err(
                "native saved data changed after staging; original generation preserved".into(),
            );
        }
        inventory(&self.path)?;
        files::write_atomic(
            &self.store.join("current.json"),
            &serde_json::to_vec(&Current {
                version: 1,
                id: self.id.clone(),
            })
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    }
    pub(crate) fn observed_ok(&self) -> Result<(), String> {
        observed_ok(&self.path)
    }
}
#[cfg(any(windows, test))]
pub(crate) fn observed_ok(path: &Path) -> Result<(), String> {
    inventory(path)?;
    let store = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or("invalid native userdata location")?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(store.join("store.lock"))
        .map_err(|error| error.to_string())?;
    match lock.try_lock_shared() {
        Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
        Err(error) => return Err(error.to_string()),
        Ok(()) => {}
    }
    let _lock = Lease(lock);
    let mut total = 0u64;
    for (index, entry) in std::fs::read_dir(store.join("runs"))
        .map_err(|error| error.to_string())?
        .enumerate()
    {
        if index >= RUNS {
            return Err("native userdata exceeds its generation bound".into());
        }
        let entry = entry.map_err(|error| error.to_string())?;
        let id = entry.file_name().to_string_lossy().into_owned();
        let data = data_path(store, &id)?;
        total = total.saturating_add(
            inventory(&data)?
                .iter()
                .map(|(_, bytes)| bytes)
                .sum::<u64>(),
        );
        if total > TOTAL_BYTES {
            return Err("native userdata exceeds its 2 GiB observed retained budget".into());
        }
    }
    Ok(())
}
pub fn list(root: &Path) -> Result<Value, String> {
    let store = base(root)?;
    let selected = current(&store)?;
    let directory = store.join("runs");
    if !directory.exists() {
        return Ok(json!({"ok":true,"current":null,"generations":[]}));
    }
    let mut generations = Vec::new();
    for (index, entry) in std::fs::read_dir(directory)
        .map_err(|error| error.to_string())?
        .enumerate()
    {
        if index >= RUNS {
            return Err("native userdata generation scan exceeds its bound".into());
        }
        let entry = entry.map_err(|error| error.to_string())?;
        let id = entry.file_name().to_string_lossy().into_owned();
        let data = data_path(&store, &id)?;
        let bytes = inventory(&data)?
            .iter()
            .map(|(_, bytes)| bytes)
            .sum::<u64>();
        generations.push(json!({"id":id,"current":selected.as_deref()==Some(&id),"bytes":bytes}));
    }
    generations.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
    Ok(
        json!({"ok":true,"current":selected,"generations":generations,"generation_limit":RUNS,"retained_bytes_limit":TOTAL_BYTES,"automatic_pruning":false}),
    )
}
pub fn forget(root: &Path, id: &str) -> Result<Value, String> {
    let store = base(root)?;
    let _store = root_lock(&store)?;
    if current(&store)?.as_deref() == Some(id) {
        return Err("current native userdata is never forgotten".into());
    }
    let data = data_path(&store, id)?;
    let directory = data.parent().ok_or("missing userdata run")?;
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.join("run.lock"))
        .map_err(|error| error.to_string())?;
    lease
        .try_lock()
        .map_err(|_| "native userdata generation is still owned by a live operation")?;
    let _lease = Lease(lease);
    if clean_path(
        directory
            .canonicalize()
            .map_err(|error| error.to_string())?,
    ) != directory
    {
        return Err("native userdata cleanup refused a redirected directory".into());
    }
    std::fs::remove_dir_all(directory).map_err(|error| error.to_string())?;
    Ok(json!({"ok":true,"forgotten":id,"recoverable":false,"source_changed":false}))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "aurum-userdata-test-{}",
                crate::random::session_id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn store_and_run_leases_release_retained_descriptors() {
        let fixture = Fixture::new();
        let store = base(&fixture.0).unwrap();
        let lease = root_lock(&store).unwrap();
        let retained_store = lease.0.try_clone().unwrap();
        drop(lease);
        drop(root_lock(&store).unwrap());
        let first = UserdataRun::stage(&fixture.0, None).unwrap();
        first.publish().unwrap();
        let first_id = first.id.clone();
        let retained_run = first._lease.0.try_clone().unwrap();
        drop(first);
        let next = UserdataRun::stage(&fixture.0, None).unwrap();
        next.publish().unwrap();
        assert!(forget(&fixture.0, &first_id).is_ok());
        drop(retained_run);
        drop(retained_store);
    }

    #[test]
    fn generations_persist_across_stop_and_seed_the_next_run() {
        let fixture = Fixture::new();
        let first = UserdataRun::stage(&fixture.0, None).unwrap();
        std::fs::create_dir_all(first.path.join("empty/profile")).unwrap();
        std::fs::write(first.path.join("save.bin"), [0, 255, 13, 10]).unwrap();
        first.publish().unwrap();
        let first_id = first.id.clone();
        let first_path = first.path.clone();
        drop(first);
        assert!(first_path.exists());
        let second = UserdataRun::stage(&fixture.0, None).unwrap();
        assert!(second.path.join("empty/profile").is_dir());
        assert_eq!(
            std::fs::read(second.path.join("save.bin")).unwrap(),
            [0, 255, 13, 10]
        );
        second.publish().unwrap();
        assert!(forget(&fixture.0, &second.id).is_err());
        assert_eq!(forget(&fixture.0, &first_id).unwrap()["recoverable"], false);
        assert_eq!(
            list(&fixture.0).unwrap()["generations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn abandoned_staging_never_overwrites_the_current_generation() {
        let fixture = Fixture::new();
        let first = UserdataRun::stage(&fixture.0, None).unwrap();
        std::fs::write(first.path.join("save"), "original").unwrap();
        first.publish().unwrap();
        let second = UserdataRun::stage(&fixture.0, Some(&first)).unwrap();
        std::fs::write(second.path.join("save"), "failed replacement").unwrap();
        assert!(forget(&fixture.0, &second.id).is_err());
        drop(second);
        assert_eq!(list(&fixture.0).unwrap()["current"], first.id);
        assert_eq!(
            std::fs::read_to_string(first.path.join("save")).unwrap(),
            "original"
        );
    }
    #[test]
    fn changed_seed_and_competing_publication_are_detected() {
        let fixture = Fixture::new();
        let first = UserdataRun::stage(&fixture.0, None).unwrap();
        first.publish().unwrap();
        let next = UserdataRun::stage(&fixture.0, Some(&first)).unwrap();
        std::fs::write(first.path.join("changed"), "new saved data").unwrap();
        assert!(!next.seed_unchanged().unwrap());
        let competitor = UserdataRun::stage(&fixture.0, Some(&first)).unwrap();
        competitor.publish().unwrap();
        assert!(next.publish().is_err());
        assert_eq!(list(&fixture.0).unwrap()["current"], competitor.id);
    }
}
