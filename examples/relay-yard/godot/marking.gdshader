shader_type spatial;
render_mode unshaded, blend_mix, cull_disabled, depth_draw_never, shadows_disabled;
// Projected deck markings in the dock's instrument language: graduated ring gauges (docking,
// extraction, drone charge collars) and graduated aim lines. One shader, per-instance materials.
uniform vec4 tint : source_color = vec4(0.93, 0.6, 0.26, 1.0);
uniform int mode = 0;            // 0: ring gauge on a 1 x 1 plane, 1: line along local -Z
uniform float fill = 0.0;        // ring: clockwise from the far side. line: from the drone outward.
uniform float inner = 0.8;       // ring band inner radius, fraction of the outer radius
uniform float graduations = 36.0;
uniform float span = 8.0;        // line length in metres
uniform float strength = 1.0;
uniform float dashed = 0.0;
varying vec2 local;

void vertex() {
	local = VERTEX.xz;
}

void fragment() {
	float alpha = 0.0;
	if (mode == 0) {
		float r = length(local) * 2.0;
		float t = fract(atan(local.x, -local.y) / TAU + 1.0);
		float aa = fwidth(r) * 1.5;
		float band = smoothstep(inner - aa, inner, r) * (1.0 - smoothstep(1.0 - aa, 1.0, r));
		float scale_band = smoothstep(inner - 0.13 - aa, inner - 0.13, r) * (1.0 - smoothstep(inner - 0.04, inner - 0.04 + aa, r));
		float major = step(fract(t * graduations / 4.0), 0.06);
		float minor = step(fract(t * graduations), 0.16);
		float lit = step(t, fill);
		float body = band * mix(0.2, 0.95, lit);
		if (dashed > 0.5) body = band * step(fract(t * 12.0), 0.5) * 0.85;
		alpha = max(body, scale_band * max(minor * 0.35, major * 0.8));
	} else {
		float x = abs(local.x) * 2.0;
		float along = 0.5 - local.y;
		float metres = along * span;
		float aa = fwidth(x) * 1.5;
		float core = 1.0 - smoothstep(0.22 - aa, 0.22, x);
		float marks = step(fract(metres / 0.5), 0.12) * (1.0 - smoothstep(0.85 - aa, 0.85, x));
		float lit = step(along, fill);
		alpha = max(core * mix(0.18, 0.9, lit), marks * mix(0.15, 0.6, lit));
		alpha *= 1.0 - smoothstep(0.82, 1.0, along);
	}
	ALBEDO = tint.rgb;
	ALPHA = clamp(alpha * strength * tint.a, 0.0, 1.0);
}
