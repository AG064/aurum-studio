extends SceneTree
## Aurum's bounded scene worker. Requests and results are JSON; edits are saved
## to a staging file and committed by the parent only after successful validation.

# Admission only: four float32 epsilons accommodate saved basis/scale round trips.
# Supplied values are never clamped, rounded or replaced by these bounds.
const TRANSFORM_SCALE_REL_TOLERANCE: float = 4.0 / 8388608.0
const TRANSFORM_SCALE_MIN_ADMISSION: float = 0.000001 * (1.0 - TRANSFORM_SCALE_REL_TOLERANCE)
const TRANSFORM_SCALE_MAX_ADMISSION: float = 100000.0 * (1.0 + TRANSFORM_SCALE_REL_TOLERANCE)

var request: Dictionary = {}
var failure := ""
var node_budget := 1000
var property_budget := 2048
var resource_budget := 128
var animation_key_budget := 512
var inspection_truncated := false

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
        # Keep inherited scene state for packing only the explicit child edits.
        scene = packed.instantiate(PackedScene.GEN_EDIT_STATE_MAIN if OS.has_feature("editor") else PackedScene.GEN_EDIT_STATE_DISABLED)
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
    return {"ok": true, "scene": scene_path, "tree": snapshot, "truncated": node_budget <= 0 or inspection_truncated,
        "inspection_limits": {"nodes":1000,"resources":128,"surfaces_per_mesh":16,"tracks_per_animation":16,"animation_keys":512}}

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
        var instance: Node = packed.instantiate(PackedScene.GEN_EDIT_STATE_INSTANCE if OS.has_feature("editor") else PackedScene.GEN_EDIT_STATE_DISABLED)
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

    if op == "set_transform":
        return set_transform(scene, operation)

    var node := find_node(scene, operation.get("node", "."))
    if node == null:
        return false
    match op:
        "set":
            mark_editable(scene, node)
            return properties(node, operation.get("properties", {}))
        "animate_transform":
            return animate_transform(scene, node, operation)
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

func mark_editable(scene: Node, node: Node) -> void:
    var ancestor: Node = node
    while ancestor != null and ancestor != scene:
        if not ancestor.scene_file_path.is_empty():
            scene.set_editable_instance(ancestor, true)
        ancestor = ancestor.get_parent()

func exact_fields(value: Dictionary, allowed: Array) -> bool:
    for key in value:
        if key not in allowed:
            failure = "unsupported authoring field: " + str(key)
            return false
    return true

func confined_resource(path) -> String:
    if not path is String or path.length() > 1024 or not path.begins_with("res://") or "\\" in path:
        failure = "resource path must be a bounded project res:// path"
        return ""
    var relative: String = path.substr(6)
    if relative.is_empty() or ":" in relative:
        failure = "resource path must stay inside the project"
        return ""
    var directory := DirAccess.open(ProjectSettings.globalize_path("res://"))
    var prefix := ""
    for component in relative.split("/"):
        if component.is_empty() or component in [".","..",".git",".aurum",".godot",".env"] or component.begins_with(".env."):
            failure = "resource path contains a protected or escaping component"
            return ""
        prefix = component if prefix.is_empty() else prefix.path_join(component)
        if directory == null or directory.is_link(prefix):
            failure = "resource references cannot traverse links or junctions"
            return ""
    if not FileAccess.file_exists(path):
        failure = "referenced resource does not exist"
        return ""
    var referenced_file := FileAccess.open(path,FileAccess.READ)
    if referenced_file == null or referenced_file.get_length() > 64 * 1024 * 1024:
        failure = "resource reference exceeds bounded regular file size"
        return ""
    referenced_file.close()
    return path

func resource_matches(resource: Resource, expected: String) -> bool:
    for kind in expected.split(","):
        if resource.is_class(kind.strip_edges()):
            return true
    return false

func properties(object: Object, values, depth: int = 0) -> bool:
    if typeof(values) != TYPE_DICTIONARY or values.size() > 64 or depth > 16:
        failure = "properties must be an object"
        return false
    var types := {}
    for property in object.get_property_list():
        types[str(property.name)] = property
    for name in values:
        property_budget -= 1
        if property_budget < 0:
            failure = "property authoring budget exceeded"
            return false
        if not types.has(name) or name == "script":
            failure = "unknown or protected property: " + str(name)
            return false
        var descriptor: Dictionary = types[name]
        var expected: String = str(descriptor.get("hint_string", "")) if descriptor.get("hint") == PROPERTY_HINT_RESOURCE_TYPE else ""
        var value = convert_value(values[name], int(descriptor.type), expected, depth)
        if not failure.is_empty():
            return false
        object.set(name, value)
    return true

