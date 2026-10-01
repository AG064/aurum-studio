@tool
extends EditorExportPlugin

func _get_name() -> String:
	return "OrbitBreakNotices"

func _export_begin(_features: PackedStringArray, _is_debug: bool, _path: String, _flags: int) -> void:
	# Aurum supplies export presets, so the notice must travel independently of filters.
	var license_path = "res://assets/fonts/OFL.txt"
	if not FileAccess.file_exists(license_path):
		push_error("The bundled display font's license is missing.")
		return
	add_file(license_path,FileAccess.get_file_as_bytes(license_path),false)
