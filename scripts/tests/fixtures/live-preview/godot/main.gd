extends Node2D
@export var speed: float = 7.0
@export var counter: int = 41
@export var caption: String = "Original"
@export var accent: Color = Color("c9af79")
@export var config_present: bool = false
@export var notice_present: bool = false
@export var private_present: bool = false
@export var elapsed: float = 0.0

func _ready():
	config_present = FileAccess.file_exists("res://extra/config.json")
	notice_present = FileAccess.file_exists("res://assets/NOTICE.txt")
	private_present = FileAccess.file_exists("res://private/internal.json")

func _process(delta):
	elapsed += delta
	if Input.is_action_just_pressed("ui_accept"): counter += 1
	queue_redraw()

func _draw():
	draw_rect(Rect2(0,0,960,540),Color("111820"))
	draw_line(Vector2(64,160),Vector2(896,160),Color("38434b"),1.0)
	draw_circle(Vector2(120+speed*12,300),36,accent)
	draw_string(ThemeDB.fallback_font,Vector2(64,104),caption,HORIZONTAL_ALIGNMENT_LEFT,-1,32,Color("eee5d4"))
	draw_string(ThemeDB.fallback_font,Vector2(64,140),"Checkpoint %d" % counter,HORIZONTAL_ALIGNMENT_LEFT,-1,18,Color("a0abb5"))
