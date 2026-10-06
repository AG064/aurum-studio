extends SceneTree
## Relay Yard asset forge: "Sodium Enamel" model set.
## Builds every runtime model procedurally and exports binary glTF (.glb) plus manifest.json.
##   godot --headless --path . --script res://tools/asset_forge.gd -- --out res://assets/models_yard
## Conventions: metres, +Y up, front faces -Z, pivot at the ground centre (lowest point at y = 0).
## Shared palette, two small authored textures, no mesh compression, named reusable surfaces.
const Mesher = preload("res://tools/forge_mesher.gd")
const OUT_DEFAULT = "res://assets/models_yard"
## key -> [builder, intended runtime width in metres (matches Art.model calls)]
const MODELS = {
	"courier": ["_courier", 1.9], "sentinel": ["_sentinel", 1.7], "skirmisher": ["_skirmisher", 1.7],
	"warden": ["_warden", 3.4], "core": ["_core", 0.75], "generator": ["_generator", 3.4],
	"transmitter": ["_transmitter", 3.4], "reactor": ["_reactor", 3.4], "antenna": ["_antenna", 4.0],
	"cargo": ["_cargo", 6.5], "hangar": ["_hangar", 8.0], "barrel": ["_barrel", 2.4], "pipe": ["_pipe", 3.5],
	"crate": ["_crate", 2.0], "crate_tall": ["_spool", 1.2], "terminal": ["_terminal", 1.7], "floor": ["_hatch", 3.8],
	"barrier": ["_barrier", 2.45], "lamp": ["_lamp", 1.56], "pad": ["_pad", 4.7], "wall": ["_wall", 2.0]
}
var mats := {}

func _initialize() -> void:
	_palette()
	var out := OUT_DEFAULT
	var args := OS.get_cmdline_user_args()
	var at := args.find("--out")
	if at >= 0 and at + 1 < args.size(): out = args[at + 1]
	DirAccess.make_dir_recursive_absolute(ProjectSettings.globalize_path(out))
	var manifest := {"version": 2, "style": "sodium-enamel", "units": "metres", "up": "+Y", "forward": "-Z", "pivot": "ground-centre",
		"generator": "res://tools/asset_forge.gd", "materials": mats.keys(), "assets": {}}
	var total := 0
	for key in MODELS:
		var m = Mesher.new(mats)
		call(MODELS[key][0], m)
		var mesh: ArrayMesh = m.commit(key)
		var box := mesh.get_aabb()
		if absf(box.position.y) > 0.002: push_warning("%s lowest point is %.3f, expected 0" % [key, box.position.y])
		var root := Node3D.new()
		root.name = key
		var node := MeshInstance3D.new()
		node.name = key + "_mesh"
		node.mesh = mesh
		root.add_child(node)
		node.owner = root
		var doc := GLTFDocument.new()
		var state := GLTFState.new()
		var err := doc.append_from_scene(root, state)
		if err == OK: err = doc.write_to_filesystem(state, "%s/%s.glb" % [out, key])
		root.free()
		if err != OK:
			push_error("export failed for %s: %s" % [key, err])
			quit(1)
			return
		var model_path := "%s/%s.glb" % [out, key]
		var exported := FileAccess.open(model_path, FileAccess.READ)
		if exported == null:
			push_error("cannot inspect exported model: %s" % model_path)
			quit(1)
			return
		var exported_bytes := exported.get_length()
		exported.close()
		var used := []
		for index in mesh.get_surface_count(): used.append(mesh.surface_get_name(index))
		manifest.assets[key] = {"file": "%s.glb" % key, "size": [snappedf(box.size.x, 0.001), snappedf(box.size.y, 0.001), snappedf(box.size.z, 0.001)],
			"min": [snappedf(box.position.x, 0.001), snappedf(box.position.y, 0.001), snappedf(box.position.z, 0.001)],
			"runtime_width": MODELS[key][1], "triangles": m.triangles, "materials": used,
			"bytes": exported_bytes, "sha256": FileAccess.get_sha256(model_path)}
		total += m.triangles
		print("%-12s %5d tris  size %s" % [key, m.triangles, str(manifest.assets[key].size)])
	manifest.assets["drone"] = manifest.assets["sentinel"].duplicate(true)
	manifest.assets["drone"]["file"] = "sentinel.glb"
	manifest.assets["drone"]["alias_of"] = "sentinel"
	var file := FileAccess.open(out + "/manifest.json", FileAccess.WRITE)
	if file == null:
		push_error("cannot write model manifest")
		quit(1)
		return
	file.store_string(JSON.stringify(manifest, "\t"))
	file.close()
	print("total %d triangles" % total)
	_textures(out.get_base_dir() + "/textures_yard")
	quit()

func _mat(name: String, color: Color, roughness: float, metallic := 0.0, glow := false, emission := Color.BLACK) -> void:
	var material := StandardMaterial3D.new()
	material.resource_name = name
	material.albedo_color = color
	material.roughness = roughness
	material.metallic = metallic
	if glow:
		var light := emission if emission != Color.BLACK else color
		material.emission_enabled = true
		# Godot 4.7's glTF exporter writes emissiveFactor through linear_to_srgb and the importer
		# converts again. Pre-linearising twice stores the spec-correct linear factor (srgb_to_linear
		# of the colour) and re-imports as exactly `color`. Verified by a round trip in 4.7.
		material.emission = light.srgb_to_linear().srgb_to_linear()
	mats[name] = material

func _palette() -> void:
	_mat("enamel", Color(0.89, 0.86, 0.78), 0.42)
	_mat("cobalt", Color(0.15, 0.29, 0.78), 0.38, 0.1)
	_mat("graphite", Color(0.12, 0.13, 0.15), 0.55, 0.35)
	_mat("slate", Color(0.27, 0.30, 0.33), 0.62, 0.35)
	_mat("steel", Color(0.55, 0.57, 0.60), 0.32, 0.85)
	_mat("concrete", Color(0.40, 0.39, 0.37), 0.92)
	_mat("oxide", Color(0.52, 0.21, 0.13), 0.68, 0.2)
	_mat("hazard", Color(0.96, 0.69, 0.15), 0.5)
	_mat("vermilion", Color(0.85, 0.21, 0.11), 0.45)
	_mat("glass", Color(0.05, 0.09, 0.17), 0.08, 0.6)
	# Lenses: a dark lit base plus moderate emission, so they read as lamps rather than white blobs.
	_mat("glow_power", Color(0.42, 0.25, 0.08), 0.25, 0.0, true, Color(0.78, 0.46, 0.16))
	_mat("glow_signal", Color(0.1, 0.17, 0.34), 0.25, 0.0, true, Color(0.3, 0.5, 0.88))
	_mat("glow_hostile", Color(0.3, 0.06, 0.03), 0.25, 0.0, true, Color(0.72, 0.16, 0.08))
	# Station status lenses; world.gd swaps this material per station (offline, needed, online).
	_mat("status_lamp", Color(0.28, 0.18, 0.08), 0.2, 0.0, true, Color(0.42, 0.25, 0.09))

