extends Node3D
const Art = preload("res://art.gd")
const World = preload("res://world.gd")
const Courier = preload("res://courier.gd")
const Drone = preload("res://drone.gd")
const Hud = preload("res://hud.gd")
const Sound = preload("res://sound.gd")
const Effects = preload("res://effects.gd")
@export_range(3.0, 12.0) var move_speed := 7.2
@export_range(1.4, 3.4) var interaction_radius := 2.8
@export_range(12.0, 25.0) var camera_distance := 17.0
@export var reduced_motion := false
var world
var player
var hud
var sound
var effects
var camera: Camera3D
var crosshair: MeshInstance3D
var powered: Array[bool] = [false, false, false]
var carried := -1
var phase := "menu"
var paused := false
var won := false
var elapsed := 0.0
var charge_progress := 0.0
var extraction := 0.0
var extraction_waves := 0
var kills := 0
var interaction_count := 0
var boss_defeated := false
var enemies: Array = []
var projectiles: Array[Dictionary] = []
var next_uid := 1
var shake := 0.0
var camera_focus := Vector3.ZERO
var rng := RandomNumberGenerator.new()
var autoplay := false
var idle_test := false
var render_demo := false
var report_path := ""
var _reported := false
var _bot_path: PackedVector2Array = PackedVector2Array()
var _bot_path_clock := 0.0
var _last_target := Vector3.ZERO
var _seed := 2091
var best_time := 0.0
var observed_player: Dictionary = {}

func _ready() -> void:
	_actions()
	var args := OS.get_cmdline_user_args()
	autoplay = "--autoplay" in args
	idle_test = "--idle-test" in args
	render_demo = "--render-demo" in args
	var index := args.find("--aurum-report")
	if index >= 0 and index + 1 < args.size(): report_path = args[index + 1]
	index = args.find("--seed")
	if index >= 0 and index + 1 < args.size() and args[index + 1].is_valid_int(): _seed = int(args[index + 1])
	rng.seed = _seed
	world = World.new()
	world.name = "Dock"
	add_child(world)
	effects = Effects.new()
	add_child(effects)
	sound = Sound.new()
	add_child(sound)
	player = Courier.new()
	add_child(player)
	player.position = World.START
	camera = Camera3D.new()
	camera.name = "Camera"
	camera.fov = 46
	camera.near = 0.2
	camera.far = 120
	add_child(camera)
	camera.current = true
	camera_focus = player.position
	crosshair = Art.ring(0.23, Art.COPPER)
	add_child(crosshair)
	_update_camera(1.0)
	hud = Hud.new()
	hud.game = self
	add_child(hud)
	_load_profile()
	_initial_patrols()
	if autoplay or idle_test or render_demo: start_game()
	observed_player = player.snapshot()
	hud.update(0)

func _actions() -> void:
	var mapping := {"move_left": [KEY_A, KEY_LEFT], "move_right": [KEY_D, KEY_RIGHT], "move_forward": [KEY_W, KEY_UP], "move_back": [KEY_S, KEY_DOWN], "interact": [KEY_E], "dash": [KEY_SPACE], "emp": [KEY_Q], "pause": [KEY_ESCAPE], "start": [KEY_ENTER], "retry": [KEY_R]}
	for action in mapping:
		if not InputMap.has_action(action): InputMap.add_action(action)
		for key in mapping[action]:
			var event := InputEventKey.new()
			event.physical_keycode = key
			if not InputMap.action_has_event(action, event): InputMap.action_add_event(action, event)
	if not InputMap.has_action("fire"): InputMap.add_action("fire")
	var mouse := InputEventMouseButton.new()
	mouse.button_index = MOUSE_BUTTON_LEFT
	if not InputMap.action_has_event("fire", mouse): InputMap.action_add_event("fire", mouse)

func _unhandled_input(event: InputEvent) -> void:
	if event.is_action_pressed("start") and phase == "menu": start_game()
	if event.is_action_pressed("pause") and phase in ["play", "extract"]:
		paused = not paused
		if paused: hud.resume_button.grab_focus()
	if event.is_action_pressed("retry") and (phase in ["won", "lost"] or paused): restart()

