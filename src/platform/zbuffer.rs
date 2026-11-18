//! A depth buffer for the 3D picture. The game draws without one, back to front by its depth
//! buckets (as on the PlayStation, 0x40ca10): a ground tile whose middle was nearer than a
//! decal lying on it painted part of the decal over, models' faces came out in the wrong
//! order. Each corner's depth is known from the hooked projections (`ao::depths`, the depth
//! attachment's), so `glide` tests the 3D primitives whose corners all have one:
//!
//! - solid ones (opaque, or alpha-blended as the game draws its solid geometry) test and write,
//!   first the depth alone where their alpha is at least 0.75 (keyed holes, a lattice's gaps, translucent liquids do
//!   not hide what is behind), then the colour where they are not behind it. The depth they
//!   write lies `SLACK` farther than they are: what is drawn later within that of a surface
//!   (decals, overlapping tiles, faces on faces) is drawn in the game's order, on top; only what
//!   is farther by more is hidden (a strict test made coplanar surfaces fight, their corners'
//!   depths are rounded apart);
//! - blended and darkening ones (smoke, glows, the game's shadows) only test, nearer by more
//!   (`TEST_BIAS`), so sprites standing in the ground are not cut by it;
//! - corners whose depth was borrowed from the others (clipped at the near plane, sprites) only
//!   test, never write.
//!
//! The rest (the HUD, primitives without depth) is drawn in the game's order as before; a
//! batch holds one kind only (`glide`'s `batch_z`). The buffer (32-bit float, 1 - NEAR / z) is
//! a renderbuffer beside the depth attachment, with its samples, cleared with it.
//!
//! REG_DWORD `DepthBuffer` (default on), `FCOP_ZBUFFER=0/1`; in the Advanced Graphics dialog.

use super::light::Setting;

static SETTING: Setting = Setting::with_default("DepthBuffer", "FCOP_ZBUFFER", true);

/// Solid primitives write their depth this share of it farther (`SLACK` of the module; decals
/// lie 0.25...0.4% of their depth above the ground: 0.2% left notches in them).
/// `FCOP_ZSLACK` for tests.
pub fn slack() -> f32 {
    static S: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *S.get_or_init(|| std::env::var("FCOP_ZSLACK").ok().and_then(|v| v.parse().ok()).unwrap_or(0.001))
}
/// Blended primitives' depth (test only) is pulled nearer by this share.
pub const TEST_BIAS: f32 = 0.004;

pub fn enabled() -> bool {
    SETTING.get()
}

/// The Advanced Graphics dialog's OK: saved, and used from the next picture on.
pub fn set_enabled(on: bool) {
    SETTING.set(on);
}
