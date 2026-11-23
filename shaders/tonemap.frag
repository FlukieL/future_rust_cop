#version 450
// Tone mapping (`tonemap.rs`; Vulkan: `vpost.rs`), as tonemap.rs's FS for OpenGL.
// glslc -O shaders/tonemap.frag -o shaders/tonemap_frag.spv
layout(push_constant) uniform P {
    int curve;
    float knee;
} pc;
layout(set = 0, binding = 0) uniform sampler2D picture;
layout(location = 0) out vec4 o;

vec3 soft(vec3 c) {
    // on the largest channel, the others scaled with it: the hue is kept
    float m = max(max(c.r, c.g), c.b);
    if (m <= pc.knee) return c;
    float s = pc.knee + (1.0 - pc.knee) * (1.0 - exp(-(m - pc.knee) / (1.0 - pc.knee)));
    return c * (s / m);
}

// AgX's base transform (no look): inset, log2 encoding, sigmoid, outset
vec3 agx(vec3 c) {
    const mat3 inset = mat3(0.842479062253094, 0.0423282422610123, 0.0423756549057051,
                            0.0784335999999992, 0.878468636469772, 0.0784336,
                            0.0792237451477643, 0.0791661274605434, 0.879142973793104);
    const mat3 outset = mat3(1.19687900512017, -0.0528968517574562, -0.0529716355144438,
                             -0.0980208811401368, 1.15190312990417, -0.0980434501171241,
                             -0.0990297440797205, -0.0989611768448433, 1.15107367264116);
    const float lo = -12.47393;
    const float hi = 4.026069;
    vec3 x = inset * pow(max(c, 0.0), vec3(2.2));
    x = (clamp(log2(max(x, 1e-10)), lo, hi) - lo) / (hi - lo);
    vec3 x2 = x * x;
    vec3 x4 = x2 * x2;
    x = 15.5 * x4 * x2 - 40.14 * x4 * x + 31.96 * x4 - 6.868 * x2 * x + 0.4298 * x2 + 0.1191 * x - 0.00232;
    return clamp(outset * x, 0.0, 1.0);
}

// ACES, Narkowicz's fit (with his exposure of 0.6)
vec3 aces(vec3 c) {
    vec3 x = pow(max(c, 0.0), vec3(2.2)) * 0.6;
    x = clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), 0.0, 1.0);
    return pow(x, vec3(1.0 / 2.2));
}

void main() {
    vec4 c = texelFetch(picture, ivec2(gl_FragCoord.xy), 0);
    vec3 r = pc.curve == 1 ? soft(c.rgb) : pc.curve == 2 ? agx(c.rgb) : aces(c.rgb);
    o = vec4(r, c.a);
}
