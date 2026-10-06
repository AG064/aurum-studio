//! Isolated simulation and rendered captures through the same CLI/HTTP/MCP operation.
use crate::{files, project_ops, snapshot, Project};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

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

pub fn run(
    project: &Project,
    input: &Value,
    frames: u64,
    fixed_fps: Option<u64>,
    mut user_args: Vec<String>,
) -> Result<Value, String> {
    let timeout = timeout_seconds(input)?;
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
    let source = directory.join("source");
    let godot = project
        .godot_project_dir()
        .ok_or("project.godot was not found")?;
    snapshot::copy(godot, &source)?;
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
    files::write_atomic(&source.join("addons/aurum_live/playtest.json"), &serde_json::to_vec(&json!({"directory":directory,"frames":frames,"rendered":rendered,"capture_frames":captures,"events":events})).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let setup = directory.join("setup.gd");
    files::write_atomic(&setup, include_bytes!("playtest_setup.gd")).map_err(|e| e.to_string())?;
    let engine = project_ops::engine_binary(project)?;
    let execute = |args: Vec<String>, seconds: u64| -> Result<crate::process::Outcome, String> {
        crate::Command::new(&engine)
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
            )
            .run(Duration::from_secs(seconds))
            .map_err(|e| e.to_string())
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
    if let Some(scene) = input.get("scene").and_then(Value::as_str) {
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
    let runtime = read_json(&directory.join("runtime.json"))?;
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
        if let Some(report) = read_json(&report_path)? {
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
    files::write_atomic(
        &directory.join("verification.json"),
        &serde_json::to_vec_pretty(&result).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(result)
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
fn validate_events(value: &Value, frames: u64) -> Result<(), String> {
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
}