func convert_value(value, type: int, expected: String = "", depth: int = 0):
    if typeof(value) == TYPE_DICTIONARY:
        if (value.has("resource_path") or value.has("expected_type")) and type != TYPE_OBJECT:
            failure = "typed resource reference requires a resource property"
            return null
        if type == TYPE_VECTOR2:
            return Vector2(value.get("x",0), value.get("y",0))
        if type == TYPE_VECTOR3:
            return Vector3(value.get("x",0), value.get("y",0), value.get("z",0))
        if type == TYPE_COLOR:
            return Color(value.get("r",0), value.get("g",0), value.get("b",0), value.get("a",1))
        if type == TYPE_OBJECT and (value.has("resource_path") or value.has("expected_type")):
            if not exact_fields(value,["resource_path","expected_type"]) or value.size() != 2 or expected.is_empty():
                failure = "typed resource reference requires its path, expected type and a resource property"
                return null
            var kind = value.get("expected_type")
            if kind not in ["Texture2D","Material","StandardMaterial3D","Mesh"]:
                failure = "unsupported expected resource type"
                return null
            var path := confined_resource(value.get("resource_path"))
            if path.is_empty():
                return null
            var referenced = ResourceLoader.load(path, str(kind), ResourceLoader.CACHE_MODE_IGNORE)
            if not referenced is Resource or not referenced.is_class(kind) or not resource_matches(referenced, expected):
                failure = "resource reference type does not match expected type and property"
                return null
            return referenced
        if type == TYPE_OBJECT and value.has("resource"):
            var kind: String = value.resource
            if not ClassDB.class_exists(kind) or not ClassDB.can_instantiate(kind) or not ClassDB.is_parent_class(kind,"Resource"):
                failure = "invalid resource class: " + kind
                return null
            var resource = ClassDB.instantiate(kind)
            if not expected.is_empty() and not resource_matches(resource, expected):
                failure = "inline resource type does not match property"
                return null
            if not properties(resource, value.get("properties",{}), depth + 1):
                return null
            return resource
    if type == TYPE_NODE_PATH:
        return NodePath(str(value))
    if type == TYPE_STRING_NAME:
        return StringName(str(value))
    return value

func finite_number(value, limit: float = 100000.0) -> bool:
    return typeof(value) in [TYPE_INT,TYPE_FLOAT] and is_finite(float(value)) and abs(float(value)) <= limit

func set_transform(scene: Node, operation: Dictionary) -> bool:
    if not exact_fields(operation,["op","node","position","rotation_degrees","scale","position_snap"]):
        return false
    var path = operation.get("node")
    if not path is String or path.is_empty() or path.length() > 512 or path.begins_with("/") or "\\" in path or ":" in path or ".." in path.split("/"):
        failure = "set_transform node must be a bounded local scene path"
        return false
    if not operation.has("position") and not operation.has("rotation_degrees") and not operation.has("scale"):
        failure = "set_transform requires position, rotation_degrees or scale"
        return false
    if operation.has("position_snap") and not operation.has("position"):
        failure = "set_transform position_snap requires position"
        return false
    var values := {}
    for property in ["position","rotation_degrees","scale"]:
        if not operation.has(property):
            continue
        var value = operation[property]
        if not value is Array or value.size() != 3:
            failure = "set_transform values must be exactly three bounded finite numbers"
            return false
        var component_limit: float = TRANSFORM_SCALE_MAX_ADMISSION if property == "scale" else 100000.0
        for component in value:
            if not finite_number(component,component_limit):
                failure = "set_transform values must be exactly three bounded finite numbers"
                return false
        if property == "scale":
            var positive: bool = float(value[0]) > 0.0
            for component in value:
                if abs(float(component)) < TRANSFORM_SCALE_MIN_ADMISSION or (float(component) > 0.0) != positive:
                    failure = "set_transform scale requires one strict sign and nominal magnitude 0.000001..100000 with relative boundary tolerance 2^-21"
                    return false
        values[property] = value.duplicate()
    if operation.has("position_snap"):
        var step = operation.position_snap
        if not finite_number(step) or float(step) <= 0.0:
            failure = "set_transform position_snap must be a positive bounded finite number"
            return false
        for axis in range(3):
            var ratio: float = float(values.position[axis]) / float(step)
            if not is_finite(ratio):
                failure = "set_transform snapped position exceeds finite transform bounds"
                return false
            # Exact halves round away from zero; never clamp an out-of-range result.
            var whole: float = floor(abs(ratio))
            if abs(ratio) - whole >= 0.5:
                whole += 1.0
            var snapped: float = (-whole if ratio < 0.0 else whole) * float(step)
            if not finite_number(snapped):
                failure = "set_transform snapped position exceeds finite transform bounds"
                return false
            values.position[axis] = snapped
    var node := find_node(scene,path)
    if not node is Node3D:
        failure = "set_transform target must be a local Node3D"
        return false
    # Validate every supplied field before changing any transform or instance override.
    mark_editable(scene,node)
    for property in ["position","rotation_degrees","scale"]:
        if values.has(property):
            var value: Array = values[property]
            node.set(property,Vector3(float(value[0]),float(value[1]),float(value[2])))
    return true

