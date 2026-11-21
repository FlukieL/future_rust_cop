//! The scene in world space (PLAN-RTX.md step 2): what the game draws, as models and their
//! transforms, for acceleration structures and motion vectors later.
//!
//! model_draw 0x4280a0 (record, flags, mode) draws one model: the record holds +0 the model,
//! +4 a 3x3 matrix, +0x28 the position, +0x48 the animation frame (i16). The model points at
//! its mission chunks' data in guest memory: +0xc flags (0x2000000: in parts), +0x1c a block
//! whose +4 points at the `4DVL` count (8-byte vertices x y z pad behind it), +0x24 at the
//! `3DQL` count (12-byte faces behind it), +0x28 the `3DTL` entries.
//!
//! The game's transform (0x4347c0, 0x4580b0, 0x4588e0; row vectors): camera space =
//! (v * R + p / 16 - c / 16) * C, R the record's matrix (16.16), p its position (20.12), c the
//! camera's position (camera [0x511f48] +0x7c, 20.12), C its matrix (+0x28, 16.16). So world
//! space in model units is v * R + p / 16.
//!
//! Each model (or part) is taken where project_points 0x457340 projects it (`check`): the
//! game's camera-space matrix for it (0x4a05c0, a part's + 0x30 * its number) with the camera
//! taken out gives its world matrix, so animated parts need no animation code of ours. A model
//! not in parts takes the vertex list of its frame ([[+0x1c] + 4 + 4 * frame]); parts (the
//! model's +0x14 list, 0x14 bytes each: +0 number, +3 first vertex, +4 count, +7 & 0x80 last)
//! ranges of the first list.
//!
//! Terrain: our terrain pass (`terrain.rs`) projects a block (a 17x17 grid of points with 3
//! height layers, s8 at sect + 0xc, -128 none) from its camera-space origin and axes (column,
//! row, height / 256; terrain camera space is 16 model units to one); with the camera taken
//! out they are the block's world frame (`terrain_block`). Each tile it keeps notes its 4
//! corners as indices into the block's points (3 per grid point; a triangle repeats its first
//! as the fourth: `terrain_tile`).
//!
//! `FCOP_GBUFFER=scene` draws each view's instances as green lines over its 3D picture
//! (`overlay`, called from `glide::post_3d`): they must lie on the game's models.
//!
//! `FCOP_SCENE_LOG=1` logs each model the first time it is drawn, the mission file's `Cobj`
//! counts for comparison, and every 2 s how far the world matrices of models not in parts are
//! from their records (matrix and position).

use crate::rt::{Cpu, ESP, call_original};
use crate::ver::Us;
use std::cell::RefCell;
use std::collections::HashMap;

pub const MODEL_DRAW: Us = Us(0x4280a0);

/// What we know of a model the game drew.
struct Model {
    flags: u32,
    vertices: u32,
    faces: u32,
}

thread_local! {
    static MODELS: RefCell<HashMap<u32, Model>> = RefCell::new(HashMap::new());
    static MISSION: RefCell<String> = const { RefCell::new(String::new()) };
    /// the record model_draw draws now
    static CURRENT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// what was drawn (for now: since the last log)
    static PICTURE: RefCell<Vec<Instance>> = const { RefCell::new(Vec::new()) };
    /// the camera of the instances collected: matrix C and position c / 16
    static VIEW_CAMERA: std::cell::Cell<Option<([f64; 9], [f64; 3])>> = const { std::cell::Cell::new(None) };
    static OVERLAY: RefCell<Option<(glow::Program, glow::VertexArray, glow::Buffer)>> = const { RefCell::new(None) };
    /// the terrain blocks projected for the view: world frame by sect
    static BLOCKS: RefCell<HashMap<u32, Block>> = RefCell::new(HashMap::new());
    /// the terrain tiles kept for the view: sect and corners
    static TILES: RefCell<Vec<(u32, [u16; 4])>> = const { RefCell::new(Vec::new()) };
    /// the last view's scene for the rays
    static LAST: RefCell<Option<Frame>> = const { RefCell::new(None) };
    static GENERATION: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// the mission's texture pages as coverage bits (`Frame::pages`)
    static PAGES: RefCell<std::sync::Arc<Vec<u32>>> = RefCell::new(std::sync::Arc::new(Vec::new()));
    static MAP_FRAME: std::cell::Cell<Option<MapFrame>> = const { std::cell::Cell::new(None) };
    /// records model_draw drew for the view
    static DRAWN: RefCell<std::collections::HashSet<u32>> = RefCell::new(std::collections::HashSet::new());
    /// points checked, their largest and summed distance (model units), since the last log
    static STATS: std::cell::Cell<(u64, f64, f64, std::option::Option<std::time::Instant>)> = const { std::cell::Cell::new((0, 0.0, 0.0, None)) };
}

