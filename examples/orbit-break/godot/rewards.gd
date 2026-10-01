extends RefCounted

# One selection per stop. No prices, submenus, rerolls or separate weapon shop.
static func offer(game: Node3D) -> Array[Dictionary]:
	var evolution = {
		"id":"evolve", "kind":"WEAPON UPGRADE", "title":["Split battery","Rail accelerator","Chain reaction"][game.weapon],
		"body":["Two additional wing shots.","Punch through two more enemies.","Lightning reaches two more targets."][game.weapon],
		"detail":["+25% shot damage", "+40% shot damage", "+35% shot damage"][game.weapon],
		"color":"69f4de", "symbol":game.weapon
	}
	if game.weapon==0 and game.split_shot:
		evolution.title = "Pulse overdrive"
		evolution.body = "Fire 25% more often."
	var hull = {"id":"hull","kind":"SURVIVAL","title":"Repair weave","body":"Restore up to 55 integrity.","detail":"+30 maximum integrity","color":"aac8dc","symbol":3}
	var wingman = {"id":"wingman","kind":"SUPPORT","title":"Wingman drone","body":"An escort fires with you.","detail":"Tracks your position and targets","color":"ffd080","symbol":4}
	var dash = {"id":"afterburner","kind":"MOBILITY","title":"Shock drive","body":"Dashing damages nearby enemies.","detail":"25% shorter dash cooldown","color":"bd93ff","symbol":5}
	if game.wave == 1:
		if game.weapon == 1: return [evolution,weapon_card(2),hull]
		if game.weapon == 2: return [evolution,weapon_card(1),hull]
		return [evolution, weapon_card(1), weapon_card(2)]
	if game.wave == 2:
		return [evolution,hull,wingman]
	if game.wave == 3:
		return [evolution,dash,weapon_card(2 if game.weapon != 2 else 1)]
	var available: Array[Dictionary] = []
	var pool = [hull,wingman,dash,
		{"id":"overclock","kind":"FIREPOWER","title":"Reactor overclock","body":"Fire 30% more often.","detail":"+20% shot damage","color":"ffd080","symbol":6},
		{"id":"magnet","kind":"RECOVERY","title":"Salvage tether","body":"Draw in cells from farther away.","detail":"+2.5 m reach / +5 repairs","color":"aac8dc","symbol":3},
		{"id":"armor","kind":"SURVIVAL","title":"Composite plating","body":"Incoming hits deal 15% less damage.","detail":"+15 hull / stacks with Bastion armour","color":"aac8dc","symbol":3},
		{"id":"siphon","kind":"RECOVERY","title":"Repair nanites","body":"Every defeated contact restores hull.","detail":"Recover 2 integrity on a kill","color":"69f4de","symbol":3},
		{"id":"capacitor","kind":"SYNERGY","title":"Breech capacitor","body":"Dashing charges your next attack.","detail":"Next volley: 1.8x damage","color":"ffd080","symbol":1},
		{"id":"range","kind":"ARMAMENT","title":"Targeting relay","body":"Arc reaches farther. Lance pierces deeper.","detail":"+3 m Arc / +1 Lance pierce","color":"69d7ff","symbol":2},
		{"id":"vector","kind":"MOBILITY","title":"Vector thrusters","body":"Dash comes back 20% sooner.","detail":"20% shorter dash cooldown","color":"bd93ff","symbol":5}]
	if is_instance_valid(game.turret):
		wingman.title = "Escort uplink"
		wingman.body = "Your fitted drone hits harder and fires faster."
		wingman.detail = "+9 damage / 15% faster fire"
	for card in pool:
		if _available(game,card.id) and (card.id!="range" or game.weapon!=0): available.append(card)
	var result: Array[Dictionary] = []
	if game.wave in [4,8]:
		var capstone = {"id":"nova","kind":"BOSS SALVAGE","title":"Ion bloom","body":"Defeated contacts release a damaging burst.","detail":"Chain nearby kills into a blast","color":"69f4de","symbol":0}
		if game.weapon==1: capstone = {"id":"capacitor","kind":"BOSS SALVAGE","title":"Breech capacitor","body":"Dashing charges your next attack.","detail":"Next volley: 1.8x damage","color":"ffd080","symbol":1}
		elif game.weapon==2: capstone = {"id":"storm","kind":"BOSS SALVAGE","title":"Storm lattice","body":"Arc links recover hull as they strike.","detail":"+2 links / 0.75 integrity per contact","color":"69d7ff","symbol":2}
		if _available(game,capstone.id): result.append(capstone)
	if result.is_empty() and _available(game,"evolve"): result.append(evolution)
	if game.health<game.max_health*0.7 and _available(game,"hull") and not result.any(func(c): return c.id=="hull"): result.append(hull)
	var offset = (game.wave*3+game.weapon)%maxi(available.size(),1)
	for i in range(available.size()):
		var card: Dictionary = available[(offset+i)%available.size()]
		if result.size()<3 and not result.any(func(c): return c.id==card.id): result.append(card)
	if result.size()<3 and _available(game,"evolve") and not result.any(func(c): return c.id=="evolve"): result.append(evolution)
	for index in [1,2]:
		if result.size()<3 and index!=game.weapon: result.append(weapon_card(index))
	if result.size()<3: result.append(hull)
	return result

static func _available(game: Node3D, id: String) -> bool:
	var caps = {"evolve":4,"hull":5,"wingman":3,"afterburner":1,"overclock":2,"magnet":2,"armor":2,"siphon":1,"capacitor":1,"range":2,"vector":2,"nova":1,"storm":1}
	return int(game.module_counts.get(id,0))<int(caps.get(id,1))

static func name_for(id: String) -> String:
	return {"evolve":"Weapon calibration","lance":"Lance array","arc":"Arc conductor","hull":"Repair weave","wingman":"Escort uplink","afterburner":"Shock drive","overclock":"Reactor overclock","magnet":"Salvage tether","armor":"Composite plating","siphon":"Repair nanites","capacitor":"Breech capacitor","range":"Targeting relay","vector":"Vector thrusters","nova":"Ion bloom","storm":"Storm lattice"}.get(id,id.capitalize())

static func weapon_card(index: int) -> Dictionary:
	if index == 1:
		return {"id":"lance","kind":"NEW WEAPON","title":"Lance array","body":"Heavy rounds pierce three enemies.","detail":"2.8x hit power. Slower fire.","color":"ffd080","symbol":1}
	return {"id":"arc","kind":"NEW WEAPON","title":"Arc conductor","body":"Lightning chains across three targets.","detail":"10 m range. Auto-targeting.","color":"69d7ff","symbol":2}
