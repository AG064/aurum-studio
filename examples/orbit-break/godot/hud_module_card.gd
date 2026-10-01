extends Button

const Style = preload("res://hud_style.gd")
var accent = Color("69f4de")
var symbol = 0
var key_hint = "1"
var design_size = Vector2(324,322)
var compact_row = false
var dense = false
var reduced_motion = false
var highlight = 0.0

func _ready() -> void:
	for state in ["normal","hover","pressed","focus","disabled"]:
		add_theme_stylebox_override(state,StyleBoxEmpty.new())
	set_process(true)

func _process(delta: float) -> void:
	var target = 1.0 if is_hovered() else (0.55 if has_focus() else 0.0)
	var next = target if reduced_motion else move_toward(highlight,target,delta*7.0)
	if next != highlight:
		highlight = next
		queue_redraw()

func _draw() -> void:
	var factor = minf(size.x/design_size.x,size.y/design_size.y)
	draw_set_transform(Vector2.ZERO,0,Vector2.ONE*factor)
	var bounds = Rect2(Vector2(1,1),design_size-Vector2(2,7))
	Style.plate(self,bounds,Color("11202b"),accent,9,highlight)
	draw_rect(Rect2(12,1,design_size.x-24,2),Color(accent,0.45+highlight*0.55))
	var bay = Rect2(14,28,72,72) if compact_row else Rect2(20,50 if dense else 58,design_size.x-40,62 if dense else 80)
	Style.well(self,bay,accent)
	Style.equipment(self,symbol,bay.get_center()-Vector2(2,2),0.77 if compact_row or dense else 0.94,accent.lerp(Color.WHITE,highlight*0.16))
	var key_rect = Rect2(design_size.x-42,17,24,24)
	Style.plate(self,key_rect,accent if highlight>0.7 else Color("1f3441"),accent,3,0)
	if has_focus():
		var focus_outline = Style.outline(bounds.grow(-1),8)
		focus_outline.append(focus_outline[0])
		draw_polyline(focus_outline,Color(accent,0.8),2,true)
	var key_font = Style.DISPLAY_FONT
	draw_set_transform(Vector2.ZERO,0,Vector2.ONE)
	draw_string(key_font,(key_rect.position+Vector2(8,17))*factor,key_hint,HORIZONTAL_ALIGNMENT_LEFT,-1,maxi(1,roundi(13*factor)),Color("0b151b") if highlight>0.7 else Color("d9e5e7"))
