//! MCP tools for driving the live Godot editor.
//!
//! These speak to the `AurumEditor` GDExtension through the file bridge the
//! editor plugin polls. There is deliberately **one** operation tool rather
//! than one tool per primitive: the bridge's op names *are* the stable native
//! surface, and mirroring them individually here would duplicate a contract
//! that is already explicit on the other side.
//!
//! That also keeps the hot-reload property intact. Tool composition lives in
//! this process, which is plain Rust and rebuilds freely; nothing here changes
//! the Godot-facing surface, so adding or changing a tool never costs an
//! editor restart.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::tools::{ok, schema, Args, Tool, ToolContext, ToolError, ToolResult};

/// Operations the editor bridge understands, mirroring the native surface.
///
/// Kept in sync with `AurumEditor::handle_request` in `crates/aurum-editor`.
/// A mismatch is caught by the editor gate, which exercises every entry.
const OPERATIONS: &[(&str, &str)] = &[
    ("describe_scene", "The edited scene as a JSON tree."),
    ("node_count", "How many nodes the edited scene holds."),
    (
        "create_node",
        "parent, type, name — create a node under a path.",
    ),
    (
        "set_property",
        "node, property, value — set a property from JSON.",
    ),
    ("attach_script", "node, script — attach a res:// script."),
    ("remove_node", "node — remove a node and its subtree."),
    ("reparent_node", "node, parent — move a node."),
    ("save_scene", "Save the scene open in the editor."),
    ("undo", "Undo the previous scene edit."),
    ("redo", "Redo the previous scene edit."),
];

/// How long to wait for the editor to answer before giving up.
///
/// The plugin pumps once per frame, so a healthy editor answers in tens of
/// milliseconds. The budget is generous because a stalled editor is far more
/// likely than a slow one, and a clear timeout beats a hang.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

fn bridge_root<'a>(context: &'a ToolContext<'_>) -> Result<&'a Path, ToolError> {
    context.editor_bridge.ok_or_else(|| {
        ToolError::Invalid(
            "no editor bridge is configured; start the server with --editor-bridge <dir> \
             (the plugin prints the path it is using)"
                .into(),
        )
    })
}

/// A request id that is unique per process and monotonic, so ordering is
/// preserved and two calls in the same millisecond cannot collide.
fn next_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("req-{n:08}-{}", std::process::id())
}

