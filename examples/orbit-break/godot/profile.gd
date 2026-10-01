extends RefCounted

const Campaign = preload("res://campaign.gd")
var wins = 0
var flights = 0
var best = 0
var furthest = 0
var discoveries: Array[String] = []
var medals: Array[String] = []
var recent: Array[Dictionary] = []
var selected_frame = "kestrel"
var muted = false

func load_record(path: String) -> void:
	var config = ConfigFile.new()
	if config.load(path)!=OK: return
	best = _number(config.get_value("record","best",0),0,9999999)
	wins = _number(config.get_value("record","wins",0),0,99999)
	flights = _number(config.get_value("record","flights",0),wins,99999)
	furthest = _number(config.get_value("record","furthest",0),0,Campaign.ENCOUNTERS.size())
	muted = config.get_value("settings","muted",false)==true
	selected_frame = Campaign.frame(str(config.get_value("settings","frame","kestrel"))).id
	var found = config.get_value("record","discoveries",[])
	if found is Array:
		for id in found:
			if id is String and Campaign.ENEMIES.has(id) and id not in discoveries: discoveries.append(id)
	var earned = config.get_value("record","medals",[])
	if earned is Array:
		for id in earned:
			if id in ["blockade","intact_core","full_manifest","dash_pilot"] and id not in medals: medals.append(id)
	var records = config.get_value("record","recent",[])
	if records is Array:
		for entry in records.slice(0,5):
			if entry is Dictionary:
				recent.append({"won":entry.get("won",false)==true,"score":_number(entry.get("score",0),0,9999999),"wave":_number(entry.get("wave",0),0,12),"seconds":_number(entry.get("seconds",0),0,86400),"frame":Campaign.frame(str(entry.get("frame","kestrel"))).id})

func _number(value: Variant, lower: int, upper: int) -> int:
	if not (value is int or value is float) or not is_finite(float(value)): return lower
	return clampi(int(value),lower,upper)

func record(game: Node3D, won: bool) -> void:
	flights += 1
	wins += 1 if won else 0
	best = maxi(best,game.score)
	furthest = maxi(furthest,game.wave)
	for id in game.discoveries_run:
		if id not in discoveries: discoveries.append(id)
	for id in game.medals_run:
		if id not in medals: medals.append(id)
	recent.push_front({"won":won,"score":game.score,"wave":game.wave,"seconds":int(game.run_time),"frame":game.selected_frame})
	recent.resize(mini(recent.size(),5))

func save_record(path: String) -> Error:
	var config = ConfigFile.new()
	for pair in [["best",best],["wins",wins],["flights",flights],["furthest",furthest],["discoveries",discoveries],["medals",medals],["recent",recent]]:
		config.set_value("record",pair[0],pair[1])
	config.set_value("settings","muted",muted)
	config.set_value("settings","frame",selected_frame)
	var temporary = path+".pending"
	var result = config.save(temporary)
	if result!=OK: return result
	return DirAccess.rename_absolute(ProjectSettings.globalize_path(temporary),ProjectSettings.globalize_path(path))