func start_game() -> void:
	if phase != "menu": return
	phase = "play"
	paused = false
	sound.start_music()
	hud.announce("SHIFT STARTED / RECOVER THE POWER CORE")

func _initial_patrols() -> void:
	_spawn(Vector3(-12, 0.7, 6), "sentinel")
	_spawn(Vector3(12, 0.7, -1), "sentinel")
	_spawn(Vector3(-2, 0.7, -15), "sentinel")

func _spawn(at: Vector3, kind: String, uid := -1):
	if enemies.size() >= 14: return null
	var enemy = Drone.new()
	enemy.kind = kind
	enemy.uid = next_uid if uid < 0 else uid
	if uid < 0: next_uid += 1
	enemy.name = "Drone%d" % enemy.uid
	enemy.health = 320.0 if kind == "warden" else (36.0 if kind == "skirmisher" else 54.0)
	enemy.position = at
	enemy.anchor = at
	enemy.cooldown = rng.randf_range(0.7, 1.6)
	add_child(enemy)
	enemy.set_meta("enemy", true)
	enemies.append(enemy)
	return enemy

func _physics_process(delta: float) -> void:
	observed_player = player.snapshot()
	if phase not in ["play", "extract"] or paused:
		world.update_state(powered, carried, powered.count(true), delta * 0.25, reduced_motion)
		hud.update(delta)
		_update_camera(delta)
		return
	elapsed += delta
	var command: Dictionary = _bot_command(delta) if autoplay else _human_command()
	if idle_test: command = {"move": Vector3.ZERO, "aim": Vector3.FORWARD, "fire": false, "interact": false, "dash": false, "emp": false}
	if render_demo: command = {"move": Vector3.LEFT if elapsed < 1.2 else Vector3.FORWARD, "aim": Vector3(-1, 0, -1), "fire": true, "interact": false, "dash": elapsed > 1.0 and elapsed < 1.05, "emp": elapsed > 3.0 and elapsed < 3.05}
	if command.dash and player.dash(command.move):
		sound.play("dash")
		effects.pulse(player.position, 1.4, Art.TEAL)
	if command.emp and player.emp():
		sound.play("emp")
		effects.pulse(player.position, 5.5, Art.TEAL)
		for enemy in enemies:
			if enemy.position.distance_to(player.position) <= 5.5:
				enemy.stunned = 0.8 if enemy.kind == "warden" else 2.8
				enemy.charge = 0
	player.step(delta, command.move, command.aim, move_speed, carried >= 0, reduced_motion)
	if command.fire and player.fire():
		sound.play("shot")
		_bolt(player.position + player.facing * 1.2 + Vector3(0, 0.20, 0), player.facing * 30, true, 18, 0)
	_interaction(delta, command.interact, command.move.length())
	for enemy in enemies.duplicate():
		if is_instance_valid(enemy): enemy.step(delta, self)
	_projectiles(delta)
	effects.step(delta)
	world.update_state(powered, carried, powered.count(true), delta, reduced_motion)
	if player.health <= 0:
		phase = "lost"
		paused = false
		sound.play("explode")
		_write_report(idle_test)
		if autoplay or idle_test: _finish_run(0 if idle_test else 1)
	if phase == "extract":
		if player.position.distance_to(World.EXIT) < 3.5: extraction += delta
		var thresholds := [3.5, 9.5, 15.5]
		if extraction_waves < 3 and extraction >= thresholds[extraction_waves]:
			extraction_waves += 1
			_spawn(Vector3(4, 0.7, -24), "sentinel")
			_spawn(Vector3(16, 0.7, -20), "skirmisher")
			sound.play("alarm")
			hud.announce("SECURITY REINFORCEMENTS / KEEP THE PAD")
		if extraction >= 22 and boss_defeated:
			phase = "won"
			won = true
			player.velocity = Vector3.ZERO
			best_time = elapsed if best_time <= 0 else minf(best_time, elapsed)
			save_profile()
			sound.play("relay")
			_write_report(true)
			if autoplay: _finish_run(0)
	hud.update(delta)
	_update_camera(delta)
	observed_player = player.snapshot()

