extends Node3D

const CYAN = Color("64eada")
const GOLD = Color("ffbf73")
const RED = Color("fa665b")
const ARMOR = Color("adb9ba")
var game: Node3D
var visual_rng = RandomNumberGenerator.new()
var motes: Array[Dictionary] = []
var rings: Array[Dictionary] = []
var particle_mesh: MultiMeshInstance3D
var engine_glows: Array[MeshInstance3D] = []
var weapon_nodes: Array[MeshInstance3D] = []
var trail_clock = 0.0
var time = 0.0
var glow_shader: Shader
var backdrop: MeshInstance3D
var shadow_shader: Shader
var pilot_visual: Node3D
var pilot_heading = 0.0
var deck_surface: MeshInstance3D
var sector_groups: Array[Node3D] = []
var core_light: OmniLight3D

func setup(owner_game: Node3D) -> void:
	game = owner_game
	visual_rng.seed = 1953
	glow_shader = Shader.new()
	glow_shader.code = "shader_type spatial; render_mode unshaded, cull_disabled, blend_add, depth_draw_never, shadows_disabled; uniform vec4 tint : source_color = vec4(1.0); void fragment(){float r=length(UV-vec2(0.5))*2.0; float a=pow(max(0.0,1.0-r),3.0); ALBEDO=tint.rgb; ALPHA=a*tint.a;}"
	shadow_shader = Shader.new()
	shadow_shader.code = "shader_type spatial; render_mode unshaded, cull_disabled, depth_draw_never, shadows_disabled; void fragment(){float r=length((UV-vec2(0.5))*2.0); ALBEDO=vec3(0.015,0.022,0.029); ALPHA=pow(max(0.0,1.0-r),1.6)*0.65;}"
	_build_station()
	_build_sectors()
	_build_particles()

func material(color: Color, emissive = false) -> StandardMaterial3D:
	var m = StandardMaterial3D.new()
	m.albedo_color = color
	m.metallic = 0.38
	m.roughness = 0.48
	if emissive:
		m.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	return m

func mesh(parent: Node3D, shape: Mesh, color: Color, pos: Vector3, emissive = false) -> MeshInstance3D:
	var node = MeshInstance3D.new()
	node.mesh = shape
	node.material_override = material(color,emissive)
	parent.add_child(node)
	node.position = pos
	return node

func box(parent: Node3D, size: Vector3, color: Color, pos: Vector3, emissive = false) -> MeshInstance3D:
	var shape = BoxMesh.new()
	shape.size = size
	return mesh(parent,shape,color,pos,emissive)

func _triangle(surface: SurfaceTool, a: Vector3, b: Vector3, c: Vector3, normal: Vector3, color: Color, transform: Transform3D) -> void:
	for point in [a,b,c]:
		surface.set_normal((transform.basis*normal).normalized())
		surface.set_color(color)
		surface.add_vertex(transform*point)

func _armor(surface: SurfaceTool, outline: PackedVector2Array, height: float, color: Color, transform: Transform3D) -> void:
	# Vertical walls and a separate chamfer preserve thickness under angled light.
	var center = Vector2.ZERO
	for point in outline: center += point
	center /= outline.size()
	var shoulder = height*0.58
	var indices = Geometry2D.triangulate_polygon(outline)
	for i in range(0,indices.size(),3):
		var a = center+(outline[indices[i]]-center)*0.86
		var b = center+(outline[indices[i+1]]-center)*0.86
		var c = center+(outline[indices[i+2]]-center)*0.86
		_triangle(surface,Vector3(a.x,height,a.y),Vector3(b.x,height,b.y),Vector3(c.x,height,c.y),Vector3.UP,color.lightened(0.08),transform)
	for i in range(outline.size()):
		var a = outline[i]
		var b = outline[(i+1)%outline.size()]
		var inner_a = center+(a-center)*0.86
		var inner_b = center+(b-center)*0.86
		var low_a = Vector3(a.x,0,a.y)
		var low_b = Vector3(b.x,0,b.y)
		var mid_a = Vector3(a.x,shoulder,a.y)
		var mid_b = Vector3(b.x,shoulder,b.y)
		var top_a = Vector3(inner_a.x,height,inner_a.y)
		var top_b = Vector3(inner_b.x,height,inner_b.y)
		var outward = Vector3(b.y-a.y,0,a.x-b.x).normalized()
		var radial = Vector3((a.x+b.x)*0.5-center.x,0,(a.y+b.y)*0.5-center.y)
		if outward.dot(radial)<0: outward = -outward
		_triangle(surface,low_a,mid_b,mid_a,outward,color.darkened(0.44),transform)
		_triangle(surface,low_a,low_b,mid_b,outward,color.darkened(0.44),transform)
		var bevel = (mid_b-mid_a).cross(top_a-mid_a).normalized()
		if bevel.dot(outward)<0: bevel = -bevel
		_triangle(surface,mid_a,top_b,top_a,bevel,color.darkened(0.06),transform)
		_triangle(surface,mid_a,mid_b,top_b,bevel,color.darkened(0.06),transform)

