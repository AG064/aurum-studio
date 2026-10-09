//! Project operations shared by the CLI, HTTP interface, and MCP server.
use crate::{files, Project};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const OPERATIONS: &[&str] = &[
    "describe",
    "status",
    "logs",
    "changes_check",
    "changes_validate",
    "changes_last",
    "files",
    "read",
    "write",
    "draft_save",
    "draft_read",
    "draft_clear",
    "undo",
    "build",
    "validate",
    "play",
    "capture",
    "web_build",
    "export",
    "presets",
    "configure_export",
    "package",
    "runtime_info",
    "scene_inspect",
    "scene_create",
    "scene_edit",
    "set_main_scene",
    "classes",
    "class_info",
];
pub fn is_read_only(op: &str) -> bool {
    matches!(
        op,
        "describe"
            | "status"
            | "logs"
            | "changes_check"
            | "changes_last"
            | "files"
            | "read"
            | "draft_read"
            | "scene_inspect"
            | "classes"
            | "class_info"
            | "presets"
            | "runtime_info"
    )
}
fn field<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("'{key}' must be a string"))
}
pub(crate) fn managed_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = files::confined(root, relative)?;
    let canonical_root = root
        .canonicalize()
        .map(crate::project::clean_path)
        .map_err(|e| e.to_string())?;
    let normalized = path
        .strip_prefix(&canonical_root)
        .map_err(|e| e.to_string())?
        .to_string_lossy();
    if normalized.replace('\\', "/").split('/').any(|part| {
        let part = part.trim_end_matches('.').to_ascii_lowercase();
        matches!(part.as_str(), ".git" | ".aurum" | ".godot" | ".env") || part.starts_with(".env.")
    }) {
        return Err(
            "internal state and secrets are not available through project file tools".into(),
        );
    }
    Ok(path)
}

pub fn execute(root: &Path, input: &Value, read_only: bool) -> Result<Value, String> {
    let op = field(input, "op")?;
    if !OPERATIONS.contains(&op) {
        return Err(format!("unknown operation '{op}'"));
    }
    if read_only && !is_read_only(op) {
        return Err(format!("'{op}' requires write permission"));
    }
    // Discovery and polling remain write-free. Open the project before creating
    // diagnostics, so an invalid project request cannot create arbitrary state.
    let project = Project::open(root).map_err(|error| error.to_string())?;
    let span = input
        .get("op")
        .and_then(Value::as_str)
        .and_then(|op| crate::diagnostics::Operation::start(&project.root, op));
    let result = execute_inner(project, input);
    if let Some(span) = span {
        span.finish(&result);
    }
    result
}

