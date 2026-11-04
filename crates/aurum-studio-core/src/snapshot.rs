//! Bounded source snapshots shared by preview exports and rendered playtests.
use crate::hash::Sha256;
use std::io::{Read, Write};
use std::path::Path;

fn check_operation() -> Result<(), String> {
    if let Some(context) = crate::operation_context::OperationContext::current() {
        context.check().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), String> {
    let Some(context) = crate::operation_context::OperationContext::current() else {
        return std::fs::copy(source, destination)
            .map(|_| ())
            .map_err(|error| error.to_string());
    };
    context.check().map_err(|error| error.to_string())?;
    let mut source = std::fs::File::open(source).map_err(|error| error.to_string())?;
    let permissions = source
        .metadata()
        .map_err(|error| error.to_string())?
        .permissions();
    let mut target = std::fs::File::create(destination).map_err(|error| error.to_string())?;
    let mut buffer = [0; 64 * 1024];
    loop {
        context.check().map_err(|error| error.to_string())?;
        let count = source
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        context.check().map_err(|error| error.to_string())?;
        target
            .write_all(&buffer[..count])
            .map_err(|error| error.to_string())?;
    }
    context.check().map_err(|error| error.to_string())?;
    std::fs::set_permissions(destination, permissions).map_err(|error| error.to_string())
}

fn ignored(name: &str) -> bool {
    (name.starts_with('.') && name != ".gdignore" && name != ".cargo")
        || name == "credentials.toml"
        || matches!(
            name,
            "dist" | "target" | "node_modules" | "logs" | "test-results" | "playwright-report"
        )
}

/// Whether the path participates in the public source snapshot, not private state.
pub fn includes(relative: &str) -> bool {
    relative
        .replace('\\', "/")
        .split('/')
        .all(|part| !ignored(part))
}

/// Bounded source inventory without reading file content.
pub fn inventory(source: &Path) -> Result<Vec<(String, u64)>, String> {
    check_operation()?;
    let mut entries = Vec::new();
    visit(
        source,
        source,
        0,
        false,
        &mut 0,
        &mut 0,
        &mut |path, relative| {
            entries.push((
                relative.to_string(),
                std::fs::metadata(path)
                    .map_err(|error| error.to_string())?
                    .len(),
            ));
            Ok(())
        },
    )?;
    Ok(entries)
}

fn visit(
    root: &Path,
    directory: &Path,
    depth: usize,
    ignore_live_data: bool,
    count: &mut usize,
    bytes: &mut u64,
    callback: &mut impl FnMut(&Path, &str) -> Result<(), String>,
) -> Result<(), String> {
    check_operation()?;
    if depth > 24 {
        return Err("Project nesting exceeds the snapshot limit".into());
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
        check_operation()?;
        entries.push(entry.map_err(|error| error.to_string())?);
    }
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        check_operation()?;
        let name = entry.file_name();
        let name = name.to_str().ok_or("Snapshot filenames must be UTF-8")?;
        if ignored(name) {
            continue;
        }
        // Live data does not affect code freshness. Avoid opening even a
        // metadata handle: it can contend with Windows atomic replacement.
        // DirEntry's type also keeps a directory or symlink with this name
        // under the ordinary traversal and confinement rules.
        if ignore_live_data
            && name == "tuning.json"
            && entry.file_type().map_err(|e| e.to_string())?.is_file()
        {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err(format!("Snapshots do not follow symlinks: {name}"));
        }
        if metadata.is_dir() {
            visit(
                root,
                &entry.path(),
                depth + 1,
                ignore_live_data,
                count,
                bytes,
                callback,
            )?;
        } else if metadata.is_file() {
            *count += 1;
            *bytes = bytes.saturating_add(metadata.len());
            if *count > 20_000 || *bytes > 4 * 1024 * 1024 * 1024 {
                return Err("Project exceeds the bounded snapshot limit".into());
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_str()
                .ok_or("Snapshot filenames must be UTF-8")?
                .replace('\\', "/");
            callback(&path, &relative)?;
        } else {
            return Err(format!("Unsupported snapshot entry: {name}"));
        }
    }
    check_operation()
}

pub fn copy(source: &Path, destination: &Path) -> Result<(), String> {
    copy_excluding(source, destination, &[])
}

/// Omit tooling from newly created snapshots. Never delete or alter source entries.
pub fn copy_excluding(source: &Path, destination: &Path, excluded: &[&str]) -> Result<(), String> {
    if let Some(context) = crate::operation_context::OperationContext::current() {
        context.phase(crate::operation_context::Phase::Staging);
    }
    check_operation()?;
    std::fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    visit(
        source,
        source,
        0,
        false,
        &mut 0,
        &mut 0,
        &mut |path, relative| {
            if excluded
                .iter()
                .any(|prefix| relative == *prefix || relative.starts_with(&format!("{prefix}/")))
            {
                return Ok(());
            }
            let target = destination.join(relative);
            std::fs::create_dir_all(target.parent().ok_or("Missing snapshot parent")?)
                .map_err(|e| e.to_string())?;
            copy_file(path, &target)?;
            Ok(())
        },
    )
}

/// Hash paths and bytes, not timestamps: same-size edits and deletes invalidate a build.
/// The root tuning file is live data and intentionally does not invalidate code exports.
pub fn fingerprint(source: &Path) -> Result<String, String> {
    fingerprint_with_live_data(source, false)
}

/// Inspection caches include live files because script initializers can read
/// them. Preview code freshness intentionally uses the lighter contract above.
pub fn fingerprint_all(source: &Path) -> Result<String, String> {
    fingerprint_with_live_data(source, true)
}

/// Snapshot-all digest with closed read/traversal limits for read-only context.
/// Existing fingerprint, inventory and copy behavior is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedFingerprint {
    pub sha256: String,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Clone, Copy)]
