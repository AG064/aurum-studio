extends Node3D
## Fixed geometry/texture workload. Timings vary with build, machine and renderer.
const SEED = 918123
const OBJECTS = 144
const TEXTURES = 24
var _meshes: Array[MeshInstance3D] = []
var _textures: Array[ImageTexture] = []
var _steps = 0

func _ready():
	var random = RandomNumberGenerator.new()
	random.seed = SEED
	for _index in TEXTURES:
		var image = Image.create(128, 128, false, Image.FORMAT_RGBA8)
		image.fill(Color(random.randf_range(0.2, 0.9), random.randf_range(0.2, 0.9), random.randf_range(0.2, 0.9)))
		_textures.append(ImageTexture.create_from_image(image))
	for index in OBJECTS:
		var instance = MeshInstance3D.new()
		instance.name = "Part_%03d" % index
		instance.mesh = BoxMesh.new()
		var material = StandardMaterial3D.new()
		material.albedo_texture = _textures[index % TEXTURES]
		instance.material_override = material
		instance.position = Vector3((index % 12) * 1.4 - 8.0, (index / 12) * 1.4 - 8.0, 0)
		add_child(instance)
		_meshes.append(instance)
	var camera = Camera3D.new()
	camera.position = Vector3(0, 0, 24)
	camera.current = true
	add_child(camera)
	var light = DirectionalLight3D.new()
	light.rotation_degrees = Vector3(-30, -25, 0)
	add_child(light)
	var environment = WorldEnvironment.new()
	var settings = Environment.new()
	settings.background_mode = Environment.BG_COLOR
	settings.background_color = Color(0.03, 0.04, 0.06)
	settings.ambient_light_source = Environment.AMBIENT_SOURCE_COLOR
	settings.ambient_light_color = Color(0.6, 0.65, 0.7)
	environment.environment = settings
	add_child(environment)

func _physics_process(_delta):
	_steps += 1
	for index in _meshes.size():
		_meshes[index].rotation.y = float((_steps + index) % 720) * PI / 360.0
