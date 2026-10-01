extends SceneTree
var failures: Array[String] = []
var checks = 0

class Fixture extends Node2D:
	@export var speed: float = 7.0
	@export var counter: int = 3
	@export var password: String = "not exposed"
	@export var amounts: Dictionary[String, int] = {"gold": 2}
	@export var steps: Array[int] = [1, 2]
	@export var weights: Array[float] = [1.5]
	@export var identifier: int = 9223372036854775807
	var history: Array[String] = ["first"]

func _init():
	call_deferred("_run")

func check(value: bool, label: String):
	checks += 1
	if not value: failures.append(label)

func _run():
	var scene = Fixture.new()
	scene.name = "Game"
	root.add_child(scene)
	current_scene = scene
	var bridge = load(OS.get_cmdline_user_args()[0]).new()
	bridge.name = "AurumLive"
	root.add_child(bridge)
	await process_frame
	var inspection = bridge.request({"op": "inspect"})
	check(inspection.ok, "ordinary scene introspection")
	check(inspection.properties.any(func(p): return p.name == "speed"), "exported properties discovered")
	check(not inspection.properties.any(func(p): return p.name == "password"), "secret fields filtered")
	check(not bridge.request({"op": "inspect", "path": "../AurumLive"}).ok, "runtime traversal refused")
	check(bridge.request({"op": "apply", "changes": [{"property": "speed", "value": 12.5}]}).ok and scene.speed == 12.5, "live edit without game restart")
	check(not bridge.request({"op": "apply", "changes": [{"property": "script", "value": "bad"}]}).ok, "executable properties refused")
	check(not bridge.request({"op": "apply", "changes": [{"property": "speed", "value": 8.0}, {"property": "counter", "value": "invalid"}]}).ok and scene.speed == 12.5, "batch type validation is atomic")
	check(not bridge.request({"op": "apply", "changes": [{"property": "counter", "value": 3.5}]}).ok, "fractional integer refused")
	check(bridge.request({"op": "apply", "changes": [{"property": "amounts", "value": {"gold": 5.0}}]}).ok and scene.amounts.gold == 5, "typed dictionary numeric coercion")
	check(not bridge.request({"op": "apply", "changes": [{"property": "amounts", "value": {"gold": "wrong"}}]}).ok and scene.amounts.gold == 5, "typed dictionary mismatch refused")
	check(bridge.request({"op": "apply", "changes": [{"property": "steps", "value": [4.0, 5.0]}]}).ok and scene.steps == [4, 5], "typed integer array numeric coercion")
	check(not bridge.request({"op": "apply", "changes": [{"property": "steps", "value": [1e30]}]}).ok, "unsafe typed array integers refused")
	check(bridge.request({"op": "apply", "changes": [{"property": "weights", "value": [2, 3]}]}).ok and scene.weights == [2.0, 3.0], "typed float array numeric coercion")
	scene.position = Vector2(40, 25)
	var render_before = RenderingServer.is_render_loop_enabled()
	var captured = bridge.request({"op": "checkpoint", "freeze": true})
	check(captured.ok and paused, "checkpoint capture and freeze")
	check(not RenderingServer.is_render_loop_enabled(), "frozen previews stop rendering while retaining runtime callbacks")
	var checkpoint = JSON.parse_string(JSON.stringify(captured.checkpoint))
	scene.speed = 1.0
	scene.counter = 0
	scene.position = Vector2.ZERO
	scene.history.assign(["changed"])
	var restored = bridge.request({"op": "restore", "checkpoint": checkpoint})
	check(restored.ok and restored.complete, "checkpoint round trip")
	check(scene.speed == 12.5 and scene.counter == 3 and scene.position == Vector2(40, 25), "scalar and vector state preserved")
	check(scene.history == ["first"], "typed array state preserved")
	check(scene.amounts.gold == 5 and scene.amounts.is_typed(), "typed dictionary state preserved")
	check(scene.identifier == 9223372036854775807, "int64 state preserved without JSON precision loss")
	check(not paused, "original pause state restored")
	check(RenderingServer.is_render_loop_enabled() == render_before, "checkpoint restores original rendering state")
	bridge.request({"op": "checkpoint", "freeze": true})
	bridge.request({"op": "resume", "paused": false})
	check(not paused and RenderingServer.is_render_loop_enabled() == render_before, "failed-build resume restores rendering and pause state")
	RenderingServer.set_render_loop_enabled(false)
	var disabled = bridge.request({"op": "checkpoint", "freeze": true})
	bridge.request({"op": "restore", "checkpoint": disabled.checkpoint})
	check(not RenderingServer.is_render_loop_enabled(), "intentionally disabled rendering is preserved")
	RenderingServer.set_render_loop_enabled(render_before)
	checkpoint.scene = "res://different.tscn"
	check(not bridge.request({"op": "restore", "checkpoint": checkpoint}).ok, "incompatible scene refused")
	var result = {"ok": failures.is_empty(), "checks": checks, "failures": failures}
	print("AURUM_RUNTIME_ACCEPTANCE " + JSON.stringify(result))
	quit(0 if failures.is_empty() else 1)