func _armor_mesh(parent: Node3D, surface: SurfaceTool, pos: Vector3) -> MeshInstance3D:
	var node = mesh(parent,surface.commit(),Color.WHITE,pos)
	node.material_override.vertex_color_use_as_albedo = true
	node.material_override.vertex_color_is_srgb = true
	node.material_override.cull_mode = BaseMaterial3D.CULL_DISABLED
	return node

func plate(parent: Node3D, outline: PackedVector2Array, height: float, color: Color, pos: Vector3) -> MeshInstance3D:
	var surface = SurfaceTool.new()
	surface.begin(Mesh.PRIMITIVE_TRIANGLES)
	_armor(surface,outline,height,color,Transform3D.IDENTITY)
	return _armor_mesh(parent,surface,pos)

func _contact_shadow(parent: Node3D, radius: float) -> void:
	var shape = PlaneMesh.new()
	shape.size = Vector2(radius*2.2,radius*2.8)
	var node = MeshInstance3D.new()
	node.mesh = shape
	var m = ShaderMaterial.new()
	m.shader = shadow_shader
	node.material_override = m
	node.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
	parent.add_child(node)
	node.position.y = -0.075

func glow(parent: Node3D, pos: Vector3, color: Color, radius: float) -> MeshInstance3D:
	var shape = PlaneMesh.new()
	shape.size = Vector2.ONE*radius*2.0
	var node = MeshInstance3D.new()
	node.mesh = shape
	var m = ShaderMaterial.new()
	m.shader = glow_shader
	m.set_shader_parameter("tint",color)
	node.material_override = m
	node.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
	parent.add_child(node)
	node.position = pos
	return node