static func T(pos := Vector3.ZERO, rot_deg := Vector3.ZERO, scale := Vector3.ONE) -> Transform3D:
	return Transform3D(Basis.from_euler(rot_deg * (PI / 180.0)) * Basis.from_scale(scale), pos)

## Mirror across X (left/right symmetric parts).
static func MX(xf: Transform3D) -> Transform3D:
	return Transform3D(Basis.from_scale(Vector3(-1, 1, 1)), Vector3.ZERO) * xf

static func P(points: Array) -> PackedVector2Array:
	return PackedVector2Array(points)

## Basis whose local +Y points along `dir`.
static func axis(dir: Vector3) -> Basis:
	var y := dir.normalized()
	var helper := Vector3.UP if absf(y.dot(Vector3.UP)) < 0.95 else Vector3.RIGHT
	var x := y.cross(helper).normalized()
	return Basis(x, y, x.cross(y).normalized())

## Indicator lamp: steel bezel ring and a domed lens facing `dir`. Shared by ships and dock equipment.
func lamp(m, at: Vector3, dir: Vector3, radius: float, lens := "status_lamp") -> void:
	var b := axis(dir)
	m.torus("steel", radius * 1.15, radius * 0.22, 10, 5, Transform3D(b, at))
	m.lathe(lens, [Vector2(0, -0.01), Vector2(radius, -0.01), Vector2(radius * 0.9, radius * 0.35), Vector2(0, radius * 0.5)], 10, Transform3D(b, at))

## Hex fastener heads on a surface facing `dir`.
func bolts(m, points: Array, dir: Vector3, radius := 0.025) -> void:
	var b := axis(dir)
	for at in points:
		m.lathe("steel", [Vector2(0, 0), Vector2(radius, 0), Vector2(radius, radius * 0.6), Vector2(0, radius * 0.7)], 6, Transform3D(b, at), false)

## Instrument head: graphite housing, enamel face, three status lamps and a small dial with a needle.
## Local frame: face points -Z, base at y = 0. The same plate-and-lamp language as the HUD and terminal.
func status_head(m, xf: Transform3D) -> void:
	m.box("graphite", Vector3(0.62, 0.36, 0.22), xf * T(Vector3(0, 0.18, 0)), 0.03)
	m.box("enamel", Vector3(0.54, 0.28, 0.03), xf * T(Vector3(0, 0.19, -0.115)), 0.008)
	for index in 3:
		lamp(m, xf * Vector3(-0.17 + index * 0.085, 0.25, -0.135), xf.basis * Vector3(0, 0, -1), 0.026)
	m.lathe("graphite", [Vector2(0, 0), Vector2(0.075, 0), Vector2(0.075, 0.012), Vector2(0, 0.012)], 12, xf * Transform3D(axis(Vector3(0, 0, -1)), Vector3(0.15, 0.17, -0.13)))
	m.box("vermilion", Vector3(0.008, 0.06, 0.006), xf * T(Vector3(0.165, 0.19, -0.145), Vector3(0, 0, -35)))
	bolts(m, [xf * Vector3(-0.24, 0.07, -0.13), xf * Vector3(0.24, 0.07, -0.13)], xf.basis * Vector3(0, 0, -1), 0.014)

# ---------------------------------------------------------------- vehicles

## Courier skiff: enamel teardrop pod inside a cobalt orbit ring. Reads as a ring from the follow camera.
func _courier(m) -> void:
	var along_z := Basis.from_euler(Vector3(-PI * 0.5, 0, 0))
	var flat := func(pos: Vector3, squash: float) -> Transform3D: return Transform3D(along_z * Basis.from_scale(Vector3(1, 1, squash)), pos)
	m.box("graphite", Vector3(0.46, 0.08, 1.2), T(Vector3(0, 0.04, 0.05)), 0.03)
	m.lathe("enamel", [Vector2(0, -0.95), Vector2(0.3, -0.85), Vector2(0.43, -0.5), Vector2(0.46, 0.0), Vector2(0.41, 0.5), Vector2(0.23, 0.85), Vector2(0, 1.0)], 16, flat.call(Vector3(0, 0.36, 0), 0.6))
	m.lathe("cobalt", [Vector2(0.44, -0.2), Vector2(0.475, -0.2), Vector2(0.482, 0.02), Vector2(0.455, 0.02), Vector2(0.44, -0.2)], 16, flat.call(Vector3(0, 0.36, 0), 0.6), true, 0.0, TAU, 60.0)
	m.lathe("glass", [Vector2(0, -0.34), Vector2(0.15, -0.3), Vector2(0.22, -0.1), Vector2(0.2, 0.12), Vector2(0.1, 0.28), Vector2(0, 0.32)], 12, flat.call(Vector3(0, 0.6, -0.32), 0.7))
	m.torus("cobalt", 0.88, 0.06, 28, 8, T(Vector3(0, 0.36, 0.05)))
	for side in [-1.0, 1.0]:
		m.box("graphite", Vector3(0.44, 0.07, 0.16), T(Vector3(side * 0.63, 0.36, 0.05)), 0.02)
		m.lathe("graphite", [Vector2(0, -0.22), Vector2(0.12, -0.2), Vector2(0.135, 0.12), Vector2(0.09, 0.2), Vector2(0, 0.22)], 10, flat.call(Vector3(side * 0.44, 0.16, 0.63), 1.0))
		m.lathe("glow_signal", [Vector2(0, 0), Vector2(0.085, 0), Vector2(0.085, 0.02), Vector2(0, 0.02)], 10, Transform3D(Basis.from_euler(Vector3(PI * 0.5, 0, 0)), Vector3(side * 0.44, 0.16, 0.84)))
		m.prism("cobalt", P([Vector2(0.42, 0.55), Vector2(0.82, 0.56), Vector2(0.92, 0.84), Vector2(0.72, 0.84)]), 0.05, T(Vector3(side * 0.17, 0, 0)), "x")
		m.box("steel", Vector3(0.05, 0.07, 0.38), T(Vector3(side * 0.19, 0.64, 0.18)), 0.01)
	m.box("glow_signal", Vector3(0.2, 0.035, 0.04), T(Vector3(0, 0.37, -0.99)))
	m.box("graphite", Vector3(0.12, 0.05, 0.42), T(Vector3(0, 0.655, -0.02), Vector3(-4, 0, 0)), 0.012)
	for index in 3: lamp(m, Vector3(0, 0.685, -0.17 + index * 0.1), Vector3(0, 1, -0.1), 0.022, "glow_signal")
	for index in 12:
		var angle := TAU * index / 12.0
		var major := index % 3 == 0
		m.box("enamel", Vector3(0.05 if major else 0.03, 0.03, 0.11 if major else 0.07), Transform3D(Basis.looking_at(Vector3(cos(angle), 0, sin(angle)), Vector3.UP), Vector3(cos(angle), 0, sin(angle)) * 0.88 + Vector3(0, 0.42, 0.05)))

