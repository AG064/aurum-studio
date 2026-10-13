//! Offline target prerequisites. No installation, process, network or device actions.
use crate::{presets, toolchain, Project};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const METADATA_BYTES: u64 = 64 * 1024;
const PRESET_BYTES: u64 = 256 * 1024;
const DIRECTORY_ENTRIES: usize = 64;
pub const METADATA_BUDGET_MS: u64 = 2_000;
pub const REPORT_BYTES: usize = 32 * 1024;
const PRESET_NAME_BYTES: usize = 1024;
const PATH_SUFFIXES: usize = 16;
const ANDROID_POLICY: &str =
    "https://docs.godotengine.org/en/4.7/tutorials/export/exporting_for_android.html";

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    op: Option<String>,
    platform: String,
    preset: Option<String>,
    debug: Option<bool>,
    gradle: Option<bool>,
    godot_path: Option<String>,
    templates_path: Option<String>,
    android_sdk_path: Option<String>,
    java_sdk_path: Option<String>,
    keystore_path: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Available,
    Missing,
    Mismatch,
    Unverified,
}

#[derive(Serialize)]
struct Check {
    id: &'static str,
    required: bool,
    status: Status,
    path: Option<String>,
    provenance: &'static str,
    expected: Vec<String>,
    observed: Vec<String>,
    detail: &'static str,
}

#[derive(Clone)]
struct Location {
    path: PathBuf,
    provenance: &'static str,
}
impl Location {
    fn display(&self) -> String {
        std::path::absolute(&self.path)
            .ok()
            .map(crate::project::clean_path)
            .unwrap_or_else(|| self.path.clone())
            .to_string_lossy()
            .replace('\\', "/")
    }
}

struct Survey {
    deadline: Instant,
    checks: Vec<Check>,
}
impl Survey {
    fn new() -> Self {
        let local = Instant::now() + Duration::from_millis(METADATA_BUDGET_MS);
        let deadline = crate::operation_context::OperationContext::current()
            .map(|context| context.deadline().min(local))
            .unwrap_or(local);
        Self {
            deadline,
            checks: Vec::new(),
        }
    }
    fn check(&self) -> Result<(), String> {
        if let Some(context) = crate::operation_context::OperationContext::current() {
            context.check().map_err(|error| error.to_string())?;
        }
        if Instant::now() >= self.deadline {
            return Err("target preflight metadata deadline expired".into());
        }
        Ok(())
    }
    fn add(
        &mut self,
        descriptor: (&'static str, bool),
        status: Status,
        location: Option<&Location>,
        expected: &[&str],
        observed: Vec<String>,
        detail: &'static str,
    ) {
        self.checks.push(Check {
            id: descriptor.0,
            required: descriptor.1,
            status,
            path: location.map(Location::display),
            provenance: location.map_or("policy", |location| location.provenance),
            expected: expected.iter().map(|value| (*value).into()).collect(),
            observed,
            detail,
        });
    }
    fn file(
        &mut self,
        id: &'static str,
        required: bool,
        location: Option<&Location>,
    ) -> Result<(), String> {
        self.check()?;
        let status = location.map_or(Status::Missing, |location| {
            match local_inspection(&location.path) {
                Ok(Some(metadata)) if metadata.is_file() && metadata.len() > 0 => Status::Available,
                Ok(Some(_)) => Status::Unverified,
                Ok(None) => Status::Missing,
                Err(()) => Status::Unverified,
            }
        });
        self.add(
            (id, required),
            status,
            location,
            &[],
            Vec::new(),
            "filesystem presence only; executable behavior and signatures were not tested",
        );
        self.check()
    }
}

fn local_path(path: &Path) -> bool {
    let normalized = crate::project::clean_path(path.to_path_buf());
    let path = normalized.as_path();
    let text = path.to_string_lossy();
    if text.starts_with("\\\\") || text.starts_with("//") || text.contains("://") {
        return false;
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDriveTypeW(root: *const u16) -> u32;
        }
        let Ok(absolute) = std::path::absolute(path) else {
            return false;
        };
        let text = absolute.to_string_lossy();
        let bytes = text.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            let root = [bytes[0] as u16, b':' as u16, b'\\' as u16, 0];
            // The local drive root is NUL-terminated and lives through this call.
            if unsafe { GetDriveTypeW(root.as_ptr()) } == 4 {
                return false;
            }
        }
    }
    true
}

fn local_inspection(path: &Path) -> Result<Option<std::fs::Metadata>, ()> {
    if !local_path(path) {
        return Err(());
    }
    let absolute = std::path::absolute(path).map_err(|_| ())?;
    let ancestors: Vec<_> = absolute.ancestors().collect();
    let mut observed = None;
    for ancestor in ancestors.into_iter().rev() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(());
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err(());
                    }
                }
                observed = Some(metadata);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(()),
        }
    }
    Ok(observed)
}
fn local_metadata(path: &Path) -> Option<std::fs::Metadata> {
    local_inspection(path).ok().flatten()
}

fn directory_exists(path: &Path) -> bool {
    local_metadata(path).is_some_and(|metadata| metadata.is_dir())
}
fn file_exists(path: &Path) -> bool {
    local_metadata(path).is_some_and(|metadata| metadata.is_file())
}

