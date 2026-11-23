//! The game's 3dfx Glide 2.x renderer on OpenGL. The game loads glide2x.dll itself and takes
//! its functions with GetProcAddress (`proc_address`): every name gets a thunk, the decorated
//! name (`_grDrawTriangle@12`) gives the stdcall argument size.
//!
//! The game draws its depth-sorted triangles with Glide's colour and alpha combine units, alpha
//! blending and a chroma key (black texels are not drawn), textures 256x256 ARGB1555 in TMU
//! memory, no depth buffer. We draw into a texture as tall as the window or the resolution
//! chosen in Preferences (`display.rs`; high resolution:
//! the Glide screen of `grSstWinOpen` only gives the coordinates; the game opens 1280x1024
//! for its 1280x960 mode and stretches its 4:3 picture to it) and show it at `grBufferSwap`. Vertices are in Glide screen pixels, or in the space
//! the display list walker sets (`set_space`, see `walker.rs`); clip windows are always in
//! screen pixels. Widescreen (`wide.rs`): the texture takes the window's aspect, the 3D
//! picture fills it and the HUD is drawn into 4:3 parts of it (`set_view`). With filtering on, the chroma key leaves the keyed texels
//! out of the bilinear filter, and their share becomes a soft alpha edge.
//!
//! Textures are kept premultiplied by their coverage (the key is black, as the game always
//! sets it): rgb and coverage in `array`, the texture's alpha (covered and keyed share) in
//! `alpha`. Filtered by the GPU that is the keyed bilinear filter above, and it also gives
//! mipmaps and anisotropic filtering without dark fringes (`filtering`: REG_DWORD
//! `TextureFilter`). A page is an atlas of small pictures: a primitive samples inside the
//! rectangle of its texture coordinates, at a level that still has a few texels of it.

use super::gl::{compile, texture};
use super::{arg, log, mem, thunk, thunk_not_implemented};
use crate::rt::{Cpu, EDX};
use glow::HasContext;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};

/// The module handle LoadLibraryA gives for glide2x.dll.
pub const MODULE: u32 = 0x1000_0000;

/// TMU memory reported: 4 MB.
const TMU_RAM: u32 = 4 << 20;

#[derive(Clone, Copy, PartialEq, Debug)]
struct State {
    /// rgb source and destination factor
    blend: (u32, u32),
    chroma: bool,
    key: u32,
    linear: bool,
    clamp: (bool, bool),
    clip: [i32; 4],
}

/// Where the following drawing goes in the frame.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum View {
    /// the whole frame
    Full,
    /// a 4:3 part: 0 at the left edge, 0.5 centred, 1 at the right edge
    Hud(f32),
    /// split screen: a 4:3 HUD whose left (or right) half is centred in the left (right)
    /// player's half of the frame, and cut to it
    Half(bool),
}

/// A texture in TMU memory: a layer of the texture array.
struct Texture {
    layer: u32,
    w: u32,
    h: u32,
    /// its size in TMU memory
    bytes: u32,
    /// when it was last downloaded (a count)
    stamp: u64,
    /// its layer in the replacements' array, when parts of it are replaced (`hd.rs`)
    hd: Option<u32>,
    /// its texels' colour (rgb, black where keyed out), for the dynamic lights' colours
    rgb: Vec<u8>,
    /// `average`'s results by texture rectangle, until the next download
    means: std::cell::RefCell<std::collections::HashMap<[u32; 4], [f32; 3]>>,
}

/// Layers of the texture array: TMU memory holds 64 textures of 256x256 at 8 bits.
pub(super) const LAYERS: u32 = 64;
/// Levels of a 256x256 texture: 256 ... 1.
pub(super) const LEVELS: i32 = 9;
/// Layers of the replacements' array.
pub(super) const HD_LAYERS: u32 = 32;

/// The replacement textures: pages at `scale` times the size, rgb premultiplied by the
/// coverage and the coverage (as `array`), with mipmaps; framebuffers to copy between layers.
struct Hd {
    array: glow::Texture,
    scale: u32,
    stale: bool,
    read: glow::Framebuffer,
    draw: glow::Framebuffer,
}

/// Texture filtering when the game filters: 0 the original (bilinear, no mipmaps),
/// 1 trilinear mipmaps, 2...16 also anisotropic.
pub const FILTER_CHOICES: &[u32] = &[0, 1, 2, 4, 8, 16];
static FILTER: AtomicU32 = AtomicU32::new(16);

/// Perspective-correct textures (PLAN.md 2a step 0): the game's are affine, as on the
/// PlayStation (every vertex oow = 1.0, 0x439456). With each vertex's view-space z (`ao.rs`)
/// the shader interpolates s/z, t/z and 1/z instead, for primitives of the 3D picture whose
/// vertices are all projected points (a clipped vertex's z is borrowed: it would warp).
/// REG_DWORD `PerspectiveTextures` (default off: the warping is part of the PS1 look),
/// `FCOP_PERSPECTIVE=0/1`; in the Advanced Graphics dialog.
static PERSPECTIVE: super::light::Setting = super::light::Setting::with_default("PerspectiveTextures", "FCOP_PERSPECTIVE", false);

pub fn perspective() -> bool {
    PERSPECTIVE.get()
}

/// The Advanced Graphics dialog's OK: saved, and used from the next frame on.
pub fn set_perspective(on: bool) {
    PERSPECTIVE.set(on);
}

/// The saved choice (default: anisotropic 16x); `FCOP_FILTER=<n>` overrides it.
pub fn filtering() -> u32 {
    if let Some(n) = std::env::var("FCOP_FILTER").ok().and_then(|v| v.parse().ok()) {
        return n;
    }
    let v = super::advapi32::value("", "TextureFilter").and_then(|(_, d)| Some(u32::from_le_bytes(d.get(..4)?.try_into().ok()?)));
    v.filter(|v| FILTER_CHOICES.contains(v)).unwrap_or(16)
}

/// Multisample anti-aliasing: samples per pixel (0 off).
pub const MSAA_CHOICES: &[u32] = &[0, 2, 4, 8];
static MSAA: AtomicU32 = AtomicU32::new(4);

/// The saved choice (REG_DWORD `Msaa`, default 4x); `FCOP_MSAA=<n>` overrides it.
pub fn msaa() -> u32 {
    if let Some(n) = std::env::var("FCOP_MSAA").ok().and_then(|v| v.parse().ok()) {
        return n;
    }
    let v = super::advapi32::value("", "Msaa").and_then(|(_, d)| Some(u32::from_le_bytes(d.get(..4)?.try_into().ok()?)));
    v.filter(|v| MSAA_CHOICES.contains(v)).unwrap_or(4)
}

/// Preferences' OK: saved, and used from the next frame on.
pub fn set_msaa(n: u32) {
    super::advapi32::set_value("", "Msaa", 4, &n.to_le_bytes());
    MSAA.store(n, Ordering::Relaxed);
}

/// At start, after the EXE is loaded.
pub fn init() {
    FILTER.store(filtering(), Ordering::Relaxed);
    MSAA.store(msaa(), Ordering::Relaxed);
}

/// Preferences' OK: saved, and used from the next batch on.
pub fn set_filtering(n: u32) {
    super::advapi32::set_value("", "TextureFilter", 4, &n.to_le_bytes());
    FILTER.store(n, Ordering::Relaxed);
}

struct Glide {
    /// the Glide screen
    res: (u32, u32),
    /// the texture drawn into
    target: (u32, u32),
    /// the target was made in RGBA16F (`colour_format`)
    target_hdr: bool,
    /// the vertices' coordinate space
    space: (u32, u32),
    view: View,
    /// what draws: OpenGL or the Vulkan renderer (`vglide.rs`)
    back: Back,
    textures: BTreeMap<u32, Texture>,
    downloads: u64,
    state: State,
    /// Vertex attributes, so that changing them draws nothing yet: the colour and alpha
    /// combine units (function, factor, local, other, invert), the constant colour (ARGB) and
    /// the TMU address of the texture
    cc: [u32; 5],
    ac: [u32; 5],
    constant: u32,
    tex: Option<u32>,
    /// vertices waiting to be drawn with `state` (`VERTEX` words each): x y r g b a s t,
    /// constant r g b a (floats), the combine units and texture (see `add`), then the
    /// rectangle of the primitive's texture coordinates s0 t0 s1 t1 (floats)
    batch: Vec<u32>,
    batch_mode: u32,
    /// how the batch's primitives meet the depth buffer (`zbuffer.rs`): 0 not, 1 tested,
    /// 2 tested and (when solid) written: every corner has its own depth
    batch_z: u8,
    gamma: f32,
    /// the 3D picture is drawn (`Some(dx)`: the walker says when, and the x its view adds to
    /// the units): it writes the depth attachment
    gbuf: Option<f32>,
    /// the sun's shadow (`shadows.rs`): this view's casters (batched vertices)
    casters: Vec<u32>,
    /// the primitives that follow only cast (`shadows::cast_kind`): 0 no, 1 yes, 2 the ring's
    /// (their corners zoomed out by `shadows::RING_ZOOM` about the view's centre)
    cast_only: u8,
    /// the dynamic lights of the view being drawn (`lights.rs`)
    lights: Vec<super::lights::Light>,
    /// `add`'s vertices' depths, kept between primitives
    zbuf: Vec<f32>,
    /// each view's (scissor box) last merged lights (`lpos`, `lcol`): its smoke is lit by them
    smoke_lights: Vec<([i32; 4], Vec<f32>, Vec<f32>)>,
    /// the heat shimmer's sources of the view being drawn (`heat.rs`)
    heat_sources: Vec<super::heat::Source>,
}

/// What draws Glide's pictures.
enum Back {
    Gl(Box<GlBack>),
    Vk(Box<super::vglide::Gpu>),
}

/// The context the backend draws with.
pub(super) enum Ctx<'a> {
    Gl(&'a glow::Context),
    Vk(&'a mut super::vkr::Vkr),
}

/// OpenGL's objects: the target, its multisampled framebuffer, the depth attachment, the
/// program, the vertex buffer, the texture arrays and the post passes.
struct GlBack {
    /// the texture shown, and its framebuffer
    fbo: glow::Framebuffer,
    color: glow::Texture,
    /// with MSAA: the multisampled framebuffer drawn into, its renderbuffer and samples;
    /// resolved into `color` at grBufferSwap
    ms: Option<(glow::Framebuffer, glow::Renderbuffer, u32)>,
    ms_max: u32,
    prog: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    /// where the next batch goes in `vbo` (bytes): batches are appended, the buffer is only
    /// made anew (orphaned) when full, so the driver does not allocate at every flush
    vbo_at: usize,
    /// every texture, 256x256 at most, in one array: rgb premultiplied by coverage and the
    /// coverage, with mipmaps (see the module)
    array: glow::Texture,
    /// the textures' alpha: its share in covered texels, and in keyed ones
    alpha: glow::Texture,
    /// a download changed level 0: the mipmaps are built before the next draw
    stale: bool,
    /// replacement textures, when there are any
    hd: Option<Hd>,
    /// the anisotropy set on the arrays, and the most the driver takes
    aniso: f32,
    aniso_max: f32,
    /// uniform locations
    uniforms: Uniforms,
    /// ambient occlusion (`ao.rs`): view-space depth and a mask, a second attachment of the
    /// framebuffer drawn into, written by the 3D picture; the passes
    gdepth: Option<Gdepth>,
    ssao: Option<super::ao::Ssao>,
    shadow_map: Option<super::shadows::ShadowMap>,
    lighting: Option<super::lights::Lighting>,
    heat: Option<super::heat::Heat>,
    bloom: Option<super::bloom::Bloom>,
    motion: Option<super::motion::Motion>,
    tonemap: Option<super::tonemap::Pass>,
}

/// A batch's drawing as `flush` decided it, for either backend.
pub(super) struct Draw<'a> {
    /// glow::TRIANGLES or glow::POINTS
    pub topology: u32,
    pub target: (u32, u32),
    /// the view in the target: x, y from the top, w, h
    pub viewport: [f32; 4],
    /// the clip window's box: x, y from below (GL's), w, h
    pub scissor: [i32; 4],
    pub space: [f32; 2],
    /// in clip space (GL's: y up)
    pub jitter: [f32; 2],
    pub mipmaps: bool,
    /// anisotropic filtering wanted (1 none)
    pub aniso: f32,
    pub clamp: [bool; 2],
    pub chroma: bool,
    pub key: [f32; 3],
    pub linear: bool,
    /// blending: None off, else GL's source and destination factors
    pub blend: Option<(u32, u32)>,
    pub edge: i32,
    pub gmode: i32,
    /// the depth buffer: 0 not, 1 tested, 2 written by a depth pass first
    pub zk: u8,
    pub zwrite: bool,
    /// the depth pass' kind (`zonly`: 1 everything, 2 where alpha >= 0.75)
    pub zonly: i32,
    pub show_effects: bool,
    /// lit smoke: lights' places and reach, colours, focal length
    pub smoke: Option<(&'a [f32], &'a [f32], f32)>,
}

/// The depth attachment, and a framebuffer with only the colour of the one drawn into (the
/// AO is applied through it: the depth is read meanwhile).
struct Gdepth {
    tex: glow::Texture,
    ms: bool,
    colour: glow::Framebuffer,
    /// the depth buffer (`zbuffer.rs`), as many samples as the picture
    zrb: glow::Renderbuffer,
}