## Sentinel "lantern": faceted graphite body, a single hostile eye band, two broad ear plates.
func _sentinel(m) -> void:
	m.torus("steel", 0.2, 0.035, 12, 6, T(Vector3(0, 0.035, 0)))
	m.lathe("graphite", [Vector2(0, 0.08), Vector2(0.22, 0.1), Vector2(0.42, 0.26), Vector2(0.5, 0.48), Vector2(0.46, 0.68), Vector2(0.3, 0.85), Vector2(0, 0.9)], 10, T(Vector3.ZERO, Vector3(0, 18, 0)), false)
	m.lathe("vermilion", [Vector2(0.43, 0.27), Vector2(0.475, 0.29), Vector2(0.49, 0.34), Vector2(0.445, 0.33), Vector2(0.43, 0.27)], 10, T(Vector3.ZERO, Vector3(0, 18, 0)), false)
	m.lathe("glow_hostile", [Vector2(0.47, 0.44), Vector2(0.515, 0.44), Vector2(0.515, 0.56), Vector2(0.47, 0.56), Vector2(0.47, 0.44)], 6, T(), false, deg_to_rad(215), deg_to_rad(325))
	for side in [-1.0, 1.0]:
		var at := Transform3D(Basis.IDENTITY, Vector3(side * 0.79, 0, 0))
		m.prism("graphite", P([Vector2(-0.3, 0.16), Vector2(0.24, 0.2), Vector2(0.36, 0.5), Vector2(0.2, 0.84), Vector2(-0.2, 0.8), Vector2(-0.38, 0.46)]), 0.08, at, "x")
		m.prism("vermilion", P([Vector2(-0.38, 0.46), Vector2(-0.3, 0.16), Vector2(-0.24, 0.18), Vector2(-0.32, 0.46), Vector2(-0.16, 0.76), Vector2(-0.2, 0.8)]), 0.1, at, "x")
		m.box("slate", Vector3(0.34, 0.09, 0.16), T(Vector3(side * 0.6, 0.5, 0)), 0.02)
	for index in 3: m.box("slate", Vector3(0.36, 0.025, 0.045), T(Vector3(0, 0.875, -0.1 + index * 0.08), Vector3(-6, 0, 0)), 0.006)
	m.lathe("graphite", [Vector2(0.505, 0.42), Vector2(0.55, 0.42), Vector2(0.55, 0.58), Vector2(0.505, 0.58), Vector2(0.505, 0.42)], 6, T(), false, deg_to_rad(205), deg_to_rad(215))
	m.lathe("graphite", [Vector2(0.505, 0.42), Vector2(0.55, 0.42), Vector2(0.55, 0.58), Vector2(0.505, 0.58), Vector2(0.505, 0.42)], 6, T(), false, deg_to_rad(325), deg_to_rad(335))
	m.tube("steel", Vector3(0.0, 0.86, 0.12), Vector3(0.1, 1.12, 0.24), 0.016, 6)
	m.lathe("glow_hostile", [Vector2(0, 0), Vector2(0.035, 0), Vector2(0.035, 0.05), Vector2(0, 0.05)], 6, T(Vector3(0.1, 1.12, 0.24)))

## Skirmisher "dart": a flat swept chevron with vermilion leading edges. Fast, sharp, unmistakable.
func _skirmisher(m) -> void:
	var along_z := Basis.from_euler(Vector3(-PI * 0.5, 0, 0))
	m.prism("graphite", P([Vector2(0, -0.95), Vector2(0.18, -0.55), Vector2(0.82, 0.38), Vector2(0.78, 0.56), Vector2(0.42, 0.42), Vector2(0.2, 0.62), Vector2(-0.2, 0.62), Vector2(-0.42, 0.42), Vector2(-0.78, 0.56), Vector2(-0.82, 0.38), Vector2(-0.18, -0.55)]), 0.08, T(Vector3(0, 0.08, 0)), "y")
	m.lathe("slate", [Vector2(0, -0.62), Vector2(0.13, -0.52), Vector2(0.19, 0.0), Vector2(0.15, 0.5), Vector2(0, 0.92)], 10, Transform3D(along_z * Basis.from_scale(Vector3(1, 1, 0.72)), Vector3(0, 0.15, 0)))
	for side in [-1.0, 1.0]:
		m.prism("vermilion", P([Vector2(side * 0.18, -0.6), Vector2(side * 0.88, 0.36), Vector2(side * 0.8, 0.43), Vector2(side * 0.13, -0.48)]), 0.1, T(Vector3(0, 0.08, 0)), "y")
		m.prism("graphite", P([Vector2(0.18, 0.11), Vector2(0.56, 0.11), Vector2(0.6, 0.42), Vector2(0.44, 0.42)]), 0.04, T(Vector3(side * 0.4, 0, 0)), "x")
	m.box("glow_hostile", Vector3(0.18, 0.04, 0.12), T(Vector3(0, 0.27, -0.42), Vector3(-12, 0, 0)))
	m.torus("glow_hostile", 0.11, 0.025, 10, 6, T(Vector3(0, 0.025, 0.2)))

