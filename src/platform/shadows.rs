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

/// The shadow map is offered in Advanced Graphics (off 2026-11-16 as not finished, offered
/// again 2026-11-19 for testing;
/// `FCOP_SHADOWS=2` turns it on as well).
pub const MAP_OFFERED: bool = true;

/// The modes as Advanced Graphics lists them (the map only with `MAP_OFFERED`).
pub const MODES: &[(Mode, &str)] = if MAP_OFFERED {
    &[(Mode::Off, "Off"), (Mode::ScreenSpace, "Screen-Space"), (Mode::Map, "Shadow Map")]
} else {
    &[(Mode::Off, "Off"), (Mode::ScreenSpace, "Screen-Space")]
};

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
        2 if MAP_OFFERED || std::env::var_os("FCOP_SHADOWS").is_some() => Mode::Map,
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
pub const SUN: [f32; 3] = [0.45, 1.0, -0.3];
/// How dark the sun's shadow is (0...1); how far the screen-space one reaches (object units).
pub const SUN_STRENGTH: f32 = 0.45;
pub const SUN_LENGTH: f32 = 900.0;
/// How far from the camera the shadow map's casters count (object units). Beyond the game's
/// own drawing in its usual view from above (casters there reach ~20000, half of them past
/// 8000): 12000 cut a sixth of them and faded the shadow out from 9600, the upper half of the
/// picture, so shadows faded in as the camera came nearer (walking forward).
const FAR: f32 = 40000.0;

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

/// Toward the sun (unit) in the view space of the camera in use (x right, y up, z ahead: the
/// game's camera space).
pub fn sun_view() -> [f32; 3] {
    let m = mem();
    let cam = m.rd32(CAMERA.at());
    if cam == 0 {
        return [0.0, 1.0, 0.0];
    }
    let s = unit(SUN);
    [0, 1, 2].map(|r| (0..3).map(|c| m.rd32(cam + MATRIX + 12 * r + 4 * c) as i32 as f32 / 65536.0 * s[c as usize]).sum::<f32>())
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

/// What this picture's extra records are for: turned-away faces (`nclip`, record addresses)
/// and the ring's (`actors_draw`, ranges of records), both only casting.
#[derive(Default)]
struct Extra {
    faces: std::collections::HashSet<u32>,
    ring: Vec<(u32, u32)>,
    /// the ring's actors this picture, and the extra triangles kept as casters or not (no
    /// depth) by kind (`FCOP_SHADOW_LOG`)
    ring_actors: u32,
    ring_objects: u32,
    cast: [[u32; 2]; 3],
}

thread_local! {
    static EXTRA: std::cell::RefCell<Extra> = Default::default();
}

/// A test switch (`FCOP_<name>=0` off).
fn switch(name: &str, cell: &'static std::sync::OnceLock<bool>) -> bool {
    *cell.get_or_init(|| std::env::var(name).map_or(true, |v| v != "0"))
}

/// The display list has room to spare (the game never checks it).
fn room() -> bool {
    let m = mem();
    let used = m.rd32(LIST_END.at()).wrapping_sub(m.rd32(LIST_START.at())) / 0x38;
    PEAK.fetch_max(used, Relaxed);
    used < LIST_RECORDS * 3 / 4
}

/// The game leaves out a model's faces turned away from the camera, which the sun sees: with
/// the shadow map they are kept, their records only cast (our walker hands them to `glide` as
/// such). Only while the display list has room to spare.
pub fn nclip(c: &mut crate::rt::Cpu) {
    use crate::rt::EAX;
    crate::rt::call_original(c, NCLIP.at());
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if c.r[EAX] == 0 || !switch("FCOP_SHADOW_FACES", &ON) || !map() || !super::walker::precise() || !super::walk::ours() || !room() {
        return;
    }
    // the face's record is the next one (0x428690 makes it right after)
    let end = mem().rd32(LIST_END.at());
    EXTRA.with_borrow_mut(|e| e.faces.insert(end));
    c.r[EAX] = 0;
}

/// Terrain tiles turned away from the camera are kept as casters too (`terrain::tiles`; walls
/// seen from their back threw no shadow until turned toward the camera): on with the turned-away
/// faces, our terrain pass (not its check), only while the display list has room.
pub fn keep_hidden() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    switch("FCOP_SHADOW_FACES", &ON) && map() && super::walker::precise() && super::walk::ours() && super::terrain::ours() && room()
}

