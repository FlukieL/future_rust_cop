//! Widescreen (Hor+) in Glide missions. The game projects into a fixed 4:3 screen of 2560x1920
//! units (`sx = 1280 + x * 4096 / z`). Narrowing camera-space x by k = (4/3) / aspect before
//! its projections and view tests puts the wider view into that screen; the renderer
//! stretches the 3D picture over the whole wide frame and draws the HUD in 4:3 parts
//! (`walker.rs`). Target markers are drawn at HUD level but belong to the 3D picture, so they
//! are moved back to their place (`to_screen`).
//!
//! The aspect follows the window (set at every frame shown, `set_aspect`). The player turns it
//! on or off in Preferences (REG_DWORD `Widescreen` in the game's key, default on);
//! `FCOP_WIDE=0` turns it off, `FCOP_WIDE=<aspect>` forces one.

use crate::ver::Us;
use super::{advapi32, mem};
use crate::rt::{Cpu, ESP, call_original};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

pub const TERRAIN_VERTEX: Us = Us(0x453740);
pub const SPRITE: Us = Us(0x457140);
pub const POINTS: Us = Us(0x457340);
pub const TERRAIN_VISIBLE: Us = Us(0x452fd0);
pub const OBJECT_VISIBLE: Us = Us(0x4572a0);
pub const MARKERS: Us = Us(0x435330);

/// The display list's bump pointer (next free 0x38-byte record).
const LIST_TOP: Us = Us(0x511dc0);
/// Nonzero in split screen (each view is 1280 units wide).
const SPLIT: Us = Us(0x511f54);
/// The game's resolution index, and its table of modes (20 bytes each: width, height, ...).
const RES_INDEX: Us = Us(0x511e58);
const RES_TABLE: Us = Us(0x487080);

/// k as f32 bits; 1.0 is off.
static K: AtomicU32 = AtomicU32::new(0x3f80_0000);
/// The shadows' ring (`shadows::actors_draw`): camera-space x and y times this before the
/// objects' projection and view test, as f32 bits; 1.0 is off.
static ZOOM: AtomicU32 = AtomicU32::new(0x3f80_0000);

pub fn set_zoom(z: f32) {
    ZOOM.store(z.to_bits(), Ordering::Relaxed);
}

fn zoom() -> f32 {
    f32::from_bits(ZOOM.load(Ordering::Relaxed))
}

fn sz(v: i32) -> i32 {
    (v as f32 * zoom()) as i32
}

/// Records moved by `to_screen` this frame (drawn with the centred HUD).
static PLACED: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// The player's choice: 0 off, 1 on, 2 not read from the registry yet.
static ENABLED: AtomicU8 = AtomicU8::new(2);

pub fn enabled() -> bool {
    let e = ENABLED.load(Ordering::Relaxed);
    if e != 2 {
        return e == 1;
    }
    let on = advapi32::value("", "Widescreen").is_none_or(|(_, d)| d.first().is_some_and(|&b| b != 0));
    ENABLED.store(on as u8, Ordering::Relaxed);
    on
}

/// Preferences' OK: saved, and used from the next frame on.
pub fn set_enabled(on: bool) {
    ENABLED.store(on as u8, Ordering::Relaxed);
    advapi32::set_value("", "Widescreen", 4, &(on as u32).to_le_bytes());
}

pub fn k() -> f32 {
    f32::from_bits(K.load(Ordering::Relaxed))
}

fn setting() -> Option<f32> {
    static S: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
    *S.get_or_init(|| std::env::var("FCOP_WIDE").ok().and_then(|v| v.parse().ok()))
}

/// The aspect of the frame to fill (None: no 3D frame, 4:3). Returns the aspect used, which
/// the renderer gives its picture. Called between frames.
pub fn set_aspect(a: Option<f32>) -> f32 {
    let a = match (setting(), a) {
        (_, None) => 4.0 / 3.0,
        (Some(f), _) if f == 0.0 => 4.0 / 3.0,
        (Some(f), _) => f,
        (None, Some(a)) if enabled() => a,
        (None, Some(_)) => 4.0 / 3.0,
    };
    let a = if super::walker::precise() { a.max(4.0 / 3.0) } else { 4.0 / 3.0 };
    let k = if a > 4.0 / 3.0 + 0.01 { 4.0 / 3.0 / a } else { 1.0 };
    K.store(k.to_bits(), Ordering::Relaxed);
    PLACED.lock().unwrap().clear();
    if k == 1.0 { 4.0 / 3.0 } else { a }
}