const CAMERA: Us = Us(0x511f48);

/// A terrain block's world frame (model units): point (row, column, height h) = origin + row *
/// rows + column * columns + h * height.
#[derive(Clone, Copy)]
pub struct Block {
    pub origin: [f64; 3],
    pub rows: [f64; 3],
    pub columns: [f64; 3],
    pub height: [f64; 3],
}

/// The camera now: matrix C (16.16 as f64) and position c / 16.
fn camera() -> ([f64; 9], [f64; 3]) {
    let m = super::mem();
    let cam = m.rd32(CAMERA.at());
    let rd = |a: u32| m.rd32(a) as i32 as f64;
    (std::array::from_fn(|k| rd(cam + 0x28 + 4 * k as u32) / 65536.0), std::array::from_fn(|k| rd(cam + 0x7c + 4 * k as u32) / 16.0))
}

/// x * C^T: a camera-space vector in world space.
fn untilt(c: &[f64; 9], x: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|j| (0..3).map(|i| x[i] * c[3 * j + i]).sum())
}

/// Our terrain pass projects block `sect` from camera-space `pos` with `rows` / `columns` steps
/// and `height` (times h / 256), terrain units (16 to a model unit).
pub fn terrain_block(sect: u32, pos: [i32; 3], rows: [i32; 3], columns: [i32; 3], height: [i32; 3]) {
    if !wanted() {
        return;
    }
    let (c, cp) = camera();
    let v = |x: [i32; 3], s: f64| untilt(&c, x.map(|x| x as f64 / s));
    let o = v(pos, 16.0);
    let b = Block { origin: std::array::from_fn(|j| o[j] + cp[j]), rows: v(rows, 16.0), columns: v(columns, 16.0), height: v(height, 16.0 * 256.0) };
    BLOCKS.with_borrow_mut(|m| m.insert(sect, b));
    VIEW_CAMERA.set(Some((c, cp)));
}

/// The terrain map's place in the world: block (row, column)'s origin = `origin` + row * `rows`
/// + column * `columns` (16 point steps each); `axes` a block's own (`Block` without origin).
#[derive(Clone, Copy)]
struct MapFrame {
    origin: [f64; 3],
    rows: [f64; 3],
    columns: [f64; 3],
    axes: Block,
}

/// render_terrain drew map block (`row`, `column`), its first point at camera-space `at`, with
/// these steps (terrain units; `height` << 8): the map's place in the world.
pub fn terrain_map(row: i32, column: i32, at: [i32; 3], rows: [i32; 3], columns: [i32; 3], height: [i32; 3]) {
    if !rays() {
        return;
    }
    let (c, cp) = camera();
    let v = |x: [i32; 3], s: f64| untilt(&c, x.map(|x| x as f64 / s));
    let (r, k, h) = (v(rows, 16.0), v(columns, 16.0), v(height, 16.0 * 256.0));
    let o = v(at, 16.0);
    let (r16, k16): ([f64; 3], [f64; 3]) = (r.map(|x| 16.0 * x), k.map(|x| 16.0 * x));
    let origin = std::array::from_fn(|j| o[j] + cp[j] - row as f64 * r16[j] - column as f64 * k16[j]);
    MAP_FRAME.set(Some(MapFrame { origin, rows: r16, columns: k16, axes: Block { origin: [0.0; 3], rows: r, columns: k, height: h } }));
}

/// Every block of the terrain map with its world frame (the whole level's ground).
fn level_blocks(f: &MapFrame) -> Vec<(u32, Block)> {
    let m = super::mem();
    let (map, blocks) = (m.rd32(super::terrain::MAP.at()), m.rd32(super::terrain::BLOCKS.at()));
    if map == 0 || blocks == 0 {
        return vec![];
    }
    let (w, h) = (m.rd32(map + 4).min(256), m.rd32(map + 8).min(256));
    let mut out = Vec::new();
    for row in 0..h {
        for col in 0..w {
            let cell = m.rd32(map + 0x24 + 4 * (w * row + col));
            let sect = m.rd32(blocks + 4 * ((cell >> 2) & 0x3fff));
            if sect == 0 {
                continue;
            }
            let origin = std::array::from_fn(|j| f.origin[j] + row as f64 * f.rows[j] + col as f64 * f.columns[j]);
            out.push((sect, Block { origin, ..f.axes }));
        }
    }
    out
}