/// The next record (at the display list's end) is a turned-away terrain tile: only casting.
/// False when the list has no room left (then it is left out).
pub fn note_hidden() -> bool {
    if !room() {
        return false;
    }
    let end = mem().rd32(LIST_END.at());
    EXTRA.with_borrow_mut(|e| e.faces.insert(end));
    true
}

/// actors_draw(view): message 3 (draw) to each actor on the view's list of visible ones: count
/// [0x4a3ac0 + 4 * view], list [0x4a31a0 + 4 * view] (actor pointers); then its second loop
/// (0x427960 for some). Called by render_world 0x453260 after the terrain and the static
/// objects.
pub const ACTORS_DRAW: Us = Us(0x4532f0);
const VISIBLE_COUNT: Us = Us(0x4a3ac0);
const VISIBLE_LIST: Us = Us(0x4a31a0);
/// The actors (`cheats.rs`): count, pointers; an actor's handler at +4, active while +8 > 0.
const ACTOR_COUNT: Us = Us(0x50e420);
const ACTORS: Us = Us(0x50e428);
/// Nonzero in split screen.
const SPLIT: Us = Us(0x511f54);
/// The ring's projection is zoomed out this much (x and y times it), which keeps points off the
/// screen inside s16.
pub const RING_ZOOM: f32 = 0.5;

/// The ring: things off the screen cast into it. After the view's actors, the static objects
/// around the view (`static_objects`) and the other actors are drawn too, with the projection
/// zoomed out (`wide::set_zoom`): while it is, the game's object test 0x4572a0 passes what may
/// cast into the view (`wide::object_visible`: the sphere or its shadow inside it) and its faces
/// count as on the screen (`wide::points`). Their records only cast, their corners taken back to
/// their real place (`glide`'s cast-only primitives with a zoom), their projected points noted at
/// it (`ao::point`). Not in split screen; only while the display list has room.
pub fn actors_draw(c: &mut crate::rt::Cpu) {
    let m = mem();
    let view = m.rd32(c.r[crate::rt::ESP] + 4);
    crate::rt::call_original(c, ACTORS_DRAW.at());
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !switch("FCOP_SHADOW_RING", &ON) || !map() || !super::walker::precise() || !super::walk::ours() || m.rd32(SPLIT.at()) != 0 || view > 3 {
        return;
    }
    let (n, list) = (m.rd32(VISIBLE_COUNT.at() + 4 * view) as i32, m.rd32(VISIBLE_LIST.at() + 4 * view));
    let visible: Vec<u32> = (0..n.clamp(0, 4096) as u32).map(|i| m.rd32(list + 4 * i)).collect();
    let regs = c.r;
    let from = m.rd32(LIST_END.at());
    let mut drawn = 0;
    super::wide::set_zoom(RING_ZOOM);
    let objects = static_objects(c, view);
    for i in 0..(m.rd32(ACTOR_COUNT.at()) as i32).clamp(0, 4096) as u32 {
        let a = m.rd32(ACTORS.at() + 4 * i);
        if a == 0 || m.rd32(a + 8) as i32 <= 0 {
            continue;
        }
        SHOWN.store(visible.contains(&a) && !ring_all(), Relaxed);
        if !room() {
            break;
        }
        super::guest_call(c, m.rd32(a + 4), &[3, a, 0, 0]);
        drawn += 1;
        if c.stop != 0 {
            break;
        }
    }
    SHOWN.store(false, Relaxed);
    super::wide::set_zoom(1.0);
    c.r = regs;
    let to = m.rd32(LIST_END.at());
    EXTRA.with_borrow_mut(|e| {
        if to > from {
            e.ring.push((from, to));
        }
        e.ring_actors += drawn;
        e.ring_objects += objects;
    });
}

