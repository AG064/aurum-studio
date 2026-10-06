extends Node
const Game = preload("res://main.tscn")
const Checkpoint = preload("res://checkpoint.gd")
const Courier = preload("res://courier.gd")
var checks: Array[Dictionary] = []

func _ready() -> void:
	_run.call_deferred()

func check(label: String, passed: bool) -> void:
	checks.append({"name": label, "ok": passed})
	if not passed: push_error("CHECK FAILED: " + label)

func _run() -> void:
	var yard = Game.instantiate()
	add_child(yard)
	yard.set_physics_process(false)
	await get_tree().physics_frame
	check("title screen on boot", yard.phase == "menu" and yard.hud.card.visible)
	check("physical courier", yard.player is CharacterBody3D)
	check("imported courier model", yard.player.model.get_child_count() > 0)
	check("three themed machinery bays", yard.world.station_gauges.size() == 3 and yard.world.status_lamps.size() == 3)
	check("three core models", yard.world.caps.size() == 3)
	check("initial security patrols", yard.enemies.size() == 3)
	check("normalized asset budget", yard.Art.manifest().assets.courier.bytes < 200000 and yard.Art.manifest().assets.courier.triangles <= 2500)
	check("authored asset units and orientation", yard.Art.manifest().units == "metres" and yard.Art.manifest().forward == "-Z" and yard.Art.manifest().pivot == "ground-centre")
	var identities := true
	for asset in yard.Art.manifest().assets.values(): identities = identities and asset.bytes > 0 and String(asset.sha256).length() == 64
	check("authored assets have byte sizes and hashes", identities)
	check("navigation avoids cargo", not yard.world.path(yard.player.position, yard.world.CAP_POSITIONS[0]).is_empty())
	var finish_probe := Node3D.new()
	add_child(finish_probe)
	var source_material := StandardMaterial3D.new()
	source_material.roughness = 0.2
	var shared_mesh := BoxMesh.new()
	shared_mesh.material = source_material
	var first_mesh := MeshInstance3D.new()
	first_mesh.mesh = shared_mesh
	finish_probe.add_child(first_mesh)
	var second_mesh := MeshInstance3D.new()
	second_mesh.mesh = shared_mesh
	finish_probe.add_child(second_mesh)
	var emissive_material := StandardMaterial3D.new()
	emissive_material.emission_enabled = true
	var lamp_mesh := MeshInstance3D.new()
	var lamp_box := BoxMesh.new()
	lamp_box.material = emissive_material
	lamp_mesh.mesh = lamp_box
	finish_probe.add_child(lamp_mesh)
	yard.Art.Finish.apply(finish_probe)
	var graded := first_mesh.get_active_material(0) as BaseMaterial3D
	check("painted finish keeps source material intact", graded != source_material and is_equal_approx(source_material.roughness, 0.2))
	check("painted finish shares graded materials", second_mesh.get_active_material(0) == graded)
	check("painted finish retains authored roughness and bounded rim", graded.rim_enabled and graded.rim >= 0 and graded.rim <= 1 and is_equal_approx(graded.roughness, 0.2))
	check("emissive materials retain their identity", lamp_mesh.get_active_material(0) == emissive_material)
	yard.Art.Finish.apply(finish_probe)
	check("painted finish is idempotent", first_mesh.get_active_material(0) == graded)
	finish_probe.queue_free()
	yard.hud.start_button.pressed.emit()
	check("title starts gameplay", yard.phase == "play")
	yard.hud.update(0)
	check("playfield unobstructed", not yard.hud.card.visible and not yard.hud.overlay.visible)
	check("health and heat HUD", yard.hud.health.value == 100 and yard.hud.heat.value == 0)
	check("numeric hull readout", yard.hud.hull_value.text == "100")
	check("HUD glyph atlas retains display detail", is_equal_approx(yard.hud.FONT.oversampling, 2.0))
	check("mission progress pips", yard.hud.pips.size() == 3)
	check("initial recharge bars ready", yard.hud.dash_bar.value == 100 and yard.hud.emp_bar.value == 100)
	yard.player.health = 20
	yard.player.heat = 0.85
	yard.hud.update(0)
	check("critical hull warning color", yard.hud.hull_value.get_theme_color("font_color") == yard.hud.HOSTILE and yard.hud.hull_label.text == "HULL LOW")
	check("high heat warning color", yard.hud.heat_value.get_theme_color("font_color") == yard.hud.HOSTILE and yard.hud.heat_label.text == "HOT")
	yard.player.health = 100
	yard.player.heat = 0
	yard.player.dash_cooldown = yard.player.DASH_RECHARGE_SECONDS * 0.5
	yard.player.emp_cooldown = yard.player.EMP_RECHARGE_SECONDS * 0.5
	yard.hud.update(0)
	check("recharge bars reflect half cooldown", is_equal_approx(yard.hud.dash_bar.value, 50) and is_equal_approx(yard.hud.emp_bar.value, 50))
	yard.player.dash_cooldown = 0
	yard.player.emp_cooldown = 0
	yard.hud.announce("CHECK")
	yard.hud.message_time = 0.2
	yard.reduced_motion = true
	yard.hud.update(0)
	check("reduced motion avoids banner animation", yard.hud.banner.modulate.a == 1)
	yard.reduced_motion = false
	yard.hud.announce("SHIFT STARTED / RECOVER THE POWER CORE")
	yard.hud.update(0.2)
	for dimensions in [Vector2(1280, 800), Vector2(960, 640), Vector2(640, 470), Vector2(480, 500), Vector2(400, 700)]:
		yard.hud._layout_pixels(dimensions)
		var bounds := Rect2(Vector2.ZERO, dimensions)
		var contained := true
		for key in ["route", "cluster", "well_hull", "well_heat", "well_dash", "well_emp", "banner_rect", "tag_rect"]:
			var rect: Rect2 = yard.hud.g[key]
			contained = contained and bounds.encloses(rect)
		check("HUD bounds %dx%d" % [dimensions.x, dimensions.y], contained)
		check("HUD instruments separated %dx%d" % [dimensions.x, dimensions.y], not yard.hud.g.well_hull.intersects(yard.hud.g.well_heat) and not yard.hud.g.well_heat.intersects(yard.hud.g.well_dash) and not yard.hud.g.well_dash.intersects(yard.hud.g.well_emp))
		if dimensions.x == 640:
			check("compact announcement avoids mission", not yard.hud.g.banner_rect.intersects(yard.hud.g.route))
			check("compact alerts clear central aim point", not yard.hud.g.banner_rect.has_point(dimensions * 0.5) and not yard.hud.g.tag_rect.has_point(dimensions * 0.5))
			check("compact prompt avoids abilities", not yard.hud.g.tag_rect.intersects(yard.hud.g.cluster))
			check("compact flags retain readable typography", yard.hud.banner.get_theme_font_size("font_size") == 18 and yard.hud.prompt.get_theme_font_size("font_size") >= 16)
			var shaped_lines: int = yard.hud.banner.get_line_count()
			check("compact announcement has no clipped lines", yard.hud.banner.get_visible_line_count() == shaped_lines)
	yard.hud._text_block(yard.hud.banner, "SYSTEM ONLINE\nNEXT BAY READY", 18, 360.0, yard.hud.tracked)
	var wrapped_lines: int = yard.hud.banner.get_line_count()
	check("two-line instrument announcement remains visible", wrapped_lines == 2 and yard.hud.banner.get_visible_line_count() == 2)
	yard.hud._layout()
	var dash_probe := Courier.new()
	dash_probe.position = Vector3(0, 10, 0)
	add_child(dash_probe)
	dash_probe.dash(Vector3.RIGHT)
	var dash_origin: Vector3 = dash_probe.position
	for index in 6: dash_probe.step(1.0 / 60.0, Vector3.ZERO, Vector3.RIGHT, 8.0, false, true)
	check("dash reaches burst speed promptly", dash_probe.velocity.x >= 20)
	check("dash produces real physical displacement", dash_probe.position.x - dash_origin.x > 1.0)
	var dash_saved: Dictionary = dash_probe.snapshot()
	var dash_restored := Courier.new()
	add_child(dash_restored)
	dash_restored.restore(dash_saved)
	dash_probe.step(1.0 / 60.0, Vector3.ZERO, Vector3.RIGHT, 8.0, false, true)
	dash_restored.step(1.0 / 60.0, Vector3.ZERO, Vector3.RIGHT, 8.0, false, true)
	check("mid-dash checkpoint continues consistently", dash_probe.velocity.is_equal_approx(dash_restored.velocity) and is_equal_approx(dash_probe.dash_time, dash_restored.dash_time))
	for index in 5: dash_probe.step(1.0 / 60.0, Vector3.ZERO, Vector3.RIGHT, 8.0, false, true)
	check("dash exit limits residual speed", Vector2(dash_probe.velocity.x, dash_probe.velocity.z).length() <= 8.0 * dash_probe.DASH_EXIT_SPEED)
	dash_probe.damage_flash = 1.0
	var steady := true
	for index in 10:
		dash_probe.step(1.0 / 60.0, Vector3.ZERO, Vector3.RIGHT, 8.0, false, true)
		steady = steady and dash_probe.model.visible
	check("reduced motion prevents damage flicker", steady)
	dash_probe.queue_free()
	dash_restored.queue_free()
	check("dash available", yard.player.dash(Vector3.FORWARD))
	check("dash grants brief protection", yard.player.invulnerable > 0 and yard.player.dash_count == 1)
	check("dash cooldown enforced", not yard.player.dash(Vector3.RIGHT))
	check("protected hull refuses damage", not yard.player.hit(12))
	yard.player.invulnerable = 0
	check("damage reduces hull", yard.player.hit(12) and yard.player.health == 88)
	check("damage grace period", not yard.player.hit(12) and yard.player.health == 88)
	check("EMP available", yard.player.emp())
	check("EMP cooldown enforced", not yard.player.emp())
	check("blaster fires", yard.player.fire())
	check("blaster cadence enforced", not yard.player.fire())
	check("heat rises with shot", yard.player.heat > 0)
	yard.player.overheated = true
	yard.player.fire_cooldown = 0
	check("overheat blocks firing", not yard.player.fire())
	yard.player.overheated = false
	yard.player.dash_time = 0
	yard.player.position = yard.world.CAP_POSITIONS[0]
	yard._interaction(0.2, true, 0)
	check("core requires hold time", yard.carried == -1)
	yard._interaction(0.4, true, 0)
	check("hold secures marked core", yard.carried == 0 and yard.interaction_count == 1)
	yard._interaction(1, true, 1)
	check("moving interrupts docking", yard.charge_progress == 0)
	yard.player.position = yard.world.STATION_POSITIONS[0] + Vector3(0, 0.7, 2.1)
	yard._interaction(1.7, true, 0)
	check("docking restores generator", yard.powered[0] and yard.carried == -1)
	check("relay repairs hull", yard.player.health == 100)
	check("relay escalates security", yard.enemies.size() == 5)
	yard.world.update_state(yard.powered, yard.carried, 1, 0, true)
	check("used core consumed", not yard.world.caps[0].visible)
	check("next core beacon visible", yard.world.beacons[1].visible)
	yard.hud.update(0)
	check("mission pip tracks restored system", yard.hud.pips[0].color == yard.hud.SIGNAL and yard.hud.pips[1].color != yard.hud.SIGNAL)
	yard.player.position = yard.world.CAP_POSITIONS[1]
	yard._interaction(0.6, true, 0)
	yard.move_speed = 8
	yard.player.position.y = 0.7
	yard._bolt(yard.player.position + Vector3(0, 0.2, 0), Vector3.FORWARD * 30, true, 18, 0)
	yard.enemies[0].cooldown = -10.0
	yard.sound.set_volume(0.35)
	var saved: Dictionary = JSON.parse_string(JSON.stringify(yard.aurum_capture_state()))
	check("live mission checkpoint valid", yard._valid_state(saved))
	var replacement = Game.instantiate()
	add_child(replacement)
	replacement.set_physics_process(false)
	check("checkpoint restored", replacement.aurum_restore_state(saved))
	check("inventory preserved", replacement.carried == 1)
	check("progress preserved", replacement.powered == yard.powered)
	check("physical position preserved", replacement.player.position.is_equal_approx(yard.player.position))
	check("live tuning preserved", replacement.move_speed == 8)
	check("enemy identities reconstructed", replacement.enemies.size() == 5 and replacement.enemies[0].uid == yard.enemies[0].uid)
	check("enemy health reconstructed", replacement.enemies[0].health == yard.enemies[0].health)
	check("aged ready cooldown safely restored", replacement.enemies[0].cooldown == 0)
	check("audio preference preserved", is_equal_approx(replacement.sound.volume, 0.35))
	check("projectile reconstructed", replacement.projectiles.size() == 1 and replacement.projectiles[0].damage == 18)
	check("RNG exact state preserved", replacement.rng.state == yard.rng.state)
	check("carried core visual restored", replacement.player.cargo.visible)
	var before := JSON.stringify(replacement.aurum_capture_state())
	var invalid: Array[Dictionary] = []
	for entry in [["version", 2], ["version", 4], ["phase", "unknown"], ["powered", [false, true, false]], ["carried", 2], ["interactions", 1], ["elapsed", -1], ["won", true], ["rng_state", "99999999999999999999"], ["next_uid", 1], ["tuning", {"move_speed": 200}]]:
		var bad := saved.duplicate(true)
		bad[entry[0]] = entry[1]
		invalid.append(bad)
	var bad_position := saved.duplicate(true)
	bad_position.player.position = [300, 0.7, 0]
	invalid.append(bad_position)
	var bad_projectile := saved.duplicate(true)
	bad_projectile.projectiles[0].velocity = [1000, 0, 0]
	invalid.append(bad_projectile)
	var duplicate_id := saved.duplicate(true)
	duplicate_id.enemies[1].uid = duplicate_id.enemies[0].uid
	invalid.append(duplicate_id)
	for index in invalid.size():
		check("invalid checkpoint %d refused" % index, not replacement.aurum_restore_state(invalid[index]))
		check("invalid checkpoint %d is atomic" % index, JSON.stringify(replacement.aurum_capture_state()) == before)
	check("migration helper rejects unsupported versions", not Checkpoint.migrate({"version": 9}, 3, {}).ok)
	var migrated := Checkpoint.migrate({"version": 1, "time": 8}, 2, {1: func(data): data.version = 2; data.elapsed = data.time; data.erase("time"); return data})
	check("detached migration helper", migrated.ok and migrated.state.elapsed == 8)
	replacement.paused = true
	var clock: float = replacement.elapsed
	var hull: float = replacement.player.health
	replacement._physics_process(0.2)
	check("pause freezes mission", replacement.elapsed == clock and replacement.player.health == hull)
	replacement.hud.update(0)
	check("pause menu opens", replacement.hud.card.visible and replacement.hud.resume_button.visible)
	check("pause menu has keyboard focus", replacement.hud.resume_button.has_focus())
	check("menus retain readable scale", replacement.hud.card.scale == Vector2.ONE)
	check("menu content can scroll", replacement.hud.menu_scroll is ScrollContainer)
	check("menu scroll follows keyboard focus", replacement.hud.menu_scroll.follow_focus)
	replacement.hud.resume_button.pressed.emit()
	replacement.hud.update(0)
	check("Resume button returns to play", not replacement.paused and not replacement.hud.card.visible)
	replacement.paused = true
	replacement.hud.update(0)
	replacement.hud.retry_button.pressed.emit()
	replacement.set_physics_process(false)
	check("retry resets mission", replacement.powered.count(true) == 0 and replacement.phase == "play" and not replacement.paused)
	check("retry resets hull", replacement.player.health == 100 and replacement.player.heat == 0)
	check("retry resets enemies", replacement.enemies.size() == 3 and replacement.kills == 0)
	for index in 3:
		replacement.player.position = replacement.world.CAP_POSITIONS[index]
		replacement._interaction(0.6, true, 0)
		replacement.player.position = replacement.world.STATION_POSITIONS[index] + Vector3(0, 0.7, 2.1)
		replacement._interaction(1.7, true, 0)
	check("all systems unlock extraction", replacement.phase == "extract" and replacement.powered.count(true) == 3)
	var boss = null
	for enemy in replacement.enemies:
		if enemy.kind == "warden": boss = enemy
	check("final Warden encounter exists", boss != null and boss.health == 320)
	replacement._damage_enemy(boss, 400)
	check("Warden defeat tracked", replacement.boss_defeated)
	var kills: int = replacement.kills
	replacement._damage_enemy(boss, 20)
	check("dead actors cannot duplicate rewards", replacement.kills == kills)
	check("wave player has genuine hull", replacement.player.health <= 100)
	replacement.hud.volume.value = 0.25
	check("audio control changes gain", replacement.sound.volume == 0.25)
	check("score and effects bundled", replacement.sound.sounds.size() == 9 and replacement.sound.music.stream != null)
	check("audio events requested", replacement.sound.event_count > 0)
	var ok := true
	for entry in checks: ok = ok and entry.ok
	var args := OS.get_cmdline_user_args()
	var report := args.find("--aurum-report")
	if report >= 0 and report + 1 < args.size():
		var file := FileAccess.open(args[report + 1], FileAccess.WRITE)
		if file != null:
			file.store_string(JSON.stringify({"ok": ok, "check_count": checks.size(), "checks": checks}))
			file.close()
	yard.sound.shutdown()
	replacement.sound.shutdown()
	yard.queue_free()
	replacement.queue_free()
	await get_tree().process_frame
	get_tree().quit(0 if ok else 1)