/// The static objects' grid: cells (8 bytes: first object, linked by +0x58), its width and height.
const OBJECT_GRID: Us = Us(0x4a13fc);
const OBJECT_GRID_WIDTH: Us = Us(0x4a13f4);
const OBJECT_GRID_HEIGHT: Us = Us(0x4a1400);
/// The actors (pointers) and their count; an actor's model record is at +0x28.
const ACTORS: Us = Us(0x50e428);
const ACTOR_COUNT: Us = Us(0x50e420);

/// A record (model_draw's: +0 model, +4 matrix, +0x28 position, +0x48 frame) as an instance by
/// its own transform: for things the game did not draw this picture (whole; parts in the pose
/// their vertices have).
fn record_instance(record: u32) -> Option<Instance> {
    let m = super::mem();
    let model = m.rd32(record);
    if model == 0 {
        return None;
    }
    let lists = m.rd32(model + 0x1c);
    let frame = if m.rd32(model + 0xc) & 0x2000000 != 0 { 0 } else { (m.rd16(record + 0x48) as i16).max(0) as u32 };
    let list = m.rd32(lists + 4 + 4 * frame);
    if list == 0 {
        return None;
    }
    let rd = |a: u32| m.rd32(a) as i32 as f64;
    Some(Instance {
        model,
        list,
        first: 0,
        count: m.rd32(list).min(4096),
        matrix: std::array::from_fn(|k| rd(record + 4 + 4 * k as u32) / 65536.0),
        offset: std::array::from_fn(|k| rd(record + 0x28 + 4 * k as u32) / 16.0),
    })
}

/// The level's static objects and actors the game did not draw this picture (`drawn`: records).
fn level_objects(drawn: &std::collections::HashSet<u32>) -> Vec<Instance> {
    let m = super::mem();
    let mut out = Vec::new();
    let grid = m.rd32(OBJECT_GRID.at());
    if grid != 0 {
        let n = m.rd32(OBJECT_GRID_WIDTH.at()).min(1024) * m.rd32(OBJECT_GRID_HEIGHT.at()).min(1024);
        for cell in 0..n {
            let mut o = m.rd32(grid + 8 * cell);
            for _ in 0..4096 {
                if o == 0 {
                    break;
                }
                if m.rd8(o + 0x4c) & 0x10 == 0 && !drawn.contains(&o) {
                    out.extend(record_instance(o));
                }
                o = m.rd32(o + 0x58);
            }
        }
    }
    for k in 0..m.rd32(ACTOR_COUNT.at()).min(1024) {
        let a = m.rd32(ACTORS.at() + 4 * k);
        if a != 0 && !drawn.contains(&(a + 0x28)) {
            out.extend(record_instance(a + 0x28));
        }
    }
    out
}

/// Our terrain pass keeps a tile of block `sect` with these corners (indices into its points).
pub fn terrain_tile(sect: u32, corners: [u16; 4]) {
    if wanted() {
        TILES.with_borrow_mut(|t| t.push((sect, corners)));
    }
}

/// Point `i` of block `sect` (its frame `b`) in world space, or none (no height there).
fn terrain_point(sect: u32, b: &Block, i: u16) -> Option<[f64; 3]> {
    let h = super::mem().rd8(sect + 0xc + i as u32) as i8;
    if h == -128 {
        return None;
    }
    let (row, col) = ((i / 3 / 17) as f64, (i / 3 % 17) as f64);
    Some(std::array::from_fn(|j| b.origin[j] + row * b.rows[j] + col * b.columns[j] + h as f64 * b.height[j]))
}

/// `FCOP_GBUFFER=scene`: the overlay.
fn overlay_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("FCOP_GBUFFER").is_ok_and(|v| v == "scene"))
}

/// `FCOP_GBUFFER=rt`: the window shows rays from the camera against the scene's acceleration
/// structures instead of the picture (`raytrace.rs`).
pub fn rt_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("FCOP_GBUFFER").is_ok_and(|v| v == "rt"))
}

/// The rays need the whole level's scene: their debug picture, or the sun's shadow by rays.
pub fn rays() -> bool {
    rt_on() || (super::shadows::mode() == super::shadows::Mode::Rays && super::vk::wanted())
}

