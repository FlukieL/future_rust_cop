#version 450
// The composition (`vkr.rs`): a picture as a rectangle of the window image.
// glslc -O shaders/blit.vert -o shaders/blit_vert.spv
layout(push_constant) uniform P {
    vec4 rect;    // x0 y0 x1 y1 in clip space (y down: y0 the top)
    int words;    // 2D frames hold 0RGB/ARGB words: their bytes are B G R A
    float gamma;
} pc;
layout(location = 0) out vec2 uv;
void main() {
    vec2 c = vec2(gl_VertexIndex & 1, gl_VertexIndex >> 1);
    uv = c;
    gl_Position = vec4(mix(pc.rect.xy, pc.rect.zw, c), 0.0, 1.0);
}