struct FingerprintLimits {
    bytes: u64,
    files: u64,
    entries: u64,
    path_bytes: usize,
    depth: usize,
}
impl Default for FingerprintLimits {
    fn default() -> Self {
        Self {
            bytes: 512 * 1024 * 1024,
            files: 20_000,
            entries: 40_000,
            path_bytes: 1024,
            depth: 24,
        }
    }
}

pub fn fingerprint_all_bounded(
    source: &Path,
    deadline: std::time::Instant,
) -> Result<BoundedFingerprint, String> {
    bounded_fingerprint(source, deadline, FingerprintLimits::default())
}

fn fingerprint_check(deadline: std::time::Instant) -> Result<(), String> {
    check_operation()?;
    if std::time::Instant::now() >= deadline {
        return Err("bounded source fingerprint deadline expired".into());
    }
    Ok(())
}

fn fingerprint_local(path: &Path, deadline: std::time::Instant) -> Result<(), String> {
    fingerprint_check(deadline)?;
    let path = crate::project::clean_path(path.to_path_buf());
    let text = path.to_string_lossy();
    if !path.is_absolute()
        || text.len() > 4096
        || text.chars().any(char::is_control)
        || text.starts_with("\\\\")
        || text.starts_with("//")
        || text.contains("://")
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("bounded source fingerprint requires an ordinary local root".into());
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDriveTypeW(root: *const u16) -> u32;
        }
        let bytes = text.as_bytes();
        if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
            return Err("bounded source fingerprint requires an ordinary local root".into());
        }
        let root = [bytes[0] as u16, b':' as u16, b'\\' as u16, 0];
        // The checked local drive root is NUL-terminated for the system call.
        if unsafe { GetDriveTypeW(root.as_ptr()) } == 4 {
            return Err("bounded source fingerprint refuses remote drives".into());
        }
    }
    let ancestors: Vec<_> = path.ancestors().collect();
    if ancestors.len() > 128 {
        return Err("bounded source fingerprint root exceeds path depth".into());
    }
    for ancestor in ancestors.into_iter().rev() {
        fingerprint_check(deadline)?;
        let metadata = std::fs::symlink_metadata(ancestor)
            .map_err(|_| "bounded source fingerprint root is unavailable")?;
        fingerprint_ordinary(&metadata)?;
    }
    fingerprint_check(deadline)
}

