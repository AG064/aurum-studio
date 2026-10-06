extends CanvasLayer
## Relay Yard HUD, "Sodium Enamel" instrument language.
## Every panel is a painted steel plate (ui_plate.gdshader): chamfered, bevel-lit, worn at raised
## edges, fastened at the corners. Readouts sit in recessed wells or behind dark glass, states are
## shown by indicator lamps with metal bezels, and quantities by graduated scales and needles,
## matching the lamps, gauges and deck markings on the ships and dock machinery.
## In play: a route plate (top left), one instrument cluster (bottom left: hull dial, heat scale,
## dash and EMP keys) and a diagnostic tag that follows the current objective in the world.
## Out of play: the DT-09 dispatch terminal. Public fields and update(delta) are unchanged.
const Art = preload("res://art.gd")
const FONT = preload("res://assets/fonts/BarlowCondensed-SemiBold.ttf")
const UI_PLATE = preload("res://ui_plate.gdshader")
const PLATE = Color(0.135, 0.142, 0.152)
const PLATE_LIGHT = Color(0.22, 0.23, 0.24)
const METAL = Color(0.5, 0.51, 0.52)
const WELL = Color(0.06, 0.065, 0.07)
const GLASS = Color(0.045, 0.055, 0.058)
const LEGEND = Color(0.88, 0.85, 0.77)
const LEGEND_SOFT = Color(0.64, 0.63, 0.59)
const NAMEPLATE = Color(0.8, 0.77, 0.69)
const INK_TEXT = Color(0.1, 0.1, 0.105)
const SIGNAL = Color(0.43, 0.63, 0.98)
const POWER = Color(0.93, 0.62, 0.28)
const HOSTILE = Color(0.9, 0.33, 0.23)
const FOCUS = Color(0.97, 0.94, 0.86)
const MARGIN = 16.0
const STOPS = ["GENERATOR", "COMM ARRAY", "REACTOR", "EXIT"]
var game
var root: Control
var hud: Control
var ink: Control
var objective: Label
var location: Label
var status: Label
var health: ProgressBar
var heat: ProgressBar
var dash_bar: ProgressBar
var emp_bar: ProgressBar
var pips: Array[ColorRect] = []
var abilities: Label
var emp_label: Label
var hull_label: Label
var hull_value: Label
var heat_label: Label
var heat_value: Label
var dash_key: Label
var dash_name: Label
var emp_key: Label
var emp_name: Label
var prompt: Label
var key_glyph: Label
var banner: Label
var callout_label: Label
var overlay: ColorRect
var card: PanelContainer
var terminal_lamps: Control
var terminal_model: Label
var kicker: Label
var title: Label
var description: Label
var route_strip: Control
var stat_rows: Array[HBoxContainer] = []
var start_button: Button
var resume_button: Button
var retry_button: Button
var settings: CheckButton
var volume: HSlider
var volume_value: Label
var help_group: Control
var menu_scroll: ScrollContainer
var menu_content: VBoxContainer
var message_time := 0.0
var last_phase := ""
var last_modal := ""
var banner_message := ""
var prompt_message := ""
var callout_time := 0.0
var callout_color := Color.WHITE
var tracked: FontVariation
var plates := {}
var caps := {}
var needle := 1.0
var plate_count := 0
var g := {}

func _ready() -> void:
	var font: FontFile = FONT
	font.oversampling = 2.0
	tracked = FontVariation.new()
	tracked.base_font = FONT
	tracked.spacing_glyph = 1
	g = {"vp": Vector2(1280, 800), "stretch": Vector2.ONE, "banner_a": 1.0, "banner_dy": 0.0, "dock": 0.0, "key": false,
		"compact": false, "callout_a": 1.0, "anchor": Vector2.ZERO, "anchored": false}
	root = _blank(self)
	var theme := Theme.new()
	theme.default_font = FONT
	theme.default_font_size = 18
	root.theme = theme
	hud = _blank(root)
	for key in ["route", "route_glass", "cluster", "well_hull", "well_heat", "well_dash", "well_emp", "banner", "tag", "callout"]:
		plates[key] = _plate(hud, _preset(key))
	ink = _blank(hud)
	ink.draw.connect(_paint)
	location = _label("DOCK 09  /  POWER BAY", 13, POWER, hud, true)
	objective = _label("", 20, LEGEND, hud)
	status = _label("", 16, LEGEND, hud)
	health = _meter()
	heat = _meter()
	dash_bar = _meter()
	emp_bar = _meter()
	for index in 3:
		var pip := ColorRect.new()
		pip.visible = false
		hud.add_child(pip)
		pips.append(pip)
	hull_label = _label("HULL", 13, LEGEND_SOFT, hud, true)
	hull_value = _label("100", 22, LEGEND, hud)
	heat_label = _label("HEAT", 13, LEGEND_SOFT, hud, true)
	heat_value = _label("0%", 16, LEGEND, hud)
	dash_name = _label("DASH", 13, LEGEND_SOFT, hud, true)
	dash_key = _label("SPACE", 13, INK_TEXT, hud, true)
	abilities = _label("READY", 14, SIGNAL, hud)
	emp_name = _label("EMP", 13, LEGEND_SOFT, hud, true)
	emp_key = _label("Q", 16, INK_TEXT, hud, true)
	emp_label = _label("READY", 14, SIGNAL, hud)
	for label in [hull_value, dash_name, dash_key, abilities, emp_name, emp_key, emp_label]:
		label.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	key_glyph = _label("E", 16, INK_TEXT, hud, true)
	key_glyph.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	prompt = _label("", 16, LEGEND, hud, true)
	banner = _label("", 18, LEGEND, hud, true)
	callout_label = _label("", 14, LEGEND, hud, true)
	callout_label.visible = false
	for label in [prompt, banner]:
		label.clip_text = false
		label.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
		label.max_lines_visible = 2
	_terminal()
	get_viewport().size_changed.connect(_layout)
	_layout.call_deferred()

# ---------------------------------------------------------------- construction helpers

func _blank(parent: Node) -> Control:
	var node := Control.new()
	node.mouse_filter = Control.MOUSE_FILTER_IGNORE
	parent.add_child(node)
	return node

func _label(text: String, font_size: int, color: Color, parent: Node, track := false) -> Label:
	var label := Label.new()
	label.text = text
	label.add_theme_font_size_override("font_size", font_size)
	label.add_theme_color_override("font_color", color)
	label.clip_text = true
	label.add_theme_constant_override("line_spacing", 0)
	if track: label.add_theme_font_override("font", tracked)
	label.mouse_filter = Control.MOUSE_FILTER_IGNORE
	parent.add_child(label)
	return label

func _meter() -> ProgressBar:
	var meter := ProgressBar.new()
	meter.visible = false
	hud.add_child(meter)
	return meter