fn execute_inner(project: Project, input: &Value) -> Result<Value, String> {
    let op = field(input, "op")?;
    let _lock = if !matches!(
        op,
        "describe"
            | "status"
            | "logs"
            | "changes_check"
            | "changes_validate"
            | "changes_last"
            | "files"
            | "read"
            | "write"
            | "undo"
            | "draft_save"
            | "draft_read"
            | "draft_clear"
            | "scene_inspect"
            | "classes"
            | "class_info"
            | "runtime_info"
    ) {
        Some(crate::BuildLock::try_acquire(&project.root).map_err(|e| e.to_string())?)
    } else {
        None
    };
    match op {
        "logs" => crate::diagnostics::query(&project.root, input),
        "changes_check" => crate::revisions::check(&project.root, input),
        "changes_validate" => crate::candidates::validate(&project, input),
        "changes_last" => crate::candidates::last(&project),
        "describe" => crate::project_contract::describe(match input.get("operation") {
            Some(value) => Some(value.as_str().ok_or("'operation' must be a string")?),
            None => None,
        }),
        "draft_save" | "draft_read" | "draft_clear" => draft(&project, input),
        "status" => Ok(
            json!({"name":project.config.name,"root":project.root,"godot_project":project.godot_project_dir(),"native_package":project.config.rust_package,"operations":OPERATIONS,"headless":true,"backend":"Godot","session_state":"headless MCP simulation is separate from project gameplay"}),
        ),
        "files" => {
            let mut paths = Vec::new();
            list_files(&project.root, &project.root, &mut paths, 0);
            let truncated = paths.len() > 500;
            paths.truncate(500);
            Ok(json!({"files":paths,"truncated":truncated}))
        }
        "read" => {
            let path = managed_path(&project.root, field(input, "path")?)?;
            let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            if metadata.len() > 2 * 1024 * 1024 {
                return Err("text files are limited to 2 MiB".into());
            }
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            Ok(
                json!({"path":field(input,"path")?,"text":text,"sha256":crate::sha256_hex(text.as_bytes())}),
            )
        }
        "write" => {
            let path = managed_path(&project.root, field(input, "path")?)?;
            let text = field(input, "text")?;
            if text.len() > 2 * 1024 * 1024 {
                return Err("text files are limited to 2 MiB".into());
            }
            commit_file(
                &project.root,
                &path,
                text.as_bytes(),
                input.get("expected_sha256").and_then(Value::as_str),
            )?;
            Ok(
                json!({"path":field(input,"path")?,"sha256":crate::sha256_hex(text.as_bytes()),"saved":true}),
            )
        }
        "undo" => {
            let path = managed_path(&project.root, field(input, "path")?)?;
            let backup = backup_path(&project.root, &path)?;
            let bytes = std::fs::read(&backup)
                .map_err(|_| "no previous version was recorded for this file")?;
            commit_file(
                &project.root,
                &path,
                &bytes,
                input.get("expected_sha256").and_then(Value::as_str),
            )?;
            Ok(json!({"restored":field(input,"path")?,"sha256":crate::sha256_hex(&bytes)}))
        }
        "build" => build_project(
            &project,
            input
                .get("release")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
        "presets" => {
            let report = crate::presets::read(godot_root(&project)?).map_err(|e| e.to_string())?;
            Ok(
                json!({"presets":report.presets.iter().map(|preset|json!({"name":preset.name,"platform":preset.platform})).collect::<Vec<_>>(),"file_present":report.file_present}),
            )
        }
        "configure_export" => {
            let platform = match input.get("platform") {
                Some(value) => value.as_str().ok_or("'platform' must be a string")?,
                None => "Windows Desktop",
            };
            let preset = ensure_export_preset(&project, platform)?;
            Ok(
                json!({"ok":true,"preset":preset,"platform":platform,"note":"Preset configured. Export still requires matching runtime templates and the platform SDK/signing tools where applicable. No device validation has been performed."}),
            )
        }
        "package" => package_windows(&project, input),
        "validate" => validate(&project),
        "play" | "capture" => play_project(&project, input),
        "web_build" => crate::web_build::build(&project),
        "export" => {
            let preset = field(input, "preset")?;
            let output = managed_path(&project.root, field(input, "output")?)?;
            if output.exists() {
                return Err("export destination already exists; choose a new output path".into());
            }
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let flag = if input.get("pack").and_then(Value::as_bool).unwrap_or(false) {
                "--export-pack"
            } else if input.get("debug").and_then(Value::as_bool).unwrap_or(false) {
                "--export-debug"
            } else {
                "--export-release"
            };
            engine_run(
                &project,
                &[
                    "--headless".into(),
                    "--path".into(),
                    godot_root(&project)?.display().to_string(),
                    flag.into(),
                    preset.into(),
                    output.display().to_string(),
                ],
                Duration::from_secs(600),
            )
        }
        _ => headless_operation(&project, input),
    }
}

fn play_options(input: &Value) -> Result<(u64, Option<u64>, Vec<String>), String> {
    let integer = |key: &str, default: u64, maximum: u64| -> Result<u64, String> {
        let value = match input.get(key) {
            Some(value) => value
                .as_u64()
                .ok_or_else(|| format!("'{key}' must be an integer"))?,
            None => default,
        };
        if value == 0 || value > maximum {
            return Err(format!("'{key}' must be between 1 and {maximum}"));
        }
        Ok(value)
    };
    let frames = integer("frames", 120, crate::project_contract::MAX_FRAMES)?;
    let fixed_fps = input
        .get("fixed_fps")
        .map(|_| integer("fixed_fps", 60, crate::project_contract::MAX_FIXED_FPS))
        .transpose()?;
    let mut arguments = Vec::new();
    if let Some(value) = input.get("user_args") {
        let values = value
            .as_array()
            .ok_or("'user_args' must be an array of strings")?;
        if values.len() > 32 {
            return Err("'user_args' is limited to 32 entries".into());
        }
        for value in values {
            let arg = value
                .as_str()
                .ok_or("'user_args' must contain only strings")?;
            if arg.len() > 4096 || arg.contains('\0') || arg.starts_with("--aurum-report") {
                return Err(
                    "user argument is oversized, contains NUL, or uses the reserved report option"
                        .into(),
                );
            }
            arguments.push(arg.to_owned());
        }
    }
    if input.get("report").is_some_and(|v| !v.is_boolean()) {
        return Err("'report' must be a boolean".into());
    }
    Ok((frames, fixed_fps, arguments))
}

fn play_project(project: &Project, input: &Value) -> Result<Value, String> {
    let (frames, fixed_fps, user_args) = play_options(input)?;
    crate::playtest::run(project, input, frames, fixed_fps, user_args)
}

#[cfg(test)]
fn read_play_report(path: &Path) -> Result<Value, String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| "game did not write the requested report")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err("game report must be a regular JSON file of at most 1 MiB".into());
    }
    let report: Value = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| format!("invalid game report: {e}"))?;
    if !report.is_object() || !report.get("ok").is_some_and(Value::is_boolean) {
        return Err("game report must be an object with a boolean 'ok' field".into());
    }
    Ok(report)
}

pub const EXPORT_PLATFORMS: &[&str] =
    &["Windows Desktop", "Linux", "macOS", "Web", "Android", "iOS"];

