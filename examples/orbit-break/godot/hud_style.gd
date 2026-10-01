extends RefCounted

const GLASS = Color("101d28")
const EDGE = Color("435966")
const TEAL = Color("69f4de")
const INK = Color("071018")
const DISPLAY_FONT = preload("res://assets/fonts/BarlowCondensed-SemiBold.ttf")

static func outline(rect: Rect2, cut: float) -> PackedVector2Array:
	var a = rect.position
	var b = rect.end
	return PackedVector2Array([Vector2(a.x+cut,a.y),Vector2(b.x-cut,a.y),Vector2(b.x,a.y+cut),Vector2(b.x,b.y-cut),Vector2(b.x-cut,b.y),Vector2(a.x+cut,b.y),Vector2(a.x,b.y-cut),Vector2(a.x,a.y+cut)])

static func plate(canvas: CanvasItem, rect: Rect2, tint = GLASS, accent = EDGE, cut = 7.0, active = 0.0) -> void:
	for depth in [9.0,5.0]:
		canvas.draw_colored_polygon(outline(Rect2(rect.position+Vector2(2,depth),rect.size),cut),Color(0,0,0,0.16 if depth==9 else 0.38))
	canvas.draw_colored_polygon(outline(Rect2(rect.position+Vector2(2,4),rect.size),cut),Color("070e14"))
	var points = outline(rect,cut)
	var shades = PackedColorArray()
	for point in points:
		var amount = (point.y-rect.position.y)/maxf(rect.size.y,1.0)
		shades.append(Color("50606a").lerp(Color("16232d"),amount).lerp(accent,active*0.09))
	canvas.draw_polygon(points,shades)
	var face = rect.grow(-4)
	var inner = outline(face,maxf(cut-2,2))
	var face_shades = PackedColorArray()
	for point in inner:
		var amount = (point.y-face.position.y)/maxf(face.size.y,1.0)
		face_shades.append(tint.lightened(0.08*(1.0-amount)+active*0.045).darkened(amount*0.17))
	canvas.draw_polygon(inner,face_shades)
	# A clipped reflection sits under text and respects the housing silhouette.
	canvas.draw_colored_polygon(PackedVector2Array([face.position+Vector2(cut,0),Vector2(face.end.x-cut,face.position.y),Vector2(face.end.x,face.position.y+cut),Vector2(face.position.x+face.size.x*0.38,face.position.y+face.size.y*0.46),Vector2(face.position.x,face.position.y+face.size.y*0.46),Vector2(face.position.x,face.position.y+cut)]),Color(0.71,0.84,0.91,0.025+active*0.025))
	var line = points.duplicate()
	line.append(points[0])
	canvas.draw_polyline(line,EDGE.lerp(accent,0.24+active*0.6),1.0,true)
	canvas.draw_line(rect.position+Vector2(cut+1,1),Vector2(rect.end.x-cut-1,rect.position.y+1),Color(0.7,0.83,0.88,0.25+active*0.22),1,true)
	canvas.draw_line(Vector2(rect.position.x+cut+1,rect.end.y-2),rect.end-Vector2(cut+1,2),Color(0,0,0,0.6),2)
	if rect.size.x>80 and rect.size.y>80:
		for x in [rect.position.x+cut+2,rect.end.x-cut-2]:
			var screw = Vector2(x,rect.end.y-9)
			canvas.draw_circle(screw,2.1,Color("080f15"))
			canvas.draw_arc(screw,1.8,PI,TAU,6,Color("70828a"),1,true)
			canvas.draw_line(screw-Vector2(1,0),screw+Vector2(1,0),Color("20313b"),1)

static func well(canvas: CanvasItem, rect: Rect2, accent: Color) -> void:
	canvas.draw_colored_polygon(outline(rect,4),Color("071019"))
	canvas.draw_colored_polygon(outline(rect.grow(-2),3),Color("0c1721"))
	canvas.draw_line(rect.position+Vector2(4,0),Vector2(rect.end.x-4,rect.position.y),Color("050b10"),2)
	canvas.draw_line(Vector2(rect.position.x+4,rect.end.y),rect.end-Vector2(4,0),Color("46515b"),1)
	var lower = rect.position.y+rect.size.y*0.76
	canvas.draw_line(Vector2(rect.position.x+8,lower),Vector2(rect.end.x-8,lower),Color(accent,0.11),1)
	for x in [rect.position.x+8,rect.end.x-8]:
		canvas.draw_line(Vector2(x,rect.position.y+8),Vector2(x,rect.position.y+16),Color(accent,0.25),1)

