extends CharacterBody3D
const Art = preload("res://art.gd")
const DASH_RECHARGE_SECONDS = 1.3
const EMP_RECHARGE_SECONDS = 7.0
const DASH_ACCEL = 260.0
const DASH_EXIT_SPEED = 1.4
const HEAT_RELEASE_THRESHOLD = 0.25
var health := 100.0
var heat := 0.0
var overheated := false
var dash_cooldown := 0.0
var emp_cooldown := 0.0
var invulnerable := 0.0
var fire_cooldown := 0.0
var dash_time := 0.0
var dash_direction := Vector3.FORWARD
var facing := Vector3.FORWARD
var model: Node3D
var cargo: Node3D
var time := 0.0
var recoil := 0.0
var damage_flash := 0.0
var travelled := 0.0
var damage_taken := 0.0
var dash_count := 0
var emp_count := 0
var shots_fired := 0
var thrusters: Array[MeshInstance3D] = []

func _ready() -> void:
	name = "Courier"
	collision_layer = 2
	collision_mask = 1
	var shape := CapsuleShape3D.new()
	shape.radius = 0.48
	shape.height = 1.2
	var collision := CollisionShape3D.new()
	collision.shape = shape
	add_child(collision)
	model = Art.model("courier", 1.9)
	model.position.y = -0.25
	add_child(model)
	cargo = Art.model("core", 0.62)
	model.add_child(cargo)
	cargo.position = Vector3(0, 0.65, 0.18)
	cargo.visible = false
	for x in [-0.45, 0.45]:
		var exhaust := Art.orb(0.09, Art.TEAL)
		model.add_child(exhaust)
		exhaust.position = Vector3(x, 0.10, 0.78)
		thrusters.append(exhaust)

func step(delta: float, direction: Vector3, aim: Vector3, speed: float, carrying: bool, reduced_motion: bool) -> void:
	time += delta
	dash_cooldown = maxf(0, dash_cooldown - delta)
	emp_cooldown = maxf(0, emp_cooldown - delta)
	invulnerable = maxf(0, invulnerable - delta)
	fire_cooldown = maxf(0, fire_cooldown - delta)
	var was_dashing := dash_time > 0
	dash_time = maxf(0, dash_time - delta)
	heat = maxf(0, heat - delta * 0.32)
	if overheated and heat < HEAT_RELEASE_THRESHOLD: overheated = false
	recoil = maxf(0, recoil - delta * 5)
	damage_flash = maxf(0, damage_flash - delta * 4)
	var desired := direction * speed * (0.76 if carrying else 1.0)
	var acceleration := 36.0
	if dash_time > 0:
		desired = dash_direction * speed * 3.0
		acceleration = DASH_ACCEL
	elif was_dashing:
		var carry := Vector2(velocity.x, velocity.z).limit_length(speed * DASH_EXIT_SPEED)
		velocity.x = carry.x
		velocity.z = carry.y
	velocity.x = move_toward(velocity.x, desired.x, delta * acceleration)
	velocity.z = move_toward(velocity.z, desired.z, delta * acceleration)
	velocity.y -= 24.0 * delta
	var before := position
	move_and_slide()
	travelled += Vector2(position.x - before.x, position.z - before.z).length()
	if aim.length_squared() > 0.01: facing = aim.normalized()
	model.rotation.y = lerp_angle(model.rotation.y, atan2(-facing.x, -facing.z), minf(1, delta * 18))
	model.position.y = -0.25 + (sin(time * 5) * 0.045 if not reduced_motion else 0.0)
	model.rotation.z = lerpf(model.rotation.z, -velocity.x * 0.012 if not reduced_motion else 0.0, delta * 8)
	model.rotation.x = lerpf(model.rotation.x, velocity.z * 0.008 - recoil * 0.05 if not reduced_motion else 0.0, delta * 8)
	cargo.visible = carrying
	for exhaust in thrusters: exhaust.scale = Vector3.ONE * (1.1 + desired.length() * 0.08)
	model.visible = damage_flash <= 0 or reduced_motion or int(time * 30) % 2 == 0

func dash(direction: Vector3) -> bool:
	if dash_cooldown > 0 or health <= 0: return false
	dash_direction = direction.normalized() if direction.length_squared() > 0.01 else facing
	dash_time = 0.18
	dash_cooldown = DASH_RECHARGE_SECONDS
	invulnerable = 0.24
	dash_count += 1
	return true

func emp() -> bool:
	if emp_cooldown > 0 or health <= 0: return false
	emp_cooldown = EMP_RECHARGE_SECONDS
	emp_count += 1
	return true

func fire() -> bool:
	if fire_cooldown > 0 or overheated or health <= 0: return false
	fire_cooldown = 0.14
	heat = minf(1, heat + 0.105)
	overheated = heat >= 0.98
	recoil = 1
	shots_fired += 1
	return true

func hit(amount: float) -> bool:
	if invulnerable > 0 or health <= 0: return false
	health = maxf(0, health - amount)
	damage_taken += amount
	invulnerable = 0.32
	damage_flash = 1
	return true

func snapshot() -> Dictionary:
	return {"position": [position.x, position.y, position.z], "velocity": [velocity.x, velocity.y, velocity.z],
		"facing": [facing.x, facing.y, facing.z], "health": health, "heat": heat, "overheated": overheated,
		"dash_cooldown": dash_cooldown, "emp_cooldown": emp_cooldown, "invulnerable": invulnerable,
		"fire_cooldown": fire_cooldown, "dash_time": dash_time, "dash_direction": [dash_direction.x, dash_direction.y, dash_direction.z],
		"travelled": travelled, "damage_taken": damage_taken, "dash_count": dash_count, "emp_count": emp_count, "shots_fired": shots_fired}

func restore(data: Dictionary) -> void:
	position = Vector3(data.position[0], data.position[1], data.position[2])
	velocity = Vector3(data.velocity[0], data.velocity[1], data.velocity[2])
	facing = Vector3(data.facing[0], data.facing[1], data.facing[2])
	dash_direction = Vector3(data.dash_direction[0], data.dash_direction[1], data.dash_direction[2])
	for key in ["health", "heat", "overheated", "dash_cooldown", "emp_cooldown", "invulnerable", "fire_cooldown", "dash_time", "travelled", "damage_taken", "dash_count", "emp_count", "shots_fired"]: set(key, data[key])