fn path_tool(
    environment: &BTreeMap<String, String>,
    name: &str,
    survey: &Survey,
) -> Option<PathBuf> {
    survey.check().ok()?;
    let path = environment.get("PATH")?;
    if path.len() > METADATA_BYTES as usize {
        return None;
    }
    let suffixes: Vec<_> = if cfg!(windows) {
        environment
            .get("PATHEXT")
            .map(String::as_str)
            .unwrap_or(".EXE;.CMD;.BAT;.COM")
            .split(';')
            .filter(|suffix| !suffix.is_empty())
            .take(PATH_SUFFIXES + 1)
            .map(str::to_ascii_lowercase)
            .collect()
    } else {
        vec![String::new()]
    };
    if suffixes.len() > PATH_SUFFIXES
        || suffixes.iter().any(|suffix| {
            suffix.len() > 16
                || (cfg!(windows)
                    && (!suffix.starts_with('.')
                        || suffix[1..]
                            .bytes()
                            .any(|byte| !byte.is_ascii_alphanumeric())))
        })
    {
        return None;
    }
    for directory in std::env::split_paths(path).take(DIRECTORY_ENTRIES) {
        survey.check().ok()?;
        if !local_path(&directory) {
            continue;
        }
        for suffix in std::iter::once("").chain(suffixes.iter().map(String::as_str)) {
            survey.check().ok()?;
            let candidate = directory.join(format!("{name}{suffix}"));
            if file_exists(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn godot_directory(directory: &Path, survey: &Survey) -> Option<PathBuf> {
    survey.check().ok()?;
    if !directory_exists(directory) {
        return None;
    }
    let mut files = Vec::new();
    for (index, entry) in std::fs::read_dir(directory).ok()?.enumerate() {
        survey.check().ok()?;
        if index >= DIRECTORY_ENTRIES {
            return None;
        }
        let path = entry.ok()?.path();
        if file_exists(&path)
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    let name = name.to_ascii_lowercase();
                    name.starts_with("godot") && (!cfg!(windows) || name.ends_with(".exe"))
                })
        {
            files.push(path);
        }
    }
    files.sort_by_key(|path| {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        (usize::from(!name.contains("console")), name.len(), name)
    });
    files.into_iter().next()
}

fn discovered_runtime(
    root: &Path,
    environment: &BTreeMap<String, String>,
    survey: &Survey,
) -> Option<Location> {
    survey.check().ok()?;
    if let Some(location) = env_location(environment, "AURUM_GODOT", "AURUM_GODOT") {
        return Some(location);
    }
    if let Some(home) = environment.get("AURUM_STUDIO_HOME") {
        if let Some(path) = godot_directory(&Path::new(home).join("runtime"), survey) {
            return Some(Location {
                path,
                provenance: "managed_runtime",
            });
        }
    }
    for name in ["godot", "godot4", "Godot"] {
        if let Some(path) = path_tool(environment, name, survey) {
            return Some(Location {
                path,
                provenance: "PATH",
            });
        }
    }
    for directory in root
        .parent()
        .map(|parent| parent.join("godot"))
        .into_iter()
        .chain(Some(root.join("godot")))
    {
        if let Some(path) = godot_directory(&directory, survey) {
            return Some(Location {
                path,
                provenance: "project_convention",
            });
        }
    }
    None
}

fn discovered_sdk(environment: &BTreeMap<String, String>, survey: &Survey) -> Option<Location> {
    let adb = path_tool(environment, "adb", survey)?;
    let root = adb.parent()?.parent()?;
    if directory_exists(&root.join("build-tools")) && directory_exists(&root.join("platforms")) {
        Some(Location {
            path: root.to_path_buf(),
            provenance: "ADB_PATH",
        })
    } else {
        None
    }
}

fn environment() -> BTreeMap<String, String> {
    [
        "AURUM_STUDIO_HOME",
        "AURUM_GODOT",
        "ANDROID_HOME",
        "ANDROID_SDK_ROOT",
        "JAVA_HOME",
        "LOCALAPPDATA",
        "APPDATA",
        "USERPROFILE",
        "HOME",
        "XDG_DATA_HOME",
        "AURUM_EMSDK",
        "PATH",
        "PATHEXT",
        "GODOT_ANDROID_KEYSTORE_DEBUG_PATH",
        "GODOT_ANDROID_KEYSTORE_RELEASE_PATH",
    ]
    .into_iter()
    .filter_map(|key| std::env::var(key).ok().map(|value| (key.into(), value)))
    .collect()
}

fn configured(root: &Path, value: Option<&str>, provenance: &'static str) -> Option<Location> {
    value.map(|value| {
        let path = PathBuf::from(value);
        Location {
            path: if path.is_absolute() {
                path
            } else {
                root.join(path)
            },
            provenance,
        }
    })
}

fn env_location(
    environment: &BTreeMap<String, String>,
    key: &str,
    provenance: &'static str,
) -> Option<Location> {
    environment
        .get(key)
        .filter(|value| !value.is_empty())
        .map(|value| Location {
            path: PathBuf::from(value),
            provenance,
        })
}

fn child(location: &Location, relative: impl AsRef<Path>) -> Location {
    Location {
        path: location.path.join(relative),
        provenance: location.provenance,
    }
}

fn executable(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.into()
    }
}

fn metadata_text(path: &Path, limit: u64, survey: &Survey) -> Option<String> {
    survey.check().ok()?;
    let metadata = local_metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    survey.check().ok()?;
    if bytes.len() as u64 > limit {
        return None;
    }
    String::from_utf8(bytes).ok()
}

pub(crate) fn open_project(root: &Path, deadline: Instant) -> Result<Project, String> {
    let survey = Survey {
        deadline: deadline.min(Survey::new().deadline),
        checks: Vec::new(),
    };
    survey.check()?;
    if !local_inspection(root)
        .ok()
        .flatten()
        .is_some_and(|metadata| metadata.is_dir())
    {
        return Err("preflight project root must be an ordinary local directory".into());
    }
    let root = root
        .canonicalize()
        .map(crate::project::clean_path)
        .map_err(|_| "preflight project root could not be resolved")?;
    survey.check()?;
    if !directory_exists(&root) {
        return Err("preflight project root changed during discovery".into());
    }
    let text = metadata_text(&root.join("aurum.toml"), METADATA_BYTES, &survey)
        .ok_or("preflight requires a readable local aurum.toml of at most 64 KiB")?;
    let config = crate::project::ProjectConfig::parse(&text).map_err(|error| error.to_string())?;
    if config
        .godot_version
        .as_ref()
        .is_some_and(|version| version.len() > 128 || version.chars().any(char::is_control))
    {
        return Err("configured Godot version exceeds preflight metadata bounds".into());
    }
    let direct = root.join("project.godot");
    let godot_project = match local_inspection(&direct) {
        Ok(Some(metadata)) if metadata.is_file() => Some(direct),
        Err(()) => return Err("preflight Godot project must be an ordinary local file".into()),
        _ => {
            let mut candidates = Vec::new();
            for directory in directories(&root, &survey)? {
                survey.check()?;
                if !directory_exists(&directory) {
                    continue;
                }
                let path = directory.join("project.godot");
                if file_exists(&path) {
                    candidates.push(path);
                }
            }
            candidates.sort();
            candidates.into_iter().next()
        }
    };
    survey.check()?;
    let godot_project =
        godot_project.ok_or("target preflight requires a confined project.godot")?;
    Ok(Project {
        root,
        config,
        layout: crate::project::Layout {
            godot_project: Some(godot_project),
            ..crate::project::Layout::default()
        },
    })
}

fn property(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        if name.trim() != key {
            return None;
        }
        let value = value.trim().trim_matches('"');
        (!value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'+')))
        .then(|| value.to_string())
    })
}

fn numeric_version(value: &str) -> Option<Vec<u32>> {
    value
        .split('+')
        .next()?
        .split('.')
        .map(|part| part.parse::<u32>().ok())
        .collect()
}

fn preset_options(text: &str, name: &str) -> Option<BTreeMap<String, String>> {
    let mut section = "";
    let mut names = BTreeMap::new();
    let mut options: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
        {
            section = header;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if let Some(index) = section
            .strip_prefix("preset.")
            .filter(|index| !index.contains('.'))
        {
            if key.trim() == "name" {
                names.insert(
                    index.to_string(),
                    value.trim().trim_matches('"').to_string(),
                );
            }
        } else if let Some(index) = section
            .strip_prefix("preset.")
            .and_then(|index| index.strip_suffix(".options"))
        {
            let key = key.trim();
            if !matches!(
                key,
                "gradle_build/use_gradle_build"
                    | "custom_template/debug"
                    | "custom_template/release"
                    | "keystore/debug"
                    | "keystore/release"
                    | "binary_format/architecture"
            ) {
                continue;
            }
            let value = value.trim();
            let decoded = if key == "gradle_build/use_gradle_build" {
                if !matches!(value, "true" | "false") {
                    return None;
                }
                value.to_string()
            } else {
                serde_json::from_str::<String>(value).ok()?
            };
            if decoded.len() > 4096 || decoded.chars().any(char::is_control) {
                return None;
            }
            options
                .entry(index.to_string())
                .or_default()
                .insert(key.to_string(), decoded);
        }
    }
    let index = names.iter().find(|(_, candidate)| *candidate == name)?.0;
    Some(options.remove(index).unwrap_or_default())
}

fn runtime_tag(version: &str) -> Option<String> {
    if version.len() > 128 {
        return None;
    }
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() < 3 || parts[0].parse::<u32>().is_err() || parts[1].parse::<u32>().is_err() {
        return None;
    }
    let status = if parts[2].parse::<u32>().is_ok() {
        3
    } else {
        2
    };
    if parts.get(status).copied() != Some("stable") {
        return None;
    }
    Some(parts[..=status].join("."))
}

fn platform_matches(preset: &str, platform: &str) -> bool {
    preset.eq_ignore_ascii_case(platform)
        || (platform == "Linux" && preset.eq_ignore_ascii_case("Linux/X11"))
}

fn template_root(location: Location, tag: Option<&str>) -> Location {
    if let Some(tag) = tag {
        if location
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(runtime_tag)
            .is_none()
        {
            return child(&location, tag);
        }
    }
    location
}

