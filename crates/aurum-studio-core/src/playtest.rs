//! Isolated simulation and rendered captures through the same CLI/HTTP/MCP operation.
use crate::{files, performance_budget, project_ops, snapshot, Project};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

pub fn timeout_seconds(input: &Value) -> Result<u64, String> {
    let seconds = input
        .get("timeout_seconds")
        .map(|v| v.as_u64().ok_or("'timeout_seconds' must be an integer"))
        .transpose()?
        .unwrap_or(90);
    if !(1..=crate::project_contract::MAX_TIMEOUT_SECONDS).contains(&seconds) {
        return Err("'timeout_seconds' must be between 1 and 600".into());
    }
    Ok(seconds)
}

pub(crate) fn seed(input: &Value) -> Result<Option<u32>, String> {
    input
        .get("seed")
        .map(|value| {
            value
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| "'seed' must be an integer between 0 and 4294967295".to_string())
        })
        .transpose()
}

pub fn run(
    project: &Project,
    input: &Value,
    frames: u64,
    fixed_fps: Option<u64>,
    user_args: Vec<String>,
) -> Result<Value, String> {
    seed(input)?;
    if input.get("performance").is_some()
        && crate::operation_context::OperationContext::current().is_none()
    {
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(timeout_seconds(input)?),
        );
        return context
            .scope(|| run_inner(project, input, frames, fixed_fps, user_args))
            .map_err(|error| error.to_string())?;
    }
    run_inner(project, input, frames, fixed_fps, user_args)
}
fn run_inner(
    project: &Project,
    input: &Value,
    frames: u64,
    fixed_fps: Option<u64>,
    mut user_args: Vec<String>,
) -> Result<Value, String> {
    let timeout = timeout_seconds(input)?;
    let performance = input
        .get("performance")
        .map(|value| performance_budget::Request::parse(value, frames, timeout))
        .transpose()?;
    if performance.is_some() && !cfg!(windows) {
        return Err("performance execution is Windows-qualified only; other platform execution qualification is pending".into());
    }
    let performance_deadline = performance.as_ref().map(|_| {
        let local = Instant::now() + Duration::from_secs(timeout);
        crate::operation_context::OperationContext::current()
            .map_or(local, |context| local.min(context.deadline()))
    });
    let request_id = crate::random::session_id();
    let rendered = input["op"] == "capture"
        || input
            .get("rendered")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if input.get("rendered").is_some_and(|v| !v.is_boolean()) {
        return Err("'rendered' must be a boolean".into());
    }
    let width = integer(input, "width", 1280, 320, 3840)?;
    let height = integer(input, "height", 720, 240, 2160)?;
    let captures = input.get("capture_frames").cloned().unwrap_or_else(|| {
        if rendered {
            json!([frames])
        } else {
            json!([])
        }
    });
    let list = captures
        .as_array()
        .ok_or("'capture_frames' must be an array")?;
    if list.len() > crate::project_contract::MAX_CAPTURE_FRAMES
        || list
            .iter()
            .any(|v| v.as_u64().is_none_or(|f| f == 0 || f > frames))
    {
        return Err("Capture frames must be within the run budget, at most 32 images".into());
    }
    if !rendered && !list.is_empty() {
        return Err("Captures require rendered=true or op=capture".into());
    }
    let events = input.get("events").cloned().unwrap_or_else(|| json!([]));
    validate_events(&events, frames)?;
    let directory = files::confined(
        &project.root,
        &format!(".aurum/playtests/{}", crate::random::session_id()),
    )?;
    let godot = project
        .godot_project_dir()
        .ok_or("project.godot was not found")?;
    let source_before = performance_deadline
        .map(|deadline| snapshot::fingerprint_all_bounded(godot, deadline))
        .transpose()?;
    let mut workspace = if performance.is_some() {
        Some(crate::candidate_workspace::Workspace::create()?)
    } else {
        None
    };
    let source = workspace.as_ref().map_or_else(
        || directory.join("source"),
        |workspace| workspace.path.join("source"),
    );
    if let Some(workspace) = &workspace {
        for name in ["userdata", "temp", "home"] {
            std::fs::create_dir(workspace.path.join(name))
                .map_err(|_| "performance private environment directory could not be created")?;
        }
    }
    if let Some(deadline) = performance_deadline {
        performance_copy(godot, &source, deadline)?;
    } else {
        snapshot::copy(godot, &source)?;
    }
    performance_check(performance_deadline)?;
    if let (Some(before), Some(deadline)) = (&source_before, performance_deadline) {
        if snapshot::fingerprint_all_bounded(godot, deadline)?.sha256 != before.sha256
            || snapshot::fingerprint_all_bounded(&source, deadline)?.sha256 != before.sha256
        {
            return Err("performance source changed during private copy".into());
        }
    }
    if source.join("addons/aurum_live").exists() {
        return Err("addons/aurum_live is reserved for private runtime snapshots".into());
    }
    files::write_atomic(
        &source.join("addons/aurum_live/runtime.gd"),
        include_bytes!("runtime_bridge.gd"),
    )
    .map_err(|e| e.to_string())?;
    files::write_atomic(
        &source.join("addons/aurum_live/playtest.gd"),
        include_bytes!("playtest_observer.gd"),
    )
    .map_err(|e| e.to_string())?;
    let mut options = json!({"directory":directory,"frames":frames,"rendered":rendered,"capture_frames":captures,"events":events});
    if let Some(seed) = seed(input)? {
        options["seed"] = json!(seed);
        if let Some(scene) = input.get("scene") {
            let scene = scene.as_str().ok_or("'scene' must be a string")?;
            files::confined(&source, scene)?;
            options["seed_scene"] = json!(format!("res://{}", scene.replace('\\', "/")));
        }
        // Seed before loading project scene code, not after its _ready callback.
        files::write_atomic(
            &source.join("addons/aurum_live/seed_host.tscn"),
            b"[gd_scene format=3]\n\n[node name=\"AurumSeedHost\" type=\"Node\"]\n",
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(request) = &performance {
        files::write_atomic(
            &source.join("addons/aurum_live/performance_sampler.gd"),
            include_bytes!("performance_sampler.gd"),
        )
        .map_err(|error| error.to_string())?;
        options["performance"] = request.options(&request_id)?;
    }
    files::write_atomic(
        &source.join("addons/aurum_live/playtest.json"),
        &serde_json::to_vec(&options).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let setup = directory.join("setup.gd");
    files::write_atomic(&setup, include_bytes!("playtest_setup.gd")).map_err(|e| e.to_string())?;
    let engine = project_ops::engine_binary(project)?;
    let executable_before = performance_deadline
        .map(|deadline| performance_executable_hash(&engine, deadline))
        .transpose()?;
    let execute = |args: Vec<String>, seconds: u64| -> Result<crate::process::Outcome, String> {
        performance_check(performance_deadline)?;
        let local_budget =
            performance_deadline.map_or(Ok(Duration::from_secs(seconds)), |deadline| {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    Err("performance host deadline expired".to_string())
                } else {
                    Ok(remaining.min(Duration::from_secs(seconds)))
                }
            })?;
        let mut command = crate::Command::new(&engine)
            .args(args)
            .directory(&directory)
            .env("APPDATA", directory.join("userdata").display().to_string())
            .env(
                "XDG_DATA_HOME",
                directory.join("userdata").display().to_string(),
            )
            .env(
                "XDG_CONFIG_HOME",
                directory.join("config").display().to_string(),
            );
        if let Some(workspace) = &workspace {
            command = command
                .candidate_storage(&workspace.path)
                .lifetime_fence(&workspace.lifetime);
            let userdata = workspace.path.join("userdata").display().to_string();
            for key in [
                "APPDATA",
                "LOCALAPPDATA",
                "XDG_DATA_HOME",
                "XDG_CONFIG_HOME",
                "XDG_CACHE_HOME",
            ] {
                command = command.env(key, &userdata);
            }
            for key in ["TEMP", "TMP", "TMPDIR"] {
                command = command.env(key, workspace.path.join("temp").display().to_string());
            }
            for key in ["USERPROFILE", "HOME"] {
                command = command.env(key, workspace.path.join("home").display().to_string());
            }
        }
        command.run(local_budget).map_err(|e| e.to_string())
    };
    let setup_result = execute(
        vec![
            "--headless".into(),
            "--path".into(),
            source.display().to_string(),
            "--script".into(),
            setup.display().to_string(),
        ],
        30,
    )?;
    if !setup_result.success() || setup_result.stderr.contains("ERROR:") {
        return Err(format!(
            "Playtest setup failed: {}",
            setup_result.tail(20).join("\n")
        ));
    }
    // Import in the disposable snapshot so first-load font/script caches never alter source.
    let import_args = vec![
        "--headless".into(),
        "--editor".into(),
        "--path".into(),
        source.display().to_string(),
        "--import".into(),
    ];
    let mut imported = execute(import_args.clone(), 120)?;
    let mut import_log = format!("{}\n{}", imported.stdout, imported.stderr);
    files::write_atomic(&directory.join("import.log"), import_log.as_bytes())
        .map_err(|e| e.to_string())?;
    let import_retried = project_ops::import_shutdown_crash(imported.code, &import_log);
    if import_retried {
        files::write_atomic(
            &directory.join("import-first-crash.log"),
            import_log.as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        imported = execute(import_args, 120)?;
        import_log = format!("{}\n{}", imported.stdout, imported.stderr);
        files::write_atomic(&directory.join("import.log"), import_log.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    if !imported.success() || imported.stderr.contains("ERROR:") {
        return Err(format!(
            "Playtest import failed (exit {:?}, wall_timeout={}): {}\nEvidence: {}",
            imported.code,
            imported.timed_out,
            imported.tail(20).join("\n"),
            directory.display()
        ));
    }
    let prepared_before = performance_deadline
        .map(|deadline| snapshot::fingerprint_all_bounded(&source, deadline))
        .transpose()?;
    let mut args = vec![
        "--path".into(),
        source.display().to_string(),
        "--audio-driver".into(),
        "Dummy".into(),
        "--quit-after".into(),
        (frames + 10).to_string(),
    ];
    if rendered {
        args.extend([
            "--rendering-method".into(),
            "gl_compatibility".into(),
            "--rendering-driver".into(),
            "opengl3".into(),
            "--resolution".into(),
            format!("{width}x{height}"),
            "--position".into(),
            "-32000,-32000".into(),
            "--disable-vsync".into(),
        ]);
    } else {
        args.push("--headless".into());
    }
    if let Some(fps) = fixed_fps {
        args.extend(["--fixed-fps".into(), fps.to_string()]);
    }
    if seed(input)?.is_some() {
        args.extend([
            "--scene".into(),
            source
                .join("addons/aurum_live/seed_host.tscn")
                .display()
                .to_string(),
        ]);
    } else if let Some(scene) = input.get("scene").and_then(Value::as_str) {
        args.extend([
            "--scene".into(),
            files::confined(&source, scene)?.display().to_string(),
        ]);
    }
    let report_path = directory.join("report.json");
    if input["report"] == true {
        user_args.extend(["--aurum-report".into(), report_path.display().to_string()]);
    }
    if !user_args.is_empty() {
        args.push("--".into());
        args.extend(user_args);
    }
    let outcome = execute(args, timeout)?;
    files::write_atomic(&directory.join("stdout.log"), outcome.stdout.as_bytes())
        .map_err(|e| e.to_string())?;
    files::write_atomic(&directory.join("stderr.log"), outcome.stderr.as_bytes())
        .map_err(|e| e.to_string())?;
    let errors: Vec<_> = outcome
        .stdout
        .lines()
        .chain(outcome.stderr.lines())
        .filter(|line| line.contains("ERROR:"))
        .take(30)
        .collect();
    let runtime = if let Some(deadline) = performance_deadline {
        read_performance_json(&directory.join("runtime.json"), deadline)?
    } else {
        read_json(&directory.join("runtime.json"))?
    };
    let mut reason = if outcome.timed_out {
        "wall_timeout"
    } else if !outcome.success() {
        "process_error"
    } else {
        runtime
            .as_ref()
            .and_then(|r| r["reason"].as_str())
            .unwrap_or("runtime_receipt_missing")
    }
    .to_owned();
    let capture_count = runtime
        .as_ref()
        .and_then(|r| r["captures"].as_array())
        .map_or(0, Vec::len);
    let runtime_ok = runtime
        .as_ref()
        .is_some_and(|r| r["errors"].as_array().is_some_and(Vec::is_empty));
    let mut result = json!({"ok":outcome.success() && errors.is_empty() && runtime_ok && (!rendered || capture_count == list.len()),"exit_code":outcome.code,"timed_out":outcome.timed_out,"frames_requested":frames,"timeout_seconds":timeout,"rendered":rendered,"runtime":runtime,"evidence_directory":directory,"errors":errors,"log":outcome.tail(60)});
    if input["report"] == true {
        result["report_path"] = json!(report_path);
        let report = if let Some(deadline) = performance_deadline {
            read_performance_json(&report_path, deadline)?
        } else {
            read_json(&report_path)?
        };
        if let Some(report) = report {
            if !report["ok"].is_boolean() {
                result["ok"] = json!(false);
                result["report_error"] = json!("game report must have a boolean 'ok' field");
            } else {
                result["ok"] = json!(result["ok"] == true && report["ok"] == true);
                result["report"] = report;
            }
        } else {
            result["ok"] = json!(false);
            result["report_error"] = json!(format!("No gameplay verdict was written; termination reason: {reason}. Inspect runtime.last_state and the retained logs."));
        }
    } else if reason == "frame_budget_exhausted" && result["ok"] == true {
        reason = "bounded_run_complete".into();
    }
    result["termination_reason"] = json!(reason);
    result["first_import_shutdown_retry"] = json!(import_retried);
    if let Some(request) = &performance {
        performance_check(performance_deadline)?;
        let deadline = performance_deadline.ok_or("performance deadline is missing")?;
        let assessed = match read_performance_json(&directory.join("performance.json"), deadline) {
            Ok(Some(producer)) => performance_budget::evaluate(request, &request_id, &producer)
                .unwrap_or_else(|_| json!({"ok":false,"reason":"invalid_performance_receipt"})),
            Ok(None) => json!({"ok":false,"reason":"performance_receipt_missing"}),
            Err(_) => json!({"ok":false,"reason":"unavailable_bounded_performance_receipt"}),
        };
        let source_after = snapshot::fingerprint_all_bounded(godot, deadline)?;
        let private_after = snapshot::fingerprint_all_bounded(&source, deadline)?;
        let executable_after = performance_executable_hash(&engine, deadline)?;
        let source_current = source_before
            .as_ref()
            .is_some_and(|before| before.sha256 == source_after.sha256);
        let private_intact = prepared_before
            .as_ref()
            .is_some_and(|before| before.sha256 == private_after.sha256);
        let executable_current = executable_before.as_ref() == Some(&executable_after);
        result["ok"] = json!(
            result["ok"] == true
                && assessed["ok"] == true
                && source_current
                && private_intact
                && executable_current
        );
        result["performance"] = assessed;
        result["performance_identity"] = json!({"working_source_sha256":source_before.as_ref().map(|value|&value.sha256),"prepared_source_sha256":prepared_before.as_ref().map(|value|&value.sha256),"executable_disk_sha256":executable_before,"source_still_current":source_current,"private_source_intact":private_intact,"executable_disk_still_current":executable_current,"runtime_pid_independently_attributed":false,"mapped_memory_hash_verified":false,"source_contents_returned":false});
        result["execution_sandboxed"] = json!(false);
        if let Some(workspace) = &mut workspace {
            let cleanup = workspace.clean();
            result["private_workspace_cleanup_verified"] = json!(cleanup.is_ok());
            if cleanup.is_err() {
                result["ok"] = json!(false);
                result["private_workspace_retained"] = json!(true);
            }
        }
    }
    files::write_atomic(
        &directory.join("verification.json"),
        &serde_json::to_vec_pretty(&result).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(result)
}

fn performance_check(deadline: Option<Instant>) -> Result<(), String> {
    if let Some(deadline) = deadline {
        if let Some(context) = crate::operation_context::OperationContext::current() {
            context.check().map_err(|error| error.to_string())?;
        }
        if Instant::now() >= deadline {
            return Err("performance host deadline expired".into());
        }
    }
    Ok(())
}
fn performance_executable_hash(path: &Path, deadline: Instant) -> Result<String, String> {
    #[cfg(windows)]
    {
        crate::runtime_context::file_sha256_bounded(path, 512 * 1024 * 1024, deadline)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, deadline);
        Err("performance executable identity is Windows-qualified only".into())
    }
}
fn performance_copy(source: &Path, destination: &Path, deadline: Instant) -> Result<(), String> {
    fn ordinary(metadata: &std::fs::Metadata) -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return false;
            }
        }
        !metadata.file_type().is_symlink() && (metadata.is_file() || metadata.is_dir())
    }
    struct CopyState {
        entries: u64,
        files: u64,
        bytes: u64,
        deadline: Instant,
    }
    fn walk(
        root: &Path,
        source: &Path,
        destination: &Path,
        depth: u32,
        state: &mut CopyState,
    ) -> Result<(), String> {
        performance_check(Some(state.deadline))?;
        if depth > 24 {
            return Err("performance source nesting exceeds limit".into());
        }
        let held = crate::snapshot::open_checked_directory(source, state.deadline)?;
        let destination_parent = destination
            .parent()
            .ok_or("private performance destination has no parent")?;
        let parent_guard =
            crate::snapshot::open_checked_directory(destination_parent, state.deadline)?;
        std::fs::create_dir(destination)
            .map_err(|_| "private performance directory could not be created fresh")?;
        let destination_guard =
            crate::snapshot::open_checked_directory(destination, state.deadline)?;
        let mut entries = Vec::new();
        for entry in
            std::fs::read_dir(source).map_err(|_| "performance source enumeration failed")?
        {
            performance_check(Some(state.deadline))?;
            state.entries += 1;
            if state.entries > 40000 {
                return Err("performance source entry limit exceeded".into());
            }
            entries.push(entry.map_err(|_| "performance source entry unavailable")?);
        }
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            performance_check(Some(state.deadline))?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "performance source escaped root")?
                .to_str()
                .ok_or("performance source path must be UTF-8")?
                .replace('\\', "/");
            if relative.len() > 1024 {
                return Err("performance source path exceeds limit".into());
            }
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|_| "performance source metadata unavailable")?;
            if !ordinary(&metadata) {
                return Err("performance source link or nonordinary entry refused".into());
            }
            if !snapshot::includes(&relative) {
                continue;
            }
            let target = destination.join(entry.file_name());
            if metadata.is_dir() {
                walk(root, &path, &target, depth + 1, state)?;
                continue;
            }
            state.files += 1;
            state.bytes = state
                .bytes
                .checked_add(metadata.len())
                .ok_or("performance source size overflow")?;
            if state.files > 20000 || state.bytes > 512 * 1024 * 1024 {
                return Err("performance source size or file limit exceeded".into());
            }
            let mut input = crate::snapshot::open_checked_file(&path, state.deadline)?;
            let expected = input
                .metadata()
                .map_err(|_| "performance source handle metadata unavailable")?
                .len();
            if expected != metadata.len() {
                return Err("performance source changed before copy".into());
            }
            let mut output = performance_output(&target, state.deadline)?;
            #[cfg(test)]
            PERFORMANCE_OUTPUT_OBSERVER.with(|observer| {
                if let Some(observer) = observer.borrow_mut().as_mut() {
                    observer(&target, &output);
                }
            });
            check_performance_output(&output, &target, 0, state.deadline)?;
            let mut copied = 0u64;
            let mut chunk = [0u8; 65536];
            loop {
                performance_check(Some(state.deadline))?;
                let capacity = (expected + 1 - copied).min(chunk.len() as u64) as usize;
                let count = input
                    .read(&mut chunk[..capacity])
                    .map_err(|_| "performance source read failed")?;
                if count == 0 {
                    break;
                }
                copied += count as u64;
                if copied > expected {
                    return Err("performance source grew during copy".into());
                }
                check_performance_output(&output, &target, copied - count as u64, state.deadline)?;
                output
                    .write_all(&chunk[..count])
                    .map_err(|_| "private performance source write failed")?;
                check_performance_output(&output, &target, copied, state.deadline)?;
            }
            if copied != expected {
                return Err("performance source shrank during copy".into());
            }
            crate::snapshot::check_opened_path(&input, &path, state.deadline)?;
            check_performance_output(&output, &target, copied, state.deadline)?;
            output
                .sync_all()
                .map_err(|_| "private performance source sync failed")?;
            check_performance_output(&output, &target, copied, state.deadline)?;
        }
        crate::snapshot::check_opened_path(&held, source, state.deadline)?;
        crate::snapshot::check_opened_path(&destination_guard, destination, state.deadline)?;
        crate::snapshot::check_opened_path(&parent_guard, destination_parent, state.deadline)
    }
    walk(
        source,
        source,
        destination,
        0,
        &mut CopyState {
            entries: 0,
            files: 0,
            bytes: 0,
            deadline,
        },
    )
}

