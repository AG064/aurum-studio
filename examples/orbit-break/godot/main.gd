extends Node3D

const HudScript = preload("res://hud.gd")
const RADIUS = 15.3
const CYAN = Color("69f4de")
const GOLD = Color("ffd080")
const RED = Color("ff697c")
const PURPLE = Color("bd93ff")
const SAVE_PATH = "user://record.cfg"
const UPGRADES = ["OVERDRIVE", "REINFORCE", "SPLIT SHOT"]
const WEAPONS = ["PULSE", "LANCE", "ARC"]
const UPGRADE_COSTS = [45, 45, 60]

var phase = "menu"
var wave = 0
var kills = 0
var score = 0
var best = 0
var health = 100.0
var max_health = 100.0
var damage = 16.0
var fire_period = 0.28
var speed = 7.6
var split_shot = false
var weapon = 0
var credits = 0
var purchased: Array[int] = []
var turret: Node3D
var turret_timer = 0.0
var workshop_visits = 0
var boss_phases_seen = 0
var hazards: Array[Dictionary] = []
var wave_quota = 0
var wave_spawned = 0
var wave_kills = 0
var run_time = 0.0
var spawn_timer = 0.0
var fire_timer = 0.0
var dash_cooldown = 0.0
var dash_time = 0.0
var dashes_run = 0
var hurt_time = 0.0
var aim = Vector3.FORWARD
var move = Vector2.ZERO
var touch_origin = Vector2.ZERO
var touch_position = Vector2.ZERO
var touch_id = -1
var touch_mode = false
var muted = false
var shake = 0.0
var flash = 0.0
var banner = ""
var banner_time = 0.0
var enemies: Array[Dictionary] = []
var shots: Array[Dictionary] = []
var pickups: Array[Dictionary] = []
var effects: Array[Dictionary] = []
var rng = RandomNumberGenerator.new()
var player: Node3D
var camera: Camera3D
var reactor: MeshInstance3D
var hud: Control
var entities: Node3D
var audio: Array[AudioStreamPlayer] = []
var tones: Dictionary = {}
var test_mode = false
var auto_pilot = false
var idle_test = false
var tuning_hash = ""
var tuning_clock = 0.0
var hot_reload_count = 0
var base_spawn_interval = 1.1
var damage_multiplier = 1.0
var web_window
var web_tuning_callback
var web_revision = ""
var web_state_clock = 0.0

func _ready() -> void:
	test_mode = "--acceptance" in OS.get_cmdline_user_args()
	auto_pilot = "--autoplay" in OS.get_cmdline_user_args()
	idle_test = "--idle-test" in OS.get_cmdline_user_args()
	auto_pilot = auto_pilot or idle_test
	var weapon_argument = OS.get_cmdline_user_args().find("--weapon")
	if weapon_argument >= 0 and weapon_argument+1 < OS.get_cmdline_user_args().size():
		weapon = clampi(int(OS.get_cmdline_user_args()[weapon_argument+1]),0,2)
	touch_mode = DisplayServer.is_touchscreen_available() or "--touch" in OS.get_cmdline_user_args()
	rng.seed = 71337 if test_mode or auto_pilot else Time.get_ticks_usec()
	_setup_input()
	_build_world()
	_build_audio()
	_load_record()
	_reload_tuning()
	_setup_web_bridge()
	var layer = CanvasLayer.new()
	add_child(layer)
	hud = HudScript.new()
	hud.game = self
	layer.add_child(hud)
	hud.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	hud.refresh()
	if auto_pilot:
		start_run()
	if test_mode:
		set_physics_process(false)
	print("ORBIT_READY phase=", phase, " touch=", touch_mode)

func _setup_input() -> void:
	var bindings = {"left":[KEY_A, KEY_LEFT], "right":[KEY_D, KEY_RIGHT], "up":[KEY_W, KEY_UP], "down":[KEY_S, KEY_DOWN], "dash":[KEY_SPACE], "pause":[KEY_ESCAPE], "confirm":[KEY_ENTER]}
	for action in bindings:
		if not InputMap.has_action(action):
			InputMap.add_action(action)
		for code in bindings[action]:
			var event = InputEventKey.new()
			event.physical_keycode = code
			InputMap.action_add_event(action, event)
			var logical_event = InputEventKey.new()
			logical_event.keycode = code
			InputMap.action_add_event(action, logical_event)

func _material(color: Color, glow = false) -> StandardMaterial3D:
	var material = StandardMaterial3D.new()
	material.albedo_color = color
	material.roughness = 0.6
	if glow:
		material.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	return material