## Warden "bulwark": octagonal armoured hull behind a curved front shield, three eyes for its fan.
func _warden(m) -> void:
	for x in [-0.82, 0.82]:
		for z in [-0.6, 0.6]:
			m.lathe("graphite", [Vector2(0, 0.0), Vector2(0.2, 0.0), Vector2(0.28, 0.12), Vector2(0.26, 0.36), Vector2(0, 0.38)], 8, T(Vector3(x, 0, z)), false)
			m.torus("glow_hostile", 0.17, 0.03, 10, 6, T(Vector3(x, 0.03, z)))
	m.lathe("graphite", [Vector2(0, 0.22), Vector2(0.86, 0.25), Vector2(1.0, 0.52), Vector2(0.96, 1.18), Vector2(0.72, 1.44), Vector2(0.3, 1.6), Vector2(0, 1.62)], 8, T(Vector3.ZERO, Vector3(0, 22.5, 0)), false)
	m.lathe("slate", [Vector2(0.94, 0.52), Vector2(1.02, 0.54), Vector2(1.0, 0.98), Vector2(0.95, 0.98), Vector2(0.94, 0.52)], 8, T(Vector3.ZERO, Vector3(0, 22.5, 0)), false)
	var shield_from := deg_to_rad(206)
	var shield_to := deg_to_rad(334)
	m.lathe("graphite", [Vector2(1.18, 0.18), Vector2(1.42, 0.18), Vector2(1.44, 1.3), Vector2(1.18, 1.2), Vector2(1.18, 0.18)], 8, T(), false, shield_from, shield_to)
	m.lathe("vermilion", [Vector2(1.18, 1.2), Vector2(1.44, 1.3), Vector2(1.44, 1.4), Vector2(1.18, 1.3), Vector2(1.18, 1.2)], 8, T(), false, shield_from, shield_to)
	m.lathe("vermilion", [Vector2(1.43, 0.42), Vector2(1.46, 0.42), Vector2(1.46, 0.52), Vector2(1.43, 0.52), Vector2(1.43, 0.42)], 8, T(), false, shield_from, shield_to)
	for degrees in [250.0, 270.0, 290.0]:
		var angle := deg_to_rad(degrees)
		var face := Vector3(cos(angle), 0, sin(angle))
		m.box("glow_hostile", Vector3(0.2, 0.12, 0.06), Transform3D(Basis.looking_at(face, Vector3.UP), face * 0.94 + Vector3(0, 1.36, 0)))
	for side in [-1.0, 1.0]:
		m.tube("steel", Vector3(side * 0.42, 1.2, 0.55), Vector3(side * 0.5, 1.95, 0.72), 0.12, 8)
		m.lathe("glow_hostile", [Vector2(0, 0), Vector2(0.1, 0), Vector2(0.1, 0.03), Vector2(0, 0.03)], 8, T(Vector3(side * 0.5, 1.95, 0.72)))
	m.torus("steel", 0.5, 0.06, 20, 6, T(Vector3(0, 0.98, 0.98), Vector3(90, 0, 0)))
	for index in 8:
		var angle := deg_to_rad(22.5 + 45.0 * index)
		var out := Vector3(cos(angle), 0, sin(angle))
		m.box("slate", Vector3(0.05, 0.62, 0.05), Transform3D(Basis.looking_at(out, Vector3.UP), out * 0.995 + Vector3(0, 0.86, 0)), 0.01)
	for index in 7:
		var angle := lerpf(shield_from, shield_to, (index + 0.5) / 7.0)
		var out := Vector3(cos(angle), 0, sin(angle))
		m.box("slate", Vector3(0.06, 0.96, 0.05), Transform3D(Basis.looking_at(out, Vector3.UP), out * 1.455 + Vector3(0, 0.72, 0)), 0.01)
	var bolt_row := []
	for index in 9:
		var angle := lerpf(shield_from, shield_to, (index + 0.5) / 9.0)
		bolt_row.append(Vector3(cos(angle), 0, sin(angle)) * 1.31 + Vector3(0, 1.37, 0))
	bolts(m, bolt_row, Vector3.UP, 0.03)
	m.tube("graphite", Vector3(0, 1.74, 0.1), Vector3(0, 2.05, 0.1), 0.05, 8)
	lamp(m, Vector3(0, 2.07, 0.1), Vector3(0, 1, -0.3), 0.06, "glow_hostile")
	m.lathe("vermilion", [Vector2(0, 1.6), Vector2(0.22, 1.6), Vector2(0.18, 1.72), Vector2(0, 1.76)], 8, T())

# ---------------------------------------------------------------- objectives and machinery

## Sodium cell: the carried power core. Amber window inside a steel cage, graphite caps, top handle.
func _core(m) -> void:
	m.lathe("graphite", [Vector2(0, 0), Vector2(0.3, 0), Vector2(0.32, 0.04), Vector2(0.32, 0.14), Vector2(0, 0.14)], 12, T(), true, 0.0, TAU, 50.0)
	m.lathe("hazard", [Vector2(0.322, 0.04), Vector2(0.33, 0.04), Vector2(0.33, 0.1), Vector2(0.322, 0.1), Vector2(0.322, 0.04)], 12, T(), false)
	m.lathe("glow_power", [Vector2(0, 0.14), Vector2(0.24, 0.14), Vector2(0.24, 0.72), Vector2(0, 0.72)], 12, T())
	for index in 6:
		var angle := TAU * index / 6.0 + 0.26
		var r := Vector3(cos(angle), 0, sin(angle)) * 0.29
		m.tube("steel", r + Vector3(0, 0.12, 0), r + Vector3(0, 0.74, 0), 0.022, 6)
	m.torus("steel", 0.29, 0.028, 16, 6, T(Vector3(0, 0.43, 0)))
	m.lathe("graphite", [Vector2(0, 0.72), Vector2(0.32, 0.72), Vector2(0.32, 0.8), Vector2(0.26, 0.86), Vector2(0, 0.86)], 12, T(), true, 0.0, TAU, 50.0)
	m.torus("steel", 0.15, 0.024, 10, 6, T(Vector3(0, 0.86, 0), Vector3(90, 0, 0)), PI, TAU)

## Generator bay: an enamel turbine drum with radial fins and an amber core socket facing the camera.
func _generator(m) -> void:
	m.lathe("concrete", [Vector2(0, 0), Vector2(1.55, 0), Vector2(1.5, 0.22), Vector2(0, 0.22)], 8, T(Vector3.ZERO, Vector3(0, 22.5, 0)), false)
	m.lathe("enamel", [Vector2(0, 0.22), Vector2(1.12, 0.22), Vector2(1.15, 0.3), Vector2(1.15, 1.12), Vector2(1.08, 1.2), Vector2(0, 1.2)], 20, T())
	m.lathe("hazard", [Vector2(1.148, 0.4), Vector2(1.17, 0.4), Vector2(1.17, 0.54), Vector2(1.148, 0.54), Vector2(1.148, 0.4)], 20, T())
	m.torus("graphite", 1.0, 0.05, 24, 6, T(Vector3(0, 1.3, 0)))
	for index in 12:
		var angle := TAU * index / 12.0
		m.box("graphite", Vector3(0.6, 0.26, 0.07), Transform3D(Basis.from_euler(Vector3(0, -angle, 0.0)) * Basis.from_euler(Vector3(0.35, 0, 0)), Vector3(cos(angle), 0, sin(angle)) * 0.72 + Vector3(0, 1.3, 0)), 0.015)
	m.lathe("steel", [Vector2(0, 1.2), Vector2(0.42, 1.2), Vector2(0.42, 1.36), Vector2(0.3, 1.52), Vector2(0, 1.58)], 12, T())
	var facing := deg_to_rad(53.0)
	var out := Vector3(cos(facing), 0, sin(facing))
	m.box("graphite", Vector3(0.62, 0.66, 0.12), Transform3D(Basis.looking_at(-out, Vector3.UP), out * 1.13 + Vector3(0, 0.74, 0)), 0.03)
	m.box("glow_power", Vector3(0.4, 0.46, 0.06), Transform3D(Basis.looking_at(-out, Vector3.UP), out * 1.18 + Vector3(0, 0.74, 0)))
	for degrees in [180.0, 215.0]:
		var angle := deg_to_rad(degrees)
		var dir := Vector3(cos(angle), 0, sin(angle))
		m.tube("steel", dir * 1.12 + Vector3(0, 0.95, 0), dir * 1.42 + Vector3(0, 0.22, 0), 0.07, 8)
	var head_angle := deg_to_rad(95.0)
	var head_out := Vector3(cos(head_angle), 0, sin(head_angle))
	status_head(m, Transform3D(Basis.looking_at(-head_out, Vector3.UP), head_out * 1.3 + Vector3(0, 0.22, 0)))

