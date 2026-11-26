//! Optional controller vibration for actual damage to a local player.
//!
//! Hooks can run on guest threads; gilrs is owned by the main thread. Requests are bounded and
//! coalesced per player, then consumed by `dinput::update`. Effects must stay alive while playing.

use super::{dinput, log, mem};
use crate::rt::{call_original, Cpu, ESP};
use crate::ver::Us;
use gilrs::ff::{BaseEffect, BaseEffectType, Effect, EffectBuilder, Repeat, Replay, Ticks};
use gilrs::Gilrs;
use std::cell::RefCell;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const ROBOT_DAMAGE: Us = Us(0x446720);
const PLAYERS: Us = Us(0x511fd0);
const NET: Us = Us(0x511fa0);

#[derive(Clone, Copy)]
struct Pulse {
    strength: u16,
    when: Instant,
    device: usize,
}

static PENDING: Mutex<[Option<Pulse>; 2]> = Mutex::new([None, None]);

thread_local! {
    static ACTIVE: RefCell<Vec<(usize, Instant, Effect)>> = const { RefCell::new(Vec::new()) };
}

static ENABLED: super::light::Setting = super::light::Setting::with_default("ControllerVibration", "FCOP_RUMBLE", true);

pub fn enabled() -> bool {
    ENABLED.get()
}

pub fn set_enabled(on: bool) {
    ENABLED.set(on);
    if !on {
        *PENDING.lock().unwrap() = [None, None];
    }
}

fn strength(lost: u16, maximum: u16) -> u16 {
    let fraction = f32::from(lost) / f32::from(maximum.max(1));
    (12000.0 + fraction.min(1.0) * 40000.0) as u16
}

/// Queue one pulse per local pad. Multiple hits between pumps become the strongest hit.
fn request(player: usize, magnitude: u16) {
    if !enabled() {
        return;
    }
    let Some(device) = dinput::player_device(player) else { return };
    if std::env::var_os("FCOP_PAD_LOG").is_some_and(|v| v != "0") {
        log!("rumble: player {} device {} strength {}", player + 1, device, magnitude);
    }
    let mut pending = PENDING.lock().unwrap();
    let slot = &mut pending[player];
    let value = slot.filter(|p| p.device == device).map_or(magnitude, |p| p.strength.max(magnitude));
    *slot = Some(Pulse { strength: value, when: Instant::now(), device });
}

/// Find the local pad slot for a player actor.
fn local_player(actor: u32) -> Option<usize> {
    let m = mem();
    let net = m.rd32(NET.at()) as i32;
    if actor == 0 {
        None
    } else if net >= 0 {
        let local = (net as u32 & 0xf) as usize;
        (local < 2 && m.rd32(PLAYERS.at() + 4 * local as u32) == actor).then_some(0)
    } else {
        (0..2).find(|&p| m.rd32(PLAYERS.at() + 4 * p as u32) == actor)
    }
}

/// A player actually changed between walker and hover mode.
pub fn transformed(actor: u32) {
    if let Some(player) = local_player(actor) {
        request(player, 24000);
    }
}

/// The player actor's health before and after the original function determines actual damage.
pub fn robot_damage(c: &mut Cpu) {
    let m = mem();
    let actor = m.rd32(c.r[ESP] + 4);
    let player = local_player(actor);
    let before = player.map(|_| (m.rd16(actor + 0x1c), m.rd16(actor + 0x1e)));
    call_original(c, ROBOT_DAMAGE.at());
    if let (Some(player), Some((health, maximum))) = (player, before) {
        let lost = health.saturating_sub(m.rd16(actor + 0x1c));
        if lost > 0 {
            request(player, strength(lost, maximum));
        }
    }
}

fn trace(args: std::fmt::Arguments) {
    if std::env::var_os("FCOP_PAD_LOG").is_some_and(|v| v != "0") {
        log!("rumble: {}", args);
    }
}

/// Service queued pulses on gilrs' owning thread. `ids` follows the gamepad binding order.
pub fn service(g: &mut Gilrs, ids: [Option<usize>; 2]) {
    let now = Instant::now();
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        if !enabled() {
            *PENDING.lock().unwrap() = [None, None];
            for (_, _, effect) in active.drain(..) {
                let _ = effect.stop();
            }
            return;
        }
        active.retain(|(_, until, _)| now < *until);
        let pending = std::mem::take(&mut *PENDING.lock().unwrap());
        for (player, pulse) in pending.into_iter().enumerate() {
            let Some(pulse) = pulse else { continue };
            // Do not play old events after a stall, disconnect, or a long pause.
            if now.duration_since(pulse.when) > Duration::from_millis(250) {
                continue;
            }
            let Some(wanted) = ids[player].filter(|&id| id == pulse.device) else {
                trace(format_args!(
                    "dropped: player {} pulse for device {} but the assigned pad is {:?}",
                    player + 1, pulse.device, ids[player]
                ));
                continue;
            };
            // Do not stack a stream of hits into an excessively strong vibration.
            if active.iter().any(|(id, _, _)| *id == wanted) {
                continue;
            }
            let Some(id) = g.gamepads().find_map(|(id, pad)| {
                (usize::from(id) == wanted && pad.is_ff_supported()).then_some(id)
            }) else {
                let state = g.gamepads().find(|(id, _)| usize::from(*id) == wanted).map(|(_, pad)| pad.is_ff_supported());
                trace(format_args!("dropped: device {wanted} cannot rumble (connected pad found, ff supported: {state:?})"));
                continue;
            };
            let duration = Ticks::from_ms(150);
            let effect = EffectBuilder::new()
                .add_effect(BaseEffect {
                    kind: BaseEffectType::Strong { magnitude: pulse.strength },
                    scheduling: Replay { play_for: duration, ..Default::default() },
                    ..Default::default()
                })
                .repeat(Repeat::For(duration))
                .gamepads(&[id])
                .finish(g);
            match effect {
                Ok(effect) => match effect.play() {
                    Ok(()) => {
                        trace(format_args!("playing: device {wanted} strength {}", pulse.strength));
                        // Allow for the gilrs worker's 50 ms scheduling interval.
                        active.push((wanted, now + Duration::from_millis(300), effect));
                    }
                    Err(e) => trace(format_args!("play failed on device {wanted}: {e:?}")),
                },
                Err(e) => trace(format_args!("effect creation failed on device {wanted}: {e:?}")),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::strength;

    #[test]
    fn strength_is_bounded_and_increases_with_damage() {
        assert!(strength(1, 100) < strength(50, 100));
        assert_eq!(strength(200, 100), strength(100, 100));
        assert!(strength(1, 0) > 0);
    }
}