fn fingerprint_ordinary(metadata: &std::fs::Metadata) -> Result<(), String> {
    if metadata.file_type().is_symlink() {
        return Err("bounded source fingerprint refuses linked entries".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("bounded source fingerprint refuses reparse entries".into());
        }
    }
    if !metadata.is_file() && !metadata.is_dir() {
        return Err("bounded source fingerprint refuses nonordinary entries".into());
    }
    Ok(())
}

fn fingerprint_key(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "pem" | "p12" | "pfx" | "jks" | "keystore" | "pkcs12"
            )
        })
}

pub(crate) fn check_opened_path(
    file: &std::fs::File,
    expected: &Path,
    deadline: std::time::Instant,
) -> Result<(), String> {
    fingerprint_check(deadline)?;
    let actual = opened_path(file)?;
    let expected = crate::project::clean_path(
        std::path::absolute(expected).map_err(|_| "checked path is unavailable")?,
    );
    let actual = crate::project::clean_path(actual);
    let actual = actual
        .to_str()
        .ok_or("checked path must be UTF-8")?
        .replace('\\', "/");
    let expected = expected
        .to_str()
        .ok_or("checked path must be UTF-8")?
        .replace('\\', "/");
    let equal = if cfg!(windows) {
        actual.eq_ignore_ascii_case(&expected)
    } else {
        actual == expected
    };
    if !equal {
        return Err("opened file resolved away from its declared path".into());
    }
    fingerprint_ordinary(
        &file
            .metadata()
            .map_err(|_| "checked handle metadata is unavailable")?,
    )?;
    fingerprint_check(deadline)
}

#[cfg(windows)]
fn opened_path(file: &std::fs::File) -> Result<std::path::PathBuf, String> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFinalPathNameByHandleW(
            handle: *mut std::ffi::c_void,
            path: *mut u16,
            length: u32,
            flags: u32,
        ) -> u32;
    }
    let mut path = vec![0u16; 4097];
    // The file owns a live handle and the buffer has the exact advertised capacity.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            path.as_mut_ptr(),
            path.len() as u32,
            0,
        )
    };
    if length == 0 || length as usize >= path.len() {
        return Err("checked handle path is unavailable or oversized".into());
    }
    Ok(std::path::PathBuf::from(std::ffi::OsString::from_wide(
        &path[..length as usize],
    )))
}
#[cfg(target_os = "linux")]
fn opened_path(file: &std::fs::File) -> Result<std::path::PathBuf, String> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|_| "checked handle path is unavailable".into())
}
#[cfg(not(any(windows, target_os = "linux")))]
fn opened_path(_file: &std::fs::File) -> Result<std::path::PathBuf, String> {
    Err("checked handle paths are unavailable on this platform".into())
}

pub(crate) fn open_checked_file(
    path: &Path,
    deadline: std::time::Instant,
) -> Result<std::fs::File, String> {
    checked_open(path, deadline, CheckedOpenKind::File)
}
pub(crate) fn open_checked_directory(
    path: &Path,
    deadline: std::time::Instant,
) -> Result<std::fs::File, String> {
    checked_open(path, deadline, CheckedOpenKind::Directory)
}

#[cfg(windows)]
pub(crate) fn open_checked_writable_directory_identity(
    path: &Path,
    deadline: std::time::Instant,
) -> Result<std::fs::File, String> {
    checked_open(path, deadline, CheckedOpenKind::WritableDirectoryIdentity)
}

