extends RefCounted
# ConfigFile is the authoritative parser for .gdextension Variant syntax.
func plan(project_root: String, godot_root: String, manifests: Array, loaded: bool = false) -> Dictionary:
	if manifests.size() > 16: return {"ok":false,"error":"Native dependencies exceed 16 manifests"}
	var results = []
	for manifest in manifests:
		if not manifest is String: return {"ok":false,"error":"Invalid native manifest path"}
		if not manifest.begins_with(godot_root.trim_suffix("/") + "/") or not manifest.ends_with(".gdextension") or ".." in manifest.split("/"): return {"ok":false,"error":"Native manifest is outside the private Godot project"}
		var config = ConfigFile.new()
		if config.load(manifest) != OK: return {"ok":false,"error":"Could not parse native manifest"}
		if not config.has_section("libraries"): return {"ok":false,"error":"Native manifest has no libraries"}
		var entry = config.get_value("configuration", "entry_symbol", "")
		if not entry is String or entry.is_empty() or entry.length() > 256: return {"ok":false,"error":"Native entry symbol is invalid"}
		var selected = ""
		var references = []
		var keys = config.get_section_keys("libraries")
		if keys.size() > 128: return {"ok":false,"error":"Too many native library variants"}
		for key in keys:
			var path = _path(project_root, godot_root, manifest, config.get_value("libraries", key))
			if path.is_empty(): return {"ok":false,"error":"Native library path escapes private source or uses an unsupported scheme"}
			references.append(path)
			if selected.is_empty() and _matches(key): selected = path
		if selected.is_empty(): return {"ok":false,"error":"Native manifest has no matching runtime library"}
		var dependencies = []
		if config.has_section("dependencies"):
			keys = config.get_section_keys("dependencies")
			if keys.size() > 128: return {"ok":false,"error":"Too many native dependency variants"}
			for key in keys:
				var value = config.get_value("dependencies", key)
				if not value is Dictionary or value.size() > 128: return {"ok":false,"error":"Invalid native dependency dictionary"}
				for raw in value:
					var path = _path(project_root, godot_root, manifest, raw)
					if path.is_empty() or not value[raw] is String: return {"ok":false,"error":"Invalid native dependency path"}
					references.append(path)
					if _matches(key): dependencies.append(path)
		if references.size() > 512: return {"ok":false,"error":"Too many native dependency paths"}
		var resource = "res://" + manifest.trim_prefix(godot_root.trim_suffix("/") + "/")
		var result = {"manifest":manifest,"resource":resource,"library":selected,"dependencies":dependencies,"references":references,"entry_symbol":entry}
		if loaded:
			result["loaded"] = GDExtensionManager.is_extension_loaded(resource)
			result["sha256"] = FileAccess.get_sha256(selected) if FileAccess.file_exists(selected) else ""
			var hashes = {}
			for path in dependencies: hashes[path] = FileAccess.get_sha256(path) if FileAccess.file_exists(path) else ""
			result["dependency_hashes"] = hashes
		results.append(result)
	var architecture = "x86_64" if OS.has_feature("x86_64") else ("arm64" if OS.has_feature("arm64") else "unsupported")
	return {"ok":true,"manifests":results,"architecture":architecture,"debug":OS.is_debug_build()}

func _matches(key: String) -> bool:
	for flag in key.split("."):
		if flag.is_empty() or not OS.has_feature(flag): return false
	return true

func _path(root: String, godot: String, manifest: String, raw) -> String:
	if not raw is String or raw.is_empty() or raw.length() > 4096 or "\\" in raw: return ""
	var path: String
	if raw.begins_with("res://"):
		path = godot.path_join(raw.trim_prefix("res://"))
	elif raw.is_absolute_path() or ":" in raw:
		return ""
	else:
		path = manifest.get_base_dir().path_join(raw)
	path = path.simplify_path()
	var normalized = root.trim_suffix("/") + "/"
	if OS.get_name() == "Windows":
		if not path.to_lower().begins_with(normalized.to_lower()): return ""
	elif not path.begins_with(normalized): return ""
	return path
