//! DirectInput 5 for the game's joysticks (it reads the keyboard with GetAsyncKeyState), on
//! gamepads through gilrs.
//!
//! joy_init 0x412590 (run at start and again later, after its cleanup 0x412740 released
//! everything) enumerates attached joysticks into room for two (no bounds check: we report at
//! most two), creates each one, asks for IDirectInputDevice2A, sets c_dfDIJoystick (80-byte
//! DIJOYSTATE), a range of -1000...1000 and a dead zone of 25% on X and Y, and acquires it.
//! Each frame joy_poll 0x412b80 polls and reads the state; any error but DIERR_INPUTLOST makes
//! it give the pad up, so we always answer DI_OK. The game reads only lX, lY (by their sign:
//! inside the dead zone they must be exactly 0) and buttons 0...16 (joy_key 0x412d70); with
//! two pads player i uses pad i, with one both share it. It looks for pads only at its start, so
//! it always finds two: the real pads behind them are chosen at every read (`connected`), and
//! pads plugged in later work. The original code loses the axis
//! magnitude there; hooks around player robot physics restore each assigned axis independently
//! for proportional translation, turning, and sidestepping in local games while keeping its
//! direction handling, collisions, and gravity.
//!
//! Our button numbers are 0 RT, 1 LT, 2 B, 3 A, 4 X, 5 Y, 6 LB, 7 RB, 8 Back,
//! 9 R3, 10 L3, 11...14 D-pad up, down, left, right. The Xbox profile maps the
//! PC game's actions to PlayStation-style positions (`PLAYSTATION_PROFILE`); existing saved
//! bindings are kept. Start is the Escape key (pause). The Controller dialog takes left-stick
//! directions and buttons 0...9 only (joy_capture 0x412c10): hooked for 10...16 and the right
//! stick. Right-stick directions use our binding keys 0x16...0x19; joy_key and controls.txt
//! parsing/formatting are hooked too, so those assignments work and persist.
//!
//! gilrs lives on the main thread and is polled at every `window::pump`; the game's threads
//! read the last state. `FCOP_PAD_LOG=1` logs pads and the game's use of them. `pad@...` items
//! in FCOP_INPUT drive a scripted pad (pad 0, before the real ones).

use crate::ver::Us;
use super::{Import, alloc, arg, cstr, free, guest_call, log, mem, put_cstr};
use crate::rt::{Cpu, EAX, ESP, call_original};
use gilrs::{Axis, Button, Gilrs};
use std::cell::{Cell, RefCell};
use std::sync::Mutex;

const DI_OK: u32 = 0;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_INVALIDARG: u32 = 0x8007_0057;
const DIERR_DEVICENOTREG: u32 = 0x8004_0154;

const DIDEVTYPE_JOYSTICK: u32 = 4;
const DIDEVTYPEJOYSTICK_GAMEPAD: u32 = 3;
const DIDC_ATTACHED: u32 = 1;
const DIPROP_RANGE: u32 = 4;
const DIPROP_DEADZONE: u32 = 5;
const DIPH_DEVICE: u32 = 0;
const DIPH_BYOFFSET: u32 = 1;

/// A GUID as its 16 bytes in memory.
const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let (a, b, c) = (d1.to_le_bytes(), d2.to_le_bytes(), d3.to_le_bytes());
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]]
}

const DI_D4: [u8; 8] = [0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0x00, 0x00];
const IID_IUNKNOWN: [u8; 16] = guid(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
const IID_IDIRECTINPUTA: [u8; 16] = guid(0x8952_1360, 0xaa8a, 0x11cf, DI_D4);
const IID_IDIRECTINPUTDEVICEA: [u8; 16] = guid(0x5944_e680, 0xc92e, 0x11cf, DI_D4);
const IID_IDIRECTINPUTDEVICE2A: [u8; 16] = guid(0x5944_e682, 0xc92e, 0x11cf, DI_D4);
/// Pad n's instance GUID has 0xfc0b0000 + n in its first dword; the product GUID is the base.
const PAD_GUID: u32 = 0xfc0b_0000;

fn pad_guid(n: u32) -> [u8; 16] {
    guid(PAD_GUID + n, 0xd5a0, 0x11cf, DI_D4)
}

fn trace() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var("FCOP_PAD_LOG").is_ok_and(|v| v != "0"))
}

macro_rules! pad_log {
    ($($t:tt)*) => { if trace() { log!("pad: {}", format!($($t)*)) } };
}

// ---- pad state --------------------------------------------------------------------------------

/// One pad as the game sees it: left and right sticks (-1...1, y down), our DirectInput
/// buttons (bit n = button n), Start.
#[derive(Clone, Copy, Default, PartialEq)]
pub struct Pad {
    pub stick: [f32; 4],
    pub(crate) buttons: u32,
    pub(crate) start: bool,
}

impl Pad {
    /// Mapped button mask, for controller diagnostics.
    pub fn buttons(&self) -> u32 {
        self.buttons
    }

    pub fn start_pressed(&self) -> bool {
        self.start
    }
}

/// A connected pad: its state, the buttons pressed since the game last read it (a short press
/// between two reads still counts: the Controller dialog reads every 250 ms), gilrs' id (None:
/// the scripted pad), its name.
struct Entry {
    pad: Pad,
    pressed: u32,
    id: Option<usize>,
    name: String,
    sony: bool,
}

/// The connected pads (the scripted one first).
static PADS: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
/// Start's new state on the first pad, for the next pump (it becomes the Escape key).
static START: Mutex<Option<bool>> = Mutex::new(None);
/// The scripted pad, once FCOP_INPUT has a `pad@` item.
static SCRIPTED: Mutex<Option<Pad>> = Mutex::new(None);

/// Explicit device IDs, automatic assignment, or the disabled sentinel.
pub const DISABLED_DEVICE: usize = (u32::MAX - 1) as usize;
static PLAYER_DEVICES: Mutex<[Option<usize>; 2]> = Mutex::new([None, None]);

pub fn controller_choices() -> (Vec<(String, u32)>, [Option<usize>; 2]) {
    let pads = PADS.lock().unwrap();
    let choices = pads.iter().filter_map(|p| {
        let id = p.id?;
        let value = u32::try_from(id).ok()?.checked_add(1)?;
        let display_id = if id >= 0x4000_0000 { id - 0x4000_0000 + 1 } else { id + 1 };
        Some((format!("{} ({})", p.name, display_id), value))
    }).collect();
    (choices, *PLAYER_DEVICES.lock().unwrap())
}

pub fn set_player_devices(devices: [Option<usize>; 2]) {
    *PLAYER_DEVICES.lock().unwrap() = devices;
}

/// Explicit devices never fall back to another controller when disconnected.
fn assigned(pads: &[Entry], player: usize, choices: [Option<usize>; 2], share_single: bool) -> Option<usize> {
    if let Some(id) = choices[player] {
        if id == DISABLED_DEVICE {
            return None;
        }
        return pads.iter().position(|p| p.id == Some(id));
    }
    if share_single && pads.len() == 1 && choices[1 - player].is_none() {
        return Some(0);
    }
    let reserved = choices[1 - player].filter(|&id| id != DISABLED_DEVICE);
    pads.iter().enumerate().filter(|(_, p)| p.id.is_none() || p.id != reserved)
        .nth(if reserved.is_some() || choices[1 - player] == Some(DISABLED_DEVICE) { 0 } else { player })
        .map(|(index, _)| index)
}

fn sony_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["sony", "playstation", "dualshock", "dualsense", "ps5 controller", "ps4 controller"]
        .iter().any(|part| name.contains(part))
}

/// Opt-in workaround for Steam Input exposing a mapped device alongside a Sony device.
#[cfg(windows)]
static STEAM_INPUT: super::light::Setting =
    super::light::Setting::with_default("SteamInputCompatibility", "FCOP_STEAM_INPUT", false);

/// In compatibility mode prefer non-Sony mapped devices when both kinds are visible.
/// This is deliberately opt-in: the devices may instead belong to two different players.
#[cfg(any(windows, test))]
fn filter_steam_duplicates(pads: &mut Vec<Entry>, enabled: bool) {
    if enabled && pads.iter().any(|p| p.id.is_some() && !p.sony) {
        pads.retain(|p| p.id.is_none() || !p.sony);
    }
}

/// Log device-list changes only when controller diagnostics are enabled.
fn trace_devices(detected: &[Entry], active: &[Entry]) {
    if !trace() {
        return;
    }
    type Devices = Vec<(Option<usize>, String, bool)>;
    static LAST: Mutex<Option<(Devices, Devices)>> = Mutex::new(None);
    let names = |entries: &[Entry]| entries.iter()
        .map(|entry| (entry.id, entry.name.clone(), entry.sony))
        .collect::<Devices>();
    let current = (names(detected), names(active));
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.as_ref() != Some(&current) {
        log!("pad devices: detected {:?}; active {:?}", current.0, current.1);
        *last = Some(current);
    }
}

/// Swap the first two connected controllers for both players, without changing their bindings.
static SWAP_PADS: super::light::Setting = super::light::Setting::with_default("SwapControllers", "FCOP_SWAP_PADS", false);

