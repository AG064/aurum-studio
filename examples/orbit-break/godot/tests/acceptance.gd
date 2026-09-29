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
	game.start_run()
	check(game.phase=="playing" and game.wave==1,"Start enters wave one")
	var key = InputEventKey.new()
	key.keycode = KEY_ESCAPE
	key.pressed = true
	check(key.is_action_pressed("pause"),"Logical keyboard events map to pause")
	game._input(key)
	check(game.phase=="paused","Keyboard pause works even with a focused interface button")
	game._input(key)
	game.phase = "upgrade"
	key.keycode = 0
	key.unicode = KEY_3
	game._input(key)
	check(game.phase=="playing" and game.split_shot,"Unicode-only number keys select upgrades")
	game.phase = "upgrade"
	key.keycode = KEY_1
	key.physical_keycode = KEY_TAB
	game._input(key)
	check(game.phase=="playing" and game.damage>20,"Logical shortcuts take precedence over synthetic physical scan codes")
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
	check(game.dash_time>0 and game.dash_cooldown>2,"Dash starts and has a cooldown")
	var cooldown: float = game.dash_cooldown
	game.dash()
	check(game.dash_cooldown==cooldown,"Dash cannot bypass its cooldown")
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
	game.start_run()
	var upgrades = 0
	for w in range(1,6):
		game.spawn_timer = 1000.0
		var quota: int = game.wave_quota
		for n in range(quota):
			var kind = "warden" if w==5 else "seeker"
			var enemy: Dictionary = game._spawn_enemy(kind,Vector3(0,0,-8))
			game.wave_spawned += 1
			var hits = ceili(enemy.hp/game.damage)
			for hit in range(hits):
				game._fire(enemy.node.position+Vector3.UP*0.55,Vector3.ZERO,false,game.damage)
				game._tick_shots(0.001)
		game._tick(0.001)
		if w<5:
			check(game.phase=="upgrade","Wave %d reaches an upgrade through combat" % w)
			game.choose_upgrade((w-1)%3)
			upgrades += 1
	check(game.phase=="won" and game.wave==5 and game.kills==59,"All five waves and boss reach the victory screen")
	check(upgrades==4 and game.split_shot and game.max_health>100 and game.damage>16,"All upgrade types apply")
	game.start_run()
	check(game.kills==0 and game.score==0 and game.health==100 and not game.split_shot,"Restart resets the run and upgrades")
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
