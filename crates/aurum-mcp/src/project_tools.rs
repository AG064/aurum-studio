use crate::tools::{Tool, ToolContext, ToolError, ToolResult};
use serde_json::{json, Value};

fn query(context: &mut ToolContext<'_>, input: &Value) -> ToolResult {
    finish(aurum_studio_core::project_ops::execute(
        context.paths.root(),
        input,
        true,
    ))
}
fn action(context: &mut ToolContext<'_>, input: &Value) -> ToolResult {
    finish(aurum_studio_core::project_ops::execute(
        context.paths.root(),
        input,
        context.read_only,
    ))
}

fn finish(result: Result<Value, String>) -> ToolResult {
    let value = result.map_err(ToolError::Engine)?;
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(ToolError::Engine(value.to_string()));
    }
    Ok(value)
}

pub fn catalog() -> Vec<Tool> {
    let common = json!({
        "type":"object",
        "properties":{
            "op":{"type":"string"},
            "path":{"type":"string","description":"Project-relative file path"},
            "scene":{"type":"string","description":"Scene path relative to the Godot project, ending in .tscn"},
            "text":{"type":"string"},
            "expected_sha256":{"type":"string","description":"Hash from the preceding read; stale writes are refused"},
            "draft_id":{"type":"string"},"base_sha256":{"type":"string"},
            "root_type":{"type":"string"},"name":{"type":"string"},
            "class":{"type":"string"},"query":{"type":"string"},
            "operations":{"type":"array","maxItems":200,"items":{"type":"object","properties":{
                "op":{"type":"string","enum":["create","instance","set","remove","reparent","attach_script"]},
                "node":{"type":"string"},"parent":{"type":"string"},"name":{"type":"string"},"type":{"type":"string"},
                "script":{"type":"string"},"source":{"type":"string"},"properties":{"type":"object"}
            },"required":["op"]}},
            "frames":{"type":"integer","minimum":1,"maximum":36000},
            "fixed_fps":{"type":"integer","minimum":1,"maximum":240,"description":"Deterministic simulation rate for headless play"},
            "user_args":{"type":"array","maxItems":32,"items":{"type":"string","maxLength":4096},"description":"Game arguments, passed after the engine separator"},
            "report":{"type":"boolean","description":"Require a fresh JSON verdict. The game receives --aurum-report <path> and must write an object with boolean ok; missing or failed reports fail the operation."},
            "platform":{"type":"string","enum":aurum_studio_core::project_ops::EXPORT_PLATFORMS,"description":"Target for configure_export; defaults to Windows Desktop. Templates, SDKs, signing, and device testing remain platform requirements."},
            "release":{"type":"boolean"},"debug":{"type":"boolean"},"preset":{"type":"string"},"output":{"type":"string"}
        },"required":["op"]
    });
    let mut read = common.clone();
    read["properties"]["op"]["enum"] = json!([
        "status",
        "files",
        "read",
        "draft_read",
        "scene_inspect",
        "classes",
        "class_info",
        "runtime_info",
        "presets"
    ]);
    let mut write = common;
    write["properties"]["op"]["enum"] = json!([
        "write",
        "draft_save",
        "draft_clear",
        "undo",
        "build",
        "validate",
        "play",
        "export",
        "scene_create",
        "scene_edit",
        "set_main_scene",
        "configure_export",
        "package"
    ]);
    vec![
        Tool {name:"aurum_project_query",description:"Inspect an Aurum project without a visible editor. Discover files, read text with hashes, inspect saved scene trees, or query the installed runtime's classes and properties. Start with op=status. Scene paths are relative to the Godot project; file paths are relative to the Aurum project.",input_schema:read,read_only:true,handler:query},
        Tool {name:"aurum_project_action",description:"Work headlessly through Aurum Studio: write/undo files, build, validate, run a bounded game, export a named preset, or transactionally create/edit a scene. Scene operations create/set/remove/reparent/attach_script. Properties accept JSON scalars, vectors {x,y,z}, colors {r,g,b,a}, and resources {resource: class, properties: object}. Saved changes survive reopening; a failed scene batch leaves the previous scene intact. Supply expected_sha256 after reading to protect concurrent edits.",input_schema:write,read_only:false,handler:action}
    ]
}
