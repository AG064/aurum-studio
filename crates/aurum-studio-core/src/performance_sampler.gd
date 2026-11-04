extends RefCounted
## Generated only into a private playtest. These are Performance monitor reads,
## including Godot's cached rolling-second CPU maxima and scoped viewport timers.
const MAX_SAMPLES = 120
const MAX_BYTES = 65536
const METRICS = ["process_peak_ms", "physics_peak_ms", "static_memory_bytes", "node_count", "resource_count", "texture_memory_bytes", "draw_call_count", "rendered_object_count", "gpu_frame_ms", "frame_presentation_ms", "viewport_cpu_render_ms", "gpu_viewport_ms"]
const MAX_POST_DRAW = 16384
const MAX_OBSERVATION_AGE_US = 250000
var _options: Dictionary = {}
var _runtime: Dictionary = {}
var _availability: Dictionary = {}
var _samples: Array[Dictionary] = []
var _started_us = 0
var _elapsed_us = 0
var _frame = 0
var _complete = false
var _finished = false
var _reason = "incomplete"
var _viewport_rid = RID()
var _viewport_enabled = false
var _method = "unknown"
var _driver = "unknown"
var _post_draw_count = 0
var _viewport_cache: Dictionary = {}
var _viewport_failed: Dictionary = {}

func configure(options: Dictionary, main_viewport: Viewport = null) -> bool:
	if options.size() != 8 or options.get("schema_version") != 1:
		return false
	var request_id: Variant = options.get("request_id")
	if not request_id is String or request_id.length() != 32:
		return false
	for character in request_id:
		if not character in "0123456789abcdef":
			return false
	var bounds = {"frame_budget": [1, 3600000], "warmup_frames": [1, 1200], "sample_frames": [1, MAX_SAMPLES], "sample_stride": [1, 60], "min_warmup_ms": [1100, 5000], "wall_budget_ms": [1101, 60000]}
	for key in bounds:
		var value: Variant = options.get(key)
		if typeof(value) not in [TYPE_INT, TYPE_FLOAT] or not is_finite(float(value)) or float(value) != floor(float(value)) or value < bounds[key][0] or value > bounds[key][1]:
			return false
		options[key] = int(value)
	if options.min_warmup_ms >= options.wall_budget_ms or options.warmup_frames + 1 + (options.sample_frames - 1) * options.sample_stride > options.frame_budget:
		return false
	options["schema_version"] = 1
	_options = options.duplicate()
	var version = Engine.get_version_info()
	_runtime = {"pid": OS.get_process_id(), "major": int(version.major), "minor": int(version.minor), "patch": int(version.patch), "debug_build": OS.is_debug_build(), "rendered": DisplayServer.get_name() != "headless"}
	for metric in METRICS:
		_availability[metric] = "available"
	_availability.gpu_frame_ms = "not_instrumented"
	_availability.frame_presentation_ms = "not_instrumented"
	_availability.viewport_cpu_render_ms = "timer_not_ready_or_unsupported"
	_availability.gpu_viewport_ms = "timer_not_ready_or_unsupported"
	if _runtime.major != 4 or _runtime.minor != 7:
		for metric in METRICS:
			if metric not in ["gpu_frame_ms", "frame_presentation_ms"]:
				_availability[metric] = "unsupported_runtime"
		_reason = "unsupported_runtime"
		_finished = true
	elif not _runtime.debug_build:
		_availability.static_memory_bytes = "requires_debug_build"
	if _runtime.major == 4 and _runtime.minor == 7 and not _runtime.rendered:
		for metric in ["texture_memory_bytes", "draw_call_count", "rendered_object_count", "viewport_cpu_render_ms", "gpu_viewport_ms"]:
			_availability[metric] = "headless_renderer"
		_method = "headless"
		_driver = "headless"
	_started_us = Time.get_ticks_usec()
	if not _finished and _runtime.rendered:
		_configure_viewport(main_viewport)
	return true