func _human_command() -> Dictionary:
	var axis := Input.get_vector("move_left", "move_right", "move_forward", "move_back")
	var direction := camera.global_transform.basis.x * axis.x + camera.global_transform.basis.z * axis.y
	direction.y = 0
	direction = direction.normalized() * minf(1, axis.length())
	var mouse := get_viewport().get_mouse_position()
	var point: Variant = Plane(Vector3.UP, player.position.y).intersects_ray(camera.project_ray_origin(mouse), camera.project_ray_normal(mouse))
	var aim: Vector3 = player.facing
	if point != null:
		aim = point - player.position
		aim.y = 0
		crosshair.position = point
		crosshair.position.y = 0.1
	return {"move": direction, "aim": aim.normalized(), "fire": Input.is_action_pressed("fire"), "interact": Input.is_action_pressed("interact"), "dash": Input.is_action_just_pressed("dash"), "emp": Input.is_action_just_pressed("emp")}

func _bot_command(delta: float) -> Dictionary:
	var active := powered.count(true)
	var target: Vector3 = World.EXIT if active == 3 else (World.STATION_POSITIONS[active] + Vector3(0, 0.7, 2.2) if carried >= 0 else World.CAP_POSITIONS[active])
	var aim: Vector3 = player.facing
	var nearest = null
	var distance := 1000.0
	for enemy in enemies:
		var candidate: float = enemy.position.distance_to(player.position)
		if candidate < distance and line_clear(player.position + Vector3(0, 0.2, 0), enemy.position + Vector3(0, 0.2, 0)):
			distance = candidate
			nearest = enemy
	var shoot := nearest != null and distance < 15
	var desired := Vector3.ZERO
	var danger := false
	if nearest != null:
		aim = nearest.position - player.position
		aim.y = 0
		danger = nearest.charge > 0.1 and distance < 13
	if shoot and (distance < 5.5 or danger):
		desired = Vector3(-aim.z, 0, aim.x).normalized()
		if distance < 4.0: desired = -aim.normalized()
	else:
		_bot_path_clock -= delta
		if _bot_path_clock <= 0 or target.distance_to(_last_target) > 1:
			_bot_path = world.path(player.position, target)
			_bot_path_clock = 0.2
			_last_target = target
		var offset: Vector3 = target - player.position
		offset.y = 0
		if (active < 3 and offset.length() > (1.0 if carried < 0 else 0.25)) or (active == 3 and offset.length() > 1.5):
			if _bot_path.size() > 1:
				var point: Vector2 = _bot_path[1]
				desired = Vector3(point.x - player.position.x, 0, point.y - player.position.z).normalized()
			else: desired = offset.normalized()
	return {"move": desired, "aim": aim.normalized(), "fire": shoot, "interact": not shoot and desired.length_squared() < 0.05, "dash": danger, "emp": shoot and distance < 5.2}

func line_clear(from: Vector3, to: Vector3) -> bool:
	return get_world_3d().direct_space_state.intersect_ray(PhysicsRayQueryParameters3D.create(from, to, 1)).is_empty()

func _bolt(at: Vector3, velocity: Vector3, friendly: bool, damage: float, owner_uid: int) -> void:
	if projectiles.size() >= 64: return
	var node := Art.orb(0.075 if friendly else 0.13, Art.TEAL if friendly else Art.RED)
	add_child(node)
	node.position = at
	var trail := Art.box(Vector3(0.07, 0.07, 0.5), Art.TEAL if friendly else Art.RED, true)
	node.add_child(trail)
	if velocity.length_squared() > 0: node.look_at(node.position + velocity)
	projectiles.append({"node": node, "velocity": velocity, "friendly": friendly, "damage": damage, "owner_uid": owner_uid, "life": 1.5})

func enemy_fire(enemy, target: Vector3) -> void:
	var aim: Vector3 = target - enemy.position
	aim.y = 0
	if aim.length_squared() < 0.01: return
	aim = aim.normalized()
	var spread := [-0.15, 0.0, 0.15] if enemy.kind == "warden" else ([-0.07, 0.07] if enemy.kind == "skirmisher" else [0.0])
	if enemy.kind == "warden" and enemy.health < 160: spread = [-0.30, -0.15, 0.0, 0.15, 0.30]
	for angle in spread:
		var direction: Vector3 = aim.rotated(Vector3.UP, angle)
		_bolt(enemy.position + direction * 1.1 + Vector3(0, 0.2, 0), direction * 10, false, 12, enemy.uid)
	sound.play("alarm")