/// Instances are collected (for the log, the overlay or the rays).
fn wanted() -> bool {
    log_on() || overlay_on() || rays()
}

/// A view's scene as `raytrace.rs` takes it: the camera (matrix C, position c / 16), widescreen's
/// x scale, the drawn models and parts, the terrain blocks projected; `generation` changes with
/// each mission (cached geometry is stale then).
pub struct Frame {
    pub camera: ([f64; 9], [f64; 3]),
    pub k: f32,
    pub models: Vec<Instance>,
    pub blocks: Vec<(u32, Block)>,
    pub generation: u32,
    /// the mission's texture pages as bits, 1 where a texel covers (not black: keyed out), 2048
    /// words a page by page number (`Cbmp` - 1) (`pages`)
    pub pages: std::sync::Arc<Vec<u32>>,
}

/// A triangle's material for the rays (`model_mesh`): texel coordinates of its corners
/// u0 v0 u1 v1 (bytes of the first word), u2 v2 and the page (second word, page from bit 16;
/// `OPAQUE` none: always covers).
pub type Material = [u32; 2];
pub const OPAQUE: u32 = 0xffff;

/// The texture pages of mission file `path` as coverage bits (`Frame::pages`).
fn page_bits(path: &str) -> Vec<u32> {
    let Ok(file) = std::fs::read(super::host_path(path)) else { return Vec::new() };
    let mut bits = Vec::new();
    for r in crate::mission::resources(&file).iter().filter(|r| r.kind == "Cbmp") {
        let (Some(t), Some(page)) = (crate::mission::page_texels(&r.data), r.id.checked_sub(1)) else { continue };
        let at = page as usize * 2048;
        if bits.len() < at + 2048 {
            // pages missing in between cover everywhere
            bits.resize(at + 2048, u32::MAX);
        }
        for (i, w) in bits[at..at + 2048].iter_mut().enumerate() {
            *w = (0..32).fold(0, |a, b| a | (((t[32 * i + b] & 0x7fff != 0) as u32) << b));
        }
    }
    bits
}

/// The last view's scene (taken by the sun's rays in `glide::post_3d`, else by the Vulkan
/// present's debug picture).
pub fn take_frame() -> Option<Frame> {
    LAST.with_borrow_mut(|l| l.take())
}

/// A model's (or part's) triangles in model space: positions, indices and each triangle's
/// material (textured faces: face flags & 0x80; their `3DTL` entry: +4 the corners' u v bytes,
/// +12 the page).
pub fn model_mesh(i: &Instance) -> (Vec<[f32; 3]>, Vec<u32>, Vec<Material>) {
    let m = super::mem();
    let pos = (0..i.count)
        .map(|n| {
            let a = i.list + 4 + 8 * (i.first + n);
            std::array::from_fn(|j| m.rd16(a + 2 * j as u32) as i16 as f32)
        })
        .collect();
    let faces = m.rd32(i.model + 0x24);
    let entries = m.rd32(i.model + 0x28);
    let mut idx = Vec::new();
    let mut mats = Vec::new();
    for f in 0..m.rd32(faces).min(4096) {
        let a = faces + 4 + 12 * f;
        let corners = (m.rd8(a + 1) & 7) as u32;
        if corners != 3 && corners != 4 {
            continue;
        }
        let vs: Vec<u32> = (0..corners).map(|k| m.rd8(a + 4 + k) as u32).collect();
        if vs.iter().any(|&v| v < i.first || v >= i.first + i.count) {
            continue;
        }
        let v: Vec<u32> = vs.iter().map(|&v| v - i.first).collect();
        // (the model's pointer is the first entry, behind the chunk's word)
        let e = entries + m.rd16(a + 2) as u32;
        let textured = m.rd8(a) & 0x80 != 0 && entries != 0;
        let uv = |k: u32| m.rd16(e + 4 + 2 * k) as u32;
        let page = if textured { m.rd32(e + 12).min(OPAQUE - 1) } else { OPAQUE };
        let mat = |a, b, c| if page == OPAQUE { [0, OPAQUE << 16] } else { material([uv(a), uv(b), uv(c)], page) };
        idx.extend([v[0], v[1], v[2]]);
        mats.push(mat(0, 1, 2));
        if corners == 4 {
            idx.extend([v[0], v[2], v[3]]);
            mats.push(mat(0, 2, 3));
        }
    }
    (pos, idx, mats)
}

/// Per tile shape its 4 corners (point indices from the cell's first) (`terrain.rs`).
const CORNERS: Us = Us(0x48c608);