fn sx(v: i32) -> i32 {
    (v as f32 * k()) as i32
}

/// 0x453740(out, x, y, z): one terrain vertex (x s32).
pub fn terrain_vertex(c: &mut Cpu) {
    if k() != 1.0 {
        let m = mem();
        let a = c.r[ESP] + 8;
        m.wr32(a, sx(m.rd32(a) as i32) as u32);
    }
    let out = mem().rd32(c.r[ESP] + 4);
    call_original(c, TERRAIN_VERTEX.at());
    // {x, y, z / 16} (s16) for the ambient occlusion's depth
    if super::ao::depth_wanted() {
        let m = mem();
        super::ao::point(m.rd16(out) as i16 as i32, m.rd16(out + 2) as i16 as i32, m.rd16(out + 4) as i16, true);
    }
}

/// 0x457140(p): a sprite {x, y, z, _, w, h} (s16).
pub fn sprite(c: &mut Cpu) {
    let zoomed = zoom() != 1.0;
    if k() != 1.0 || zoomed {
        let m = mem();
        let p = m.rd32(c.r[ESP] + 4);
        for a in [p, p + 8] {
            m.wr16(a, sz(sx(m.rd16(a) as i16 as i32)) as u16);
        }
        if zoomed {
            for a in [p + 2, p + 10] {
                m.wr16(a, sz(m.rd16(a) as i16 as i32) as u16);
            }
        }
    }
    let p = mem().rd32(c.r[ESP] + 4);
    call_original(c, SPRITE.at());
    // projected in place: centre {sx, sy}, z, size {w, h} at +8 (al = 0: in front of the camera)
    if super::ao::depth_wanted() && !zoomed && c.r[crate::rt::EAX] & 0xff == 0 {
        let m = mem();
        let r = |o: u32| m.rd16(p + o) as i16;
        super::ao::sprite(r(0), r(2), r(8), r(10), r(4));
    }
}

/// 0x457340(points, flags, n): n points {x, y, z, _} (s16, 8 bytes each), projected in place.
pub fn points(c: &mut Cpu) {
    let z = zoom();
    if k() != 1.0 || z != 1.0 {
        let m = mem();
        let p = m.rd32(c.r[ESP] + 4);
        let n = m.rd32(c.r[ESP] + 12) as i32;
        for i in 0..n.max(0) as u32 {
            m.wr16(p + 8 * i, sz(sx(m.rd16(p + 8 * i) as i16 as i32)) as u16);
            if z != 1.0 {
                m.wr16(p + 8 * i + 2, sz(m.rd16(p + 8 * i + 2) as i16 as i32) as u16);
            }
        }
    }
    let m = mem();
    let (p, flags, n) = (m.rd32(c.r[ESP] + 4), m.rd32(c.r[ESP] + 8), m.rd32(c.r[ESP] + 12) as i32);
    // the ring's: camera-space x, y (zoomed) and z before the projection
    let before: Vec<(i32, i32, i32)> = if z != 1.0 {
        (0..n.max(0) as u32).map(|i| (m.rd16(p + 8 * i) as i16 as i32, m.rd16(p + 8 * i + 2) as i16 as i32, m.rd16(p + 8 * i + 4) as i16 as i32)).collect()
    } else {
        Vec::new()
    };
    call_original(c, POINTS.at());
    // projected in place: {sx, sy, z}; flags: 1 = in front of the near plane (not projected),
    // else 0x40 when x is on the screen, 0x80 when y is; a face is drawn when its points' flags
    // or'ed are 0xc0 (one point on the screen in x, one in y)
    if z != 1.0 {
        // the ring draws faces off the screen too, but none with a point projected outside s16
        // (x / z up to ~7 zoomed; their real place, twice as far off, goes to `ao::point` as
        // i32: cut to s16 it met others at its ends and took their depth, shadows across the
        // whole picture)
        for (i, &(x, y, d)) in before.iter().enumerate() {
            let a = flags + i as u32;
            if m.rd8(a) & 1 == 0 {
                let fits = |v: i32| (v.abs() as i64) * 4096 < 30000 * d as i64;
                m.wr8(a, if fits(x) && fits(y) { 0xc0 } else { 1 });
            }
        }
    }
    if super::shadows::enabled() {
        super::shadows::camera();
    }
    if super::ao::depth_wanted() {
        for i in 0..n.max(0) as u32 {
            if m.rd8(flags + i) != 1 {
                let a = p + 8 * i;
                let (x, y) = (m.rd16(a) as i16 as i32, m.rd16(a + 2) as i16 as i32);
                // the ring's: at their real place (2560x1920 units, centre 1280, 960)
                let (x, y) = if z == 1.0 {
                    (x, y)
                } else {
                    ((1280.0 + (x as f32 - 1280.0) / z).round() as i32, (960.0 + (y as f32 - 960.0) / z).round() as i32)
                };
                super::ao::point(x, y, m.rd16(a + 4) as i16, false);
            }
        }
    }
}

