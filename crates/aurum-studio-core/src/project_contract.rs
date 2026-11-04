//! Discovery for the project operations shared by CLI, HTTP and MCP.
use crate::project_ops::{is_read_only, EXPORT_PLATFORMS, OPERATIONS};
use serde_json::{json, Map, Value};

pub const MAX_FRAMES: u64 = 3_600_000;
pub const MAX_FIXED_FPS: u64 = 240;
pub const MAX_TIMEOUT_SECONDS: u64 = 600;
pub const MAX_CAPTURE_FRAMES: usize = 32;
pub const MAX_EVENTS: usize = 1024;

fn typed_resource_reference_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["resource_path","expected_type"],"properties":{
        "resource_path":{"type":"string","maxLength":1024,"pattern":"^res://","description":"Existing confined regular project resource within 64 MiB; no links, internal state or traversal"},
        "expected_type":{"type":"string","enum":["Texture2D","Material","StandardMaterial3D","Mesh"]}
    }})
}

fn transform_tracks_schema() -> Value {
    json!({"type":"array","minItems":1,"maxItems":16,"items":{"type":"object","additionalProperties":false,"required":["target","property","keys"],"properties":{
        "target":{"type":"string","maxLength":512,"description":"Node3D path local to AnimationPlayer root; no absolute, parent traversal or subproperty path"},
        "property":{"type":"string","enum":["position","rotation","scale"]},
        "keys":{"type":"array","minItems":1,"maxItems":256,"description":"At most 256 aggregate keys across all tracks, with finite strictly ascending times within length","items":{"type":"object","additionalProperties":false,"required":["time","value"],"properties":{
            "time":{"type":"number","minimum":0,"maximum":600},"value":{"type":"array","minItems":3,"maxItems":4,"items":{"type":"number","minimum":-100000,"maximum":100000},"description":"position/scale xyz; rotation normalized xyzw quaternion arrays"}
        }}}
    }}})
}

fn local_transform_vector_schema() -> Value {
    json!({"type":"array","minItems":3,"maxItems":3,"items":{"type":"number","minimum":-100000,"maximum":100000},"description":"Exactly xyz bounded finite local components"})
}

fn local_transform_scale_schema() -> Value {
    let mut scale = local_transform_vector_schema();
    let minimum = crate::project_ops::TRANSFORM_SCALE_MIN_ADMISSION;
    let maximum = crate::project_ops::TRANSFORM_SCALE_MAX_ADMISSION;
    scale["description"] = json!("Exactly xyz finite same-sign local scale; nominal magnitude 0.000001..100000 admits a relative 2^-21 float32 round-trip boundary band; supplied values are never clamped or rounded");
    scale["items"]["minimum"] = json!(-maximum);
    scale["items"]["maximum"] = json!(maximum);
    scale["anyOf"] = json!([
        {"items":{"type":"number","minimum":minimum,"maximum":maximum}},
        {"items":{"type":"number","minimum":-maximum,"maximum":-minimum}}
    ]);
    scale
}

fn set_transform_schema() -> Value {
    let vector = local_transform_vector_schema();
    json!({"type":"object","additionalProperties":false,"required":["op","node"],"properties":{
        "op":{"const":"set_transform"},
        "node":{"type":"string","minLength":1,"maxLength":512,"description":"Local scene node path; no absolute path, parent traversal, colon or backslash"},
        "position":vector.clone(),"rotation_degrees":vector,"scale":local_transform_scale_schema(),
        "position_snap":{"type":"number","exclusiveMinimum":0,"maximum":100000,"description":"Scene-unit step for supplied local position only; exact halves away from zero; overflow or out-of-bounds results refused"}
    },"anyOf":[{"required":["position"]},{"required":["rotation_degrees"]},{"required":["scale"]}],
    "allOf":[{"if":{"required":["position_snap"]},"then":{"required":["position"]}}]})
}

fn scene_operations_schema() -> Value {
    let tracks = transform_tracks_schema();
    let vector = local_transform_vector_schema();
    json!({"type":"array","maxItems":200,"items":{"type":"object","properties":{
            "op":{"type":"string","enum":["create","instance","set","remove","reparent","attach_script","animate_transform","set_transform"]},
            "node":{"type":"string"},"parent":{"type":"string"},"name":{"type":"string"},"type":{"type":"string"},
            "script":{"type":"string"},"source":{"type":"string"},"properties":{"type":"object","maxProperties":64,"description":"Typed existing resources use {resource_path:res://...,expected_type:Texture2D|Material|StandardMaterial3D|Mesh}; loaded and property types must both match"},
            "animation":{"type":"string","minLength":1,"maxLength":64},"length":{"type":"number","exclusiveMinimum":0,"maximum":600},"loop":{"type":"boolean","default":false},
            "tracks":tracks,"position":vector.clone(),"rotation_degrees":vector,"scale":local_transform_scale_schema(),
            "position_snap":{"type":"number","exclusiveMinimum":0,"maximum":100000}
        },"required":["op"]}})
}