thread_local! {
    static GLIDE: RefCell<Option<Glide>> = const { RefCell::new(None) };
}

const VS: &str = r#"
uniform vec2 space;
uniform vec2 jitter;    // the 3D picture's subpixel offset (`jitter.rs`), in clip space
uniform int ztest;      // the depth buffer (`zbuffer.rs`): 1 the depth from z
uniform float zbias;    // pulled nearer by this share
invariant gl_Position;
layout(location = 0) in vec2 pos;
layout(location = 1) in vec4 col;
layout(location = 2) in vec2 tc;
layout(location = 3) in vec4 kc;
layout(location = 4) in uvec4 mode;
layout(location = 5) in vec4 box;
layout(location = 6) in float z;
layout(location = 7) in uint effect;
out vec4 vcol;
noperspective out float vinvz;
noperspective out vec2 st_affine;
noperspective out vec3 stq;
flat out vec4 konst;
flat out uvec4 vmode;
flat out vec4 st_box;
flat out uint veffect;
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
    float d = ztest != 0 && z > 0.0 ? clamp(1.0 - 8.0 / (z * (1.0 - zbias)), 0.0, 1.0) : 0.5;
    gl_Position = vec4(pos.x / space.x * 2.0 - 1.0 + jitter.x, 1.0 - pos.y / space.y * 2.0 + jitter.y, 2.0 * d - 1.0, 1.0);
}
"#;

const FS: &str = r#"
uniform sampler2DArray tex;     // rgb * coverage, coverage
uniform sampler2DArray tex_a;   // alpha * coverage, alpha * (1 - coverage)
uniform sampler2DArray tex_hd;  // replacements: rgb * coverage, coverage, larger
uniform int mipmaps;            // filtered by the GPU with mipmaps
uniform ivec2 clamp_st;
uniform int chroma;
uniform vec3 key;
uniform int linear_filter;
uniform int edge;      // how the chroma key's soft edge is applied (see `flush`)
uniform int gmode;     // the depth attachment: 0 not written, 1 depth, 2/3 mask (blended/additive)
uniform int zonly;     // the depth buffer's own pass: 1 everything, 2 where alpha >= 0.75
in vec4 vcol;
noperspective in float vinvz;
noperspective in vec2 st_affine;
noperspective in vec3 stq;
vec2 st;               // the texture coordinate: affine or perspective-correct (`vmode.z` bit 3)
flat in vec4 konst;    // the constant colour
flat in uvec4 vmode;   // combine units and texture, packed by `add`
flat in vec4 st_box;   // the primitive's texture rectangle
flat in uint veffect;  // what effect it is (`effects::Effect`), 0 none
uniform int show_effects;  // FCOP_GBUFFER=effects: each kind in a colour of its own
// lit smoke (`lights.rs`): the dynamic lights of this view's last picture, as its pass has them
const int SMOKE_MAX = 32;
uniform int smoke_count;
uniform vec4 smoke_lpos[SMOKE_MAX];  // view space, reach
uniform vec3 smoke_lcol[SMOKE_MAX];
uniform vec2 smoke_size;   // the target in pixels
uniform float smoke_focal; // pixels per unit at z = 1
uniform float smoke_k;     // the smoke times 1 + this much of the light
layout(location = 0) out vec4 color;
layout(location = 1) out vec4 gbuf;

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
    p = ivec2(clamp_st.x != 0 ? clamp(p.x, 0, s.x - 1) : ((p.x % s.x) + s.x) % s.x,
              clamp_st.y != 0 ? clamp(p.y, 0, s.y - 1) : ((p.y % s.y) + s.y) % s.y);
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
    if (chroma == 0) {
        cover = 1.0;
        return vec4(c.rgb, a.r + a.g);
    }
    cover = c.a;
    return c.a > 0.0 ? vec4(c.rgb / c.a, ta) : vec4(0.0);
}

bool keyed(vec3 c) { return chroma != 0 && all(lessThan(abs(c - key), vec3(0.5 / 255.0))); }

// the texture colour and how much of it is not keyed out
vec4 sample_tex(out float cover) {
    if (hd_layer >= 0 || linear_filter != 0 && mipmaps != 0) return sample_mip(cover);
    if (linear_filter == 0) {
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
    if (edge == 0 && cover < 0.5) discard;
    if (edge == 1) a = cover;
    if (edge == 2) a *= cover;
    if (edge == 3) c *= cover;
    if (cover <= 0.0) discard;
    if (zonly != 0) {
        // translucent surfaces (the PlayStation's semi-transparency, alpha 0x80: liquids) write
        // none: what dips into them stays visible, as the game draws it after them
        if (zonly == 2 && a < 0.75) discard;
        color = vec4(0.0);
        gbuf = vec4(0.0);
        return;
    }
    // lit smoke: by distance (no normal), and from behind through it (forward scattering)
    if (smoke_count > 0 && veffect == 1u && vinvz > 0.0) {
        float z = 1.0 / vinvz;
        vec3 p = vec3((gl_FragCoord.xy - 0.5 * smoke_size) * z / smoke_focal, z);
        vec3 sum = vec3(0.0);
        for (int i = 0; i < smoke_count; i++) {
            vec3 l = smoke_lpos[i].xyz - p;
            float r = smoke_lpos[i].w;
            float d = length(l);
            float w = d < r ? (1.0 - d / r) * (1.0 - d / r) : 0.0;
            // from behind: fading in from the smoke's own depth (a hard start at l.z = 0 drew a
            // line across smoke whose depth changes over the picture: rocket trails)
            if (l.z > 0.0 && l.z < 2.0 * r) {
                float s = max(1.0 - length(l.xy) / (1.5 * r), 0.0);
                w += 0.5 * s * s * smoothstep(0.0, 0.5 * r, l.z) * (1.0 - l.z / (2.0 * r));
            }
            sum += smoke_lcol[i] * w;
        }
        c = min(c * (1.0 + smoke_k * sum), vec3(1.0));
    }
    color = vec4(c, a);
    if (show_effects != 0) {
        const vec3 kinds[8] = vec3[8](vec3(0.0), vec3(0.6), vec3(1.0, 0.4, 0.0), vec3(1.0, 1.0, 0.0),
            vec3(1.0, 0.0, 1.0), vec3(0.0, 1.0, 1.0), vec3(0.0, 0.3, 1.0), vec3(0.0, 1.0, 0.0));
        float grey = dot(color.rgb, vec3(0.3, 0.5, 0.2)) * 0.35;
        color.rgb = veffect == 0u ? vec3(grey) : kinds[min(veffect, 7u)];
    }
    if (gmode == 1) gbuf = vec4(vinvz > 0.0 ? 1.0 / vinvz : 0.0, 0.0, 0.0, step(0.5, a));
    else gbuf = vec4(0.0, clamp(2.0 * (gmode == 3 ? max(c.r, max(c.g, c.b)) : a), 0.0, 1.0), 0.0, 0.0);
}
"#;

/// Words per batched vertex.
pub(super) const VERTEX: usize = 22;
/// The vertex buffer's size: some 40 pictures of the game's ~4000 vertices.
const VBO_SIZE: usize = 16 << 20;

use super::effects::Effect;

/// The batched vertices' attributes, on the vertex array and buffer bound (`VERTEX` words).
pub(super) fn vertex_layout(gl: &glow::Context) {
    unsafe {
        for (loc, n, off) in [(0, 2, 0), (1, 4, 8), (2, 2, 24), (3, 4, 32)] {
            gl.enable_vertex_attrib_array(loc);
            gl.vertex_attrib_pointer_f32(loc, n, glow::FLOAT, false, 4 * VERTEX as i32, off);
        }
        gl.enable_vertex_attrib_array(4);
        gl.vertex_attrib_pointer_i32(4, 4, glow::UNSIGNED_INT, 4 * VERTEX as i32, 48);
        gl.enable_vertex_attrib_array(5);
        gl.vertex_attrib_pointer_f32(5, 4, glow::FLOAT, false, 4 * VERTEX as i32, 64);
        gl.enable_vertex_attrib_array(6);
        gl.vertex_attrib_pointer_f32(6, 1, glow::FLOAT, false, 4 * VERTEX as i32, 80);
        gl.enable_vertex_attrib_array(7);
        gl.vertex_attrib_pointer_i32(7, 1, glow::UNSIGNED_INT, 4 * VERTEX as i32, 84);
    }
}

/// The shaders' uniforms, looked up once.
struct Uniforms {
    space: Option<glow::UniformLocation>,
    jitter: Option<glow::UniformLocation>,
    tex: Option<glow::UniformLocation>,
    tex_a: Option<glow::UniformLocation>,
    tex_hd: Option<glow::UniformLocation>,
    mipmaps: Option<glow::UniformLocation>,
    clamp_st: Option<glow::UniformLocation>,
    chroma: Option<glow::UniformLocation>,
    key: Option<glow::UniformLocation>,
    linear_filter: Option<glow::UniformLocation>,
    edge: Option<glow::UniformLocation>,
    gmode: Option<glow::UniformLocation>,
    ztest: Option<glow::UniformLocation>,
    zbias: Option<glow::UniformLocation>,
    zonly: Option<glow::UniformLocation>,
    show_effects: Option<glow::UniformLocation>,
    smoke_count: Option<glow::UniformLocation>,
    smoke_lpos: Option<glow::UniformLocation>,
    smoke_lcol: Option<glow::UniformLocation>,
    smoke_size: Option<glow::UniformLocation>,
    smoke_focal: Option<glow::UniformLocation>,
    smoke_k: Option<glow::UniformLocation>,
}

impl Uniforms {
    fn new(gl: &glow::Context, prog: glow::Program) -> Uniforms {
        let u = |n: &str| unsafe { gl.get_uniform_location(prog, n) };
        Uniforms {
            space: u("space"),
            jitter: u("jitter"),
            tex: u("tex"),
            tex_a: u("tex_a"),
            tex_hd: u("tex_hd"),
            mipmaps: u("mipmaps"),
            clamp_st: u("clamp_st"),
            chroma: u("chroma"),
            key: u("key"),
            linear_filter: u("linear_filter"),
            edge: u("edge"),
            gmode: u("gmode"),
            ztest: u("ztest"),
            zbias: u("zbias"),
            zonly: u("zonly"),
            show_effects: u("show_effects"),
            smoke_count: u("smoke_count"),
            smoke_lpos: u("smoke_lpos"),
            smoke_lcol: u("smoke_lcol"),
            smoke_size: u("smoke_size"),
            smoke_focal: u("smoke_focal"),
            smoke_k: u("smoke_k"),
        }
    }
}

/// For a savestate's fingerprint (`savestate.rs`): the textures' TMU addresses.
pub fn texture_addresses() -> Vec<u32> {
    with(|g, _| g.textures.keys().copied().collect()).unwrap_or_default()
}

fn with<R>(f: impl FnOnce(&mut Glide, &mut Ctx) -> R) -> Option<R> {
    GLIDE.with(|g| {
        let mut g = g.borrow_mut();
        let g = g.as_mut()?;
        with_ctx(|ctx| f(g, ctx))
    })
}

/// Runs `f` with what draws: the OpenGL context or the Vulkan renderer.
fn with_ctx<R>(f: impl FnOnce(&mut Ctx) -> R) -> Option<R> {
    if super::window::vulkan() {
        super::window::with_vkr(|v| f(&mut Ctx::Vk(v)))
    } else {
        super::window::with_gl(|gl| f(&mut Ctx::Gl(&gl.gl)))
    }
}

fn f32_arg(c: &Cpu, n: u32) -> f32 {
    f32::from_bits(arg(c, n))
}

/// Size of a texture level: LOD 0 is 256, 8 is 1; aspect 0..6 is 8:1 .. 1:8.
fn lod_size(lod: u32, aspect: u32) -> (u32, u32) {
    let l = 256 >> lod.min(8);
    match aspect {
        0..=3 => (l, (l >> (3 - aspect)).max(1)),
        _ => ((l >> (aspect - 3)).max(1), l),
    }
}

/// Texels of a format as RGBA bytes (alpha 255 where the format has none).
fn convert(format: u32, data: u32, n: usize) -> Vec<u8> {
    let m = mem();
    let e5 = |v: u32| ((v << 3) | (v >> 2)) as u8;
    let e6 = |v: u32| ((v << 2) | (v >> 4)) as u8;
    let e4 = |v: u32| (v * 17) as u8;
    let mut out = Vec::with_capacity(n * 4);
    for k in 0..n as u32 {
        let px: [u8; 4] = match format {
            0xb => {
                let v = m.rd16(data + 2 * k) as u32;
                [e5(v >> 10 & 31), e5(v >> 5 & 31), e5(v & 31), if v & 0x8000 != 0 { 255 } else { 0 }]
            }
            0xa => {
                let v = m.rd16(data + 2 * k) as u32;
                [e5(v >> 11), e6(v >> 5 & 63), e5(v & 31), 255]
            }
            0xc => {
                let v = m.rd16(data + 2 * k) as u32;
                [e4(v >> 8 & 15), e4(v >> 4 & 15), e4(v & 15), e4(v >> 12)]
            }
            0xd => {
                let v = m.rd16(data + 2 * k);
                let i = v as u8;
                [i, i, i, (v >> 8) as u8]
            }
            0 => {
                let v = m.rd8(data + k) as u32;
                [(v >> 5) as u8 * 36, (v >> 2 & 7) as u8 * 36, (v & 3) as u8 * 85, 255]
            }
            2 => [255, 255, 255, m.rd8(data + k)],
            3 => {
                let i = m.rd8(data + k);
                [i, i, i, 255]
            }
            _ => [255, 0, 255, 255],
        };
        out.extend(px);
    }
    out
}

fn bytes_per_texel(format: u32) -> u32 {
    if format >= 8 { 2 } else { 1 }
}

impl Texture {
    /// The mean colour of a texture rectangle (texels, keyed out ones count as black).
    fn average(&self, st: [f32; 4]) -> [f32; 3] {
        let key = st.map(f32::to_bits);
        if let Some(&m) = self.means.borrow().get(&key) {
            return m;
        }
        let m = self.sample(st);
        let mut means = self.means.borrow_mut();
        // rectangles of a texture are few; bounded all the same
        if means.len() >= 4096 {
            means.clear();
        }
        means.insert(key, m);
        m
    }

    /// `average` from 6x6 texels spread over the rectangle.
    fn sample(&self, st: [f32; 4]) -> [f32; 3] {
        const N: u32 = 6;
        let mut sum = [0u32; 3];
        for j in 0..N {
            for i in 0..N {
                let s = st[0] + (st[2] - st[0]) * (i as f32 + 0.5) / N as f32;
                let t = st[1] + (st[3] - st[1]) * (j as f32 + 0.5) / N as f32;
                let x = (s.floor() as i64).rem_euclid(self.w as i64) as usize;
                let y = (t.floor() as i64).rem_euclid(self.h as i64) as usize;
                let p = 3 * (y * self.w as usize + x);
                for c in 0..3 {
                    sum[c] += self.rgb[p + c] as u32;
                }
            }
        }
        sum.map(|v| v as f32 / (N * N) as f32 / 255.0)
    }

    /// A texture rectangle's texels (rgb, black keyed out; wrapping) and its size.
    fn cut(&self, r: [u32; 4]) -> (Vec<u8>, u32, u32) {
        let (w, h) = (r[2].saturating_sub(r[0]).max(1), r[3].saturating_sub(r[1]).max(1));
        let mut px = Vec::with_capacity((w * h * 3) as usize);
        for y in r[1]..r[1] + h {
            for x in r[0]..r[0] + w {
                let p = 3 * ((y % self.h) * self.w + x % self.w) as usize;
                px.extend_from_slice(&self.rgb[p..p + 3]);
            }
        }
        (px, w, h)
    }
}

impl Hd {
    /// The array for replacements, when there are any.
    fn open(gl: &glow::Context) -> Option<Hd> {
        let scale = super::hd::scale();
        if scale == 0 {
            return None;
        }
        let size = 256 * scale as i32;
        unsafe {
            let array = gl.create_texture().expect("GL texture");
            gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(array));
            gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_MIN_FILTER, glow::LINEAR_MIPMAP_LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            for p in [glow::TEXTURE_WRAP_S, glow::TEXTURE_WRAP_T] {
                gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, p, glow::CLAMP_TO_EDGE as i32);
            }
            gl.tex_storage_3d(glow::TEXTURE_2D_ARRAY, LEVELS + scale.trailing_zeros() as i32, glow::RGBA8, size, size, HD_LAYERS as i32);
            let read = gl.create_framebuffer().expect("GL framebuffer");
            let draw = gl.create_framebuffer().expect("GL framebuffer");
            Some(Hd { array, scale, stale: false, read, draw })
        }
    }

    /// Rows `r0..=r1` of the original texture (`w` wide, in `layer` of `array`) scaled up into
    /// the replacement's `hd_layer`.
    fn scale_up(&mut self, gl: &glow::Context, array: glow::Texture, layer: u32, hd_layer: u32, w: u32, (r0, r1): (u32, u32)) {
        let k = self.scale as i32;
        let (w, r0, r1) = (w as i32, r0 as i32, r1 as i32 + 1);
        unsafe {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.read));
            gl.framebuffer_texture_layer(glow::READ_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, Some(array), 0, layer as i32);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.draw));
            gl.framebuffer_texture_layer(glow::DRAW_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, Some(self.array), 0, hd_layer as i32);
            gl.blit_framebuffer(0, r0, w, r1, 0, r0 * k, w * k, r1 * k, glow::COLOR_BUFFER_BIT, glow::LINEAR);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        self.stale = true;
    }

    /// A replacement (straight alpha) into rows from `r0` (of the original) of `hd_layer`.
    fn put(&mut self, gl: &glow::Context, hd_layer: u32, r0: u32, img: &super::hd::Image) {
        let px: Vec<u8> = img
            .rgba
            .chunks(4)
            .flat_map(|p| {
                let a = p[3] as u32;
                let m = |c: u8| ((c as u32 * a + 127) / 255) as u8;
                [m(p[0]), m(p[1]), m(p[2]), p[3]]
            })
            .collect();
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(self.array));
            let y = (r0 * self.scale) as i32;
            gl.tex_sub_image_3d(glow::TEXTURE_2D_ARRAY, 0, 0, y, hd_layer as i32, img.w as i32, img.h as i32, 1, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(Some(&px)));
        }
        self.stale = true;
    }
}

