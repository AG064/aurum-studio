//! Read-only evidence from an already accepted, owned game.
//! Controller integration must retain its operation lock throughout inspection.
use crate::hash::Sha256;
use crate::ownership::{LiveProcess, OwnershipRecord, ProcessKind};
use crate::runtime_channel::Channel;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

pub const BUDGET_MS: u64 = 10_000;
pub const REPORT_BYTES: usize = 64 * 1024;
pub const SOURCE_BYTES: u64 = 512 * 1024 * 1024;
pub const SOURCE_FILES: u64 = 20_000;
pub const SOURCE_ENTRIES: u64 = 40_000;
pub const PATH_BYTES: usize = 1024;
pub const MANIFESTS: usize = 16;
pub const DEPENDENCIES: usize = 128;
const PLAN_BYTES: usize = 256 * 1024;
const BINARY_BYTES: u64 = 128 * 1024 * 1024;
const EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const MANIFEST_BYTES: u64 = 64 * 1024;

pub(crate) fn native_plan_fits(value: &Value) -> bool {
    fits(value, PLAN_BYTES)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    op: Option<String>,
    #[serde(default)]
    resources: Vec<String>,
}

/// Private host evidence. Deliberately neither Debug nor Serialize.
pub struct Preparation {
    pub owner: OwnershipRecord,
    pub private_source: PathBuf,
    pub source_sha256: String,
    pub prepared_sha256: String,
    pub native_plan: Value,
    pub executable_sha256: Option<String>,
}

/// The implementation must only dispatch fixed, authenticated read-only requests.
pub trait RuntimeQuery {
    fn request(&self, input: &Value, budget: Duration) -> Result<Value, String>;
}
impl RuntimeQuery for Channel {
    fn request(&self, input: &Value, budget: Duration) -> Result<Value, String> {
        Channel::request(self, input, budget)
    }
}
impl<F> RuntimeQuery for F
where
    F: Fn(&Value, Duration) -> Result<Value, String>,
{
    fn request(&self, input: &Value, budget: Duration) -> Result<Value, String> {
        self(input, budget)
    }
}

/// Ready is only constructed while the controller's preview operation lock is held.
pub enum Session<'a> {
    Unavailable,
    Busy,
    Ready {
        preparation: &'a Preparation,
        query: &'a dyn RuntimeQuery,
    },
}

#[derive(Clone)]
pub struct Fingerprint {
    pub sha256: String,
    pub files: u64,
    pub bytes: u64,
}

/// Adapter for a bounded snapshot-all helper, not a command or Cargo discovery.
/// Preserve snapshot::includes and recursive sorted hash ordering. Enforce these
/// limits before allocation/read, reject links/reparse points and key files,
/// check the passed deadline and OperationContext between entries/read chunks.
pub trait SourceFingerprint {
    fn fingerprint(
        &self,
        root: &Path,
        deadline: Instant,
        limits: FingerprintLimits,
    ) -> Result<Fingerprint, String>;
}

#[derive(Clone, Copy)]
pub struct FingerprintLimits {
    pub bytes: u64,
    pub files: u64,
    pub entries: u64,
    pub path_bytes: usize,
    pub depth: usize,
}
impl Default for FingerprintLimits {
    fn default() -> Self {
        Self {
            bytes: SOURCE_BYTES,
            files: SOURCE_FILES,
            entries: SOURCE_ENTRIES,
            path_bytes: PATH_BYTES,
            depth: 24,
        }
    }
}

pub struct BoundedSnapshots;
impl SourceFingerprint for BoundedSnapshots {
    fn fingerprint(
        &self,
        root: &Path,
        deadline: Instant,
        limits: FingerprintLimits,
    ) -> Result<Fingerprint, String> {
        if limits.bytes != SOURCE_BYTES
            || limits.files != SOURCE_FILES
            || limits.entries != SOURCE_ENTRIES
            || limits.path_bytes != PATH_BYTES
            || limits.depth != 24
        {
            return Err("unsupported bounded snapshot limits".into());
        }
        let result = crate::snapshot::fingerprint_all_bounded(root, deadline)?;
        Ok(Fingerprint {
            sha256: result.sha256,
            files: result.files,
            bytes: result.bytes,
        })
    }
}

#[derive(Clone, Copy)]
enum Failure {
    Cancelled,
    Deadline,
    Evidence(&'static str),
}
struct Budget {
    deadline: Instant,
}
impl Budget {
    fn new() -> Self {
        let local = Instant::now() + Duration::from_millis(BUDGET_MS);
        Self {
            deadline: crate::operation_context::OperationContext::current()
                .map_or(local, |context| local.min(context.deadline())),
        }
    }
    fn check(&self) -> Result<(), Failure> {
        if let Some(context) = crate::operation_context::OperationContext::current() {
            if let Err(error) = context.check() {
                return Err(if error.kind() == std::io::ErrorKind::TimedOut {
                    Failure::Deadline
                } else {
                    Failure::Cancelled
                });
            }
        }
        if Instant::now() >= self.deadline {
            return Err(Failure::Deadline);
        }
        Ok(())
    }
    fn remaining(&self) -> Result<Duration, Failure> {
        self.check()?;
        Ok(self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(5)))
    }
}

