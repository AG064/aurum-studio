@tool
extends EditorPlugin

const NoticeExporter = preload("res://addons/game_notices/export_notices.gd")
var exporter: EditorExportPlugin

func _enter_tree() -> void:
	exporter = NoticeExporter.new()
	add_export_plugin(exporter)

func _exit_tree() -> void:
	remove_export_plugin(exporter)
	exporter = null