fn discovered_templates(
    environment: &BTreeMap<String, String>,
    tag: Option<&str>,
    template_name: &str,
    gradle_source: bool,
    survey: &Survey,
) -> Result<Option<Location>, String> {
    let candidates = [
        env_location(environment, "AURUM_STUDIO_HOME", "managed_runtime")
            .map(|root| child(&root, "runtime/templates")),
        env_location(environment, "APPDATA", "conventional")
            .map(|root| child(&root, "Godot/export_templates")),
        env_location(environment, "XDG_DATA_HOME", "conventional")
            .map(|root| child(&root, "godot/export_templates")),
        env_location(environment, "HOME", "conventional")
            .map(|root| child(&root, ".local/share/godot/export_templates")),
    ];
    let mut fallback = None;
    for candidate in candidates.into_iter().flatten() {
        survey.check()?;
        let candidate = template_root(candidate, tag);
        fallback.get_or_insert_with(|| candidate.clone());
        let template = local_metadata(&candidate.path.join(template_name));
        let source = !gradle_source
            || local_metadata(&candidate.path.join("android_source.zip"))
                .is_some_and(|metadata| metadata.is_file() && metadata.len() > 0);
        if template.is_some_and(|metadata| metadata.is_file() && metadata.len() > 0) && source {
            survey.check()?;
            return Ok(Some(candidate));
        }
    }
    survey.check()?;
    Ok(fallback)
}

fn directories(path: &Path, survey: &Survey) -> Result<Vec<PathBuf>, String> {
    survey.check()?;
    if !directory_exists(path) {
        return Ok(Vec::new());
    }
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Ok(Vec::new()),
    };
    let mut found = Vec::new();
    for (index, entry) in entries.enumerate() {
        survey.check()?;
        if index >= DIRECTORY_ENTRIES {
            return Err("target preflight directory inventory exceeds 64 entries".into());
        }
        let entry = entry.map_err(|error| error.to_string())?;
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        if kind.is_dir() && !kind.is_symlink() {
            found.push(entry.path());
        }
    }
    found.sort();
    survey.check()?;
    Ok(found)
}

fn package_versions(path: &Path, survey: &Survey) -> Result<(Vec<String>, bool), String> {
    let mut values = Vec::new();
    let mut mismatched = false;
    for directory in directories(path, survey)? {
        if let Some(value) =
            metadata_text(&directory.join("source.properties"), METADATA_BYTES, survey)
                .and_then(|text| property(&text, "Pkg.Revision"))
        {
            if directory.file_name().and_then(|name| name.to_str()) == Some(value.as_str()) {
                values.push(value);
            } else {
                mismatched = true;
            }
        }
    }
    values.sort();
    values.dedup();
    Ok((values, mismatched))
}

fn exact_package(
    survey: &mut Survey,
    id: &'static str,
    required: bool,
    location: &Location,
    expected: Option<&'static str>,
) -> Result<Vec<String>, String> {
    let (versions, mismatched) = package_versions(&location.path, survey)?;
    let status = match expected {
        Some(expected) if versions.iter().any(|value| value == expected) => Status::Available,
        Some(_) if mismatched || !versions.is_empty() => Status::Mismatch,
        Some(_) if directory_exists(&location.path) => Status::Unverified,
        Some(_) => Status::Missing,
        None => Status::Unverified,
    };
    survey.add(
        (id, required),
        status,
        Some(location),
        &expected.into_iter().collect::<Vec<_>>(),
        versions.clone(),
        "installed metadata only; unknown runtime versions do not inherit Android 4.7 requirements",
    );
    Ok(versions)
}

/// Reusable CLI/HTTP/MCP report. This function does not run tools or query ADB.
pub fn inspect(project: &Project, input: &Value) -> Result<Value, String> {
    inspect_with(project, input, &environment(), observed_runtime_version)
}

/// Uses one deadline for confined project discovery and the complete report.
pub fn inspect_root(root: &Path, input: &Value) -> Result<Value, String> {
    let survey = Survey::new();
    survey.check()?;
    parse_request(input)?;
    let project =
        Project::open_preflight(root, survey.deadline).map_err(|error| error.to_string())?;
    inspect_with_survey(
        &project,
        input,
        &environment(),
        observed_runtime_version,
        survey,
    )
}

fn inspect_with(
    project: &Project,
    input: &Value,
    environment: &BTreeMap<String, String>,
    observe_runtime: impl FnOnce(&Path) -> Option<String>,
) -> Result<Value, String> {
    inspect_with_survey(project, input, environment, observe_runtime, Survey::new())
}

fn parse_request(input: &Value) -> Result<Request, String> {
    if serde_json::to_vec(input)
        .map_err(|_| "invalid preflight request")?
        .len()
        > REPORT_BYTES
    {
        return Err("target preflight request exceeds 32 KiB".into());
    }
    let request: Request =
        serde_json::from_value(input.clone()).map_err(|error| error.to_string())?;
    if request
        .op
        .as_deref()
        .is_some_and(|op| op != "target_preflight")
    {
        return Err("target preflight received a different operation".into());
    }
    for value in [
        request.preset.as_deref(),
        request.godot_path.as_deref(),
        request.templates_path.as_deref(),
        request.android_sdk_path.as_deref(),
        request.java_sdk_path.as_deref(),
        request.keystore_path.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(
                "preflight paths and preset names require 1..4096 bytes without control characters"
                    .into(),
            );
        }
    }
    for path in [
        request.godot_path.as_deref(),
        request.templates_path.as_deref(),
        request.android_sdk_path.as_deref(),
        request.java_sdk_path.as_deref(),
        request.keystore_path.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if !local_path(Path::new(path)) {
            return Err("target preflight accepts local filesystem paths only".into());
        }
    }
    if request
        .preset
        .as_ref()
        .is_some_and(|name| name.len() > PRESET_NAME_BYTES)
    {
        return Err("target preflight preset names exceed 1024 bytes".into());
    }
    if !crate::project_ops::EXPORT_PLATFORMS
        .iter()
        .any(|platform| platform.eq_ignore_ascii_case(&request.platform))
    {
        return Err("unknown export target platform".into());
    }
    Ok(request)
}

