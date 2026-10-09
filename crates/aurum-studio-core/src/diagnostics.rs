//! Bounded local diagnostics. Operation payloads never enter the journal.
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const FILE_BYTES: u64 = 2 * 1024 * 1024;
pub const ARCHIVES: usize = 3;
pub const LINE_BYTES: usize = 4096;
pub const TAIL_BYTES: u64 = 256 * 1024;
pub const SESSION_BYTES: u64 = 64 * 1024 * 1024;
pub const SESSION_AGE: Duration = Duration::from_secs(14 * 24 * 60 * 60);
const LOCK_WAIT: Duration = Duration::from_millis(20);
static WRITTEN: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static WARNED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
pub struct Policy {
    pub file_bytes: u64,
    pub archives: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            file_bytes: FILE_BYTES,
            archives: ARCHIVES,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    operation_id: String,
    timestamp_ms: u64,
    operation: String,
    phase: String,
    duration_ms: u64,
    outcome: Option<String>,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn generation(path: &Path, index: usize) -> PathBuf {
    if index == 0 {
        return path.to_path_buf();
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{index}"));
    path.with_file_name(name)
}

// Defensive path hygiene, not isolation from malicious local programs.
pub(crate) fn checked_path(path: &Path) -> io::Result<()> {
    let absolute = std::path::absolute(path)?;
    for ancestor in absolute.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(invalid("diagnostic paths cannot contain symbolic links"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn lock(path: &Path, create: bool) -> io::Result<File> {
    let mut name = path
        .file_name()
        .ok_or_else(|| invalid("log has no filename"))?
        .to_os_string();
    name.push(".lock");
    let path = path.with_file_name(name);
    checked_path(&path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .open(path)?;
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if started.elapsed() < LOCK_WAIT => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn validate(policy: Policy) -> io::Result<()> {
    if !(128..=FILE_BYTES).contains(&policy.file_bytes) || policy.archives > ARCHIVES {
        return Err(invalid("diagnostic policy exceeds supported bounds"));
    }
    Ok(())
}

fn check_generations(path: &Path, policy: Policy) -> io::Result<()> {
    for index in 0..=policy.archives {
        let path = generation(path, index);
        checked_path(&path)?;
        if std::fs::symlink_metadata(&path).is_ok_and(|meta| !meta.is_file()) {
            return Err(invalid("diagnostic generation is not a regular file"));
        }
    }
    Ok(())
}

/// Append one record. No fsync: diagnostics are not a transaction journal.
/// A sidecar OS lock serializes rotation across threads and processes.
pub fn append(path: &Path, line: &str, policy: Policy) -> io::Result<()> {
    validate(policy)?;
    if line.contains(['\n', '\r'])
        || line.len() > LINE_BYTES
        || line.len() as u64 + 1 > policy.file_bytes
    {
        return Err(invalid(
            "diagnostic record exceeds bounds or contains a newline",
        ));
    }
    checked_path(path)?;
    std::fs::create_dir_all(path.parent().ok_or_else(|| invalid("log has no parent"))?)?;
    let _lock = lock(path, true)?;
    check_generations(path, policy)?;
    let length = match std::fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error),
    };
    if length + line.len() as u64 + 1 > policy.file_bytes {
        // Never promote an unbounded legacy log into a retained generation.
        if length > policy.file_bytes || policy.archives == 0 {
            std::fs::remove_file(path)?;
        } else {
            let oldest = generation(path, policy.archives);
            if oldest.exists() {
                std::fs::remove_file(oldest)?;
            }
            for index in (0..policy.archives).rev() {
                let source = generation(path, index);
                if source.exists() {
                    std::fs::rename(source, generation(path, index + 1))?;
                }
            }
        }
    }
    // Repair a partial last record left by a crash without scanning the file.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let length = file.metadata()?.len();
    if length > 0 {
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0];
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            let keep = length.min(LINE_BYTES as u64);
            file.seek(SeekFrom::Start(length - keep))?;
            let mut tail = vec![0; keep as usize];
            file.read_exact(&mut tail)?;
            let intact = tail
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map(|index| length - keep + index as u64 + 1)
                .unwrap_or(0);
            file.set_len(intact)?;
        }
    }
    file.seek(SeekFrom::End(0))?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")
}

/// Escape multiline output, redact before truncating, and keep valid UTF-8.
pub fn text_line(line: &str, secrets: &[&str]) -> String {
    let safe = crate::session::redact(line, secrets)
        .replace('\r', "\\r")
        .replace('\n', "\\n");
    if safe.len() <= LINE_BYTES {
        return safe;
    }
    let marker = " [truncated]";
    let mut end = LINE_BYTES - marker.len();
    while !safe.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{marker}", &safe[..end])
}

pub(crate) fn report(result: io::Result<()>) {
    if result.is_ok() {
        WRITTEN.fetch_add(1, Ordering::Relaxed);
    } else {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        if !WARNED.swap(true, Ordering::Relaxed) {
            eprintln!("aurum diagnostics: records could not be persisted; query op=logs for process-local counters");
        }
    }
}

pub fn counters() -> Value {
    json!({"scope":"current_process","written":WRITTEN.load(Ordering::Relaxed),"dropped":DROPPED.load(Ordering::Relaxed)})
}

/// A payload-free operation span. Unknown names are never logged.
pub struct Operation {
    root: PathBuf,
    op: &'static str,
    id: String,
    started: Instant,
}

impl Operation {
    pub fn start(root: &Path, op: &str) -> Option<Self> {
        let op = *crate::project_ops::OPERATIONS
            .iter()
            .find(|entry| **entry == op)?;
        if matches!(
            op,
            "describe"
                | "status"
                | "files"
                | "read"
                | "draft_read"
                | "logs"
                | "changes_check"
                | "changes_last"
        ) {
            return None;
        }
        let span = Self {
            root: root.to_path_buf(),
            op,
            id: crate::random::session_id(),
            started: Instant::now(),
        };
        span.record("started", None);
        Some(span)
    }

    fn record(&self, phase: &str, outcome: Option<&str>) {
        let record = Record {
            version: 1,
            operation_id: self.id.clone(),
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            operation: self.op.to_string(),
            phase: phase.to_string(),
            duration_ms: self.started.elapsed().as_millis() as u64,
            outcome: outcome.map(str::to_string),
        };
        let path = self.root.join(".aurum/logs/operations.jsonl");
        report(append(
            &path,
            &serde_json::to_string(&record).expect("static diagnostic schema"),
            Policy::default(),
        ));
    }

    pub fn finish(self, result: &Result<Value, String>) {
        let category = match result {
            Ok(value) if value.get("ok").and_then(Value::as_bool) == Some(false) => {
                "failed_verdict"
            }
            Ok(_) => "succeeded",
            Err(error) if error.contains("write permission") => "permission_denied",
            Err(error) if error.contains("changed") || error.contains("stale") => "conflict",
            Err(error) if error.contains("timed out") || error.contains("timeout") => "timeout",
            Err(_) => "execution_failed",
        };
        self.record("completed", Some(category));
    }
}

fn valid_record(record: &Record) -> bool {
    record.version == 1
        && record.operation_id.len() == 32
        && record
            .operation_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        && crate::project_ops::OPERATIONS.contains(&record.operation.as_str())
        && matches!(record.phase.as_str(), "started" | "completed")
        && matches!(
            record.outcome.as_deref(),
            None | Some(
                "succeeded"
                    | "failed_verdict"
                    | "permission_denied"
                    | "conflict"
                    | "timeout"
                    | "execution_failed"
            )
        )
}

/// Newest matching records, bounded independently of on-disk file sizes.
/// Missing logs return an empty tail without creating state directories.
pub fn query(root: &Path, input: &Value) -> Result<Value, String> {
    let limit = match input.get("limit") {
        None => 50,
        Some(value) => value
            .as_u64()
            .filter(|value| (1..=200).contains(value))
            .ok_or("'limit' must be 1..200")? as usize,
    };
    let operation = input
        .get("operation")
        .map(|value| {
            let op = value.as_str().ok_or("'operation' must be a string")?;
            if !crate::project_ops::OPERATIONS.contains(&op) {
                return Err("unknown operation filter");
            }
            Ok(op)
        })
        .transpose()?;
    let failures_only = match input.get("failures_only") {
        None => false,
        Some(value) => value.as_bool().ok_or("'failures_only' must be a boolean")?,
    };
    let path = root.join(".aurum/logs/operations.jsonl");
    let mut records = Vec::new();
    let mut remaining = TAIL_BYTES;
    let mut truncated = false;
    let mut invalid_records = 0;
    for index in 0..=ARCHIVES {
        if records.len() >= limit || remaining == 0 {
            truncated = true;
            break;
        }
        let path = generation(&path, index);
        checked_path(&path).map_err(|error| error.to_string())?;
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("diagnostic generation is not a regular file".into());
        }
        let take = metadata.len().min(remaining);
        file.seek(SeekFrom::Start(metadata.len() - take))
            .map_err(|error| error.to_string())?;
        let mut bytes = Vec::with_capacity(take as usize);
        file.take(take)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        remaining -= bytes.len() as u64;
        truncated |= take < metadata.len();
        let ends_with_newline = bytes.ends_with(b"\n");
        let text = String::from_utf8_lossy(&bytes);
        let mut lines: Vec<_> = text.lines().collect();
        if take < metadata.len() && !lines.is_empty() {
            lines.remove(0);
        }
        if !ends_with_newline {
            lines.pop();
        }
        for line in lines.into_iter().rev() {
            let Ok(record) = serde_json::from_str::<Record>(line) else {
                invalid_records += 1;
                continue;
            };
            if !valid_record(&record) {
                invalid_records += 1;
                continue;
            }
            if operation.is_some_and(|op| record.operation != op) {
                continue;
            }
            if failures_only && matches!(record.outcome.as_deref(), None | Some("succeeded")) {
                continue;
            }
            records.push(record);
            if records.len() >= limit {
                truncated = true;
                break;
            }
        }
    }
    records.reverse();
    Ok(
        json!({"records":records,"truncated":truncated,"invalid_records":invalid_records,
        "budget":{"file_bytes":FILE_BYTES,"files":ARCHIVES+1,"total_bytes":FILE_BYTES*(ARCHIVES+1) as u64,"query_bytes":TAIL_BYTES,"records_max":200},
        "counters":counters(),"payloads_logged":false}),
    )
}

/// Prune recognized session logs only. Called on startup, not per record.
pub fn retain_sessions(root: &Path) -> io::Result<()> {
    checked_path(root)?;
    let started = Instant::now();
    let budget = Duration::from_millis(250);
    let now = SystemTime::now();
    let mut candidates = Vec::new();
    let mut total = 0u64;
    for entry in std::fs::read_dir(root)?.take(4096) {
        if started.elapsed() >= budget {
            break;
        }
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() != 32
            || !name.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !entry.file_type()?.is_dir()
        {
            continue;
        }
        let log = entry.path().join("session.log");
        for index in 0..=ARCHIVES {
            let path = generation(&log, index);
            if let Ok(metadata) = std::fs::symlink_metadata(&path) {
                if !metadata.is_file() {
                    continue;
                }
                total = total.saturating_add(metadata.len());
                candidates.push((metadata.modified()?, metadata.len(), path, log.clone()));
            }
        }
    }
    candidates.sort_by_key(|candidate| candidate.0);
    for (modified, bytes, path, log) in candidates {
        if started.elapsed() >= budget {
            break;
        }
        if total <= SESSION_BYTES && now.duration_since(modified).unwrap_or_default() <= SESSION_AGE
        {
            continue;
        }
        let Ok(_lock) = lock(&log, true) else {
            continue;
        };
        checked_path(&path)?;
        let metadata = match std::fs::metadata(&path) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if metadata.len() != bytes || metadata.modified()? != modified {
            continue;
        }
        std::fs::remove_file(path)?;
        total = total.saturating_sub(bytes);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("aurum-diagnostics-{}", crate::random::session_id()));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn log(&self) -> PathBuf {
            self.0.join("session.log")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn rotation_is_bounded_and_keeps_newest_records() {
        let fixture = Fixture::new();
        let policy = Policy {
            file_bytes: 128,
            archives: 3,
        };
        for index in 0..100 {
            append(
                &fixture.log(),
                &format!("record-{index:03}-{}", "x".repeat(70)),
                policy,
            )
            .unwrap();
        }
        let mut total = 0;
        for index in 0..=3 {
            let path = generation(&fixture.log(), index);
            let size = std::fs::metadata(&path).unwrap().len();
            assert!(size <= 128);
            total += size;
        }
        assert!(total <= 512);
        assert!(std::fs::read_to_string(fixture.log())
            .unwrap()
            .contains("record-099"));
        assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 5);
    }

    #[test]
    fn concurrent_threads_do_not_interleave_records() {
        let fixture = Fixture::new();
        std::thread::scope(|scope| {
            for worker in 0..4 {
                let path = fixture.log();
                scope.spawn(move || {
                    for index in 0..30 {
                        let line = format!("{worker}-{index}-{}", "x".repeat(80));
                        // Contention is allowed to drop a diagnostic, never to
                        // hang an operation. Retry here only to test integrity.
                        for attempt in 0..100 {
                            if append(&path, &line, Policy::default()).is_ok() {
                                break;
                            }
                            assert!(attempt < 99);
                        }
                    }
                });
            }
        });
        let text = std::fs::read_to_string(fixture.log()).unwrap();
        assert_eq!(text.lines().count(), 120);
        for line in text.lines() {
            assert!(line.ends_with(&"x".repeat(80)));
        }
    }

    #[test]
    fn lock_contention_has_a_deadline_and_releases_on_drop() {
        let fixture = Fixture::new();
        let held = lock(&fixture.log(), true).unwrap();
        let start = Instant::now();
        assert!(append(&fixture.log(), "blocked", Policy::default()).is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        drop(held);
        append(&fixture.log(), "recovered", Policy::default()).unwrap();
        assert_eq!(
            std::fs::read_to_string(fixture.log()).unwrap(),
            "recovered\n"
        );
    }

    #[test]
    fn partial_record_is_removed_before_the_next_append() {
        let fixture = Fixture::new();
        std::fs::write(fixture.log(), "complete\npartial").unwrap();
        append(&fixture.log(), "next", Policy::default()).unwrap();
        assert_eq!(
            std::fs::read_to_string(fixture.log()).unwrap(),
            "complete\nnext\n"
        );
    }

    #[test]
    fn unicode_multiline_and_credentials_are_bounded() {
        let text = text_line(&format!("line\n{}", "界".repeat(5000)), &[]);
        assert!(text.len() <= LINE_BYTES && text.contains("\\n") && text.ends_with(" [truncated]"));
        assert!(!text.contains('\n'));
        for line in [
            "Authorization: Bearer abc",
            "api_key=abcdef",
            "https://localhost/?t=secret",
            "password: xyz",
            "token=abcdef",
            "sk-ant-abcdef",
        ] {
            let text = text_line(line, &[]);
            assert!(text.contains(crate::session::REDACTED));
            assert!(!text.contains("abcdef"));
        }
        assert!(!text_line("a known-secret value", &["known-secret"]).contains("known-secret"));
    }

    #[test]
    fn empty_tail_and_polling_do_not_create_logs() {
        let fixture = Fixture::new();
        assert!(Operation::start(&fixture.0, "read").is_none());
        assert!(Operation::start(&fixture.0, "status").is_none());
        assert!(Operation::start(&fixture.0, "not-an-operation-secret").is_none());
        assert_eq!(query(&fixture.0, &json!({})).unwrap()["records"], json!([]));
        assert!(!fixture.0.join(".aurum").exists());
        for limit in [0, 201] {
            assert!(query(&fixture.0, &json!({"limit":limit})).is_err());
        }
        assert!(query(&fixture.0, &json!({"failures_only":"yes"})).is_err());
    }

    #[test]
    fn operation_spans_report_pairing_failure_and_no_payloads() {
        let fixture = Fixture::new();
        Operation::start(&fixture.0, "write")
            .unwrap()
            .finish(&Ok(json!({"ok":true,"source":"secret-source"})));
        Operation::start(&fixture.0, "build")
            .unwrap()
            .finish(&Ok(json!({"ok":false,"error":"secret-output"})));
        Operation::start(&fixture.0, "write")
            .unwrap()
            .finish(&Err("stale hash secret-password".into()));
        let value = query(&fixture.0, &json!({})).unwrap();
        let records = value["records"].as_array().unwrap();
        assert_eq!(records.len(), 6);
        assert_eq!(records[0]["operation_id"], records[1]["operation_id"]);
        assert_eq!(records[3]["outcome"], "failed_verdict");
        assert!(!value.to_string().contains("secret-"));
        let filtered = query(
            &fixture.0,
            &json!({"failures_only":true,"operation":"write"}),
        )
        .unwrap();
        assert_eq!(filtered["records"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["records"][0]["outcome"], "conflict");
        assert_eq!(
            query(&fixture.0, &json!({"limit":2})).unwrap()["records"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn tail_refuses_unknown_fields_and_is_bounded_on_external_files() {
        let fixture = Fixture::new();
        let path = fixture.0.join(".aurum/logs/operations.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let huge = "x".repeat(TAIL_BYTES as usize + 10);
        std::fs::write(
            &path,
            format!("{huge}\n{{\"version\":1,\"payload\":\"secret\"}}\n"),
        )
        .unwrap();
        let result = query(&fixture.0, &json!({})).unwrap();
        assert_eq!(result["records"], json!([]));
        assert_eq!(result["truncated"], true);
        assert_eq!(result["invalid_records"], 1);
        assert!(!result.to_string().contains("secret"));
    }

    #[test]
    fn session_retention_preserves_tokens_metadata_and_other_logs() {
        let fixture = Fixture::new();
        let session = fixture.0.join(crate::random::session_id());
        std::fs::create_dir_all(&session).unwrap();
        let log = session.join("session.log");
        std::fs::write(&log, "old\n").unwrap();
        File::options()
            .write(true)
            .open(&log)
            .unwrap()
            .set_modified(SystemTime::now() - SESSION_AGE - Duration::from_secs(10))
            .unwrap();
        for name in ["token", "session.json", "custom.log"] {
            std::fs::write(session.join(name), "keep").unwrap();
        }
        retain_sessions(&fixture.0).unwrap();
        assert!(!log.exists());
        for name in ["token", "session.json", "custom.log"] {
            assert!(session.join(name).exists());
        }
        append(&log, "new", Policy::default()).unwrap();
        retain_sessions(&fixture.0).unwrap();
        assert!(log.exists());
    }

    #[test]
    fn session_retention_caps_managed_logs_without_deleting_session_state() {
        let fixture = Fixture::new();
        for _ in 0..34 {
            let session = fixture.0.join(crate::random::session_id());
            std::fs::create_dir_all(&session).unwrap();
            File::create(session.join("session.log"))
                .unwrap()
                .set_len(FILE_BYTES)
                .unwrap();
            std::fs::write(session.join("session.json"), "keep").unwrap();
        }
        retain_sessions(&fixture.0).unwrap();
        let mut total = 0;
        for entry in std::fs::read_dir(&fixture.0).unwrap() {
            let entry = entry.unwrap();
            assert!(entry.path().join("session.json").exists());
            if let Ok(metadata) = std::fs::metadata(entry.path().join("session.log")) {
                total += metadata.len();
            }
        }
        assert!(total <= SESSION_BYTES);
    }

    #[test]
    fn logging_failure_does_not_change_the_operation_result() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join(".aurum"), "blocked").unwrap();
        let payload = json!({"ok":true,"important":"unchanged"});
        let result = Ok(payload.clone());
        Operation::start(&fixture.0, "write")
            .unwrap()
            .finish(&result);
        assert_eq!(result, Ok(payload));
    }

    #[test]
    fn invalid_policy_or_non_file_generation_preserves_existing_data() {
        let fixture = Fixture::new();
        std::fs::write(fixture.log(), "keep\n").unwrap();
        assert!(append(
            &fixture.log(),
            "new",
            Policy {
                file_bytes: 1,
                archives: 3
            }
        )
        .is_err());
        std::fs::create_dir(generation(&fixture.log(), 2)).unwrap();
        assert!(append(&fixture.log(), "new", Policy::default()).is_err());
        assert_eq!(std::fs::read_to_string(fixture.log()).unwrap(), "keep\n");
    }

    #[cfg(unix)]
    #[test]
    fn links_cannot_redirect_logging_or_retention() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        std::os::unix::fs::symlink(&outside.0, fixture.0.join("linked")).unwrap();
        assert!(append(&fixture.0.join("linked/log"), "secret", Policy::default()).is_err());
        assert_eq!(std::fs::read_dir(&outside.0).unwrap().count(), 0);
    }
}