func _configure_viewport(main_viewport: Viewport):
	_method = RenderingServer.get_current_rendering_method()
	_driver = RenderingServer.get_current_rendering_driver_name()
	if _method not in ["forward_plus", "mobile", "gl_compatibility"]:
		_method = "unknown"
	if _driver not in ["vulkan", "d3d12", "metal", "opengl3", "opengl3_es", "opengl3_angle"]:
		_driver = "unknown"
	var driver_matches = _driver in ["opengl3", "opengl3_es", "opengl3_angle"] if _method == "gl_compatibility" else _driver in ["vulkan", "d3d12", "metal"]
	if _method == "unknown" or not driver_matches:
		_invalidate_viewport("viewport_cpu_render_ms", "unsupported_renderer")
		_invalidate_viewport("gpu_viewport_ms", "unsupported_renderer")
		return
	var main_loop = Engine.get_main_loop()
	if main_viewport == null or not is_instance_valid(main_viewport) or not main_loop is SceneTree or main_viewport != main_loop.root:
		_invalidate_viewport("viewport_cpu_render_ms", "viewport_unavailable")
		_invalidate_viewport("gpu_viewport_ms", "viewport_unavailable")
		return
	_viewport_rid = main_viewport.get_viewport_rid()
	if not _viewport_rid.is_valid():
		_invalidate_viewport("viewport_cpu_render_ms", "viewport_unavailable")
		_invalidate_viewport("gpu_viewport_ms", "viewport_unavailable")
		return
	RenderingServer.viewport_set_measure_render_time(_viewport_rid, true)
	_viewport_enabled = true
	RenderingServer.frame_post_draw.connect(_on_frame_post_draw)

func _on_frame_post_draw():
	if _finished or not _viewport_enabled:
		return
	if _post_draw_count >= MAX_POST_DRAW:
		_invalidate_viewport("viewport_cpu_render_ms", "observation_bound_exceeded")
		_invalidate_viewport("gpu_viewport_ms", "observation_bound_exceeded")
		return
	_post_draw_count += 1
	# These are the latest available engine timestamp results. This signal does
	# not prove the GPU query belongs to the current process/draw frame.
	_viewport_cache = {"event": _post_draw_count, "process_frame": _frame, "elapsed_us": Time.get_ticks_usec() - _started_us,
		"cpu_ms": RenderingServer.viewport_get_measured_render_time_cpu(_viewport_rid), "gpu_ms": RenderingServer.viewport_get_measured_render_time_gpu(_viewport_rid)}

func _invalidate_viewport(metric: String, reason: String):
	if _viewport_failed.has(metric):
		return
	_viewport_failed[metric] = true
	_availability[metric] = reason
	for row in _samples:
		row.metrics[metric] = null
	if _availability.viewport_cpu_render_ms != "available" and _availability.gpu_viewport_ms != "available":
		for row in _samples:
			row.viewport_observation = null

func _viewport_values(frame: int, values: Dictionary) -> Variant:
	if not _viewport_enabled:
		return null
	var stale = _viewport_cache.is_empty() or _post_draw_count < 2
	if not stale:
		stale = _viewport_cache.process_frame < frame - 1 or _viewport_cache.process_frame > frame or _viewport_cache.elapsed_us > _elapsed_us or _elapsed_us - _viewport_cache.elapsed_us > MAX_OBSERVATION_AGE_US
	if not stale and not _samples.is_empty():
		var previous: Variant = _samples.back().viewport_observation
		if previous != null and _viewport_cache.event <= previous.event:
			stale = true
	for pair in [["viewport_cpu_render_ms", "cpu_ms"], ["gpu_viewport_ms", "gpu_ms"]]:
		var metric: String = pair[0]
		# A failure during the sampling window invalidates that entire metric's
		# window; it cannot quietly recover into a passing partial GPU budget.
		if _viewport_failed.has(metric) or _availability[metric] not in ["available", "timer_not_ready_or_unsupported"]:
			continue
		if stale:
			_invalidate_viewport(metric, "stale_viewport_observation" if _post_draw_count >= 2 else "timer_not_ready_or_unsupported")
			continue
		var value = float(_viewport_cache[pair[1]])
		if not is_finite(value) or value < 0 or value > 60000.0:
			_invalidate_viewport(metric, "invalid_viewport_timing")
		elif value == 0:
			_invalidate_viewport(metric, "timer_not_ready_or_unsupported")
		else:
			_availability[metric] = "available"
			values[metric] = value
	if values.viewport_cpu_render_ms == null and values.gpu_viewport_ms == null:
		return null
	return {"event": _viewport_cache.event, "process_frame": _viewport_cache.process_frame, "elapsed_us": _viewport_cache.elapsed_us}

