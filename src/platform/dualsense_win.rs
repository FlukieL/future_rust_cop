//! Windows multimedia joystick fallback for Bluetooth DualSense controllers.
//! Only provides input; native DualSense HID vibration is not implemented.
use super::dinput::Pad;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[repr(C)]
struct JoyCaps {
    mid: u16,
    pid: u16,
    name: [u16; 32],
    xmin: u32, xmax: u32, ymin: u32, ymax: u32, zmin: u32, zmax: u32,
    buttons: u32, period_min: u32, period_max: u32,
    rmin: u32, rmax: u32, umin: u32, umax: u32, vmin: u32, vmax: u32,
    caps: u32, max_axes: u32, axes: u32, max_buttons: u32,
    reg: [u16; 32], oem: [u16; 260],
}

#[repr(C)]
#[derive(Default)]
struct JoyInfo {
    size: u32, flags: u32, x: u32, y: u32, z: u32, r: u32, u: u32, v: u32,
    buttons: u32, button_number: u32, pov: u32, reserved1: u32, reserved2: u32,
}

#[link(name = "winmm")]
unsafe extern "system" {
    fn joyGetNumDevs() -> u32;
    fn joyGetDevCapsW(id: usize, caps: *mut JoyCaps, size: u32) -> u32;
    fn joyGetPosEx(id: u32, info: *mut JoyInfo) -> u32;
}

fn axis(value: u32, lo: u32, hi: u32) -> f32 {
    if hi <= lo { return 0.0 }
    (2.0 * (value.saturating_sub(lo) as f32) / (hi - lo) as f32 - 1.0).clamp(-1.0, 1.0)
}

/// The D-pad as our buttons 11...14 (up, down, left, right). Windows reports "centred" as
/// 0xFFFF or 0xFFFFFFFF depending on the driver; only 0...35999 are real angles.
fn pov_buttons(pov: u32) -> u32 {
    if pov >= 36000 {
        return 0;
    }
    let mut buttons = 0;
    if pov >= 31500 || pov <= 4500 { buttons |= 1 << 11; }
    if (13500..=22500).contains(&pov) { buttons |= 1 << 12; }
    if (22500..=31500).contains(&pov) { buttons |= 1 << 13; }
    if (4500..=13500).contains(&pov) { buttons |= 1 << 14; }
    buttons
}

fn trigger(value: u32, rest: u32, hi: u32) -> bool {
    hi > rest && value.saturating_sub(rest) as f32 / (hi - rest) as f32 > 0.30
}

/// U is R2 and V is L2 on the Windows DualSense fallback.
fn trigger_buttons(u: u32, v: u32, urest: u32, vrest: u32, umax: u32, vmax: u32) -> u32 {
    (trigger(u, urest, umax) as u32) | (trigger(v, vrest, vmax) as u32) << 1
}

static SNAPSHOT: Mutex<Vec<(usize, Pad)>> = Mutex::new(Vec::new());

/// Read the raw winmm state for an identified fallback device, for diagnostics only.
pub fn diagnostic_raw(id: usize) -> Option<(u32, [u32; 6], u32, u32)> {
    let slot = id.checked_sub(0x4000_0000)?;
    let slot = u32::try_from(slot).ok()?;
    let mut state = JoyInfo { size: std::mem::size_of::<JoyInfo>() as u32, flags: 0xff, ..JoyInfo::default() };
    let result = unsafe { joyGetPosEx(slot, &mut state) };
    Some((result, [state.x, state.y, state.z, state.r, state.u, state.v], state.buttons, state.pov))
}

/// Read the latest snapshot without blocking on Windows joystick calls.
pub fn poll() -> Vec<(usize, Pad)> {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        std::thread::spawn(|| {
            let mut rests = Vec::new();
            let mut devices = Vec::new();
            let mut discovery = None;
            loop {
                if discovery.is_none_or(|last: Instant| last.elapsed() >= Duration::from_secs(1)) {
                    devices = discover();
                    discovery = Some(Instant::now());
                }
                let snapshot = sample(&devices, &mut rests);
                *SNAPSHOT.lock().unwrap() = snapshot;
                std::thread::sleep(Duration::from_millis(10));
            }
        });
    });
    SNAPSHOT.lock().unwrap().clone()
}