fn performance_output(path: &Path, deadline: Instant) -> Result<std::fs::File, String> {
    performance_check(Some(deadline))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .access_mode(0x40000000 | 0x80)
            .custom_flags(0x00200000)
            .share_mode(1);
    }
    let file = options
        .open(path)
        .map_err(|_| "private performance file could not be created")?;
    check_performance_output(&file, path, 0, deadline)?;
    Ok(file)
}
fn check_performance_output(
    file: &std::fs::File,
    path: &Path,
    expected: u64,
    deadline: Instant,
) -> Result<(), String> {
    performance_check(Some(deadline))?;
    let metadata = file
        .metadata()
        .map_err(|_| "private performance output metadata unavailable")?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("private performance output reparse point refused".into());
        }
    }
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != expected {
        return Err("private performance output type or size changed".into());
    }
    crate::snapshot::check_opened_path(file, path, deadline)
}
#[cfg(test)]
type PerformanceOutputObserver = Box<dyn FnMut(&Path, &std::fs::File)>;
#[cfg(test)]
thread_local! {
    static PERFORMANCE_OUTPUT_OBSERVER:std::cell::RefCell<Option<PerformanceOutputObserver>>=const{std::cell::RefCell::new(None)};
}
fn read_performance_json(path: &Path, deadline: Instant) -> Result<Option<Value>, String> {
    performance_check(Some(deadline))?;
    if !path.exists() {
        return Ok(None);
    }
    let parent = path.parent().ok_or("performance receipt has no parent")?;
    let directory = crate::snapshot::open_checked_directory(parent, deadline)?;
    let mut file = crate::snapshot::open_checked_file(path, deadline)?;
    let expected = file
        .metadata()
        .map_err(|_| "performance metadata unavailable")?
        .len();
    if expected > performance_budget::RECEIPT_BYTES as u64 {
        return Err("performance receipt byte bound exceeded".into());
    }
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        performance_check(Some(deadline))?;
        let capacity =
            ((expected + 1).saturating_sub(bytes.len() as u64)).min(chunk.len() as u64) as usize;
        if capacity == 0 {
            return Err("performance receipt grew during read".into());
        }
        let count = file
            .read(&mut chunk[..capacity])
            .map_err(|_| "performance receipt read failed")?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() as u64 > expected {
            return Err("performance receipt grew during read".into());
        }
    }
    if bytes.len() as u64 != expected {
        return Err("performance receipt shrank during read".into());
    }
    crate::snapshot::check_opened_path(&directory, parent, deadline)?;
    crate::snapshot::check_opened_path(&file, path, deadline)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "invalid bounded performance JSON".into())
}

