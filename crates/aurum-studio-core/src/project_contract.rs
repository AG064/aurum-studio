//! Discovery for the project operations shared by CLI, HTTP and MCP.
use crate::project_ops::{is_read_only, EXPORT_PLATFORMS, OPERATIONS};
use serde_json::{json, Map, Value};

pub const MAX_FRAMES: u64 = 3_600_000;
pub const MAX_FIXED_FPS: u64 = 240;
pub const MAX_TIMEOUT_SECONDS: u64 = 600;
pub const MAX_CAPTURE_FRAMES: usize = 32;
pub const MAX_EVENTS: usize = 1024;

fn parameters() -> Value {
    json!({
        "op":{"type":"string"},
        "operation":{"type":"string","enum":OPERATIONS,"description":"Operation to describe, or diagnostic operation filter; omit for a compact catalog"},
        "path":{"type":"string","description":"Project-relative file path"},
        "scene":{"type":"string","description":"Scene path relative to the Godot project"},
        "text":{"type":"string","description":"UTF-8 text, at most 2 MiB"},
        "expected_sha256":{"type":"string","description":"Hash from the preceding read; stale writes are refused"},
        "draft_id":{"type":"string"},"base_sha256":{"type":"string"},
        "root_type":{"type":"string"},"name":{"type":"string"},
        "class":{"type":"string"},"query":{"type":"string"},
        "limit":{"type":"integer","minimum":1,"maximum":200,"default":50,"description":"Maximum local diagnostic records; reads scan at most 256 KiB"},
        "failures_only":{"type":"boolean","default":false},
        "changes":{"type":"array","minItems":1,"maxItems":64,"description":"Text change set for preflight or disposable candidate validation, never publication. At most 8 MiB aggregate text; transport body limits also apply.","items":{"type":"object","additionalProperties":false,"properties":{
            "path":{"type":"string","minLength":1,"maxLength":4096},
            "action":{"type":"string","enum":["create","replace","delete"]},
            "expected_sha256":{"type":"string","description":"Empty for create/absent; preceding SHA-256 required for replace/delete"},
            "text":{"type":"string","maxLength":2097152,"description":"UTF-8 bytes also limited to 2 MiB; required for create/replace, forbidden for delete"}
        },"required":["path","action","expected_sha256"]}},
        "operations":{"type":"array","maxItems":200,"items":{"type":"object","properties":{
            "op":{"type":"string","enum":["create","instance","set","remove","reparent","attach_script"]},
            "node":{"type":"string"},"parent":{"type":"string"},"name":{"type":"string"},"type":{"type":"string"},
            "script":{"type":"string"},"source":{"type":"string"},"properties":{"type":"object"}
        },"required":["op"]}},
        "frames":{"type":"integer","minimum":1,"maximum":MAX_FRAMES,"default":120},
        "fixed_fps":{"type":"integer","minimum":1,"maximum":MAX_FIXED_FPS,"description":"Fixed simulation rate; game logic may still be nondeterministic"},
        "timeout_seconds":{"type":"integer","minimum":1,"maximum":MAX_TIMEOUT_SECONDS,"default":90},
        "user_args":{"type":"array","maxItems":32,"items":{"type":"string","maxLength":4096},"description":"Game arguments after the engine separator; 4096 UTF-8 bytes each; --aurum-report is reserved"},
        "report":{"type":"boolean","description":"Require a fresh game-written JSON object with boolean ok"},
        "rendered":{"type":"boolean","description":"Use a renderer; capture always renders"},
        "width":{"type":"integer","minimum":320,"maximum":3840,"default":1280},
        "height":{"type":"integer","minimum":240,"maximum":2160,"default":720},
        "capture_frames":{"type":"array","maxItems":MAX_CAPTURE_FRAMES,"items":{"type":"integer","minimum":1,"maximum":MAX_FRAMES},"description":"Frame indices within the run budget; capture defaults to the final frame"},
        "events":{"type":"array","maxItems":MAX_EVENTS,"items":{"type":"object"},"description":"Bounded action, key, mouse_button or property timeline; op=describe operation=capture gives event fields"},
        "platform":{"type":"string","enum":EXPORT_PLATFORMS,"description":"Export target; SDKs, signing and device testing are separate requirements"},
        "release":{"type":"boolean"},"debug":{"type":"boolean"},"pack":{"type":"boolean"},
        "preset":{"type":"string"},"output":{"type":"string","description":"New project-relative destination; existing outputs are refused"}
    })
}

fn fields(op: &str) -> &'static [&'static str] {
    match op {
        "describe" => &["operation"],
        "logs" => &["limit", "operation", "failures_only"],
        "changes_check" | "changes_validate" => &["changes"],
        "read" | "draft_read" => &["path"],
        "write" => &["path", "text", "expected_sha256"],
        "draft_save" => &["path", "text", "base_sha256", "draft_id"],
        "draft_clear" => &["path", "draft_id"],
        "undo" => &["path", "expected_sha256"],
        "build" => &["release"],
        "play" | "capture" => &[
            "scene",
            "frames",
            "fixed_fps",
            "timeout_seconds",
            "user_args",
            "report",
            "rendered",
            "width",
            "height",
            "capture_frames",
            "events",
        ],
        "configure_export" => &["platform"],
        "export" => &["preset", "output", "debug", "pack"],
        "package" => &["output"],
        "scene_inspect" | "set_main_scene" => &["scene", "expected_sha256"],
        "scene_create" => &["scene", "root_type", "name", "expected_sha256"],
        "scene_edit" => &["scene", "operations", "expected_sha256"],
        "classes" => &["query"],
        "class_info" => &["class"],
        _ => &[],
    }
}