fn ensure_export_preset(project: &Project, platform: &str) -> Result<String, String> {
    if !EXPORT_PLATFORMS.contains(&platform) {
        return Err(format!(
            "unsupported export platform '{platform}'; expected one of {}",
            EXPORT_PLATFORMS.join(", ")
        ));
    }
    let godot = godot_root(project)?;
    let report = crate::presets::read(godot).map_err(|e| e.to_string())?;
    if let Some(preset) = report
        .presets
        .iter()
        .find(|preset| preset.platform == platform)
    {
        return Ok(preset.name.clone());
    }
    if report.unreadable_lines != 0 {
        return Err(
            "existing export presets contain unreadable lines; correct them before adding a preset"
                .into(),
        );
    }
    let path = godot.join("export_presets.cfg");
    let old = if path.exists() {
        std::fs::read_to_string(&path).map_err(|e| e.to_string())?
    } else {
        String::new()
    };
    let index = old
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("[preset.")?
                .strip_suffix(']')?
                .parse::<u32>()
                .ok()
        })
        .max()
        .map(|n| n.checked_add(1).ok_or("preset index is too large"))
        .transpose()?
        .unwrap_or(0);
    let preset = if platform == "Windows Desktop" {
        "Aurum Windows".to_owned()
    } else {
        format!("Aurum {platform}")
    };
    let options = match platform {
        "Windows Desktop" | "Linux" => "binary_format/architecture=\"x86_64\"\n",
        "Web" => "variant/thread_support=false\n",
        "Android" => "gradle_build/use_gradle_build=false\npackage/unique_name=\"org.aurum.$genname\"\npermissions/internet=false\n",
        "macOS" | "iOS" => "application/bundle_identifier=\"org.aurum.game\"\n",
        _ => "",
    };
    let text=format!("{old}\n[preset.{index}]\nname=\"{preset}\"\nplatform=\"{platform}\"\nrunnable=true\nexport_filter=\"all_resources\"\ninclude_filter=\"\"\nexclude_filter=\".aurum/*,dist/*,builds/*\"\nexport_path=\"\"\nscript_export_mode=2\n\n[preset.{index}.options]\n{options}");
    let expected = if path.exists() {
        crate::sha256_hex(old.as_bytes())
    } else {
        String::new()
    };
    commit_file(&project.root, &path, text.as_bytes(), Some(&expected))?;
    Ok(preset)
}

fn package_windows(project: &Project, input: &Value) -> Result<Value, String> {
    if !cfg!(windows) {
        return Err("portable Windows packaging requires a Windows runtime".into());
    }
    let output = managed_path(
        &project.root,
        input
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or("dist/windows"),
    )?;
    if output.exists() {
        return Err("package destination already exists; choose a new directory".into());
    }
    if project.config.rust_package.is_some() {
        build_project(project, true)?;
    }
    let check = validate(project)?;
    if check["ok"] != true {
        return Ok(check);
    }
    let preset = ensure_export_preset(project, "Windows Desktop")?;
    let stage = files::confined(
        &project.root,
        &format!(".aurum/packages/{}", crate::random::session_id()),
    )?;
    std::fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
    let packed = engine_run(
        project,
        &[
            "--headless".into(),
            "--path".into(),
            godot_root(project)?.display().to_string(),
            "--export-pack".into(),
            preset,
            stage.join("game.pck").display().to_string(),
        ],
        Duration::from_secs(600),
    )?;
    if packed["ok"] != true {
        return Ok(packed);
    }
    let runtime = engine_binary(project)?;
    let runtime_hash = crate::sha256_file(&runtime).map_err(|e| e.to_string())?;
    std::fs::copy(&runtime, stage.join("game.exe")).map_err(|e| e.to_string())?;
    if crate::sha256_file(&stage.join("game.exe")).map_err(|e| e.to_string())? != runtime_hash {
        return Err("runtime copy failed hash verification".into());
    }
    copy_native_libraries(godot_root(project)?, godot_root(project)?, &stage, 0)?;
    let licenses = headless_operation(project, &json!({"op":"runtime_info"}))?;
    let license = licenses
        .get("license")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("runtime license text was unavailable")?;
    files::write_atomic(&stage.join("LICENSE-Godot.txt"), license.as_bytes())
        .map_err(|e| e.to_string())?;
    files::write_atomic(
        &stage.join("third-party-licenses.json"),
        serde_json::to_string_pretty(&licenses)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    files::write_atomic(&stage.join("README.txt"),b"Launch game.exe. Keep game.pck and the supplied libraries beside it. This Windows package includes the Godot runtime; no separate engine installation is required.\n").map_err(|e|e.to_string())?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&stage, &output).map_err(|e| e.to_string())?;
    Ok(
        json!({"ok":true,"directory":output,"executable":output.join("game.exe"),"runtime_sha256":runtime_hash,"requires_separate_godot":false}),
    )
}

fn copy_native_libraries(
    root: &Path,
    directory: &Path,
    output: &Path,
    depth: usize,
) -> Result<(), String> {
    if depth > 24 {
        return Err("native library directory nesting is too deep".into());
    }
    for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name.starts_with('~') || name == "target" || kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            copy_native_libraries(root, &entry.path(), output, depth + 1)?;
        } else if entry.path().extension().is_some_and(|ext| ext == "dll")
            && !name.ends_with(".debug.dll")
        {
            let path = output.join(entry.path().strip_prefix(root).map_err(|e| e.to_string())?);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let copied = crate::build::install(&entry.path(), &path).map_err(|e| e.to_string())?;
            if copied.sha256 != crate::sha256_file(&entry.path()).map_err(|e| e.to_string())? {
                return Err("native library copy hash mismatch".into());
            }
        }
    }
    Ok(())
}