impl GlBack {
    fn open(gl: &glow::Context, target: (u32, u32)) -> GlBack {
        unsafe {
            let color = texture(gl, true);
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, target.0 as i32, target.1 as i32, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
            let fbo = gl.create_framebuffer().expect("GL framebuffer");
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(color), 0);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            let prog = compile(gl, VS, FS);
            let vao = gl.create_vertex_array().expect("GL vertex array");
            let vbo = gl.create_buffer().expect("GL buffer");
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_size(glow::ARRAY_BUFFER, VBO_SIZE as i32, glow::STREAM_DRAW);
            vertex_layout(gl);
            let aniso_max = if gl.supported_extensions().iter().any(|e| e.ends_with("texture_filter_anisotropic")) {
                gl.get_parameter_f32(glow::MAX_TEXTURE_MAX_ANISOTROPY_EXT)
            } else {
                1.0
            };
            let layers = |format: u32| {
                let t = gl.create_texture().expect("GL texture");
                gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(t));
                gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_MIN_FILTER, glow::LINEAR_MIPMAP_LINEAR as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
                for p in [glow::TEXTURE_WRAP_S, glow::TEXTURE_WRAP_T] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, p, glow::CLAMP_TO_EDGE as i32);
                }
                gl.tex_storage_3d(glow::TEXTURE_2D_ARRAY, LEVELS, format, 256, 256, LAYERS as i32);
                t
            };
            let array = layers(glow::RGBA8);
            let alpha = layers(glow::RG8);
            GlBack {
                fbo,
                color,
                ms: None,
                ms_max: gl.get_parameter_i32(glow::MAX_SAMPLES).max(0) as u32,
                prog,
                vao,
                vbo,
                vbo_at: 0,
                array,
                alpha,
                stale: false,
                hd: Hd::open(gl),
                aniso: 0.0,
                aniso_max,
                uniforms: Uniforms::new(gl, prog),
                gdepth: None,
                ssao: None,
                shadow_map: None,
                lighting: None,
                heat: None,
                bloom: None,
                motion: None,
                tonemap: None,
            }
        }
    }

    /// The target made anew at w x h (`colour_format`).
    fn retarget(&mut self, gl: &glow::Context, (w, h): (u32, u32)) {
        self.drop_gbuffer(gl);
        let (format, kind) = colour_format();
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.color));
            gl.tex_image_2d(glow::TEXTURE_2D, 0, format as i32, w as i32, h as i32, 0, glow::RGBA, kind, glow::PixelUnpackData::Slice(None));
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.disable(glow::SCISSOR_TEST);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
    }

    /// The multisampled framebuffer for the chosen samples and the target's size (`resized`:
    /// the target changed).
    fn multisample(&mut self, gl: &glow::Context, target: (u32, u32), resized: bool) {
        let samples = MSAA.load(Ordering::Relaxed).min(self.ms_max);
        let samples = if samples < 2 { 0 } else { samples };
        if !resized && self.ms.map_or(0, |m| m.2) == samples {
            return;
        }
        // it was attached to the framebuffer drawn into
        self.drop_gbuffer(gl);
        unsafe {
            if let Some((fbo, rb, _)) = self.ms.take() {
                gl.delete_framebuffer(fbo);
                gl.delete_renderbuffer(rb);
            }
            if samples == 0 {
                return;
            }
            let rb = gl.create_renderbuffer().expect("GL renderbuffer");
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(rb));
            gl.renderbuffer_storage_multisample(glow::RENDERBUFFER, samples as i32, colour_format().0, target.0 as i32, target.1 as i32);
            let fbo = gl.create_framebuffer().expect("GL framebuffer");
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::RENDERBUFFER, Some(rb));
            gl.disable(glow::SCISSOR_TEST);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            self.ms = Some((fbo, rb, samples));
        }
    }

    /// The depth attachment when the AO is on (made anew after the target or the samples
    /// changed: `drop_gbuffer`), none when it is off.
    fn gbuffer(&mut self, gl: &glow::Context, target: (u32, u32)) {
        if !super::ao::depth_wanted() {
            self.drop_gbuffer(gl);
            return;
        }
        if self.gdepth.is_some() {
            return;
        }
        let (w, h) = (target.0 as i32, target.1 as i32);
        unsafe {
            let tex = gl.create_texture().expect("GL texture");
            let ms = self.ms.map(|m| m.2);
            let kind = if let Some(n) = ms {
                gl.bind_texture(glow::TEXTURE_2D_MULTISAMPLE, Some(tex));
                gl.tex_image_2d_multisample(glow::TEXTURE_2D_MULTISAMPLE, n as i32, glow::RG32F as i32, w, h, true);
                glow::TEXTURE_2D_MULTISAMPLE
            } else {
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                for p in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, p, glow::NEAREST as i32);
                }
                gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RG32F as i32, w, h, 0, glow::RG, glow::FLOAT, glow::PixelUnpackData::Slice(None));
                glow::TEXTURE_2D
            };
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.draw_fbo()));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT1, kind, Some(tex), 0);
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0, glow::COLOR_ATTACHMENT1]);
            if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
                log!("warn: no ambient occlusion (the depth attachment is not taken)");
                gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT1, kind, None, 0);
                gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
                gl.delete_texture(tex);
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                return;
            }
            let zrb = gl.create_renderbuffer().expect("GL renderbuffer");
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(zrb));
            gl.renderbuffer_storage_multisample(glow::RENDERBUFFER, ms.unwrap_or(0) as i32, glow::DEPTH_COMPONENT32F, w, h);
            gl.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::DEPTH_ATTACHMENT, glow::RENDERBUFFER, Some(zrb));
            if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
                log!("warn: no depth buffer (not taken beside the depth attachment)");
                gl.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::DEPTH_ATTACHMENT, glow::RENDERBUFFER, None);
            }
            gl.disable(glow::SCISSOR_TEST);
            gl.color_mask_draw_buffer(1, true, true, true, true);
            gl.clear_buffer_f32_slice(glow::COLOR, 1, &[0.0; 4]);
            gl.depth_mask(true);
            gl.clear_buffer_f32_slice(glow::DEPTH, 0, &[1.0]);
            let colour = gl.create_framebuffer().expect("GL framebuffer");
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(colour));
            match self.ms {
                Some((_, rb, _)) => gl.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::RENDERBUFFER, Some(rb)),
                None => gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(self.color), 0),
            }
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            self.gdepth = Some(Gdepth { tex, ms: ms.is_some(), colour, zrb });
        }
    }

    fn drop_gbuffer(&mut self, gl: &glow::Context) {
        let Some(d) = self.gdepth.take() else { return };
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.draw_fbo()));
            let kind = if d.ms { glow::TEXTURE_2D_MULTISAMPLE } else { glow::TEXTURE_2D };
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT1, kind, None, 0);
            gl.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::DEPTH_ATTACHMENT, glow::RENDERBUFFER, None);
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.delete_framebuffer(d.colour);
            gl.delete_texture(d.tex);
            gl.delete_renderbuffer(d.zrb);
        }
    }

    /// The depth attachment emptied (no depth: nothing to occlude), for the next frame.
    fn clear_gbuffer(&self, gl: &glow::Context) {
        if self.gdepth.is_none() {
            return;
        }
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.draw_fbo()));
            gl.disable(glow::SCISSOR_TEST);
            gl.color_mask_draw_buffer(1, true, true, true, true);
            gl.clear_buffer_f32_slice(glow::COLOR, 1, &[0.0; 4]);
            gl.depth_mask(true);
            gl.clear_buffer_f32_slice(glow::DEPTH, 0, &[1.0]);
        }
    }

    /// The framebuffer drawn into.
    fn draw_fbo(&self) -> glow::Framebuffer {
        self.ms.map_or(self.fbo, |m| m.0)
    }

    /// The multisampled picture into the texture shown.
    fn resolve(&self, gl: &glow::Context, target: (u32, u32)) {
        if let Some((fbo, _, _)) = self.ms {
            let (w, h) = (target.0 as i32, target.1 as i32);
            unsafe {
                gl.disable(glow::SCISSOR_TEST);
                gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbo));
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.fbo));
                gl.blit_framebuffer(0, 0, w, h, 0, 0, w, h, glow::COLOR_BUFFER_BIT, glow::NEAREST);
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            }
        }
    }

    /// The view's viewport and the clip window's scissor box.
    fn scissor(&self, gl: &glow::Context, d: &Draw) {
        let [vx, vy, vw, vh] = d.viewport;
        let th = d.target.1 as f32;
        let b = d.scissor;
        unsafe {
            gl.viewport(vx.round() as i32, (th - vy - vh).round() as i32, vw.round() as i32, vh.round() as i32);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(b[0], b[1], b[2], b[3]);
        }
    }

    /// Clears the clip window's box: the colour, and the depth attachment and buffer.
    fn clear(&self, gl: &glow::Context, d: &Draw, colour: [f32; 4]) {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.draw_fbo()));
            self.scissor(gl, d);
            gl.clear_buffer_f32_slice(glow::COLOR, 0, &colour);
            if self.gdepth.is_some() {
                gl.color_mask_draw_buffer(1, true, true, true, true);
                gl.clear_buffer_f32_slice(glow::COLOR, 1, &[0.0; 4]);
                gl.depth_mask(true);
                gl.clear_buffer_f32_slice(glow::DEPTH, 0, &[1.0]);
            }
            gl.disable(glow::SCISSOR_TEST);
        }
    }

    /// Draws a batch.
    fn draw(&mut self, gl: &glow::Context, d: &Draw, batch: &[u32]) {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.draw_fbo()));
            self.scissor(gl, d);
            gl.use_program(Some(self.prog));
            gl.bind_vertex_array(Some(self.vao));
            let u = &self.uniforms;
            gl.uniform_2_f32(u.space.as_ref(), d.space[0], d.space[1]);
            gl.uniform_2_f32(u.jitter.as_ref(), d.jitter[0], d.jitter[1]);
            gl.uniform_1_i32(u.mipmaps.as_ref(), d.mipmaps as i32);
            gl.uniform_1_i32(u.tex_a.as_ref(), 1);
            gl.uniform_1_i32(u.tex.as_ref(), 0);
            gl.uniform_1_i32(u.tex_hd.as_ref(), 2);
            let aniso = d.aniso.min(self.aniso_max);
            // replacements are always drawn with mipmaps, and read the originals' alpha with them
            let levels = d.mipmaps || self.hd.is_some();
            if let Some(hd) = &mut self.hd {
                gl.active_texture(glow::TEXTURE2);
                gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(hd.array));
                if hd.stale {
                    gl.generate_mipmap(glow::TEXTURE_2D_ARRAY);
                    hd.stale = false;
                }
                if aniso != self.aniso && self.aniso_max > 1.0 {
                    gl.tex_parameter_f32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_MAX_ANISOTROPY_EXT, aniso);
                }
            }
            for (k, t) in [(glow::TEXTURE1, self.alpha), (glow::TEXTURE0, self.array)] {
                gl.active_texture(k);
                gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(t));
                if levels && self.stale {
                    gl.generate_mipmap(glow::TEXTURE_2D_ARRAY);
                }
                if aniso != self.aniso && self.aniso_max > 1.0 {
                    gl.tex_parameter_f32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_MAX_ANISOTROPY_EXT, aniso);
                }
            }
            if levels {
                self.stale = false;
            }
            self.aniso = aniso;
            let u = &self.uniforms;
            gl.uniform_2_i32(u.clamp_st.as_ref(), d.clamp[0] as i32, d.clamp[1] as i32);
            gl.uniform_1_i32(u.chroma.as_ref(), d.chroma as i32);
            gl.uniform_3_f32(u.key.as_ref(), d.key[0], d.key[1], d.key[2]);
            gl.uniform_1_i32(u.linear_filter.as_ref(), d.linear as i32);
            gl.uniform_1_i32(u.edge.as_ref(), d.edge);
            match d.blend {
                None => gl.disable(glow::BLEND),
                Some((src, dst)) => {
                    gl.enable(glow::BLEND);
                    gl.blend_func(src, dst);
                }
            }
            gl.uniform_1_i32(u.gmode.as_ref(), d.gmode);
            gl.uniform_1_i32(u.show_effects.as_ref(), d.show_effects as i32);
            gl.uniform_1_i32(u.smoke_count.as_ref(), d.smoke.map_or(0, |l| (l.0.len() / 4) as i32));
            if let Some((pos, col, focal)) = d.smoke {
                gl.uniform_4_f32_slice(u.smoke_lpos.as_ref(), pos);
                gl.uniform_3_f32_slice(u.smoke_lcol.as_ref(), col);
                gl.uniform_2_f32(u.smoke_size.as_ref(), d.target.0 as f32, d.target.1 as f32);
                gl.uniform_1_f32(u.smoke_focal.as_ref(), focal);
                gl.uniform_1_f32(u.smoke_k.as_ref(), super::lights::SMOKE_STRENGTH);
            }
            let has_gdepth = self.gdepth.is_some();
            let gmode = d.gmode;
            let masks = || {
                if has_gdepth {
                    match gmode {
                        1 => gl.color_mask_draw_buffer(1, true, true, false, false),
                        2 | 3 => {
                            gl.color_mask_draw_buffer(1, false, true, false, false);
                            gl.disable_draw_buffer(glow::BLEND, 1);
                        }
                        _ => gl.color_mask_draw_buffer(1, false, false, false, false),
                    }
                }
            };
            masks();
            gl.uniform_1_i32(u.ztest.as_ref(), (d.zk != 0) as i32);
            gl.uniform_1_i32(u.zonly.as_ref(), 0);
            if d.zk != 0 {
                gl.enable(glow::DEPTH_TEST);
                gl.depth_func(glow::LEQUAL);
                gl.depth_mask(false);
                gl.uniform_1_f32(u.zbias.as_ref(), if d.zwrite { 0.0 } else { super::zbuffer::TEST_BIAS });
            }
            let (zonly, ztest_u, zbias_u) = (u.zonly.clone(), u.ztest.clone(), u.zbias.clone());
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            let bytes = std::slice::from_raw_parts(batch.as_ptr() as *const u8, batch.len() * 4);
            if self.vbo_at + bytes.len() > VBO_SIZE {
                gl.buffer_data_size(glow::ARRAY_BUFFER, VBO_SIZE.max(bytes.len()) as i32, glow::STREAM_DRAW);
                self.vbo_at = 0;
            }
            // the part written was never drawn from since the buffer was made: no wait (glBufferSubData:
            // mapping the range made the driver's thread catch up at every batch, ~0.3 ms a picture)
            gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, self.vbo_at as i32, bytes);
            let (first, count) = ((self.vbo_at / (4 * VERTEX)) as i32, (batch.len() / VERTEX) as i32);
            if d.zwrite {
                // first the depth alone, where the alpha is at least 0.75 when it counts
                // (keyed holes and soft edges leave what is behind visible)
                gl.color_mask(false, false, false, false);
                gl.depth_mask(true);
                gl.uniform_1_i32(zonly.as_ref(), d.zonly);
                gl.uniform_1_f32(zbias_u.as_ref(), -super::zbuffer::slack());
                gl.draw_arrays(d.topology, first, count);
                gl.uniform_1_f32(zbias_u.as_ref(), 0.0);
                gl.uniform_1_i32(zonly.as_ref(), 0);
                gl.depth_mask(false);
                gl.color_mask(true, true, true, true);
                masks();
            }
            gl.draw_arrays(d.topology, first, count);
            self.vbo_at += bytes.len();
            if d.zk != 0 {
                gl.disable(glow::DEPTH_TEST);
                gl.depth_mask(true);
                gl.uniform_1_i32(ztest_u.as_ref(), 0);
            }
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
        }
    }

    /// Rows of a texture layer: rgb premultiplied by coverage and the coverage, and the alpha.
    #[allow(clippy::too_many_arguments)]
    fn upload(&mut self, gl: &glow::Context, layer: u32, r0: u32, w: u32, rows: u32, rgb: &[u8], alpha: &[u8]) {
        unsafe {
            for (t, format, px) in [(self.array, glow::RGBA, rgb), (self.alpha, glow::RG, alpha)] {
                gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(t));
                gl.tex_sub_image_3d(glow::TEXTURE_2D_ARRAY, 0, 0, r0 as i32, layer as i32, w as i32, rows as i32, 1, format, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(Some(px)));
            }
        }
        self.stale = true;
    }

    fn close(&mut self, gl: &glow::Context) {
        unsafe {
            if let Some((fbo, rb, _)) = self.ms.take() {
                gl.delete_framebuffer(fbo);
                gl.delete_renderbuffer(rb);
            }
            self.drop_gbuffer(gl);
            if let Some(mut s) = self.ssao.take() {
                s.delete(gl);
            }
            if let Some(mut s) = self.shadow_map.take() {
                s.delete(gl);
            }
            if let Some(mut s) = self.lighting.take() {
                s.delete(gl);
            }
            if let Some(mut s) = self.heat.take() {
                s.delete(gl);
            }
            if let Some(mut s) = self.bloom.take() {
                s.delete(gl);
            }
            if let Some(mut s) = self.motion.take() {
                s.delete(gl);
            }
            if let Some(mut s) = self.tonemap.take() {
                s.delete(gl);
            }
            gl.delete_framebuffer(self.fbo);
            gl.delete_texture(self.color);
            gl.delete_texture(self.array);
            gl.delete_texture(self.alpha);
        }
    }
}

