extends Control

const Instrument = preload("res://hud_style.gd")
const ModuleCard = preload("res://hud_module_card.gd")
const INK = Color("0b1420")
const TEAL = Color("69f4de")
const CREAM = Color("e5e8e5")
const MUTED = Color("a2b1bd")
const GOLD = Color("ffbf73")
var game: Node3D
var buttons: Array[Dictionary] = []
var font: Font = ThemeDB.fallback_font
var draw_origin = Vector2.ZERO
var draw_scale = 1.0
var hull_echo = 1.0
var hull_last = 1.0
var hull_echo_delay = 0.0

func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	resized.connect(refresh)

func _hull_ratio() -> float:
	return clampf(game.health/maxf(game.max_health,1.0),0,1)

func _dash_charge() -> float:
	return 1.0-clampf(game.dash_cooldown/maxf(game.dash_period,0.001),0,1)

func _process(delta: float) -> void:
	if not game or game.phase!="playing": return
	var ratio = _hull_ratio()
	if game.reduced_motion or ratio>=hull_echo:
		hull_echo = ratio
		hull_echo_delay = 0.0
	elif ratio<hull_last:
		hull_echo_delay = 0.35
	elif hull_echo_delay>0:
		hull_echo_delay = maxf(0,hull_echo_delay-delta)
	else:
		hull_echo = move_toward(hull_echo,ratio,delta*0.6)
	hull_last = ratio

func _portrait() -> bool:
	return size.y > size.x

func _compact_choices() -> bool:
	return game.phase in ["upgrade","hangar"] and not _portrait() and get_window().size.y<520

func _short_portrait_choices() -> bool:
	return game.phase in ["upgrade","hangar"] and _portrait() and get_window().size.y<620

func _design_size() -> Vector2:
	if _portrait(): return Vector2(480,620 if _short_portrait_choices() else 800)
	return Vector2(960,500) if _compact_choices() else Vector2(1280,800)

func _label(text: String, pos: Vector2, size_px: int, color = CREAM) -> void:
	# Rasterize text at its final canvas size, rather than enlarging a small atlas.
	draw_set_transform(Vector2.ZERO,0,Vector2.ONE)
	var label_font: Font = Instrument.DISPLAY_FONT if size_px>=24 else font
	draw_string(label_font,draw_origin+pos*draw_scale,text,HORIZONTAL_ALIGNMENT_LEFT,-1,maxi(1,roundi(size_px*draw_scale)),color)
	draw_set_transform(draw_origin,0,Vector2.ONE*draw_scale)

func _panel(rect: Rect2, color = INK, border = Color("35434c")) -> void:
	Instrument.plate(self,rect,color,border)

func _paragraph(text: String, pos: Vector2, width: float, size_px: int, color = MUTED) -> void:
	var line = ""
	var row = 0
	var face: Font = Instrument.DISPLAY_FONT if size_px>=24 else font
	for word in text.split(" "):
		var next = word if line.is_empty() else line+" "+word
		if not line.is_empty() and face.get_string_size(next,HORIZONTAL_ALIGNMENT_LEFT,-1,size_px).x>width:
			_label(line,pos+Vector2(0,row*size_px*1.45),size_px,color)
			row += 1
			line = word
		else: line = next
	if not line.is_empty(): _label(line,pos+Vector2(0,row*size_px*1.45),size_px,color)

func _button(text: String, rect: Rect2, callback: Callable, primary = false) -> Button:
	var button = Button.new()
	button.text = text
	button.focus_mode = Control.FOCUS_NONE if game.phase == "playing" else Control.FOCUS_ALL
	button.mouse_default_cursor_shape = Control.CURSOR_POINTING_HAND
	for state in ["normal","hover","pressed","focus"]:
		var style = StyleBoxFlat.new()
		style.bg_color = Color("c5d9ce") if primary else Color("17232f")
		if state == "hover": style.bg_color = style.bg_color.lightened(0.12)
		style.border_color = TEAL if state=="focus" else Color("425362")
		style.set_border_width_all(2 if state=="focus" else 1)
		style.set_corner_radius_all(4)
		style.shadow_color = Color(0,0,0,0.3)
		style.shadow_size = 3
		style.shadow_offset = Vector2(0,2)
		button.add_theme_stylebox_override(state,style)
	for state in ["font_color","font_focus_color","font_hover_color","font_pressed_color"]:
		button.add_theme_color_override(state,INK if primary else CREAM)
	button.pressed.connect(callback)
	add_child(button)
	buttons.append({"node":button,"rect":rect,"labels":[]})
	return button

