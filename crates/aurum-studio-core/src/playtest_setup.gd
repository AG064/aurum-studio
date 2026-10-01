extends SceneTree
func _init():
	var config = ConfigFile.new()
	if config.load("res://project.godot") != OK:
		push_error("Could not read playtest snapshot settings")
		quit(1)
		return
	if config.has_section_key("autoload", "AurumLive") or config.has_section_key("autoload", "AurumPlaytest"):
		push_error("AurumLive and AurumPlaytest are reserved playtest autoload names")
		quit(1)
		return
	config.set_value("autoload", "AurumLive", "*res://addons/aurum_live/runtime.gd")
	config.set_value("autoload", "AurumPlaytest", "*res://addons/aurum_live/playtest.gd")
	if config.save("res://project.godot") != OK:
		push_error("Could not save playtest snapshot settings")
		quit(1)
		return
	quit()