impl Glide {
    fn open(ctx: &mut Ctx, w: u32, h: u32) -> Option<Glide> {
        let target = (w, w * 3 / 4);
        let back = match ctx {
            Ctx::Gl(gl) => Back::Gl(Box::new(GlBack::open(gl, target))),
            Ctx::Vk(v) => match super::vglide::Gpu::open(v) {
                Ok(g) => Back::Vk(Box::new(g)),
                Err(e) => {
                    log!("error: vulkan: Glide: {e}");
                    return None;
                }
            },
        };
        Some(Glide {
            res: (w, h),
            target,
            // made in RGBA8: the first resize makes it anew when tone mapping is on
            target_hdr: false,
            space: (w, h),
            view: View::Full,
            back,
            textures: BTreeMap::new(),
            downloads: 0,
            cc: [1, 0, 0, 0, 0],
            ac: [1, 0, 0, 0, 0],
            constant: 0xffff_ffff,
            tex: None,
            state: State {
                blend: (4, 0),
                chroma: false,
                key: 0,
                linear: false,
                clamp: (true, true),
                clip: [0, 0, w as i32, h as i32],
            },
            batch: Vec::new(),
            batch_mode: glow::TRIANGLES,
            batch_z: 0,
            gamma: 1.0,
            gbuf: None,
            casters: Vec::new(),
            cast_only: 0,
            lights: Vec::new(),
            zbuf: Vec::new(),
            smoke_lights: Vec::new(),
            heat_sources: Vec::new(),
        })
    }

    /// The view's rectangle in the target (x, y from the top, w, h) and the columns it is cut to.
    fn view_rect(&self) -> ([f32; 4], (f32, f32)) {
        let (tw, th) = (self.target.0 as f32, self.target.1 as f32);
        let w43 = (th * 4.0 / 3.0).min(tw);
        match self.view {
            View::Full => ([0.0, 0.0, tw, th], (0.0, tw)),
            View::Hud(at) => ([(tw - w43) * at, 0.0, w43, th], (0.0, tw)),
            View::Half(false) => ([tw / 4.0 - w43 / 4.0, 0.0, w43, th], (0.0, tw / 2.0)),
            View::Half(true) => ([tw * 0.75 - w43 * 0.75, 0.0, w43, th], (tw / 2.0, tw)),
        }
    }

    /// The depth attachment is there (the depth for the AO, the lights, the depth buffer...).
    fn has_depth(&self) -> bool {
        match &self.back {
            Back::Gl(b) => b.gdepth.is_some(),
            Back::Vk(b) => b.has_depth(),
        }
    }

    /// Replacement textures are there.
    fn has_hd(&self) -> bool {
        match &self.back {
            Back::Gl(b) => b.hd.is_some(),
            Back::Vk(b) => b.has_hd(),
        }
    }

    /// Size the target (between frames): as tall as the render size `h` (the window's or the
    /// chosen resolution: high resolution), as wide as the aspect `a` makes it.
    fn resize(&mut self, ctx: &mut Ctx, a: f32, h: u32) {
        let h = h.clamp(240, 4320);
        let w = (h as f32 * a).round() as u32;
        let changed = (w, h) != self.target || self.target_hdr != super::tonemap::hdr();
        if changed {
            self.target = (w, h);
            self.target_hdr = super::tonemap::hdr();
            TARGET_HDR.store(self.target_hdr, Ordering::Relaxed);
        }
        let target = self.target;
        match (&mut self.back, ctx) {
            (Back::Gl(b), Ctx::Gl(gl)) => {
                if changed {
                    b.retarget(gl, target);
                }
                // the samples may have changed in Preferences, the AO in Advanced Graphics
                b.multisample(gl, target, changed);
                b.gbuffer(gl, target);
            }
            (Back::Vk(b), Ctx::Vk(v)) => {
                let samples = MSAA.load(Ordering::Relaxed);
                if let Err(e) = b.resize(v, target, self.target_hdr, if samples < 2 { 1 } else { samples }, super::ao::depth_wanted()) {
                    log!("error: vulkan: Glide target: {e}");
                }
            }
            _ => {}
        }
    }