#[derive(Clone, Copy)]
enum CheckedOpenKind {
    File,
    Directory,
    #[cfg(windows)]
    WritableDirectoryIdentity,
}
fn checked_open(
    path: &Path,
    deadline: std::time::Instant,
    kind: CheckedOpenKind,
) -> Result<std::fs::File, String> {
    let directory = !matches!(kind, CheckedOpenKind::File);
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (path, deadline, directory);
        return Err("checked reads are unavailable on this platform".into());
    }
    #[cfg(any(windows, target_os = "linux"))]
    {
        let absolute = std::path::absolute(path).map_err(|_| "checked path is unavailable")?;
        fingerprint_local(&absolute, deadline)?;
        let before =
            std::fs::symlink_metadata(&absolute).map_err(|_| "checked path is unavailable")?;
        fingerprint_ordinary(&before)?;
        if before.is_dir() != directory || (!directory && !before.is_file()) {
            return Err("checked path has the wrong type".into());
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000 | if directory { 0x0200_0000 } else { 0 });
            options.share_mode(if matches!(kind, CheckedOpenKind::Directory) {
                1
            } else {
                3
            });
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(0x0002_0000 | if directory { 0x0001_0000 } else { 0 });
        }
        fingerprint_check(deadline)?;
        let file = options
            .open(&absolute)
            .map_err(|_| "checked path could not be opened")?;
        check_opened_path(&file, &absolute, deadline)?;
        let after = file
            .metadata()
            .map_err(|_| "checked handle metadata is unavailable")?;
        if after.is_dir() != directory || (!directory && !after.is_file()) {
            return Err("checked handle has the wrong type".into());
        }
        Ok(file)
    }
}

fn fingerprint_reader(
    reader: &mut impl Read,
    expected: u64,
    hash: &mut Sha256,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check()?;
        let remaining = expected.saturating_sub(bytes).saturating_add(1);
        let capacity = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let count = reader
            .read(&mut buffer[..capacity])
            .map_err(|_| "bounded source read failed")?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .ok_or("bounded source read exceeds byte limit")?;
        if bytes > expected {
            return Err("source grew during bounded fingerprint".into());
        }
        hash.update(&buffer[..count]);
    }
    if bytes != expected {
        return Err("source shrank during bounded fingerprint".into());
    }
    check()
}

fn bounded_fingerprint(
    source: &Path,
    deadline: std::time::Instant,
    limits: FingerprintLimits,
) -> Result<BoundedFingerprint, String> {
    let deadline = crate::operation_context::OperationContext::current()
        .map_or(deadline, |context| deadline.min(context.deadline()));
    fingerprint_check(deadline)?;
    let source = std::path::absolute(source).map_err(|_| "bounded source root is unavailable")?;
    fingerprint_local(&source, deadline)?;
    if !std::fs::symlink_metadata(&source)
        .map_err(|_| "bounded source root is unavailable")?
        .is_dir()
    {
        return Err("bounded source fingerprint requires a directory".into());
    }
    let mut state = FingerprintState {
        hash: Sha256::new(),
        files: 0,
        bytes: 0,
        entries: 0,
        deadline,
        limits,
    };
    state.visit(&source, &source, 0)?;
    fingerprint_check(deadline)?;
    Ok(BoundedFingerprint {
        sha256: state
            .hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        files: state.files,
        bytes: state.bytes,
    })
}