func _mesh(parent: Node3D, shape: Mesh, color: Color, pos: Vector3, glow = false) -> MeshInstance3D:
	var instance = MeshInstance3D.new()
	instance.mesh = shape
	instance.material_override = _material(color, glow)
	parent.add_child(instance)
	instance.position = pos
	return instance

func _box(parent: Node3D, size: Vector3, color: Color, pos: Vector3, glow = false) -> MeshInstance3D:
	var shape = BoxMesh.new()
	shape.size = size
	return _mesh(parent, shape, color, pos, glow)

func _sphere(parent: Node3D, radius: float, color: Color, pos: Vector3, glow = false) -> MeshInstance3D:
	var shape = SphereMesh.new()
	shape.radius = radius
	shape.height = radius * 2.0
	shape.radial_segments = 12
	shape.rings = 6
	return _mesh(parent, shape, color, pos, glow)

func _ring(parent: Node3D, radius: float, width: float, color: Color, y: float) -> MeshInstance3D:
	var shape = TorusMesh.new()
	shape.inner_radius = radius - width
	shape.outer_radius = radius
	shape.rings = 64
	shape.ring_segments = 8
	return _mesh(parent, shape, color, Vector3(0,y,0), true)

func _build_world() -> void:
	var world = WorldEnvironment.new()
	var env = Environment.new()
	env.background_mode = Environment.BG_COLOR
	env.background_color = Color("060e20")
	env.ambient_light_source = Environment.AMBIENT_SOURCE_COLOR
	env.ambient_light_color = Color("7996bc")
	env.ambient_light_energy = 0.75
	world.environment = env
	add_child(world)
	var light = DirectionalLight3D.new()
	light.rotation_degrees = Vector3(-55,-30,0)
	light.light_color = Color("c7d9ff")
	light.light_energy = 1.1
	add_child(light)
	camera = Camera3D.new()
	camera.projection = Camera3D.PROJECTION_ORTHOGONAL
	camera.size = 36
	camera.position = Vector3(0,32,24)
	add_child(camera)
	camera.look_at(Vector3.ZERO)
	camera.current = true
	var pad = CylinderMesh.new()
	pad.top_radius = 16.2
	pad.bottom_radius = 16.8
	pad.height = 0.8
	pad.radial_segments = 96
	_mesh(self, pad, Color("14283b"), Vector3(0,-0.5,0))
	_ring(self, 16.3, 0.09, CYAN * 0.65, 0.01)
	_ring(self, 15.3, 0.035, Color("31566b"), 0.01)
	for x in range(-14,15,2):
		var length = sqrt(15.3*15.3-float(x*x)) * 2.0
		_box(self, Vector3(0.025,0.015,length), Color("203b4d"), Vector3(x,-0.085,0), true)
		_box(self, Vector3(length,0.015,0.025), Color("203b4d"), Vector3(0,-0.085,x), true)
	_ring(self, 3.0, 0.05, Color("557d8c"), 0.01)
	reactor = _ring(self, 1.8, 0.22, GOLD * 0.65, 0.08)
	for n in range(12):
		var angle = n * TAU / 12.0
		var pos = Vector3(cos(angle),0,sin(angle))*17.1
		_box(self, Vector3(0.8,0.8,0.8), Color("253953"), pos)
		_box(self, Vector3(0.6,0.08,0.6), GOLD if n % 3 == 0 else CYAN, pos+Vector3(0,0.45,0),true)
		var support = _box(self,Vector3(1.2,0.25,2.7),Color("1b3245"),pos*0.87-Vector3.UP*0.14)
		support.rotation.y = -angle+PI/2.0
		var marking = _box(self,Vector3(0.055,0.02,1.4),GOLD*0.55, pos*0.8)
		marking.rotation.y = -angle+PI/2.0
	for n in range(65):
		var pos = Vector3(rng.randf_range(-45,45),rng.randf_range(-8,-4),rng.randf_range(-40,30))
		_sphere(self, rng.randf_range(0.025,0.07), Color("506f9c"),pos,true)
	entities = Node3D.new()
	entities.name = "Entities"
	add_child(entities)
	player = Node3D.new()
	player.name = "Pilot"
	add_child(player)
	var hull = _sphere(player,0.48,CYAN,Vector3(0,0.65,0))
	hull.scale = Vector3(0.8,0.6,1.3)
	var nose = PrismMesh.new()
	nose.size = Vector3(0.7,0.3,1.0)
	var prow = _mesh(player,nose,Color("d9e7e9"),Vector3(0,0.52,-0.45))
	prow.rotation_degrees.x = -90
	_box(player,Vector3(1.3,0.14,0.65),Color("277b85"),Vector3(0,0.5,0.2))
	_box(player,Vector3(0.2,0.15,0.8),GOLD,Vector3(-0.65,0.5,0.2),true)
	_box(player,Vector3(0.2,0.15,0.8),GOLD,Vector3(0.65,0.5,0.2),true)
	_sphere(player,0.13,Color.WHITE,Vector3(0,0.7,-0.45),true)
	_ring(player,0.85,0.05,CYAN * 0.65,0.02)

