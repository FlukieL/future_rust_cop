//! Ambient occlusion (SSAO) for Glide missions: corners, the ground under robots and walls,
//! the gaps between buildings get darker, as light reaches them less.
//!
//! Depth: the game's display list holds only screen positions, so the projections tell it.
//! 0x457340 (object points, in place {sx, sy, z}) and 0x453740 (terrain vertex, out {sx, sy,
//! z / 16}: terrain coordinates are 16 times finer, so both are in object units) are hooked
//! (`wide.rs`) and every projected point's z is kept by its screen position in 2560x1920
//! units (`point`). The walker passes those units on unchanged (precise mode), so the Glide
//! renderer finds each 3D vertex's z by its position (`depths`): two points at the same place
//! (rare) are told apart by the primitive's other vertices; points the game made by clipping
//! at the near plane, and sprites, take the others' or none. Cleared at every frame shown.
//!
//! The renderer writes view-space z into a second colour attachment (RG32F, multisampled with
//! MSAA) for opaque 3D primitives, interpolated as 1/z; blended ones (smoke, glows) write
//! only a mask in its second channel, where the result is not darkened. When the HUD starts
//! (`walker.rs` after the 3D picture, `glide::post_3d`) the passes here run: AO at half
//! resolution (12 samples on a disc of `RADIUS` around each point, normals from the depth's
//! neighbours, occlusion = how far each sample rises above the surface's plane, within the
//! radius; the screen-space sun's shadow in a second channel), a blur in two passes along each
//! pixel's surface (each channel its own width), then the 3D picture times both, with
//! depth-aware upsampling (no values across an edge), inside the view's clip window (split
//! screen: each view its own).
//! The camera: sx = 1280 + x * 4096 / z (2560x1920 units, widescreen narrowed by k before), so a
//! target pixel is 4096 * height / 1920 pixels per unit at z = 1, in both directions.
//!
//! REG_DWORD `AmbientOcclusion` (default off), `FCOP_SSAO=0/1`; in the Advanced Graphics
//! dialog. `FCOP_GBUFFER=depth` or `=ao` shows the depth or the AO alone instead of the picture.

use super::gl::compile;
use super::light::Setting;
use glow::HasContext;
use std::collections::HashMap;
use std::cell::RefCell;
use std::sync::OnceLock;

static SETTING: Setting = Setting::with_default("AmbientOcclusion", "FCOP_SSAO", false);

pub fn enabled() -> bool {
    SETTING.get()
}

/// Something needs the depth (AO, dynamic lights, shadows, the heat shimmer, the depth buffer).
pub fn depth_wanted() -> bool {
    enabled() || super::zbuffer::enabled() || super::lights::enabled() || super::shadows::enabled() || super::heat::enabled() || super::bloom::enabled() || super::glide::perspective()
}

/// The Advanced Graphics dialog's OK: saved, and used from the next frame on.
pub fn set_enabled(on: bool) {
    SETTING.set(on);
}

/// `FCOP_GBUFFER`: 0 the picture, 1 the depth, 2 the AO (with the sun's shadow), 3 the dynamic
/// lights, 4 the sun's shadow alone.
pub(super) fn debug_view() -> i32 {
    static V: OnceLock<i32> = OnceLock::new();
    *V.get_or_init(|| match std::env::var("FCOP_GBUFFER").as_deref() {
        Ok("depth") => 1,
        Ok("ao") => 2,
        Ok("lights") => 3,
        Ok("shadows") => 4,
        _ => 0,
    })
}

/// A fast hash for the points' keys (std's SipHash cost most of the time spent here).
#[derive(Default, Clone, Copy)]
pub(crate) struct KeyHash(u64);