pub fn controllers_swapped() -> bool {
    SWAP_PADS.get()
}

pub fn set_controllers_swapped(swapped: bool) {
    SWAP_PADS.set(swapped);
}

fn swap_controllers<T>(pads: &mut [T], swapped: bool) {
    if swapped && pads.len() >= 2 {
        pads.swap(0, 1);
    }
}

// Analog movement in the current robot-physics call. The original game turns the stick
// into four bits; the movement-vector hooks below put its magnitude back.
thread_local! {
    static ANALOG_MOVE: Cell<Option<AnalogMove>> = const { Cell::new(None) };
}

fn scripted_pad() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(|| super::window::script_text().contains("pad@"))
}

thread_local! {
    /// gilrs on the main thread; None when it could not start (then no pads)
    /// (never dropped: its thread must not end at exit)
    static GILRS: RefCell<Option<Option<&'static mut Gilrs>>> = const { RefCell::new(None) };
}

const BUTTONS: [Button; 13] = [
    Button::East,
    Button::South,
    Button::West,
    Button::North,
    Button::LeftTrigger,
    Button::RightTrigger,
    Button::Select,
    Button::RightThumb,
    Button::LeftThumb,
    Button::DPadUp,
    Button::DPadDown,
    Button::DPadLeft,
    Button::DPadRight,
];
/// our button numbers for `BUTTONS`
const BUTTON_NUMBERS: [u32; 13] = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14];

/// Each pad's triggers' rest (pad, left): the lowest value seen, the first read at first. Some
/// pads (a "HID-konformer Gamecontroller") rest at ~0.45, which counted as pressed: the heavy
/// weapon (LT) fired all the time.
static REST: Mutex<Vec<((usize, bool), f32)>> = Mutex::new(Vec::new());

/// A trigger at `v` is pressed: 30% of its travel beyond its rest.
fn trigger_down(pad: usize, left: bool, v: f32) -> bool {
    let mut r = REST.lock().unwrap();
    let rest = match r.iter_mut().find(|e| e.0 == (pad, left)) {
        Some(e) => {
            e.1 = e.1.min(v);
            e.1
        }
        None => {
            r.push(((pad, left), v));
            v
        }
    };
    v > rest + 0.3 * (1.0 - rest)
}

/// Our button for a gilrs event of `pad` that presses one.
fn pressed_by(pad: usize, e: &gilrs::EventType) -> Option<u32> {
    match *e {
        gilrs::EventType::ButtonChanged(Button::RightTrigger2, v, _) if trigger_down(pad, false, v) => Some(0),
        gilrs::EventType::ButtonChanged(Button::LeftTrigger2, v, _) if trigger_down(pad, true, v) => Some(1),
        gilrs::EventType::ButtonPressed(b, _) => BUTTONS.iter().position(|&x| x == b).map(|k| BUTTON_NUMBERS[k]),
        _ => None,
    }
}

fn read(g: &gilrs::Gamepad) -> Pad {
    let id: usize = g.id().into();
    let trigger = |b, left| g.button_data(b).is_some_and(|d| trigger_down(id, left, d.value()));
    let mut buttons = trigger(Button::RightTrigger2, false) as u32 | (trigger(Button::LeftTrigger2, true) as u32) << 1;
    for (b, n) in BUTTONS.into_iter().zip(BUTTON_NUMBERS) {
        buttons |= (g.is_pressed(b) as u32) << n;
    }
    Pad {
        stick: [
            g.value(Axis::LeftStickX),
            -g.value(Axis::LeftStickY),
            g.value(Axis::RightStickX),
            -g.value(Axis::RightStickY),
        ],
        buttons,
        start: g.is_pressed(Button::Start),
    }
}

fn new_gilrs() -> Option<&'static mut Gilrs> {
    let g = Gilrs::new().map_err(|e| log!("warn: no gamepads: {e}")).ok()?;
    Some(Box::leak(Box::new(g)))
}

/// Start gilrs on the main thread before the game: pads are found some time after (Windows
/// reports them ~75 ms later), and the game looks for them once, at its start.
pub fn start() {
    GILRS.with(|g| {
        g.borrow_mut().get_or_insert_with(new_gilrs);
    });
}

/// Read the pads (main thread, at every pump). Returns Start's new state when it changed on the
/// first pad (it becomes the Escape key).
pub fn poll() -> Option<bool> {
    update();
    START.lock().unwrap().take()
}

fn update() {
    let scripted = {
        let mut s = SCRIPTED.lock().unwrap();
        if scripted_pad() {
            s.get_or_insert_default();
        }
        *s
    };
    let mut pads = PADS.lock().unwrap();
    let pressed_before = |id| pads.iter().find(|e| e.id == id).map_or(0, |e| e.pressed);
    let mut now: Vec<Entry> = scripted
        .map(|pad| Entry { pad, pressed: pressed_before(None) | pad.buttons, id: None, name: "Scripted pad".to_string(), sony: false })
        .into_iter()
        .collect();
    let mut has_dualsense = false;
    GILRS.with(|g| {
        let mut g = g.borrow_mut();
        let Some(g) = g.get_or_insert_with(new_gilrs) else { return };
        let mut pressed: Vec<(usize, u32)> = Vec::new();
        while let Some(e) = g.next_event() {
            match e.event {
                gilrs::EventType::Connected => {
                    if let Some(p) = g.connected_gamepad(e.id) {
                        pad_log!("connected: {} (USB {:04x}:{:04x})", p.name(), p.vendor_id().unwrap_or(0), p.product_id().unwrap_or(0));
                    } else {
                        pad_log!("connected event for unavailable controller {}", usize::from(e.id));
                    }
                }
                gilrs::EventType::Disconnected => pad_log!("disconnected controller {}", usize::from(e.id)),
                ref ev => {
                    if matches!(ev, gilrs::EventType::ButtonPressed(..) | gilrs::EventType::ButtonReleased(..)) {
                        pad_log!("event: device {} {:?}", usize::from(e.id), ev);
                    }
                    if let Some(b) = pressed_by(e.id.into(), ev) {
                        pressed.push((e.id.into(), 1 << b));
                    }
                }
            }
        }
        #[cfg(windows)]
        { has_dualsense = g.gamepads().any(|(_, p)| {
            p.vendor_id() == Some(0x054c) && matches!(p.product_id(), Some(0x0ce6 | 0x0df2))
        }); }
        now.extend(g.gamepads().map(|(id, p)| {
            let id: usize = id.into();
            let pad = read(&p);
            let new = pressed.iter().filter(|e| e.0 == id).fold(0, |a, e| a | e.1);
            Entry { pad, pressed: pressed_before(Some(id)) | new | pad.buttons, id: Some(id), name: p.name().to_string(), sony: p.vendor_id() == Some(0x054c) || sony_name(p.name()) }
        }));
    });
    #[cfg(windows)]
    if !has_dualsense {
        now.extend(super::dualsense_win::poll().into_iter().map(|(id, pad)| Entry {
            pad, pressed: pressed_before(Some(id)) | pad.buttons, id: Some(id),
            name: "DualSense (Windows joystick)".to_string(), sony: true,
        }));
    }
    #[cfg(not(windows))]
    let _ = has_dualsense;
    #[cfg(windows)]
    {
        if trace() {
            let detected = now.iter().map(|entry| Entry {
                pad: entry.pad, pressed: entry.pressed, id: entry.id,
                name: entry.name.clone(), sony: entry.sony,
            }).collect::<Vec<_>>();
            filter_steam_duplicates(&mut now, STEAM_INPUT.get());
            trace_devices(&detected, &now);
        } else {
            filter_steam_duplicates(&mut now, STEAM_INPUT.get());
        }
    }
    #[cfg(not(windows))]
    trace_devices(&now, &now);
    swap_controllers(&mut now, controllers_swapped());
    if trace() {
        // Log what each pad looks like to the game when it changes (sticks in halves).
        type Seen = Vec<(Option<usize>, u32, bool, [i8; 4])>;
        static SEEN: Mutex<Seen> = Mutex::new(Vec::new());
        let current: Seen = now.iter().map(|e| {
            (e.id, e.pad.buttons, e.pad.start, e.pad.stick.map(|v| (v * 2.0).round() as i8))
        }).collect();
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if *seen != current {
            for entry in &current {
                if !seen.contains(entry) {
                    log!("pad state: id={:?} buttons={:#06x} start={} sticks(x2)={:?}", entry.0, entry.1, entry.2, entry.3);
                }
            }
            *seen = current;
        }
    }
    #[cfg(windows)]
    if trace() {
        // Raw winmm reading of the Bluetooth DualSense fallback: logged when it changes
        // (coarsely) and at least every 3 seconds, to show whether Windows delivers input.
        type Raw = Option<(u32, [u32; 6], u32, u32)>;
        static RAW: Mutex<(Raw, Option<std::time::Instant>)> = Mutex::new((None, None));
        for e in now.iter() {
            let Some(id) = e.id.filter(|&id| id >= 0x4000_0000) else { continue };
            let raw: Raw = super::dualsense_win::diagnostic_raw(id).map(|(r, a, b, p)| (r, a.map(|v| v >> 12), b, p));
            let mut last = RAW.lock().unwrap_or_else(|e| e.into_inner());
            let due = last.1.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(3));
            if last.0 != raw || due {
                log!("pad raw: id={id} (result, axes>>12, buttons, pov)={raw:?}");
                *last = (raw, Some(std::time::Instant::now()));
            }
        }
    }
    let choices = *PLAYER_DEVICES.lock().unwrap();
    let ids = std::array::from_fn(|p| assigned(&now, p, choices, false).and_then(|k| now[k].id));
    GILRS.with(|g| {
        if let Some(Some(g)) = g.borrow_mut().as_mut() {
            super::rumble::service(g, ids);
        }
    });
    if super::record::on() {
        // the first real pad (with a press between two reads held for one)
        let e = now.iter().find(|e| e.id.is_some());
        let p = e.map_or(Pad::default(), |e| e.pad);
        let buttons = p.buttons | e.map_or(0, |e| e.pressed & !pressed_before(e.id));
        let mut v = [0.0; CONTROLS.len()];
        for (k, x) in v.iter_mut().enumerate().take(15) {
            *x = (buttons >> k & 1) as f32;
        }
        v[15] = p.start as u8 as f32;
        v[16..].copy_from_slice(&[p.stick[0], -p.stick[1], p.stick[2], -p.stick[3]]);
        super::record::pad(&v);
    }
    let choices = *PLAYER_DEVICES.lock().unwrap();
    let was = assigned(&pads, 0, choices, false).is_some_and(|k| pads[k].pad.start);
    let start = assigned(&now, 0, choices, false).is_some_and(|k| now[k].pad.start);
    *pads = now;
    if was != start {
        *START.lock().unwrap() = Some(start);
    }
}