fn external_manifest_reference_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["root_id","path"],"properties":{
        "root_id":{"type":"string","pattern":"^[a-f0-9]{32}$"},
        "path":{"type":"string","minLength":1,"maxLength":1024,"description":"Closed relative Cargo manifest path within an existing attended read grant"}
    }})
}
fn external_gdextension_manifest_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["root_id","path"],"properties":{
        "root_id":{"type":"string","pattern":"^(project|[a-f0-9]{32})$","description":"project or one of this request's selected existing attended read grants"},
        "path":{"type":"string","minLength":1,"maxLength":1024,"pattern":r"\.gdextension$","description":"Confined relative .gdextension path in the selected staged root; requires compile_native:true. ConfigFile comparison grants no loading, ABI or source-publication authority"}
    }})
}
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
        "operations":scene_operations_schema(),
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
    result["seed"] = json!({"type":"integer","minimum":0,"maximum":4294967295u64,"description":"Optional global Godot RNG seed before the selected main scene loads; custom RNG and prior autoload work are not controlled"});
    result["metric"] = json!({"type":"string","description":"Single performance failure predicate metric"});
    result["allow_project_code"] = json!({"type":"boolean","description":"Explicit current-user replay execution, without a sandbox"});
    result["mode"] = json!({"type":"string","enum":["dependency_read"],"description":"Attended native confirmation required; ordinary project authorization cannot approve"});
    result["compile_native"] = json!({"type":"boolean","default":false,"description":"Opt-in frozen/offline compilation of the selected local-only member into an owned private target; no library load, activation or source publication"});
    result["request_id"] = json!({"type":"string","pattern":"^[a-f0-9]{32}$","description":"Pending attended request selected only for cancellation"});
    result["root_ids"] = json!({"type":"array","minItems":1,"maxItems":7,"uniqueItems":true,"items":{"type":"string","pattern":"^[a-f0-9]{32}$"},"description":"Existing session-only attended DependencyRead grants; paths do not authorize access"});
    result["placements"] = json!({"type":"array","minItems":2,"maxItems":8,"items":{"type":"object","additionalProperties":false,"required":["root_id","slot"],"properties":{
        "root_id":{"type":"string","description":"project or a selected existing read-grant ID"},"slot":{"type":"string","minLength":1,"maxLength":1024,"description":"Explicit closed relative layout; no common ancestor copy or manifest rewriting"}
    }}});
    result["workspace"] = external_manifest_reference_schema();
    result["member"] = external_manifest_reference_schema();
    result["gdextension_manifest"] = external_gdextension_manifest_schema();
    result["performance"] = crate::performance_budget::input_schema();
    result["prefix"] = json!({"type":"string","maxLength":1024,"description":"Public project-relative context scope; omit for all public source"});
    result["extensions"] = json!({"type":"array","maxItems":16,"items":{"type":"string","maxLength":33},"description":"Case-insensitive file extensions, for example gd, tscn or rs; omit for all"});
    result["baseline_id"] = json!({"type":"string","pattern":"^[a-f0-9]{64}$","description":"Retained context baseline identity"});
    result["current_id"] = json!({"type":"string","pattern":"^[a-f0-9]{64}$","description":"Content identity from the preceding delta page; required with after"});
    result["after"] = json!({"type":"string","maxLength":1024,"description":"next_after from the preceding context page"});
    result["job_id"] = json!({"type":"string","pattern":"^[a-f0-9]{32}$","description":"Background job identity from job_submit"});
    result["owner_capability"] = json!({"type":"string","pattern":"^[a-f0-9]{64}$","description":"Private owner capability returned by the first submission; required for reconnect, results and cancellation"});
    result["after_sequence"] = json!({"type":"integer","minimum":0,"description":"Last observed monotonic job progress sequence"});
    result["request"] = json!({"type":"object","required":["op"],"description":"One supported project operation; nested jobs, project scope overrides and controller shutdown are refused. Detailed schemas remain available through describe."});
    result["gradle"] = json!({"type":"boolean","description":"Inspect Gradle prerequisite metadata; omit to use the selected preset setting"});
    for key in [
        "godot_path",
        "templates_path",
        "android_sdk_path",
        "java_sdk_path",
        "keystore_path",
    ] {
        let mut schema = bounded_metadata_string();
        schema["not"] = json!({"pattern":r"(^[\\/]{2}|://)"});
        schema["description"] = json!("Optional local filesystem path, 1..4096 UTF-8 bytes without control characters; presence and metadata only");
        if key == "keystore_path" {
            schema["description"] = json!("Optional local keystore path, 1..4096 UTF-8 bytes without control characters; file presence only, contents and signing credentials are not read");
        }
        result[key] = schema;
    }
    result
}

