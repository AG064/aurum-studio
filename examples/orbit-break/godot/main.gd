extends Node3D

const HudScript = preload("res://hud.gd")
const ArtScript = preload("res://art.gd")
const Rewards = preload("res://rewards.gd")
const Campaign = preload("res://campaign.gd")
const Mission = preload("res://mission.gd")
const Profile = preload("res://profile.gd")
const EnemyBehavior = preload("res://enemy_behavior.gd")
const RADIUS = 15.3
const CYAN = Color("69f4de")
const GOLD = Color("ffd080")
const RED = Color("ff697c")
const PURPLE = Color("bd93ff")
const SAVE_PATH = "user://record.cfg"
const WEAPONS = ["PULSE", "LANCE", "ARC"]

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
var choices: Array[Dictionary] = []
var chosen_upgrades: Array[String] = []
var pierce_count = 3
var arc_links = 3
var arc_range = 10.0
var dash_period = 2.2
var shock_drive = false
var starting_weapon = 0
var art: Node3D
var player_shield: MeshInstance3D
var reduced_motion = false
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
var mission = Mission.new()
var profile = Profile.new()
var encounter: Dictionary = {}
var selected_frame = "kestrel"
var act = 0
var armor = 0.0
var pickup_radius = 4.0
var repair_value = 10.0
var lifesteal = false
var charged_round = false
var capacitor = false
var nova = false
var storm = false
var nova_clock = 0.0
var burst_queue: Array[Dictionary] = []
var module_counts: Dictionary = {}
var turret_damage = 18.0
var turret_period = 0.4
var bosses_defeated = 0
var salvage_recovered = 0
var reactor_lowest = 100.0
var damage_taken = 0.0
var discoveries_run: Array[String] = []
var medals_run: Array[String] = []
var transmission = ""
var transmission_time = 0.0
var soundtrack: Node
var archive_index = 0
var failure_reason = ""

func aurum_capture_state() -> Dictionary:
	return preload("res://checkpoint.gd").capture(self)

func aurum_restore_state(state: Dictionary) -> bool:
	return preload("res://checkpoint.gd").restore(self,state)

func _ready() -> void:
	test_mode = "--acceptance" in OS.get_cmdline_user_args()
	auto_pilot = "--autoplay" in OS.get_cmdline_user_args()
	idle_test = "--idle-test" in OS.get_cmdline_user_args()
	auto_pilot = auto_pilot or idle_test
	var weapon_argument = OS.get_cmdline_user_args().find("--weapon")
	if weapon_argument >= 0 and weapon_argument+1 < OS.get_cmdline_user_args().size():
		starting_weapon = clampi(int(OS.get_cmdline_user_args()[weapon_argument+1]),0,2)
	touch_mode = DisplayServer.is_touchscreen_available() or "--touch" in OS.get_cmdline_user_args()
	rng.seed = 71337 if test_mode or auto_pilot else Time.get_ticks_usec()
	_setup_input()
	_build_world()
	_build_audio()
	_load_record()
	var frame_argument = OS.get_cmdline_user_args().find("--frame")
	if frame_argument>=0 and frame_argument+1<OS.get_cmdline_user_args().size():
		selected_frame = Campaign.frame(OS.get_cmdline_user_args()[frame_argument+1]).id
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
	env.background_color = Color("080d17")
	env.ambient_light_source = Environment.AMBIENT_SOURCE_COLOR
	env.ambient_light_color = Color("a2b4ca")
	env.ambient_light_energy = 0.34
	world.environment = env
	add_child(world)
	var light = DirectionalLight3D.new()
	light.rotation_degrees = Vector3(-47,-38,0)
	light.light_color = Color("f4dcc4")
	light.light_energy = 1.85
	light.shadow_enabled = true
	light.directional_shadow_max_distance = 55
	add_child(light)
	var fill = DirectionalLight3D.new()
	fill.rotation_degrees = Vector3(-36,138,0)
	fill.light_color = Color("93bce5")
	fill.light_energy = 0.42
	add_child(fill)
	camera = Camera3D.new()
	camera.projection = Camera3D.PROJECTION_PERSPECTIVE
	camera.fov = 43
	camera.position = Vector3(0,40,27)
	add_child(camera)
	camera.look_at(Vector3.ZERO)
	camera.current = true
	_fit_camera()
	get_viewport().size_changed.connect(_fit_camera)
	art = ArtScript.new()
	add_child(art)
	art.setup(self)
	entities = Node3D.new()
	entities.name = "Entities"
	add_child(entities)
	player = Node3D.new()
	player.name = "Pilot"
	add_child(player)
	art.ship(player,"pilot")
	player_shield = art.glow(player,Vector3(0,0.32,0),Color(0.2,0.75,1,0.75),1.35)
	player_shield.visible = false

