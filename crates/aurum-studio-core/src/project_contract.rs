//! Discovery for the project operations shared by CLI, HTTP and MCP.
use crate::project_ops::{is_read_only, EXPORT_PLATFORMS, OPERATIONS};
use serde_json::{json, Map, Value};

pub const MAX_FRAMES: u64 = 3_600_000;
pub const MAX_FIXED_FPS: u64 = 240;
pub const MAX_TIMEOUT_SECONDS: u64 = 600;
pub const MAX_CAPTURE_FRAMES: usize = 32;
pub const MAX_EVENTS: usize = 1024;

fn parameters() -> Value {
    let mut result = json!({
        "op":{"type":"string"},
        "operation":{"type":"string","enum":OPERATIONS,"description":"Operation to describe, or diagnostic operation filter; omit for a compact catalog"},
        "path":{"type":"string","description":"Project-relative file path"},
        "scene":{"type":"string","description":"Scene path relative to the Godot project"},
        "text":{"type":"string","description":"UTF-8 text, at most 2 MiB"},
        "expected_sha256":{"type":"string","description":"Hash from the preceding read; stale writes are refused"},
        "draft_id":{"type":"string"},"base_sha256":{"type":"string"},
        "root_type":{"type":"string"},"name":{"type":"string"},
        "class":{"type":"string"},"query":{"type":"string"},
        "node_path":{"type":"string","maxLength":512,"default":".","description":"Path within the current native scene, not a filesystem path"},
        "include_state":{"type":"boolean","default":false},
        "input_actions":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"object","properties":{"action":{"type":"string","maxLength":128},"pressed":{"type":"boolean"}},"required":["action","pressed"]},"description":"Registered Godot action events inside the owned game, not OS keyboard input"},
        "properties":{"type":"array","minItems":1,"maxItems":128,"items":{"type":"object","properties":{"path":{"type":"string","maxLength":512},"property":{"type":"string","maxLength":256},"value":{}},"required":["path","property","value"]}},
        "limit":{"type":"integer","minimum":1,"maximum":200,"default":50,"description":"Maximum log, revision detail or context delta rows; logs scan at most 256 KiB"},
        "failures_only":{"type":"boolean","default":false},
        "revision_id":{"type":"string","pattern":"^[a-fA-F0-9]{32}$","description":"Committed revision identity for guarded whole-revision undo"},
        "userdata_id":{"type":"string","pattern":"^[a-f0-9]{32}$","description":"Non-current, unowned saved-data generation selected for permanent deletion"},
        "changes":{"type":"array","minItems":1,"maxItems":64,"description":"Text change set for preflight, disposable validation or validated journaled publication. At most 8 MiB aggregate text; transport body limits also apply.","items":{"type":"object","additionalProperties":false,"properties":{
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
    });
    result["prefix"] = json!({"type":"string","maxLength":1024,"description":"Public project-relative context scope; omit for all public source"});
    result["extensions"] = json!({"type":"array","maxItems":16,"items":{"type":"string","maxLength":33},"description":"Case-insensitive file extensions, for example gd, tscn or rs; omit for all"});
    result["baseline_id"] = json!({"type":"string","pattern":"^[a-f0-9]{64}$","description":"Retained context baseline identity"});
    result["current_id"] = json!({"type":"string","pattern":"^[a-f0-9]{64}$","description":"Content identity from the preceding delta page; required with after"});
    result["after"] = json!({"type":"string","maxLength":1024,"description":"next_after from the preceding context page"});
    result["job_id"] = json!({"type":"string","pattern":"^[a-f0-9]{32}$","description":"Background job identity from job_submit"});
    result["owner_capability"] = json!({"type":"string","pattern":"^[a-f0-9]{64}$","description":"Private owner capability returned by the first submission; required for reconnect, results and cancellation"});
    result["after_sequence"] = json!({"type":"integer","minimum":0,"description":"Last observed monotonic job progress sequence"});
    result["request"] = json!({"type":"object","required":["op"],"description":"One supported project operation; nested jobs, project scope overrides and controller shutdown are refused. Detailed schemas remain available through describe."});
    result
}

fn fields(op: &str) -> &'static [&'static str] {
    match op {
        "describe" => &["operation"],
        "logs" => &["limit", "operation", "failures_only"],
        "context_snapshot" => &["prefix", "extensions"],
        "context_delta" => &["baseline_id", "current_id", "after", "limit"],
        "context_forget" => &["baseline_id"],
        "job_submit" => &["request", "owner_capability", "timeout_seconds"],
        "job_status" => &["job_id", "owner_capability", "after_sequence"],
        "job_result" | "job_cancel" | "job_forget" => &["job_id", "owner_capability"],
        "job_list" | "job_reconnect" => &["owner_capability", "limit"],
        "changes_check" | "changes_validate" | "changes_apply" => &["changes"],
        "changes_undo" | "changes_forget" => &["revision_id"],
        "changes_history" => &["revision_id", "limit"],
        "native_start" => &["rendered"],
        "native_inspect" => &["node_path", "include_state"],
        "native_apply" => &["properties"],
        "native_input" => &["input_actions"],
        "native_userdata_forget" => &["userdata_id"],
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
        "context_delta" | "context_forget" => &["baseline_id"],
        "job_submit" => &["request"],
        "job_status" | "job_result" | "job_cancel" | "job_forget" => {
            &["job_id", "owner_capability"]
        }
        "job_list" | "job_reconnect" => &["owner_capability"],
        "changes_check" | "changes_validate" | "changes_apply" => &["changes"],
        "changes_undo" | "changes_forget" => &["revision_id"],
        "native_apply" => &["properties"],
        "native_input" => &["input_actions"],
        "native_userdata_forget" => &["userdata_id"],
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
    if crate::project_jobs::supported(op) {
        result["constraints"] = json!({"active_max":crate::jobs::MAX_ACTIVE,"queued_max":crate::jobs::MAX_QUEUED,"terminal_receipts_max":crate::jobs::MAX_TERMINAL,
            "request_bytes_max":crate::jobs::MAX_REQUEST_BYTES,"retained_result_bytes_max":crate::jobs::MAX_RESULT_BYTES,"default_timeout_seconds":120,
            "deadline":"includes queue waiting and operation work","status":"monotonic bounded summaries; job_result retrieves retained result separately",
            "owner":"random private capability; caller owner names confer no authority",
            "reconnect":"explicit job_reconnect starts a controller and loads summaries; interrupted mutations are never replayed",
            "cancellation":"only owned transient workers; committed effects remain committed; unresolved commit/rollback reports uncertainty and recovery evidence",
            "export":"background export requires a fresh output directory; executable, pack and sidecars publish together after bounded private staging (1 GiB maximum)",
            "execution_sandboxed":false});
    }
    if matches!(op, "context_snapshot" | "context_delta" | "context_forget") {
        result["constraints"] = json!({"baseline_limit":crate::agent_context::BASELINES,"baseline_bytes_max":crate::agent_context::RECORD_BYTES,"scope_source_bytes_max":crate::agent_context::SOURCE_BYTES,
            "files_max":20000,"page_limit_max":200,"content_in_results":false,"automatic_pruning":false,
            "delta":"read-only; does not advance baseline; pagination requires unchanged current_id",
            "scope":"content-hashed public source, not native runtime state or knowledge; external writers do not share an atomic filesystem snapshot",
            "savings":"compact context is not a measured model-token saving"});
    }
    if matches!(op, "changes_validate" | "changes_apply") {
        result["constraints"] = json!({"qualified_platforms":["Windows"],"script_projects_only":true,"source_bytes_max":crate::candidates::MAX_SOURCE_BYTES,
            "gameplay_tested":false,"execution_sandboxed":false,
            "workspace_reserved_bytes":crate::candidate_workspace::WORKSPACE_BYTES,"workspace_slots":crate::candidate_workspace::SLOTS,
            "storage":"observed bytes and shared admission, not a hard filesystem quota",
            "cleanup":"normal cleanup and recognized orphan reclamation; failures report retained state",
            "worker_lifecycle":"Windows 10+ atomically assigned kill-on-host-exit job; no unowned fallback",
            "receipt":"changes_last returns the last completed receipt, not live freshness"});
        if op == "changes_validate" {
            result["constraints"]["applied"] = json!(false);
        } else {
            result["constraints"]["publication"] = json!("validated journaled text revision; stale sources and changed proposals are refused");
        }
    }
    if op == "changes_history" {
        result["constraints"] = json!({"revisions_max":32,"scan_entries_max":256,"detail_bytes_max":131072,
            "details":"revision_id selects bounded path/hash records; omit for compact summaries",
            "ordering":"revision ID, not chronology","live_source_checked":false,"undo_preconditions_checked":false,
            "scope":"retained journaled revisions, not every project mutation"});
    }
    if crate::native_operations::supported(op) {
        result["constraints"] = json!({"qualified_platforms":["Windows"],"persistent_host_required":true,"host_routing":"CLI/MCP discover and share Studio controller; start/recover can launch it headlessly",
            "script_projects_only":false,"native_dependencies":"self-contained precompiled GDExtensions or a local Rust cdylib with pinned offline dependencies; external engine/workspace sources and custom validators remain unsupported",
            "compiler_seconds_max":120,"native_manifest_limit":16,"native_manifest_bytes_max":65536,"host_lifetime":"host exit terminates owned games",
            "execution_sandboxed":false,"native_slots":4,"native_slots_scope":"per temporary pool","validation_slots_separate":true,
            "property_values":"JSON primitives/arrays/maps or __aurum_type tags for Vector2, Vector3, Color and int64",
            "replacement":"private validated boot, complete restoration and verification before retiring old; custom hooks declare completeness"});
    }
    if matches!(op, "native_userdata_list" | "native_userdata_forget") {
        result["constraints"] = json!({"generation_limit":16,"retained_bytes_limit":crate::native_userdata::TOTAL_BYTES,
            "generation_bytes_limit":crate::native_userdata::RUN_BYTES,"automatic_pruning":false,
            "forget":"permanent removal of a selected non-current, unowned generation; source is unchanged"});
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
    fn candidate_discovery_distinguishes_publication_and_observed_storage() {
        let validate = describe(Some("changes_validate")).unwrap();
        let apply = describe(Some("changes_apply")).unwrap();
        assert_eq!(validate["constraints"]["applied"], false);
        assert!(apply["constraints"].get("applied").is_none());
        assert!(apply["constraints"]["publication"].is_string());
        for operation in [validate, apply] {
            assert_eq!(operation["constraints"]["workspace_slots"], 2);
            assert_eq!(
                operation["constraints"]["workspace_reserved_bytes"],
                1024u64 * 1024 * 1024
            );
            assert_eq!(operation["constraints"]["execution_sandboxed"], false);
        }
    }
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