fn bounded_metadata_string() -> Value {
    json!({"type":"string","minLength":1,"maxLength":4096,"pattern":r"^[^\u0000-\u001f\u007f-\u009f]+$(?![\s\S])"})
}

fn parameter(op: &str, key: &str, all: &Value) -> Value {
    if op == "target_preflight" && key == "preset" {
        let mut schema = bounded_metadata_string();
        schema["description"] = json!("Optional preset name, 1..4096 UTF-8 bytes without control characters; omit to select by target platform");
        schema
    } else if op == "target_preflight" && key == "debug" {
        json!({"type":"boolean","default":true,"description":"Inspect prerequisites for the selected debug or release profile"})
    } else if op == "runtime_context" && key == "resources" {
        json!({"type":"array","minItems":1,"maxItems":crate::runtime_context::MANIFESTS,"uniqueItems":true,
            "items":{"type":"string","minLength":19,"maxLength":crate::runtime_context::PATH_BYTES,
                "pattern":r"^res://(?![\s\S]*[\u0000-\u001f\u007f-\u009f\\:])(?!(?:[\s\S]*/)?\.{1,2}(?:/|$))[^/]+(?:/[^/]+)*\.gdextension$(?![\s\S])"},
            "description":"Optional exact accepted-manifest resource names. Filters returned dependency rows only, in prepared-manifest order. All accepted-session identity, source, dependency and health checks still run; private-source exclusions and roster membership are host-validated. The whole request remains limited to 1 KiB."})
    } else {
        all[key].clone()
    }
}

fn fields(op: &str) -> &'static [&'static str] {
    match op {
        "workspace_access_request" => &["mode"],
        "workspace_access_cancel" => &["request_id"],
        "external_workspace_validate" => &[
            "root_ids",
            "placements",
            "workspace",
            "member",
            "compile_native",
            "gdextension_manifest",
        ],
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
        "runtime_context" => &["resources"],
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
            "seed",
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
            "performance",
        ],
        "failure_bundle_create" => &["request", "metric", "seed"],
        "failure_bundle_inspect" => &["path"],
        "failure_bundle_replay" => &["path", "allow_project_code"],
        "configure_export" => &["platform"],
        "target_preflight" => &[
            "platform",
            "preset",
            "debug",
            "gradle",
            "godot_path",
            "templates_path",
            "android_sdk_path",
            "java_sdk_path",
            "keystore_path",
        ],
        "export" => &["preset", "output", "debug", "pack"],
        "package" => &["output"],
        "scene_inspect" | "set_main_scene" => &["scene", "expected_sha256"],
        "scene_create" => &[
            "scene",
            "root_type",
            "name",
            "operations",
            "expected_sha256",
        ],
        "scene_edit" => &["scene", "operations", "expected_sha256"],
        "classes" => &["query"],
        "class_info" => &["class"],
        _ => &[],
    }
}

fn required(op: &str) -> &'static [&'static str] {
    match op {
        "workspace_access_request" => &["mode"],
        "workspace_access_cancel" => &["request_id"],
        "external_workspace_validate" => &["root_ids", "placements", "workspace", "member"],
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
        "failure_bundle_create" => &["request", "metric", "seed"],
        "failure_bundle_inspect" => &["path"],
        "failure_bundle_replay" => &["path", "allow_project_code"],
        "draft_clear" => &["path", "draft_id"],
        "write" => &["path", "text"],
        "draft_save" => &["path", "text", "draft_id"],
        "export" => &["preset", "output"],
        "target_preflight" => &["platform"],
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
            properties.insert((*key).into(), parameter(op, key, &all));
        }
    }
    let mut schema = json!({"type":"object","properties":properties,"required":["op"]});
    let closed:Vec<_>=["target_preflight","runtime_context","workspace_access_request","workspace_access_status","workspace_access_cancel","external_workspace_validate","failure_bundle_create","failure_bundle_inspect","failure_bundle_replay"].into_iter().filter(|op|supported.contains(op)).map(|op|json!({
        "if":{"properties":{"op":{"const":op}},"required":["op"]},"then":operation_schema(op,&all)
    })).collect();
    if !closed.is_empty() {
        schema["allOf"] = json!(closed);
    }
    schema
}