func _fit_camera() -> void:
	var viewport_size = get_viewport().get_visible_rect().size
	var aspect = viewport_size.x/maxf(viewport_size.y,1.0)
	# Preserve the vertical view on desktop and the full arena width in portrait.
	camera.keep_aspect = Camera3D.KEEP_WIDTH
	camera.fov = rad_to_deg(2.0*atan(tan(deg_to_rad(43.0)*0.5)*maxf(aspect,1.0)))

func _build_audio() -> void:
	if test_mode or DisplayServer.get_name()=="headless": return
	for spec in [["shoot",680.0,0.045],["lance",1150.0,0.11],["arc",830.0,0.16],["explosion",75.0,0.35],["hit",150.0,0.09],["dash",280.0,0.14],["pickup",950.0,0.15],["wave",440.0,0.4],["fail",85.0,0.6]]:
		var rate = 22050
		var data = PackedByteArray()
		var count = int(rate * spec[2])
		data.resize(count*2)
		for i in range(count):
			var t = float(i)/rate
			var envelope = pow(1.0-float(i)/count,2.0) * minf(t*150.0,1.0)
			var sample = sin(t*TAU*spec[1]*(1.0-0.35*t/spec[2])) * envelope * 0.18
			if spec[0] in ["explosion","arc","lance"]:
				var noise = (fposmod(sin(float(i)*127.1)*43758.545,1.0)*2.0-1.0)
				sample += noise*envelope*(0.12 if spec[0]=="explosion" else 0.035)
			data.encode_s16(i*2,int(sample*32767.0))
		var stream = AudioStreamWAV.new()
		stream.format = AudioStreamWAV.FORMAT_16_BITS
		stream.mix_rate = rate
		stream.data = data
		tones[spec[0]] = stream
	for i in range(8):
		var voice = AudioStreamPlayer.new()
		voice.volume_db = -8.0
		add_child(voice)
		audio.append(voice)
	if not test_mode and DisplayServer.get_name()!="headless":
		soundtrack = load("res://score.gd").new()
		add_child(soundtrack)

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
	profile.load_record(SAVE_PATH)
	best = profile.best
	muted = profile.muted
	selected_frame = profile.selected_frame

func _save_record() -> void:
	if test_mode:
		return
	profile.best = best
	profile.muted = muted
	profile.selected_frame = selected_frame
	var error = profile.save_record(SAVE_PATH)
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
	reduced_motion = bool(web_window.matchMedia("(prefers-reduced-motion: reduce)").matches)
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
	web_window.aurumState = JSON.stringify({"phase":phase,"wave":wave,"health":health,"kills":kills,"time":run_time,"x":player.position.x,"z":player.position.z,"speed":speed,"damage_multiplier":damage_multiplier,"hot_reloads":hot_reload_count,"revision":web_revision,"dash_cooldown":dash_cooldown,"dashes":dashes_run,"weapon":WEAPONS[weapon],"workshops":workshop_visits,"choices":choices.map(func(c): return c.id),"upgrades":chosen_upgrades.size(),"view_width":get_viewport().get_visible_rect().size.x,"view_height":get_viewport().get_visible_rect().size.y,"frame":selected_frame,"act":act+1,"encounters":Campaign.ENCOUNTERS.size(),"objective":mission.kind,"reactor":mission.reactor_health,"remaining":mission.remaining,"bosses":bosses_defeated,"modules":chosen_upgrades,"content_version":2})

