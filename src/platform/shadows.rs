//! Shadows for Glide missions (PLAN.md 2a step 3): the sun casts shadows of robots, buildings,
//! walls and ramps, and the strongest dynamic lights (`lights.rs`) are blocked by what stands
//! between them and a surface.
//!
//! The sun: a shadow map of the game's own triangles. The 3D picture's triangles that write
//! depth (opaque and alpha-blended solid ones, every corner with a depth) are kept as `glide`
//! draws them (its batched vertices, the chroma key in the constant colour's place). Each
//! vertex's place in view space is known from its screen position and depth (`ao.rs`), so
//! they are drawn a second time, orthographically from the sun, into a depth texture
//! (`ShadowMap::render`; keyed texels cut out: trees, fences). The AO's last pass looks every
//! pixel up in it (hardware PCF), at full resolution: the shapes stay sharp. Only what the game
//! draws casts: things just off the screen cast nothing (a shadow can pop in at the edge).
//! Casters farther than `FAR` are left out (the map's texels would grow), the shadow fades
//! out before it. (Before 2026-11-14 a screen-space ray march through the depth, at half
//! resolution and blurred with the AO: the shapes could not be made out.)
//!
//! The lights: per pixel a short ray march in view space toward the light through the depth
//! attachment: a step that lands behind the depth there, by less than an assumed thickness,
//! is in shadow (`GLSL`).
//!
//! The sun: a direction in the world (y up, from above and a little aside), turned into view
//! space by the camera's matrix. 0x434760 builds it at the camera [0x511f48] + 0x58: 3x3 i32,
//! 16.16, its rows the camera's right, up and forward in the world (yaw w: forward (sin w, 0,
//! cos w), right (cos w, 0, -sin w); pitch > 0 looks down). View space = game camera space:
//! x right, y up (0x457340: sy = 960 - y * 4096 / z), z ahead. The projection hooks (`wide.rs`)
//! note the camera of each view as it is drawn (`camera`), `glide::post_3d` takes them in that
//! order (split screen: two). The shaders' view space has x narrowed by widescreen's k (the
//! projection scales x before it): `next_view` turns the sun into it.
//!
//! The sun's shadow map is looked up in the AO's last pass (`SUN_GLSL`, also with the AO off),
//! at Low quality in its half-resolution pass, blurred with it; the screen-space one (the ray
//! march, `Mode::ScreenSpace`, the older way) there too. The lights' shadows go into the
//! lights' pass. REG_DWORD `Shadows` 0 off (default), 1 screen-space, 2 shadow map,
//! `FCOP_SHADOWS=0/1/2`; REG_DWORD `ShadowQuality` 0...3 Low / Medium (default) / High / Ultra
//! (`look`), `FCOP_SHADOW_QUALITY=low|mid|high|ultra`; combos in the Advanced Graphics dialog. `FCOP_GBUFFER=shadows` shows the sun's shadow alone;
//! `FCOP_SHADOW_FAR` (object units) for tests.

use super::advapi32;
use super::mem;
use crate::ver::Us;
use glow::HasContext;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::Mutex;

/// How the sun's shadow is made (REG_DWORD `Shadows`; 1 was the box's on).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Off = 0,
    /// a ray march through the depth (`GLSL`), at half resolution blurred with the AO
    ScreenSpace = 1,
    /// the shadow map (`ShadowMap`)
    Map = 2,
}

/// The modes as Advanced Graphics lists them.
pub const MODES: [(Mode, &str); 3] = [(Mode::Off, "Off"), (Mode::ScreenSpace, "Screen-Space"), (Mode::Map, "Shadow Map")];

/// The shadow map's quality (REG_DWORD `ShadowQuality`), as nuclear_rust_strike's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    Low = 0,
    Medium = 1,
    High = 2,
    Ultra = 3,
}

pub const QUALITIES: [(Quality, &str); 4] = [(Quality::Low, "Low"), (Quality::Medium, "Medium"), (Quality::High, "High"), (Quality::Ultra, "Ultra")];

/// A stored choice: a REG_DWORD, an environment variable (a number or a name) over it;
/// `u32::MAX` until read.
struct Choice {
    v: AtomicU32,
    value: &'static str,
    env: &'static str,
    names: &'static [&'static str],
    default: u32,
}

