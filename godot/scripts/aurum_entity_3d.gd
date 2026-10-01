extends Node3D
class_name AurumEntity3D

# AurumEntity3D: 3D entity that mirrors a Rust-side engine entity.
#
# Same pattern as `aurum_entity.gd` but for Node3D. On _ready, spawns
# an entity in the engine and attaches Position3D + Velocity3D. On
# _process, syncs the engine's position back to the scene node.

var entity_id: int = 0
var tag: String = ""


func _ready() -> void:
	entity_id = Aurum.spawn()
	Aurum.set_component(entity_id, "Position3D", {
		"x": position.x,
		"y": position.y,
		"z": position.z,
	})
	Aurum.set_component(entity_id, "Velocity3D", {
		"x": 0.0,
		"y": 0.0,
		"z": 0.0,
	})
	if not tag.is_empty():
		Aurum.set_component(entity_id, "Tag", {"name": tag})
	add_to_group("aurum_entities_3d")
	var kin := _find_kinematics()
	if kin:
		kin.register_node(self)


func _exit_tree() -> void:
	if entity_id != 0 and Aurum.entity_exists(entity_id):
		Aurum.despawn(entity_id)


func _process(_delta: float) -> void:
	if entity_id == 0:
		return
	var pos_dict: Dictionary = Aurum.get_component(entity_id, "Position3D")
	if pos_dict.is_empty():
		return
	position = Vector3(
		pos_dict.get("x", position.x),
		pos_dict.get("y", position.y),
		pos_dict.get("z", position.z),
	)


func _find_kinematics() -> Node:
	for node in get_tree().get_nodes_in_group("aurum_kinematics_3d"):
		return node
	return null