func _projectiles(delta: float) -> void:
	for index in range(projectiles.size() - 1, -1, -1):
		var bolt: Dictionary = projectiles[index]
		var from: Vector3 = bolt.node.position
		var target: Vector3 = from + bolt.velocity * delta
		var hit := get_world_3d().direct_space_state.intersect_ray(PhysicsRayQueryParameters3D.create(from, target, 5 if bolt.friendly else 3))
		bolt.life -= delta
		if not hit.is_empty():
			if bolt.friendly and hit.collider.has_meta("enemy"): _damage_enemy(hit.collider, bolt.damage)
			elif not bolt.friendly and hit.collider == player and player.hit(bolt.damage):
				shake = 0.30
				charge_progress = 0
				sound.play("hit")
			effects.burst(hit.position, Art.TEAL if bolt.friendly else Art.RED, 5)
		if not hit.is_empty() or bolt.life <= 0:
			bolt.node.queue_free()
			projectiles.remove_at(index)
		else: bolt.node.position = target

func _damage_enemy(enemy, damage: float) -> void:
	if enemy.health <= 0: return
	enemy.health -= damage
	enemy.hurt = 1.0
	sound.play("hit")
	if enemy.health <= 0:
		enemy.collision_layer = 0
		kills += 1
		if enemy.kind == "warden":
			boss_defeated = true
			hud.announce("WARDEN DISABLED / HOLD THE EXTRACTION PAD")
		effects.burst(enemy.position, Art.COPPER, 18)
		sound.play("explode")
		enemies.erase(enemy)
		enemy.queue_free()

func _interaction(delta: float, held: bool, moving: float) -> void:
	var active := powered.count(true)
	if active >= 3 or not held or moving > 0.1:
		charge_progress = 0
		return
	var target: Vector3 = World.CAP_POSITIONS[active] if carried < 0 else World.STATION_POSITIONS[active]
	if Vector2(player.position.x - target.x, player.position.z - target.z).length() > interaction_radius:
		charge_progress = 0
		return
	charge_progress += delta
	if charge_progress < (0.55 if carried < 0 else 1.6): return
	charge_progress = 0
	interaction_count += 1
	if carried < 0:
		carried = active
		sound.play("pickup")
		hud.announce("CORE SECURED / MOVEMENT REDUCED")
	else:
		powered[active] = true
		carried = -1
		player.health = minf(100, player.health + 20)
		sound.play("relay")
		effects.pulse(World.STATION_POSITIONS[active], 4, Art.TEAL)
		hud.announce("%s ONLINE / HULL REPAIRED" % World.NAMES[active])
		if active < 2:
			_spawn(Vector3(-2, 0.7, -4 - active * 9), "sentinel")
			_spawn(Vector3(8, 0.7, 6 - active * 14), "sentinel")
		else:
			phase = "extract"
			_spawn(Vector3(10, 0.7, -18), "warden")
			_spawn(Vector3(3, 0.7, -24), "sentinel")
			_spawn(Vector3(15, 0.7, -21), "sentinel")
			player.health = minf(100, player.health + 20)
			hud.announce("WARDEN INBOUND / REACH EXTRACTION")

func context_prompt() -> String:
	if phase == "menu" or paused or phase in ["won", "lost"]: return ""
	if phase == "extract": return "EXTRACTION  %02d / 22 s  |  %s" % [int(extraction), "Warden disabled" if boss_defeated else "Disable the Warden"]
	var active := powered.count(true)
	var target: Vector3 = World.CAP_POSITIONS[active] if carried < 0 else World.STATION_POSITIONS[active]
	if Vector2(player.position.x - target.x, player.position.z - target.z).length() <= interaction_radius:
		return "HOLD E  /  %s  %d%%" % ["SECURE CORE" if carried < 0 else "CONNECT CORE", int(charge_progress / (0.55 if carried < 0 else 1.6) * 100)]
	if player.overheated: return "BLASTER OVERHEATED / USE COVER"
	return "CORE %d SECURED / REACH %s" % [carried + 1, World.NAMES[active]] if carried >= 0 else "Recover the marked power core"