func _build_audio() -> void:
	for spec in [["shoot",680.0,0.045],["hit",150.0,0.09],["dash",280.0,0.14],["pickup",950.0,0.15],["wave",440.0,0.4],["fail",85.0,0.6]]:
		var rate = 22050
		var data = PackedByteArray()
		var count = int(rate * spec[2])
		data.resize(count*2)
		for i in range(count):
			var t = float(i)/rate
			var envelope = pow(1.0-float(i)/count,2.0) * minf(t*150.0,1.0)
			var sample = sin(t*TAU*spec[1]*(1.0-0.35*t/spec[2])) * envelope * 0.18
			data.encode_s16(i*2,int(sample*32767.0))
		var stream = AudioStreamWAV.new()
		stream.format = AudioStreamWAV.FORMAT_16_BITS
		stream.mix_rate = rate
		stream.data = data
		tones[spec[0]] = stream
	for i in range(8):
		var voice = AudioStreamPlayer.new()
		add_child(voice)
		audio.append(voice)

func _sound(id: String) -> void:
	if muted or test_mode:
		return
	for voice in audio:
		if not voice.playing:
			voice.stream = tones[id]
			voice.play()
			return

func _load_record() -> void:
	if test_mode:
		return
	var config = ConfigFile.new()
	if config.load(SAVE_PATH) == OK:
		best = maxi(0,int(config.get_value("record","best",0)))
		muted = bool(config.get_value("settings","muted",false))

func _save_record() -> void:
	if test_mode:
		return
	var config = ConfigFile.new()
	config.set_value("record","best",best)
	config.set_value("settings","muted",muted)
	var error = config.save(SAVE_PATH)
	if error != OK:
		push_warning("Could not save local record: " + str(error))

func _reload_tuning() -> void:
	var path = "res://tuning.json"
	if not FileAccess.file_exists(path):
		return
	var text = FileAccess.get_file_as_string(path)
	var fingerprint = str(text.hash())
	if fingerprint == tuning_hash:
		return
	var data = JSON.parse_string(text)
	if not data is Dictionary:
		return
	var next_speed = data.get("player_speed",7.6)
	var next_spawn = data.get("spawn_interval",1.1)
	var next_damage = data.get("damage_multiplier",1.0)
	if not (next_speed is float or next_speed is int) or not (next_spawn is float or next_spawn is int) or not (next_damage is float or next_damage is int):
		return
	if not is_finite(float(next_speed)) or not is_finite(float(next_spawn)) or not is_finite(float(next_damage)):
		return
	speed = clampf(float(next_speed),3.0,14.0)
	base_spawn_interval = clampf(float(next_spawn),0.3,3.0)
	damage_multiplier = clampf(float(next_damage),0.25,4.0)
	if not tuning_hash.is_empty():
		hot_reload_count += 1
		banner = "TUNING UPDATED LIVE"
		banner_time = 2.0
	tuning_hash = fingerprint

func _setup_web_bridge() -> void:
	if not OS.has_feature("web"):
		return
	web_window = JavaScriptBridge.get_interface("window")
	web_tuning_callback = JavaScriptBridge.create_callback(_receive_web_tuning)
	web_window.aurumApplyTuning = web_tuning_callback

func _receive_web_tuning(arguments: Array) -> void:
	if arguments.is_empty():
		return
	var payload = JSON.parse_string(str(arguments[0]))
	if not payload is Dictionary or not payload.get("values") is Dictionary:
		return
	var values: Dictionary = payload.values
	for field in ["player_speed","spawn_interval","damage_multiplier"]:
		var value = values.get(field,damage_multiplier if field=="damage_multiplier" else (speed if field=="player_speed" else base_spawn_interval))
		if not (value is int or value is float) or not is_finite(float(value)):
			web_window.aurumTuningError = "Invalid numeric tuning: " + field
			return
	speed = clampf(float(values.get("player_speed",speed)),3.0,14.0)
	base_spawn_interval = clampf(float(values.get("spawn_interval",base_spawn_interval)),0.3,3.0)
	damage_multiplier = clampf(float(values.get("damage_multiplier",damage_multiplier)),0.25,4.0)
	if not web_revision.is_empty() and web_revision != str(payload.get("sha256","")):
		hot_reload_count += 1
	web_revision = str(payload.get("sha256",""))
	web_window.aurumAppliedTuning = web_revision
	web_window.aurumTuningError = ""

