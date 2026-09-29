extends Node3D

const HudScript = preload("res://hud.gd")
const RADIUS = 15.3
const CYAN = Color("69f4de")
const GOLD = Color("ffd080")
const RED = Color("ff697c")
const PURPLE = Color("bd93ff")
const SAVE_PATH = "user://record.cfg"
const UPGRADES = ["OVERDRIVE", "REINFORCE", "SPLIT SHOT"]

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
var wave_quota = 0
var wave_spawned = 0
var wave_kills = 0
var run_time = 0.0
var spawn_timer = 0.0
var fire_timer = 0.0
var dash_cooldown = 0.0
var dash_time = 0.0
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

func _ready() -> void:
	test_mode = "--acceptance" in OS.get_cmdline_user_args()
	auto_pilot = "--autoplay" in OS.get_cmdline_user_args()
	idle_test = "--idle-test" in OS.get_cmdline_user_args()
	auto_pilot = auto_pilot or idle_test
	touch_mode = DisplayServer.is_touchscreen_available() or "--touch" in OS.get_cmdline_user_args()
	rng.seed = 71337 if test_mode or auto_pilot else Time.get_ticks_usec()
	_setup_input()
	_build_world()
	_build_audio()
	_load_record()
	_reload_tuning()
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
	if not (next_speed is float or next_speed is int) or not (next_spawn is float or next_spawn is int):
		return
	if not is_finite(float(next_speed)) or not is_finite(float(next_spawn)):
		return
	speed = clampf(float(next_speed),3.0,14.0)
	base_spawn_interval = clampf(float(next_spawn),0.3,3.0)
	if not tuning_hash.is_empty():
		hot_reload_count += 1
		banner = "TUNING UPDATED LIVE"
		banner_time = 2.0
	tuning_hash = fingerprint

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
	run_time = 0.0
	dash_cooldown = 0.0
	dash_time = 0.0
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

func choose_upgrade(index: int) -> void:
	if phase != "upgrade" or index < 0 or index > 2:
		return
	if index == 0:
		fire_period = maxf(0.09,fire_period*0.8)
		damage += 5.0
	elif index == 1:
		max_health += 25.0
		health = minf(max_health,health+55.0)
	else:
		split_shot = true
		damage += 4.0
	health = minf(max_health,health+15.0)
	_next_wave()

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
	dash_cooldown = 2.2
	dash_time = 0.18
	hurt_time = maxf(hurt_time,0.28)
	_sound("dash")
	_burst(player.position+Vector3.UP*0.3,CYAN,1.0)

func _input(event: InputEvent) -> void:
	if not event is InputEventKey:
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
	elif event is InputEventKey and event.pressed and not event.echo:
		var key = event.keycode if event.keycode != 0 else event.physical_keycode
		if key == 0:
			key = event.unicode
		if key == KEY_M or key == 109:
			toggle_mute()
		elif phase == "upgrade" and key in [KEY_1,KEY_2,KEY_3]:
			choose_upgrade(key-KEY_1)

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
	tuning_clock += delta
	if tuning_clock > 0.5:
		tuning_clock = 0.0
		_reload_tuning()
	reactor.rotation.y += delta*0.5
	if auto_pilot and phase == "upgrade":
		choose_upgrade([2,1,0,0][wave-1])
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
		fire_timer = fire_period
		_fire(player.position+Vector3.UP*0.55,aim,false,damage)
		if split_shot:
			_fire(player.position+Vector3.UP*0.55,aim.rotated(Vector3.UP,0.15),false,damage*0.65)
			_fire(player.position+Vector3.UP*0.55,aim.rotated(Vector3.UP,-0.15),false,damage*0.65)
		_sound("shoot")
	spawn_timer -= delta
	if spawn_timer <= 0.0 and wave_spawned < wave_quota:
		spawn_timer = maxf(0.45,base_spawn_interval-wave*0.1)
		var kind = "warden" if wave == 5 else ("brute" if wave >= 3 and wave_spawned%4==0 else ("spitter" if wave>=2 and wave_spawned%3==0 else "seeker"))
		_spawn_enemy(kind)
		wave_spawned += 1
	_tick_enemies(delta)
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
			player.visible = true
			touch_id = -1
			hud.refresh()

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
	_ring(node,radius*1.2,0.09,color,0.04)
	if kind in ["brute","warden"]:
		_box(node,Vector3(radius*2.5,0.25,radius*0.7),Color("693d50"),Vector3(0,radius,0))
	var hp = 2600.0 if boss else (150.0 if kind == "brute" else (70.0 if kind == "spitter" else 42.0))
	var enemy = {"node":node,"core":core,"kind":kind,"hp":hp,"max_hp":hp,"radius":radius,"fire":1.5,"age":0.0,"speed":1.45 if boss else (1.8 if kind=="brute" else 2.7+wave*0.15)}
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
		if distance < enemy.radius+0.55:
			_hurt(22.0 if enemy.kind=="brute" else 14.0)
			enemy.node.position -= direction*1.2
		enemy.fire -= delta
		if enemy.age > 0.8 and enemy.fire <= 0.0 and enemy.kind in ["spitter","warden"]:
			enemy.fire = 1.6 if enemy.kind=="spitter" else 1.4
			_fire(enemy.node.position+Vector3.UP*0.55,direction,true,12.0)
			if enemy.kind == "warden":
				for i in range(10):
					_fire(enemy.node.position+Vector3.UP*0.55,Vector3.FORWARD.rotated(Vector3.UP,i*TAU/10.0+enemy.age),true,10.0)

func _fire(pos: Vector3, direction: Vector3, hostile: bool, power: float) -> void:
	if shots.size()>=240:
		return
	var node = _sphere(entities,0.18 if hostile else 0.11,RED if hostile else GOLD,pos,true)
	shots.append({"node":node,"velocity":direction.normalized()*(6.0 if hostile else 28.0),"hostile":hostile,"power":power,"life":4.0})

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
				if Vector2(shot.node.position.x-enemy.node.position.x,shot.node.position.z-enemy.node.position.z).length()<enemy.radius+0.2:
					enemy.hp -= shot.power
					remove = true
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
		var report = {"ok":passed,"won":won,"mode":"idle-test" if idle_test else "autoplay","wave":wave,"kills":kills,"score":score,"seconds":run_time,"health":health,"hot_reloads":hot_reload_count}
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
		effect.node.scale = Vector3.ONE*(1.0+(0.35-effect.life)*3.0)
		if effect.life<=0.0:
			effect.node.queue_free()
			effects.remove_at(i)
