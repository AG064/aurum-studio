extends RefCounted
## Low-poly mesh builder for the Relay Yard asset forge.
## Every primitive takes a Transform3D. Faces are wound for Godot's clockwise front-face convention
## from an explicit outward normal, so mirrored transforms stay correct. One surface per material.

var materials: Dictionary
var tools := {}
var order: Array[String] = []
var triangles := 0

func _init(material_table: Dictionary) -> void:
	materials = material_table

func _tool(name: String) -> SurfaceTool:
	if not tools.has(name):
		var surface := SurfaceTool.new()
		surface.begin(Mesh.PRIMITIVE_TRIANGLES)
		surface.set_material(materials[name])
		tools[name] = surface
		order.append(name)
	return tools[name]

func _vertex(surface: SurfaceTool, p: Vector3, n: Vector3) -> void:
	var ax := absf(n.x)
	var ay := absf(n.y)
	var az := absf(n.z)
	var uv := Vector2(p.z, p.y) if ax >= ay and ax >= az else (Vector2(p.x, p.z) if ay >= az else Vector2(p.x, p.y))
	surface.set_normal(n)
	surface.set_uv(uv * 0.5)
	surface.add_vertex(p)

static func _normal_basis(xf: Transform3D) -> Basis:
	return xf.basis.inverse().transposed()

## Triangle in world space. Normals may be null for flat shading.
func tri(name: String, a: Vector3, b: Vector3, c: Vector3, outward: Vector3, na = null, nb = null, nc = null) -> void:
	var cross := (b - a).cross(c - a)
	if cross.length_squared() < 1e-14: return
	var flat := cross.normalized()
	if flat.dot(outward) < 0: flat = -flat
	var n0: Vector3 = flat if na == null else na
	var n1: Vector3 = flat if nb == null else nb
	var n2: Vector3 = flat if nc == null else nc
	# Godot front faces are clockwise seen from outside: the raw cross product must point inward.
	if cross.dot(outward) > 0:
		var swap := b
		b = c
		c = swap
		var swap_n := n1
		n1 = n2
		n2 = swap_n
	var surface := _tool(name)
	_vertex(surface, a, n0)
	_vertex(surface, b, n1)
	_vertex(surface, c, n2)
	triangles += 1

## Convex planar polygon, flat shaded.
func poly(name: String, points: Array, outward: Vector3) -> void:
	for index in range(1, points.size() - 1):
		tri(name, points[0], points[index], points[index + 1], outward)

## Box centred on xf.origin with optional chamfer on every edge.
func box(name: String, size: Vector3, xf := Transform3D.IDENTITY, bevel := 0.0) -> void:
	var h := size * 0.5
	var b := clampf(bevel, 0.0, minf(h.x, minf(h.y, h.z)) * 0.9)
	var nb := _normal_basis(xf)
	for axis in 3:
		var u := (axis + 1) % 3
		var v := (axis + 2) % 3
		for side in [-1.0, 1.0]:
			var points := []
			for corner in [Vector2(-1, -1), Vector2(1, -1), Vector2(1, 1), Vector2(-1, 1)]:
				var s := Vector3.ZERO
				s[axis] = side
				s[u] = corner.x
				s[v] = corner.y
				points.append(xf * _chamfer_point(h, b, s, axis))
			var n := Vector3.ZERO
			n[axis] = side
			poly(name, points, (nb * n).normalized())
	if b <= 0.0001: return
	for pair in [[0, 1], [0, 2], [1, 2]]:
		var a1: int = pair[0]
		var a2: int = pair[1]
		var w := 3 - a1 - a2
		for s1 in [-1.0, 1.0]:
			for s2 in [-1.0, 1.0]:
				var points := []
				for step in [[-1.0, a1], [1.0, a1], [1.0, a2], [-1.0, a2]]:
					var s := Vector3.ZERO
					s[a1] = s1
					s[a2] = s2
					s[w] = step[0]
					points.append(xf * _chamfer_point(h, b, s, step[1]))
				var n := Vector3.ZERO
				n[a1] = s1
				n[a2] = s2
				poly(name, points, (nb * n).normalized())
	for sx in [-1.0, 1.0]:
		for sy in [-1.0, 1.0]:
			for sz in [-1.0, 1.0]:
				var s := Vector3(sx, sy, sz)
				tri(name, xf * _chamfer_point(h, b, s, 0), xf * _chamfer_point(h, b, s, 1), xf * _chamfer_point(h, b, s, 2), (nb * s).normalized())

static func _chamfer_point(h: Vector3, b: float, s: Vector3, axis: int) -> Vector3:
	var p := Vector3(s.x * (h.x - b), s.y * (h.y - b), s.z * (h.z - b))
	p[axis] = s[axis] * h[axis]
	return p

