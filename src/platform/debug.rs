//! Debug switches: what the `FCOP_*` test variables set, readable at any time. Each starts from
//! its environment variable (as before) and can be changed while the game runs in the debug
//! menu (Ctrl+Shift+D, only in a `--features cheats` build, `dialog::debug`); every reader asks
//! here each time, so a change applies at once. Only switches without a setting of their own in
//! the game's menus, and none that write or read files outside the log (recordings, shots,
//! scripts, texture dumps).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

/// What a switch holds.
pub enum Kind {
    /// on / off (default); set: anything but "0" (or empty) is on
    Flag(bool),
    /// one of these (environment value, shown name); the first is the default
    Choice(&'static [(&'static str, &'static str)]),
    /// a number (default, smallest, largest)
    Num(f32, f32, f32),
}

pub struct Var {
    pub env: &'static str,
    pub label: &'static str,
    pub page: usize,
    pub kind: Kind,
}

/// The debug menu's pages.
pub const PAGES: [&str; 5] = ["Views", "Renderer", "Shadows", "Logs", "Performance"];

macro_rules! vars {
    ($($id:ident: $env:literal, $label:literal, $page:literal, $kind:expr;)*) => {
        /// A switch (index into `VARS`).
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum V { $($id),* }
        pub static VARS: &[Var] = &[$(Var { env: $env, label: $label, page: $page, kind: $kind }),*];
        /// Every switch, in `VARS`' order.
        pub const ALL: &[V] = &[$(V::$id),*];
    };
}

const WALK: &[(&str, &str)] = &[("", "Ours"), ("0", "Game's"), ("check", "Check")];

vars! {
    GBuffer: "FCOP_GBUFFER", "G-buffer view", 0, Kind::Choice(&[("", "Picture"), ("depth", "Depth"), ("ao", "AO"), ("lights", "Lights"), ("shadows", "Shadows"), ("scene", "Scene lines"), ("rt", "Ray traced (Vulkan)"), ("effects", "Effects"), ("bloom", "Bloom"), ("motion", "Motion vectors"), ("reproject", "Reprojection check")]);
    RtHigh: "FCOP_RT_HIGH", "RT view from above", 0, Kind::Num(0.0, 0.0, 100000.0);
    Camera: "FCOP_CAMERA", "Player 1 camera", 0, Kind::Choice(&[("", "Game's"), ("first", "First person"), ("behind", "Chase")]);
    NoVideo: "FCOP_NO_VIDEO", "Skip videos", 0, Kind::Flag(false);
    Grab: "FCOP_GRAB", "Catch the cursor", 0, Kind::Flag(true);
    Precise: "FCOP_PRECISE", "Precise walker", 1, Kind::Flag(true);
    Walk: "FCOP_WALK", "Display list walker", 1, Kind::Choice(WALK);
    Terrain: "FCOP_TERRAIN", "Terrain pass", 1, Kind::Choice(WALK);
    Hd: "FCOP_HD", "Replacement textures", 1, Kind::Flag(true);
    Jitter: "FCOP_JITTER", "Jitter (for DLSS)", 1, Kind::Flag(false);
    ZSlack: "FCOP_ZSLACK", "Depth slack", 1, Kind::Num(0.001, 0.0, 0.1);
    AoStep: "FCOP_AO_STEP", "AO step", 1, Kind::Num(8.0, 0.0, 100.0);
    RtAoRays: "FCOP_RT_AO_RAYS", "RT AO rays / pixel", 1, Kind::Num(4.0, 1.0, 16.0);
    ShadowFaces: "FCOP_SHADOW_FACES", "Back faces cast", 2, Kind::Flag(true);
    ShadowFar: "FCOP_SHADOW_FAR", "Map: casters up to", 2, Kind::Num(super::shadows::FAR, 1000.0, 200000.0);
    ShadowReach: "FCOP_SHADOW_REACH", "Ring reach", 2, Kind::Num(super::shadows::RING_REACH, 0.0, 20000.0);
    ShadowRing: "FCOP_SHADOW_RING", "Ring (off screen)", 2, Kind::Choice(&[("", "On"), ("0", "Off"), ("wide", "Wide (test)"), ("all", "All (test)")]);
    ShadowStatic: "FCOP_SHADOW_STATIC", "Ring: static objects", 2, Kind::Flag(true);
    ShadowTerrain: "FCOP_SHADOW_TERRAIN", "Ring: terrain", 2, Kind::Choice(&[("", "On"), ("0", "Off"), ("all", "All (test)")]);
    TraceApi: "FCOP_TRACE_API", "Import calls", 3, Kind::Flag(false);
    TraceMsg: "FCOP_TRACE_MSG", "Window messages", 3, Kind::Flag(false);
    TraceReg: "FCOP_TRACE_REG", "Registry", 3, Kind::Flag(false);
    DdLog: "FCOP_DD_LOG", "DirectDraw", 3, Kind::Flag(false);
    DlgLog: "FCOP_DLG_LOG", "Dialogs", 3, Kind::Flag(false);
    VideoLog: "FCOP_VIDEO_LOG", "Videos", 3, Kind::Flag(false);
    PadLog: "FCOP_PAD_LOG", "Gamepad", 3, Kind::Flag(false);
    NetLog: "FCOP_NET_LOG", "Network", 3, Kind::Flag(false);
    SceneLog: "FCOP_SCENE_LOG", "Scene", 3, Kind::Flag(false);
    ShadowLog: "FCOP_SHADOW_LOG", "Shadows", 3, Kind::Flag(false);
    Fps: "FCOP_FPS", "Frames a second", 4, Kind::Flag(false);
    Phases: "FCOP_PHASES", "Effects' phases", 4, Kind::Flag(false);
    RtTime: "FCOP_RT_TIME", "RT GPU times", 4, Kind::Flag(false);
    Prof: "FCOP_PROF", "Time in imports", 4, Kind::Flag(false);
    Watch: "FCOP_WATCH", "Watch (hangs)", 4, Kind::Flag(false);
    Sample: "FCOP_SAMPLE", "Sample the game thread", 4, Kind::Flag(false);
}

/// The values: flags 0/1, choices their index, numbers their f32 bits.
fn values() -> &'static [AtomicU32] {
    static VALUES: OnceLock<Vec<AtomicU32>> = OnceLock::new();
    VALUES.get_or_init(|| VARS.iter().map(|v| AtomicU32::new(parse(&v.kind, std::env::var(v.env).ok().as_deref()))).collect())
}