func _publish_web_state(delta: float) -> void:
	if web_window == null:
		return
	web_state_clock += delta
	if web_state_clock < 0.2:
		return
	web_state_clock = 0.0
	web_window.aurumState = JSON.stringify({"phase":phase,"wave":wave,"health":health,"kills":kills,"time":run_time,"x":player.position.x,"z":player.position.z,"speed":speed,"damage_multiplier":damage_multiplier,"hot_reloads":hot_reload_count,"revision":web_revision,"dash_cooldown":dash_cooldown,"dashes":dashes_run,"credits":credits,"weapon":WEAPONS[weapon],"workshops":workshop_visits})

func start_run() -> void:
	_clear_entities()
	wave = 0
	kills = 0
	score = 0
	health = 100.0
	max_health = 100.0
	damage = 16.0
	fire_period = 0.28
	split_shot = false
	credits = 0
	purchased.clear()
	workshop_visits = 0
	boss_phases_seen = 0
	turret_timer = 0.0
	run_time = 0.0
	dash_cooldown = 0.0
	dash_time = 0.0
	dashes_run = 0
	hurt_time = 0.0
	fire_timer = 0.0
	player.position = Vector3(0,0,7)
	aim = Vector3.FORWARD
	move = Vector2.ZERO
	touch_id = -1
	_next_wave()
	print("ORBIT_RUN_STARTED")

func _clear_entities() -> void:
	for node in entities.get_children():
		node.free()
	enemies.clear()
	shots.clear()
	pickups.clear()
	effects.clear()
	hazards.clear()
	turret = null

func _next_wave() -> void:
	wave += 1
	phase = "playing"
	wave_spawned = 0
	wave_kills = 0
	wave_quota = 7 + wave * 3 if wave < 5 else 1
	spawn_timer = 1.0
	banner = "WAVE %02d / 05" % wave if wave < 5 else "FINAL CONTACT: THE WARDEN"
	banner_time = 3.0
	_sound("wave")
	hud.refresh()

func select_weapon(index: int) -> void:
	if phase not in ["menu", "upgrade"] or index < 0 or index >= WEAPONS.size():
		return
	weapon = index
	hud.refresh()

func shot_power(base: float, flat: float, additive: float, multiplier: float) -> float:
	return (base + flat) * (1.0 + additive) * multiplier

func continue_run() -> void:
	if phase == "upgrade":
		_next_wave()

func choose_upgrade(index: int) -> void:
	if phase != "upgrade" or index < 0 or index > 2 or index in purchased or credits < UPGRADE_COSTS[index]:
		return
	credits -= UPGRADE_COSTS[index]
	purchased.append(index)
	if index == 0:
		fire_period = maxf(0.09,fire_period*0.8)
		damage += 5.0
	elif index == 1:
		max_health += 25.0
		health = minf(max_health,health+25.0)
	else:
		split_shot = true
		damage += 4.0
	hud.refresh()
	_sound("pickup")

func buy_repair() -> void:
	if phase != "upgrade" or credits < 30 or health >= max_health:
		return
	credits -= 30
	health = minf(max_health,health+45.0)
	hud.refresh()

func buy_turret() -> void:
	if phase != "upgrade" or credits < 80 or is_instance_valid(turret):
		return
	credits -= 80
	turret = Node3D.new()
	entities.add_child(turret)
	_box(turret,Vector3(1.0,0.6,1.0),Color("385a66"),Vector3.UP*0.3)
	_box(turret,Vector3(0.24,0.22,1.6),CYAN,Vector3.UP*0.7,true)
	_ring(turret,1.1,0.06,CYAN,0.04)
	hud.refresh()

func pause_game() -> void:
	if phase == "playing":
		phase = "paused"
		touch_id = -1
	elif phase == "paused":
		phase = "playing"
	hud.refresh()

func dash() -> void:
	if phase != "playing" or dash_cooldown > 0.0:
		return
	dashes_run += 1
	dash_cooldown = 2.2
	dash_time = 0.18
	hurt_time = maxf(hurt_time,0.28)
	_sound("dash")
	_burst(player.position+Vector3.UP*0.3,CYAN,1.0)