## Tapered block: base rectangle half-extents `bottom` (x, z) at local y = 0, `top` at y = height.
func taper(name: String, bottom: Vector2, top: Vector2, height: float, xf := Transform3D.IDENTITY, shift := Vector2.ZERO) -> void:
	var lo := [Vector3(-bottom.x, 0, -bottom.y), Vector3(bottom.x, 0, -bottom.y), Vector3(bottom.x, 0, bottom.y), Vector3(-bottom.x, 0, bottom.y)]
	var hi := []
	for p in [Vector3(-top.x, height, -top.y), Vector3(top.x, height, -top.y), Vector3(top.x, height, top.y), Vector3(-top.x, height, top.y)]:
		hi.append(p + Vector3(shift.x, 0, shift.y))
	var centre := xf * Vector3(shift.x * 0.5, height * 0.5, shift.y * 0.5)
	var faces := [[lo[0], lo[1], lo[2], lo[3]], [hi[0], hi[1], hi[2], hi[3]]]
	for index in 4:
		var next := (index + 1) % 4
		faces.append([lo[index], lo[next], hi[next], hi[index]])
	for face in faces:
		var points := []
		var mid := Vector3.ZERO
		for p in face:
			points.append(xf * p)
			mid += xf * p
		poly(name, points, (mid / 4.0 - centre).normalized())

## Extruded polygon. Profile coordinates map to (U, V); extrusion runs along W, centred on xf.origin.
## axis "x": profile (z, y) extruded along X. "y": profile (x, z) along Y. "z": profile (x, y) along Z.
func prism(name: String, profile: PackedVector2Array, depth: float, xf := Transform3D.IDENTITY, axis := "x") -> void:
	var u := Vector3(0, 0, 1)
	var v := Vector3(0, 1, 0)
	var w := Vector3(1, 0, 0)
	if axis == "y":
		u = Vector3(1, 0, 0)
		v = Vector3(0, 0, 1)
		w = Vector3(0, 1, 0)
	elif axis == "z":
		u = Vector3(1, 0, 0)
		v = Vector3(0, 1, 0)
		w = Vector3(0, 0, 1)
	var nb := _normal_basis(xf)
	var area := 0.0
	for index in profile.size():
		var p := profile[index]
		var q := profile[(index + 1) % profile.size()]
		area += p.x * q.y - q.x * p.y
	var ccw := area > 0
	var half := depth * 0.5
	var indices := Geometry2D.triangulate_polygon(profile)
	for side in [-1.0, 1.0]:
		var outward: Vector3 = (nb * (w * side)).normalized()
		for index in range(0, indices.size(), 3):
			var a := profile[indices[index]]
			var b := profile[indices[index + 1]]
			var c := profile[indices[index + 2]]
			tri(name, xf * (u * a.x + v * a.y + w * half * side), xf * (u * b.x + v * b.y + w * half * side), xf * (u * c.x + v * c.y + w * half * side), outward)
	for index in profile.size():
		var p := profile[index]
		var q := profile[(index + 1) % profile.size()]
		var edge := q - p
		var n2 := Vector2(edge.y, -edge.x) if ccw else Vector2(-edge.y, edge.x)
		var outward := (nb * (u * n2.x + v * n2.y)).normalized()
		poly(name, [xf * (u * p.x + v * p.y - w * half), xf * (u * q.x + v * q.y - w * half), xf * (u * q.x + v * q.y + w * half), xf * (u * p.x + v * p.y + w * half)], outward)

