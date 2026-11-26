# Local gilrs-core patch

This directory is based on the published `gilrs-core` 0.6.8 crate from
[the gilrs project](https://gitlab.com/gilrs-project/gilrs). Its source and
package metadata retain their upstream copyright and Apache-2.0/MIT license
notices. The published package did not include the license texts.

`Cargo.toml` uses this copy through `[patch.crates-io]`. Only the Windows Gaming
Input backend (`src/platform/windows_wgi/`) is changed; other backends are
unchanged.

## Changes

- **Hotplug ordering:** a device first seen through an input event is announced
  with `Connected` before that event is delivered. Late `Connected` events for
  known devices and events for disconnected devices are dropped. This fixed a
  crash when a controller was disconnected.
- **Disconnect:** every button is released and every axis centred before
  `Disconnected` is forwarded, so nothing stays held in gilrs.
- **Reconnect:** the device is rebuilt from the new WGI handle, and stale
  readings are discarded once the device list is read successfully.
- **Missed releases:** a reading is no longer skipped when its timestamp equals
  the previous one. Steam Input's virtual controller can change state without
  advancing it, which left buttons and the D-pad stuck.
- **Initial state:** when a device is first tracked, all button states are sent,
  including releases.
- **Force feedback:** `is_ff_supported` depends only on the mapped `Gamepad`.
  The `ForceFeedbackMotors()` query it also required fails for Steam's virtual
  controller, which disabled vibration.
- **Fallibility:** failed reads and failed capability queries no longer panic
  or produce false button transitions.
- **Diagnostics:** when `FCOP_PAD_LOG` is set (the launcher's "Controller
  diagnostics" checkbox), raw button events, event routing and vibration calls
  are logged to stderr.

## Tests

The `device_event_tests` unit tests cover event ordering, the neutral state on
disconnect and the initial-state button transitions. Vendored tests need a
temporary manifest, because the crate's optional dependencies are not in the
offline cache. Disconnect, stuck buttons and vibration were verified on real
hardware through Steam Input.