func _module_button(rect: Rect2, choice: Dictionary, index: int) -> Button:
	var button = ModuleCard.new()
	button.accent = Color(choice.color)
	button.symbol = choice.symbol
	button.key_hint = str(index+1)
	button.design_size = rect.size
	button.compact_row = _portrait()
	button.dense = _compact_choices()
	button.reduced_motion = game.reduced_motion
	button.focus_mode = Control.FOCUS_ALL
	button.mouse_default_cursor_shape = Control.CURSOR_POINTING_HAND
	button.tooltip_text = choice.title + ". " + choice.body + " " + choice.detail
	button.pressed.connect(func():
		if game.phase=="hangar": game.choose_frame(index)
		else: game.choose_upgrade(index))
	add_child(button)
	buttons.append({"node":button,"rect":rect,"labels":[]})
	return button

func _card_label(button: Button, text: String, rect: Rect2, font_size: int, color: Color) -> void:
	var label = Label.new()
	label.text = text
	label.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	label.mouse_filter = Control.MOUSE_FILTER_IGNORE
	label.add_theme_color_override("font_color",color)
	if font_size>=24: label.add_theme_font_override("font",Instrument.DISPLAY_FONT)
	button.add_child(label)
	buttons[-1].labels.append({"node":label,"rect":rect,"font_size":font_size})

func _layout() -> void:
	var design = _design_size()
	var factor = minf(size.x/design.x,size.y/design.y)
	var origin = (size-design*factor)*0.5
	for entry in buttons:
		entry.node.position = origin+entry.rect.position*factor
		entry.node.size = entry.rect.size*factor
		entry.node.add_theme_font_size_override("font_size",maxi(11,int(19*factor)))
		for label in entry.labels:
			label.node.position = label.rect.position*factor
			label.node.size = label.rect.size*factor
			label.node.add_theme_font_size_override("font_size",maxi(11,int(label.font_size*factor)))
	queue_redraw()

func refresh() -> void:
	for entry in buttons:
		entry.node.disabled = true
		entry.node.queue_free()
	buttons.clear()
	var narrow = _portrait()
	var compact = _compact_choices()
	var short_portrait = _short_portrait_choices()
	var center = _design_size().x*0.5
	if game.phase == "menu":
		_button("Launch flight  /  Enter",Rect2(56 if narrow else 96,520,368,58),game.open_hangar,true)
		_button("Sound off" if game.muted else "Sound on",Rect2(56 if narrow else 96,596,175,42),game.toggle_mute)
		_button("Flight records",Rect2(249 if narrow else 289,596,175,42),game.open_records)
	elif game.phase == "playing":
		_button("Pause",Rect2(386,32,64,32) if narrow else Rect2(1178,36,64,32),game.pause_game)
		if game.touch_mode: _button("DASH",Rect2(320 if narrow else 1070,635,135,100),game.dash,true)
	elif game.phase == "paused":
		_button("Resume  /  Esc",Rect2(center-170,402,340,54),game.pause_game,true)
		_button("Sound off" if game.muted else "Sound on",Rect2(center-170,474,340,44),game.toggle_mute)
		_button("Restart flight",Rect2(center-170,536,340,44),game.start_run)
	elif game.phase in ["upgrade","hangar"]:
		var offers: Array = game.choices if game.phase=="upgrade" else game.Campaign.FRAMES
		for i in range(offers.size()):
			var choice: Dictionary = offers[i].duplicate()
			if game.phase=="hangar":
				choice.kind = choice.role
				choice.title = choice.name
			var accent = Color(choice.color)
			var rect = Rect2(28,120+i*154,424,150) if short_portrait else (Rect2(28,212+i*176,424,162) if narrow else (Rect2(28+i*310,144,284,286) if compact else Rect2(128+i*348,286,324,322)))
			var button = _module_button(rect,choice,i)
			var content_width = rect.size.x-40
			_card_label(button,choice.kind,Rect2(100,12,270,20) if narrow else Rect2(20,20,content_width-34,26),13 if narrow or compact else 14,accent)
			_card_label(button,choice.title,Rect2(100,32 if short_portrait else 36,304,32) if narrow else Rect2(20,124 if compact else 152,content_width,44),24 if narrow or compact else 28,CREAM)
			_card_label(button,choice.body,Rect2(100,64 if short_portrait else 76,304,40) if narrow else Rect2(20,174 if compact else 206,content_width,56),17 if narrow else (18 if compact else 20),MUTED)
			_card_label(button,choice.detail,Rect2(100,120 if short_portrait else 128,304,22) if narrow else Rect2(20,238 if compact else 278,content_width,36),14 if narrow else (15 if compact else 17),accent)
			if not game.reduced_motion and not game.test_mode and not game.auto_pilot:
				button.modulate.a = 0.0
				create_tween().tween_property(button,"modulate:a",1.0,0.16).set_delay(i*0.04)
		if game.phase=="hangar": _button("Back",Rect2(356,24 if short_portrait else 94,80,32) if narrow else (Rect2(28,442,85,36) if compact else Rect2(128,692,94,36)),game.return_menu)
	elif game.phase=="records":
		_button("Main menu",Rect2(center-150,718,300,42),game.return_menu,true)
		var ids = game.Campaign.ENEMIES.keys()
		if narrow:
			_button("Previous",Rect2(32,621,130,36),func(): game.archive_index = posmod(game.archive_index-1,ids.size()); refresh())
			_button("Next",Rect2(318,621,130,36),func(): game.archive_index = (game.archive_index+1)%ids.size(); refresh())
		else:
			for i in range(ids.size()):
				var id: String = ids[i]
				_button(game.Campaign.ENEMIES[id].name if id in game.profile.discoveries else "Unidentified",Rect2(565+(i%2)*260,232+int(i/2)*46,242,36),func(): game.archive_index=i; refresh(),i==game.archive_index)
	elif game.phase in ["won","lost"]:
		_button("Fly again  /  Enter",Rect2(center-170,474,340,54),game.start_run,true)
		_button("Main menu",Rect2(center-170,548,340,44),game.return_menu)
	_layout()
	if not buttons.is_empty() and game.phase!="playing":
		var index = 0
		if game.phase=="hangar":
			for i in range(game.Campaign.FRAMES.size()):
				if game.Campaign.FRAMES[i].id==game.selected_frame: index=i
		buttons[index].node.grab_focus()