func start_run() -> void:
	_clear_entities()
	wave = 0
	kills = 0
	score = 0
	health = Campaign.frame(selected_frame).health
	max_health = health
	armor = Campaign.frame(selected_frame).armor
	damage = 16.0
	fire_period = 0.28
	split_shot = false
	choices.clear()
	chosen_upgrades.clear()
	weapon = starting_weapon
	art.update_weapon()
	pierce_count = 3
	arc_links = 3
	arc_range = 10.0
	dash_period = 2.2
	shock_drive = false
	pickup_radius = 4.0
	repair_value = 10.0
	lifesteal = false
	charged_round = false
	capacitor = false
	nova = false
	storm = false
	nova_clock = 0.0
	module_counts.clear()
	burst_queue.clear()
	turret_damage = 18.0
	turret_period = 0.4
	bosses_defeated = 0
	salvage_recovered = 0
	reactor_lowest = 100.0
	damage_taken = 0.0
	failure_reason = ""
	discoveries_run.clear()
	medals_run.clear()
	workshop_visits = 0
	boss_phases_seen = 0
	turret_timer = 0.0
	run_time = 0.0
	dash_cooldown = 0.0
	dash_time = 0.0
	dashes_run = 0
	hurt_time = 0.0
	player_shield.visible = false
	fire_timer = 0.0
	player.position = Vector3(0,0,7)
	aim = Vector3.FORWARD
	move = Vector2.ZERO
	touch_id = -1
	art.configure_frame(selected_frame)
	if selected_frame=="relay":
		_fit_wingman()
		turret_damage = 7.0
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
	if art: art.reset_effects()
	burst_queue.clear()

func open_hangar() -> void:
	_clear_entities()
	phase = "hangar"
	act = 0
	player.position = Vector3.ZERO
	art.set_sector(0)
	hud.refresh()

func choose_frame(index: int) -> void:
	if phase!="hangar" or index<0 or index>=Campaign.FRAMES.size(): return
	selected_frame = Campaign.FRAMES[index].id
	_save_record()
	start_run()

func return_menu() -> void:
	_clear_entities()
	phase = "menu"
	act = 0
	wave = 0
	player.position = Vector3.ZERO
	player.visible = true
	art.set_sector(0)
	hud.refresh()

func open_records() -> void:
	if phase!="menu": return
	phase = "records"
	hud.refresh()

func _next_wave() -> void:
	wave += 1
	phase = "playing"
	wave_spawned = 0
	wave_kills = 0
	encounter = Campaign.encounter(wave)
	act = int(encounter.act)
	wave_quota = int(encounter.quota)
	mission.begin(self,encounter)
	art.set_sector(act)
	spawn_timer = 1.0
	banner = encounter.name
	banner_time = 3.0
	transmission = encounter.brief
	transmission_time = 7.0
	_sound("wave")
	hud.refresh()

func shot_power(base: float, flat: float, additive: float, multiplier: float) -> float:
	return (base + flat) * (1.0 + additive) * multiplier

func open_workshop() -> void:
	phase = "upgrade"
	choices = Rewards.offer(self)
	workshop_visits += 1
	player.visible = true
	touch_id = -1
	hud.refresh()

func choose_upgrade(index: int) -> void:
	if phase != "upgrade" or index < 0 or index >= choices.size():
		return
	var id: String = choices[index].id
	chosen_upgrades.append(id)
	module_counts[id] = int(module_counts.get(id,0))+1
	# Resolve the phase before applying, so repeated input cannot select twice.
	phase = "refitting"
	match id:
		"lance": weapon = 1
		"arc": weapon = 2
		"evolve":
			if weapon == 0:
				if split_shot: fire_period = maxf(0.09,fire_period/1.25)
				split_shot = true
				damage *= 1.25
			elif weapon == 1:
				pierce_count += 2
				damage *= 1.4
			else:
				arc_links += 2
				damage *= 1.35
		"hull": max_health += 30.0; health = minf(max_health,health+55.0)
		"wingman":
			if is_instance_valid(turret): turret_damage += 9.0; turret_period = maxf(0.22,turret_period*0.85)
			else: _fit_wingman()
		"afterburner": shock_drive = true; dash_period *= 0.75
		"overclock": fire_period = maxf(0.09,fire_period/1.3); damage *= 1.2
		"magnet": pickup_radius += 2.5; repair_value += 5.0
		"armor": armor = minf(0.5,armor+0.15); max_health += 15; health = minf(max_health,health+15)
		"siphon": lifesteal = true
		"capacitor": capacitor = true
		"range": arc_range += 3.0; pierce_count += 1
		"vector": dash_period = maxf(1.1,dash_period*0.8)
		"nova": nova = true
		"storm": storm = true; arc_links += 2
	choices.clear()
	_sound("pickup")
	art.update_weapon()
	_next_wave()
	spawn_timer = 2.0