func _input(event: InputEvent) -> void:
	if not event is InputEventKey:
		return
	if event.echo:
		return
	if event.is_action_pressed("pause"):
		pause_game()
		get_viewport().set_input_as_handled()
	elif event.is_action_pressed("dash"):
		dash()
		get_viewport().set_input_as_handled()
	elif event.is_action_pressed("confirm") and phase in ["menu","won","lost"]:
		start_run()
		get_viewport().set_input_as_handled()
	elif event.is_action_pressed("confirm") and phase == "upgrade":
		continue_run()
		get_viewport().set_input_as_handled()
	elif event is InputEventKey and event.pressed and not event.echo:
		var key = event.keycode if event.keycode != 0 else event.physical_keycode
		if key == 0:
			key = event.unicode
		if key == KEY_M or key == 109:
			toggle_mute()
		elif phase == "upgrade" and key in [KEY_1,KEY_2,KEY_3]:
			choose_upgrade(key-KEY_1)
		elif phase == "upgrade" and key == KEY_R:
			buy_repair()
		elif phase == "upgrade" and key == KEY_T:
			buy_turret()
		elif phase in ["menu","upgrade"] and key == KEY_Q:
			select_weapon((weapon+1)%WEAPONS.size())

func _unhandled_input(event: InputEvent) -> void:
	if event is InputEventScreenTouch:
		if not touch_mode:
			touch_mode = true
			hud.refresh()
		if event.pressed and event.position.x < get_viewport().get_visible_rect().size.x*0.55 and touch_id == -1 and phase == "playing":
			touch_id = event.index
			touch_origin = event.position
			touch_position = event.position
		elif not event.pressed and event.index == touch_id:
			touch_id = -1
	elif event is InputEventScreenDrag and event.index == touch_id:
		touch_position = event.position

func toggle_mute() -> void:
	muted = not muted
	if muted:
		for voice in audio:
			voice.stop()
	_save_record()
	hud.refresh()

func _notification(what: int) -> void:
	if what == NOTIFICATION_APPLICATION_FOCUS_OUT and phase == "playing" and not test_mode and not auto_pilot:
		pause_game()

func _physics_process(delta: float) -> void:
	_publish_web_state(delta)
	tuning_clock += delta
	if tuning_clock > 0.5:
		tuning_clock = 0.0
		_reload_tuning()
	reactor.rotation.y += delta*0.5
	if auto_pilot and phase == "upgrade":
		choose_upgrade([2,1,0,0][wave-1])
		buy_repair()
		buy_turret()
		continue_run()
	if phase == "playing":
		move = Input.get_vector("left","right","up","down")
		if touch_id >= 0:
			move = ((touch_position-touch_origin)/65.0).limit_length()
		if auto_pilot and not idle_test:
			var destination = Vector3(cos(run_time*0.25)*10.0,0,sin(run_time*0.25)*10.0)
			move = Vector2(destination.x-player.position.x,destination.z-player.position.z).normalized()
			if enemies.any(func(e): return e.node.position.distance_to(player.position)<3.0):
				dash()
		_tick(delta)
	_tick_effects(delta)
	flash = maxf(0.0,flash-delta*3.0)
	shake = maxf(0.0,shake-delta*4.0)
	if not test_mode:
		camera.h_offset = sin(Time.get_ticks_msec()*0.077)*shake*0.12
		camera.v_offset = cos(Time.get_ticks_msec()*0.093)*shake*0.12
	if hud:
		hud.queue_redraw()

func _tick(delta: float) -> void:
	if phase != "playing":
		return
	run_time += delta
	banner_time = maxf(0.0,banner_time-delta)
	dash_cooldown = maxf(0.0,dash_cooldown-delta)
	dash_time = maxf(0.0,dash_time-delta)
	hurt_time = maxf(0.0,hurt_time-delta)
	var direction = Vector3(move.x,0,move.y)
	if dash_time > 0.0 and direction.length_squared()<0.1:
		direction = aim
	player.position += direction * speed * (3.8 if dash_time>0.0 else 1.0) * delta
	player.position = player.position.limit_length(RADIUS)
	player.visible = hurt_time <= 0.0 or int(hurt_time*20.0)%2 == 0
	var target = _nearest_enemy()
	if not target.is_empty():
		aim = (target.node.position-player.position).normalized()
	if Input.is_mouse_button_pressed(MOUSE_BUTTON_LEFT) and not touch_mode and not test_mode:
		var mouse = get_viewport().get_mouse_position()
		var hit = Plane(Vector3.UP,0.5).intersects_ray(camera.project_ray_origin(mouse),camera.project_ray_normal(mouse))
		if hit is Vector3:
			aim = (hit-player.position).normalized()
	aim.y = 0.0
	if aim.length_squared()>0.01:
		player.rotation.y = atan2(-aim.x,-aim.z)
	fire_timer -= delta
	if fire_timer <= 0.0 and (not target.is_empty() or Input.is_mouse_button_pressed(MOUSE_BUTTON_LEFT)):
		fire_timer = fire_period * [1.0,2.1,2.6][weapon]
		_fire_weapon()
		_sound("shoot")
	spawn_timer -= delta
	if spawn_timer <= 0.0 and wave_spawned < wave_quota:
		spawn_timer = maxf(0.45,base_spawn_interval-wave*0.1)
		var kind = "warden" if wave == 5 else ("brute" if wave >= 3 and wave_spawned%4==0 else ("spitter" if wave>=2 and wave_spawned%3==0 else "seeker"))
		_spawn_enemy(kind)
		wave_spawned += 1
	_tick_enemies(delta)
	_tick_hazards(delta)
	_tick_turret(delta)
	_tick_shots(delta)
	_tick_pickups(delta)
	if phase == "playing" and wave_spawned >= wave_quota and enemies.is_empty():
		for shot in shots:
			shot.node.queue_free()
		shots.clear()
		score += wave*100
		if wave == 5:
			_finish(true)
		else:
			phase = "upgrade"
			credits += 25 + wave*5
			purchased.clear()
			workshop_visits += 1
			for hazard in hazards:
				hazard.node.queue_free()
			hazards.clear()
			player.visible = true
			touch_id = -1
			hud.refresh()

