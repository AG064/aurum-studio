extends SceneTree
func _init():
	var args = OS.get_cmdline_user_args()
	if args.size() != 3:
		quit(1)
		return
	var input = JSON.parse_string(FileAccess.get_file_as_string(args[0]))
	var helper = load(args[2]).new()
	var result = helper.plan(input.project, input.godot, input.manifests)
	var file = FileAccess.open(args[1], FileAccess.WRITE)
	if file == null:
		quit(1)
		return
	file.store_string(JSON.stringify(result))
	file.close()
	quit(0 if result.get("ok", false) else 1)
