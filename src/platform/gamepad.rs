//! The gamepad's own bindings, used besides the game's (issue #8). The game has one binding
//! per action (controls.txt: keyboard or joystick); players want the keyboard and a pad at the
//! same time. So each player has a second set of 14 bindings for their own pad (player 1 pad
//! 1, player 2 pad 2; `dinput::player_pad`), kept in `gamepad.txt` in the game folder, in
//! controls.txt's format (default: our Xbox profile, `dinput::xbox_profile`).
//!
//! binding_down 0x4162a0 (player, &binding) is hooked: an action is down when the game's
//! binding or ours is. It also notes how far each action is pushed (`amounts`): an action held
//! only by stick directions moves the robot in proportion (`dinput::robot_physics`).
//!
//! The "Gamepad" buttons (Preferences, pause bar) open the game's Controller dialog (113) on
//! these bindings: `begin` puts them where the game keeps its own (0x511c20, player 2 +0x80),
//! keys are not captured (`editing`, `user32`), saving controls.txt (0x417bc0) writes
//! gamepad.txt instead, and `end` puts the game's back.

use crate::ver::Us;
use super::{alloc, cstr, dinput, free, game_dir, guest_call, log, mem, put_cstr};
use crate::rt::{Cpu, EAX, ESP, call_original};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub const BINDING_DOWN: Us = Us(0x4162a0);
pub const SAVE_CONTROLS: Us = Us(0x417bc0);
/// The Controller dialog and its procedure.
pub const CONTROLLER: u32 = 113;
pub const CONTROLLER_PROC: Us = Us(0x402bb0);
/// The game's bindings: 14 (device, key) pairs per player, 0x80 apart.
const BINDINGS: Us = Us(0x511c20);
const SIZE: u32 = 0x80;
const ACTIONS: u32 = 14;
/// The default profiles the Controller dialog's OK sets (two dwords).
const PROFILE_CHOICES: Us = Us(0x4a01b0);
const FILE: &str = "gamepad.txt";
const SECONDARY_FILE: &str = "gamepad-secondary.txt";
const CONSOLE_SECONDARY: [(u32, u32); 14] = [
    (0, 0), (0, 0), (1, 14), (1, 13), (0, 0), (0, 0), (0, 0),
    (0, 0), (0, 0), (0, 0), (1, 0x12), (1, 0x13), (0, 0), (0, 0),
];
static SECONDARY_EDIT: AtomicBool = AtomicBool::new(false);
/// Profile changes are staged until the controller dialog is confirmed.
static PENDING_PROFILES: Mutex<[Option<bool>; 2]> = Mutex::new([None, None]);

/// Our bindings in guest memory (both players, `SIZE` apart); 0 until loaded.
static TABLES: Mutex<u32> = Mutex::new(0);
/// Whether the Controller dialog shows our bindings.
static EDITING: AtomicBool = AtomicBool::new(false);
/// The game's bindings and profile choices while it does.
static KEPT: Mutex<Option<(Vec<u8>, [u32; 2])>> = Mutex::new(None);
/// How far each player's actions were pushed at their last read: None when a digital binding
/// holds the action (or nothing does).
static AMOUNTS: Mutex<[[Option<f32>; ACTIONS as usize]; 2]> = Mutex::new([[None; ACTIONS as usize]; 2]);

/// Last reported original/mapped binding states, for opt-in input diagnostics.
static INPUT_TRACE: Mutex<[[Option<(bool, bool)>; ACTIONS as usize]; 2]> =
    Mutex::new([[None; ACTIONS as usize]; 2]);

fn trace_binding(player: u32, action: u32, game: (u32, u32), ours: (u32, u32),
                 secondary: (u32, u32), game_down: bool, our_down: bool, pad: Option<dinput::Pad>) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var_os("FCOP_PAD_LOG").is_some_and(|v| v != "0")) {
        return;
    }
    let value = (game_down, our_down);
    let mut previous = INPUT_TRACE.lock().unwrap();
    let slot = &mut previous[player as usize][action as usize];
    if *slot == Some(value) || slot.is_none() && value == (false, false) {
        return;
    }
    *slot = Some(value);
    drop(previous);
    log!("pad input: player {} action {} original {:?}={} mapped {:?}/{:?}={} device {:?} state {:?}",
        player + 1, action, game, game_down, ours, secondary, our_down,
        dinput::player_device(player as usize),
        pad.map(|p| (p.buttons(), p.stick, p.start_pressed())));
}