struct FingerprintState {
    hash: Sha256,
    files: u64,
    bytes: u64,
    entries: u64,
    deadline: std::time::Instant,
    limits: FingerprintLimits,
}
impl FingerprintState {
    fn visit(&mut self, root: &Path, directory: &Path, depth: usize) -> Result<(), String> {
        fingerprint_check(self.deadline)?;
        if depth > self.limits.depth {
            return Err("bounded source nesting exceeds limit".into());
        }
        let directory_guard = open_checked_directory(directory, self.deadline)?;
        let mut entries = Vec::new();
        for entry in
            std::fs::read_dir(directory).map_err(|_| "bounded source directory is unavailable")?
        {
            fingerprint_check(self.deadline)?;
            self.entries = self
                .entries
                .checked_add(1)
                .ok_or("bounded source entry limit exceeded")?;
            if self.entries > self.limits.entries {
                return Err("bounded source entry limit exceeded".into());
            }
            entries.push(entry.map_err(|_| "bounded source entry is unavailable")?);
        }
        entries.sort_by_key(|entry| entry.file_name());
        check_opened_path(&directory_guard, directory, self.deadline)?;
        for entry in entries {
            fingerprint_check(self.deadline)?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or("bounded source filenames must be UTF-8")?;
            if ignored(name) {
                continue;
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "bounded source path escaped root")?
                .to_str()
                .ok_or("bounded source filenames must be UTF-8")?
                .replace('\\', "/");
            if relative.len() > self.limits.path_bytes || relative.chars().any(char::is_control) {
                return Err("bounded source relative path exceeds limit".into());
            }
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|_| "bounded source entry is unavailable")?;
            fingerprint_ordinary(&metadata)?;
            if metadata.is_dir() {
                self.visit(root, &path, depth + 1)?;
                continue;
            }
            if fingerprint_key(&path) {
                return Err("bounded source fingerprint refuses signing or key files".into());
            }
            self.files = self
                .files
                .checked_add(1)
                .ok_or("bounded source file limit exceeded")?;
            self.bytes = self
                .bytes
                .checked_add(metadata.len())
                .ok_or("bounded source byte limit exceeded")?;
            if self.files > self.limits.files || self.bytes > self.limits.bytes {
                return Err("bounded source file or byte limit exceeded".into());
            }
            let mut file = open_checked_file(&path, self.deadline)?;
            let opened = file
                .metadata()
                .map_err(|_| "bounded source file is unavailable")?;
            fingerprint_ordinary(&opened)?;
            if !opened.is_file() || opened.len() != metadata.len() {
                return Err("source changed before bounded fingerprint read".into());
            }
            self.hash.update(&(relative.len() as u64).to_le_bytes());
            self.hash.update(relative.as_bytes());
            self.hash.update(&metadata.len().to_le_bytes());
            let deadline = self.deadline;
            fingerprint_reader(&mut file, metadata.len(), &mut self.hash, || {
                fingerprint_check(deadline)
            })?;
            check_opened_path(&file, &path, self.deadline)?;
            let after = std::fs::symlink_metadata(&path)
                .map_err(|_| "bounded source file changed during read")?;
            fingerprint_ordinary(&after)?;
            if !after.is_file()
                || after.len() != metadata.len()
                || file
                    .metadata()
                    .map_err(|_| "bounded source file changed during read")?
                    .len()
                    != metadata.len()
            {
                return Err("source changed during bounded fingerprint read".into());
            }
        }
        check_opened_path(&directory_guard, directory, self.deadline)?;
        fingerprint_check(self.deadline)
    }
}

