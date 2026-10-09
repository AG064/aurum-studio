//! Crash-released, shared/exclusive project-content coordination.
use std::fs::{File, OpenOptions};
use std::path::Path;

/// Scope locks before content/file locks. Never acquire a build lock for the
/// same project while holding this lock. Compilation runs after it is dropped.
pub struct ContentLock(File);

impl ContentLock {
    pub fn shared(root: &Path) -> Result<Self, String> {
        Self::acquire(root, true)
    }
    pub fn exclusive(root: &Path) -> Result<Self, String> {
        Self::acquire(root, false)
    }

    fn acquire(root: &Path, shared: bool) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map(crate::project::clean_path)
            .map_err(|error| error.to_string())?;
        let name = root.to_string_lossy().replace('\\', "/");
        let name = if cfg!(windows) {
            name.to_lowercase()
        } else {
            name
        };
        let directory = std::env::temp_dir().join("aurum-content-locks");
        crate::diagnostics::checked_path(&directory).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let path = directory.join(format!("{}.lock", crate::sha256_hex(name.as_bytes())));
        crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|error| error.to_string())?;
        let result = if shared {
            file.try_lock_shared()
        } else {
            file.try_lock()
        };
        match result {
            Ok(()) => Ok(Self(file)),
            Err(std::fs::TryLockError::WouldBlock) => {
                Err("project content is busy; retry after the current content operation".into())
            }
            Err(error) => Err(error.to_string()),
        }
    }
}

impl Drop for ContentLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_lease_child() {
        if let Some(root) = std::env::var_os("AURUM_CONTENT_LEASE_FIXTURE") {
            let root = std::path::PathBuf::from(root);
            let _lease = ContentLock::shared(&root).unwrap();
            std::fs::write(root.join("ready"), "ready").unwrap();
            let mut byte = [0];
            std::io::Read::read(&mut std::io::stdin(), &mut byte).unwrap();
        }
    }

    #[test]
    fn a_second_process_contends_and_a_crash_releases_the_lease() {
        let root = std::env::temp_dir().join(format!(
            "aurum-content-process-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir(&root).unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command
            .args([
                "--exact",
                "content_lock::tests::read_lease_child",
                "--nocapture",
            ])
            .env("AURUM_CONTENT_LEASE_FIXTURE", &root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        while !root.join("ready").exists() {
            if started.elapsed() > std::time::Duration::from_secs(3) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("content lease child did not become ready");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(ContentLock::exclusive(&root).is_err());
        assert!(ContentLock::shared(&root).is_ok());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(ContentLock::exclusive(&root).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn readers_share_and_writers_are_exclusive_then_release() {
        let root = std::env::temp_dir().join(format!(
            "aurum-content-test-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let first = ContentLock::shared(&root).unwrap();
        let second = ContentLock::shared(&root).unwrap();
        assert!(ContentLock::exclusive(&root).is_err());
        drop(first);
        drop(second);
        let writer = ContentLock::exclusive(&root).unwrap();
        assert!(ContentLock::shared(&root).is_err());
        assert!(ContentLock::exclusive(&root).is_err());
        drop(writer);
        assert!(ContentLock::exclusive(&root).is_ok());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
}