impl Choice {
    fn get(&self) -> u32 {
        let v = self.v.load(Relaxed);
        if v != u32::MAX {
            return v;
        }
        let env = std::env::var(self.env).ok().and_then(|e| e.parse().ok().or_else(|| self.names.iter().position(|n| n.eq_ignore_ascii_case(&e)).map(|i| i as u32)));
        let stored = || advapi32::value("", self.value).and_then(|(_, d)| Some(u32::from_le_bytes(d.get(..4)?.try_into().ok()?)));
        let v = env.or_else(stored).filter(|&v| (v as usize) < self.names.len()).unwrap_or(self.default);
        self.v.store(v, Relaxed);
        v
    }

    fn set(&self, v: u32) {
        self.v.store(v, Relaxed);
        advapi32::set_value("", self.value, 4, &v.to_le_bytes());
    }
}

static MODE: Choice = Choice { v: AtomicU32::new(u32::MAX), value: "Shadows", env: "FCOP_SHADOWS", names: &["off", "screen", "map"], default: 0 };
static QUALITY: Choice = Choice { v: AtomicU32::new(u32::MAX), value: "ShadowQuality", env: "FCOP_SHADOW_QUALITY", names: &["low", "mid", "high", "ultra"], default: 1 };

pub fn mode() -> Mode {
    match MODE.get() {
        1 => Mode::ScreenSpace,
        2 => Mode::Map,
        _ => Mode::Off,
    }
}

/// Any sun shadow (the lights' shadows come with it).
pub fn enabled() -> bool {
    mode() != Mode::Off
}

/// The shadow map is made (its casters kept).
pub fn map() -> bool {
    mode() == Mode::Map
}

/// The Advanced Graphics dialog's OK: saved, and used from the next frame on.
pub fn set_mode(m: Mode) {
    MODE.set(m as u32);
}

pub fn quality() -> Quality {
    QUALITIES[QUALITY.get() as usize % QUALITIES.len()].0
}

pub fn set_quality(q: Quality) {
    QUALITY.set(q as u32);
}

/// How a quality looks: the map's size in texels; the lookup's taps, (2r + 1)² hardware PCF
/// taps `step` texels apart (how soft the edge is); whether it is looked up for every pixel
/// in the AO's last pass, else at half resolution and blurred with the AO (very soft).
pub struct Look {
    pub size: i32,
    pub taps: i32,
    pub step: f32,
    pub full: bool,
}

pub fn look() -> Look {
    match quality() {
        Quality::Low => Look { size: 1024, taps: 1, step: 1.0, full: false },
        Quality::Medium => Look { size: 2048, taps: 2, step: 1.5, full: true },
        Quality::High => Look { size: 4096, taps: 2, step: 1.5, full: true },
        Quality::Ultra => Look { size: 4096, taps: 2, step: 1.0, full: true },
    }
}

const CAMERA: Us = Us(0x511f48);
/// The camera's matrix (rows: right, up, forward in the world; 16.16).
const MATRIX: u32 = 0x58;

/// Toward the sun in the world (y up): high, from the right and behind the start's view.
const SUN: [f32; 3] = [0.45, 1.0, -0.3];
/// How dark the sun's shadow is (0...1); how far the screen-space one reaches (object units).
pub const SUN_STRENGTH: f32 = 0.45;
pub const SUN_LENGTH: f32 = 900.0;
/// How far from the camera the shadow map's casters count (object units).
const FAR: f32 = 12000.0;

/// The cameras of this frame's views, in the order they were first projected with: the
/// camera's address and the sun in its view space.
static VIEWS: Mutex<Vec<(u32, [f32; 3])>> = Mutex::new(Vec::new());
/// The views' post passes so far this frame.
static DONE: Mutex<usize> = Mutex::new(0);

/// A projection hook: the camera in use draws a view.
pub fn camera() {
    let m = mem();
    let cam = m.rd32(CAMERA.at());
    if cam == 0 {
        return;
    }
    let mut v = VIEWS.lock().unwrap();
    if v.iter().any(|l| l.0 == cam) || v.len() >= 4 {
        return;
    }
    let row = |r: u32| (0..3).map(|c| m.rd32(cam + MATRIX + 12 * r + 4 * c) as i32 as f32 / 65536.0).collect::<Vec<_>>();
    let s = unit(SUN);
    let d = [0, 1, 2].map(|r| row(r).iter().zip(s).map(|(a, b)| a * b).sum::<f32>());
    v.push((cam, d));
}