    /// The scissor box of a clip rectangle (x, y from below, w, h).
    fn scissor_box(&self, c: [i32; 4]) -> [i32; 4] {
        let ([vx, vy, vw, vh], (cut0, cut1)) = self.view_rect();
        let th = self.target.1 as f32;
        let (sx, sy) = (vw / self.res.0 as f32, vh / self.res.1 as f32);
        let x0 = (vx + c[0] as f32 * sx).max(cut0).round() as i32;
        let x1 = (vx + c[2] as f32 * sx).min(cut1).round() as i32;
        let (y0, y1) = ((vy + c[1] as f32 * sy).round() as i32, (vy + c[3] as f32 * sy).round() as i32);
        [x0, th as i32 - y1, (x1 - x0).max(0), (y1 - y0).max(0)]
    }

    /// A drawing of nothing yet: the view and clip window, the rest off.
    fn draw_base(&self) -> Draw<'static> {
        Draw {
            topology: self.batch_mode,
            target: self.target,
            viewport: self.view_rect().0,
            scissor: self.scissor_box(self.state.clip),
            space: [self.space.0 as f32, self.space.1 as f32],
            jitter: [0.0; 2],
            mipmaps: false,
            aniso: 1.0,
            clamp: [true; 2],
            chroma: false,
            key: [0.0; 3],
            linear: false,
            blend: None,
            edge: 0,
            gmode: 0,
            zk: 0,
            zwrite: false,
            zonly: 0,
            show_effects: false,
            smoke: None,
        }
    }

    /// Draw what is batched with the current state.
    fn flush(&mut self, ctx: &mut Ctx) {
        if self.batch.is_empty() {
            return;
        }
        let s = self.state;
        let mut d = self.draw_base();
        // only the 3D picture is jittered, the HUD stays still
        let j = if self.gbuf.is_some() { super::jitter::now() } else { [0.0; 2] };
        d.jitter = [2.0 * j[0] / self.target.0 as f32, 2.0 * j[1] / self.target.1 as f32];
        let filter = FILTER.load(Ordering::Relaxed);
        d.mipmaps = filter != 0 && s.key & 0xff_ffff == 0;
        d.aniso = filter.max(1) as f32;
        d.clamp = [s.clamp.0, s.clamp.1];
        d.chroma = s.chroma;
        let key = s.key;
        d.key = [(key >> 16 & 0xff) as f32 / 255.0, (key >> 8 & 0xff) as f32 / 255.0, (key & 0xff) as f32 / 255.0];
        d.linear = s.linear;
        // blending: Glide factors 0 zero, 1 src alpha, 2 src/dst colour, 3 dst alpha, 4 one,
        // 5 1 - src alpha, 6 1 - src/dst colour, 7 1 - dst alpha
        let factor = |f: u32, dst: bool| match f {
            0 => glow::ZERO,
            1 => glow::SRC_ALPHA,
            2 => if dst { glow::SRC_COLOR } else { glow::DST_COLOR },
            3 => glow::DST_ALPHA,
            5 => glow::ONE_MINUS_SRC_ALPHA,
            6 => if dst { glow::ONE_MINUS_SRC_COLOR } else { glow::ONE_MINUS_DST_COLOR },
            7 => glow::ONE_MINUS_DST_ALPHA,
            _ => glow::ONE,
        };
        let (src, dst) = (factor(s.blend.0, false), factor(s.blend.1, true));
        let opaque = src == glow::ONE && dst == glow::ZERO;
        let soft = s.chroma && s.linear;
        // the soft chroma key edge: 0 cut at half, 1 alpha = cover (blending turned on for
        // it), 2 alpha *= cover, 3 rgb *= cover (additive). Untextured vertices have a
        // cover of 1, which leaves them as they are.
        d.edge = if !soft {
            0
        } else if opaque {
            1
        } else if src == glow::SRC_ALPHA || dst == glow::ONE_MINUS_SRC_ALPHA {
            2
        } else if src == glow::ONE && dst == glow::ONE {
            3
        } else {
            0
        };
        d.blend = if opaque && d.edge != 1 {
            None
        } else if d.edge == 1 {
            Some((glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA))
        } else {
            Some((src, dst))
        };
        // the depth attachment: depth from opaque 3D primitives and alpha-blended ones (the
        // game draws its solid geometry so, with alpha 1), where their alpha is at least
        // half (the attachment blended with its own alpha 0 or 1); a mask from those that
        // add colour; nothing from those that only darken (the game's shadows) or the HUD
        d.gmode = match (self.has_depth(), self.gbuf.is_some()) {
            (true, true) if opaque || src == glow::SRC_ALPHA && dst == glow::ONE_MINUS_SRC_ALPHA => 1,
            (true, true) if src == glow::ZERO || src == glow::DST_COLOR => 0,
            (true, true) if dst == glow::ONE => 3,
            (true, true) => 2,
            _ => 0,
        };
        // the sun's shadow casters: triangles with depth at every corner, the chroma key in
        // the constant colour's place (`shadows.rs`)
        if d.gmode == 1 && self.batch_mode == glow::TRIANGLES && super::shadows::map() {
            let kc = [d.key[0], d.key[1], d.key[2], s.chroma as u32 as f32].map(f32::to_bits);
            for t in self.batch.chunks_exact(3 * VERTEX) {
                if t.chunks_exact(VERTEX).all(|v| f32::from_bits(v[20]) > 0.0) {
                    for v in t.chunks_exact(VERTEX) {
                        self.casters.extend_from_slice(&v[..8]);
                        self.casters.extend(kc);
                        self.casters.extend_from_slice(&v[12..]);
                    }
                }
            }
        }
        d.show_effects = self.gbuf.is_some() && super::effects::showing();
        // the depth buffer (`zbuffer.rs`): solid primitives with their own depth at every
        // corner write it, the others with depth only test
        d.zk = self.batch_z;
        d.zwrite = d.zk == 2 && d.gmode == 1;
        d.zonly = if opaque && d.edge != 1 { 1 } else { 2 };
        // lit smoke: additive effects of the 3D picture take the view's last lights
        let focal = 4096.0 * self.target.1 as f32 / 1920.0;
        let lit = match self.gbuf {
            Some(_) if self.has_depth() && dst == glow::ONE && super::lights::smoke_enabled() => {
                let clip = d.scissor;
                self.smoke_lights.iter().find(|l| l.0 == clip).filter(|l| !l.1.is_empty())
            }
            _ => None,
        };
        let d = Draw { smoke: lit.map(|(_, pos, col)| (&pos[..], &col[..], focal)), ..d };
        match (&mut self.back, ctx) {
            (Back::Gl(b), Ctx::Gl(gl)) => b.draw(gl, &d, &self.batch),
            (Back::Vk(b), Ctx::Vk(v)) => b.draw(v, &d, &self.batch),
            _ => {}
        }
        self.batch.clear();
    }

    /// Change the state, drawing what was batched with the old one first.
    fn set(&mut self, ctx: &mut Ctx, f: impl FnOnce(&mut State)) {
        let mut s = self.state;
        f(&mut s);
        if s != self.state {
            self.flush(ctx);
            self.state = s;
        }
    }

    /// Add vertices (GrVertex at guest addresses) as primitives of `mode`.
    fn add(&mut self, ctx: &mut Ctx, mode: u32, verts: &[u32]) {
        let _t = super::fps::timer(super::fps::Phase::Add);
        // the game's round shadows under objects (blend ZERO, 1 - SRC_ALPHA: they darken) are
        // left out of the 3D picture when ours are drawn (`shadows.rs`)
        if self.state.blend == (0, 5) && self.gbuf.is_some() && super::shadows::enabled() {
            return;
        }
        if mode != self.batch_mode {
            self.flush(ctx);
            self.batch_mode = mode;
        }
        let m = mem();
        let k = self.constant;
        let ch = |sh: u32| ((k >> sh & 0xff) as f32 / 255.0).to_bits();
        let konst = [ch(16), ch(8), ch(0), ch(24)];
        // the combine units a byte each, then invert bits, texturing, the layer and the
        // replacement's layer + 1, its size
        let bytes = |u: [u32; 5]| u[0] | u[1] << 8 | u[2] << 16 | u[3] << 24;
        let tex = self.tex.and_then(|a| self.textures.get(&a));
        let mode = [
            bytes(self.cc),
            bytes(self.ac),
            (self.cc[4] & 1) | (self.ac[4] & 1) << 1 | (tex.is_some() as u32) << 2 | tex.map_or(0, |t| t.layer << 8 | t.hd.map_or(0, |l| l + 1) << 16),
            tex.map_or(0, |t| t.w | t.h << 16),
        ];
        let textured = tex.is_some();
        // x y z r g b ooz a oow, then tmu 0: sow tow oow
        let st = |v: u32| {
            let oow = f32::from_bits(m.rd32(v + 0x2c));
            let oow = if oow != 0.0 { oow } else { 1.0 };
            (f32::from_bits(m.rd32(v + 0x24)) / oow, f32::from_bits(m.rd32(v + 0x28)) / oow)
        };
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for &v in verts {
            let (s, t) = st(v);
            b = [b[0].min(s), b[1].min(t), b[2].max(s), b[3].max(t)];
        }
        let st_box = b;
        let b = b.map(f32::to_bits);
        // the ring's corners back to their real place (2560x1920 units, centre 1280, 960 and
        // the view's origin)
        let ring = self.cast_only == 2;
        let ox = self.gbuf.unwrap_or(0.0);
        let xy = |v: u32| {
            let (x, y) = (f32::from_bits(m.rd32(v)), f32::from_bits(m.rd32(v + 4)));
            if ring {
                let z = super::shadows::RING_ZOOM;
                (ox + 1280.0 + (x - ox - 1280.0) / z, 960.0 + (y - 960.0) / z)
            } else {
                (x, y)
            }
        };
        // a line: a quad as wide as the game's line at its resolution
        let quad = if let [v0, v1] = *verts { Some(self.line_offsets(xy(v0), xy(v1))) } else { None };
        // the view-space depth of each vertex (3D picture, AO on), by its place in the units
        let mut z = std::mem::take(&mut self.zbuf);
        let exact = match (self.has_depth(), self.gbuf) {
            (true, Some(dx)) => super::ao::depths(verts.iter().map(|&v| (xy(v).0 - dx, xy(v).1)), &mut z),
            _ => {
                z.clear();
                z.resize(verts.len(), 0.0);
                false
            }
        };
        // the depth buffer: the 3D picture's primitives with a depth at every corner (written
        // only with their own), a batch of one kind
        let zk = if super::zbuffer::enabled() && self.gbuf.is_some() && self.has_depth() && self.cast_only == 0 && z.iter().all(|&d| d > 0.0) {
            if exact { 2 } else { 1 }
        } else {
            0
        };
        // ...but not the game's round shadows (blend ZERO, 1 - SRC_ALPHA): a fan whose corners
        // are no projected points of their own, so their depths were other things' (the robot's
        // feet): the test cut most of the shadow away. Drawn in the game's order, as before.
        let zk = if self.state.blend == (0, 5) { 0 } else { zk };
        if zk != self.batch_z {
            self.flush(ctx);
            self.batch_z = zk;
        }
        // perspective-correct textures: only with every vertex's own depth
        let mut mode = mode;
        if exact && textured && verts.len() >= 3 && perspective() {
            mode[2] |= 8;
        }
        // the display list record it comes from (`effects.rs`), in the 3D picture
        // (and whether it is a model's light)
        let (rec, model_light) = match (self.gbuf, verts.first()) {
            // (records that only cast are no effects; the ring's are not in their list)
            (Some(_), Some(_)) if self.cast_only != 0 => (None, false),
            (Some(dx), Some(&v)) => super::effects::record_of(xy(v).0 - dx, xy(v).1).map_or((None, false), |(r, l)| (Some(r), l)),
            _ => (None, false),
        };
        if let (Some(r), true) = (rec, super::effects::logging()) {
            self.census(r, verts, st_box);
        }
        // a glow of the 3D picture (additive): a dynamic light
        // (not lines: the laser sight would light the ground along it)
        if self.cast_only == 0 && self.gbuf.is_some() && self.has_depth() && super::lights::enabled() && verts.len() >= 3 && self.state.blend.1 == 4 && matches!(self.state.blend.0, 1 | 4) {
            let _t = super::fps::timer(super::fps::Phase::Lights);
            let colour = self.glow_colour(verts, st_box);
            let (tw, th) = (self.target.0 as f32, self.target.1 as f32);
            let (sw, sh) = (self.space.0 as f32, self.space.1 as f32);
            let corners: Vec<_> = verts.iter().map(|&v| (xy(v).0 / sw * tw, th - xy(v).1 / sh * th)).collect();
            let focal = 4096.0 * th / 1920.0;
            // no depth found (a rocket's smoke trail): the nearest projected point's
            let smoke = z.iter().all(|&d| d <= 0.0);
            let z = match (smoke, self.gbuf) {
                (true, Some(dx)) => {
                    let n = verts.len() as f32;
                    let (cx, cy) = verts.iter().fold((0.0, 0.0), |a, &v| (a.0 + (xy(v).0 - dx) / n, a.1 + xy(v).1 / n));
                    super::ao::depth_near(cx, cy, 160.0).map_or(z.clone(), |d| vec![d; verts.len()])
                }
                _ => z.clone(),
            };
            // a robot's police light: pure red vertex colours (the game's glows at the ends of
            // the light bar, sprites: one depth for all corners); it flashes red and blue
            // (`lights::police`). A rocket's trail is pure red too, but 3D geometry.
            let sprite = z.iter().all(|&d| d > 0.0 && d == z[0]);
            let red_only = self.cc[2] == 0 && verts.iter().all(|&v| m.rd32(v + 0x10) == 0 && m.rd32(v + 0x14) == 0);
            if red_only && !sprite {
                if let Some(l) = super::lights::light(&corners, &z, colour, focal) {
                    self.lights.push(super::lights::trail(l, 0.15));
                }
            } else if red_only {
                for c in super::lights::police(colour[0]) {
                    if let Some(l) = super::lights::light(&corners, &z, c, focal) {
                        self.lights.push(super::lights::police_size(l));
                    }
                }
            } else if let Some(l) = super::lights::light(&corners, &z, colour, focal) {
                self.lights.push(if smoke { super::lights::trail(l, 0.4) } else { l });
            }
        }
        let effect = rec.map_or(Effect::None, |r| self.effect(r, verts, st_box));
        // fire, explosions and rocket trails make the picture shimmer (`heat.rs`); an
        // explosion's fireball is a glow (untextured), told from muzzle flashes and police
        // lights by its warm vertex colours; lamps' glows are models' lights (`effects.rs`)
        let warm = || {
            let n = verts.len().max(1) as f32;
            let avg = |o: u32| verts.iter().map(|&v| f32::from_bits(m.rd32(v + o))).sum::<f32>() / n;
            let (r, g, b) = (avg(0xc), avg(0x10), avg(0x14));
            r > 60.0 && g > 0.35 * r && b < 0.7 * r
        };
        let hot = effect == Effect::Fire || effect == Effect::Glow && self.cc[2] == 0 && !model_light && warm();
        // of the smoke only a rocket's trail (records 0xd_, 3D ribbons): grey clouds and fog
        // are not hot (sprites 0xbc)
        let trail = effect == Effect::Smoke && rec.is_some_and(|r| r[0] & 0xf0 == 0xd0);
        if (hot || trail) && self.cast_only == 0 && super::heat::enabled() {
            let (tw, th) = (self.target.0 as f32, self.target.1 as f32);
            let (sw, sh) = (self.space.0 as f32, self.space.1 as f32);
            let corners: Vec<_> = verts.iter().map(|&v| (xy(v).0 / sw * tw, th - xy(v).1 / sh * th)).collect();
            if let Some(s) = super::heat::source(&corners, &z, hot, 4096.0 * th / 1920.0) {
                self.heat_sources.push(s);
            }
        }
        let effect = effect as u32;
        let start = self.batch.len();
        let mut put = |v: u32, (x, y): (f32, f32), z: f32| {
            let (s, t) = st(v);
            self.batch.extend([x.to_bits(), y.to_bits(), m.rd32(v + 0xc), m.rd32(v + 0x10), m.rd32(v + 0x14), m.rd32(v + 0x1c), s.to_bits(), t.to_bits()]);
            self.batch.extend(konst);
            self.batch.extend(mode);
            self.batch.extend(b);
            self.batch.push(z.to_bits());
            self.batch.push(effect);
        };
        if let (Some([n0, n1, n2, n3]), &[v0, v1]) = (quad, verts) {
            put(v0, n0, z[0]);
            put(v0, n1, z[0]);
            put(v1, n2, z[1]);
            put(v1, n2, z[1]);
            put(v0, n1, z[0]);
            put(v1, n3, z[1]);
        } else {
            for (&v, &z) in verts.iter().zip(&z) {
                put(v, xy(v), z);
            }
        }
        self.zbuf = z;
        if self.cast_only != 0 {
            self.cast(start);
        }
    }

    /// The primitive batched from `start` only casts (`set_cast_only`): not drawn, kept as a
    /// caster when solid, a triangle and with depth at every corner (as `flush` keeps them).
    fn cast(&mut self, start: usize) {
        let s = self.state;
        let solid = s.blend == (4, 0) || s.blend == (1, 5);
        let tri = self.batch_mode == glow::TRIANGLES && self.batch.len() - start == 3 * VERTEX;
        if solid && tri && self.gbuf.is_some() && super::shadows::map() {
            let key = s.key;
            let kc = [(key >> 16 & 0xff) as f32 / 255.0, (key >> 8 & 0xff) as f32 / 255.0, (key & 0xff) as f32 / 255.0, s.chroma as u32 as f32].map(f32::to_bits);
            let t = &self.batch[start..];
            let ok = t.chunks_exact(VERTEX).all(|v| f32::from_bits(v[20]) > 0.0);
            super::shadows::note_cast(self.cast_only, ok);
            if ok {
                for v in t.chunks_exact(VERTEX) {
                    self.casters.extend_from_slice(&v[..8]);
                    self.casters.extend(kc);
                    self.casters.extend_from_slice(&v[12..]);
                }
            }
        }
        self.batch.truncate(start);
    }

    /// What effect a primitive of record `rec` is: by its record's blend (+6: 2 additive,
    /// 3 darkening), its vertices, and the mean colour of its texture rectangle.
    fn effect(&self, rec: super::effects::Record, verts: &[u32], st: [f32; 4]) -> Effect {
        if !matches!(rec[2], 2 | 3) {
            return Effect::None;
        }
        if verts.len() == 2 {
            return Effect::Laser;
        }
        let tex = self.tex.filter(|_| self.cc[0] != 1 && self.cc[3] == 1).and_then(|a| self.textures.get(&a));
        let Some(t) = tex else { return Effect::Glow };
        let [r, g, b] = t.average(st);
        let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
        if rec[2] == 3 || hi - lo < 0.3 * hi {
            Effect::Smoke
        } else if b > r {
            Effect::Energy
        } else {
            Effect::Fire
        }
    }

    /// `FCOP_EFFECT_LOG`: a primitive of the 3D picture to `effects::census`: its record, its
    /// texture rectangle's texels, blend and colour.
    fn census(&self, rec: super::effects::Record, verts: &[u32], st: [f32; 4]) {
        let m = mem();
        let n = verts.len().max(1) as f32;
        let avg = |o: u32| (verts.iter().map(|&v| f32::from_bits(m.rd32(v + o))).sum::<f32>() / n) as u8;
        let colour = if self.cc[2] == 0 { [avg(0xc), avg(0x10), avg(0x14), avg(0x1c)] } else { self.constant.rotate_left(8).to_be_bytes() };
        let rect = st.map(|v| v.round().max(0.0) as u32);
        // the texture, when the colour combine takes it ("other" 1, not the local colour alone)
        let textured = self.cc[0] != 1 && self.cc[3] == 1;
        let cut = self.tex.filter(|_| textured).and_then(|a| self.textures.get(&a)).map(|t| t.cut(rect));
        super::effects::census(rec, rect, self.state.blend, colour, cut);
    }

    /// The colour a glowing primitive adds (0...1): its local colour (the vertices' or the
    /// constant), times the average of its texture rectangle when the colour unit takes the
    /// texture, times its alpha when it is blended by it.
    fn glow_colour(&self, verts: &[u32], st: [f32; 4]) -> [f32; 3] {
        let m = mem();
        let n = verts.len().max(1) as f32;
        let avg = |o: u32| verts.iter().map(|&v| f32::from_bits(m.rd32(v + o))).sum::<f32>() / n / 255.0;
        let k = self.constant;
        let konst = |sh: u32| (k >> sh & 0xff) as f32 / 255.0;
        let local = if self.cc[2] == 0 { [avg(0xc), avg(0x10), avg(0x14)] } else { [konst(16), konst(8), konst(0)] };
        let alpha = match (self.state.blend.0, self.ac[2]) {
            (4, _) => 1.0,
            (_, 0) => avg(0x1c),
            _ => konst(24),
        };
        // grColorCombine: function 1 is the local colour alone; "other" 1 the texture
        let c = if self.cc[0] != 1 && self.cc[3] == 1 {
            let t = self.tex.and_then(|a| self.textures.get(&a)).map_or([1.0; 3], |t| t.average(st));
            if self.cc[1] == 8 { t } else { [t[0] * local[0], t[1] * local[1], t[2] * local[2]] }
        } else {
            local
        };
        c.map(|c| (c * alpha).clamp(0.0, 1.0))
    }

    /// The corners of a line from `a` to `b` (in the vertices' space) drawn as a quad: a pixel
    /// wide at 720 lines (the laser sight: 1 pixel stayed hair-thin at high resolution), at
    /// least one; past the ends by half that, as GL draws lines.
    fn line_offsets(&self, a: (f32, f32), b: (f32, f32)) -> [(f32, f32); 4] {
        let ([_, _, vw, vh], _) = self.view_rect();
        // target pixels per unit of the vertices' space
        let (kx, ky) = (vw / self.space.0 as f32, vh / self.space.1 as f32);
        let width = (self.target.1 as f32 / 720.0).round().max(1.0);
        let (dx, dy) = ((b.0 - a.0) * kx, (b.1 - a.1) * ky);
        let len = (dx * dx + dy * dy).sqrt();
        let (ux, uy) = if len > 0.0 { (dx / len, dy / len) } else { (1.0, 0.0) };
        let h = width / 2.0;
        // along (u) and across (n = u turned) in pixels, back to the space's units
        let p = |o: (f32, f32), along: f32, across: f32| (o.0 + (ux * along - uy * across) / kx, o.1 + (uy * along + ux * across) / ky);
        [p(a, -h, -h), p(a, -h, h), p(b, h, -h), p(b, h, h)]
    }

    /// Texels into the texture at TMU address `addr` (rows `rows` of its top level, or all).
    fn download(&mut self, ctx: &mut Ctx, addr: u32, lod: u32, aspect: u32, format: u32, data: u32, rows: Option<(u32, u32)>) {
        self.flush(ctx);
        let (w, h) = lod_size(lod, aspect);
        let bytes = w * h * bytes_per_texel(format);
        self.downloads += 1;
        let stamp = self.downloads;
        let layer = match self.textures.get_mut(&addr) {
            Some(t) if (t.w, t.h, t.bytes) == (w, h, bytes) => {
                t.stamp = stamp;
                t.layer
            }
            _ => {
                // what it overwrites in TMU memory is gone; then a free layer, or the one
                // downloaded longest ago
                self.textures.retain(|&a, t| a + t.bytes <= addr || addr + bytes <= a);
                let layer = (0..LAYERS).find(|l| self.textures.values().all(|t| t.layer != *l)).unwrap_or_else(|| {
                    let (&a, t) = self.textures.iter().min_by_key(|t| t.1.stamp).unwrap();
                    let l = t.layer;
                    self.textures.remove(&a);
                    l
                });
                self.textures.insert(addr, Texture { layer, w, h, bytes, stamp, hd: None, rgb: vec![0; (w * h * 3) as usize], means: Default::default() });
                layer
            }
        };
        let (r0, r1) = rows.unwrap_or((0, h - 1));
        let r1 = r1.min(h - 1);
        if r0 > r1 {
            return;
        }
        let n = ((r1 - r0 + 1) * w) as usize;
        let mut px = convert(format, data, n);
        let key = super::hd::hash(format, w, r1 - r0 + 1, mem().slice(data, n * bytes_per_texel(format) as usize));
        super::light::recolour(key, &mut px);
        // as a replacement has it: alpha 0 where keyed out
        let original: Vec<u8> = px.chunks(4).flat_map(|p| [p[0], p[1], p[2], if p[..3] == [0, 0, 0] { 0 } else { 255 }]).collect();
        super::hd::dump(key, w, r1 - r0 + 1, &original);
        // premultiplied by coverage: a black texel is keyed out
        let mut rgb = Vec::with_capacity(n * 4);
        let mut alpha = Vec::with_capacity(n * 2);
        for p in px.chunks(4) {
            let covered = p[..3] != [0, 0, 0];
            rgb.extend(if covered { [p[0], p[1], p[2], 255] } else { [0; 4] });
            alpha.extend(if covered { [p[3], 0] } else { [0, p[3]] });
        }
        if let Some(t) = self.textures.get_mut(&addr) {
            let at = (r0 * w * 3) as usize;
            t.means.get_mut().clear();
            for (k, p) in px.chunks(4).enumerate() {
                t.rgb[at + 3 * k..at + 3 * k + 3].copy_from_slice(&p[..3]);
            }
        }
        match (&mut self.back, &mut *ctx) {
            (Back::Gl(b), Ctx::Gl(gl)) => b.upload(gl, layer, r0, w, r1 - r0 + 1, &rgb, &alpha),
            (Back::Vk(b), Ctx::Vk(v)) => b.upload(v, layer, r0, w, r1 - r0 + 1, &rgb, &alpha),
            _ => {}
        }
        self.replace(ctx, addr, key, (r0, r1), &original);
    }

    /// After a download of rows `r0..=r1` (known by `key`) into the texture at `addr`: its
    /// replacement into the replacements' layer, or the original scaled up when other parts of
    /// the page are replaced. A whole page without a replacement gives its layer up.
    fn replace(&mut self, ctx: &mut Ctx, addr: u32, key: u64, rows: (u32, u32), original: &[u8]) {
        if !self.has_hd() {
            return;
        }
        let t = &self.textures[&addr];
        let (layer, w, h) = (t.layer, t.w, t.h);
        let img = super::hd::find(key, w, rows.1 - rows.0 + 1, original);
        let whole = rows == (0, h - 1);
        let hd_layer = match (t.hd, &img) {
            (Some(l), Some(_)) => l,
            (Some(l), None) if !whole => l,
            (Some(_), None) => {
                self.textures.get_mut(&addr).unwrap().hd = None;
                return;
            }
            (None, None) => return,
            (None, Some(_)) => {
                // a free layer, or the one of the texture downloaded longest ago
                let l = (0..HD_LAYERS).find(|l| self.textures.values().all(|t| t.hd != Some(*l))).unwrap_or_else(|| {
                    let t = self.textures.values_mut().filter(|t| t.hd.is_some()).min_by_key(|t| t.stamp).unwrap();
                    t.hd.take().unwrap()
                });
                self.textures.get_mut(&addr).unwrap().hd = Some(l);
                // the rest of the page as it is
                if !whole {
                    self.hd_scale_up(ctx, layer, l, w, (0, h - 1));
                }
                l
            }
        };
        match img {
            Some(img) => match (&mut self.back, ctx) {
                (Back::Gl(b), Ctx::Gl(gl)) => b.hd.as_mut().unwrap().put(gl, hd_layer, rows.0, &img),
                (Back::Vk(b), Ctx::Vk(v)) => b.hd_put(v, hd_layer, rows.0, &img),
                _ => {}
            },
            None => self.hd_scale_up(ctx, layer, hd_layer, w, rows),
        }
    }

    /// Rows of the original texture in `layer` scaled up into the replacement's `hd_layer`.
    fn hd_scale_up(&mut self, ctx: &mut Ctx, layer: u32, hd_layer: u32, w: u32, rows: (u32, u32)) {
        match (&mut self.back, ctx) {
            (Back::Gl(b), Ctx::Gl(gl)) => {
                let array = b.array;
                b.hd.as_mut().unwrap().scale_up(gl, array, layer, hd_layer, w, rows);
            }
            (Back::Vk(b), Ctx::Vk(v)) => b.hd_scale_up(v, layer, hd_layer, w, rows),
            _ => {}
        }
    }
}

