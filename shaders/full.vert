#version 450
// A triangle over the whole target, for the post passes (`vpost.rs`).
// glslc -O shaders/full.vert -o shaders/full_vert.spv
void main() {
    vec2 p = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
