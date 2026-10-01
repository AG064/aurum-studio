extends Node
## Generated into private snapshots only. No filesystem or arbitrary method API.
const MAX_NODES = 1024
const MAX_ITEMS = 2048
const MAX_BYTES = 1048576
const UNSUPPORTED = {"__aurum_unsupported": true}
const EDITABLE = ["position", "rotation", "scale", "visible", "modulate", "self_modulate", "text", "value", "volume_db", "pitch_scale", "speed_scale", "light_energy", "fov"]
var _callback
var _window
var _baseline: Dictionary = {}
var _baseline_scene_id = 0

func _process(_delta):
	_ensure_baseline()

func _ensure_baseline():
	var scene = get_tree().current_scene
	if scene == null or not scene.is_node_ready() or scene.has_method("aurum_capture_state") or scene.get_instance_id() == _baseline_scene_id:
		return
	_baseline_scene_id = scene.get_instance_id()
	_baseline.clear()
	var queue: Array[Node] = [scene]
	var count = 0
	while not queue.is_empty() and count < MAX_NODES:
		var node = queue.pop_front()
		var values = {}
		for name in _properties(node, true):
			var value = _encode(node.get(name))
			if not _unsupported(value): values[name] = value
		_baseline[str(scene.get_path_to(node))] = values
		queue.append_array(node.get_children())
		count += 1

func _ready():
	process_mode = Node.PROCESS_MODE_ALWAYS
	if OS.has_feature("web"):
		_window = JavaScriptBridge.get_interface("window")
		if _window != null and JavaScriptBridge.eval("window.aurumManagedPreview === true"):
			_callback = JavaScriptBridge.create_callback(_javascript_request)
			_window.aurumRuntimeRequest = _callback

func _javascript_request(arguments):
	if arguments.size() != 1:
		return
	var text = str(arguments[0])
	if text.length() > MAX_BYTES:
		_window.aurumRuntimeResponse = JSON.stringify({"ok": false, "error": "Runtime request exceeds 1 MiB"})
		return
	_window.aurumRuntimeResponse = JSON.stringify(request(JSON.parse_string(text)))

func request(input):
	if not input is Dictionary or get_tree().current_scene == null or not get_tree().current_scene.is_node_ready():
		return {"ok": false, "error": "Runtime scene is not ready"}
	_ensure_baseline()
	match str(input.get("op", "")):
		"class_info":
			var type = str(input.get("class", ""))
			if type.length() > 128 or not ClassDB.class_exists(type): return {"ok": false, "error": "Runtime class was not registered"}
			var methods = []
			for info in ClassDB.class_get_method_list(type,true):
				if methods.size() >= 512: break
				methods.append(str(info.name))
			return {"ok":true,"class":type,"registered":true,"methods":methods}
		"inspect":
			return _inspect(str(input.get("path", ".")), bool(input.get("include_state", false)))
		"apply":
			return _apply(input)
		"checkpoint":
			var result = _capture()
			if input.get("freeze", false) and result.get("ok", false):
				get_tree().paused = true
			return result
		"restore":
			return _restore(input.get("checkpoint"))
		"resume":
			get_tree().paused = bool(input.get("paused", false))
			return {"ok": true, "paused": get_tree().paused}
	return {"ok": false, "error": "Unknown runtime operation"}

func _node(path: String):
	if path.length() > 512 or path.begins_with("/") or ":" in path or ".." in path.split("/"):
		return null
	return get_tree().current_scene.get_node_or_null(NodePath(path))

func _secret(name: String) -> bool:
	var lower = name.to_lower()
	return "password" in lower or "secret" in lower or "token" in lower or "api_key" in lower

func _properties(node: Node, snapshot: bool = false) -> Dictionary:
	var result = {}
	for info in node.get_property_list():
		var name = str(info.name)
		if _secret(name):
			continue
		var scripted = int(info.usage) & PROPERTY_USAGE_SCRIPT_VARIABLE != 0
		var exported = int(info.usage) & PROPERTY_USAGE_EDITOR != 0
		var stored = int(info.usage) & PROPERTY_USAGE_STORAGE != 0
		var value_type = int(info.type)
		var native_value = exported and stored and value_type in [TYPE_BOOL,TYPE_INT,TYPE_FLOAT,TYPE_STRING,TYPE_VECTOR2,TYPE_VECTOR3,TYPE_COLOR,TYPE_ARRAY,TYPE_DICTIONARY]
		if name in EDITABLE or native_value or (scripted and (snapshot or exported)):
			result[name] = info
	return result

func _inspect(path: String, include_state: bool = false) -> Dictionary:
	var node = _node(path)
	if node == null:
		return {"ok": false, "error": "Runtime node was not found"}
	var values = []
	var properties = _properties(node, include_state)
	for name in properties:
		var value = _encode(node.get(name))
		if not _unsupported(value):
			var info = properties[name]
			values.append({"name": name, "value": value, "type": int(info.type), "hint": int(info.hint), "hint_string": str(info.hint_string)})
	var children = []
	for child in node.get_children():
		if children.size() == MAX_NODES:
			break
		children.append({"path": str(get_tree().current_scene.get_path_to(child)), "name": str(child.name), "class": child.get_class()})
	return {"ok": true, "path": path, "class": node.get_class(), "properties": values, "children": children}

