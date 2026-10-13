extends SceneTree
func _init():
	var config = ConfigFile.new()
	if config.load("res://project.godot") != OK:
		push_error("Could not read private native project settings")
		quit(1)
		return
	if config.has_section_key("autoload", "AurumLive"):
		push_error("AurumLive is reserved for private managed native snapshots")
		quit(1)
		return
	config.set_value("autoload", "AurumLive", "*res://addons/aurum_live/runtime.gd")
	# Output already goes through Aurum's bounded logger. Avoid duplicate logs
	# in user:// and log rotation changing save-copy fingerprints.
	config.set_value("debug", "file_logging/enable_file_logging", false)
	if config.save("res://project.godot") != OK:
		push_error("Could not save private native project settings")
		quit(1)
		return
	quit()
