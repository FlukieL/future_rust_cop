//! Random menu music from the PC mission streams and, optionally, the PlayStation FE.MIS.
//! The Audio dialog selects tracks; choices are saved in the game's registry. Videos pause
//! playback and returning from a mission starts a new track.

use super::{advapi32, dsound, mem};
use crate::ver::Us;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MENU_UP: Us = Us(0x499a9c);
const VIDEO: Us = Us(0x499740);
const VIDEO_FLAGS: Us = Us(0x499750);
const GAME_STATE: Us = Us(0x511fb8);
const MUSIC_VOLUME: Us = Us(0x4a1d34);
const PC_RATE: u32 = 14260;

struct Track {
    name: String,
    frames: Arc<Vec<[i16; 2]>>,
    rate: u32,
    default_enabled: bool,
}

fn log() -> bool {
    static L: OnceLock<bool> = OnceLock::new();
    *L.get_or_init(|| std::env::var_os("FCOP_MUSIC_LOG").is_some_and(|v| v != "0"))
}

fn discover(game: &Path, ps1_enabled: bool) -> Vec<Track> {
    let mut tracks = Vec::new();
    let mut seen = HashSet::new();
    let missions = std::fs::read_dir(game).ok().and_then(|entries| {
        entries.flatten().map(|e| e.path()).find(|p| p.is_dir() && p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("missions")))
    });
    if let Some(missions) = missions {
        let mut files: Vec<_> = std::fs::read_dir(missions).into_iter().flatten().flatten().map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_none()).collect();
        files.sort_by_key(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase();
            (name.contains("joke"), name)
        });
        for path in files {
            let Ok(data) = std::fs::read(&path) else { continue };
            let mission = path.file_name().unwrap_or_default().to_string_lossy();
            for (i, samples) in crate::mission::streams(&data).music.into_iter().enumerate() {
                if samples.len() < 2 || !seen.insert(Sha256::digest(&samples)) {
                    continue;
                }
                let name = if i == 0 { format!("{mission}_music") } else { format!("{mission}_music{}", i + 1) };
                let frames = samples.chunks_exact(2).map(|s| [((s[0] as i16) - 128) * 256, ((s[1] as i16) - 128) * 256]).collect();
                tracks.push(Track { name, frames: Arc::new(frames), rate: PC_RATE, default_enabled: !mission.to_ascii_lowercase().contains("joke") });
            }
        }
    }
    if ps1_enabled {
        let path = game.parent().unwrap_or(game).join(crate::setup::PS1).join(crate::ps1::FILE);
        if let Some(frames) = std::fs::read(path).ok().and_then(|file| crate::ps1::menu_music(&file)).filter(|f| !f.is_empty()) {
            tracks.push(Track { name: "PlayStation menu".into(), frames: Arc::new(frames), rate: crate::ps1::RATE, default_enabled: true });
        }
    }
    tracks
}

fn tracks() -> &'static [Track] {
    static TRACKS: OnceLock<Vec<Track>> = OnceLock::new();
    TRACKS.get_or_init(|| {
        let Some(game) = super::game_dir_set() else { return Vec::new() };
        let ps1_enabled = !std::env::var_os("FCOP_MUSIC").is_some_and(|v| v == "0");
        let tracks = discover(game, ps1_enabled);
        if log() {
            eprintln!("menu music: discovered {} tracks", tracks.len());
        }
        tracks
    })
}

/// Track names and defaults for external settings editors such as the launcher.
pub fn catalog(game: &Path) -> Vec<(String, bool)> {
    discover(game, true).into_iter().map(|track| (track.name, track.default_enabled)).collect()
}

/// Registry name shared by the game and launcher.
pub fn key(name: &str) -> String {
    format!("MenuMusic_{name}")
}

fn saved(track: &Track) -> bool {
    advapi32::value("", &key(&track.name))
        .and_then(|(kind, bytes)| (kind == 4).then(|| bytes.get(..4)?.try_into().ok().map(u32::from_le_bytes)).flatten())
        .map_or(track.default_enabled, |v| v != 0)
}

/// Names and selected state, in stable order, for the Audio dialog.
pub fn choices() -> Vec<(String, bool)> {
    let tracks = tracks();
    let mut state = STATE.lock().unwrap();
    state.init(tracks);
    tracks.iter().zip(&state.enabled).map(|(t, &on)| (t.name.clone(), on)).collect()
}

/// Save all selections when the Audio dialog is confirmed.
pub fn set_choices(enabled: &[bool]) {
    let tracks = tracks();
    let mut state = STATE.lock().unwrap();
    state.init(tracks);
    for (i, (&on, track)) in enabled.iter().zip(tracks).enumerate() {
        state.enabled[i] = on;
        advapi32::set_value("", &key(&track.name), 4, &(on as u32).to_le_bytes());
    }
    if state.current.is_some_and(|i| !state.enabled[i]) {
        state.current = None;
        dsound::music_volume(0.0, false);
    }
    state.last = None;
}