/// Our bindings, loaded at their first use.
fn tables(c: &mut Cpu) -> u32 {
    let mut t = TABLES.lock().unwrap();
    if *t != 0 {
        return *t;
    }
    let m = mem();
    let a = alloc(4 * SIZE);
    m.slice(a, 4 * SIZE as usize).fill(0);
    let xbox = dinput::xbox_profile();
    for p in 0..2 {
        for i in 0..ACTIONS * 2 {
            m.wr32(a + SIZE * p + 4 * i, m.rd32(xbox + 4 * i));
        }
    }
    if !game_dir().join(FILE).exists() {
        for p in 0..2 {
            write_secondary_defaults(a, p);
        }
    }
    *t = a;
    drop(t);
    load(c, a, FILE);
    load(c, a + 2 * SIZE, SECONDARY_FILE);
    a
}

fn write_secondary_defaults(table: u32, player: u32) {
    let m = mem();
    for (action, (device, key)) in CONSOLE_SECONDARY.into_iter().enumerate() {
        let b = table + 2 * SIZE + player * SIZE + 8 * action as u32;
        m.wr32(b, device);
        m.wr32(b + 4, key);
    }
}

pub fn reset_secondary_profile(player: u32, modern: bool) {
    if !secondary_editing() && player < 2 {
        PENDING_PROFILES.lock().unwrap()[player as usize] = Some(modern);
    }
}

pub fn secondary_editing() -> bool {
    SECONDARY_EDIT.load(Ordering::Relaxed)
}

/// Select which binding set the next Gamepad dialog edits.
pub fn select_secondary(secondary: bool) {
    SECONDARY_EDIT.store(secondary, Ordering::Relaxed);
}

/// Read gamepad.txt into `table` (lines the game cannot read keep the default).
fn load(c: &mut Cpu, table: u32, file: &str) {
    let Ok(text) = std::fs::read_to_string(game_dir().join(file)) else { return };
    let buf = alloc(0x80);
    for (k, line) in text.lines().take(2 * ACTIONS as usize).enumerate() {
        let (p, i) = (k as u32 / ACTIONS, k as u32 % ACTIONS);
        let entry = table + SIZE * p + 8 * i;
        if line.trim().eq_ignore_ascii_case("Unbound") {
            mem().wr32(entry, 0);
            mem().wr32(entry + 4, 0);
        } else {
            put_cstr(buf, line.trim(), 0x80);
            guest_call(c, dinput::BINDING_PARSE.at(), &[buf, entry]);
        }
    }
    free(buf);
}

/// Write `table` to gamepad.txt as the game writes controls.txt.
fn save(c: &mut Cpu, table: u32, file: &str) {
    let mut out = String::new();
    for p in 0..2 {
        for i in 0..ACTIONS {
            let b = table + SIZE * p + 8 * i;
            if mem().rd32(b) == 0 && mem().rd32(b + 4) == 0 {
                out += "Unbound";
            } else {
                let name = guest_call(c, dinput::BINDING_NAME.at(), &[b]);
                out += &cstr(name);
            }
            out += "\r\n";
        }
    }
    if let Err(e) = std::fs::write(game_dir().join(file), out) {
        log!("warn: cannot save {file}: {e}");
    }
}

/// The amounts of player p's actions at their last read (`dinput::analog_for`).
pub fn amounts(p: u32) -> [Option<f32>; ACTIONS as usize] {
    AMOUNTS.lock().unwrap()[p.min(1) as usize]
}