static func _prism(canvas: CanvasItem, center: Vector2, size: Vector2, depth: Vector2, color: Color) -> void:
	var r = Rect2(center-size*0.5,size)
	canvas.draw_rect(Rect2(r.position+depth+Vector2(0,4),r.size),Color(0,0,0,0.3))
	canvas.draw_colored_polygon(PackedVector2Array([Vector2(r.end.x,r.position.y),r.end,r.end+depth,Vector2(r.end.x,r.position.y)+depth]),color.darkened(0.52))
	canvas.draw_colored_polygon(PackedVector2Array([Vector2(r.position.x,r.end.y),r.end,r.end+depth,Vector2(r.position.x,r.end.y)+depth]),color.darkened(0.32))
	canvas.draw_rect(r,color)
	canvas.draw_line(r.position,Vector2(r.end.x,r.position.y),color.lightened(0.3),1,true)
	canvas.draw_line(r.position,Vector2(r.position.x,r.end.y),color.lightened(0.12),1,true)

static func equipment(canvas: CanvasItem, kind: int, center: Vector2, scale: float, accent: Color) -> void:
	var steel = Color("718895")
	var depth = Vector2(5,6)*scale
	canvas.draw_circle(center+Vector2(3,5)*scale,31*scale,Color(0,0,0,0.16))
	match kind:
		7,8,9:
			var hull = PackedVector2Array()
			var width = 29.0 if kind==8 else 24.0
			for p in [Vector2(0,-31),Vector2(8,-7),Vector2(width,18),Vector2(7,12),Vector2(0,26),Vector2(-7,12),Vector2(-width,18),Vector2(-8,-7)]: hull.append(center+p*scale)
			var shadow = hull.duplicate()
			for i in range(shadow.size()): shadow[i] += depth
			canvas.draw_colored_polygon(shadow,steel.darkened(0.55))
			canvas.draw_colored_polygon(hull,steel.lightened(0.15) if kind==8 else steel)
			if kind==8:
				for side in [-1,1]: _prism(canvas,center+Vector2(side*21,6)*scale,Vector2(12,24)*scale,depth,steel.lightened(0.12))
			_prism(canvas,center-Vector2(0,3)*scale,Vector2(8,22)*scale,depth*0.6,Color("203746"))
			for side in [-1,1]:
				canvas.draw_rect(Rect2(center+Vector2(side*13-3,14)*scale,Vector2(6,5)*scale),accent)
			if kind==9:
				var escort = center+Vector2(25,-16)*scale
				canvas.draw_colored_polygon(PackedVector2Array([escort+Vector2(0,-9)*scale,escort+Vector2(7,6)*scale,escort,escort+Vector2(-7,6)*scale]),accent)
		0:
			for x in [-24.0,0.0,24.0]:
				var offset = Vector2(x,-6 if x==0 else 5)*scale
				_prism(canvas,center+offset,Vector2(13,43)*scale,depth,steel)
				canvas.draw_rect(Rect2(center+offset+Vector2(-5,-19)*scale,Vector2(10,5)*scale),accent)
				canvas.draw_rect(Rect2(center+offset+Vector2(-5,5)*scale,Vector2(10,4)*scale),Color("243b49"))
				canvas.draw_line(center+offset+Vector2(0,-31)*scale,center+offset+Vector2(0,-24)*scale,Color(accent,0.55),2*scale,true)
		1:
			_prism(canvas,center+Vector2(0,8)*scale,Vector2(34,31)*scale,depth,Color("314958"))
			for x in [-17.0,17.0]:
				_prism(canvas,center+Vector2(x,-4)*scale,Vector2(8,54)*scale,depth,steel)
				canvas.draw_rect(Rect2(center+Vector2(x-3,-25)*scale,Vector2(6,5)*scale),accent)
			canvas.draw_line(center+Vector2(0,18)*scale,center+Vector2(0,-31)*scale,accent,3*scale,true)
			canvas.draw_line(center+Vector2(-23,12)*scale,center+Vector2(23,12)*scale,steel.lightened(0.2),2*scale,true)
		2:
			var nodes = [Vector2(-28,13),Vector2(0,-19),Vector2(28,13)]
			for i in range(2):
				var middle: Vector2 = nodes[i].lerp(nodes[i+1],0.5)
				canvas.draw_polyline(PackedVector2Array([center+nodes[i]*scale,center+(middle+Vector2(4,2))*scale,center+(middle-Vector2(4,2))*scale,center+nodes[i+1]*scale]),Color(accent,0.8),2*scale,true)
			for p in nodes:
				_prism(canvas,center+p*scale,Vector2(17,19)*scale,depth,Color("344f61"))
				canvas.draw_colored_polygon(PackedVector2Array([center+(p+Vector2(0,-16))*scale,center+(p+Vector2(6,-3))*scale,center+(p+Vector2(0,9))*scale,center+(p+Vector2(-6,-3))*scale]),accent)
				canvas.draw_colored_polygon(PackedVector2Array([center+(p+Vector2(0,-16))*scale,center+(p+Vector2(6,-3))*scale,center+(p+Vector2(0,9))*scale]),accent.darkened(0.3))
		3:
			var shield = PackedVector2Array()
			for p in [Vector2(0,-27),Vector2(24,-16),Vector2(20,9),Vector2(0,29),Vector2(-20,9),Vector2(-24,-16)]: shield.append(center+p*scale)
			var shadow = shield.duplicate()
			for i in range(shadow.size()): shadow[i] += depth
			canvas.draw_colored_polygon(shadow,steel.darkened(0.5))
			canvas.draw_colored_polygon(shield,steel)
			glyph(canvas,3,center,scale*0.7,accent)
		4:
			canvas.draw_arc(center,29*scale,0.1,TAU-0.4,32,Color(accent,0.35),1.5*scale,true)
			for p in [Vector2(-17,11),Vector2(17,-9)]:
				var craft = PackedVector2Array([center+(p+Vector2(0,-17))*scale,center+(p+Vector2(12,10))*scale,center+p*scale,center+(p+Vector2(-12,10))*scale])
				canvas.draw_colored_polygon(craft,steel)
				canvas.draw_line(center+(p+Vector2(0,-9))*scale,center+(p+Vector2(0,3))*scale,accent,3*scale,true)
		5:
			_prism(canvas,center-Vector2(0,6)*scale,Vector2(29,38)*scale,depth,steel)
			canvas.draw_rect(Rect2(center+Vector2(-11,7)*scale,Vector2(22,5)*scale),accent)
			for y in [19.0,29.0]: canvas.draw_polyline(PackedVector2Array([center+Vector2(-11,y)*scale,center+Vector2(0,y+7)*scale,center+Vector2(11,y)*scale]),Color(accent,0.8 if y==19 else 0.4),2*scale,true)
		_:
			_prism(canvas,center,Vector2(42,42)*scale,depth,steel)
			canvas.draw_circle(center,14*scale,Color("152b38"))
			canvas.draw_arc(center,12*scale,0,TAU,24,accent,3*scale,true)
			glyph(canvas,6,center,scale*0.42,accent)