fn draft(project: &Project, input: &Value) -> Result<Value, String> {
    let path = managed_path(&project.root, field(input, "path")?)?;
    let relative = path
        .strip_prefix(&project.root)
        .map_err(|e| e.to_string())?
        .to_string_lossy();
    let normalized = relative.replace('\\', "/");
    let key_name = if cfg!(windows) {
        normalized.to_lowercase()
    } else {
        normalized
    };
    let key = crate::sha256_hex(key_name.as_bytes());
    let directory = files::confined(&project.root, ".aurum/drafts")?;
    if input["op"] == "draft_read" {
        let latest = std::fs::read_dir(&directory)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{key}-"))
                    && entry.path().extension().is_some_and(|ext| ext == "json")
                    && entry.file_type().is_ok_and(|kind| kind.is_file())
            })
            .max_by_key(|entry| entry.metadata().and_then(|meta| meta.modified()).ok());
        let value = latest
            .and_then(|entry| std::fs::read(entry.path()).ok())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(|value| {
                value["text"].is_string()
                    && value["base_sha256"].is_string()
                    && value["draft_id"].is_string()
            });
        return Ok(json!({"draft":value}));
    }
    let id = field(input, "draft_id")?;
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("invalid draft identifier".into());
    }
    let target = directory.join(format!("{key}-{id}.json"));
    if input["op"] == "draft_clear" {
        match std::fs::remove_file(&target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        return Ok(json!({"ok":true}));
    }
    let text = field(input, "text")?;
    if text.len() > 2 * 1024 * 1024 {
        return Err("drafts are limited to 2 MiB".into());
    }
    let value = json!({"path":relative,"draft_id":id,"text":text,"base_sha256":input.get("base_sha256").and_then(Value::as_str).unwrap_or("")});
    files::write_atomic(
        &target,
        &serde_json::to_vec(&value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({"ok":true,"draft_id":id}))
}

fn list_files(root: &Path, directory: &Path, out: &mut Vec<String>, depth: usize) {
    if depth > 24 || out.len() > 500 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || matches!(name.as_str(), "target" | "node_modules" | "logs") {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            list_files(root, &entry.path(), out, depth + 1);
        } else if kind.is_file() {
            out.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
        if out.len() > 500 {
            break;
        }
    }
}

fn backup_path(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "file is outside project")?;
    files::confined(
        root,
        &format!(
            ".aurum/undo/{}",
            relative.to_string_lossy().replace('\\', "/")
        ),
    )
}
fn commit_file(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    expected: Option<&str>,
) -> Result<(), String> {
    let _content_lock = crate::content_lock::ContentLock::exclusive(root)?;
    let _file_lock = crate::BuildLock::try_acquire(path).map_err(|e| e.to_string())?;
    if let Some(expected) = expected {
        let actual = if path.is_file() {
            crate::sha256_file(path).map_err(|e| e.to_string())?
        } else {
            String::new()
        };
        if !actual.eq_ignore_ascii_case(expected) {
            return Err("file changed since it was read; inspect it before retrying".into());
        }
    }
    if path.is_file() {
        let old = std::fs::read(path).map_err(|e| e.to_string())?;
        files::write_atomic(&backup_path(root, path)?, &old).map_err(|e| e.to_string())?;
    }
    files::write_atomic(path, bytes).map_err(|e| e.to_string())
}
fn godot_root(project: &Project) -> Result<&Path, String> {
    project
        .godot_project_dir()
        .ok_or_else(|| "project.godot was not found".into())
}

pub fn engine_binary(project: &Project) -> Result<PathBuf, String> {
    let hint = std::env::var_os("AURUM_GODOT").map(PathBuf::from);
    let flavor = crate::toolchain::GodotFlavor::PreferWindowed;
    if let Some(path) = crate::toolchain::discover_godot(hint.as_deref(), &project.root, flavor) {
        return Ok(path);
    }
    if hint.is_some() {
        return Err("AURUM_GODOT does not name an available runtime".into());
    }
    if let Some(home) = std::env::var_os("AURUM_STUDIO_HOME") {
        if let Some(path) =
            crate::toolchain::find_godot_in(&PathBuf::from(home).join("runtime"), flavor)
        {
            return Ok(path);
        }
    }
    if let Some(engine) = project.config.engine_path_hint.as_deref() {
        if let Some(path) =
            crate::toolchain::discover_godot(None, &project.root.join(engine), flavor)
        {
            return Ok(path);
        }
    }
    Err("Aurum runtime is not configured. Set AURUM_GODOT or install the runtime under AURUM_STUDIO_HOME/runtime".into())
}

/// Godot 4.7 on Windows can crash after completing first-import shutdown.
/// Never retry an actual resource error, an incomplete import, or a wall timeout.
pub fn import_shutdown_crash(code: Option<i32>, log: &str) -> bool {
    cfg!(windows)
        && code == Some(-1073741819)
        && log.contains("loading_editor_layout")
        && !log.contains("ERROR:")
}

fn engine_run(project: &Project, args: &[String], timeout: Duration) -> Result<Value, String> {
    engine_run_with_context(project, args, timeout, None)
}

pub(crate) struct RuntimeContext {
    pub engine: PathBuf,
    pub userdata: PathBuf,
}

fn engine_run_with_context(
    project: &Project,
    args: &[String],
    timeout: Duration,
    context: Option<&RuntimeContext>,
) -> Result<Value, String> {
    let engine = match context {
        Some(context) => context.engine.clone(),
        None => engine_binary(project)?,
    };
    let mut command = crate::Command::new(engine)
        .args(args.iter().cloned())
        .directory(&project.root);
    if let Some(context) = context {
        for key in [
            "APPDATA",
            "LOCALAPPDATA",
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
        ] {
            command = command.env(key, context.userdata.to_string_lossy());
        }
    }
    let result = crate::native_runtime::run(&command, timeout).map_err(|e| e.to_string())?;
    let errors: Vec<_> = result
        .stdout
        .lines()
        .chain(result.stderr.lines())
        .filter(|line| line.contains("ERROR:") || line.contains("SCRIPT ERROR:"))
        .take(30)
        .collect();
    Ok(
        json!({"ok":result.success() && errors.is_empty(),"exit_code":result.code,"timed_out":result.timed_out,"errors":errors,"log":result.tail(60)}),
    )
}