func _build_station() -> void:
	var background_path = "res://assets/orbital-nebula.png"
	if ResourceLoader.exists(background_path):
		var quad = QuadMesh.new()
		quad.size = Vector2(60,40)
		backdrop = mesh(self,quad,Color("96acbd"),game.camera.position-game.camera.basis.z*85.0,true)
		backdrop.basis = game.camera.basis
		backdrop.material_override.albedo_texture = load(background_path)
		backdrop.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
		_fit_backdrop()
		get_viewport().size_changed.connect(_fit_backdrop)
	var base = CylinderMesh.new()
	base.top_radius = 16.0
	base.bottom_radius = 16.6
	base.height = 2.2
	base.radial_segments = 96
	mesh(self,base,Color("1c2833"),Vector3(0,-1.24,0))
	var deck = PlaneMesh.new()
	deck.size = Vector2(32,32)
	var floor_node = mesh(self,deck,Color("c3cad0"),Vector3(0,-0.13,0))
	deck_surface = floor_node
	if ResourceLoader.exists("res://assets/station-deck.png"):
		var shader = Shader.new()
		shader.code = """shader_type spatial;
uniform sampler2D deck_texture : source_color, filter_linear_mipmap;
uniform vec3 sector_tint = vec3(1.0);
void fragment(){
	if(length(UV-vec2(0.5))>0.495){discard;}
	vec3 deck=texture(deck_texture,UV).rgb;
	vec3 weights=vec3(0.299,0.587,0.114);
	vec2 texel=vec2(0.0008);
	float left=dot(texture(deck_texture,UV-vec2(texel.x,0.0)).rgb,weights);
	float right=dot(texture(deck_texture,UV+vec2(texel.x,0.0)).rgb,weights);
	float up=dot(texture(deck_texture,UV-vec2(0.0,texel.y)).rgb,weights);
	float down=dot(texture(deck_texture,UV+vec2(0.0,texel.y)).rgb,weights);
	NORMAL_MAP=normalize(vec3((left-right)*2.4,(up-down)*2.4,1.0))*0.5+0.5;
	NORMAL_MAP_DEPTH=0.55;
	ALBEDO=deck*vec3(0.48,0.57,0.65)*sector_tint;
	METALLIC=0.45;
	ROUGHNESS=mix(0.84,0.46,dot(deck,weights));
}"""
		var m = ShaderMaterial.new()
		m.shader = shader
		m.set_shader_parameter("deck_texture",load("res://assets/station-deck.png"))
		floor_node.material_override = m
	game._ring(self,15.5,0.028,Color("557578"),-0.05)
	game._ring(self,16.05,0.07,CYAN*0.55,-0.09)
	var structural = SurfaceTool.new()
	structural.begin(Mesh.PRIMITIVE_TRIANGLES)
	for i in range(24):
		var angle = TAU*i/24.0
		var basis = Basis(Vector3.UP,-angle+PI/2)
		var radial = Vector3(cos(angle),0,sin(angle))
		var color = Color("51636d") if i%3==0 else Color("344b5b")
		var rim = PackedVector2Array([Vector2(-1.67,-0.42),Vector2(1.67,-0.42),Vector2(1.5,0.54),Vector2(-1.5,0.54)])
		_armor(structural,rim,0.28,color,Transform3D(basis,radial*15.8+Vector3.UP*0.01))
		var rib = PackedVector2Array([Vector2(-0.22,-0.34),Vector2(0.22,-0.34),Vector2(0.28,0.43),Vector2(-0.28,0.43)])
		_armor(structural,rib,1.72,Color("30414d"),Transform3D(basis,radial*16.1+Vector3.DOWN*1.76))
		if i%2==0:
			var shoulder = PackedVector2Array([Vector2(-0.62,-0.4),Vector2(0.62,-0.4),Vector2(0.83,0.42),Vector2(-0.83,0.42)])
			_armor(structural,shoulder,0.12,Color("3a4752"),Transform3D(basis,radial*14.75+Vector3.UP*0.02))
	_armor_mesh(self,structural,Vector3.ZERO)
	var lettering = preload("res://assets/fonts/BarlowCondensed-SemiBold.ttf")
	for marker in [["01",Vector3(-8.2,0.01,-8.8)],["02",Vector3(8.2,0.01,-8.8)],["03",Vector3(8.2,0.01,8.8)],["04",Vector3(-8.2,0.01,8.8)]]:
		var label = Label3D.new()
		label.text = marker[0]
		label.font = lettering
		label.font_size = 100
		label.pixel_size = 0.015
		label.outline_size = 0
		label.modulate = Color(0.64,0.73,0.77,0.36)
		label.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
		label.position = marker[1]
		label.rotation_degrees.x = -90
		add_child(label)
	for i in range(12):
		var angle = TAU*i/12.0
		var mount = Node3D.new()
		add_child(mount)
		mount.position = Vector3(cos(angle)*16.0,-0.25,sin(angle)*16.0)
		mount.rotation.y = -angle+PI/2
		plate(mount,PackedVector2Array([Vector2(-0.9,-0.8),Vector2(0.9,-0.8),Vector2(1.2,0.8),Vector2(-1.2,0.8)]),0.65,Color("35414c"),Vector3.ZERO)
		box(mount,Vector3(0.8,0.08,0.14),GOLD if i%3==0 else CYAN,Vector3(0,0.69,-0.38),true)
		glow(mount,Vector3(0,0.7,-0.38),Color(0.4,0.8,0.85,0.7),1.15)
		if i%3==0:
			box(mount,Vector3(0.24,1.2,0.24),Color("57636d"),Vector3(0,0.55,0.65))
			box(mount,Vector3(0.3,0.12,0.3),GOLD,Vector3(0,1.2,0.65),true)
	var socket = CylinderMesh.new()
	socket.top_radius = 1.85
	socket.bottom_radius = 2.15
	socket.height = 0.26
	socket.radial_segments = 48
	mesh(self,socket,Color("131d28"),Vector3(0,-0.075,0))
	var frame = game._ring(self,1.92,0.2,Color("6e7f88"),0.09)
	frame.material_override = material(Color("697b87"))
	var lens = CylinderMesh.new()
	lens.top_radius = 1.54
	lens.bottom_radius = 1.54
	lens.height = 0.05
	lens.radial_segments = 48
	var glass = mesh(self,lens,Color("173345"),Vector3(0,0.085,0))
	glass.material_override.metallic = 0.7
	glass.material_override.roughness = 0.18
	game.reactor = game._ring(self,1.48,0.035,GOLD*0.8,0.12)
	glow(self,Vector3(0,0.13,0),Color(1,0.52,0.2,0.65),2.0)
	core_light = OmniLight3D.new()
	core_light.light_color = Color("ffb56d")
	core_light.light_energy = 0.75
	core_light.omni_range = 5.5
	core_light.position = Vector3(0,1.2,0)
	add_child(core_light)

