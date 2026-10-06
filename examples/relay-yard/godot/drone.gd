extends CharacterBody3D
const Art = preload("res://art.gd")
var uid := 0
var kind := "sentinel"
var health := 54.0
var cooldown := 1.2
var charge := 0.0
var stunned := 0.0
var anchor := Vector3.ZERO
var time := 0.0
var hurt := 0.0
var model: Node3D
var warning: MeshInstance3D
var health_bar: MeshInstance3D
## Intent shown in the world: a graduated collar that closes as the shot charges, and an aim line.
var collar: MeshInstance3D
var aim_line: MeshInstance3D
var path: PackedVector2Array = PackedVector2Array()
var path_clock := 0.0
var target_at := Vector3.ZERO

func _ready() -> void:
	collision_layer = 4
	collision_mask = 1
	var shape := SphereShape3D.new()
	shape.radius = 0.65 if kind != "warden" else 1.1
	var collision := CollisionShape3D.new()
	collision.shape = shape
	add_child(collision)
	model = Art.model(kind if kind in ["warden", "skirmisher"] else "sentinel", 3.4 if kind == "warden" else 1.7)
	model.position.y = -0.25
	add_child(model)
	var beacon := Art.orb(0.11, Art.RED)
	model.add_child(beacon)
	beacon.position.y = 0.65
	if kind == "warden":
		var title := Art.label("WARDEN", Art.RED, 24)
		add_child(title)
		title.position.y = 1.6
	warning = Art.ring(1.1 if kind != "warden" else 2, Art.RED)
	add_child(warning)
	warning.position.y = -0.50
	warning.visible = false
	collar = Art.marking(Art.HOSTILE, 0)
	collar.material_override.set_shader_parameter("inner", 0.74)
	collar.material_override.set_shader_parameter("graduations", 24.0)
	collar.top_level = true
	add_child(collar)
	collar.scale = Vector3.ONE * (4.6 if kind == "warden" else 2.7)
	collar.visible = false
	aim_line = Art.marking(Art.HOSTILE, 1)
	aim_line.top_level = true
	add_child(aim_line)
	aim_line.visible = false
	health_bar = Art.box(Vector3(1.2, 0.08, 0.08), Art.RED, true)
	add_child(health_bar)
	health_bar.position.y = 1.0
	anchor = position if anchor == Vector3.ZERO else anchor

func step(delta: float, game) -> void:
	time += delta
	stunned = maxf(0, stunned - delta)
	hurt = maxf(0, hurt - delta * 5)
	cooldown = maxf(0, cooldown - delta)
	path_clock -= delta
	var offset: Vector3 = game.player.position - position
	offset.y = 0
	var distance := offset.length()
	var seen: bool = distance < 16 and game.line_clear(position + Vector3(0, 0.3, 0), game.player.position + Vector3(0, 0.25, 0))
	var desired := Vector3.ZERO
	if stunned <= 0:
		var target: Vector3 = game.player.position if distance < 18 else anchor + Vector3(sin(time * 0.4) * 2, 0, cos(time * 0.4) * 2)
		if distance > (6.0 if kind != "warden" else 9.0) or not seen:
			if path_clock <= 0:
				path = game.world.path(position, target)
				path_clock = 0.45
			if path.size() > 1:
				var point: Vector2 = path[1]
				desired = Vector3(point.x - position.x, 0, point.y - position.z).normalized()
			else: desired = (target - position).normalized()
		elif distance < 4:
			desired = -offset.normalized()
		if seen and cooldown <= 0 and charge <= 0:
			charge = 0.8 if kind != "warden" else 1.0
			target_at = game.player.position
		if charge > 0:
			charge -= delta
			desired *= 0.2
			if charge <= 0:
				game.enemy_fire(self, target_at)
				cooldown = 2.1 if kind != "warden" else (1.4 if health < 160 else 2.6)
	var speed := 4.0 if kind == "skirmisher" else (2.7 if kind == "sentinel" else 1.8)
	velocity.x = desired.x * speed
	velocity.z = desired.z * speed
	velocity.y -= 24 * delta
	move_and_slide()
	if offset.length_squared() > 0.01: model.rotation.y = lerp_angle(model.rotation.y, atan2(-offset.x, -offset.z), delta * 6)
	model.position.y = -0.22 + sin(time * 4 + uid) * 0.08
	warning.visible = false
	_show_intent(game)
	var ratio := health / (320.0 if kind == "warden" else (36.0 if kind == "skirmisher" else 54.0))
	health_bar.scale.x = maxf(0.02, ratio)
	health_bar.visible = ratio < 0.99 or kind == "warden"
	model.visible = hurt <= 0 or int(time * 40) % 2 == 0

func _show_intent(game) -> void:
	var total := 1.0 if kind == "warden" else 0.8
	var charging := charge > 0 and stunned <= 0
	collar.visible = charging or stunned > 0
	collar.global_position = Vector3(position.x, 0.04, position.z)
	var shader := collar.material_override as ShaderMaterial
	shader.set_shader_parameter("tint", Art.TEAL if stunned > 0 else Art.RED)
	shader.set_shader_parameter("dashed", 1.0 if stunned > 0 else 0.0)
	shader.set_shader_parameter("fill", clampf(charge / total, 0.0, 1.0))
	var aim := target_at - position
	aim.y = 0
	aim_line.visible = charging and aim.length() > 1.2
	if not aim_line.visible: return
	var direction := aim.normalized()
	var length := clampf(aim.length() + 2.0, 3.0, 12.0)
	var start := position + direction * 1.0
	aim_line.global_transform = Transform3D(Basis.looking_at(direction, Vector3.UP), Vector3(start.x, 0.05, start.z) + direction * length * 0.5)
	aim_line.scale = Vector3(0.34, 1.0, length)
	var line := aim_line.material_override as ShaderMaterial
	line.set_shader_parameter("span", length)
	line.set_shader_parameter("fill", clampf(1.0 - charge / total, 0.0, 1.0))

func snapshot() -> Dictionary:
	return {"uid": uid, "kind": kind, "position": [position.x, position.y, position.z], "velocity": [velocity.x, velocity.y, velocity.z],
		"health": health, "cooldown": cooldown, "charge": charge, "stunned": stunned,
		"anchor": [anchor.x, anchor.y, anchor.z], "target_at": [target_at.x, target_at.y, target_at.z], "time": time}

func restore(data: Dictionary) -> void:
	position = Vector3(data.position[0], data.position[1], data.position[2])
	velocity = Vector3(data.velocity[0], data.velocity[1], data.velocity[2])
	anchor = Vector3(data.anchor[0], data.anchor[1], data.anchor[2])
	target_at = Vector3(data.target_at[0], data.target_at[1], data.target_at[2])
	for key in ["health", "cooldown", "charge", "stunned", "time"]: set(key, data[key])
	cooldown = maxf(0, cooldown)
