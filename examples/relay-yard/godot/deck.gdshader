shader_type spatial;
// Night apron: poured concrete slabs with saw-cut joints, tyre polish along the service lane,
// damp patches that catch the sodium lamps and sparse oil stains. World space, no textures or UVs.
uniform vec4 concrete : source_color = vec4(0.135, 0.13, 0.124, 1.0);
uniform vec4 joint : source_color = vec4(0.045, 0.045, 0.047, 1.0);
uniform float slab : hint_range(1.0, 8.0, 0.25) = 4.0;
uniform float joint_width : hint_range(0.005, 0.08) = 0.022;
uniform float lane_half_width : hint_range(0.0, 8.0) = 3.0;
uniform float wear : hint_range(0.0, 2.0) = 1.0;
uniform float wetness : hint_range(0.0, 1.0) = 0.55;
varying vec3 world_position;

float hash12(vec2 p) {
	vec3 p3 = fract(vec3(p.xyx) * 0.1031);
	p3 += dot(p3, p3.yzx + 33.33);
	return fract((p3.x + p3.y) * p3.z);
}
float vnoise(vec2 p) {
	vec2 i = floor(p);
	vec2 f = fract(p);
	f = f * f * (3.0 - 2.0 * f);
	return mix(mix(hash12(i), hash12(i + vec2(1.0, 0.0)), f.x), mix(hash12(i + vec2(0.0, 1.0)), hash12(i + vec2(1.0, 1.0)), f.x), f.y);
}
float fbm(vec2 p) {
	float v = 0.0;
	float a = 0.5;
	for (int i = 0; i < 4; i++) {
		v += a * vnoise(p);
		p = p * 2.07 + 13.1;
		a *= 0.5;
	}
	return v;
}
void vertex() {
	world_position = (MODEL_MATRIX * vec4(VERTEX, 1.0)).xyz;
}
void fragment() {
	vec2 p = world_position.xz;
	float spacing = max(0.5, slab);
	vec2 cell = p / spacing;
	vec2 id = floor(cell);
	vec2 to_edge = (0.5 - abs(fract(cell) - 0.5)) * spacing;
	float edge = min(to_edge.x, to_edge.y);
	float aa = fwidth(edge) + 0.002;
	float cut = 1.0 - smoothstep(joint_width, joint_width + aa * 1.5, edge);
	float tone = hash12(id * 1.37);
	float speck = hash12(floor(p * 26.0));
	float mottle = fbm(p * 0.45 + id * 3.7);
	float lane = 1.0 - smoothstep(lane_half_width - 0.6, lane_half_width + 0.4, abs(p.x));
	float tyre = smoothstep(0.45, 0.8, fbm(vec2(p.x * 7.0, p.y * 0.35))) * lane * wear;
	float damp = smoothstep(0.56, 0.66, fbm(p * 0.08 + 7.0)) * clamp(wetness, 0.0, 1.0);
	float oil = smoothstep(0.78, 0.86, fbm(p * 0.6 + 41.0)) * 0.6;
	vec3 base = concrete.rgb * (0.86 + tone * 0.12 + mottle * 0.18) * (0.94 + speck * 0.1);
	base *= 1.0 - 0.22 * tyre;
	base *= 1.0 - 0.45 * oil;
	base *= 1.0 - 0.35 * damp;
	base = mix(base, joint.rgb, cut);
	ALBEDO = base;
	ROUGHNESS = clamp(mix(0.88 - speck * 0.08 - tyre * 0.18, 0.16, damp), 0.08, 1.0);
	ROUGHNESS = mix(ROUGHNESS, 0.95, cut);
	SPECULAR = mix(0.35, 0.6, damp);
	METALLIC = 0.0;
}
