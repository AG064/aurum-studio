shader_type canvas_item;
// Painted steel plates for the dock instrument language, shared by the HUD and the dispatch
// terminal: chamfered corners, a bevel lit from the top left, selective paint wear on raised
// edges, optional corner fasteners, recessed wells with an inner shadow, and dark glass.
uniform vec2 size = vec2(240.0, 120.0);
uniform vec4 paint : source_color = vec4(0.135, 0.142, 0.152, 1.0);
uniform vec4 metal : source_color = vec4(0.5, 0.51, 0.52, 1.0);
uniform float chamfer = 6.0;
uniform float bevel = 2.0;
uniform float recess = 0.0;
uniform float wear = 0.3;
uniform float fasteners = 0.0;
uniform float glass = 0.0;
uniform float pressed = 0.0;
uniform float seed = 0.0;
uniform float opacity = 1.0;

float hash(vec2 p) {
	return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}

float noise(vec2 p) {
	vec2 i = floor(p);
	vec2 f = fract(p);
	f = f * f * (3.0 - 2.0 * f);
	return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), f.x), mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), f.x), f.y);
}

float shape(vec2 p) {
	vec2 half_size = size * 0.5;
	vec2 q = abs(p - half_size) - half_size;
	float box = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0);
	float cut = (q.x + q.y + chamfer) * 0.70710678;
	return max(box, cut);
}

void fragment() {
	vec2 p = UV * size;
	float d = shape(p);
	float coverage = clamp(0.5 - d, 0.0, 1.0);
	vec2 grad = vec2(shape(p + vec2(0.5, 0.0)) - shape(p - vec2(0.5, 0.0)), shape(p + vec2(0.0, 0.5)) - shape(p - vec2(0.0, 0.5)));
	grad = normalize(grad + vec2(1e-5));
	float facing = dot(grad, vec2(-0.70710678));
	float grain = noise(p * 0.07 + seed) * 0.55 + noise(p * 0.31 + seed * 2.3) * 0.3 + hash(floor(p) + seed) * 0.15;
	vec3 color = paint.rgb * (0.93 + grain * 0.11);
	float band = smoothstep(-bevel - 0.6, -bevel + 0.6, d);
	float lift = (recess > 0.5 ? -1.0 : 1.0) * (1.0 - pressed * 0.75);
	color *= 1.0 + band * facing * 0.45 * lift;
	if (recess > 0.5) {
		color *= 1.0 - smoothstep(-9.0, -bevel, d) * max(facing, 0.0) * 0.5;
	}
	float chips = smoothstep(0.68, 0.76, noise(p * 0.19 + seed * 5.0)) * smoothstep(-bevel - 4.0, -bevel, d) * wear * (1.0 - recess);
	color = mix(color, metal.rgb * (0.78 + grain * 0.35), chips);
	if (glass > 0.5) {
		float s = (p.x + (size.y - p.y) * 0.8) / max(size.x + size.y, 1.0);
		color += vec3(0.022) * (smoothstep(0.16, 0.2, s) - smoothstep(0.27, 0.31, s));
		color *= 0.88 + 0.12 * smoothstep(0.0, 18.0, -d);
	}
	if (fasteners > 0.5) {
		float inset = chamfer * 0.45 + 9.0;
		for (int i = 0; i < 2; i++) {
			for (int j = 0; j < 2; j++) {
				vec2 c = vec2(i == 0 ? inset : size.x - inset, j == 0 ? inset : size.y - inset);
				vec2 o = p - c;
				float r = length(o);
				float hole = 1.0 - smoothstep(3.7, 4.5, r);
				float head = 1.0 - smoothstep(2.8, 3.4, r);
				color = mix(color, paint.rgb * 0.35, hole);
				vec3 steel = metal.rgb * (1.12 - o.y * 0.07);
				float angle = hash(c + seed) * 3.14159;
				vec2 dir = vec2(cos(angle), sin(angle));
				float slot = (1.0 - smoothstep(0.35, 0.95, abs(dot(o, vec2(-dir.y, dir.x))))) * step(abs(dot(o, dir)), 2.6);
				steel *= 1.0 - slot * 0.6;
				color = mix(color, steel, head);
			}
		}
	}
	COLOR = vec4(color, coverage * opacity);
}