impl std::hash::Hasher for KeyHash {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u32(b as u32);
        }
    }
    fn write_u32(&mut self, k: u32) {
        self.0 = (self.0.rotate_left(5) ^ k as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    fn write_u64(&mut self, k: u64) {
        self.0 = (self.0.rotate_left(5) ^ k).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

type KeyMap<V> = HashMap<u64, V, std::hash::BuildHasherDefault<KeyHash>>;

/// A set of display list record addresses (looked up for every record each picture).
pub(crate) type RecordSet = std::collections::HashSet<u32, std::hash::BuildHasherDefault<KeyHash>>;

/// The cells `depth_near` looks in: this many units square.
const CELL: f32 = 160.0;

/// This frame's projections. The projection hooks and the renderer run on one thread (the
/// game's), so no lock; cleared by length, not freed.
#[derive(Default)]
struct Frame {
    /// screen position (x | y << 32, 2560x1920 units; the shadows' ring's points lie up to
    /// several screens off, beyond s16) -> up to two z (0: none) and whether each is the
    /// terrain's (a ground vertex and a model's can meet at one place)
    points: KeyMap<([i16; 2], [bool; 2])>,
    /// sprites: their rectangle (x0, y0, x1, y1 in 2560x1920 units) and z; their corners are
    /// not projected points
    sprites: Vec<([i16; 4], i16)>,
    /// the points by `CELL` (made at the first `depth_near` of a frame): cell -> (x, y, z)
    cells: KeyMap<Vec<(i32, i32, i16)>>,
    cells_made: bool,
    /// `depths`' scratch
    found: Vec<([i16; 2], [bool; 2])>,
    sure: Vec<i16>,
}

thread_local! {
    static FRAME: RefCell<Frame> = RefCell::new(Frame::default());
}

fn key(x: i32, y: i32) -> u64 {
    x as u32 as u64 | (y as u32 as u64) << 32
}

fn cell_of(x: f32, y: f32) -> (i32, i32) {
    ((x / CELL).floor() as i32, (y / CELL).floor() as i32)
}

fn cell_key((cx, cy): (i32, i32)) -> u64 {
    key(cx, cy)
}

/// A point the game projected to (x, y) at depth z (`terrain`: a ground vertex).
pub fn point(x: i32, y: i32, z: i16, terrain: bool) {
    if z <= 0 {
        return;
    }
    let _t = super::fps::timer(super::fps::Phase::Points);
    FRAME.with_borrow_mut(|f| {
        let (z2, t2) = f.points.entry(key(x, y)).or_insert(([0, 0], [false; 2]));
        if z2[0] == 0 || z2[0] == z {
            (z2[0], t2[0]) = (z, terrain);
        } else if z2[1] == 0 {
            (z2[1], t2[1]) = (z, terrain);
        }
        f.cells_made = false;
    });
}

/// A sprite the game projected to the centre (x, y), half size (w, h), at depth z.
pub fn sprite(x: i16, y: i16, w: i16, h: i16, z: i16) {
    if z <= 0 {
        return;
    }
    let (w, h) = (w.saturating_abs().max(1), h.saturating_abs().max(1));
    FRAME.with_borrow_mut(|f| f.sprites.push(([x.saturating_sub(w), y.saturating_sub(h), x.saturating_add(w), y.saturating_add(h)], z)));
}

/// The z of the nearest sprite whose rectangle holds (x, y).
fn sprite_at(sprites: &[([i16; 4], i16)], x: f32, y: f32) -> Option<i16> {
    let (x, y) = (x as i32, y as i32);
    sprites
        .iter()
        .filter(|(r, _)| (r[0] as i32..=r[2] as i32).contains(&x) && (r[1] as i32..=r[3] as i32).contains(&y))
        .map(|(_, z)| *z)
        .min()
}

/// The z of the projected point nearest to (x, y) within `reach` units (at most `CELL`; a glow
/// the game placed without projecting a point or a sprite there: a rocket's smoke trail).
pub fn depth_near(x: f32, y: f32, reach: f32) -> Option<f32> {
    let reach = reach.min(CELL);
    FRAME.with_borrow_mut(|f| {
        if !f.cells_made {
            let Frame { points, cells, .. } = f;
            cells.values_mut().for_each(Vec::clear);
            for (&k, (z, _)) in points.iter() {
                let (px, py) = (k as u32 as i32, (k >> 32) as u32 as i32);
                cells.entry(cell_key(cell_of(px as f32, py as f32))).or_default().push((px, py, z[0]));
            }
            f.cells_made = true;
        }
        let (cx, cy) = cell_of(x, y);
        (cy - 1..=cy + 1)
            .flat_map(|j| (cx - 1..=cx + 1).map(move |i| (i, j)))
            .filter_map(|c| f.cells.get(&cell_key(c)))
            .flatten()
            .map(|&(px, py, z)| ((px as f32 - x).hypot(py as f32 - y), z))
            .filter(|&(d, _)| d <= reach)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, z)| z as f32)
    })
}

/// At every frame shown: the next frame projects anew.
pub fn clear() {
    FRAME.with_borrow_mut(|f| {
        f.points.clear();
        f.sprites.clear();
        f.cells_made = false;
    });
}

/// The z of a primitive's vertices (x, y in 2560x1920 units) into `out`, 0 where none is known.
/// True when every vertex is a projected point (none taken from the others or a sprite).
pub fn depths(verts: impl ExactSizeIterator<Item = (f32, f32)> + Clone, out: &mut Vec<f32>) -> bool {
    let _t = super::fps::timer(super::fps::Phase::Depths);
    out.clear();
    let n = verts.len();
    FRAME.with_borrow_mut(|f| {
        let Frame { points, sprites, found, sure, .. } = f;
        found.clear();
        found.extend(verts.clone().map(|(x, y)| points.get(&key(x.round() as i32, y.round() as i32)).copied().unwrap_or(([0, 0], [false; 2]))));
        // a reference: the vertices with one candidate (their median), else any candidate
        sure.clear();
        sure.extend(found.iter().filter(|f| f.0[0] != 0 && f.0[1] == 0).map(|f| f.0[0]));
        // and what they are: a ground tile's corner where a model's point lies too is the
        // ground's (the nearer one by the reference tipped a tile over a pickup box)
        let (mut ground, mut model) = (0, 0);
        for f in found.iter().filter(|f| f.0[0] != 0 && f.0[1] == 0) {
            if f.1[0] { ground += 1 } else { model += 1 }
        }
        let kind = (ground != model).then_some(ground > model);
        sure.sort_unstable();
        let reference = sure.get(sure.len() / 2).copied().or_else(|| found.iter().find(|f| f.0[0] != 0).map(|f| f.0[0]));
        // none of its corners is a projected point: a sprite's, by its centre
        let reference = reference.or_else(|| {
            let k = n.max(1) as f32;
            let (cx, cy) = verts.clone().fold((0.0, 0.0), |a, v| (a.0 + v.0 / k, a.1 + v.1 / k));
            sprite_at(sprites, cx, cy)
        });
        let Some(r) = reference else {
            out.resize(n, 0.0);
            return false;
        };
        out.extend(found.iter().map(|&(z, t)| match z {
            [0, _] => r,
            [a, 0] => a,
            [a, b] => match kind {
                Some(k) if t[0] != t[1] => if t[0] == k { a } else { b },
                _ => if (a - r).abs() <= (b - r).abs() { a } else { b },
            },
        } as f32));
        found.iter().all(|f| f.0[0] != 0)
    })
}

/// The occlusion's reach in object units, and how strong it darkens (0...1).
const RADIUS: f32 = 400.0;
const STRENGTH: f32 = 1.0;

/// A rise above a point's surface smaller than this (object units, plus 0.4% of the depth) does
/// not occlude it (`FCOP_AO_STEP` for tests).
fn step() -> f32 {
    static S: OnceLock<f32> = OnceLock::new();
    *S.get_or_init(|| std::env::var("FCOP_AO_STEP").ok().and_then(|v| v.parse().ok()).unwrap_or(8.0))
}

pub(super) const VS: &str = r#"
void main() {
    gl_Position = vec4(float((gl_VertexID & 1) * 4 - 1), float((gl_VertexID >> 1) * 4 - 1), 0.0, 1.0);
}
"#;

/// Shared by the passes: the depth attachment (multisampled: sample 0) and view positions.
pub(super) const COMMON: &str = r#"
#ifdef MS
uniform sampler2DMS gdepth;
#else
uniform sampler2D gdepth;
#endif
uniform ivec2 size;     // the target in pixels
uniform float focal;    // pixels per unit at z = 1
vec2 G(ivec2 q) {
    return texelFetch(gdepth, clamp(q, ivec2(0), size - 1), 0).rg;
}
vec3 P(vec2 q, float z) {
    return vec3((q - vec2(size) * 0.5) * z / focal, z);
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
    // q's 1/z as each side predicts it (one pixel only where the second has none)
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
"#;

const AO_FS: &str = r#"
uniform float radius;
uniform float max_px;
uniform int use_ao;
uniform int sun_mode;     // the sun's shadow here: 0 none, 1 screen-space, 2 the map (shadows.rs)
uniform vec3 sun;         // toward the sun, view space
uniform float sun_len;
uniform float sun_k;
out vec4 o;
const int SAMPLES = 12;
uniform float STEP;
uniform sampler2D near_z;
void main() {
    ivec2 q = ivec2(gl_FragCoord.xy) * 2;
    float z = G(q).r;
    // r the AO, g the sun's light (blurred apart)
    if (z <= 0.0) { o = vec4(1.0); return; }
    vec3 p = P(vec2(q), z);
    vec3 n = N(q, z, p, 2);
    // a different turn of the samples at every pixel (interleaved gradient noise), blurred after
    float ign = fract(52.9829189 * fract(dot(gl_FragCoord.xy, vec2(0.06711056, 0.00583715))));
    float lit = sun_mode == 1 ? 1.0 - sun_k * shadow(p, n, sun, sun_len, ign, 16, 0.6)
              : sun_mode == 2 ? 1.0 - sun_k * sun_shadow(p, n) : 1.0;
    float rpx = min(radius * focal / z, max_px);
    if (use_ao == 0 || rpx < 2.0) { o = vec4(1.0, lit, 0.0, 1.0); return; }
    float a = 6.2831853 * ign;
    float occ = 0.0;
    float step_h = STEP + 0.004 * z;
    for (int i = 0; i < SAMPLES; i++) {
        float t = (float(i) + 0.5) / float(SAMPLES);
        float ang = a + float(i) * 2.3999632;
        vec2 s = vec2(q) + vec2(cos(ang), sin(ang)) * t * rpx;
        // the occluders from the nearest of 4x4 pixels: thin things and keyed holes (lattices,
        // fences) were hit or missed by turns as they moved past the samples: they flickered
        float zs = texelFetch(near_z, ivec2(s) / 4, 0).r;
        if (zs <= 0.0) continue;
        vec3 v = P(s, zs) - p;
        float d = length(v);
        if (d < 1e-3) continue;
        // a step of a few units (the depth is rough) is no occluder
        occ += max((dot(v, n) - step_h) / d - 0.05, 0.0) * (1.0 - smoothstep(0.5 * radius, radius, d));
    }
    o = vec4(pow(clamp(1.0 - 4.0 * occ / float(SAMPLES), 0.0, 1.0), 2.0), lit, 0.0, 1.0);
}
"#;

/// The half-resolution values blurred along `dir`, the AO (r) and the sun's light (g) each with
/// its own width. Neighbours count by how well they lie on the centre's surface: 1/z is linear
/// across a plane on the screen, so its slope (from the nearer neighbour) predicts theirs; a
/// plain depth difference left grazing ground unblurred.
/// The nearest depth of each 4x4 pixels (none: 0), the AO's occluders.
const NEAR_FS: &str = r#"
out vec4 o;
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
"#;

const BLUR_FS: &str = r#"
uniform sampler2D ao;
uniform ivec2 dir;
out vec4 o;
float Z(ivec2 h) { return G(h * 2).r; }
void main() {
    ivec2 p = ivec2(gl_FragCoord.xy);
    ivec2 hs = textureSize(ao, 0);
    vec2 c = texelFetch(ao, p, 0).rg;
    float z = Z(p);
    if (z <= 0.0) { o = vec4(c, 0.0, 1.0); return; }
    float iz = 1.0 / z;
    float za = Z(clamp(p + dir, ivec2(0), hs - 1)), zb = Z(clamp(p - dir, ivec2(0), hs - 1));
    float ga = za > 0.0 ? 1.0 / za - iz : 1e9, gb = zb > 0.0 ? iz - 1.0 / zb : 1e9;
    float g = abs(ga) < abs(gb) ? ga : gb;
    if (abs(g) > 1e8) g = 0.0;
    vec2 sum = c, wsum = vec2(1.0);
    for (int k = -6; k <= 6; k++) {
        ivec2 s = p + dir * k;
        if (k == 0 || any(lessThan(s, ivec2(0))) || any(greaterThanEqual(s, hs))) continue;
        float zs = Z(s);
        if (zs <= 0.0) continue;
        float e = abs(1.0 / zs - (iz + g * float(k))) / iz;
        float kk = float(k * k);
        vec2 w = vec2(exp(-kk / 10.0), exp(-kk / 18.0)) * max(1.0 - e / 0.04, 0.0);
        sum += texelFetch(ao, s, 0).rg * w;
        wsum += w;
    }
    o = vec4(sum / wsum, 0.0, 1.0);
}
"#;

const APPLY_FS: &str = r#"
uniform sampler2D ao;
uniform float strength;
uniform int show;       // 0 the AO as a factor, 1 the depth, 2 the AO alone
uniform int use_half;   // the half-resolution pass ran (AO, a half-resolution sun's shadow)
uniform int use_sun;    // the sun's shadow per pixel: 1 the map (shadows.rs), 2 traced by rays
uniform float sun_k;
uniform sampler2D rmask; // traced: G 1 where the ray toward the sun is blocked
uniform vec3 rsun;       // traced: toward the sun, view space
out vec4 o;
void main() {
    ivec2 q = ivec2(gl_FragCoord.xy);
    vec2 g = G(q);
    float z = g.r;
    if (show == 1) { o = vec4(vec3(z > 0.0 ? exp(-z / 6000.0) : 0.0), 1.0); return; }
    float v = 1.0;
    if (z > 0.0 && use_half != 0) {
        // the four nearest half-resolution values, weighed by how near their depth is (the
        // tolerance grows with the depth's slope here: grazing ground)
        float zr = G(q + ivec2(1, 0)).r, zl = G(q - ivec2(1, 0)).r;
        float zu = G(q + ivec2(0, 1)).r, zd = G(q - ivec2(0, 1)).r;
        float dr = zr > 0.0 ? abs(zr - z) : 1e9, dl = zl > 0.0 ? abs(zl - z) : 1e9;
        float du = zu > 0.0 ? abs(zu - z) : 1e9, dd = zd > 0.0 ? abs(zd - z) : 1e9;
        float tol = 0.02 * z + 2.0 * min(min(dr, dl) + min(du, dd), 0.1 * z);
        ivec2 hs = textureSize(ao, 0);
        vec2 h = (vec2(q) + 0.5) * 0.5 - 0.5;
        ivec2 b = ivec2(floor(h));
        vec2 f = h - floor(h);
        vec2 sum = vec2(0.0);
        float wsum = 0.0;
        for (int k = 0; k < 4; k++) {
            ivec2 o2 = ivec2(k & 1, k >> 1);
            ivec2 c = clamp(b + o2, ivec2(0), hs - 1);
            float zc = G(c * 2).r;
            float w = (o2.x == 1 ? f.x : 1.0 - f.x) * (o2.y == 1 ? f.y : 1.0 - f.y);
            w *= zc > 0.0 ? max(1.0 - abs(zc - z) / tol, 0.0) : 0.0;
            sum += texelFetch(ao, c, 0).rg * w;
            wsum += w;
        }
        vec2 a = sum / max(wsum, 1e-6);
        if (wsum < 1e-4) {
            // none on this surface (a thin thing, an edge): the nearest in depth around,
            // not a mix across the edge
            float best = 1e30;
            for (int j = -1; j <= 2; j++)
                for (int i = -1; i <= 2; i++) {
                    ivec2 c = clamp(b + ivec2(i, j), ivec2(0), hs - 1);
                    float zc = G(c * 2).r;
                    float d = zc > 0.0 ? abs(zc - z) : 1e29;
                    if (d < best) { best = d; a = texelFetch(ao, c, 0).rg; }
                }
            if (best >= 1e29) a = vec2(1.0);
        }
        v = a.r * a.g;
    }
    if (z > 0.0 && use_sun != 0) {
        vec3 p = P(vec2(q), z);
        vec3 n = N(q, z, p, 1);
        // traced: faces turned away and those the sun grazes are the game's own shading, as the map's
        float s = use_sun == 2 ? texelFetch(rmask, q, 0).g * smoothstep(0.02, 0.15, dot(n, normalize(rsun))) : sun_shadow(p, n);
        v *= 1.0 - sun_k * s;
    }
    if (show == 2) { o = vec4(vec3(v), 1.0); return; }
    o = vec4(vec3(mix(1.0, v, strength * (1.0 - clamp(g.g, 0.0, 1.0)))), 1.0);
}
"#;

/// The sun's shadow a view gets (`shadows.rs`).
#[derive(Clone, Copy)]
pub enum Sun<'a> {
    None,
    /// screen-space: toward the sun in view space
    Ray([f32; 3]),
    /// the view's shadow map
    Map(&'a super::shadows::View),
    /// traced by rays (`vk::Vk::sun_rays`): a texture whose G is 1 where blocked, and toward
    /// the sun in view space
    Traced(glow::Texture, [f32; 3]),
}

/// A half-resolution texture and its framebuffer.
type Half = (glow::Texture, glow::Framebuffer);

/// The passes' programs (single-sampled and multisampled depth), their half-resolution
/// textures and framebuffers.
pub struct Ssao {
    progs: [[glow::Program; 2]; 4],
    vao: glow::VertexArray,
    half: Option<((i32, i32), [Half; 2])>,
    /// `NEAR_FS`'s target (R32F, a quarter of the size)
    near: Option<((i32, i32), Half)>,
}

impl Ssao {
    pub fn new(gl: &glow::Context) -> Ssao {
        let prog = |fs: &str, ms: bool| compile(gl, VS, &format!("{}{COMMON}{}{}{fs}", if ms { "#define MS\n" } else { "" }, super::shadows::GLSL, super::shadows::SUN_GLSL));
        let progs = [AO_FS, BLUR_FS, APPLY_FS, NEAR_FS].map(|fs| [prog(fs, false), prog(fs, true)]);
        let vao = unsafe { gl.create_vertex_array().expect("GL vertex array") };
        Ssao { progs, vao, half: None, near: None }
    }

    pub fn delete(&mut self, gl: &glow::Context) {
        unsafe {
            for p in self.progs.iter().flatten() {
                super::gl::delete_program(gl, *p);
            }
            gl.delete_vertex_array(self.vao);
        }
        self.drop_half(gl);
    }

    fn drop_half(&mut self, gl: &glow::Context) {
        let near = self.near.take().map(|n| n.1);
        if let Some((_, h)) = self.half.take() {
            for (t, f) in h.into_iter().chain(near) {
                unsafe {
                    gl.delete_framebuffer(f);
                    gl.delete_texture(t);
                }
            }
        }
    }

    fn half(&mut self, gl: &glow::Context, size: (i32, i32)) -> [Half; 2] {
        if self.half.as_ref().is_some_and(|h| h.0 == size) {
            return self.half.as_ref().unwrap().1;
        }
        self.drop_half(gl);
        let make = || unsafe {
            let t = super::gl::texture(gl, false);
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RG8 as i32, size.0, size.1, 0, glow::RG, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
            let f = gl.create_framebuffer().expect("GL framebuffer");
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(t), 0);
            (t, f)
        };
        let h = [make(), make()];
        self.half = Some((size, h));
        h
    }

    /// The quarter-size nearest depth's texture and framebuffer.
    fn near(&mut self, gl: &glow::Context, size: (i32, i32)) -> Half {
        if let Some((s, n)) = self.near
            && s == size
        {
            return n;
        }
        if let Some((_, (t, f))) = self.near.take() {
            unsafe {
                gl.delete_framebuffer(f);
                gl.delete_texture(t);
            }
        }
        let n = unsafe {
            let t = super::gl::texture(gl, false);
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::R32F as i32, size.0, size.1, 0, glow::RED, glow::FLOAT, glow::PixelUnpackData::Slice(None));
            let f = gl.create_framebuffer().expect("GL framebuffer");
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(t), 0);
            (t, f)
        };
        self.near = Some((size, n));
        n
    }

    /// The passes over the 3D picture: `depth` the attachment (multisampled or not), `target`
    /// its size, `out` a framebuffer with only the picture's colour, `clip` the scissor box
    /// (x, y from below, w, h) the result is limited to; `ao` whether the AO is wanted, `sun`
    /// the sun's shadow (`Sun`).
    #[allow(clippy::too_many_arguments)]
    pub fn run(&mut self, gl: &glow::Context, depth: glow::Texture, ms: bool, target: (u32, u32), out: glow::Framebuffer, clip: [i32; 4], ao: bool, sun: Sun) {
        let look = super::shadows::look();
        // the sun's shadow in the half-resolution pass (1 screen-space, 2 the map at Low
        // quality), or the map per pixel in the last
        let (sun_half, sun_full) = match sun {
            Sun::Ray(_) => (1, false),
            Sun::Map(_) if !look.full => (2, false),
            Sun::Map(_) | Sun::Traced(..) => (0, true),
            Sun::None => (0, false),
        };
        let half_pass = ao || sun_half != 0;
        // the shadow map's uniforms (and texture, on unit 2) for a program
        let set_map = |u: &dyn Fn(&'static str) -> Option<glow::UniformLocation>| {
            if let Sun::Map(s) = sun {
                unsafe {
                    gl.active_texture(glow::TEXTURE2);
                    gl.bind_texture(glow::TEXTURE_2D, Some(s.tex));
                    gl.active_texture(glow::TEXTURE0);
                    let [x, y, z] = s.axes;
                    gl.uniform_3_f32(u("sx").as_ref(), x[0], x[1], x[2]);
                    gl.uniform_3_f32(u("sy").as_ref(), y[0], y[1], y[2]);
                    gl.uniform_3_f32(u("sz").as_ref(), z[0], z[1], z[2]);
                    gl.uniform_4_f32(u("srect").as_ref(), s.rect[0], s.rect[1], s.rect[2], s.rect[3]);
                    gl.uniform_2_f32(u("sdepth").as_ref(), s.depth[0], s.depth[1]);
                    gl.uniform_1_f32(u("sfar").as_ref(), super::shadows::far());
                    gl.uniform_1_i32(u("staps").as_ref(), look.taps);
                    gl.uniform_1_f32(u("sstep").as_ref(), look.step);
                }
            }
        };
        let (w, h) = (target.0 as i32, target.1 as i32);
        let hs = ((w + 1) / 2, (h + 1) / 2);
        let half = self.half(gl, hs);
        let qs = ((w + 3) / 4, (h + 3) / 4);
        let near = self.near(gl, qs);
        let focal = 4096.0 * h as f32 / 1920.0;
        let v = ms as usize;
        unsafe {
            gl.disable(glow::BLEND);
            gl.disable(glow::SCISSOR_TEST);
            gl.bind_vertex_array(Some(self.vao));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(if ms { glow::TEXTURE_2D_MULTISAMPLE } else { glow::TEXTURE_2D }, Some(depth));
            let common = |p: glow::Program| {
                gl.use_program(Some(p));
                let u = move |n: &'static str| super::gl::uniform(gl, p, n);
                gl.uniform_1_i32(u("gdepth").as_ref(), 0);
                gl.uniform_1_i32(u("ao").as_ref(), 1);
                // the shadow map's unit (a sampler of another type must not share a unit)
                gl.uniform_1_i32(u("smap").as_ref(), 2);
                gl.uniform_2_i32(u("size").as_ref(), w, h);
                gl.uniform_1_f32(u("focal").as_ref(), focal);
                gl.uniform_1_i32(u("near_z").as_ref(), 3);
                gl.uniform_1_i32(u("rmask").as_ref(), 4);
                u
            };
            if ao {
                // the occluders' nearest depth, on unit 3
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(near.1));
                gl.viewport(0, 0, qs.0, qs.1);
                let _ = common(self.progs[3][v]);
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
                gl.active_texture(glow::TEXTURE3);
                gl.bind_texture(glow::TEXTURE_2D, Some(near.0));
                gl.active_texture(glow::TEXTURE0);
            }
            if half_pass {
                // the AO (and the sun's shadow) at half resolution
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(half[0].1));
                gl.viewport(0, 0, hs.0, hs.1);
                let u = common(self.progs[0][v]);
                gl.uniform_1_f32(u("radius").as_ref(), RADIUS);
                gl.uniform_1_f32(u("STEP").as_ref(), step());
                gl.uniform_1_f32(u("max_px").as_ref(), h as f32 / 4.0);
                gl.uniform_1_i32(u("use_ao").as_ref(), ao as i32);
                gl.uniform_1_i32(u("sun_mode").as_ref(), sun_half);
                let d = if let Sun::Ray(d) = sun { d } else { [0.0, 1.0, 0.0] };
                gl.uniform_3_f32(u("sun").as_ref(), d[0], d[1], d[2]);
                gl.uniform_1_f32(u("sun_len").as_ref(), super::shadows::SUN_LENGTH);
                gl.uniform_1_f32(u("sun_k").as_ref(), super::shadows::sun_strength());
                if sun_half == 2 {
                    set_map(&u);
                }
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
                // blurred across, then down
                gl.active_texture(glow::TEXTURE1);
                for (from, to, dir) in [(0, 1, (1, 0)), (1, 0, (0, 1))] {
                    gl.bind_texture(glow::TEXTURE_2D, Some(half[from].0));
                    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(half[to].1));
                    let u = common(self.progs[1][v]);
                    gl.uniform_2_i32(u("dir").as_ref(), dir.0, dir.1);
                    gl.draw_arrays(glow::TRIANGLES, 0, 3);
                }
            }
            // the picture times the AO and the sun's shadow
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(half[0].0));
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(out));
            gl.viewport(0, 0, w, h);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(clip[0], clip[1], clip[2], clip[3]);
            let show = match debug_view() {
                4 => 2,
                v => v,
            };
            if show == 0 {
                gl.enable(glow::BLEND);
                gl.blend_func(glow::DST_COLOR, glow::ZERO);
            }
            let u = common(self.progs[2][v]);
            gl.uniform_1_f32(u("strength").as_ref(), STRENGTH);
            gl.uniform_1_i32(u("show").as_ref(), show);
            gl.uniform_1_i32(u("use_half").as_ref(), half_pass as i32);
            gl.uniform_1_i32(u("use_sun").as_ref(), if matches!(sun, Sun::Traced(..)) { 2 } else { sun_full as i32 });
            gl.uniform_1_f32(u("sun_k").as_ref(), super::shadows::sun_strength());
            if sun_full {
                set_map(&u);
            }
            if let Sun::Traced(t, d) = sun {
                gl.active_texture(glow::TEXTURE4);
                gl.bind_texture(glow::TEXTURE_2D, Some(t));
                gl.active_texture(glow::TEXTURE0);
                gl.uniform_3_f32(u("rsun").as_ref(), d[0], d[1], d[2]);
            }
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            if matches!(sun, Sun::Map(_)) {
                gl.active_texture(glow::TEXTURE2);
                gl.bind_texture(glow::TEXTURE_2D, None);
            }
            if matches!(sun, Sun::Traced(..)) {
                gl.active_texture(glow::TEXTURE4);
                gl.bind_texture(glow::TEXTURE_2D, None);
            }
            gl.disable(glow::BLEND);
            gl.disable(glow::SCISSOR_TEST);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
    }
}