fn inspect_with_survey(
    project: &Project,
    input: &Value,
    environment: &BTreeMap<String, String>,
    observe_runtime: impl FnOnce(&Path) -> Option<String>,
    mut survey: Survey,
) -> Result<Value, String> {
    survey.check()?;
    let request = parse_request(input)?;
    let platform = crate::project_ops::EXPORT_PLATFORMS
        .iter()
        .find(|platform| platform.eq_ignore_ascii_case(&request.platform))
        .copied()
        .ok_or("unknown export target platform")?;
    survey.check()?;
    let runtime = configured(&project.root, request.godot_path.as_deref(), "configured")
        .or_else(|| discovered_runtime(&project.root, environment, &survey));
    survey.file("runtime", true, runtime.as_ref())?;
    let version = runtime
        .as_ref()
        .filter(|runtime| file_exists(&runtime.path))
        .and_then(|runtime| observe_runtime(&runtime.path));
    survey.check()?;
    let tag = version.as_deref().and_then(runtime_tag);
    let version_status = match version.as_deref() {
        Some(version)
            if project
                .config
                .godot_version
                .as_deref()
                .is_some_and(|required| !toolchain::version_satisfies(Some(version), required)) =>
        {
            Status::Mismatch
        }
        Some(_) if tag.is_some() => Status::Available,
        _ => Status::Unverified,
    };
    survey.add(("runtime_version", true), version_status, runtime.as_ref(),
        &project.config.godot_version.as_deref().into_iter().collect::<Vec<_>>(),
        version.clone().into_iter().collect(),
        "runtime metadata observation; configured requirements and filenames are not runtime version evidence");

    let godot = project
        .godot_project_dir()
        .ok_or("target preflight requires project.godot")?;
    let preset_text = metadata_text(&godot.join(presets::PRESETS_FILE), PRESET_BYTES, &survey);
    let parsed = preset_text
        .as_deref()
        .map(presets::parse)
        .unwrap_or_default();
    let selected = parsed.presets.iter().find(|preset| {
        request.preset.as_deref().map_or_else(
            || platform_matches(&preset.platform, platform),
            |name| preset.name == name,
        )
    });
    if selected.is_some_and(|preset| {
        preset.name.len() > PRESET_NAME_BYTES || preset.name.chars().any(char::is_control)
    }) {
        return Err("selected export preset name exceeds the metadata report bounds".into());
    }
    let preset_status = if preset_text.is_none() && file_exists(&godot.join(presets::PRESETS_FILE))
    {
        Status::Unverified
    } else if selected.is_none() {
        Status::Missing
    } else if selected.is_some_and(|preset| !platform_matches(&preset.platform, platform)) {
        Status::Mismatch
    } else if parsed.unreadable_lines != 0 {
        Status::Unverified
    } else {
        Status::Available
    };
    let preset_location = Location {
        path: godot.join(presets::PRESETS_FILE),
        provenance: "project",
    };
    survey.add(("preset", true), preset_status, Some(&preset_location), &[platform],
        selected.map(|preset| preset.name.clone()).into_iter().collect(),
        "preset headers and selected non-secret options; passwords and private keys are not returned or tested");
    let options = selected.and_then(|preset| {
        preset_text
            .as_deref()
            .and_then(|text| preset_options(text, &preset.name))
    });
    if selected.is_some() && options.is_none() {
        survey.add(
            ("preset_options", true),
            Status::Unverified,
            Some(&preset_location),
            &[],
            Vec::new(),
            "selected export options could not be decoded without guessing",
        );
    }
    let options = options.unwrap_or_default();
    let debug = request.debug.unwrap_or(true);
    let profile = if debug { "debug" } else { "release" };
    let architecture = options
        .get("binary_format/architecture")
        .map(String::as_str)
        .unwrap_or(match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86" => "x86_32",
            "arm" => "arm32",
            other => other,
        });
    if matches!(platform, "Windows Desktop" | "Linux")
        && !matches!(architecture, "x86_64" | "x86_32" | "arm64" | "arm32")
    {
        survey.add(
            ("target_architecture", true),
            Status::Unverified,
            Some(&preset_location),
            &[],
            Vec::new(),
            "selected desktop architecture does not have a known template mapping",
        );
    }
    let template_name = match platform {
        "Android" => format!("android_{profile}.apk"),
        "Web" if project.config.rust_package.is_some() => {
            format!("web_dlink_nothreads_{profile}.zip")
        }
        "Web" => format!("web_nothreads_{profile}.zip"),
        "Windows Desktop" => format!("windows_{profile}_{architecture}.exe"),
        "Linux" => format!("linux_{profile}.{architecture}"),
        "macOS" => "macos.zip".into(),
        "iOS" => "ios.zip".into(),
        _ => return Err("target template policy is not implemented for this platform".into()),
    };
    let templates = if let Some(location) = configured(
        &project.root,
        request.templates_path.as_deref(),
        "configured",
    ) {
        if let (Some(tag), Some(observed)) = (
            tag.as_ref(),
            location
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(runtime_tag),
        ) {
            if observed != *tag {
                survey.add(
                    ("template_version", true),
                    Status::Mismatch,
                    Some(&location),
                    &[tag],
                    vec![observed],
                    "configured template version differs from observed runtime version",
                );
            }
        }
        Some(template_root(location, tag.as_deref()))
    } else {
        let gradle_source = platform == "Android"
            && request
                .gradle
                .or_else(|| {
                    options
                        .get("gradle_build/use_gradle_build")
                        .map(|value| value == "true")
                })
                .unwrap_or(false);
        discovered_templates(
            environment,
            tag.as_deref(),
            &template_name,
            gradle_source,
            &survey,
        )?
    };
    let template_file = templates
        .as_ref()
        .map(|location| child(location, &template_name));
    let custom = options
        .get(if debug {
            "custom_template/debug"
        } else {
            "custom_template/release"
        })
        .filter(|value| !value.is_empty())
        .and_then(|path| {
            configured(
                godot,
                Some(path.strip_prefix("res://").unwrap_or(path)),
                "preset",
            )
        });
    if let Some(custom) = custom {
        survey.file("custom_export_template_presence", false, Some(&custom))?;
        survey.add(
            ("export_template", true),
            Status::Unverified,
            Some(&custom),
            &[],
            Vec::new(),
            "custom template compatibility requires version-matched template validation",
        );
    } else if tag.is_some() {
        survey.file("export_template", true, template_file.as_ref())?;
    } else {
        survey.add(
            ("export_template", true),
            Status::Unverified,
            template_file.as_ref(),
            &[],
            Vec::new(),
            "runtime version is unknown; no template version directory was inferred",
        );
    }
    if platform == "Android" {
        android(
            &mut survey,
            project,
            &request,
            environment,
            version.as_deref(),
            templates.as_ref(),
            &options,
        )?;
    } else if platform == "iOS" {
        survey.add(("ios_host_toolchain", true), Status::Unverified, None, &["macOS", "Xcode"],
            vec![std::env::consts::OS.into()], "Apple toolchain and signing qualification remain separate; no physical iOS readiness claimed");
    } else if platform == "Web" && project.config.rust_package.is_some() {
        survey.add(
            ("rust_web_toolchain", true),
            Status::Unverified,
            None,
            &[],
            Vec::new(),
            "Rust Web SDK/compiler compatibility must be checked by the managed Web build path",
        );
    }
    survey.check()?;
    let prerequisites = survey
        .checks
        .iter()
        .filter(|check| check.required)
        .all(|check| check.status == Status::Available);
    let report = json!({
        "ok":true,"report_version":1,"platform":platform,"debug":debug,"metadata_budget_ms":METADATA_BUDGET_MS,
        "prerequisites_detected":prerequisites,"export_readiness":"unverified","deployed":false,"signing_verified":false,
        "physical_readiness":"unverified","device_query_performed":false,"devices":null,
        "runtime_version":version,"runtime_version_evidence":if cfg!(windows) {"Windows version resource"} else {"unavailable without an explicit tool probe"},
        "checks":survey.checks,"read_only":true,"tools_invoked":false,"network_used":false,
        "private_keys_read":false,"installed":false,"workspace_changed":false,"device_changed":false,
        "freshness":"point-in-time metadata; no atomic filesystem snapshot or execution validation",
        "scope":"metadata availability only; no export readiness, signing, deployment, launch, input or performance proof"
    });
    if serde_json::to_vec(&report)
        .map_err(|error| error.to_string())?
        .len()
        > REPORT_BYTES
    {
        return Err("target preflight report exceeds 32 KiB; use explicit tool paths".into());
    }
    survey.check()?;
    Ok(report)
}