fn integer(input: &Value, key: &str, default: u64, min: u64, max: u64) -> Result<u64, String> {
    let value = input
        .get(key)
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| format!("'{key}' must be an integer"))
        })
        .transpose()?
        .unwrap_or(default);
    if !(min..=max).contains(&value) {
        return Err(format!("'{key}' must be between {min} and {max}"));
    }
    Ok(value)
}
fn read_json(path: &Path) -> Result<Option<Value>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err("Playtest receipts must be regular JSON files of at most 1 MiB".into());
    }
    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map(Some)
        .map_err(|e| e.to_string())
}
pub(crate) fn validate_events(value: &Value, frames: u64) -> Result<(), String> {
    let events = value.as_array().ok_or("'events' must be an array")?;
    if events.len() > crate::project_contract::MAX_EVENTS
        || serde_json::to_vec(value).map_err(|e| e.to_string())?.len() > 64 * 1024
    {
        return Err("Input timeline exceeds 1024 events or 64 KiB".into());
    }
    for event in events {
        if event["frame"].as_u64().is_none_or(|f| f == 0 || f > frames) {
            return Err("Event frame must be within the run budget".into());
        }
        if event.get("pressed").is_some_and(|v| !v.is_boolean()) {
            return Err("Event pressed must be a boolean".into());
        }
        match event["type"].as_str().unwrap_or("action") {
            "action" | "key" => {
                let key = if event["type"] == "key" {
                    "key"
                } else {
                    "action"
                };
                if event[key]
                    .as_str()
                    .is_none_or(|s| s.is_empty() || s.len() > 64)
                {
                    return Err(
                        "Input action/key must be a nonempty string of at most 64 bytes".into(),
                    );
                }
            }
            "mouse_button" => {
                if ["x", "y"].iter().any(|key| {
                    event[key]
                        .as_f64()
                        .is_none_or(|n| !n.is_finite() || n.abs() > 8192.0)
                }) || event
                    .get("button")
                    .is_some_and(|v| v.as_u64().is_none_or(|n| !(1..=9).contains(&n)))
                {
                    return Err("Invalid mouse input event".into());
                }
            }
            "property" => {
                if event["changes"]
                    .as_array()
                    .is_none_or(|a| a.is_empty() || a.len() > 128)
                {
                    return Err("Property events require 1 to 128 changes".into());
                }
            }
            _ => return Err("Unsupported input event type".into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budgets_and_timelines_are_bounded() {
        assert_eq!(
            timeout_seconds(&json!({"timeout_seconds":600})).unwrap(),
            600
        );
        for input in [
            json!({"timeout_seconds":0}),
            json!({"timeout_seconds":601}),
            json!({"timeout_seconds":1.5}),
        ] {
            assert!(timeout_seconds(&input).is_err());
        }
        assert!(validate_events(
            &json!([{"frame":1,"action":"move_right","pressed":true}]),
            90000
        )
        .is_ok());
        for events in [
            json!([{"frame":0,"action":"a"}]),
            json!([{"frame":2,"type":"execute"}]),
            json!([{"frame":1,"type":"key","key":"W","pressed":4}]),
            json!([{"frame":1,"type":"mouse_button","x":"bad","y":0}]),
        ] {
            assert!(validate_events(&events, 10).is_err());
        }
    }

    #[cfg(windows)]
    #[test]
    fn performance_copy_held_output_refuses_rename_and_redirecting_mutation_handle() {
        use std::cell::Cell;
        use std::os::windows::fs::OpenOptionsExt;
        use std::rc::Rc;
        use std::time::SystemTime;
        let temporary = std::env::temp_dir().canonicalize().unwrap();
        let leaf = format!(
            "aurum-performance-output-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let fixture = temporary.join(&leaf);
        assert_eq!(fixture.parent(), Some(temporary.as_path()));
        std::fs::create_dir(&fixture).unwrap();
        struct OwnedFixture {
            path: std::path::PathBuf,
            parent: std::path::PathBuf,
        }
        impl Drop for OwnedFixture {
            fn drop(&mut self) {
                if std::thread::panicking() {
                    eprintln!(
                        "Performance output fixture retained: {}",
                        self.path.display()
                    );
                    return;
                }
                let resolved = self.path.canonicalize().unwrap();
                assert_eq!(resolved.parent(), Some(self.parent.as_path()));
                assert_eq!(resolved, self.path);
                std::fs::remove_dir_all(&resolved).unwrap();
            }
        }
        let _fixture = OwnedFixture {
            path: fixture.clone(),
            parent: temporary,
        };
        let source = fixture.join("source");
        let destination = fixture.join("copy");
        let outside = fixture.join("outside");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let sentinel = outside.join("sentinel.bin");
        let sentinel_bytes = b"owned outside sentinel must remain exact";
        std::fs::write(&sentinel, sentinel_bytes).unwrap();
        let payload = vec![0x5a; 130_001];
        std::fs::write(source.join("payload.bin"), &payload).unwrap();
        let called = Rc::new(Cell::new(false));
        let observed = called.clone();
        let allowed_root = fixture.clone();
        let outside_sentinel = sentinel.clone();
        struct ObserverReset;
        impl Drop for ObserverReset {
            fn drop(&mut self) {
                PERFORMANCE_OUTPUT_OBSERVER.with(|observer| {
                    observer.borrow_mut().take();
                });
            }
        }
        let _reset = ObserverReset;
        PERFORMANCE_OUTPUT_OBSERVER.with(|observer| {
            *observer.borrow_mut() = Some(Box::new(move |target, held| {
                assert!(target.starts_with(&allowed_root));
                assert_eq!(target.file_name().unwrap(), "payload.bin");
                assert_eq!(held.metadata().unwrap().len(), 0);
                assert!(std::fs::rename(target, &outside_sentinel).is_err());
                // A redirecting reparse mutation or handle-based rename first
                // requires write/delete access. This must be refused because
                // the production output itself shares read access only.
                let mutation = std::fs::OpenOptions::new()
                    .access_mode(0x40000000 | 0x10000 | 0x80)
                    .share_mode(7)
                    .custom_flags(0x00200000)
                    .open(target);
                let error = mutation.unwrap_err();
                assert!(
                    matches!(error.raw_os_error(), Some(5 | 32)),
                    "unexpected mutation refusal: {error:?}"
                );
                let write = std::fs::OpenOptions::new()
                    .write(true)
                    .share_mode(7)
                    .custom_flags(0x00200000)
                    .open(target);
                let error = write.unwrap_err();
                assert!(
                    matches!(error.raw_os_error(), Some(5 | 32)),
                    "unexpected write refusal: {error:?}"
                );
                assert_eq!(std::fs::read(&outside_sentinel).unwrap(), sentinel_bytes);
                assert_eq!(held.metadata().unwrap().len(), 0);
                observed.set(true);
            }));
        });
        performance_copy(
            &source,
            &destination,
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
        assert!(called.get());
        assert_eq!(
            std::fs::read(destination.join("payload.bin")).unwrap(),
            payload
        );
        assert_eq!(std::fs::read(&sentinel).unwrap(), sentinel_bytes);
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    }
}
