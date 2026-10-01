extends RefCounted
## Versioned gameplay checkpoint. Renderer objects are reconstructed, never serialized.
const FIELDS = ["phase","wave","kills","score","best","health","max_health","damage","fire_period","speed","split_shot","weapon","choices","chosen_upgrades","pierce_count","arc_links","arc_range","dash_period","shock_drive","starting_weapon","reduced_motion","turret_timer","workshop_visits","boss_phases_seen","wave_quota","wave_spawned","wave_kills","run_time","spawn_timer","fire_timer","dash_cooldown","dash_time","dashes_run","hurt_time","aim","muted","shake","flash","banner","banner_time","tuning_hash","hot_reload_count","base_spawn_interval","damage_multiplier","web_revision","encounter","selected_frame","act","armor","pickup_radius","repair_value","lifesteal","charged_round","capacitor","nova","storm","nova_clock","burst_queue","module_counts","turret_damage","turret_period","bosses_defeated","salvage_recovered","reactor_lowest","damage_taken","discoveries_run","medals_run","transmission","transmission_time","archive_index","failure_reason"]
const MISSION = ["kind","collected","target","remaining","reactor_health","vent_clock","spec"]
const PROFILE = ["wins","flights","best","furthest","discoveries","medals","recent","selected_frame","muted"]

static func capture(game: Node3D) -> Dictionary:
	var enemy_ids = []
	for enemy in game.enemies: enemy_ids.append(enemy.node.get_instance_id())
	var enemies = []
	for enemy in game.enemies:
		var entry = _entity(enemy)
		entry["warning_visible"] = enemy.warning.visible
		enemies.append(entry)
	var shots = []
	for shot in game.shots:
		var entry = _entity(shot)
		entry["hit"] = []
		for id in shot.hit:
			var index = enemy_ids.find(id)
			if index >= 0: entry.hit.append(index)
		shots.append(entry)
	return {"version":1,"fields":_values(game,FIELDS),"mission":_values(game.mission,MISSION),"profile":_values(game.profile,PROFILE),"player_position":game.player.position,"player_rotation":game.player.rotation,"player_visible":game.player.visible,"rng_state":str(game.rng.state),"rng_seed":str(game.rng.seed),"enemies":enemies,"shots":shots,"hazards":game.hazards.map(_entity),"pickups":game.pickups.map(_entity),"wingman":is_instance_valid(game.turret)}