/// The static objects' grid (trees, lamps, wall parts...): cells of 0x20000 (20.12) in rows
/// of [0x4a13f4] cells, [0x4a1400] rows; 8 bytes a cell: +0 first object (linked by +0x58),
/// +4 the game time it was last drawn. 0 without one.
const GRID: Us = Us(0x4a13fc);
const GRID_WIDTH: Us = Us(0x4a13f4);
const GRID_HEIGHT: Us = Us(0x4a1400);
/// The view's window of cells static_objects_draw 0x42fe20 draws: first row and column (bytes,
/// [0x4a3b00 + 2 * view] and + 1, set by render_visibility 0x452880), 5x5 entries of 0x14 bytes
/// at 0x50dc80 + 0x1f4 * view, +0 the mask of a cell's 4x4 parts seen (0x452a20); an object is
/// drawn when its +0x4e has one of them.
const WINDOW: Us = Us(0x4a3b00);
const ENTRIES: Us = Us(0x50dc80);
const MODEL_DRAW: Us = Us(0x4280a0);
/// The ring reaches this many cells beyond the view's window on every side.
const RING_CELLS: i32 = 2;

/// The ring's static objects: the objects of the cells around the view's window and those in
/// it the game left out (none of their parts seen), drawn with model_draw 0x4280a0 alone (the
/// ring's object test is in it). Not 0x42fe20: that also advances their animations and marks
/// the cells as seen (logic). An object's place is the one it had when last drawn (static ones
/// do not move; spinning ones turn only while seen). Returns how many were drawn.
/// `FCOP_SHADOW_STATIC=0` off.
fn static_objects(c: &mut crate::rt::Cpu, view: u32) -> u32 {
    let m = mem();
    let grid = m.rd32(GRID.at());
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if grid == 0 || !switch("FCOP_SHADOW_STATIC", &ON) {
        return 0;
    }
    let (w, h) = (m.rd32(GRID_WIDTH.at()) as i32, m.rd32(GRID_HEIGHT.at()) as i32);
    let (a, b) = (m.rd8(WINDOW.at() + 2 * view) as i32, m.rd8(WINDOW.at() + 2 * view + 1) as i32);
    let entries = ENTRIES.at() + 0x1f4 * view;
    // model_draw's flags (a dword, 0) below the guest stack
    c.r[crate::rt::ESP] -= 16;
    let flags = c.r[crate::rt::ESP];
    let mut drawn = 0;
    'cells: for row in -RING_CELLS..5 + RING_CELLS {
        for col in -RING_CELLS..5 + RING_CELLS {
            let (r, k) = (a + row, b + col);
            if !(0..h).contains(&r) || !(0..w).contains(&k) {
                continue;
            }
            let seen = if ring_all() {
                0
            } else if (0..5).contains(&row) && (0..5).contains(&col) { m.rd32(entries + 0x14 * (row * 5 + col) as u32) } else { 0 };
            let mut o = m.rd32(grid + 8 * (r * w + k) as u32);
            let mut n = 0;
            while o != 0 && n < 4096 {
                if m.rd8(o + 0x4c) & 0x10 == 0 && m.rd32(o) != 0 {
                    SHOWN.store(m.rd16(o + 0x4e) as u32 & seen != 0, Relaxed);
                    if !room() {
                        break 'cells;
                    }
                    m.wr32(flags, 0);
                    super::guest_call(c, MODEL_DRAW.at(), &[o, flags, 1]);
                    drawn += 1;
                    if c.stop != 0 {
                        break 'cells;
                    }
                }
                o = m.rd32(o + 0x58);
                n += 1;
            }
        }
    }
    drawn
}