## Plate presets: paint, chamfer, bevel, recess, wear, fasteners, glass.
func _preset(kind: String) -> Dictionary:
	match kind:
		"cluster", "route", "housing": return {"paint": PLATE, "chamfer": 9.0, "bevel": 2.5, "wear": 0.4, "fasteners": 1.0}
		"route_glass", "glass": return {"paint": GLASS, "chamfer": 4.0, "bevel": 2.0, "recess": 1.0, "glass": 1.0}
		"banner", "tag", "callout": return {"paint": PLATE, "chamfer": 5.0, "bevel": 2.0, "wear": 0.25}
		"nameplate": return {"paint": NAMEPLATE, "chamfer": 3.0, "bevel": 1.5, "wear": 0.5, "fasteners": 1.0, "metal": Color(0.36, 0.36, 0.35)}
		"cap_primary": return {"paint": NAMEPLATE, "chamfer": 3.0, "bevel": 2.5, "wear": 0.35}
		"cap": return {"paint": PLATE_LIGHT, "chamfer": 3.0, "bevel": 2.5, "wear": 0.3}
	return {"paint": WELL, "chamfer": 5.0, "bevel": 2.0, "recess": 1.0}

func _plate(parent: Node, preset: Dictionary) -> ColorRect:
	var rect := ColorRect.new()
	rect.mouse_filter = Control.MOUSE_FILTER_IGNORE
	var material := ShaderMaterial.new()
	material.shader = UI_PLATE
	for key in preset:
		material.set_shader_parameter(key, preset[key])
	plate_count += 1
	material.set_shader_parameter("seed", float(plate_count) * 7.31)
	rect.material = material
	rect.resized.connect(func(): material.set_shader_parameter("size", rect.size))
	parent.add_child(rect)
	return rect

func _set_plate(name: String, rect: Rect2, opacity := 1.0) -> void:
	var plate: ColorRect = plates[name]
	plate.position = rect.position
	plate.size = rect.size
	(plate.material as ShaderMaterial).set_shader_parameter("size", rect.size)
	(plate.material as ShaderMaterial).set_shader_parameter("opacity", opacity)

## Section with a plate behind padded content. Returns the content box.
func _section(parent: Node, preset: Dictionary, padding: Vector4) -> VBoxContainer:
	var holder := PanelContainer.new()
	holder.add_theme_stylebox_override("panel", StyleBoxEmpty.new())
	parent.add_child(holder)
	_plate(holder, preset)
	var margin := MarginContainer.new()
	margin.add_theme_constant_override("margin_left", int(padding.x))
	margin.add_theme_constant_override("margin_top", int(padding.y))
	margin.add_theme_constant_override("margin_right", int(padding.z))
	margin.add_theme_constant_override("margin_bottom", int(padding.w))
	holder.add_child(margin)
	var box := VBoxContainer.new()
	box.add_theme_constant_override("separation", 8)
	margin.add_child(box)
	return box

func _focus_box() -> StyleBoxFlat:
	var style := StyleBoxFlat.new()
	style.draw_center = false
	style.border_color = FOCUS
	style.set_border_width_all(2)
	style.set_corner_radius_all(2)
	style.set_expand_margin_all(3)
	return style

## Anti-aliased icon raster for physical controls.
func _icon(size: Vector2i, shade: Callable) -> ImageTexture:
	var image := Image.create_empty(size.x, size.y, false, Image.FORMAT_RGBA8)
	for y in size.y:
		for x in size.x:
			image.set_pixel(x, y, shade.call(Vector2(x + 0.5, y + 0.5)))
	return ImageTexture.create_from_image(image)

static func _cover(distance: float) -> float:
	return clampf(0.5 - distance, 0.0, 1.0)

static func _box_distance(p: Vector2, centre: Vector2, half: Vector2) -> float:
	var q := (p - centre).abs() - half
	return Vector2(maxf(q.x, 0.0), maxf(q.y, 0.0)).length() + minf(maxf(q.x, q.y), 0.0)

static func _segment_distance(p: Vector2, a: Vector2, b: Vector2) -> float:
	var t := clampf((p - a).dot(b - a) / maxf((b - a).length_squared(), 0.0001), 0.0, 1.0)
	return p.distance_to(a + (b - a) * t)

## Toggle switch: a slotted steel plate with a bat lever up (on) or down (off).
func _lever_icon(on: bool) -> ImageTexture:
	return _icon(Vector2i(34, 44), func(p: Vector2) -> Color:
		var plate := _cover(_box_distance(p, Vector2(17, 22), Vector2(14, 19)) - 2.0)
		var color := PLATE_LIGHT.lerp(PLATE, p.y / 44.0)
		var slot := _cover(_box_distance(p, Vector2(17, 22), Vector2(3, 10)))
		color = color.lerp(WELL, slot)
		var tip := Vector2(17, 7) if on else Vector2(17, 37)
		var lever := _cover(_segment_distance(p, Vector2(17, 22), tip) - 2.6)
		var knob := _cover(p.distance_to(tip) - 4.6)
		var metal := METAL.lerp(LEGEND, 0.35 if on else 0.0)
		color = color.lerp(metal, maxf(lever, knob))
		for screw in [Vector2(8, 6), Vector2(26, 38)]:
			color = color.lerp(METAL * 0.9, _cover(p.distance_to(screw) - 2.0))
		return Color(color, maxf(plate, maxf(lever, knob))))

## Fader cap for the audio slider.
func _fader_icon() -> ImageTexture:
	return _icon(Vector2i(18, 30), func(p: Vector2) -> Color:
		var body := _cover(_box_distance(p, Vector2(9, 15), Vector2(7, 13)) - 1.0)
		var color := NAMEPLATE.lerp(NAMEPLATE * 0.7, p.y / 30.0)
		color = color.lerp(INK_TEXT, _cover(absf(p.y - 15.0) - 0.8) * _cover(absf(p.x - 9.0) - 5.0))
		for y in [9.0, 21.0]: color = color.lerp(NAMEPLATE * 0.6, _cover(absf(p.y - y) - 0.5) * 0.6)
		return Color(color, body))

# ---------------------------------------------------------------- dispatch terminal