func _fit_wingman() -> void:
	if is_instance_valid(turret):
		return
	turret = Node3D.new()
	entities.add_child(turret)
	art.ship(turret,"pilot")
	turret.scale = Vector3.ONE*0.5

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
	dash_cooldown = dash_period
	dash_time = 0.18
	if capacitor: charged_round = true
	hurt_time = maxf(hurt_time,0.28)
	_sound("dash")
	_burst(player.position+Vector3.UP*0.3,CYAN,2.0 if shock_drive else 1.0)
	if shock_drive:
		for i in range(enemies.size()-1,-1,-1):
			if enemies[i].node.position.distance_to(player.position) < 3.5:
				enemies[i].hp -= 90.0
				if enemies[i].hp <= 0: _kill_enemy(i)

func _input(event: InputEvent) -> void:
	if not event is InputEventKey:
		return
	if event.echo:
		return
	if event.is_action_pressed("pause"):
		if phase in ["hangar","records"]: return_menu()
		else: pause_game()
		get_viewport().set_input_as_handled()
	elif event.is_action_pressed("dash"):
		dash()
		get_viewport().set_input_as_handled()
	elif event.is_action_pressed("confirm") and phase in ["menu","won","lost"]:
		if phase=="menu": open_hangar()
		else: start_run()
		get_viewport().set_input_as_handled()
	elif event.is_action_pressed("confirm") and phase=="hangar":
		var index = 0
		for i in range(Campaign.FRAMES.size()):
			if Campaign.FRAMES[i].id==selected_frame: index=i
		choose_frame(index)
		get_viewport().set_input_as_handled()
	elif event is InputEventKey and event.pressed and not event.echo:
		var key = event.keycode if event.keycode != 0 else event.physical_keycode
		if key == 0:
			key = event.unicode
		if key == KEY_M or key == 109:
			toggle_mute()
		elif phase == "upgrade" and key in [KEY_1,KEY_2,KEY_3]:
			choose_upgrade(key-KEY_1)
		elif phase=="hangar" and key in [KEY_1,KEY_2,KEY_3]:
			choose_frame(key-KEY_1)

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

func _exit_tree() -> void:
	for voice in audio:
		if is_instance_valid(voice): voice.stop(); voice.stream = null
	audio.clear()
	tones.clear()

func _physics_process(delta: float) -> void:
	_publish_web_state(delta)
	if soundtrack: soundtrack.update_music(act,phase,muted,delta)
	tuning_clock += delta
	if tuning_clock > 0.5:
		tuning_clock = 0.0
		_reload_tuning()
	reactor.rotation.y += delta*0.5
	if auto_pilot and phase == "upgrade":
		var choice = 0
		var best_value = -INF
		for i in range(choices.size()):
			var id: String = choices[i].id
			var value = {"evolve":60.0,"wingman":58.0,"overclock":52.0,"nova":90.0,"storm":90.0,"capacitor":70.0,"siphon":55.0,"armor":40.0,"afterburner":45.0,"vector":36.0,"range":35.0,"magnet":30.0,"hull":20.0,"lance":-1000.0,"arc":-1000.0}.get(id,0.0)
			if id=="hull" and health<max_health*0.75: value = 140.0
			if value>best_value: best_value=value; choice=i
		choose_upgrade(choice)
	if phase == "playing":
		move = Input.get_vector("left","right","up","down")
		if touch_id >= 0:
			move = ((touch_position-touch_origin)/65.0).limit_length()
		if auto_pilot and not idle_test:
			var orbit = 7.0 if mission.kind=="defend" else 10.0
			var destination = Vector3(cos(run_time*0.25)*orbit,0,sin(run_time*0.25)*orbit)
			if mission.kind=="salvage":
				var nearest = INF
				for item in pickups:
					if item.get("kind","")=="salvage" and item.node.position.distance_squared_to(player.position)<nearest:
						nearest = item.node.position.distance_squared_to(player.position)
						destination = Vector3(item.node.position.x,0,item.node.position.z)
			destination = _safe_destination(destination)
			move = Vector2(destination.x-player.position.x,destination.z-player.position.z).normalized()
			if enemies.any(func(e): return e.node.position.distance_to(player.position)<3.0):
				dash()
		_tick(delta)
	_tick_effects(delta)
	flash = maxf(0.0,flash-delta*3.0)
	shake = maxf(0.0,shake-delta*4.0)
	if not test_mode and not reduced_motion:
		camera.h_offset = sin(Time.get_ticks_msec()*0.077)*shake*0.12
		camera.v_offset = cos(Time.get_ticks_msec()*0.093)*shake*0.12
	if hud:
		hud.queue_redraw()