func _build_sectors() -> void:
	for index in range(3):
		var group = Node3D.new()
		group.name = ["Dock","Relay","Foundry"][index]
		add_child(group)
		sector_groups.append(group)
	var relay = sector_groups[1]
	for side in [-1,1]:
		var pylon = Node3D.new()
		relay.add_child(pylon)
		pylon.position = Vector3(side*13.5,0,-11.5)
		plate(pylon,PackedVector2Array([Vector2(-0.6,-0.6),Vector2(0.6,-0.6),Vector2(0.6,0.6),Vector2(-0.6,0.6)]),0.55,Color("344b60"),Vector3.ZERO)
		box(pylon,Vector3(0.35,2.7,0.35),Color("5d7184"),Vector3(0,1.75,0))
		var dish = game._ring(pylon,1.3,0.12,Color("6b91aa"),3.05)
		dish.material_override = material(Color("6b91aa"))
		box(pylon,Vector3(0.18,0.6,0.18),CYAN,Vector3(0,3.0,0),true)
		glow(pylon,Vector3(0,2.8,0),Color(0.3,0.7,1,0.6),1.7)
	for i in range(6):
		var angle = PI+PI*i/5.0
		var chimney = Node3D.new()
		sector_groups[2].add_child(chimney)
		chimney.position = Vector3(cos(angle)*16.6,0,sin(angle)*16.6)
		var pipe = CylinderMesh.new()
		pipe.top_radius = 0.34
		pipe.bottom_radius = 0.5
		pipe.height = 1.6
		pipe.radial_segments = 12
		mesh(chimney,pipe,Color("67574d"),Vector3(0,0.85,0))
		game._ring(chimney,0.44,0.12,GOLD,1.66)
		glow(chimney,Vector3(0,1.72,0),Color(1,0.34,0.08,0.6),1.3)
	set_sector(0)

func set_sector(index: int) -> void:
	for i in range(sector_groups.size()): sector_groups[i].visible = i==index
	if deck_surface.material_override is ShaderMaterial:
		deck_surface.material_override.set_shader_parameter("sector_tint",[Vector3.ONE,Vector3(0.72,0.93,1.13),Vector3(1.16,0.84,0.67)][clampi(index,0,2)])
	if backdrop: backdrop.material_override.albedo_color = [Color("96acbd"),Color("78a4c6"),Color("bd9d85")][clampi(index,0,2)]
	if core_light: core_light.light_color = [GOLD,Color("7cccff"),Color("ff8047")][clampi(index,0,2)]
	reactor_feedback(1.0)

func reactor_feedback(ratio: float) -> void:
	if game.reactor: game.reactor.material_override.albedo_color = GOLD.lerp(RED,1.0-clampf(ratio,0,1))

func salvage_cache(parent: Node3D, pos: Vector3) -> Node3D:
	var node = Node3D.new()
	parent.add_child(node)
	node.position = pos
	plate(node,PackedVector2Array([Vector2(-0.38,-0.3),Vector2(0.38,-0.3),Vector2(0.38,0.3),Vector2(-0.38,0.3)]),0.26,Color("65737d"),Vector3.ZERO)
	box(node,Vector3(0.58,0.035,0.08),GOLD,Vector3(0,0.29,0),true)
	glow(node,Vector3(0,0.08,0),Color(1,0.65,0.25,0.7),1.2)
	return node