/// At every frame shown: the next frame's views come anew.
pub fn clear() {
    VIEWS.lock().unwrap().clear();
    *DONE.lock().unwrap() = 0;
}

/// The post passes of the next view: toward the sun in its view space (the shaders').
pub fn next_view() -> [f32; 3] {
    let mut done = DONE.lock().unwrap();
    let v = VIEWS.lock().unwrap();
    let d = v.get(*done).or(v.last()).map_or([0.0, 1.0, 0.0], |l| l.1);
    *done += 1;
    unit([d[0] * super::wide::k(), d[1], d[2]])
}

/// A model face's winding test (called by 0x428690, model_face_draw, for its triangles and
/// quads): (p0, p1, p2) screen points (x | y << 16); 1 when it is turned away (the face is
/// left out), else 0.
pub const NCLIP: Us = Us(0x428c10);
/// The display list (`fps.rs`): its start, its end (where the next record goes), and how many
/// records it holds (0x45aeb3 makes it: malloc(0x13a0 * 0x38)).
const LIST_START: Us = Us(0x499b0c);
const LIST_END: Us = Us(0x511dc0);
const LIST_RECORDS: u32 = 0x13a0;

/// The most records the display list held at a turned-away face (`FCOP_SHADOW_LOG`).
static PEAK: AtomicU32 = AtomicU32::new(0);

thread_local! {
    /// The records of turned-away faces this picture: they only cast (`glide::set_cast_only`).
    static FACES: std::cell::RefCell<std::collections::HashSet<u32>> = Default::default();
}

/// The game leaves out a model's faces turned away from the camera, which the sun sees: with
/// the shadow map they are kept, their records only cast (our walker hands them to `glide` as
/// such). Only while the display list has room to spare (it is never checked).
pub fn nclip(c: &mut crate::rt::Cpu) {
    use crate::rt::EAX;
    crate::rt::call_original(c, NCLIP.at());
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let on = *ON.get_or_init(|| std::env::var("FCOP_SHADOW_FACES").map_or(true, |v| v != "0"));
    if c.r[EAX] == 0 || !on || !map() || !super::walker::precise() || !super::walk::ours() {
        return;
    }
    let m = mem();
    let end = m.rd32(LIST_END.at());
    let used = end.wrapping_sub(m.rd32(LIST_START.at())) / 0x38;
    PEAK.fetch_max(used, Relaxed);
    if used >= LIST_RECORDS * 3 / 4 {
        return;
    }
    // the face's record is the next one (0x428690 makes it right after)
    FACES.with_borrow_mut(|f| f.insert(end));
    c.r[EAX] = 0;
}

/// Some faces only cast this picture.
pub fn has_faces() -> bool {
    FACES.with_borrow(|f| !f.is_empty())
}

pub fn is_face(record: u32) -> bool {
    FACES.with_borrow(|f| f.contains(&record))
}

/// The picture is drawn: the list is made anew.
pub fn forget_faces() {
    static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if *LOG.get_or_init(|| std::env::var("FCOP_SHADOW_LOG").is_ok_and(|v| v != "0")) && N.fetch_add(1, Relaxed) % 300 == 0 {
        let peak = PEAK.swap(0, Relaxed);
        super::log!("shadows: {} turned-away faces cast, display list at most {peak} of {LIST_RECORDS}", FACES.with_borrow(|f| f.len()));
    }
    FACES.with_borrow_mut(|f| f.clear());
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn unit(a: [f32; 3]) -> [f32; 3] {
    let n = dot(a, a).sqrt().max(1e-9);
    a.map(|c| c / n)
}

/// The far end of the casters (`FCOP_SHADOW_FAR` for tests).
pub fn far() -> f32 {
    static F: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var("FCOP_SHADOW_FAR").ok().and_then(|v| v.parse().ok()).unwrap_or(FAR))
}

