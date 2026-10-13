//! Explicit, bounded source baselines and read-only filtered deltas.
use crate::{files, snapshot};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const RECORD_BYTES: usize = 4 * 1024 * 1024;
pub const SOURCE_BYTES: u64 = 512 * 1024 * 1024;
pub const BASELINES: usize = 16;

fn check_operation() -> Result<(), String> {
    if let Some(context) = crate::operation_context::OperationContext::current() {
        context.check().map_err(|error| error.to_string())?;
    }
    Ok(())
}
fn publication_barrier() -> Result<Option<crate::operation_context::CommitGuard>, String> {
    let context = crate::operation_context::OperationContext::current();
    if let Some(context) = &context {
        context.phase(crate::operation_context::Phase::Publishing);
    }
    context
        .as_ref()
        .map(|context| context.commit_barrier())
        .transpose()
        .map_err(|error| error.to_string())
}
fn digest(bytes: &[u8]) -> Result<String, String> {
    let mut hash = crate::hash::Sha256::new();
    for chunk in bytes.chunks(65536) {
        check_operation()?;
        hash.update(chunk);
    }
    check_operation()?;
    Ok(hash.hex())
}
fn hash_source(reader: impl Read, expected: u64) -> Result<String, String> {
    let mut reader = reader.take(expected.saturating_add(1));
    let mut hash = crate::hash::Sha256::new();
    let mut chunk = [0u8; 65536];
    let mut observed = 0u64;
    loop {
        check_operation()?;
        let count = reader.read(&mut chunk).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        observed += count as u64;
        hash.update(&chunk[..count]);
        check_operation()?;
    }
    if observed != expected {
        return Err("source changed during context scan; retry".into());
    }
    Ok(hash.hex())
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    prefix: String,
    extensions: Vec<String>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    bytes: u64,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    project: PathBuf,
    scope: Scope,
    files: BTreeMap<String, Entry>,
}
fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn clean_prefix(value: &str) -> Result<String, String> {
    let value = value.replace('\\', "/");
    if value.starts_with('/') {
        return Err("context prefix must be project-relative".into());
    }
    let value = value.trim_end_matches('/');
    if !value.is_empty()
        && (value.len() > 1024
            || value
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || value.contains(':')
            || value.chars().any(char::is_control)
            || !snapshot::includes(value))
    {
        return Err("context prefix must be a public project-relative path".into());
    }
    Ok(value.to_string())
}
fn scope(input: &Value) -> Result<Scope, String> {
    let prefix = input
        .get("prefix")
        .map(|value| value.as_str().ok_or("prefix must be a string"))
        .transpose()?
        .unwrap_or("");
    let mut extensions = Vec::new();
    if let Some(value) = input.get("extensions") {
        let values = value.as_array().ok_or("extensions must be an array")?;
        if values.len() > 16 {
            return Err("context supports at most 16 extensions".into());
        }
        for value in values {
            let value = value
                .as_str()
                .ok_or("extension must be a string")?
                .trim_start_matches('.')
                .to_ascii_lowercase();
            if value.is_empty()
                || value.len() > 32
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err("invalid context extension".into());
            }
            extensions.push(value);
        }
    }
    extensions.sort();
    extensions.dedup();
    Ok(Scope {
        prefix: clean_prefix(prefix)?,
        extensions,
    })
}
fn root(root: &Path) -> Result<PathBuf, String> {
    root.canonicalize()
        .map(crate::project::clean_path)
        .map_err(|error| error.to_string())
}
fn directory(root: &Path) -> Result<PathBuf, String> {
    let path = files::confined(root, ".aurum/agent-context")?;
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    Ok(path)
}
struct StoreLease(File);
impl Drop for StoreLease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
fn lease(directory: &Path) -> Result<StoreLease, String> {
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
        .map_err(|_| "context baseline store is busy")?;
    Ok(StoreLease(file))
}
fn accepts(scope: &Scope, path: &str) -> bool {
    (scope.prefix.is_empty()
        || path == scope.prefix
        || path.starts_with(&format!("{}/", scope.prefix)))
        && (scope.extensions.is_empty()
            || Path::new(path)
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|extension| {
                    scope
                        .extensions
                        .iter()
                        .any(|expected| expected.eq_ignore_ascii_case(extension))
                }))
}
fn scan_once(root: &Path, scope: &Scope) -> Result<BTreeMap<String, Entry>, String> {
    check_operation()?;
    let mut result = BTreeMap::new();
    let mut total = 0u64;
    let inventory = if scope.prefix.is_empty() {
        snapshot::inventory(root)?
    } else {
        let target = files::confined(root, &scope.prefix)?;
        crate::diagnostics::checked_path(&target).map_err(|error| error.to_string())?;
        match std::fs::symlink_metadata(&target) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.to_string()),
            Ok(metadata) if metadata.is_dir() => snapshot::inventory(&target)?
                .into_iter()
                .map(|(path, bytes)| (format!("{}/{path}", scope.prefix), bytes))
                .collect(),
            Ok(metadata) if metadata.is_file() => vec![(scope.prefix.clone(), metadata.len())],
            Ok(_) => return Err("context scope is not an ordinary source path".into()),
        }
    };
    for (path, bytes) in inventory {
        check_operation()?;
        if !accepts(scope, &path) {
            continue;
        }
        total = total.saturating_add(bytes);
        if total > SOURCE_BYTES || path.len() > 1024 {
            return Err(
                "context scope exceeds its source/path budget; select a narrower prefix".into(),
            );
        }
        let target = files::confined(root, &path)?;
        crate::diagnostics::checked_path(&target).map_err(|error| error.to_string())?;
        let file = File::open(target).map_err(|error| error.to_string())?;
        let sha256 = hash_source(file, bytes)?;
        result.insert(path, Entry { bytes, sha256 });
    }
    Ok(result)
}
fn capture(root: &Path, scope: Scope) -> Result<Record, String> {
    check_operation()?;
    if let Some(context) = crate::operation_context::OperationContext::current() {
        context.phase(crate::operation_context::Phase::Staging);
    }
    let _lease = crate::content_lock::ContentLock::shared(root)?;
    crate::transactions::ensure_writable(root)?;
    let files = scan_once(root, &scope)?;
    if scan_once(root, &scope)? != files {
        return Err("source changed during context scan; no baseline published".into());
    }
    Ok(Record {
        version: 1,
        project: root.to_path_buf(),
        scope,
        files,
    })
}
fn encoded(record: &Record) -> Result<Vec<u8>, String> {
    check_operation()?;
    let bytes = serde_json::to_vec(record).map_err(|error| error.to_string())?;
    check_operation()?;
    if bytes.len() > RECORD_BYTES {
        return Err("context manifest exceeds 4 MiB; select a narrower scope".into());
    }
    Ok(bytes)
}
fn id(record: &Record) -> Result<String, String> {
    digest(&encoded(record)?)
}
fn read(root: &Path, baseline: &str) -> Result<Record, String> {
    check_operation()?;
    if !valid_id(baseline) {
        return Err("baseline_id must be 64 lowercase hexadecimal characters".into());
    }
    let path = directory(root)?.join(format!("{baseline}.json"));
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| "context baseline is unavailable; create a fresh snapshot")?
        .take(RECORD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    check_operation()?;
    if bytes.len() > RECORD_BYTES {
        return Err("context baseline exceeds its bound".into());
    }
    let record: Record =
        serde_json::from_slice(&bytes).map_err(|_| "invalid context baseline schema")?;
    if record.version != 1
        || !crate::ownership::paths_equal(&record.project, root)
        || id(&record)? != baseline
        || clean_prefix(&record.scope.prefix)? != record.scope.prefix
        || record.files.len() > 20_000
    {
        return Err("context baseline identity is invalid".into());
    }
    // A digest alone cannot establish the shape of manually copied manifests.
    if scope(&serde_json::to_value(&record.scope).map_err(|error| error.to_string())?)?
        != record.scope
    {
        return Err("context baseline contents are invalid".into());
    }
    let mut total = 0u64;
    for (path, entry) in &record.files {
        check_operation()?;
        if path.is_empty()
            || !clean_prefix(path).is_ok_and(|clean| clean == *path)
            || !accepts(&record.scope, path)
            || !valid_id(&entry.sha256)
        {
            return Err("context baseline contents are invalid".into());
        }
        total = total
            .checked_add(entry.bytes)
            .filter(|total| *total <= SOURCE_BYTES)
            .ok_or("context baseline contents are invalid")?;
    }
    Ok(record)
}
pub fn snapshot(root_path: &Path, input: &Value) -> Result<Value, String> {
    check_operation()?;
    let root = root(root_path)?;
    crate::Project::open(&root).map_err(|error| error.to_string())?;
    let selected = scope(input)?;
    let record = capture(&root, selected)?;
    let bytes = encoded(&record)?;
    let baseline = digest(&bytes)?;
    let directory = directory(&root)?;
    check_operation()?;
    let _lease = lease(&directory)?;
    let path = directory.join(format!("{baseline}.json"));
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    let reused = path.exists();
    if reused {
        read(&root, &baseline)?;
    } else {
        let mut count = 0;
        for (scanned, entry) in std::fs::read_dir(&directory)
            .map_err(|error| error.to_string())?
            .enumerate()
        {
            check_operation()?;
            if scanned >= 64 {
                return Err("context baseline directory exceeds its entry bound".into());
            }
            let name = entry
                .map_err(|error| error.to_string())?
                .file_name()
                .to_string_lossy()
                .into_owned();
            if name.ends_with(".json") {
                count += 1;
            }
        }
        if count >= BASELINES {
            return Err("context retains 16 baselines; explicitly forget an old baseline".into());
        }
        let guard = publication_barrier()?;
        files::write_atomic(&path, &bytes).map_err(|error| error.to_string())?;
        if let Some(guard) = guard {
            guard.finish();
        }
    }
    Ok(
        json!({"ok":true,"baseline_id":baseline,"scope":record.scope,"files":record.files.len(),"source_bytes":record.files.values().map(|entry|entry.bytes).sum::<u64>(),"reused":reused,"source_mutated":false}),
    )
}
pub fn delta(root_path: &Path, input: &Value) -> Result<Value, String> {
    let root = root(root_path)?;
    let baseline = input["baseline_id"]
        .as_str()
        .ok_or("baseline_id is required")?;
    let old = read(&root, baseline)?;
    let limit = input
        .get("limit")
        .map(|value| {
            value
                .as_u64()
                .filter(|value| (1..=200).contains(value))
                .ok_or("context limit must be 1 to 200")
        })
        .transpose()?
        .unwrap_or(50) as usize;
    let after = input
        .get("after")
        .map(|value| value.as_str().ok_or("after must be a string"))
        .transpose()?;
    if after
        .is_some_and(|path| path.is_empty() || !clean_prefix(path).is_ok_and(|clean| clean == path))
    {
        return Err("invalid context pagination path".into());
    }
    let expected = input
        .get("current_id")
        .map(|value| {
            value
                .as_str()
                .filter(|value| valid_id(value))
                .ok_or("current_id must be a context digest")
        })
        .transpose()?;
    if after.is_some() && expected.is_none() {
        return Err("context pagination requires current_id from the previous page".into());
    }
    let current = capture(&root, old.scope.clone())?;
    let current_id = id(&current)?;
    if expected.is_some_and(|expected| expected != current_id) {
        return Err("source changed between context pages; restart the delta query".into());
    }
    let mut paths: Vec<_> = old.files.keys().chain(current.files.keys()).collect();
    paths.sort();
    paths.dedup();
    let changed: Vec<_> = paths
        .into_iter()
        .filter(|path| old.files.get(*path) != current.files.get(*path))
        .collect();
    let mut pending = changed
        .iter()
        .copied()
        .filter(|path| after.is_none_or(|after| path.as_str() > after));
    let rows: Vec<_> = pending.by_ref().take(limit).map(|path| {
        let previous = old.files.get(path); let next = current.files.get(path);
        json!({"path":path,"change":if previous.is_none(){"created"}else if next.is_none(){"deleted"}else{"modified"},"before":previous,"after":next})
    }).collect();
    let truncated = pending.next().is_some();
    let next_after = if truncated {
        rows.last().and_then(|row| row["path"].as_str())
    } else {
        None
    };
    Ok(
        json!({"ok":true,"baseline_id":baseline,"current_id":current_id,"scope":old.scope,"changes":rows,"changed_files":changed.len(),"truncated":truncated,"next_after":next_after,"source_mutated":false,"baseline_advanced":false}),
    )
}
pub fn forget(root_path: &Path, input: &Value) -> Result<Value, String> {
    check_operation()?;
    let root = root(root_path)?;
    let baseline = input["baseline_id"]
        .as_str()
        .ok_or("baseline_id is required")?;
    read(&root, baseline)?;
    let directory = directory(&root)?;
    let _lease = lease(&directory)?;
    let path = directory.join(format!("{baseline}.json"));
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    // Recheck under the store lease before entering the removal barrier.
    read(&root, baseline)?;
    let guard = publication_barrier()?;
    std::fs::remove_file(path).map_err(|error| error.to_string())?;
    if let Some(guard) = guard {
        guard.finish();
    }
    Ok(json!({"ok":true,"forgotten":baseline,"source_mutated":false,"recoverable":false}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation_context::OperationContext;
    use std::time::{Duration, Instant};
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "aurum-context-test-{}",
                crate::random::session_id()
            ));
            std::fs::create_dir(&root).unwrap();
            std::fs::write(
                root.join("aurum.toml"),
                "schema_version=1\nname=\"context\"\nmodules=[]\n",
            )
            .unwrap();
            std::fs::create_dir(root.join("game")).unwrap();
            std::fs::write(root.join("game/main.gd"), "old\n").unwrap();
            std::fs::write(root.join("game/keep.gd"), "keep\n").unwrap();
            Self(root)
        }
        fn baseline(&self) -> String {
            snapshot(&self.0, &json!({"prefix":"game","extensions":[".GD"]})).unwrap()
                ["baseline_id"]
                .as_str()
                .unwrap()
                .into()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn file_scopes_and_deleted_scopes_are_supported() {
        let fixture = Fixture::new();
        let first = snapshot(&fixture.0, &json!({"prefix":"game/main.gd"})).unwrap();
        assert_eq!(first["files"], 1);
        std::fs::remove_file(fixture.0.join("game/main.gd")).unwrap();
        let result = delta(&fixture.0, &json!({"baseline_id":first["baseline_id"]})).unwrap();
        assert_eq!(result["changed_files"], 1);
        assert_eq!(result["changes"][0]["change"], "deleted");
    }
    #[test]
    fn copied_foreign_baseline_is_not_trusted() {
        let fixture = Fixture::new();
        let baseline = fixture.baseline();
        let other = Fixture::new();
        std::fs::create_dir_all(other.0.join(".aurum/agent-context")).unwrap();
        let name = format!("{baseline}.json");
        std::fs::copy(
            fixture.0.join(".aurum/agent-context").join(&name),
            other.0.join(".aurum/agent-context").join(&name),
        )
        .unwrap();
        assert!(delta(&other.0, &json!({"baseline_id":baseline}))
            .unwrap_err()
            .contains("identity"));
    }
    #[test]
    fn same_size_edits_creates_deletes_and_scope_are_reported() {
        let fixture = Fixture::new();
        let baseline = fixture.baseline();
        std::fs::write(fixture.0.join("game/main.gd"), "new\n").unwrap();
        std::fs::remove_file(fixture.0.join("game/keep.gd")).unwrap();
        std::fs::write(fixture.0.join("game/new.gd"), "created\n").unwrap();
        std::fs::write(fixture.0.join("game/ignored.png"), [0, 255]).unwrap();
        let result = delta(&fixture.0, &json!({"baseline_id":baseline})).unwrap();
        assert_eq!(result["changed_files"], 3);
        assert_eq!(result["changes"][0]["change"], "deleted");
        assert_eq!(result["changes"][1]["change"], "modified");
        assert_eq!(result["changes"][2]["change"], "created");
        assert!(
            result["changes"][1]["after"]["sha256"] != result["changes"][1]["before"]["sha256"]
        );
    }
    #[test]
    fn pagination_is_bound_to_current_content_and_does_not_advance_baseline() {
        let fixture = Fixture::new();
        let baseline = fixture.baseline();
        for name in ["a.gd", "b.gd", "c.gd"] {
            std::fs::write(fixture.0.join("game").join(name), name).unwrap();
        }
        let first = delta(&fixture.0, &json!({"baseline_id":baseline,"limit":1})).unwrap();
        assert_eq!(first["changes"].as_array().unwrap().len(), 1);
        assert_eq!(first["truncated"], true);
        let input = json!({"baseline_id":baseline,"limit":1,"after":first["next_after"],"current_id":first["current_id"]});
        assert_eq!(
            delta(&fixture.0, &input).unwrap()["changes"][0]["path"],
            "game/b.gd"
        );
        std::fs::write(fixture.0.join("game/c.gd"), "changed").unwrap();
        assert!(delta(&fixture.0, &input)
            .unwrap_err()
            .contains("between context pages"));
        assert_eq!(
            read(&root(&fixture.0).unwrap(), &baseline)
                .unwrap()
                .files
                .len(),
            2
        );
    }
    #[test]
    fn invalid_scope_fails_before_creating_state_and_permissions_are_shared() {
        let fixture = Fixture::new();
        for prefix in [
            "../other",
            "/absolute",
            ".aurum",
            "game/../other",
            "C:/outside",
        ] {
            assert!(snapshot(&fixture.0, &json!({"prefix":prefix})).is_err());
        }
        assert!(snapshot(&fixture.0, &json!({"extensions":["bad/path"]})).is_err());
        assert!(
            crate::project_ops::execute(&fixture.0, &json!({"op":"context_snapshot"}), true)
                .is_err()
        );
        assert!(!fixture.0.join(".aurum").exists());
    }
    #[test]
    fn unchanged_baseline_is_reused_and_readonly_delta_writes_nothing() {
        let fixture = Fixture::new();
        let baseline = fixture.baseline();
        let before = std::fs::read_dir(fixture.0.join(".aurum/agent-context"))
            .unwrap()
            .count();
        let result = delta(&fixture.0, &json!({"baseline_id":baseline})).unwrap();
        assert_eq!(result["changed_files"], 0);
        assert_eq!(result["current_id"], baseline);
        assert_eq!(
            std::fs::read_dir(fixture.0.join(".aurum/agent-context"))
                .unwrap()
                .count(),
            before
        );
        assert_eq!(
            snapshot(&fixture.0, &json!({"prefix":"game","extensions":["gd"]})).unwrap()["reused"],
            true
        );
    }
    #[test]
    fn corrupt_foreign_and_missing_baselines_are_refused_and_forget_is_exact() {
        let fixture = Fixture::new();
        let baseline = fixture.baseline();
        let path = fixture
            .0
            .join(".aurum/agent-context")
            .join(format!("{baseline}.json"));
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, "{}").unwrap();
        assert!(delta(&fixture.0, &json!({"baseline_id":baseline})).is_err());
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(
            forget(&fixture.0, &json!({"baseline_id":baseline})).unwrap()["recoverable"],
            false
        );
        assert!(!path.exists());
        assert!(fixture.0.join("game/main.gd").exists());
        assert!(delta(&fixture.0, &json!({"baseline_id":baseline})).is_err());
        assert!(forget(&fixture.0, &json!({"baseline_id":"../main.gd"})).is_err());
    }
    #[test]
    fn retention_never_silently_discards_a_baseline() {
        let fixture = Fixture::new();
        let first = fixture.baseline();
        for index in 1..BASELINES {
            std::fs::write(fixture.0.join("game/main.gd"), index.to_string()).unwrap();
            fixture.baseline();
        }
        std::fs::write(fixture.0.join("game/main.gd"), "next").unwrap();
        assert!(
            snapshot(&fixture.0, &json!({"prefix":"game","extensions":["gd"]}))
                .unwrap_err()
                .contains("16 baselines")
        );
        forget(&fixture.0, &json!({"baseline_id":first})).unwrap();
        fixture.baseline();
    }
    #[test]
    fn source_hash_cancellation_stops_after_one_chunk_without_a_commit() {
        struct CancellingReader {
            source: std::io::Cursor<Vec<u8>>,
            context: OperationContext,
            reads: usize,
        }
        impl Read for CancellingReader {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                let count = self.source.read(output)?;
                self.reads += 1;
                if count > 0 {
                    self.context.request_cancel()?;
                }
                Ok(count)
            }
        }
        let context = OperationContext::new(Instant::now() + Duration::from_secs(20));
        let mut reader = CancellingReader {
            source: std::io::Cursor::new(vec![b'x'; 2 * 65536]),
            context: context.clone(),
            reads: 0,
        };
        let error = context
            .scope(|| hash_source(&mut reader, 2 * 65536))
            .unwrap()
            .unwrap_err();
        assert!(error.contains("cancellation"));
        assert_eq!(reader.reads, 1);
        assert_eq!(reader.source.position(), 65536);
        assert!(!context.committed());
        assert!(context.commit_status_verified());
    }
    #[test]
    fn only_a_new_baseline_publication_marks_the_job_committed() {
        let fixture = Fixture::new();
        let publication = OperationContext::new(Instant::now() + Duration::from_secs(20));
        let first = publication
            .scope(|| snapshot(&fixture.0, &json!({"prefix":"game"})))
            .unwrap()
            .unwrap();
        assert_eq!(first["reused"], false);
        assert!(publication.committed());
        assert!(publication.commit_status_verified());
        let reuse = OperationContext::new(Instant::now() + Duration::from_secs(20));
        let second = reuse
            .scope(|| snapshot(&fixture.0, &json!({"prefix":"game"})))
            .unwrap()
            .unwrap();
        assert_eq!(second["reused"], true);
        assert_eq!(second["baseline_id"], first["baseline_id"]);
        assert!(!reuse.committed());
        assert!(reuse.commit_status_verified());
    }
    #[test]
    fn refused_context_preconditions_do_not_enter_a_commit_barrier() {
        let fixture = Fixture::new();
        let invalid = OperationContext::new(Instant::now() + Duration::from_secs(20));
        assert!(invalid
            .scope(|| snapshot(&fixture.0, &json!({"prefix":"../outside"})))
            .unwrap()
            .is_err());
        assert!(!invalid.committed());
        assert!(invalid.commit_status_verified());
        let missing = OperationContext::new(Instant::now() + Duration::from_secs(20));
        assert!(missing
            .scope(|| forget(&fixture.0, &json!({"baseline_id":"0".repeat(64)})))
            .unwrap()
            .is_err());
        assert!(!missing.committed());
        assert!(missing.commit_status_verified());
        assert!(!fixture.0.join(".aurum").exists());
    }
    #[test]
    fn successful_context_removal_marks_only_the_completed_mutation() {
        let fixture = Fixture::new();
        let baseline = fixture.baseline();
        let context = OperationContext::new(Instant::now() + Duration::from_secs(20));
        context
            .scope(|| forget(&fixture.0, &json!({"baseline_id":baseline})))
            .unwrap()
            .unwrap();
        assert!(context.committed());
        assert!(context.commit_status_verified());
        assert!(!fixture
            .0
            .join(".aurum/agent-context")
            .join(format!("{baseline}.json"))
            .exists());
        assert!(fixture.0.join("game/main.gd").exists());
    }
    #[cfg(windows)]
    #[test]
    fn failed_context_removal_keeps_commit_status_unconfirmed() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        let baseline = fixture.baseline();
        let path = fixture
            .0
            .join(".aurum/agent-context")
            .join(format!("{baseline}.json"));
        let locked = OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2)
            .open(&path)
            .unwrap();
        let context = OperationContext::new(Instant::now() + Duration::from_secs(20));
        assert!(context
            .scope(|| forget(&fixture.0, &json!({"baseline_id":baseline})))
            .unwrap()
            .is_err());
        assert!(!context.committed());
        assert!(!context.commit_status_verified());
        assert!(path.exists());
        drop(locked);
        assert_eq!(
            std::fs::read_to_string(fixture.0.join("game/main.gd")).unwrap(),
            "old\n"
        );
    }
}
