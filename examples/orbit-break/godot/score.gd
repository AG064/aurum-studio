extends Node

const TRACKS = [preload("res://assets/music/breakwater.wav"),preload("res://assets/music/aperture.wav"),preload("res://assets/music/foundry.wav")]
var player: AudioStreamPlayer
var current = -1

func _ready() -> void:
	player = AudioStreamPlayer.new()
	player.volume_db = -25.0
	add_child(player)

func update_music(act: int, phase: String, muted: bool, delta: float) -> void:
	if current!=act:
		current = act
		var stream: AudioStreamWAV = TRACKS[clampi(act,0,2)].duplicate()
		stream.loop_mode = AudioStreamWAV.LOOP_FORWARD
		stream.loop_begin = 0
		stream.loop_end = roundi(stream.get_length()*stream.mix_rate)
		player.stream = stream
		player.play()
	player.stream_paused = muted
	player.volume_db = move_toward(player.volume_db,-20.0 if phase=="playing" else -27.0,delta*8.0)

func _exit_tree() -> void:
	if is_instance_valid(player):
		player.stop()
		player.stream = null
