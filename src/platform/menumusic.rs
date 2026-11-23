//! The PlayStation version's menu music (`crate::ps1`) in the menus, which the PC version left
//! silent: from `game_files/PS1/FE.MIS` (`setup`), looping while the menu is up and no video
//! plays (a video only pauses it), from its start again after a mission. Its volume is the
//! game's Music Volume (Audio dialog, 0...127 at [0x4a1d34], set by 0x432a20), in proportion as
//! the effects' (`dsound::set_volume`). Without the file nothing changes. `FCOP_MUSIC=0` off,
//! `FCOP_MUSIC_LOG=1` logs when it plays.

use super::{dsound, mem};
use crate::ver::Us;
use std::sync::{Arc, Mutex, OnceLock};

/// Nonzero while the menu is up.
const MENU_UP: Us = Us(0x499a9c);
/// The video playing (0x408af0): graph, flags (bit 0: playing).
const VIDEO: Us = Us(0x499740);
const VIDEO_FLAGS: Us = Us(0x499750);
/// game_state: nonzero in a mission
const GAME_STATE: Us = Us(0x511fb8);
/// the Music Volume, 0...127
const MUSIC_VOLUME: Us = Us(0x4a1d34);

fn log() -> bool {
    static L: OnceLock<bool> = OnceLock::new();
    *L.get_or_init(|| std::env::var_os("FCOP_MUSIC_LOG").is_some_and(|v| v != "0"))
}

/// The music, decoded once (None: no FE.MIS, or switched off).
fn music() -> Option<&'static Arc<Vec<[i16; 2]>>> {
    static M: OnceLock<Option<Arc<Vec<[i16; 2]>>>> = OnceLock::new();
    M.get_or_init(|| {
        if std::env::var_os("FCOP_MUSIC").is_some_and(|v| v == "0") {
            return None;
        }
        let game = super::game_dir();
        let path = game.parent().unwrap_or(game).join(crate::setup::PS1).join(crate::ps1::FILE);
        let file = std::fs::read(&path).ok()?;
        let Some(m) = crate::ps1::menu_music(&file) else {
            eprintln!("menu music: {} holds none", path.display());
            return None;
        };
        if log() {
            eprintln!("menu music: {} s from {}", m.len() as u32 / crate::ps1::RATE, path.display());
        }
        let m = Arc::new(m);
        dsound::music_set(m.clone(), crate::ps1::RATE);
        Some(m)
    })
    .as_ref()
}

struct State {
    /// what was last set: (gain, menu up, video, game state)
    last: Option<(f32, bool, bool, u32)>,
    /// a mission ran since the music last played: start it again
    mission: bool,
}

static STATE: Mutex<State> = Mutex::new(State { last: None, mission: false });

/// At every pump (main thread): play or pause the music as the game is.
pub fn poll() {
    if music().is_none() {
        return;
    }
    let m = mem();
    let menu = m.rd32(MENU_UP.at()) != 0;
    let video = m.rd32(VIDEO.at()) != 0 && m.rd32(VIDEO_FLAGS.at()) & 1 != 0;
    let state = m.rd32(GAME_STATE.at());
    let on = menu && !video && state == 0;
    let gain = if on { m.rd32(MUSIC_VOLUME.at()).min(127) as f32 / 127.0 } else { 0.0 };
    let mut s = STATE.lock().unwrap();
    if state != 0 {
        s.mission = true;
    }
    let now = (gain, menu, video, state);
    if s.last == Some(now) {
        return;
    }
    let restart = on && std::mem::take(&mut s.mission);
    if log() {
        eprintln!("menu music: menu {menu} video {video} game state {state:#x}: volume {gain:.2}{}", if restart { ", from the start" } else { "" });
    }
    s.last = Some(now);
    dsound::music_volume(gain, restart);
}