/// The physical controller currently owning menu navigation (including the scripted pad).
static MENU_OWNER: Mutex<Option<Option<usize>>> = Mutex::new(None);
const MENU_BUTTONS: u32 = (1 << 2) | (1 << 3) | (0xf << 11);

/// Read exactly one controller until it releases its menu controls. A neutral frame separates
/// owners, allowing the menu to release any click before another controller takes over.
fn select_menu_pad(pads: &[Entry], owner: &mut Option<Option<usize>>) -> Pad {
    let active = |p: &Pad, threshold: f32| {
        p.start || p.buttons & MENU_BUTTONS != 0 || p.stick[0].abs().max(p.stick[1].abs()) >= threshold
    };
    if let Some(id) = *owner {
        if let Some(entry) = pads.iter().find(|entry| entry.id == id) {
            if active(&entry.pad, 0.35) {
                return entry.pad;
            }
        }
        *owner = None;
        return Pad::default();
    }
    if let Some(entry) = pads.iter().find(|entry| active(&entry.pad, 0.5)) {
        *owner = Some(entry.id);
        return entry.pad;
    }
    Pad::default()
}

/// A single active controller's menu state. Buttons, stick and Start never come from
/// different controllers.
pub fn menu_state() -> Pad {
    let pads = PADS.lock().unwrap();
    select_menu_pad(&pads, &mut MENU_OWNER.lock().unwrap())
}

/// Forget the presses the game has not read yet (a dialog used them).
pub fn forget_presses() {
    for e in PADS.lock().unwrap().iter_mut() {
        e.pressed = 0;
    }
}

/// The connected pad behind the game's pad n. The game always sees two pads (`enum_devices`),
/// so pads plugged in later work: with two connected pad i is pad i, one serves both (as the
/// game shares a single pad), none reads as centred and released.
fn connected(pads: &[Entry], n: u32) -> Option<usize> {
    assigned(pads, n.min(1) as usize, *PLAYER_DEVICES.lock().unwrap(), true)
}

/// Buffered presses are for the slow binding-capture dialog, not gameplay. Replaying
/// them during normal polling can make an old press appear held after its release.
fn read_pad_state(entry: &mut Entry, capturing: bool) -> Pad {
    let mut pad = entry.pad;
    let pressed = std::mem::take(&mut entry.pressed);
    if capturing {
        pad.buttons |= pressed;
    }
    pad
}

/// Pad n as the game reads it. Only binding capture replays buffered presses.
fn pad(n: u32) -> Pad {
    // fresh on the main thread (the Controller dialog reads from there)
    if GILRS.with(|g| g.borrow().is_some()) {
        update();
    }
    // Obtain dialog state before locking PADS: dialog handling may clear pending presses.
    let capturing = super::gamepad::editing() && super::dialog::pad_capture();
    let mut pads = PADS.lock().unwrap();
    let Some(k) = connected(&pads, n) else { return Pad::default() };
    read_pad_state(&mut pads[k], capturing)
}

/// The scripted pad's controls (FCOP_INPUT `pad@A*500`, `pad@LX=-1*1000`): our buttons in
/// order (the D-pad's too), then Start and both sticks' axes (y up, as gilrs has it).
pub const CONTROLS: [&str; 20] = [
    "RT", "LT", "B", "A", "X", "Y", "LB", "RB", "Back", "R3", "L3", "Up", "Down", "Left", "Right", "Start", "LX", "LY",
    "RX", "RY",
];

/// Set one control of the scripted pad (`CONTROLS[k]`): held when `v` is not 0, or the axis' value.
pub fn script(k: usize, v: f32) {
    let mut s = SCRIPTED.lock().unwrap();
    let p = s.get_or_insert_default();
    let on = v != 0.0;
    match k {
        0..=14 => p.buttons = p.buttons & !(1 << k) | (on as u32) << k,
        15 => p.start = on,
        16 => p.stick[0] = v,
        17 => p.stick[1] = -v,
        18 => p.stick[2] = v,
        _ => p.stick[3] = -v,
    }
}

/// Make sure the pads were read once (EnumDevices may come before the first pump).
fn ensure_polled() {
    super::window::pump(std::time::Duration::ZERO);
}

// ---- IDirectInputA ----------------------------------------------------------------------------
// object: vtable, references

fn guid_at(p: u32) -> [u8; 16] {
    mem().slice(p, 16).try_into().unwrap()
}

fn direct_input_create(c: &mut Cpu) -> u32 {
    let (version, out) = (arg(c, 1), arg(c, 2));
    let o = super::com_new(DIRECT_INPUT, 4);
    mem().wr32(o + 4, 1);
    mem().wr32(out, o);
    pad_log!("DirectInputCreateA(version {version:#x})");
    DI_OK
}

fn di_query(c: &mut Cpu) -> u32 {
    let (this, iid, out) = (arg(c, 0), guid_at(arg(c, 1)), arg(c, 2));
    if iid != IID_IUNKNOWN && iid != IID_IDIRECTINPUTA {
        mem().wr32(out, 0);
        return E_NOINTERFACE;
    }
    add_ref(c);
    mem().wr32(out, this);
    DI_OK
}

/// AddRef and Release of both interfaces: the count is at +4.
fn add_ref(c: &mut Cpu) -> u32 {
    let o = arg(c, 0);
    let n = mem().rd32(o + 4) + 1;
    mem().wr32(o + 4, n);
    n
}

fn release(c: &mut Cpu) -> u32 {
    let o = arg(c, 0);
    let n = mem().rd32(o + 4).saturating_sub(1);
    mem().wr32(o + 4, n);
    if n == 0 {
        free(o);
    }
    n
}

fn create_device(c: &mut Cpu) -> u32 {
    let (g, out) = (guid_at(arg(c, 1)), arg(c, 2));
    let n = u32::from_le_bytes(g[..4].try_into().unwrap()).wrapping_sub(PAD_GUID);
    if n >= 2 || g != pad_guid(n) {
        mem().wr32(out, 0);
        return DIERR_DEVICENOTREG;
    }
    let d = super::com_new(DEVICE, DEVICE_SIZE);
    let m = mem();
    m.wr32(d + 4, 1);
    m.wr32(d + 8, n);
    for axis in 0..2 {
        m.wr32(d + RANGE + 8 * axis, 0);
        m.wr32(d + RANGE + 8 * axis + 4, 0xffff);
    }
    m.wr32(out, d);
    pad_log!("CreateDevice: pad {n}");
    DI_OK
}

/// EnumDevices(type, callback, ref, flags): always two pads, DIDEVICEINSTANCEA (DX5). The game
/// looks for pads only at its start; `connected` finds the real one behind each when it reads.
fn enum_devices(c: &mut Cpu) -> u32 {
    let (kind, cb, user) = (arg(c, 1) & 0xff, arg(c, 2), arg(c, 3));
    if kind != 0 && kind != DIDEVTYPE_JOYSTICK {
        return DI_OK;
    }
    ensure_polled();
    let names: Vec<String> = {
        let pads = PADS.lock().unwrap();
        (0..2).map(|n| pads.get(n).map_or_else(|| format!("Gamepad {}", n + 1), |e| e.name.clone())).collect()
    };
    let ddi = alloc(0x244);
    let m = mem();
    for (n, name) in names.iter().enumerate() {
        pad_log!("EnumDevices: pad {n}: {name}");
        m.slice(ddi, 0x244).fill(0);
        m.wr32(ddi, 0x244);
        m.slice(ddi + 4, 16).copy_from_slice(&pad_guid(n as u32));
        m.slice(ddi + 0x14, 16).copy_from_slice(&pad_guid(0));
        m.wr32(ddi + 0x24, DIDEVTYPE_JOYSTICK | DIDEVTYPEJOYSTICK_GAMEPAD << 8);
        put_cstr(ddi + 0x28, name, 260);
        put_cstr(ddi + 0x12c, name, 260);
        if guest_call(c, cb, &[ddi, user]) == 0 {
            break;
        }
    }
    free(ddi);
    DI_OK
}