func _terminal() -> void:
	overlay = ColorRect.new()
	overlay.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	overlay.color = Color(0.01, 0.012, 0.018, 0.55)
	overlay.mouse_filter = Control.MOUSE_FILTER_IGNORE
	root.add_child(overlay)
	card = PanelContainer.new()
	card.add_theme_stylebox_override("panel", StyleBoxEmpty.new())
	root.add_child(card)
	_plate(card, _preset("housing"))
	var margin := MarginContainer.new()
	for side in ["left", "right"]: margin.add_theme_constant_override("margin_" + side, 18)
	margin.add_theme_constant_override("margin_top", 16)
	margin.add_theme_constant_override("margin_bottom", 14)
	card.add_child(margin)
	var frame := VBoxContainer.new()
	frame.add_theme_constant_override("separation", 12)
	margin.add_child(frame)
	var header := HBoxContainer.new()
	header.add_theme_constant_override("separation", 14)
	frame.add_child(header)
	var plate_box := _section(header, _preset("nameplate"), Vector4(14, 7, 14, 8))
	plate_box.add_theme_constant_override("separation", -2)
	var brand := _label("RELAY YARD", 26, INK_TEXT, plate_box)
	brand.clip_text = false
	terminal_model = _label("DT-09  DISPATCH TERMINAL", 13, Color(0.28, 0.28, 0.27), plate_box, true)
	terminal_model.clip_text = false
	var spacer := Control.new()
	spacer.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	header.add_child(spacer)
	terminal_lamps = _blank(header)
	terminal_lamps.custom_minimum_size = Vector2(156, 54)
	terminal_lamps.draw.connect(_paint_terminal_lamps)
	menu_scroll = ScrollContainer.new()
	menu_scroll.follow_focus = true
	menu_scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	menu_scroll.size_flags_vertical = Control.SIZE_EXPAND_FILL
	var bar := menu_scroll.get_v_scroll_bar()
	for item in [["scroll", WELL], ["grabber", METAL * 0.8], ["grabber_highlight", METAL], ["grabber_pressed", LEGEND]]:
		var style := StyleBoxFlat.new()
		style.bg_color = item[1]
		style.content_margin_left = 3
		style.content_margin_right = 3
		bar.add_theme_stylebox_override(item[0], style)
	frame.add_child(menu_scroll)
	menu_content = VBoxContainer.new()
	menu_content.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	menu_content.add_theme_constant_override("separation", 8)
	menu_scroll.add_child(menu_content)
	# Readout behind glass
	var readout := _section(menu_content, _preset("glass"), Vector4(16, 10, 16, 12))
	kicker = _label("WAYBILL 09  /  NIGHT SHIFT", 13, POWER, readout, true)
	kicker.clip_text = false
	title = _label("RELAY YARD", 24, LEGEND, readout, true)
	title.clip_text = false
	title.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	description = _label("", 16, LEGEND_SOFT, readout)
	description.clip_text = false
	description.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	route_strip = _blank(readout)
	route_strip.custom_minimum_size.y = 50
	route_strip.draw.connect(_paint_route_strip)
	route_strip.resized.connect(route_strip.queue_redraw)
	for index in 6:
		var row := HBoxContainer.new()
		readout.add_child(row)
		var key := _label("", 13, LEGEND_SOFT, row, true)
		key.clip_text = false
		key.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		key.vertical_alignment = VERTICAL_ALIGNMENT_CENTER
		var value := _label("", 18, LEGEND, row)
		value.clip_text = false
		stat_rows.append(row)
	# Physical push buttons
	start_button = _push_button("BEGIN SHIFT", func(): game.start_game())
	resume_button = _push_button("RESUME", func(): game.paused = false)
	retry_button = _push_button("RETRY SHIFT", func(): game.restart())
	# Settings panel: toggle lever and graduated fader
	var deck := _section(menu_content, _preset("well"), Vector4(14, 10, 14, 12))
	settings = CheckButton.new()
	settings.text = "REDUCED MOTION   OFF"
	settings.add_theme_font_override("font", tracked)
	settings.add_theme_font_size_override("font_size", 14)
	for state in ["font_color", "font_hover_color", "font_pressed_color", "font_hover_pressed_color", "font_focus_color"]:
		settings.add_theme_color_override(state, LEGEND)
	var row_style := StyleBoxEmpty.new()
	row_style.content_margin_top = 2
	row_style.content_margin_bottom = 2
	for state in ["normal", "hover", "pressed", "hover_pressed", "disabled"]:
		settings.add_theme_stylebox_override(state, row_style)
	settings.add_theme_stylebox_override("focus", _focus_box())
	for suffix in ["", "_disabled"]:
		settings.add_theme_icon_override("checked" + suffix, _lever_icon(true))
		settings.add_theme_icon_override("unchecked" + suffix, _lever_icon(false))
	settings.toggled.connect(func(value): game.reduced_motion = value; game.save_profile())
	deck.add_child(settings)
	var audio_row := HBoxContainer.new()
	deck.add_child(audio_row)
	var audio := _label("AUDIO", 14, LEGEND, audio_row, true)
	audio.clip_text = false
	audio.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	volume_value = _label("60%", 16, LEGEND, audio_row)
	volume_value.clip_text = false
	var fader := PanelContainer.new()
	fader.add_theme_stylebox_override("panel", StyleBoxEmpty.new())
	fader.custom_minimum_size.y = 42
	deck.add_child(fader)
	var scale_marks := _blank(fader)
	scale_marks.draw.connect(_paint_fader_scale.bind(scale_marks))
	scale_marks.resized.connect(scale_marks.queue_redraw)
	volume = HSlider.new()
	volume.min_value = 0
	volume.max_value = 1
	volume.step = 0.05
	volume.value = 0.6
	volume.size_flags_vertical = Control.SIZE_SHRINK_BEGIN
	var slot := StyleBoxFlat.new()
	slot.bg_color = WELL
	slot.border_color = METAL * 0.7
	slot.set_border_width_all(1)
	slot.content_margin_top = 3
	slot.content_margin_bottom = 3
	var travelled := StyleBoxFlat.new()
	travelled.bg_color = POWER * 0.7
	travelled.content_margin_top = 3
	travelled.content_margin_bottom = 3
	volume.add_theme_stylebox_override("slider", slot)
	volume.add_theme_stylebox_override("grabber_area", travelled)
	volume.add_theme_stylebox_override("grabber_area_highlight", travelled)
	volume.add_theme_stylebox_override("focus", _focus_box())
	var cap_icon := _fader_icon()
	volume.add_theme_icon_override("grabber", cap_icon)
	volume.add_theme_icon_override("grabber_highlight", cap_icon)
	volume.value_changed.connect(func(value): game.sound.set_volume(value); game.save_profile())
	fader.add_child(volume)
	# Engraved key legend
	var legend := _section(menu_content, _preset("well"), Vector4(14, 10, 14, 10))
	help_group = legend.get_parent().get_parent()
	var grid := GridContainer.new()
	grid.columns = 4
	grid.add_theme_constant_override("h_separation", 10)
	grid.add_theme_constant_override("v_separation", 6)
	legend.add_child(grid)
	var keycap := StyleBoxFlat.new()
	keycap.bg_color = PLATE_LIGHT
	keycap.border_color = METAL * 0.8
	keycap.set_border_width_all(1)
	keycap.border_width_bottom = 2
	keycap.set_corner_radius_all(2)
	keycap.content_margin_left = 6
	keycap.content_margin_right = 6
	for pair in [["WASD", "MOVE"], ["MOUSE", "AIM"], ["LMB", "FIRE"], ["SPACE", "DASH"], ["Q", "EMP"], ["HOLD E", "DOCK"], ["ESC", "PAUSE"]]:
		var key := _label(pair[0], 13, LEGEND, grid, true)
		key.clip_text = false
		key.add_theme_stylebox_override("normal", keycap)
		key.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
		key.size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
		var action := _label(pair[1], 13, LEGEND_SOFT, grid, true)
		action.clip_text = false
		action.custom_minimum_size.x = 56
	var credits := _label("FONT BARLOW (OFL)  /  MODELS RELAY YARD ASSET FORGE", 13, LEGEND_SOFT * 0.85, menu_content, true)
	credits.clip_text = false
	credits.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART

## Push button: a raised cap plate, an indicator lamp, and a transparent Button for input and focus.
func _push_button(text: String, callback: Callable) -> Button:
	var holder := PanelContainer.new()
	holder.add_theme_stylebox_override("panel", StyleBoxEmpty.new())
	holder.custom_minimum_size.y = 48
	menu_content.add_child(holder)
	var cap := _plate(holder, _preset("cap_primary"))
	var button := Button.new()
	button.text = text
	button.add_theme_font_override("font", tracked)
	button.add_theme_font_size_override("font_size", 17)
	var empty := StyleBoxEmpty.new()
	for state in ["normal", "hover", "pressed", "disabled", "hover_pressed"]:
		button.add_theme_stylebox_override(state, empty)
	button.add_theme_stylebox_override("focus", _focus_box())
	holder.add_child(button)
	var lamp_layer := _blank(holder)
	lamp_layer.draw.connect(_paint_button_lamp.bind(button, lamp_layer))
	caps[button] = {"cap": cap, "lamp": lamp_layer, "primary": true}
	for signal_name in ["mouse_entered", "mouse_exited", "focus_entered", "focus_exited", "button_down", "button_up"]:
		button.connect(signal_name, _refresh_button.bind(button))
	button.pressed.connect(func(): game.sound.play("ui"); callback.call())
	return button

func _button_role(button: Button, primary: bool) -> void:
	var entry: Dictionary = caps[button]
	entry.primary = primary
	var material := (entry.cap as ColorRect).material as ShaderMaterial
	material.set_shader_parameter("paint", NAMEPLATE if primary else PLATE_LIGHT)
	var text := INK_TEXT if primary else LEGEND
	for state in ["font_color", "font_hover_color", "font_pressed_color", "font_focus_color", "font_hover_pressed_color"]:
		button.add_theme_color_override(state, text)
	_refresh_button(button)

func _refresh_button(button: Button) -> void:
	var entry: Dictionary = caps[button]
	var material := (entry.cap as ColorRect).material as ShaderMaterial
	var down := button.get_draw_mode() in [BaseButton.DRAW_PRESSED, BaseButton.DRAW_HOVER_PRESSED]
	material.set_shader_parameter("pressed", 1.0 if down else 0.0)
	var base: Color = NAMEPLATE if entry.primary else PLATE_LIGHT
	material.set_shader_parameter("paint", base * (0.9 if down else (1.06 if button.is_hovered() else 1.0)))
	(entry.lamp as Control).queue_redraw()

# ---------------------------------------------------------------- drawing primitives

## Indicator lamp: metal bezel, lens, and a soft halo only when lit.
func _lamp(ci: CanvasItem, at: Vector2, radius: float, color: Color, lit: bool) -> void:
	if lit: ci.draw_circle(at, radius + 5.0, Color(color, 0.14), true, -1.0, true)
	ci.draw_circle(at, radius + 2.5, METAL * 0.55, true, -1.0, true)
	ci.draw_arc(at, radius + 2.0, PI * 1.0, PI * 1.5, 12, METAL * 1.25, 1.2, true)
	ci.draw_circle(at, radius, color if lit else color.darkened(0.78), true, -1.0, true)
	if lit: ci.draw_circle(at, radius * 0.55, color.lightened(0.35), true, -1.0, true)
	ci.draw_circle(at + Vector2(-radius * 0.35, -radius * 0.35), radius * 0.28, Color(1, 1, 1, 0.35 if lit else 0.12), true, -1.0, true)

## Raised key cap drawn with primitives (used inside gauge wells).
func _keycap(rect: Rect2, ready: bool) -> void:
	var face := NAMEPLATE if ready else PLATE_LIGHT
	ink.draw_rect(rect.grow(1.0), Color(0, 0, 0, 0.55))
	ink.draw_rect(rect, face)
	ink.draw_line(rect.position, Vector2(rect.end.x, rect.position.y), face.lightened(0.25), 1.5)
	ink.draw_line(rect.position, Vector2(rect.position.x, rect.end.y), face.lightened(0.18), 1.5)
	ink.draw_line(Vector2(rect.position.x, rect.end.y), rect.end, face.darkened(0.45), 2.0)
	ink.draw_line(Vector2(rect.end.x, rect.position.y), rect.end, face.darkened(0.35), 1.5)

func _arc(ci: CanvasItem, at: Vector2, radius: float, from: float, to: float, color: Color, width: float) -> void:
	if absf(to - from) < 0.001: return
	ci.draw_arc(at, radius, from, to, maxi(8, int(absf(to - from) * radius / 5.0)), color, width, true)

# ---------------------------------------------------------------- play painting

func _paint() -> void:
	if game == null or game.player == null or not g.has("cluster"): return
	var player = game.player
	var active: int = game.powered.count(true)
	_paint_route(active)
	_paint_hull(player)
	_paint_heat(player)
	_paint_key(g.well_dash, player.dash_cooldown, player.DASH_RECHARGE_SECONDS)
	_paint_key(g.well_emp, player.emp_cooldown, player.EMP_RECHARGE_SECONDS)
	if prompt.visible: _paint_tag()
	if banner.visible:
		var r: Rect2 = g.banner_rect
		_lamp(ink, r.position + Vector2(17, r.size.y * 0.5), 5.0, SIGNAL, true)
	if callout_label.visible:
		var c: Rect2 = g.callout_rect
		_lamp(ink, c.position + Vector2(14, c.size.y * 0.5), 4.5, callout_color, true)
	_paint_intent()
	if player.damage_flash > 0 and not game.reduced_motion:
		ink.draw_rect(Rect2(Vector2(3, 3), g.vp - Vector2(6, 6)), Color(HOSTILE, 0.35 * player.damage_flash), false, 5.0)

func _paint_route(active: int) -> void:
	var y: float = g.route_lamps.y
	var x0: float = g.route_lamps.x
	var gap := 26.0
	ink.draw_line(Vector2(x0, y), Vector2(x0 + gap * 3.0, y), Color(0, 0, 0, 0.6), 3.0)
	ink.draw_line(Vector2(x0, y + 1), Vector2(x0 + gap * 3.0, y + 1), METAL * 0.45, 1.0)
	for index in 4:
		var at := Vector2(x0 + gap * index, y)
		var done := index < active
		var current := index == active
		_lamp(ink, at, 5.5 if current else 4.5, SIGNAL if done else POWER, done or current)

## Hull dial: a 180 degree scale with a red band below 30, a needle and a digital readout.
func _paint_hull(player) -> void:
	var well: Rect2 = g.well_hull
	var centre := Vector2(well.get_center().x, well.position.y + 66.0)
	var radius := minf(48.0, well.size.x * 0.5 - 12.0)
	_arc(ink, centre, radius - 3.0, PI, PI * 1.3, Color(HOSTILE, 0.55), 5.0)
	for index in 21:
		var t := float(index) / 20.0
		var angle := PI + PI * t
		var dir := Vector2(cos(angle), sin(angle))
		var major := index % 5 == 0
		ink.draw_line(centre + dir * (radius - (9.0 if major else 5.0)), centre + dir * radius, LEGEND_SOFT if major else LEGEND_SOFT * 0.75, 1.5 if major else 1.0, true)
	for mark in [[0.0, "0"], [0.5, "50"], [1.0, "100"]]:
		var angle: float = PI + PI * float(mark[0])
		var at := centre + Vector2(cos(angle), sin(angle)) * (radius - 18.0)
		var w := FONT.get_string_size(mark[1], HORIZONTAL_ALIGNMENT_LEFT, -1, 13).x
		ink.draw_string(FONT, at + Vector2(-w * 0.5, 5.0), mark[1], HORIZONTAL_ALIGNMENT_LEFT, -1, 13, LEGEND_SOFT)
	var angle := PI + PI * needle
	var tip := centre + Vector2(cos(angle), sin(angle)) * (radius - 4.0)
	var critical: bool = player.health <= 30.0
	ink.draw_line(centre, tip, Color(0, 0, 0, 0.5), 4.0, true)
	ink.draw_line(centre, tip, HOSTILE if critical else LEGEND, 2.0, true)
	ink.draw_circle(centre, 5.0, METAL * 0.7, true, -1.0, true)
	ink.draw_circle(centre, 2.5, METAL * 1.2, true, -1.0, true)

