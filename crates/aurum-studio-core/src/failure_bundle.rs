//! Portable, bounded performance failures. Bundled project code is trusted-local
//! execution, never a sandbox, signature, checkpoint or determinism guarantee.
use crate::{performance_budget, snapshot, Project};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const VERSION: u32 = 1;
const FILE_BYTES: u64 = 2 * 1024 * 1024;
const SOURCE_BYTES: u64 = 8 * 1024 * 1024;
const BUNDLE_BYTES: u64 = 18 * 1024 * 1024;
const REPLAY_BYTES: u64 = 128 * 1024;
const MAX_FILES: usize = 256;
const MAX_DIRECTORIES: usize = 256;
const MAX_BUNDLES: usize = 4;
const MAX_REPLAYS: usize = 8;
const STORE: &str = ".aurum/failure-bundles";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Content {
    bytes: u64,
    sha256: String,
    hex: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    directories: Vec<String>,
    files: BTreeMap<String, Content>,
    snapshot_sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    aurum_version: String,
    aurum_executable_sha256: String,
    runtime_executable_sha256: String,
    os: String,
    arch: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Run {
    frames: u64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_fps"
    )]
    fixed_fps: Option<u64>,
    timeout_seconds: u64,
    scene: String,
    events: Value,
    performance: Value,
    seed: u32,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Predicate {
    kind: String,
    metric: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Diagnostics {
    exit_code: i32,
    timed_out: bool,
    source_still_current: bool,
    private_source_intact: bool,
    executable_disk_still_current: bool,
    private_workspace_cleanup_verified: bool,
    first_import_shutdown_retry: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    identity: Identity,
    source: Source,
    run: Run,
    predicate: Predicate,
    // Strict existing performance schema only: no stdout, paths, environment or
    // arbitrary user diagnostic strings enter the portable diagnostic record.
    performance_record: Value,
    diagnostics: Diagnostics,
    captures: Vec<Value>,
    checkpoint: Option<Value>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    schema_version: u32,
    payload_sha256: String,
    payload: Payload,
}

fn optional_fps<'de, D: serde::Deserializer<'de>>(value: D) -> Result<Option<u64>, D::Error> {
    // Omission alone represents the default real-time path; explicit null is not
    // an integer and must not silently become a second representation of it.
    u64::deserialize(value).map(Some)
}

pub fn supported(op: &str) -> bool {
    matches!(
        op,
        "failure_bundle_create" | "failure_bundle_inspect" | "failure_bundle_replay"
    )
}
fn check() -> Result<(), String> {
    crate::operation_context::OperationContext::current()
        .ok_or("failure bundle requires its original operation context")?
        .check()
        .map_err(|e| e.to_string())
}
fn finish<T>(
    original: &crate::operation_context::OperationContext,
    result: Result<T, String>,
) -> Result<T, String> {
    let value = result?;
    original.check().map_err(|e| e.to_string())?;
    Ok(value)
}
fn deadline() -> Result<Instant, String> {
    check()?;
    Ok(crate::operation_context::OperationContext::current()
        .ok_or("failure bundle context missing")?
        .deadline())
}
fn closed(value: &Value, keys: &[&str]) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or("failure bundle request must be an object")?;
    if object.keys().any(|key| !keys.contains(&key.as_str())) {
        return Err("unknown failure bundle request field".into());
    }
    Ok(())
}
fn encode<T: Serialize>(value: &T, cap: u64) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(value).map_err(|_| "bundle encoding failed")?;
    if bytes.len() as u64 > cap {
        return Err("failure bundle encoding limit exceeded".into());
    }
    Ok(bytes)
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn relative(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 240 || value.split('/').count() > 16 {
        return Err("failure bundle relative path bound".into());
    }
    for part in value.split('/') {
        let base = part.split('.').next().unwrap_or("").to_ascii_lowercase();
        if part.is_empty()
            || matches!(part, "." | "..")
            || part.ends_with('.')
            || !part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || matches!(base.as_str(), "con" | "prn" | "aux" | "nul")
            || (base.len() == 4
                && (base.starts_with("com") || base.starts_with("lpt"))
                && matches!(base.as_bytes()[3], b'1'..=b'9'))
        {
            return Err("noncanonical or reserved failure bundle path".into());
        }
    }
    Ok(())
}
fn source_path(path: &str) -> Result<(), String> {
    relative(path)?;
    if !snapshot::includes(path) || path.starts_with("addons/aurum_live/") {
        return Err("private or reserved source is unsupported in failure bundles".into());
    }
    let extension = path.rsplit('.').next().unwrap_or("");
    if !matches!(
        extension,
        "gd" | "tscn"
            | "tres"
            | "godot"
            | "toml"
            | "json"
            | "uid"
            | "cfg"
            | "txt"
            | "md"
            | "png"
            | "jpg"
            | "jpeg"
            | "svg"
            | "webp"
            | "wav"
            | "ogg"
            | "gdshader"
            | "gdshaderinc"
            | "gdignore"
    ) {
        return Err("unsupported source/asset format; native binaries and checkpoints are not portable in v1".into());
    }
    Ok(())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn decode(content: &Content) -> Result<Vec<u8>, String> {
    if content.bytes > FILE_BYTES
        || content.hex.len() as u64 != content.bytes * 2
        || !digest(&content.sha256)
    {
        return Err("bundle file length/hash bound".into());
    }
    let mut raw = Vec::with_capacity(content.bytes as usize);
    for pair in content.hex.as_bytes().as_chunks::<2>().0 {
        let digit = |b| match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err("noncanonical bundle hex"),
        };
        raw.push(digit(pair[0])? * 16 + digit(pair[1])?);
    }
    if crate::sha256_hex(&raw) != content.sha256 {
        return Err("bundle file integrity mismatch".into());
    }
    Ok(raw)
}
fn read(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    let mut file = snapshot::open_checked_file(path, deadline()?)?;
    let expected = file
        .metadata()
        .map_err(|_| "bundle file metadata unavailable")?
        .len();
    if expected > cap {
        return Err("bounded bundle read exceeded".into());
    }
    let mut bytes = Vec::with_capacity(expected as usize);
    let mut buffer = [0; 65536];
    loop {
        check()?;
        let n = file
            .read(&mut buffer)
            .map_err(|_| "bundle file read unavailable")?;
        if bytes.len() as u64 + n as u64 > expected {
            return Err("bundle file grew during read".into());
        }
        bytes.extend_from_slice(&buffer[..n]);
        check()?;
        if n == 0 {
            break;
        }
    }
    snapshot::check_opened_path(&file, path, deadline()?)?;
    if bytes.len() as u64 != expected {
        return Err("bundle file shrank during read".into());
    }
    Ok(bytes)
}
fn read_source_file(path: &Path, total: &mut u64) -> Result<Vec<u8>, String> {
    let remaining = SOURCE_BYTES
        .checked_sub(*total)
        .ok_or("bundle aggregate source byte bound")?;
    let raw = read(path, FILE_BYTES.min(remaining))?;
    *total = total
        .checked_add(raw.len() as u64)
        .filter(|n| *n <= SOURCE_BYTES)
        .ok_or("bundle aggregate source byte bound")?;
    Ok(raw)
}
fn image_hash(path: &Path) -> Result<String, String> {
    let mut file = snapshot::open_checked_file(path, deadline()?)?;
    let expected = file
        .metadata()
        .map_err(|_| "bundle tool metadata unavailable")?
        .len();
    if expected == 0 || expected > 512 * 1024 * 1024 {
        return Err("bundle tool image bound".into());
    }
    let mut hash = crate::Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0; 65536];
    loop {
        check()?;
        let n = file
            .read(&mut buffer)
            .map_err(|_| "bundle tool read unavailable")?;
        count += n as u64;
        if count > expected {
            return Err("bundle tool changed during read".into());
        }
        hash.update(&buffer[..n]);
        check()?;
        if n == 0 {
            break;
        }
    }
    snapshot::check_opened_path(&file, path, deadline()?)?;
    if count != expected {
        return Err("bundle tool changed during read".into());
    }
    Ok(hex(&hash.finalize()))
}
impl Run {
    fn validate(&self, predicate: &Predicate) -> Result<(), String> {
        if !(1..=3600).contains(&self.frames)
            || self.fixed_fps.is_some_and(|fps| !(1..=240).contains(&fps))
            || !(1..=90).contains(&self.timeout_seconds)
        {
            return Err("bundle play bounds exceeded".into());
        }
        source_path(&self.scene)?;
        if !self.scene.ends_with(".tscn") {
            return Err("bundle v1 requires an explicit text scene".into());
        }
        crate::playtest::validate_events(&self.events, self.frames)?;
        if self.events.as_array().is_none_or(|v| v.len() > 256) {
            return Err("bundle event count bound".into());
        }
        performance_budget::Request::parse(&self.performance, self.frames, self.timeout_seconds)?;
        let budgets = self.performance["budgets"]
            .as_object()
            .ok_or("bundle requires one performance budget")?;
        if predicate.kind != "performance_budget_exceeded"
            || budgets.len() != 1
            || !budgets.contains_key(&predicate.metric)
        {
            return Err("bundle v1 requires exactly its one declared performance predicate".into());
        }
        encode(self, 80 * 1024)?;
        Ok(())
    }
    fn input(&self) -> Value {
        let mut input = json!({"op":"play","frames":self.frames,
            "timeout_seconds":self.timeout_seconds,"scene":self.scene,"events":self.events,
            "performance":self.performance,"seed":self.seed,"rendered":false});
        if let Some(fps) = self.fixed_fps {
            input["fixed_fps"] = json!(fps);
        }
        input
    }
}
fn assessment(run: &Run, predicate: &Predicate, producer: &Value) -> Result<Value, String> {
    let request =
        performance_budget::Request::parse(&run.performance, run.frames, run.timeout_seconds)?;
    let id = producer["request_id"]
        .as_str()
        .ok_or("bundle performance identity missing")?;
    let value = performance_budget::evaluate(&request, id, producer)?;
    if value["measurement_complete"] != true
        || value["runtime"]["rendered"] != false
        || value["measurements"][&predicate.metric]["status"] != "available"
        || value["budgets"].as_array().is_none_or(|v| v.len() != 1)
        || !matches!(
            value["budgets"][0]["verdict"].as_str(),
            Some("exceeded" | "passed")
        )
    {
        return Err("unavailable or incomplete measurement cannot reproduce a failure".into());
    }
    Ok(value)
}
fn validate(payload: &Payload) -> Result<BTreeMap<String, Vec<u8>>, String> {
    if !payload.captures.is_empty() || payload.checkpoint.is_some() {
        return Err("capture/checkpoint compatibility is unsupported by bundle v1".into());
    }
    if payload.identity.aurum_version != env!("CARGO_PKG_VERSION")
        || payload.identity.os != std::env::consts::OS
        || payload.identity.arch != std::env::consts::ARCH
        || !digest(&payload.identity.aurum_executable_sha256)
        || !digest(&payload.identity.runtime_executable_sha256)
        || !digest(&payload.source.snapshot_sha256)
    {
        return Err("unsupported failure bundle build/platform identity".into());
    }
    payload.run.validate(&payload.predicate)?;
    let d = &payload.diagnostics;
    if d.exit_code != 0
        || d.timed_out
        || !d.source_still_current
        || !d.private_source_intact
        || !d.executable_disk_still_current
        || !d.private_workspace_cleanup_verified
    {
        return Err("recorded host checks do not support a completed failure".into());
    }
    let files = validate_source(&payload.source, &payload.run.scene)?;
    if assessment(
        &payload.run,
        &payload.predicate,
        &payload.performance_record,
    )?["budgets"][0]["verdict"]
        != "exceeded"
    {
        return Err(
            "recorded predicate was not reproduced in the original available measurement".into(),
        );
    }
    Ok(files)
}
fn validate_source(source: &Source, scene: &str) -> Result<BTreeMap<String, Vec<u8>>, String> {
    if source.files.is_empty()
        || source.files.len() > MAX_FILES
        || source.directories.len() > MAX_DIRECTORIES
    {
        return Err("bundle source entry bound".into());
    }
    let mut names = BTreeSet::new();
    let mut directories = BTreeSet::new();
    let mut last = "";
    for dir in &source.directories {
        relative(dir)?;
        if !snapshot::includes(dir)
            || dir == "addons/aurum_live"
            || dir.starts_with("addons/aurum_live/")
            || dir.as_str() <= last
            || !names.insert(dir.to_ascii_lowercase())
        {
            return Err("bundle directory order/alias/private path refused".into());
        }
        directories.insert(dir.as_str());
        last = dir;
    }
    let mut bytes = 0u64;
    let mut files = BTreeMap::new();
    for (path, content) in &source.files {
        source_path(path)?;
        if !names.insert(path.to_ascii_lowercase()) {
            return Err("bundle source alias refused".into());
        }
        bytes = bytes
            .checked_add(content.bytes)
            .filter(|n| *n <= SOURCE_BYTES)
            .ok_or("bundle aggregate source byte bound")?;
        files.insert(path.clone(), decode(content)?);
    }
    for path in source.directories.iter().chain(source.files.keys()) {
        if let Some((parent, _)) = path.rsplit_once('/') {
            if !directories.contains(parent) {
                return Err("bundle source parent missing".into());
            }
        }
    }
    if !files.contains_key("project.godot") || !files.contains_key(scene) {
        return Err("bundle project or selected scene is missing".into());
    }
    // Reconstruct the existing snapshot hash's depth-first, sorted-name format,
    // independently of any recorded success flag or filename lookup.
    fn fingerprint(
        prefix: &str,
        source: &Source,
        files: &BTreeMap<String, Vec<u8>>,
        hash: &mut crate::Sha256,
    ) {
        let mut children = BTreeMap::new();
        for dir in &source.directories {
            if let Some(leaf) = dir.strip_prefix(prefix).filter(|s| !s.contains('/')) {
                children.insert(leaf, true);
            }
        }
        for path in files.keys() {
            if let Some(leaf) = path.strip_prefix(prefix).filter(|s| !s.contains('/')) {
                children.insert(leaf, false);
            }
        }
        for (leaf, directory) in children {
            let path = format!("{prefix}{leaf}");
            if directory {
                fingerprint(&(path + "/"), source, files, hash);
            } else {
                hash.update(&(path.len() as u64).to_le_bytes());
                hash.update(path.as_bytes());
                hash.update(&(files[&path].len() as u64).to_le_bytes());
                hash.update(&files[&path]);
            }
        }
    }
    let mut hash = crate::Sha256::new();
    fingerprint("", source, &files, &mut hash);
    if hex(&hash.finalize()) != source.snapshot_sha256 {
        return Err("bundle source snapshot integrity mismatch".into());
    }
    Ok(files)
}
fn capture(root: &Path) -> Result<Source, String> {
    fn walk(
        root: &Path,
        dir: &Path,
        source: &mut Source,
        entries: &mut usize,
        total: &mut u64,
    ) -> Result<(), String> {
        check()?;
        let guard = snapshot::open_checked_directory(dir, deadline()?)?;
        let mut children = Vec::new();
        for entry in std::fs::read_dir(dir).map_err(|_| "bundle source directory unavailable")? {
            check()?;
            *entries += 1;
            if *entries > 1024 {
                return Err("bundle directory enumeration bound".into());
            }
            children.push(entry.map_err(|_| "bundle source entry unavailable")?);
        }
        children.sort_by_key(|e| e.file_name());
        for entry in children {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "bundle source escaped root")?
                .to_str()
                .ok_or("bundle requires UTF8 source paths")?
                .replace('\\', "/");
            if !snapshot::includes(&relative) {
                continue;
            }
            self::relative(&relative)?;
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|_| "bundle source metadata unavailable")?;
            if metadata.is_dir() {
                if source.directories.len() >= MAX_DIRECTORIES {
                    return Err("bundle directory count bound".into());
                }
                source.directories.push(relative);
                walk(root, &path, source, entries, total)?;
            } else {
                source_path(&relative)?;
                if source.files.len() >= MAX_FILES
                    || metadata.len() > FILE_BYTES
                    || *total + metadata.len() > SOURCE_BYTES
                {
                    return Err("bundle source file/byte bound".into());
                }
                let raw = read_source_file(&path, total)?;
                source.files.insert(
                    relative,
                    Content {
                        bytes: raw.len() as u64,
                        sha256: crate::sha256_hex(&raw),
                        hex: hex(&raw),
                    },
                );
            }
        }
        snapshot::check_opened_path(&guard, dir, deadline()?)?;
        Ok(())
    }
    // Apply the smaller bundle traversal/file limits before the existing full
    // fingerprint helper can read an oversized otherwise valid project.
    let mut source = Source {
        directories: Vec::new(),
        files: BTreeMap::new(),
        snapshot_sha256: String::new(),
    };
    walk(root, root, &mut source, &mut 0, &mut 0)?;
    source.directories.sort();
    source.snapshot_sha256 = snapshot::fingerprint_all_bounded(root, deadline()?)?.sha256;
    validate_source(&source, "project.godot")?;
    Ok(source)
}