/// A terrain block's tiles (all of them, as `terrain.rs` walks a group's) relative to its
/// origin, in world axes.
pub fn block_mesh(sect: u32, b: &Block) -> (Vec<[f32; 3]>, Vec<u32>, Vec<Material>) {
    let m = super::mem();
    let mut pos = Vec::new();
    let mut idx = Vec::new();
    let mut mats = Vec::new();
    // a tile's texture (u16 in the table at [sect + 4]: page bits 8-10, bit 13 four texel
    // coordinates) and its coordinates (u v bytes at [sect + 8]), as `terrain.rs` reads them
    let (textures, coords) = (m.rd32(sect + 4), m.rd32(sect + 8));
    let mut point = |i: u32| -> Option<u32> {
        let h = m.rd8(sect + 0xc + i) as i8;
        if h == -128 {
            return None;
        }
        let (row, col) = ((i / 3 / 17) as f64, (i / 3 % 17) as f64);
        pos.push(std::array::from_fn(|j| (row * b.rows[j] + col * b.columns[j] + h as f64 * b.height[j]) as f32));
        Some(pos.len() as u32 - 1)
    };
    for g in 0..16u32 {
        let gofs = (g / 4) * 64 + (g % 4) * 4;
        let mut tp = sect + 0x5d0 + (m.rd16(sect + 0x3cc + 2 * gofs) as u32 >> 4);
        let (mut c, mut ci) = (0, gofs);
        for _ in 0..1024 {
            let t = m.rd32(tp);
            tp += 4;
            let cell = ci;
            if t & 1 != 0 {
                c += 1;
                ci += 1;
                if c & 3 == 0 {
                    ci += 0xc;
                }
            }
            let shape = (t >> 15) & 0x7f;
            let k: Vec<u32> = (0..4).map(|i| 3 * (cell + (cell >> 4)) + m.rd8(CORNERS.at() + 4 * shape + i) as u32).collect();
            let n = if k[3] == k[0] { 3 } else { 4 };
            let p: Option<Vec<u32>> = k[..n].iter().map(|&i| point(i)).collect();
            if let Some(p) = p {
                let tw = m.rd16(textures + ((t >> 21) & 0x7fe)) as u32;
                let at = coords + (t & 0x7fe);
                let mut uv: [u32; 4] = std::array::from_fn(|i| m.rd16(at + 2 * i as u32) as u32);
                if tw & 0x2000 == 0 {
                    // three coordinates: the fourth corner's completes the parallelogram
                    let c = |w: u32, s: u32| (w >> s & 255) as i32;
                    let f = |s| (c(uv[0], s) + c(uv[2], s) - c(uv[1], s)).clamp(0, 255) as u32;
                    uv[3] = f(0) | f(8) << 8;
                }
                let page = (tw >> 8) & 7;
                idx.extend([p[0], p[1], p[2]]);
                mats.push(material([uv[0], uv[1], uv[2]], page));
                if n == 4 {
                    idx.extend([p[0], p[2], p[3]]);
                    mats.push(material([uv[0], uv[2], uv[3]], page));
                }
            }
            if c >= 16 {
                break;
            }
        }
    }
    (pos, idx, mats)
}

/// A triangle's material: texel coordinates of its corners (u | v << 8) on `page`; `OPAQUE`
/// when no texel of their box is keyed out (the rays then need not test its texels).
fn material(uv: [u32; 3], page: u32) -> Material {
    let keyed = PAGES.with_borrow(|bits| {
        let c = |s: u32| uv.map(|w| w >> s & 255);
        let (us, vs) = (c(0), c(8));
        let (u0, u1) = (*us.iter().min().unwrap(), *us.iter().max().unwrap());
        let (v0, v1) = (*vs.iter().min().unwrap(), *vs.iter().max().unwrap());
        (v0..=v1).any(|v| {
            (u0..=u1).any(|u| {
                let i = (page * 65536 + v * 256 + u) as usize;
                bits.get(i >> 5).is_some_and(|w| w >> (i & 31) & 1 == 0)
            })
        })
    });
    let page = if keyed { page } else { OPAQUE };
    [uv[0] | uv[1] << 16, uv[2] | page << 16]
}

fn log_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("FCOP_SCENE_LOG").is_ok_and(|v| v == "1"))
}

