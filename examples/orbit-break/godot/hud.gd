extends Control

const INK = Color("091525")
const TEAL = Color("69f4de")
const CREAM = Color("f5e8cf")
const MUTED = Color("94acbd")
const GOLD = Color("ffd080")
var game: Node3D
var buttons: Array[Dictionary] = []
var font: Font = ThemeDB.fallback_font

func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	resized.connect(_layout)

func _label(text: String, pos: Vector2, size_px: int, color = CREAM) -> void:
	draw_string(font,pos,text,HORIZONTAL_ALIGNMENT_LEFT,-1,size_px,color)

func _panel(rect: Rect2, color = INK, border = Color("274455")) -> void:
	var style = StyleBoxFlat.new()
	style.bg_color = color
	style.border_color = border
	style.set_border_width_all(1)
	style.set_corner_radius_all(12)
	draw_style_box(style,rect)

func _button(text: String, rect: Rect2, callback: Callable, primary = false) -> void:
	var button = Button.new()
	button.text = text
	if game.phase == "playing":
		button.focus_mode = Control.FOCUS_NONE
	button.mouse_default_cursor_shape = Control.CURSOR_POINTING_HAND
	button.add_theme_color_override("font_color",INK if primary else CREAM)
	button.add_theme_color_override("font_focus_color",INK if primary else CREAM)
	button.add_theme_color_override("font_hover_color",INK if primary else Color.WHITE)
	button.add_theme_color_override("font_pressed_color",INK if primary else Color.WHITE)
	for state in ["normal","hover","pressed","focus"]:
		var style = StyleBoxFlat.new()
		style.bg_color = TEAL if primary else Color("173447")
		if state == "hover":
			style.bg_color = style.bg_color.lightened(0.13)
		style.border_color = TEAL if state=="focus" else Color("396271")
		style.set_border_width_all(2 if state=="focus" else 1)
		style.set_corner_radius_all(8)
		button.add_theme_stylebox_override(state,style)
	button.pressed.connect(callback)
	add_child(button)
	buttons.append({"node":button,"rect":rect})

func _layout() -> void:
	for entry in buttons:
		entry.node.position = entry.rect.position * size / Vector2(1280,800)
		entry.node.size = entry.rect.size * size / Vector2(1280,800)
		entry.node.add_theme_font_size_override("font_size",int(19*minf(size.x/1280.0,size.y/800.0)))
	queue_redraw()

func refresh() -> void:
	for entry in buttons:
		entry.node.queue_free()
	buttons.clear()
	if game.phase == "menu":
		_button("LAUNCH RUN    /    ENTER",Rect2(120,512,470,58),game.start_run,true)
		_button("SOUND: OFF" if game.muted else "SOUND: ON",Rect2(120,588,225,46),game.toggle_mute)
		_button("EXIT",Rect2(365,588,225,46),func(): get_tree().quit())
	elif game.phase == "playing":
		_button("II",Rect2(1182,28,62,50),game.pause_game)
		if game.touch_mode:
			_button("DASH",Rect2(1050,620,154,110),game.dash,true)
	elif game.phase == "paused":
		_button("RESUME    /    ESC",Rect2(425,370,430,58),game.pause_game,true)
		_button("SOUND: OFF" if game.muted else "SOUND: ON",Rect2(425,450,430,50),game.toggle_mute)
		_button("RESTART RUN",Rect2(425,522,430,50),game.start_run)
	elif game.phase == "upgrade":
		_button("1   OVERDRIVE",Rect2(190,505,280,58),func(): game.choose_upgrade(0),true)
		_button("2   REINFORCE",Rect2(500,505,280,58),func(): game.choose_upgrade(1),true)
		_button("3   SPLIT SHOT",Rect2(810,505,280,58),func(): game.choose_upgrade(2),true)
	elif game.phase in ["won","lost"]:
		_button("FLY AGAIN    /    ENTER",Rect2(425,491,430,58),game.start_run,true)
		_button("MAIN MENU",Rect2(425,570,430,50),func(): game.phase="menu"; refresh())
	_layout()
	if buttons.size()>0 and game.phase!="playing":
		buttons[0].node.grab_focus()