func _safe_destination(desired: Vector3) -> Vector3:
	if hazards.is_empty(): return desired
	var predicted = player.position+(desired-player.position).normalized()*2.5
	var danger = false
	for hazard in hazards:
		if _inside_hazard(hazard,player.position) or _inside_hazard(hazard,predicted): danger=true
	if not danger: return desired
	var best = player.position
	var cost = INF
	for i in range(16):
		var angle = TAU*i/16.0
		var candidate = (player.position+Vector3(cos(angle),0,sin(angle))*4.0).limit_length(RADIUS-0.5)
		var unsafe = false
		for hazard in hazards:
			if _inside_hazard(hazard,candidate): unsafe=true; break
		var next_cost = candidate.distance_squared_to(desired)*0.15+candidate.distance_squared_to(player.position)
		if not unsafe and next_cost<cost: best=candidate; cost=next_cost
	if best.distance_squared_to(player.position)>0.1: dash()
	return best

func _tick(delta: float) -> void:
	if phase != "playing":
		return
	run_time += delta
	mission.tick(self,delta)
	if phase!="playing": return
	nova_clock = maxf(0,nova_clock-delta)
	banner_time = maxf(0.0,banner_time-delta)
	transmission_time = maxf(0,transmission_time-delta)
	dash_cooldown = maxf(0.0,dash_cooldown-delta)
	dash_time = maxf(0.0,dash_time-delta)
	hurt_time = maxf(0.0,hurt_time-delta)
	var direction = Vector3(move.x,0,move.y)
	if dash_time > 0.0 and direction.length_squared()<0.1:
		direction = aim
	player.position += direction * speed * (3.8 if dash_time>0.0 else 1.0) * delta
	player.position = player.position.limit_length(RADIUS)
	player.visible = true
	player_shield.visible = hurt_time>0.0
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
		_sound(["shoot","lance","arc"][weapon])
	spawn_timer -= delta
	if spawn_timer <= 0.0 and wave_spawned < wave_quota:
		spawn_timer = maxf(0.45,base_spawn_interval-wave*0.1)
		if enemies.size()<28:
			var kind: String = encounter.mix[wave_spawned%encounter.mix.size()]
			_spawn_enemy(kind)
			wave_spawned += 1
	_tick_enemies(delta)
	if phase!="playing": return
	_tick_hazards(delta)
	if phase!="playing": return
	_tick_turret(delta)
	_tick_shots(delta)
	if phase!="playing": return
	_tick_pickups(delta)
	_tick_bursts()
	if phase == "playing" and wave_spawned >= wave_quota and enemies.is_empty() and mission.complete():
		for shot in shots:
			shot.node.queue_free()
		shots.clear()
		score += wave*100
		if wave == Campaign.ENCOUNTERS.size():
			_finish(true)
		else:
			for hazard in hazards:
				hazard.node.queue_free()
			hazards.clear()
			open_workshop()

func _fire_weapon() -> void:
	var origin = player.position + Vector3.UP*0.55
	var power = shot_power(16.0,damage-16.0,0.0,damage_multiplier)
	if charged_round:
		power *= 1.8
		charged_round = false
	art.muzzle(origin+aim*0.7,CYAN if weapon==2 else GOLD)
	if weapon == 2:
		# Arc chains by proximity from the previous hit, with diminishing damage.
		var cursor = player.position
		var hit: Array[Dictionary] = []
		for hop in range(arc_links):
			var nearest: Dictionary = {}
			var distance = arc_range if hop == 0 else 6.0
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
			if storm: health = minf(max_health,health+0.75)
			cursor = destination
		for i in range(enemies.size()-1,-1,-1):
			if enemies[i].hp <= 0:
				_kill_enemy(i)
	else:
		var lance = weapon == 1
		_fire(origin,aim,false,power*(2.8 if lance else 1.0),pierce_count if lance else 1)
		if split_shot and not lance:
			for angle in [-0.15,0.15]:
				_fire(origin,aim.rotated(Vector3.UP,angle),false,power*0.65*(2.8 if lance else 1.0),3 if lance else 1)