struct ByteLimit {
    bytes: usize,
    limit: usize,
}
impl Write for ByteLimit {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes) {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn fits(value: &Value, limit: usize) -> bool {
    serde_json::to_writer(&mut ByteLimit { bytes: 0, limit }, value).is_ok()
}

fn requested_resources(
    input: &Value,
    request: &Request,
) -> Result<Option<BTreeSet<String>>, String> {
    if input.get("resources").is_none() {
        return Ok(None);
    }
    if request.resources.is_empty() || request.resources.len() > MANIFESTS {
        return Err("runtime context resources requires 1..16 unique resource names".into());
    }
    let mut resources = BTreeSet::new();
    for resource in &request.resources {
        let path = resource
            .strip_prefix("res://")
            .filter(|path| path.ends_with(".gdextension"))
            .ok_or("runtime context resources requires res://*.gdextension names")?;
        relative(path).map_err(|_| "invalid runtime context resource name")?;
        if !resources.insert(resource.clone()) {
            return Err("duplicate runtime context resource name".into());
        }
    }
    Ok(Some(resources))
}

pub(crate) fn operation_request(input: &Value) -> Result<Value, String> {
    let mut request = input.clone();
    if let Some(object) = request.as_object_mut() {
        object.remove("project");
    }
    if !fits(&request, 1024) {
        return Err("runtime context request exceeds 1 KiB".into());
    }
    let parsed: Request =
        serde_json::from_value(request.clone()).map_err(|_| "invalid runtime context request")?;
    if parsed.op.as_deref() != Some("runtime_context") {
        return Err("runtime context requires its operation name".into());
    }
    requested_resources(&request, &parsed)?;
    Ok(request)
}

#[cfg(windows)]
pub(crate) fn file_sha256_bounded(
    path: &Path,
    limit: u64,
    deadline: Instant,
) -> Result<String, String> {
    let deadline = crate::operation_context::OperationContext::current()
        .map_or(deadline, |context| deadline.min(context.deadline()));
    disk_hash(path, limit, &Budget { deadline }).map_err(|failure| {
        match failure {
            Failure::Cancelled => "bounded file hashing cancelled",
            Failure::Deadline => "bounded file hashing deadline expired",
            Failure::Evidence(code) => code,
        }
        .to_string()
    })
}

#[cfg(windows)]
pub(crate) fn dependency_capture_request(plan: &Value) -> Option<Value> {
    let rows = plan["manifests"]
        .as_array()
        .filter(|rows| rows.len() <= MANIFESTS)?;
    let mut resources = Vec::new();
    let mut features = BTreeSet::new();
    for row in rows {
        let resource = row["resource"].as_str()?;
        relative(resource.strip_prefix("res://")?).ok()?;
        resources.push(resource);
        for tag in row["feature_tags"].as_array()? {
            let tag = tag.as_str()?;
            if !feature_identifier(tag) {
                return None;
            }
            features.insert(tag);
            if features.len() > 128 {
                return None;
            }
        }
    }
    Some(json!({"op":"context_dependencies","resources":resources,"features":features}))
}
#[cfg(windows)]
pub(crate) fn record_dependency_capture(plan: &mut Value, request: &Value, reply: &Value) {
    let Some(resources) = request["resources"].as_array().and_then(|items| {
        items
            .iter()
            .map(|item| item.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
    }) else {
        return;
    };
    let Some(features) = request["features"].as_array().and_then(|items| {
        items
            .iter()
            .map(|item| item.as_str().map(|tag| (tag.to_string(), false)))
            .collect::<Option<BTreeMap<_, _>>>()
    }) else {
        return;
    };
    if let Ok(actual) = context_dependencies(reply, &resources, &features) {
        plan["runtime_features"] = json!(actual.features);
        plan["runtime_architecture"] = json!(actual.architecture);
        plan["runtime_debug"] = json!(actual.debug);
    }
}
fn feature_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}
#[derive(PartialEq, Eq)]
struct ContextDependencies {
    loaded: BTreeMap<String, bool>,
    features: BTreeMap<String, bool>,
    architecture: String,
    debug: bool,
}
fn context_dependencies(
    value: &Value,
    resources: &[String],
    features: &BTreeMap<String, bool>,
) -> Result<ContextDependencies, Failure> {
    if !fits(value, PLAN_BYTES) || value["ok"] != true {
        return Err(Failure::Evidence("dependency_reply_incomplete"));
    }
    let rows = value["manifests"]
        .as_array()
        .filter(|rows| rows.len() == resources.len())
        .ok_or(Failure::Evidence("dependency_reply_incomplete"))?;
    let mut loaded = BTreeMap::new();
    for row in rows {
        let resource = row["resource"]
            .as_str()
            .filter(|resource| resources.iter().any(|expected| expected == resource))
            .ok_or(Failure::Evidence("dependency_reply_identity_mismatch"))?;
        let flag = row["loaded"]
            .as_bool()
            .ok_or(Failure::Evidence("dependency_reply_incomplete"))?;
        if loaded.insert(resource.to_string(), flag).is_some() {
            return Err(Failure::Evidence("duplicate_dependency_reply"));
        }
    }
    let observed = value["features"]
        .as_object()
        .filter(|map| map.len() == features.len())
        .ok_or(Failure::Evidence("dependency_feature_identity_mismatch"))?;
    let mut flags = BTreeMap::new();
    for name in features.keys() {
        flags.insert(
            name.clone(),
            observed
                .get(name)
                .and_then(Value::as_bool)
                .ok_or(Failure::Evidence("dependency_feature_identity_mismatch"))?,
        );
    }
    let architecture = value["architecture"]
        .as_str()
        .filter(|value| matches!(*value, "x86_64" | "x86_32" | "arm64" | "arm32"))
        .ok_or(Failure::Evidence("runtime_architecture_unverified"))?
        .to_string();
    let debug = value["debug"]
        .as_bool()
        .ok_or(Failure::Evidence("runtime_identity_mismatch"))?;
    Ok(ContextDependencies {
        loaded,
        features: flags,
        architecture,
        debug,
    })
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn identifier(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}
fn relative(value: &str) -> Result<String, Failure> {
    if value.is_empty()
        || value.len() > PATH_BYTES
        || value.chars().any(char::is_control)
        || value.contains(':')
        || value.contains('\\')
        || value.starts_with('/')
        || value
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || !crate::snapshot::includes(value)
    {
        return Err(Failure::Evidence("invalid_dependency_path"));
    }
    Ok(value.to_string())
}
fn local(path: &Path, budget: &Budget) -> Result<std::fs::Metadata, Failure> {
    budget.check()?;
    let cleaned = crate::project::clean_path(path.to_path_buf());
    let text = cleaned.to_string_lossy();
    if !cleaned.is_absolute()
        || text.len() > 4096
        || text.chars().any(char::is_control)
        || text.starts_with("\\\\")
        || text.starts_with("//")
        || text.contains("://")
        || cleaned
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(Failure::Evidence("local_path_required"));
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDriveTypeW(root: *const u16) -> u32;
        }
        let bytes = text.as_bytes();
        if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
            return Err(Failure::Evidence("local_path_required"));
        }
        let root = [bytes[0] as u16, b':' as u16, b'\\' as u16, 0];
        // The checked drive root is NUL-terminated; no file or program is opened.
        if unsafe { GetDriveTypeW(root.as_ptr()) } == 4 {
            return Err(Failure::Evidence("remote_path_refused"));
        }
    }
    let ancestors: Vec<_> = cleaned.ancestors().collect();
    if ancestors.len() > 128 {
        return Err(Failure::Evidence("path_depth_limit"));
    }
    let mut final_metadata = None;
    for ancestor in ancestors.into_iter().rev() {
        budget.check()?;
        let metadata = std::fs::symlink_metadata(ancestor)
            .map_err(|_| Failure::Evidence("path_unavailable"))?;
        if metadata.file_type().is_symlink() {
            return Err(Failure::Evidence("linked_path_refused"));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(Failure::Evidence("reparse_path_refused"));
            }
        }
        final_metadata = Some(metadata);
    }
    budget.check()?;
    final_metadata.ok_or(Failure::Evidence("path_unavailable"))
}
fn canonical_directory(path: &Path, budget: &Budget) -> Result<PathBuf, Failure> {
    let absolute =
        std::path::absolute(path).map_err(|_| Failure::Evidence("local_path_required"))?;
    if !local(&absolute, budget)?.is_dir() {
        return Err(Failure::Evidence("directory_required"));
    }
    let canonical = absolute
        .canonicalize()
        .map(crate::project::clean_path)
        .map_err(|_| Failure::Evidence("path_unavailable"))?;
    if !local(&canonical, budget)?.is_dir() {
        return Err(Failure::Evidence("directory_required"));
    }
    Ok(canonical)
}
fn dependency_path(
    root: &Path,
    value: &str,
    budget: &Budget,
) -> Result<(PathBuf, String), Failure> {
    let path = crate::project::clean_path(PathBuf::from(value));
    let tail = path
        .strip_prefix(root)
        .map_err(|_| Failure::Evidence("external_dependency_refused"))?
        .to_str()
        .ok_or(Failure::Evidence("invalid_dependency_path"))?
        .replace('\\', "/");
    let tail = relative(&tail)?;
    if key_file(&path) {
        return Err(Failure::Evidence("key_file_refused"));
    }
    if !local(&path, budget)?.is_file() {
        return Err(Failure::Evidence("dependency_file_required"));
    }
    Ok((path, tail))
}
fn key_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "pem" | "p12" | "pfx" | "jks" | "keystore" | "pkcs12"
            )
        })
}
fn disk_hash(path: &Path, limit: u64, budget: &Budget) -> Result<String, Failure> {
    if key_file(path) {
        return Err(Failure::Evidence("key_file_refused"));
    }
    let before = local(path, budget)?;
    if !before.is_file() || before.len() > limit {
        return Err(Failure::Evidence("file_byte_limit"));
    }
    let mut file = crate::snapshot::open_checked_file(path, budget.deadline)
        .map_err(|_| Failure::Evidence("opened_path_unverified"))?;
    if file
        .metadata()
        .map_err(|_| Failure::Evidence("file_unavailable"))?
        .len()
        != before.len()
    {
        return Err(Failure::Evidence("file_drift"));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut bytes = 0u64;
    loop {
        budget.check()?;
        let capacity = usize::try_from(
            before
                .len()
                .saturating_sub(bytes)
                .saturating_add(1)
                .min(buffer.len() as u64),
        )
        .unwrap_or(buffer.len());
        let count = file
            .read(&mut buffer[..capacity])
            .map_err(|_| Failure::Evidence("file_unavailable"))?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .ok_or(Failure::Evidence("file_byte_limit"))?;
        if bytes > limit || bytes > before.len() {
            return Err(Failure::Evidence("file_drift"));
        }
        hash.update(&buffer[..count]);
    }
    if bytes != before.len() || local(path, budget)?.len() != before.len() {
        return Err(Failure::Evidence("file_drift"));
    }
    budget.check()?;
    crate::snapshot::check_opened_path(&file, path, budget.deadline)
        .map_err(|_| Failure::Evidence("opened_path_unverified"))?;
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

struct Dependency {
    absolute: PathBuf,
    wire: String,
    path: String,
    prepared_sha256: String,
}
struct Manifest {
    absolute: PathBuf,
    path: String,
    resource: String,
    prepared_manifest_sha256: Option<String>,
    library: Dependency,
    dependencies: Vec<Dependency>,
}
fn plan(
    preparation: &Preparation,
    private: &Path,
    budget: &Budget,
) -> Result<Vec<Manifest>, Failure> {
    if !native_plan_fits(&preparation.native_plan) {
        return Err(Failure::Evidence("dependency_plan_byte_limit"));
    }
    let rows = preparation.native_plan["manifests"]
        .as_array()
        .filter(|rows| rows.len() <= MANIFESTS)
        .ok_or(Failure::Evidence("dependency_manifest_limit"))?;
    if preparation.native_plan["native"].as_bool() != Some(!rows.is_empty()) {
        return Err(Failure::Evidence("invalid_dependency_plan"));
    }
    let mut seen = BTreeSet::new();
    let mut count = 0usize;
    let mut output = Vec::new();
    for row in rows {
        budget.check()?;
        let manifest_wire = row["manifest"]
            .as_str()
            .ok_or(Failure::Evidence("invalid_dependency_plan"))?;
        let (absolute, path) = dependency_path(private, manifest_wire, budget)?;
        if !path.ends_with(".gdextension") || !seen.insert(path.clone()) {
            return Err(Failure::Evidence("invalid_dependency_plan"));
        }
        let resource = row["resource"]
            .as_str()
            .and_then(|value| value.strip_prefix("res://"))
            .ok_or(Failure::Evidence("invalid_dependency_resource"))?;
        let resource = format!("res://{}", relative(resource)?);
        let library_wire = row["library"]
            .as_str()
            .ok_or(Failure::Evidence("invalid_dependency_plan"))?;
        let (library_absolute, library_path) = dependency_path(private, library_wire, budget)?;
        let prepared_sha256 = row["sha256"]
            .as_str()
            .filter(|hash| digest(hash))
            .ok_or(Failure::Evidence("invalid_prepared_hash"))?
            .to_string();
        let prepared_manifest_sha256 = match row.get("manifest_sha256") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_str()
                    .filter(|hash| digest(hash))
                    .ok_or(Failure::Evidence("invalid_prepared_hash"))?
                    .to_string(),
            ),
        };
        let paths = row["dependencies"]
            .as_array()
            .filter(|paths| paths.len() <= DEPENDENCIES)
            .ok_or(Failure::Evidence("dependency_path_limit"))?;
        let hashes = row["dependency_hashes"]
            .as_object()
            .filter(|hashes| hashes.len() == paths.len())
            .ok_or(Failure::Evidence("invalid_dependency_hashes"))?;
        let mut dependencies = Vec::new();
        let mut dependency_seen = BTreeSet::new();
        for value in paths {
            count += 1;
            if count > DEPENDENCIES {
                return Err(Failure::Evidence("dependency_path_limit"));
            }
            let value = value
                .as_str()
                .ok_or(Failure::Evidence("invalid_dependency_plan"))?;
            let (absolute, path) = dependency_path(private, value, budget)?;
            if !dependency_seen.insert(path.clone()) {
                return Err(Failure::Evidence("duplicate_dependency"));
            }
            let prepared_sha256 = hashes
                .get(value)
                .and_then(Value::as_str)
                .filter(|hash| digest(hash))
                .ok_or(Failure::Evidence("invalid_prepared_hash"))?
                .to_string();
            dependencies.push(Dependency {
                absolute,
                wire: value.to_string(),
                path,
                prepared_sha256,
            });
        }
        output.push(Manifest {
            absolute,
            path,
            resource,
            prepared_manifest_sha256,
            library: Dependency {
                absolute: library_absolute,
                wire: library_wire.to_string(),
                path: library_path,
                prepared_sha256,
            },
            dependencies,
        });
    }
    Ok(output)
}
fn compiled_library_matches(compiled_library: &str, library: &Dependency) -> bool {
    let compiled_library = compiled_library.replace('\\', "/");
    let selected_library = library.wire.replace('\\', "/");
    if cfg!(windows) {
        compiled_library.eq_ignore_ascii_case(&selected_library)
    } else {
        compiled_library == selected_library
    }
}