func _apply(input: Dictionary) -> Dictionary:
	var changes = input.get("changes", [])
	if not changes is Array or changes.is_empty() or changes.size() > 128:
		return {"ok": false, "error": "Supply between 1 and 128 property changes"}
	var pending = []
	for change in changes:
		if not change is Dictionary:
			return {"ok": false, "error": "Invalid property change"}
		var node = _node(str(change.get("path", ".")))
		var name = str(change.get("property", ""))
		if node == null or not _properties(node).has(name):
			return {"ok": false, "error": "Property is not live-editable: " + name}
		var value = _decode(change.get("value"))
		if _unsupported(value) or not _compatible(node.get(name), value):
			return {"ok": false, "error": "Property type mismatch: " + name}
		pending.append([node, name, _coerce(node.get(name), value)])
	for item in pending:
		item[0].set(item[1], item[2])
	return {"ok": true, "applied": pending.size(), "restart": false}

func _capture() -> Dictionary:
	_ensure_baseline()
	var scene = get_tree().current_scene
	var checkpoint = {"version": 1, "scene": scene.scene_file_path, "paused": get_tree().paused}
	if scene.has_method("aurum_capture_state") and scene.has_method("aurum_restore_state"):
		var custom = _encode(scene.call("aurum_capture_state"))
		if _unsupported(custom):
			return {"ok": false, "error": "Custom checkpoint contains unsupported values"}
		checkpoint["custom"] = custom
	else:
		var nodes = []
		var queue: Array[Node] = [scene]
		var skipped = 0
		while not queue.is_empty():
			if nodes.size() >= MAX_NODES:
				return {"ok": false, "error": "Checkpoint exceeds 1024 nodes; provide custom checkpoint hooks"}
			var node = queue.pop_front()
			var values = {}
			for name in _properties(node, true):
				var value = _encode(node.get(name))
				if _unsupported(value):
					skipped += 1
				else:
					values[name] = value
			var path = str(scene.get_path_to(node))
			nodes.append({"path": path, "class": node.get_class(), "values": values, "defaults": _baseline.get(path,{})})
			queue.append_array(node.get_children())
		checkpoint["nodes"] = nodes
		checkpoint["skipped"] = skipped
	if JSON.stringify(checkpoint).length() > MAX_BYTES:
		return {"ok": false, "error": "Checkpoint exceeds 1 MiB; provide a smaller custom checkpoint"}
	return {"ok": true, "checkpoint": checkpoint, "mode": "custom" if checkpoint.has("custom") else "properties", "complete": checkpoint.has("custom") or checkpoint.get("skipped", 0) == 0}

func _restore(checkpoint) -> Dictionary:
	_ensure_baseline()
	if not checkpoint is Dictionary or checkpoint.get("version") != 1 or checkpoint.get("scene") != get_tree().current_scene.scene_file_path or JSON.stringify(checkpoint).length() > MAX_BYTES:
		return {"ok": false, "error": "Checkpoint schema or scene does not match"}
	var scene = get_tree().current_scene
	var applied = 0
	var reconfigured = 0
	var skipped = int(checkpoint.get("skipped", 0))
	if checkpoint.has("custom"):
		if not scene.has_method("aurum_restore_state"):
			return {"ok": false, "error": "The new scene has no custom restore hook"}
		var value = _decode(checkpoint.custom)
		if _unsupported(value) or scene.call("aurum_restore_state", value) != true:
			return {"ok": false, "error": "Custom checkpoint restore was rejected"}
	else:
		var nodes = checkpoint.get("nodes", [])
		if not nodes is Array or nodes.size() > MAX_NODES:
			return {"ok": false, "error": "Invalid checkpoint nodes"}
		var pending = []
		for saved in nodes:
			if not saved is Dictionary or not saved.get("values") is Dictionary:
				return {"ok": false, "error": "Invalid checkpoint property record"}
			var node = _node(str(saved.get("path", "")))
			if node == null or node.get_class() != saved.get("class"):
				skipped += 1
				continue
			var allowed = _properties(node, true)
			for name in saved.values:
				var old_defaults = saved.get("defaults",{})
				var next_defaults = _baseline.get(str(saved.path),{})
				if old_defaults is Dictionary and old_defaults.has(name) and next_defaults.has(name) and saved.values[name] == old_defaults[name] and next_defaults[name] != old_defaults[name]:
					reconfigured += 1
					continue
				var value = _decode(saved.values[name])
				if not allowed.has(name) or _unsupported(value) or not _compatible(node.get(name), value):
					skipped += 1
					continue
				pending.append([node, name, _coerce(node.get(name), value)])
		for item in pending:
			item[0].set(item[1], item[2])
			applied += 1
	get_tree().paused = bool(checkpoint.get("paused", false))
	return {"ok": true, "applied": applied, "skipped": skipped, "reconfigured": reconfigured, "complete": skipped == 0, "mode": "custom" if checkpoint.has("custom") else "properties"}

