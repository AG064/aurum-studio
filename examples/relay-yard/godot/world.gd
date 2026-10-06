extends Node3D
const Art = preload("res://art.gd")
const START = Vector3(0, 0.7, 18)
const CAP_POSITIONS = [Vector3(-12, 0.65, 12), Vector3(13, 0.65, 4), Vector3(1, 0.65, -9)]
const STATION_POSITIONS = [Vector3(-14, 0, -3), Vector3(14, 0, -12), Vector3(-3, 0, -23)]
const EXIT = Vector3(10, 0.7, -24)
const NAMES = ["GENERATOR", "COMM ARRAY", "REACTOR"]
## Sodium Enamel art pass: visuals only. Every solid() call, collider size and nav cell is unchanged.
const SODIUM = Color(1.0, 0.6, 0.28)
const LAMPS = [Vector3(-18, 0, 15), Vector3(18, 0, 8), Vector3(-18, 0, -9), Vector3(18, 0, -19)]
var navigation := AStarGrid2D.new()
var caps: Array[Node3D] = []
var station_lights: Array[OmniLight3D] = []
var station_rings: Array[MeshInstance3D] = []
var beacons: Array[Node3D] = []
var exit_ring: MeshInstance3D
## In-world instrument layer: graduated deck gauges, machinery status lamps and status signs.
var station_gauges: Array[MeshInstance3D] = []
var core_gauges: Array[MeshInstance3D] = []
var exit_gauge: MeshInstance3D
var status_lamps: Array = []
var signs: Array[Label3D] = []
var sign_text: Array[String] = ["", "", ""]
var backdrop: WorldEnvironment
var time := 0.0

func _ready() -> void:
	navigation.region = Rect2i(-22, -30, 44, 56)
	navigation.cell_size = Vector2.ONE
	navigation.diagonal_mode = AStarGrid2D.DIAGONAL_MODE_ONLY_IF_NO_OBSTACLES
	navigation.update()
	_lighting()
	_deck()
	_set_dressing()
	_objectives()

func _lighting() -> void:
	backdrop = WorldEnvironment.new()
	var environment := Environment.new()
	environment.background_mode = Environment.BG_COLOR
	environment.background_color = Color(0.018, 0.022, 0.034)
	environment.ambient_light_source = Environment.AMBIENT_SOURCE_COLOR
	environment.ambient_light_color = Color(0.34, 0.38, 0.52)
	environment.ambient_light_energy = 0.55
	environment.fog_enabled = true
	environment.fog_density = 0.006
	environment.fog_light_color = Color(0.06, 0.065, 0.095)
	environment.tonemap_mode = Environment.TONE_MAPPER_FILMIC
	backdrop.environment = environment
	add_child(backdrop)
	var moon := DirectionalLight3D.new()
	moon.rotation_degrees = Vector3(-48, -30, 0)
	moon.light_color = Color(0.64, 0.7, 0.96)
	moon.light_energy = 0.95
	moon.shadow_enabled = true
	moon.directional_shadow_max_distance = 60
	add_child(moon)
	for at in LAMPS:
		var toward := -signf(at.x)
		var lamp := prop("lamp", 1.56, at + Vector3(0, -0.01, 0), 0.0 if toward > 0 else PI)
		lamp.name = "SodiumLamp"
		_light(at + Vector3(toward * 1.05, 3.15, 0), SODIUM, 2.6, 9.0)

func _light(at: Vector3, color: Color, energy: float, radius: float) -> OmniLight3D:
	var light := OmniLight3D.new()
	light.position = at
	light.light_color = color
	light.light_energy = energy
	light.omni_range = radius
	add_child(light)
	return light

func solid(size: Vector3, at: Vector3, visible_color: Color = Color.TRANSPARENT) -> StaticBody3D:
	var body := StaticBody3D.new()
	body.collision_layer = 1
	body.collision_mask = 0
	add_child(body)
	body.position = at
	var shape := BoxShape3D.new()
	shape.size = size
	var collision := CollisionShape3D.new()
	collision.shape = shape
	body.add_child(collision)
	if visible_color.a > 0:
		body.add_child(Art.box(size, visible_color))
	if size.y > 0.45:
		for x in range(floori(at.x - size.x * 0.5 - 0.45), ceili(at.x + size.x * 0.5 + 0.45) + 1):
			for z in range(floori(at.z - size.z * 0.5 - 0.45), ceili(at.z + size.z * 0.5 + 0.45) + 1):
				if navigation.is_in_boundsv(Vector2i(x, z)): navigation.set_point_solid(Vector2i(x, z))
	return body

