//! The scene in world space (PLAN-RTX.md step 2): what the game draws, as models and their
//! transforms, for acceleration structures and motion vectors later.
//!
//! model_draw 0x4280a0 (record, flags, mode) draws one model: the record holds +0 the model,
//! +4 a 3x3 matrix, +0x28 the position, +0x48 the animation frame (i16). The model points at
//! its mission chunks' data in guest memory: +0xc flags (0x2000000: in parts), +0x1c a block
//! whose +4 points at the `4DVL` count (8-byte vertices x y z pad behind it), +0x24 at the
//! `3DQL` count (12-byte faces behind it), +0x28 the `3DTL` entries.
//!
//! First stage: `FCOP_SCENE_LOG=1` logs each model the first time it is drawn and, for
//! comparison, the counts of the mission file's `Cobj` resources.

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
    call_original(c, MODEL_DRAW.at());
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