fn ok(_: &mut Cpu) -> u32 {
    DI_OK
}

fn not_impl(_: &mut Cpu) -> u32 {
    E_NOTIMPL
}

static DIRECT_INPUT: &[Import] = &[
    ("IDirectInputA::QueryInterface", 3, di_query),
    ("IDirectInputA::AddRef", 1, add_ref),
    ("IDirectInputA::Release", 1, release),
    ("IDirectInputA::CreateDevice", 4, create_device),
    ("IDirectInputA::EnumDevices", 5, enum_devices),
    ("IDirectInputA::GetDeviceStatus", 2, ok),
    ("IDirectInputA::RunControlPanel", 3, ok),
    ("IDirectInputA::Initialize", 3, ok),
];

// ---- IDirectInputDevice2A ---------------------------------------------------------------------
// object: vtable, references, pad number, X and Y: range (min, max), dead zone (0...10000)

const RANGE: u32 = 0xc;
const DEAD_ZONE: u32 = 0x1c;
const DEVICE_SIZE: u32 = 0x20;

fn dev_query(c: &mut Cpu) -> u32 {
    let (this, iid, out) = (arg(c, 0), guid_at(arg(c, 1)), arg(c, 2));
    if ![IID_IUNKNOWN, IID_IDIRECTINPUTDEVICEA, IID_IDIRECTINPUTDEVICE2A].contains(&iid) {
        mem().wr32(out, 0);
        return E_NOINTERFACE;
    }
    add_ref(c);
    mem().wr32(out, this);
    DI_OK
}

/// GetCapabilities(DIDEVCAPS): attached, no force feedback, 2 axes, 15 buttons, no POV.
fn get_capabilities(c: &mut Cpu) -> u32 {
    let p = arg(c, 1);
    let m = mem();
    let size = m.rd32(p);
    if size < 24 {
        return E_INVALIDARG;
    }
    m.slice(p + 4, size as usize - 4).fill(0);
    for (k, v) in [DIDC_ATTACHED, DIDEVTYPE_JOYSTICK | DIDEVTYPEJOYSTICK_GAMEPAD << 8, 2, 15, 0].into_iter().enumerate() {
        m.wr32(p + 4 + 4 * k as u32, v);
    }
    DI_OK
}

/// The axes (0 X, 1 Y) a property header (dwObj +8, dwHow +0xc) names.
fn axes_of(h: u32) -> std::ops::Range<u32> {
    match (mem().rd32(h + 0xc), mem().rd32(h + 8)) {
        (DIPH_DEVICE, _) => 0..2,
        (DIPH_BYOFFSET, o @ (0 | 4)) => o / 4..o / 4 + 1,
        _ => 0..0,
    }
}

/// SetProperty(property, header): the range and the dead zone of X and Y; others are accepted.
fn set_property(c: &mut Cpu) -> u32 {
    let (d, prop, h) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let m = mem();
    for axis in axes_of(h) {
        match prop {
            DIPROP_RANGE => {
                m.wr32(d + RANGE + 8 * axis, m.rd32(h + 0x10));
                m.wr32(d + RANGE + 8 * axis + 4, m.rd32(h + 0x14));
            }
            DIPROP_DEADZONE => m.wr32(d + DEAD_ZONE + 4 * axis, m.rd32(h + 0x10)),
            _ => {}
        }
    }
    pad_log!("SetProperty({prop}) of pad {}", m.rd32(d + 8));
    DI_OK
}

fn get_property(c: &mut Cpu) -> u32 {
    let (d, prop, h) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let m = mem();
    let axis = axes_of(h).start.min(1);
    match prop {
        DIPROP_RANGE => {
            m.wr32(h + 0x10, m.rd32(d + RANGE + 8 * axis));
            m.wr32(h + 0x14, m.rd32(d + RANGE + 8 * axis + 4));
        }
        DIPROP_DEADZONE => m.wr32(h + 0x10, m.rd32(d + DEAD_ZONE + 4 * axis)),
        _ => return E_NOTIMPL,
    }
    DI_OK
}

fn acquire(c: &mut Cpu) -> u32 {
    pad_log!("Acquire: pad {}", mem().rd32(arg(c, 0) + 8));
    DI_OK
}

/// An axis value in the device's range: the stick outside its dead zone, else exactly the
/// centre (the game tests the sign).
fn axis_value(d: u32, axis: u32, p: &Pad) -> u32 {
    let m = mem();
    let (lo, hi) = (m.rd32(d + RANGE + 8 * axis) as i32 as f32, m.rd32(d + RANGE + 8 * axis + 4) as i32 as f32);
    let dz = m.rd32(d + DEAD_ZONE + 4 * axis).min(9999) as f32 / 10000.0;
    let s = p.stick[axis as usize].clamp(-1.0, 1.0);
    let v = if s.abs() > dz { s.signum() * (s.abs() - dz) / (1.0 - dz) } else { 0.0 };
    let centre = (lo + hi) / 2.0;
    if v == 0.0 {
        return centre.floor() as i32 as u32;
    }
    let x = centre + v * (hi - lo) / 2.0;
    // away from the centre, so a small push never reads as the centre
    (if v > 0.0 { x.ceil() } else { x.floor() }).clamp(lo, hi) as i32 as u32
}

/// GetDeviceState(size, DIJOYSTATE): lX, lY, the other axes centred, POVs centred, buttons.
fn get_device_state(c: &mut Cpu) -> u32 {
    let (d, size, out) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let m = mem();
    let p = pad(m.rd32(d + 8));
    let mut s = [0u8; 0x50];
    s[0..4].copy_from_slice(&axis_value(d, 0, &p).to_le_bytes());
    s[4..8].copy_from_slice(&axis_value(d, 1, &p).to_le_bytes());
    s[0x20..0x30].fill(0xff);
    for b in 0..32 {
        s[0x30 + b] = if p.buttons >> b & 1 != 0 { 0x80 } else { 0 };
    }
    let n = (size as usize).min(s.len());
    m.slice(out, n).copy_from_slice(&s[..n]);
    DI_OK
}

static DEVICE: &[Import] = &[
    ("IDirectInputDevice2A::QueryInterface", 3, dev_query),
    ("IDirectInputDevice2A::AddRef", 1, add_ref),
    ("IDirectInputDevice2A::Release", 1, release),
    ("IDirectInputDevice2A::GetCapabilities", 2, get_capabilities),
    ("IDirectInputDevice2A::EnumObjects", 4, ok),
    ("IDirectInputDevice2A::GetProperty", 3, get_property),
    ("IDirectInputDevice2A::SetProperty", 3, set_property),
    ("IDirectInputDevice2A::Acquire", 1, acquire),
    ("IDirectInputDevice2A::Unacquire", 1, ok),
    ("IDirectInputDevice2A::GetDeviceState", 3, get_device_state),
    ("IDirectInputDevice2A::GetDeviceData", 5, not_impl),
    ("IDirectInputDevice2A::SetDataFormat", 2, ok),
    ("IDirectInputDevice2A::SetEventNotification", 2, ok),
    ("IDirectInputDevice2A::SetCooperativeLevel", 3, ok),
    ("IDirectInputDevice2A::GetObjectInfo", 4, not_impl),
    ("IDirectInputDevice2A::GetDeviceInfo", 2, not_impl),
    ("IDirectInputDevice2A::RunControlPanel", 3, ok),
    ("IDirectInputDevice2A::Initialize", 4, ok),
    ("IDirectInputDevice2A::CreateEffect", 5, not_impl),
    ("IDirectInputDevice2A::EnumEffects", 4, ok),
    ("IDirectInputDevice2A::GetEffectInfo", 3, not_impl),
    ("IDirectInputDevice2A::GetForceFeedbackState", 2, not_impl),
    ("IDirectInputDevice2A::SendForceFeedbackCommand", 2, not_impl),
    ("IDirectInputDevice2A::EnumCreatedEffectObjects", 4, ok),
    ("IDirectInputDevice2A::Escape", 2, not_impl),
    ("IDirectInputDevice2A::Poll", 1, ok),
    ("IDirectInputDevice2A::SendDeviceData", 5, not_impl),
];

// ---- the Controller dialog and extended axes ---------------------------------------------------

