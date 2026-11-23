// Shared by the Vulkan post passes (as ao.rs's COMMON and shadows.rs's GLSL for OpenGL): the
// depth attachment (multisampled with -DMS: sample 0) and view positions. Needs the pass's push
// constants `pc` with `size` (the target in pixels) and `focal` (pixels per unit at z = 1).
// Vulkan's y points down: view space here has y down too (its sun and lights flipped by the CPU).
#ifdef MS
layout(set = 0, binding = 0) uniform sampler2DMS gdepth;
#else
layout(set = 0, binding = 0) uniform sampler2D gdepth;
#endif
vec2 G(ivec2 q) {
    return texelFetch(gdepth, clamp(q, ivec2(0), pc.size - 1), 0).rg;
}
vec3 P(vec2 q, float z) {
    return vec3((q - vec2(pc.size) * 0.5) * z / pc.focal, z);
}
// one axis of the surface at q along e: from the side whose two pixels (e, 2e away) continue
// to q best (1/z is linear across a plane, so a line through them predicts q; not across an
// edge, also beside a second one); where q fits neither side (a pixel off the surface by a
// little, as decals' rims) but both sides fit each other, from both neighbours
vec3 T(ivec2 q, float z, vec3 p, ivec2 e) {
    float za = G(q + e).r, zb = G(q - e).r;
    float za2 = G(q + 2 * e).r, zb2 = G(q - 2 * e).r;
    if (za <= 0.0 && zb <= 0.0) return P(vec2(q + e), z) - p;
    if (zb <= 0.0) return P(vec2(q + e), za) - p;
    if (za <= 0.0) return p - P(vec2(q - e), zb);
    float iz = 1.0 / z;
    float pa = za2 > 0.0 ? 2.0 / za - 1.0 / za2 : 1.0 / za;
    float pb = zb2 > 0.0 ? 2.0 / zb - 1.0 / zb2 : 1.0 / zb;
    float ea = abs(pa - iz), eb = abs(pb - iz);
    if (min(ea, eb) > 0.002 * iz && abs(pa - pb) < 0.5 * min(ea, eb))
        return 0.5 * (P(vec2(q + e), za) - P(vec2(q - e), zb));
    return ea < eb ? P(vec2(q + e), za) - p : p - P(vec2(q - e), zb);
}
// the normal at q (depth z, place p) from the neighbours s pixels away, facing the camera
vec3 N(ivec2 q, float z, vec3 p, int s) {
    vec3 dx = T(q, z, p, ivec2(s, 0));
    vec3 dy = T(q, z, p, ivec2(0, s));
    vec3 n = cross(dx, dy);
    n = length(n) > 0.0 ? normalize(n) : vec3(0.0, 0.0, -1.0);
    return dot(n, p) > 0.0 ? -n : n;
}
// how much of the way from p (normal n) toward `dir` (view space, unit) over `len` units is
// blocked, 0...1; `j` a jitter 0...1; blockers past `fade` of the way block less toward its end
float shadow(vec3 p, vec3 n, vec3 dir, float len, float j, int steps, float fade) {
    float ndl = dot(n, dir);
    if (ndl <= 0.02) return 0.0;
    vec3 o = p + n * (0.004 * p.z + 4.0) + dir * (0.006 * p.z / max(ndl, 0.2));
    float thick = max(220.0, 0.02 * p.z);
    for (int i = 0; i < steps; i++) {
        float f = (float(i) + j) / float(steps);
        float t = len * f * f;
        vec3 s = o + dir * t;
        if (s.z <= 50.0) break;
        vec2 q = s.xy * pc.focal / s.z + vec2(pc.size) * 0.5;
        if (q.x < 0.0 || q.y < 0.0 || q.x >= float(pc.size.x) || q.y >= float(pc.size.y)) break;
        float zs = G(ivec2(q)).r;
        if (zs <= 0.0) continue;
        float dz = s.z - zs;
        if (dz > 0.003 * s.z + 2.0 && dz < thick) {
            return f < fade ? 1.0 : 1.0 - smoothstep(fade, 1.0, f);
        }
    }
    return 0.0;
}