/// Evaluate the two controller bindings for an action and retain analog travel unless a
/// digital input is also held.
fn pad_action(pad: Option<dinput::Pad>, bindings: [(u32, u32); 2], fallback_down: bool) -> (bool, Option<f32>) {
    let inputs = bindings.map(|binding| {
        let down = binding.0 == 1 && pad.is_some_and(|p| dinput::key_down(&p, binding.1));
        let amount = pad.and_then(|p| dinput::binding_amount(p.stick, binding));
        (down, amount)
    });
    let digital = fallback_down || inputs.iter().any(|(down, amount)| *down && amount.is_none());
    let amount = inputs.iter().filter_map(|(down, amount)| if *down { *amount } else { None }).reduce(f32::max);
    (fallback_down || inputs.iter().any(|(down, _)| *down), if digital { None } else { amount })
}

/// binding_down hook: the game's binding or ours.
pub fn binding_down(c: &mut Cpu) {
    let m = mem();
    let (player, binding) = (m.rd32(c.r[ESP] + 4), m.rd32(c.r[ESP] + 8));
    let read = |b: u32| (m.rd32(b), m.rd32(b + 4));
    let game = read(binding);
    call_original(c, BINDING_DOWN.at());
    let action = binding.wrapping_sub(BINDINGS.at() + SIZE * player) / 8;
    // read_controls also asks for Escape (not one of the 14)
    if player > 1 || binding < BINDINGS.at() + SIZE * player || action >= ACTIONS || EDITING.load(Ordering::Relaxed) {
        return;
    }
    // Parsing saved bindings invokes guest code on the first read. Do not let those
    // calls replace the original input result or the registers returned to its caller.
    let registers = c.r;
    let flags = (c.cf, c.pf, c.zf, c.sf, c.of, c.df);
    let base = tables(c);
    c.r = registers;
    (c.cf, c.pf, c.zf, c.sf, c.of, c.df) = flags;
    let ours = read(base + SIZE * player + 8 * action);
    let secondary = read(base + 2 * SIZE + SIZE * player + 8 * action);
    let game_down = c.r[EAX] & 0xff != 0;
    let game_amount = dinput::binding_amount(dinput::current_pad(player).stick, game);
    let pad = dinput::player_pad(player);
    let fallback = match (action, ours) {
        (0, (1, 0x14)) => Some(11),
        (1, (1, 0x15)) => Some(12),
        (2, (1, 0x13)) => Some(14),
        (3, (1, 0x12)) => Some(13),
        _ => None,
    };
    let dpad = fallback.is_some_and(|button| {
        !(0..2).any(|slot| (0..ACTIONS).any(|i| {
            i != action && read(base + slot * 2 * SIZE + SIZE * player + 8 * i) == (1, button)
        })) && pad.is_some_and(|p| dinput::dpad_axis_down(&p, ours.1))
    });
    let (our_down, our_amount) = pad_action(pad, [ours, secondary], dpad);
    trace_binding(player, action, game, ours, secondary, game_down, our_down, pad);
    let digital = game_down && game_amount.is_none() || our_down && our_amount.is_none();
    let amount = [(game_down, game_amount), (our_down, our_amount)]
        .into_iter()
        .filter_map(|(down, a)| if down { a } else { None })
        .reduce(f32::max);
    AMOUNTS.lock().unwrap()[player as usize][action as usize] = if digital { None } else { amount };
    if our_down {
        c.r[EAX] = c.r[EAX] & !0xff | 1;
    }
}

/// Saving controls.txt (the Controller dialog's OK): gamepad.txt while it shows ours.
pub fn save_controls(c: &mut Cpu) {
    if editing() {
        let table = *TABLES.lock().unwrap();
        let offset = if secondary_editing() { 2 * SIZE } else { 0 };
        mem().slice(table + offset, 2 * SIZE as usize).copy_from_slice(&mem().slice(BINDINGS.at(), 2 * SIZE as usize).to_vec());
        let pending = std::mem::replace(&mut *PENDING_PROFILES.lock().unwrap(), [None, None]);
        for (player, profile) in pending.into_iter().enumerate() {
            match profile {
                Some(true) => write_secondary_defaults(table, player as u32),
                Some(false) => mem().slice(table + 2 * SIZE + player as u32 * SIZE, SIZE as usize).fill(0),
                None => {}
            }
        }
        save(c, table, FILE);
        save(c, table + 2 * SIZE, SECONDARY_FILE);
        // The original is not called on this path, so emulate its ret.
        c.r[ESP] += 4;
    } else {
        call_original(c, SAVE_CONTROLS.at());
    }
}