func animate_transform(scene: Node, node: Node, operation: Dictionary) -> bool:
    if not node is AnimationPlayer or not exact_fields(operation,["op","node","animation","length","loop","tracks"]):
        failure = "transform animation requires a local AnimationPlayer and supported fields"
        return false
    var name = operation.get("animation")
    var length = operation.get("length")
    var loop = operation.get("loop",false)
    var tracks = operation.get("tracks")
    if not name is String or name.is_empty() or name.length() > 64 or name.validate_node_name() != name or "/" in name or node.has_animation(name):
        failure = "animation name is invalid or already exists"
        return false
    if not finite_number(length,600.0) or float(length) <= 0.0 or not loop is bool or not tracks is Array or tracks.is_empty() or tracks.size() > 16:
        failure = "animation requires bounded length, loop and transform tracks"
        return false
    var animation_root: Node = node.get_node_or_null(node.root_node)
    if animation_root == null or (animation_root != scene and not scene.is_ancestor_of(animation_root)):
        failure = "animation root must remain local to the edited scene"
        return false
    var animation := Animation.new()
    animation.length = float(length)
    animation.loop_mode = Animation.LOOP_LINEAR if loop else Animation.LOOP_NONE
    var used := {}
    var total := 0
    for track in tracks:
        if not track is Dictionary or not exact_fields(track,["target","property","keys"]) or track.size() != 3:
            failure = "invalid transform animation track"
            return false
        var target_path = track.get("target")
        var property = track.get("property")
        var keys = track.get("keys")
        if not target_path is String or target_path.length() > 512 or ":" in target_path or property not in ["position","rotation","scale"] or not keys is Array or keys.is_empty():
            failure = "only explicit local position, quaternion rotation and scale tracks are supported"
            return false
        var target := find_node(animation_root,target_path)
        if not target is Node3D:
            failure = "transform animation target must be a local Node3D"
            return false
        var identity: String = str(animation_root.get_path_to(target)) + ":" + property
        if used.has(identity):
            failure = "duplicate transform animation target/property"
            return false
        used[identity] = true
        total += keys.size()
        if total > 256:
            failure = "animation is limited to 256 explicit keys"
            return false
        var type: int = {"position":Animation.TYPE_POSITION_3D,"rotation":Animation.TYPE_ROTATION_3D,"scale":Animation.TYPE_SCALE_3D}[property]
        var index := animation.add_track(type)
        animation.track_set_path(index,animation_root.get_path_to(target))
        animation.track_set_interpolation_type(index,Animation.INTERPOLATION_LINEAR)
        var previous := -1.0
        for key in keys:
            if not key is Dictionary or not exact_fields(key,["time","value"]) or key.size() != 2:
                failure = "invalid transform animation key"
                return false
            var time = key.get("time")
            var value = key.get("value")
            var width := 4 if property == "rotation" else 3
            if not finite_number(time,600.0) or float(time) < 0.0 or float(time) > float(length) or float(time) <= previous or not value is Array or value.size() != width:
                failure = "animation times must be finite, ascending and within length; key values must match transform type"
                return false
            for component in value:
                if not finite_number(component):
                    failure = "animation values must be bounded finite numbers"
                    return false
            var converted
            if property == "rotation":
                var rotation := Quaternion(float(value[0]),float(value[1]),float(value[2]),float(value[3]))
                if abs(rotation.length_squared()-1.0) > 0.001:
                    failure = "animation rotation must be a normalized quaternion"
                    return false
                converted = rotation
            else:
                converted = Vector3(float(value[0]),float(value[1]),float(value[2]))
            if animation.track_insert_key(index,float(time),converted) < 0:
                failure = "animation key could not be inserted"
                return false
            previous = float(time)
    var library := AnimationLibrary.new()
    if node.has_animation_library(""):
        library = node.get_animation_library("").duplicate(true)
        library.resource_path = ""
        node.remove_animation_library("")
    if library.add_animation(name,animation) != OK or node.add_animation_library("",library) != OK:
        failure = "animation library could not be authored"
        return false
    mark_editable(scene,node)
    return true

