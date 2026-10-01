extends RefCounted

var kind = "clear"
var collected = 0
var target = 3
var remaining = 0.0
var reactor_health = 100.0
var vent_clock = 6.0
var spec: Dictionary = {}

func begin(game: Node3D, encounter: Dictionary) -> void:
	spec = encounter
	kind = str(spec.objective)
	collected = 0
	remaining = float(spec.get("duration",120.0 if kind=="salvage" else 0.0))
	reactor_health = 100.0
	vent_clock = 6.0
	for i in range(game.pickups.size()-1,-1,-1):
		if game.pickups[i].get("kind","")=="salvage":
			game.pickups[i].node.queue_free()
			game.pickups.remove_at(i)
	if kind=="salvage":
		for pos in [Vector3(-9,0.4,-7),Vector3(9,0.4,-2),Vector3(0,0.4,11)]:
			var node: Node3D = game.art.salvage_cache(game.entities,pos)
			game.pickups.append({"node":node,"kind":"salvage","life":-1.0})

func tick(game: Node3D, delta: float) -> void:
	if kind in ["hold","salvage"]: remaining = maxf(0,remaining-delta)
	if kind=="salvage" and remaining<=0 and collected<target:
		game.failure_reason = "Recovery window closed."
		game._finish(false)
		return
	if int(spec.get("act",0))==2:
		vent_clock -= delta
		if vent_clock<=0:
			vent_clock = 7.5
			var lane = -6.0 if int(game.run_time/7.5)%2==0 else 6.0
			game._warn_lane(Vector3(0,0,lane),Vector3.RIGHT,28.0,2.1,1.65,22.0)

func damage_reactor(game: Node3D, amount: float) -> void:
	reactor_health = maxf(0,reactor_health-amount)
	game.reactor_lowest = minf(game.reactor_lowest,reactor_health)
	game.art.reactor_feedback(reactor_health/100.0)
	if reactor_health<=0:
		game.failure_reason = "Reactor containment failed."
		game._finish(false)

func complete() -> bool:
	match kind:
		"salvage": return collected>=target
		"hold": return remaining<=0
		"defend": return reactor_health>0
	return true

func label(game: Node3D) -> String:
	match kind:
		"salvage": return "CACHES %02d / %02d" % [collected,target]
		"hold": return "HOLD %02d:%02d" % [ceili(remaining)/60,ceili(remaining)%60]
		"defend": return "REACTOR %03d%%" % ceili(reactor_health)
	return "%02d / %02d CONTACTS" % [game.wave_kills,game.wave_quota]

func progress(game: Node3D) -> float:
	match kind:
		"salvage": return float(collected)/target
		"hold": return 1.0-remaining/maxf(float(spec.get("duration",1.0)),1.0)
		"defend": return reactor_health/100.0
	return clampf(float(game.wave_kills)/maxi(game.wave_quota,1),0,1)
