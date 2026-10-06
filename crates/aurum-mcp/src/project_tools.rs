use crate::tools::{Tool, ToolContext, ToolError, ToolResult};
use serde_json::Value;

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
    let read = aurum_studio_core::project_contract::input_schema(true);
    let write = aurum_studio_core::project_contract::input_schema(false);
    vec![
        Tool {name:"aurum_project_query",description:"Inspect an Aurum project without a visible editor. Start with op=status; op=describe returns operation discovery, and operation selects a detailed request schema. Discover files, hashed text, saved scenes or installed runtime classes. Scene paths are relative to the Godot project; file paths are relative to the Aurum project.",input_schema:read,read_only:true,handler:query},
        Tool {name:"aurum_project_action",description:"Work headlessly through Aurum: save/undo, build, validate, bounded play or rendered capture, Rust web_build, export/package and transactional scene edits. Query op=describe with operation for details and timeline examples. Supply expected_sha256 after reading to protect concurrent edits. Failed scene batches preserve the saved scene.",input_schema:write,read_only:false,handler:action}
    ]
}
