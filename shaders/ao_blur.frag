#version 450
#extension GL_GOOGLE_include_directive : require
// The half-resolution values blurred along `dir` along their surface (ao.rs's BLUR_FS).
layout(push_constant) uniform PC { ivec2 size; float focal; float pad; ivec2 dir; } pc;
#include "post_common.glsl"
layout(set = 0, binding = 1) uniform sampler2D ao;
layout(location = 0) out vec4 o;
float Z(ivec2 h) { return G(h * 2).r; }
void main() {
    ivec2 p = ivec2(gl_FragCoord.xy);
    ivec2 hs = textureSize(ao, 0);
    vec2 c = texelFetch(ao, p, 0).rg;
    float z = Z(p);
    if (z <= 0.0) { o = vec4(c, 0.0, 1.0); return; }
    float iz = 1.0 / z;
    float za = Z(clamp(p + pc.dir, ivec2(0), hs - 1)), zb = Z(clamp(p - pc.dir, ivec2(0), hs - 1));
    float ga = za > 0.0 ? 1.0 / za - iz : 1e9, gb = zb > 0.0 ? iz - 1.0 / zb : 1e9;
    float g = abs(ga) < abs(gb) ? ga : gb;
    if (abs(g) > 1e8) g = 0.0;
    vec2 sum = c, wsum = vec2(1.0);
    for (int k = -6; k <= 6; k++) {
        ivec2 s = p + pc.dir * k;
        if (k == 0 || any(lessThan(s, ivec2(0))) || any(greaterThanEqual(s, hs))) continue;
        float zs = Z(s);
        if (zs <= 0.0) continue;
        float e = abs(1.0 / zs - (iz + g * float(k))) / iz;
        float kk = float(k * k);
        vec2 w = vec2(exp(-kk / 10.0), exp(-kk / 18.0)) * max(1.0 - e / 0.04, 0.0);
        sum += texelFetch(ao, s, 0).rg * w;
        wsum += w;
    }
    o = vec4(sum / wsum, 0.0, 1.0);
}