/// The original keys 0x12...0x15 are left, right, up, down on the left stick. These four
/// continue that private key space for the right stick; they are handled by our hooks only.
const RIGHT_LEFT: u32 = 0x16;
const RIGHT_RIGHT: u32 = 0x17;
const RIGHT_UP: u32 = 0x18;
const RIGHT_DOWN: u32 = 0x19;
const RIGHT_KEYS: [(&str, u32); 4] = [
    // The Controller dialog reads at most 19 characters plus NUL from each field. With its
    // "Joystick: " prefix these names make exactly 18 characters and survive OK/save.
    ("RStick L", RIGHT_LEFT),
    ("RStick R", RIGHT_RIGHT),
    ("RStick U", RIGHT_UP),
    ("RStick D", RIGHT_DOWN),
];
const PAD_KEYS: [(&str, u32); 15] = [
    ("Pad RT", 0), ("Pad LT", 1), ("Pad B", 2), ("Pad A", 3),
    ("Pad X", 4), ("Pad Y", 5), ("Pad LB", 6), ("Pad RB", 7),
    ("Pad Back", 8), ("Pad R3", 9), ("Pad L3", 10), ("Pad Up", 11),
    ("Pad Down", 12), ("Pad Left", 13), ("Pad Right", 14),
];
const MOUSE_KEYS: [(&str, u32); 5] =
    [("Mouse L", 0x01), ("Mouse R", 0x02), ("Mouse M", 0x04), ("Mouse X1", 0x05), ("Mouse X2", 0x06)];
const AXIS_DEAD_ZONE: f32 = 0.25;

fn named_key_from_text(text: &str, wanted_device: &str, keys: &[(&str, u32)]) -> Option<u32> {
    text.split_once(':').and_then(|(device, name)| {
        device
            .trim()
            .eq_ignore_ascii_case(wanted_device)
            .then(|| name.trim())
            .and_then(|name| keys.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|e| e.1))
    })
}

fn right_key_from_text(text: &str) -> Option<u32> {
    named_key_from_text(text, "Joystick", &RIGHT_KEYS)
}

fn mouse_key_from_text(text: &str) -> Option<u32> {
    named_key_from_text(text, "Keyboard", &MOUSE_KEYS)
}

/// Capturing a binding requires a deliberate deflection, not the smaller gameplay dead zone.
fn right_stick_key(p: &Pad) -> Option<u32> {
    const CAPTURE_THRESHOLD: f32 = 0.65;
    let [x, y] = [p.stick[2], p.stick[3]];
    if x.abs().max(y.abs()) < CAPTURE_THRESHOLD {
        None
    } else if x.abs() > y.abs() {
        Some(if x < 0.0 { RIGHT_LEFT } else { RIGHT_RIGHT })
    } else {
        Some(if y < 0.0 { RIGHT_UP } else { RIGHT_DOWN })
    }
}

fn right_stick_down(p: &Pad, key: u32) -> bool {
    match key {
        RIGHT_LEFT => p.stick[2] < -AXIS_DEAD_ZONE,
        RIGHT_RIGHT => p.stick[2] > AXIS_DEAD_ZONE,
        RIGHT_UP => p.stick[3] < -AXIS_DEAD_ZONE,
        RIGHT_DOWN => p.stick[3] > AXIS_DEAD_ZONE,
        _ => false,
    }
}

/// The game's pad n as it is now.
pub fn current_pad(n: u32) -> Pad {
    let pads = PADS.lock().unwrap();
    connected(&pads, n).map_or(Pad::default(), |k| pads[k].pad)
}

/// Player p's own pad for the gamepad bindings (`gamepad.rs`): pad p only (with one pad,
/// player 2 has none).
pub fn player_pad(p: u32) -> Option<Pad> {
    let pads = PADS.lock().unwrap();
    assigned(&pads, p.min(1) as usize, *PLAYER_DEVICES.lock().unwrap(), false).map(|k| pads[k].pad)
}

/// Whether the currently assigned controller has a PlayStation-style name.
pub fn player_is_sony(player: usize) -> bool {
    if player >= 2 {
        return false;
    }
    let pads = PADS.lock().unwrap();
    assigned(&pads, player, *PLAYER_DEVICES.lock().unwrap(), false)
        .is_some_and(|k| pads[k].sony || sony_name(&pads[k].name))
}

/// Physical device currently assigned to this player's own gamepad bindings.
pub fn player_device(player: usize) -> Option<usize> {
    if player >= 2 {
        return None;
    }
    let pads = PADS.lock().unwrap();
    assigned(&pads, player, *PLAYER_DEVICES.lock().unwrap(), false).and_then(|k| pads[k].id)
}

/// D-pad alternative for a left-stick direction binding.
pub fn dpad_axis_down(p: &Pad, key: u32) -> bool {
    let button = match key {
        0x12 => 13, 0x13 => 14, 0x14 => 11, 0x15 => 12,
        _ => return false,
    };
    p.buttons & (1 << button) != 0
}

/// Whether a joystick binding's key (`joy_key`'s numbers) is down on `p`: buttons 0...16, the
/// left stick 0x12...0x15 and the right stick 0x16...0x19 outside the dead zone.
pub fn key_down(p: &Pad, key: u32) -> bool {
    match key {
        0..=16 => p.buttons >> key & 1 != 0,
        0x12..=0x15 | RIGHT_LEFT..=RIGHT_DOWN => binding_amount(p.stick, (1, key)).is_some_and(|a| a > 0.0),
        _ => false,
    }
}

/// joy_key(pad, key): extend the game's four axis keys with the right stick directions.
pub const JOY_KEY: Us = Us(0x412d70);

pub fn joy_key(c: &mut Cpu) {
    let (pad, key) = (mem().rd32(c.r[ESP] + 4), mem().rd32(c.r[ESP] + 8));
    if (RIGHT_LEFT..=RIGHT_DOWN).contains(&key) {
        c.r[EAX] = right_stick_down(&current_pad(pad), key) as u32;
        // The original is not called on this path, so emulate its ret.
        c.r[ESP] += 4;
    } else {
        call_original(c, JOY_KEY.at());
    }
}

/// joy_capture(pad, &binding): the stick direction or button pressed, as a binding (device 1,
/// key); al = 1 when one was. The game looks at buttons 0...9 only: hooked for 10...16 (L3).
pub const JOY_CAPTURE: Us = Us(0x412c10);
const JOY_STATES: Us = Us(0x49fd98);

pub fn joy_capture(c: &mut Cpu) {
    let m = mem();
    let (pad, binding) = (m.rd32(c.r[ESP] + 4), m.rd32(c.r[ESP] + 8));
    let old = (m.rd32(binding), m.rd32(binding + 4));
    call_original(c, JOY_CAPTURE.at());
    // a pad choosing in the dialog does not bind (`dialog::pad_capture`); the pad's bindings
    // are made in its own dialog (`gamepad.rs`), the game's binds the keyboard
    if !super::dialog::pad_capture() || !super::gamepad::editing() {
        m.wr32(binding, old.0);
        m.wr32(binding + 4, old.1);
        c.r[EAX] &= !0xff;
        return;
    }
    if c.r[EAX] & 0xff == 0 && pad < 2 {
        if let Some(key) = right_stick_key(&current_pad(pad)) {
            m.wr32(binding, 1);
            m.wr32(binding + 4, key);
            c.r[EAX] = c.r[EAX] & !0xff | 1;
        } else {
            let buttons = JOY_STATES.at() + 0x50 * pad + 0x30;
            if let Some(b) = (10..=16).find(|&b| m.rd8(buttons + b) & 0x80 != 0) {
                m.wr32(binding, 1);
                m.wr32(binding + 4, b);
                c.r[EAX] = c.r[EAX] & !0xff | 1;
            }
        }
    }
    if c.r[EAX] & 0xff != 0 {
        super::dialog::pad_captured();
    }
}

/// The Choose Controller Profile dialog (165): its buttons Keyboard, Sidewinder Gamepad and
/// Gravis Gamepad show a profile's bindings (`PROFILE_SHOW`(dialog, table, 14)) and set
/// [`PROFILE_CHOICE`] to 0...2; after OK the Controller dialog applies the table at 0x48afb8
/// (player 2: 0x48b120) + choice * 0x78 with `PROFILE_APPLY`(dialog, player, table, 14). Our
/// "Xbox Controller" button (`extend.rs`) is choice 3, whose table is ours (issue #9).
pub const PROFILE_CHOICE: Us = Us(0x486290);
pub const PROFILE_SHOW: Us = Us(0x4037d0);
pub const PROFILE_APPLY: Us = Us(0x403380);
pub const XBOX_CHOICE: u32 = 3;
pub const CLASSIC_CHOICE: u32 = 4;
const PROFILES: [Us; 2] = [Us(0x48afb8), Us(0x48b120)];
const PROFILE_SIZE: u32 = 0x78;

/// Xbox equivalents of the PlayStation controls, in controls.txt action order.
const PLAYSTATION_PROFILE: [u32; 14] = [
    0x14, 0x15, RIGHT_RIGHT, RIGHT_LEFT, // left stick: forward/back; right stick: turn
    4, 6, 2, 5,             // X gun, LB action, B special, Y jump
    8, 9, 1, 0,             // Back map, R3 camera, LT/RT jink
    3, 7,                    // A heavy weapon, RB change target
];

/// Classic keeps the original left-stick turning and trigger strafing.
const CLASSIC_PROFILE: [u32; 14] = [
    0x14, 0x15, 0x13, 0x12,
    4, 6, 2, 5,
    8, 9, 1, 0,
    3, 7,
];