func configure_frame(id: String) -> void:
	if not is_instance_valid(pilot_visual): return
	var previous = pilot_visual.get_node_or_null("FrameFit")
	if previous:
		pilot_visual.remove_child(previous)
		previous.queue_free()
	var fit = Node3D.new()
	fit.name = "FrameFit"
	pilot_visual.add_child(fit)
	if id=="bastion":
		for side in [-1,1]: plate(fit,PackedVector2Array([Vector2(-0.23,-0.55),Vector2(0.23,-0.55),Vector2(0.28,0.6),Vector2(-0.28,0.6)]),0.23,Color("9e8b73"),Vector3(side*0.86,0.64,0.18))
	elif id=="relay":
		for side in [-1,1]:
			box(fit,Vector3(0.12,0.18,0.62),Color("739bb5"),Vector3(side*0.55,0.76,0.05))
			box(fit,Vector3(0.06,0.12,0.25),CYAN,Vector3(side*0.55,0.94,0.03),true)

func ship(parent: Node3D, kind: String) -> Node3D:
	var root = Node3D.new()
	parent.add_child(root)
	_contact_shadow(parent,1.9 if kind=="warden" else (1.1 if kind=="pilot" else 0.85))
	if kind == "pilot":
		plate(root,PackedVector2Array([Vector2(0,-1.45),Vector2(0.44,-0.3),Vector2(0.4,0.9),Vector2(-0.4,0.9),Vector2(-0.44,-0.3)]),0.42,ARMOR,Vector3(0,0.42,0))
		plate(root,PackedVector2Array([Vector2(0,-1.47),Vector2(0.12,-0.97),Vector2(-0.12,-0.97)]),0.045,GOLD.darkened(0.25),Vector3(0,0.86,0))
		for side in [-1,1]:
			plate(root,PackedVector2Array([Vector2(side*0.25,-0.4),Vector2(side*1.28,0.4),Vector2(side*1.04,0.85),Vector2(side*0.3,0.4)]),0.22,Color("405361"),Vector3(0,0.43,0))
			plate(root,PackedVector2Array([Vector2(side*0.66,0.11),Vector2(side*1.18,0.46),Vector2(side*1.02,0.69),Vector2(side*0.58,0.4)]),0.065,ARMOR,Vector3(0,0.67,0))
			var barrel = box(root,Vector3(0.12,0.12,0.8),ARMOR,Vector3(side*0.82,0.6,-0.05))
			if parent==game.player: weapon_nodes.append(barrel)
			box(root,Vector3(0.17,0.12,0.24),CYAN,Vector3(side*0.29,0.51,0.8),true)
			engine_glows.append(glow(root,Vector3(side*0.29,0.47,1.05),CYAN,0.65))
		var cockpit = plate(root,PackedVector2Array([Vector2(0,-0.82),Vector2(0.24,-0.24),Vector2(0.16,0.23),Vector2(-0.16,0.23),Vector2(-0.24,-0.24)]),0.23,Color("193b50"),Vector3(0,0.81,0))
		cockpit.material_override.metallic = 0.72
		cockpit.material_override.roughness = 0.15
		box(root,Vector3(0.055,0.03,0.4),CYAN*0.7,Vector3(0,1.055,-0.19),true)
		root.scale = Vector3.ONE*0.83
		if parent==game.player: pilot_visual = root
	elif kind == "seeker":
		plate(root,PackedVector2Array([Vector2(0,-0.9),Vector2(0.66,0.42),Vector2(0.2,0.2),Vector2(0,0.6),Vector2(-0.2,0.2),Vector2(-0.66,0.42)]),0.3,Color("9b4f4b"),Vector3(0,0.4,0))
		box(root,Vector3(0.16,0.08,0.55),RED,Vector3(0,0.75,-0.1),true)
		plate(root,PackedVector2Array([Vector2(0,-0.52),Vector2(0.17,-0.09),Vector2(0,0.18),Vector2(-0.17,-0.09)]),0.13,Color("3a222d"),Vector3(0,0.72,0))
		glow(root,Vector3(0,0.4,0.48),Color(1,0.23,0.13,0.8),0.7)
	elif kind == "spitter":
		plate(root,PackedVector2Array([Vector2(0,-0.65),Vector2(1.05,0.2),Vector2(0.65,0.55),Vector2(-0.65,0.55),Vector2(-1.05,0.2)]),0.3,Color("66547f"),Vector3(0,0.4,0))
		for side in [-1,1]:
			box(root,Vector3(0.18,0.17,0.9),Color("ba97d4"),Vector3(side*0.63,0.65,-0.2))
			plate(root,PackedVector2Array([Vector2(-0.12,-0.3),Vector2(0.12,-0.3),Vector2(0.17,0.3),Vector2(-0.17,0.3)]),0.12,Color("382940"),Vector3(side*0.75,0.7,0.2))
		plate(root,PackedVector2Array([Vector2(0,-0.5),Vector2(0.24,-0.03),Vector2(0.14,0.35),Vector2(-0.14,0.35),Vector2(-0.24,-0.03)]),0.2,Color("30253c"),Vector3(0,0.74,0))
		glow(root,Vector3(0,0.71,0),Color(0.68,0.45,1,1),0.8)
	elif kind == "brute":
		plate(root,PackedVector2Array([Vector2(-0.7,-0.7),Vector2(0.7,-0.7),Vector2(1,0),Vector2(0.7,0.8),Vector2(-0.7,0.8),Vector2(-1,0)]),0.5,Color("785849"),Vector3(0,0.25,0))
		plate(root,PackedVector2Array([Vector2(-0.5,-0.55),Vector2(0.5,-0.55),Vector2(0.55,0.45),Vector2(-0.55,0.45)]),0.23,Color("533b35"),Vector3(0,0.75,0))
		box(root,Vector3(0.6,0.09,0.12),RED,Vector3(0,0.8,-0.57),true)
		for side in [-1,1]:
			plate(root,PackedVector2Array([Vector2(-0.17,-0.43),Vector2(0.17,-0.43),Vector2(0.17,0.48),Vector2(-0.17,0.48)]),0.19,Color("7d726a"),Vector3(side*0.64,0.64,0))
			for vent in [-0.23,0.0,0.23]: box(root,Vector3(0.27,0.03,0.055),Color("28252a"),Vector3(side*0.64,0.845,vent))
	elif kind=="skirmisher":
		plate(root,PackedVector2Array([Vector2(0,-1.1),Vector2(0.34,-0.1),Vector2(1.0,0.65),Vector2(0.17,0.39),Vector2(0,0.76),Vector2(-0.17,0.39),Vector2(-1.0,0.65),Vector2(-0.34,-0.1)]),0.28,Color("a15a4c"),Vector3(0,0.45,0))
		box(root,Vector3(0.12,0.1,0.55),GOLD,Vector3(0,0.82,-0.21),true)
		for side in [-1,1]: glow(root,Vector3(side*0.4,0.44,0.66),RED,0.6)
	elif kind=="sentinel":
		plate(root,PackedVector2Array([Vector2(-0.8,-0.45),Vector2(0.8,-0.45),Vector2(0.9,0.45),Vector2(0,0.85),Vector2(-0.9,0.45)]),0.5,Color("526977"),Vector3(0,0.3,0))
		box(root,Vector3(0.65,0.13,0.12),CYAN,Vector3(0,0.85,-0.3),true)
		var guard = Node3D.new()
		guard.name = "Guard"
		root.add_child(guard)
		for i in range(5):
			var angle = PI+PI*(i+0.5)/5.0
			var panel = box(guard,Vector3(0.34,0.42,0.035),Color(0.25,0.85,1,0.4),Vector3(cos(angle)*0.97,0.53,sin(angle)*0.97),true)
			panel.rotation.y = -angle+PI/2
			panel.material_override.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	elif kind=="bomber":
		plate(root,PackedVector2Array([Vector2(-0.8,-0.6),Vector2(0.8,-0.6),Vector2(0.62,0.65),Vector2(-0.62,0.65)]),0.43,Color("786149"),Vector3(0,0.4,0))
		game._ring(root,0.38,0.09,RED,0.93)
		for side in [-1,1]: box(root,Vector3(0.19,0.2,0.8),Color("30343c"),Vector3(side*0.66,0.84,0))
	elif kind=="mine":
		var mine = SphereMesh.new()
		mine.radius = 0.42
		mine.height = 0.7
		mine.radial_segments = 12
		mine.rings = 6
		mesh(root,mine,Color("5e514b"),Vector3(0,0.48,0))
		game._ring(root,0.53,0.07,RED,0.45)
		glow(root,Vector3(0,0.84,0),RED,0.55)
	elif kind=="gatekeeper":
		plate(root,PackedVector2Array([Vector2(-1.65,-1.2),Vector2(1.65,-1.2),Vector2(1.65,1.2),Vector2(-1.65,1.2)]),0.65,Color("637681"),Vector3(0,0.4,0))
		for side in [-1,1]: plate(root,PackedVector2Array([Vector2(-0.32,-1.6),Vector2(0.32,-1.6),Vector2(0.32,1.6),Vector2(-0.32,1.6)]),0.55,Color("9e8870"),Vector3(side*1.4,0.82,0))
		game._ring(root,0.88,0.12,GOLD,1.35)
		glow(root,Vector3(0,1.38,0),GOLD,1.5)
	elif kind=="carrier":
		plate(root,PackedVector2Array([Vector2(-1.45,-2.0),Vector2(1.45,-2.0),Vector2(1.65,1.65),Vector2(-1.65,1.65)]),0.7,Color("5f5470"),Vector3(0,0.3,0))
		plate(root,PackedVector2Array([Vector2(-0.7,-1.4),Vector2(0.7,-1.4),Vector2(0.6,1.2),Vector2(-0.6,1.2)]),0.5,Color("897987"),Vector3(0,1.0,0))
		for side in [-1,1]: box(root,Vector3(0.18,0.1,1.65),RED,Vector3(side*1.2,1.03,0.2),true)
	else:
		var outline = PackedVector2Array()
		for i in range(8):
			outline.append(Vector2(cos(i*TAU/8),sin(i*TAU/8))*2.1)
		plate(root,outline,0.6,Color("514953"),Vector3(0,0.4,0))
		var inner = PackedVector2Array()
		for point in outline: inner.append(point*0.62)
		plate(root,inner,0.5,Color("908277"),Vector3(0,1.0,0))
		game._ring(root,1.12,0.18,RED,1.58)
		glow(root,Vector3(0,1.6,0),Color(1,0.32,0.12,1),1.65)
		for side in [-1,1]:
			box(root,Vector3(0.42,0.32,2.2),Color("bda588"),Vector3(side*1.5,0.92,-0.1))
	return root

