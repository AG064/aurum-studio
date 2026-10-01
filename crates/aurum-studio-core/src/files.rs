//! Confined paths and same-directory atomic writes used by Studio operations.
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn sibling_temp(path: &Path) -> PathBuf {
    // Snapshot readers must not enumerate staging files that disappear at
    // commit. Preserve the filename without lossy Unicode conversion, but
    // prefix it so the existing private-file rules exclude it before stat.
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(name)
}

pub fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(source: *const u16, destination: *const u16, flags: u32) -> i32;
        }
        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // Same-volume replace; the destination is never removed first.
        if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 1 | 8) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(source, destination)
    }
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = sibling_temp(path);
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Resolve a relative path without allowing traversal through a symlink outside root.
pub fn confined(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map(crate::project::clean_path)
        .map_err(|e| e.to_string())?;
    let relative = relative.strip_prefix("res://").unwrap_or(relative);
    let path = Path::new(relative);
    if relative.contains(':')
        || path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("path must be relative and stay inside the project".into());
    }
    let candidate = root.join(path);
    let mut ancestor = candidate.as_path();
    while std::fs::symlink_metadata(ancestor).is_err() {
        ancestor = ancestor.parent().ok_or("path has no existing ancestor")?;
    }
    let resolved = ancestor
        .canonicalize()
        .map(crate::project::clean_path)
        .map_err(|e| e.to_string())?;
    if !resolved.starts_with(&root) {
        return Err("path resolves outside the project".into());
    }
    let remainder = candidate
        .strip_prefix(ancestor)
        .map_err(|e| e.to_string())?;
    // Joining an empty path appends a separator. For an existing file that
    // changes the OS request from opening a file to opening a directory.
    if remainder.as_os_str().is_empty() {
        Ok(resolved)
    } else {
        Ok(resolved.join(remainder))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolving_an_existing_file_preserves_a_readable_file_path() {
        let root = std::env::temp_dir().join(format!(
            "aurum-existing-path-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = crate::project::clean_path(root.canonicalize().unwrap());
        let file = root.join("existing.gd");
        write_atomic(&file, b"saved content").unwrap();
        let resolved = confined(&root, "existing.gd").unwrap();
        assert_eq!(resolved.as_os_str(), file.as_os_str());
        assert_eq!(std::fs::read(resolved).unwrap(), b"saved content");
        assert_eq!(confined(&root, "").unwrap().as_os_str(), root.as_os_str());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn replaces_without_leaving_a_staging_file() {
        let root = std::env::temp_dir().join(format!("aurum-atomic-{}", std::process::id()));
        let target = root.join("state.json");
        write_atomic(&target, b"before").unwrap();
        write_atomic(&target, b"after").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"after");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        assert!(confined(&root, "../escape").is_err());
        assert!(confined(&root, "asset.gd:stream").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