/// A switch's value from its environment variable's text (None: its default).
fn parse(kind: &Kind, text: Option<&str>) -> u32 {
    match *kind {
        Kind::Flag(d) => text.map_or(d, |t| !t.is_empty() && t != "0") as u32,
        Kind::Choice(c) => text.and_then(|t| c.iter().position(|o| o.0 == t)).unwrap_or(0) as u32,
        Kind::Num(d, lo, hi) => text.and_then(|t| t.parse::<f32>().ok()).map_or(d, |x| x.clamp(lo, hi)).to_bits(),
    }
}

pub fn get(v: V) -> u32 {
    values()[v as usize].load(Relaxed)
}

pub fn set(v: V, value: u32) {
    values()[v as usize].store(value, Relaxed);
}

pub fn flag(v: V) -> bool {
    get(v) != 0
}

/// A choice's environment value ("" the default).
pub fn choice(v: V) -> &'static str {
    match VARS[v as usize].kind {
        Kind::Choice(c) => c.get(get(v) as usize).map_or("", |o| o.0),
        _ => "",
    }
}

pub fn num(v: V) -> f32 {
    f32::from_bits(get(v))
}

/// A number from its text, clamped to its range (None: not a number).
pub fn num_from(v: V, text: &str) -> Option<u32> {
    match VARS[v as usize].kind {
        Kind::Num(_, lo, hi) => text.trim().parse::<f32>().ok().filter(|x| x.is_finite()).map(|x| x.clamp(lo, hi).to_bits()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        assert_eq!(parse(&Kind::Flag(true), None), 1);
        assert_eq!(parse(&Kind::Flag(true), Some("0")), 0);
        assert_eq!(parse(&Kind::Flag(false), Some("1")), 1);
        assert_eq!(parse(&Kind::Choice(WALK), Some("check")), 2);
        assert_eq!(parse(&Kind::Choice(WALK), Some("nonsense")), 0);
        assert_eq!(f32::from_bits(parse(&Kind::Num(4.0, 1.0, 16.0), Some("99"))), 16.0);
        assert_eq!(f32::from_bits(parse(&Kind::Num(4.0, 1.0, 16.0), None)), 4.0);
        assert_eq!(VARS.len(), V::Sample as usize + 1);
    }
}
