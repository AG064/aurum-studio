extends SceneTree
var _source_dir = ""

func _init():
	var args = OS.get_cmdline_user_args()
	if args.size() != 7:
		push_error("Preview setup arguments are incomplete")
		quit(1)
		return
	_source_dir = args[6].replace("\\","/").trim_suffix("/")
	# Only the owned native editor bridge is omitted from new preview copies.
	# Keep other export plugins, including project-specific asset importers.
	var snapshot_project = ConfigFile.new()
	if snapshot_project.load(_local("res://project.godot")) != OK:
		_fail("Cannot read the copied preview project")
		return
	var active_plugins = snapshot_project.get_value("editor_plugins","enabled",PackedStringArray())
	var preview_plugins = PackedStringArray()
	for plugin in active_plugins:
		if str(plugin) != "res://addons/aurum_editor/plugin.cfg": preview_plugins.append(str(plugin))
	snapshot_project.set_value("editor_plugins","enabled",preview_plugins)
	if snapshot_project.save(_local("res://project.godot")) != OK:
		_fail("Cannot configure plugins in the copied preview project")
		return
	var presets = ConfigFile.new()
	var preset_file = _local("res://export_presets.cfg")
	if FileAccess.file_exists(preset_file) and presets.load(preset_file) != OK:
		_fail("Cannot read project export presets")
		return
	var selected = ""
	for section in presets.get_sections():
		if section.begins_with("preset.") and not section.ends_with(".options") and presets.get_value(section, "platform", "") == "Web" and (args[5] == "" or presets.get_value(section, "name", "") == args[5]):
			selected = section
			break
	if selected == "":
		if args[5] != "":
			_fail("The configured web.preset was not found")
			return
		selected = "preset.0"
		var index = 0
		while presets.has_section(selected):
			index += 1
			selected = "preset.%d" % index
		presets.set_value(selected, "name", "Aurum Preview")
		presets.set_value(selected, "platform", "Web")
		presets.set_value(selected, "export_filter", "all_resources")
		presets.set_value(selected, "script_export_mode", 2)
	var filters = str(presets.get_value(selected, "include_filter", ""))
	for pattern in ["tuning.json", "addons/aurum_live/*", "*LICENSE*", "*license*", "*OFL*", "*NOTICE*", "*notice*", "*COPYING*"]:
		if not pattern in filters.split(","):
			filters += ("," if filters != "" else "") + pattern
	presets.set_value(selected, "include_filter", filters)
	var excludes = str(presets.get_value(selected, "exclude_filter", ""))
	for pattern in ["tests/*", ".aurum/*", "dist/*"]:
		if not pattern in excludes.split(","):
			excludes += ("," if excludes != "" else "") + pattern
	presets.set_value(selected, "exclude_filter", excludes)
	var options = selected + ".options"
	if presets.get_value(options, "variant/thread_support", false):
		_fail("The isolated preview requires a non-threaded Web preset; keep threaded builds as a separate export target")
		return
	presets.set_value(options, "variant/thread_support", false)
	presets.set_value(options, "html/canvas_resize_policy", 2)
	presets.set_value(options, "progressive_web_app/enabled", false)
	var extensions: Array[String] = []
	_collect_extensions(_source_dir, extensions)
	var artifacts = []
	var host_artifacts = []
	if args[4] != "":
		var built = JSON.parse_string(FileAccess.get_file_as_string(args[4]))
		if not built is Dictionary or not built.get("ok", false):
			_fail("The Rust Web build receipt is invalid")
			return
		artifacts = built.get("artifacts", [])
		host_artifacts = built.get("host_artifacts", [])
	for manifest in extensions:
		var config = ConfigFile.new()
		if config.load(_local(manifest)) != OK:
			_fail("Invalid extension manifest: " + manifest)
			return
		for artifact in host_artifacts:
			var matching = extensions.size() == 1 and host_artifacts.size() == 1
			for key in config.get_section_keys("libraries") if config.has_section("libraries") else []:
				if str(config.get_value("libraries",key)).get_file().begins_with(str(artifact.name)): matching = true
			if not matching: continue
			var native_library = manifest.get_base_dir().path_join(str(artifact.path).get_file())
			if DirAccess.copy_absolute(str(artifact.path),_local(native_library)) != OK:
				_fail("Could not stage the host class registry")
				return
			var platform = {"Windows":"windows","Linux":"linux","macOS":"macos"}.get(OS.get_name(),"")
			if platform == "":
				_fail("Host extension import is not configured for this operating system")
				return
			config.set_value("libraries",platform+".debug."+Engine.get_architecture_name(),native_library)
			if config.save(_local(manifest)) != OK:
				_fail("Could not save the snapshot host registry mapping")
				return
		var library = ""
		for key in config.get_section_keys("libraries") if config.has_section("libraries") else []:
			if str(key).begins_with("web.release") and not "threads" in str(key):
				library = str(config.get_value("libraries", key))
				break
		for artifact in artifacts:
			var matches_artifact = extensions.size() == 1 and artifacts.size() == 1
			for key in config.get_section_keys("libraries") if config.has_section("libraries") else []:
				if str(config.get_value("libraries", key)).get_file().begins_with(str(artifact.name)):
					matches_artifact = true
			if matches_artifact:
				if library == "":
					library = manifest.get_base_dir().path_join(str(artifact.name) + ".wasm")
				if not library.begins_with("res://") or ".." in library.trim_prefix("res://").split("/"):
					_fail("Web extension libraries must stay inside the project")
					return
				DirAccess.make_dir_recursive_absolute(_local(library.get_base_dir()))
				if DirAccess.copy_absolute(str(artifact.path), _local(library)) != OK:
					_fail("Could not stage the Rust Web artifact")
					return
				config.set_value("libraries", "web.release.wasm32", library)
				if config.save(_local(manifest)) != OK:
					_fail("Could not update the snapshot Web library mapping")
					return
				break
		if not library.begins_with("res://") or ".." in library.trim_prefix("res://").split("/") or not library.ends_with(".wasm") or not FileAccess.file_exists(_local(library)):
			_fail("Extension needs an existing, project-local non-threaded web.release.wasm32 library: " + manifest)
			return
	presets.set_value(options, "variant/extensions_support", not extensions.is_empty())
	var template = str(presets.get_value(options, "custom_template/release", ""))
	if template == "":
		template = args[0] if extensions.is_empty() else args[3]
	if not FileAccess.file_exists(_local(template)):
		_fail("Matching Web export template is missing: " + template)
		return
	presets.set_value(options, "custom_template/release", template)
	if presets.save(preset_file) != OK:
		_fail("Cannot save snapshot export presets")
		return
	var project = ConfigFile.new()
	if project.load(_local("res://project.godot")) != OK:
		_fail("Cannot read snapshot project settings")
		return
	if project.has_section_key("autoload", "AurumLive"):
		_fail("AurumLive is reserved for the managed preview bridge")
		return
	project.set_value("autoload", "AurumLive", "*" + args[2])
	if project.save(_local("res://project.godot")) != OK:
		_fail("Cannot save snapshot runtime bridge")
		return
	var report = FileAccess.open(args[1], FileAccess.WRITE)
	if report == null:
		_fail("Cannot write preview setup receipt")
		return
	report.store_string(JSON.stringify({"ok": true, "preset": presets.get_value(selected, "name"), "extensions": extensions, "requires_isolation": not extensions.is_empty()}))
	report.close()
	call_deferred("_finish")

func _finish():
	await process_frame
	await process_frame
	quit()

func _local(path: String) -> String:
	return _source_dir.path_join(path.trim_prefix("res://")) if path.begins_with("res://") else path

func _collect_extensions(path: String, result: Array[String]):
	for file in DirAccess.get_files_at(path):
		if file.ends_with(".gdextension"):
			result.append("res://" + path.path_join(file).trim_prefix(_source_dir + "/"))
	for directory in DirAccess.get_directories_at(path):
		if not directory.begins_with("."):
			_collect_extensions(path.path_join(directory), result)

func _fail(message: String):
	push_error(message)
	quit(1)