## Comm array: graphite plinth, enamel cabinet, mast and a dish tilted toward the follow camera.
func _transmitter(m) -> void:
	m.box("graphite", Vector3(2.2, 0.36, 2.2), T(Vector3(0, 0.18, 0)), 0.06)
	m.box("enamel", Vector3(1.2, 1.0, 0.9), T(Vector3(-0.35, 0.86, 0.45)), 0.05)
	for index in 3: m.box("graphite", Vector3(0.9, 0.05, 0.02), T(Vector3(-0.35, 0.62 + index * 0.12, 0.91)))
	status_head(m, T(Vector3(-0.35, 1.36, 0.3), Vector3(0, 180, 0)))
	m.tube("steel", Vector3(0.35, 0.36, -0.25), Vector3(0.35, 2.05, -0.25), 0.12, 10)
	m.box("graphite", Vector3(0.34, 0.3, 0.34), T(Vector3(0.35, 2.05, -0.25)), 0.04)
	var aim := T(Vector3(0.35, 2.1, -0.25), Vector3(48, 35, 0))
	m.dish("enamel", 1.12, 0.28, 0.06, 20, aim)
	m.box("graphite", Vector3(0.5, 0.1, 0.5), aim * T(Vector3(0, -0.02, 0)), 0.02)
	var focus := aim * Vector3(0, 0.78, 0)
	for index in 3:
		var angle := TAU * index / 3.0
		m.tube("graphite", aim * Vector3(cos(angle) * 1.0, 0.26, sin(angle) * 1.0), focus, 0.02, 6)
	m.lathe("glow_signal", [Vector2(0, 0), Vector2(0.07, 0), Vector2(0.05, 0.12), Vector2(0, 0.14)], 8, Transform3D(aim.basis, focus))
	for x in [-0.75, -0.05]:
		m.tube("steel", Vector3(x, 1.36, 0.25), Vector3(x, 1.95, 0.25), 0.015, 6)
		m.lathe("glow_signal", [Vector2(0, 0), Vector2(0.03, 0), Vector2(0.03, 0.04), Vector2(0, 0.04)], 6, T(Vector3(x, 1.95, 0.25)))

## Reactor bay: hex plinth, four enamel columns, an amber light column in a steel cage, domed cap.
func _reactor(m) -> void:
	m.lathe("graphite", [Vector2(0, 0), Vector2(1.6, 0), Vector2(1.55, 0.3), Vector2(0, 0.3)], 6, T(Vector3.ZERO, Vector3(0, 30, 0)), false)
	m.lathe("concrete", [Vector2(0, 0.3), Vector2(1.3, 0.3), Vector2(1.25, 0.42), Vector2(0, 0.42)], 6, T(Vector3.ZERO, Vector3(0, 30, 0)), false)
	for x in [-0.82, 0.82]:
		for z in [-0.82, 0.82]:
			m.box("enamel", Vector3(0.34, 1.56, 0.34), T(Vector3(x, 1.2, z)), 0.05)
			m.box("hazard", Vector3(0.36, 0.1, 0.36), T(Vector3(x, 0.62, z)), 0.02)
			m.tube("steel", Vector3(x * 1.0, 1.55, z * 1.0), Vector3(x * 1.55, 0.3, z * 1.55), 0.07, 8)
	m.lathe("glow_power", [Vector2(0, 0.42), Vector2(0.44, 0.42), Vector2(0.44, 1.98), Vector2(0, 1.98)], 14, T())
	for index in 8:
		var angle := TAU * index / 8.0
		var r := Vector3(cos(angle), 0, sin(angle)) * 0.53
		m.tube("steel", r + Vector3(0, 0.42, 0), r + Vector3(0, 1.98, 0), 0.028, 6)
	for y in [0.75, 1.3, 1.85]: m.torus("steel", 0.53, 0.05, 20, 6, T(Vector3(0, y, 0)))
	m.lathe("enamel", [Vector2(0, 1.98), Vector2(1.12, 1.98), Vector2(1.16, 2.1), Vector2(0.95, 2.26), Vector2(0.42, 2.4), Vector2(0, 2.42)], 20, T())
	m.lathe("cobalt", [Vector2(1.13, 2.0), Vector2(1.175, 2.0), Vector2(1.175, 2.08), Vector2(1.13, 2.08), Vector2(1.13, 2.0)], 20, T())
	m.lathe("glow_power", [Vector2(0, 2.4), Vector2(0.16, 2.4), Vector2(0.16, 2.46), Vector2(0, 2.48)], 10, T())
	status_head(m, T(Vector3(0.82, 0.42, 1.16), Vector3(0, 180, 0)))

## Lattice radio mast with an enamel top section and amber beacon.
func _antenna(m) -> void:
	var height := 5.4
	var legs := []
	for index in 3:
		var angle := deg_to_rad(90.0 + index * 120.0)
		legs.append([Vector3(cos(angle), 0, sin(angle)) * 1.8, Vector3(cos(angle), 0, sin(angle)) * 0.26 + Vector3(0, height, 0)])
	for leg in legs:
		m.box("concrete", Vector3(0.5, 0.2, 0.5), T((leg[0] as Vector3) + Vector3(0, 0.1, 0)), 0.04)
		m.tube("steel", (leg[0] as Vector3) + Vector3(0, 0.06, 0), (leg[0] as Vector3).lerp(leg[1], 0.75), 0.07, 6)
		m.tube("enamel", (leg[0] as Vector3).lerp(leg[1], 0.75), leg[1], 0.07, 6)
	for level in 5:
		var t0 := (level + 0.6) / 5.4
		var t1 := (level + 1.4) / 5.4
		for index in 3:
			var a: Vector3 = (legs[index][0] as Vector3).lerp(legs[index][1], t0)
			var b: Vector3 = (legs[(index + 1) % 3][0] as Vector3).lerp(legs[(index + 1) % 3][1], t0)
			var c: Vector3 = (legs[(index + 1) % 3][0] as Vector3).lerp(legs[(index + 1) % 3][1], t1)
			m.tube("steel", a, b, 0.03, 5)
			m.tube("steel", a, c, 0.025, 5)
	m.lathe("graphite", [Vector2(0, height - 0.05), Vector2(0.55, height - 0.05), Vector2(0.55, height + 0.05), Vector2(0, height + 0.05)], 8, T(), false)
	m.tube("steel", Vector3(0, height, 0), Vector3(0, height + 0.9, 0), 0.04, 6)
	m.lathe("glow_power", [Vector2(0, 0), Vector2(0.09, 0), Vector2(0.09, 0.14), Vector2(0, 0.18)], 8, T(Vector3(0, height + 0.9, 0)))
	m.dish("enamel", 0.42, 0.1, 0.04, 12, T(Vector3(0.3, height - 1.1, 0.25), Vector3(80, 40, 0)))

