extends SceneTree
var failures: Array[String] = []
var checks = 0
func _init(): call_deferred("_run")
func check(value: bool, label: String):
	checks += 1
	if not value: failures.append(label)
func _run():
	var game = load("res://main.tscn").instantiate()
	root.add_child(game)
	current_scene = game
	await process_frame
	game.set_physics_process(false)
	game.start_run()
	game.health = 63.5
	game.score = 840
	game.run_time = 47.25
	game.chosen_upgrades.assign(["shield","wingman"])
	game._spawn_enemy("sentinel",Vector3(6,0,4))
	game._spawn_enemy("warden",Vector3(-7,0,-4),false)
	game.enemies[0].hp = 31.25
	game.enemies[1].stage = 2
	game.enemies[0].node.rotation.y = 1.5
	game._fire(Vector3(3,0.5,2),Vector3.FORWARD,false,22,3)
	game.shots[0].hit.append(game.enemies[0].node.get_instance_id())
	game._warn_lane(Vector3(0,0,-5),Vector3.RIGHT,28,2,1.65,22)
	game.mission.kind = "defend"
	game.mission.reactor_health = 74.5
	game.profile.wins = 2
	game.rng.state = 9223372036854775000
	game.phase = "paused"
	var bridge = load(OS.get_cmdline_user_args()[0]).new()
	bridge.name = "AurumLive"
	root.add_child(bridge)
	var saved = bridge.request({"op":"checkpoint","freeze":true})
	check(saved.ok and saved.complete and saved.mode == "custom", "custom gameplay checkpoint captured")
	var checkpoint = JSON.parse_string(JSON.stringify(saved.checkpoint))
	game._clear_entities()
	game.health = 1.0
	game.score = 0
	game.run_time = 0.0
	game.rng.state = 12
	var restored = bridge.request({"op":"restore","checkpoint":checkpoint})
	check(restored.ok and restored.complete, "custom checkpoint restored")
	check(game.phase == "paused" and game.health == 63.5 and game.score == 840 and game.run_time == 47.25, "progress and pause retained")
	check(game.enemies.size() == 2 and game.enemies[0].hp == 31.25 and game.enemies[1].stage == 2, "enemy health and boss phase retained")
	check(game.enemies[0].node.position == Vector3(6,0,4) and is_equal_approx(game.enemies[0].node.rotation.y,1.5), "enemy transforms retained")
	check(game.shots.size() == 1 and game.shots[0].hit == [game.enemies[0].node.get_instance_id()], "projectile hit references remapped")
	check(game.hazards.size() == 1 and game.hazards[0].shape == "lane", "active hazard retained")
	check(game.mission.reactor_health == 74.5 and game.profile.wins == 2, "mission and profile retained")
	check(game.rng.state == 9223372036854775000, "64-bit randomness retained exactly")
	check(game.chosen_upgrades == ["shield","wingman"], "typed module inventory retained")
	print("AURUM_ORBIT_CHECKPOINT " + JSON.stringify({"ok":failures.is_empty(),"checks":checks,"failures":failures}))
	quit(0 if failures.is_empty() else 1)