func prop(key: String, width: float, at: Vector3, angle := 0.0, obstacle := Vector3.ZERO) -> Node3D:
	var model := Art.model(key, width)
	add_child(model)
	model.position = at
	model.rotation.y = angle
	if obstacle != Vector3.ZERO: solid(obstacle, at + Vector3(0, obstacle.y * 0.5, 0))
	return model

func _deck() -> void:
	solid(Vector3(44, 0.45, 56), Vector3(0, -0.3, -2), Color(0.06, 0.06, 0.065))
	var slab := Art.box(Vector3(42, 0.06, 54), Color(0.13, 0.13, 0.12))
	var deck_material := ShaderMaterial.new()
	deck_material.shader = preload("res://deck.gdshader")
	slab.material_override = deck_material
	add_child(slab)
	slab.position = Vector3(0, -0.04, -2)
	# Perimeter: same colliders as before, dressed with 2 m wall modules instead of flat boxes.
	for x in [-21.5, 21.5]:
		solid(Vector3(0.5, 1.8, 56), Vector3(x, 0.8, -2))
		_wall_run(Vector3(x, -0.05, -29), Vector3(0, 0, 2), 28, PI * 0.5)
	for z in [-29.5, 25.5]:
		solid(Vector3(44, 1.8, 0.5), Vector3(0, 0.8, z))
		_wall_run(Vector3(-21, -0.05, z), Vector3(2, 0, 0), 22, 0.0)
	# Three working bays around a clear service lane, marked in enamel paint.
	for at in [Vector3(-13, 0.02, 9), Vector3(13, 0.02, -3), Vector3(-3, 0.02, -20)]:
		var pad := Art.box(Vector3(12, 0.02, 13), Color(0.16, 0.155, 0.15))
		add_child(pad)
		pad.position = at
		_paint_outline(at + Vector3(0, 0.012, 0), Vector2(12, 13), 0.1, Art.PAINT * 0.8)
	for z in range(-25, 22, 3):
		for x in [-3.2, 3.2]:
			var stripe := Art.box(Vector3(0.14, 0.02, 1.6), Art.PAINT)
			add_child(stripe)
			stripe.position = Vector3(x, 0.012, z)
	for at in [Vector3(-14, 0, 17), Vector3(14, 0, 9), Vector3(-8, 0, -19), Vector3(5, 0, -21)]:
		var plate := prop("floor", 3.8, at)
		plate.scale.y = 0.05
		plate.position.y = -0.005
	var bay := Art.floor_paint("DOCK 09", Color(Art.ENAMEL, 0.22), 110)
	add_child(bay)
	bay.position = Vector3(-8.5, 0.03, 19.5)

func _paint_outline(centre: Vector3, extent: Vector2, width: float, color: Color) -> void:
	for side in [-1.0, 1.0]:
		var long_edge := Art.box(Vector3(extent.x, 0.01, width), color)
		add_child(long_edge)
		long_edge.position = centre + Vector3(0, 0, side * (extent.y * 0.5 - width * 0.5))
		var short_edge := Art.box(Vector3(width, 0.01, extent.y), color)
		add_child(short_edge)
		short_edge.position = centre + Vector3(side * (extent.x * 0.5 - width * 0.5), 0, 0)

func _wall_run(origin: Vector3, step: Vector3, count: int, yaw: float) -> void:
	var multimesh := MultiMesh.new()
	multimesh.transform_format = MultiMesh.TRANSFORM_3D
	multimesh.mesh = Art.mesh("wall")
	multimesh.instance_count = count
	for index in count:
		multimesh.set_instance_transform(index, Transform3D(Basis(Vector3.UP, yaw), origin + step * index))
	var node := MultiMeshInstance3D.new()
	node.name = "WallRun"
	node.multimesh = multimesh
	add_child(node)

