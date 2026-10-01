extends RefCounted

const Campaign = preload("res://campaign.gd")

static func tick(game: Node3D, delta: float) -> void:
	for i in range(game.enemies.size()-1,-1,-1):
		if game.phase!="playing": break
		var enemy: Dictionary = game.enemies[i]
		enemy.age += delta
		enemy.fire -= delta
		var target: Vector3 = Vector3.ZERO if enemy.reactor and game.mission.kind=="defend" else game.player.position
		var offset: Vector3 = target-enemy.node.position
		var distance = offset.length()
		var direction = offset.normalized()
		var ranged = enemy.kind in ["spitter","bomber","carrier"]
		var travel = direction
		if ranged and distance<4.5: travel = -direction
		elif ranged and distance<=7: travel = Vector3.ZERO
		if enemy.kind=="skirmisher":
			var side = Vector3(-direction.z,0,direction.x)
			travel = (direction*0.45+side*(1.0 if int(enemy.age/2.0)%2==0 else -1.0)).normalized()
			if fmod(enemy.age,4.8)>4.2: travel = direction*2.6
		if enemy.kind=="mine": travel = direction if distance<5 else Vector3.ZERO
		enemy.node.position = (enemy.node.position+travel*enemy.speed*delta).limit_length(15.0)
		enemy.core.position.y = 0.0 if game.reduced_motion else sin(enemy.age*2.5)*0.06
		if direction.length_squared()>0.01 and enemy.kind not in ["gatekeeper","warden"]:
			var facing = atan2(-direction.x,-direction.z)
			enemy.node.rotation.y = rotate_toward(enemy.node.rotation.y,facing,delta*0.8) if enemy.kind=="sentinel" else facing
		enemy.warning.visible = enemy.kind in ["spitter","skirmisher","sentinel","gatekeeper","carrier","warden"] and enemy.fire<0.65
		if enemy.reactor and game.mission.kind=="defend" and distance<1.8:
			game.mission.damage_reactor(game,12.0 if enemy.kind=="brute" else 9.0)
			game._kill_enemy(i,false)
			continue
		if distance<enemy.radius+0.55 and not enemy.reactor:
			game._hurt(22.0 if enemy.kind=="brute" else 14.0)
			enemy.node.position -= direction*1.2
		if enemy.kind=="sentinel":
			enemy.guard_active = fmod(enemy.age,4.7)<3.2
			var guard: Node3D = enemy.core.get_node_or_null("Guard")
			if guard: guard.visible = enemy.guard_active
		if enemy.kind=="bomber":
			enemy.strike -= delta
			if enemy.strike<=0:
				enemy.strike = 3.4
				game._warn_strike(game.player.position)
		if enemy.kind=="mine" and (distance<2.4 or enemy.age>14):
			game._warn_strike(enemy.node.position)
			game._kill_enemy(i,false)
			continue
		if enemy.kind in Campaign.BOSSES:
			_boss(game,enemy,direction,delta)
		elif enemy.age>0.8 and enemy.fire<=0 and enemy.kind in ["spitter","skirmisher","sentinel"]:
			enemy.fire = 1.8 if enemy.kind=="spitter" else 2.4
			game._fire(enemy.node.position+Vector3.UP*0.55,direction,true,12.0,1,enemy.reactor)

static func _boss(game: Node3D, enemy: Dictionary, direction: Vector3, delta: float) -> void:
	var stage = 3 if enemy.hp<=enemy.max_hp/3.0 else (2 if enemy.hp<=enemy.max_hp*2.0/3.0 else 1)
	if stage!=enemy.stage:
		game.boss_phases_seen += stage-enemy.stage
		enemy.stage = stage
		game.banner = Campaign.ENEMIES[enemy.kind].name.to_upper()+" / "+["CONTAINMENT","PURSUIT","OVERLOAD"][stage-1]
		game.banner_time = 2.5
		enemy.speed = float(Campaign.ENEMIES[enemy.kind].speed)+(stage-1)*0.45
	enemy.strike -= delta
	if enemy.strike<=0 and stage>=2:
		enemy.strike = 3.4 if stage==2 else 2.4
		if enemy.kind=="gatekeeper":
			game._warn_lane(Vector3.ZERO,Vector3.RIGHT,28.0,2.0,1.6,24.0)
			if stage==3: game._warn_lane(Vector3.ZERO,Vector3.FORWARD,28.0,2.0,1.6,24.0)
		elif enemy.kind=="carrier":
			game._warn_strike(game.player.position)
		else:
			game._warn_strike(game.player.position)
			if stage==3: game._warn_lane(game.player.position,Vector3.RIGHT,24.0,1.8,1.6,24.0)
	if enemy.kind=="carrier":
		enemy.deploy -= delta
		if enemy.deploy<=0 and game.enemies.size()<24:
			enemy.deploy = 6.0
			for side in [-1,1]: game._spawn_enemy("skirmisher" if stage>=2 else "seeker",(enemy.node.position+Vector3(side*2.5,0,1.5)).limit_length(14.0),false)
	if enemy.age<=0.8 or enemy.fire>0: return
	enemy.fire = 2.2-stage*0.2
	var origin: Vector3 = enemy.node.position+Vector3.UP*0.55
	if enemy.kind=="carrier":
		for angle in [-0.25,0.0,0.25]: game._fire(origin,direction.rotated(Vector3.UP,angle),true,11.0)
	else:
		var count = 6+stage*2 if enemy.kind=="gatekeeper" else 8+stage*2
		for i in range(count): game._fire(origin,Vector3.FORWARD.rotated(Vector3.UP,i*TAU/count+enemy.age*0.38),true,10.0)
