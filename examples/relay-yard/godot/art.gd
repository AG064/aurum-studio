@static_unload
extends RefCounted
## "Sodium Enamel" art layer: enamel machines under sodium lamps, cobalt for the courier service,
## vermilion for dock security. Models come from res://tools/asset_forge.gd (metres, -Z forward,
## ground-centred pivots) and keep the original asset keys, plus sentinel, skirmisher, core,
## lamp, pad and wall. "drone" stays as an alias of the sentinel for older callers.
const Finish = preload("res://art_finish.gd")
const FONT = preload("res://assets/fonts/BarlowCondensed-SemiBold.ttf")
const MODEL_DIR = "res://assets/models_yard/"
const MODELS = {
	"courier": preload("res://assets/models_yard/courier.glb"),
	"sentinel": preload("res://assets/models_yard/sentinel.glb"),
	"skirmisher": preload("res://assets/models_yard/skirmisher.glb"),
	"drone": preload("res://assets/models_yard/sentinel.glb"),
	"warden": preload("res://assets/models_yard/warden.glb"),
	"core": preload("res://assets/models_yard/core.glb"),
	"generator": preload("res://assets/models_yard/generator.glb"),
	"transmitter": preload("res://assets/models_yard/transmitter.glb"),
	"reactor": preload("res://assets/models_yard/reactor.glb"),
	"antenna": preload("res://assets/models_yard/antenna.glb"),
	"cargo": preload("res://assets/models_yard/cargo.glb"),
	"hangar": preload("res://assets/models_yard/hangar.glb"),
	"barrel": preload("res://assets/models_yard/barrel.glb"),
	"pipe": preload("res://assets/models_yard/pipe.glb"),
	"crate": preload("res://assets/models_yard/crate.glb"),
	"crate_tall": preload("res://assets/models_yard/crate_tall.glb"),
	"terminal": preload("res://assets/models_yard/terminal.glb"),
	"floor": preload("res://assets/models_yard/floor.glb"),
	"barrier": preload("res://assets/models_yard/barrier.glb"),
	"lamp": preload("res://assets/models_yard/lamp.glb"),
	"pad": preload("res://assets/models_yard/pad.glb"),
	"wall": preload("res://assets/models_yard/wall.glb")
}
## Semantic palette. SIGNAL: the courier, ours, ready. POWER: objectives, power, heat. HOSTILE: enemy intent.
const SIGNAL = Color(0.4, 0.61, 0.98)
const POWER = Color(0.93, 0.6, 0.26)
const HOSTILE = Color(0.86, 0.27, 0.17)
const ENAMEL = Color(0.93, 0.90, 0.82)
const PAINT = Color(0.6, 0.58, 0.52)
## Earlier names kept as aliases so unchanged scripts (yard.gd, effects.gd) follow the new palette.
const COPPER = POWER
const TEAL = SIGNAL
const RED = HOSTILE
static var _manifest: Dictionary = {}
static var _materials: Dictionary = {}
static var _meshes: Dictionary = {}
static var _lenses: Dictionary = {}
const MARKING = preload("res://marking.gdshader")

static func manifest() -> Dictionary:
	if _manifest.is_empty(): _manifest = JSON.parse_string(FileAccess.get_file_as_string(MODEL_DIR + "manifest.json"))
	return _manifest

static func model(key: String, width: float) -> Node3D:
	var root := Node3D.new()
	root.name = key.capitalize()
	var imported: Node3D = MODELS[key].instantiate()
	root.add_child(imported)
	var size: Array = manifest().assets[key].size
	var factor := width / maxf(float(size[0]), float(size[2]))
	imported.scale = Vector3.ONE * factor
	Finish.apply(imported)
	return root

## Graded, shared mesh of a single-mesh model, for MultiMesh runs (walls). Unscaled, in metres.
static func mesh(key: String) -> Mesh:
	if _meshes.has(key): return _meshes[key]
	var scene: Node = MODELS[key].instantiate()
	var found: MeshInstance3D = scene.find_children("*", "MeshInstance3D", true, false)[0]
	var result := found.mesh.duplicate() as Mesh
	for index in result.get_surface_count():
		var source := result.surface_get_material(index) as BaseMaterial3D
		if source != null: result.surface_set_material(index, Finish.grade(source))
	scene.free()
	_meshes[key] = result
	return result

## Status lens for machinery lamps: dark glass base, emission only when lit.
static func status_lens(color: Color, lit: bool) -> StandardMaterial3D:
	var key := color.to_html() + str(lit)
	if _lenses.has(key): return _lenses[key]
	var lens := StandardMaterial3D.new()
	lens.albedo_color = color * (0.42 if lit else 0.16)
	lens.roughness = 0.18
	lens.emission_enabled = true
	lens.emission = color * (0.85 if lit else 0.1)
	_lenses[key] = lens
	return lens

## Deck marking in the instrument language (see marking.gdshader). Mode 0 ring gauge, 1 aim line.
static func marking(color: Color, mode := 0) -> MeshInstance3D:
	var node := MeshInstance3D.new()
	var plane := PlaneMesh.new()
	plane.size = Vector2.ONE
	node.mesh = plane
	var shader := ShaderMaterial.new()
	shader.shader = MARKING
	shader.set_shader_parameter("tint", color)
	shader.set_shader_parameter("mode", mode)
	node.material_override = shader
	node.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
	return node

static func material(color: Color, glow := false) -> StandardMaterial3D:
	var key := color.to_html() + str(glow)
	if _materials.has(key): return _materials[key]
	var result := StandardMaterial3D.new()
	result.albedo_color = color
	result.roughness = 0.65
	if glow:
		result.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
		result.emission_enabled = true
		result.emission = color
	_materials[key] = result
	return result

static func box(size: Vector3, color: Color, glow := false) -> MeshInstance3D:
	var node := MeshInstance3D.new()
	var mesh := BoxMesh.new()
	mesh.size = size
	node.mesh = mesh
	node.material_override = material(color, glow)
	return node

static func orb(radius: float, color: Color) -> MeshInstance3D:
	var node := MeshInstance3D.new()
	var mesh := SphereMesh.new()
	mesh.radius = radius
	mesh.height = radius * 2.0
	mesh.radial_segments = 12
	mesh.rings = 6
	node.mesh = mesh
	node.material_override = material(color, true)
	return node

static func ring(radius: float, color: Color) -> MeshInstance3D:
	var node := MeshInstance3D.new()
	var mesh := TorusMesh.new()
	mesh.inner_radius = radius - 0.055
	mesh.outer_radius = radius + 0.055
	mesh.rings = 24
	mesh.ring_segments = 8
	node.mesh = mesh
	node.material_override = material(color, true)
	return node

static func label(text: String, color: Color, size := 36) -> Label3D:
	var node := Label3D.new()
	node.text = text
	node.font = FONT
	node.font_size = size
	node.pixel_size = 0.013
	node.modulate = color
	node.outline_size = 8
	node.outline_modulate = Color(0.03, 0.035, 0.045, 0.9)
	node.billboard = BaseMaterial3D.BILLBOARD_ENABLED
	node.no_depth_test = false
	return node

## Flat deck paint, turned so it reads upright from the follow camera.
static func floor_paint(text: String, color: Color, size := 96) -> Label3D:
	var node := Label3D.new()
	node.text = text
	node.font = FONT
	node.font_size = size
	node.pixel_size = 0.02
	node.modulate = color
	node.outline_size = 0
	node.billboard = BaseMaterial3D.BILLBOARD_DISABLED
	node.rotation = Vector3(-PI * 0.5, atan2(0.58, 0.78), 0)
	return node