func _set_dressing() -> void:
	prop("hangar", 8, Vector3(-15, 0, -25), 0, Vector3(6.5, 3, 5.5))
	prop("cargo", 6.5, Vector3(-16, 0.0, 21), -0.3)
	prop("antenna", 4.0, Vector3(18, 0, -26), 0, Vector3(2.5, 2.5, 2.5))
	for at in [Vector3(-8, 0, 9), Vector3(-17, 0, 5), Vector3(9, 0, 0), Vector3(16, 0, -6), Vector3(-7, 0, -12), Vector3(4, 0, -16), Vector3(11, 0, -18)]:
		prop("crate", 2.0, at, 0.15, Vector3(1.9, 1.6, 1.9))
		prop("crate_tall", 1.2, at + Vector3(0.3, 1.8, 0.1), -0.12)
	for at in [Vector3(-18, 0, 0), Vector3(18, 0, 15), Vector3(18, 0, -15), Vector3(-17, 0, -15)]:
		prop("barrel", 2.4, at, 0, Vector3(1.8, 1.5, 1.8))
	for at in [Vector3(-10, 0, 3), Vector3(7, 0, -8), Vector3(-8, 0, -17)]:
		for offset in [-1.25, 1.25]: prop("barrier", 2.45, at + Vector3(offset, 0, 0))
		solid(Vector3(5, 1.1, 0.4), at + Vector3(0, 0.5, 0))
	for z in [-15, -8, 0, 7, 14]: prop("pipe", 3.5, Vector3(20.1, 0.15, z), PI * 0.5)
	for at in [Vector3(-13, 0, 1), Vector3(12, 0, -9), Vector3(-1, 0, -20)]: prop("terminal", 1.7, at, 0, Vector3(1.3, 0.8, 1.0))

func _objectives() -> void:
	for index in 3:
		var cap := Node3D.new()
		cap.name = "Core%d" % index
		add_child(cap)
		cap.position = CAP_POSITIONS[index]
		cap.add_child(Art.model("core", 0.75))
		caps.append(cap)
		var core_gauge := _gauge(CAP_POSITIONS[index], 3.0, 0.78, 24.0)
		core_gauges.append(core_gauge)
		var platform := Art.model("pad", 4.7)
		add_child(platform)
		platform.position = STATION_POSITIONS[index]
		var machine_key: String = ["generator", "transmitter", "reactor"][index]
		var machine := prop(machine_key, 3.4, STATION_POSITIONS[index] + Vector3(0, 0.2, 0))
		var lenses := []
		for mesh_node in machine.find_children("*", "MeshInstance3D", true, false):
			for surface in mesh_node.mesh.get_surface_count():
				var source: Material = mesh_node.get_active_material(surface)
				if source != null and source.resource_name == "status_lamp": lenses.append([mesh_node, surface])
		status_lamps.append(lenses)
		solid(Vector3(2.2, 1.2, 2.2), STATION_POSITIONS[index] + Vector3(0, 0.13 + 0.6, 0))
		var extent: Array = Art.manifest().assets[machine_key].size
		var machine_height := float(extent[1]) * 3.4 / maxf(float(extent[0]), float(extent[2]))
		station_gauges.append(_gauge(STATION_POSITIONS[index], 6.4, 0.84, 48.0))
		station_lights.append(_light(STATION_POSITIONS[index] + Vector3(0, 2.2, 0), Art.COPPER, 1.7, 5.5))
		var sign := Art.label("0%d / %s" % [index + 1, NAMES[index]], Art.ENAMEL, 30)
		add_child(sign)
		signs.append(sign)
		sign.position = STATION_POSITIONS[index] + Vector3(0, maxf(3.5, machine_height + 0.9), 0)
		var beacon := Art.orb(0.14, Art.POWER)
		add_child(beacon)
		beacon.position = CAP_POSITIONS[index] + Vector3(0, 2.2, 0)
		beacons.append(beacon)
	exit_ring = Art.ring(3.2, Art.TEAL)
	add_child(exit_ring)
	exit_ring.position = EXIT - Vector3(0, 0.58, 0)
	exit_ring.scale = Vector3(1, 0.4, 1)
	exit_gauge = _gauge(EXIT, 8.2, 0.86, 44.0)
	for x in [6.7, 13.3]:
		var light := Art.box(Vector3(0.14, 1.4, 0.14), Art.TEAL, true)
		add_child(light)
		light.position = Vector3(x, 0.7, -24)
	var exit_label := Art.label("EXTRACTION", Art.TEAL, 36)
	add_child(exit_label)
	exit_label.position = EXIT + Vector3(0, 3.6, 0)