## Surface of revolution around local Y. Profile points are (radius, height), counter-clockwise
## (bottom, outside, top). Partial sweeps of closed profiles get end caps.
func lathe(name: String, profile: Array, segments: int, xf := Transform3D.IDENTITY, smooth := true, from_angle := 0.0, to_angle := TAU, crease_degrees := 40.0) -> void:
	var nb := _normal_basis(xf)
	var closed: bool = (profile[0] as Vector2).distance_to(profile[profile.size() - 1]) < 1e-5
	var seg_normals: Array[Vector2] = []
	for index in profile.size() - 1:
		var t: Vector2 = profile[index + 1] - profile[index]
		seg_normals.append(Vector2(t.y, -t.x).normalized() if t.length() > 1e-6 else Vector2.ZERO)
	var crease := cos(deg_to_rad(crease_degrees))
	var full := absf(to_angle - from_angle - TAU) < 1e-4
	for j in profile.size() - 1:
		var n_seg := seg_normals[j]
		if n_seg == Vector2.ZERO: continue
		var n_lo := n_seg
		var n_hi := n_seg
		if smooth:
			var prev := j - 1 if j > 0 else (profile.size() - 2 if closed else -1)
			var next := j + 1 if j < profile.size() - 2 else (0 if closed else -1)
			if prev >= 0 and seg_normals[prev] != Vector2.ZERO and seg_normals[prev].dot(n_seg) > crease: n_lo = (seg_normals[prev] + n_seg).normalized()
			if next >= 0 and seg_normals[next] != Vector2.ZERO and seg_normals[next].dot(n_seg) > crease: n_hi = (seg_normals[next] + n_seg).normalized()
		var p0: Vector2 = profile[j]
		var p1: Vector2 = profile[j + 1]
		for k in segments:
			var a0 := from_angle + (to_angle - from_angle) * k / segments
			var a1 := from_angle + (to_angle - from_angle) * (k + 1) / segments
			var am := (a0 + a1) * 0.5
			var v00 := xf * Vector3(p0.x * cos(a0), p0.y, p0.x * sin(a0))
			var v01 := xf * Vector3(p0.x * cos(a1), p0.y, p0.x * sin(a1))
			var v10 := xf * Vector3(p1.x * cos(a0), p1.y, p1.x * sin(a0))
			var v11 := xf * Vector3(p1.x * cos(a1), p1.y, p1.x * sin(a1))
			var outward := (nb * Vector3(n_seg.x * cos(am), n_seg.y, n_seg.x * sin(am))).normalized()
			if smooth:
				var m00 := (nb * Vector3(n_lo.x * cos(a0), n_lo.y, n_lo.x * sin(a0))).normalized()
				var m01 := (nb * Vector3(n_lo.x * cos(a1), n_lo.y, n_lo.x * sin(a1))).normalized()
				var m10 := (nb * Vector3(n_hi.x * cos(a0), n_hi.y, n_hi.x * sin(a0))).normalized()
				var m11 := (nb * Vector3(n_hi.x * cos(a1), n_hi.y, n_hi.x * sin(a1))).normalized()
				tri(name, v00, v01, v11, outward, m00, m01, m11)
				tri(name, v00, v11, v10, outward, m00, m11, m10)
			else:
				tri(name, v00, v01, v11, outward)
				tri(name, v00, v11, v10, outward)
	if closed and not full:
		var flat := PackedVector2Array()
		for index in profile.size() - 1: flat.append(profile[index])
		var indices := Geometry2D.triangulate_polygon(flat)
		for cap in [[from_angle, -1.0], [to_angle, 1.0]]:
			var angle: float = cap[0]
			var tangent: Vector3 = Vector3(-sin(angle), 0, cos(angle)) * float(cap[1])
			var outward: Vector3 = (nb * tangent).normalized()
			for index in range(0, indices.size(), 3):
				var pts := []
				for corner in 3:
					var p := flat[indices[index + corner]]
					pts.append(xf * Vector3(p.x * cos(angle), p.y, p.x * sin(angle)))
				tri(name, pts[0], pts[1], pts[2], outward)

## Ring around local Y with a circular section.
func torus(name: String, radius: float, thickness: float, segments: int, sides := 8, xf := Transform3D.IDENTITY, from_angle := 0.0, to_angle := TAU) -> void:
	var profile := []
	for index in sides + 1:
		var phi := -PI * 0.5 + TAU * index / sides
		profile.append(Vector2(radius + thickness * cos(phi), thickness * sin(phi)))
	lathe(name, profile, segments, xf, true, from_angle, to_angle, 70.0)

## Cylinder between two points.
func tube(name: String, a: Vector3, b: Vector3, radius: float, segments := 8, xf := Transform3D.IDENTITY, radius_b := -1.0) -> void:
	var axis := b - a
	var length := axis.length()
	if length < 1e-5: return
	var y := axis / length
	var helper := Vector3.UP if absf(y.dot(Vector3.UP)) < 0.95 else Vector3.RIGHT
	var x := y.cross(helper).normalized()
	var z := x.cross(y).normalized()
	var rb := radius if radius_b < 0 else radius_b
	lathe(name, [Vector2(0, 0), Vector2(radius, 0), Vector2(rb, length), Vector2(0, length)], segments, xf * Transform3D(Basis(x, y, z), a), true)

## Bowl-shaped dish around local Y opening upward, vertex at the origin.
func dish(name: String, radius: float, depth: float, thickness: float, segments: int, xf := Transform3D.IDENTITY) -> void:
	var rings := 5
	var profile := []
	for index in rings + 1:
		var r := radius * index / rings
		profile.append(Vector2(r, depth * pow(r / radius, 2)))
	for index in range(rings, -1, -1):
		var r := radius * index / rings
		profile.append(Vector2(r * 0.97, depth * pow(r / radius, 2) + thickness))
	lathe(name, profile, segments, xf, true, 0.0, TAU, 30.0)

func commit(name: String) -> ArrayMesh:
	var mesh := ArrayMesh.new()
	for key in order:
		var surface: SurfaceTool = tools[key]
		surface.index()
		surface.generate_tangents()
		surface.commit(mesh)
		mesh.surface_set_name(mesh.get_surface_count() - 1, key)
	mesh.resource_name = name
	return mesh