# ---------------------------------------------------------------- environment kit

## 6 m freight container in oxide with corrugated sides, graphite frame and steel door bars.
func _cargo(m) -> void:
	var lx := 3.05
	var ly := 2.5
	var lz := 1.22
	for x in [-lx + 0.08, lx - 0.08]:
		for z in [-lz + 0.08, lz - 0.08]: m.box("graphite", Vector3(0.16, ly, 0.16), T(Vector3(x, ly * 0.5, z)))
	for y in [0.08, ly - 0.08]:
		for z in [-lz + 0.08, lz - 0.08]: m.box("graphite", Vector3(lx * 2.0, 0.16, 0.16), T(Vector3(0, y, z)))
		for x in [-lx + 0.08, lx - 0.08]: m.box("graphite", Vector3(0.16, 0.16, lz * 2.0), T(Vector3(x, y, 0)))
	for z in [-lz + 0.1, lz - 0.1]:
		m.box("oxide", Vector3(lx * 2.0 - 0.3, ly - 0.3, 0.05), T(Vector3(0, ly * 0.5, z)))
		var face := signf(z)
		for index in 17:
			m.box("oxide", Vector3(0.14, ly - 0.36, 0.05), T(Vector3(-2.72 + index * 0.34, ly * 0.5, z + face * 0.045)))
		m.box("enamel", Vector3(1.8, 0.22, 0.02), T(Vector3(-1.4, ly - 0.42, z + face * 0.075)))
	m.box("oxide", Vector3(lx * 2.0 - 0.3, 0.05, lz * 2.0 - 0.3), T(Vector3(0, ly - 0.06, 0)))
	m.box("oxide", Vector3(0.05, ly - 0.3, lz * 2.0 - 0.3), T(Vector3(-lx + 0.1, ly * 0.5, 0)))
	m.box("oxide", Vector3(0.05, ly - 0.3, lz * 2.0 - 0.3), T(Vector3(lx - 0.1, ly * 0.5, 0)))
	for z in [-0.78, -0.3, 0.3, 0.78]:
		m.tube("steel", Vector3(lx - 0.04, 0.2, z), Vector3(lx - 0.04, ly - 0.2, z), 0.03, 6)
		m.box("hazard", Vector3(0.05, 0.08, 0.16), T(Vector3(lx - 0.02, 1.1, z + 0.1)))

## Quonset hangar: corrugated slate arch, enamel end wall with a dark door and hazard jambs.
func _hangar(m) -> void:
	var r := 3.2
	var half := 2.4
	var to_z := Basis.from_euler(Vector3(PI * 0.5, 0, 0))
	m.lathe("slate", [Vector2(r - 0.12, -half), Vector2(r, -half), Vector2(r, half), Vector2(r - 0.12, half), Vector2(r - 0.12, -half)], 16, Transform3D(to_z, Vector3.ZERO), true, PI, TAU, 30.0)
	for z in [-2.2, -0.75, 0.75, 2.2]:
		m.lathe("graphite", [Vector2(r - 0.02, -0.07), Vector2(r + 0.08, -0.07), Vector2(r + 0.08, 0.07), Vector2(r - 0.02, 0.07), Vector2(r - 0.02, -0.07)], 16, Transform3D(to_z, Vector3(0, 0, z)), true, PI, TAU, 30.0)
	var arch := []
	for index in 17:
		var angle := PI * index / 16.0
		arch.append(Vector2(cos(angle) * (r - 0.06), sin(angle) * (r - 0.06)))
	var front := [Vector2(-1.2, 0), Vector2(-1.2, 2.3), Vector2(1.2, 2.3), Vector2(1.2, 0)]
	front.append_array(arch)
	m.prism("enamel", P(front), 0.12, T(Vector3(0, 0, half - 0.06)), "z")
	m.prism("enamel", P(arch), 0.12, T(Vector3(0, 0, -half + 0.06)), "z")
	m.box("graphite", Vector3(1.18, 2.28, 0.08), T(Vector3(-0.6, 1.14, half - 0.12)), 0.02)
	m.box("graphite", Vector3(1.18, 2.28, 0.08), T(Vector3(0.6, 1.14, half - 0.16)), 0.02)
	for x in [-1.26, 1.26]: m.box("hazard", Vector3(0.12, 2.3, 0.16), T(Vector3(x, 1.15, half)))
	m.box("hazard", Vector3(2.64, 0.12, 0.16), T(Vector3(0, 2.36, half)))
	m.box("glow_power", Vector3(0.5, 0.1, 0.08), T(Vector3(0, 2.62, half + 0.02)))
	m.box("concrete", Vector3(2 * r + 0.2, 0.08, 2 * half + 0.3), T(Vector3(0, 0.04, 0)))

## Pallet of four drums: two hazard drums, two slate, ribbed lids.
func _barrel(m) -> void:
	for x in [-0.66, 0.0, 0.66]: m.box("graphite", Vector3(0.18, 0.08, 2.0), T(Vector3(x, 0.04, 0)))
	for z in [-0.82, -0.41, 0.0, 0.41, 0.82]: m.box("slate", Vector3(2.0, 0.05, 0.3), T(Vector3(0, 0.105, z)))
	var drums := [[Vector3(-0.47, 0, -0.47), "hazard"], [Vector3(0.47, 0, -0.47), "slate"], [Vector3(-0.47, 0, 0.47), "slate"], [Vector3(0.47, 0, 0.47), "hazard"]]
	for drum in drums:
		var at: Vector3 = drum[0] + Vector3(0, 0.13, 0)
		m.lathe(drum[1], [Vector2(0, 0), Vector2(0.42, 0), Vector2(0.43, 0.04), Vector2(0.43, 1.0), Vector2(0.4, 1.04), Vector2(0.38, 1.0), Vector2(0, 1.0)], 14, T(at), true, 0.0, TAU, 50.0)
		for y in [0.34, 0.68]: m.torus("graphite", 0.435, 0.022, 14, 4, T(at + Vector3(0, y, 0)))
		m.lathe("graphite", [Vector2(0, 0), Vector2(0.06, 0), Vector2(0.06, 0.03), Vector2(0, 0.03)], 6, T(at + Vector3(0.18, 1.0, 0.1)))