fn discover() -> Vec<(u32, JoyCaps)> {
    let mut devices = Vec::new();
    for id in 0..unsafe { joyGetNumDevs() } {
        // JOYCAPSW is a plain C data structure and all-zero is a valid initial state.
        let mut caps: JoyCaps = unsafe { std::mem::zeroed() };
        if unsafe { joyGetDevCapsW(id as usize, &mut caps, std::mem::size_of::<JoyCaps>() as u32) } != 0
            || caps.mid != 0x054c || ![0x0ce6, 0x0df2].contains(&caps.pid) {
            continue;
        }
        devices.push((id, caps));
    }
    devices
}

/// Slot IDs have a separate namespace from gilrs. Called only by the worker.
fn sample(devices: &[(u32, JoyCaps)], rests: &mut Vec<(u32, u32, u32)>) -> Vec<(usize, Pad)> {
    let mut result = Vec::new();
    for (id, caps) in devices {
        let id = *id;
        let mut state = JoyInfo { size: std::mem::size_of::<JoyInfo>() as u32, flags: 0xff, ..JoyInfo::default() };
        if unsafe { joyGetPosEx(id, &mut state) } != 0 {
            continue;
        }
        // SDL's Windows mapping for Sony 054c:0ce6: X=b0, A=b1, B=b2, Y=b3.
        let mapping = [(0, 4), (1, 3), (2, 2), (3, 5), (4, 6), (5, 7), (8, 8), (10, 10), (11, 9)];
        let mut buttons = 0;
        for (source, target) in mapping {
            if state.buttons & (1 << source) != 0 { buttons |= 1 << target; }
        }
        let index = match rests.iter().position(|r| r.0 == id) {
            Some(index) => index,
            None => {
                rests.push((id, state.u, state.v));
                rests.len() - 1
            }
        };
        let rest = &mut rests[index];
        // winmm axes of the DualSense: X, Y left stick; Z, R right stick; U R2; V L2
        // (our button 0 is RT, 1 is LT)
        rest.1 = rest.1.min(state.u);
        rest.2 = rest.2.min(state.v);
        buttons |= trigger_buttons(state.u, state.v, rest.1, rest.2, caps.umax, caps.vmax);
        buttons |= pov_buttons(state.pov);
        let pad = Pad {
            stick: [axis(state.x, caps.xmin, caps.xmax), axis(state.y, caps.ymin, caps.ymax),
                    axis(state.z, caps.zmin, caps.zmax), axis(state.r, caps.rmin, caps.rmax)],
            buttons,
            start: state.buttons & (1 << 9) != 0,
        };
        result.push((0x4000_0000usize + id as usize, pad));
    }
    rests.retain(|r| result.iter().any(|(id, _)| *id == 0x4000_0000usize + r.0 as usize));
    result
}

#[cfg(test)]
mod tests {
    use super::{axis, pov_buttons, trigger, trigger_buttons};

    #[test]
    fn dualsense_triggers_map_to_correct_buttons() {
        assert_eq!(trigger_buttons(0, 0, 0, 0, 1000, 1000), 0);
        assert_eq!(trigger_buttons(1000, 0, 0, 0, 1000, 1000), 1);
        assert_eq!(trigger_buttons(0, 1000, 0, 0, 1000, 1000), 2);
        assert_eq!(trigger_buttons(1000, 1000, 0, 0, 1000, 1000), 3);
    }

    #[test]
    fn centred_pov_presses_nothing() {
        assert_eq!(pov_buttons(0xffff), 0);
        assert_eq!(pov_buttons(0xffff_ffff), 0);
        assert_eq!(pov_buttons(27000), 1 << 13);
        assert_eq!(pov_buttons(0), 1 << 11);
    }

    #[test]
    fn joystick_ranges_and_triggers() {
        assert_eq!(axis(0, 0, 65535), -1.0);
        assert_eq!(axis(65535, 0, 65535), 1.0);
        assert!(axis(32768, 0, 65535).abs() < 0.001);
        assert_eq!(axis(42, 1, 1), 0.0);
        assert!(!trigger(100, 0, 1000));
        assert!(!trigger(300, 0, 1000));
        assert!(trigger(301, 0, 1000));
    }
}