/// GLSL after `ao::COMMON`: how much of the way from p (normal n) toward `dir` (view space,
/// unit) over `len` units is blocked, 0...1; `j` a jitter 0...1; blockers past `fade` of the way
/// block less toward its end (1: never less). The dynamic lights' shadows.
pub const GLSL: &str = r#"
float shadow(vec3 p, vec3 n, vec3 dir, float len, float j, int steps, float fade) {
    float ndl = dot(n, dir);
    // faces turned away are the game's own shading
    if (ndl <= 0.02) return 0.0;
    // off the surface a little (rough depth), more where the ray grazes it
    vec3 o = p + n * (0.004 * p.z + 4.0) + dir * (0.006 * p.z / max(ndl, 0.2));
    // what lies behind a depth by more than this is not blocked by it (thin things)
    float thick = max(220.0, 0.02 * p.z);
    for (int i = 0; i < steps; i++) {
        float f = (float(i) + j) / float(steps);
        float t = len * f * f;
        vec3 s = o + dir * t;
        if (s.z <= 50.0) break;
        vec2 q = s.xy * focal / s.z + vec2(size) * 0.5;
        if (q.x < 0.0 || q.y < 0.0 || q.x >= float(size.x) || q.y >= float(size.y)) break;
        float zs = G(ivec2(q)).r;
        if (zs <= 0.0) continue;
        float dz = s.z - zs;
        if (dz > 0.003 * s.z + 2.0 && dz < thick) {
            // softer toward the shadow's end
            return f < fade ? 1.0 : 1.0 - smoothstep(fade, 1.0, f);
        }
    }
    return 0.0;
}
"#;

/// GLSL after `ao::COMMON`: how much of p (normal n) is in the sun's shadow, 0...1.
pub const SUN_GLSL: &str = r#"
uniform sampler2DShadow smap;
uniform vec3 sx, sy, sz;   // the sun's axes
uniform vec4 srect;
uniform vec2 sdepth;
uniform float sfar;
uniform int staps;         // the lookup's taps: (2 staps + 1)², `sstep` texels apart
uniform float sstep;
float sun_shadow(vec3 p, vec3 n) {
    // faces turned away are the game's own shading
    float ndl = dot(n, -sz);
    if (ndl <= 0.02) return 0.0;
    vec2 texel = (srect.zw - srect.xy) / vec2(textureSize(smap, 0));
    // off the surface by a texel or two (against acne), more where the sun grazes it
    vec3 o = p + n * (2.0 * max(texel.x, texel.y) / max(ndl, 0.25));
    vec3 l = vec3(dot(o, sx), dot(o, sy), dot(o, sz));
    vec2 uv = (l.xy - srect.xy) / (srect.zw - srect.xy);
    if (any(lessThan(uv, vec2(0.0))) || any(greaterThan(uv, vec2(1.0)))) return 0.0;
    float d = (l.z - sdepth.x) / (sdepth.y - sdepth.x) - 0.001;
    vec2 t = sstep / vec2(textureSize(smap, 0));
    float lit = 0.0;
    for (int j = -staps; j <= staps; j++)
        for (int i = -staps; i <= staps; i++)
            lit += texture(smap, vec3(uv + vec2(i, j) * t, d));
    float k = float(2 * staps + 1);
    // faces the sun grazes are mostly the game's own shading: faded in (they striped)
    return (1.0 - lit / (k * k)) * smoothstep(0.02, 0.15, ndl) * (1.0 - smoothstep(0.8 * sfar, sfar, p.z));
}
"#;

/// A view's shadow map as the lookup takes it: the texture, the sun's axes in view space
/// (right, up, away from the sun), the box drawn (x0, y0, x1, y1) and the depth range.
pub struct View {
    pub tex: glow::Texture,
    pub axes: [[f32; 3]; 3],
    pub rect: [f32; 4],
    pub depth: [f32; 2],
}

