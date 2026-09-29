//! Aurum editor bridge: a stable GDExtension surface for driving the live
//! Godot editor.
//!
//! ## Why this is small on purpose
//!
//! Phase 0 established the reload boundary (`docs/HOT_RELOAD.md`): safe Rust
//! *implementation* changes hot-reload into a running editor, but changes to
//! the native class registration or its Godot-facing methods need a controlled
//! editor restart.
//!
//! That makes the size of this surface the thing that decides whether hot
//! reload is useful in practice. So it exposes a **handful of generic
//! primitives** — create a node, set a property, attach a script, save — and
//! nothing else. Every higher-level tool lives in `aurum-mcp`, which is plain
//! Rust and rebuilds freely. Adding a tool therefore never touches the native
//! surface and never costs an editor restart.
//!
//! ## Why there is no editor API here
//!
//! gdext 0.5 gates `EditorPlugin` and friends behind `experimental-godot-api`.
//! That flag only gates gdext's *Rust bindings*, not Godot itself, and
//! enabling it workspace-wide would change the build of the Phase 0-verified
//! game shim. So the split is:
//!
//! - **Rust (here)** does the work, using only APIs that exist in a normal
//!   running game: `Node`, `ClassDb`, `ResourceLoader`, `PackedScene`.
//! - **GDScript (`addons/aurum_editor/plugin.gd`)** supplies the three things
//!   that genuinely need the editor: the edited scene root, undo integration,
//!   and marking the scene dirty.
//!
//! That is the same shape as `AurumNode`: one stable Rust surface, with the
//! scriptable side kept thin.
//!
//! ## Bridge transport
//!
//! A directory of JSON files rather than a socket. Godot is not thread-safe,
//! so a socket would need a worker thread plus a main-thread hand-off queue
//! and `Send` plumbing, buying latency nobody needs for editor operations.
//! The plugin calls [`AurumEditor::pump_requests`] once per frame; requests
//! arrive when they arrive, which for authoring is soon enough.

use godot::classes::{ClassDb, Node, PackedScene, ResourceLoader};
use godot::init::{gdextension, ExtensionLibrary, InitLevel};
use godot::prelude::*;

mod bridge_socket;

/// Marker type for this GDExtension's entry point.
pub struct AurumEditorExtension;

#[gdextension(entry_symbol = aurum_editor_init)]
unsafe impl ExtensionLibrary for AurumEditorExtension {
    fn min_level() -> InitLevel {
        InitLevel::Scene
    }
}

/// The single Godot-facing class for editor control. Registered as
/// `AurumEditor`.
#[derive(GodotClass)]
#[class(base=Node, rename=AurumEditor, tool)]
pub struct AurumEditor {
    base: Base<Node>,
    /// The edited scene root, handed over by the GDScript editor plugin.
    ///
    /// Held weakly in spirit: this is a plain reference to a node that Godot
    /// owns, and it is cleared when the editor closes the scene.
    scene_root: Option<Gd<Node>>,
    /// The socket bridge, when one was started.
    ///
    /// Optional because the file pump remains a supported transport: a project
    /// that has not started a socket keeps working exactly as before.
    bridge: Option<bridge_socket::BridgeServer>,
    command_handler: Option<Callable>,
}

#[godot_api]
impl INode for AurumEditor {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            scene_root: None,
            bridge: None,
            command_handler: None,
        }
    }
}

/// Build a JSON error response.
fn gstring(text: String) -> GString {
    GString::from(text.as_str())
}

fn failure(message: impl std::fmt::Display) -> GString {
    gstring(serde_json::json!({ "ok": false, "error": message.to_string() }).to_string())
}

/// Build a JSON success response.
fn success(value: serde_json::Value) -> GString {
    let mut object = match value {
        serde_json::Value::Object(map) => map,
        other => {
            let mut map = serde_json::Map::new();
            map.insert("value".into(), other);
            map
        }
    };
    object.insert("ok".into(), serde_json::Value::Bool(true));
    gstring(serde_json::Value::Object(object).to_string())
}