func _beam(from: Vector3, to: Vector3, color: Color) -> void:
	var previous = from
	for i in range(1,5):
		if effects.size()>=64: return
		var point = from.lerp(to,float(i)/4.0)
		if i<4: point += Vector3(art.visual_rng.randf_range(-0.3,0.3),0,art.visual_rng.randf_range(-0.3,0.3))
		var node = _box(entities,Vector3(0.065,0.065,maxf(0.01,previous.distance_to(point))),color,(previous+point)*0.5,true)
		if previous.distance_squared_to(point)>0.01: node.look_at(point)
		effects.append({"node":node,"life":0.16,"radius":0.0})
		previous = point
	art.sparks(to,CYAN,5,0.45)

func _tick_turret(delta: float) -> void:
	if not is_instance_valid(turret):
		return
	turret.position = turret.position.lerp(player.position+Vector3(-1.8,0,1.0),minf(1.0,delta*5))
	turret_timer -= delta
	if turret_timer > 0.0:
		return
	var target: Dictionary = {}
	var distance = 12.0
	for enemy in enemies:
		var d = enemy.node.position.distance_to(turret.position)
		if d < distance:
			distance = d
			target = enemy
	if not target.is_empty():
		turret_timer = turret_period
		var direction: Vector3 = (target.node.position-turret.position).normalized()
		turret.rotation.y = atan2(-direction.x,-direction.z)
		_fire(turret.position+Vector3.UP*0.55,direction,false,turret_damage*damage_multiplier)

func _warn_strike(pos: Vector3) -> void:
	if hazards.size() >= 8:
		return
	var node = _ring(entities,2.6,0.09,RED,0.05)
	node.position = Vector3(pos.x,0.05,pos.z)
	hazards.append({"node":node,"life":1.4,"radius":2.6})

func _warn_lane(pos: Vector3, direction: Vector3, length: float, width: float, duration: float, power: float) -> void:
	if hazards.size()>=8: return
	var node = _box(entities,Vector3(length,0.025,width),Color(1,0.25,0.2,0.12),Vector3(pos.x,0.02,pos.z),true)
	node.material_override.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	node.rotation.y = atan2(-direction.z,direction.x)
	for edge in [-1,1]: _box(node,Vector3(length,0.03,0.045),RED,Vector3(0,0.025,edge*width*0.5),true)
	hazards.append({"node":node,"life":duration,"duration":duration,"shape":"lane","direction":direction.normalized(),"length":length,"width":width,"power":power})

func _inside_hazard(hazard: Dictionary, point: Vector3) -> bool:
	var offset: Vector3 = point-hazard.node.position
	if hazard.get("shape","")=="lane":
		var direction: Vector3 = hazard.direction
		return absf(offset.dot(direction))<hazard.length*0.5 and absf(offset.dot(Vector3(-direction.z,0,direction.x)))<hazard.width*0.5
	return Vector2(offset.x,offset.z).length()<hazard.radius

func _tick_hazards(delta: float) -> void:
	for i in range(hazards.size()-1,-1,-1):
		var hazard = hazards[i]
		hazard.life -= delta
		# The marked radius always matches the eventual damage area.
		var tint = RED.lerp(GOLD,1.0-clampf(hazard.life/float(hazard.get("duration",1.4)),0,1))
		if hazard.get("shape","")=="lane": tint.a = 0.12
		hazard.node.material_override.albedo_color = tint
		if hazard.life <= 0:
			var pos: Vector3 = hazard.node.position
			_burst(pos,RED,2.6)
			if _inside_hazard(hazard,player.position): _hurt(float(hazard.get("power",28.0)))
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

