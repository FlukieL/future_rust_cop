#version 450
#extension GL_GOOGLE_include_directive : require
// The AO (r) and the screen-space sun's light (g) at half resolution (ao.rs's AO_FS; the shadow
// map's sun not yet).
layout(push_constant) uniform PC {
    ivec2 size;
    float focal;
    float radius;
    float max_px;
    int use_ao;
    int sun_mode;   // the sun's shadow here: 0 none, 1 screen-space
    float sun_len;
    vec4 sun;       // toward the sun, view space (y down), w: the shadow's strength
    float step_h;
} pc;
#include "post_common.glsl"
layout(set = 0, binding = 3) uniform sampler2D near_z;
layout(location = 0) out vec4 o;
const int SAMPLES = 12;
void main() {
    ivec2 q = ivec2(gl_FragCoord.xy) * 2;
    float z = G(q).r;
    if (z <= 0.0) { o = vec4(1.0); return; }
    vec3 p = P(vec2(q), z);
    vec3 n = N(q, z, p, 2);
    float ign = fract(52.9829189 * fract(dot(gl_FragCoord.xy, vec2(0.06711056, 0.00583715))));
    float lit = pc.sun_mode == 1 ? 1.0 - pc.sun.w * shadow(p, n, pc.sun.xyz, pc.sun_len, ign, 16, 0.6) : 1.0;
    float rpx = min(pc.radius * pc.focal / z, pc.max_px);
    if (pc.use_ao == 0 || rpx < 2.0) { o = vec4(1.0, lit, 0.0, 1.0); return; }
    float a = 6.2831853 * ign;
    float occ = 0.0;
    float step_h = pc.step_h + 0.004 * z;
    for (int i = 0; i < SAMPLES; i++) {
        float t = (float(i) + 0.5) / float(SAMPLES);
        float ang = a + float(i) * 2.3999632;
        vec2 s = vec2(q) + vec2(cos(ang), sin(ang)) * t * rpx;
        float zs = texelFetch(near_z, ivec2(s) / 4, 0).r;
        if (zs <= 0.0) continue;
        vec3 v = P(s, zs) - p;
        float d = length(v);
        if (d < 1e-3) continue;
        occ += max((dot(v, n) - step_h) / d - 0.05, 0.0) * (1.0 - smoothstep(0.5 * pc.radius, pc.radius, d));
    }
    o = vec4(pow(clamp(1.0 - 4.0 * occ / float(SAMPLES), 0.0, 1.0), 2.0), lit, 0.0, 1.0);
}
