#version 450
// Glide's primitives (`vglide.rs`), as glide.rs's VS for OpenGL; Vulkan's y points down.
// glslc -O shaders/glide.vert -o shaders/glide_vert.spv
layout(push_constant) uniform P {
    vec2 space;
    vec2 jitter;        // the 3D picture's subpixel offset (`jitter.rs`), clip space (GL's: y up)
    vec4 key;
    ivec2 clamp_st;
    int mipmaps;
    int chroma;
    int linear_filter;
    int edge;
    int gmode;
    int ztest;          // the depth buffer (`zbuffer.rs`): 1 the depth from z
    float zbias;        // pulled nearer by this share
    int zonly;
    int show_effects;
    int pad;
} pc;
invariant gl_Position;
layout(location = 0) in vec2 pos;
layout(location = 1) in vec4 col;
layout(location = 2) in vec2 tc;
layout(location = 3) in vec4 kc;
layout(location = 4) in uvec4 mode;
layout(location = 5) in vec4 box;
layout(location = 6) in float z;
layout(location = 7) in uint effect;
layout(location = 0) out vec4 vcol;
layout(location = 1) noperspective out float vinvz;
layout(location = 2) noperspective out vec2 st_affine;
layout(location = 3) noperspective out vec3 stq;
layout(location = 4) flat out vec4 konst;
layout(location = 5) flat out uvec4 vmode;
layout(location = 6) flat out vec4 st_box;
layout(location = 7) flat out uint veffect;
void main() {
    veffect = effect;
    vcol = col / 255.0;
    konst = kc;
    st_box = box;
    vmode = mode;
    st_affine = tc;
    vinvz = z > 0.0 ? 1.0 / z : 0.0;
    // perspective-correct: s/z, t/z, 1/z interpolated linearly on the screen
    stq = vec3(tc * vinvz, vinvz);
    // 1 - NEAR / z: linear in 1/z, so exact across a plane on the screen (w is 1)
    float d = pc.ztest != 0 && z > 0.0 ? clamp(1.0 - 8.0 / (z * (1.0 - pc.zbias)), 0.0, 1.0) : 0.5;
    gl_Position = vec4(pos.x / pc.space.x * 2.0 - 1.0 + pc.jitter.x, pos.y / pc.space.y * 2.0 - 1.0 - pc.jitter.y, d, 1.0);
    gl_PointSize = 1.0;
}