func _draw() -> void:
	if not game:
		return
	draw_set_transform(Vector2.ZERO,0,size/Vector2(1280,800))
	if game.phase == "menu":
		_panel(Rect2(80,117,560,560),Color(0.025,0.065,0.11,0.97))
		_label("AURUM STUDIO   /   FIELD TEST 01",Vector2(120,166),17,TEAL)
		_label("ORBIT",Vector2(114,257),80)
		_label("BREAK",Vector2(114,336),80)
		draw_line(Vector2(120,365),Vector2(590,365),Color("315064"),1)
		_label("One pilot. Five waves. A way out.",Vector2(120,402),24)
		_label("Keep moving. Your ship fires automatically.",Vector2(120,442),18,MUTED)
		_label("Build your loadout. Defeat the Warden.",Vector2(120,471),18,MUTED)
		_label("BEST  %05d" % game.best,Vector2(954,699),21,GOLD)
		_controls()
		return
	_panel(Rect2(28,24,400,78),Color(0.025,0.065,0.11,0.94))
	_label("PILOT INTEGRITY",Vector2(48,50),14,MUTED)
	_label("%03d / %03d" % [ceili(game.health),int(game.max_health)],Vector2(286,51),17,CREAM)
	draw_rect(Rect2(48,67,358,12),Color("283645"))
	draw_rect(Rect2(48,67,358*clampf(game.health/game.max_health,0,1),12),TEAL if game.health>30 else Color("ff697c"))
	_panel(Rect2(449,24,350,78),Color(0.025,0.065,0.11,0.94))
	_label("WAVE  %02d / 05" % game.wave,Vector2(471,53),21)
	_label("%02d / %02d CONTACTS CLEARED" % [game.wave_kills,game.wave_quota],Vector2(471,80),14,MUTED)
	_label("%05d" % game.score,Vector2(984,55),30,GOLD)
	_label("%02d:%02d" % [int(game.run_time)/60,int(game.run_time)%60],Vector2(1100,54),19,MUTED)
	if game.phase == "playing":
		_panel(Rect2(28,702,255,62),Color(0.025,0.065,0.11,0.92))
		_label("DASH READY" if game.dash_cooldown<=0 else "DASH  %.1fs" % game.dash_cooldown,Vector2(48,729),18,TEAL if game.dash_cooldown<=0 else MUTED)
		draw_rect(Rect2(48,743,210,3),Color("304454"))
		draw_rect(Rect2(48,743,210*(1.0-clampf(game.dash_cooldown/2.2,0,1)),3),TEAL)
		_label("AUTO FIRE ACTIVE",Vector2(982,776),14,TEAL)
		if not game.touch_mode:
			_label("WASD  move     SPACE  dash     ESC  pause     M  sound",Vector2(365,762),16,MUTED)
		else:
			var origin = game.touch_origin if game.touch_id>=0 else Vector2(size.x*0.15,size.y*0.76)
			var cursor = origin+(game.touch_position-origin).limit_length(65.0) if game.touch_id>=0 else origin
			draw_arc(origin*Vector2(1280,800)/size,65,0,TAU,48,Color(0.4,0.9,0.85,0.5),2,true)
			draw_circle(cursor*Vector2(1280,800)/size,23,Color(0.4,0.9,0.85,0.4))
			_label("DRAG TO MOVE",Vector2(99,695),14,MUTED)
		if game.banner_time>0.0:
			var width = font.get_string_size(game.banner,HORIZONTAL_ALIGNMENT_LEFT,-1,23).x
			_panel(Rect2(620-width/2,123,width+40,48),Color(0.025,0.065,0.11,0.9))
			_label(game.banner,Vector2(640-width/2,155),23,GOLD)
		for enemy in game.enemies:
			if enemy.kind == "warden":
				_panel(Rect2(435,650,410,42),INK)
				_label("WARDEN",Vector2(452,677),14,GOLD)
				draw_rect(Rect2(545,665,280,8),Color("45343f"))
				draw_rect(Rect2(545,665,280*maxf(0,enemy.hp/enemy.max_hp),8),GOLD)
		if game.flash>0:
			draw_rect(Rect2(0,0,1280,800),Color(0.8,0.1,0.2,game.flash*0.12))
		return
	draw_rect(Rect2(0,108,1280,692),Color(0.01,0.025,0.055,0.78))
	if game.phase == "paused":
		_panel(Rect2(385,207,510,404))
		_label("FLIGHT PAUSED",Vector2(433,286),37)
		_label("Take a breath. Your run is safe.",Vector2(434,324),19,MUTED)
	elif game.phase == "upgrade":
		_panel(Rect2(150,220,980,383))
		_label("CONTACTS CLEARED",Vector2(190,275),17,TEAL)
		_label("Make the next wave yours.",Vector2(190,326),36)
		var descriptions = [["Faster fire","+25% fire rate","+5 shot damage"],["Stronger hull","+25 maximum integrity","Repair 55 integrity"],["Wider coverage","Three-projectile spread","+4 shot damage"]]
		for i in range(3):
			var x = 190+i*310
			_label(descriptions[i][0],Vector2(x,389),25,GOLD)
			_label(descriptions[i][1],Vector2(x,430),18,MUTED)
			_label(descriptions[i][2],Vector2(x,461),18,MUTED)
		_label("Every upgrade also repairs 15 integrity.  /  Choose with 1, 2, or 3.",Vector2(190,586),15,MUTED)
	elif game.phase in ["won","lost"]:
		_panel(Rect2(385,182,510,470))
		_label("TRANSMISSION RESTORED" if game.phase=="won" else "SIGNAL LOST",Vector2(425,238),17,TEAL if game.phase=="won" else Color("ff697c"))
		_label("CLEAR SKIES." if game.phase=="won" else "ONE MORE RUN?",Vector2(425,299),36)
		_label("The Warden is down. You made it out." if game.phase=="won" else "Keep moving and dash through danger.",Vector2(425,342),18,MUTED)
		_label("SCORE   %05d" % game.score,Vector2(425,400),30,GOLD)
		_label("%d contacts   /   %02d:%02d flight time" % [game.kills,int(game.run_time)/60,int(game.run_time)%60],Vector2(425,442),18,MUTED)

func _controls() -> void:
	_label("WASD / ARROWS   move       SPACE   dash       LMB   manual aim",Vector2(120,720),17,MUTED)
	_label("Touch: drag left side to move, tap DASH. Auto fire handles aiming.",Vector2(120,750),16,MUTED)