func _update_camera(delta: float) -> void:
	camera_focus = camera_focus.lerp(player.position + Vector3(0, 0, -1), minf(1, delta * 5))
	shake = maxf(0, shake - delta * 2)
	var offset := Vector3(0.58, 0.92, 0.78) * camera_distance
	if shake > 0 and not reduced_motion: offset += Vector3(sin(elapsed * 93), cos(elapsed * 81), 0) * shake
	camera.position = camera_focus + offset
	camera.look_at(camera_focus)
	crosshair.visible = phase in ["play", "extract"] and not autoplay

func rank() -> String:
	if elapsed < 150 and player.damage_taken < 36: return "S"
	if elapsed < 240 and player.damage_taken < 84: return "A"
	return "B"

func restart() -> void:
	for enemy in enemies:
		remove_child(enemy)
		enemy.queue_free()
	enemies.clear()
	for bolt in projectiles: bolt.node.queue_free()
	projectiles.clear()
	effects.clear()
	remove_child(player)
	player.queue_free()
	player = Courier.new()
	add_child(player)
	player.position = World.START
	powered.assign([false, false, false])
	carried = -1
	phase = "menu"
	paused = false
	won = false
	elapsed = 0
	charge_progress = 0
	extraction = 0
	extraction_waves = 0
	kills = 0
	interaction_count = 0
	boss_defeated = false
	next_uid = 1
	_reported = false
	_bot_path_clock = 0
	rng.seed = _seed
	_initial_patrols()
	start_game()

func _load_profile() -> void:
	if not FileAccess.file_exists("user://relay-yard-profile.json"): return
	var text := FileAccess.get_file_as_string("user://relay-yard-profile.json")
	if text.length() > 4096: return
	var profile: Variant = JSON.parse_string(text)
	if not profile is Dictionary: return
	if _number(profile.get("best_time"), 0, 1800): best_time = profile.best_time
	if profile.get("reduced_motion") is bool: reduced_motion = profile.reduced_motion
	if _number(profile.get("volume"), 0, 1): sound.set_volume(profile.volume)
	hud.settings.set_pressed_no_signal(reduced_motion)
	hud.volume.set_value_no_signal(sound.volume)

func save_profile() -> void:
	var path := "user://relay-yard-profile.json"
	var file := FileAccess.open(path + ".tmp", FileAccess.WRITE)
	if file == null: return
	file.store_string(JSON.stringify({"version": 1, "best_time": best_time, "reduced_motion": reduced_motion, "volume": sound.volume}))
	file.close()
	DirAccess.rename_absolute(ProjectSettings.globalize_path(path + ".tmp"), ProjectSettings.globalize_path(path))

func aurum_capture_state() -> Dictionary:
	var actors: Array = []
	var aim_points: Array = []
	for enemy in enemies: actors.append(enemy.snapshot())
	for enemy in enemies:
		var screen := camera.unproject_position(enemy.position)
		aim_points.append({"uid": enemy.uid, "screen": [screen.x, screen.y], "visible": not camera.is_position_behind(enemy.position)})
	var bolts: Array = []
	for bolt in projectiles:
		var at: Vector3 = bolt.node.position
		var velocity: Vector3 = bolt.velocity
		bolts.append({"position": [at.x, at.y, at.z], "velocity": [velocity.x, velocity.y, velocity.z], "life": bolt.life, "friendly": bolt.friendly, "damage": bolt.damage, "owner_uid": bolt.owner_uid})
	return {"version": 3, "phase": phase, "paused": paused, "won": won, "powered": powered.duplicate(), "carried": carried,
		"elapsed": elapsed, "charge_progress": charge_progress, "extraction": extraction, "extraction_waves": extraction_waves, "kills": kills, "interactions": interaction_count,
		"boss_defeated": boss_defeated, "next_uid": next_uid, "rng_state": str(rng.state), "player": player.snapshot(), "position": [player.position.x, player.position.y, player.position.z],
		"enemies": actors, "projectiles": bolts, "view": {"width": get_viewport().get_visible_rect().size.x, "height": get_viewport().get_visible_rect().size.y, "aim_points": aim_points},
		"tuning": {"move_speed": move_speed, "interaction_radius": interaction_radius, "camera_distance": camera_distance, "reduced_motion": reduced_motion, "volume": sound.volume}}