func _flight_instruments(narrow: bool, design: Vector2) -> void:
	var left = Vector2(16,20) if narrow else Vector2(24,24)
	var width = 266.0 if narrow else 328.0
	var hull_color = Color("ff8978") if _hull_ratio()<=0.3 else TEAL
	_panel(Rect2(left,Vector2(width,176)),Color("101e28"),hull_color.darkened(0.55))
	draw_rect(Rect2(left+Vector2(10,0),Vector2(54,2)),hull_color)
	_label("HULL INTEGRITY",left+Vector2(18,28),13,MUTED)
	_label("%03d" % maxi(0,ceili(game.health)),left+Vector2(16,79),43)
	_label("/ %d" % int(game.max_health),left+Vector2(106,77),16,MUTED)
	_label("CRITICAL" if _hull_ratio()<=0.3 else "STABLE",left+Vector2(width-87,76),12,hull_color)
	Instrument.segmented(self,Rect2(left+Vector2(18,96),Vector2(width-36,8)),_hull_ratio(),hull_color,16,hull_echo)
	draw_line(left+Vector2(18,119),left+Vector2(width-18,119),Color("31434e"),1)
	var weapon_color = [TEAL,GOLD,Color("69d7ff")][game.weapon]
	Instrument.glyph(self,game.weapon,left+Vector2(34,145),0.38,weapon_color)
	_label("ARMAMENT",left+Vector2(58,140),10,MUTED)
	_label(game.WEAPONS[game.weapon].to_upper(),left+Vector2(58,160),18,weapon_color)
	var charge = _dash_charge()
	Instrument.dial(self,left+Vector2(width-38,146),17,charge,TEAL if charge>=1 else MUTED)
	_label("DASH",left+Vector2(width-113,140),10,MUTED)
	_label("READY" if charge>=1 else "%.1fs" % game.dash_cooldown,left+Vector2(width-113,160),14,TEAL if charge>=1 else CREAM)
	var right = Vector2(298,20) if narrow else Vector2(design.x-270,24)
	var right_width = 166.0 if narrow else 246.0
	_panel(Rect2(right,Vector2(right_width,194)),Color("14212b"),Color("6f6753"))
	_label("ACT %02d" % [game.act+1] if narrow else game.Campaign.ACTS[game.act].short,right+Vector2(16,28),12,MUTED)
	_label("%02d" % game.wave,right+Vector2(14,83),43)
	_label("/ 12",right+Vector2(77,81),16,MUTED)
	_label(game.mission.label(game),right+Vector2(16,108),12,CREAM)
	Instrument.segmented(self,Rect2(right+Vector2(16,120),Vector2(right_width-32,5)),game.mission.progress(game),Color(game.Campaign.ACTS[game.act].color),12)
	_label("%05d" % game.score,right+Vector2(16,158),23,GOLD)
	_label("PTS",right+Vector2(100,156),11,MUTED)
	var detail = "THREATS %02d / %02d" % [game.wave_kills,game.wave_quota]
	if game.mission.kind=="salvage": detail = "LOCKDOWN %02d:%02d" % [ceili(game.mission.remaining)/60,ceili(game.mission.remaining)%60]
	_label(detail,right+Vector2(16,180),11,MUTED)

