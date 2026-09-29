extends SceneTree
## Aurum's bounded scene worker. Requests and results are JSON; edits are saved
## to a staging file and committed by the parent only after successful validation.

var request: Dictionary = {}
var failure := ""
var node_budget := 1000

func _initialize() -> void:
    var args := OS.get_cmdline_user_args()
    if args.size() != 2:
        quit(64)
        return
    var parsed = JSON.parse_string(FileAccess.get_file_as_string(args[0]))
    var result: Dictionary
    if typeof(parsed) != TYPE_DICTIONARY:
        result = {"ok": false, "error": "invalid operation request"}
    else:
        request = parsed
        result = execute()
    var output := FileAccess.open(args[1], FileAccess.WRITE)
    if output == null:
        quit(1)
        return
    output.store_string(JSON.stringify(result))
    output.close()
    quit(0 if result.get("ok", false) else 1)

func reject(message: String) -> Dictionary:
    return {"ok": false, "error": message}

func execute() -> Dictionary:
    var op: String = request.get("op", "")
    if op == "runtime_info":
        return {"ok": true, "version": Engine.get_version_info(), "license": Engine.get_license_text(), "copyright": Engine.get_copyright_info(), "third_party_licenses": Engine.get_license_info()}
    if op == "validate":
        var main: String = ProjectSettings.get_setting("application/run/main_scene", "")
        if not main.is_empty() and not ResourceLoader.exists(main, "PackedScene"):
            return reject("the configured start scene does not exist: " + main)
        var pending: Array[String] = ["res://"]
        var checked := 0
        var directories := 0
        while not pending.is_empty():
            var folder: String = pending.pop_back()
            directories += 1
            if directories > 2000:
                return reject("validation is limited to 2000 directories per run")
            for directory in DirAccess.get_directories_at(folder):
                if not directory.begins_with("."):
                    pending.append(folder.path_join(directory))
            for file in DirAccess.get_files_at(folder):
                if file.get_extension() in ["gd", "tscn", "tres"]:
                    var resource = ResourceLoader.load(folder.path_join(file), "", ResourceLoader.CACHE_MODE_IGNORE)
                    if resource == null:
                        return reject("could not validate " + folder.path_join(file))
                    checked += 1
                    if checked > 2000:
                        return reject("validation is limited to 2000 resources per run")
        return {"ok": true, "checked_resources": checked}
    if op == "set_main_scene":
        var main: String = request.get("scene", "")
        if not ResourceLoader.exists(main, "PackedScene"):
            return reject("the selected start scene does not exist")
        ProjectSettings.set_setting("application/run/main_scene", main)
        var saved := ProjectSettings.save_custom(request.output_project)
        return {"ok": saved == OK, "main_scene": main}
    if op == "classes":
        var classes: Array[String] = []
        var query: String = request.get("query", "").to_lower()
        for name in ClassDB.get_class_list():
            if query.is_empty() or str(name).to_lower().contains(query):
                classes.append(str(name))
        classes.sort()
        return {"ok": true, "version": Engine.get_version_info(), "classes": classes.slice(0, 250), "truncated": classes.size() > 250}
    if op == "class_info":
        var name: String = request.get("class", "Node")
        if not ClassDB.class_exists(name):
            return reject("unknown class: " + name)
        return {"ok": true, "class": name, "parent": ClassDB.get_parent_class(name),
            "properties": ClassDB.class_get_property_list(name),
            "methods": ClassDB.class_get_method_list(name, true),
            "signals": ClassDB.class_get_signal_list(name, true),
            "version": Engine.get_version_info()}

    var scene_path: String = request.get("scene", "")
    if not scene_path.begins_with("res://") or not scene_path.ends_with(".tscn"):
        return reject("a res:// scene ending in .tscn is required")
    var scene: Node
    if op == "scene_create":
        var kind: String = request.get("root_type", "Node3D")
        scene = make_node(kind)
        if scene == null:
            return reject(failure)
        scene.name = request.get("name", "Main")
    else:
        var packed = ResourceLoader.load(scene_path, "PackedScene", ResourceLoader.CACHE_MODE_IGNORE)
        if not packed is PackedScene:
            return reject("could not load scene: " + scene_path)
        scene = packed.instantiate()
        if scene == null:
            return reject("could not instantiate scene")

    if op != "scene_inspect":
        var operations = request.get("operations", [])
        if typeof(operations) != TYPE_ARRAY or operations.size() > 200:
            scene.free()
            return reject("operations must be an array of at most 200 edits")
        for operation in operations:
            if typeof(operation) != TYPE_DICTIONARY or not edit(scene, operation):
                scene.free()
                return reject(failure if not failure.is_empty() else "invalid scene operation")

    var snapshot := describe(scene, scene, 0)
    if op != "scene_inspect":
        var packed := PackedScene.new()
        if packed.pack(scene) != OK:
            scene.free()
            return reject("scene could not be packed")
        var saved := ResourceSaver.save(packed, request.output_scene)
        if saved != OK:
            scene.free()
            return reject("scene could not be staged: " + error_string(saved))
    scene.free()
    return {"ok": true, "scene": scene_path, "tree": snapshot, "truncated": node_budget <= 0}

func make_node(kind: String) -> Node:
    if not ClassDB.class_exists(kind) or not ClassDB.can_instantiate(kind) or not ClassDB.is_parent_class(kind, "Node"):
        failure = "class is not an instantiable Node: " + kind
        return null
    return ClassDB.instantiate(kind) as Node

