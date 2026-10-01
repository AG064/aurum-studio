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

func _ready():
	process_mode = Node.PROCESS_MODE_ALWAYS
	_options = JSON.parse_string(FileAccess.get_file_as_string("res://addons/aurum_live/playtest.json"))
	for frame in _options.get("capture_frames", []): _capture_frames.append(int(frame))
	_write_receipt()

func _process(_delta):
	if _finished:
		return
	_frame += 1
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
	if bridge != null and get_tree().current_scene != null:
		var snapshot = bridge.request({"op": "inspect", "include_state": true})
		if snapshot.get("ok",false):
			_last_state = snapshot
			_last_metrics = {"nodes": Performance.get_monitor(Performance.OBJECT_NODE_COUNT), "objects": Performance.get_monitor(Performance.OBJECT_COUNT), "draw_calls": Performance.get_monitor(Performance.RENDER_TOTAL_DRAW_CALLS_IN_FRAME), "static_memory": Performance.get_monitor(Performance.MEMORY_STATIC), "sample_frame": _frame}
	var receipt = {"version": 1, "reason": _reason, "frames_observed": _frame, "frame_budget": _options.frames, "elapsed_ms": Time.get_ticks_msec() - _started, "rendered": _options.get("rendered", false), "captures": _captures, "errors": _errors, "last_state": _last_state, "metrics": _last_metrics}
	var path = str(_options.directory).path_join("runtime.json")
	var file = FileAccess.open(path + ".tmp", FileAccess.WRITE)
	if file != null:
		file.store_string(JSON.stringify(receipt))
		file.close()
		DirAccess.rename_absolute(path + ".tmp", path)

func _exit_tree():
	if not _finished: _write_receipt()