fn provenance(
    preparation: &Preparation,
    library: &Dependency,
    graph_summary: Option<&Value>,
) -> Result<Value, Failure> {
    match &preparation.native_plan["compiled"] {
        Value::Null | Value::Bool(false) => {
            Ok(json!({"origin":"precompiled","cargo_graph":"not_applicable"}))
        }
        Value::Object(_) => {
            let row = &preparation.native_plan["compiled"];
            let compiled_library = row["library"]
                .as_str()
                .ok_or(Failure::Evidence("invalid_compile_provenance"))?;
            let selected = compiled_library_matches(compiled_library, library);
            if !selected {
                return Ok(json!({"origin":"precompiled","cargo_graph":"not_applicable"}));
            }
            if row["sha256"].as_str() != Some(library.prepared_sha256.as_str()) {
                return Err(Failure::Evidence("invalid_compile_provenance"));
            }
            let package = row["package"]
                .as_str()
                .filter(|value| identifier(value, 128))
                .ok_or(Failure::Evidence("invalid_compile_provenance"))?;
            let target = row["target"]
                .as_str()
                .filter(|value| identifier(value, 128))
                .ok_or(Failure::Evidence("invalid_compile_provenance"))?;
            let lock = row["lock_sha256"]
                .as_str()
                .filter(|hash| digest(hash))
                .ok_or(Failure::Evidence("invalid_compile_provenance"))?;
            if row["offline"] != true {
                return Err(Failure::Evidence("unverified_compile_provenance"));
            }
            Ok(
                json!({"origin":"private_cargo_build","package":package,"target":target,"lock_sha256":lock,"offline":true,
                "cargo_graph":graph_summary.cloned().unwrap_or_else(||json!("not_retained")),
                "scope":"selected artifact provenance and optional filtered workspace resolution; not exact build units"}),
            )
        }
        _ => Err(Failure::Evidence("invalid_compile_provenance")),
    }
}
fn version_identity(value: &Value, owner: &OwnershipRecord) -> Result<Value, Failure> {
    if value["ok"] != true || value["pid"].as_u64() != Some(owner.pid as u64) {
        return Err(Failure::Evidence("runtime_identity_mismatch"));
    }
    let version = &value["version"];
    let integer = |field: &str| {
        version[field]
            .as_u64()
            .filter(|value| *value <= 100_000)
            .ok_or(Failure::Evidence("runtime_version_unverified"))
    };
    let status = version["status"]
        .as_str()
        .filter(|value| identifier(value, 32))
        .ok_or(Failure::Evidence("runtime_version_unverified"))?;
    let build = version["build"]
        .as_str()
        .filter(|value| identifier(value, 64))
        .ok_or(Failure::Evidence("runtime_version_unverified"))?;
    let architecture = value["architecture"]
        .as_str()
        .filter(|value| matches!(*value, "x86_64" | "x86_32" | "arm64" | "arm32"))
        .ok_or(Failure::Evidence("runtime_architecture_unverified"))?;
    let debug = value["debug"]
        .as_bool()
        .ok_or(Failure::Evidence("runtime_identity_mismatch"))?;
    Ok(
        json!({"major":integer("major")?,"minor":integer("minor")?,"patch":integer("patch")?,
        "status":status,"build":build,"architecture":architecture,"debug":debug,"evidence":"authenticated accepted-game response"}),
    )
}
fn source(
    adapter: &dyn SourceFingerprint,
    root: &Path,
    budget: &Budget,
) -> Result<Fingerprint, Failure> {
    budget.check()?;
    let result = adapter.fingerprint(root, budget.deadline, FingerprintLimits::default());
    budget.check()?;
    let fingerprint = result.map_err(|_| Failure::Evidence("source_fingerprint_unavailable"))?;
    if !digest(&fingerprint.sha256)
        || fingerprint.files > SOURCE_FILES
        || fingerprint.bytes > SOURCE_BYTES
    {
        return Err(Failure::Evidence("source_fingerprint_limit"));
    }
    Ok(fingerprint)
}
fn native_live(pid: u32) -> Option<LiveProcess> {
    #[cfg(windows)]
    {
        crate::ownership::inspect(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        None
    }
}

/// Uses the bounded snapshot-all helper; never starts a game or persists context.
pub fn inspect(root: &Path, input: &Value, session: Session<'_>) -> Result<Value, String> {
    inspect_native(root, input, session, &BoundedSnapshots)
}
pub fn inspect_with_source(
    root: &Path,
    input: &Value,
    session: Session<'_>,
    source: &dyn SourceFingerprint,
) -> Result<Value, String> {
    inspect_native(root, input, session, source)
}
fn inspect_native(
    root: &Path,
    input: &Value,
    session: Session<'_>,
    source: &dyn SourceFingerprint,
) -> Result<Value, String> {
    #[cfg(not(windows))]
    if matches!(session, Session::Ready { .. }) {
        return inspect_inner(root, input, Session::Unavailable, Some(source), native_live).map(
            |report| {
                if report["state"] == "unavailable" {
                    base("unavailable", "accepted_runtime_platform_unqualified")
                } else {
                    report
                }
            },
        );
    }
    inspect_inner(root, input, session, Some(source), native_live)
}
fn base(state: &str, reason: &str) -> Value {
    json!({"ok":true,"report_version":1,"state":state,"complete":false,"reasons":[reason],
        "runtime":null,"source":null,"dependencies":[],"read_only":true,
        "limits":{"budget_ms":BUDGET_MS,"report_bytes":REPORT_BYTES,"source_bytes":SOURCE_BYTES,
            "source_files":SOURCE_FILES,"source_entries":SOURCE_ENTRIES,"path_bytes":PATH_BYTES,"manifests":MANIFESTS,"dependencies":DEPENDENCIES},
        "evidence":{"mapped_memory_hash":"unverified","filesystem_atomicity":"unverified","model_tokens_measured":false,"model_latency_measured":false,
            "source_contents_read_for_hashing":if matches!(state,"busy"|"unavailable") {json!(false)} else {Value::Null},"source_contents_returned":false,"recognized_key_files_read":false,
            "privacy":"hashing may read material embedded in ordinary source; no raw source, environment, authentication or checkpoints returned"},
        "effects":{"game_started":false,"compiled":false,"game_mutated":false,"persisted":false,"environment_changed":false}})
}
fn inspect_inner(
    root: &Path,
    input: &Value,
    session: Session<'_>,
    fingerprints: Option<&dyn SourceFingerprint>,
    observe: impl Fn(u32) -> Option<LiveProcess>,
) -> Result<Value, String> {
    let budget = Budget::new();
    if !fits(input, 1024) {
        return Err("runtime context request exceeds 1 KiB".into());
    }
    let request: Request =
        serde_json::from_value(input.clone()).map_err(|_| "invalid runtime context request")?;
    if request
        .op
        .as_deref()
        .is_some_and(|op| op != "runtime_context")
    {
        return Err("runtime context received a different operation".into());
    }
    let selection = requested_resources(input, &request)?;
    match budget.check() {
        Err(Failure::Cancelled) => return Err("runtime context cancelled".into()),
        Err(_) => return Ok(base("incomplete", "deadline_expired")),
        Ok(()) => {}
    }
    let (preparation, query) = match session {
        Session::Unavailable => return Ok(base("unavailable", "no_accepted_session")),
        Session::Busy => return Ok(base("busy", "controller_operation_active")),
        Session::Ready { preparation, query } => (preparation, query),
    };
    let result = collect(
        root,
        preparation,
        query,
        fingerprints,
        &budget,
        &observe,
        selection.as_ref(),
    );
    match budget.check() {
        Err(Failure::Cancelled) => return Err("runtime context cancelled".into()),
        Err(Failure::Deadline) => return Ok(base("incomplete", "deadline_expired")),
        _ => {}
    }
    match result {
        Ok(report) if fits(&report, REPORT_BYTES) => match budget.check() {
            Ok(()) => Ok(report),
            Err(Failure::Cancelled) => Err("runtime context cancelled".into()),
            Err(_) => Ok(base("incomplete", "deadline_expired")),
        },
        Ok(_) => Ok(base("incomplete", "report_byte_limit")),
        Err(Failure::Cancelled) => Err("runtime context cancelled".into()),
        Err(Failure::Deadline) => Ok(base("incomplete", "deadline_expired")),
        Err(Failure::Evidence(code)) => Ok(base(
            if matches!(code, "ownership_mismatch" | "runtime_identity_mismatch") {
                "unhealthy"
            } else if matches!(
                code,
                "file_drift"
                    | "private_preparation_drift"
                    | "executable_disk_drift"
                    | "runtime_metadata_drift"
            ) {
                "drift"
            } else {
                "incomplete"
            },
            code,
        )),
    }
}
fn collect(
    root: &Path,
    preparation: &Preparation,
    query: &dyn RuntimeQuery,
    fingerprints: Option<&dyn SourceFingerprint>,
    budget: &Budget,
    observe: &impl Fn(u32) -> Option<LiveProcess>,
    selection: Option<&BTreeSet<String>>,
) -> Result<Value, Failure> {
    budget.check()?;
    let root = canonical_directory(root, budget)?;
    if preparation.owner.kind != ProcessKind::Game
        || !crate::ownership::paths_equal(&root, &preparation.owner.project)
        || preparation.owner.pid == 0
        || preparation.owner.started.is_empty()
        || preparation.owner.started.len() > 128
        || preparation.owner.started.chars().any(char::is_control)
        || !digest(&preparation.source_sha256)
        || !digest(&preparation.prepared_sha256)
    {
        return Err(Failure::Evidence("invalid_preparation_identity"));
    }
    let owned =
        || observe(preparation.owner.pid).is_some_and(|live| preparation.owner.describes(&live));
    if !owned() {
        return Err(Failure::Evidence("ownership_mismatch"));
    }
    let private = canonical_directory(&preparation.private_source, budget)?;
    let manifests = plan(preparation, &private, budget)?;
    // Selection is an exact retained-roster join, never a new resource/path query.
    // Resolve it before calling project code; all later verification stays whole-session.
    if let Some(selection) = selection {
        for resource in selection {
            match manifests
                .iter()
                .filter(|row| row.resource == *resource)
                .count()
            {
                1 => {}
                0 => return Err(Failure::Evidence("unknown_dependency_selection")),
                _ => return Err(Failure::Evidence("ambiguous_dependency_selection")),
            }
        }
    }
    match &preparation.native_plan["compiled"] {
        Value::Null | Value::Bool(false) => {}
        Value::Object(compiled) => {
            let library = compiled
                .get("library")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty() && s.len() <= 4096 && !s.chars().any(char::is_control))
                .ok_or(Failure::Evidence("invalid_compile_provenance"))?;
            if manifests
                .iter()
                .filter(|row| compiled_library_matches(library, &row.library))
                .count()
                != 1
            {
                return Err(Failure::Evidence("compiled_library_selection_mismatch"));
            }
        }
        _ => return Err(Failure::Evidence("invalid_compile_provenance")),
    }
    let graph_summary = preparation.native_plan["compiled"]
        .get("cargo_graph")
        .map(crate::cargo_graph::summary)
        .transpose()
        .map_err(|_| Failure::Evidence("invalid_retained_cargo_graph"))?;
    budget.check()?;
    // Validate all prepared provenance before any call into project code.
    let provenances = manifests
        .iter()
        .map(|row| provenance(preparation, &row.library, graph_summary.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut reasons = BTreeSet::new();
    let current = fingerprints
        .map(|adapter| source(adapter, &root, budget))
        .transpose()?;
    let prepared_current = fingerprints
        .map(|adapter| source(adapter, &private, budget))
        .transpose()?;
    if fingerprints.is_none() {
        reasons.insert("bounded_source_fingerprint_unavailable");
    }
    let mut drift = prepared_current
        .as_ref()
        .is_some_and(|fingerprint| fingerprint.sha256 != preparation.prepared_sha256);
    if drift {
        if !owned() {
            return Err(Failure::Evidence("ownership_mismatch"));
        }
        return Err(Failure::Evidence("private_preparation_drift"));
    }
    let executable = disk_hash(&preparation.owner.executable, EXECUTABLE_BYTES, budget)?;
    let executable_matches = match &preparation.executable_sha256 {
        Some(hash) if digest(hash) => Some(hash == &executable),
        Some(_) => return Err(Failure::Evidence("invalid_prepared_hash")),
        None => {
            reasons.insert("launch_executable_hash_unrecorded");
            None
        }
    };
    drift |= executable_matches == Some(false);
    if drift {
        return Err(Failure::Evidence("executable_disk_drift"));
    }
    if !owned() {
        return Err(Failure::Evidence("ownership_mismatch"));
    }
    let resources: Vec<String> = manifests.iter().map(|row| row.resource.clone()).collect();
    let expected_features: Option<BTreeMap<String, bool>> = preparation.native_plan
        ["runtime_features"]
        .as_object()
        .filter(|map| map.len() <= 128)
        .and_then(|map| {
            map.iter()
                .map(|(name, value)| {
                    feature_identifier(name)
                        .then(|| value.as_bool().map(|flag| (name.clone(), flag)))
                        .flatten()
                })
                .collect()
        });
    if expected_features.is_none() {
        reasons.insert("runtime_feature_capture_unrecorded");
    }
    let expected_features = expected_features.unwrap_or_default();
    let dependency_request = json!({"op":"context_dependencies","resources":resources,"features":expected_features.keys().collect::<Vec<_>>()});
    if !fits(&dependency_request, PLAN_BYTES) {
        return Err(Failure::Evidence("dependency_request_byte_limit"));
    }
    let actual = query.request(&dependency_request, budget.remaining()?);
    budget.check()?;
    let actual = actual.map_err(|_| Failure::Evidence("dependency_query_unavailable"))?;
    if !owned() {
        return Err(Failure::Evidence("ownership_mismatch"));
    }
    if !fits(&actual, PLAN_BYTES) {
        return Err(Failure::Evidence("dependency_reply_byte_limit"));
    }
    let actual = context_dependencies(&actual, &resources, &expected_features)?;
    if actual.features != expected_features {
        return Err(Failure::Evidence("runtime_identity_mismatch"));
    }
    if let (Some(architecture), Some(debug)) = (
        preparation.native_plan["runtime_architecture"].as_str(),
        preparation.native_plan["runtime_debug"].as_bool(),
    ) {
        if architecture != actual.architecture || debug != actual.debug {
            return Err(Failure::Evidence("runtime_identity_mismatch"));
        }
    } else {
        reasons.insert("runtime_feature_capture_unrecorded");
    }
    let mut unhealthy = actual.loaded.values().any(|loaded| !*loaded);
    let runtime_reply = query.request(&json!({"op":"context_identity"}), budget.remaining()?);
    budget.check()?;
    if !owned() {
        return Err(Failure::Evidence("ownership_mismatch"));
    }
    let runtime_identity = match runtime_reply {
        Ok(value) if fits(&value, 4096) => match version_identity(&value, &preparation.owner) {
            Ok(identity) => Some(identity),
            Err(Failure::Evidence("runtime_identity_mismatch")) => {
                return Err(Failure::Evidence("runtime_identity_mismatch"));
            }
            Err(_) => {
                reasons.insert("runtime_version_unverified");
                None
            }
        },
        _ => {
            reasons.insert("runtime_identity_unavailable");
            None
        }
    };
    if let Some(identity) = runtime_identity.as_ref() {
        if identity["architecture"].as_str() != Some(actual.architecture.as_str())
            || identity["debug"].as_bool() != Some(actual.debug)
        {
            return Err(Failure::Evidence("runtime_identity_mismatch"));
        }
    }
    let mut output = Vec::new();
    let mut host_verified = json!({"ok":true,"manifests":preparation.native_plan["manifests"]});
    for (index, (row, provenance)) in manifests.iter().zip(&provenances).enumerate() {
        budget.check()?;
        let manifest_hash = disk_hash(&row.absolute, MANIFEST_BYTES, budget)?;
        let manifest_matches = row
            .prepared_manifest_sha256
            .as_ref()
            .map(|hash| hash == &manifest_hash);
        if manifest_matches.is_none() {
            reasons.insert("prepared_manifest_hash_unrecorded");
        }
        drift |= manifest_matches == Some(false);
        let library_hash = disk_hash(&row.library.absolute, BINARY_BYTES, budget)?;
        drift |= library_hash != row.library.prepared_sha256;
        let loaded = actual
            .loaded
            .get(&row.resource)
            .copied()
            .ok_or(Failure::Evidence("dependency_reply_identity_mismatch"))?;
        unhealthy |= !loaded;
        host_verified["manifests"][index]["loaded"] = json!(loaded);
        host_verified["manifests"][index]["sha256"] = json!(library_hash);
        let mut host_hashes = serde_json::Map::new();
        let mut dependencies = Vec::new();
        for dependency in &row.dependencies {
            let hash = disk_hash(&dependency.absolute, BINARY_BYTES, budget)?;
            drift |= hash != dependency.prepared_sha256;
            host_hashes.insert(dependency.wire.clone(), json!(hash));
            dependencies.push(json!({"path":dependency.path,"prepared_sha256":dependency.prepared_sha256,"current_disk_sha256":hash}));
        }
        host_verified["manifests"][index]["dependency_hashes"] = Value::Object(host_hashes);
        output.push(json!({"manifest":row.path,"resource":row.resource,"loaded_resource":loaded,
            "prepared_manifest_sha256":row.prepared_manifest_sha256,"current_manifest_disk_sha256":manifest_hash,
            "library":{"path":row.library.path,"prepared_sha256":row.library.prepared_sha256,"current_disk_sha256":library_hash,"disk_hash_provenance":"host bounded file reads"},
            "dependencies":dependencies,"provenance":provenance,"evidence":"game loaded-resource flag; captured manifest bindings and host disk hashes; mapped memory unverified"}));
    }
    #[cfg(windows)]
    if crate::native_dependencies::verify(&preparation.native_plan, &host_verified).is_err() {
        reasons.insert("loaded_dependency_identity_mismatch");
        if !unhealthy {
            drift = true;
        }
    }
    if let (Some(adapter), Some(before), Some(private_before)) =
        (fingerprints, current.as_ref(), prepared_current.as_ref())
    {
        let after = source(adapter, &root, budget)?;
        let private_after = source(adapter, &private, budget)?;
        if before.sha256 != after.sha256
            || before.files != after.files
            || before.bytes != after.bytes
            || private_before.sha256 != private_after.sha256
            || private_before.files != private_after.files
            || private_before.bytes != private_after.bytes
        {
            return Err(Failure::Evidence("file_drift"));
        }
    }
    if disk_hash(&preparation.owner.executable, EXECUTABLE_BYTES, budget)? != executable {
        return Err(Failure::Evidence("file_drift"));
    }
    let after = query.request(&dependency_request, budget.remaining()?);
    budget.check()?;
    if !owned() {
        return Err(Failure::Evidence("ownership_mismatch"));
    }
    let after = after.map_err(|_| Failure::Evidence("dependency_query_unavailable"))?;
    if context_dependencies(&after, &resources, &expected_features)? != actual {
        return Err(Failure::Evidence("runtime_metadata_drift"));
    }
    if !owned() {
        return Err(Failure::Evidence("ownership_mismatch"));
    }
    budget.check()?;
    let stale = current
        .as_ref()
        .is_some_and(|fingerprint| fingerprint.sha256 != preparation.source_sha256);
    if drift {
        reasons.insert("prepared_or_runtime_disk_drift");
    }
    if unhealthy {
        reasons.insert("native_extension_not_loaded");
    }
    let complete = reasons.is_empty();
    let state = if unhealthy {
        "unhealthy"
    } else if drift {
        "drift"
    } else if !complete {
        "incomplete"
    } else if stale {
        "stale"
    } else {
        "available"
    };
    let mut report = base(state, "unused");
    report["complete"] = json!(complete);
    report["reasons"] = json!(reasons);
    report["runtime"] = json!({"pid":preparation.owner.pid,"started":preparation.owner.started,"owned_process_verified":true,
        "version":runtime_identity,"launch_executable_sha256":preparation.executable_sha256,"current_executable_disk_sha256":executable,
        "executable_matches_launch":executable_matches,"process_rechecked":true});
    report["source"] = json!({"launched_sha256":preparation.source_sha256,"prepared_sha256":preparation.prepared_sha256,
        "current_sha256":current.as_ref().map(|fingerprint|&fingerprint.sha256),"current_files":current.as_ref().map(|fingerprint|fingerprint.files),
        "current_bytes":current.as_ref().map(|fingerprint|fingerprint.bytes),"prepared_current_sha256":prepared_current.as_ref().map(|fingerprint|&fingerprint.sha256),
        "stale":if current.is_some() {json!(stale)} else {Value::Null},"evidence":"bounded snapshot-all digest; source bodies not returned"});
    if let Some(selection) = selection {
        output.retain(|row| {
            row["resource"]
                .as_str()
                .is_some_and(|resource| selection.contains(resource))
        });
        report["dependency_selection"] = json!({
            "scope":"returned_rows_only",
            "verification_scope":"whole_accepted_session",
            "resources":selection,
            "checked_manifest_count":manifests.len(),
            "returned_manifest_count":output.len(),
            "order":"prepared_manifest_order"
        });
    }
    report["dependencies"] = json!(output);
    report["evidence"]["source_contents_read_for_hashing"] = json!(fingerprints.is_some());
    if !fits(&report, REPORT_BYTES.saturating_sub(96)) {
        return Err(Failure::Evidence("report_byte_limit"));
    }
    let encoded =
        serde_json::to_vec(&report).map_err(|_| Failure::Evidence("report_encoding_failed"))?;
    report["context_id"] = json!(crate::sha256_hex(&encoded));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Fixture {
        base: PathBuf,
        public: PathBuf,
        private: PathBuf,
        executable: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let base = std::env::temp_dir().join(format!(
                "aurum-runtime-context-{}",
                crate::random::session_id()
            ));
            let public = base.join("public");
            let private = base.join("private");
            let executable = base.join("runtime/Godot.exe");
            for root in [&public, &private] {
                Self::file(&root.join("godot/project.godot"), b"config_version=5\n");
                Self::file(&root.join("rust/game/src/lib.rs"), b"before");
                Self::file(
                    &root.join("godot/addons/native/bin/active.gdextension"),
                    b"[libraries]\nwindows.debug.x86_64=\"res://addons/native/bin/active.dll\"\n",
                );
                Self::file(
                    &root.join("godot/addons/native/bin/active.dll"),
                    b"accepted binary",
                );
                Self::file(&root.join(".env"), b"PRIVATE-ENV-SENTINEL");
                Self::file(
                    &root.join("credentials.toml"),
                    b"PRIVATE-CREDENTIAL-SENTINEL",
                );
            }
            Self::file(&executable, b"owned runtime binary");
            let public = public
                .canonicalize()
                .map(crate::project::clean_path)
                .unwrap();
            let private = private
                .canonicalize()
                .map(crate::project::clean_path)
                .unwrap();
            let executable = executable
                .canonicalize()
                .map(crate::project::clean_path)
                .unwrap();
            Self {
                base,
                public,
                private,
                executable,
            }
        }
        fn file(path: &Path, body: &[u8]) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        fn wire(path: &Path) -> String {
            path.to_string_lossy().replace('\\', "/")
        }
        fn preparation(&self) -> Preparation {
            let manifest = self
                .private
                .join("godot/addons/native/bin/active.gdextension");
            let library = self.private.join("godot/addons/native/bin/active.dll");
            Preparation {
                owner: OwnershipRecord {
                    session: "a".repeat(32),
                    executable: self.executable.clone(),
                    pid: 12345,
                    started: "fixture-start-100".into(),
                    project: self.public.clone(),
                    kind: ProcessKind::Game,
                },
                private_source: self.private.clone(),
                source_sha256: crate::snapshot::fingerprint_all_bounded(
                    &self.public,
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap()
                .sha256,
                prepared_sha256: crate::snapshot::fingerprint_all_bounded(
                    &self.private,
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap()
                .sha256,
                native_plan: json!({"native":true,"compiled":null,"runtime_features":{"windows":true,"debug":true,"x86_64":true},"runtime_architecture":"x86_64","runtime_debug":true,"manifests":[{
                    "manifest":Self::wire(&manifest),"resource":"res://addons/native/bin/active.gdextension",
                    "library":Self::wire(&library),"sha256":crate::sha256_file(&library).unwrap(),
                    "manifest_sha256":crate::sha256_file(&manifest).unwrap(),"feature_tags":["windows","debug","x86_64"],"dependencies":[],"dependency_hashes":{}}]}),
                executable_sha256: Some(crate::sha256_file(&self.executable).unwrap()),
            }
        }
        fn two_manifest_preparation(&self) -> Preparation {
            for root in [&self.public, &self.private] {
                Self::file(
                    &root.join("godot/addons/vendor/vendor.gdextension"),
                    b"[libraries]\nwindows.debug.x86_64=\"res://addons/vendor/vendor.dll\"\n",
                );
                Self::file(
                    &root.join("godot/addons/vendor/vendor.dll"),
                    b"vendor binary",
                );
            }
            let mut preparation = self.preparation();
            let manifest = self.private.join("godot/addons/vendor/vendor.gdextension");
            let library = self.private.join("godot/addons/vendor/vendor.dll");
            preparation.native_plan["manifests"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "manifest":Self::wire(&manifest),"resource":"res://addons/vendor/vendor.gdextension",
                    "library":Self::wire(&library),"sha256":crate::sha256_file(&library).unwrap(),
                    "manifest_sha256":crate::sha256_file(&manifest).unwrap(),
                    "feature_tags":["windows","debug","x86_64"],"dependencies":[],"dependency_hashes":{}
                }));
            preparation
        }
        fn reply(&self, preparation: &Preparation, input: &Value) -> Result<Value, String> {
            match input["op"].as_str() {
                Some("context_dependencies") => {
                    assert!(input.get("manifests").is_none());
                    let rows: Vec<_> = input["resources"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|resource| json!({"resource":resource,"loaded":true}))
                        .collect();
                    let features: BTreeMap<_, _> = input["features"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|tag| {
                            (
                                tag.as_str().unwrap(),
                                preparation.native_plan["runtime_features"][tag.as_str().unwrap()]
                                    .as_bool()
                                    .unwrap_or(false),
                            )
                        })
                        .collect();
                    Ok(
                        json!({"ok":true,"manifests":rows,"features":features,"architecture":"x86_64","debug":true,
                        "token":"PRIVATE-TOKEN-SENTINEL","checkpoint":{"source":"PRIVATE-STATE-SENTINEL"}}),
                    )
                }
                Some("context_identity") => Ok(json!({"ok":true,"pid":preparation.owner.pid,
                    "version":{"major":4,"minor":7,"patch":0,"status":"stable","build":"official",
                        "unused_private_body":"PRIVATE-SOURCE-SENTINEL"},"architecture":"x86_64","debug":true,
                    "environment":"PRIVATE-ENV-SENTINEL"})),
                _ => Err("unexpected fixture operation".into()),
            }
        }
        fn live(preparation: &Preparation) -> LiveProcess {
            LiveProcess {
                pid: preparation.owner.pid,
                executable: preparation.owner.executable.clone(),
                started: Some(preparation.owner.started.clone()),
            }
        }
        fn inspect(&self, preparation: &Preparation, query: &dyn RuntimeQuery) -> Value {
            self.inspect_request(preparation, query, &json!({"op":"runtime_context"}))
        }
        fn inspect_request(
            &self,
            preparation: &Preparation,
            query: &dyn RuntimeQuery,
            input: &Value,
        ) -> Value {
            inspect_inner(
                &self.public,
                input,
                Session::Ready { preparation, query },
                Some(&BoundedSnapshots),
                |_| Some(Self::live(preparation)),
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }
    #[test]
    fn runtime_context_selection_syntax_is_bounded_closed_and_array_only() {
        let names: Vec<_> = (0..MANIFESTS)
            .map(|index| format!("res://m{index}.gdextension"))
            .collect();
        assert!(operation_request(&json!({"op":"runtime_context","resources":names})).is_ok());
        assert!(operation_request(&json!({"op":"runtime_context"})).is_ok());
        let invalid = [
            Value::Null,
            json!([]),
            json!("res://a.gdextension"),
            json!({"resource":"res://a.gdextension"}),
            json!([42]),
            json!(["res://a.gdextension", "res://a.gdextension"]),
            json!((0..=MANIFESTS)
                .map(|n| format!("res://m{n}.gdextension"))
                .collect::<Vec<_>>()),
            json!(["res://../a.gdextension"]),
            json!(["res://addons/../a.gdextension"]),
            json!(["res://addons//a.gdextension"]),
            json!(["res://C:/a.gdextension"]),
            json!(["res://addons\\a.gdextension"]),
            json!(["res://.aurum/a.gdextension"]),
            json!(["file:///A:/a.gdextension"]),
            json!(["res://a.gdextension\n"]),
            json!(["res://scene.tscn"]),
            json!([format!("res://{}.gdextension", "x".repeat(1024))]),
        ];
        for resources in invalid {
            let input = json!({"op":"runtime_context","resources":resources});
            assert!(operation_request(&input).is_err(), "{input}");
            assert!(inspect_inner(
                Path::new("//untrusted.invalid/project"),
                &input,
                Session::Unavailable,
                None,
                |_| panic!("invalid selection must not inspect a process")
            )
            .is_err());
        }
        assert!(operation_request(
            &json!({"op":"runtime_context","resources":["res://a.gdextension"],"limit":1})
        )
        .is_err());
    }
    #[test]
    fn runtime_context_selection_filters_rows_without_narrowing_whole_session_queries() {
        let fixture = Fixture::new();
        let preparation = fixture.two_manifest_preparation();
        let requests = std::cell::RefCell::new(Vec::new());
        let query = |input: &Value, _: Duration| {
            requests.borrow_mut().push(input.clone());
            fixture.reply(&preparation, input)
        };
        let all = fixture.inspect(&preparation, &query);
        assert_eq!(all["state"], "available");
        assert_eq!(all["dependencies"].as_array().unwrap().len(), 2);
        assert!(all.get("dependency_selection").is_none());
        requests.borrow_mut().clear();
        let selected = fixture.inspect_request(
            &preparation,
            &query,
            &json!({"op":"runtime_context","resources":["res://addons/vendor/vendor.gdextension"]}),
        );
        assert_eq!(selected["state"], all["state"]);
        assert_eq!(selected["complete"], true);
        assert_eq!(
            selected["dependencies"],
            json!([all["dependencies"][1].clone()])
        );
        assert_eq!(
            selected["dependency_selection"]["scope"],
            "returned_rows_only"
        );
        assert_eq!(
            selected["dependency_selection"]["verification_scope"],
            "whole_accepted_session"
        );
        assert_eq!(
            selected["dependency_selection"]["checked_manifest_count"],
            2
        );
        assert_eq!(
            selected["dependency_selection"]["returned_manifest_count"],
            1
        );
        let requests = requests.borrow();
        assert_eq!(requests.len(), 3);
        let dependencies: Vec<_> = requests
            .iter()
            .filter(|row| row["op"] == "context_dependencies")
            .collect();
        assert_eq!(dependencies.len(), 2);
        for request in dependencies {
            assert_eq!(
                request["resources"],
                json!([
                    "res://addons/native/bin/active.gdextension",
                    "res://addons/vendor/vendor.gdextension"
                ])
            );
        }
        assert!(!selected.to_string().contains("PRIVATE-"));
        assert!(!fixture.public.join(".aurum").exists());
    }
    #[test]
    fn runtime_context_selection_preserves_prepared_order_and_canonical_selection() {
        let fixture = Fixture::new();
        let preparation = fixture.two_manifest_preparation();
        let query = |input: &Value, _: Duration| fixture.reply(&preparation, input);
        let forward = fixture.inspect_request(&preparation, &query, &json!({"op":"runtime_context","resources":[
            "res://addons/native/bin/active.gdextension", "res://addons/vendor/vendor.gdextension"
        ]}));
        let reversed = fixture.inspect_request(&preparation, &query, &json!({"op":"runtime_context","resources":[
            "res://addons/vendor/vendor.gdextension", "res://addons/native/bin/active.gdextension"
        ]}));
        assert_eq!(forward, reversed);
        assert_eq!(
            forward["dependencies"][0]["resource"],
            "res://addons/native/bin/active.gdextension"
        );
        assert_eq!(
            forward["dependencies"][1]["resource"],
            "res://addons/vendor/vendor.gdextension"
        );
    }
    #[test]
    fn runtime_context_selection_unknown_or_ambiguous_roster_refuses_before_game_queries() {
        for ambiguous in [false, true] {
            let fixture = Fixture::new();
            let mut preparation = fixture.two_manifest_preparation();
            let resource = if ambiguous {
                preparation.native_plan["manifests"][1]["resource"] =
                    json!("res://addons/native/bin/active.gdextension");
                "res://addons/native/bin/active.gdextension"
            } else {
                "res://addons/native/bin/not-retained.gdextension"
            };
            let calls = AtomicUsize::new(0);
            let query = |_: &Value, _: Duration| {
                calls.fetch_add(1, Ordering::Relaxed);
                Err("unreached".into())
            };
            let report = fixture.inspect_request(
                &preparation,
                &query,
                &json!({"op":"runtime_context","resources":[resource]}),
            );
            assert_eq!(report["state"], "incomplete");
            assert_eq!(report["complete"], false);
            assert_eq!(
                report["reasons"][0],
                if ambiguous {
                    "ambiguous_dependency_selection"
                } else {
                    "unknown_dependency_selection"
                }
            );
            assert_eq!(calls.load(Ordering::Relaxed), 0);
            assert!(report["dependencies"].as_array().unwrap().is_empty());
            assert!(!report.to_string().contains("not-retained"));
        }
    }
    #[test]
    fn runtime_context_selection_unselected_unloaded_manifest_remains_unhealthy() {
        let fixture = Fixture::new();
        let preparation = fixture.two_manifest_preparation();
        let query = |input: &Value, _: Duration| {
            let mut reply = fixture.reply(&preparation, input)?;
            if input["op"] == "context_dependencies" {
                reply["manifests"][1]["loaded"] = json!(false);
            }
            Ok(reply)
        };
        let report = fixture.inspect_request(
            &preparation,
            &query,
            &json!({"op":"runtime_context","resources":[
                "res://addons/native/bin/active.gdextension"
            ]}),
        );
        assert_eq!(report["state"], "unhealthy");
        assert_eq!(report["complete"], false);
        assert_eq!(report["dependencies"].as_array().unwrap().len(), 1);
        assert_eq!(report["dependencies"][0]["loaded_resource"], true);
        assert!(report["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("native_extension_not_loaded")));
    }
    #[test]
    fn runtime_context_selection_unselected_disk_drift_remains_nonavailable() {
        let fixture = Fixture::new();
        let preparation = fixture.two_manifest_preparation();
        Fixture::file(
            &fixture.private.join("godot/addons/vendor/vendor.dll"),
            b"changed vendor",
        );
        let calls = AtomicUsize::new(0);
        let query = |_: &Value, _: Duration| {
            calls.fetch_add(1, Ordering::Relaxed);
            Err("unreached".into())
        };
        let report = fixture.inspect_request(
            &preparation,
            &query,
            &json!({"op":"runtime_context","resources":[
                "res://addons/native/bin/active.gdextension"
            ]}),
        );
        assert_eq!(report["state"], "drift");
        assert_eq!(report["complete"], false);
        assert_eq!(report["reasons"][0], "private_preparation_drift");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn runtime_context_selection_unselected_runtime_drift_remains_nonavailable() {
        let fixture = Fixture::new();
        let preparation = fixture.two_manifest_preparation();
        let calls = AtomicUsize::new(0);
        let query = |input: &Value, _: Duration| {
            let mut reply = fixture.reply(&preparation, input)?;
            if input["op"] == "context_dependencies" && calls.fetch_add(1, Ordering::Relaxed) > 0 {
                reply["manifests"][1]["loaded"] = json!(false);
            }
            Ok(reply)
        };
        let report = fixture.inspect_request(
            &preparation,
            &query,
            &json!({"op":"runtime_context","resources":[
                "res://addons/native/bin/active.gdextension"
            ]}),
        );
        assert_eq!(report["state"], "drift");
        assert_eq!(report["complete"], false);
        assert_eq!(report["reasons"][0], "runtime_metadata_drift");
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn accepted_context_distinguishes_disk_and_loaded_resource_evidence_without_secret_bodies() {
        let fixture = Fixture::new();
        let preparation = fixture.preparation();
        let query = |input: &Value, _: Duration| fixture.reply(&preparation, input);
        let report = fixture.inspect(&preparation, &query);
        assert_eq!(report["state"], "available");
        assert_eq!(report["complete"], true);
        assert_eq!(report["runtime"]["pid"], preparation.owner.pid);
        assert_eq!(report["runtime"]["version"]["minor"], 7);
        assert_eq!(
            report["dependencies"][0]["manifest"],
            "godot/addons/native/bin/active.gdextension"
        );
        assert_eq!(
            report["dependencies"][0]["library"]["path"],
            "godot/addons/native/bin/active.dll"
        );
        assert_eq!(report["dependencies"][0]["loaded_resource"], true);
        assert_eq!(report["evidence"]["mapped_memory_hash"], "unverified");
        assert_eq!(report["evidence"]["source_contents_read_for_hashing"], true);
        assert_eq!(report["evidence"]["source_contents_returned"], false);
        let encoded = serde_json::to_string(&report).unwrap();
        for value in [
            "PRIVATE-",
            fixture.base.to_str().unwrap(),
            "before",
            "accepted binary",
        ] {
            assert!(!encoded.contains(value), "{value}");
        }
        assert!(fits(&report, REPORT_BYTES));
        assert!(!fixture.public.join(".aurum").exists());
    }
    #[test]
    fn same_size_public_edit_reports_staleness_without_changing_accepted_library_or_pid() {
        let fixture = Fixture::new();
        let preparation = fixture.preparation();
        let query = |input: &Value, _: Duration| fixture.reply(&preparation, input);
        let before = fixture.inspect(&preparation, &query);
        Fixture::file(&fixture.public.join("rust/game/src/lib.rs"), b"after!");
        let after = fixture.inspect(&preparation, &query);
        assert_eq!(after["state"], "stale");
        assert_eq!(after["complete"], true);
        assert_eq!(after["source"]["stale"], true);
        assert_ne!(before["context_id"], after["context_id"]);
        assert_eq!(before["runtime"]["pid"], after["runtime"]["pid"]);
        assert_eq!(
            before["dependencies"][0]["library"],
            after["dependencies"][0]["library"]
        );
        assert_eq!(
            std::fs::read(fixture.private.join("godot/addons/native/bin/active.dll")).unwrap(),
            b"accepted binary"
        );
    }
    #[test]
    fn private_manifest_drift_refuses_queries_into_changed_preparation() {
        let fixture = Fixture::new();
        let preparation = fixture.preparation();
        Fixture::file(
            &fixture
                .private
                .join("godot/addons/native/bin/active.gdextension"),
            b"changed manifest",
        );
        let calls = AtomicUsize::new(0);
        let query = |input: &Value, _: Duration| {
            calls.fetch_add(1, Ordering::Relaxed);
            fixture.reply(&preparation, input)
        };
        let report = fixture.inspect(&preparation, &query);
        assert_eq!(report["state"], "drift");
        assert_eq!(report["complete"], false);
        assert_eq!(report["reasons"][0], "private_preparation_drift");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn source_edit_during_query_discards_mixed_freshness_evidence() {
        let fixture = Fixture::new();
        let preparation = fixture.preparation();
        let query = |input: &Value, _: Duration| {
            let result = fixture.reply(&preparation, input);
            if input["op"] == "context_dependencies" {
                Fixture::file(&fixture.public.join("rust/game/src/lib.rs"), b"during");
            }
            result
        };
        let report = fixture.inspect(&preparation, &query);
        assert_eq!(report["state"], "drift");
        assert_eq!(report["complete"], false);
        assert_eq!(report["reasons"][0], "file_drift");
        assert!(report["runtime"].is_null());
    }
    #[test]
    fn missing_launch_hashes_or_game_identity_never_qualify_disk_evidence() {
        let fixture = Fixture::new();
        let mut preparation = fixture.preparation();
        preparation.executable_sha256 = None;
        preparation.native_plan["manifests"][0]
            .as_object_mut()
            .unwrap()
            .remove("manifest_sha256");
        let query = |input: &Value, _: Duration| fixture.reply(&preparation, input);
        let report = fixture.inspect(&preparation, &query);
        assert_eq!(report["state"], "incomplete");
        assert_eq!(report["complete"], false);
        assert!(report["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("launch_executable_hash_unrecorded")));
        assert!(report["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("prepared_manifest_hash_unrecorded")));
        let query = |input: &Value, _: Duration| {
            if input["op"] == "context_identity" {
                Err("PRIVATE-QUERY-ERROR".into())
            } else {
                fixture.reply(&preparation, input)
            }
        };
        let report = fixture.inspect(&preparation, &query);
        assert!(report["runtime"]["version"].is_null());
        assert!(!serde_json::to_string(&report)
            .unwrap()
            .contains("PRIVATE-QUERY"));
    }
    #[test]
    fn unavailable_and_busy_are_distinct_without_discovery_or_game_queries() {
        let root = Path::new("//untrusted.invalid/project");
        let input = json!({"op":"runtime_context"});
        assert_eq!(
            inspect(root, &input, Session::Unavailable).unwrap()["state"],
            "unavailable"
        );
        assert_eq!(
            inspect(root, &input, Session::Busy).unwrap()["state"],
            "busy"
        );
        assert!(inspect(
            root,
            &json!({"op":"runtime_context","private_path":"secret"}),
            Session::Unavailable
        )
        .is_err());
    }
    #[cfg(not(windows))]
    #[test]
    fn unsupported_public_ready_refuses_without_source_or_game_queries() {
        struct UnusedSource;
        impl SourceFingerprint for UnusedSource {
            fn fingerprint(
                &self,
                _: &Path,
                _: Instant,
                _: FingerprintLimits,
            ) -> Result<Fingerprint, String> {
                panic!("unsupported runtime context read project sources")
            }
        }
        let root = Path::new("//untrusted.invalid/project");
        let preparation = Preparation {
            owner: OwnershipRecord {
                session: "a".repeat(32),
                executable: root.join("runtime"),
                pid: 12345,
                started: "unverified".into(),
                project: root.to_path_buf(),
                kind: ProcessKind::Game,
            },
            private_source: root.join("private"),
            source_sha256: "0".repeat(64),
            prepared_sha256: "0".repeat(64),
            native_plan: Value::Null,
            executable_sha256: None,
        };
        let query = |_: &Value, _: Duration| -> Result<Value, String> {
            panic!("unsupported runtime context queried a game")
        };
        let ready = || Session::Ready {
            preparation: &preparation,
            query: &query,
        };
        let report = inspect_with_source(
            root,
            &json!({"op":"runtime_context"}),
            ready(),
            &UnusedSource,
        )
        .unwrap();
        assert_eq!(report["state"], "unavailable");
        assert_eq!(report["complete"], false);
        assert_eq!(
            report["reasons"][0],
            "accepted_runtime_platform_unqualified"
        );
        assert_eq!(
            report["evidence"]["source_contents_read_for_hashing"],
            false
        );
        assert!(inspect_with_source(
            root,
            &json!({"op":"runtime_context","private_path":"secret"}),
            ready(),
            &UnusedSource,
        )
        .is_err());
    }
    #[test]
    fn changed_process_creation_identity_refuses_before_or_after_the_same_query() {
        let fixture = Fixture::new();
        let preparation = fixture.preparation();
        let queried = AtomicUsize::new(0);
        let reused = AtomicBool::new(false);
        let query = |input: &Value, _: Duration| {
            queried.fetch_add(1, Ordering::Relaxed);
            reused.store(true, Ordering::Relaxed);
            fixture.reply(&preparation, input)
        };
        let observe = |_: u32| {
            let mut live = Fixture::live(&preparation);
            if reused.load(Ordering::Relaxed) {
                live.started = Some("different-start".into());
            }
            Some(live)
        };
        let report = inspect_inner(
            &fixture.public,
            &json!({"op":"runtime_context"}),
            Session::Ready {
                preparation: &preparation,
                query: &query,
            },
            Some(&BoundedSnapshots),
            observe,
        )
        .unwrap();
        assert_eq!(report["state"], "unhealthy");
        assert_eq!(queried.load(Ordering::Relaxed), 1);
        let report = inspect_inner(
            &fixture.public,
            &json!({"op":"runtime_context"}),
            Session::Ready {
                preparation: &preparation,
                query: &query,
            },
            Some(&BoundedSnapshots),
            |_| None,
        )
        .unwrap();
        assert_eq!(report["state"], "unhealthy");
        assert_eq!(queried.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn external_plan_paths_and_oversized_replies_are_refused_without_returning_them() {
        let fixture = Fixture::new();
        let mut preparation = fixture.preparation();
        preparation.native_plan["manifests"][0]["manifest"] = json!(Fixture::wire(
            &fixture
                .public
                .join("godot/addons/native/bin/active.gdextension")
        ));
        let calls = AtomicUsize::new(0);
        let query = |_: &Value, _: Duration| {
            calls.fetch_add(1, Ordering::Relaxed);
            Err("not reached".into())
        };
        let report = fixture.inspect(&preparation, &query);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(report["reasons"][0], "external_dependency_refused");
        assert!(!serde_json::to_string(&report)
            .unwrap()
            .contains(fixture.public.to_str().unwrap()));
        let preparation = fixture.preparation();
        let query = |_: &Value, _: Duration| {
            Ok(json!({"ok":true,"private_body":"x".repeat(PLAN_BYTES+1),"manifests":[]}))
        };
        let report = fixture.inspect(&preparation, &query);
        assert_eq!(report["reasons"][0], "dependency_reply_byte_limit");
        assert!(fits(&report, REPORT_BYTES));
    }
    #[test]
    fn key_file_presence_refuses_source_hashing_before_game_queries() {
        let fixture = Fixture::new();
        let preparation = fixture.preparation();
        Fixture::file(
            &fixture.public.join("debug.keystore"),
            b"PRIVATE-KEY-SENTINEL",
        );
        let calls = AtomicUsize::new(0);
        let query = |_: &Value, _: Duration| {
            calls.fetch_add(1, Ordering::Relaxed);
            Err("not reached".into())
        };
        let report = fixture.inspect(&preparation, &query);
        assert_eq!(report["complete"], false);
        assert_eq!(report["reasons"][0], "source_fingerprint_unavailable");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(report["evidence"]["recognized_key_files_read"], false);
        assert!(!serde_json::to_string(&report)
            .unwrap()
            .contains("PRIVATE-KEY"));
    }
    #[test]
    fn unloaded_native_resource_is_unhealthy_despite_matching_current_files() {
        let fixture = Fixture::new();
        let preparation = fixture.preparation();
        let query = |input: &Value, _: Duration| {
            let mut result = fixture.reply(&preparation, input)?;
            if input["op"] == "context_dependencies" {
                result["manifests"][0]["loaded"] = json!(false);
            }
            Ok(result)
        };
        let report = fixture.inspect(&preparation, &query);
        assert_eq!(report["state"], "unhealthy");
        assert_eq!(report["complete"], false);
        assert_eq!(report["dependencies"][0]["loaded_resource"], false);
        assert!(report["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("native_extension_not_loaded")));
    }
    #[test]
    fn cancellation_during_query_stays_cancellation_and_has_no_commit_or_state_files() {
        let fixture = Fixture::new();
        let preparation = fixture.preparation();
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(30),
        );
        let query = |input: &Value, _: Duration| {
            context.request_cancel().unwrap();
            fixture.reply(&preparation, input)
        };
        context
            .scope(|| {
                assert!(inspect_inner(
                    &fixture.public,
                    &json!({"op":"runtime_context"}),
                    Session::Ready {
                        preparation: &preparation,
                        query: &query
                    },
                    Some(&BoundedSnapshots),
                    |_| Some(Fixture::live(&preparation))
                )
                .unwrap_err()
                .contains("cancelled"));
            })
            .unwrap();
        assert!(!context.committed());
        assert!(context.commit_status_verified());
        assert!(!fixture.public.join(".aurum").exists());
    }
    #[test]
    fn feature_mismatch_and_loaded_flag_drift_refuse_consistent_context() {
        let fixture = Fixture::new();
        let preparation = fixture.preparation();
        let query = |input: &Value, _: Duration| {
            let mut value = fixture.reply(&preparation, input)?;
            if input["op"] == "context_dependencies" {
                value["features"]["windows"] = json!(false);
            }
            Ok(value)
        };
        assert_eq!(fixture.inspect(&preparation, &query)["state"], "unhealthy");
        let calls = AtomicUsize::new(0);
        let query = |input: &Value, _: Duration| {
            let mut value = fixture.reply(&preparation, input)?;
            if input["op"] == "context_dependencies" && calls.fetch_add(1, Ordering::Relaxed) > 0 {
                value["manifests"][0]["loaded"] = json!(false);
            }
            Ok(value)
        };
        let report = fixture.inspect(&preparation, &query);
        assert_eq!(report["state"], "drift");
        assert_eq!(report["reasons"][0], "runtime_metadata_drift");
    }
    #[test]
    fn compiled_origin_belongs_only_to_selected_library_with_precompiled_vendor_separate() {
        let fixture = Fixture::new();
        let manifest = fixture
            .private
            .join("godot/addons/vendor/bin/vendor.gdextension");
        let library = fixture.private.join("godot/addons/vendor/bin/vendor.dll");
        Fixture::file(&manifest, b"vendor manifest");
        Fixture::file(&library, b"vendor binary");
        let mut preparation = fixture.preparation();
        preparation.native_plan["compiled"] = json!({"package":"game","target":"game_logic","library":preparation.native_plan["manifests"][0]["library"],
            "sha256":preparation.native_plan["manifests"][0]["sha256"],"lock_sha256":"1".repeat(64),"offline":true});
        preparation.native_plan["manifests"].as_array_mut().unwrap().push(json!({
            "manifest":Fixture::wire(&manifest),"resource":"res://addons/vendor/bin/vendor.gdextension","library":Fixture::wire(&library),
            "sha256":crate::sha256_file(&library).unwrap(),"manifest_sha256":crate::sha256_file(&manifest).unwrap(),"dependencies":[],"dependency_hashes":{},"feature_tags":["windows","debug","x86_64"]}));
        let query = |input: &Value, _: Duration| fixture.reply(&preparation, input);
        let report = fixture.inspect(&preparation, &query);
        assert_eq!(report["state"], "available");
        assert_eq!(
            report["dependencies"][0]["provenance"]["origin"],
            "private_cargo_build"
        );
        assert_eq!(
            report["dependencies"][0]["provenance"]["cargo_graph"],
            "not_retained"
        );
        assert_eq!(
            report["dependencies"][1]["provenance"]["origin"],
            "precompiled"
        );
        assert!(report["dependencies"][0]["library"]
            .get("game_reported_disk_sha256")
            .is_none());
    }

    fn compiled_with_graph(fixture: &Fixture) -> Preparation {
        let mut preparation = fixture.preparation();
        let graph = crate::cargo_graph::capture(
            &json!({"version":1,"packages":[{"id":"private-prepared-package","name":"game","version":"1.0.0","source":null}],
                "resolve":{"nodes":[{"id":"private-prepared-package","features":[],"dependencies":[],"deps":[]}]}}),
            "private-prepared-package",
            "x86_64-pc-windows-msvc",
        ).unwrap();
        preparation.native_plan["compiled"] = json!({"package":"game","target":"game_logic","library":preparation.native_plan["manifests"][0]["library"],
            "sha256":preparation.native_plan["manifests"][0]["sha256"],"lock_sha256":"1".repeat(64),"offline":true,"cargo_graph":graph});
        preparation
    }

    #[test]
    fn accepted_context_returns_bounded_retained_graph_summary_without_payload() {
        let fixture = Fixture::new();
        let preparation = compiled_with_graph(&fixture);
        let query = |input: &Value, _: Duration| fixture.reply(&preparation, input);
        let report = fixture.inspect(&preparation, &query);
        assert_eq!(report["state"], "available");
        let graph = &report["dependencies"][0]["provenance"]["cargo_graph"];
        assert_eq!(graph["retained"], true);
        assert_eq!(graph["packages"], 1);
        assert_eq!(graph["selected_transitive_packages"], 1);
        assert_eq!(graph["full_graph_returned"], false);
        assert_eq!(graph["exact_build_units_verified"], false);
        assert!(!report.to_string().contains("private-prepared-package"));
        assert!(graph.get("edges").unwrap().is_number());
        assert_eq!(
            graph["sha256"],
            preparation.native_plan["compiled"]["cargo_graph"]["sha256"]
        );
    }

    #[test]
    fn malformed_retained_graph_refuses_before_any_runtime_query() {
        let fixture = Fixture::new();
        let mut preparation = compiled_with_graph(&fixture);
        preparation.native_plan["compiled"]["cargo_graph"]["sha256"] = json!("0".repeat(64));
        let calls = AtomicUsize::new(0);
        let query = |input: &Value, _: Duration| {
            calls.fetch_add(1, Ordering::Relaxed);
            fixture.reply(&preparation, input)
        };
        let report = fixture.inspect(&preparation, &query);
        assert_eq!(report["state"], "incomplete");
        assert_eq!(report["reasons"][0], "invalid_retained_cargo_graph");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn unmatched_compiled_library_refuses_before_any_runtime_query() {
        let fixture = Fixture::new();
        let mut preparation = compiled_with_graph(&fixture);
        preparation.native_plan["compiled"]["library"] = json!("private/missing.dll");
        preparation.native_plan["compiled"]["offline"] = json!(false);
        let calls = AtomicUsize::new(0);
        let query = |input: &Value, _: Duration| {
            calls.fetch_add(1, Ordering::Relaxed);
            fixture.reply(&preparation, input)
        };
        let report = fixture.inspect(&preparation, &query);
        assert_eq!(report["state"], "incomplete");
        assert_eq!(report["reasons"][0], "compiled_library_selection_mismatch");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}