// ---- the Glide functions ---------------------------------------------------------------------

fn ret1(_: &mut Cpu) -> u32 {
    1
}
fn ret0(_: &mut Cpu) -> u32 {
    0
}

fn sst_query_boards(c: &mut Cpu) -> u32 {
    mem().wr32(arg(c, 0), 1);
    1
}

fn sst_query_hardware(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let m = mem();
    m.slice(p, 0x40).fill(0);
    // one Voodoo with 6 MB frame buffer memory (the game offers up to 1280x960 for it)
    for (off, v) in [(0, 1), (4, 0), (8, 6), (0xc, 2), (0x10, 1), (0x14, 0), (0x18, 1), (0x1c, TMU_RAM >> 20)] {
        m.wr32(p + off, v);
    }
    1
}

fn sst_win_open(c: &mut Cpu) -> u32 {
    let res = arg(c, 1);
    let (w, h) = match res {
        0 => (320, 200),
        1 => (320, 240),
        2 => (400, 256),
        3 => (512, 384),
        4 => (640, 200),
        5 => (640, 350),
        6 => (640, 400),
        7 => (640, 480),
        8 => (800, 600),
        9 => (960, 720),
        0xa => (856, 480),
        0xb => (512, 256),
        0xc => (1024, 768),
        0xd => (1280, 1024),
        0xe => (1600, 1200),
        _ => (640, 480),
    };
    log!("glide: grSstWinOpen {w}x{h}");
    let mut g = with_ctx(|ctx| Glide::open(ctx, w, h)).flatten();
    if let Some(g) = &mut g {
        let (ww, wh) = super::display::render_size();
        let a = super::wide::set_aspect(Some(ww as f32 / wh.max(1) as f32));
        with_ctx(|ctx| g.resize(ctx, a, wh));
    }
    let ok = g.is_some();
    GLIDE.with(|s| *s.borrow_mut() = g);
    ok as u32
}