/// Whether the Controller dialog shows our bindings.
pub fn editing() -> bool {
    EDITING.load(Ordering::Relaxed)
}

/// Before the Controller dialog opens for the pad: ours where the game keeps its bindings.
pub fn begin(c: &mut Cpu) {
    *PENDING_PROFILES.lock().unwrap() = [None, None];
    let t = tables(c);
    let secondary = secondary_editing();
    let m = mem();
    let kept = m.slice(BINDINGS.at(), 2 * SIZE as usize).to_vec();
    *KEPT.lock().unwrap() = Some((kept, [m.rd32(PROFILE_CHOICES.at()), m.rd32(PROFILE_CHOICES.at() + 4)]));
    let ours = m.slice(t + if secondary { 2 * SIZE } else { 0 }, 2 * SIZE as usize).to_vec();
    m.slice(BINDINGS.at(), ours.len()).copy_from_slice(&ours);
    EDITING.store(true, Ordering::Relaxed);
}

/// After it closed: ours (changed when OK was pressed) back to our table, the game's back.
pub fn end() {
    let Some((kept, choices)) = KEPT.lock().unwrap().take() else { return };
    let m = mem();
    // save_controls commits edits only on OK; Cancel must leave both tables unchanged.
    m.slice(BINDINGS.at(), kept.len()).copy_from_slice(&kept);
    m.wr32(PROFILE_CHOICES.at(), choices[0]);
    m.wr32(PROFILE_CHOICES.at() + 4, choices[1]);
    EDITING.store(false, Ordering::Relaxed);
    SECONDARY_EDIT.store(false, Ordering::Relaxed);
    *PENDING_PROFILES.lock().unwrap() = [None, None];
}

#[cfg(test)]
mod tests {
    use super::{CONSOLE_SECONDARY, pad_action};
    use crate::platform::dinput::Pad;

    #[test]
    fn secondary_defaults_map_dpad_and_left_stick() {
        assert_eq!(CONSOLE_SECONDARY[2], (1, 14));
        assert_eq!(CONSOLE_SECONDARY[3], (1, 13));
        assert_eq!(CONSOLE_SECONDARY[10], (1, 0x12));
        assert_eq!(CONSOLE_SECONDARY[11], (1, 0x13));
    }

    #[test]
    fn either_slot_can_activate_and_digital_overrides_analog() {
        let bindings = [(1, 1), (1, 0x12)];
        let pad = Pad { stick: [-0.625, 0.0, 0.0, 0.0], ..Pad::default() };
        let (down, amount) = pad_action(Some(pad), bindings, false);
        assert!(down);
        assert!((amount.unwrap() - 0.5).abs() < 1e-6);
        let pad = Pad { buttons: 1 << 1, ..pad };
        assert_eq!(pad_action(Some(pad), bindings, false), (true, None));
        assert_eq!(pad_action(None, bindings, false), (false, None));
        assert_eq!(pad_action(Some(Pad::default()), bindings, false), (false, None));
    }

    #[test]
    fn primary_buttons_work_when_secondary_is_unbound() {
        let pad = Pad { buttons: 1 << 3, ..Pad::default() };
        assert_eq!(pad_action(Some(pad), [(1, 3), (0, 0)], false), (true, None));
        assert_eq!(pad_action(Some(Pad::default()), [(1, 3), (0, 0)], false), (false, None));
    }

    #[test]
    fn dpad_and_right_stick_both_turn() {
        let bindings = [(1, 0x17), CONSOLE_SECONDARY[2]];
        let stick = Pad { stick: [0.0, 0.0, 1.0, 0.0], ..Pad::default() };
        assert_eq!(pad_action(Some(stick), bindings, false), (true, Some(1.0)));
        let dpad = Pad { buttons: 1 << 14, ..Pad::default() };
        assert_eq!(pad_action(Some(dpad), bindings, false), (true, None));
    }
}