func tick(frame: int):
	if _finished or _options.is_empty():
		return
	_elapsed_us = Time.get_ticks_usec() - _started_us
	_frame = frame
	if _elapsed_us > _options.wall_budget_ms * 1000:
		_finish("wall_budget_exhausted")
		return
	if frame <= _options.warmup_frames or (frame - _options.warmup_frames - 1) % _options.sample_stride != 0:
		return
	if _elapsed_us < _options.min_warmup_ms * 1000:
		_finish("warmup_wall_unmet")
		return
	var values = {}
	for metric in METRICS:
		values[metric] = null
	values.process_peak_ms = Performance.get_monitor(Performance.TIME_PROCESS) * 1000.0
	values.physics_peak_ms = Performance.get_monitor(Performance.TIME_PHYSICS_PROCESS) * 1000.0
	values.node_count = Performance.get_monitor(Performance.OBJECT_NODE_COUNT)
	values.resource_count = Performance.get_monitor(Performance.OBJECT_RESOURCE_COUNT)
	if _runtime.debug_build:
		values.static_memory_bytes = Performance.get_monitor(Performance.MEMORY_STATIC)
	if _runtime.rendered:
		values.texture_memory_bytes = Performance.get_monitor(Performance.RENDER_TEXTURE_MEM_USED)
		values.draw_call_count = Performance.get_monitor(Performance.RENDER_TOTAL_DRAW_CALLS_IN_FRAME)
		values.rendered_object_count = Performance.get_monitor(Performance.RENDER_TOTAL_OBJECTS_IN_FRAME)
	var viewport_observation: Variant = _viewport_values(frame, values)
	if values.process_peak_ms <= 0:
		_finish("monitor_not_initialized")
		return
	for metric in METRICS:
		if values[metric] != null:
			var value = float(values[metric])
			var maximum = 60000.0 if metric.ends_with("ms") else (1099511627776.0 if metric.ends_with("bytes") else 1000000000.0)
			if not is_finite(value) or value < 0 or value > maximum:
				_finish("invalid_monitor_value")
				return
			if not metric.ends_with("ms"):
				if value != floor(value):
					_finish("invalid_monitor_value")
					return
				values[metric] = int(value)
	_samples.append({"frame": frame, "elapsed_us": _elapsed_us, "metrics": values, "viewport_observation": viewport_observation})
	if _samples.size() >= _options.sample_frames:
		_complete = true
		_finish("sample_window_complete")

func _finish(reason: String):
	_reason = reason
	_finished = true
	if RenderingServer.frame_post_draw.is_connected(_on_frame_post_draw):
		RenderingServer.frame_post_draw.disconnect(_on_frame_post_draw)
	if _viewport_enabled and _viewport_rid.is_valid():
		RenderingServer.viewport_set_measure_render_time(_viewport_rid, false)

func finish_on_exit():
	if not _finished:
		_elapsed_us = Time.get_ticks_usec() - _started_us
		_finish("game_exit")

func receipt() -> Dictionary:
	var result = _options.duplicate()
	var viewport_timing = {"scope": "main_viewport_only", "backend": "rendering_server_viewport_timer", "enabled_during_sampling": _viewport_enabled,
		"method": _method, "driver": _driver, "cpu_state": _availability.viewport_cpu_render_ms, "gpu_state": _availability.gpu_viewport_ms,
		"post_draw_observations": _post_draw_count, "observation_read_age_limit_us": MAX_OBSERVATION_AGE_US,
		"subviewports_included": false, "presentation_included": false, "distinct_gpu_frame_ids_verified": false}
	result.merge({"complete": _complete, "reason": _reason, "elapsed_us": _elapsed_us, "frames_observed": _frame, "runtime": _runtime, "availability": _availability, "viewport_timing": viewport_timing, "samples": _samples.duplicate(true)})
	return result

func encoded_receipt() -> PackedByteArray:
	var encoded = JSON.stringify(receipt()).to_utf8_buffer()
	if encoded.size() <= MAX_BYTES:
		return encoded
	_complete = false
	_finish("output_budget_exceeded")
	_samples.clear()
	return JSON.stringify(receipt()).to_utf8_buffer()