fn required(op: &str) -> &'static [&'static str] {
    match op {
        "changes_check" | "changes_validate" => &["changes"],
        "read" | "draft_read" | "undo" => &["path"],
        "draft_clear" => &["path", "draft_id"],
        "write" => &["path", "text"],
        "draft_save" => &["path", "text", "draft_id"],
        "export" => &["preset", "output"],
        "scene_inspect" | "scene_create" | "set_main_scene" => &["scene"],
        "scene_edit" => &["scene", "operations"],
        _ => &[],
    }
}

/// The MCP profile stays compact. Detailed per-operation schemas are queried on demand.
pub fn input_schema(read_only: bool) -> Value {
    let supported: Vec<_> = OPERATIONS
        .iter()
        .copied()
        .filter(|op| is_read_only(op) == read_only)
        .collect();
    let all = parameters();
    let mut properties = Map::new();
    properties.insert("op".into(), json!({"type":"string","enum":supported}));
    for op in &supported {
        for key in fields(op) {
            properties.insert((*key).into(), all[*key].clone());
        }
    }
    json!({"type":"object","properties":properties,"required":["op"]})
}

pub fn describe(operation: Option<&str>) -> Result<Value, String> {
    let Some(op) = operation else {
        return Ok(
            json!({"contract_version":1,"operations":OPERATIONS.iter().map(|op|json!({"op":op,"read_only":is_read_only(op),"required":required(op)})).collect::<Vec<_>>(),"execution_sandbox":false,"details":"describe with operation returns its request schema"}),
        );
    };
    if !OPERATIONS.contains(&op) {
        return Err(format!("unknown operation '{op}'"));
    }
    let all = parameters();
    let mut properties = Map::new();
    properties.insert("op".into(), json!({"type":"string","enum":[op]}));
    for key in fields(op) {
        properties.insert((*key).into(), all[*key].clone());
    }
    let mut required_fields = vec!["op"];
    required_fields.extend_from_slice(required(op));
    let mut result = json!({"contract_version":1,"op":op,"read_only":is_read_only(op),"input_schema":{"type":"object","properties":properties,"required":required_fields}});
    if op == "changes_validate" {
        result["constraints"] = json!({"qualified_platforms":["Windows"],"script_projects_only":true,"source_bytes_max":crate::candidates::MAX_SOURCE_BYTES,
            "applied":false,"gameplay_tested":false,"execution_sandboxed":false,"candidate_retained":false,
            "receipt":"changes_last returns the last completed receipt, not live freshness"});
    }
    if matches!(op, "play" | "capture") {
        result["event_types"] = json!({
            "action":{"required":["frame","action"],"optional":["type","pressed"],"example":{"frame":1,"type":"action","action":"move_forward","pressed":true}},
            "key":{"required":["frame","type","key"],"optional":["pressed"],"example":{"frame":1,"type":"key","key":"W","pressed":true}},
            "mouse_button":{"required":["frame","type","x","y"],"optional":["button","pressed"],"example":{"frame":1,"type":"mouse_button","x":640,"y":360,"button":1,"pressed":true}},
            "property":{"required":["frame","type","changes"],"example":{"frame":1,"type":"property","changes":[{"path":".","property":"speed","value":6.0}]}}
        });
        result["constraints"] = json!({"events_max_bytes":65536,"event_frame":"1..frames","capture_frame":"1..frames","report":"Fresh game-written JSON with boolean ok; not independent correctness proof","rendering":"capture requires a graphical driver; headless play does not prove graphics"});
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_backend_operation_is_discoverable_in_exactly_one_profile() {
        for op in OPERATIONS {
            let schema = input_schema(is_read_only(op));
            assert!(schema["properties"]["op"]["enum"]
                .as_array()
                .unwrap()
                .contains(&json!(op)));
            let details = describe(Some(op)).unwrap();
            assert_eq!(details["op"], *op);
            for key in fields(op) {
                assert!(
                    !details["input_schema"]["properties"][*key].is_null(),
                    "{op}.{key}"
                );
            }
            for key in required(op) {
                assert!(fields(op).contains(key), "{op}.{key}");
            }
        }
        assert!(describe(Some("execute_shell")).is_err());
    }
    #[test]
    fn captures_and_web_builds_are_advertised_with_backend_budgets() {
        let schema = input_schema(false);
        let ops = schema["properties"]["op"]["enum"].as_array().unwrap();
        assert!(ops.contains(&json!("capture")) && ops.contains(&json!("web_build")));
        assert_eq!(schema["properties"]["frames"]["maximum"], MAX_FRAMES);
        assert_eq!(
            schema["properties"]["timeout_seconds"]["maximum"],
            MAX_TIMEOUT_SECONDS
        );
        assert!(!schema["properties"]["events"].is_null());
        assert!(input_schema(true)["properties"].get("events").is_none());
    }
}