func _spawn_enemy(kind: String, position_override = Vector3.INF, counts = true) -> Dictionary:
	if not Campaign.ENEMIES.has(kind): kind = "seeker"
	var node = Node3D.new()
	entities.add_child(node)
	var angle = rng.randf()*TAU
	node.position = Vector3(cos(angle)*14.9,0,sin(angle)*14.9) if position_override == Vector3.INF else position_override
	if position_override == Vector3.INF and node.position.distance_to(player.position)<6.0:
		node.position = -node.position
	var definition: Dictionary = Campaign.ENEMIES[kind]
	var boss = kind in Campaign.BOSSES
	var radius = float(definition.radius)
	var color = GOLD if boss else (PURPLE if kind == "spitter" else RED)
	var core = art.ship(node,kind)
	var hp = float(definition.hp)*(1.0 if boss else 1.0+act*0.3)
	if boss and encounter.get("boss","")==kind: hp = float(encounter.boss_hp)
	var warning = _ring(node,radius*1.6,0.045,color,0.05)
	warning.visible = false
	var enemy = {"node":node,"core":core,"warning":warning,"kind":kind,"hp":hp,"max_hp":hp,"radius":radius,"fire":1.5,"age":0.0,"stage":1,"strike":3.0,"deploy":5.5,"speed":float(definition.speed),"reactor":mission.kind=="defend" and counts and wave_spawned%3==0,"counts":counts,"guard_active":kind=="sentinel"}
	enemies.append(enemy)
	if boss: boss_phases_seen += 1
	if kind not in discoveries_run: discoveries_run.append(kind)
	_burst(node.position+Vector3.UP*0.3,color,1.4)
	return enemy

func _tick_enemies(delta: float) -> void:
	EnemyBehavior.tick(self,delta)

func _fire(pos: Vector3, direction: Vector3, hostile: bool, power: float, pierce = 1, reactor_target = false) -> void:
	if shots.size()>=240:
		return
	var node = _box(entities,Vector3(0.12,0.1,0.48) if not hostile else Vector3(0.22,0.12,0.32),RED if hostile else GOLD,pos,true)
	art.glow(node,Vector3.ZERO,Color(1,0.2,0.12,0.8) if hostile else Color(1,0.64,0.22,0.8),0.55)
	if direction.length_squared()>0.01: node.look_at(pos+direction)
	if pierce > 1:
		node.scale = Vector3(0.8,0.8,3.5)
		if direction.length_squared()>0.01:
			node.look_at(pos+direction)
	shots.append({"node":node,"velocity":direction.normalized()*(6.0 if hostile else 28.0),"hostile":hostile,"power":power,"life":4.0,"pierce":pierce,"hit":[],"reactor_target":reactor_target,"bypass":pierce>=5})

func _tick_shots(delta: float) -> void:
	for i in range(shots.size()-1,-1,-1):
		var shot = shots[i]
		shot.node.position += shot.velocity*delta
		shot.life -= delta
		var remove = shot.life<=0.0 or Vector2(shot.node.position.x,shot.node.position.z).length()>19.0
		if shot.hostile:
			if shot.reactor_target and mission.kind=="defend" and Vector2(shot.node.position.x,shot.node.position.z).length()<1.8:
				mission.damage_reactor(self,shot.power*0.55)
				remove = true
			if Vector2(shot.node.position.x-player.position.x,shot.node.position.z-player.position.z).length()<0.6:
				_hurt(shot.power)
				remove = true
		else:
			for j in range(enemies.size()-1,-1,-1):
				var enemy = enemies[j]
				if enemy.node.get_instance_id() not in shot.hit and Vector2(shot.node.position.x-enemy.node.position.x,shot.node.position.z-enemy.node.position.z).length()<enemy.radius+0.2:
					var guarded = enemy.guard_active and not shot.bypass and shot.velocity.normalized().dot(enemy.node.basis.z)>0.2
					enemy.hp -= shot.power*(0.25 if guarded else 1.0)
					art.sparks(shot.node.position,GOLD,4,0.65)
					shot.hit.append(enemy.node.get_instance_id())
					shot.pierce -= 1
					remove = shot.pierce <= 0
					if enemy.hp<=0.0:
						_kill_enemy(j)
					break
		if remove:
			shot.node.queue_free()
			shots.remove_at(i)

func _kill_enemy(index: int, reward = true) -> void:
	var enemy = enemies[index]
	var pos: Vector3 = enemy.node.position
	_burst(pos+Vector3.UP*0.5,GOLD if enemy.kind=="warden" else RED,enemy.radius*1.8)
	if reward and kills%4==3 and pickups.size()<15:
		var node = _sphere(entities,0.28,CYAN,pos+Vector3.UP*0.5,true)
		pickups.append({"node":node,"life":14.0})
	enemy.node.queue_free()
	enemies.remove_at(index)
	if enemy.counts: wave_kills += 1
	if reward:
		kills += 1
		score += int(Campaign.ENEMIES[enemy.kind].bounty)
		if lifesteal: health = minf(max_health,health+2.0)
		if enemy.kind in Campaign.BOSSES:
			bosses_defeated += 1
			health = minf(max_health,health+25.0)
		if nova and nova_clock<=0:
			nova_clock = 0.2
			burst_queue.append({"pos":pos,"radius":2.8,"power":damage*0.8})
	_sound("explosion")