fn sst_win_close(_: &mut Cpu) -> u32 {
    with(|g, ctx| {
        g.casters.clear();
        match (&mut g.back, ctx) {
            (Back::Gl(b), Ctx::Gl(gl)) => b.close(gl),
            (Back::Vk(b), Ctx::Vk(v)) => b.close(v),
            _ => {}
        }
    });
    GLIDE.with(|s| *s.borrow_mut() = None);
    super::wide::set_aspect(None);
    1
}

fn buffer_clear(c: &mut Cpu) -> u32 {
    let color = arg(c, 0);
    with(|g, ctx| {
        g.flush(ctx);
        let ch = |sh: u32| (color >> sh & 0xff) as f32 / 255.0;
        let colour = [ch(16), ch(8), ch(0), 1.0];
        let d = g.draw_base();
        match (&mut g.back, ctx) {
            (Back::Gl(b), Ctx::Gl(gl)) => b.clear(gl, &d, colour),
            (Back::Vk(b), Ctx::Vk(v)) => b.clear(v, &d, colour),
            _ => {}
        }
    });
    0
}

fn buffer_swap(_: &mut Cpu) -> u32 {
    super::sample::start();
    let shown = with(|g, ctx| {
        g.flush(ctx);
        let (target, gamma) = (g.target, super::light::gamma(g.gamma));
        match (&mut g.back, ctx) {
            (Back::Gl(b), Ctx::Gl(gl)) => {
                b.resolve(gl, target);
                b.clear_gbuffer(gl);
                Some(Shown::Gl(b.color, target, gamma))
            }
            (Back::Vk(b), Ctx::Vk(v)) => b.finish(v).map(|view| Shown::Vk(view, target, gamma)),
            _ => None,
        }
    })
    .flatten();
    super::ao::clear();
    super::shadows::clear();
    if let Some(shown) = shown {
        match shown {
            Shown::Gl(tex, (w, h), gamma) => super::window::show_texture(tex, w, h, gamma),
            Shown::Vk(view, (w, h), gamma) => super::window::show_scene(view, w, h, gamma),
        }
        super::fps::frame_shown();
        super::jitter::next();
        // the next frame follows the window
        let (ww, wh) = super::display::render_size();
        let a = super::wide::set_aspect(Some(ww as f32 / wh.max(1) as f32));
        with(|g, ctx| g.resize(ctx, a, wh));
    }
    0
}

/// A picture to show at grBufferSwap.
enum Shown {
    Gl(glow::Texture, (u32, u32), f32),
    Vk(ash::vk::ImageView, (u32, u32), f32),
}

/// The coordinate space of the vertices that follow (None: screen pixels).
pub fn set_space(space: Option<(u32, u32)>) {
    with(|g, ctx| {
        let s = space.unwrap_or(g.res);
        if s != g.space {
            g.flush(ctx);
            g.space = s;
        }
    });
}

/// Whether the primitives that follow only cast the sun's shadow (`shadows::cast_kind`).
pub fn set_cast_only(kind: u8) {
    with(|g, _| g.cast_only = kind);
}

/// The 3D picture's primitives follow (`Some(dx)`: the x the walker adds to their units, the
/// view's origin in split screen), or not: they write the depth attachment for the AO.
pub fn set_gbuffer(on: Option<f32>) {
    with(|g, ctx| {
        if on != g.gbuf {
            g.flush(ctx);
            g.gbuf = on;
        }
    });
}

/// The 3D picture of a view is drawn (the HUD comes next): the ambient occlusion and the
/// dynamic lights over it, inside the clip window.
pub fn post_3d() {
    GLIDE.with(|g| {
        let mut g = g.borrow_mut();
        let Some(g) = g.as_mut() else { return };
        if super::window::with_gl(|w| {
            let (gl, vk) = w.parts();
            post_3d_with(g, gl, vk)
        })
        .is_none()
        {
            // the Vulkan renderer: no passes yet (PLAN-RTX.md step 6)
            super::window::with_vkr(|v| {
                g.flush(&mut Ctx::Vk(v));
                g.lights.clear();
                g.heat_sources.clear();
                g.casters.clear();
                let clip = g.scissor_box(g.state.clip);
                let Back::Vk(b) = &mut g.back else { return };
                // last: the picture back into 0..1 (the HUD follows unmapped)
                if g.target_hdr {
                    b.tonemap(v, clip, super::tonemap::curve());
                }
            });
        }
    });
}

fn post_3d_with(g: &mut Glide, gl: &glow::Context, mut vk: Option<&mut super::vk::Vk>) {
    {
        g.flush(&mut Ctx::Gl(gl));
        let clip = g.scissor_box(g.state.clip);
        let Back::Gl(b) = &mut g.back else { return };
        // FCOP_GBUFFER=scene: our scene's models as lines over the picture (PLAN-RTX.md step 2)
        super::scene::overlay(gl, b.draw_fbo(), g.target.0, g.target.1, super::wide::k());
        let _t = super::fps::timer(super::fps::Phase::Post);
        let lights = std::mem::take(&mut g.lights);
        let heat = std::mem::take(&mut g.heat_sources);
        let Some(d) = &b.gdepth else {
            // no depth effect on: only the tone mapping, on the framebuffer drawn into
            if g.target_hdr {
                let (target, out) = (g.target, b.draw_fbo());
                b.tonemap.get_or_insert_with(|| super::tonemap::Pass::new(gl)).run(gl, target, out, clip, super::tonemap::curve());
            }
            return;
        };
        let (tex, ms, colour) = (d.tex, d.ms, d.colour);
        let target = g.target;
        // the sun's shadow: screen-space, or the shadow map of this view's casters
        let next = super::shadows::next_view();
        let dir = next.sun;
        let casters = std::mem::take(&mut g.casters);
        let map = match super::shadows::map() {
            true => {
                let (textures, space) = ((b.array, b.alpha), (g.space.0 as f32, g.space.1 as f32));
                b.shadow_map.get_or_insert_with(|| super::shadows::ShadowMap::new(gl)).render(gl, &casters, &next, target, space, textures)
            }
            false => None,
        };
        g.casters = casters;
        g.casters.clear();
        // ray traced: the view's scene (`scene::overlay` above made it) against our depth
        // (with the dynamic lights' shadows: the strongest lights' rays go with the sun's)
        let merged = super::lights::merged(&lights, target);
        let casters = super::lights::casters(&merged);
        let rays = match (super::shadows::mode(), vk.as_deref_mut()) {
            (super::shadows::Mode::Rays, Some(vk)) => super::scene::take_frame().and_then(|frame| vk.sun_rays(gl, tex, ms, target, &frame, dir, &casters, super::ao::traced())),
            _ => None,
        };
        let sun = match (super::shadows::mode(), &map, rays) {
            (super::shadows::Mode::ScreenSpace, _, _) => super::ao::Sun::Ray(dir),
            (super::shadows::Mode::Map, Some(v), _) => super::ao::Sun::Map(v),
            (super::shadows::Mode::Rays, _, Some(t)) => super::ao::Sun::Traced(t, dir, super::ao::traced()),
            // ray traced chosen, no rays yet (GL presents until the next start): the map
            (super::shadows::Mode::Rays, Some(v), None) => super::ao::Sun::Map(v),
            _ => super::ao::Sun::None,
        };
        // FCOP_GBUFFER=shadows: the sun's shadow alone
        let ao = super::ao::enabled() && super::ao::debug_view() != 4;
        if ao || !matches!(sun, super::ao::Sun::None) {
            b.ssao.get_or_insert_with(|| super::ao::Ssao::new(gl)).run(gl, tex, ms, target, colour, clip, ao, sun);
        }
        if super::lights::enabled() {
            let (pos, col) = b.lighting.get_or_insert_with(|| super::lights::Lighting::new(gl)).run(gl, tex, ms, target, colour, clip, &merged, rays.filter(|_| !casters.is_empty()));
            g.smoke_lights.retain(|l| l.0 != clip);
            // a view's box changes with the window: the oldest go
            if g.smoke_lights.len() >= 4 {
                g.smoke_lights.remove(0);
            }
            g.smoke_lights.push((clip, pos, col));
        }
        if super::heat::enabled() {
            b.heat.get_or_insert_with(|| super::heat::Heat::new(gl)).run(gl, tex, ms, target, colour, clip, &heat);
        }
        // DLSS (PLAN-RTX.md step 4): the lit picture, its depth and motion vectors; bloom and tone
        // mapping work on its output
        let mut mvec = None;
        if let (Some(vk), Some(cam)) = (vk.as_deref_mut().filter(|_| super::vk::dlss_ready()), next.camera) {
            let m = b.motion.get_or_insert_with(|| super::motion::Motion::new(gl)).run(gl, tex, ms, target, colour, clip, next.index, cam);
            mvec = Some(m);
            let rect = clip.map(|v| v.max(0) as u32);
            let frame = super::streamline::Frame {
                index: super::jitter::picture(),
                view: next.index as u32,
                rect,
                out: rect,
                mode: super::streamline::Mode::Dlaa,
                jitter: super::jitter::now(),
                focal: 4096.0 * target.1 as f32 / 1920.0,
                near: super::vk::DLSS_NEAR,
                hdr: g.target_hdr,
                reset: false,
            };
            vk.dlss(gl, colour, tex, ms, m, target, clip, &frame);
        }
        if super::bloom::enabled() {
            b.bloom.get_or_insert_with(|| super::bloom::Bloom::new(gl)).run(gl, tex, ms, target, colour, clip);
        }
        // last: the picture back into 0..1 (the HUD follows unmapped)
        if g.target_hdr {
            b.tonemap.get_or_insert_with(|| super::tonemap::Pass::new(gl)).run(gl, target, colour, clip, super::tonemap::curve());
        }
        // the motion vectors (PLAN-RTX.md step 3): the camera's, from the depth (after the rest: FCOP_GBUFFER=motion shows them over the picture)
        if let Some(cam) = next.camera.filter(|_| super::motion::wanted() && mvec.is_none()) {
            b.motion.get_or_insert_with(|| super::motion::Motion::new(gl)).run(gl, tex, ms, target, colour, clip, next.index, cam);
        }
    }
}