func _gauge(at: Vector3, diameter: float, inner: float, graduations: float) -> MeshInstance3D:
	var gauge := Art.marking(Art.POWER, 0)
	gauge.material_override.set_shader_parameter("inner", inner)
	gauge.material_override.set_shader_parameter("graduations", graduations)
	add_child(gauge)
	gauge.position = Vector3(at.x, 0.03, at.z)
	gauge.scale = Vector3(diameter, 1, diameter)
	return gauge

static func _set_gauge(gauge: MeshInstance3D, color: Color, fill: float, strength: float) -> void:
	var shader := gauge.material_override as ShaderMaterial
	shader.set_shader_parameter("tint", color)
	shader.set_shader_parameter("fill", clampf(fill, 0.0, 1.0))
	shader.set_shader_parameter("strength", strength)

## Hold progress for the active objective, read from the yard so yard.gd stays unchanged.
func _hold_progress(carried: int) -> float:
	var yard := get_parent()
	if yard == null: return 0.0
	if yard.has_method("dock_state"):
		var state: Dictionary = yard.dock_state()
		return float(state.progress) if state.near else 0.0
	var progress = yard.get("charge_progress")
	if progress == null: return 0.0
	# Baseline yard.gd timings: 0.55 s to secure a core, 1.6 s to connect it.
	return float(progress) / (0.55 if carried < 0 else 1.6)

func update_state(powered: Array, carried: int, active: int, delta: float, reduced_motion: bool) -> void:
	time += delta
	var hold := _hold_progress(carried)
	for index in 3:
		caps[index].visible = not powered[index] and carried != index
		beacons[index].visible = index == active and carried < 0
		beacons[index].position.y = 2.5 + (sin(time * 2.5) * 0.15 if not reduced_motion else 0.0)
		if not reduced_motion: caps[index].rotation.y = time * 0.45
		var online: bool = powered[index]
		var needed := index == active and carried == index
		station_lights[index].light_color = Art.TEAL if online else Art.COPPER
		if online: _set_gauge(station_gauges[index], Art.SIGNAL, 1.0, 0.55)
		elif needed: _set_gauge(station_gauges[index], Art.POWER, hold, 1.0)
		elif index == active: _set_gauge(station_gauges[index], Art.POWER, 0.0, 0.55)
		else: _set_gauge(station_gauges[index], Art.ENAMEL, 0.0, 0.3)
		core_gauges[index].visible = index == active and carried < 0 and not online
		if core_gauges[index].visible: _set_gauge(core_gauges[index], Art.POWER, hold, 1.0)
		var lens := Art.status_lens(Art.SIGNAL, true) if online else Art.status_lens(Art.POWER, index == active)
		for entry in status_lamps[index]: entry[0].set_surface_override_material(entry[1], lens)
		var label := "0%d %s  /  %s" % [index + 1, NAMES[index], "ONLINE" if online else ("CONNECT CORE" if needed else ("AWAITING CORE" if index == active else "OFFLINE"))]
		if label != sign_text[index]:
			sign_text[index] = label
			signs[index].text = label
			signs[index].modulate = Art.SIGNAL if online else (Art.POWER if index == active else Art.ENAMEL * 0.8)
	exit_ring.visible = active == 3
	exit_gauge.visible = active == 3
	if active == 3:
		var yard := get_parent()
		var held = yard.get("extraction") if yard != null else null
		_set_gauge(exit_gauge, Art.SIGNAL, float(held) / 22.0 if held != null else 0.0, 1.0)

func path(from: Vector3, target: Vector3) -> PackedVector2Array:
	var start := Vector2i(roundi(from.x), roundi(from.z))
	var end := Vector2i(roundi(target.x), roundi(target.z))
	if not navigation.is_in_boundsv(start) or not navigation.is_in_boundsv(end): return PackedVector2Array()
	start = _nearest_walkable(start)
	end = _nearest_walkable(end)
	return navigation.get_point_path(start, end)

func _nearest_walkable(point: Vector2i) -> Vector2i:
	if not navigation.is_point_solid(point): return point
	var best := point
	var distance := 100000
	for x in range(point.x - 4, point.x + 5):
		for z in range(point.y - 4, point.y + 5):
			var candidate := Vector2i(x, z)
			if navigation.is_in_boundsv(candidate) and not navigation.is_point_solid(candidate):
				var d := candidate.distance_squared_to(point)
				if d < distance:
					distance = d
					best = candidate
	return best
