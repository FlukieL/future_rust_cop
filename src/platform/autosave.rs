//! Separate automatic menu-progress save. Manual save.cfg slots are untouched.
use super::{game_dir, log, mem};
use crate::rt::{call_original, Cpu, ESP};
use crate::ver::Us;
use std::sync::atomic::{AtomicBool, Ordering};

pub const RESET_PROGRESS: Us = Us(0x409a20);
pub const MISSION_PROGRESS: Us = Us(0x409630);
const STARTUP_CALL_RETURN: Us = Us(0x40ae8e);
const PROGRESS: Us = Us(0x511e00);
const SIZE: usize = 0x44;
const MAGIC: &[u8; 8] = b"FCOPA001";
static RESTORED: AtomicBool = AtomicBool::new(false);

fn checksum(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c9dc5u32, |hash, byte| (hash ^ u32::from(*byte)).wrapping_mul(0x01000193))
}

fn decode(data: &[u8]) -> Option<&[u8]> {
    if data.len() != MAGIC.len() + SIZE + 4 || &data[..8] != MAGIC {
        return None;
    }
    let payload = &data[8..8 + SIZE];
    let stored = u32::from_le_bytes(data[8 + SIZE..].try_into().ok()?);
    (checksum(payload) == stored).then_some(payload)
}

fn restore() {
    let path = game_dir().join("autosave.cfg");
    match std::fs::read(&path) {
        Ok(data) => {
            if let Some(payload) = decode(&data) {
                mem().slice(PROGRESS.at(), SIZE).copy_from_slice(payload);
                log!("autosave: restored {} (crime {}, precinct {:?})", path.display(), payload[2], &payload[7..13]);
            } else {
                log!("warn: autosave.cfg is invalid; keeping initialized progress");
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log!("warn: cannot read autosave.cfg: {e}"),
    }
}

fn save() {
    let payload = mem().slice(PROGRESS.at(), SIZE).to_vec();
    let mut data = Vec::with_capacity(8 + SIZE + 4);
    data.extend_from_slice(MAGIC);
    data.extend_from_slice(&payload);
    data.extend_from_slice(&checksum(&payload).to_le_bytes());
    let temp = game_dir().join("autosave.tmp");
    let path = game_dir().join("autosave.cfg");
    match std::fs::write(&temp, data).and_then(|()| std::fs::rename(&temp, &path)) {
        Ok(()) => log!("autosave: saved {}", path.display()),
        Err(e) => log!("warn: cannot write {}: {e}", path.display()),
    }
}

/// Restore once, only after the startup menu's own progress initializer.
pub fn reset_progress(c: &mut Cpu) {
    let caller = mem().rd32(c.r[ESP]);
    call_original(c, RESET_PROGRESS.at());
    if caller == STARTUP_CALL_RETURN.at() && !RESTORED.swap(true, Ordering::Relaxed) {
        restore();
    }
}

/// Save after the game has updated its mission progress.
pub fn mission_progress(c: &mut Cpu) {
    let m = mem();
    let before = m.slice(PROGRESS.at(), SIZE).to_vec();
    let game = m.rd32(Us(0x511f80).at());
    if game != 0 {
        log!("autosave: match result: state {}, players {}, result {}, level {}",
            m.rd32(Us(0x511fb8).at()), m.rd32(Us(0x511f50).at()),
            m.rd32(game + 0x10), m.rd32(game + 0x4c));
    } else {
        log!("autosave: progress update without a game record");
    }
    call_original(c, MISSION_PROGRESS.at());
    let after = mem().slice(PROGRESS.at(), SIZE).to_vec();
    log!("autosave: mode {} map {} crime {} -> {} precinct {:?} -> {:?}",
        after[3], after[0], before[2], after[2], &before[7..13], &after[7..13]);
    save();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_length_and_checksum() {
        let payload = [42; SIZE];
        let mut data = MAGIC.to_vec();
        data.extend_from_slice(&payload);
        data.extend_from_slice(&checksum(&payload).to_le_bytes());
        assert_eq!(decode(&data), Some(payload.as_slice()));
        data[9] ^= 1;
        assert!(decode(&data).is_none());
        assert!(decode(&data[..12]).is_none());
    }
}
