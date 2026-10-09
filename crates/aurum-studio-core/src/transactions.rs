//! Journaled text revision publication and conflict-preserving recovery.
use crate::{files, Project};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_ROWS: usize = 256;
const MAX_BACKUP_BYTES: usize = 32 * 1024 * 1024;
const MAX_JOURNAL_BYTES: u64 = 1024 * 1024;
const MAX_HISTORY_BYTES: u64 = 128 * 1024 * 1024;
type ChangeBytes = (String, Option<Vec<u8>>, Option<Vec<u8>>);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    path: String,
    before: Option<String>,
    after: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum State {
    Prepared,
    Publishing,
    Committed,
    RollingBack,
    RolledBack,
    RecoveryConflict,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u8,
    id: String,
    state: State,
    rows: Vec<Row>,
    created_dirs: Vec<String>,
    undo_of: Option<String>,
}

fn id(value: &str) -> Result<(), String> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("revision ID must be 32 hexadecimal characters".into());
    }
    Ok(())
}
fn state_root(root: &Path) -> Result<PathBuf, String> {
    let path = files::confined(root, ".aurum/revisions")?;
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    Ok(path)
}
fn active(root: &Path) -> Result<PathBuf, String> {
    Ok(state_root(root)?.join("active.json"))
}

pub(crate) fn ensure_writable(root: &Path) -> Result<(), String> {
    if active(root)?.exists() {
        return Err(
            "an unresolved revision is present; run changes_recover before writing project content"
                .into(),
        );
    }
    Ok(())
}