/// 0x452fd0(p, r): is the sphere at camera-space p {x, y, z} (s32) with radius r inside the
/// 4:3 view? Terrain blocks and cells are culled with it (0x452a20).
pub fn terrain_visible(c: &mut Cpu) {
    if super::shadows::enabled() {
        super::shadows::camera();
    }
    if k() == 1.0 {
        return call_original(c, TERRAIN_VISIBLE.at());
    }
    let m = mem();
    let p = m.rd32(c.r[ESP] + 4);
    let x = m.rd32(p);
    m.wr32(p, sx(x as i32) as u32);
    call_original(c, TERRAIN_VISIBLE.at());
    m.wr32(p, x);
}

/// The shadows' ring: may the sphere at camera-space {x, y, z} with radius r (object units)
/// cast a shadow into the view? The sphere swept away from the sun by `reach` (its shadow:
/// things high above the ground, flying enemies, bridges, throw theirs far) tested against the
/// real view as the game does, the segment clipped by the view's planes. `shown`: a thing the
/// game tried itself, false when the sphere is in the view (the game drew it).
pub fn casts_into_view(x: f32, y: f32, z: f32, r: f32, reach: f32, shown: bool) -> bool {
    let s = super::shadows::sun_view();
    let (xk, sx) = (x * k(), s[0] * k());
    // each plane: g0 - g1 * t >= 0 along the point P - s * t, t in [0, reach]
    let planes = [
        (z + r - 1029.0 / 16.0, s[2]),
        (0.3125 * z + 1.05 * r - xk, 0.3125 * s[2] - sx),
        (0.3125 * z + 1.05 * r + xk, 0.3125 * s[2] + sx),
        (0.234 * z + 1.03 * r - y, 0.234 * s[2] - s[1]),
        (0.234 * z + 1.03 * r + y, 0.234 * s[2] + s[1]),
    ];
    // the sphere itself in the view: the game drew it (things it tried itself)
    if shown && planes.iter().all(|&(g0, _)| g0 >= 0.0) {
        return false;
    }
    let (mut lo, mut hi) = (0.0f32, reach);
    for (g0, g1) in planes {
        if g1.abs() < 1e-6 {
            if g0 < 0.0 {
                return false;
            }
        } else if g1 > 0.0 {
            hi = hi.min(g0 / g1);
        } else {
            lo = lo.max(g0 / g1);
        }
    }
    lo <= hi
}

/// 0x4572a0(p, r): the same test for objects, p {x, y, z} s16, r in z * 16's units: 1 when
/// the sphere is not beyond the near plane (z * 16 + r <= 0x405), 0 beside the view (|x| <=
/// 0.3125 z plus a bit of r: the 4:3 view's sides; y alike), else 2. Only 2 is drawn.
pub fn object_visible(c: &mut Cpu) {
    if k() == 1.0 && zoom() == 1.0 {
        return call_original(c, OBJECT_VISIBLE.at());
    }
    let m = mem();
    let p = m.rd32(c.r[ESP] + 4);
    if zoom() != 1.0 {
        // the shadows' ring: what casts a shadow into the view (`casts_into_view`); the zoom
        // keeps the projected points inside s16 (`points` leaves out the rest). As if it
        // returned.
        let (x, y, z) = (m.rd16(p) as i16 as f32, m.rd16(p + 2) as i16 as f32, m.rd16(p + 4) as i16 as f32);
        let r = m.rd32(c.r[ESP] + 8) as i32 as f32 / 16.0;
        let swept = || casts_into_view(x, y, z, r, super::shadows::ring_reach() + 2.3 * r, super::shadows::ring_shown());
        let near = z * 16.0 + r * 16.0 <= 1029.0;
        c.r[crate::rt::EAX] = (c.r[crate::rt::EAX] & !0xff)
            | if !near && (super::shadows::ring_wide() || swept()) {
                2
            } else if near {
                1
            } else {
                0
            };
        c.r[ESP] += 4;
        return;
    }
    let (x, y) = (m.rd16(p), m.rd16(p + 2));
    m.wr16(p, sz(sx(x as i16 as i32)) as u16);
    m.wr16(p + 2, sz(y as i16 as i32) as u16);
    call_original(c, OBJECT_VISIBLE.at());
    m.wr16(p, x);
    m.wr16(p + 2, y);
}