struct State {
    enabled: Vec<bool>,
    initialized: bool,
    current: Option<usize>,
    last_track: Option<usize>,
    last: Option<(f32, bool, bool, u32)>,
    mission: bool,
    rng: u64,
}

impl State {
    fn init(&mut self, tracks: &[Track]) {
        if !self.initialized {
            self.enabled = tracks.iter().map(saved).collect();
            self.initialized = true;
            self.rng = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64 | 1;
        }
    }

    fn next(&mut self) -> Option<usize> {
        let candidates: Vec<_> = self.enabled.iter().enumerate().filter_map(|(i, &on)| on.then_some(i)).collect();
        if candidates.is_empty() {
            return None;
        }
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        let previous = self.last_track.filter(|i| candidates.contains(i) && candidates.len() > 1);
        let count = candidates.len() - usize::from(previous.is_some());
        let mut pick = (self.rng as usize) % count;
        for i in candidates {
            if previous == Some(i) {
                continue;
            }
            if pick == 0 {
                return Some(i);
            }
            pick -= 1;
        }
        None
    }
}

static STATE: Mutex<State> = Mutex::new(State {
    enabled: Vec::new(), initialized: false, current: None, last_track: None,
    last: None, mission: false, rng: 1,
});

/// At every pump, play or pause the music as the game is.
pub fn poll() {
    if super::game_dir_set().is_none() {
        return;
    }
    let tracks = tracks();
    let m = mem();
    let menu = m.rd32(MENU_UP.at()) != 0;
    let video = m.rd32(VIDEO.at()) != 0 && m.rd32(VIDEO_FLAGS.at()) & 1 != 0;
    let game = m.rd32(GAME_STATE.at());
    let on = menu && !video && game == 0;
    let gain = if on { m.rd32(MUSIC_VOLUME.at()).min(127) as f32 / 127.0 } else { 0.0 };
    let mut state = STATE.lock().unwrap();
    state.init(tracks);
    if game != 0 {
        state.mission = true;
    }
    let restart = on && std::mem::take(&mut state.mission);
    let next = on && state.enabled.iter().any(|&enabled| enabled)
        && (restart || state.current.is_none() || dsound::music_finished());
    if next {
        if let Some(i) = state.next() {
            if log() {
                eprintln!("menu music: playing {}", tracks[i].name);
            }
            dsound::music_set(tracks[i].frames.clone(), tracks[i].rate);
            state.current = Some(i);
            state.last_track = Some(i);
        } else {
            state.current = None;
        }
        state.last = None;
    }
    let now = (gain, menu, video, game);
    if state.last != Some(now) {
        dsound::music_volume(if state.current.is_some() { gain } else { 0.0 }, false);
        state.last = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(enabled: Vec<bool>) -> State {
        State { enabled, initialized: true, current: None, last_track: None, last: None, mission: false, rng: 12345 }
    }

    #[test]
    fn selection_handles_empty_single_and_multiple_tracks() {
        let mut s = state(vec![false, false, false]);
        assert_eq!(s.next(), None);
        s.enabled[1] = true;
        s.last_track = Some(1);
        assert_eq!(s.next(), Some(1));
        s.enabled[2] = true;
        for _ in 0..100 {
            let next = s.next().unwrap();
            assert_ne!(Some(next), s.last_track);
            assert!(s.enabled[next]);
            s.last_track = Some(next);
        }
    }

    fn chunk(samples: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; 28 + samples.len()];
        bytes[..4].copy_from_slice(b"CISM");
        let len = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&len.to_le_bytes());
        bytes[24..28].copy_from_slice(&((samples.len() / 2) as u32).to_le_bytes());
        bytes[28..].copy_from_slice(samples);
        bytes
    }

    #[test]
    fn discovers_deduplicated_tracks_and_disables_jokes() {
        let root = std::env::temp_dir().join(format!("fcop-menumusic-{}", std::process::id()));
        let missions = root.join("MiSsIoNs");
        std::fs::create_dir_all(&missions).unwrap();
        let mut regular = chunk(&[0, 255]);
        regular.extend(chunk(&[128, 129]));
        std::fs::write(missions.join("LEVEL"), regular).unwrap();
        std::fs::write(missions.join("JOKE"), chunk(&[4, 5])).unwrap();
        std::fs::write(missions.join("JOKE2"), chunk(&[0, 255])).unwrap();
        let tracks = discover(&root, false);
        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[0].name, "LEVEL_music");
        assert_eq!(tracks[1].name, "LEVEL_music2");
        assert_eq!(tracks[0].frames[0], [-32768, 32512]);
        assert!(tracks[0].default_enabled && tracks[1].default_enabled);
        assert_eq!(tracks[2].name, "JOKE_music");
        assert!(!tracks[2].default_enabled);
        std::fs::remove_dir_all(root).unwrap();
    }
}