static func segmented(canvas: CanvasItem, rect: Rect2, ratio: float, color: Color, count = 16, echo = 0.0) -> void:
	canvas.draw_rect(rect.grow(2),INK)
	var gap = 3.0
	var width = (rect.size.x-gap*(count-1))/count
	for i in range(count):
		var slot = Rect2(rect.position+Vector2(i*(width+gap),0),Vector2(width,rect.size.y))
		canvas.draw_rect(slot,Color("293a44"))
		var lingering = clampf(echo*count-i,0,1)
		if lingering>0: canvas.draw_rect(Rect2(slot.position,Vector2(width*lingering,slot.size.y)),Color("d99559"))
		var fill = clampf(ratio*count-i,0,1)
		if fill>0:
			canvas.draw_rect(Rect2(slot.position,Vector2(width*fill,slot.size.y)),color)
			canvas.draw_line(slot.position,slot.position+Vector2(width*fill,0),color.lightened(0.4),1)

static func glyph(canvas: CanvasItem, kind: int, center: Vector2, scale: float, color: Color) -> void:
	var weight = maxf(1.0,2.0*scale)
	match kind:
		0:
			for x in [-20.0,0.0,20.0]:
				var y = -10.0 if x==0 else 4.0
				var points = PackedVector2Array()
				for p in [Vector2(x-4,y+18),Vector2(x-4,y-12),Vector2(x,y-19),Vector2(x+4,y-12),Vector2(x+4,y+18)]: points.append(center+p*scale)
				canvas.draw_polyline(points,color,weight,true)
				canvas.draw_line(center+Vector2(x,y+24)*scale,center+Vector2(x,y+31)*scale,color.darkened(0.35),weight)
		1:
			for x in [-12.0,12.0]:
				canvas.draw_line(center+Vector2(x,-24)*scale,center+Vector2(x,24)*scale,color,weight*1.5,true)
			canvas.draw_line(center+Vector2(0,30)*scale,center+Vector2(0,-30)*scale,color.lightened(0.4),weight,true)
			canvas.draw_polyline(PackedVector2Array([center+Vector2(-7,-20)*scale,center+Vector2(0,-31)*scale,center+Vector2(7,-20)*scale]),color,weight,true)
			for y in [-10.0,10.0]: canvas.draw_line(center+Vector2(-18,y)*scale,center+Vector2(18,y)*scale,color.darkened(0.45),weight,true)
		2:
			var nodes = [Vector2(-29,12),Vector2(0,-22),Vector2(29,12)]
			for i in range(2):
				var a: Vector2 = nodes[i]
				var b: Vector2 = nodes[i+1]
				var middle: Vector2 = a.lerp(b,0.5)
				canvas.draw_polyline(PackedVector2Array([center+a*scale,center+(middle+Vector2(6,0))*scale,center+(middle-Vector2(6,0))*scale,center+b*scale]),color,weight,true)
			for node in nodes:
				canvas.draw_arc(center+node*scale,5*scale,0,TAU,16,color,weight,true)
				canvas.draw_circle(center+node*scale,2*scale,color.lightened(0.4))
		3:
			var points = PackedVector2Array()
			for p in [Vector2(0,-27),Vector2(24,-17),Vector2(20,9),Vector2(0,29),Vector2(-20,9),Vector2(-24,-17),Vector2(0,-27)]: points.append(center+p*scale)
			canvas.draw_polyline(points,color,weight,true)
			canvas.draw_line(center+Vector2(-9,0)*scale,center+Vector2(9,0)*scale,color,weight*1.6)
			canvas.draw_line(center+Vector2(0,-9)*scale,center+Vector2(0,9)*scale,color,weight*1.6)
		4:
			canvas.draw_arc(center,28*scale,0.1,TAU-0.4,28,color.darkened(0.4),weight,true)
			for p in [Vector2(-20,12),Vector2(19,-10)]:
				canvas.draw_colored_polygon(PackedVector2Array([center+(p+Vector2(0,-10))*scale,center+(p+Vector2(8,7))*scale,center+p*scale,center+(p+Vector2(-8,7))*scale]),color)
		5:
			for y in [-12.0,10.0]:
				canvas.draw_polyline(PackedVector2Array([center+Vector2(-20,y+12)*scale,center+Vector2(0,y-10)*scale,center+Vector2(20,y+12)*scale]),color,weight*1.5,true)
		_:
			canvas.draw_arc(center,25*scale,0,TAU,32,color,weight,true)
			canvas.draw_polyline(PackedVector2Array([center+Vector2(4,-21)*scale,center+Vector2(-10,3)*scale,center+Vector2(3,0)*scale,center+Vector2(-4,22)*scale]),color,weight*1.6,true)

static func dial(canvas: CanvasItem, center: Vector2, radius: float, progress: float, color: Color) -> void:
	canvas.draw_circle(center,radius+5,INK)
	for i in range(12):
		var a = PI*0.7+(PI*1.6)*i/12.0
		var b = a+PI*1.6/12.0*0.7
		canvas.draw_arc(center,radius,a,b,5,color if float(i)/12.0<progress else Color("30434c"),3,true)
	canvas.draw_polyline(PackedVector2Array([center+Vector2(-5,3),center+Vector2(0,-5),center+Vector2(5,3)]),color,1.7,true)