const CASTER_VS: &str = r#"
uniform vec2 space;      // the vertices' units
uniform vec2 size;       // the target in pixels
uniform float focal;
uniform vec3 ax, ay, az; // the sun's axes in view space
uniform vec4 rect;
uniform vec2 drange;
layout(location = 0) in vec2 pos;
layout(location = 2) in vec2 tc;
layout(location = 3) in vec4 kc;
layout(location = 4) in uvec4 mode;
layout(location = 5) in vec4 box;
layout(location = 6) in float z;
noperspective out vec2 st;
flat out vec4 key;
flat out uvec4 vmode;
flat out vec4 st_box;
void main() {
    st = tc;
    key = kc;
    vmode = mode;
    st_box = box;
    // as `ao::COMMON`'s P: target pixels from below, the camera at the centre
    vec2 q = vec2(pos.x / space.x * size.x, size.y - pos.y / space.y * size.y);
    vec3 p = vec3((q - 0.5 * size) * z / focal, z);
    vec3 l = vec3(dot(p, ax), dot(p, ay), dot(p, az));
    vec2 xy = (l.xy - rect.xy) / (rect.zw - rect.xy) * 2.0 - 1.0;
    float d = (l.z - drange.x) / (drange.y - drange.x) * 2.0 - 1.0;
    gl_Position = vec4(xy, d, 1.0);
}
"#;

const CASTER_FS: &str = r#"
uniform sampler2DArray tex;     // rgb * coverage, coverage
uniform sampler2DArray tex_a;   // alpha * coverage, alpha * (1 - coverage)
noperspective in vec2 st;
flat in vec4 key;        // the chroma key (rgb) and whether it is on (a)
flat in uvec4 vmode;
flat in vec4 st_box;
void main() {
    if (((vmode.z >> 2) & 1u) == 0u) return;
    int layer = int((vmode.z >> 8) & 0xffu);
    ivec2 s = ivec2(vmode.w & 0xffffu, vmode.w >> 16);
    ivec2 lo = ivec2(floor(st_box.xy));
    ivec2 p = clamp(ivec2(floor(st)), lo, max(ivec2(ceil(st_box.zw)) - 1, lo));
    p = ((p % s) + s) % s;
    vec4 c = texelFetch(tex, ivec3(p, layer), 0);
    vec4 a = texelFetch(tex_a, ivec3(p, layer), 0);
    // keyed out: no coverage where keying is on
    if (key.a > 0.5 && c.a < 0.5) discard;
    // the alpha combine takes the texture's alpha: cut where it is low
    uint other = (vmode.y >> 24) & 255u;
    if (other == 1u && a.r + a.g < 0.5) discard;
}
"#;

/// The shadow map: its program, vertex array and buffer, depth texture and framebuffer. Made
/// and used on the game's thread.
pub struct ShadowMap {
    prog: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    tex: glow::Texture,
    fbo: glow::Framebuffer,
    /// the texture's size (made anew when the quality changes it)
    size: i32,
}

/// The depth texture's storage, on the texture bound.
fn storage(gl: &glow::Context, size: i32) {
    unsafe { gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::DEPTH_COMPONENT24 as i32, size, size, 0, glow::DEPTH_COMPONENT, glow::UNSIGNED_INT, glow::PixelUnpackData::Slice(None)) };
}