fn fingerprint_with_live_data(source: &Path, include_live: bool) -> Result<String, String> {
    check_operation()?;
    let mut hash = Sha256::new();
    visit(
        source,
        source,
        0,
        !include_live,
        &mut 0,
        &mut 0,
        &mut |path, relative| {
            if !include_live && (relative == "tuning.json" || relative.ends_with("/tuning.json")) {
                return Ok(());
            }
            hash.update(&(relative.len() as u64).to_le_bytes());
            hash.update(relative.as_bytes());
            let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
            hash.update(
                &file
                    .metadata()
                    .map_err(|e| e.to_string())?
                    .len()
                    .to_le_bytes(),
            );
            let mut buffer = [0; 64 * 1024];
            loop {
                check_operation()?;
                let size = file.read(&mut buffer).map_err(|e| e.to_string())?;
                if size == 0 {
                    break;
                }
                hash.update(&buffer[..size]);
            }
            check_operation()
        },
    )?;
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod bounded_tests {
    use super::*;
    use std::time::{Duration, Instant};
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "aurum-bounded-fingerprint-{}",
                crate::random::session_id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn file(&self, name: &str, body: &[u8]) {
            let path = self.0.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        fn deadline() -> Instant {
            Instant::now() + Duration::from_secs(10)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn checked_handles_bind_ordinary_file_and_directory_before_reads() {
        let fixture = Fixture::new();
        fixture.file("source.gd", b"bounded source");
        let directory = open_checked_directory(&fixture.0, Fixture::deadline()).unwrap();
        let mut file =
            open_checked_file(&fixture.0.join("source.gd"), Fixture::deadline()).unwrap();
        check_opened_path(&directory, &fixture.0, Fixture::deadline()).unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"bounded source");
        assert!(open_checked_directory(&fixture.0.join("source.gd"), Fixture::deadline()).is_err());
        assert!(open_checked_file(&fixture.0, Fixture::deadline()).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn checked_directory_handle_blocks_rename_and_reparse_write_open() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        let path = fixture.0.join("directory");
        std::fs::create_dir(&path).unwrap();
        let _guard = open_checked_directory(&path, Fixture::deadline()).unwrap();
        assert!(std::fs::rename(&path, fixture.0.join("renamed")).is_err());
        assert!(std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(7)
            .custom_flags(0x0200_0000 | 0x0020_0000)
            .open(&path)
            .is_err());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn renamed_open_file_fails_declared_path_check_before_more_reads() {
        let fixture = Fixture::new();
        fixture.file("source.gd", b"source");
        let path = fixture.0.join("source.gd");
        let file = open_checked_file(&path, Fixture::deadline()).unwrap();
        std::fs::rename(&path, fixture.0.join("renamed.gd")).unwrap();
        assert!(check_opened_path(&file, &path, Fixture::deadline()).is_err());
    }
    #[test]
    fn bounded_digest_preserves_recursive_order_and_all_live_source_semantics() {
        let fixture = Fixture::new();
        for (path, body) in [
            ("a/deep.gd", b"nested".as_slice()),
            ("a.gd", b"root".as_slice()),
            ("tuning.json", b"{\"value\":1}".as_slice()),
            ("assets/tuning.json", b"live nested".as_slice()),
        ] {
            fixture.file(path, body);
        }
        let bounded = fingerprint_all_bounded(&fixture.0, Fixture::deadline()).unwrap();
        assert_eq!(bounded.sha256, fingerprint_all(&fixture.0).unwrap());
        assert_eq!(bounded.files, 4);
        assert_eq!(bounded.bytes, 6 + 4 + 11 + 11);
        fixture.file("tuning.json", b"{\"value\":2}");
        let changed = fingerprint_all_bounded(&fixture.0, Fixture::deadline()).unwrap();
        assert_ne!(bounded.sha256, changed.sha256);
        assert_eq!(changed.sha256, fingerprint_all(&fixture.0).unwrap());
    }
    #[test]
    fn bounded_digest_preserves_ignored_private_and_generated_paths() {
        let fixture = Fixture::new();
        fixture.file("main.gd", b"public source");
        fixture.file(".cargo/config.toml", b"public cargo config");
        let before = fingerprint_all_bounded(&fixture.0, Fixture::deadline()).unwrap();
        for path in [
            ".env",
            "credentials.toml",
            ".cargo/credentials.toml",
            "target/key.pem",
            "logs/debug.keystore",
            ".private/secret.gd",
        ] {
            fixture.file(path, b"PRIVATE-CONTENT");
        }
        let after = fingerprint_all_bounded(&fixture.0, Fixture::deadline()).unwrap();
        assert_eq!(before, after);
        assert_eq!(after.sha256, fingerprint_all(&fixture.0).unwrap());
    }
    #[test]
    fn ignored_entries_count_toward_closed_enumeration_limit() {
        let fixture = Fixture::new();
        for index in 0..5 {
            fixture.file(&format!(".hidden-{index}"), b"ignored");
        }
        let limits = FingerprintLimits {
            entries: 4,
            ..FingerprintLimits::default()
        };
        assert!(bounded_fingerprint(&fixture.0, Fixture::deadline(), limits)
            .unwrap_err()
            .contains("entry limit"));
    }
    #[test]
    fn bounded_reader_reads_at_most_expected_size_plus_one_and_refuses_shrink() {
        let mut reader = std::io::Cursor::new(vec![b'x'; 100]);
        let mut hash = Sha256::new();
        assert!(fingerprint_reader(&mut reader, 3, &mut hash, || Ok(()))
            .unwrap_err()
            .contains("grew"));
        assert_eq!(reader.position(), 4);
        let mut reader = std::io::Cursor::new(b"ab");
        assert!(
            fingerprint_reader(&mut reader, 3, &mut Sha256::new(), || Ok(()))
                .unwrap_err()
                .contains("shrank")
        );
    }
    #[test]
    fn bounded_reader_checks_between_chunks_before_consuming_more_source() {
        let mut reader = std::io::Cursor::new(vec![b'x'; 128 * 1024]);
        let mut calls = 0;
        assert!(
            fingerprint_reader(&mut reader, 128 * 1024, &mut Sha256::new(), || {
                calls += 1;
                if calls > 1 {
                    Err("cancelled fixture".into())
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert_eq!(reader.position(), 64 * 1024);
    }
    #[test]
    fn bounded_fingerprint_refuses_byte_file_path_and_nesting_limits() {
        let fixture = Fixture::new();
        fixture.file("long-filename.gd", b"abc");
        let limits = FingerprintLimits {
            bytes: 2,
            ..FingerprintLimits::default()
        };
        assert!(bounded_fingerprint(&fixture.0, Fixture::deadline(), limits).is_err());
        let limits = FingerprintLimits {
            path_bytes: 8,
            ..FingerprintLimits::default()
        };
        assert!(bounded_fingerprint(&fixture.0, Fixture::deadline(), limits).is_err());
        fixture.file("next.gd", b"x");
        let limits = FingerprintLimits {
            files: 1,
            ..FingerprintLimits::default()
        };
        assert!(bounded_fingerprint(&fixture.0, Fixture::deadline(), limits).is_err());
        fixture.file("a/b/deep.gd", b"x");
        let limits = FingerprintLimits {
            depth: 1,
            ..FingerprintLimits::default()
        };
        assert!(bounded_fingerprint(&fixture.0, Fixture::deadline(), limits).is_err());
    }
    #[test]
    fn expired_or_cancelled_fingerprint_refuses_before_discovery_without_committing() {
        let fixture = Fixture::new();
        assert!(fingerprint_all_bounded(&fixture.0, Instant::now()).is_err());
        let context = crate::operation_context::OperationContext::new(Fixture::deadline());
        context
            .scope(|| {
                context.request_cancel().unwrap();
                assert!(fingerprint_all_bounded(&fixture.0, Fixture::deadline()).is_err());
            })
            .unwrap();
        assert!(!context.committed());
        assert!(context.commit_status_verified());
    }
    #[test]
    fn signing_files_and_non_directory_or_remote_roots_are_refused() {
        let fixture = Fixture::new();
        fixture.file("debug.keystore", b"SECRET-KEY-SENTINEL");
        let error = fingerprint_all_bounded(&fixture.0, Fixture::deadline()).unwrap_err();
        assert!(error.contains("signing or key"));
        assert!(!error.contains("SENTINEL"));
        assert!(
            fingerprint_all_bounded(&fixture.0.join("debug.keystore"), Fixture::deadline())
                .is_err()
        );
        assert!(fingerprint_all_bounded(
            Path::new("//untrusted.invalid/project"),
            Fixture::deadline()
        )
        .is_err());
    }
    #[cfg(unix)]
    #[test]
    fn bounded_fingerprint_refuses_links_and_nonordinary_source_entries() {
        let fixture = Fixture::new();
        std::os::unix::fs::symlink("missing-external", fixture.0.join("linked.gd")).unwrap();
        assert!(fingerprint_all_bounded(&fixture.0, Fixture::deadline()).is_err());
        std::fs::remove_file(fixture.0.join("linked.gd")).unwrap();
        let _socket = std::os::unix::net::UnixListener::bind(fixture.0.join("local-ipc")).unwrap();
        assert!(fingerprint_all_bounded(&fixture.0, Fixture::deadline()).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_snapshot_work_stops_without_modifying_source_or_committing() {
        let root = std::env::temp_dir().join(format!(
            "aurum-snapshot-cancel-{}",
            crate::random::session_id()
        ));
        let source = root.join("source");
        let destination = root.join("copy");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("first.gd"), b"first source").unwrap();
        std::fs::write(source.join("second.gd"), b"second source").unwrap();
        let before = fingerprint_all(&source).unwrap();
        let context = crate::operation_context::OperationContext::new(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        );
        std::fs::create_dir(&destination).unwrap();
        let result = context
            .scope(|| {
                visit(
                    &source,
                    &source,
                    0,
                    false,
                    &mut 0,
                    &mut 0,
                    &mut |path, relative| {
                        copy_file(path, &destination.join(relative))?;
                        context.request_cancel().unwrap();
                        Ok(())
                    },
                )
            })
            .unwrap();
        assert!(result.is_err());
        assert!(destination.join("first.gd").exists());
        assert!(!destination.join("second.gd").exists());
        assert!(!context.committed());
        assert_eq!(fingerprint_all(&source).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelled_copy_and_fingerprint_refuse_even_empty_trees() {
        let root = std::env::temp_dir().join(format!(
            "aurum-snapshot-empty-cancel-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir(&root).unwrap();
        let destination = root.join("copy");
        let context = crate::operation_context::OperationContext::new(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        );
        context
            .scope(|| {
                context.request_cancel().unwrap();
                assert!(copy(&root, &destination).is_err());
                assert!(fingerprint_all(&root).is_err());
                assert!(inventory(&root).is_err());
            })
            .unwrap();
        assert!(!destination.exists());
        assert!(!context.committed());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn atomic_write_staging_does_not_enter_fingerprints_or_copies() {
        let root = std::env::temp_dir().join(format!(
            "aurum-snapshot-staging-{}",
            crate::random::session_id()
        ));
        let source = root.join("source");
        let destination = root.join("copy");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("main.gd"), "extends Node").unwrap();
        std::fs::write(source.join("tuning.json"), "{}").unwrap();
        let revision = fingerprint(&source).unwrap();
        let staging = crate::files::sibling_temp(&source.join("tuning.json"));
        std::fs::write(&staging, "partially written live data").unwrap();
        assert_eq!(fingerprint(&source).unwrap(), revision);
        copy(&source, &destination).unwrap();
        assert!(!destination.join(staging.file_name().unwrap()).exists());
        assert_eq!(
            std::fs::read(destination.join("tuning.json")).unwrap(),
            b"{}"
        );
        let directory = source.join("assets/tuning.json");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("visible.gd"), "extends Node").unwrap();
        assert_ne!(fingerprint(&source).unwrap(), revision);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn live_atomic_writes_do_not_break_concurrent_source_scans() {
        let root = std::env::temp_dir().join(format!(
            "aurum-snapshot-live-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("main.gd"), "extends Node").unwrap();
        crate::files::write_atomic(&root.join("tuning.json"), b"{}").unwrap();
        let revision = fingerprint(&root).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let writer_root = root.clone();
        let writer_barrier = barrier.clone();
        let writer = std::thread::spawn(move || {
            writer_barrier.wait();
            for value in 0..100 {
                crate::files::write_atomic(
                    &writer_root.join("tuning.json"),
                    format!("{{\"player_speed\":{value}}}").as_bytes(),
                )
                .unwrap();
            }
        });
        barrier.wait();
        for _ in 0..100 {
            assert_eq!(fingerprint(&root).unwrap(), revision);
        }
        writer.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn revisions_detect_same_size_edits_deletes_and_ignore_live_or_generated_data() {
        let root =
            std::env::temp_dir().join(format!("aurum-snapshot-{}", crate::random::session_id()));
        std::fs::create_dir_all(root.join(".godot")).unwrap();
        std::fs::write(root.join("main.gd"), "one").unwrap();
        let first = fingerprint(&root).unwrap();
        std::fs::write(root.join("tuning.json"), "{}").unwrap();
        let complete = fingerprint_all(&root).unwrap();
        std::fs::write(root.join("tuning.json"), "{\"speed\":9}").unwrap();
        assert_ne!(fingerprint_all(&root).unwrap(), complete);
        std::fs::write(root.join(".godot/cache"), "cached").unwrap();
        assert_eq!(fingerprint(&root).unwrap(), first);
        std::fs::write(root.join("main.gd"), "two").unwrap();
        assert_ne!(fingerprint(&root).unwrap(), first);
        std::fs::remove_file(root.join("main.gd")).unwrap();
        assert_ne!(fingerprint(&root).unwrap(), first);
        std::fs::remove_dir_all(root).unwrap();
    }
}
