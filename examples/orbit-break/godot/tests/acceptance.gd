extends Node

var checks: Array[String] = []
var failures: Array[String] = []
var game: Node3D

func check(condition: bool, label: String) -> void:
	if condition:
		checks.append(label)
	else:
		failures.append(label)
		push_error("ACCEPTANCE: "+label)

func _ready() -> void:
	call_deferred("run")

func run() -> void:
	if not "--acceptance" in OS.get_cmdline_user_args():
		push_error("This test requires --acceptance and an isolated project copy.")
		get_tree().quit(2)
		return
	game = load("res://main.gd").new()
	add_child(game)
	check(game.phase=="menu","Launch opens the menu")
	game.open_hangar()
	check(game.phase=="hangar" and game.hud.buttons.size()==4,"Hangar offers three flight frames and one back action")
	game.choose_frame(1)
	check(game.selected_frame=="bastion" and game.health==140 and is_equal_approx(game.armor,0.1),"Bastion selection launches with its advertised hull and armour")
	game.open_hangar()
	game.choose_frame(2)
	check(game.health==95 and is_instance_valid(game.turret) and game.turret_damage==7,"Relay selection launches with its advertised escort")
	game.selected_frame = "kestrel"
	game.start_run()
	check(game.phase=="playing" and game.wave==1,"Start enters wave one")
	var original_window_size: Vector2i = get_window().size
	var arena_framed = true
	for window_size in [Vector2i(1280,800),Vector2i(390,844),Vector2i(960,480)]:
		get_window().size = window_size
		await get_tree().process_frame
		game._fit_camera()
		var bounds = game.get_viewport().get_visible_rect().grow(-8)
		for angle in range(16):
			var point = Vector3(cos(angle*TAU/16)*game.RADIUS,0,sin(angle*TAU/16)*game.RADIUS)
			arena_framed = arena_framed and bounds.has_point(game.camera.unproject_position(point))
	get_window().size = original_window_size
	await get_tree().process_frame
	game._fit_camera()
	check(arena_framed,"All arena movement limits stay in camera view on desktop, portrait and compact layouts")
	game.health = 25
	game.dash_cooldown = game.dash_period*0.5
	check(is_equal_approx(game.hud._hull_ratio(),0.25) and is_equal_approx(game.hud._dash_charge(),0.5),"Instrument gauges reflect actual hull and dash cooldown")
	game.hud._process(0.016)
	check(game.hud.hull_echo>game.hud._hull_ratio() and game.health==25,"Damage echo is presentation only and preserves actual integrity")
	var previous_motion: bool = game.reduced_motion
	game.reduced_motion = true
	game.hud._process(0.016)
	check(is_equal_approx(game.hud.hull_echo,0.25),"Reduced motion updates damage feedback without trailing animation")
	game.reduced_motion = previous_motion
	game.health = 100
	game.dash_cooldown = 0
	game.hud._process(0.016)
	var key = InputEventKey.new()
	key.keycode = KEY_ESCAPE
	key.pressed = true
	check(key.is_action_pressed("pause"),"Logical keyboard events map to pause")
	game._input(key)
	check(game.phase=="paused","Keyboard pause works even with a focused interface button")
	game._input(key)
	game.open_workshop()
	check(game.choices.size()==3 and game.hud.buttons.size()==3,"Workshop exposes exactly three choices and no extra shop actions")
	var modules_match = true
	var labels_contained = true
	for i in range(game.hud.buttons.size()):
		var entry: Dictionary = game.hud.buttons[i]
		modules_match = modules_match and entry.node.symbol==game.choices[i].symbol and entry.node.key_hint==str(i+1) and entry.node.focus_mode==Control.FOCUS_ALL
		for label in entry.labels:
			labels_contained = labels_contained and Rect2(Vector2(8,8),entry.rect.size-Vector2(16,16)).encloses(label.rect)
	check(modules_match,"Every equipment module has a matching functional diagram, shortcut and keyboard focus")
	check(labels_contained,"All workshop label regions stay inside their equipment modules")
	var text_contained = true
	for number in range(1,12):
		game.wave = number
		game.choices = game.Rewards.offer(game)
		game.hud.refresh()
		for entry in game.hud.buttons:
			var caption: Dictionary = entry.labels[-1]
			var face: Font = caption.node.get_theme_font("font")
			text_contained = text_contained and face.get_string_size(caption.node.text,HORIZONTAL_ALIGNMENT_LEFT,-1,caption.font_size).x<=caption.rect.size.x
	game.wave = 1
	game.choices = game.Rewards.offer(game)
	game.hud.refresh()
	check(text_contained,"Upgrade effect captions fit one line across the full campaign offer pool")
	var valid_offers = game.choices.map(func(c): return c.id)==["evolve","lance","arc"]
	for equipped in [1,2]:
		game.weapon = equipped
		var offered: Array = game.Rewards.offer(game)
		valid_offers = valid_offers and offered.size()==3 and not offered.any(func(c): return c.id==game.WEAPONS[equipped].to_lower())
	game.weapon = 0
	check(valid_offers,"Weapon offers share the three-choice pool and never repeat the equipped weapon as a no-op")
	var workshop_time: float = game.run_time
	game._physics_process(10.0)
	check(game.run_time==workshop_time and game.phase=="upgrade","Workshop waits indefinitely for the player's choice")
	game.choose_upgrade(-1)
	game.choose_upgrade(3)
	check(game.phase=="upgrade" and game.chosen_upgrades.is_empty(),"Out-of-range choices leave the workshop and build untouched")
	key.keycode = 0
	key.unicode = KEY_2
	game._input(key)
	check(game.phase=="playing" and game.weapon==1 and game.wave==2 and game.chosen_upgrades.size()==1,"One keyboard choice fits the new weapon and advances exactly one wave")
	var chosen_damage: float = game.damage
	game.choose_upgrade(0)
	check(game.damage==chosen_damage and game.chosen_upgrades.size()==1,"Repeated selection cannot apply a second reward")
	game.open_workshop()
	key.keycode = KEY_1
	key.physical_keycode = KEY_TAB
	game._input(key)
	check(game.phase=="playing" and game.pierce_count==5 and is_equal_approx(game.damage,22.4),"Lance upgrade adds piercing and damage through the displayed logical shortcut")
	game.weapon = 2
	game.wave = 2
	game.open_workshop()
	game.choose_upgrade(0)
	check(game.arc_links==5 and game.damage>22.4,"Arc evolution adds links and damage rather than a generic weapon swap")
	game.start_run()
	game.wave = 2
	game.health = 35
	game.open_workshop()
	game.choose_upgrade(1)
	check(game.health==90 and game.max_health==130,"Repair weave restores the advertised hull and increases maximum integrity")
	game.wave = 2
	game.open_workshop()
	game.choose_upgrade(2)
	check(is_instance_valid(game.turret),"Wingman is a single choice, not a separate purchase flow")
	game.wave = 3
	game.open_workshop()
	game.choose_upgrade(1)
	check(game.shock_drive and is_equal_approx(game.dash_period,1.65),"Shock drive changes dash combat and cooldown meaningfully")
	check(is_equal_approx(game.shot_power(16,4,0.5,2),60.0),"Damage stacking follows flat then additive then multiplicative order")
	game.start_run()
	var position_before: Vector3 = game.player.position
	Input.action_press("right")
	game._physics_process(0.1)
	Input.action_release("right")
	check(game.player.position.x>position_before.x,"Mapped movement moves the ship")
	game.player.position = Vector3(15.2,0,0)
	game.move = Vector2.RIGHT
	game._tick(0.2)
	check(game.player.position.length()<=game.RADIUS+0.001,"Arena boundary contains the player")
	game.dash()
	check(game.dash_time>0 and game.dash_cooldown>2 and game.dashes_run==1,"Dash starts, acknowledges one activation and has a cooldown")
	var cooldown: float = game.dash_cooldown
	game.dash()
	check(game.dash_cooldown==cooldown and game.dashes_run==1,"Dash cannot bypass its cooldown or acknowledge a rejected activation")
	game.pause_game()
	var before_time: float = game.run_time
	game._tick(1.0)
	check(game.run_time==before_time,"Pause freezes gameplay time")
	game.pause_game()
	check(game.phase=="playing","Pause resumes")
	game._clear_entities()
	game.player.position = Vector3.ZERO
	game.wave_spawned = game.wave_quota-1
	game.spawn_timer = 100.0
	game.dash_time = 0.0
	game.move = Vector2.ZERO
	game._spawn_enemy("seeker",Vector3(0,0,-4))
	for step in range(60):
		game._tick(1.0/60.0)
		game._tick_effects(1.0/60.0)
	check(game.kills==1 and game.score>0,"Auto aim, projectiles, damage, and kill scoring work together")
	game.hurt_time = 0.0
	game._hurt(25.0)
	check(game.health==75.0,"Damage reduces integrity")
	game._hurt(25.0)
	check(game.health==75.0,"Hit invulnerability prevents stacked damage")
	var health_before: float = game.health
	var pickup = game._sphere(game.entities,0.2,Color.WHITE,game.player.position+Vector3.UP*0.5,true)
	game.pickups.append({"node":pickup,"life":5.0})
	game._tick_pickups(0.1)
	check(game.health>health_before and game.pickups.is_empty(),"Repair pickup heals and is consumed")
	game._clear_entities()
	var spitter: Dictionary = game._spawn_enemy("spitter",Vector3(0,0,-5))
	spitter.age = 2.0
	spitter.fire = 0.0
	game._tick_enemies(0.01)
	check(game.shots.any(func(s): return s.hostile),"Ranged enemies fire hostile projectiles")
	game._clear_entities()
	game.health = 100.0
	game.hurt_time = 0.0
	game._fire(game.player.position+Vector3.UP*0.5,Vector3.ZERO,true,12.0)
	game._tick_shots(0.01)
	check(game.health==88.0,"Hostile projectile collision damages the player")
	game._clear_entities()
	game.player.position = Vector3.ZERO
	game.hurt_time = 0.0
	game._warn_strike(Vector3.ZERO)
	game._tick_hazards(0.5)
	check(game.health==88 and game.hazards.size()==1 and game.hazards[0].node.scale==Vector3.ONE,"Boss telegraph preserves the full damage radius until impact")
	game.player.position = Vector3(5,0,0)
	game._tick_hazards(1.0)
	check(game.health==88 and game.hazards.is_empty(),"Moving outside the marked strike avoids damage")
	game._warn_strike(game.player.position)
	game._tick_hazards(1.5)
	check(game.health==60,"Remaining in a telegraphed strike causes the advertised damage")
	game._clear_entities()
	var boss: Dictionary = game._spawn_enemy("warden",Vector3(0,0,-10))
	boss.hp = boss.max_hp*0.6
	game._tick_enemies(0.01)
	check(boss.stage==2 and boss.speed>1.45,"Boss enters pursuit below two-thirds health")
	boss.hp = boss.max_hp*0.2
	boss.strike = 0.0
	game._tick_enemies(0.01)
	check(boss.stage==3 and not game.hazards.is_empty(),"Boss overload phase schedules a marked strike")
	game._clear_entities()
	game.player.position = Vector3.ZERO
	game.aim = Vector3.FORWARD
	game.weapon = 1
	game._spawn_enemy("brute",Vector3(0,0,-2))
	game._spawn_enemy("brute",Vector3(0,0,-4))
	game._fire_weapon()
	for step in range(20):
		game._tick_shots(1.0/120.0)
	check(game.enemies.size()==2 and game.enemies.all(func(e): return e.hp<e.max_hp),"Lance projectile pierces two separate targets")
	check(game.enemies.all(func(e): return is_equal_approx(e.hp,150.0-44.8)),"A piercing projectile cannot hit the same target twice")
	game._clear_entities()
	game.weapon = 2
	game._spawn_enemy("brute",Vector3(0,0,-3))
	game._spawn_enemy("brute",Vector3(3,0,-4))
	game._spawn_enemy("brute",Vector3(13,0,10))
	game._fire_weapon()
	check(game.enemies[0].hp<150 and game.enemies[1].hp<150 and game.enemies[2].hp==150,"Arc chains nearby targets but never jumps beyond its range")
	game.weapon = 0
	game.start_run()
	var upgrades = 0
	for w in range(1,game.Campaign.ENCOUNTERS.size()+1):
		game.spawn_timer = 1000.0
		var quota: int = game.wave_quota
		for n in range(quota):
			var kind: String = game.encounter.mix[n%game.encounter.mix.size()]
			var enemy: Dictionary = game._spawn_enemy(kind,Vector3(0,0,-8))
			game.wave_spawned += 1
			var hits = ceili(enemy.hp/game.damage)
			for hit in range(hits):
				game._fire(enemy.node.position+Vector3.UP*0.55,Vector3.ZERO,false,game.damage)
				game._tick_shots(0.001)
		if game.mission.kind=="salvage":
			for item in game.pickups.duplicate():
				if item.get("kind","")=="salvage":
					game.player.position = Vector3(item.node.position.x,0,item.node.position.z)
					game._tick_pickups(0.001)
		if game.mission.kind=="hold": game.mission.tick(game,100.0)
		game._tick(0.001)
		if w<game.Campaign.ENCOUNTERS.size():
			check(game.phase=="upgrade","Wave %d reaches an upgrade through combat" % w)
			game.choose_upgrade((w-1)%3)
			upgrades += 1
	check(game.phase=="won" and game.wave==12 and game.bosses_defeated==3 and game.salvage_recovered==6,"All twelve authored encounters, three bosses and cache objectives reach victory")
	check(upgrades==11 and game.split_shot and game.damage>16,"Every workshop applies exactly one build choice through the campaign")
	game.start_run()
	check(game.kills==0 and game.score==0 and game.health==100 and not game.split_shot and game.chosen_upgrades.is_empty() and not is_instance_valid(game.turret),"Restart resets the run and all chosen upgrades")
	game._hurt(1000.0)
	check(game.phase=="lost","Zero integrity reaches the loss screen")
	game.start_run()
	game.touch_mode = true
	var down = InputEventScreenTouch.new()
	down.index = 0
	down.pressed = true
	down.position = Vector2(180,450)
	game._unhandled_input(down)
	var drag = InputEventScreenDrag.new()
	drag.index = 0
	drag.position = Vector2(240,450)
	game._unhandled_input(drag)
	var touch_before: Vector3 = game.player.position
	game._physics_process(0.1)
	check(game.player.position.x>touch_before.x,"Touch drag moves the ship")
	down.pressed = false
	game._unhandled_input(down)
	check(game.touch_id==-1,"Releasing touch stops the virtual stick")
	game.start_run()
	game.mission.begin(game,{"objective":"salvage","act":0})
	check(not game.mission.complete() and game.pickups.size()==3,"Recovery cannot finish before its three caches are collected")
	for item in game.pickups.duplicate():
		game.player.position = Vector3(item.node.position.x,0,item.node.position.z)
		game._tick_pickups(0.001)
	check(game.mission.complete() and game.mission.collected==3,"Actual pickup collection completes the recovery objective")
	game.mission.begin(game,{"objective":"salvage","duration":0.5,"act":0})
	game.mission.tick(game,0.6)
	check(game.phase=="lost","Unrecovered caches at lockdown fail the mission instead of leaving a soft lock")
	game.start_run()
	game.mission.begin(game,{"objective":"hold","duration":2.0,"act":0})
	game.mission.tick(game,1.0)
	check(not game.mission.complete() and game.mission.remaining==1.0,"A hold objective does not end early after killing its patrol")
	game.pause_game()
	game._tick(5.0)
	check(game.mission.remaining==1.0,"Pause freezes the objective clock as well as the ship")
	game.pause_game()
	game.mission.tick(game,1.1)
	check(game.mission.complete(),"The hold objective ends when its actual timer expires")
	game.mission.begin(game,{"objective":"defend","act":0})
	game.mission.damage_reactor(game,30.0)
	check(game.mission.reactor_health==70 and game.health==100,"Defense damage affects the reactor independently from pilot hull")
	game.mission.damage_reactor(game,100.0)
	check(game.phase=="lost","A destroyed reactor ends the sortie")
	game.start_run()
	game.player.position = Vector3.ZERO
	game._warn_lane(Vector3.ZERO,Vector3.RIGHT,20,2.0,1.4,24)
	check(game._inside_hazard(game.hazards[0],Vector3(3,0,0.8)) and not game._inside_hazard(game.hazards[0],Vector3(3,0,1.2)),"Lane damage uses the same width as its marked boundary")
	game._tick_hazards(1.5)
	check(game.health==76,"Remaining in a marked lane takes the stated damage")
	game.start_run()
	game.player.position = Vector3.ZERO
	var sentinel: Dictionary = game._spawn_enemy("sentinel",Vector3(0,0,-3))
	sentinel.node.rotation.y = PI
	game._fire(sentinel.node.position+Vector3.UP*0.55,Vector3.FORWARD,false,40)
	game._tick_shots(0.001)
	check(sentinel.hp==120,"The active frontal shield blocks three quarters of a projectile hit")
	sentinel.guard_active = false
	game._fire(sentinel.node.position+Vector3.UP*0.55,Vector3.FORWARD,false,40)
	game._tick_shots(0.001)
	check(sentinel.hp==80,"A sentinel opening exposes full projectile damage")
	game._clear_entities()
	var carrier: Dictionary = game._spawn_enemy("carrier",Vector3(0,0,-8))
	carrier.deploy = 0
	game._tick_enemies(0.01)
	check(game.enemies.size()==3 and game.enemies.filter(func(e): return not e.counts).size()==2,"Carrier escorts are real enemies and do not inflate the scheduled contact quota")
	game.start_run()
	game.capacitor = true
	game.dash()
	game._fire_weapon()
	check(is_equal_approx(game.shots[0].power,28.8) and not game.charged_round,"A dash capacitor changes the next volley once, then consumes its charge")
	game._clear_entities()
	game.nova = true
	game._spawn_enemy("seeker",Vector3(0,0,-3))
	var adjacent: Dictionary = game._spawn_enemy("seeker",Vector3(1,0,-3))
	game._kill_enemy(0)
	game._tick_bursts()
	check(adjacent.hp<adjacent.max_hp,"Ion bloom deals actual area damage to a neighbouring contact")
	var profile = game.Profile.new()
	profile.best = 1234
	profile.wins = 2
	profile.discoveries.append("sentinel")
	check(profile.save_record("user://acceptance-profile.cfg")==OK,"Flight records save successfully to isolated user data")
	profile.best = 2345
	check(profile.save_record("user://acceptance-profile.cfg")==OK,"An atomic profile update replaces the previous valid record")
	var restored = game.Profile.new()
	restored.load_record("user://acceptance-profile.cfg")
	check(restored.best==2345 and restored.wins==2 and restored.discoveries==["sentinel"],"Flight records and discovered contacts survive a save/load round trip")
	var original = FileAccess.get_file_as_string("res://tuning.json")
	var tuning = FileAccess.open("res://tuning.json",FileAccess.WRITE)
	tuning.store_string('{"player_speed":9.0,"spawn_interval":0.8}')
	tuning.close()
	var run_before: float = game.run_time
	game._reload_tuning()
	check(game.speed==9.0 and game.hot_reload_count==1 and game.run_time==run_before,"Live tuning reload preserves the running game state")
	tuning = FileAccess.open("res://tuning.json",FileAccess.WRITE)
	tuning.store_string('{"player_speed":"invalid"}')
	tuning.close()
	game._reload_tuning()
	check(game.speed==9.0,"Invalid live tuning retains the previous valid values")
	tuning = FileAccess.open("res://tuning.json",FileAccess.WRITE)
	tuning.store_string(original)
	tuning.close()
	game._reload_tuning()
	game._clear_entities()
	await get_tree().process_frame
	check(game.entities.get_child_count()==0,"Transient gameplay nodes are cleaned up")
	check(game.art.motes.is_empty() and game.art.rings.is_empty(),"Restart clears the bounded particle and light-effect pools")
	var report = {"ok":failures.is_empty(),"checks":checks,"failures":failures,"check_count":checks.size(),"scope":"Deterministic gameplay integration; desktop and device playtests are separate."}
	var args = OS.get_cmdline_user_args()
	var report_index = args.find("--aurum-report")
	if report_index>=0 and report_index+1<args.size():
		var file = FileAccess.open(args[report_index+1],FileAccess.WRITE)
		if file:
			file.store_string(JSON.stringify(report,"  "))
			file.close()
	print("ORBIT_ACCEPTANCE ",JSON.stringify(report))
	get_tree().quit(0 if failures.is_empty() else 1)