fn android(
    survey: &mut Survey,
    project: &Project,
    request: &Request,
    environment: &BTreeMap<String, String>,
    version: Option<&str>,
    templates: Option<&Location>,
    options: &BTreeMap<String, String>,
) -> Result<(), String> {
    let policy = version
        .and_then(runtime_tag)
        .is_some_and(|tag| tag == "4.7.stable" || tag.starts_with("4.7."));
    survey.add(
        ("android_requirement_policy", true),
        if policy {
            Status::Available
        } else {
            Status::Unverified
        },
        None,
        &[],
        if policy {
            vec!["Godot 4.7".into(), ANDROID_POLICY.into()]
        } else {
            Vec::new()
        },
        "only observed stable Godot 4.7 has a version-pinned Android policy",
    );
    let sdk = configured(
        &project.root,
        request.android_sdk_path.as_deref(),
        "configured",
    )
    .or_else(|| env_location(environment, "ANDROID_SDK_ROOT", "ANDROID_SDK_ROOT"))
    .or_else(|| env_location(environment, "ANDROID_HOME", "ANDROID_HOME"))
    .or_else(|| {
        env_location(environment, "AURUM_STUDIO_HOME", "managed_runtime")
            .map(|location| child(&location, "runtime/android-sdk"))
            .filter(|location| directory_exists(&location.path))
    })
    .or_else(|| discovered_sdk(environment, survey))
    .or_else(|| {
        env_location(environment, "LOCALAPPDATA", "conventional")
            .map(|location| child(&location, "Android/Sdk"))
    });
    let sdk_status = match sdk.as_ref().map(|sdk| local_inspection(&sdk.path)) {
        Some(Ok(Some(metadata))) if metadata.is_dir() => Status::Available,
        Some(Err(())) => Status::Unverified,
        Some(Ok(Some(_))) => Status::Mismatch,
        _ => Status::Missing,
    };
    if request.android_sdk_path.is_none() {
        if let (Some(root), Some(home)) = (
            env_location(environment, "ANDROID_SDK_ROOT", "ANDROID_SDK_ROOT"),
            env_location(environment, "ANDROID_HOME", "ANDROID_HOME"),
        ) {
            let equal = if cfg!(windows) {
                root.display().eq_ignore_ascii_case(&home.display())
            } else {
                root.display() == home.display()
            };
            if !equal {
                survey.add(
                    ("android_sdk_environment", true),
                    Status::Mismatch,
                    Some(&root),
                    &[],
                    Vec::new(),
                    "ANDROID_SDK_ROOT and ANDROID_HOME disagree; provide an explicit SDK path",
                );
            }
        }
    }
    survey.add(("android_sdk", true), sdk_status, sdk.as_ref(), &[], Vec::new(),
        "configured locations are authoritative; missing SDK configuration does not mean every SDK on this computer is absent");
    let java = configured(
        &project.root,
        request.java_sdk_path.as_deref(),
        "configured",
    )
    .or_else(|| env_location(environment, "JAVA_HOME", "JAVA_HOME"))
    .or_else(|| {
        path_tool(environment, "java", survey).and_then(|path| {
            path.parent()?.parent().map(|root| Location {
                path: root.to_path_buf(),
                provenance: "PATH",
            })
        })
    });
    let java_version = java.as_ref().and_then(|location| {
        metadata_text(&location.path.join("release"), METADATA_BYTES, survey)
            .and_then(|text| property(&text, "JAVA_VERSION"))
    });
    let java_status = match java_version.as_deref().and_then(numeric_version) {
        Some(parts) if policy && parts.first().is_some_and(|major| *major >= 17) => {
            Status::Available
        }
        Some(_) if policy => Status::Mismatch,
        _ => Status::Unverified,
    };
    survey.add(
        ("java_version", true),
        java_status,
        java.as_ref(),
        if policy { &["17 or newer"] } else { &[] },
        java_version.into_iter().collect(),
        "JDK release metadata only; Godot 4.7 recommends JDK 17 and also supports higher versions",
    );
    for name in ["java", "keytool", "jarsigner"] {
        let location = java
            .as_ref()
            .map(|java| child(java, Path::new("bin").join(executable(name))));
        survey.file(
            match name {
                "java" => "java",
                "keytool" => "keytool",
                _ => "jarsigner",
            },
            true,
            location.as_ref(),
        )?;
    }
    let adb = sdk
        .as_ref()
        .map(|sdk| child(sdk, Path::new("platform-tools").join(executable("adb"))))
        .filter(|location| file_exists(&location.path))
        .or_else(|| {
            path_tool(environment, "adb", survey).map(|path| Location {
                path,
                provenance: "PATH",
            })
        });
    survey.file("adb", true, adb.as_ref())?;
    if let Some(sdk) = sdk {
        let platform_tools = child(&sdk, "platform-tools");
        let version = metadata_text(
            &platform_tools.path.join("source.properties"),
            METADATA_BYTES,
            survey,
        )
        .and_then(|text| property(&text, "Pkg.Revision"));
        let status = match version.as_deref().and_then(numeric_version) {
            Some(parts) if policy && parts >= vec![35, 0, 0] => Status::Available,
            Some(_) if policy => Status::Mismatch,
            _ => Status::Unverified,
        };
        survey.add(
            ("platform_tools_version", true),
            status,
            Some(&platform_tools),
            if policy { &["35.0.0 or newer"] } else { &[] },
            version.into_iter().collect(),
            "SDK package metadata; no ADB client or server was started",
        );
        let build_tools = child(&sdk, "build-tools");
        let versions = exact_package(
            survey,
            "android_build_tools",
            true,
            &build_tools,
            policy.then_some("35.0.1"),
        )?;
        let tool_version = if policy && versions.iter().any(|version| version == "35.0.1") {
            Some("35.0.1".to_string())
        } else {
            versions
                .iter()
                .filter_map(|version| numeric_version(version).map(|parts| (parts, version)))
                .max_by(|left, right| left.0.cmp(&right.0))
                .map(|(_, version)| version.clone())
        };
        for (id, name) in [
            (
                "apksigner",
                if cfg!(windows) {
                    "apksigner.bat"
                } else {
                    "apksigner"
                },
            ),
            (
                "zipalign",
                if cfg!(windows) {
                    "zipalign.exe"
                } else {
                    "zipalign"
                },
            ),
            ("aapt2", if cfg!(windows) { "aapt2.exe" } else { "aapt2" }),
        ] {
            let location = tool_version
                .as_ref()
                .map(|version| child(&build_tools, Path::new(version).join(name)));
            survey.file(id, true, location.as_ref())?;
        }
        let platforms = child(&sdk, "platforms");
        let mut apis = Vec::new();
        let mut mismatched = false;
        let mut incomplete = false;
        for directory in directories(&platforms.path, survey)? {
            if let Some(api) =
                metadata_text(&directory.join("source.properties"), METADATA_BYTES, survey)
                    .and_then(|text| property(&text, "AndroidVersion.ApiLevel"))
            {
                if directory.file_name().and_then(|name| name.to_str())
                    != Some(format!("android-{api}").as_str())
                {
                    mismatched = true;
                } else if local_metadata(&directory.join("android.jar"))
                    .is_some_and(|metadata| metadata.is_file() && metadata.len() > 0)
                {
                    apis.push(api);
                } else {
                    incomplete = true;
                }
            }
        }
        apis.sort();
        let status = if !policy {
            Status::Unverified
        } else if apis.iter().any(|api| api == "35") {
            Status::Available
        } else if mismatched || !apis.is_empty() {
            Status::Mismatch
        } else if incomplete {
            Status::Unverified
        } else {
            Status::Missing
        };
        survey.add(
            ("android_platform", true),
            status,
            Some(&platforms),
            if policy { &["35"] } else { &[] },
            apis,
            "matching platform directory, API metadata and android.jar presence; no repository lookup or SDK installation",
        );
        let commandline = child(
            &sdk,
            if cfg!(windows) {
                "cmdline-tools/latest/bin/sdkmanager.bat"
            } else {
                "cmdline-tools/latest/bin/sdkmanager"
            },
        );
        survey.file("sdkmanager", true, Some(&commandline))?;
        let commandline_metadata = child(&sdk, "cmdline-tools/latest/source.properties");
        let commandline_version = metadata_text(&commandline_metadata.path, METADATA_BYTES, survey)
            .and_then(|text| property(&text, "Pkg.Revision"));
        survey.add(
            ("android_commandline_tools_version", false),
            if commandline_version.is_some() {
                Status::Available
            } else {
                Status::Unverified
            },
            Some(&commandline_metadata),
            &[],
            commandline_version.into_iter().collect(),
            "local package version only; online freshness was not checked",
        );
        let native = project.config.rust_package.is_some();
        exact_package(
            survey,
            "android_ndk",
            native,
            &child(&sdk, "ndk"),
            policy.then_some("28.1.13356709"),
        )?;
        exact_package(
            survey,
            "android_cmake",
            native,
            &child(&sdk, "cmake"),
            policy.then_some("3.10.2.4988404"),
        )?;
        if native {
            survey.add(("rust_android_artifacts", true), Status::Unverified, None, &[], Vec::new(),
                "Android Rust targets, ABI-specific GDExtensions and cross-linker compatibility need separate native build qualification");
        }
    }
    let preset_gradle = options
        .get("gradle_build/use_gradle_build")
        .map(|value| value == "true");
    if request
        .gradle
        .zip(preset_gradle)
        .is_some_and(|(requested, configured)| requested != configured)
    {
        survey.add(
            ("android_export_mode", true),
            Status::Mismatch,
            None,
            &[],
            Vec::new(),
            "requested Gradle mode differs from the selected export preset",
        );
    }
    if request.gradle.or(preset_gradle).unwrap_or(false) {
        let source = templates.map(|templates| child(templates, "android_source.zip"));
        survey.file("android_gradle_template", true, source.as_ref())?;
    }
    let keystore = configured(
        &project.root,
        request.keystore_path.as_deref(),
        "configured",
    )
    .or_else(|| {
        env_location(
            environment,
            if request.debug.unwrap_or(true) {
                "GODOT_ANDROID_KEYSTORE_DEBUG_PATH"
            } else {
                "GODOT_ANDROID_KEYSTORE_RELEASE_PATH"
            },
            "signing_environment",
        )
    })
    .or_else(|| {
        options
            .get(if request.debug.unwrap_or(true) {
                "keystore/debug"
            } else {
                "keystore/release"
            })
            .filter(|value| !value.is_empty())
            .and_then(|value| {
                configured(
                    project.godot_project_dir()?,
                    Some(value.strip_prefix("res://").unwrap_or(value)),
                    "preset",
                )
            })
    })
    .or_else(|| {
        if request.debug.unwrap_or(true) {
            env_location(environment, "USERPROFILE", "conventional")
                .or_else(|| env_location(environment, "HOME", "conventional"))
                .map(|location| child(&location, ".android/debug.keystore"))
        } else {
            None
        }
    });
    survey.file("signing_keystore_presence", true, keystore.as_ref())?;
    survey.add(
        ("signing_verification", false),
        Status::Unverified,
        None,
        &[],
        Vec::new(),
        "keystore contents, passwords, key aliases and signing credentials were not read or tested",
    );
    Ok(())
}

