@static_unload
extends RefCounted
## Shared finish for generated models. Materials are graded once per palette name (the glTF
## material name), so every model that uses "enamel" shares one runtime material.
## Painted materials get a paint-wear multiplier and metal plate gets seams with rivets, both
## through object-space triplanar mapping, so no UV work is needed and the textures stay tiny.
const PAINT_WEAR = preload("res://assets/textures_yard/paint_wear.png")
const PLATE_SEAMS = preload("res://assets/textures_yard/plate_seams.png")
## name: [texture, triplanar scale]
const DETAIL = {
	"enamel": ["paint", 0.35], "cobalt": ["paint", 0.35], "vermilion": ["paint", 0.35], "hazard": ["paint", 0.4],
	"oxide": ["paint", 0.3], "concrete": ["paint", 0.25], "graphite": ["plate", 0.5], "slate": ["plate", 0.5]
}
## Lens materials: [lit base albedo, emission]. Kept below full brightness so lamps read as lenses.
const GLOWS = {
	"glow_power": [Color(0.42, 0.25, 0.08), Color(0.78, 0.46, 0.16)],
	"glow_signal": [Color(0.1, 0.17, 0.34), Color(0.3, 0.5, 0.88)],
	"glow_hostile": [Color(0.3, 0.06, 0.03), Color(0.72, 0.16, 0.08)],
	"status_lamp": [Color(0.28, 0.18, 0.08), Color(0.42, 0.25, 0.09)]
}
static var _graded: Dictionary = {}

static func grade(source: BaseMaterial3D, rim := 0.3) -> BaseMaterial3D:
	if source == null or source.get_meta("aurum_finished", false): return source
	var amount := clampf(rim, 0.0, 1.0)
	var name := source.resource_name
	if source.emission_enabled and not GLOWS.has(name): return source
	var key := ("%s/%.3f" % [name, amount]) if not name.is_empty() else ("%s/%.3f" % [source.get_instance_id(), amount])
	if not _graded.has(key):
		var graded := source.duplicate() as BaseMaterial3D
		if GLOWS.has(name):
			# Authoritative lens values, independent of how an importer converts glTF emissive factors.
			graded.albedo_color = GLOWS[name][0]
			graded.emission_enabled = true
			graded.emission = GLOWS[name][1]
			graded.emission_energy_multiplier = 1.0
		elif not graded.emission_enabled:
			graded.rim_enabled = true
			graded.rim = amount
			graded.rim_tint = 0.6
			graded.metallic_specular = 0.4
			if DETAIL.has(name):
				var detail: Array = DETAIL[name]
				graded.albedo_texture = PAINT_WEAR if detail[0] == "paint" else PLATE_SEAMS
				graded.uv1_triplanar = true
				graded.uv1_scale = Vector3.ONE * float(detail[1])
		graded.set_meta("aurum_finished", true)
		_graded[key] = graded
	return _graded[key]

static func apply(node: Node, rim := 0.3) -> void:
	if node is MeshInstance3D:
		var instance := node as MeshInstance3D
		if instance.material_override is BaseMaterial3D:
			instance.material_override = grade(instance.material_override, rim)
		elif instance.mesh != null and instance.material_override == null:
			for surface in instance.mesh.get_surface_count():
				var source := instance.get_active_material(surface) as BaseMaterial3D
				if source != null: instance.set_surface_override_material(surface, grade(source, rim))
	for child in node.get_children(): apply(child, rim)
