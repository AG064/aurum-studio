extends Node3D
const Art = preload("res://art.gd")
var sparks: Array[Dictionary] = []

func burst(at: Vector3, color: Color, count := 10) -> void:
	for index in mini(count, 48 - sparks.size()):
		var node := Art.orb(0.04 if count < 12 else 0.075, color)
		add_child(node)
		node.position = at
		var angle := float(index) * TAU / maxf(1, count)
		sparks.append({"node": node, "velocity": Vector3(cos(angle), 0.4 + index % 3 * 0.3, sin(angle)) * 3.0, "life": 0.55, "total": 0.55})

func pulse(at: Vector3, radius: float, color: Color) -> void:
	if sparks.size() >= 48: return
	var node := Art.ring(1.0, color)
	add_child(node)
	node.position = at
	node.position.y = 0.15
	sparks.append({"node": node, "velocity": Vector3.ZERO, "life": 0.5, "total": 0.5, "radius": radius})

func step(delta: float) -> void:
	for index in range(sparks.size() - 1, -1, -1):
		var spark: Dictionary = sparks[index]
		spark.life -= delta
		if spark.life <= 0:
			spark.node.queue_free()
			sparks.remove_at(index)
			continue
		if spark.has("radius"):
			spark.node.scale = Vector3.ONE * lerpf(0.1, spark.radius, 1.0 - spark.life / spark.total)
		else:
			spark.node.position += spark.velocity * delta
			spark.velocity.y -= delta * 7
			spark.node.scale = Vector3.ONE * (spark.life / spark.total)

func clear() -> void:
	for spark in sparks: spark.node.queue_free()
	sparks.clear()