func find_node(scene: Node, path: String) -> Node:
    if path.is_empty() or path == ".":
        return scene
    if path.begins_with("/") or ".." in path.split("/"):
        failure = "node path must stay inside the scene"
        return null
    var found := scene.get_node_or_null(NodePath(path))
    if found == null:
        failure = "node not found: " + path
    return found

func own(node: Node, scene: Node) -> void:
    node.owner = scene
    if not node.scene_file_path.is_empty():
        return
    for child in node.get_children():
        own(child, scene)

func edit(scene: Node, operation: Dictionary) -> bool:
    var op: String = operation.get("op", "")
    if op == "instance":
        var parent := find_node(scene, operation.get("parent", "."))
        var source: String = operation.get("source", "")
        if parent == null or not source.begins_with("res://") or ".." in source.split("/"):
            failure = "instance source must be a project scene or imported model"
            return false
        var packed = ResourceLoader.load(source)
        if not packed is PackedScene:
            failure = "instance source did not load as a scene"
            return false
        var instance: Node = packed.instantiate()
        var name: String = operation.get("name", str(instance.name))
        if name.is_empty() or name.contains("/") or parent.has_node(NodePath(name)):
            instance.free()
            failure = "instance name is invalid or already exists"
            return false
        instance.name = name
        parent.add_child(instance)
        own(instance, scene)
        return properties(instance, operation.get("properties", {}))
    if op == "create":
        var parent := find_node(scene, operation.get("parent", "."))
        if parent == null:
            return false
        var name: String = operation.get("name", "Node")
        if name.is_empty() or name.contains("/") or name.contains(":") or parent.has_node(NodePath(name)):
            failure = "node name is invalid or already exists"
            return false
        var node := make_node(operation.get("type", "Node3D"))
        if node == null:
            return false
        node.name = name
        parent.add_child(node)
        node.owner = scene
        return properties(node, operation.get("properties", {}))

    var node := find_node(scene, operation.get("node", "."))
    if node == null:
        return false
    match op:
        "set":
            return properties(node, operation.get("properties", {}))
        "attach_script":
            var script_path: String = operation.get("script", "")
            if not script_path.begins_with("res://") or ".." in script_path.split("/") or not script_path.ends_with(".gd"):
                failure = "script must be a project res:// GDScript path"
                return false
            var script = ResourceLoader.load(script_path)
            if not script is Script:
                failure = "script could not be loaded"
                return false
            node.set_script(script)
        "remove":
            if node == scene:
                failure = "cannot remove the scene root"
                return false
            node.get_parent().remove_child(node)
            node.free()
        "reparent":
            var parent := find_node(scene, operation.get("parent", "."))
            if parent == null or node == scene or node == parent or node.is_ancestor_of(parent):
                failure = "reparent would break the scene hierarchy"
                return false
            node.reparent(parent)
            own(node, scene)
        _:
            failure = "unknown scene operation: " + op
            return false
    return true

func properties(object: Object, values) -> bool:
    if typeof(values) != TYPE_DICTIONARY:
        failure = "properties must be an object"
        return false
    var types := {}
    for property in object.get_property_list():
        types[str(property.name)] = int(property.type)
    for name in values:
        if not types.has(name) or name == "script":
            failure = "unknown or protected property: " + str(name)
            return false
        var value = convert_value(values[name], types[name])
        if not failure.is_empty():
            return false
        object.set(name, value)
    return true

func convert_value(value, type: int):
    if typeof(value) == TYPE_DICTIONARY:
        if type == TYPE_VECTOR2:
            return Vector2(value.get("x",0), value.get("y",0))
        if type == TYPE_VECTOR3:
            return Vector3(value.get("x",0), value.get("y",0), value.get("z",0))
        if type == TYPE_COLOR:
            return Color(value.get("r",0), value.get("g",0), value.get("b",0), value.get("a",1))
        if type == TYPE_OBJECT and value.has("resource"):
            var kind: String = value.resource
            if not ClassDB.class_exists(kind) or not ClassDB.can_instantiate(kind) or not ClassDB.is_parent_class(kind,"Resource"):
                failure = "invalid resource class: " + kind
                return null
            var resource = ClassDB.instantiate(kind)
            if not properties(resource, value.get("properties",{})):
                return null
            return resource
    if type == TYPE_NODE_PATH:
        return NodePath(str(value))
    if type == TYPE_STRING_NAME:
        return StringName(str(value))
    return value

func describe(node: Node, scene: Node, depth: int) -> Dictionary:
    node_budget -= 1
    var result := {"name":str(node.name),"type":node.get_class(),"path":str(scene.get_path_to(node)),"children":[]}
    if node is Node3D:
        result.position = {"x":node.position.x,"y":node.position.y,"z":node.position.z}
        result.rotation = {"x":node.rotation.x,"y":node.rotation.y,"z":node.rotation.z}
        result.scale = {"x":node.scale.x,"y":node.scale.y,"z":node.scale.z}
    elif node is Node2D:
        result.position = {"x":node.position.x,"y":node.position.y}
    if node.get_script() != null:
        result.script = node.get_script().resource_path
    if node is MeshInstance3D and node.mesh != null:
        result.mesh = {"type": node.mesh.get_class(), "path": node.mesh.resource_path}
    if depth < 32:
        for child in node.get_children():
            if node_budget <= 0:
                break
            result.children.append(describe(child,scene,depth+1))
    return result