/// The thing the ring draws now is one the game tried itself (a visible actor, an object of a
/// cell part seen): those lists go by cells, so the game's own view test may have left it out
/// although its shadow reaches into the view (it came with the thing). `wide::object_visible`
/// leaves out only those whose sphere is in the view: the game drew them.
static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn ring_shown() -> bool {
    SHOWN.load(Relaxed)
}

/// `FCOP_SHADOW_RING=all` (a test): the ring draws the visible actors and static objects too, its shadows
/// should lie exactly on theirs.
fn ring_all() -> bool {
    static A: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *A.get_or_init(|| std::env::var("FCOP_SHADOW_RING").is_ok_and(|v| v == "all"))
}

/// How far beyond a thing the ring looks for its shadow along the sun's rays (object units, on
/// top of 2.3 radii): a thing this high above the ground throws its shadow ~1.1 times as far.
/// `FCOP_SHADOW_REACH` for tests.
const RING_REACH: f32 = 2000.0;

pub fn ring_reach() -> f32 {
    static R: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *R.get_or_init(|| std::env::var("FCOP_SHADOW_REACH").ok().and_then(|v| v.parse().ok()).unwrap_or(RING_REACH))
}

/// `FCOP_SHADOW_RING=wide` (a test): the ring draws everything beyond the near plane, not only
/// what may cast into the view; the shadows should be the same.
pub fn ring_wide() -> bool {
    static A: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *A.get_or_init(|| std::env::var("FCOP_SHADOW_RING").is_ok_and(|v| v == "wide"))
}

/// `glide` kept an extra triangle of `kind` (`cast_kind`) as a caster, or not.
pub fn note_cast(kind: u8, ok: bool) {
    EXTRA.with_borrow_mut(|e| e.cast[kind as usize % 3][ok as usize] += 1);
}

/// Some records only cast this picture.
pub fn has_cast() -> bool {
    EXTRA.with_borrow(|e| !e.faces.is_empty() || !e.ring.is_empty())
}

/// How a record is drawn: 0 as usual, 1 only casting, 2 only casting from the ring (its
/// corners zoomed by `RING_ZOOM`).
pub fn cast_kind(record: u32) -> u8 {
    EXTRA.with_borrow(|e| {
        if e.ring.iter().any(|&(a, b)| (a..b).contains(&record)) {
            2
        } else {
            e.faces.contains(&record) as u8
        }
    })
}

/// The picture is drawn: the list is made anew.
pub fn forget_faces() {
    static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let log = *LOG.get_or_init(|| std::env::var("FCOP_SHADOW_LOG").is_ok_and(|v| v != "0")) && N.fetch_add(1, Relaxed) % 300 == 0;
    EXTRA.with_borrow_mut(|e| {
        if log {
            let peak = PEAK.swap(0, Relaxed);
            let ring: u32 = e.ring.iter().map(|&(a, b)| (b - a) / 0x38).sum();
            super::log!("shadows: {} turned-away faces cast, ring {} actors {} static objects {ring} records, display list at most {peak} of {LIST_RECORDS}; casters kept/without depth: faces {}/{}, ring {}/{}", e.faces.len(), e.ring_actors, e.ring_objects, e.cast[1][1], e.cast[1][0], e.cast[2][1], e.cast[2][0]);
        }
        e.faces.clear();
        e.ring.clear();
        e.ring_actors = 0;
        e.ring_objects = 0;
        e.cast = [[0; 2]; 3];
    });
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
    /// the caster buffer's size in bytes (kept: made anew each picture, ~1 MB with the rings,
    /// it cost the driver an allocation each time)
    cap: usize,
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
            ShadowMap { prog, vao, vbo, tex, fbo, size, cap: 0 }
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
            if bytes.len() > self.cap {
                self.cap = bytes.len().next_power_of_two();
                gl.buffer_data_size(glow::ARRAY_BUFFER, self.cap as i32, glow::STREAM_DRAW);
            }
            gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, bytes);
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
