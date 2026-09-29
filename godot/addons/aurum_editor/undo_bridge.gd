@tool
extends RefCounted
## Live editor mutations participate in Godot's scene history. Headless edits
## use the separate staged scene transaction and saved-file undo path.
var plugin: EditorPlugin

func _init(owner_plugin: EditorPlugin) -> void:
    plugin = owner_plugin

func dispatch(text: String) -> String:
    var request = JSON.parse_string(text)
    if typeof(request) != TYPE_DICTIONARY:
        return JSON.stringify({"ok": false, "error": "invalid JSON request"})
    return JSON.stringify(apply_request(request))

func find_node(scene: Node, path: String) -> Node:
    if path == "." or path.is_empty():
        return scene
    if path.begins_with("/") or ".." in path.split("/"):
        return null
    return scene.get_node_or_null(NodePath(path))

func restore_owner(node: Node, scene: Node) -> void:
    node.owner = scene
    if not node.scene_file_path.is_empty():
        return
    for child in node.get_children():
        restore_owner(child, scene)

func error(message: String) -> Dictionary:
    return {"ok": false, "error": message}

func apply_request(request: Dictionary) -> Dictionary:
    var scene := plugin.get_editor_interface().get_edited_scene_root()
    if scene == null:
        return error("no scene is open")
    var op: String = request.get("op", "")
    var history := plugin.get_undo_redo()
    if op == "save_scene":
        var result := plugin.get_editor_interface().save_scene()
        if result != OK:
            return error(error_string(result))
        return {"ok": true, "saved": scene.scene_file_path}
    if op == "undo" or op == "redo":
        var undo_redo := history.get_history_undo_redo(history.get_object_history_id(scene))
        if undo_redo == null:
            return error("no scene history is available")
        return {"ok": undo_redo.undo() if op == "undo" else undo_redo.redo()}

    var node := find_node(scene, str(request.get("node", ".")))
    if op == "create_node":
        var parent := find_node(scene, str(request.get("parent", ".")))
        var kind: String = request.get("type", "Node3D")
        var name: String = request.get("name", "Node")
        if parent == null or not ClassDB.class_exists(kind) or not ClassDB.can_instantiate(kind) or not ClassDB.is_parent_class(kind, "Node"):
            return error("invalid parent or node class")
        if name.is_empty() or name.contains("/") or name.contains(":") or parent.has_node(NodePath(name)):
            return error("node name is invalid or already exists")
        node = ClassDB.instantiate(kind)
        node.name = name
        history.create_action("Aurum: create node", 0, scene)
        history.add_do_method(parent, "add_child", node)
        history.add_do_method(node, "set_owner", scene)
        history.add_undo_method(parent, "remove_child", node)
        history.add_do_reference(node)
    elif node == null:
        return error("node not found")
    elif op == "set_property":
        var property: String = request.get("property", "")
        var found := false
        for item in node.get_property_list():
            if str(item.name) == property:
                found = true
                break
        if not found or property == "script":
            return error("unknown or protected property")
        var old = node.get(property)
        var encoded = request.get("value", "null")
        var value = JSON.parse_string(str(encoded))
        if value == null and typeof(old) == TYPE_STRING:
            value = str(encoded)
        if typeof(old) == TYPE_VECTOR3 and typeof(value) == TYPE_DICTIONARY:
            value = Vector3(value.get("x",0),value.get("y",0),value.get("z",0))
        elif typeof(old) == TYPE_VECTOR2 and typeof(value) == TYPE_DICTIONARY:
            value = Vector2(value.get("x",0),value.get("y",0))
        elif typeof(old) == TYPE_COLOR and typeof(value) == TYPE_DICTIONARY:
            value = Color(value.get("r",0),value.get("g",0),value.get("b",0),value.get("a",1))
        history.create_action("Aurum: set " + property, 0, scene)
        history.add_do_property(node, property, value)
        history.add_undo_property(node, property, old)
    elif op == "attach_script":
        var path: String = request.get("script", "")
        if not path.begins_with("res://") or ".." in path.split("/") or not path.ends_with(".gd"):
            return error("script must be inside the project")
        var script = load(path)
        if not script is Script:
            return error("script could not be loaded")
        history.create_action("Aurum: attach script", 0, scene)
        history.add_do_property(node, "script", script)
        history.add_undo_property(node, "script", node.get_script())
    elif op == "remove_node":
        if node == scene:
            return error("cannot remove the scene root")
        var parent := node.get_parent()
        history.create_action("Aurum: remove node", 0, scene)
        history.add_do_method(parent, "remove_child", node)
        history.add_undo_method(parent, "add_child", node)
        history.add_undo_method(parent, "move_child", node, node.get_index())
        history.add_undo_method(self, "restore_owner", node, scene)
        history.add_undo_reference(node)
    elif op == "reparent_node":
        var parent := find_node(scene, str(request.get("parent", ".")))
        if parent == null or node == scene or node == parent or node.is_ancestor_of(parent):
            return error("invalid reparent target")
        var old_parent := node.get_parent()
        history.create_action("Aurum: reparent node", 0, scene)
        history.add_do_method(node, "reparent", parent)
        history.add_do_method(self, "restore_owner", node, scene)
        history.add_undo_method(node, "reparent", old_parent)
        history.add_undo_method(self, "restore_owner", node, scene)
    else:
        return error("unknown editor operation")
    history.commit_action()
    plugin.get_editor_interface().mark_scene_as_unsaved()
    return {"ok": true, "operation": op, "undoable": true}