func _number(value: Variant, low: float, high: float) -> bool:
	return (value is int or value is float) and is_finite(float(value)) and float(value) >= low and float(value) <= high

func _integer(value: Variant, low: int, high: int) -> bool:
	return _number(value, low, high) and float(value) == floor(float(value))

func _vector(value: Variant, limit: float) -> bool:
	if not value is Array or value.size() != 3: return false
	for axis in value:
		if not _number(axis, -limit, limit): return false
	return true

func _valid_rng(value: Variant) -> bool:
	if not value is String or value.is_empty(): return false
	var negative: bool = value.begins_with("-")
	var digits: String = value.substr(1) if negative else value
	if digits.is_empty() or digits.length() > 19 or (digits.length() > 1 and digits.begins_with("0")): return false
	for index in digits.length():
		if digits.unicode_at(index) < 48 or digits.unicode_at(index) > 57: return false
	if negative and digits == "0": return false
	var limit := "9223372036854775808" if negative else "9223372036854775807"
	return digits.length() < 19 or digits.casecmp_to(limit) <= 0

func _valid_state(state: Dictionary) -> bool:
	if state.get("version") != 3 or state.get("phase") not in ["menu", "play", "extract", "won", "lost"]: return false
	if not state.get("paused") is bool or not state.get("won") is bool or not state.get("boss_defeated") is bool: return false
	if not state.get("powered") is Array or state.powered.size() != 3: return false
	var false_seen := false
	for value in state.powered:
		if not value is bool: return false
		if not value: false_seen = true
		elif false_seen: return false
	if not _integer(state.get("carried"), -1, 2): return false
	var active: int = state.powered.count(true)
	if state.carried >= 0 and state.carried != active: return false
	if not _integer(state.get("interactions"), 0, 6) or state.interactions != active * 2 + (1 if state.carried >= 0 else 0): return false
	if not _number(state.get("elapsed"), 0, 1800) or not _number(state.get("charge_progress"), 0, 1.6) or not _number(state.get("extraction"), 0, 30): return false
	if state.has("extraction_waves") and not _integer(state.extraction_waves, 0, 3): return false
	if not _integer(state.get("kills"), 0, 1000) or not _integer(state.get("next_uid"), 1, 10000): return false
	if state.won != (state.phase == "won") or (state.phase in ["extract", "won"] and active != 3): return false
	if state.phase == "won" and (not state.boss_defeated or state.extraction < 22): return false
	if not _valid_rng(state.get("rng_state")): return false
	var p: Variant = state.get("player")
	if not p is Dictionary or not _vector(p.get("position"), 31) or not _vector(p.get("velocity"), 100) or not _vector(p.get("facing"), 1.1) or not _vector(p.get("dash_direction"), 1.1): return false
	if absf(p.position[0]) > 22 or not _number(p.position[1], 0.2, 3) or not _number(p.position[2], -30, 26): return false
	for spec in [["health", 0, 100], ["heat", 0, 1], ["dash_cooldown", 0, 1.3], ["emp_cooldown", 0, 7], ["invulnerable", 0, 0.5], ["fire_cooldown", 0, 0.14], ["dash_time", 0, 0.18], ["travelled", 0, 100000], ["damage_taken", 0, 100000]]:
		if not _number(p.get(spec[0]), spec[1], spec[2]): return false
	for key in ["dash_count", "emp_count", "shots_fired"]:
		if not _integer(p.get(key), 0, 100000): return false
	if not p.get("overheated") is bool: return false
	var actors: Variant = state.get("enemies")
	if not actors is Array or actors.size() > 14: return false
	var ids := {}
	for actor in actors:
		if not actor is Dictionary or not _integer(actor.get("uid"), 1, state.next_uid - 1) or ids.has(actor.uid): return false
		ids[actor.uid] = true
		if actor.get("kind") not in ["sentinel", "skirmisher", "warden"]: return false
		for key in ["position", "anchor", "target_at"]:
			if not _vector(actor.get(key), 40): return false
		if not _vector(actor.get("velocity"), 100) or not _number(actor.get("health"), 0.01, 320): return false
		if not _number(actor.get("cooldown"), -1800, 10): return false
		for key in ["charge", "stunned"]:
			if not _number(actor.get(key), -2, 10): return false
		if not _number(actor.get("time"), 0, 1800): return false
	var bolts: Variant = state.get("projectiles")
	if not bolts is Array or bolts.size() > 64: return false
	for bolt in bolts:
		if not bolt is Dictionary or not _vector(bolt.get("position"), 100) or not _vector(bolt.get("velocity"), 40): return false
		if not _number(bolt.get("life"), 0, 1.5) or not _number(bolt.get("damage"), 1, 50) or not bolt.get("friendly") is bool or not _integer(bolt.get("owner_uid"), 0, 10000): return false
	var tuning: Variant = state.get("tuning")
	if not tuning is Dictionary or not _number(tuning.get("move_speed"), 3, 12) or not _number(tuning.get("interaction_radius"), 1.4, 3.4) or not _number(tuning.get("camera_distance"), 12, 25) or not tuning.get("reduced_motion") is bool: return false
	if tuning.has("volume") and not _number(tuning.volume, 0, 1): return false
	return JSON.stringify(state).to_utf8_buffer().size() <= 65536