func _tick_bursts() -> void:
	for burst in burst_queue:
		_burst(burst.pos,CYAN,burst.radius)
		for i in range(enemies.size()-1,-1,-1):
			if enemies[i].node.position.distance_to(burst.pos)<burst.radius:
				enemies[i].hp -= burst.power
				if enemies[i].hp<=0: _kill_enemy(i)
	burst_queue.clear()

func _tick_pickups(delta: float) -> void:
	for i in range(pickups.size()-1,-1,-1):
		var item = pickups[i]
		var salvage = item.get("kind","")=="salvage"
		if not salvage: item.life -= delta
		var delta_pos = player.position+Vector3.UP*0.5-item.node.position
		if delta_pos.length()<pickup_radius:
			item.node.position += delta_pos.normalized()*delta*8.0
		if delta_pos.length()<0.8:
			if salvage:
				mission.collected += 1
				salvage_recovered += 1
				score += 75
			else:
				health = minf(max_health,health+repair_value)
				score += 10
			if selected_frame=="kestrel": dash_cooldown = maxf(0,dash_cooldown-0.35)
			item.life = 0.0
			_sound("pickup")
		if item.life==0.0 or (not salvage and item.life<0):
			item.node.queue_free()
			pickups.remove_at(i)

func _hurt(amount: float) -> void:
	if hurt_time>0.0 or phase!="playing":
		return
	var actual = amount*(1.0-armor)
	health = maxf(0.0,health-actual)
	damage_taken += actual
	hurt_time = 0.65
	flash = 0.8
	shake = 1.0
	_sound("hit")
	if health<=0.0:
		failure_reason = "Ship integrity lost."
		_finish(false)

func _finish(won: bool) -> void:
	if phase in ["won","lost"]: return
	phase = "won" if won else "lost"
	player.visible = true
	touch_id = -1
	best = maxi(best,score)
	if won: medals_run.append("blockade")
	if won and reactor_lowest>=75: medals_run.append("intact_core")
	if salvage_recovered>=6: medals_run.append("full_manifest")
	if dashes_run>=30: medals_run.append("dash_pilot")
	profile.record(self,won)
	_save_record()
	_sound("wave" if won else "fail")
	hud.refresh()
	print("ORBIT_RUN_FINISHED won=",won," score=",score," seconds=",int(run_time))
	if auto_pilot:
		var passed = not won if idle_test else won
		var report = {"ok":passed,"won":won,"mode":"idle-test" if idle_test else "autoplay","wave":wave,"kills":kills,"score":score,"seconds":run_time,"health":health,"hot_reloads":hot_reload_count,"workshops":workshop_visits,"boss_phases":boss_phases_seen,"bosses":bosses_defeated,"weapon":WEAPONS[weapon],"frame":selected_frame,"salvage":salvage_recovered,"content_version":2,"modules":chosen_upgrades}
		var args = OS.get_cmdline_user_args()
		var index = args.find("--aurum-report")
		if index>=0 and index+1<args.size():
			var file = FileAccess.open(args[index+1],FileAccess.WRITE)
			if file:
				file.store_string(JSON.stringify(report))
				file.close()
		print("ORBIT_AUTOPLAY ",JSON.stringify(report))
		call_deferred("_quit_cleanly",0 if passed else 1)

func _quit_cleanly(code: int) -> void:
	_clear_entities()
	await get_tree().process_frame
	get_tree().quit(code)

func _burst(pos: Vector3, color: Color, radius: float) -> void:
	art.explosion(pos,color,radius)

func _tick_effects(delta: float) -> void:
	art.tick(delta)
	for i in range(effects.size()-1,-1,-1):
		var effect = effects[i]
		effect.life -= delta
		if effect.radius > 0:
			effect.node.scale = Vector3.ONE*(1.0+(0.35-effect.life)*3.0)
		if effect.life<=0.0:
			effect.node.queue_free()
			effects.remove_at(i)