func update_weapon() -> void:
	for barrel in weapon_nodes:
		barrel.scale = [Vector3.ONE,Vector3(1.1,1,1.8),Vector3(1.8,1.3,0.6)][game.weapon]
		barrel.material_override.albedo_color = [ARMOR,GOLD,CYAN][game.weapon]
		barrel.material_override.emission_enabled = game.weapon==2
		barrel.material_override.emission = CYAN*0.45

func _fit_backdrop() -> void:
	if not backdrop: return
	var viewport_size = get_viewport().get_visible_rect().size
	var a = game.camera.project_position(Vector2.ZERO,85)
	var b = game.camera.project_position(viewport_size,85)
	var extent: Vector3 = game.camera.basis.inverse()*(b-a)
	var view = Vector2(absf(extent.x),absf(extent.y))*1.01
	var texture: Texture2D = backdrop.material_override.albedo_texture
	var texture_aspect = float(texture.get_width())/texture.get_height()
	if view.x/view.y>texture_aspect:
		view.y = view.x/texture_aspect
	else:
		view.x = view.y*texture_aspect
	backdrop.mesh.size = view

func _build_particles() -> void:
	particle_mesh = MultiMeshInstance3D.new()
	var multi = MultiMesh.new()
	multi.transform_format = MultiMesh.TRANSFORM_3D
	multi.use_colors = true
	var shape = BoxMesh.new()
	shape.size = Vector3.ONE
	multi.mesh = shape
	multi.instance_count = 192
	multi.visible_instance_count = 0
	particle_mesh.multimesh = multi
	var m = material(Color.WHITE,true)
	m.vertex_color_use_as_albedo = true
	m.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	particle_mesh.material_override = m
	particle_mesh.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
	add_child(particle_mesh)