/// Build a game-compatible profile in guest memory.
fn make_profile(keys: [u32; 14]) -> u32 {
    let m = mem();
    let t = alloc(PROFILE_SIZE);
    for i in 0..PROFILE_SIZE / 4 {
        m.wr32(t + 4 * i, m.rd32(PROFILES[0].at() + PROFILE_SIZE + 4 * i));
    }
    for (action, key) in keys.into_iter().enumerate() {
        m.wr32(t + 8 * action as u32, 1);
        m.wr32(t + 8 * action as u32 + 4, key);
    }
    t
}

/// Modern console profile. Existing gamepad.txt bindings remain unchanged.
pub fn xbox_profile() -> u32 {
    static T: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *T.get_or_init(|| make_profile(PLAYSTATION_PROFILE))
}

/// Classic console profile, with the left stick turning instead of strafing.
pub fn classic_profile() -> u32 {
    static T: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *T.get_or_init(|| make_profile(CLASSIC_PROFILE))
}

/// The custom profile choice for the table pointer calculated by the game.
fn console_choice(player: u32, table: u32, profiles: [u32; 2]) -> Option<u32> {
    let base = *profiles.get(player as usize)?;
    [XBOX_CHOICE, CLASSIC_CHOICE].into_iter().find(|&choice| {
        table == base + choice * PROFILE_SIZE
    })
}

/// Replace the game's out-of-range profile tables with our own.
pub fn profile_apply(c: &mut Cpu) {
    let m = mem();
    let (player, table) = (m.rd32(c.r[ESP] + 8), c.r[ESP] + 12);
    // Each player can choose a different profile. The global PROFILE_CHOICE is only the
    // most recently previewed choice, so identify the profile from this player's table.
    let choice = console_choice(player, m.rd32(table), PROFILES.map(|p| p.at()));
    if let Some(choice) = choice {
        m.wr32(table, if choice == CLASSIC_CHOICE { classic_profile() } else { xbox_profile() });
    }
    call_original(c, PROFILE_APPLY.at());
    if let Some(choice) = choice.filter(|_| super::gamepad::editing()) {
        super::gamepad::reset_secondary_profile(player, choice == XBOX_CHOICE);
    }
}

/// controls.txt parsing and display. The game's fixed name tables do not contain mouse buttons
/// or our right-stick keys, so both need names in both directions to survive a restart.
pub const BINDING_PARSE: Us = Us(0x417870);
pub const BINDING_NAME: Us = Us(0x4179e0);
const BINDING_NAME_BUF: Us = Us(0x4a0410);

pub fn binding_parse(c: &mut Cpu) {
    let m = mem();
    let (text, binding) = (m.rd32(c.r[ESP] + 4), m.rd32(c.r[ESP] + 8));
    let text = cstr(text);
    let custom = (super::gamepad::editing() && text.trim().eq_ignore_ascii_case("Unbound")).then_some((0, 0))
        .or_else(|| mouse_key_from_text(&text).map(|key| (0, key)))
        .or_else(|| right_key_from_text(&text).map(|key| (1, key)))
        .or_else(|| named_key_from_text(&text, "Joystick", &PAD_KEYS).map(|key| (1, key)));
    if let Some((device, key)) = custom {
        m.wr32(binding, device);
        m.wr32(binding + 4, key);
        c.r[EAX] = 1;
        // The original is not called on this path, so emulate its ret.
        c.r[ESP] += 4;
    } else {
        // The original parser writes the device before it knows whether the name is valid.
        // Keep an invalid/truncated dialog field from leaving a half-updated live binding.
        let old = [m.rd32(binding), m.rd32(binding + 4)];
        call_original(c, BINDING_PARSE.at());
        if c.r[EAX] & 0xff == 0 {
            m.wr32(binding, old[0]);
            m.wr32(binding + 4, old[1]);
        }
    }
}

pub fn binding_name(c: &mut Cpu) {
    let binding = mem().rd32(c.r[ESP] + 4);
    let device = mem().rd32(binding);
    let key = mem().rd32(binding + 4);
    if super::gamepad::editing() && device == 0 && key == 0 {
        put_cstr(BINDING_NAME_BUF.at(), "Unbound", 64);
        c.r[EAX] = BINDING_NAME_BUF.at();
        c.r[ESP] += 4;
        return;
    }
    let custom = match device {
        0 => MOUSE_KEYS.iter().find(|e| e.1 == key).map(|e| ("Keyboard", e.0)),
        1 => RIGHT_KEYS.iter().chain(PAD_KEYS.iter()).find(|e| e.1 == key).map(|e| ("Joystick", e.0)),
        _ => None,
    };
    if let Some((device, name)) = custom {
        put_cstr(BINDING_NAME_BUF.at(), &format!("{device}: {name}"), 64);
        c.r[EAX] = BINDING_NAME_BUF.at();
        // The original is not called on this path, so emulate its ret.
        c.r[ESP] += 4;
    } else {
        call_original(c, BINDING_NAME.at());
    }
}

// ---- analog player movement ------------------------------------------------------------------

/// Player robot physics and the helpers that build its desired horizontal movement vector.
pub const ROBOT_PHYSICS: Us = Us(0x442120);
pub const HOVER_MOVE_VECTOR: Us = Us(0x443e80);
pub const WALK_MOVE_VECTOR: Us = Us(0x444f40);

const PLAYERS: Us = Us(0x511fd0);
const NET: Us = Us(0x511fa0);
const ROBOT_DATA: u32 = 0xac;
const CONTROLS_BITS: u32 = 0x30;
const HEADING: u32 = 0xb0;
const MOVE_BITS: u32 = 0xf030_0000;
const TURN_BITS: u32 = 0x3000_0000;

#[derive(Clone, Copy)]
struct AnalogMove {
    /// Forward/back and sidestep magnitudes when either active action is analog.
    vector: Option<[f32; 2]>,
    turn: Option<f32>,
}

/// DirectInput's per-axis dead zone, as configured by the game. The remaining travel is
/// rescaled, so leaving the dead zone starts at zero speed.
fn movement_axis(v: f32) -> f32 {
    const DEAD_ZONE: f32 = 0.25;
    let v = v.clamp(-1.0, 1.0);
    if v.abs() <= DEAD_ZONE {
        0.0
    } else {
        v.signum() * (v.abs() - DEAD_ZONE) / (1.0 - DEAD_ZONE)
    }
}

/// Magnitude of one active action's assigned stick direction. A non-axis binding is digital.
pub fn binding_amount(stick: [f32; 4], binding: (u32, u32)) -> Option<f32> {
    if binding.0 != 1 {
        return None;
    }
    let amount = match binding.1 {
        0x12 => -stick[0],
        0x13 => stick[0],
        0x14 => -stick[1],
        0x15 => stick[1],
        RIGHT_LEFT => -stick[2],
        RIGHT_RIGHT => stick[2],
        RIGHT_UP => -stick[3],
        RIGHT_DOWN => stick[3],
        _ => return None,
    };
    Some(movement_axis(amount).max(0.0))
}

/// Scale each active action independently by its amount (None: held by a digital binding, see
/// `gamepad::binding_down`). If an active action is digital, that dimension remains digital; it
/// does not disable analog handling of the other actions.
fn analog_amounts(amounts: [Option<f32>; 14], controls: u32) -> AnalogMove {
    let amount = |actions: &[(usize, u32, f32)]| {
        let mut analog = false;
        let mut amount = 0.0;
        for &(action, bit, direction) in actions {
            if controls & bit == 0 {
                continue;
            }
            if let Some(a) = amounts[action] {
                analog = true;
                amount += direction * a;
            } else {
                amount += direction;
            }
        }
        (analog, amount.clamp(-1.0, 1.0))
    };
    let translation = amount(&[(0, 0x4000_0000, 1.0), (1, 0x8000_0000, -1.0)]);
    let turn = amount(&[(2, 0x2000_0000, 1.0), (3, 0x1000_0000, -1.0)]);
    let sidestep = amount(&[(10, 0x0010_0000, -1.0), (11, 0x0020_0000, 1.0)]);
    let vector = (translation.0 || sidestep.0).then_some([translation.1, sidestep.1]);
    AnalogMove {
        vector,
        turn: turn.0.then_some(turn.1.abs()),
    }
}

/// Analog amounts for the active local-player actions. Network input contains only direction
/// bits, not magnitude; retaining digital movement there keeps both peers deterministic.
fn analog_for(actor: u32, controls: u32) -> Option<AnalogMove> {
    let m = mem();
    if m.rd32(NET.at()) as i32 >= 0 {
        return None;
    }
    let player = (0..2).find(|&p| m.rd32(PLAYERS.at() + 4 * p) == actor)?;
    let analog = analog_amounts(super::gamepad::amounts(player), controls);
    (analog.vector.is_some() || analog.turn.is_some()).then_some(analog)
}

fn scale_move_speed(speed: i32, amount: f32) -> i32 {
    (speed as f32 * amount).round() as i32
}