## Pipe rack: two enamel pipes on graphite stands with steel flanges and an amber valve wheel.
func _pipe(m) -> void:
	for x in [-1.4, 1.4]:
		for z in [-0.36, 0.36]: m.box("graphite", Vector3(0.12, 1.16, 0.12), T(Vector3(x, 0.58, z)), 0.02)
		for y in [0.52, 0.92]: m.box("graphite", Vector3(0.14, 0.1, 0.9), T(Vector3(x, y, 0)), 0.02)
		m.box("concrete", Vector3(0.36, 0.06, 0.96), T(Vector3(x, 0.03, 0)))
	var pipes := [[Vector3(0, 0.73, -0.14), 0.16], [Vector3(0, 1.08, 0.18), 0.12]]
	for pipe in pipes:
		var at: Vector3 = pipe[0]
		m.tube("enamel", at + Vector3(-1.7, 0, 0), at + Vector3(1.7, 0, 0), pipe[1], 12)
		for x in [-0.55, 0.85]:
			m.tube("steel", at + Vector3(x - 0.04, 0, 0), at + Vector3(x + 0.04, 0, 0), float(pipe[1]) + 0.05, 12)
		m.tube("cobalt", at + Vector3(0.15, 0, 0), at + Vector3(0.35, 0, 0), float(pipe[1]) + 0.012, 12)
	m.tube("steel", Vector3(0.25, 1.2, 0.18), Vector3(0.25, 1.42, 0.18), 0.03, 6)
	m.torus("hazard", 0.17, 0.025, 14, 6, T(Vector3(0.25, 1.42, 0.18)))

## Stackable freight crate in oxide with a graphite edge frame and an enamel stencil bar.
func _crate(m) -> void:
	var s := Vector3(1.9, 1.8, 1.9)
	m.box("oxide", s - Vector3(0.08, 0.08, 0.08), T(Vector3(0, s.y * 0.5, 0)), 0.04)
	var h := s * 0.5
	for x in [-h.x + 0.06, h.x - 0.06]:
		for z in [-h.z + 0.06, h.z - 0.06]: m.box("graphite", Vector3(0.12, s.y, 0.12), T(Vector3(x, h.y, z)), 0.02)
	for y in [0.06, s.y - 0.06]:
		for z in [-h.z + 0.06, h.z - 0.06]: m.box("graphite", Vector3(s.x, 0.12, 0.12), T(Vector3(0, y, z)), 0.02)
		for x in [-h.x + 0.06, h.x - 0.06]: m.box("graphite", Vector3(0.12, 0.12, s.z), T(Vector3(x, y, 0)), 0.02)
	for y in [0.62, 1.18]:
		m.box("oxide", Vector3(s.x - 0.24, 0.1, 0.05), T(Vector3(0, y, h.z - 0.02)), 0.01)
		m.box("oxide", Vector3(0.05, 0.1, s.z - 0.24), T(Vector3(h.x - 0.02, y, 0)), 0.01)
	m.box("enamel", Vector3(1.1, 0.16, 0.02), T(Vector3(-0.2, 1.48, h.z + 0.005)))
	m.box("enamel", Vector3(0.02, 0.16, 1.1), T(Vector3(h.x + 0.005, 1.48, 0.2)))

## Cable spool lying on its side (axis X): graphite flanges, amber power cable.
func _spool(m) -> void:
	var axis := Basis.from_euler(Vector3(0, 0, PI * 0.5))
	for x in [-0.41, 0.41]:
		m.lathe("graphite", [Vector2(0, -0.03), Vector2(0.6, -0.03), Vector2(0.6, 0.03), Vector2(0, 0.03)], 16, Transform3D(axis, Vector3(x, 0.6, 0)), true, 0.0, TAU, 50.0)
		m.lathe("steel", [Vector2(0, -0.04), Vector2(0.12, -0.04), Vector2(0.12, 0.04), Vector2(0, 0.04)], 8, Transform3D(axis, Vector3(x * 1.1, 0.6, 0)))
	m.lathe("steel", [Vector2(0, -0.38), Vector2(0.3, -0.38), Vector2(0.3, 0.38), Vector2(0, 0.38)], 12, Transform3D(axis, Vector3(0, 0.6, 0)))
	for index in 5:
		m.torus("hazard", 0.4, 0.07, 18, 6, Transform3D(axis, Vector3(-0.28 + index * 0.14, 0.6, 0)))

## Dock terminal: graphite pedestal, enamel sloped desk, a signal-blue screen facing the camera.
func _terminal(m) -> void:
	m.taper("graphite", Vector2(0.55, 0.4), Vector2(0.46, 0.32), 0.76, T())
	m.prism("enamel", P([Vector2(-0.42, 0.76), Vector2(0.46, 0.76), Vector2(0.5, 0.86), Vector2(-0.34, 1.02)]), 1.2, T(), "x")
	m.box("glow_signal", Vector3(0.9, 0.02, 0.56), T(Vector3(0, 0.95, 0.08), Vector3(-10.6, 0, 0)))
	for index in 3: m.box("glow_power", Vector3(0.08, 0.05, 0.02), T(Vector3(-0.2 + index * 0.2, 0.55, 0.36)))
	m.tube("steel", Vector3(-0.5, 1.0, -0.3), Vector3(-0.5, 1.6, -0.3), 0.02, 6)
	m.lathe("glow_signal", [Vector2(0, 0), Vector2(0.035, 0), Vector2(0.035, 0.05), Vector2(0, 0.05)], 6, T(Vector3(-0.5, 1.6, -0.3)))

## Deck hatch plate (used flattened as floor detail).
func _hatch(m) -> void:
	m.box("graphite", Vector3(3.8, 0.12, 3.8), T(Vector3(0, 0.06, 0)), 0.04)
	m.lathe("steel", [Vector2(0, 0.12), Vector2(1.2, 0.12), Vector2(1.2, 0.18), Vector2(0, 0.2)], 16, T(), true, 0.0, TAU, 50.0)
	m.torus("hazard", 1.28, 0.05, 20, 4, T(Vector3(0, 0.14, 0)))
	for x in [-1.6, 1.6]:
		for z in [-1.6, 1.6]: m.box("steel", Vector3(0.14, 0.04, 0.14), T(Vector3(x, 0.14, z)))