/// Convert a JSON value into a Godot `Variant`.
fn json_to_variant(value: &serde_json::Value) -> Variant {
    match value {
        serde_json::Value::Null => Variant::nil(),
        serde_json::Value::Bool(b) => Variant::from(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Variant::from(i)
            } else {
                Variant::from(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => Variant::from(GString::from(s.as_str())),
        serde_json::Value::Array(items) => {
            let mut array = godot::builtin::Array::<Variant>::new();
            for item in items {
                array.push(&json_to_variant(item));
            }
            Variant::from(array)
        }
        serde_json::Value::Object(map) => {
            let mut dictionary = godot::builtin::Dictionary::<GString, Variant>::new();
            for (key, item) in map {
                let key = GString::from(key.as_str());
                dictionary.set(&key, &json_to_variant(item));
            }
            Variant::from(dictionary)
        }
    }
}

/// Convert JSON to a `Variant` matching the type the target property already
/// holds.
///
/// JSON has no vector or colour type, so `{"x": 1, "y": 2, "z": 3}` would
/// otherwise arrive as a Dictionary and Godot would refuse to assign it to
/// `position`. Reading the property's current type first is what makes a
/// JSON-driven property set work for the properties that actually matter.
fn json_to_typed_variant(
    value: &serde_json::Value,
    expected: godot::builtin::VariantType,
) -> Variant {
    use godot::builtin::VariantType;
    use godot::builtin::{Color, Vector2, Vector3};

    let component =
        |key: &str| -> Option<f32> { value.get(key).and_then(|v| v.as_f64()).map(|v| v as f32) };

    match expected {
        VariantType::VECTOR3 => {
            if let (Some(x), Some(y), Some(z)) = (component("x"), component("y"), component("z")) {
                return Variant::from(Vector3::new(x, y, z));
            }
        }
        VariantType::VECTOR2 => {
            if let (Some(x), Some(y)) = (component("x"), component("y")) {
                return Variant::from(Vector2::new(x, y));
            }
        }
        VariantType::COLOR => {
            if let (Some(r), Some(g), Some(b)) = (component("r"), component("g"), component("b")) {
                let a = component("a").unwrap_or(1.0);
                return Variant::from(Color::from_rgba(r, g, b, a));
            }
        }
        _ => {}
    }

    // Anything else falls back to the plain JSON mapping.
    json_to_variant(value)
}

/// Convert a Godot `Variant` into JSON, as far as it can be represented.
fn variant_to_json(value: &Variant) -> serde_json::Value {
    use godot::builtin::VariantType;
    match value.get_type() {
        VariantType::NIL => serde_json::Value::Null,
        VariantType::BOOL => serde_json::Value::Bool(value.to::<bool>()),
        VariantType::INT => serde_json::Value::from(value.to::<i64>()),
        VariantType::FLOAT => serde_json::Value::from(value.to::<f64>()),
        VariantType::STRING => serde_json::Value::String(value.to::<GString>().to_string()),
        VariantType::STRING_NAME => serde_json::Value::String(value.to::<StringName>().to_string()),
        VariantType::NODE_PATH => serde_json::Value::String(value.to::<NodePath>().to_string()),
        VariantType::VECTOR2 => {
            let v = value.to::<godot::builtin::Vector2>();
            serde_json::json!({ "x": v.x, "y": v.y })
        }
        VariantType::VECTOR3 => {
            let v = value.to::<godot::builtin::Vector3>();
            serde_json::json!({ "x": v.x, "y": v.y, "z": v.z })
        }
        VariantType::COLOR => {
            let c = value.to::<godot::builtin::Color>();
            serde_json::json!({ "r": c.r, "g": c.g, "b": c.b, "a": c.a })
        }
        VariantType::ARRAY => {
            let array = value.to::<godot::builtin::Array<Variant>>();
            serde_json::Value::Array(array.iter_shared().map(|v| variant_to_json(&v)).collect())
        }
        VariantType::DICTIONARY => {
            let dictionary = value.to::<godot::builtin::Dictionary<GString, Variant>>();
            let mut map = serde_json::Map::new();
            for (key, item) in dictionary.iter_shared() {
                map.insert(key.to_string(), variant_to_json(&item));
            }
            serde_json::Value::Object(map)
        }
        // Anything else (resources, objects, transforms) is reported by class
        // rather than guessed at, so a caller can tell it was not serialized.
        other => serde_json::Value::String(format!("<{other:?}>")),
    }
}

#[godot_api]
impl AurumEditor {
    #[func]
    fn set_command_handler(&mut self, handler: Callable) {
        self.command_handler = Some(handler);
    }
    // ----- editor context -------------------------------------------------

    /// Hand over the edited scene root. Called by the editor plugin.
    #[func]
    fn set_scene_root(&mut self, root: Gd<Node>) {
        self.scene_root = Some(root);
    }

    /// Forget the current scene. Called when the editor closes it.
    #[func]
    fn clear_scene_root(&mut self) {
        self.scene_root = None;
    }

    /// Whether a scene is currently being edited.
    #[func]
    fn has_scene_root(&self) -> bool {
        self.scene_root.is_some()
    }

    // ----- inspection -----------------------------------------------------

    /// The edited scene as a JSON tree: name, class, path, and children.
    ///
    /// This is the grounding call: one call tells a tool what it is working
    /// with, instead of a walk of `get_node` calls.
    #[func]
    fn describe_scene(&self) -> GString {
        let Some(root) = self.scene_root.as_ref() else {
            return failure("no scene is open in the editor");
        };
        success(serde_json::json!({
            "root": describe_node(root, root),
        }))
    }

    /// Total nodes in the edited scene.
    #[func]
    fn node_count(&self) -> i64 {
        match self.scene_root.as_ref() {
            Some(root) => count_nodes(root) as i64,
            None => 0,
        }
    }

    /// The node classes available to `create_node`, for discovery.
    ///
    /// A fixed, curated list rather than all of `ClassDb`: these are the types
    /// that make sense to place directly, and an explicit list is something a
    /// tool can act on without guessing.
    #[func]
    fn node_types(&self) -> GString {
        let types = [
            "Node3D",
            "MeshInstance3D",
            "Camera3D",
            "DirectionalLight3D",
            "OmniLight3D",
            "SpotLight3D",
            "StaticBody3D",
            "RigidBody3D",
            "CharacterBody3D",
            "Area3D",
            "CollisionShape3D",
            "Marker3D",
            "GPUParticles3D",
            "AudioStreamPlayer3D",
            "Node2D",
            "Sprite2D",
            "AnimatedSprite2D",
            "Camera2D",
            "CharacterBody2D",
            "StaticBody2D",
            "Area2D",
            "CollisionShape2D",
            "Marker2D",
            "AudioStreamPlayer2D",
            "Control",
            "Label",
            "Button",
            "Panel",
            "TextureRect",
            "ProgressBar",
            "AnimationPlayer",
            "Timer",
            "Node",
        ];
        gstring(
            serde_json::Value::Array(types.iter().map(|t| serde_json::Value::from(*t)).collect())
                .to_string(),
        )
    }

    // ----- mutation -------------------------------------------------------

    /// Create a node under `parent_path`, or under the scene root when the
    /// path is empty or `.`.
    #[func]
    fn create_node(
        &mut self,
        parent_path: GString,
        type_name: GString,
        node_name: GString,
    ) -> GString {
        let Some(root) = self.scene_root.as_ref() else {
            return failure("no scene is open in the editor");
        };

        let class = type_name.to_string();
        if class.trim().is_empty() {
            return failure("type_name must not be empty");
        }

        let class_db = ClassDb::singleton();
        if !class_db.class_exists(class.as_str()) {
            return failure(format!("no such class: {class}"));
        }

        let parent = match resolve_node(root, &parent_path.to_string()) {
            Ok(node) => node,
            Err(message) => return failure(message),
        };

        // ClassDb::instantiate returns a bare Variant; anything that is not a
        // Node (a RefCounted, or a class needing constructor arguments) is
        // rejected rather than inserted into the tree.
        let instance = class_db.instantiate(class.as_str());
        let Ok(mut node) = instance.try_to::<Gd<Node>>() else {
            return failure(format!("'{class}' is not a Node type and cannot be placed"));
        };

        let requested = node_name.to_string();
        if !requested.trim().is_empty() {
            node.set_name(requested.as_str());
        }

        let mut parent = parent;
        parent.add_child(&node);
        node.set_owner(root);

        success(serde_json::json!({
            "name": node.get_name().to_string(),
            "class": node.get_class().to_string(),
            "path": node_path_from(root, &node),
        }))
    }

    /// Set a property from a JSON value.
    #[func]
    fn set_node_property(
        &mut self,
        node_path: GString,
        property: GString,
        value_json: GString,
    ) -> GString {
        let Some(root) = self.scene_root.as_ref() else {
            return failure("no scene is open in the editor");
        };
        let node = match resolve_node(root, &node_path.to_string()) {
            Ok(node) => node,
            Err(message) => return failure(message),
        };

        let name = property.to_string();
        if name.trim().is_empty() {
            return failure("property must not be empty");
        }

        let parsed: serde_json::Value = match serde_json::from_str(&value_json.to_string()) {
            Ok(value) => value,
            Err(e) => return failure(format!("value is not valid JSON: {e}")),
        };

        let mut node = node;
        // Match the property's existing type so vectors and colours accept a
        // JSON object, then re-read to confirm what actually landed.
        let expected = node.get(name.as_str()).get_type();
        let variant = json_to_typed_variant(&parsed, expected);
        node.set(name.as_str(), &variant);
        let read_back = node.get(name.as_str());

        success(serde_json::json!({
            "property": name,
            "value": variant_to_json(&read_back),
        }))
    }

    /// Attach a script to a node.
    #[func]
    fn attach_script(&mut self, node_path: GString, script_path: GString) -> GString {
        let Some(root) = self.scene_root.as_ref() else {
            return failure("no scene is open in the editor");
        };
        let node = match resolve_node(root, &node_path.to_string()) {
            Ok(node) => node,
            Err(message) => return failure(message),
        };

        let path = script_path.to_string();
        if !path.starts_with("res://") {
            return failure(format!("script path must start with res://, got '{path}'"));
        }

        let mut loader = ResourceLoader::singleton();
        let Some(resource) = loader.load(path.as_str()) else {
            return failure(format!("could not load script '{path}'"));
        };
        // gdext downcasts objects through Variant rather than TryFrom.
        let Ok(script) = Variant::from(resource).try_to::<Gd<godot::classes::Script>>() else {
            return failure(format!("'{path}' is not a script"));
        };

        let mut node = node;
        node.set_script(&script);

        success(serde_json::json!({
            "node": node_path.to_string(),
            "script": path,
        }))
    }

    /// Remove a node and its subtree from the edited scene.
    #[func]
    fn remove_node(&mut self, node_path: GString) -> GString {
        let Some(root) = self.scene_root.as_ref() else {
            return failure("no scene is open in the editor");
        };
        let requested = node_path.to_string();
        if requested.trim().is_empty() || requested == "." {
            return failure("refusing to remove the scene root");
        }

        let node = match resolve_node(root, &requested) {
            Ok(node) => node,
            Err(message) => return failure(message),
        };

        let path = node_path_from(root, &node);
        let mut parent = match node.get_parent() {
            Some(parent) => parent,
            None => return failure("node has no parent"),
        };
        parent.remove_child(&node);
        node.free();

        success(serde_json::json!({ "removed": path }))
    }

    /// Move a node under a different parent.
    #[func]
    fn reparent_node(&mut self, node_path: GString, new_parent_path: GString) -> GString {
        let Some(root) = self.scene_root.as_ref() else {
            return failure("no scene is open in the editor");
        };
        let requested = node_path.to_string();
        if requested.trim().is_empty() || requested == "." {
            return failure("refusing to reparent the scene root");
        }

        let node = match resolve_node(root, &requested) {
            Ok(node) => node,
            Err(message) => return failure(message),
        };
        let new_parent = match resolve_node(root, &new_parent_path.to_string()) {
            Ok(parent) => parent,
            Err(message) => return failure(message),
        };

        // Re-parenting a node into its own subtree would detach the scene.
        if is_ancestor_of(&node, &new_parent) {
            return failure("cannot reparent a node into its own subtree");
        }

        let mut old_parent = match node.get_parent() {
            Some(parent) => parent,
            None => return failure("node has no parent"),
        };
        old_parent.remove_child(&node);
        let mut new_parent = new_parent;
        new_parent.add_child(&node);
        let mut node = node;
        node.set_owner(root);

        success(serde_json::json!({
            "node": requested,
            "parent": new_parent_path.to_string(),
        }))
    }

    // ----- output ---------------------------------------------------------

    /// Pack the edited scene and return it, or null on failure.
    ///
    /// Saving happens in GDScript: `ResourceSaver` is the editor's business,
    /// and returning the resource keeps that decision on the script side.
    #[func]
    fn pack_scene(&self) -> Variant {
        let Some(root) = self.scene_root.as_ref() else {
            return Variant::nil();
        };
        let mut scene = PackedScene::new_gd();
        if scene.pack(root) != godot::global::Error::OK {
            return Variant::nil();
        }
        Variant::from(scene)
    }

    // ----- bridge ---------------------------------------------------------

    /// Drain a directory of JSON request files, apply each, and write the
    /// response next to it.
    ///
    /// Returns how many requests were handled. The editor plugin calls this
    /// once per frame. Requests are processed in filename order so a caller
    /// can sequence operations by numbering them.
    #[func]
    fn pump_requests(&mut self, request_dir: GString, response_dir: GString) -> i64 {
        let request_path = std::path::PathBuf::from(request_dir.to_string());
        let response_path = std::path::PathBuf::from(response_dir.to_string());

        if std::fs::create_dir_all(&response_path).is_err() {
            return 0;
        }
        let entries = match std::fs::read_dir(&request_path) {
            Ok(entries) => entries,
            Err(_) => return 0,
        };

        let mut files: Vec<std::path::PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == "json"))
            .collect();
        files.sort();

        let mut handled = 0i64;
        for file in files {
            let processing = file.with_extension("processing");
            if std::fs::rename(&file, &processing).is_err() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&processing) else {
                continue;
            };
            let id = file
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            let response = self.handle_request(&text);

            // The request is removed only after its response is written, so a
            // crash mid-request leaves it to be retried rather than lost.
            let target = response_path.join(format!("{id}.json"));
            let staged = target.with_extension("json.tmp");
            if std::fs::write(&staged, response.to_string()).is_ok()
                && std::fs::rename(&staged, &target).is_ok()
            {
                let _ = std::fs::remove_file(&processing);
                handled += 1;
            }
        }
        handled
    }

    /// Start the socket bridge on `127.0.0.1:port`.
    ///
    /// A port of zero asks the operating system for a free one, which is the
    /// only way to run a second editor without guessing what is already
    /// listening; the chosen port comes back in the reply and is also readable
    /// from [`AurumEditor::bridge_port`].
    ///
    /// The token is required on every request. A socket is reachable by every
    /// process on this machine, so the bind address is loopback and is not
    /// configurable — that is the difference between a local tool and an open
    /// door — and the token is what separates this editor's caller from every
    /// other program that can open a socket.
    ///
    /// Starting a bridge that is already running replaces it, so a caller
    /// reconnecting after a reload does not have to stop first.
    #[func]
    fn start_bridge(&mut self, port: i64, token: GString) -> GString {
        if token.is_empty() {
            return failure("the bridge needs a token; an unauthenticated socket is an open door");
        }
        let requested = match u16::try_from(port) {
            Ok(port) => port,
            Err(_) => return failure(format!("'{port}' is not a valid port")),
        };

        match bridge_socket::BridgeServer::start(requested, token.to_string()) {
            Ok(server) => {
                let bound = server.port();
                self.bridge = Some(server);
                success(serde_json::json!({ "port": bound, "host": "127.0.0.1" }))
            }
            Err(error) => failure(format!(
                "could not listen on 127.0.0.1:{requested}: {error}"
            )),
        }
    }

    /// Stop the bridge, if one is running.
    #[func]
    fn stop_bridge(&mut self) -> GString {
        let was_running = self.bridge.take().is_some();
        success(serde_json::json!({ "stopped": was_running }))
    }

    /// The port the bridge is listening on, or zero when it is not running.
    #[func]
    fn bridge_port(&self) -> i64 {
        self.bridge
            .as_ref()
            .map(|bridge| i64::from(bridge.port()))
            .unwrap_or(0)
    }

    /// Run the requests that arrived since the last frame.
    ///
    /// Called once per frame by the editor plugin, for the same reason the file
    /// pump is: the scene tree is not thread-safe, so a request that touches it
    /// has to run here, on the main thread, rather than on the connection that
    /// delivered it.
    ///
    /// Returns how many were handled.
    #[func]
    fn pump_bridge(&mut self) -> i64 {
        let mut handled = 0i64;
        // Bounded by construction: the queue refuses work past its ceiling, so
        // this cannot be made to spin for an unbounded time in one frame.
        while let Some(job) = self
            .bridge
            .as_ref()
            .and_then(bridge_socket::BridgeServer::take)
        {
            let response = self.handle_request(&job.text);
            // A caller that gave up is not an error; the work was still done.
            let _ = job.reply.send(response.to_string());
            handled += 1;
        }
        handled
    }

    /// How many requests are waiting to be run.
    ///
    /// Reported so a caller can see the bridge falling behind rather than only
    /// feeling it as latency.
    #[func]
    fn bridge_depth(&self) -> i64 {
        self.bridge
            .as_ref()
            .map(|bridge| bridge.depth() as i64)
            .unwrap_or(0)
    }

    /// The dispatch table. Kept deliberately flat and small: this is the
    /// stable surface, and every variant here is a permanent commitment.
    fn handle_request(&mut self, text: &str) -> GString {
        let request: serde_json::Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(e) => return failure(format!("request is not valid JSON: {e}")),
        };
        let Some(op) = request.get("op").and_then(|v| v.as_str()) else {
            return failure("request is missing 'op'");
        };
        if !matches!(op, "describe_scene" | "node_count") {
            if let Some(handler) = self
                .command_handler
                .as_ref()
                .filter(|handler| handler.is_valid())
            {
                return handler
                    .call(&[GString::from(text).to_variant()])
                    .try_to::<GString>()
                    .unwrap_or_else(|_| {
                        failure("editor command handler returned an invalid response")
                    });
            }
        }
        let argument = |key: &str| -> GString {
            gstring(
                request
                    .get(key)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            )
        };

        match op {
            "describe_scene" => self.describe_scene(),
            "node_count" => success(serde_json::json!({ "count": self.node_count() })),
            "create_node" => {
                self.create_node(argument("parent"), argument("type"), argument("name"))
            }
            "set_property" => {
                self.set_node_property(argument("node"), argument("property"), argument("value"))
            }
            "attach_script" => self.attach_script(argument("node"), argument("script")),
            "remove_node" => self.remove_node(argument("node")),
            "reparent_node" => self.reparent_node(argument("node"), argument("parent")),
            other => failure(format!("unknown op '{other}'")),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve a path relative to the scene root. Empty and `.` mean the root.
fn resolve_node(root: &Gd<Node>, path: &str) -> Result<Gd<Node>, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Ok(root.clone());
    }
    root.get_node_or_null(trimmed)
        .ok_or_else(|| format!("no node at '{trimmed}'"))
}