func _fire_weapon() -> void:
	var origin = player.position + Vector3.UP*0.55
	var power = shot_power(16.0,damage-16.0,0.0,damage_multiplier)
	if weapon == 2:
		# Arc chains by proximity from the previous hit, with diminishing damage.
		var cursor = player.position
		var hit: Array[Dictionary] = []
		for hop in range(5 if split_shot else 3):
			var nearest: Dictionary = {}
			var distance = 10.0 if hop == 0 else 6.0
			for enemy in enemies:
				var d = enemy.node.position.distance_to(cursor)
				if enemy not in hit and d < distance:
					distance = d
					nearest = enemy
			if nearest.is_empty():
				break
			hit.append(nearest)
			var destination: Vector3 = nearest.node.position
			_beam(cursor+Vector3.UP*0.6,destination+Vector3.UP*0.6,CYAN)
			nearest.hp -= power*2.2*pow(0.8,hop)
			cursor = destination
		for i in range(enemies.size()-1,-1,-1):
			if enemies[i].hp <= 0:
				_kill_enemy(i)
	else:
		var lance = weapon == 1
		_fire(origin,aim,false,power*(2.8 if lance else 1.0),3 if lance else 1)
		if split_shot:
			for angle in [-0.15,0.15]:
				_fire(origin,aim.rotated(Vector3.UP,angle),false,power*0.65*(2.8 if lance else 1.0),3 if lance else 1)

func _beam(from: Vector3, to: Vector3, color: Color) -> void:
	if effects.size() > 60:
		return
	var node = _box(entities,Vector3(0.07,0.07,maxf(0.01,from.distance_to(to))),color,(from+to)*0.5,true)
	if from.distance_squared_to(to) > 0.01:
		node.look_at(to)
	effects.append({"node":node,"life":0.16,"radius":0.0})

func _tick_turret(delta: float) -> void:
	if not is_instance_valid(turret):
		return
	turret_timer -= delta
	if turret_timer > 0.0:
		return
	var target: Dictionary = {}
	var distance = 12.0
	for enemy in enemies:
		var d = enemy.node.position.length()
		if d < distance:
			distance = d
			target = enemy
	if not target.is_empty():
		turret_timer = 0.4
		var direction: Vector3 = target.node.position.normalized()
		turret.rotation.y = atan2(-direction.x,-direction.z)
		_fire(Vector3.UP*0.55,direction,false,12.0*damage_multiplier)

func _warn_strike(pos: Vector3) -> void:
	if hazards.size() >= 8:
		return
	var node = _ring(entities,2.6,0.09,RED,0.05)
	node.position = Vector3(pos.x,0.05,pos.z)
	hazards.append({"node":node,"life":1.4,"radius":2.6})

func _tick_hazards(delta: float) -> void:
	for i in range(hazards.size()-1,-1,-1):
		var hazard = hazards[i]
		hazard.life -= delta
		hazard.node.scale = Vector3.ONE*(0.6+0.4*clampf(hazard.life/1.4,0.0,1.0))
		if hazard.life <= 0:
			var pos: Vector3 = hazard.node.position
			_burst(pos,RED,2.6)
			if Vector2(player.position.x-pos.x,player.position.z-pos.z).length() < hazard.radius:
				_hurt(28.0)
			hazard.node.queue_free()
			hazards.remove_at(i)

func _nearest_enemy() -> Dictionary:
	var target: Dictionary = {}
	var distance = INF
	for enemy in enemies:
		var d = enemy.node.position.distance_squared_to(player.position)
		if d < distance:
			distance = d
			target = enemy
	return target