fn move_vector(c: &mut Cpu, addr: u32) {
    let m = mem();
    let sp = c.r[ESP];
    let mut restore = None;
    ANALOG_MOVE.with(|a| {
        if let Some([forward, side]) = a.get().and_then(|a| a.vector) {
            let actor = m.rd32(sp + 4);
            let speed = m.rd32(sp + 8) as i32;
            let side_mode = m.rd32(sp + 12);
            let strafe = m.rd32(sp + 20);
            if strafe == 0 && side_mode == 0 {
                m.wr32(sp + 8, scale_move_speed(speed, forward.abs()) as u32);
                restore = Some((actor, None, speed, side_mode, strafe));
                return;
            }

            let x = m.rd32(actor + 0xa0) as i32;
            let z = m.rd32(actor + 0xa8) as i32;
            let length = forward.hypot(side);
            if length == 0.0 {
                m.wr32(sp + 8, 0);
            } else {
                let nx = (x as f32 * forward + z as f32 * side) / length;
                let nz = (z as f32 * forward - x as f32 * side) / length;
                m.wr32(actor + 0xa0, nx.round() as i32 as u32);
                m.wr32(actor + 0xa8, nz.round() as i32 as u32);
                m.wr32(sp + 8, scale_move_speed(speed.abs(), length.min(1.0)) as u32);
            }
            m.wr32(sp + 12, 0);
            m.wr32(sp + 20, 0);
            restore = Some((actor, Some((x, z)), speed, side_mode, strafe));
        }
    });
    call_original(c, addr);
    if let Some((actor, direction, speed, side_mode, strafe)) = restore {
        if let Some((x, z)) = direction {
            m.wr32(actor + 0xa0, x as u32);
            m.wr32(actor + 0xa8, z as u32);
        }
        m.wr32(sp + 8, speed as u32);
        m.wr32(sp + 12, side_mode);
        m.wr32(sp + 20, strafe);
    }
}

pub fn hover_move_vector(c: &mut Cpu) {
    move_vector(c, HOVER_MOVE_VECTOR.at());
}

pub fn walk_move_vector(c: &mut Cpu) {
    move_vector(c, WALK_MOVE_VECTOR.at());
}

/// Run the original physics with proportional desired translation, then scale only the
/// heading change requested by left/right. The heading is a 12-bit angle.
pub fn robot_physics(c: &mut Cpu) {
    let m = mem();
    let actor = m.rd32(c.r[ESP] + 4);
    let data = m.rd32(actor + ROBOT_DATA);
    let was_hover = m.rd8(data + 0xad) == 1;
    let controls = m.rd32(data + CONTROLS_BITS);
    let analog = (controls & MOVE_BITS != 0).then(|| analog_for(actor, controls)).flatten();
    let before = m.rd16(actor + HEADING) as i32 & 0xfff;
    let previous = ANALOG_MOVE.replace(analog);
    call_original(c, ROBOT_PHYSICS.at());
    ANALOG_MOVE.set(previous);
    if was_hover != (m.rd8(data + 0xad) == 1) {
        super::rumble::transformed(actor);
    }

    let Some(amount) = analog.and_then(|a| a.turn) else { return };
    debug_assert!(controls & TURN_BITS != 0);
    let after = m.rd16(actor + HEADING) as i32 & 0xfff;
    let delta = (after - before + 0x800).rem_euclid(0x1000) - 0x800;
    let heading = before + (delta as f32 * amount).round() as i32;
    m.wr16(actor + HEADING, heading.rem_euclid(0x1000) as u16);
}

#[cfg(test)]
mod analog_tests {
    use super::{
        AnalogMove, MOUSE_KEYS, MOVE_BITS, Pad, RIGHT_DOWN, RIGHT_KEYS, RIGHT_LEFT, RIGHT_RIGHT, RIGHT_UP, binding_amount,
        key_down, mouse_key_from_text, movement_axis, right_key_from_text, right_stick_down, right_stick_key,
        scale_move_speed,
    };

    fn analog_amounts(stick: [f32; 4], bindings: [(u32, u32); 14], controls: u32) -> AnalogMove {
        super::analog_amounts(bindings.map(|b| binding_amount(stick, b)), controls)
    }

    #[test]
    fn gameplay_does_not_replay_released_buttons_but_capture_can() {
        let mut entry = super::Entry {
            pad: Pad::default(), pressed: 1 << 3, id: Some(1),
            name: String::new(), sony: false,
        };
        assert_eq!(super::read_pad_state(&mut entry, false).buttons, 0);
        assert_eq!(entry.pressed, 0);
        entry.pad.buttons = 1 << 4;
        entry.pressed = 1 << 3;
        assert_eq!(super::read_pad_state(&mut entry, false).buttons, 1 << 4);
        entry.pad.buttons = 0;
        assert_eq!(super::read_pad_state(&mut entry, false).buttons, 0);
        entry.pressed = 1 << 3;
        assert_eq!(super::read_pad_state(&mut entry, true).buttons, 1 << 3);
        assert_eq!(super::read_pad_state(&mut entry, true).buttons, 0);
    }

    #[test]
    fn steam_compatibility_prefers_mapped_devices_only_when_enabled_and_available() {
        let entry = |id, sony| super::Entry {
            pad: Pad::default(), pressed: 0, id, name: String::new(), sony,
        };
        let mut mixed = vec![entry(Some(1), true), entry(Some(2), false), entry(None, false)];
        super::filter_steam_duplicates(&mut mixed, false);
        assert_eq!(mixed.len(), 3);
        super::filter_steam_duplicates(&mut mixed, true);
        assert_eq!(mixed.iter().map(|e| e.id).collect::<Vec<_>>(), vec![Some(2), None]);
        let mut sony_only = vec![entry(Some(1), true)];
        super::filter_steam_duplicates(&mut sony_only, true);
        assert_eq!(sony_only.len(), 1);
        assert!(super::sony_name("Wireless DualSense Controller"));
        assert!(!super::sony_name("Xbox Controller"));
    }

    #[test]
    fn menu_uses_one_controller_and_handoffs_after_release_or_disconnect() {
        let entry = |id, buttons, x| super::Entry {
            pad: Pad { buttons, stick: [x, 0.0, 0.0, 0.0], ..Pad::default() },
            pressed: 0, id: Some(id), name: String::new(), sony: false,
        };
        let mut owner = None;
        let mut pads = [entry(4, 1 << 3, 0.0), entry(7, 1 << 2, 0.9)];
        assert_eq!(super::select_menu_pad(&pads, &mut owner).buttons, 1 << 3);
        pads.swap(0, 1);
        assert_eq!(super::select_menu_pad(&pads, &mut owner).buttons, 1 << 3);
        pads[1].pad.buttons = 0;
        assert_eq!(super::select_menu_pad(&pads, &mut owner).buttons, 0);
        assert_eq!(super::select_menu_pad(&pads, &mut owner).buttons, 1 << 2);
        assert_eq!(super::select_menu_pad(&pads[1..], &mut owner).buttons, 0);
        assert_eq!(owner, None);
    }

    #[test]
    fn menu_start_obeys_controller_ownership() {
        let entry = |id, pad| super::Entry {
            pad, pressed: 0, id: Some(id), name: String::new(), sony: false,
        };
        let mut owner = None;
        let mut pads = [
            entry(1, Pad { buttons: 1 << 3, ..Pad::default() }),
            entry(2, Pad { start: true, ..Pad::default() }),
        ];
        let active = super::select_menu_pad(&pads, &mut owner);
        assert_eq!(active.buttons, 1 << 3);
        assert!(!active.start);
        pads[0].pad.buttons = 0;
        assert!(!super::select_menu_pad(&pads, &mut owner).start);
        assert!(super::select_menu_pad(&pads, &mut owner).start);
        assert_eq!(owner, Some(Some(2)));
        pads[0].pad.buttons = 1 << 3;
        let active = super::select_menu_pad(&pads, &mut owner);
        assert!(active.start);
        assert_eq!(active.buttons, 0);
        assert!(!super::select_menu_pad(&pads[..1], &mut owner).start);
    }

    #[test]
    fn menu_owner_keeps_stick_hysteresis_and_scripted_identity() {
        let entry = |id, x, buttons| super::Entry {
            pad: Pad { buttons, stick: [x, 0.0, 0.0, 0.0], ..Pad::default() },
            pressed: 0, id, name: String::new(), sony: false,
        };
        let mut owner = None;
        let mut pads = [entry(None, 0.49, 0), entry(Some(7), 0.0, 0)];
        assert_eq!(super::select_menu_pad(&pads, &mut owner).stick[0], 0.0);
        pads[0].pad.stick[0] = 0.6;
        pads[1].pad.buttons = 1 << 3;
        assert_eq!(super::select_menu_pad(&pads, &mut owner).stick[0], 0.6);
        assert_eq!(owner, Some(None));
        pads[0].pad.stick[0] = 0.4;
        assert_eq!(super::select_menu_pad(&pads, &mut owner).stick[0], 0.4);
        pads[0].pad.stick[0] = 0.3;
        assert_eq!(super::select_menu_pad(&pads, &mut owner).buttons, 0);
        assert_eq!(super::select_menu_pad(&pads, &mut owner).buttons, 1 << 3);
    }