/// Hook of model_draw 0x4280a0.
pub fn model_draw(c: &mut Cpu) {
    if log_on() {
        note(c);
    }
    if wanted() {
        let record = super::mem().rd32(c.r[ESP] + 4);
        CURRENT.set(record);
        if rays() {
            DRAWN.with_borrow_mut(|d| d.insert(record));
        }
    }
    call_original(c, MODEL_DRAW.at());
    CURRENT.set(0);
}

/// The game's matrix for what project_points projects (0x4a05c0; in parts + 0x30 * part
/// number): 3x3 (16.16, row vectors) and a translation row (20.12), in camera space.
const MATRICES: Us = Us(0x4a05c0);
/// Where models have their points transformed and projected (0x4280f0 reads it; the flags at
/// [0x4a074c]).
const PART_POINTS: Us = Us(0x4a0f58);

/// A drawn model or part in world space: its vertices `first..first + count` of the vertex list
/// at `list` (guest), times `matrix` (3x3, row vectors, model units) plus `offset`.
#[derive(Clone, Copy)]
pub struct Instance {
    pub model: u32,
    pub list: u32,
    pub first: u32,
    pub count: u32,
    pub matrix: [f64; 9],
    pub offset: [f64; 3],
}

/// project_points 0x457340 gets `n` camera-space points at `p` (before widescreen scales them):
/// while model_draw runs, a whole model's or one part's. Its matrix back in world space: the
/// camera taken out (its matrix is a rotation: the inverse is the transpose).
pub fn check(p: u32, n: u32) {
    let record = CURRENT.get();
    if record == 0 {
        return;
    }
    let m = super::mem();
    let model = m.rd32(record);
    new_mission();
    let rd = |a: u32| m.rd32(a) as i32 as f64;
    let lists = m.rd32(model + 0x1c);
    let parts = m.rd32(model + 0xc) & 0x2000000 != 0;
    let (list, first, number) = if parts {
        let base = m.rd32(PART_POINTS.at());
        let first = p.wrapping_sub(base) / 8;
        let mut part = m.rd32(model + 0x14);
        let mut found = None;
        for _ in 0..64 {
            if m.rd8(part + 3) as u32 == first && m.rd8(part + 4) as u32 == n {
                found = Some(m.rd8(part) as u32);
                break;
            }
            if m.rd8(part + 7) & 0x80 != 0 {
                break;
            }
            part += 0x14;
        }
        let Some(k) = found else { return };
        (m.rd32(lists + 4), first, k)
    } else {
        let frame = m.rd16(record + 0x48) as i16 as u32;
        (m.rd32(lists + 4 + 4 * frame), 0, 0)
    };
    let gm = MATRICES.at() + 0x30 * number;
    let g: Vec<f64> = (0..9).map(|k| rd(gm + 4 * k) / 65536.0).collect();
    let gt: Vec<f64> = (0..3).map(|k| rd(gm + 36 + 4 * k) / 16.0).collect();
    let cam = m.rd32(CAMERA.at());
    let c: Vec<f64> = (0..9).map(|k| rd(cam + 0x28 + 4 * k) / 65536.0).collect();
    let cp: Vec<f64> = (0..3).map(|k| rd(cam + 0x7c + 4 * k) / 16.0).collect();
    // x * C^T: (x * C^T)_j = sum_i x_i * C[j][i]
    let untilt = |x: &[f64]| -> [f64; 3] { std::array::from_fn(|j| (0..3).map(|i| x[i] * c[3 * j + i]).sum()) };
    let mut matrix = [0.0; 9];
    for r in 0..3 {
        matrix[3 * r..3 * r + 3].copy_from_slice(&untilt(&g[3 * r..3 * r + 3]));
    }
    let t = untilt(&gt);
    let offset: [f64; 3] = std::array::from_fn(|j| t[j] + cp[j]);
    let inst = Instance { model, list, first, count: n, matrix, offset };
    let (mut count, mut max, mut sum, mut since) = STATS.get();
    if !parts {
        // against the record: its matrix and its position
        let dr = (0..9).map(|k| (matrix[k] - rd(record + 4 + 4 * k as u32) / 65536.0).abs()).fold(0.0, f64::max);
        let dt = (0..3).map(|k| (offset[k] - rd(record + 0x28 + 4 * k as u32) / 16.0).abs()).fold(0.0, f64::max);
        count += 1;
        sum += dt;
        max = f64::max(max, dr * 1000.0);
    }
    PICTURE.with_borrow_mut(|v| v.push(inst));
    VIEW_CAMERA.set(Some((std::array::from_fn(|k| c[k]), std::array::from_fn(|k| cp[k]))));
    if !log_on() {
        return;
    }
    let now = std::time::Instant::now();
    let since = *since.get_or_insert(now);
    if now - since >= std::time::Duration::from_secs(2) {
        super::log!(
            "scene: {count} models checked against their records: position off by {:.2} model units on average, matrix by at most {:.4}",
            sum / count.max(1) as f64,
            max / 1000.0
        );
        if !overlay_on() {
            PICTURE.with_borrow_mut(|v| v.clear());
        }
        STATS.set((0, 0.0, 0.0, Some(now)));
    } else {
        STATS.set((count, max, sum, Some(since)));
    }
}