## Heat scale: ten cells, a marked danger zone above 80, the release mark and a vent lamp.
func _paint_heat(player) -> void:
	var slot: Rect2 = g.heat_slot
	ink.draw_rect(slot, WELL.darkened(0.3))
	var cell := (slot.size.x - 2.0) / 10.0
	var hot: bool = player.heat >= 0.8 or player.overheated
	for index in 10:
		var r := Rect2(slot.position.x + 1.0 + index * cell + 1.0, slot.position.y + 2.0, cell - 2.0, slot.size.y - 4.0)
		var part := clampf(player.heat * 10.0 - index, 0.0, 1.0)
		ink.draw_rect(r, Color(0.13, 0.135, 0.14))
		if part > 0.0: ink.draw_rect(Rect2(r.position, Vector2(r.size.x * part, r.size.y)), HOSTILE if hot else POWER)
	ink.draw_rect(Rect2(slot.position.x + slot.size.x * 0.8, slot.position.y - 5.0, slot.size.x * 0.2, 3.0), Color(HOSTILE, 0.75))
	for index in 11:
		var x := slot.position.x + 1.0 + index * cell
		var major := index % 5 == 0
		ink.draw_line(Vector2(x, slot.end.y + 2.0), Vector2(x, slot.end.y + (7.0 if major else 4.0)), LEGEND_SOFT, 1.0)
	for mark in [[0.0, "0"], [0.5, "50"], [1.0, "100"]]:
		var x := slot.position.x + 1.0 + float(mark[0]) * cell * 10.0
		var w := FONT.get_string_size(mark[1], HORIZONTAL_ALIGNMENT_LEFT, -1, 13).x
		ink.draw_string(FONT, Vector2(clampf(x - w * 0.5, slot.position.x - 2.0, slot.end.x - w + 2.0), slot.end.y + 20.0), mark[1], HORIZONTAL_ALIGNMENT_LEFT, -1, 13, LEGEND_SOFT)
	if player.overheated:
		var rx: float = slot.position.x + 1.0 + player.HEAT_RELEASE_THRESHOLD * cell * 10.0
		ink.draw_line(Vector2(rx, slot.position.y - 3.0), Vector2(rx, slot.end.y + 3.0), LEGEND, 2.0)
	var lamp_at: Vector2 = g.vent_lamp
	_lamp(ink, lamp_at, 5.0, HOSTILE, player.overheated)
	ink.draw_string(tracked, lamp_at + Vector2(11.0, 5.0), "VENT", HORIZONTAL_ALIGNMENT_LEFT, -1, 13, LEGEND if player.overheated else LEGEND_SOFT)

func _paint_key(well: Rect2, cooldown: float, total: float) -> void:
	var ready := cooldown <= 0
	_lamp(ink, Vector2(well.get_center().x, well.position.y + 30.0), 5.0, SIGNAL, ready)
	_keycap(Rect2(well.position.x + 6.0, well.position.y + 41.0, well.size.x - 12.0, 26.0), ready)
	var strip := Rect2(well.position.x + 8.0, well.position.y + 72.0, well.size.x - 16.0, 4.0)
	ink.draw_rect(strip, Color(0.12, 0.125, 0.13))
	var charged := clampf(1.0 - cooldown / maxf(0.01, total), 0.0, 1.0)
	ink.draw_rect(Rect2(strip.position, Vector2(strip.size.x * charged, strip.size.y)), SIGNAL if ready else POWER)

## Diagnostic tag: a leader from the objective in the world to a small plate with the key cap.
func _paint_tag() -> void:
	var r: Rect2 = g.tag_rect
	if g.anchored:
		var anchor: Vector2 = g.anchor
		var corner := Vector2(r.position.x, r.end.y - 6.0) if anchor.x < r.position.x else Vector2(r.end.x, r.end.y - 6.0)
		ink.draw_line(anchor, corner, Color(0, 0, 0, 0.5), 3.0, true)
		ink.draw_line(anchor, corner, LEGEND_SOFT, 1.2, true)
		ink.draw_circle(anchor, 3.5, POWER, true, -1.0, true)
		_arc(ink, anchor, 7.0, 0, TAU, Color(POWER, 0.6), 1.2)
	if g.key:
		var cap: Rect2 = g.key_rect
		_keycap(cap, true)
		var bar := Rect2(r.position.x + 10.0, r.end.y - 7.0, r.size.x - 20.0, 3.0)
		ink.draw_rect(bar, Color(0.1, 0.105, 0.11))
		ink.draw_rect(Rect2(bar.position, Vector2(bar.size.x * float(g.dock), bar.size.y)), POWER)
		for index in 11:
			var x := bar.position.x + bar.size.x * index / 10.0
			ink.draw_line(Vector2(x, bar.position.y - 2.0), Vector2(x, bar.position.y), LEGEND_SOFT * 0.8, 1.0)
	else:
		_lamp(ink, r.position + Vector2(15.0, r.size.y * 0.5), 4.5, POWER, true)

## Off-screen threats only; on-screen intent is drawn in the world by each drone (collar, aim line).
func _paint_intent() -> void:
	var camera: Camera3D = game.camera
	if camera == null or game.paused or game.phase not in ["play", "extract"]: return
	var vp: Vector2 = g.vp
	if vp.x < 160.0 or vp.y < 160.0: return
	var inset := Rect2(Vector2(26, 26), vp - Vector2(52, 52))
	var centre := vp * 0.5
	for enemy in game.enemies:
		if not is_instance_valid(enemy): continue
		var boss: bool = enemy.kind == "warden"
		var charging: bool = enemy.charge > 0 and enemy.stunned <= 0
		if not (charging or boss): continue
		var at3: Vector3 = enemy.global_position
		var behind := camera.is_position_behind(at3)
		var stretch: Vector2 = g.stretch
		var point: Vector2 = camera.unproject_position(at3) * stretch
		if behind: point = centre + (centre - point) * 4.0
		if inset.has_point(point) and not behind: continue
		var direction := (point - centre).normalized()
		if direction.is_zero_approx(): continue
		var reach := INF
		if absf(direction.x) > 0.001: reach = minf(reach, ((inset.end.x if direction.x > 0 else inset.position.x) - centre.x) / direction.x)
		if absf(direction.y) > 0.001: reach = minf(reach, ((inset.end.y if direction.y > 0 else inset.position.y) - centre.y) / direction.y)
		var tip := centre + direction * reach
		var side := Vector2(-direction.y, direction.x)
		var base := tip - direction * 15.0
		var color := HOSTILE if charging else LEGEND_SOFT
		var shape := PackedVector2Array([tip, base + side * 9.0, base - side * 9.0])
		ink.draw_colored_polygon(shape, Color(0.05, 0.05, 0.055, 0.85))
		var outline := shape.duplicate()
		outline.append(shape[0])
		ink.draw_polyline(outline, color, 2.0, true)
		if charging: ink.draw_colored_polygon(PackedVector2Array([tip - direction * 4.0, base + side * 5.0 + direction * 2.0, base - side * 5.0 + direction * 2.0]), color)
		if boss: ink.draw_line(base - side * 11.0 - direction * 4.0, base + side * 11.0 - direction * 4.0, color, 2.0)

