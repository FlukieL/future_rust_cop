#version 450
#extension GL_GOOGLE_include_directive : require
// The nearest depth of each 4x4 pixels (none: 0), the AO's occluders (ao.rs's NEAR_FS).
layout(push_constant) uniform PC { ivec2 size; float focal; } pc;
#include "post_common.glsl"
layout(location = 0) out vec4 o;
void main() {
    ivec2 b = ivec2(gl_FragCoord.xy) * 4;
    float m = 1e30;
    for (int j = 0; j < 4; j++)
        for (int i = 0; i < 4; i++) {
            float z = G(b + ivec2(i, j)).r;
            if (z > 0.0) m = min(m, z);
        }
    o = vec4(m < 1e30 ? m : 0.0);
}