fn tool_editor_status(context: &mut ToolContext<'_>, _params: &Value) -> ToolResult {
    let operations: Vec<Value> = OPERATIONS
        .iter()
        .map(|(name, description)| json!({ "op": name, "description": description }))
        .collect();

    let Some(root) = context.editor_bridge else {
        return ok(json!({
            "configured": false,
            "operations": operations,
            "hint": "start the server with --editor-bridge <dir>; the Godot plugin prints it",
        }));
    };

    let requests = root.join("requests");
    let responses = root.join("responses");
    let heartbeat: Value = std::fs::read(root.join("status.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default();
    let connected = heartbeat["updated_unix"]
        .as_f64()
        .is_some_and(|last| now >= last && now - last < 5.0);
    ok(json!({
        "configured": true,
        "connected": connected,
        "heartbeat": heartbeat,
        "bridge": root.display().to_string(),
        "requests_dir": requests.display().to_string(),
        "responses_dir": responses.display().to_string(),
        "requests_dir_exists": requests.is_dir(),
        "responses_dir_exists": responses.is_dir(),
        "operations": operations,
    }))
}

fn tool_editor_op(context: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let op = args.str("op")?.to_string();
    if !OPERATIONS.iter().any(|(name, _)| *name == op) {
        let known: Vec<&str> = OPERATIONS.iter().map(|(name, _)| *name).collect();
        return Err(ToolError::Invalid(format!(
            "unknown op '{op}'; expected one of {}",
            known.join(", ")
        )));
    }

    let root = bridge_root(context)?.to_path_buf();
    let requests = root.join("requests");
    let responses = root.join("responses");
    std::fs::create_dir_all(&requests)
        .map_err(|e| ToolError::Io(format!("could not create '{}': {e}", requests.display())))?;

    // Only the declared arguments are forwarded, so a stray key cannot change
    // what the editor sees.
    let mut request = serde_json::Map::new();
    request.insert("op".into(), Value::String(op.clone()));
    for key in [
        "parent", "type", "name", "node", "property", "value", "script",
    ] {
        if let Some(value) = args.get(key) {
            let encoded = match value {
                // The editor parses these as strings, so an object is passed
                // through as its JSON text rather than re-encoded.
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            request.insert(key.into(), Value::String(encoded));
        }
    }

    let id = next_request_id();
    let request_path = requests.join(format!("{id}.json"));
    let response_path = responses.join(format!("{id}.json"));
    let staged = request_path.with_extension("json.tmp");
    std::fs::write(
        &staged,
        serde_json::to_string(&Value::Object(request)).map_err(|e| ToolError::Io(e.to_string()))?,
    )
    .map_err(|e| ToolError::Io(format!("could not write '{}': {e}", request_path.display())))?;
    std::fs::rename(&staged, &request_path).map_err(|e| ToolError::Io(e.to_string()))?;

    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        if let Ok(text) = std::fs::read_to_string(&response_path) {
            let parsed: Value = serde_json::from_str(&text)
                .map_err(|e| ToolError::Engine(format!("the editor returned invalid JSON: {e}")))?;
            let _ = std::fs::remove_file(&response_path);
            if parsed.get("ok").and_then(Value::as_bool) == Some(false) {
                return Err(ToolError::Engine(
                    parsed
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("editor operation failed")
                        .to_string(),
                ));
            }
            return ok(parsed);
        }
        if Instant::now() >= deadline {
            let outcome = if std::fs::remove_file(&request_path).is_ok() {
                "the pending request was cancelled before the editor claimed it"
            } else {
                "the outcome is unknown; inspect the scene before retrying"
            };
            return Err(ToolError::Engine(format!(
                "the editor did not answer '{op}' within {}s; check that the Aurum Editor \
                 plugin is enabled and the editor window is responsive; {outcome}",
                RESPONSE_TIMEOUT.as_secs()
            )));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The editor tool catalog.
pub fn catalog() -> Vec<Tool> {
    vec![
        Tool {
            name: "aurum_editor_status",
            description: "Report whether the live editor bridge is configured and reachable, \
                          and list the operations it accepts. Call this before driving the \
                          editor, and to discover what is available.",
            input_schema: schema(json!({}), json!([])),
            read_only: true,
            handler: tool_editor_status,
        },
        Tool {
            name: "aurum_editor_op",
            description: "Run one operation against the scene open in the Godot editor. \
                          Operations: describe_scene, node_count, create_node (parent, type, \
                          name), set_property (node, property, value), attach_script (node, \
                          script), remove_node (node), reparent_node (node, parent). Node paths \
                          are relative to the edited scene root, where \".\" means the root \
                          itself. Requires the editor to be running with the Aurum Editor \
                          plugin enabled.",
            input_schema: schema(
                json!({
                    "op": {
                        "type": "string",
                        "enum": ["describe_scene", "node_count", "create_node", "set_property", "attach_script", "remove_node", "reparent_node", "save_scene", "undo", "redo"],
                        "description": "Which operation to run."
                    },
                    "parent": { "type": "string", "description": "Parent node path, for create_node and reparent_node." },
                    "type": { "type": "string", "description": "Godot class to create, for create_node." },
                    "name": { "type": "string", "description": "Name for the created node." },
                    "node": { "type": "string", "description": "Target node path." },
                    "property": { "type": "string", "description": "Property name, for set_property." },
                    "value": { "description": "Property value as JSON, for set_property." },
                    "script": { "type": "string", "description": "res:// script path, for attach_script." }
                }),
                json!(["op"]),
            ),
            read_only: false,
            handler: tool_editor_op,
        },
    ]
}

/// Where the bridge lives, resolved from a `--editor-bridge` argument.
pub fn resolve_bridge(argument: &str) -> PathBuf {
    let path = PathBuf::from(argument);
    // The plugin's directory is the parent of requests/ and responses/, and
    // callers point at either, so normalize to the parent.
    if path
        .file_name()
        .is_some_and(|name| name == "requests" || name == "responses")
    {
        return path.parent().map(Path::to_path_buf).unwrap_or(path);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::tools::PathGuard;

    fn temp_root(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aurum-editor-tool-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Calls the tool with an explicit bridge, emulating the editor by
    /// answering the request file the tool writes.
    fn call_with_bridge(
        engine: &mut Engine,
        paths: &PathGuard,
        bridge: Option<&Path>,
        name: &str,
        args: Value,
    ) -> ToolResult {
        let tool = crate::tools::find(name).expect("tool exists");
        // Drive the exchange from a helper thread that plays the editor's part.
        let bridge_owned = bridge.map(Path::to_path_buf);
        let responder = bridge_owned.clone().map(|root| {
            std::thread::spawn(move || {
                let requests = root.join("requests");
                let responses = root.join("responses");
                std::fs::create_dir_all(&responses).ok();
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline {
                    if let Ok(entries) = std::fs::read_dir(&requests) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.extension().is_some_and(|e| e == "json") {
                                let text = std::fs::read_to_string(&path).unwrap_or_default();
                                let request: Value =
                                    serde_json::from_str(&text).unwrap_or(Value::Null);
                                let reply = json!({
                                    "ok": true,
                                    "echoed_op": request.get("op").cloned().unwrap_or(Value::Null),
                                    "echoed_value": request.get("value").cloned().unwrap_or(Value::Null),
                                });
                                let stem = path.file_stem().unwrap().to_string_lossy().to_string();
                                std::fs::write(
                                    responses.join(format!("{stem}.json")),
                                    reply.to_string(),
                                )
                                .ok();
                                std::fs::remove_file(&path).ok();
                                return;
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        });

        let mut context = ToolContext {
            engine,
            paths,
            editor_bridge: bridge,
            read_only: false,
            denied: &[],
        };
        let result = (tool.handler)(&mut context, &args);
        if let Some(handle) = responder {
            let _ = handle.join();
        }
        result
    }

    #[test]
    fn status_reports_a_missing_bridge_without_failing() {
        let root = temp_root("status-none");
        let paths = PathGuard::new(&root);
        let mut engine = Engine::new();
        let out =
            call_with_bridge(&mut engine, &paths, None, "aurum_editor_status", json!({})).unwrap();
        assert_eq!(out["configured"], false);
        // The operations are still listed, so a client can see what exists.
        assert!(out["operations"].as_array().unwrap().len() >= 7);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn status_reports_a_configured_bridge() {
        let root = temp_root("status-set");
        let paths = PathGuard::new(&root);
        let mut engine = Engine::new();
        let bridge = temp_root("bridge-status");
        std::fs::create_dir_all(bridge.join("requests")).unwrap();

        let out = call_with_bridge(
            &mut engine,
            &paths,
            Some(&bridge),
            "aurum_editor_status",
            json!({}),
        )
        .unwrap();
        assert_eq!(out["configured"], true);
        assert_eq!(out["requests_dir_exists"], true);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&bridge);
    }

    #[test]
    fn op_round_trips_through_the_bridge() {
        let root = temp_root("op");
        let paths = PathGuard::new(&root);
        let mut engine = Engine::new();
        let bridge = temp_root("bridge-op");

        let out = call_with_bridge(
            &mut engine,
            &paths,
            Some(&bridge),
            "aurum_editor_op",
            json!({"op": "create_node", "parent": ".", "type": "Node3D", "name": "Hero"}),
        )
        .unwrap();

        assert_eq!(out["ok"], true);
        assert_eq!(out["echoed_op"], "create_node");
        assert_eq!(out["echoed_value"], Value::Null);

        // The request must be cleaned up once answered.
        let leftover = std::fs::read_dir(bridge.join("requests"))
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(leftover, 0, "the request file was not consumed");

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&bridge);
    }

    #[test]
    fn op_forwards_only_declared_arguments() {
        let root = temp_root("op-args");
        let paths = PathGuard::new(&root);
        let mut engine = Engine::new();
        let bridge = temp_root("bridge-args");

        // A value is forwarded as its JSON text, since the editor parses it.
        let out = call_with_bridge(
            &mut engine,
            &paths,
            Some(&bridge),
            "aurum_editor_op",
            json!({"op": "set_property", "node": "Hero", "property": "position",
                   "value": {"x": 1, "y": 2, "z": 3}}),
        )
        .unwrap();
        let echoed = out["echoed_value"].as_str().unwrap();
        assert!(
            echoed.contains("\"x\""),
            "value was not forwarded: {echoed}"
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&bridge);
    }

    #[test]
    fn unknown_operations_are_refused_before_touching_the_bridge() {
        let root = temp_root("op-bad");
        let paths = PathGuard::new(&root);
        let mut engine = Engine::new();
        let bridge = temp_root("bridge-bad");

        let err = call_with_bridge(
            &mut engine,
            &paths,
            Some(&bridge),
            "aurum_editor_op",
            json!({"op": "teleport"}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown op"), "got: {err}");

        // Refused locally, so nothing was written for the editor to pick up.
        let pending = std::fs::read_dir(bridge.join("requests"))
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(pending, 0, "a refused op must not reach the editor");

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&bridge);
    }

    #[test]
    fn op_without_a_bridge_says_how_to_configure_one() {
        let root = temp_root("op-nobridge");
        let paths = PathGuard::new(&root);
        let mut engine = Engine::new();
        let err = call_with_bridge(
            &mut engine,
            &paths,
            None,
            "aurum_editor_op",
            json!({"op": "node_count"}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("--editor-bridge"), "got: {err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bridge_paths_normalize_from_either_subdirectory() {
        let base = PathBuf::from("/tmp/aurum_editor");
        assert_eq!(resolve_bridge("/tmp/aurum_editor"), base);
        assert_eq!(resolve_bridge("/tmp/aurum_editor/requests"), base);
        assert_eq!(resolve_bridge("/tmp/aurum_editor/responses"), base);
    }

    #[test]
    fn operations_match_the_editor_surface() {
        // The gate exercises every entry on the other side; this pins the
        // list so the two cannot drift silently.
        let names: Vec<&str> = OPERATIONS.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            vec![
                "describe_scene",
                "node_count",
                "create_node",
                "set_property",
                "attach_script",
                "remove_node",
                "reparent_node",
                "save_scene",
                "undo",
                "redo"
            ]
        );
    }
}
