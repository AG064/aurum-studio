extends SceneTree

# Only the Rust-admitted quoted subset reaches this neutral project. No
# ResourceLoader, extension manager, original project or library is opened.
func _initialize() -> void:
	var input := FileAccess.open("res://input.json", FileAccess.READ)
	if input == null or input.get_length() > 262144:
		quit(2)
		return
	var request = JSON.parse_string(input.get_as_text())
	input.close()
	if not request is Dictionary or request.size() != 3:
		quit(2)
		return
	if request.get("schema") != "aurum.configfile.input/1" or not request.get("text") is String or not request.get("target_tags") is Array:
		quit(2)
		return
	var config := ConfigFile.new()
	var error := config.parse(request.text)
	if error != OK:
		quit(2)
		return
	var sections: Array = []
	var libraries: Array = []
	for section in config.get_sections():
		if section != "configuration" and section != "libraries":
			quit(2)
			return
		var entries: Array = []
		for key in config.get_section_keys(section):
			var value = config.get_value(section, key)
			var kind := ""
			if typeof(value) == TYPE_STRING:
				kind = "string"
			elif typeof(value) == TYPE_BOOL:
				kind = "bool"
			else:
				quit(2)
				return
			entries.append({"key":key,"type":kind,"value":value})
			if section == "libraries":
				var flags: Array = Array(key.split("."))
				var selected := true
				for flag in flags:
					if not request.target_tags.has(flag):
						selected = false
				libraries.append({"key":key,"flags":flags,"path":value,"matches_target":selected})
		sections.append({"name":section,"entries":entries})
	var observed: Array = []
	for tag in ["windows","debug","release","x86_64","arm64","single","double","64","32","editor","template"]:
		if OS.has_feature(tag):
			observed.append(tag)
	var result := {"schema":"aurum.configfile.output/1","parse_error":0,"sections":sections,"libraries":libraries,"dependencies":[],"target_tags":request.target_tags,"observed_tags":observed}
	var encoded := JSON.stringify(result)
	if encoded.to_utf8_buffer().size() > 262000:
		quit(2)
		return
	print("AURUM_CONFIGFILE_PARSED " + encoded)
	quit(0)