# ---------------------------------------------------------------- terminal painting

func _paint_terminal_lamps() -> void:
	if game == null: return
	var lost: bool = game.phase == "lost"
	var held: bool = game.paused
	var linked: bool = game.phase in ["play", "extract"]
	var items := [["POWER", SIGNAL, true], ["LINK", SIGNAL, linked or game.phase == "won"], ["ALERT", HOSTILE if lost else POWER, lost or held]]
	var w := terminal_lamps.size.x / 3.0
	for index in 3:
		var at := Vector2(w * (index + 0.5), 16.0)
		_lamp(terminal_lamps, at, 6.5, items[index][1], items[index][2])
		var label: String = items[index][0]
		var tw := tracked.get_string_size(label, HORIZONTAL_ALIGNMENT_LEFT, -1, 13).x
		terminal_lamps.draw_string(tracked, Vector2(at.x - tw * 0.5, 45.0), label, HORIZONTAL_ALIGNMENT_LEFT, -1, 13, LEGEND_SOFT)

func _paint_route_strip() -> void:
	if game == null: return
	var active: int = game.powered.count(true)
	if game.phase == "won": active = 4
	var width := route_strip.size.x
	var x0 := 10.0
	var gap := (width - 20.0) / 3.0
	var y := 10.0
	route_strip.draw_line(Vector2(x0, y), Vector2(x0 + gap * 3.0, y), Color(0, 0, 0, 0.6), 3.0)
	route_strip.draw_line(Vector2(x0, y + 1.0), Vector2(x0 + gap * 3.0, y + 1.0), METAL * 0.4, 1.0)
	for index in 4:
		var at := Vector2(x0 + gap * index, y)
		var done := index < active
		var current: bool = index == active and game.phase != "menu"
		_lamp(route_strip, at, 5.5, SIGNAL if done else POWER, done or current)
		var name: String = STOPS[index]
		var tw := tracked.get_string_size(name, HORIZONTAL_ALIGNMENT_LEFT, -1, 13).x
		var tx := clampf(at.x - tw * 0.5, 0.0, width - tw)
		route_strip.draw_string(tracked, Vector2(tx, y + 32.0), name, HORIZONTAL_ALIGNMENT_LEFT, -1, 13, LEGEND if done or current else LEGEND_SOFT)

func _paint_button_lamp(button: Button, layer: Control) -> void:
	if not button.visible: return
	var entry: Dictionary = caps[button]
	var lit := button.has_focus() or button.is_hovered()
	_lamp(layer, Vector2(20.0, layer.size.y * 0.5), 5.0, SIGNAL, lit)

func _paint_fader_scale(control: Control) -> void:
	var x0 := 9.0
	var x1 := control.size.x - 9.0
	for index in 11:
		var x := lerpf(x0, x1, index / 10.0)
		var major := index % 5 == 0
		control.draw_line(Vector2(x, 22.0), Vector2(x, 28.0 if major else 25.0), LEGEND_SOFT, 1.0)
		if major:
			var text := str(index)
			var w := FONT.get_string_size(text, HORIZONTAL_ALIGNMENT_LEFT, -1, 13).x
			control.draw_string(FONT, Vector2(x - w * 0.5, 41.0), text, HORIZONTAL_ALIGNMENT_LEFT, -1, 13, LEGEND_SOFT)

# ---------------------------------------------------------------- layout

func _place(label: Label, x: float, y: float, width: float, align := -1) -> void:
	label.position = Vector2(x, y)
	label.custom_minimum_size.x = maxf(1.0, width)
	label.size.x = maxf(1.0, width)
	if align >= 0: label.horizontal_alignment = align

func _layout() -> void:
	if root == null or menu_scroll == null: return
	var stretch := get_viewport().get_stretch_transform().get_scale()
	scale = Vector2(1.0 / maxf(0.01, stretch.x), 1.0 / maxf(0.01, stretch.y))
	g.stretch = stretch
	_layout_pixels(get_viewport().get_visible_rect().size * stretch)

func _layout_pixels(vp: Vector2) -> void:
	g.vp = vp
	for node in [root, hud, ink]: node.size = vp
	g.compact = vp.x < 760.0
	var narrow := vp.x < 440.0
	# Route plate, top left
	var route := Rect2(MARGIN, MARGIN, minf(320.0, vp.x - 2.0 * MARGIN), 84.0)
	g.route = route
	_set_plate("route", route)
	_place(location, route.position.x + 16.0, route.position.y + 10.0, route.size.x - 32.0)
	_place(objective, route.position.x + 16.0, route.position.y + 25.0, route.size.x - 32.0)
	g.route_lamps = Vector2(route.position.x + 24.0, route.end.y - 17.0)
	var glass := Rect2(route.position.x + 126.0, route.end.y - 30.0, route.size.x - 142.0, 22.0)
	_set_plate("route_glass", glass)
	_place(status, glass.position.x + 8.0, glass.position.y + 1.0, glass.size.x - 16.0, HORIZONTAL_ALIGNMENT_RIGHT)
	# Instrument cluster, bottom left
	var hull_w := 120.0 if narrow else 132.0
	var heat_w := 98.0 if narrow else 116.0
	var key_w := 52.0 if narrow else 58.0
	var height := 120.0
	var width := 12.0 + hull_w + 6.0 + heat_w + 6.0 + key_w + 4.0 + key_w + 12.0
	var cluster := Rect2(MARGIN, vp.y - MARGIN - height, width, height)
	g.cluster = cluster
	_set_plate("cluster", cluster)
	var x := cluster.position.x + 12.0
	var y := cluster.position.y + 12.0
	g.well_hull = Rect2(x, y, hull_w, 96.0)
	x += hull_w + 6.0
	g.well_heat = Rect2(x, y, heat_w, 96.0)
	x += heat_w + 6.0
	g.well_dash = Rect2(x, y, key_w, 96.0)
	x += key_w + 4.0
	g.well_emp = Rect2(x, y, key_w, 96.0)
	for key in ["hull", "heat", "dash", "emp"]: _set_plate("well_" + key, g["well_" + key])
	var hull: Rect2 = g.well_hull
	_place(hull_label, hull.position.x + 8.0, hull.position.y + 4.0, 60.0)
	_place(hull_value, hull.position.x, hull.position.y + 68.0, hull.size.x)
	var heat_well: Rect2 = g.well_heat
	_place(heat_label, heat_well.position.x + 8.0, heat_well.position.y + 4.0, 50.0)
	_place(heat_value, heat_well.position.x + 8.0, heat_well.position.y + 2.0, heat_well.size.x - 16.0, HORIZONTAL_ALIGNMENT_RIGHT)
	g.heat_slot = Rect2(heat_well.position.x + 9.0, heat_well.position.y + 34.0, heat_well.size.x - 18.0, 14.0)
	g.vent_lamp = Vector2(heat_well.position.x + 15.0, heat_well.end.y - 12.0)
	for item in [[g.well_dash, dash_name, dash_key, abilities], [g.well_emp, emp_name, emp_key, emp_label]]:
		var well: Rect2 = item[0]
		_place(item[1], well.position.x, well.position.y + 4.0, well.size.x)
		_place(item[2], well.position.x + 6.0, well.position.y + 41.0 + (26.0 - FONT.get_height(int(item[2].get_theme_font_size("font_size")))) * 0.5, well.size.x - 12.0)
		_place(item[3], well.position.x, well.position.y + 77.0, well.size.x)
	# Dispatch terminal
	terminal_model.text = "DT-09  DISPATCH" if narrow else "DT-09  DISPATCH TERMINAL"
	terminal_lamps.custom_minimum_size.x = 128.0 if narrow else 156.0
	var wide := vp.x >= 900.0
	var card_w := minf(500.0, vp.x - 2.0 * MARGIN) if wide else vp.x - 2.0 * 12.0
	var card_h := vp.y - 2.0 * (24.0 if wide else 12.0)
	card.size = Vector2(card_w, card_h)
	card.position = Vector2(vp.x - card_w - (40.0 if wide else 12.0), 24.0 if wide else 12.0)
	menu_scroll.custom_minimum_size.y = 0
	_place_overlays()
	ink.queue_redraw()