func _spawn_enemy(kind: String, position_override = Vector3.INF) -> Dictionary:
	var node = Node3D.new()
	entities.add_child(node)
	var angle = rng.randf()*TAU
	node.position = Vector3(cos(angle)*14.9,0,sin(angle)*14.9) if position_override == Vector3.INF else position_override
	if position_override == Vector3.INF and node.position.distance_to(player.position)<6.0:
		node.position = -node.position
	var boss = kind == "warden"
	var radius = 1.6 if boss else (0.85 if kind == "brute" else 0.5)
	var color = GOLD if boss else (PURPLE if kind == "spitter" else RED)
	var core = _sphere(node,radius,color,Vector3(0,radius,0))
	core.scale.y = 0.7
	if kind == "spitter":
		for side in [-1,1]:
			_box(node,Vector3(0.2,0.4,1.1),PURPLE,Vector3(side*0.7,0.5,0))
	elif kind == "seeker":
		var fin = PrismMesh.new()
		fin.size = Vector3(1.5,0.3,0.8)
		_mesh(node,fin,Color("aa4255"),Vector3(0,0.4,0.15))
	_ring(node,radius*1.2,0.09,color,0.04)
	if kind in ["brute","warden"]:
		_box(node,Vector3(radius*2.5,0.25,radius*0.7),Color("693d50"),Vector3(0,radius,0))
	var hp = 4800.0 if boss else (150.0 if kind == "brute" else (70.0 if kind == "spitter" else 42.0))
	var warning = _ring(node,radius*1.6,0.045,color,0.05)
	warning.visible = false
	var enemy = {"node":node,"core":core,"warning":warning,"kind":kind,"hp":hp,"max_hp":hp,"radius":radius,"fire":1.5,"age":0.0,"stage":1,"strike":3.0,"speed":1.45 if boss else (1.8 if kind=="brute" else 2.7+wave*0.15)}
	enemies.append(enemy)
	_burst(node.position+Vector3.UP*0.3,color,1.4)
	return enemy

func _tick_enemies(delta: float) -> void:
	for enemy in enemies:
		enemy.age += delta
		var delta_pos: Vector3 = player.position-enemy.node.position
		var distance = delta_pos.length()
		var direction = delta_pos.normalized()
		if enemy.kind != "spitter" or distance > 7.0:
			enemy.node.position += direction*enemy.speed*delta
		elif distance < 4.5:
			enemy.node.position -= direction*enemy.speed*delta
		enemy.node.position = enemy.node.position.limit_length(15.0)
		enemy.core.rotation.y += delta*1.2
		if enemy.kind != "warden" and direction.length_squared()>0.01:
			enemy.node.rotation.y = atan2(-direction.x,-direction.z)
		if distance < enemy.radius+0.55:
			_hurt(22.0 if enemy.kind=="brute" else 14.0)
			enemy.node.position -= direction*1.2
		enemy.fire -= delta
		enemy.warning.visible = enemy.kind in ["spitter","warden"] and enemy.fire < 0.65
		enemy.warning.scale = Vector3.ONE*(1.0+maxf(0.0,0.65-enemy.fire))
		if enemy.kind == "warden":
			var stage = 3 if enemy.hp/enemy.max_hp < 0.33 else (2 if enemy.hp/enemy.max_hp < 0.67 else 1)
			boss_phases_seen = maxi(boss_phases_seen,stage)
			if stage != enemy.stage:
				enemy.stage = stage
				banner = "WARDEN / " + ["CONTAINMENT","PURSUIT","OVERLOAD"][stage-1]
				banner_time = 2.5
				enemy.speed = 1.45 + (stage-1)*0.45
			if stage >= 2:
				enemy.strike -= delta
				if enemy.strike <= 0.0:
					enemy.strike = 2.8 if stage == 2 else 1.8
					_warn_strike(player.position)
		if enemy.age > 0.8 and enemy.fire <= 0.0 and enemy.kind in ["spitter","warden"]:
			enemy.fire = 1.6 if enemy.kind=="spitter" else 1.8-enemy.stage*0.15
			_fire(enemy.node.position+Vector3.UP*0.55,direction,true,12.0)
			if enemy.kind == "warden":
				var count = 8+enemy.stage*2
				for i in range(count):
					_fire(enemy.node.position+Vector3.UP*0.55,Vector3.FORWARD.rotated(Vector3.UP,i*TAU/count+enemy.age*0.4),true,10.0)

func _fire(pos: Vector3, direction: Vector3, hostile: bool, power: float, pierce = 1) -> void:
	if shots.size()>=240:
		return
	var node = _sphere(entities,0.18 if hostile else 0.11,RED if hostile else GOLD,pos,true)
	if pierce > 1:
		node.scale = Vector3(0.8,0.8,3.5)
		if direction.length_squared()>0.01:
			node.look_at(pos+direction)
	shots.append({"node":node,"velocity":direction.normalized()*(6.0 if hostile else 28.0),"hostile":hostile,"power":power,"life":4.0,"pierce":pierce,"hit":[]})