struct Output {
    file: File,
    path: PathBuf,
    relative: String,
    _directory: File,
}
impl Output {
    fn reserve(root: &Path, replay: bool) -> Result<Self, String> {
        check()?;
        let dir = crate::files::confined(root, STORE)?;
        crate::diagnostics::checked_path(&dir).map_err(|_| "bundle output path unavailable")?;
        std::fs::create_dir_all(&dir).map_err(|_| "bundle output directory unavailable")?;
        #[cfg(windows)]
        let guard = snapshot::open_checked_writable_directory_identity(&dir, deadline()?)?;
        #[cfg(not(windows))]
        let guard = snapshot::open_checked_directory(&dir, deadline()?)?;
        let mut bundles = 0;
        let mut replays = 0;
        for entry in std::fs::read_dir(&dir).map_err(|_| "bundle retention unavailable")? {
            check()?;
            let entry = entry.map_err(|_| "bundle retention entry unavailable")?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "noncanonical bundle retention entry")?;
            let meta = std::fs::symlink_metadata(entry.path())
                .map_err(|_| "bundle retention metadata unavailable")?;
            let kind = if valid_record_name(&name, "bundle-") {
                bundles += 1;
                BUNDLE_BYTES
            } else if valid_record_name(&name, "replay-") {
                replays += 1;
                REPLAY_BYTES
            } else {
                return Err("unknown bundle retention entry; manual inspection required".into());
            };
            if !meta.is_file()
                || meta.file_type().is_symlink()
                || meta.len() > kind
                || bundles > MAX_BUNDLES
                || replays > MAX_REPLAYS
            {
                return Err("bundle retention bounds exceeded".into());
            }
            let held = snapshot::open_checked_file(&entry.path(), deadline()?)?;
            if held
                .metadata()
                .map_err(|_| "bundle retention file unavailable")?
                .len()
                != meta.len()
            {
                return Err("bundle retention changed".into());
            }
        }
        if (!replay && bundles >= MAX_BUNDLES) || (replay && replays >= MAX_REPLAYS) {
            return Err(
                "failure bundle retention full; explicitly remove unwanted records first".into(),
            );
        }
        let relative = format!(
            "{STORE}/{}{}.json",
            if replay { "replay-" } else { "bundle-" },
            crate::random::session_id()
        );
        let path = crate::files::confined(root, &relative)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| "fresh bundle record unavailable")?;
        // Empty/partial records are deliberately retained on every later failure.
        Ok(Self {
            file,
            path,
            relative,
            _directory: guard,
        })
    }
    fn finish(&mut self, bytes: &[u8], cap: u64) -> Result<Value, String> {
        if bytes.len() as u64 > cap {
            return Err("bundle record bound".into());
        }
        for chunk in bytes.chunks(65536) {
            check()?;
            self.file
                .write_all(chunk)
                .map_err(|_| "bundle record write failed")?;
        }
        self.file
            .sync_all()
            .map_err(|_| "bundle record flush failed")?;
        check()?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| "bundle record seek failed")?;
        let mut hash = crate::Sha256::new();
        let mut count = 0;
        let mut buffer = [0; 65536];
        loop {
            check()?;
            let n = self
                .file
                .read(&mut buffer)
                .map_err(|_| "bundle record readback failed")?;
            count += n;
            if count > bytes.len() {
                return Err("bundle record grew".into());
            }
            hash.update(&buffer[..n]);
            if n == 0 {
                break;
            }
        }
        snapshot::check_opened_path(&self.file, &self.path, deadline()?)?;
        if count != bytes.len() || hex(&hash.finalize()) != crate::sha256_hex(bytes) {
            return Err("bundle record readback mismatch".into());
        }
        check()?;
        Ok(json!({"path":self.relative,"bytes":bytes.len(),"sha256":crate::sha256_hex(bytes)}))
    }
}
fn valid_record_name(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .and_then(|s| s.strip_suffix(".json"))
        .is_some_and(|id| {
            id.len() == 32
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}
fn load(root: &Path, path: &str) -> Result<Bundle, String> {
    relative(path)?;
    let leaf = path
        .strip_prefix(&format!("{STORE}/"))
        .ok_or("bundle must be in the selected project's failure-bundles directory")?;
    if !valid_record_name(leaf, "bundle-") {
        return Err("noncanonical bundle record name".into());
    }
    let raw = read(&crate::files::confined(root, path)?, BUNDLE_BYTES)?;
    let bundle: Bundle =
        serde_json::from_slice(&raw).map_err(|_| "invalid/unsupported failure bundle schema")?;
    if bundle.schema_version != VERSION
        || !digest(&bundle.payload_sha256)
        || encode(&bundle, BUNDLE_BYTES)? != raw
        || crate::sha256_hex(&encode(&bundle.payload, BUNDLE_BYTES)?) != bundle.payload_sha256
    {
        return Err("noncanonical, corrupt or unsupported failure bundle".into());
    }
    validate(&bundle.payload)?;
    Ok(bundle)
}
struct Observation {
    record: Value,
    diagnostics: Diagnostics,
}
fn run_evidence(
    project: &Project,
    run: &Run,
    predicate: &Predicate,
    expected_source: &str,
    runtime_hash: &str,
) -> Result<Observation, String> {
    let result =
        crate::playtest::run(project, &run.input(), run.frames, run.fixed_fps, Vec::new())?;
    check()?;
    if result["exit_code"] != 0
        || result["timed_out"] != false
        || result["errors"].as_array().is_none_or(|v| !v.is_empty())
        || result["runtime"]["errors"]
            .as_array()
            .is_none_or(|v| !v.is_empty())
        || result["runtime"]["seed"] != run.seed
        || result["runtime"]["seed_applied"] != true
        || result["private_workspace_cleanup_verified"] != true
        || result["performance_identity"]["working_source_sha256"] != expected_source
        || result["performance_identity"]["executable_disk_sha256"] != runtime_hash
        || [
            "source_still_current",
            "private_source_intact",
            "executable_disk_still_current",
        ]
        .iter()
        .any(|key| result["performance_identity"][key] != true)
    {
        return Err("bundle requires complete clean play/source/runtime/seed evidence".into());
    }
    let dir = result["evidence_directory"]
        .as_str()
        .ok_or("play evidence location missing")?;
    let producer: Value = serde_json::from_slice(&read(
        &Path::new(dir).join("performance.json"),
        performance_budget::RECEIPT_BYTES as u64,
    )?)
    .map_err(|_| "bounded performance evidence unavailable")?;
    let assessed = assessment(run, predicate, &producer)?;
    if assessed != result["performance"] {
        return Err("performance evidence changed after host assessment".into());
    }
    let flag = |key: &str| {
        result["performance_identity"][key]
            .as_bool()
            .ok_or("host identity flag missing")
    };
    Ok(Observation {
        record: producer,
        diagnostics: Diagnostics {
            exit_code: result["exit_code"]
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .ok_or("host exit code missing")?,
            timed_out: result["timed_out"]
                .as_bool()
                .ok_or("host timeout flag missing")?,
            source_still_current: flag("source_still_current")?,
            private_source_intact: flag("private_source_intact")?,
            executable_disk_still_current: flag("executable_disk_still_current")?,
            private_workspace_cleanup_verified: result["private_workspace_cleanup_verified"]
                .as_bool()
                .ok_or("host cleanup flag missing")?,
            first_import_shutdown_retry: result["first_import_shutdown_retry"]
                .as_bool()
                .ok_or("host import diagnostic missing")?,
        },
    })
}
fn create(root: &Path, input: &Value) -> Result<Value, String> {
    closed(input, &["op", "request", "metric", "seed"])?;
    closed(
        &input["request"],
        &[
            "op",
            "frames",
            "fixed_fps",
            "timeout_seconds",
            "scene",
            "events",
            "performance",
        ],
    )?;
    if input["request"]["op"] != "play" {
        return Err("bundle v1 requires an explicit play request".into());
    }
    let mut request = input["request"].clone();
    request
        .as_object_mut()
        .ok_or("bundle play request missing")?
        .remove("op");
    request["seed"] = input["seed"].clone();
    let run: Run = serde_json::from_value(request)
        .map_err(|_| "closed bundle request fields/types missing or invalid")?;
    let predicate = Predicate {
        kind: "performance_budget_exceeded".into(),
        metric: input["metric"]
            .as_str()
            .ok_or("bundle metric required")?
            .into(),
    };
    run.validate(&predicate)?;
    let project =
        Project::open_preflight(root, deadline()?).map_err(|_| "bundle project unavailable")?;
    if project.config.rust_package.is_some() {
        return Err("native project bundles are unsupported in v1".into());
    }
    let source_root = project
        .godot_project_dir()
        .ok_or("bundle Godot project missing")?;
    let _content = crate::content_lock::ContentLock::exclusive(&project.root)?;
    crate::transactions::ensure_writable(&project.root)?;
    let source = capture(source_root)?;
    validate_source(&source, &run.scene)?;
    let engine = crate::project_ops::engine_binary(&project)?;
    let aurum = std::env::current_exe().map_err(|_| "current Aurum image unavailable")?;
    let identity = Identity {
        aurum_version: env!("CARGO_PKG_VERSION").into(),
        aurum_executable_sha256: image_hash(&aurum)?,
        runtime_executable_sha256: image_hash(&engine)?,
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
    };
    let mut output = Output::reserve(&project.root, false)?;
    let observed = run_evidence(
        &project,
        &run,
        &predicate,
        &source.snapshot_sha256,
        &identity.runtime_executable_sha256,
    )?;
    if snapshot::fingerprint_all_bounded(source_root, deadline()?)?.sha256 != source.snapshot_sha256
        || image_hash(&aurum)? != identity.aurum_executable_sha256
        || image_hash(&engine)? != identity.runtime_executable_sha256
    {
        return Err("bundle source/tool identity drift".into());
    }
    let payload = Payload {
        identity,
        source,
        run,
        predicate,
        performance_record: observed.record,
        diagnostics: observed.diagnostics,
        captures: Vec::new(),
        checkpoint: None,
    };
    validate(&payload)?;
    let payload_sha256 = crate::sha256_hex(&encode(&payload, BUNDLE_BYTES)?);
    let bundle = Bundle {
        schema_version: VERSION,
        payload_sha256,
        payload,
    };
    let artifact = output.finish(&encode(&bundle, BUNDLE_BYTES)?, BUNDLE_BYTES)?;
    Ok(
        json!({"ok":true,"bundle":artifact,"predicate":"performance_budget_exceeded","seed":bundle.payload.run.seed,
        "sanitized_diagnostics":"strict performance record only; no logs, absolute paths or environment",
        "portable_integrity_only":true,"reproduced_in_fresh_workspace":false,"execution_sandboxed":false}),
    )
}
fn replay(root: &Path, input: &Value) -> Result<Value, String> {
    closed(input, &["op", "path", "allow_project_code"])?;
    if input["allow_project_code"] != true {
        return Err("replay requires explicit allow_project_code=true; project code runs as current user, not in a sandbox".into());
    }
    let bundle = load(root, input["path"].as_str().ok_or("bundle path required")?)?;
    let payload = &bundle.payload;
    let aurum = std::env::current_exe().map_err(|_| "current Aurum image unavailable")?;
    if image_hash(&aurum)? != payload.identity.aurum_executable_sha256 {
        return Err("bundle requires the exact producing Aurum executable".into());
    }
    Project::open_preflight(root, deadline()?).map_err(|_| "replay record project unavailable")?;
    let _content = crate::content_lock::ContentLock::exclusive(root)?;
    crate::transactions::ensure_writable(root)?;
    let mut output = Output::reserve(root, true)?;
    let files = validate(payload)?;
    let mut workspace = crate::candidate_workspace::Workspace::create()?;
    let private = workspace.path.join("project");
    std::fs::create_dir(&private).map_err(|_| "fresh replay project unavailable")?;
    let mut config = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(private.join("aurum.toml"))
        .map_err(|_| "fresh replay configuration unavailable")?;
    config
        .write_all(b"schema_version=1\nname=\"Failure bundle replay\"\ngodot_version=\"4.7\"\n")
        .and_then(|_| config.sync_all())
        .map_err(|_| "replay configuration write failed")?;
    drop(config);
    let source = private.join("godot");
    std::fs::create_dir(&source).map_err(|_| "fresh replay source unavailable")?;
    for dir in &payload.source.directories {
        check()?;
        std::fs::create_dir(source.join(dir)).map_err(|_| "fresh replay directory unavailable")?;
    }
    for (path, raw) in files {
        check()?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(source.join(path))
            .map_err(|_| "fresh replay file unavailable")?;
        file.write_all(&raw)
            .and_then(|_| file.sync_all())
            .map_err(|_| "replay file write failed")?;
    }
    if snapshot::fingerprint_all_bounded(&source, deadline()?)?.sha256
        != payload.source.snapshot_sha256
    {
        return Err("materialized replay source differs from bundle".into());
    }
    let project =
        Project::open_preflight(&private, deadline()?).map_err(|_| "replay project unavailable")?;
    let engine = crate::project_ops::engine_binary(&project)?;
    if image_hash(&engine)? != payload.identity.runtime_executable_sha256 {
        return Err(
            "bundle runtime missing or mismatched; configure the exact runtime before replay"
                .into(),
        );
    }
    let observed = run_evidence(
        &project,
        &payload.run,
        &payload.predicate,
        &payload.source.snapshot_sha256,
        &payload.identity.runtime_executable_sha256,
    );
    let tools_equal = image_hash(&aurum)? == payload.identity.aurum_executable_sha256
        && image_hash(&engine)? == payload.identity.runtime_executable_sha256;
    let cleaned = workspace.clean().is_ok();
    let (assessment, record, diagnostics) = match observed {
        Ok(observed) => (
            Some(assessment(
                &payload.run,
                &payload.predicate,
                &observed.record,
            )?),
            Some(observed.record),
            Some(observed.diagnostics),
        ),
        Err(_) => (None, None, None),
    };
    let available = assessment.is_some() && tools_equal && cleaned;
    let reproduced = available
        && assessment
            .as_ref()
            .is_some_and(|v| v["budgets"][0]["verdict"] == "exceeded");
    let diagnostic_reason = if !tools_equal {
        "tool_identity_drift"
    } else if !cleaned {
        "workspace_cleanup_unconfirmed"
    } else if assessment.is_none() {
        "play_or_evidence_unavailable"
    } else {
        "complete_available_measurement"
    };
    let result = json!({"schema_version":1,"ok":reproduced,"status":if !available{"UNAVAILABLE"}else if reproduced{"REPRODUCED"}else{"NOT_REPRODUCED"},
        "bundle_payload_sha256":bundle.payload_sha256,"seed":payload.run.seed,"seed_scope":"Godot global RNG before selected main scene load; custom/project RNG and autoload work are not controlled",
        "predicate":payload.predicate,"assessment":assessment,"performance_record":record,"diagnostics":diagnostics,"tools_unchanged":tools_equal,
        "fresh_workspace_cleanup_verified":cleaned,"execution_sandboxed":false,"determinism_guaranteed":false,
        "diagnostic_reason":diagnostic_reason,
        "runtime_pid_independently_attributed":false,"mapped_memory_hash_verified":false,"logs_included":false});
    let artifact = output.finish(&encode(&result, REPLAY_BYTES)?, REPLAY_BYTES)?;
    Ok(json!({"ok":reproduced,"result":result,"evidence":artifact}))
}
pub fn execute(root: &Path, input: &Value) -> Result<Value, String> {
    let work = || -> Result<Value, String> {
        let original = crate::operation_context::OperationContext::current()
            .ok_or("failure bundle requires its original operation context")?;
        check()?;
        let result = match input["op"].as_str() {
            Some("failure_bundle_inspect") => {
                closed(input, &["op", "path"])?;
                let bundle = load(root, input["path"].as_str().ok_or("bundle path required")?)?;
                Ok(
                    json!({"ok":true,"schema_version":VERSION,"payload_sha256":bundle.payload_sha256,"identity":bundle.payload.identity,
                    "source_files":bundle.payload.source.files.len(),"source_sha256":bundle.payload.source.snapshot_sha256,
                    "request":bundle.payload.run,"predicate":bundle.payload.predicate,"diagnostics":bundle.payload.diagnostics,"recorded_integrity_valid":true,
                    "runtime_availability_checked":false,"exact_current_Aurum_image_checked":false,"reproduction_verified":false,"project_execution":false,
                    "captures_supported":false,"checkpoints_supported":false,"execution_sandboxed":false}),
                )
            }
            Some("failure_bundle_create" | "failure_bundle_replay") if !cfg!(windows) => {
                Err("bundle execution is Windows-qualified only; inspect remains read-only".into())
            }
            Some("failure_bundle_create") => create(root, input),
            Some("failure_bundle_replay") => replay(root, input),
            _ => Err("unsupported failure bundle operation".into()),
        };
        finish(&original, result)
    };
    if crate::operation_context::OperationContext::current().is_some() {
        work()
    } else {
        crate::operation_context::OperationContext::new(Instant::now() + Duration::from_secs(120))
            .scope(work)
            .map_err(|e| e.to_string())?
    }
}

pub fn input_schema(op: &str) -> Value {
    let mut properties = json!({"op":{"const":op}});
    let required = match op {
        "failure_bundle_create" => {
            properties["request"] = json!({"type":"object","additionalProperties":false,"required":["op","frames","timeout_seconds","scene","events","performance"],"properties":{
                "op":{"const":"play"},
                "frames":{"type":"integer","minimum":1,"maximum":3600},"fixed_fps":{"type":"integer","minimum":1,"maximum":240},
                "timeout_seconds":{"type":"integer","minimum":1,"maximum":90},"scene":{"type":"string","maxLength":240},
                "events":{"type":"array","maxItems":256},"performance":performance_budget::input_schema()}});
            properties["metric"] = json!({"type":"string","description":"Exactly one performance budget metric; only a completed available exceeded measurement can be exported"});
            properties["seed"] = json!({"type":"integer","minimum":0,"maximum":u32::MAX});
            vec!["op", "request", "metric", "seed"]
        }
        "failure_bundle_replay" => {
            properties["path"] = json!({"type":"string","maxLength":240});
            properties["allow_project_code"] = json!({"const":true,"description":"Explicit current-user execution; not sandboxed. Inspect first."});
            vec!["op", "path", "allow_project_code"]
        }
        _ => {
            properties["path"] = json!({"type":"string","maxLength":240});
            vec!["op", "path"]
        }
    };
    json!({"type":"object","additionalProperties":false,"required":required,"properties":properties})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> Run {
        Run {
            frames: 300,
            fixed_fps: None,
            timeout_seconds: 90,
            scene: "main.tscn".into(),
            events: json!([]),
            seed: 17,
            performance: json!({"warmup_frames":120,"sample_frames":120,"sample_stride":1,
                "min_warmup_ms":1100,"wall_budget_ms":10000,"budgets":{"node_count":32}}),
        }
    }
    fn content(raw: &[u8]) -> Content {
        Content {
            bytes: raw.len() as u64,
            sha256: crate::sha256_hex(raw),
            hex: hex(raw),
        }
    }
    #[test]
    fn bundle_paths_refuse_escape_alias_and_native_payloads() {
        for path in [
            "../main.gd",
            "/main.gd",
            "C:/main.gd",
            "a\\b.gd",
            "a//b.gd",
            "a/./b.gd",
            "main.gd:stream",
            "NUL.gd",
            "com1.png",
            "a/trailing.",
            "a/space .gd",
            "a/é.gd",
        ] {
            assert!(relative(path).is_err(), "{path}");
        }
        for path in [
            ".env",
            ".aurum/state.json",
            "game.dll",
            "plugin.gdextension",
            "save.bin",
            "addons/aurum_live/main.gd",
        ] {
            assert!(source_path(path).is_err(), "{path}");
        }
        assert!(source_path("stage/actor.gd").is_ok());
        assert!(source_path("images/sprite.png").is_ok());
    }
    #[test]
    fn bundle_content_refuses_corruption_and_oversize_before_replay() {
        let mut entry = content(b"abc");
        assert_eq!(decode(&entry).unwrap(), b"abc");
        entry.hex = "616264".into();
        assert_eq!(
            decode(&entry).unwrap_err(),
            "bundle file integrity mismatch"
        );
        entry = content(b"abc");
        entry.bytes = FILE_BYTES + 1;
        assert_eq!(decode(&entry).unwrap_err(), "bundle file length/hash bound");
        entry = content(b"abc");
        entry.hex = "6A6263".into();
        assert_eq!(decode(&entry).unwrap_err(), "noncanonical bundle hex");
    }
    #[test]
    fn bundle_request_requires_bounded_single_metric_and_typed_seed() {
        let predicate = Predicate {
            kind: "performance_budget_exceeded".into(),
            metric: "node_count".into(),
        };
        let mut request = run();
        assert!(request.validate(&predicate).is_ok());
        assert!(request.input().get("fixed_fps").is_none());
        let canonical = serde_json::to_value(&request).unwrap();
        assert!(canonical.get("fixed_fps").is_none());
        assert_eq!(
            serde_json::from_value::<Run>(canonical.clone())
                .unwrap()
                .fixed_fps,
            None
        );
        for fps in [1, 240] {
            request.fixed_fps = Some(fps);
            assert!(request.validate(&predicate).is_ok());
            assert_eq!(request.input()["fixed_fps"], fps);
            assert_eq!(
                serde_json::from_value::<Run>(serde_json::to_value(&request).unwrap())
                    .unwrap()
                    .fixed_fps,
                Some(fps)
            );
        }
        for fps in [0, 241] {
            request.fixed_fps = Some(fps);
            assert!(request.validate(&predicate).is_err());
        }
        for fps in [Value::Null, json!(true), json!(-1), json!(1.5)] {
            let mut value = canonical.clone();
            value["fixed_fps"] = fps;
            assert!(serde_json::from_value::<Run>(value).is_err());
        }
        request.fixed_fps = None;
        request.performance["budgets"]["resource_count"] = json!(1);
        assert_eq!(
            request.validate(&predicate).unwrap_err(),
            "bundle v1 requires exactly its one declared performance predicate"
        );
        let mut value = serde_json::to_value(run()).unwrap();
        value["seed"] = json!(4294967296u64);
        assert!(serde_json::from_value::<Run>(value).is_err());
        let mut value = serde_json::to_value(run()).unwrap();
        value["command"] = json!("anything");
        assert!(serde_json::from_value::<Run>(value).is_err());
    }
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn bundle_source_matches_existing_snapshot_and_refuses_alias_drift_missing_file() {
        let root = std::env::temp_dir().join(format!(
            "aurum-bundle-schema-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("stage")).unwrap();
        std::fs::create_dir(root.join("empty")).unwrap();
        std::fs::write(root.join("project.godot"), b"config_version=5\n").unwrap();
        std::fs::write(
            root.join("main.tscn"),
            b"[gd_scene format=3]\n[node name=\"Main\" type=\"Node\"]\n",
        )
        .unwrap();
        std::fs::write(root.join("stage/actor.gd"), b"extends Node\n").unwrap();
        crate::operation_context::OperationContext::new(Instant::now() + Duration::from_secs(10))
            .scope(|| {
                let source = capture(&root).unwrap();
                let mut almost_full = SOURCE_BYTES - 5;
                assert_eq!(
                    read_source_file(&root.join("stage/actor.gd"), &mut almost_full).unwrap_err(),
                    "bounded bundle read exceeded"
                );
                assert_eq!(almost_full, SOURCE_BYTES - 5);
                assert_eq!(validate_source(&source, "main.tscn").unwrap().len(), 3);
                assert!(source.directories.contains(&"empty".to_string()));
                let clone = || {
                    serde_json::from_value::<Source>(serde_json::to_value(&source).unwrap())
                        .unwrap()
                };
                let mut changed = clone();
                changed
                    .files
                    .insert("stage/actor.gd".into(), content(b"extends Node2D\n"));
                assert_eq!(
                    validate_source(&changed, "main.tscn").unwrap_err(),
                    "bundle source snapshot integrity mismatch"
                );
                let mut alias = clone();
                alias.files.insert("MAIN.tscn".into(), content(b"alias"));
                assert_eq!(
                    validate_source(&alias, "main.tscn").unwrap_err(),
                    "bundle source alias refused"
                );
                let mut missing = clone();
                missing.files.remove("main.tscn");
                assert_eq!(
                    validate_source(&missing, "main.tscn").unwrap_err(),
                    "bundle project or selected scene is missing"
                );
                let mut parent = clone();
                parent.directories.retain(|p| p != "stage");
                assert_eq!(
                    validate_source(&parent, "main.tscn").unwrap_err(),
                    "bundle source parent missing"
                );
            })
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn bundle_contract_keeps_inspection_read_only_and_replay_explicit() {
        assert!(crate::project_ops::is_read_only("failure_bundle_inspect"));
        for op in ["failure_bundle_create", "failure_bundle_replay"] {
            assert!(!crate::project_ops::is_read_only(op));
            let contract = crate::project_contract::describe(Some(op)).unwrap();
            assert_eq!(contract["input_schema"]["additionalProperties"], false);
        }
        assert_eq!(
            input_schema("failure_bundle_replay")["properties"]["allow_project_code"]["const"],
            true
        );
        assert_eq!(
            input_schema("failure_bundle_create")["properties"]["seed"]["maximum"],
            u32::MAX
        );
        assert!(
            !input_schema("failure_bundle_create")["properties"]["request"]["required"]
                .as_array()
                .unwrap()
                .contains(&json!("fixed_fps"))
        );
        let original = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(60),
        );
        original
            .scope(|| {
                assert_eq!(finish(&original, Ok(7u8)).unwrap(), 7);
                original.request_cancel().unwrap();
                assert!(finish(&original, Ok(7u8)).is_err());
                assert_eq!(
                    finish::<u8>(&original, Err("first validation refusal".into())).unwrap_err(),
                    "first validation refusal"
                );
            })
            .unwrap();
        let expired = crate::operation_context::OperationContext::new(Instant::now());
        assert!(finish(&expired, Ok(7u8)).is_err());
    }
    #[test]
    fn play_seed_is_optional_and_u32_closed() {
        assert_eq!(crate::playtest::seed(&json!({})).unwrap(), None);
        assert_eq!(crate::playtest::seed(&json!({"seed":0})).unwrap(), Some(0));
        assert_eq!(
            crate::playtest::seed(&json!({"seed":u32::MAX})).unwrap(),
            Some(u32::MAX)
        );
        for seed in [
            json!(-1),
            json!(true),
            json!(1.5),
            json!(4294967296u64),
            Value::Null,
        ] {
            assert!(crate::playtest::seed(&json!({"seed":seed})).is_err());
        }
    }
}