func aurum_restore_state(input: Dictionary) -> bool:
	# Earlier fixture checkpoints cannot describe this mission. Refuse them explicitly.
	if not _valid_state(input): return false
	var state := input.duplicate(true)
	for enemy in enemies:
		remove_child(enemy)
		enemy.queue_free()
	enemies.clear()
	for bolt in projectiles: bolt.node.queue_free()
	projectiles.clear()
	effects.clear()
	player.restore(state.player)
	powered.assign(state.powered)
	for key in ["phase", "paused", "won", "carried", "elapsed", "charge_progress", "extraction", "kills", "interaction_count", "boss_defeated", "next_uid"]: set(key, state.interactions if key == "interaction_count" else state[key])
	extraction_waves = int(state.get("extraction_waves", 0))
	for key in ["move_speed", "interaction_radius", "camera_distance", "reduced_motion"]: set(key, state.tuning[key])
	for actor in state.enemies:
		var enemy = _spawn(Vector3(actor.position[0], actor.position[1], actor.position[2]), actor.kind, actor.uid)
		enemy.restore(actor)
	for bolt in state.projectiles:
		_bolt(Vector3(bolt.position[0], bolt.position[1], bolt.position[2]), Vector3(bolt.velocity[0], bolt.velocity[1], bolt.velocity[2]), bolt.friendly, bolt.damage, bolt.owner_uid)
		projectiles.back().life = bolt.life
	rng.state = int(state.rng_state)
	_bot_path_clock = 0
	camera_focus = player.position
	world.update_state(powered, carried, powered.count(true), 0, reduced_motion)
	player.cargo.visible = carried >= 0
	sound.set_volume(state.tuning.get("volume", sound.volume))
	if phase in ["play", "extract"]: sound.start_music()
	hud.update(0)
	return true

func _write_report(ok: bool) -> void:
	if _reported or report_path.is_empty(): return
	_reported = true
	var file := FileAccess.open(report_path, FileAccess.WRITE)
	if file != null:
		file.store_string(JSON.stringify({"ok": ok, "won": won, "phase": phase, "powered": powered.count(true), "interactions": interaction_count, "kills": kills, "boss_defeated": boss_defeated, "extraction": extraction, "extraction_waves": extraction_waves,
			"distance": player.travelled, "shots": player.shots_fired, "damage_taken": player.damage_taken, "dash_count": player.dash_count, "emp_count": player.emp_count, "audio_events": sound.event_count, "seed": _seed, "state": aurum_capture_state()}))
		file.close()

func _finish_run(code: int) -> void:
	sound.shutdown()
	get_tree().quit.call_deferred(code)
