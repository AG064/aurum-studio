//! Test-only all-entry content/identity oracle. No production source filters.
use super::{ordinary, stamp, Budget, File, Path, PathBuf, Read, Sha256, Stamp};
use std::time::UNIX_EPOCH;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GuardPoint {
    BeforeRead,
    BeforeFinalCheck,
}
type GuardProbe = Box<dyn FnMut(GuardPoint)>;
std::thread_local! {
    static GUARD_PROBE: std::cell::RefCell<Option<GuardProbe>> = const { std::cell::RefCell::new(None) };
}
// Inline test barrier: the walk cannot continue until this probe returns.
// It observes the actual guard on this stack, without sleeps or a new thread.
fn guard_point(point: GuardPoint) {
    GUARD_PROBE.with(|slot| {
        if let Some(probe) = slot.borrow_mut().as_mut() {
            probe(point);
        }
    });
}

pub(crate) type Roots<'a> = [(&'static str, &'a Path)];
pub(crate) type Outcome = Result<Vec<RootDigest>, Refusal>;
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RootDigest {
    role: &'static str,
    sha256: String,
    files: usize,
    directories: usize,
    bytes: u64,
    entries: usize,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) role: &'static str,
    pub(crate) category: &'static str,
}
fn refused(role: &'static str, category: &'static str) -> Refusal {
    Refusal { role, category }
}
fn check(budget: &Budget<'_>, role: &'static str) -> Result<(), Refusal> {
    budget
        .check()
        .map_err(|_| refused(role, "budget_or_cancelled"))?;
    if let Some(context) = crate::operation_context::OperationContext::current() {
        context
            .check()
            .map_err(|_| refused(role, "budget_or_cancelled"))?;
    }
    Ok(())
}
struct Held {
    path: PathBuf,
    file: File,
    before: Stamp,
    directory: bool,
}
fn opened(
    path: &Path,
    directory: bool,
    budget: &Budget<'_>,
    role: &'static str,
) -> Result<Held, Refusal> {
    check(budget, role)?;
    let meta = std::fs::symlink_metadata(path).map_err(|_| refused(role, "metadata"))?;
    ordinary(&meta).map_err(|_| refused(role, "linked_or_special"))?;
    let file = if directory {
        crate::snapshot::open_checked_directory(path, budget.deadline())
    } else {
        super::open_compile_source(path, budget)
    }
    .map_err(|_| refused(role, "checked_open"))?;
    let before = stamp(&file).map_err(|_| refused(role, "stamp"))?;
    if meta.is_dir() != directory
        || meta.len() != before.bytes
        || meta.modified().ok() != before.modified
        || meta.created().ok() != before.identity.created
        || before.modified.is_none()
        || before.identity.created.is_none()
    {
        return Err(refused(role, "changed_or_unstamped"));
    }
    Ok(Held {
        path: path.to_path_buf(),
        file,
        before,
        directory,
    })
}
fn recheck(held: &Held, budget: &Budget<'_>, role: &'static str) -> Result<(), Refusal> {
    check(budget, role)?;
    crate::snapshot::check_opened_path(&held.file, &held.path, budget.deadline())
        .map_err(|_| refused(role, "identity_changed"))?;
    let current = opened(&held.path, held.directory, budget, role)?;
    if stamp(&held.file).map_err(|_| refused(role, "stamp"))? != held.before
        || current.before != held.before
    {
        return Err(refused(role, "identity_or_stamp_changed"));
    }
    Ok(())
}
fn add_stamp(hash: &mut Sha256, value: &Stamp, role: &'static str) -> Result<(), Refusal> {
    hash.update(&value.identity.volume.to_le_bytes());
    hash.update(&value.identity.id);
    for time in [value.identity.created, value.modified] {
        let duration = time
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .ok_or_else(|| refused(role, "timestamp"))?;
        hash.update(&duration.as_secs().to_le_bytes());
        hash.update(&duration.subsec_nanos().to_le_bytes());
    }
    Ok(())
}
#[derive(Default)]
struct Totals {
    entries: usize,
    files: usize,
    directories: usize,
    bytes: u64,
    enumerated: usize,
}
struct Row {
    relative: String,
    directory: bool,
    bytes: u64,
    content: String,
    before: Stamp,
}
#[derive(Default)]
struct Scan {
    held: Vec<Held>,
    rows: Vec<Row>,
}
fn names(
    path: &Path,
    budget: &Budget<'_>,
    role: &'static str,
    totals: &mut Totals,
) -> Result<Vec<String>, Refusal> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir(path).map_err(|_| refused(role, "enumeration"))? {
        check(budget, role)?;
        totals.enumerated = totals
            .enumerated
            .checked_add(1)
            .ok_or_else(|| refused(role, "entries"))?;
        if totals.enumerated > budget.max_entries.saturating_mul(2) {
            return Err(refused(role, "entries"));
        }
        let name = entry.map_err(|_| refused(role, "enumeration"))?.file_name();
        let name = name
            .to_str()
            .filter(|name| !name.chars().any(char::is_control))
            .ok_or_else(|| refused(role, "filename"))?;
        result.push(name.to_owned());
    }
    result.sort();
    Ok(result)
}
fn content(
    reader: &mut impl Read,
    expected: u64,
    budget: &Budget<'_>,
    role: &'static str,
) -> Result<String, Refusal> {
    let mut hash = Sha256::new();
    let mut read = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check(budget, role)?;
        let capacity = expected
            .saturating_sub(read)
            .saturating_add(1)
            .min(buffer.len() as u64) as usize;
        let count = reader
            .read(&mut buffer[..capacity])
            .map_err(|_| refused(role, "read"))?;
        if count == 0 {
            break;
        }
        read = read
            .checked_add(count as u64)
            .ok_or_else(|| refused(role, "bytes"))?;
        if read > expected {
            return Err(refused(role, "file_changed"));
        }
        hash.update(&buffer[..count]);
    }
    if read != expected {
        return Err(refused(role, "file_changed"));
    }
    Ok(hash.hex())
}
fn walk(
    path: &Path,
    relative: &str,
    depth: usize,
    role: &'static str,
    budget: &Budget<'_>,
    totals: &mut Totals,
    scan: &mut Scan,
) -> Result<(), Refusal> {
    check(budget, role)?;
    if depth > 24 {
        return Err(refused(role, "depth"));
    }
    if relative.len() > 1024 {
        return Err(refused(role, "path_bytes"));
    }
    totals.entries = totals
        .entries
        .checked_add(1)
        .ok_or_else(|| refused(role, "entries"))?;
    if totals.entries > budget.max_entries {
        return Err(refused(role, "entries"));
    }
    let meta = std::fs::symlink_metadata(path).map_err(|_| refused(role, "metadata"))?;
    ordinary(&meta).map_err(|_| refused(role, "linked_or_special"))?;
    let mut handle = opened(path, meta.is_dir(), budget, role)?;
    let value = if meta.is_dir() {
        totals.directories += 1;
        let before_names = names(path, budget, role, totals)?;
        for name in &before_names {
            let tail = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            walk(
                &path.join(name),
                &tail,
                depth + 1,
                role,
                budget,
                totals,
                scan,
            )?;
        }
        if names(path, budget, role, totals)? != before_names {
            return Err(refused(role, "entries_changed"));
        }
        String::new()
    } else {
        totals.files = totals
            .files
            .checked_add(1)
            .ok_or_else(|| refused(role, "files"))?;
        totals.bytes = totals
            .bytes
            .checked_add(handle.before.bytes)
            .ok_or_else(|| refused(role, "bytes"))?;
        if totals.files > budget.max_files {
            return Err(refused(role, "files"));
        }
        if handle.before.bytes > budget.max_file {
            return Err(refused(role, "file_bytes"));
        }
        if totals.bytes > budget.max_bytes {
            return Err(refused(role, "bytes"));
        }
        guard_point(GuardPoint::BeforeRead);
        content(&mut handle.file, handle.before.bytes, budget, role)?
    };
    recheck(&handle, budget, role)?;
    scan.rows.push(Row {
        relative: relative.into(),
        directory: meta.is_dir(),
        bytes: if meta.is_dir() {
            0
        } else {
            handle.before.bytes
        },
        content: value,
        before: handle.before.clone(),
    });
    scan.held.push(handle);
    Ok(())
}
pub(crate) fn capture(roots: &Roots<'_>, budget: &Budget<'_>) -> Outcome {
    if roots.is_empty() || roots.len() > 8 {
        return Err(refused("all", "roots"));
    }
    let mut totals = Totals::default();
    let mut result = Vec::new();
    let mut guards = Vec::new();
    for (role, root) in roots {
        check(budget, role)?;
        if !root.is_absolute()
            || !root
                .to_str()
                .and_then(|text| text.get(..3))
                .is_some_and(|prefix| {
                    prefix.eq_ignore_ascii_case(&format!("{}\\", crate::test_drive()))
                        || prefix.eq_ignore_ascii_case(&format!("{}/", crate::test_drive()))
                })
        {
            return Err(refused(role, "fixture_A_root"));
        }
        let start = (
            totals.entries,
            totals.files,
            totals.directories,
            totals.bytes,
        );
        let mut scan = Scan::default();
        walk(root, "", 0, role, budget, &mut totals, &mut scan)?;
        scan.rows.sort_by(|a, b| a.relative.cmp(&b.relative));
        let mut hash = Sha256::new();
        for row in scan.rows {
            hash.update(&(row.relative.len() as u64).to_le_bytes());
            hash.update(row.relative.as_bytes());
            hash.update(&[u8::from(row.directory)]);
            hash.update(&row.bytes.to_le_bytes());
            hash.update(row.content.as_bytes());
            add_stamp(&mut hash, &row.before, role)?;
        }
        result.push(RootDigest {
            role,
            sha256: hash.hex(),
            entries: totals.entries - start.0,
            files: totals.files - start.1,
            directories: totals.directories - start.2,
            bytes: totals.bytes - start.3,
        });
        guards.push((*role, scan.held));
    }
    for (role, held) in &guards {
        for guard in held {
            if !guard.directory {
                guard_point(GuardPoint::BeforeFinalCheck);
            }
            recheck(guard, budget, role)?;
        }
    }
    check(budget, "all")?;
    Ok(result)
}
pub(crate) fn status(value: &Outcome) -> serde_json::Value {
    match value {
        Ok(rows) => serde_json::json!({"status":"COMPLETE","coverage":"ALL_ENTRY",
            "payload_exported":false,"roots":rows.iter().map(|row|serde_json::json!({
                "role":row.role,"sha256":row.sha256,"files":row.files,"directories":row.directories,
                "entries":row.entries,"bytes":row.bytes})).collect::<Vec<_>>() }),
        Err(error) => serde_json::json!({"status":"REFUSED","coverage":null,"fingerprint":null,
            "root_role":error.role,"category":error.category,"payload_exported":false}),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;
    const ORIGINAL: &[u8] = b"controlled oracle source\n";
    fn source_file() -> (PathBuf, PathBuf) {
        let temp = crate::project::clean_path(std::env::temp_dir().canonicalize().unwrap());
        assert!(temp
            .to_string_lossy()
            .get(..3)
            .is_some_and(|p| p.eq_ignore_ascii_case(&format!("{}\\", crate::test_drive()))));
        let root = temp.join(format!(
            "all-entry-file-guard-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("original.bin");
        std::fs::write(&file, ORIGINAL).unwrap();
        (root, file)
    }
    fn unchanged_and_remove(root: &Path, file: &Path) {
        assert_eq!(std::fs::read(file).unwrap(), ORIGINAL);
        std::fs::remove_file(file).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    struct ResetProbe;
    impl Drop for ResetProbe {
        fn drop(&mut self) {
            GUARD_PROBE.with(|slot| {
                slot.borrow_mut().take();
            });
        }
    }
    #[test]
    fn all_entry_oracle_refuses_preexisting_write_handle() {
        let (root, file) = source_file();
        let b = Budget::new(std::time::Duration::from_secs(30), &|| Ok(())).unwrap();
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .share_mode(3)
            .open(&file)
            .unwrap();
        let outcome = capture(&[("fixture", root.as_path())], &b);
        assert_eq!(outcome.as_ref().unwrap_err().category, "checked_open");
        let report = status(&outcome);
        assert!(report["coverage"].is_null() && report["fingerprint"].is_null());
        drop(writer);
        assert_eq!(
            status(&capture(&[("fixture", root.as_path())], &b))["coverage"],
            "ALL_ENTRY"
        );
        unchanged_and_remove(&root, &file);
    }
    #[test]
    fn all_entry_oracle_denies_write_through_final_guard_check() {
        let (root, file) = source_file();
        let b = Budget::new(std::time::Duration::from_secs(30), &|| Ok(())).unwrap();
        let attempts = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = attempts.clone();
        let target = file.clone();
        GUARD_PROBE.with(|slot| {
            assert!(slot
                .borrow_mut()
                .replace(Box::new(move |point| {
                    let (blocked, raw) = match std::fs::OpenOptions::new()
                        .write(true)
                        .share_mode(3)
                        .open(&target)
                    {
                        Ok(writer) => {
                            drop(writer);
                            (false, None)
                        }
                        Err(error) => (true, error.raw_os_error()),
                    };
                    seen.borrow_mut().push((point, blocked, raw));
                }))
                .is_none());
        });
        let reset = ResetProbe;
        let outcome = capture(&[("fixture", root.as_path())], &b);
        drop(reset);
        assert_eq!(status(&outcome)["coverage"], "ALL_ENTRY");
        assert_eq!(
            attempts.borrow().as_slice(),
            &[
                (GuardPoint::BeforeRead, true, Some(32)),
                (GuardPoint::BeforeFinalCheck, true, Some(32)),
            ]
        );
        // The original read guards have now dropped; WRITE admission resumes.
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .share_mode(3)
            .open(&file)
            .unwrap();
        drop(writer);
        unchanged_and_remove(&root, &file);
    }
    #[test]
    fn all_entry_reader_hashes_bytes_and_bounds_growth_shrink() {
        let b = Budget::new(std::time::Duration::from_secs(30), &|| Ok(())).unwrap();
        let mut exact = std::io::Cursor::new(b"abc");
        assert_eq!(
            content(&mut exact, 3, &b, "fixture").unwrap(),
            crate::sha256_hex(b"abc")
        );
        let mut changed = std::io::Cursor::new(b"abd");
        assert_ne!(
            content(&mut changed, 3, &b, "fixture").unwrap(),
            crate::sha256_hex(b"abc")
        );
        let mut grown = std::io::Cursor::new([b'x'; 100]);
        assert_eq!(
            content(&mut grown, 3, &b, "fixture").unwrap_err().category,
            "file_changed"
        );
        assert_eq!(grown.position(), 4);
        let mut shrunk = std::io::Cursor::new(b"ab");
        assert_eq!(
            content(&mut shrunk, 3, &b, "fixture").unwrap_err().category,
            "file_changed"
        );
    }
}