/// Path of `node` relative to `root`, using `.` for the root itself.
///
/// Walks parent links rather than calling `get_path()`. Godot refuses that on
/// any node outside a running SceneTree ("cannot get path of node as it is not
/// in a scene tree"), which includes a scene handed over before the tree is
/// live — exactly the situation when an editor opens a scene.
fn node_path_from(root: &Gd<Node>, node: &Gd<Node>) -> String {
    if node == root {
        return ".".to_string();
    }

    let mut segments: Vec<String> = Vec::new();
    let mut current = node.clone();
    loop {
        if &current == root {
            segments.reverse();
            return segments.join("/");
        }
        let Some(parent) = current.get_parent() else {
            // Not a descendant of root. Report the detached name rather than
            // inventing a path that would not resolve.
            return format!("<detached>/{}", node.get_name());
        };
        segments.push(current.get_name().to_string());
        current = parent;
    }
}

/// Whether `ancestor` is `node` or sits above it.
fn is_ancestor_of(ancestor: &Gd<Node>, node: &Gd<Node>) -> bool {
    let mut current = Some(node.clone());
    while let Some(candidate) = current {
        if &candidate == ancestor {
            return true;
        }
        current = candidate.get_parent();
    }
    false
}

fn count_nodes(node: &Gd<Node>) -> usize {
    1 + node
        .get_children()
        .iter_shared()
        .map(|child| count_nodes(&child))
        .sum::<usize>()
}

/// One node and its subtree as JSON.
fn describe_node(root: &Gd<Node>, node: &Gd<Node>) -> serde_json::Value {
    let children: Vec<serde_json::Value> = node
        .get_children()
        .iter_shared()
        .map(|child| describe_node(root, &child))
        .collect();

    let mut object = serde_json::Map::new();
    object.insert(
        "name".into(),
        serde_json::Value::String(node.get_name().to_string()),
    );
    object.insert(
        "class".into(),
        serde_json::Value::String(node.get_class().to_string()),
    );
    object.insert(
        "path".into(),
        serde_json::Value::String(node_path_from(root, node)),
    );
    if node.get_script().is_some() {
        // Only whether a script is attached, not which: reading the resource
        // path would need the resource itself, and "is there one" is what a
        // tool needs to decide whether to attach one.
        object.insert("has_script".into(), serde_json::Value::Bool(true));
    }
    if !children.is_empty() {
        object.insert("children".into(), serde_json::Value::Array(children));
    }
    serde_json::Value::Object(object)
}
