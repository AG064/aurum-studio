extends Node
var _options: Dictionary
var _frame = 0
var _started = Time.get_ticks_msec()
var _finished = false
var _capture_busy = false
var _captures: Array[String] = []
var _errors: Array[String] = []
var _reason = "game_exit"
var _capture_frames: Array[int] = []
var _last_state: Dictionary = {}
var _last_metrics: Dictionary = {}
var _performance
var _seed_pending = false
var _seed_applied = false

func _ready():
	process_mode = Node.PROCESS_MODE_ALWAYS
	_options = JSON.parse_string(FileAccess.get_file_as_string("res://addons/aurum_live/playtest.json"))
	if _options.has("seed"):
		_seed_pending = true
		call_deferred("_start_seeded_scene")
	if _options.has("performance"):
		_performance = load("res://addons/aurum_live/performance_sampler.gd").new()
		if not _performance.configure(_options.performance, get_tree().root):
			_errors.append("Invalid bounded performance sampler options")
			_performance = null
	for frame in _options.get("capture_frames", []): _capture_frames.append(int(frame))
	_write_receipt()

func _start_seeded_scene():
	var selected = str(_options.get("seed_scene", ""))
	if selected.is_empty():
		var config = ConfigFile.new()
		if config.load("res://project.godot") == OK:
			selected = str(config.get_value("application", "run/main_scene", ""))
	if selected.is_empty() or selected == "res://addons/aurum_live/seed_host.tscn":
		_errors.append("Seeded play requires an original main scene")
		get_tree().quit(1)
		return
	seed(int(_options.seed))
	_seed_applied = true
	var packed = load(selected)
	if not packed is PackedScene or get_tree().change_scene_to_packed(packed) != OK:
		_errors.append("Seeded main scene could not be loaded")
		get_tree().quit(1)
		return
	await get_tree().scene_changed
	var current_scene: Node = get_tree().current_scene
	if not is_instance_valid(current_scene) or not current_scene.is_node_ready():
		_errors.append("Seeded main scene is not ready")
		get_tree().quit(1)
		return
	_seed_pending = false

func _process(_delta):
	if _finished or _seed_pending:
		return
	_frame += 1
	if _performance != null:
		_performance.tick(_frame)
	for event in _options.get("events", []):
		if int(event.frame) == _frame:
			_input_event(event)
	if _options.get("rendered", false) and _frame in _capture_frames and not _capture_busy:
		_capture_busy = true
		_capture()
	if _frame == 1 or _frame % 60 == 0:
		_write_receipt()
	if _frame >= int(_options.frames) and not _capture_busy:
		_reason = "frame_budget_exhausted"
		_finished = true
		_write_receipt()
		get_tree().quit(0 if _errors.is_empty() else 1)

func _input_event(event: Dictionary):
	if event.get("type", "action") == "property":
		var result = get_node("/root/AurumLive").request({"op": "apply", "changes": event.changes})
		if not result.get("ok", false): _errors.append(str(result.get("error")))
		return
	var input
	match event.get("type", "action"):
		"action":
			input = InputEventAction.new()
			input.action = str(event.action)
			input.pressed = bool(event.get("pressed", true))
		"key":
			input = InputEventKey.new()
			input.keycode = OS.find_keycode_from_string(str(event.key))
			input.physical_keycode = input.keycode
			input.pressed = bool(event.get("pressed", true))
		"mouse_button":
			input = InputEventMouseButton.new()
			input.position = Vector2(float(event.x), float(event.y))
			input.global_position = input.position
			input.button_index = int(event.get("button", MOUSE_BUTTON_LEFT))
			input.pressed = bool(event.get("pressed", true))
	if input != null:
		Input.parse_input_event(input)

func _capture():
	await RenderingServer.frame_post_draw
	var image = get_viewport().get_texture().get_image()
	if image == null or image.is_empty():
		_errors.append("Renderer did not produce a capturable frame")
	else:
		var path = str(_options.directory).path_join("frame-%06d.png" % _frame)
		if image.save_png(path) == OK:
			_captures.append(path)
		else:
			_errors.append("Could not save rendered capture")
	_capture_busy = false

func _write_receipt():
	if _options.is_empty(): return
	var bridge = get_node_or_null("/root/AurumLive")
	if bridge != null and get_tree().current_scene != null and not _options.has("performance"):
		var snapshot = bridge.request({"op": "inspect", "include_state": true})
		if snapshot.get("ok",false):
			_last_state = snapshot
			_last_metrics = {"nodes": Performance.get_monitor(Performance.OBJECT_NODE_COUNT), "objects": Performance.get_monitor(Performance.OBJECT_COUNT), "draw_calls": Performance.get_monitor(Performance.RENDER_TOTAL_DRAW_CALLS_IN_FRAME), "static_memory": Performance.get_monitor(Performance.MEMORY_STATIC), "sample_frame": _frame}
	var receipt = {"version": 1, "reason": _reason, "frames_observed": _frame, "frame_budget": _options.frames, "elapsed_ms": Time.get_ticks_msec() - _started, "rendered": _options.get("rendered", false), "captures": _captures, "errors": _errors, "last_state": _last_state, "metrics": _last_metrics}
	if _options.has("seed"):
		receipt["seed"] = int(_options.seed)
		receipt["seed_applied"] = _seed_applied
		receipt["seed_scope"] = "Godot global RNG before main scene load; custom RNG and autoload work are not controlled"
	var path = str(_options.directory).path_join("runtime.json")
	var file = FileAccess.open(path + ".tmp", FileAccess.WRITE)
	if file != null:
		file.store_string(JSON.stringify(receipt))
		file.close()
		DirAccess.rename_absolute(path + ".tmp", path)
	if _performance != null:
		var encoded: PackedByteArray = _performance.encoded_receipt()
		if encoded.size() <= 65536:
			var performance_path = str(_options.directory).path_join("performance.json")
			var staging_path = performance_path + ".%d.tmp" % OS.get_process_id()
			var performance_file = FileAccess.open(staging_path, FileAccess.WRITE)
			if performance_file != null:
				performance_file.store_buffer(encoded)
				performance_file.close()
				DirAccess.rename_absolute(staging_path, performance_path)

func _exit_tree():
	if _performance != null:
		_performance.finish_on_exit()
	if not _finished: _write_receipt()
