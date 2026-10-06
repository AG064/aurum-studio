extends RefCounted
## Validate and migrate a detached JSON checkpoint before touching a live scene.
## Games own their field validation; this helper owns version and copy boundaries.

static func migrate(input: Variant, current_version: int, steps: Dictionary) -> Dictionary:
	if current_version < 1 or current_version > 64:
		return {"ok": false, "error": "Supported migration versions are 1 through 64"}
	if not input is Dictionary or not input.has("version"):
		return {"ok": false, "error": "Checkpoint needs a versioned object"}
	var raw: Variant = input.version
	if not (raw is int or raw is float) or not is_finite(float(raw)) or float(raw) != floor(float(raw)):
		return {"ok": false, "error": "Checkpoint version must be an integer"}
	var version := int(raw)
	if version < 1 or version > current_version:
		return {"ok": false, "error": "Unsupported checkpoint version"}
	var encoded := JSON.stringify(input)
	if encoded.to_utf8_buffer().size() > 65536:
		return {"ok": false, "error": "Checkpoint exceeds 64 KiB"}
	var state: Dictionary = input.duplicate(true)
	while version < current_version:
		if not steps.has(version) or not steps[version] is Callable:
			return {"ok": false, "error": "Checkpoint migration is missing"}
		var next: Variant = steps[version].call(state)
		if not next is Dictionary or next.get("version") != version + 1:
			return {"ok": false, "error": "Checkpoint migration did not advance one version"}
		state = next
		version += 1
	return {"ok": true, "state": state}