func _compatible(old, value) -> bool:
	if typeof(old) == TYPE_INT and typeof(value) == TYPE_FLOAT:
		return is_finite(value) and value == floor(value) and abs(value) <= 9007199254740991.0
	if typeof(old) == TYPE_FLOAT and typeof(value) in [TYPE_INT, TYPE_FLOAT]:
		return is_finite(float(value))
	if typeof(old) != typeof(value):
		return false
	if old is Array and old.is_typed():
		for item in value:
			if not _typed_value(old.get_typed_builtin(), item):
				return false
	if old is Dictionary and old.is_typed():
		for key in value:
			if not _typed_value(old.get_typed_key_builtin(), key) or not _typed_value(old.get_typed_value_builtin(), value[key]):
				return false
	return true

func _typed_value(type: int, value) -> bool:
	if type == TYPE_NIL: return true
	if type == TYPE_INT and typeof(value) == TYPE_FLOAT:
		return is_finite(value) and value == floor(value) and abs(value) <= 9007199254740991.0
	if type == TYPE_FLOAT and typeof(value) in [TYPE_INT, TYPE_FLOAT]: return is_finite(float(value))
	return typeof(value) == type

func _coerce(old, value):
	if typeof(old) == TYPE_INT:
		return int(value)
	if typeof(old) == TYPE_FLOAT:
		return float(value)
	if old is Array and old.is_typed():
		var result = old.duplicate()
		result.assign(value)
		return result
	if old is Dictionary and old.is_typed():
		var result = old.duplicate()
		result.assign(value)
		return result
	return value

func _unsupported(value) -> bool:
	if value is Dictionary:
		if value.get("__aurum_unsupported", false):
			return true
		for child in value.values():
			if _unsupported(child):
				return true
	if value is Array:
		for child in value:
			if _unsupported(child):
				return true
	return false

func _encode(value, depth: int = 0):
	if depth > 12:
		return UNSUPPORTED
	match typeof(value):
		TYPE_NIL, TYPE_BOOL, TYPE_STRING:
			return value if not value is String or value.length() <= 65536 else UNSUPPORTED
		TYPE_INT:
			return {"__aurum_type": "int64", "value": str(value)} if value > 9007199254740991 or value < -9007199254740991 else value
		TYPE_FLOAT:
			return value if is_finite(value) else UNSUPPORTED
		TYPE_VECTOR2:
			return {"__aurum_type": "Vector2", "value": [value.x, value.y]} if value.is_finite() else UNSUPPORTED
		TYPE_VECTOR3:
			return {"__aurum_type": "Vector3", "value": [value.x, value.y, value.z]} if value.is_finite() else UNSUPPORTED
		TYPE_COLOR:
			return {"__aurum_type": "Color", "value": [value.r, value.g, value.b, value.a]} if is_finite(value.r) and is_finite(value.g) and is_finite(value.b) and is_finite(value.a) else UNSUPPORTED
		TYPE_ARRAY, TYPE_DICTIONARY:
			if value.size() > MAX_ITEMS:
				return UNSUPPORTED
			var result = [] if value is Array else {}
			if value is Array:
				for child in value:
					result.append(_encode(child, depth + 1))
			else:
				for key in value:
					if not key is String or _secret(key):
						continue
					result[key] = _encode(value[key], depth + 1)
			return result
	return UNSUPPORTED

func _decode(value, depth: int = 0):
	if depth > 12:
		return UNSUPPORTED
	if value is Dictionary and value.has("__aurum_type"):
		var parts = value.get("value")
		var kind = str(value.__aurum_type)
		if kind == "int64":
			var text = value.get("value")
			if not text is String or text.length() > 20 or not text.is_valid_int() or str(text.to_int()) != text: return UNSUPPORTED
			return text.to_int()
		var expected = {"Vector2": 2, "Vector3": 3, "Color": 4}.get(kind, 0)
		if not parts is Array or expected == 0 or parts.size() != expected:
			return UNSUPPORTED
		for part in parts:
			if not typeof(part) in [TYPE_INT, TYPE_FLOAT] or not is_finite(float(part)):
				return UNSUPPORTED
		match kind:
			"Vector2": return Vector2(parts[0], parts[1])
			"Vector3": return Vector3(parts[0], parts[1], parts[2])
			"Color": return Color(parts[0], parts[1], parts[2], parts[3])
	if value is Array:
		if value.size() > MAX_ITEMS: return UNSUPPORTED
		var result = []
		for item in value: result.append(_decode(item, depth + 1))
		return result
	if value is Dictionary:
		if value.size() > MAX_ITEMS: return UNSUPPORTED
		var result = {}
		for key in value: result[key] = _decode(value[key], depth + 1)
		return result
	return _encode(value, depth)
