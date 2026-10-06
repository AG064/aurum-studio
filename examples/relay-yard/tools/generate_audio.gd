extends SceneTree
## Original deterministic score and effects, baked once rather than synthesized per frame.
const RATE = 22050
var random := RandomNumberGenerator.new()

func _initialize() -> void:
	var args := OS.get_cmdline_user_args()
	if args.size() != 1 or DirAccess.dir_exists_absolute(args[0]):
		push_error("Supply a new output directory")
		quit(1)
		return
	DirAccess.make_dir_recursive_absolute(args[0])
	random.seed = 2091
	for entry in [["shot", 0.12, 850, 100, 0.22], ["hit", 0.16, 180, 60, 0.28], ["dash", 0.22, 160, 480, 0.24], ["emp", 0.7, 90, 600, 0.30], ["pickup", 0.4, 390, 900, 0.23], ["relay", 0.8, 220, 660, 0.25], ["alarm", 0.45, 330, 220, 0.22], ["explode", 0.5, 100, 35, 0.32], ["ui", 0.07, 650, 800, 0.14]]:
		_effect(args[0], entry)
	_music(args[0])
	print("ORIGINAL_AUDIO_READY")
	quit(0)

func _save(path: String, values: PackedFloat32Array) -> void:
	var data := PackedByteArray()
	data.resize(values.size() * 2)
	for index in values.size():
		var sample := int(clampf(values[index], -0.8, 0.8) * 32767)
		data.encode_s16(index * 2, sample)
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = RATE
	stream.data = data
	if stream.save_to_wav(path) != OK: push_error("Could not write audio")

func _effect(directory: String, entry: Array) -> void:
	var count := int(float(entry[1]) * RATE)
	var samples := PackedFloat32Array()
	samples.resize(count)
	var phase := 0.0
	for index in count:
		var progress := float(index) / count
		phase += TAU * lerpf(float(entry[2]), float(entry[3]), progress) / RATE
		var envelope := minf(1, progress * 25) * pow(1 - progress, 2)
		var noise := random.randf_range(-1, 1) * (0.55 if entry[0] in ["hit", "explode", "dash"] else 0.05)
		samples[index] = (sin(phase) * 0.7 + sin(phase * 1.5) * 0.15 + noise) * envelope * float(entry[4])
	_save(directory.path_join(str(entry[0]) + ".wav"), samples)

func _music(directory: String) -> void:
	var duration := 32.0
	var samples := PackedFloat32Array()
	samples.resize(int(duration * RATE))
	var bass_notes := [55.0, 65.406, 73.416, 49.0]
	var melody := [220.0, 0.0, 293.665, 261.626, 0.0, 329.628, 293.665, 196.0]
	for index in samples.size():
		var time := float(index) / RATE
		var chord := int(time / 8.0) % 4
		var beat := fmod(time, 0.5)
		var bass: float = bass_notes[chord]
		var pad := (sin(TAU * bass * time) + sin(TAU * bass * 1.5 * time) * 0.5 + sin(TAU * bass * 2 * time) * 0.25) * 0.045
		var pulse := sin(TAU * bass * 2 * time) * exp(-beat * 8) * 0.042
		var kick := sin(TAU * (50 * beat + 5 * (1 - exp(-beat * 30)))) * exp(-beat * 24) * 0.12
		var note: float = melody[int(time / 0.5) % melody.size()]
		var lead := sin(TAU * note * time) * exp(-beat * 7) * 0.024 if note > 0 else 0.0
		var hat := random.randf_range(-1, 1) * exp(-fmod(time, 0.25) * 80) * 0.02
		samples[index] = (pad + pulse + kick + lead + hat) * minf(1, time * 2) * minf(1, (duration - time) * 2)
	_save(directory.path_join("nightshift.wav"), samples)