/// Where the vertices that follow go in the frame.
pub fn set_view(view: View) {
    with(|g, ctx| {
        if view != g.view {
            g.flush(ctx);
            g.view = view;
        }
    });
}

fn draw_triangle(c: &mut Cpu) -> u32 {
    draw(glow::TRIANGLES, &[arg(c, 0), arg(c, 1), arg(c, 2)]);
    0
}

fn draw_line(c: &mut Cpu) -> u32 {
    draw(glow::TRIANGLES, &[arg(c, 0), arg(c, 1)]);
    0
}

fn draw_point(c: &mut Cpu) -> u32 {
    draw(glow::POINTS, &[arg(c, 0)]);
    0
}

fn clip_window(c: &mut Cpu) -> u32 {
    set_clip([arg(c, 0) as i32, arg(c, 1) as i32, arg(c, 2) as i32, arg(c, 3) as i32]);
    0
}

fn combine(c: &Cpu) -> [u32; 5] {
    [arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4)]
}

fn color_combine(c: &mut Cpu) -> u32 {
    set_color_combine(combine(c));
    0
}

fn alpha_combine(c: &mut Cpu) -> u32 {
    set_alpha_combine(combine(c));
    0
}

fn tex_combine(c: &mut Cpu) -> u32 {
    // grTexCombine(tmu, rgb function, factor, alpha function, factor, rgb invert, alpha invert):
    // the game passes the texture through (LOCAL, NONE)
    let (rgb, alpha) = (arg(c, 1), arg(c, 3));
    if rgb != 1 || alpha != 1 {
        log!("warn: grTexCombine({rgb}, {alpha}) taken as LOCAL");
    }
    0
}

fn constant_color(c: &mut Cpu) -> u32 {
    set_constant(arg(c, 0));
    0
}

fn alpha_blend(c: &mut Cpu) -> u32 {
    set_blend((arg(c, 0), arg(c, 1)));
    0
}

// ---- the calls the display list walker makes (the imports above and `walk.rs`) ----------------

/// Draws the guest vertices (Glide 2 GrVertex) at `verts`: a point, a line or a triangle.
pub fn draw(mode: u32, verts: &[u32]) {
    if traced(|| Call::Draw(mode, verts.iter().map(|&v| std::array::from_fn(|k| mem().rd32(v + 4 * k as u32))).collect())) {
        return;
    }
    with(|g, ctx| g.add(ctx, mode, verts));
}

pub fn set_clip(r: [i32; 4]) {
    if traced(|| Call::Clip(r)) {
        return;
    }
    with(|g, ctx| g.set(ctx, |s| s.clip = r));
}

pub fn set_color_combine(v: [u32; 5]) {
    if traced(|| Call::ColorCombine(v)) {
        return;
    }
    with(|g, _| g.cc = v);
}

pub fn set_alpha_combine(v: [u32; 5]) {
    if traced(|| Call::AlphaCombine(v)) {
        return;
    }
    with(|g, _| g.ac = v);
}

pub fn set_constant(v: u32) {
    if traced(|| Call::Constant(v)) {
        return;
    }
    with(|g, _| g.constant = v);
}

pub fn set_blend(b: (u32, u32)) {
    if traced(|| Call::Blend(b)) {
        return;
    }
    with(|g, ctx| g.set(ctx, |s| s.blend = b));
}

/// grTexSource with the texture's TMU address.
pub fn set_tex(addr: u32) {
    if traced(|| Call::Tex(addr)) {
        return;
    }
    with(|g, _| g.tex = Some(addr));
}

/// A call above, kept while tracing (`FCOP_WALK=check`: the original walker's calls and ours
/// compared); a draw keeps its vertices' 15 words.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Draw(u32, Vec<[u32; 15]>),
    Clip([i32; 4]),
    ColorCombine([u32; 5]),
    AlphaCombine([u32; 5]),
    Constant(u32),
    Blend((u32, u32)),
    Tex(u32),
}

static TRACING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

thread_local! {
    /// the calls kept, and whether they only are kept (not done)
    static TRACE: std::cell::RefCell<(Vec<Call>, bool)> = const { std::cell::RefCell::new((Vec::new(), false)) };
}

/// Keeps the calls from now on; with `dry` they are not done.
pub fn trace_begin(dry: bool) {
    TRACE.with_borrow_mut(|t| *t = (Vec::new(), dry));
    TRACING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The calls kept since `trace_begin`.
pub fn trace_end() -> Vec<Call> {
    TRACING.store(false, std::sync::atomic::Ordering::Relaxed);
    TRACE.with_borrow_mut(|t| std::mem::take(&mut t.0))
}

/// Keeps the call while tracing; true when it must not be done.
fn traced(call: impl FnOnce() -> Call) -> bool {
    if !TRACING.load(std::sync::atomic::Ordering::Relaxed) {
        return false;
    }
    TRACE.with_borrow_mut(|t| {
        t.0.push(call());
        t.1
    })
}

fn chromakey_mode(c: &mut Cpu) -> u32 {
    let on = arg(c, 0) != 0;
    with(|g, ctx| g.set(ctx, |s| s.chroma = on));
    0
}

fn chromakey_value(c: &mut Cpu) -> u32 {
    let k = arg(c, 0);
    with(|g, ctx| g.set(ctx, |s| s.key = k));
    0
}

fn tex_filter_mode(c: &mut Cpu) -> u32 {
    let linear = arg(c, 2) != 0;
    with(|g, ctx| g.set(ctx, |s| s.linear = linear));
    0
}

fn tex_clamp_mode(c: &mut Cpu) -> u32 {
    let cl = (arg(c, 1) != 0, arg(c, 2) != 0);
    with(|g, ctx| g.set(ctx, |s| s.clamp = cl));
    0
}

fn gamma_value(c: &mut Cpu) -> u32 {
    set_gamma(f32_arg(c, 0));
    0
}

/// grGammaCorrectionValue; Preferences' OK too (the game sets it only when Glide starts).
pub fn set_gamma(v: f32) {
    with(|g, _| g.gamma = if v > 0.0 { v } else { 1.0 });
}

fn tex_min_address(_: &mut Cpu) -> u32 {
    0
}

fn tex_max_address(_: &mut Cpu) -> u32 {
    // the last start address a 256x256 16-bit texture fits at
    TMU_RAM - 0x2_0000
}

/// GrTexInfo: smallLod, largeLod, aspect, format, data
fn tex_info(p: u32) -> (u32, u32, u32, u32, u32) {
    let m = mem();
    (m.rd32(p), m.rd32(p + 4), m.rd32(p + 8), m.rd32(p + 12), m.rd32(p + 16))
}

fn tex_mem_required(info: (u32, u32, u32, u32, u32)) -> u32 {
    let (small, large, aspect, format, _) = info;
    (large..=small.max(large)).map(|l| {
        let (w, h) = lod_size(l, aspect);
        w * h * bytes_per_texel(format)
    }).sum::<u32>().next_multiple_of(8)
}

fn tex_texture_mem_required(c: &mut Cpu) -> u32 {
    tex_mem_required(tex_info(arg(c, 1)))
}

fn tex_calc_mem_required(c: &mut Cpu) -> u32 {
    tex_mem_required((arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3), 0))
}

fn tex_download_mipmap(c: &mut Cpu) -> u32 {
    let (addr, info) = (arg(c, 1), tex_info(arg(c, 3)));
    let (_, large, aspect, format, data) = info;
    with(|g, ctx| g.download(ctx, addr, large, aspect, format, data, None));
    0
}

fn tex_download_mipmap_level(c: &mut Cpu) -> u32 {
    let (addr, this, large, aspect, format, data) = (arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5), arg(c, 7));
    if this == large {
        with(|g, ctx| g.download(ctx, addr, this, aspect, format, data, None));
    }
    0
}

fn tex_download_mipmap_level_partial(c: &mut Cpu) -> u32 {
    let (addr, this, large, aspect, format, data, r0, r1) = (arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5), arg(c, 7), arg(c, 8), arg(c, 9));
    if this == large {
        // `data` points at the first row downloaded
        with(|g, ctx| g.download(ctx, addr, this, aspect, format, data, Some((r0, r1))));
    }
    0
}

fn tex_source(c: &mut Cpu) -> u32 {
    set_tex(arg(c, 1));
    0
}

fn sst_screen_width(c: &mut Cpu) -> u32 {
    let w = with(|g, _| g.res.0).unwrap_or(640);
    c.r[EDX] = 0;
    w
}

fn sst_screen_height(_: &mut Cpu) -> u32 {
    with(|g, _| g.res.1).unwrap_or(480)
}

/// The target was made in RGBA16F (a curve chosen in Advanced Graphics takes effect when the
/// target is made anew, at the next swap: until then the copies keep its format).
static TARGET_HDR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The 3D picture's colour target format (internal format, texel type): RGBA16F while it was
/// made for a tone mapping curve (`tonemap`: values above 1.0 kept), else RGBA8 as before. The
/// post passes' copies of the picture take it too.
pub fn colour_format() -> (u32, u32) {
    if TARGET_HDR.load(Ordering::Relaxed) { (glow::RGBA16F, glow::HALF_FLOAT) } else { (glow::RGBA8, glow::UNSIGNED_BYTE) }
}

/// The Glide functions we implement; anything else the game asks for gets `not_implemented`.
const FUNCTIONS: &[(&str, fn(&mut Cpu) -> u32)] = &[
    ("grGlideInit", ret0),
    ("grGlideShutdown", ret0),
    ("grSstQueryBoards", sst_query_boards),
    ("grSstQueryHardware", sst_query_hardware),
    ("grSstSelect", ret0),
    ("grSstWinOpen", sst_win_open),
    ("grSstWinClose", sst_win_close),
    ("grSstIdle", ret0),
    ("grSstControl", ret1),
    ("grSstScreenWidth", sst_screen_width),
    ("grSstScreenHeight", sst_screen_height),
    ("grBufferClear", buffer_clear),
    ("grBufferSwap", buffer_swap),
    ("grDrawTriangle", draw_triangle),
    ("guDrawTriangleWithClip", draw_triangle),
    ("grDrawLine", draw_line),
    ("grDrawPoint", draw_point),
    ("grClipWindow", clip_window),
    ("grColorCombine", color_combine),
    ("grAlphaCombine", alpha_combine),
    ("grTexCombine", tex_combine),
    ("grConstantColorValue", constant_color),
    ("grAlphaBlendFunction", alpha_blend),
    ("grChromakeyMode", chromakey_mode),
    ("grChromakeyValue", chromakey_value),
    ("grTexFilterMode", tex_filter_mode),
    ("grTexClampMode", tex_clamp_mode),
    ("grTexMipMapMode", ret0),
    ("grDepthBufferMode", ret0),
    ("grDitherMode", ret0),
    ("grGammaCorrectionValue", gamma_value),
    ("grTexMinAddress", tex_min_address),
    ("grTexMaxAddress", tex_max_address),
    ("grTexTextureMemRequired", tex_texture_mem_required),
    ("grTexCalcMemRequired", tex_calc_mem_required),
    ("grTexDownloadMipMap", tex_download_mipmap),
    ("grTexDownloadMipMapLevel", tex_download_mipmap_level),
    ("grTexDownloadMipMapLevelPartial", tex_download_mipmap_level_partial),
    ("grTexSource", tex_source),
];

/// GetProcAddress on glide2x.dll: a thunk for the name (`_grName@argbytes`).
pub fn proc_address(name: &str) -> u32 {
    let plain = name.trim_start_matches('_');
    let (base, bytes) = plain.split_once('@').map_or((plain, 0), |(b, n)| (b, n.parse().unwrap_or(0)));
    let name = format!("glide2x!{base}");
    match FUNCTIONS.iter().find(|e| e.0 == base) {
        Some(&(_, f)) => thunk(&name, (bytes / 4) as i32, f),
        None => thunk_not_implemented(&name, (bytes / 4) as i32),
    }
}