func sparks(pos: Vector3, color: Color, count: int, strength = 1.0) -> void:
	for i in range(mini(count,192-motes.size())):
		var direction = Vector3(visual_rng.randf_range(-1,1),visual_rng.randf_range(0.15,0.7),visual_rng.randf_range(-1,1)).normalized()
		var life = visual_rng.randf_range(0.18,0.48)
		motes.append({"pos":pos,"velocity":direction*visual_rng.randf_range(3,9)*strength,"life":life,"total":life,"color":color,"size":visual_rng.randf_range(0.04,0.12),"stretch":visual_rng.randf_range(1,3)})

func explosion(pos: Vector3, color: Color, radius: float) -> void:
	sparks(pos+Vector3.UP*0.45,color,18 if radius<2 else 36,radius)
	if rings.size() >= 32:
		return
	var flash_node = glow(self,pos+Vector3.UP*0.6,color.lerp(Color.WHITE,0.45),radius*2.6)
	var ring = game._ring(self,maxf(0.2,radius*0.55),0.045,color,pos.y+0.08)
	ring.position.x = pos.x
	ring.position.z = pos.z
	ring.material_override.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	rings.append({"node":flash_node,"ring":ring,"life":0.45,"total":0.45,"scale":1.0})

func muzzle(pos: Vector3, color: Color) -> void:
	if rings.size()>=32:
		return
	var node = glow(self,pos,color,0.8)
	rings.append({"node":node,"ring":null,"life":0.09,"total":0.09,"scale":1.0})

