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
//! `FCOP_SCENE_LOG=1` logs each model the first time it is drawn, the mission file's `Cobj`
//! counts for comparison, and every 2 s how far our camera-space points are from the game's
//! (models not in parts, checked in project_points 0x457340: `check`).

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

/// project_points 0x457340 gets `n` camera-space points at `p` (before widescreen scales them):
/// compared with ours when they are a whole model's (not in parts).
pub fn check(p: u32, n: u32) {
    let record = CURRENT.get();
    if record == 0 {
        return;
    }
    let m = super::mem();
    let model = m.rd32(record);
    let vl = m.rd32(m.rd32(model + 0x1c) + 4);
    if m.rd32(model + 0xc) & 0x2000000 != 0 || m.rd32(vl) != n {
        return;
    }
    let rd = |a: u32| m.rd32(a) as i32 as f64;
    let r: Vec<f64> = (0..9).map(|k| rd(record + 4 + 4 * k) / 65536.0).collect();
    let pos: Vec<f64> = (0..3).map(|k| rd(record + 0x28 + 4 * k) / 16.0).collect();
    let cam = m.rd32(CAMERA.at());
    let cm: Vec<f64> = (0..9).map(|k| rd(cam + 0x28 + 4 * k) / 65536.0).collect();
    let cp: Vec<f64> = (0..3).map(|k| rd(cam + 0x7c + 4 * k) / 16.0).collect();
    let (mut count, mut max, mut sum, mut since) = STATS.get();
    for k in 0..n {
        let v: Vec<f64> = (0..3).map(|i| m.rd16(vl + 4 + 8 * k + 2 * i) as i16 as f64).collect();
        let w: Vec<f64> = (0..3).map(|j| (0..3).map(|i| v[i] * r[3 * i + j]).sum::<f64>() + pos[j] - cp[j]).collect();
        let ours: Vec<f64> = (0..3).map(|j| (0..3).map(|i| w[i] * cm[3 * i + j]).sum()).collect();
        let game: Vec<f64> = (0..3).map(|i| m.rd16(p + 8 * k + 2 * i) as i16 as f64).collect();
        let d = (0..3).map(|i| (ours[i] - game[i]).abs()).fold(0.0, f64::max);
        count += 1;
        sum += d;
        max = max.max(d);
    }
    let now = std::time::Instant::now();
    let since = *since.get_or_insert(now);
    if now - since >= std::time::Duration::from_secs(2) {
        super::log!("scene: {count} points checked, distance to the game's mean {:.2} max {:.2} (model units)", sum / count as f64, max);
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