func _text_block(label: Label, text: String, font_size: int, max_width: float, font: Font = null) -> Vector2:
	var face: Font = FONT if font == null else font
	label.text = text
	var width := face.get_string_size(text, HORIZONTAL_ALIGNMENT_LEFT, -1, font_size).x + 4.0
	var w := minf(width, max_width)
	_place(label, 0, 0, w)
	var lines := clampi(label.get_line_count(), 1, 2)
	var height := ceilf(face.get_height(font_size) * lines) + 2.0
	label.size.y = height
	return Vector2(w, height)

## Objective point for the diagnostic tag, from the baseline yard state.
func _objective_world() -> Variant:
	var active: int = game.powered.count(true)
	var world = game.world
	if game.phase == "extract" or active >= 3: return world.EXIT
	if game.carried >= 0: return world.STATION_POSITIONS[active] + Vector3(0, 1.4, 0)
	return world.CAP_POSITIONS[active]

func _place_overlays() -> void:
	if not g.has("cluster"): return
	var vp: Vector2 = g.vp
	var cluster: Rect2 = g.cluster
	var route: Rect2 = g.route
	# Diagnostic tag: anchored beside the objective when it is on screen, else above the cluster.
	var lead := 48.0 if g.key else 30.0
	var text := _text_block(prompt, prompt.text, 16, minf(360.0, vp.x - 2.0 * MARGIN - lead - 16.0), tracked)
	var size := Vector2(text.x + lead + 16.0, maxf(38.0, text.y + 18.0))
	g.anchored = false
	var at := Vector2(clampf((vp.x - size.x) * 0.5, MARGIN, vp.x - MARGIN - size.x), cluster.position.y - 12.0 - size.y)
	if cluster.end.x + 16.0 + size.x < vp.x - MARGIN: at = Vector2(cluster.end.x + 16.0, vp.y - MARGIN - size.y)
	if game != null and game.camera != null and game.world != null and prompt.visible:
		var target = _objective_world()
		if target != null and not game.camera.is_position_behind(target):
			var point: Vector2 = game.camera.unproject_position(target) * g.stretch
			var safe := Rect2(Vector2(MARGIN, route.end.y + 8.0), Vector2(vp.x - 2.0 * MARGIN, cluster.position.y - route.end.y - 16.0))
			if safe.size.x > 40.0 and safe.size.y > 40.0 and safe.has_point(point):
				var spot := point + Vector2(34.0, -size.y - 34.0)
				if spot.x + size.x > vp.x - MARGIN: spot.x = point.x - 34.0 - size.x
				spot.x = clampf(spot.x, MARGIN, vp.x - MARGIN - size.x)
				spot.y = clampf(spot.y, safe.position.y, safe.end.y - size.y)
				at = spot
				g.anchor = point
				g.anchored = true
	g.tag_rect = Rect2(at, size)
	_set_plate("tag", g.tag_rect)
	g.key_rect = Rect2(at + Vector2(10.0, (size.y - 24.0) * 0.5 - 2.0), Vector2(26.0, 24.0))
	prompt.position = at + Vector2(lead, (size.y - text.y) * 0.5 - 2.0)
	_place(key_glyph, g.key_rect.position.x, g.key_rect.position.y + (24.0 - FONT.get_height(16)) * 0.5, 26.0)
	# Banner: a teleprinter strip at the top, below the route plate when space is short.
	var max_banner := minf(560.0, vp.x - 2.0 * MARGIN) - 44.0
	var line := _text_block(banner, banner.text, 18, max_banner, tracked)
	var strip := Vector2(line.x + 44.0, line.y + 16.0)
	var bx := (vp.x - strip.x) * 0.5
	var by: float = MARGIN + float(g.banner_dy)
	if bx < route.end.x + 12.0: by = route.end.y + 10.0 + float(g.banner_dy)
	if bx < route.end.x + 12.0 and g.compact: bx = MARGIN
	g.banner_rect = Rect2(Vector2(bx, by), strip)
	_set_plate("banner", g.banner_rect, float(g.banner_a))
	banner.position = g.banner_rect.position + Vector2(32.0, 8.0)
	# Callouts (optional; used by the gameplay milestone) sit above the ability keys.
	var call_w := tracked.get_string_size(callout_label.text, HORIZONTAL_ALIGNMENT_LEFT, -1, 14).x + 40.0
	g.callout_rect = Rect2(Vector2(g.well_emp.end.x + 12.0 - call_w, cluster.position.y - 34.0), Vector2(call_w, 28.0))
	_set_plate("callout", g.callout_rect, float(g.callout_a))
	_place(callout_label, g.callout_rect.position.x + 26.0, g.callout_rect.position.y + 5.0, call_w - 30.0)

# ---------------------------------------------------------------- game contract

func announce(text: String) -> void:
	banner_message = text.to_upper()
	banner.text = banner_message
	message_time = 3.2

## Optional instrument feedback. The baseline yard.gd does not call it.
func callout(text: String, color: Color = SIGNAL) -> void:
	callout_label.text = text.to_upper()
	callout_color = color
	callout_time = 1.4

func _clock(seconds: float) -> String:
	return "%02d:%02d" % [floori(seconds / 60.0), int(seconds) % 60]

func _dock_fraction() -> float:
	if game.has_method("dock_state"):
		var state: Dictionary = game.dock_state()
		return float(state.progress) if state.near else 0.0
	# Baseline yard.gd timings: 0.55 s to secure a core, 1.6 s to connect it.
	var needed := 0.55 if game.carried < 0 else 1.6
	return clampf(game.charge_progress / needed, 0.0, 1.0)