fn note(c: &mut Cpu) {
    let m = super::mem();
    let record = m.rd32(c.r[ESP] + 4);
    let model = m.rd32(record);
    if model == 0 {
        return;
    }
    new_mission();
    MODELS.with_borrow_mut(|models| {
        models.entry(model).or_insert_with(|| {
            let flags = m.rd32(model + 0xc);
            let vl = m.rd32(m.rd32(model + 0x1c) + 4);
            let ql = m.rd32(model + 0x24);
            let n = Model { flags, vertices: m.rd32(vl), faces: m.rd32(ql) };
            let matrix: Vec<i32> = (0..9).map(|k| m.rd32(record + 4 + 4 * k) as i32).collect();
            let pos: Vec<i32> = (0..3).map(|k| m.rd32(record + 0x28 + 4 * k) as i32).collect();
            super::log!(
                "scene: model {model:#x} flags {:#x} vertices {} faces {} frame {} matrix {matrix:?} position {pos:?}",
                n.flags,
                n.vertices,
                n.faces,
                m.rd16(record + 0x48) as i16
            );
            n
        });
    });
}

/// A new mission: models forgotten, the mission file's `Cobj` counts logged.
fn new_mission() {
    let path = super::kernel32::LAST_MISSION.lock().unwrap().clone();
    if MISSION.with_borrow(|p| *p == path) {
        return;
    }
    MISSION.with_borrow_mut(|p| *p = path.clone());
    MODELS.with_borrow_mut(|m| m.clear());
    MAP_FRAME.set(None);
    GENERATION.set(GENERATION.get() + 1);
    if rays() {
        PAGES.set(std::sync::Arc::new(page_bits(&path)));
    }
    if !log_on() {
        return;
    }
    let Ok(file) = std::fs::read(super::host_path(&path)) else { return };
    for r in crate::mission::resources(&file).iter().filter(|r| r.kind == "Cobj") {
        let count = |tag: &str| crate::mission::chunks(&r.data).iter().find(|c| c.0 == tag).map(|c| u32::from_le_bytes(c.1[4..8].try_into().unwrap()));
        super::log!("scene: {path} Cobj {} vertices {:?} faces {:?}", r.id, count("4DVL"), count("3DQL"));
    }
}

const LINE_VS: &str = r#"
layout(location = 0) in vec2 pos;
void main() { gl_Position = vec4(pos, 0.0, 1.0); }
"#;

const LINE_FS: &str = r#"
uniform vec4 tint;
out vec4 color;
void main() { color = tint; }
"#;