func _tick_shots(delta: float) -> void:
	for i in range(shots.size()-1,-1,-1):
		var shot = shots[i]
		shot.node.position += shot.velocity*delta
		shot.life -= delta
		var remove = shot.life<=0.0 or Vector2(shot.node.position.x,shot.node.position.z).length()>19.0
		if shot.hostile:
			if Vector2(shot.node.position.x-player.position.x,shot.node.position.z-player.position.z).length()<0.6:
				_hurt(shot.power)
				remove = true
		else:
			for j in range(enemies.size()-1,-1,-1):
				var enemy = enemies[j]
				if enemy.node.get_instance_id() not in shot.hit and Vector2(shot.node.position.x-enemy.node.position.x,shot.node.position.z-enemy.node.position.z).length()<enemy.radius+0.2:
					enemy.hp -= shot.power
					shot.hit.append(enemy.node.get_instance_id())
					shot.pierce -= 1
					remove = shot.pierce <= 0
					if enemy.hp<=0.0:
						_kill_enemy(j)
					break
		if remove:
			shot.node.queue_free()
			shots.remove_at(i)

func _kill_enemy(index: int) -> void:
	var enemy = enemies[index]
	var pos: Vector3 = enemy.node.position
	_burst(pos+Vector3.UP*0.5,GOLD if enemy.kind=="warden" else RED,enemy.radius*1.8)
	if kills%4==3 and pickups.size()<15:
		var node = _sphere(entities,0.28,CYAN,pos+Vector3.UP*0.5,true)
		pickups.append({"node":node,"life":14.0})
	enemy.node.queue_free()
	enemies.remove_at(index)
	kills += 1
	credits += 30 if enemy.kind=="warden" else (12 if enemy.kind=="brute" else 8)
	wave_kills += 1
	score += 250 if enemy.kind=="warden" else (35 if enemy.kind=="brute" else 20)
	_sound("hit")

func _tick_pickups(delta: float) -> void:
	for i in range(pickups.size()-1,-1,-1):
		var item = pickups[i]
		item.life -= delta
		var delta_pos = player.position+Vector3.UP*0.5-item.node.position
		if delta_pos.length()<4.0:
			item.node.position += delta_pos.normalized()*delta*8.0
		if delta_pos.length()<0.8:
			health = minf(max_health,health+10.0)
			score += 10
			item.life = 0.0
			_sound("pickup")
		if item.life<=0.0:
			item.node.queue_free()
			pickups.remove_at(i)

func _hurt(amount: float) -> void:
	if hurt_time>0.0 or phase!="playing":
		return
	health = maxf(0.0,health-amount)
	hurt_time = 0.65
	flash = 0.8
	shake = 1.0
	_sound("hit")
	if health<=0.0:
		_finish(false)

func _finish(won: bool) -> void:
	phase = "won" if won else "lost"
	player.visible = true
	touch_id = -1
	best = maxi(best,score)
	_save_record()
	_sound("wave" if won else "fail")
	hud.refresh()
	print("ORBIT_RUN_FINISHED won=",won," score=",score," seconds=",int(run_time))
	if auto_pilot:
		var passed = not won if idle_test else won
		var report = {"ok":passed,"won":won,"mode":"idle-test" if idle_test else "autoplay","wave":wave,"kills":kills,"score":score,"seconds":run_time,"health":health,"hot_reloads":hot_reload_count,"workshops":workshop_visits,"boss_phases":boss_phases_seen,"weapon":WEAPONS[weapon]}
		var args = OS.get_cmdline_user_args()
		var index = args.find("--aurum-report")
		if index>=0 and index+1<args.size():
			var file = FileAccess.open(args[index+1],FileAccess.WRITE)
			if file:
				file.store_string(JSON.stringify(report))
				file.close()
		print("ORBIT_AUTOPLAY ",JSON.stringify(report))
		get_tree().quit(0 if passed else 1)

func _burst(pos: Vector3, color: Color, radius: float) -> void:
	if effects.size()>36:
		return
	var node = _ring(entities,maxf(radius,0.3),0.06,color,0.0)
	node.position = pos
	effects.append({"node":node,"life":0.35,"radius":radius})

func _tick_effects(delta: float) -> void:
	for i in range(effects.size()-1,-1,-1):
		var effect = effects[i]
		effect.life -= delta
		if effect.radius > 0:
			effect.node.scale = Vector3.ONE*(1.0+(0.35-effect.life)*3.0)
		if effect.life<=0.0:
			effect.node.queue_free()
			effects.remove_at(i)