fn durable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    files::write_atomic(path, bytes).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    std::fs::File::open(path.parent().ok_or("missing journal directory")?)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}
fn save(directory: &Path, journal: &Journal) -> Result<(), String> {
    durable(
        &directory.join("journal.json"),
        &serde_json::to_vec(journal).map_err(|error| error.to_string())?,
    )
}
fn load(directory: &Path) -> Result<Journal, String> {
    crate::diagnostics::checked_path(directory).map_err(|error| error.to_string())?;
    let path = directory.join("journal.json");
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    if std::fs::metadata(&path)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_JOURNAL_BYTES
    {
        return Err("revision journal exceeds its size bound".into());
    }
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let journal: Journal = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    id(&journal.id)?;
    if journal.version != 1
        || journal.rows.is_empty()
        || journal.rows.len() > MAX_ROWS
        || journal.created_dirs.len() > 2048
    {
        return Err("unsupported revision journal".into());
    }
    for row in &journal.rows {
        for hash in [&row.before, &row.after].into_iter().flatten() {
            if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err("invalid journal content hash".into());
            }
        }
    }
    Ok(journal)
}
fn read_text(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(metadata) if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 => {
            return Err("revision targets must be regular text files of at most 2 MiB".into())
        }
        Ok(_) => {}
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 2 * 1024 * 1024 || std::str::from_utf8(&bytes).is_err() {
        return Err("revision text file exceeds supported bounds".into());
    }
    Ok(Some(bytes))
}
fn hash(path: &Path) -> Result<Option<String>, String> {
    Ok(read_text(path)?.map(|bytes| crate::sha256_hex(&bytes)))
}
fn blob(
    directory: &Path,
    index: usize,
    before: bool,
    expected: &Option<String>,
) -> Result<Option<Vec<u8>>, String> {
    let Some(expected) = expected else {
        return Ok(None);
    };
    let path = directory.join(format!(
        "{index:03}.{}",
        if before { "before" } else { "after" }
    ));
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    let bytes = read_text(&path)?.ok_or("revision backup is missing")?;
    if crate::sha256_hex(&bytes) != *expected {
        return Err("revision backup failed integrity verification".into());
    }
    Ok(Some(bytes))
}
fn apply_bytes(path: &Path, bytes: Option<&[u8]>) -> Result<(), String> {
    match bytes {
        Some(bytes) => durable(path, bytes),
        None => {
            if path.exists() {
                std::fs::remove_file(path).map_err(|error| error.to_string())?;
            }
            Ok(())
        }
    }
}
fn clear_active(root: &Path) -> Result<(), String> {
    let path = active(root)?;
    if path.exists() {
        std::fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    Ok(())
}
fn restore(root: &Path, directory: &Path, journal: &mut Journal) -> Result<Value, String> {
    let mut conflicts = Vec::new();
    for (index, row) in journal.rows.iter().enumerate() {
        let path = crate::project_ops::managed_path(root, &row.path)?;
        let current = hash(&path)?;
        if current != row.before && current != row.after {
            conflicts.push(json!({"path":row.path,"actual_sha256":current}));
        }
        blob(directory, index, true, &row.before)?;
    }
    if !conflicts.is_empty() {
        journal.state = State::RecoveryConflict;
        save(directory, journal)?;
        return Ok(
            json!({"ok":false,"recovery_blocked":true,"conflicts":conflicts,"revision_id":journal.id}),
        );
    }
    journal.state = State::RollingBack;
    save(directory, journal)?;
    for (index, row) in journal.rows.iter().enumerate().rev() {
        let path = crate::project_ops::managed_path(root, &row.path)?;
        let current = hash(&path)?;
        if current == row.before {
            continue;
        }
        if current != row.after {
            journal.state = State::RecoveryConflict;
            save(directory, journal)?;
            return Ok(
                json!({"ok":false,"recovery_blocked":true,"revision_id":journal.id,"conflicts":[{"path":row.path,"actual_sha256":current}]}),
            );
        }
        let bytes = blob(directory, index, true, &row.before)?;
        apply_bytes(&path, bytes.as_deref())?;
    }
    for relative in journal.created_dirs.iter().rev() {
        let path = crate::project_ops::managed_path(root, relative)?;
        if path.is_dir() {
            let _ = std::fs::remove_dir(path);
        }
    }
    journal.state = State::RolledBack;
    save(directory, journal)?;
    clear_active(root)?;
    Ok(json!({"ok":true,"rolled_back":true,"revision_id":journal.id}))
}

pub fn recover(project: &Project) -> Result<Value, String> {
    recover_root(&project.root)
}

pub(crate) fn recover_root(root: &Path) -> Result<Value, String> {
    let root = root
        .canonicalize()
        .map(crate::project::clean_path)
        .map_err(|error| error.to_string())?;
    let _lease = crate::content_lock::ContentLock::exclusive(&root)?;
    let pointer = active(&root)?;
    if !pointer.exists() {
        return Ok(json!({"ok":true,"recovered":false}));
    }
    if std::fs::metadata(&pointer)
        .map_err(|error| error.to_string())?
        .len()
        > 4096
    {
        return Err("active revision pointer exceeds its size bound".into());
    }
    let value: Value =
        serde_json::from_slice(&std::fs::read(&pointer).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let revision = value["revision_id"]
        .as_str()
        .ok_or("invalid active revision pointer")?;
    id(revision)?;
    let directory = state_root(&root)?.join(revision);
    let mut journal = load(&directory)?;
    if journal.id != revision {
        return Err("active pointer and journal identity disagree".into());
    }
    if journal.state == State::Committed || journal.state == State::RolledBack {
        clear_active(&root)?;
        return Ok(
            json!({"ok":true,"recovered":true,"revision_id":revision,"content_changed":false}),
        );
    }
    restore(&root, &directory, &mut journal)
}

fn publish_rows(
    root: &Path,
    changes: Vec<ChangeBytes>,
    undo_of: Option<String>,
    fail_after: Option<usize>,
) -> Result<Value, String> {
    ensure_writable(root)?;
    if changes.is_empty() || changes.len() > MAX_ROWS {
        return Err("revision exceeds supported row bounds".into());
    }
    let total: usize = changes
        .iter()
        .map(|(_, before, after)| {
            before.as_ref().map_or(0, Vec::len) + after.as_ref().map_or(0, Vec::len)
        })
        .sum();
    if total > MAX_BACKUP_BYTES {
        return Err("revision backups exceed the 32 MiB recovery budget".into());
    }
    history_budget(root, total as u64)?;
    let directory = state_root(root)?.join(crate::random::session_id());
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let revision = directory
        .file_name()
        .ok_or("invalid revision directory")?
        .to_string_lossy()
        .into_owned();
    let mut journal = Journal {
        version: 1,
        id: revision.clone(),
        state: State::Prepared,
        rows: Vec::new(),
        created_dirs: Vec::new(),
        undo_of,
    };
    for (index, (relative, before, after)) in changes.iter().enumerate() {
        let path = crate::project_ops::managed_path(root, relative)?;
        if hash(&path)? != before.as_ref().map(|bytes| crate::sha256_hex(bytes)) {
            return Err("content changed before revision preparation".into());
        }
        for (name, bytes) in [("before", before), ("after", after)] {
            if let Some(bytes) = bytes {
                durable(&directory.join(format!("{index:03}.{name}")), bytes)?;
            }
        }
        let mut missing = Vec::new();
        let mut parent = path.parent();
        while let Some(path) = parent {
            if path.exists() {
                break;
            }
            missing.push(
                path.strip_prefix(root)
                    .map_err(|error| error.to_string())?
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
            parent = path.parent();
        }
        for entry in missing.into_iter().rev() {
            if !journal.created_dirs.contains(&entry) {
                journal.created_dirs.push(entry);
            }
        }
        journal.rows.push(Row {
            path: relative.clone(),
            before: before.as_ref().map(|bytes| crate::sha256_hex(bytes)),
            after: after.as_ref().map(|bytes| crate::sha256_hex(bytes)),
        });
    }
    save(&directory, &journal)?;
    durable(
        &active(root)?,
        &serde_json::to_vec(&json!({"revision_id":revision})).map_err(|error| error.to_string())?,
    )?;
    journal.state = State::Publishing;
    save(&directory, &journal)?;
    let result = (|| -> Result<(), String> {
        for (index, row) in journal.rows.iter().enumerate() {
            #[cfg(test)]
            if std::env::var("AURUM_TRANSACTION_CRASH_AFTER")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                == Some(index)
            {
                std::process::exit(91);
            }
            if fail_after == Some(index) {
                return Err("injected publication interruption".into());
            }
            let path = crate::project_ops::managed_path(root, &row.path)?;
            if hash(&path)? != row.before {
                return Err("content changed during revision publication".into());
            }
            let bytes = blob(&directory, index, false, &row.after)?;
            apply_bytes(&path, bytes.as_deref())?;
            if hash(&path)? != row.after {
                return Err("published file failed hash verification".into());
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        let recovery = restore(root, &directory, &mut journal)?;
        return Ok(
            json!({"ok":false,"applied":false,"error":error,"revision_id":revision,"recovery":recovery}),
        );
    }
    journal.state = State::Committed;
    save(&directory, &journal)?;
    clear_active(root)?;
    Ok(
        json!({"ok":true,"applied":true,"revision_id":revision,"files_changed":journal.rows.len(),"undo_of":journal.undo_of}),
    )
}

fn history_budget(root: &Path, additional: u64) -> Result<(), String> {
    let directory = state_root(root)?;
    let mut bytes = 0u64;
    let mut count = 0;
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if id(&name).is_err() {
            continue;
        }
        count += 1;
        if count >= 32 {
            return Err("revision history reached its 32-entry limit; explicitly forget an old completed revision".into());
        }
        crate::diagnostics::checked_path(&entry.path()).map_err(|error| error.to_string())?;
        for file in std::fs::read_dir(entry.path()).map_err(|error| error.to_string())? {
            let file = file.map_err(|error| error.to_string())?;
            let metadata =
                std::fs::symlink_metadata(file.path()).map_err(|error| error.to_string())?;
            if !metadata.is_file() {
                return Err("unexpected entry in revision history".into());
            }
            bytes = bytes.saturating_add(metadata.len());
        }
    }
    if bytes
        .saturating_add(additional)
        .saturating_add(MAX_JOURNAL_BYTES)
        > MAX_HISTORY_BYTES
    {
        return Err("revision history reached its 128 MiB budget; explicitly forget an old completed revision".into());
    }
    Ok(())
}

pub fn forget(project: &Project, revision: &str) -> Result<Value, String> {
    id(revision)?;
    let _lease = crate::content_lock::ContentLock::exclusive(&project.root)?;
    ensure_writable(&project.root)?;
    let base = state_root(&project.root)?;
    let directory = base.join(revision);
    let journal = load(&directory)?;
    if journal.id != revision || !matches!(journal.state, State::Committed | State::RolledBack) {
        return Err("only completed revisions can be forgotten".into());
    }
    let canonical = crate::project::clean_path(
        directory
            .canonicalize()
            .map_err(|error| error.to_string())?,
    );
    let expected =
        crate::project::clean_path(base.canonicalize().map_err(|error| error.to_string())?)
            .join(revision);
    if canonical != expected {
        return Err("revision cleanup refused a redirected path".into());
    }
    std::fs::remove_dir_all(&directory).map_err(|error| error.to_string())?;
    Ok(
        json!({"ok":true,"forgotten_revision":revision,"content_changed":false,"undo_available":false}),
    )
}

pub(crate) fn publish_candidate(
    project: &Project,
    candidate: &Project,
    source_sha: &str,
    candidate_sha: &str,
    checked: &Value,
) -> Result<Value, String> {
    let _lease = crate::content_lock::ContentLock::exclusive(&project.root)?;
    ensure_writable(&project.root)?;
    if crate::snapshot::fingerprint_all(&project.root)? != source_sha
        || crate::snapshot::fingerprint_all(&candidate.root)? != candidate_sha
    {
        return Err("source or candidate changed before publication; revalidate".into());
    }
    let mut paths: Vec<String> = checked["changes"]
        .as_array()
        .ok_or("missing checked changes")?
        .iter()
        .map(|row| {
            row["path"]
                .as_str()
                .ok_or("missing checked path")
                .map(str::to_string)
        })
        .collect::<Result<_, _>>()?;
    for (relative, _) in crate::snapshot::inventory(&project.root)? {
        if paths.contains(&relative) {
            continue;
        }
        let original = crate::project_ops::managed_path(&project.root, &relative)?;
        let copied = crate::project_ops::managed_path(&candidate.root, &relative)?;
        if !copied.is_file()
            || crate::sha256_file(&original).map_err(|error| error.to_string())?
                != crate::sha256_file(&copied).map_err(|error| error.to_string())?
        {
            return Err(
                "validation removed or altered unrequested source content; publication refused"
                    .into(),
            );
        }
    }
    for (relative, _) in crate::snapshot::inventory(&candidate.root)? {
        if paths.contains(&relative) {
            continue;
        }
        let original = crate::project_ops::managed_path(&project.root, &relative)?;
        let copied = crate::project_ops::managed_path(&candidate.root, &relative)?;
        if original.exists() {
            if crate::sha256_file(&original).map_err(|error| error.to_string())?
                != crate::sha256_file(&copied).map_err(|error| error.to_string())?
            {
                return Err(
                    "validation altered unrequested source content; publication refused".into(),
                );
            }
        } else if relative.ends_with(".uid") {
            let bytes = read_text(&copied)?.ok_or("generated UID file disappeared")?;
            let text = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
            if bytes.len() > 128
                || !text.trim().strip_prefix("uid://").is_some_and(|value| {
                    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
            {
                return Err("unsupported generated UID metadata".into());
            }
            paths.push(relative);
        } else {
            return Err(
                "validation generated unsupported source files; publication refused".into(),
            );
        }
    }
    let mut changes = Vec::new();
    for relative in paths {
        let before = read_text(&crate::project_ops::managed_path(&project.root, &relative)?)?;
        let after = read_text(&crate::project_ops::managed_path(
            &candidate.root,
            &relative,
        )?)?;
        changes.push((relative, before, after));
    }
    publish_rows(&project.root, changes, None, None)
}

pub fn undo(project: &Project, revision: &str) -> Result<Value, String> {
    id(revision)?;
    let _lease = crate::content_lock::ContentLock::exclusive(&project.root)?;
    ensure_writable(&project.root)?;
    let directory = state_root(&project.root)?.join(revision);
    let journal = load(&directory)?;
    if journal.id != revision || journal.state != State::Committed {
        return Err("only a committed revision can be undone".into());
    }
    let mut changes = Vec::new();
    for (index, row) in journal.rows.iter().enumerate() {
        if hash(&crate::project_ops::managed_path(&project.root, &row.path)?)? != row.after {
            return Err(
                "revision undo conflicts with newer content; newer edits were preserved".into(),
            );
        }
        changes.push((
            row.path.clone(),
            blob(&directory, index, false, &row.after)?,
            blob(&directory, index, true, &row.before)?,
        ));
    }
    publish_rows(&project.root, changes, Some(revision.to_string()), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(Project);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("aurum-transaction-{}", crate::random::session_id()));
            std::fs::create_dir(&root).unwrap();
            std::fs::write(
                root.join("aurum.toml"),
                "schema_version=1\nname=\"transaction-test\"\nmodules=[]\n",
            )
            .unwrap();
            Self(Project::open(&root).unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0.root);
        }
    }

    #[test]
    fn crash_child() {
        if let Some(root) = std::env::var_os("AURUM_TRANSACTION_CRASH_ROOT") {
            let root = PathBuf::from(root);
            let _lease = crate::content_lock::ContentLock::exclusive(&root).unwrap();
            publish_rows(
                &root,
                vec![
                    (
                        "first.gd".into(),
                        Some(b"before".to_vec()),
                        Some(b"after".to_vec()),
                    ),
                    (
                        "second.gd".into(),
                        Some(b"before".to_vec()),
                        Some(b"after".to_vec()),
                    ),
                ],
                None,
                None,
            )
            .unwrap();
        }
    }

    fn crash_fixture() -> Fixture {
        let fixture = Fixture::new();
        for name in ["first.gd", "second.gd"] {
            std::fs::write(fixture.0.root.join(name), "before").unwrap();
        }
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let status = command
            .args(["--exact", "transactions::tests::crash_child", "--nocapture"])
            .env("AURUM_TRANSACTION_CRASH_ROOT", &fixture.0.root)
            .env("AURUM_TRANSACTION_CRASH_AFTER", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(91));
        fixture
    }

    #[test]
    fn real_process_exit_recovers_the_partially_published_revision() {
        let fixture = crash_fixture();
        assert!(ensure_writable(&fixture.0.root).is_err());
        assert_eq!(
            std::fs::read(fixture.0.root.join("first.gd")).unwrap(),
            b"after"
        );
        let result = recover(&fixture.0).unwrap();
        assert_eq!(result["rolled_back"], true);
        for name in ["first.gd", "second.gd"] {
            assert_eq!(std::fs::read(fixture.0.root.join(name)).unwrap(), b"before");
        }
        assert!(ensure_writable(&fixture.0.root).is_ok());
    }

    #[test]
    fn recovery_conflict_preserves_external_content_and_blocks_new_writes() {
        let fixture = crash_fixture();
        std::fs::write(fixture.0.root.join("first.gd"), "external").unwrap();
        let result = recover(&fixture.0).unwrap();
        assert_eq!(result["recovery_blocked"], true);
        assert_eq!(
            std::fs::read(fixture.0.root.join("first.gd")).unwrap(),
            b"external"
        );
        assert!(ensure_writable(&fixture.0.root).is_err());
    }

    #[test]
    fn corrupt_recovery_backups_are_not_silently_accepted() {
        let fixture = crash_fixture();
        let pointer: Value =
            serde_json::from_slice(&std::fs::read(active(&fixture.0.root).unwrap()).unwrap())
                .unwrap();
        let directory = state_root(&fixture.0.root)
            .unwrap()
            .join(pointer["revision_id"].as_str().unwrap());
        std::fs::write(directory.join("000.before"), "corrupted").unwrap();
        assert!(recover(&fixture.0).is_err());
        assert!(ensure_writable(&fixture.0.root).is_err());
        assert_eq!(
            std::fs::read(fixture.0.root.join("first.gd")).unwrap(),
            b"after"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_sharing_failure_rolls_back_already_replaced_files() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        for name in ["first.gd", "second.gd"] {
            std::fs::write(fixture.0.root.join(name), "before").unwrap();
        }
        let _reader = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1 | 2)
            .open(fixture.0.root.join("second.gd"))
            .unwrap();
        let result = publish_rows(
            &fixture.0.root,
            vec![
                (
                    "first.gd".into(),
                    Some(b"before".to_vec()),
                    Some(b"after".to_vec()),
                ),
                (
                    "second.gd".into(),
                    Some(b"before".to_vec()),
                    Some(b"after".to_vec()),
                ),
            ],
            None,
            None,
        )
        .unwrap();
        assert_eq!(result["recovery"]["rolled_back"], true);
        assert_eq!(
            std::fs::read(fixture.0.root.join("first.gd")).unwrap(),
            b"before"
        );
    }
    #[test]
    fn publication_and_whole_revision_undo_are_guarded() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.root.join("old.gd"), "old").unwrap();
        let result = publish_rows(
            &fixture.0.root,
            vec![
                (
                    "old.gd".into(),
                    Some(b"old".to_vec()),
                    Some(b"new".to_vec()),
                ),
                ("new.gd".into(), None, Some(b"created".to_vec())),
            ],
            None,
            None,
        )
        .unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(
            std::fs::read(fixture.0.root.join("old.gd")).unwrap(),
            b"new"
        );
        let reversed = undo(&fixture.0, result["revision_id"].as_str().unwrap()).unwrap();
        assert_eq!(reversed["ok"], true);
        assert_eq!(
            std::fs::read(fixture.0.root.join("old.gd")).unwrap(),
            b"old"
        );
        assert!(!fixture.0.root.join("new.gd").exists());
    }
    #[test]
    fn failure_after_first_file_restores_all_originals() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.root.join("old.gd"), "old").unwrap();
        let result = publish_rows(
            &fixture.0.root,
            vec![
                (
                    "old.gd".into(),
                    Some(b"old".to_vec()),
                    Some(b"new".to_vec()),
                ),
                ("new.gd".into(), None, Some(b"created".to_vec())),
            ],
            None,
            Some(1),
        )
        .unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["recovery"]["rolled_back"], true);
        assert_eq!(
            std::fs::read(fixture.0.root.join("old.gd")).unwrap(),
            b"old"
        );
        assert!(!active(&fixture.0.root).unwrap().exists());
    }
    #[test]
    fn undo_never_overwrites_a_newer_edit() {
        let fixture = Fixture::new();
        let result = publish_rows(
            &fixture.0.root,
            vec![("main.gd".into(), None, Some(b"first".to_vec()))],
            None,
            None,
        )
        .unwrap();
        std::fs::write(fixture.0.root.join("main.gd"), "external").unwrap();
        assert!(undo(&fixture.0, result["revision_id"].as_str().unwrap()).is_err());
        assert_eq!(
            std::fs::read(fixture.0.root.join("main.gd")).unwrap(),
            b"external"
        );
    }
}