/// `FCOP_GBUFFER=scene`: the view's instances as lines into `fbo` (the 3D picture, w x h
/// pixels, `k` widescreen's x scale), then forgotten.
pub fn overlay(gl: &glow::Context, fbo: glow::Framebuffer, w: u32, h: u32, k: f32) {
    use glow::HasContext;
    let instances = PICTURE.with_borrow_mut(std::mem::take);
    let camera = VIEW_CAMERA.take();
    if rays() {
        let drawn = DRAWN.with_borrow_mut(std::mem::take);
        let projected: Vec<(u32, Block)> = BLOCKS.with_borrow_mut(std::mem::take).into_iter().collect();
        TILES.with_borrow_mut(|t| t.clear());
        if let Some(camera) = camera {
            // the whole level: every map block, the objects and actors not drawn (by their records)
            let blocks = match MAP_FRAME.get() {
                Some(f) => level_blocks(&f),
                None => projected,
            };
            let mut models = instances;
            let drawn_count = models.len();
            models.extend(level_objects(&drawn));
            if log_on() {
                static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 1000 == 0 {
                    super::log!("scene: frame of {} blocks, {drawn_count} drawn instances, {} more objects and actors", blocks.len(), models.len() - drawn_count);
                }
            }
            LAST.set(Some(Frame { camera, k, models, blocks, generation: GENERATION.get(), pages: PAGES.with_borrow(|p| p.clone()) }));
        }
        return;
    }
    let Some((c, cp)) = camera.filter(|_| overlay_on()) else {
        BLOCKS.with_borrow_mut(|b| b.clear());
        TILES.with_borrow_mut(|t| t.clear());
        return;
    };
    let m = super::mem();
    let focal = 4096.0 * h as f64 / 1920.0;
    let ndc = |p: [f64; 3]| -> Option<[f32; 2]> {
        (p[2] > 16.0).then(|| [(p[0] * k as f64 * focal / p[2] / (w as f64 * 0.5)) as f32, (p[1] * focal / p[2] / (h as f64 * 0.5)) as f32])
    };
    let mut lines: Vec<[f32; 2]> = Vec::new();
    for i in &instances {
        let faces = m.rd32(i.model + 0x24);
        let cam: Vec<Option<[f32; 2]>> = (0..i.count)
            .map(|n| {
                let a = i.list + 4 + 8 * (i.first + n);
                let v: [f64; 3] = std::array::from_fn(|j| m.rd16(a + 2 * j as u32) as i16 as f64);
                let w: [f64; 3] = std::array::from_fn(|j| (0..3).map(|r| v[r] * i.matrix[3 * r + j]).sum::<f64>() + i.offset[j] - cp[j]);
                ndc(std::array::from_fn(|j| (0..3).map(|r| w[r] * c[3 * r + j]).sum()))
            })
            .collect();
        for f in 0..m.rd32(faces).min(4096) {
            let a = faces + 4 + 12 * f;
            let corners = (m.rd8(a + 1) & 7) as u32;
            if corners != 3 && corners != 4 {
                continue;
            }
            let vs: Vec<u32> = (0..corners).map(|k| m.rd8(a + 4 + k) as u32).collect();
            if vs.iter().any(|&v| v < i.first || v >= i.first + i.count) {
                continue;
            }
            for e in 0..corners as usize {
                if let (Some(p), Some(q)) = (cam[(vs[e] - i.first) as usize], cam[(vs[(e + 1) % vs.len()] - i.first) as usize]) {
                    lines.push(p);
                    lines.push(q);
                }
            }
        }
    }
    let blocks = BLOCKS.with_borrow_mut(std::mem::take);
    let tiles = TILES.with_borrow_mut(std::mem::take);
    let to_cam = |w: [f64; 3]| -> [f64; 3] {
        let d: [f64; 3] = std::array::from_fn(|j| w[j] - cp[j]);
        std::array::from_fn(|j| (0..3).map(|r| d[r] * c[3 * r + j]).sum())
    };
    let ground_from = lines.len();
    for (sect, corners) in &tiles {
        let Some(b) = blocks.get(sect) else { continue };
        let n = if corners[3] == corners[0] { 3 } else { 4 };
        let p: Vec<Option<[f32; 2]>> = corners[..n].iter().map(|&i| terrain_point(*sect, b, i).and_then(|w| ndc(to_cam(w)))).collect();
        for e in 0..n {
            if let (Some(a), Some(q)) = (p[e], p[(e + 1) % n]) {
                lines.push(a);
                lines.push(q);
            }
        }
    }
    OVERLAY.with_borrow_mut(|o| {
        let (prog, vao, vbo) = *o.get_or_insert_with(|| unsafe {
            let prog = super::gl::compile(gl, LINE_VS, LINE_FS);
            let vao = gl.create_vertex_array().expect("GL vertex array");
            let vbo = gl.create_buffer().expect("GL buffer");
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            (prog, vao, vbo)
        });
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.viewport(0, 0, w as i32, h as i32);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.disable(glow::DEPTH_TEST);
            gl.use_program(Some(prog));
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            let bytes = std::slice::from_raw_parts(lines.as_ptr() as *const u8, lines.len() * 8);
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STREAM_DRAW);
            // models green, terrain yellow
            let tint = super::gl::uniform(gl, prog, "tint");
            gl.uniform_4_f32(tint.as_ref(), 0.1, 1.0, 0.2, 1.0);
            gl.draw_arrays(glow::LINES, 0, ground_from as i32);
            gl.uniform_4_f32(tint.as_ref(), 1.0, 0.85, 0.1, 1.0);
            gl.draw_arrays(glow::LINES, ground_from as i32, (lines.len() - ground_from) as i32);
        }
    });
}
