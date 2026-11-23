#version 450
#extension GL_GOOGLE_include_directive : require
// The picture times the AO and the sun's light (ao.rs's APPLY_FS; the shadow map's and the
// rays' sun not yet).
layout(push_constant) uniform PC {
    ivec2 size;
    float focal;
    float strength;
    int show;       // 0 the AO as a factor, 1 the depth, 2 the AO alone
    int use_half;   // the half-resolution pass ran
} pc;
#include "post_common.glsl"
layout(set = 0, binding = 1) uniform sampler2D ao;
layout(location = 0) out vec4 o;
void main() {
    ivec2 q = ivec2(gl_FragCoord.xy);
    vec2 g = G(q);
    float z = g.r;
    if (pc.show == 1) { o = vec4(vec3(z > 0.0 ? exp(-z / 6000.0) : 0.0), 1.0); return; }
    float v = 1.0;
    if (z > 0.0 && pc.use_half != 0) {
        float zr = G(q + ivec2(1, 0)).r, zl = G(q - ivec2(1, 0)).r;
        float zu = G(q + ivec2(0, 1)).r, zd = G(q - ivec2(0, 1)).r;
        float dr = zr > 0.0 ? abs(zr - z) : 1e9, dl = zl > 0.0 ? abs(zl - z) : 1e9;
        float du = zu > 0.0 ? abs(zu - z) : 1e9, dd = zd > 0.0 ? abs(zd - z) : 1e9;
        float tol = 0.02 * z + 2.0 * min(min(dr, dl) + min(du, dd), 0.1 * z);
        ivec2 hs = textureSize(ao, 0);
        vec2 h = (vec2(q) + 0.5) * 0.5 - 0.5;
        ivec2 b = ivec2(floor(h));
        vec2 f = h - floor(h);
        vec2 sum = vec2(0.0);
        float wsum = 0.0;
        for (int k = 0; k < 4; k++) {
            ivec2 o2 = ivec2(k & 1, k >> 1);
            ivec2 c = clamp(b + o2, ivec2(0), hs - 1);
            float zc = G(c * 2).r;
            float w = (o2.x == 1 ? f.x : 1.0 - f.x) * (o2.y == 1 ? f.y : 1.0 - f.y);
            w *= zc > 0.0 ? max(1.0 - abs(zc - z) / tol, 0.0) : 0.0;
            sum += texelFetch(ao, c, 0).rg * w;
            wsum += w;
        }
        vec2 a = sum / max(wsum, 1e-6);
        if (wsum < 1e-4) {
            float best = 1e30;
            for (int j = -1; j <= 2; j++)
                for (int i = -1; i <= 2; i++) {
                    ivec2 c = clamp(b + ivec2(i, j), ivec2(0), hs - 1);
                    float zc = G(c * 2).r;
                    float d = zc > 0.0 ? abs(zc - z) : 1e29;
                    if (d < best) { best = d; a = texelFetch(ao, c, 0).rg; }
                }
            if (best >= 1e29) a = vec2(1.0);
        }
        v = a.r * a.g;
    }
    if (pc.show == 2) { o = vec4(vec3(v), 1.0); return; }
    o = vec4(vec3(mix(1.0, v, pc.strength * (1.0 - clamp(g.g, 0.0, 1.0)))), 1.0);
}