## Jersey barrier segment (2.45 m): concrete profile with a hazard reflector strip each side.
func _barrier(m) -> void:
	m.prism("concrete", P([Vector2(-0.3, 0), Vector2(0.3, 0), Vector2(0.3, 0.08), Vector2(0.14, 0.3), Vector2(0.1, 0.95), Vector2(-0.1, 0.95), Vector2(-0.14, 0.3), Vector2(-0.3, 0.08)]), 2.42, T(), "x")
	for side in [-1.0, 1.0]:
		m.box("hazard", Vector3(2.2, 0.08, 0.03), T(Vector3(0, 0.78, side * 0.112), Vector3(side * 3.5, 0, 0)))
		m.box("graphite", Vector3(0.3, 0.1, 0.62), T(Vector3(side * 0.7, 0.05, 0)))

## Sodium lamp post: concrete footing, graphite pole and arm, enamel head with an amber lens. Head points +X.
func _lamp(m) -> void:
	m.lathe("concrete", [Vector2(0, 0), Vector2(0.22, 0), Vector2(0.2, 0.25), Vector2(0.09, 0.4), Vector2(0, 0.4)], 8, T(), false)
	m.tube("graphite", Vector3(0, 0.3, 0), Vector3(0, 3.3, 0), 0.07, 8)
	m.tube("graphite", Vector3(0, 3.25, 0), Vector3(0.55, 3.45, 0), 0.05, 6)
	m.tube("graphite", Vector3(0.55, 3.45, 0), Vector3(0.98, 3.44, 0), 0.05, 6)
	m.box("enamel", Vector3(0.62, 0.14, 0.32), T(Vector3(1.03, 3.42, 0)), 0.05)
	m.box("glow_power", Vector3(0.44, 0.03, 0.22), T(Vector3(1.05, 3.34, 0)))

## Docking pad: octagonal concrete plinth, dark inset disc, bolt ring and hazard chevrons.
func _pad(m) -> void:
	m.lathe("concrete", [Vector2(0, 0), Vector2(2.35, 0), Vector2(2.35, 0.1), Vector2(2.22, 0.2), Vector2(0, 0.2)], 8, T(Vector3.ZERO, Vector3(0, 22.5, 0)), false)
	m.lathe("graphite", [Vector2(0, 0.2), Vector2(1.6, 0.2), Vector2(1.6, 0.225), Vector2(0, 0.225)], 8, T(Vector3.ZERO, Vector3(0, 22.5, 0)), false)
	for index in 8:
		var angle := TAU * index / 8.0
		m.box("steel", Vector3(0.12, 0.04, 0.12), T(Vector3(cos(angle), 0, sin(angle)) * 1.9 + Vector3(0, 0.21, 0)))
	for index in 4:
		var angle := TAU * index / 4.0 + PI * 0.25
		var dir := Vector3(cos(angle), 0, sin(angle))
		m.box("hazard", Vector3(0.6, 0.02, 0.16), Transform3D(Basis.looking_at(dir, Vector3.UP), dir * 2.02 + Vector3(0, 0.205, 0)))
	for index in 24:
		if index % 6 == 3: continue
		var angle := TAU * index / 24.0
		var dir := Vector3(cos(angle), 0, sin(angle))
		m.box("enamel", Vector3(0.035, 0.012, 0.16 if index % 2 == 0 else 0.09), Transform3D(Basis.looking_at(dir, Vector3.UP), dir * 2.2 + Vector3(0, 0.104, 0)))

## Perimeter wall module (2 m): concrete panel, graphite cap and foot, steel post, enamel reflector.
func _wall(m) -> void:
	m.box("concrete", Vector3(1.96, 1.44, 0.36), T(Vector3(0, 0.84, 0)), 0.04)
	m.box("graphite", Vector3(2.0, 0.12, 0.46), T(Vector3(0, 1.62, 0)), 0.02)
	m.box("graphite", Vector3(2.0, 0.12, 0.5), T(Vector3(0, 0.06, 0)), 0.02)
	m.box("steel", Vector3(0.12, 1.7, 0.42), T(Vector3(0.94, 0.85, 0)), 0.02)
	for side in [-1.0, 1.0]: m.box("enamel", Vector3(1.5, 0.06, 0.02), T(Vector3(-0.05, 1.3, side * 0.19)))

# ---------------------------------------------------------------- detail textures

## Two small tileable greyscale multipliers used through triplanar mapping (see art_finish.gd).
## paint_wear: enamel and painted parts, fine mottling and sparse edge chips.
## plate_seams: graphite and steel, plate seams every 128 px with rivets and grime at the joints.
func _textures(dir: String) -> void:
	DirAccess.make_dir_recursive_absolute(ProjectSettings.globalize_path(dir))
	var size := 256
	var fine := FastNoiseLite.new()
	fine.seed = 9
	fine.frequency = 0.045
	var broad := FastNoiseLite.new()
	broad.seed = 21
	broad.frequency = 0.012
	var chips := FastNoiseLite.new()
	chips.seed = 37
	chips.frequency = 0.03
	chips.fractal_octaves = 2
	var paint := Image.create_empty(size, size, false, Image.FORMAT_L8)
	var plate := Image.create_empty(size, size, false, Image.FORMAT_L8)
	for y in size:
		for x in size:
			var u := float(x)
			var v := float(y)
			var mottle := (fine.get_noise_2d(u, v) * 0.5 + 0.5) * 0.03 + (broad.get_noise_2d(u, v) * 0.5 + 0.5) * 0.04
			var chip := smoothstep(0.68, 0.72, chips.get_noise_2d(u, v) * 0.5 + 0.5)
			var scratch := 0.0
			if int(u + v * 0.35) % 61 == 0 and fine.get_noise_2d(u * 3.0, v) > 0.2: scratch = 0.08
			var shade := clampf(0.99 - mottle - chip * 0.07 - scratch * 0.5, 0.0, 1.0)
			paint.set_pixel(x, y, Color(shade, shade, shade))
			var gx := minf(fmod(u, 128.0), 128.0 - fmod(u, 128.0))
			var gy := minf(fmod(v, 128.0), 128.0 - fmod(v, 128.0))
			var seam := 1.0 - smoothstep(0.6, 1.8, minf(gx, gy))
			var grime := (1.0 - smoothstep(1.0, 14.0, minf(gx, gy))) * (broad.get_noise_2d(u, v) * 0.5 + 0.5)
			var rivet := 0.0
			var rx := fmod(u + 16.0, 32.0) - 16.0
			var ry := fmod(v + 16.0, 32.0) - 16.0
			if gy < 8.0 and gy > 4.0 and absf(rx) < 2.2: rivet = 1.0
			if gx < 8.0 and gx > 4.0 and absf(ry) < 2.2: rivet = 1.0
			var value := 0.97 - mottle * 0.4 - seam * 0.3 - grime * 0.08 + rivet * 0.05
			value = clampf(value, 0.0, 1.0)
			plate.set_pixel(x, y, Color(value, value, value))
	paint.save_png(dir + "/paint_wear.png")
	plate.save_png(dir + "/plate_seams.png")
	print("textures written to %s" % dir)
