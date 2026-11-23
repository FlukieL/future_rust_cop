#version 450
// Glide's colour and alpha combine units, chroma key and texture filtering (`vglide.rs`), as
// glide.rs's FS for OpenGL (lit smoke not yet: PLAN-RTX.md step 6).
// glslc -O shaders/glide.frag -o shaders/glide_frag.spv
layout(push_constant) uniform P {
    vec2 space;
    vec2 jitter;
    vec4 key;
    ivec2 clamp_st;
    int mipmaps;        // filtered by the GPU with mipmaps
    int chroma;
    int linear_filter;
    int edge;           // how the chroma key's soft edge is applied (see `flush`)
    int gmode;          // the depth attachment: 0 not written, 1 depth, 2/3 mask (blended/additive)
    int ztest;
    float zbias;
    int zonly;          // the depth buffer's own pass: 1 everything, 2 where alpha >= 0.75
    int show_effects;   // FCOP_GBUFFER=effects: each kind in a colour of its own
    int pad;
} pc;
layout(set = 0, binding = 0) uniform sampler2DArray tex;     // rgb * coverage, coverage
layout(set = 0, binding = 1) uniform sampler2DArray tex_a;   // alpha * coverage, alpha * (1 - coverage)
layout(set = 0, binding = 2) uniform sampler2DArray tex_hd;  // replacements: rgb * coverage, coverage, larger
layout(location = 0) in vec4 vcol;
layout(location = 1) noperspective in float vinvz;
layout(location = 2) noperspective in vec2 st_affine;
layout(location = 3) noperspective in vec3 stq;
layout(location = 4) flat in vec4 konst;    // the constant colour
layout(location = 5) flat in uvec4 vmode;   // combine units and texture, packed by `add`
layout(location = 6) flat in vec4 st_box;   // the primitive's texture rectangle
layout(location = 7) flat in uint veffect;  // what effect it is (`effects::Effect`), 0 none
layout(location = 0) out vec4 color;
layout(location = 1) out vec4 gbuf;

vec2 st;               // the texture coordinate: affine or perspective-correct (`vmode.z` bit 3)
ivec4 cc;              // colour combine: function, factor, local, other
int cc_inv;
ivec4 ac;              // alpha combine
int ac_inv;
int use_tex;
int layer;
int hd_layer;          // the replacement's layer, -1: none
ivec2 tex_size;

ivec4 bytes4(uint w) { return ivec4(w & 255u, (w >> 8) & 255u, (w >> 16) & 255u, w >> 24); }

vec4 texel(ivec2 p) {
    ivec2 s = tex_size;
    p = ivec2(pc.clamp_st.x != 0 ? clamp(p.x, 0, s.x - 1) : ((p.x % s.x) + s.x) % s.x,
              pc.clamp_st.y != 0 ? clamp(p.y, 0, s.y - 1) : ((p.y % s.y) + s.y) % s.y);
    vec4 a = texelFetch(tex_a, ivec3(p, layer), 0);
    return vec4(texelFetch(tex, ivec3(p, layer), 0).rgb, a.r + a.g);
}

// The GPU's trilinear (and anisotropic) filter: inside the primitive's texture rectangle,
// at most at the level where that still spans 4 texels (a page is an atlas). A replacement
// gives the colour and coverage; the texel alpha stays the original's.
vec4 sample_mip(out float cover) {
    vec2 size = vec2(textureSize(tex, 0).xy);
    vec2 dx = dFdx(st), dy = dFdy(st);
    vec2 lo = st_box.xy, hi = st_box.zw;
    float lod = log2(max(max(length(dx), length(dy)), 1e-8));
    float top = max(log2(max(min(hi.x - lo.x, hi.y - lo.y), 1.0)) - 2.0, 0.0);
    if (lod > top) {
        float k = exp2(top - lod);
        dx *= k;
        dy *= k;
        lod = top;
    }
    vec2 m = vec2(0.5 * exp2(max(lod, 0.0)));
    vec2 p = clamp(st, lo + min(m, (hi - lo) * 0.5), hi - min(m, (hi - lo) * 0.5));
    vec3 uv = vec3(p / size, float(layer));
    vec4 c = textureGrad(tex, uv, dx / size, dy / size);
    vec4 a = textureGrad(tex_a, uv, dx / size, dy / size);
    float ta = c.a > 0.0 ? a.r / c.a : 0.0;
    if (hd_layer >= 0) c = textureGrad(tex_hd, vec3(uv.xy, float(hd_layer)), dx / size, dy / size);
    if (pc.chroma == 0) {
        cover = 1.0;
        return vec4(c.rgb, a.r + a.g);
    }
    cover = c.a;
    return c.a > 0.0 ? vec4(c.rgb / c.a, ta) : vec4(0.0);
}

bool keyed(vec3 c) { return pc.chroma != 0 && all(lessThan(abs(c - pc.key.rgb), vec3(0.5 / 255.0))); }