static func restore(game: Node3D, data) -> bool:
	if not data is Dictionary or data.get("version") != 1: return false
	for pair in [[game,FIELDS,"fields"],[game.mission,MISSION,"mission"],[game.profile,PROFILE,"profile"]]:
		if not _valid_values(pair[0],pair[1],data.get(pair[2])): return false
	if not data.get("player_position") is Vector3 or not data.get("player_rotation") is Vector3: return false
	if str(data.fields.get("phase","")) not in ["menu","hangar","records","playing","paused","upgrade","won","lost"]: return false
	if int(data.fields.get("weapon",-1)) not in [0,1,2] or int(data.fields.get("wave",-1)) < 0 or int(data.fields.wave) > 12: return false
	for pair in [["enemies",28],["shots",240],["hazards",8],["pickups",32]]:
		var entries = data.get(pair[0])
		if not entries is Array or entries.size() > pair[1]: return false
		for entry in entries:
			if not entry is Dictionary or not entry.get("position") is Vector3 or not entry.get("rotation") is Vector3 or not entry.get("scale") is Vector3 or not entry.get("values") is Dictionary: return false
			for reserved in ["node","core","warning"]:
				if entry.values.has(reserved): return false
	for entry in data.enemies:
		if not game.Campaign.ENEMIES.has(str(entry.values.get("kind",""))): return false
	for entry in data.shots:
		if not entry.values.get("velocity") is Vector3 or not entry.get("hit") is Array: return false
		for index in entry.hit:
			if not (index is int or index is float) or int(index) < 0 or int(index) >= data.enemies.size(): return false
	for entry in data.hazards:
		if entry.values.get("shape","") == "lane" and not entry.values.get("direction") is Vector3: return false
	game._clear_entities()
	_assign(game,data.fields,FIELDS)
	_assign(game.mission,data.mission,MISSION)
	_assign(game.profile,data.profile,PROFILE)
	game.art.configure_frame(game.selected_frame)
	game.art.set_sector(game.act)
	game.art.update_weapon()
	for entry in data.enemies:
		var enemy = game._spawn_enemy(str(entry.values.kind),entry.position,bool(entry.values.get("counts",true)))
		_merge(enemy,entry)
		enemy.warning.visible = bool(entry.get("warning_visible",false))
	for entry in data.shots:
		var values: Dictionary = entry.values
		game._fire(entry.position,values.velocity.normalized(),bool(values.hostile),float(values.power),int(values.pierce),bool(values.get("reactor_target",false)))
		var shot: Dictionary = game.shots.back()
		_merge(shot,entry)
		shot.hit = []
		for index in entry.hit: shot.hit.append(game.enemies[int(index)].node.get_instance_id())
	for entry in data.hazards:
		var values: Dictionary = entry.values
		if values.get("shape","") == "lane":
			game._warn_lane(entry.position,values.direction,float(values.length),float(values.width),float(values.duration),float(values.power))
		else: game._warn_strike(entry.position)
		_merge(game.hazards.back(),entry)
	for entry in data.pickups:
		var node = game.art.salvage_cache(game.entities,entry.position) if entry.values.get("kind","") == "salvage" else game._sphere(game.entities,0.28,game.CYAN,entry.position,true)
		var pickup = {"node":node}
		_merge(pickup,entry)
		game.pickups.append(pickup)
	if bool(data.get("wingman",false)): game._fit_wingman()
	# Factories may advance randomness and presentation counters. Restore them last.
	_assign(game,data.fields,FIELDS)
	game.rng.seed = str(data.get("rng_seed","0")).to_int()
	game.rng.state = str(data.get("rng_state","0")).to_int()
	game.player.position = data.player_position
	game.player.rotation = data.player_rotation
	game.player.visible = bool(data.get("player_visible",true))
	game.player_shield.visible = game.hurt_time > 0
	game.move = Vector2.ZERO
	game.touch_id = -1
	game.art.reactor_feedback(game.mission.reactor_health/100.0)
	game.hud.refresh()
	game._publish_web_state(1.0)
	return true

static func _values(owner: Object, fields: Array) -> Dictionary:
	var result = {}
	for name in fields: result[name] = owner.get(name)
	return result

static func _valid_values(owner: Object, fields: Array, values) -> bool:
	if not values is Dictionary: return false
	for name in fields:
		if not values.has(name): return false
		var old = owner.get(name)
		var value = values[name]
		if typeof(old) in [TYPE_INT,TYPE_FLOAT]:
			if not typeof(value) in [TYPE_INT,TYPE_FLOAT] or not is_finite(float(value)): return false
			if typeof(old) == TYPE_INT and float(value) != floor(float(value)): return false
		elif typeof(old) != typeof(value): return false
		if old is Array and old.is_typed():
			for item in value:
				if typeof(item) != old.get_typed_builtin(): return false
	return true

static func _assign(owner: Object, values: Dictionary, fields: Array):
	for name in fields:
		var old = owner.get(name)
		var value = values[name]
		if typeof(old) == TYPE_INT: value = int(value)
		elif typeof(old) == TYPE_FLOAT: value = float(value)
		elif old is Array and old.is_typed():
			value = old.duplicate()
			value.assign(values[name])
		owner.set(name,value)

static func _entity(entity: Dictionary) -> Dictionary:
	var values = {}
	for key in entity:
		if not entity[key] is Object: values[key] = entity[key]
	return {"values":values,"position":entity.node.position,"rotation":entity.node.rotation,"scale":entity.node.scale}

static func _merge(entity: Dictionary, saved: Dictionary):
	for key in saved.values: entity[key] = saved.values[key]
	entity.node.position = saved.position
	entity.node.rotation = saved.rotation
	entity.node.scale = saved.scale