func _draw() -> void:
	if not game: return
	var narrow = _portrait()
	var compact = _compact_choices()
	var short_portrait = _short_portrait_choices()
	var design = _design_size()
	var center = design.x*0.5
	var factor = minf(size.x/design.x,size.y/design.y)
	var origin = (size-design*factor)*0.5
	draw_origin = origin
	draw_scale = factor
	draw_set_transform(origin,0,Vector2.ONE*factor)
	if game.phase == "menu":
		if narrow:
			draw_origin = origin+Vector2(-40,0)*factor
			draw_set_transform(draw_origin,0,Vector2.ONE*factor)
		_panel(Rect2(64,135,432 if narrow else 495,540),Color(0.03,0.055,0.085,0.94),Color("344350"))
		_label("ORBITAL SALVAGE DIVISION",Vector2(96,182),15,GOLD)
		_label("ORBIT",Vector2(94,282),104)
		_label("BREAK",Vector2(94,369),104)
		draw_line(Vector2(96,390),Vector2(515,390),Color("394650"),1)
		_label("Break the blockade.",Vector2(96,433),27)
		_label("Three stations. Twelve encounters. One way out.",Vector2(96,473),17,MUTED)
		_label("BEST  %05d   /   %d flights" % [game.best,game.profile.flights],Vector2(96,665),16,GOLD)
		_label("Drag to move. Tap DASH to evade." if narrow else "WASD move   /   Space dash   /   Automatic fire",Vector2(96,726),17,MUTED)
		return
	if game.phase == "playing":
		_flight_instruments(narrow,design)
		if game.banner_time>0:
			var width = font.get_string_size(game.banner,HORIZONTAL_ALIGNMENT_LEFT,-1,19).x
			_label(game.banner,Vector2(center-width*0.5,244 if narrow else 92),19,GOLD)
		if game.transmission_time>0:
			_paragraph(game.transmission,Vector2(24,281) if narrow else Vector2(390,132),432 if narrow else 500,14,MUTED)
		for enemy in game.enemies:
			if enemy.kind in game.Campaign.BOSSES:
				var boss_top = 570.0 if narrow else 700.0
				_panel(Rect2(center-200,boss_top,400,56),Color("231f27"),Color("694742"))
				_label(game.Campaign.ENEMIES[enemy.kind].name.to_upper()+" / " + ["CONTAINMENT","PURSUIT","OVERLOAD"][enemy.stage-1],Vector2(center-180,boss_top+24),13,GOLD)
				Instrument.segmented(self,Rect2(center-180,boss_top+38,360,5),clampf(enemy.hp/enemy.max_hp,0,1),Color("f88362"),24)
		if game.touch_mode:
			var touch = game.touch_origin if game.touch_id>=0 else Vector2(size.x*.15,size.y*.8)
			draw_arc((touch-origin)/factor,55,0,TAU,48,Color(0.5,0.9,0.9,0.4),2,true)
		if game.flash>0:
			for i in range(6):
				var alpha = game.flash*0.11*(1.0-float(i)/6)
				draw_rect(Rect2(i*8,i*8,design.x-i*16,800-i*16),Color(0.95,0.2,0.14,alpha),false,8)
		return
	draw_rect(Rect2(-origin/factor,size/factor),Color(0.02,0.035,0.055,0.82))
	if game.phase in ["upgrade","hangar"]:
		var heading = "FLIGHT FRAME" if game.phase=="hangar" else "ENCOUNTER %02d COMPLETE" % game.wave
		if game.phase=="upgrade" and game.wave in [4,8]: heading = "BOSS SALVAGE / ACT %02d CLEAR" % (game.act+1)
		_label(heading,Vector2(28,55 if short_portrait else 136) if narrow else (Vector2(28,47) if compact else Vector2(128,191)),16,GOLD)
		_label("Choose your flight frame." if game.phase=="hangar" else "Choose your next upgrade.",Vector2(28,97 if short_portrait else 179) if narrow else (Vector2(28,96) if compact else Vector2(128,246)),27 if narrow else 38)
		_label("Choose one to launch." if game.phase=="hangar" else ("Choose one to continue." if narrow else "Pick one. The next encounter starts when you choose."),Vector2(28,610 if short_portrait else 756) if narrow else (Vector2(142 if game.phase=="hangar" else 28,471) if compact else Vector2(128,654)),18,MUTED)
		if not narrow and not compact: _label("1 / 2 / 3 or click a frame" if game.phase=="hangar" else "1 / 2 / 3 or click a module",Vector2(905,654),17,MUTED)
	elif game.phase == "paused":
		_panel(Rect2(center-210,210,420,402))
		_label("FLIGHT PAUSED",Vector2(center-170,276),31)
		_label("WASD / arrows  Move",Vector2(center-170,324),18,MUTED)
		_label("Space  Dash       M  Sound",Vector2(center-170,356),18,MUTED)
		if not narrow:
			_panel(Rect2(908,250,310,420))
			_label("FITTED MODULES",Vector2(931,282),17,GOLD)
			_label(game.Campaign.frame(game.selected_frame).name+" frame",Vector2(931,313),19)
			var line = 0
			for id in game.module_counts:
				_label(game.Rewards.name_for(id)+(" x%d" % game.module_counts[id] if game.module_counts[id]>1 else ""),Vector2(931,348+line*25),15,MUTED)
				line += 1
			if line==0: _label("No modules fitted yet.",Vector2(931,348),15,MUTED)
	elif game.phase=="records":
		_records(narrow)
	elif game.phase in ["won","lost"]:
		_panel(Rect2(center-210,193,420,433))
		_label("BLOCKADE BROKEN" if game.phase=="won" else "SIGNAL LOST",Vector2(center-170,251),16,TEAL if game.phase=="won" else Color("fa665b"))
		_label("CLEAR SKIES." if game.phase=="won" else "ONE MORE FLIGHT?",Vector2(center-170,310),32)
		_label("%05d" % game.score,Vector2(center-170,382),42,GOLD)
		_label("%d contacts / %02d:%02d" % [game.kills,int(game.run_time)/60,int(game.run_time)%60],Vector2(center-170,425),19,MUTED)
		_label(game.failure_reason if game.phase=="lost" else "%d bosses / %d caches recovered" % [game.bosses_defeated,game.salvage_recovered],Vector2(center-170,453),15,MUTED)