// the texture colour and how much of it is not keyed out
vec4 sample_tex(out float cover) {
    if (hd_layer >= 0 || pc.linear_filter != 0 && pc.mipmaps != 0) return sample_mip(cover);
    if (pc.linear_filter == 0) {
        vec4 t = texel(ivec2(floor(st)));
        cover = keyed(t.rgb) ? 0.0 : 1.0;
        return t;
    }
    vec2 p = st - 0.5;
    ivec2 i = ivec2(floor(p));
    vec2 f = p - floor(p);
    vec4 sum = vec4(0.0);
    float wsum = 0.0;
    for (int k = 0; k < 4; k++) {
        ivec2 o = ivec2(k & 1, k >> 1);
        vec4 t = texel(i + o);
        float w = (o.x == 1 ? f.x : 1.0 - f.x) * (o.y == 1 ? f.y : 1.0 - f.y);
        if (keyed(t.rgb)) w = 0.0;
        sum += t * w;
        wsum += w;
    }
    cover = wsum;
    return wsum > 0.0 ? sum / wsum : vec4(0.0);
}

vec3 factor3(int f, vec3 local, float local_a, float other_a, float tex_a) {
    if (f == 0) return vec3(0.0);
    if (f == 1) return local;
    if (f == 2) return vec3(other_a);
    if (f == 3) return vec3(local_a);
    if (f == 4) return vec3(tex_a);
    if (f == 8) return vec3(1.0);
    if (f == 9) return 1.0 - local;
    if (f == 10) return vec3(1.0 - other_a);
    if (f == 11) return vec3(1.0 - local_a);
    if (f == 12) return vec3(1.0 - tex_a);
    return vec3(1.0);
}

vec3 combine3(int fn_, vec3 f, vec3 local, float local_a, vec3 other) {
    if (fn_ == 0) return vec3(0.0);
    if (fn_ == 1) return local;
    if (fn_ == 2) return vec3(local_a);
    if (fn_ == 3) return f * other;
    if (fn_ == 4) return f * other + local;
    if (fn_ == 5) return f * other + local_a;
    if (fn_ == 6) return f * (other - local);
    if (fn_ == 7) return f * (other - local) + local;
    if (fn_ == 8) return f * (other - local) + local_a;
    if (fn_ == 9) return -f * local + local;
    if (fn_ == 16) return -f * local + local_a;
    return local;
}

void main() {
    cc = bytes4(vmode.x);
    ac = bytes4(vmode.y);
    cc_inv = int(vmode.z & 1u);
    ac_inv = int((vmode.z >> 1) & 1u);
    use_tex = int((vmode.z >> 2) & 1u);
    st = (vmode.z & 8u) != 0u && stq.z > 0.0 ? stq.xy / stq.z : st_affine;
    layer = int((vmode.z >> 8) & 255u);
    hd_layer = int(vmode.z >> 16) - 1;
    tex_size = ivec2(vmode.w & 0xffffu, vmode.w >> 16);
    float cover = 1.0;
    vec4 t = use_tex != 0 ? sample_tex(cover) : vec4(1.0);
    // alpha first: colour factors may use it
    float a_local = ac.z == 0 ? vcol.a : konst.a;
    float a_other = ac.w == 0 ? vcol.a : ac.w == 1 ? t.a : konst.a;
    float af = factor3(ac.y, vec3(a_local), a_local, a_other, t.a).x;
    float a = clamp(combine3(ac.x, vec3(af), vec3(a_local), a_local, vec3(a_other)).x, 0.0, 1.0);
    if (ac_inv != 0) a = 1.0 - a;
    vec3 c_local = cc.z == 0 ? vcol.rgb : konst.rgb;
    vec3 c_other = cc.w == 0 ? vcol.rgb : cc.w == 1 ? t.rgb : konst.rgb;
    // the chroma key compares the colour unit's "other" input
    if (cc.w != 1 && keyed(c_other)) discard;
    if (cc.w != 1) cover = 1.0;
    vec3 cf = factor3(cc.y, c_local, a_local, a_other, t.a);
    vec3 c = clamp(combine3(cc.x, cf, c_local, a_local, c_other), 0.0, 1.0);
    if (cc_inv != 0) c = 1.0 - c;
    if (pc.edge == 0 && cover < 0.5) discard;
    if (pc.edge == 1) a = cover;
    if (pc.edge == 2) a *= cover;
    if (pc.edge == 3) c *= cover;
    if (cover <= 0.0) discard;
    if (pc.zonly != 0) {
        // translucent surfaces (the PlayStation's semi-transparency, alpha 0x80: liquids) write
        // none: what dips into them stays visible, as the game draws it after them
        if (pc.zonly == 2 && a < 0.75) discard;
        color = vec4(0.0);
        gbuf = vec4(0.0);
        return;
    }
    color = vec4(c, a);
    if (pc.show_effects != 0) {
        const vec3 kinds[8] = vec3[8](vec3(0.0), vec3(0.6), vec3(1.0, 0.4, 0.0), vec3(1.0, 1.0, 0.0),
            vec3(1.0, 0.0, 1.0), vec3(0.0, 1.0, 1.0), vec3(0.0, 0.3, 1.0), vec3(0.0, 1.0, 0.0));
        float grey = dot(color.rgb, vec3(0.3, 0.5, 0.2)) * 0.35;
        color.rgb = veffect == 0u ? vec3(grey) : kinds[min(veffect, 7u)];
    }
    if (pc.gmode == 1) gbuf = vec4(vinvz > 0.0 ? 1.0 / vinvz : 0.0, 0.0, 0.0, step(0.5, a));
    else gbuf = vec4(0.0, clamp(2.0 * (pc.gmode == 3 ? max(c.r, max(c.g, c.b)) : a), 0.0, 1.0), 0.0, 0.0);
}