#[cfg(not(windows))]
fn observed_runtime_version(_path: &Path) -> Option<String> {
    None
}

#[cfg(windows)]
fn observed_runtime_version(path: &Path) -> Option<String> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    if path
        .extension()
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("exe"))
    {
        return None;
    }
    #[link(name = "version")]
    unsafe extern "system" {
        fn GetFileVersionInfoSizeW(path: *const u16, handle: *mut u32) -> u32;
        fn GetFileVersionInfoW(
            path: *const u16,
            handle: u32,
            length: u32,
            data: *mut c_void,
        ) -> i32;
        fn VerQueryValueW(
            data: *const c_void,
            query: *const u16,
            value: *mut *mut c_void,
            length: *mut u32,
        ) -> i32;
    }
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut handle = 0;
    // Both pointers reference live buffers; Windows only reads the path.
    let size = unsafe { GetFileVersionInfoSizeW(path.as_ptr(), &mut handle) };
    if size == 0 || size > 1024 * 1024 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    // The byte buffer has the exact checked capacity requested from Windows.
    if unsafe { GetFileVersionInfoW(path.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
        return None;
    }
    let query = |name: &str| -> Option<(*mut c_void, usize)> {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let mut value = std::ptr::null_mut();
        let mut length = 0;
        // Output pointers are live; returned offsets are checked before access.
        if unsafe { VerQueryValueW(data.as_ptr().cast(), name.as_ptr(), &mut value, &mut length) }
            == 0
        {
            return None;
        }
        Some((value, length as usize))
    };
    let slice = |pointer: *mut c_void, length: usize| -> Option<&[u8]> {
        let offset = (pointer as usize).checked_sub(data.as_ptr() as usize)?;
        let end = offset.checked_add(length)?;
        data.get(offset..end)
    };
    let mut languages = vec![(0x0409u16, 0x04b0u16), (0x0409, 0x04e4)];
    if let Some((pointer, length)) = query("\\VarFileInfo\\Translation") {
        if let Some(bytes) = slice(pointer, length.min(32)) {
            for block in bytes.as_chunks::<4>().0 {
                languages.insert(
                    0,
                    (
                        u16::from_le_bytes([block[0], block[1]]),
                        u16::from_le_bytes([block[2], block[3]]),
                    ),
                );
            }
        }
    }
    for (language, codepage) in languages {
        let (pointer, length) = match query(&format!(
            "\\StringFileInfo\\{language:04x}{codepage:04x}\\ProductVersion"
        )) {
            Some(value) => value,
            None => continue,
        };
        if length == 0 || length > 129 {
            continue;
        }
        let bytes = slice(pointer, length.checked_mul(2)?)?;
        let words: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|block| u16::from_le_bytes([block[0], block[1]]))
            .take_while(|word| *word != 0)
            .collect();
        let version = String::from_utf16(&words).ok()?;
        if version.len() <= 128
            && version.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-' | b'_')
            })
        {
            let mut parts = version.split('.');
            if parts.next().is_some_and(|part| part.parse::<u32>().is_ok())
                && parts.next().is_some_and(|part| part.parse::<u32>().is_ok())
            {
                return Some(version);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(Project);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "aurum-target-preflight-{}",
                crate::random::session_id()
            ));
            std::fs::create_dir(&root).unwrap();
            std::fs::write(
                root.join("aurum.toml"),
                "schema_version=1\nname=\"preflight\"\ngodot_version=\"4.7\"\nmodules=[]\n",
            )
            .unwrap();
            std::fs::write(root.join("project.godot"), "config_version=5\n").unwrap();
            std::fs::write(
                root.join(presets::PRESETS_FILE),
                "[preset.0]\nname=\"Android\"\nplatform=\"Android\"\n",
            )
            .unwrap();
            Self(Project::open(&root).unwrap())
        }
        fn file(&self, path: &str, text: &str) {
            let path = self.0.root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        fn complete(&self) -> Value {
            self.file("godot.exe", "fake runtime");
            self.file("templates/4.7.stable/android_debug.apk", "fake template");
            for tool in ["java", "keytool", "jarsigner"] {
                self.file(&format!("jdk/bin/{}", executable(tool)), "fake tool");
            }
            self.file("jdk/release", "JAVA_VERSION=\"17.0.20.1\"\n");
            self.file(
                "sdk/platform-tools/source.properties",
                "Pkg.Revision=37.0.1\n",
            );
            self.file(
                &format!("sdk/platform-tools/{}", executable("adb")),
                "fake tool",
            );
            self.file(
                "sdk/build-tools/35.0.1/source.properties",
                "Pkg.Revision=35.0.1\n",
            );
            for tool in ["apksigner", "zipalign", "aapt2"] {
                let name = if cfg!(windows) {
                    if tool == "apksigner" {
                        "apksigner.bat".into()
                    } else {
                        executable(tool)
                    }
                } else {
                    tool.into()
                };
                self.file(&format!("sdk/build-tools/35.0.1/{name}"), "fake tool");
            }
            self.file(
                "sdk/platforms/android-35/source.properties",
                "AndroidVersion.ApiLevel=35\nPkg.Revision=2\n",
            );
            self.file("sdk/platforms/android-35/android.jar", "fake API jar");
            self.file(
                if cfg!(windows) {
                    "sdk/cmdline-tools/latest/bin/sdkmanager.bat"
                } else {
                    "sdk/cmdline-tools/latest/bin/sdkmanager"
                },
                "fake tool",
            );
            self.file("debug.keystore", "PRIVATE-KEY-CONTENT-SENTINEL");
            json!({"op":"target_preflight","platform":"Android","preset":"Android","godot_path":"godot.exe",
                "templates_path":"templates","android_sdk_path":"sdk","java_sdk_path":"jdk","keystore_path":"debug.keystore"})
        }
        fn inspect(&self, input: &Value, version: Option<&str>) -> Value {
            inspect_with(&self.0, input, &BTreeMap::new(), |_| {
                version.map(str::to_string)
            })
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0.root);
        }
    }
    fn status<'a>(report: &'a Value, id: &str) -> &'a Value {
        &report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["id"] == id)
            .unwrap()["status"]
    }
    #[test]
    fn complete_fake_tree_reports_metadata_detection_without_device_or_key_actions() {
        let fixture = Fixture::new();
        let input = fixture.complete();
        let before = crate::snapshot::fingerprint_all(&fixture.0.root).unwrap();
        let report = fixture.inspect(&input, Some("4.7.stable.official"));
        assert_eq!(report["prerequisites_detected"], true);
        assert_eq!(report["export_readiness"], "unverified");
        assert_eq!(report["physical_readiness"], "unverified");
        assert_eq!(report["tools_invoked"], false);
        assert_eq!(report["network_used"], false);
        assert_eq!(report["private_keys_read"], false);
        assert_eq!(report["signing_verified"], false);
        assert!(report["devices"].is_null());
        assert!(!report.to_string().contains("PRIVATE-KEY-CONTENT-SENTINEL"));
        assert_eq!(
            crate::snapshot::fingerprint_all(&fixture.0.root).unwrap(),
            before
        );
        assert_eq!(report["checks"][0]["provenance"], "configured");
    }
    #[test]
    fn unknown_runtime_does_not_borrow_project_or_filename_android_requirements() {
        let fixture = Fixture::new();
        let input = fixture.complete();
        let report = fixture.inspect(&input, None);
        assert_eq!(report["prerequisites_detected"], false);
        assert_eq!(status(&report, "android_requirement_policy"), "unverified");
        assert_eq!(status(&report, "android_build_tools"), "unverified");
        let check = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["id"] == "android_build_tools")
            .unwrap();
        assert!(check["expected"].as_array().unwrap().is_empty());
    }
    #[test]
    fn explicit_missing_sdk_is_authoritative_and_distinct_from_version_mismatch() {
        let fixture = Fixture::new();
        let mut input = fixture.complete();
        input["android_sdk_path"] = json!("missing-sdk");
        let report = fixture.inspect(&input, Some("4.7.stable.official"));
        assert_eq!(status(&report, "android_sdk"), "missing");
        assert_eq!(report["prerequisites_detected"], false);
        input["android_sdk_path"] = json!("sdk");
        fixture.file(
            "sdk/build-tools/35.0.1/source.properties",
            "Pkg.Revision=36.0.0\n",
        );
        let report = fixture.inspect(&input, Some("4.7.stable.official"));
        assert_eq!(status(&report, "android_build_tools"), "mismatch");
    }
    #[test]
    fn release_keystore_is_not_inferred_from_debug_key_and_java_mismatch_is_reported() {
        let fixture = Fixture::new();
        let mut input = fixture.complete();
        fixture.file("jdk/release", "JAVA_VERSION=\"11.0.24\"\n");
        input["debug"] = json!(false);
        input.as_object_mut().unwrap().remove("keystore_path");
        let report = fixture.inspect(&input, Some("4.7.stable.official"));
        assert_eq!(status(&report, "java_version"), "mismatch");
        assert_eq!(status(&report, "signing_keystore_presence"), "missing");
        assert_eq!(report["prerequisites_detected"], false);
    }
    #[test]
    fn oversized_metadata_is_unverified_and_unknown_request_fields_are_refused() {
        let fixture = Fixture::new();
        let mut input = fixture.complete();
        fixture.file("jdk/release", &"x".repeat(METADATA_BYTES as usize + 1));
        let report = fixture.inspect(&input, Some("4.7.stable.official"));
        assert_eq!(status(&report, "java_version"), "unverified");
        input["install"] = json!(true);
        assert!(inspect_with(&fixture.0, &input, &BTreeMap::new(), |_| Some(
            "4.7.stable.official".into()
        ))
        .is_err());
    }
    #[test]
    fn preset_gradle_and_custom_templates_do_not_silently_use_default_requirements() {
        let fixture = Fixture::new();
        let input = fixture.complete();
        fixture.file(presets::PRESETS_FILE, "[preset.0]\nname=\"Android\"\nplatform=\"Android\"\n[preset.0.options]\ngradle_build/use_gradle_build=true\n");
        let report = fixture.inspect(&input, Some("4.7.stable.official"));
        assert_eq!(status(&report, "android_gradle_template"), "missing");
        assert_eq!(report["prerequisites_detected"], false);
        fixture.file("custom.apk", "custom template");
        fixture.file(presets::PRESETS_FILE, "[preset.0]\nname=\"Android\"\nplatform=\"Android\"\n[preset.0.options]\ncustom_template/debug=\"res://custom.apk\"\n");
        let report = fixture.inspect(&input, Some("4.7.stable.official"));
        assert_eq!(status(&report, "export_template"), "unverified");
        assert_eq!(report["prerequisites_detected"], false);
    }
    #[test]
    fn runtime_discovery_records_managed_provenance_and_other_versions_do_not_reuse_policy() {
        let fixture = Fixture::new();
        let mut input = fixture.complete();
        fixture.file("managed/runtime/Godot_v4.7.exe", "fake runtime");
        input.as_object_mut().unwrap().remove("godot_path");
        let environment = BTreeMap::from([(
            "AURUM_STUDIO_HOME".into(),
            fixture
                .0
                .root
                .join("managed")
                .to_string_lossy()
                .into_owned(),
        )]);
        let report = inspect_with(&fixture.0, &input, &environment, |_| {
            Some("4.6.stable.official".into())
        })
        .unwrap();
        assert_eq!(report["checks"][0]["provenance"], "managed_runtime");
        assert_eq!(status(&report, "runtime_version"), "mismatch");
        assert_eq!(status(&report, "android_requirement_policy"), "unverified");
    }
    #[test]
    fn remote_paths_and_device_actions_are_not_accepted_by_the_offline_schema() {
        let fixture = Fixture::new();
        let mut input = fixture.complete();
        input["android_sdk_path"] = json!("//server/sdk");
        assert!(inspect_with(&fixture.0, &input, &BTreeMap::new(), |_| Some(
            "4.7.stable.official".into()
        ))
        .is_err());
        input["android_sdk_path"] = json!("sdk");
        input["probe_devices"] = json!(true);
        assert!(inspect_with(&fixture.0, &input, &BTreeMap::new(), |_| Some(
            "4.7.stable.official".into()
        ))
        .is_err());
    }
    #[test]
    fn cancelled_metadata_report_stops_without_any_committed_effect() {
        let fixture = Fixture::new();
        let input = fixture.complete();
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(30),
        );
        context
            .scope(|| {
                context.request_cancel().unwrap();
                assert!(inspect_with(&fixture.0, &input, &BTreeMap::new(), |_| Some(
                    "4.7.stable.official".into()
                ))
                .is_err());
            })
            .unwrap();
        assert!(!context.committed());
        assert!(context.commit_status_verified());
    }
    #[test]
    fn adb_path_discovers_a_full_sdk_but_not_a_standalone_platform_tools_folder() {
        let fixture = Fixture::new();
        let mut input = fixture.complete();
        input.as_object_mut().unwrap().remove("android_sdk_path");
        let path = std::env::join_paths([fixture.0.root.join("sdk/platform-tools")]).unwrap();
        let environment = BTreeMap::from([("PATH".into(), path.to_string_lossy().into_owned())]);
        let report = inspect_with(&fixture.0, &input, &environment, |_| {
            Some("4.7.stable.official".into())
        })
        .unwrap();
        let sdk = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["id"] == "android_sdk")
            .unwrap();
        assert_eq!(sdk["status"], "available");
        assert_eq!(sdk["provenance"], "ADB_PATH");
        fixture.file(
            &format!("standalone/platform-tools/{}", executable("adb")),
            "fake adb",
        );
        let path =
            std::env::join_paths([fixture.0.root.join("standalone/platform-tools")]).unwrap();
        let environment = BTreeMap::from([("PATH".into(), path.to_string_lossy().into_owned())]);
        let report = inspect_with(&fixture.0, &input, &environment, |_| {
            Some("4.7.stable.official".into())
        })
        .unwrap();
        assert_eq!(status(&report, "android_sdk"), "missing");
        assert_eq!(status(&report, "adb"), "available");
    }

    #[test]
    fn bounded_project_open_skips_external_hints_cargo_and_addon_manifests() {
        let fixture = Fixture::new();
        fixture.file("aurum.toml", "schema_version=1\nname=\"preflight\"\nmodules=[]\naddon_destination=\"//untrusted/addon\"\n[engine]\npath_hint=\"//untrusted/workspace\"\n");
        fixture.file("Cargo.toml", "malformed Cargo metadata");
        fixture.file(
            "addons/aurum/bin/active.gdextension",
            &"x".repeat(PRESET_BYTES as usize + 1),
        );
        let project = Project::open_preflight(&fixture.0.root, Survey::new().deadline).unwrap();
        assert_eq!(project.godot_project_dir(), Some(fixture.0.root.as_path()));
        assert!(project.layout.cargo_manifest.is_none());
        assert!(project.layout.addon_directory.is_none());
        assert!(project.layout.gdextension.is_none());
        assert!(project.layout.installed_library.is_none());
        assert!(project.layout.build_script.is_none());
    }

    #[test]
    fn bounded_project_open_rejects_oversized_config_and_excess_layout_entries() {
        let fixture = Fixture::new();
        fixture.file("aurum.toml", &"#".repeat(METADATA_BYTES as usize + 1));
        assert!(Project::open_preflight(&fixture.0.root, Survey::new().deadline).is_err());
        fixture.file(
            "aurum.toml",
            "schema_version=1\nname=\"preflight\"\nmodules=[]\n",
        );
        std::fs::remove_file(fixture.0.root.join("project.godot")).unwrap();
        fixture.file("godot/project.godot", "config_version=5\n");
        let project = Project::open_preflight(&fixture.0.root, Survey::new().deadline).unwrap();
        assert_eq!(
            project.godot_project_dir(),
            Some(fixture.0.root.join("godot").as_path())
        );
        for index in 0..DIRECTORY_ENTRIES {
            fixture.file(&format!("entry-{index}/placeholder"), "x");
        }
        assert!(Project::open_preflight(&fixture.0.root, Survey::new().deadline).is_err());
    }

    #[test]
    fn sdk_metadata_cannot_qualify_misnamed_package_directories() {
        let fixture = Fixture::new();
        let input = fixture.complete();
        std::fs::rename(
            fixture.0.root.join("sdk/platforms/android-35"),
            fixture.0.root.join("sdk/platforms/android-36"),
        )
        .unwrap();
        std::fs::rename(
            fixture.0.root.join("sdk/build-tools/35.0.1"),
            fixture.0.root.join("sdk/build-tools/36.0.0"),
        )
        .unwrap();
        let mut project = fixture.0.clone();
        project.config.rust_package = Some("fake-game".into());
        fixture.file(
            "sdk/ndk/27.0.0/source.properties",
            "Pkg.Revision=28.1.13356709\n",
        );
        fixture.file(
            "sdk/cmake/3.22.0/source.properties",
            "Pkg.Revision=3.10.2.4988404\n",
        );
        let report = inspect_with(&project, &input, &BTreeMap::new(), |_| {
            Some("4.7.stable.official".into())
        })
        .unwrap();
        for id in [
            "android_platform",
            "android_build_tools",
            "android_ndk",
            "android_cmake",
        ] {
            assert_eq!(status(&report, id), "mismatch", "{id}");
        }
        assert_eq!(report["prerequisites_detected"], false);
    }

    #[test]
    fn platform_metadata_without_android_jar_is_unverified() {
        let fixture = Fixture::new();
        let input = fixture.complete();
        std::fs::remove_file(fixture.0.root.join("sdk/platforms/android-35/android.jar")).unwrap();
        let report = fixture.inspect(&input, Some("4.7.stable.official"));
        assert_eq!(status(&report, "android_platform"), "unverified");
        assert_eq!(report["prerequisites_detected"], false);
    }

    #[test]
    fn discovered_templates_fall_back_by_presence_but_explicit_paths_stay_authoritative() {
        let fixture = Fixture::new();
        let mut input = fixture.complete();
        input.as_object_mut().unwrap().remove("templates_path");
        input["gradle"] = json!(true);
        fixture.file(
            "managed/runtime/templates/4.7.stable/android_debug.apk",
            "managed APK without Gradle source",
        );
        fixture.file(
            "appdata/Godot/export_templates/4.7.stable/android_debug.apk",
            "conventional APK",
        );
        fixture.file(
            "appdata/Godot/export_templates/4.7.stable/android_source.zip",
            "conventional Gradle source",
        );
        let environment = BTreeMap::from([
            (
                "AURUM_STUDIO_HOME".into(),
                fixture
                    .0
                    .root
                    .join("managed")
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "APPDATA".into(),
                fixture
                    .0
                    .root
                    .join("appdata")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ]);
        let report = inspect_with(&fixture.0, &input, &environment, |_| {
            Some("4.7.stable.official".into())
        })
        .unwrap();
        assert_eq!(status(&report, "export_template"), "available");
        assert_eq!(status(&report, "android_gradle_template"), "available");
        let template = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == "export_template")
            .unwrap();
        assert_eq!(template["provenance"], "conventional");
        input["templates_path"] = json!("missing-explicit");
        let report = inspect_with(&fixture.0, &input, &environment, |_| {
            Some("4.7.stable.official".into())
        })
        .unwrap();
        assert_eq!(status(&report, "export_template"), "missing");
        assert_eq!(report["prerequisites_detected"], false);
    }

    #[test]
    fn selected_preset_names_and_serialized_reports_are_bounded() {
        let fixture = Fixture::new();
        let mut input = fixture.complete();
        input.as_object_mut().unwrap().remove("preset");
        fixture.file(
            presets::PRESETS_FILE,
            &format!(
                "[preset.0]\nname=\"{}\"\nplatform=\"Android\"\n",
                "x".repeat(PRESET_NAME_BYTES + 1)
            ),
        );
        assert!(inspect_with(&fixture.0, &input, &BTreeMap::new(), |_| Some(
            "4.7.stable.official".into()
        ))
        .unwrap_err()
        .contains("preset name"));
        fixture.file(
            presets::PRESETS_FILE,
            "[preset.0]\nname=\"Android\"\nplatform=\"Android\"\n",
        );
        input["android_sdk_path"] = json!("x".repeat(4000));
        assert!(inspect_with(&fixture.0, &input, &BTreeMap::new(), |_| Some(
            "4.7.stable.official".into()
        ))
        .unwrap_err()
        .contains("report exceeds 32 KiB"));
    }

    #[test]
    fn path_discovery_rejects_expired_budget_and_oversized_environment() {
        let fixture = Fixture::new();
        fixture.complete();
        let path = std::env::join_paths([fixture.0.root.join("sdk/platform-tools")])
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let environment = BTreeMap::from([("PATH".into(), path)]);
        let expired = Survey {
            deadline: Instant::now(),
            checks: Vec::new(),
        };
        assert!(path_tool(&environment, "adb", &expired).is_none());
        assert!(Project::open_preflight(&fixture.0.root, expired.deadline).is_err());
        let environment =
            BTreeMap::from([("PATH".into(), "x".repeat(METADATA_BYTES as usize + 1))]);
        assert!(path_tool(&environment, "adb", &Survey::new()).is_none());
        #[cfg(windows)]
        {
            let mut environment = BTreeMap::from([(
                "PATH".into(),
                fixture
                    .0
                    .root
                    .join("sdk/platform-tools")
                    .to_string_lossy()
                    .into_owned(),
            )]);
            environment.insert("PATHEXT".into(), vec![".EXE"; PATH_SUFFIXES + 1].join(";"));
            assert!(path_tool(&environment, "adb", &Survey::new()).is_none());
        }
    }

    #[test]
    fn late_cancellation_after_runtime_metadata_refuses_success() {
        let fixture = Fixture::new();
        let input = fixture.complete();
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(30),
        );
        context
            .scope(|| {
                assert!(inspect_with(&fixture.0, &input, &BTreeMap::new(), |_| {
                    context.request_cancel().unwrap();
                    Some("4.7.stable.official".into())
                })
                .is_err());
            })
            .unwrap();
        assert!(!context.committed());
        assert!(context.commit_status_verified());
    }

    #[cfg(unix)]
    #[test]
    fn bounded_project_open_rejects_linked_config_without_reading_it() {
        let fixture = Fixture::new();
        fixture.file("linked-config", "untrusted config body");
        std::fs::remove_file(fixture.0.root.join("aurum.toml")).unwrap();
        std::os::unix::fs::symlink(
            fixture.0.root.join("linked-config"),
            fixture.0.root.join("aurum.toml"),
        )
        .unwrap();
        assert!(Project::open_preflight(&fixture.0.root, Survey::new().deadline).is_err());
    }
}
