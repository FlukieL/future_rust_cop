#version 450
// glslc -O shaders/blit.frag -o shaders/blit_frag.spv
layout(push_constant) uniform P {
    vec4 rect;
    int words;
    float gamma;
} pc;
layout(set = 0, binding = 0) uniform sampler2D tex;
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 color;
void main() {
    vec4 c = texture(tex, uv);
    if (pc.words != 0) c = c.bgra;
    color = vec4(pow(c.rgb, vec3(1.0 / pc.gamma)), c.a);
}