fn validate(project: &Project) -> Result<Value, String> {
    validate_with_context(project, None)
}

pub(crate) fn validate_with_context(
    project: &Project,
    context: Option<&RuntimeContext>,
) -> Result<Value, String> {
    let _cache_lock = crate::BuildLock::try_acquire(&godot_root(project)?.join(".godot"))
        .map_err(|e| e.to_string())?;
    let args = vec![
        "--headless".into(),
        "--editor".into(),
        "--path".into(),
        godot_root(project)?.display().to_string(),
        "--import".into(),
    ];
    let first = engine_run_with_context(project, &args, Duration::from_secs(120), context)?;
    // Godot 4.7 on Windows can fail during first-import shutdown. Retry only
    // that exact exit and only when there were no reported script/resource errors.
    if first["exit_code"].as_i64() == Some(-1073741819)
        && first["errors"].as_array().is_some_and(Vec::is_empty)
    {
        let mut result =
            engine_run_with_context(project, &args, Duration::from_secs(120), context)?;
        result["first_import_shutdown_retry"] = json!(true);
        if result["ok"] == true {
            let checked =
                headless_operation_with_context(project, &json!({"op":"validate"}), context)?;
            result["resource_validation"] = checked.clone();
            result["ok"] = checked["ok"].clone();
        }
        return Ok(result);
    }
    let mut result = first;
    if result["ok"] == true {
        let checked = headless_operation_with_context(project, &json!({"op":"validate"}), context)?;
        result["resource_validation"] = checked.clone();
        result["ok"] = checked["ok"].clone();
    }
    Ok(result)
}