fn operation_schema(op: &str, all: &Value) -> Value {
    if crate::failure_bundle::supported(op) {
        return crate::failure_bundle::input_schema(op);
    }
    let mut properties = Map::new();
    properties.insert("op".into(), json!({"type":"string","enum":[op]}));
    for key in fields(op) {
        properties.insert((*key).into(), parameter(op, key, all));
    }
    let mut required_fields = vec!["op"];
    required_fields.extend_from_slice(required(op));
    let mut schema = json!({"type":"object","properties":properties,"required":required_fields});
    if op == "external_workspace_validate" {
        schema["allOf"] = json!([{
            "if":{"required":["gdextension_manifest"]},
            "then":{"required":["compile_native"],"properties":{"compile_native":{"const":true}}}
        }]);
    }
    if matches!(op, "scene_create" | "scene_edit") {
        let tracks = schema["properties"]["operations"]["items"]["properties"]["tracks"].clone();
        schema["properties"]["operations"]["items"]["allOf"] = json!([{
            "if":{"properties":{"op":{"const":"animate_transform"}},"required":["op"]},
            "then":{"additionalProperties":false,"required":["op","node","animation","length","tracks"],"properties":{
                "op":{"const":"animate_transform"},"node":{"type":"string","maxLength":512},
                "animation":{"type":"string","minLength":1,"maxLength":64},"length":{"type":"number","exclusiveMinimum":0,"maximum":600},
                "loop":{"type":"boolean"},"tracks":tracks
            }}
        },{
            "if":{"properties":{"op":{"const":"set_transform"}},"required":["op"]},
            "then":set_transform_schema()
        }]);
        let resource_reference = typed_resource_reference_schema();
        schema["properties"]["operations"]["items"]["properties"]["properties"]
            ["additionalProperties"] = json!({"allOf":[{
            "if":{"type":"object","anyOf":[{"required":["resource_path"]},{"required":["expected_type"]}]},
            "then":resource_reference
        }]});
    }
    if matches!(
        op,
        "target_preflight"
            | "runtime_context"
            | "workspace_access_request"
            | "workspace_access_status"
            | "workspace_access_cancel"
            | "external_workspace_validate"
    ) {
        schema["additionalProperties"] = json!(false);
    }
    schema
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
    let mut result = json!({"contract_version":1,"op":op,"read_only":is_read_only(op),"input_schema":operation_schema(op, &all)});
    if matches!(op, "scene_create" | "scene_edit" | "scene_inspect") {
        result["constraints"] = crate::project_ops::scene_authoring_schema();
        result["inspection"] = json!({"materials":"Effective mesh surface and global override material class/path/PBR scalars and texture class/path/dimensions","animations":"Bounded local library/clip/transform track/time/key values; unsupported track bodies omitted","render_verified":false});
    }
    if op == "target_preflight" {
        result["description"] = json!("Inspect bounded local metadata for export prerequisites. Available paths do not verify export, signing or physical device readiness.");
        result["constraints"] = json!({
            "metadata_only":true,"tools_invoked":false,"network_used":false,"device_query_performed":false,
            "installed":false,"keystore_contents_read":false,"metadata_bytes_max":65536,"preset_bytes_max":262144,
            "directory_entries_max":64,"deadline_seconds_max":2,
            "paths":"local filesystem paths only; 1..4096 UTF-8 bytes without control characters",
            "runtime_policy":"version-pinned Android requirements apply only to observed stable Godot 4.7; other versions remain unverified",
            "export_readiness":"unverified","signing_verified":false,"physical_readiness":"unverified",
            "freshness":"point-in-time metadata; no atomic filesystem snapshot or executable validation"
        });
    }
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
    if op == "runtime_context" {
        result["description"]=json!("Inspect an already accepted owned game's identity, loaded resource flags and bounded source/disk hashes without starting or mutating a game. Optional resources selects returned dependency rows; verification still covers the whole accepted session.");
        result["constraints"] = json!({"qualified_platforms":["Windows"],"host_launch":false,"build":false,"mutation":false,
            "deadline_seconds_max":10,"report_bytes_max":crate::runtime_context::REPORT_BYTES,"request_bytes_max":1024,
            "source_bytes_max":crate::runtime_context::SOURCE_BYTES,"source_files_max":crate::runtime_context::SOURCE_FILES,
            "source_entries_max":crate::runtime_context::SOURCE_ENTRIES,"relative_path_bytes_max":crate::runtime_context::PATH_BYTES,"source_depth_max":24,
            "native_manifest_limit":crate::runtime_context::MANIFESTS,"selected_dependencies_limit":crate::runtime_context::DEPENDENCIES,
            "resource_filter_max":crate::runtime_context::MANIFESTS,"resource_filter_scope":"returned_rows_only",
            "verification_scope":"whole_accepted_session","unknown_resource_filter":"refused against retained prepared roster before game queries",
            "resource_filter_IO_or_token_savings_claimed":false,
            "game_disk_hashing":false,"game_manifest_parsing":false,"source_contents_returned":false,"recognized_key_files_read":false,
            "source_privacy":"ordinary source is read for hashing; embedded material is not returned",
            "mapped_memory_hash":"unverified","atomic_filesystem_snapshot":"unverified","model_tokens_measured":false,"model_latency_measured":false,
            "states":["unavailable","busy","available","stale","drift","unhealthy","incomplete"],"runtime_version":"accepted game response; separate headless runtime_info is not identity proof",
            "provenance":"captured preparation bindings and fresh bounded host disk hashes; full transitive Cargo graph is not retained"});
    }
    if matches!(op, "native_userdata_list" | "native_userdata_forget") {
        result["constraints"] = json!({"generation_limit":16,"retained_bytes_limit":crate::native_userdata::TOTAL_BYTES,
            "generation_bytes_limit":crate::native_userdata::RUN_BYTES,"automatic_pruning":false,
            "forget":"permanent removal of a selected non-current, unowned generation; source is unchanged"});
    }
    if matches!(op, "play" | "capture") {
        result["performance"] = crate::performance_budget::input_schema();
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
    #[test]
    fn configfile_parser_manifest_has_a_distinct_closed_selector() {
        let described = describe(Some("external_workspace_validate")).unwrap();
        let schema = &described["input_schema"];
        let selection = &schema["properties"]["gdextension_manifest"];
        assert_eq!(selection["type"], "object");
        assert_eq!(selection["additionalProperties"], false);
        assert_eq!(selection["required"], json!(["root_id", "path"]));
        assert_eq!(selection["properties"].as_object().unwrap().len(), 2);
        assert_eq!(
            selection["properties"]["root_id"]["pattern"],
            "^(project|[a-f0-9]{32})$"
        );
        assert_eq!(selection["properties"]["path"]["type"], "string");
        assert_eq!(selection["properties"]["path"]["minLength"], 1);
        assert_eq!(selection["properties"]["path"]["maxLength"], 1024);
        assert_eq!(selection["properties"]["path"]["pattern"], r"\.gdextension$");
        for cargo_manifest in ["workspace", "member"] {
            assert_eq!(
                schema["properties"][cargo_manifest]["properties"]["root_id"]["pattern"],
                "^[a-f0-9]{32}$"
            );
        }
    }

    #[test]
    fn configfile_parser_compile_requirement_is_shared_by_action_discovery() {
        let described = describe(Some("external_workspace_validate")).unwrap();
        let schema = &described["input_schema"];
        assert_eq!(
            schema["required"],
            json!(["op", "root_ids", "placements", "workspace", "member"])
        );
        assert_eq!(schema["properties"]["compile_native"]["default"], false);
        assert_eq!(
            schema["allOf"],
            json!([{
                "if":{"required":["gdextension_manifest"]},
                "then":{"required":["compile_native"],"properties":{"compile_native":{"const":true}}}
            }])
        );
        let action = input_schema(false);
        let embedded = action["allOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["if"]["properties"]["op"]["const"] == "external_workspace_validate")
            .unwrap();
        assert_eq!(&embedded["then"], schema);
        let query = input_schema(true);
        assert!(!query["properties"]["op"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("external_workspace_validate")));
        assert!(query["properties"].get("gdextension_manifest").is_none());
        assert_eq!(described["read_only"], false);
        assert_eq!(
            describe(Some("workspace_access_status")).unwrap()["read_only"],
            true
        );
    }

    #[test]
    fn configfile_parser_read_only_barrier_precedes_selector_preflight() {
        let root = std::env::temp_dir().join(format!(
            "aurum-configfile-query-no-io-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(!root.exists());
        // Deliberately incomplete requests keep this refusal test inert even if
        // the barrier regresses: preflight cannot admit a worker or controller.
        for selection in [
            json!({"root_id":"project","path":"game.gdextension"}),
            json!({"root_id":"project","path":"game.gdextension","approved":true}),
        ] {
            let request = json!({"op":"external_workspace_validate","compile_native":true,
                "root_ids":[],"gdextension_manifest":selection});
            assert_eq!(
                crate::project_ops::execute(&root, &request, true).unwrap_err(),
                "'external_workspace_validate' requires write permission"
            );
        }
        assert!(!root.exists());
    }

    #[test]
    fn external_compile_opt_in_is_closed_optional_and_default_staging_only() {
        let described = describe(Some("external_workspace_validate")).unwrap();
        let schema = &described["input_schema"];
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["compile_native"]["type"], "boolean");
        assert_eq!(schema["properties"]["compile_native"]["default"], false);
        assert!(!schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("compile_native")));
        let action = input_schema(false);
        let embedded = action["allOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["if"]["properties"]["op"]["const"] == "external_workspace_validate")
            .unwrap();
        assert_eq!(&embedded["then"], schema);
        assert!(!is_read_only("external_workspace_validate"));
        assert!(schema["properties"].get("source_publish").is_none());
    }
    #[test]
    fn performance_contract_preserves_full_frame_refusal_with_scoped_viewport_timers() {
        for op in ["play", "capture"] {
            let details = describe(Some(op)).unwrap();
            let performance = &details["performance"];
            let metrics = &performance["properties"]["budgets"]["properties"];
            assert!(metrics["gpu_viewport_ms"]["description"]
                .as_str()
                .unwrap()
                .contains("Main viewport"));
            assert!(metrics["viewport_cpu_render_ms"]["description"]
                .as_str()
                .unwrap()
                .contains("excludes scripts"));
            assert!(metrics["gpu_frame_ms"]["description"]
                .as_str()
                .unwrap()
                .contains("not instrumented"));
            assert_eq!(
                performance["viewport_timing_semantics"]["scope"],
                "main_viewport_only"
            );
            assert_eq!(
                performance["viewport_timing_semantics"]["total_scene_gpu_time_verified"],
                false
            );
        }
    }
    #[test]
    fn scene_authoring_schema_embeds_closed_resource_and_transform_values() {
        fn references(value: &Value) -> bool {
            match value {
                Value::Object(fields) => {
                    fields.contains_key("$ref") || fields.values().any(references)
                }
                Value::Array(values) => values.iter().any(references),
                _ => false,
            }
        }
        for op in ["scene_create", "scene_edit"] {
            let detail = describe(Some(op)).unwrap();
            let items = &detail["input_schema"]["properties"]["operations"]["items"];
            assert!(items["properties"]["op"]["enum"]
                .as_array()
                .unwrap()
                .contains(&json!("animate_transform")));
            let conditional =
                &items["properties"]["properties"]["additionalProperties"]["allOf"][0]["then"];
            assert_eq!(conditional["additionalProperties"], false);
            assert_eq!(
                conditional["properties"]["resource_path"]["maxLength"],
                1024
            );
            assert_eq!(
                conditional["properties"]["expected_type"]["enum"],
                json!(["Texture2D", "Material", "StandardMaterial3D", "Mesh"])
            );
            assert_eq!(items["allOf"][0]["then"]["additionalProperties"], false);
            assert_eq!(
                items["allOf"][0]["then"]["properties"]["tracks"]["maxItems"],
                16
            );
            let wrapped =
                json!({"type":"object","properties":{"request":detail["input_schema"].clone()}});
            assert!(
                !references(&wrapped),
                "Inline constraints cannot contain root-relative references broken by a wrapper"
            );
        }
    }
    #[test]
    fn set_transform_schema_requires_sparse_closed_vectors_and_position_for_snap() {
        for operation in ["scene_create", "scene_edit"] {
            let detail = describe(Some(operation)).unwrap();
            let items = &detail["input_schema"]["properties"]["operations"]["items"];
            assert!(items["properties"]["op"]["enum"]
                .as_array()
                .unwrap()
                .contains(&json!("set_transform")));
            let transform = &items["allOf"][1]["then"];
            assert_eq!(transform["additionalProperties"], false);
            assert_eq!(transform["required"], json!(["op", "node"]));
            assert_eq!(
                transform["anyOf"],
                json!([
                    {"required":["position"]},
                    {"required":["rotation_degrees"]},
                    {"required":["scale"]}
                ])
            );
            assert_eq!(
                transform["allOf"][0]["then"]["required"],
                json!(["position"])
            );
            for field in ["position", "rotation_degrees"] {
                assert_eq!(transform["properties"][field]["minItems"], 3);
                assert_eq!(transform["properties"][field]["maxItems"], 3);
                assert_eq!(transform["properties"][field]["items"]["minimum"], -100000);
                assert_eq!(transform["properties"][field]["items"]["maximum"], 100000);
            }
            let scale = &transform["properties"]["scale"];
            assert_eq!(scale["minItems"], 3);
            assert_eq!(scale["maxItems"], 3);
            let minimum = crate::project_ops::TRANSFORM_SCALE_MIN_ADMISSION;
            let maximum = crate::project_ops::TRANSFORM_SCALE_MAX_ADMISSION;
            assert_eq!(scale["items"]["minimum"], json!(-maximum));
            assert_eq!(scale["items"]["maximum"], json!(maximum));
            assert_eq!(scale["anyOf"][0]["items"]["minimum"], json!(minimum));
            assert_eq!(scale["anyOf"][0]["items"]["maximum"], json!(maximum));
            assert_eq!(scale["anyOf"][1]["items"]["minimum"], json!(-maximum));
            assert_eq!(scale["anyOf"][1]["items"]["maximum"], json!(-minimum));
            assert_eq!(
                detail["constraints"]["set_transform"]["scale_admission_abs_min"],
                json!(minimum)
            );
            assert_eq!(
                detail["constraints"]["set_transform"]["scale_admission_abs_max"],
                json!(maximum)
            );
            assert_eq!(
                detail["constraints"]["set_transform"]["scale_boundary_relative_tolerance"],
                json!(4.0 * f32::EPSILON as f64)
            );
            assert_eq!(
                detail["constraints"]["set_transform"]["scale_values_adjusted"],
                false
            );
            assert_eq!(
                detail["constraints"]["set_transform"]["scale_abs_min"],
                json!(0.000001)
            );
            assert_eq!(
                transform["properties"]["position_snap"]["exclusiveMinimum"],
                0
            );
            assert_eq!(
                detail["constraints"]["set_transform"]["unspecified_fields"],
                "preserved"
            );
        }
    }

    use super::*;
    #[test]
    fn runtime_context_has_closed_read_only_schema_and_explicit_evidence_boundary() {
        let details = describe(Some("runtime_context")).unwrap();
        assert_eq!(details["read_only"], true);
        assert_eq!(details["input_schema"]["additionalProperties"], false);
        assert_eq!(
            details["input_schema"]["properties"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(details["constraints"]["deadline_seconds_max"], 10);
        assert_eq!(details["constraints"]["report_bytes_max"], 65536);
        assert_eq!(details["constraints"]["game_disk_hashing"], false);
        assert_eq!(details["constraints"]["game_manifest_parsing"], false);
        assert_eq!(details["constraints"]["mapped_memory_hash"], "unverified");
        assert!(input_schema(true)["properties"]["op"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("runtime_context")));
        assert!(!input_schema(false)["properties"]["op"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("runtime_context")));
        assert!(input_schema(true)["allOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |row| row["if"]["properties"]["op"]["const"] == "runtime_context"
                    && row["then"]["additionalProperties"] == false
            ));
    }
    #[test]
    fn runtime_context_selection_schema_is_optional_bounded_and_query_only() {
        let details = describe(Some("runtime_context")).unwrap();
        let schema = &details["input_schema"];
        let resources = &schema["properties"]["resources"];
        assert_eq!(schema["required"], json!(["op"]));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(resources["type"], "array");
        assert_eq!(resources["minItems"], 1);
        assert_eq!(resources["maxItems"], crate::runtime_context::MANIFESTS);
        assert_eq!(resources["uniqueItems"], true);
        assert_eq!(resources["items"]["type"], "string");
        assert_eq!(resources["items"]["maxLength"], crate::runtime_context::PATH_BYTES);
        assert!(resources["items"]["pattern"].as_str().unwrap().starts_with("^res://"));
        assert_eq!(input_schema(true)["properties"]["resources"], *resources);
        assert!(input_schema(false)["properties"].get("resources").is_none());
        assert_eq!(details["constraints"]["resource_filter_scope"], "returned_rows_only");
        assert_eq!(details["constraints"]["verification_scope"], "whole_accepted_session");
        assert_eq!(details["constraints"]["resource_filter_IO_or_token_savings_claimed"], false);
    }
    #[test]
    fn runtime_context_selection_schema_and_host_agree_on_array_and_closed_fields() {
        let query = input_schema(true);
        let branch = query["allOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["if"]["properties"]["op"]["const"] == "runtime_context")
            .unwrap();
        assert_eq!(branch["then"], describe(Some("runtime_context")).unwrap()["input_schema"]);
        let valid = json!({"op":"runtime_context","resources":["res://addons/a.gdextension"]});
        assert_eq!(crate::runtime_context::operation_request(&valid).unwrap(), valid);
        for resources in [Value::Null, json!([]), json!("res://a.gdextension"), json!([true]),
            json!(["res://a.gdextension","res://a.gdextension"])] {
            assert!(crate::runtime_context::operation_request(
                &json!({"op":"runtime_context","resources":resources})
            ).is_err());
        }
        assert!(crate::runtime_context::operation_request(&json!({
            "op":"runtime_context","resources":["res://a.gdextension"],"unknown":true
        })).is_err());
    }
    #[test]
    fn target_preflight_is_read_only_with_curated_bounded_metadata_paths() {
        let details = describe(Some("target_preflight")).unwrap();
        assert_eq!(details["read_only"], true);
        assert_eq!(
            details["input_schema"]["required"],
            json!(["op", "platform"])
        );
        let query = input_schema(true);
        let action = input_schema(false);
        assert!(query["properties"]["op"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("target_preflight")));
        assert!(!action["properties"]["op"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("target_preflight")));
        for key in [
            "godot_path",
            "templates_path",
            "android_sdk_path",
            "java_sdk_path",
            "keystore_path",
        ] {
            let schema = &details["input_schema"]["properties"][key];
            assert_eq!(schema["type"], "string", "{key}");
            assert_eq!(schema["minLength"], 1, "{key}");
            assert_eq!(schema["maxLength"], 4096, "{key}");
            assert_eq!(
                schema["pattern"],
                bounded_metadata_string()["pattern"],
                "{key}"
            );
            assert!(schema["not"]["pattern"].as_str().unwrap().contains("://"));
            assert_eq!(query["properties"][key], *schema);
            assert!(action["properties"].get(key).is_none());
        }
        let preset = &details["input_schema"]["properties"]["preset"];
        assert_eq!(preset["minLength"], 1);
        assert_eq!(preset["maxLength"], 4096);
        assert_eq!(
            details["input_schema"]["properties"]["debug"]["default"],
            true
        );
    }
    #[test]
    fn compact_target_preflight_schema_preserves_operation_specific_refusals() {
        let details = describe(Some("target_preflight")).unwrap();
        let strict = &details["input_schema"];
        assert_eq!(strict["additionalProperties"], false);
        assert_eq!(strict["properties"].as_object().unwrap().len(), 10);
        for forbidden in [
            "password",
            "key_alias",
            "credentials",
            "probe",
            "install",
            "device",
            "devices",
            "limit",
            "path",
            "request",
        ] {
            assert!(strict["properties"].get(forbidden).is_none(), "{forbidden}");
        }
        let compact = input_schema(true);
        assert_eq!(
            compact["allOf"][0]["if"]["properties"]["op"]["const"],
            "target_preflight"
        );
        assert_eq!(compact["allOf"][0]["then"], *strict);
    }
    #[test]
    fn target_preflight_discovery_separates_metadata_from_export_and_device_proof() {
        let details = describe(Some("target_preflight")).unwrap();
        let constraints = &details["constraints"];
        assert_eq!(constraints["metadata_only"], true);
        for key in [
            "tools_invoked",
            "network_used",
            "device_query_performed",
            "installed",
            "keystore_contents_read",
            "signing_verified",
        ] {
            assert_eq!(constraints[key], false, "{key}");
        }
        assert_eq!(constraints["metadata_bytes_max"], 64 * 1024);
        assert_eq!(constraints["preset_bytes_max"], 256 * 1024);
        assert_eq!(constraints["directory_entries_max"], 64);
        assert_eq!(constraints["deadline_seconds_max"], 2);
        assert_eq!(constraints["export_readiness"], "unverified");
        assert_eq!(constraints["physical_readiness"], "unverified");
        assert!(constraints["runtime_policy"]
            .as_str()
            .unwrap()
            .contains("observed stable Godot 4.7"));
    }
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

#[cfg(test)]
mod attended_access_schema_tests {
    use super::*;
    #[test]
    fn attended_access_discovery_has_no_transport_approval_fields() {
        for op in crate::workspace_access::OPERATIONS {
            let result = describe(Some(op)).unwrap();
            let schema = operation_schema(op, &parameters());
            assert_eq!(schema["additionalProperties"], false);
            let text = schema.to_string();
            for key in ["nonce", "approved", "directory_id", "owner_capability"] {
                assert!(!text.contains(key));
            }
            assert!(result.to_string().contains(op));
        }
        assert_eq!(parameters()["mode"]["enum"], json!(["dependency_read"]));
        assert!(crate::project_ops::is_read_only("workspace_access_status"));
        assert!(!crate::project_ops::is_read_only(
            "external_workspace_validate"
        ));
        let schema = input_schema(false).to_string();
        assert!(!schema.contains("$ref"));
        assert!(schema.contains("external_workspace_validate"));
    }
}