func tick(delta: float) -> void:
	time += delta
	if is_instance_valid(pilot_visual):
		var heading: float = game.player.rotation.y
		var turn = clampf(angle_difference(pilot_heading,heading)/maxf(delta,0.001)*0.025,-0.13,0.13)
		pilot_visual.rotation.z = 0.0 if game.reduced_motion else lerpf(pilot_visual.rotation.z,turn,minf(delta*10.0,1.0))
		pilot_heading = heading
	for glow_node in engine_glows:
		if is_instance_valid(glow_node):
			glow_node.scale = Vector3.ONE*(0.8+sin(time*24)*0.07+(0.6 if game.dash_time>0 else 0.0))
	trail_clock -= delta
	if game.phase == "playing" and trail_clock<=0:
		trail_clock = 0.025 if game.dash_time>0 else 0.075
		if game.move.length_squared()>0.1 or game.dash_time>0:
			var pos = game.player.position + game.player.basis.z*0.7 + Vector3.UP*0.35
			sparks(pos,CYAN,3 if game.dash_time>0 else 1,0.15)
	for i in range(motes.size()-1,-1,-1):
		var p = motes[i]
		p.life -= delta
		p.pos += p.velocity*delta
		p.velocity.y -= delta*3.5
		if p.life <= 0:
			motes.remove_at(i)
	particle_mesh.multimesh.visible_instance_count = motes.size()
	for i in range(motes.size()):
		var p = motes[i]
		var size_factor = p.size*(0.4+0.6*p.life/p.total)
		var basis = Basis.IDENTITY.scaled(Vector3(size_factor,size_factor,size_factor*p.stretch))
		particle_mesh.multimesh.set_instance_transform(i,Transform3D(basis,p.pos))
		var tint: Color = p.color
		tint.a = clampf(p.life/p.total,0,1)
		particle_mesh.multimesh.set_instance_color(i,tint)
	for i in range(rings.size()-1,-1,-1):
		var effect = rings[i]
		effect.life -= delta
		var progress = clampf(1.0-effect.life/effect.total,0,1)
		effect.node.scale = Vector3.ONE*(1.0+progress*1.2)
		var tint: Color = effect.node.material_override.get_shader_parameter("tint")
		tint.a = (1.0-progress)*0.9
		effect.node.material_override.set_shader_parameter("tint",tint)
		if is_instance_valid(effect.ring):
			effect.ring.scale = Vector3.ONE*(1.0+progress*2.5)
			var ring_tint: Color = effect.ring.material_override.albedo_color
			ring_tint.a = 1.0-progress
			effect.ring.material_override.albedo_color = ring_tint
		if effect.life <= 0:
			effect.node.queue_free()
			if is_instance_valid(effect.ring): effect.ring.queue_free()
			rings.remove_at(i)

func reset_effects() -> void:
	engine_glows = engine_glows.filter(func(node): return is_instance_valid(node))
	if is_instance_valid(pilot_visual):
		pilot_visual.rotation.z = 0.0
		pilot_heading = game.player.rotation.y
	motes.clear()
	for effect in rings:
		effect.node.queue_free()
		if is_instance_valid(effect.ring): effect.ring.queue_free()
	rings.clear()
	if particle_mesh: particle_mesh.multimesh.visible_instance_count = 0