pub fn build_project(project: &Project, release: bool) -> Result<Value, String> {
    let Some(package) = project.config.rust_package.as_deref() else {
        return Ok(
            json!({"ok":true,"native_build":false,"message":"Script project; no native build required"}),
        );
    };
    let cargo = crate::toolchain::find_on_path("cargo").ok_or("Cargo was not found")?;
    let addon = project
        .layout
        .addon_directory
        .as_ref()
        .ok_or("configured native add-on directory was not found")?;
    let profile = if release {
        crate::Profile::Release
    } else {
        crate::Profile::Debug
    };
    let mut request = crate::BuildRequest::new(
        project.build_workspace(),
        package,
        profile,
        addon
            .join("bin")
            .join(profile.installed_filename(&crate::build::library_name(package))),
        cargo,
    );
    request.locked = project.build_workspace().join("Cargo.lock").is_file();
    let _workspace_lock = if project.build_workspace() != project.root {
        Some(crate::BuildLock::try_acquire(project.build_workspace()).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let report =
        crate::build::run(&request, false, Duration::from_secs(1800)).map_err(|e| e.to_string())?;
    Ok(
        json!({"ok":true,"built":report.built,"replaced":report.replaced,"sha256":report.installed.sha256,"message":report.summary(),"diagnostics":report.output}),
    )
}

fn headless_operation(project: &Project, input: &Value) -> Result<Value, String> {
    headless_operation_with_context(project, input, None)
}

fn headless_operation_with_context(
    project: &Project,
    input: &Value,
    context: Option<&RuntimeContext>,
) -> Result<Value, String> {
    let needs_import = matches!(
        input["op"].as_str(),
        Some("scene_inspect" | "scene_create" | "scene_edit" | "set_main_scene")
    );
    let inspection_source = if needs_import && project.config.rust_package.is_none() {
        Some(private_inspection_source(project)?)
    } else {
        None
    };
    if needs_import && inspection_source.is_none() {
        ensure_imported(project)?;
    }
    let godot = godot_root(project)?;
    let root = project
        .root
        .canonicalize()
        .map(crate::project::clean_path)
        .map_err(|e| e.to_string())?;
    let scene = input.get("scene").and_then(Value::as_str);
    let scene_path = scene.map(|scene| managed_path(godot, scene)).transpose()?;
    if let Some(operations) = input.get("operations").and_then(Value::as_array) {
        for operation in operations {
            if let Some(script) = operation.get("script").and_then(Value::as_str) {
                files::confined(godot, script)?;
            }
            if let Some(source) = operation.get("source").and_then(Value::as_str) {
                files::confined(godot, source)?;
            }
        }
    }
    if let Some(path) = &scene_path {
        if path.extension().is_none_or(|ext| ext != "tscn") {
            return Err("scene path must end in .tscn".into());
        }
        if input["op"] == "scene_create" && path.exists() {
            return Err("scene already exists; use scene_edit".into());
        }
    }
    let commit_target = if input["op"] == "set_main_scene" {
        Some(godot.join("project.godot"))
    } else {
        scene_path.clone()
    };
    let before_hash = commit_target
        .as_ref()
        .filter(|p| p.is_file())
        .map(|p| crate::sha256_file(p).map_err(|e| e.to_string()))
        .transpose()?
        .unwrap_or_default();
    if let Some(expected) = input.get("expected_sha256").and_then(Value::as_str) {
        if !expected.eq_ignore_ascii_case(&before_hash) {
            return Err("scene changed since it was read".into());
        }
    }
    let id = format!("job-{}-{}", std::process::id(), crate::random::session_id());
    let job = files::confined(&root, &format!(".aurum/jobs/{id}"))?;
    std::fs::create_dir_all(&job).map_err(|e| e.to_string())?;
    let script = job.join("operation.gd");
    files::write_atomic(&script, include_bytes!("headless.gd")).map_err(|e| e.to_string())?;
    let mut request = input.clone();
    if let Some(scene) = scene {
        request["scene"] = json!(format!(
            "res://{}",
            scene.trim_start_matches("res://").replace('\\', "/")
        ));
    }
    let staged_scene = job.join("scene.tscn");
    let staged_project = job.join("project.godot");
    request["output_project"] = json!(staged_project.to_string_lossy().replace('\\', "/"));
    request["output_scene"] = json!(staged_scene.to_string_lossy().replace('\\', "/"));
    let request_path = job.join("request.json");
    let response_path = job.join("response.json");
    files::write_atomic(
        &request_path,
        &serde_json::to_vec(&request).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let execution = engine_run_with_context(
        project,
        &[
            "--headless".into(),
            "--path".into(),
            inspection_source
                .as_deref()
                .unwrap_or(godot)
                .display()
                .to_string(),
            "--script".into(),
            script.display().to_string(),
            "--".into(),
            request_path.display().to_string(),
            response_path.display().to_string(),
        ],
        Duration::from_secs(60),
        context,
    )?;
    let mut response: Value = std::fs::read(&response_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| format!("headless operation did not return a result: {execution}"))?;
    if execution["ok"] != true {
        response["ok"] = json!(false);
        response["error"] = json!(execution["errors"]
            .as_array()
            .map(|errors| errors
                .iter()
                .filter_map(Value::as_str)
                .take(4)
                .collect::<Vec<_>>()
                .join("\n"))
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "Headless operation failed; inspect diagnostics".into()));
        response["diagnostics"] = execution;
    }
    if response["ok"] == true
        && matches!(
            input["op"].as_str(),
            Some("scene_create" | "scene_edit" | "set_main_scene")
        )
    {
        let target = commit_target.ok_or("scene is required")?;
        let staged = if input["op"] == "set_main_scene" {
            staged_project
        } else {
            staged_scene
        };
        let bytes = std::fs::read(staged).map_err(|e| e.to_string())?;
        commit_file(&root, &target, &bytes, Some(&before_hash))?;
        response["sha256"] = json!(crate::sha256_hex(&bytes));
        response["saved"] = json!(true);
    } else if response["ok"] == true && scene.is_some() {
        response["sha256"] = json!(before_hash);
    }
    if let Some(scene) = scene {
        let file = if input["op"] == "set_main_scene" {
            godot.join("project.godot")
        } else {
            files::confined(godot, scene)?
        };
        response["file_path"] = json!(file
            .strip_prefix(&root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/"));
    }
    Ok(response)
}

/// Resource inspection must work without requiring the user to open an editor first.
/// Import only when source changed, serialize native cache writers, and never edit scene data.
/// Import script-only scene workers in immutable, revision-keyed snapshots.
/// Never re-import a user's original cache to service a source inspection/edit.
fn plain_text_inspection_tree(source: &Path) -> Result<bool, String> {
    let mut pending = vec![source.to_path_buf()];
    let mut count = 0;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            count += 1;
            if count > 5000 {
                return Ok(false);
            }
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                if entry.file_name() == "addons" {
                    return Ok(false);
                }
                pending.push(entry.path());
            } else if kind.is_file() {
                let path = entry.path();
                let extension = path
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or("");
                if !matches!(
                    extension,
                    "gd" | "tscn" | "tres" | "godot" | "cfg" | "json" | "txt" | "md"
                ) {
                    return Ok(false);
                }
                if matches!(extension, "gd" | "tscn" | "tres" | "godot" | "cfg") {
                    if entry.metadata().map_err(|e| e.to_string())?.len() > 2 * 1024 * 1024 {
                        return Ok(false);
                    }
                    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
                    // Global script names and UID-only resources need editor metadata.
                    // Imported media, native extensions and add-ons take the full path.
                    if text.contains("class_name") || text.contains("uid://") {
                        return Ok(false);
                    }
                }
            } else {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn private_inspection_source(project: &Project) -> Result<PathBuf, String> {
    let diagnostic = std::env::var("AURUM_NATIVE_DIAGNOSTICS").as_deref() == Ok("1");
    let started = std::time::Instant::now();
    let godot = godot_root(project)?;
    let engine = engine_binary(project)?;
    let source_revision = crate::snapshot::fingerprint_all(godot)?;
    if diagnostic {
        eprintln!(
            "AURUM_INSPECTION_STAGE stage=source_fingerprint elapsed_ms={}",
            started.elapsed().as_millis()
        );
    }
    let identity_started = std::time::Instant::now();
    let runtime_revision = crate::sha256_file(&engine).map_err(|e| e.to_string())?;
    if diagnostic {
        eprintln!(
            "AURUM_INSPECTION_STAGE stage=runtime_identity elapsed_ms={}",
            identity_started.elapsed().as_millis()
        );
    }
    let revision = crate::sha256_hex(
        format!(
            "inspection-v2\n{}\n{source_revision}\n{runtime_revision}",
            project.root.display()
        )
        .as_bytes(),
    );
    // Keep native working directories short: Windows process startup and
    // Godot's helper tools still have stricter limits than Rust file I/O.
    let cache_root = std::env::temp_dir();
    let relative = format!("aurum-inspection-{revision}");
    let work = files::confined(&cache_root, &relative)?;
    let _cache_lock = crate::BuildLock::try_acquire(&work).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let ready = files::confined(&cache_root, &format!("{relative}/ready.json"))?;
    if let Some(stage) = std::fs::read(&ready)
        .ok()
        .filter(|bytes| bytes.len() <= 4096)
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| value["stage"].as_str().map(str::to_owned))
        .filter(|stage| {
            stage
                .strip_prefix("stage-")
                .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        })
    {
        let cached = files::confined(&cache_root, &format!("{relative}/{stage}/source"))?;
        if cached.join("project.godot").is_file() && cached.join(".godot").is_dir() {
            if diagnostic {
                eprintln!(
                    "AURUM_INSPECTION_STAGE stage=cache_reused elapsed_ms={}",
                    started.elapsed().as_millis()
                );
            }
            return Ok(cached);
        }
    }
    let stage_name = format!("stage-{}", crate::random::session_id());
    let stage = work.join(&stage_name);
    let source = stage.join("source");
    crate::snapshot::copy_excluding(godot, &source, &["addons/aurum_editor"])?;
    if crate::snapshot::fingerprint_all(godot)? != source_revision {
        return Err("Source changed while preparing scene inspection; reload after saving".into());
    }
    if plain_text_inspection_tree(&source)? {
        // Raw text scenes/scripts are loaded by the real runtime worker below.
        // Starting an editor merely to import a comment-only revision is unnecessary.
        // Never mark a media/native/global-class tree ready through this path.
        std::fs::create_dir_all(source.join(".godot")).map_err(|e| e.to_string())?;
        files::write_atomic(
            &ready,
            &serde_json::to_vec(&json!({"stage":stage_name,"plain_text":true}))
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if diagnostic {
            eprintln!(
                "AURUM_INSPECTION_STAGE stage=text_snapshot_ready elapsed_ms={}",
                started.elapsed().as_millis()
            );
        }
        return Ok(source);
    }
    let state = stage.join("native-state");
    std::fs::create_dir_all(&state).map_err(|e| e.to_string())?;
    let command = crate::Command::new(engine)
        .args([
            "--headless".into(),
            "--editor".into(),
            "--path".into(),
            source.display().to_string(),
            "--import".into(),
        ])
        .directory(&cache_root)
        .env("APPDATA", state.to_string_lossy())
        .env("LOCALAPPDATA", state.to_string_lossy())
        .env("XDG_DATA_HOME", state.to_string_lossy())
        .env("XDG_CONFIG_HOME", state.to_string_lossy())
        .env("XDG_CACHE_HOME", state.to_string_lossy());
    let mut outcome = crate::native_runtime::run(&command, Duration::from_secs(120))
        .map_err(|e| e.to_string())?;
    let mut log = format!("{}\n{}", outcome.stdout, outcome.stderr);
    if import_shutdown_crash(outcome.code, &log) {
        files::write_atomic(&stage.join("import-first-crash.log"), log.as_bytes())
            .map_err(|e| e.to_string())?;
        outcome = crate::native_runtime::run(&command, Duration::from_secs(120))
            .map_err(|e| e.to_string())?;
        log = format!("{}\n{}", outcome.stdout, outcome.stderr);
    }
    files::write_atomic(&stage.join("import.log"), log.as_bytes()).map_err(|e| e.to_string())?;
    if !outcome.success() || log.contains("ERROR:") {
        return Err(format!(
            "Private scene import failed: {} (wall_timeout={}). Evidence: {}",
            outcome.tail(20).join("\n"),
            outcome.timed_out,
            stage.display()
        ));
    }
    files::write_atomic(
        &ready,
        &serde_json::to_vec(&json!({"stage":stage_name})).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(source)
}

fn ensure_imported(project: &Project) -> Result<(), String> {
    let godot = godot_root(project)?;
    // Preview exports build a private copy. They must not block read-only inspection.
    // Only native import-cache writers share this separate lock.
    let _cache_lock =
        crate::BuildLock::try_acquire(&godot.join(".godot")).map_err(|e| e.to_string())?;
    let revision = crate::snapshot::fingerprint(godot)?;
    let stamp = files::confined(&project.root, ".aurum/import.json")?;
    let cached = std::fs::metadata(&stamp)
        .ok()
        .filter(|metadata| metadata.is_file() && metadata.len() <= 4096)
        .and_then(|_| std::fs::read(&stamp).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    if godot.join(".godot").is_dir()
        && cached
            .as_ref()
            .is_some_and(|value| value["source_sha256"] == revision)
    {
        return Ok(());
    }
    let args = vec![
        "--headless".into(),
        "--editor".into(),
        "--path".into(),
        godot.display().to_string(),
        "--import".into(),
    ];
    let mut result = engine_run(project, &args, Duration::from_secs(120))?;
    let log = result["log"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if import_shutdown_crash(
        result["exit_code"]
            .as_i64()
            .and_then(|code| i32::try_from(code).ok()),
        &log,
    ) {
        files::write_atomic(
            &files::confined(&project.root, ".aurum/import-first-crash.json")?,
            &serde_json::to_vec(&result).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        result = engine_run(project, &args, Duration::from_secs(120))?;
    }
    if result["ok"] != true {
        return Err(format!(
            "Project asset import failed before scene inspection: {result}"
        ));
    }
    // Importers may generate resource identifiers and remap metadata. Hash their final state.
    let value = json!({"source_sha256":crate::snapshot::fingerprint(godot)?});
    files::write_atomic(
        &stamp,
        &serde_json::to_vec(&value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plain_text_scenes_do_not_require_editor_import() {
        let root = std::env::temp_dir().join(format!(
            "aurum-text-inspection-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for (name, text) in [
            ("project.godot", "config_version=5"),
            ("main.tscn", "[gd_scene format=3]"),
            ("main.gd", "extends Node2D"),
            ("config.json", "{}"),
        ] {
            std::fs::write(root.join(name), text).unwrap();
        }
        assert!(plain_text_inspection_tree(&root).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn imported_media_and_native_extensions_never_take_text_fast_path() {
        let root = std::env::temp_dir().join(format!(
            "aurum-media-inspection-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for name in [
            "model.glb",
            "font.ttf",
            "texture.png",
            "module.gdextension",
            "main.gd.uid",
        ] {
            let path = root.join(name);
            std::fs::write(&path, b"fixture").unwrap();
            assert!(!plain_text_inspection_tree(&root).unwrap(), "{name}");
            std::fs::remove_file(path).unwrap();
        }
        std::fs::create_dir(root.join("addons")).unwrap();
        assert!(!plain_text_inspection_tree(&root).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn global_classes_and_uid_references_require_import_metadata() {
        let root = std::env::temp_dir().join(format!(
            "aurum-class-inspection-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for (name, text) in [
            ("main.gd", "class_name Actor\nextends Node3D"),
            ("scene.tscn", "[ext_resource path=\"uid://test\"]"),
            ("project.godot", "autoload=\"uid://test\""),
        ] {
            let path = root.join(name);
            std::fs::write(&path, text).unwrap();
            assert!(!plain_text_inspection_tree(&root).unwrap(), "{name}");
            std::fs::remove_file(path).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_completed_windows_import_shutdown_crashes_are_recoverable() {
        let complete = "[ DONE ] loading_editor_layout";
        assert_eq!(
            import_shutdown_crash(Some(-1073741819), complete),
            cfg!(windows)
        );
        for (code, log) in [
            (None, complete),
            (Some(1), complete),
            (Some(-1073741819), "reimport started"),
            (
                Some(-1073741819),
                "loading_editor_layout\nSCRIPT ERROR: bad script",
            ),
        ] {
            assert!(!import_shutdown_crash(code, log));
        }
    }
    #[test]
    fn play_options_are_bounded_and_user_arguments_are_validated() {
        assert_eq!(play_options(&json!({})).unwrap(), (120, None, vec![]));
        assert_eq!(
            play_options(&json!({"frames":36000,"fixed_fps":60,"user_args":["--acceptance"]}))
                .unwrap(),
            (36000, Some(60), vec!["--acceptance".into()])
        );
        for input in [
            json!({"frames":0}),
            json!({"frames":3600001}),
            json!({"frames":1.5}),
            json!({"fixed_fps":241}),
            json!({"user_args":"x"}),
            json!({"user_args":[1]}),
            json!({"user_args":["--aurum-report=secret"]}),
            json!({"user_args":["\0"]}),
            json!({"report":"yes"}),
        ] {
            assert!(play_options(&input).is_err(), "accepted {input}");
        }
    }
    #[test]
    fn play_report_requires_a_fresh_explicit_verdict() {
        let root =
            std::env::temp_dir().join(format!("aurum-play-report-{}", crate::random::session_id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("report.json");
        assert!(read_play_report(&path).is_err());
        for bytes in [b"{}".as_slice(), b"[]", b"{\"ok\":\"true\"}", b"not json"] {
            std::fs::write(&path, bytes).unwrap();
            assert!(read_play_report(&path).is_err());
        }
        std::fs::write(&path, b"{\"ok\":false,\"failures\":[\"collision\"]}").unwrap();
        assert_eq!(read_play_report(&path).unwrap()["ok"], false);
        std::fs::write(&path, b"{\"ok\":true,\"checks\":[\"collision\"]}").unwrap();
        assert_eq!(read_play_report(&path).unwrap()["ok"], true);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn snapshot_content_lease_blocks_commits_without_modifying_originals() {
        let root = std::env::temp_dir().join(format!(
            "aurum-content-commit-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = crate::project::clean_path(root.canonicalize().unwrap());
        let path = root.join("main.gd");
        commit_file(&root, &path, b"before", Some("")).unwrap();
        let lease = crate::content_lock::ContentLock::shared(&root).unwrap();
        assert!(
            commit_file(&root, &path, b"after", Some(&crate::sha256_hex(b"before")))
                .unwrap_err()
                .contains("content is busy")
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"before");
        drop(lease);
        commit_file(&root, &path, b"after", Some(&crate::sha256_hex(b"before"))).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"after");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_writes_are_refused_and_undo_preserves_the_previous_file() {
        let root = std::env::temp_dir().join(format!("aurum-ops-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let root = crate::project::clean_path(root.canonicalize().unwrap());
        let file = root.join("main.gd");
        commit_file(&root, &file, b"first", Some("")).unwrap();
        assert!(commit_file(&root, &file, b"lost", Some("wrong")).is_err());
        commit_file(&root, &file, b"second", Some(&crate::sha256_hex(b"first"))).unwrap();
        assert_eq!(
            std::fs::read(backup_path(&root, &file).unwrap()).unwrap(),
            b"first"
        );
        assert!(managed_path(&root, "../outside.gd").is_err());
        assert!(managed_path(&root, ".git/config").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
