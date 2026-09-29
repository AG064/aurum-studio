//! The `aurum_*` tool catalog and its handlers.
//!
//! Every handler is a thin adapter over [`Engine`]: it validates arguments,
//! calls the engine, and serializes the result. No simulation semantics live
//! here — those belong to `aurum-core` and `aurum-space` (design risk R6).
//!
//! Tool names are flat `snake_case` with an `aurum_` prefix for maximum MCP
//! client compatibility, and every read-only tool carries `readOnlyHint` so a
//! client can run in a read-only posture without a second code path.

use std::path::{Component, Path, PathBuf};

use aurum_core::prelude::StateValue;
use aurum_vn::{Story, VarValue};
use serde_json::{json, Value};

use crate::engine::{var_value_to_json, Engine};

/// A tool invocation failure.
///
/// These are reported to the client as `isError: true` inside a successful
/// JSON-RPC result, so the model can read the message and correct itself
/// rather than losing the transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// The arguments were missing, malformed, or out of range.
    Invalid(String),
    /// The call was refused by a guard (for example a path outside the root).
    Denied(String),
    /// The engine rejected the operation.
    Engine(String),
    /// A filesystem operation failed.
    Io(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Denied(m) | Self::Engine(m) | Self::Io(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ToolError {}

pub(crate) type ToolResult = Result<Value, ToolError>;

/// Everything a handler may touch.
pub struct ToolContext<'a> {
    pub engine: &'a mut Engine,
    pub paths: &'a PathGuard,
    /// Where the live-editor bridge lives, when the server was started with
    /// one. `None` means the editor tools explain how to configure it.
    pub editor_bridge: Option<&'a Path>,
    /// Whether mutating tools are refused.
    ///
    /// Carried here so a handler can *describe* the server's posture rather
    /// than guess at it. Without it a status tool could only report what it
    /// hoped the configuration was, which is worse than not reporting at all.
    pub read_only: bool,
    /// Tools withheld by name, whatever their annotations say.
    pub denied: &'a [String],
}

/// The signature every tool handler implements.
pub type Handler = fn(&mut ToolContext<'_>, &Value) -> ToolResult;

/// One entry in the catalog.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    /// When true the tool cannot mutate engine state or the filesystem.
    pub read_only: bool,
    pub handler: Handler,
}

// ---------------------------------------------------------------------------
// Argument extraction
// ---------------------------------------------------------------------------

/// A thin, strict reader over a tool call's arguments object.
pub(crate) struct Args<'a>(pub(crate) &'a Value);

impl<'a> Args<'a> {
    pub(crate) fn new(params: &'a Value) -> Result<Self, ToolError> {
        match params {
            Value::Null => Ok(Self(&Value::Null)),
            Value::Object(_) => Ok(Self(params)),
            other => Err(ToolError::Invalid(format!(
                "arguments must be an object, got {}",
                kind_of(other)
            ))),
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&'a Value> {
        match self.0 {
            Value::Object(map) => map.get(key).filter(|v| !v.is_null()),
            _ => None,
        }
    }

    /// A required string.
    pub(crate) fn str(&self, key: &str) -> Result<&'a str, ToolError> {
        match self.get(key) {
            Some(Value::String(s)) => Ok(s.as_str()),
            Some(other) => Err(ToolError::Invalid(format!(
                "'{key}' must be a string, got {}",
                kind_of(other)
            ))),
            None => Err(ToolError::Invalid(format!(
                "missing required argument '{key}'"
            ))),
        }
    }

    /// A required integer.
    pub(crate) fn i64(&self, key: &str) -> Result<i64, ToolError> {
        match self.get(key) {
            Some(Value::Number(n)) => n
                .as_i64()
                .ok_or_else(|| ToolError::Invalid(format!("'{key}' must be an integer"))),
            Some(other) => Err(ToolError::Invalid(format!(
                "'{key}' must be an integer, got {}",
                kind_of(other)
            ))),
            None => Err(ToolError::Invalid(format!(
                "missing required argument '{key}'"
            ))),
        }
    }

    pub(crate) fn f64(&self, key: &str) -> Result<f64, ToolError> {
        match self.get(key) {
            Some(Value::Number(n)) => n
                .as_f64()
                .ok_or_else(|| ToolError::Invalid(format!("'{key}' must be a number"))),
            Some(other) => Err(ToolError::Invalid(format!(
                "'{key}' must be a number, got {}",
                kind_of(other)
            ))),
            None => Err(ToolError::Invalid(format!(
                "missing required argument '{key}'"
            ))),
        }
    }

    pub(crate) fn bool_or(&self, key: &str, default: bool) -> Result<bool, ToolError> {
        match self.get(key) {
            Some(Value::Bool(b)) => Ok(*b),
            Some(other) => Err(ToolError::Invalid(format!(
                "'{key}' must be a boolean, got {}",
                kind_of(other)
            ))),
            None => Ok(default),
        }
    }

    pub(crate) fn usize_or(&self, key: &str, default: usize) -> Result<usize, ToolError> {
        match self.get(key) {
            Some(Value::Number(n)) => n.as_u64().map(|v| v as usize).ok_or_else(|| {
                ToolError::Invalid(format!("'{key}' must be a non-negative integer"))
            }),
            Some(other) => Err(ToolError::Invalid(format!(
                "'{key}' must be a non-negative integer, got {}",
                kind_of(other)
            ))),
            None => Ok(default),
        }
    }

    /// Required raw JSON.
    pub(crate) fn value(&self, key: &str) -> Result<&'a Value, ToolError> {
        self.get(key)
            .ok_or_else(|| ToolError::Invalid(format!("missing required argument '{key}'")))
    }

    /// An optional array of strings.
    pub(crate) fn string_array(&self, key: &str) -> Result<Vec<String>, ToolError> {
        match self.get(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| {
                    v.as_str().map(str::to_string).ok_or_else(|| {
                        ToolError::Invalid(format!("'{key}' must be an array of strings"))
                    })
                })
                .collect(),
            Some(other) => Err(ToolError::Invalid(format!(
                "'{key}' must be an array of strings, got {}",
                kind_of(other)
            ))),
        }
    }
}

pub(crate) fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn entity_required(args: &Args<'_>) -> Result<i64, ToolError> {
    let id = args.i64("entity")?;
    if id < 1 {
        return Err(ToolError::Invalid(format!(
            "'entity' must be a positive id, got {id}"
        )));
    }
    Ok(id)
}

// ---------------------------------------------------------------------------
// Path guard
// ---------------------------------------------------------------------------

/// Confines tool-driven file access to a single root directory.
///
/// The design's rule is "prefer tools that do not take paths at all", but
/// save and load genuinely need one. This guard is the compromise: paths are
/// resolved against a configured root, `..` escapes are refused lexically, and
/// when the target exists its canonical form is re-checked so a symlink cannot
/// step outside either.
#[derive(Debug, Clone)]
pub struct PathGuard {
    root: PathBuf,
    canonical_root: Option<PathBuf>,
}