/// 0x435330 draws the target markers at the projected position of their target, on the HUD
/// level (depth key 0).
pub fn markers(c: &mut Cpu) {
    let m = mem();
    let player = m.rd32(c.r[ESP] + 4);
    let from = m.rd32(LIST_TOP.at());
    call_original(c, MARKERS.at());
    super::camera::drop_laser(player, from);
    let to = m.rd32(LIST_TOP.at());
    if k() == 1.0 || to < from || to - from > 0x38 * 256 {
        return;
    }
    // +0 holds the depth key until the list is sorted
    for r in (from..to).step_by(0x38) {
        if m.rd8(r + 4) != 0 && m.rd32(r) == 0 {
            to_screen(r);
        }
    }
}

pub fn placed(r: u32) -> bool {
    PLACED.lock().unwrap().contains(&r)
}

/// The centre of the view in 2560x1920 units (split screen: each view's own coordinates).
fn centre() -> i32 {
    if mem().rd8(SPLIT.at()) != 0 { 640 } else { 1280 }
}

/// The game's resolution in pixels (the space of type-6 records).
pub fn game_px() -> (u32, u32) {
    let m = mem();
    let r = m.rd32(RES_INDEX.at());
    let (w, h) = if r <= 10 { (m.rd32(RES_TABLE.at() + 20 * r), m.rd32(RES_TABLE.at() + 4 + 20 * r)) } else { (1280, 960) };
    if w < 320 || h < 200 { (1280, 960) } else { (w, h) }
}

/// 2560x1920 units per unit of a record: type 5 sprites are in 640x480 units, type 6 in
/// pixels of the game's resolution.
fn units(n: u8) -> i32 {
    if n == 5 { 4 } else { (2560 / game_px().0 as i32).max(1) }
}

/// A record's extent in 2560x1920 units (x0, y0, x1, y1), if it has one.
pub fn extent(r: u32) -> Option<[i32; 4]> {
    let m = mem();
    let t = m.rd8(r + 4);
    let n = t & 7;
    let xy = |i: u32| (m.rd16(r + 0x28 + 4 * i) as i16 as i32, m.rd16(r + 0x2a + 4 * i) as i16 as i32);
    match n {
        1..=4 => {
            let mut e = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
            for i in 0..n as u32 {
                let (x, y) = xy(i);
                e = [e[0].min(x), e[1].min(y), e[2].max(x), e[3].max(y)];
            }
            Some(e)
        }
        5 | 6 => {
            let (u, scale) = (units(n), if n == 5 { 2 } else { 1 });
            let (w, h) = if t & 0x80 != 0 {
                (m.rd8(r + 10) as i32 * scale, m.rd8(r + 11) as i32 * scale)
            } else {
                xy(1)
            };
            let (x, y) = xy(0);
            Some([x * u, y * u, (x + w) * u, (y + h) * u])
        }
        _ => None,
    }
}

/// A HUD-level record whose place belongs to the 3D picture (a target marker, the
/// split-screen divider): the HUD is drawn 4:3, so move it from x to cx + (x - cx) / k.
/// Polygons: every vertex; sprites {x, y, w, h}: the centre (the size stays).
pub fn to_screen(r: u32) {
    PLACED.lock().unwrap().push(r);
    let m = mem();
    let t = m.rd8(r + 4);
    let n = t & 7;
    let (k, cx) = (k(), centre());
    let move_x = |x: i32, cx: i32| (cx as f32 + (x - cx) as f32 / k) as i32;
    match n {
        1..=4 => {
            for i in 0..n as u32 {
                let a = r + 0x28 + 4 * i;
                m.wr16(a, move_x(m.rd16(a) as i16 as i32, cx) as u16);
            }
        }
        5 | 6 => {
            let w = if t & 0x80 != 0 { m.rd8(r + 10) as i32 * if n == 5 { 2 } else { 1 } } else { m.rd16(r + 0x2c) as i16 as i32 };
            let x = m.rd16(r + 0x28) as i16 as i32 + w / 2;
            m.wr16(r + 0x28, (move_x(x, cx / units(n)) - w / 2) as u16);
        }
        _ => {}
    }
}