func _records(narrow: bool) -> void:
	var center = _design_size().x*0.5
	_panel(Rect2(20 if narrow else 128,90,440 if narrow else 1024,590))
	_label("FLIGHT RECORDS",Vector2(40 if narrow else 160,140),35)
	_label("%d flights / %d clears / best %05d" % [game.profile.flights,game.profile.wins,game.best],Vector2(40 if narrow else 160,185),16,GOLD)
	var row = 0
	for entry in game.profile.recent:
		if row>=(2 if narrow else 5): break
		_label("%s / %02d / %05d" % ["CLEAR" if entry.won else "LOST",entry.wave,entry.score],Vector2(40 if narrow else 160,232+row*32),16,MUTED)
		row += 1
	if row==0: _paragraph("Your next flight will start the ledger.",Vector2(40 if narrow else 160,232),330,17)
	if narrow:
		_label("%d commendations" % game.profile.medals.size(),Vector2(40,307),14,GOLD)
	else:
		_label("COMMENDATIONS",Vector2(160,432),14,GOLD)
		var medal_row = 0
		for medal in game.profile.medals:
			_label({"blockade":"Blockade broken","intact_core":"Core keeper","full_manifest":"Full manifest","dash_pilot":"Evasive pilot"}[medal],Vector2(160,467+medal_row*30),18,CREAM)
			medal_row += 1
		if medal_row==0: _paragraph("Clear the route, protect the cores and recover the full manifest.",Vector2(160,467),320,17)
	var ids = game.Campaign.ENEMIES.keys()
	var id: String = ids[clampi(game.archive_index,0,ids.size()-1)]
	var found = id in game.profile.discoveries
	var origin = Vector2(40,343) if narrow else Vector2(565,517)
	_label("CONTACT ARCHIVE",origin,14,GOLD)
	_label(game.Campaign.ENEMIES[id].name if found else "Unidentified contact",origin+Vector2(0,38),28)
	_paragraph(game.Campaign.ENEMIES[id].note if found else "Identify this craft in flight to add its tactical record.",origin+Vector2(0,74),390 if narrow else 500,17,MUTED)