impl PathGuard {
    /// Create a guard rooted at `root`. A relative root is resolved against
    /// the current directory.
    ///
    /// The root is absolutized up front. Without that, a root of `"."` breaks
    /// the containment check below: normalizing `./save.json` yields
    /// `save.json`, which no longer starts with the literal `"."`, so every
    /// path would be denied.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let root = std::path::absolute(&root).unwrap_or(root);
        let canonical_root = root.canonicalize().ok();
        Self {
            root,
            canonical_root,
        }
    }

    /// The configured root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a client-supplied path, or refuse it.
    pub fn resolve(&self, requested: &str) -> Result<PathBuf, ToolError> {
        if requested.trim().is_empty() {
            return Err(ToolError::Invalid("'path' must not be empty".into()));
        }
        if requested.contains('\0') {
            return Err(ToolError::Denied("path contains a NUL byte".into()));
        }

        let candidate = Path::new(requested);
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.root.join(candidate)
        };

        let normalized = lexical_normalize(&joined).ok_or_else(|| {
            ToolError::Denied(format!(
                "path '{requested}' escapes the allowed root '{}'",
                self.root.display()
            ))
        })?;

        if !normalized.starts_with(&self.root) {
            return Err(ToolError::Denied(format!(
                "path '{requested}' is outside the allowed root '{}'",
                self.root.display()
            )));
        }

        // Re-check through symlinks whenever we can: the lexical check above
        // cannot see a link that points outside the root.
        if let Some(canonical_root) = &self.canonical_root {
            let probe = if normalized.exists() {
                Some(normalized.clone())
            } else {
                normalized
                    .parent()
                    .filter(|p| p.exists())
                    .map(Path::to_path_buf)
            };
            if let Some(probe) = probe {
                if let Ok(real) = probe.canonicalize() {
                    if !real.starts_with(canonical_root) {
                        return Err(ToolError::Denied(format!(
                            "path '{requested}' resolves outside the allowed root '{}'",
                            self.root.display()
                        )));
                    }
                }
            }
        }

        Ok(normalized)
    }
}

/// Normalize `.` and `..` without touching the filesystem.
///
/// Returns `None` when the path climbs above its own root, which is the
/// escape signal callers care about.
fn lexical_normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn json_to_state_value(value: &Value) -> Option<StateValue> {
    match value {
        Value::Bool(b) => Some(StateValue::Bool(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(StateValue::Int(i))
            } else {
                n.as_f64().map(StateValue::Float)
            }
        }
        Value::String(s) => Some(StateValue::String(s.clone())),
        // aurum-core reserves StateValue::Json for opaque structured blobs.
        Value::Object(_) | Value::Array(_) => Some(StateValue::Json(value.to_string())),
        Value::Null => None,
    }
}

fn state_value_to_json(value: &StateValue) -> Value {
    match value {
        StateValue::Bool(b) => json!(b),
        StateValue::Int(i) => json!(i),
        StateValue::Float(f) => json!(f),
        StateValue::String(s) => json!(s),
        StateValue::Json(raw) => serde_json::from_str(raw).unwrap_or_else(|_| json!(raw)),
    }
}

pub(crate) fn ok(value: Value) -> ToolResult {
    Ok(value)
}

/// The shared "you forgot to load a story" failure.
fn no_story() -> ToolError {
    ToolError::Invalid("no story is loaded; call aurum_story_load first".into())
}

// ---------------------------------------------------------------------------
// Read handlers
// ---------------------------------------------------------------------------

fn tool_world_snapshot(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    const DEFAULT_LIMIT: usize = 200;
    let args = Args::new(params)?;
    let include_entities = args.bool_or("include_entities", true)?;
    let entity_limit = args.usize_or("entity_limit", DEFAULT_LIMIT)?;
    ok(ctx.engine.snapshot(include_entities, entity_limit))
}

fn tool_entity_list(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let all_of = args.string_array("all_of")?;
    let ids = if !all_of.is_empty() {
        ctx.engine.world().entities_with_all(&all_of)
    } else {
        match args.get("component_type") {
            Some(_) => ctx
                .engine
                .world()
                .entities_with(args.str("component_type")?),
            None => ctx.engine.world().entity_ids(),
        }
    };
    ok(json!({ "count": ids.len(), "entities": ids }))
}

fn tool_component_get(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let entity = entity_required(&args)?;
    let type_name = args.str("type_name")?;

    if !ctx.engine.world().exists(entity) {
        return Err(ToolError::Invalid(format!(
            "entity {entity} does not exist"
        )));
    }
    match ctx.engine.world().get_component(entity, type_name) {
        Some(data) => ok(json!({ "entity": entity, "type_name": type_name, "data": data })),
        None => ok(json!({
            "entity": entity,
            "type_name": type_name,
            "data": Value::Null,
            "present": false,
        })),
    }
}

fn tool_state_get(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let key = args.str("key")?;
    match ctx.engine.state().get(key) {
        Some(value) => {
            ok(json!({ "key": key, "present": true, "value": state_value_to_json(value) }))
        }
        None => ok(json!({ "key": key, "present": false, "value": Value::Null })),
    }
}

fn tool_state_list(ctx: &mut ToolContext<'_>, _params: &Value) -> ToolResult {
    let entries = ctx.engine.state_entries();
    let count = entries.as_object().map(|m| m.len()).unwrap_or(0);
    ok(json!({ "count": count, "entries": entries }))
}

fn tool_module_list(ctx: &mut ToolContext<'_>, _params: &Value) -> ToolResult {
    ok(json!({ "modules": ctx.engine.modules() }))
}

fn tool_fingerprint(_ctx: &mut ToolContext<'_>, _params: &Value) -> ToolResult {
    ok(json!({
        "fingerprint": crate::engine::runtime_fingerprint(),
        "server": "aurum-mcp",
        "version": env!("CARGO_PKG_VERSION"),
        "surface": "headless",
    }))
}

fn tool_space_state(ctx: &mut ToolContext<'_>, _params: &Value) -> ToolResult {
    let snapshot = ctx.engine.space().snapshot();
    ok(serde_json::to_value(snapshot).unwrap_or(Value::Null))
}

fn tool_time_get(ctx: &mut ToolContext<'_>, _params: &Value) -> ToolResult {
    ok(json!({
        "time_scale": ctx.engine.time_scale(),
        "fixed_step_seconds": ctx.engine.fixed_step_seconds(),
    }))
}

// ---------------------------------------------------------------------------
// Write handlers
// ---------------------------------------------------------------------------

fn tool_entity_spawn(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let count = args.usize_or("count", 1)?;
    if count == 0 {
        return Err(ToolError::Invalid("'count' must be at least 1".into()));
    }
    if count > 10_000 {
        return Err(ToolError::Invalid(
            "'count' is capped at 10000 per call".into(),
        ));
    }
    let ids: Vec<i64> = (0..count).map(|_| ctx.engine.world_mut().spawn()).collect();
    if ids.len() == 1 {
        ok(json!({ "entity": ids[0] }))
    } else {
        ok(json!({ "entities": ids, "count": ids.len() }))
    }
}

fn tool_entity_despawn(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let entity = entity_required(&args)?;
    let existed = ctx.engine.world_mut().despawn(entity);
    ok(json!({ "entity": entity, "despawned": existed }))
}

fn tool_component_set(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let entity = entity_required(&args)?;
    let type_name = args.str("type_name")?;
    let data = args.value("data")?.clone();

    if !ctx.engine.world().exists(entity) {
        return Err(ToolError::Invalid(format!(
            "entity {entity} does not exist; spawn it first"
        )));
    }
    ctx.engine
        .world_mut()
        .set_component(entity, type_name, data.clone());
    ok(json!({ "entity": entity, "type_name": type_name, "data": data }))
}