impl ShadowMap {
    pub fn new(gl: &glow::Context) -> ShadowMap {
        let prog = super::gl::compile(gl, CASTER_VS, CASTER_FS);
        unsafe {
            let vao = gl.create_vertex_array().expect("GL vertex array");
            let vbo = gl.create_buffer().expect("GL buffer");
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            super::glide::vertex_layout(gl);
            gl.bind_vertex_array(None);
            let tex = gl.create_texture().expect("GL texture");
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            let size = look().size;
            storage(gl, size);
            for (p, v) in [
                (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_COMPARE_MODE, glow::COMPARE_REF_TO_TEXTURE),
                (glow::TEXTURE_COMPARE_FUNC, glow::LEQUAL),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, p, v as i32);
            }
            gl.bind_texture(glow::TEXTURE_2D, None);
            let fbo = gl.create_framebuffer().expect("GL framebuffer");
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::DEPTH_ATTACHMENT, glow::TEXTURE_2D, Some(tex), 0);
            gl.draw_buffers(&[]);
            gl.read_buffer(glow::NONE);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            ShadowMap { prog, vao, vbo, tex, fbo, size }
        }
    }

    pub fn delete(&mut self, gl: &glow::Context) {
        unsafe {
            super::gl::delete_program(gl, self.prog);
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
            gl.delete_framebuffer(self.fbo);
            gl.delete_texture(self.tex);
        }
    }

    /// Draw the casters (`glide`'s batched vertices, triangles; x, y in `space` units) from
    /// the sun `sun` (view space, toward it); `target` the picture's size in pixels,
    /// `textures` the texture arrays (colour, alpha). None: nothing casts.
    pub fn render(&mut self, gl: &glow::Context, casters: &[u32], sun: [f32; 3], target: (u32, u32), space: (f32, f32), textures: (glow::Texture, glow::Texture)) -> Option<View> {
        use super::glide::VERTEX;
        let (tw, th) = (target.0 as f32, target.1 as f32);
        let focal = 4096.0 * th / 1920.0;
        // the sun's axes: away from it, and two across
        let az = sun.map(|c| -c);
        let up = if az[1].abs() > 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
        let ax = unit(cross(up, az));
        let ay = cross(az, ax);
        // the bounds in the sun's space of the casters nearer than `far`
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        let far = far();
        for v in casters.chunks_exact(VERTEX) {
            let (x, y, z) = (f32::from_bits(v[0]), f32::from_bits(v[1]), f32::from_bits(v[20]));
            if z > far {
                continue;
            }
            let q = (x / space.0 * tw, th - y / space.1 * th);
            let p = [(q.0 - 0.5 * tw) * z / focal, (q.1 - 0.5 * th) * z / focal, z];
            let l = [dot(p, ax), dot(p, ay), dot(p, az)];
            for i in 0..3 {
                lo[i] = lo[i].min(l[i]);
                hi[i] = hi[i].max(l[i]);
            }
        }
        if lo[0] > hi[0] {
            return None;
        }
        let size = look().size;
        if size != self.size {
            self.size = size;
            unsafe { gl.bind_texture(glow::TEXTURE_2D, Some(self.tex)) };
            storage(gl, size);
            unsafe { gl.bind_texture(glow::TEXTURE_2D, None) };
        }
        let pad = |a: f32, b: f32| (b - a).max(1.0) * 0.01;
        let rect = [lo[0] - pad(lo[0], hi[0]), lo[1] - pad(lo[1], hi[1]), hi[0] + pad(lo[0], hi[0]), hi[1] + pad(lo[1], hi[1])];
        let depth = [lo[2] - pad(lo[2], hi[2]), hi[2] + pad(lo[2], hi[2])];
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.viewport(0, 0, size, size);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.disable(glow::CULL_FACE);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.depth_mask(true);
            gl.clear_depth_f32(1.0);
            gl.clear(glow::DEPTH_BUFFER_BIT);
            gl.use_program(Some(self.prog));
            let p = self.prog;
            let u = |n: &'static str| super::gl::uniform(gl, p, n);
            gl.uniform_2_f32(u("space").as_ref(), space.0, space.1);
            gl.uniform_2_f32(u("size").as_ref(), tw, th);
            gl.uniform_1_f32(u("focal").as_ref(), focal);
            gl.uniform_3_f32(u("ax").as_ref(), ax[0], ax[1], ax[2]);
            gl.uniform_3_f32(u("ay").as_ref(), ay[0], ay[1], ay[2]);
            gl.uniform_3_f32(u("az").as_ref(), az[0], az[1], az[2]);
            gl.uniform_4_f32(u("rect").as_ref(), rect[0], rect[1], rect[2], rect[3]);
            gl.uniform_2_f32(u("drange").as_ref(), depth[0], depth[1]);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(textures.0));
            gl.uniform_1_i32(u("tex").as_ref(), 0);
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(textures.1));
            gl.uniform_1_i32(u("tex_a").as_ref(), 1);
            gl.bind_vertex_array(Some(self.vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            let bytes = std::slice::from_raw_parts(casters.as_ptr() as *const u8, casters.len() * 4);
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STREAM_DRAW);
            gl.draw_arrays(glow::TRIANGLES, 0, (casters.len() / VERTEX) as i32);
            gl.bind_vertex_array(None);
            gl.disable(glow::DEPTH_TEST);
            gl.bind_texture(glow::TEXTURE_2D_ARRAY, None);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D_ARRAY, None);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        Some(View { tex: self.tex, axes: [ax, ay, az], rect, depth })
    }
}