    #[test]
    fn player_device_selection_handles_automatic_and_disconnects() {
        let entry = |id| super::Entry {
            pad: Pad::default(), pressed: 0, id: Some(id), name: String::new(), sony: false,
        };
        let pads = [entry(4), entry(7), entry(9)];
        assert_eq!(super::assigned(&pads, 0, [None, None], false), Some(0));
        assert_eq!(super::assigned(&pads, 1, [None, None], false), Some(1));
        assert_eq!(super::assigned(&pads, 0, [Some(9), None], false), Some(2));
        assert_eq!(super::assigned(&pads, 1, [Some(9), None], false), Some(0));
        assert_eq!(super::assigned(&pads, 0, [Some(99), None], false), None);
        assert_eq!(super::assigned(&pads, 1, [Some(99), None], false), Some(0));
        assert_eq!(super::assigned(&pads[..1], 1, [None, None], true), Some(0));
        assert_eq!(super::assigned(&pads[..1], 1, [None, None], false), None);
        assert_eq!(super::assigned(&pads, 0, [Some(super::DISABLED_DEVICE), None], false), None);
        assert_eq!(super::assigned(&pads, 1, [Some(super::DISABLED_DEVICE), None], false), Some(0));
        assert_eq!(super::assigned(&pads[..1], 1, [Some(super::DISABLED_DEVICE), None], false), Some(0));
        assert_eq!(super::assigned(&pads, 0, [None, Some(super::DISABLED_DEVICE)], false), Some(0));
    }

    #[test]
    fn controller_swap_handles_zero_one_and_multiple_pads() {
        let mut empty: [u8; 0] = [];
        super::swap_controllers(&mut empty, true);
        let mut one = [1];
        super::swap_controllers(&mut one, true);
        assert_eq!(one, [1]);
        let mut two = [1, 2, 3];
        super::swap_controllers(&mut two, true);
        assert_eq!(two, [2, 1, 3]);
        super::swap_controllers(&mut two, true);
        assert_eq!(two, [1, 2, 3]);
        super::swap_controllers(&mut two, false);
        assert_eq!(two, [1, 2, 3]);
    }

    #[test]
    fn xbox_button_names_round_trip_within_dialog_limit() {
        for (name, key) in super::PAD_KEYS {
            let text = format!("Joystick: {name}");
            assert!(text.len() < 20, "{text}");
            assert_eq!(super::named_key_from_text(&text, "Joystick", &super::PAD_KEYS), Some(key));
        }
    }

    #[test]
    fn playstation_profile_maps_requested_buttons() {
        let keys = super::PLAYSTATION_PROFILE;
        assert_eq!(&keys[..4], &[0x14, 0x15, RIGHT_RIGHT, RIGHT_LEFT]);
        assert_eq!(&keys[4..8], &[4, 6, 2, 5]);
        assert_eq!(&keys[10..], &[1, 0, 3, 7]);
        let pad = Pad { buttons: 1 << 11 | 1 << 14, ..Pad::default() };
        assert!(super::dpad_axis_down(&pad, 0x14));
        assert!(super::dpad_axis_down(&pad, 0x13));
        assert!(!super::dpad_axis_down(&pad, 0x12));
    }

    #[test]
    fn console_profiles_are_identified_per_player() {
        let profiles = [0x1000, 0x2000];
        for player in 0..2 {
            for choice in [super::XBOX_CHOICE, super::CLASSIC_CHOICE] {
                let table = profiles[player as usize] + choice * super::PROFILE_SIZE;
                assert_eq!(super::console_choice(player, table, profiles), Some(choice));
            }
        }
        assert_eq!(super::console_choice(0, profiles[0], profiles), None);
        assert_eq!(super::console_choice(2, 0, profiles), None);
    }

    #[test]
    fn classic_profile_keeps_left_stick_turning_and_triggers_for_strafing() {
        let classic = super::CLASSIC_PROFILE;
        let modern = super::PLAYSTATION_PROFILE;
        assert_eq!(&classic[..4], &[0x14, 0x15, 0x13, 0x12]);
        assert_eq!(&classic[4..], &modern[4..]);
        assert_eq!(&classic[10..12], &[1, 0]);
    }

    #[test]
    fn gamepad_binding_keys() {
        let pad = Pad { stick: [-0.8, 0.1, 0.0, 0.9], buttons: 1 << 3 | 1 << 14, start: false };
        assert!(key_down(&pad, 3) && key_down(&pad, 14) && !key_down(&pad, 0));
        assert!(key_down(&pad, 0x12) && !key_down(&pad, 0x13) && !key_down(&pad, 0x14) && !key_down(&pad, 0x15));
        assert!(key_down(&pad, RIGHT_DOWN) && !key_down(&pad, RIGHT_UP));
    }

    #[test]
    fn dead_zone_is_removed_and_remaining_travel_is_rescaled() {
        assert_eq!(movement_axis(0.25), 0.0);
        assert!((movement_axis(0.50) - 1.0 / 3.0).abs() < 1e-6);
        assert_eq!(movement_axis(-1.0), -1.0);
    }

    #[test]
    fn movement_speed_follows_radial_stick_travel() {
        assert_eq!(scale_move_speed(12_000, 0.25), 3_000);
        assert_eq!(scale_move_speed(12_000, 1.0), 12_000);
        assert_eq!(scale_move_speed(-9_000, 0.5), -4_500);
    }

    #[test]
    fn each_active_analog_binding_is_independent() {
        let stick = [0.0, 0.7, 0.8, 0.0];
        let mut bindings = [(0, 0); 14];
        bindings[..4].copy_from_slice(&[(0, 0x26), (1, 0x15), (1, RIGHT_RIGHT), (0, 0x25)]);
        let backwards = analog_amounts(stick, bindings, 0x8000_0000);
        assert!((backwards.vector.unwrap()[0] + (0.7 - 0.25) / 0.75).abs() < 1e-6);
        assert!(backwards.turn.is_none());

        let right = analog_amounts(stick, bindings, 0x2000_0000);
        assert!(right.vector.is_none());
        assert!((right.turn.unwrap() - (0.8 - 0.25) / 0.75).abs() < 1e-6);

        let digital_forward_and_analog_right = analog_amounts(stick, bindings, 0x6000_0000);
        assert!(digital_forward_and_analog_right.vector.is_none());
        assert!(digital_forward_and_analog_right.turn.is_some());
    }

    #[test]
    fn analog_sidestep_blends_with_other_movement() {
        let stick = [0.8, -0.7, 0.0, 0.0];
        let mut bindings = [(0, 0); 14];
        bindings[0] = (0, 0x26); // digital forward
        bindings[10] = (1, 0x13); // analog sidestep
        let movement = analog_amounts(stick, bindings, 0x4010_0000).vector.unwrap();
        assert_eq!(movement[0], 1.0);
        assert!((movement[1] + (0.8 - 0.25) / 0.75).abs() < 1e-6);
        assert_ne!(MOVE_BITS & 0x0010_0000, 0);
        assert_ne!(MOVE_BITS & 0x0020_0000, 0);
    }

    #[test]
    fn analog_sidestep_from_rest_has_its_own_vector() {
        let stick = [0.8, 0.0, 0.0, 0.0];
        let mut bindings = [(0, 0); 14];
        bindings[10] = (1, 0x13);
        let movement = analog_amounts(stick, bindings, 0x0010_0000).vector.unwrap();
        assert_eq!(movement[0], 0.0);
        assert!((movement[1] + (0.8 - 0.25) / 0.75).abs() < 1e-6);
    }

    #[test]
    fn right_stick_directions_use_extended_binding_keys() {
        let pad = |x, y| Pad { stick: [0.0, 0.0, x, y], ..Pad::default() };
        assert_eq!(right_stick_key(&pad(-1.0, 0.0)), Some(RIGHT_LEFT));
        assert_eq!(right_stick_key(&pad(1.0, 0.0)), Some(RIGHT_RIGHT));
        assert_eq!(right_stick_key(&pad(0.0, -1.0)), Some(RIGHT_UP));
        assert_eq!(right_stick_key(&pad(0.0, 1.0)), Some(RIGHT_DOWN));
        assert_eq!(right_stick_key(&pad(0.2, -0.2)), None);
        assert_eq!(right_stick_key(&pad(-0.5, 0.0)), None);
        assert_eq!(right_stick_key(&pad(-0.7, 0.9)), Some(RIGHT_DOWN));
        let diagonal = pad(0.8, -0.8);
        assert!(right_stick_down(&diagonal, RIGHT_RIGHT));
        assert!(right_stick_down(&diagonal, RIGHT_UP));
        for (name, key) in RIGHT_KEYS {
            let text = format!("Joystick: {name}");
            assert!(text.len() < 20);
            assert_eq!(right_key_from_text(&text), Some(key));
        }
        for (name, key) in MOUSE_KEYS {
            let text = format!("Keyboard: {name}");
            assert!(text.len() < 20);
            assert_eq!(mouse_key_from_text(&text), Some(key));
        }
    }
}

pub const IMPORTS: &[Import] = &[("DirectInputCreateA", 4, direct_input_create)];