fn tool_component_remove(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let entity = entity_required(&args)?;
    let type_name = args.str("type_name")?;
    let removed = ctx.engine.world_mut().remove_component(entity, type_name);
    ok(json!({ "entity": entity, "type_name": type_name, "removed": removed }))
}

fn tool_event_emit(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let type_name = args.str("type_name")?;
    let data = args.get("data").cloned().unwrap_or_else(|| json!({}));
    ctx.engine.emit_event(type_name, data);
    ok(json!({
        "type_name": type_name,
        "pending_events": ctx.engine.pending_event_count(),
    }))
}

fn tool_event_drain(ctx: &mut ToolContext<'_>, _params: &Value) -> ToolResult {
    let drained = ctx.engine.drain_events();
    let events: Vec<Value> = drained
        .iter()
        .map(|e| json!({ "type_name": e.type_name, "data": e.data }))
        .collect();
    ok(json!({ "count": events.len(), "events": events }))
}

fn tool_state_set(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let key = args.str("key")?;
    if key.is_empty() {
        return Err(ToolError::Invalid("'key' must not be empty".into()));
    }
    let raw = args.value("value")?;
    let value = json_to_state_value(raw).ok_or_else(|| {
        ToolError::Invalid(
            "'value' must be a bool, integer, number, string, object, or array (not null)".into(),
        )
    })?;
    let echoed = state_value_to_json(&value);
    // set_owned, not set: the key comes from the client, so it must not be
    // leaked to satisfy a &'static str bound.
    ctx.engine.state_mut().set_owned(key.to_string(), value);
    ok(json!({ "key": key, "value": echoed }))
}

fn tool_time_set_scale(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let scale = args.f64("scale")?;
    ctx.engine.set_time_scale(scale as f32);
    ok(json!({
        "time_scale": ctx.engine.time_scale(),
        "clamped": (scale as f32) != ctx.engine.time_scale(),
    }))
}

fn tool_module_register(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let name = args.str("name")?;
    ctx.engine.register_module(name);
    ok(json!({ "modules": ctx.engine.modules() }))
}

fn tool_space_step(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let seconds = args.f64("seconds")?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(ToolError::Invalid(
            "'seconds' must be a non-negative finite number".into(),
        ));
    }
    let ticks = ctx.engine.step_space(seconds as f32);
    ok(json!({
        "ticks": ticks,
        "tick_seconds": ctx.engine.fixed_step_seconds(),
        "simulated_seconds": ticks as f64 * ctx.engine.fixed_step_seconds() as f64,
        "time_scale": ctx.engine.time_scale(),
    }))
}

fn tool_save(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let requested = args.str("path")?;
    let path = ctx.paths.resolve(requested)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ToolError::Io(format!("could not create '{}': {e}", parent.display())))?;
    }

    let payload = ctx.engine.to_save_json();
    let text = serde_json::to_string_pretty(&payload)
        .map_err(|e| ToolError::Io(format!("could not serialize: {e}")))?;
    std::fs::write(&path, &text)
        .map_err(|e| ToolError::Io(format!("could not write '{}': {e}", path.display())))?;

    ok(json!({
        "path": path.display().to_string(),
        "bytes": text.len(),
        "entity_count": ctx.engine.world().entity_count(),
    }))
}

fn tool_load(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let requested = args.str("path")?;
    let path = ctx.paths.resolve(requested)?;

    let text = std::fs::read_to_string(&path)
        .map_err(|e| ToolError::Io(format!("could not read '{}': {e}", path.display())))?;
    let payload: Value = serde_json::from_str(&text)
        .map_err(|e| ToolError::Invalid(format!("'{}' is not valid JSON: {e}", path.display())))?;

    ctx.engine
        .load_save_json(&payload)
        .map_err(|e| ToolError::Engine(e.to_string()))?;

    ok(json!({
        "path": path.display().to_string(),
        "entity_count": ctx.engine.world().entity_count(),
        "state_keys": ctx.engine.state().len(),
        "time_scale": ctx.engine.time_scale(),
    }))
}

fn tool_reset(ctx: &mut ToolContext<'_>, _params: &Value) -> ToolResult {
    ctx.engine.reset();
    ok(json!({ "reset": true }))
}

// ---------------------------------------------------------------------------
// Story (visual novel) handlers
// ---------------------------------------------------------------------------

fn tool_story_load(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;

    let story_json = match args.get("path") {
        Some(_) => {
            let path = ctx.paths.resolve(args.str("path")?)?;
            std::fs::read_to_string(&path)
                .map_err(|e| ToolError::Io(format!("could not read '{}': {e}", path.display())))?
        }
        None => match args.get("story") {
            Some(Value::String(s)) => s.clone(),
            Some(object @ Value::Object(_)) => object.to_string(),
            Some(other) => {
                return Err(ToolError::Invalid(format!(
                    "'story' must be a JSON object or string, got {}",
                    kind_of(other)
                )))
            }
            None => {
                return Err(ToolError::Invalid(
                    "provide either 'path' (a story file) or 'story' (inline JSON)".into(),
                ))
            }
        },
    };

    let start = match args.get("start_scene") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => {
            return Err(ToolError::Invalid(format!(
                "'start_scene' must be a string, got {}",
                kind_of(other)
            )))
        }
        // Scenes are held in a BTreeMap, so "first" is the lowest name.
        None => {
            let story = Story::from_json(&story_json)
                .map_err(|e| ToolError::Invalid(format!("invalid story JSON: {e}")))?;
            story
                .scenes
                .keys()
                .next()
                .cloned()
                .ok_or_else(|| ToolError::Invalid("story contains no scenes".into()))?
        }
    };

    ctx.engine
        .load_story(&story_json, &start)
        .map_err(|e| ToolError::Engine(e.to_string()))?;

    let mut out = ctx.engine.story_state_json();
    out["started_at"] = json!(start);
    ok(out)
}

fn tool_story_state(ctx: &mut ToolContext<'_>, _params: &Value) -> ToolResult {
    ok(ctx.engine.story_state_json())
}

fn tool_story_advance(ctx: &mut ToolContext<'_>, _params: &Value) -> ToolResult {
    if !ctx.engine.has_story() {
        return Err(no_story());
    }
    match ctx.engine.advance_story() {
        Some(event) => ok(event),
        None => Err(no_story()),
    }
}

fn tool_story_pick_choice(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let index = args.i64("index")?;
    if index < 0 {
        return Err(ToolError::Invalid(format!(
            "'index' must be zero or greater, got {index}"
        )));
    }
    if !ctx.engine.has_story() {
        return Err(no_story());
    }
    match ctx.engine.pick_story_choice(index as usize) {
        Some(Ok(event)) => ok(event),
        Some(Err(message)) => Err(ToolError::Invalid(message)),
        None => Err(no_story()),
    }
}

fn tool_story_jump_to(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let target = args.str("target")?;
    if !ctx.engine.has_story() {
        return Err(no_story());
    }
    match ctx.engine.jump_story_to(target) {
        Some(Ok(event)) => ok(event),
        Some(Err(message)) => Err(ToolError::Invalid(message)),
        None => Err(no_story()),
    }
}

fn tool_story_get_variable(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let name = args.str("name")?;
    match ctx.engine.story_variable(name) {
        Some(value) => ok(json!({
            "name": name,
            "present": true,
            "value": var_value_to_json(value),
        })),
        None => ok(json!({ "name": name, "present": false, "value": Value::Null })),
    }
}