func update(delta: float) -> void:
	if game == null or game.player == null: return
	if not g.has("cluster"): _layout()
	var player = game.player
	var active: int = game.powered.count(true)
	var live: bool = game.phase in ["play", "extract"] and not game.paused
	var modal: bool = game.phase in ["menu", "won", "lost"] or game.paused
	message_time = maxf(0, message_time - delta)
	hud.visible = live
	objective.text = ("RESTORE %s" % game.world.NAMES[active]).to_upper() if active < 3 else "HOLD THE EXTRACTION PAD"
	status.text = "%d / 3     %s" % [active, _clock(game.elapsed)]
	location.text = "DOCK 09  /  %s" % ("EXTRACTION" if active == 3 else ["POWER BAY", "COMM BAY", "REACTOR BAY"][active])
	for index in 3: pips[index].color = SIGNAL if index < active else METAL
	health.value = player.health
	heat.value = player.heat * 100
	var hull := clampf(player.health / 100.0, 0.0, 1.0)
	needle = hull if game.reduced_motion else lerpf(needle, hull, 1.0 - exp(-delta * 10.0))
	var critical: bool = player.health <= 30.0
	var hot: bool = player.heat >= 0.8
	hull_value.text = str(ceili(player.health))
	hull_value.add_theme_color_override("font_color", HOSTILE if critical else LEGEND)
	hull_label.text = "HULL LOW" if critical else "HULL"
	hull_label.add_theme_color_override("font_color", HOSTILE if critical else LEGEND_SOFT)
	heat_label.text = "OVERHEAT" if player.overheated else ("HOT" if hot else "HEAT")
	heat_label.add_theme_color_override("font_color", HOSTILE if (player.overheated or hot) else LEGEND_SOFT)
	heat_value.text = "%d%%" % roundi(player.heat * 100.0)
	heat_value.add_theme_color_override("font_color", HOSTILE if (player.overheated or hot) else LEGEND)
	var dash: float = player.dash_cooldown
	var emp: float = player.emp_cooldown
	dash_bar.value = clampf(100.0 * (1.0 - dash / player.DASH_RECHARGE_SECONDS), 0, 100)
	emp_bar.value = clampf(100.0 * (1.0 - emp / player.EMP_RECHARGE_SECONDS), 0, 100)
	abilities.text = "READY" if dash <= 0 else "%.1f s" % (ceilf(dash * 10.0) / 10.0)
	emp_label.text = "READY" if emp <= 0 else "%.1f s" % (ceilf(emp * 10.0) / 10.0)
	abilities.add_theme_color_override("font_color", SIGNAL if dash <= 0 else POWER)
	emp_label.add_theme_color_override("font_color", SIGNAL if emp <= 0 else POWER)
	dash_key.add_theme_color_override("font_color", INK_TEXT if dash <= 0 else LEGEND_SOFT)
	emp_key.add_theme_color_override("font_color", INK_TEXT if emp <= 0 else LEGEND_SOFT)
	# Banner, tag and callout
	var shown := 3.2 - message_time
	banner.visible = message_time > 0 and live
	plates.banner.visible = banner.visible
	g.banner_a = 1.0 if game.reduced_motion else minf(clampf(message_time / 0.4, 0, 1), clampf(shown / 0.12, 0, 1))
	g.banner_dy = 0.0 if game.reduced_motion else -8.0 * clampf(1.0 - shown / 0.2, 0, 1)
	banner.modulate.a = g.banner_a
	prompt_message = String(game.context_prompt()).to_upper()
	g.key = prompt_message.begins_with("HOLD E")
	var display := prompt_message
	if g.key: display = display.trim_prefix("HOLD E").strip_edges().trim_prefix("/").strip_edges()
	prompt.text = display
	prompt.visible = not prompt_message.is_empty() and live
	plates.tag.visible = prompt.visible
	key_glyph.visible = prompt.visible and g.key
	g.dock = _dock_fraction() if g.key else 0.0
	callout_time = maxf(0, callout_time - delta)
	callout_label.visible = callout_time > 0 and live
	plates.callout.visible = callout_label.visible
	g.callout_a = 1.0 if game.reduced_motion else clampf(callout_time / 0.3, 0, 1)
	callout_label.modulate.a = g.callout_a
	_place_overlays()
	# Dispatch terminal
	overlay.visible = modal
	card.visible = modal
	var rows: Array = []
	if game.phase == "menu":
		kicker.text = "WAYBILL 09  /  NIGHT SHIFT"
		title.text = "READY FOR DISPATCH"
		description.text = "Carry three power cores to their bays, then hold the extraction pad until the courier is clear."
		if game.best_time > 0: rows = [["BEST SHIFT", _clock(game.best_time)]]
	elif game.phase == "won":
		kicker.text = "SHIFT REPORT  /  DELIVERED"
		title.text = "SHIFT COMPLETE"
		description.text = "All three systems online. Courier clear of the yard."
		rows = [["RANK", str(game.rank())], ["SHIFT TIME", _clock(game.elapsed)], ["DRONES DISABLED", str(game.kills)], ["HULL DAMAGE", "%.0f" % player.damage_taken]]
		if game.best_time > 0: rows.append(["BEST SHIFT", _clock(game.best_time)])
	elif game.phase == "lost":
		kicker.text = "SHIFT REPORT  /  LOST"
		title.text = "COURIER LOST"
		description.text = "Break firing lines with cover. Dash through a volley, or use EMP to interrupt a charge."
		rows = [["SYSTEMS RESTORED", "%d / 3" % active], ["SHIFT TIME", _clock(game.elapsed)], ["DRONES DISABLED", str(game.kills)]]
	else:
		kicker.text = "DOCK 09  /  ON HOLD"
		title.text = "SHIFT PAUSED"
		description.text = "The dock will wait. Adjust motion and audio below."
		rows = [["SYSTEMS RESTORED", "%d / 3" % active], ["SHIFT TIME", _clock(game.elapsed)], ["DRONES DISABLED", str(game.kills)], ["HULL", "%d / 100" % ceili(player.health)]]
	title.add_theme_color_override("font_color", HOSTILE if game.phase == "lost" else LEGEND)
	for index in stat_rows.size():
		var row := stat_rows[index]
		row.visible = index < rows.size()
		if row.visible:
			(row.get_child(0) as Label).text = rows[index][0]
			(row.get_child(1) as Label).text = rows[index][1]
			(row.get_child(1) as Label).add_theme_color_override("font_color", SIGNAL if game.phase == "won" and index == 0 else LEGEND)
	help_group.visible = game.phase == "menu" or game.paused
	settings.set_pressed_no_signal(game.reduced_motion)
	settings.text = "REDUCED MOTION   " + ("ON" if game.reduced_motion else "OFF")
	volume.set_value_no_signal(game.sound.volume)
	volume_value.text = "%d%%" % roundi(volume.value * 100.0)
	for pair in [[start_button, game.phase == "menu"], [resume_button, game.paused and game.phase in ["play", "extract"]], [retry_button, game.phase in ["won", "lost"] or game.paused]]:
		var button: Button = pair[0]
		button.visible = pair[1]
		button.get_parent().visible = pair[1]
	var modal_key := "%s/%s" % [game.phase, game.paused]
	if last_modal != modal_key:
		last_modal = modal_key
		last_phase = game.phase
		_button_role(start_button, true)
		_button_role(resume_button, true)
		_button_role(retry_button, game.phase in ["won", "lost"])
		menu_scroll.scroll_vertical = 0
		route_strip.queue_redraw()
		terminal_lamps.queue_redraw()
		if start_button.visible: start_button.grab_focus()
		elif resume_button.visible: resume_button.grab_focus()
		elif retry_button.visible: retry_button.grab_focus()
		_layout.call_deferred()
	ink.queue_redraw()