func texture_description(texture: Texture2D) -> Dictionary:
    return {"type":texture.get_class(),"path":texture.resource_path.substr(0,1024),"width":texture.get_width(),"height":texture.get_height()}

func material_description(material: Material) -> Dictionary:
    if material == null:
        return {}
    resource_budget -= 1
    if resource_budget < 0:
        inspection_truncated = true
        return {"truncated":true}
    var result := {"type":material.get_class(),"path":material.resource_path.substr(0,1024)}
    if material is BaseMaterial3D:
        result.albedo_color = [material.albedo_color.r,material.albedo_color.g,material.albedo_color.b,material.albedo_color.a]
        result.roughness = material.roughness
        result.metallic = material.metallic
        if material.albedo_texture != null:
            result.albedo_texture = texture_description(material.albedo_texture)
        if material.normal_texture != null:
            result.normal_texture = texture_description(material.normal_texture)
    return result

func animation_description(player: AnimationPlayer) -> Array:
    var result := []
    var libraries := player.get_animation_library_list()
    libraries.sort()
    for library_name in libraries:
        if result.size() >= 16 or resource_budget <= 0:
            inspection_truncated = true
            break
        var library: AnimationLibrary = player.get_animation_library(library_name)
        var row := {"name":str(library_name),"animations":[]}
        var animations := library.get_animation_list()
        animations.sort()
        for animation_name in animations:
            if row.animations.size() >= 16 or resource_budget <= 0:
                inspection_truncated = true
                break
            resource_budget -= 1
            var animation: Animation = library.get_animation(animation_name)
            var description := {"name":str(animation_name),"length":animation.length,"loop_mode":animation.loop_mode,"tracks":[]}
            for index in range(min(animation.get_track_count(),16)):
                var type := animation.track_get_type(index)
                var track := {"type":type,"target":str(animation.track_get_path(index)).substr(0,512),"key_count":animation.track_get_key_count(index),"keys":[]}
                if type in [Animation.TYPE_POSITION_3D,Animation.TYPE_ROTATION_3D,Animation.TYPE_SCALE_3D]:
                    for key in range(animation.track_get_key_count(index)):
                        if animation_key_budget <= 0:
                            inspection_truncated = true
                            break
                        animation_key_budget -= 1
                        var value = animation.track_get_key_value(index,key)
                        var encoded: Array = [value.x,value.y,value.z,value.w] if value is Quaternion else [value.x,value.y,value.z]
                        track.keys.append({"time":animation.track_get_key_time(index,key),"value":encoded})
                description.tracks.append(track)
            if animation.get_track_count() > 16:
                inspection_truncated = true
            row.animations.append(description)
        result.append(row)
    return result

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
        result.mesh = {"type": node.mesh.get_class(), "path": node.mesh.resource_path.substr(0,1024),"surface_count":node.mesh.get_surface_count(),"surfaces":[]}
        result.material_override = material_description(node.material_override)
        for index in range(min(node.mesh.get_surface_count(),16)):
            result.mesh.surfaces.append({"index":index,"material":material_description(node.get_active_material(index))})
        if node.mesh.get_surface_count() > 16:
            inspection_truncated = true
    if node is AnimationPlayer:
        result.animation_libraries = animation_description(node)
    if depth < 32:
        for child in node.get_children():
            if node_budget <= 0:
                break
            result.children.append(describe(child,scene,depth+1))
    return result
