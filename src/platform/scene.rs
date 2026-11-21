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
    /// points checked, their largest and summed distance (model units), since the last log
    static STATS: std::cell::Cell<(u64, f64, f64, std::option::Option<std::time::Instant>)> = const { std::cell::Cell::new((0, 0.0, 0.0, None)) };
}

const CAMERA: Us = Us(0x511f48);

fn log_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("FCOP_SCENE_LOG").is_ok_and(|v| v == "1"))
}

/// Hook of model_draw 0x4280a0.
pub fn model_draw(c: &mut Cpu) {
    if log_on() {
        note(c);
        CURRENT.set(super::mem().rd32(c.r[ESP] + 4));
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
    let now = std::time::Instant::now();
    let since = *since.get_or_insert(now);
    if now - since >= std::time::Duration::from_secs(2) {
        let (whole, inparts) = PICTURE.with_borrow(|v| (v.iter().filter(|i| i.first == 0).count(), v.iter().filter(|i| i.first != 0).count()));
        super::log!(
            "scene: {count} models checked against their records: position off by {:.2} model units on average, matrix by at most {:.4}; {whole} + {inparts} instances since",
            sum / count.max(1) as f64,
            max / 1000.0
        );
        PICTURE.with_borrow_mut(|v| v.clear());
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
    let Ok(file) = std::fs::read(super::host_path(&path)) else { return };
    for r in crate::mission::resources(&file).iter().filter(|r| r.kind == "Cobj") {
        let count = |tag: &str| crate::mission::chunks(&r.data).iter().find(|c| c.0 == tag).map(|c| u32::from_le_bytes(c.1[4..8].try_into().unwrap()));
        super::log!("scene: {path} Cobj {} vertices {:?} faces {:?}", r.id, count("4DVL"), count("3DQL"));
    }
}