fn tool_story_set_variable(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let name = args.str("name")?;
    let raw = args.value("value")?;
    let value = match raw {
        Value::Bool(b) => VarValue::Bool(*b),
        Value::Number(n) => VarValue::Number(
            n.as_f64()
                .ok_or_else(|| ToolError::Invalid("'value' must be a finite number".into()))?,
        ),
        Value::String(s) => VarValue::String(s.clone()),
        other => {
            return Err(ToolError::Invalid(format!(
                "story variables are bool, number, or string; got {}",
                kind_of(other)
            )))
        }
    };
    let echoed = var_value_to_json(&value);
    if !ctx.engine.set_story_variable(name, value) {
        return Err(no_story());
    }
    ok(json!({ "name": name, "value": echoed }))
}

fn tool_story_export_state(ctx: &mut ToolContext<'_>, _params: &Value) -> ToolResult {
    let Some(interpreter) = ctx.engine.story() else {
        return Err(no_story());
    };
    let raw = interpreter.export_state();
    ok(serde_json::from_str(&raw).unwrap_or_else(|_| json!(raw)))
}

fn tool_story_import_state(ctx: &mut ToolContext<'_>, params: &Value) -> ToolResult {
    let args = Args::new(params)?;
    let state = args.value("state")?;
    if !ctx.engine.has_story() {
        return Err(no_story());
    }
    let imported = ctx
        .engine
        .story_mut()
        .map(|interpreter| interpreter.import_state(&state.to_string()))
        .unwrap_or(false);
    if !imported {
        return Err(ToolError::Invalid(
            "'state' is not a valid exported story state".into(),
        ));
    }
    ok(ctx.engine.story_state_json())
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

pub(crate) fn schema(properties: Value, required: Value) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

/// Every tool this server exposes, including the content-authoring group.
///
/// Order is presentation order in `tools/list`: runtime reads first, then
/// runtime writes, then content authoring, which is also the order a cautious
/// client should consider them in.
/// Report what this server is, and what it will do.
///
/// The first thing a client should call, because every other tool's answer
/// depends on permissions the client cannot otherwise observe. Withheld tools
/// are *named* rather than merely absent, so a client can tell "this server
/// refuses that" from "I mistyped the name" — which are the same observation
/// from the outside otherwise.
///
/// This tool is exempt from the deny list. A server that can withhold its own
/// explanation of what it withholds leaves a client with no way to find out
/// why, which is a worse failure than the one the deny list exists to prevent.
fn mcp_status(ctx: &mut ToolContext<'_>, _args: &Value) -> ToolResult {
    let all = catalog();

    let denied: Vec<&str> = all
        .iter()
        .filter(|tool| tool.name != STATUS_TOOL && ctx.denied.iter().any(|d| d == tool.name))
        .map(|tool| tool.name)
        .collect();

    let withheld_by_read_only: Vec<&str> = all
        .iter()
        .filter(|tool| ctx.read_only && !tool.read_only && !denied.contains(&tool.name))
        .map(|tool| tool.name)
        .collect();

    let withheld = denied.len() + withheld_by_read_only.len();

    Ok(json!({
        "root": ctx.paths.root().display().to_string(),
        "read_only": ctx.read_only,
        "editor_bridge": ctx.editor_bridge.map(|path| path.display().to_string()),
        "tools": {
            "total": all.len(),
            "exposed": all.len() - withheld,
            "withheld": withheld,
            "denied": denied,
            "withheld_by_read_only": withheld_by_read_only,
        },
    }))
}

/// The name of the tool that describes the others.
///
/// Named once because three places have to agree on it: the catalog, the deny
/// list's exemption, and the error a refused call returns.
pub const STATUS_TOOL: &str = "aurum_mcp_status";

pub fn catalog() -> Vec<Tool> {
    let mut tools = vec![
        Tool {
            name: "aurum_world_snapshot",
            description: "Return the whole engine session in one call: entity ids and their \
                          components, global state, time scale, registered modules, pending event \
                          count, and the space simulation. Call this first to ground yourself \
                          before issuing other tools. Entity payloads are bounded by \
                          `entity_limit` (default 200) and report `entities_truncated` when cut.",
            input_schema: schema(
                json!({
                    "include_entities": { "type": "boolean", "description": "Include per-entity component data (default true)." },
                    "entity_limit": { "type": "integer", "minimum": 0, "description": "Maximum entities to include (default 200)." }
                }),
                json!([]),
            ),
            read_only: true,
            handler: tool_world_snapshot,
        },
        Tool {
            name: "aurum_entity_list",
            description:
                "List entity ids, optionally filtered. Pass `component_type` for entities \
                          carrying that component, or `all_of` for entities carrying every listed \
                          component. With neither, returns all live entities.",
            input_schema: schema(
                json!({
                    "component_type": { "type": "string", "description": "Only entities with this component type." },
                    "all_of": { "type": "array", "items": { "type": "string" }, "description": "Only entities carrying all of these component types. Takes precedence over component_type." }
                }),
                json!([]),
            ),
            read_only: true,
            handler: tool_entity_list,
        },
        Tool {
            name: "aurum_component_get",
            description:
                "Read one component blob from one entity. Reports `present: false` rather \
                          than failing when the entity exists but lacks the component.",
            input_schema: schema(
                json!({
                    "entity": { "type": "integer", "minimum": 1, "description": "Entity id." },
                    "type_name": { "type": "string", "description": "Component type name, e.g. Position2D." }
                }),
                json!(["entity", "type_name"]),
            ),
            read_only: true,
            handler: tool_component_get,
        },
        Tool {
            name: "aurum_state_get",
            description: "Read one global state value by key.",
            input_schema: schema(
                json!({ "key": { "type": "string", "description": "State key." } }),
                json!(["key"]),
            ),
            read_only: true,
            handler: tool_state_get,
        },
        Tool {
            name: "aurum_state_list",
            description: "List every global state key and value, sorted by key.",
            input_schema: schema(json!({}), json!([])),
            read_only: true,
            handler: tool_state_list,
        },
        Tool {
            name: "aurum_module_list",
            description: "List the genre modules registered on this session.",
            input_schema: schema(json!({}), json!([])),
            read_only: true,
            handler: tool_module_list,
        },
        Tool {
            name: "aurum_fingerprint",
            description: "Identify the runtime that would answer other calls: its compile-time \
                          fingerprint, server name, version, and surface.",
            input_schema: schema(json!({}), json!([])),
            read_only: true,
            handler: tool_fingerprint,
        },
        Tool {
            name: "aurum_space_state",
            description: "Return the full 6DOF space simulation snapshot: sector and local \
                          position, orientation, velocity, angular velocity, fuel, heat, shield, \
                          hull, boost, docked, and tick.",
            input_schema: schema(json!({}), json!([])),
            read_only: true,
            handler: tool_space_state,
        },
        Tool {
            name: "aurum_time_get",
            description: "Return the current time scale and the fixed timestep in seconds.",
            input_schema: schema(json!({}), json!([])),
            read_only: true,
            handler: tool_time_get,
        },
        Tool {
            name: "aurum_entity_spawn",
            description: "Create one or more entities and return their ids. Ids start at 1 and \
                          only move forward.",
            input_schema: schema(
                json!({ "count": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "How many entities to create (default 1)." } }),
                json!([]),
            ),
            read_only: false,
            handler: tool_entity_spawn,
        },
        Tool {
            name: "aurum_entity_despawn",
            description: "Destroy an entity and all of its components.",
            input_schema: schema(
                json!({ "entity": { "type": "integer", "minimum": 1, "description": "Entity id." } }),
                json!(["entity"]),
            ),
            read_only: false,
            handler: tool_entity_despawn,
        },
        Tool {
            name: "aurum_component_set",
            description: "Attach or replace a component blob on an existing entity. `data` is \
                          arbitrary JSON. The entity must already exist; spawn it first.",
            input_schema: schema(
                json!({
                    "entity": { "type": "integer", "minimum": 1, "description": "Entity id." },
                    "type_name": { "type": "string", "description": "Component type name." },
                    "data": { "description": "Component payload as arbitrary JSON." }
                }),
                json!(["entity", "type_name", "data"]),
            ),
            read_only: false,
            handler: tool_component_set,
        },
        Tool {
            name: "aurum_component_remove",
            description: "Remove a component from an entity.",
            input_schema: schema(
                json!({
                    "entity": { "type": "integer", "minimum": 1, "description": "Entity id." },
                    "type_name": { "type": "string", "description": "Component type name." }
                }),
                json!(["entity", "type_name"]),
            ),
            read_only: false,
            handler: tool_component_remove,
        },
        Tool {
            name: "aurum_event_emit",
            description: "Queue an event for delivery on the next aurum_event_drain. The payload \
                          is coerced to an object.",
            input_schema: schema(
                json!({
                    "type_name": { "type": "string", "description": "Event type name." },
                    "data": { "type": "object", "description": "Event payload object (default {})." }
                }),
                json!(["type_name"]),
            ),
            read_only: false,
            handler: tool_event_emit,
        },
        Tool {
            name: "aurum_event_drain",
            description: "Remove and return every queued event, oldest first.",
            input_schema: schema(json!({}), json!([])),
            read_only: false,
            handler: tool_event_drain,
        },
        Tool {
            name: "aurum_state_set",
            description: "Set a global state value. Accepts a bool, integer, number, string, \
                          object, or array; objects and arrays are stored as opaque JSON.",
            input_schema: schema(
                json!({
                    "key": { "type": "string", "description": "State key." },
                    "value": { "description": "Value to store." }
                }),
                json!(["key", "value"]),
            ),
            read_only: false,
            handler: tool_state_set,
        },
        Tool {
            name: "aurum_time_set_scale",
            description: "Set the time scale. 1.0 is normal, 2.0 fast, 0.5 slow, 0.0 paused. \
                          Clamped to 0.0..=100.0 by the engine.",
            input_schema: schema(
                json!({ "scale": { "type": "number", "minimum": 0, "description": "Time scale." } }),
                json!(["scale"]),
            ),
            read_only: false,
            handler: tool_time_set_scale,
        },
        Tool {
            name: "aurum_module_register",
            description: "Register a genre module name on this session. Idempotent.",
            input_schema: schema(
                json!({ "name": { "type": "string", "description": "Module name, e.g. aurum-space." } }),
                json!(["name"]),
            ),
            read_only: false,
            handler: tool_module_register,
        },
        Tool {
            name: "aurum_space_step",
            description: "Advance the space simulation by simulated seconds using the fixed \
                          timestep accumulator, scaled by the current time scale. Returns the \
                          number of fixed ticks that ran.",
            input_schema: schema(
                json!({ "seconds": { "type": "number", "minimum": 0, "description": "Simulated seconds to advance." } }),
                json!(["seconds"]),
            ),
            read_only: false,
            handler: tool_space_step,
        },
        Tool {
            name: "aurum_save",
            description: "Write the whole session to a JSON file, in the same format the Godot \
                          bridge uses. The path is resolved inside the server's root directory \
                          and may not escape it.",
            input_schema: schema(
                json!({ "path": { "type": "string", "description": "Path relative to the server root." } }),
                json!(["path"]),
            ),
            read_only: false,
            handler: tool_save,
        },
        Tool {
            name: "aurum_load",
            description: "Replace the session from a JSON file written by aurum_save. A rejected \
                          payload leaves the current session untouched.",
            input_schema: schema(
                json!({ "path": { "type": "string", "description": "Path relative to the server root." } }),
                json!(["path"]),
            ),
            read_only: false,
            handler: tool_load,
        },
        Tool {
            name: "aurum_reset",
            description: "Clear the world, state, events, story, and space simulation. Registered \
                          modules and the time scale survive.",
            input_schema: schema(json!({}), json!([])),
            read_only: false,
            handler: tool_reset,
        },
        Tool {
            name: "aurum_story_load",
            description: "Load a visual-novel story and position the cursor. Give either `path` \
                          (a JSON file inside the server root) or `story` (inline JSON). When \
                          `start_scene` is omitted the lowest-named scene is used. Replaces any \
                          story already loaded; a failure leaves the previous one intact.",
            input_schema: schema(
                json!({
                    "path": { "type": "string", "description": "Story file relative to the server root." },
                    "story": { "type": ["object", "string"], "description": "Inline story JSON." },
                    "start_scene": { "type": "string", "description": "Scene to begin at." }
                }),
                json!([]),
            ),
            read_only: false,
            handler: tool_story_load,
        },
        Tool {
            name: "aurum_story_state",
            description: "The story cursor: current scene and entry index, variables, and any \
                          pending choice block. Reports `loaded: false` when no story is loaded.",
            input_schema: schema(json!({}), json!([])),
            read_only: true,
            handler: tool_story_state,
        },
        Tool {
            name: "aurum_story_advance",
            description: "Advance the story one step and return the event: Dialogue, Choice, \
                          SceneEnded, Quit, Goto, Command, or Error. On a Choice event, call \
                          aurum_story_pick_choice with one of the returned indices.",
            input_schema: schema(json!({}), json!([])),
            read_only: false,
            handler: tool_story_advance,
        },
        Tool {
            name: "aurum_story_pick_choice",
            description: "Choose an option from the pending choice block by its visible index, \
                          then return the next event.",
            input_schema: schema(
                json!({ "index": { "type": "integer", "minimum": 0, "description": "Index from the Choice event's choices array." } }),
                json!(["index"]),
            ),
            read_only: false,
            handler: tool_story_pick_choice,
        },
        Tool {
            name: "aurum_story_jump_to",
            description: "Jump to a label or scene, then return the next event.",
            input_schema: schema(
                json!({ "target": { "type": "string", "description": "Label or scene name." } }),
                json!(["target"]),
            ),
            read_only: false,
            handler: tool_story_jump_to,
        },
        Tool {
            name: "aurum_story_get_variable",
            description: "Read one story variable.",
            input_schema: schema(
                json!({ "name": { "type": "string", "description": "Variable name." } }),
                json!(["name"]),
            ),
            read_only: true,
            handler: tool_story_get_variable,
        },
        Tool {
            name: "aurum_story_set_variable",
            description: "Set a story variable. Values are bool, number, or string, matching the \
                          interpreter's variable model.",
            input_schema: schema(
                json!({
                    "name": { "type": "string", "description": "Variable name." },
                    "value": { "type": ["boolean", "number", "string"], "description": "New value." }
                }),
                json!(["name", "value"]),
            ),
            read_only: false,
            handler: tool_story_set_variable,
        },
        Tool {
            name: "aurum_story_export_state",
            description: "Export the story cursor and variables as JSON, for handing to \
                          aurum_story_import_state later.",
            input_schema: schema(json!({}), json!([])),
            read_only: true,
            handler: tool_story_export_state,
        },
        Tool {
            name: "aurum_story_import_state",
            description: "Restore a story cursor and variables from a value produced by \
                          aurum_story_export_state. Requires a story to be loaded already.",
            input_schema: schema(
                json!({ "state": { "type": "object", "description": "An exported story state." } }),
                json!(["state"]),
            ),
            read_only: false,
            handler: tool_story_import_state,
        },
        Tool {
            name: STATUS_TOOL,
            description: "Report this server's effective permissions: the directory every path \
                          is confined to, whether mutating tools are refused, and which tools \
                          are withheld and why. Call this first when a tool you expected is \
                          missing from the list.",
            input_schema: json!({ "type": "object", "properties": {} }),
            read_only: true,
            handler: mcp_status,
        },
    ];
    tools.extend(crate::content_tools::catalog());
    tools.extend(crate::editor_tools::catalog());
    tools.extend(crate::project_tools::catalog());
    tools
}

