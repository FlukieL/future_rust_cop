#version 450
#extension GL_GOOGLE_include_directive : require
// The dynamic lights over the 3D picture (lights.rs's LIGHT_FS): per pixel the lights in reach,
// by the angle, their shadows by the screen-space march (the rays' bits not yet).
layout(push_constant) uniform PC {
    ivec2 size;
    float focal;
    int count;
    float strength;
    int show;       // 0 the light as a factor, 3 the light alone
    float added;    // this much of the light is added (dark surfaces get some too)
    int shadowed;   // the first this many lights cast shadows
} pc;
#include "post_common.glsl"
// row 0: place (view space, y down) and reach; row 1: colour
layout(set = 0, binding = 5) uniform sampler2D lights;
layout(location = 0) out vec4 o;
void main() {
    ivec2 q = ivec2(gl_FragCoord.xy);
    vec2 g = G(q);
    float z = g.r;
    vec3 sum = vec3(0.0);
    if (z > 0.0) {
        vec3 p = P(vec2(q), z);
        vec3 n = N(q, z, p, 1);
        float ign = fract(52.9829189 * fract(dot(gl_FragCoord.xy, vec2(0.06711056, 0.00583715))));
        for (int i = 0; i < pc.count; i++) {
            vec4 lp = texelFetch(lights, ivec2(i, 0), 0);
            vec3 l = lp.xyz - p;
            float d = length(l);
            float r = lp.w;
            if (d >= r) continue;
            float a = 1.0 - d / r;
            // less at its core: what the glow sits on (its robot) is not flooded
            a *= a * mix(0.3, 1.0, smoothstep(0.0, 0.2 * r, d));
            float ndl = d > 0.0 ? max(dot(n, l / d), 0.0) : 1.0;
            if (i < pc.shadowed && d > 0.1 * r) a *= 1.0 - shadow(p, n, l / d, d - 0.08 * r, ign, 16, 1.0);
            sum += texelFetch(lights, ivec2(i, 1), 0).rgb * a * (0.2 + 0.8 * ndl);
        }
        sum *= pc.strength * (1.0 - clamp(g.g, 0.0, 1.0));
    }
    o = vec4(sum * pc.added, 1.0);
}
