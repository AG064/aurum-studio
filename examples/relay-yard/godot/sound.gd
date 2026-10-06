extends Node
var players: Array[AudioStreamPlayer] = []
var music: AudioStreamPlayer
var sounds: Dictionary = {}
var volume := 0.6
var event_count := 0
var silent_driver := false

func _ready() -> void:
	# Dummy has no audible mixer. Keep event evidence without queuing real playbacks.
	silent_driver = AudioServer.get_driver_name() == "Dummy"
	for key in ["shot", "hit", "dash", "emp", "pickup", "relay", "alarm", "explode", "ui"]:
		sounds[key] = load("res://assets/audio/%s.wav" % key)
	for index in 12:
		var player := AudioStreamPlayer.new()
		add_child(player)
		players.append(player)
	music = AudioStreamPlayer.new()
	add_child(music)
	var track: AudioStreamWAV = load("res://assets/audio/nightshift.wav")
	track.loop_mode = AudioStreamWAV.LOOP_FORWARD
	track.loop_end = track.data.size() / 2
	music.stream = track
	music.volume_db = -13

func start_music() -> void:
	if not silent_driver and not music.playing: music.play()

func play(key: String) -> void:
	if not sounds.has(key): return
	event_count += 1
	if silent_driver: return
	for player in players:
		if not player.playing:
			player.stream = sounds[key]
			player.volume_db = linear_to_db(maxf(0.001, volume)) - (7 if key == "shot" else 2)
			player.play()
			return

func set_volume(value: float) -> void:
	volume = clampf(value, 0, 1)
	music.volume_db = linear_to_db(maxf(0.001, volume)) - 10

func shutdown() -> void:
	for player in players:
		player.stop()
		player.stream = null
	if music != null:
		music.stop()
		music.stream = null
	sounds.clear()

func _exit_tree() -> void:
	shutdown()