/// Find a tool by name.
pub fn find(name: &str) -> Option<Tool> {
    catalog().into_iter().find(|t| t.name == name)
}

/// The `tools/list` payload under the server's effective permissions.
///
/// Withheld tools are **omitted rather than merely refused**, so a client never
/// sees a tool it cannot use. That is the whole reason the filtering happens
/// here and not only at the call site: advertising something that will always
/// be refused wastes a turn and reads as a broken server.
///
/// The status tool is never omitted, for the same reason it can never be
/// denied — a client has to be able to find out what it is missing.
///
/// There is deliberately no argument-less `list_payload()`. There was one, and
/// a `list_payload_for(read_only_only)` beside it, and between them they made
/// it easy to build a payload that ignored the server's permissions — the
/// exact mistake that puts a tool in front of a client that will always refuse
/// it. The two parameters are not decoration; making a caller write `false,
/// &[]` to mean "unrestricted" is the point.
pub fn list_payload_with(read_only_only: bool, denied: &[String]) -> Value {
    let tools: Vec<Value> = catalog()
        .iter()
        .filter(|t| t.name == STATUS_TOOL || !denied.iter().any(|d| d == t.name))
        .filter(|t| !read_only_only || t.read_only)
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema,
                "annotations": { "readOnlyHint": t.read_only },
            })
        })
        .collect();
    json!({ "tools": tools })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_root(root: &str) -> (Engine, PathGuard) {
        (Engine::new(), PathGuard::new(root))
    }

    fn call(engine: &mut Engine, paths: &PathGuard, name: &str, args: Value) -> ToolResult {
        let tool = find(name).expect("tool exists");
        let mut ctx = ToolContext {
            engine,
            paths,
            editor_bridge: None,
            read_only: false,
            denied: &[],
        };
        (tool.handler)(&mut ctx, &args)
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aurum-mcp-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn catalog_names_are_unique_and_prefixed() {
        let tools = catalog();
        let mut names: Vec<&str> = tools.iter().map(|t| t.name).collect();
        assert!(names.iter().all(|n| n.starts_with("aurum_")));
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(before, names.len(), "tool names must be unique");
    }

    #[test]
    fn every_tool_has_a_description_and_object_schema() {
        for tool in catalog() {
            assert!(
                !tool.description.is_empty(),
                "{} lacks a description",
                tool.name
            );
            assert_eq!(
                tool.input_schema["type"], "object",
                "{} schema must be an object",
                tool.name
            );
        }
    }

    #[test]
    fn read_only_tools_are_annotated() {
        let payload = list_payload_with(false, &[]);
        let tools = payload["tools"].as_array().unwrap();
        assert!(!tools.is_empty());
        let snapshot = tools
            .iter()
            .find(|t| t["name"] == "aurum_world_snapshot")
            .unwrap();
        assert_eq!(snapshot["annotations"]["readOnlyHint"], true);
        let spawn = tools
            .iter()
            .find(|t| t["name"] == "aurum_entity_spawn")
            .unwrap();
        assert_eq!(spawn["annotations"]["readOnlyHint"], false);
    }

    #[test]
    fn spawn_then_set_component_then_read_it_back() {
        let (mut engine, paths) = ctx_with_root(".");
        let spawned = call(&mut engine, &paths, "aurum_entity_spawn", json!({})).unwrap();
        let id = spawned["entity"].as_i64().unwrap();

        call(
            &mut engine,
            &paths,
            "aurum_component_set",
            json!({"entity": id, "type_name": "Position2D", "data": {"x": 1.0}}),
        )
        .unwrap();

        let got = call(
            &mut engine,
            &paths,
            "aurum_component_get",
            json!({"entity": id, "type_name": "Position2D"}),
        )
        .unwrap();
        assert_eq!(got["data"]["x"], 1.0);
    }

    #[test]
    fn component_set_on_missing_entity_is_an_error() {
        let (mut engine, paths) = ctx_with_root(".");
        let err = call(
            &mut engine,
            &paths,
            "aurum_component_set",
            json!({"entity": 99, "type_name": "Tag", "data": {}}),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)));
    }

    #[test]
    fn entity_list_filters() {
        let (mut engine, paths) = ctx_with_root(".");
        let a = call(&mut engine, &paths, "aurum_entity_spawn", json!({})).unwrap()["entity"]
            .as_i64()
            .unwrap();
        let _b = call(&mut engine, &paths, "aurum_entity_spawn", json!({})).unwrap()["entity"]
            .as_i64()
            .unwrap();
        call(
            &mut engine,
            &paths,
            "aurum_component_set",
            json!({"entity": a, "type_name": "Player", "data": {}}),
        )
        .unwrap();

        let all = call(&mut engine, &paths, "aurum_entity_list", json!({})).unwrap();
        assert_eq!(all["count"], 2);
        let filtered = call(
            &mut engine,
            &paths,
            "aurum_entity_list",
            json!({"component_type": "Player"}),
        )
        .unwrap();
        assert_eq!(filtered["entities"], json!([a]));
    }

    #[test]
    fn state_set_round_trips_every_supported_type() {
        let (mut engine, paths) = ctx_with_root(".");
        for (key, value) in [
            ("b", json!(true)),
            ("i", json!(7)),
            ("f", json!(1.5)),
            ("s", json!("hi")),
        ] {
            call(
                &mut engine,
                &paths,
                "aurum_state_set",
                json!({"key": key, "value": value}),
            )
            .unwrap();
            let got = call(&mut engine, &paths, "aurum_state_get", json!({"key": key})).unwrap();
            assert_eq!(got["value"], value, "state {key} did not round-trip");
        }
    }

    #[test]
    fn state_set_stores_objects_as_opaque_json() {
        let (mut engine, paths) = ctx_with_root(".");
        call(
            &mut engine,
            &paths,
            "aurum_state_set",
            json!({"key": "loadout", "value": {"weapon": "rail", "ammo": 3}}),
        )
        .unwrap();
        let got = call(
            &mut engine,
            &paths,
            "aurum_state_get",
            json!({"key": "loadout"}),
        )
        .unwrap();
        assert_eq!(got["value"]["weapon"], "rail");
    }

    #[test]
    fn state_set_rejects_null() {
        let (mut engine, paths) = ctx_with_root(".");
        let err = call(
            &mut engine,
            &paths,
            "aurum_state_set",
            json!({"key": "k", "value": Value::Null}),
        );
        assert!(err.is_err());
    }

    #[test]
    fn missing_required_argument_is_a_clear_error() {
        let (mut engine, paths) = ctx_with_root(".");
        let err = call(
            &mut engine,
            &paths,
            "aurum_component_get",
            json!({"entity": 1}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("type_name"), "got: {err}");
    }

    #[test]
    fn wrong_argument_type_names_the_offender() {
        let (mut engine, paths) = ctx_with_root(".");
        let err = call(
            &mut engine,
            &paths,
            "aurum_component_get",
            json!({"entity": "one", "type_name": "Tag"}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("'entity'"), "got: {err}");
    }

    #[test]
    fn non_object_arguments_are_rejected() {
        let (mut engine, paths) = ctx_with_root(".");
        assert!(call(&mut engine, &paths, "aurum_world_snapshot", json!([1, 2])).is_err());
    }

    #[test]
    fn events_emit_and_drain_through_tools() {
        let (mut engine, paths) = ctx_with_root(".");
        call(
            &mut engine,
            &paths,
            "aurum_event_emit",
            json!({"type_name": "PlayerHit", "data": {"damage": 3}}),
        )
        .unwrap();
        let drained = call(&mut engine, &paths, "aurum_event_drain", json!({})).unwrap();
        assert_eq!(drained["count"], 1);
        assert_eq!(drained["events"][0]["type_name"], "PlayerHit");
        assert_eq!(drained["events"][0]["data"]["damage"], 3);
    }

    #[test]
    fn time_scale_tool_reports_clamping() {
        let (mut engine, paths) = ctx_with_root(".");
        let ok = call(
            &mut engine,
            &paths,
            "aurum_time_set_scale",
            json!({"scale": 2.0}),
        )
        .unwrap();
        assert_eq!(ok["clamped"], false);
        let clamped = call(
            &mut engine,
            &paths,
            "aurum_time_set_scale",
            json!({"scale": 999.0}),
        )
        .unwrap();
        assert_eq!(clamped["clamped"], true);
        assert_eq!(clamped["time_scale"], 100.0);
    }

    #[test]
    fn save_then_load_round_trips_through_the_filesystem() {
        let root = temp_root("roundtrip");
        let paths = PathGuard::new(&root);
        let mut engine = Engine::new();

        let id = call(&mut engine, &paths, "aurum_entity_spawn", json!({})).unwrap()["entity"]
            .as_i64()
            .unwrap();
        call(
            &mut engine,
            &paths,
            "aurum_component_set",
            json!({"entity": id, "type_name": "Tag", "data": {"kind": "hero"}}),
        )
        .unwrap();
        call(
            &mut engine,
            &paths,
            "aurum_state_set",
            json!({"key": "score", "value": 5}),
        )
        .unwrap();

        let saved = call(
            &mut engine,
            &paths,
            "aurum_save",
            json!({"path": "save.json"}),
        )
        .unwrap();
        assert!(saved["bytes"].as_u64().unwrap() > 0);

        call(&mut engine, &paths, "aurum_reset", json!({})).unwrap();
        assert_eq!(engine.world().entity_count(), 0);

        let loaded = call(
            &mut engine,
            &paths,
            "aurum_load",
            json!({"path": "save.json"}),
        )
        .unwrap();
        assert_eq!(loaded["entity_count"], 1);
        assert_eq!(
            engine.world().get_component(id, "Tag"),
            Some(&json!({"kind": "hero"}))
        );
        assert_eq!(engine.state().get_int("score").unwrap(), 5);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_of_a_missing_file_is_an_io_error() {
        let root = temp_root("missing");
        let paths = PathGuard::new(&root);
        let mut engine = Engine::new();
        let err = call(
            &mut engine,
            &paths,
            "aurum_load",
            json!({"path": "nope.json"}),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::Io(_)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn path_guard_confines_writes_to_the_root() {
        let root = temp_root("guard");
        let paths = PathGuard::new(&root);
        let mut engine = Engine::new();

        for escape in ["../outside.json", "sub/../../outside.json", ".."] {
            let result = call(&mut engine, &paths, "aurum_save", json!({"path": escape}));
            assert!(
                matches!(result, Err(ToolError::Denied(_))),
                "'{escape}' should have been denied, got {result:?}"
            );
        }

        // A path inside the root, including a nested one, is allowed.
        assert!(call(
            &mut engine,
            &paths,
            "aurum_save",
            json!({"path": "nested/save.json"})
        )
        .is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn path_guard_rejects_empty_and_nul() {
        let root = temp_root("guard2");
        let paths = PathGuard::new(&root);
        let mut engine = Engine::new();
        assert!(call(&mut engine, &paths, "aurum_save", json!({"path": "  "})).is_err());
        assert!(call(
            &mut engine,
            &paths,
            "aurum_save",
            json!({"path": "a\u{0}b"})
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_relative_root_still_admits_its_own_children() {
        // Regression: with a literal "." root, normalizing "./save.json"
        // yields "save.json", which does not start with "." -- so every path
        // was denied. `aurum mcp --root .` would have refused all file work.
        let paths = PathGuard::new(".");
        assert!(paths.root().is_absolute(), "the root must be absolutized");
        assert!(paths.resolve("save.json").is_ok());
        assert!(paths.resolve("nested/deep.json").is_ok());
        assert!(matches!(
            paths.resolve("../escape.json"),
            Err(ToolError::Denied(_))
        ));
    }

    #[test]
    fn lexical_normalize_handles_dot_and_dotdot() {
        assert_eq!(
            lexical_normalize(Path::new("/a/b/../c")).unwrap(),
            PathBuf::from("/a/c")
        );
        assert_eq!(
            lexical_normalize(Path::new("/a/./b")).unwrap(),
            PathBuf::from("/a/b")
        );
        assert!(lexical_normalize(Path::new("/a/../../b")).is_none());
    }

    #[test]
    fn spawn_caps_the_batch_size() {
        let (mut engine, paths) = ctx_with_root(".");
        assert!(call(
            &mut engine,
            &paths,
            "aurum_entity_spawn",
            json!({"count": 0})
        )
        .is_err());
        assert!(call(
            &mut engine,
            &paths,
            "aurum_entity_spawn",
            json!({"count": 999999})
        )
        .is_err());
        let many = call(
            &mut engine,
            &paths,
            "aurum_entity_spawn",
            json!({"count": 3}),
        )
        .unwrap();
        assert_eq!(many["count"], 3);
    }

    #[test]
    fn component_get_reports_absence_without_failing() {
        let (mut engine, paths) = ctx_with_root(".");
        let id = call(&mut engine, &paths, "aurum_entity_spawn", json!({})).unwrap()["entity"]
            .as_i64()
            .unwrap();
        let got = call(
            &mut engine,
            &paths,
            "aurum_component_get",
            json!({"entity": id, "type_name": "Nope"}),
        )
        .unwrap();
        assert_eq!(got["present"], false);
    }

    #[test]
    fn fingerprint_reports_the_headless_surface() {
        let (mut engine, paths) = ctx_with_root(".");
        let out = call(&mut engine, &paths, "aurum_fingerprint", json!({})).unwrap();
        assert_eq!(out["surface"], "headless");
        assert!(!out["fingerprint"].as_str().unwrap().is_empty());
    }

    #[test]
    fn space_step_reports_tick_accounting() {
        let (mut engine, paths) = ctx_with_root(".");
        let out = call(
            &mut engine,
            &paths,
            "aurum_space_step",
            json!({"seconds": 1.0}),
        )
        .unwrap();
        assert_eq!(out["ticks"], 5, "spiral guard caps at 5 per call");
        assert!(out["tick_seconds"].as_f64().unwrap() > 0.0);
    }

    const STORY: &str = r#"{
        "version": "1.0",
        "variables": { "visited": false },
        "scenes": {
            "start": {
                "entries": [
                    { "type": "dialogue", "speaker": "Narrator", "text": "You wake up." },
                    { "type": "choice", "choices": [
                        { "text": "Go left", "goto": "left" },
                        { "text": "Go right", "goto": "right" }
                    ] }
                ]
            },
            "left": { "entries": [ { "type": "dialogue", "text": "A cold corridor." } ] },
            "right": { "entries": [ { "type": "quit" } ] }
        }
    }"#;

    fn load_story_via_tool(engine: &mut Engine, paths: &PathGuard) -> Value {
        call(
            engine,
            paths,
            "aurum_story_load",
            json!({"story": STORY, "start_scene": "start"}),
        )
        .unwrap()
    }

    #[test]
    fn story_load_advance_and_branch_through_tools() {
        let (mut engine, paths) = ctx_with_root(".");
        let loaded = load_story_via_tool(&mut engine, &paths);
        assert_eq!(loaded["loaded"], true);
        assert_eq!(loaded["current_scene"], "start");

        let dialogue = call(&mut engine, &paths, "aurum_story_advance", json!({})).unwrap();
        assert_eq!(dialogue["type"], "Dialogue");
        assert_eq!(dialogue["text"], "You wake up.");

        let choice = call(&mut engine, &paths, "aurum_story_advance", json!({})).unwrap();
        assert_eq!(choice["type"], "Choice");
        assert_eq!(choice["choices"].as_array().unwrap().len(), 2);

        let after = call(
            &mut engine,
            &paths,
            "aurum_story_pick_choice",
            json!({"index": 0}),
        )
        .unwrap();
        assert_eq!(after["text"], "A cold corridor.");

        let state = call(&mut engine, &paths, "aurum_story_state", json!({})).unwrap();
        assert_eq!(state["current_scene"], "left");
    }

    #[test]
    fn story_load_defaults_to_the_first_scene_name() {
        let (mut engine, paths) = ctx_with_root(".");
        let loaded = call(
            &mut engine,
            &paths,
            "aurum_story_load",
            json!({"story": STORY}),
        )
        .unwrap();
        // "left" sorts before "right" and "start".
        assert_eq!(loaded["started_at"], "left");
    }

    #[test]
    fn story_load_from_a_file_inside_the_root() {
        let root = temp_root("story");
        let paths = PathGuard::new(&root);
        let mut engine = Engine::new();

        std::fs::write(root.join("story.json"), STORY).unwrap();
        let loaded = call(
            &mut engine,
            &paths,
            "aurum_story_load",
            json!({"path": "story.json", "start_scene": "start"}),
        )
        .unwrap();
        assert_eq!(loaded["current_scene"], "start");

        // The same guard protects story files as save files.
        let denied = call(
            &mut engine,
            &paths,
            "aurum_story_load",
            json!({"path": "../story.json"}),
        );
        assert!(matches!(denied, Err(ToolError::Denied(_))));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn story_tools_require_a_loaded_story() {
        let (mut engine, paths) = ctx_with_root(".");
        for name in [
            "aurum_story_advance",
            "aurum_story_pick_choice",
            "aurum_story_jump_to",
            "aurum_story_export_state",
        ] {
            let args = if name == "aurum_story_pick_choice" {
                json!({"index": 0})
            } else if name == "aurum_story_jump_to" {
                json!({"target": "start"})
            } else {
                json!({})
            };
            let result = call(&mut engine, &paths, name, args);
            assert!(
                matches!(result, Err(ToolError::Invalid(_))),
                "{name} should require a story, got {result:?}"
            );
        }
        // Reading story state is safe without one and reports absence.
        let state = call(&mut engine, &paths, "aurum_story_state", json!({})).unwrap();
        assert_eq!(state["loaded"], false);
    }

    #[test]
    fn story_variables_through_tools() {
        let (mut engine, paths) = ctx_with_root(".");
        load_story_via_tool(&mut engine, &paths);

        let got = call(
            &mut engine,
            &paths,
            "aurum_story_get_variable",
            json!({"name": "visited"}),
        )
        .unwrap();
        assert_eq!(got["present"], true);
        assert_eq!(got["value"], false);

        call(
            &mut engine,
            &paths,
            "aurum_story_set_variable",
            json!({"name": "visited", "value": true}),
        )
        .unwrap();
        let after = call(
            &mut engine,
            &paths,
            "aurum_story_get_variable",
            json!({"name": "visited"}),
        )
        .unwrap();
        assert_eq!(after["value"], true);
    }

    #[test]
    fn story_variables_reject_structured_values() {
        let (mut engine, paths) = ctx_with_root(".");
        load_story_via_tool(&mut engine, &paths);
        let err = call(
            &mut engine,
            &paths,
            "aurum_story_set_variable",
            json!({"name": "x", "value": {"nested": 1}}),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("bool, number, or string"),
            "got: {err}"
        );
    }

    #[test]
    fn story_export_then_import_restores_the_cursor() {
        let (mut engine, paths) = ctx_with_root(".");
        load_story_via_tool(&mut engine, &paths);
        call(&mut engine, &paths, "aurum_story_advance", json!({})).unwrap();
        call(&mut engine, &paths, "aurum_story_advance", json!({})).unwrap();
        call(
            &mut engine,
            &paths,
            "aurum_story_pick_choice",
            json!({"index": 0}),
        )
        .unwrap();

        let exported = call(&mut engine, &paths, "aurum_story_export_state", json!({})).unwrap();
        assert_eq!(exported["current_scene"], "left");

        // Reload the story from scratch, then restore the exported cursor.
        load_story_via_tool(&mut engine, &paths);
        assert_eq!(
            call(&mut engine, &paths, "aurum_story_state", json!({})).unwrap()["current_scene"],
            "start"
        );
        let restored = call(
            &mut engine,
            &paths,
            "aurum_story_import_state",
            json!({"state": exported}),
        )
        .unwrap();
        assert_eq!(restored["current_scene"], "left");
    }

    #[test]
    fn story_import_rejects_a_malformed_state() {
        let (mut engine, paths) = ctx_with_root(".");
        load_story_via_tool(&mut engine, &paths);
        let err = call(
            &mut engine,
            &paths,
            "aurum_story_import_state",
            json!({"state": "not an object"}),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)));
    }

    #[test]
    fn story_load_rejects_an_invalid_story() {
        let (mut engine, paths) = ctx_with_root(".");
        assert!(call(
            &mut engine,
            &paths,
            "aurum_story_load",
            json!({"story": "{not json"})
        )
        .is_err());
        assert!(call(
            &mut engine,
            &paths,
            "aurum_story_load",
            json!({"story": STORY, "start_scene": "nope"})
        )
        .is_err());
        // Neither argument form is a valid call on its own.
        assert!(call(&mut engine, &paths, "aurum_story_load", json!({})).is_err());
    }

    #[test]
    fn story_reset_clears_it() {
        let (mut engine, paths) = ctx_with_root(".");
        load_story_via_tool(&mut engine, &paths);
        call(&mut engine, &paths, "aurum_reset", json!({})).unwrap();
        assert_eq!(
            call(&mut engine, &paths, "aurum_story_state", json!({})).unwrap()["loaded"],
            false
        );
    }
}
