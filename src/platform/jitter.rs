//! Jitter (PLAN-RTX.md step 3, for DLSS and temporal filters): each picture's 3D primitives are
//! moved by a subpixel offset, a Halton (2, 3) sequence of `PHASES` points, so pictures in a row
//! sample each pixel at other places. The HUD stays still.
//!
//! Not in the game's projections (0x457340 objects, 0x453740 terrain, 0x457140 sprites): they
//! give whole 2560x1920 units, a unit is more than a pixel above 2560 pixels wide. Instead
//! Glide's vertex shader adds the offset to the 3D picture's primitives (`glide::flush`, while
//! the walker draws the 3D picture: `gbuf`). The depth attachment is drawn jittered too; the
//! motion vectors take it out (`motion.rs`: a pixel shows the point at its centre less the
//! offset), as DLSS wants them.
//!
//! On with `FCOP_JITTER=1` (the debug menu, Renderer page): without a temporal filter the
//! picture only shakes by a pixel. DLSS will turn it on.

use std::sync::atomic::{AtomicU32, Ordering};

/// Points of the sequence before it starts again.
pub const PHASES: u32 = 8;

/// The picture's place in the sequence.
static INDEX: AtomicU32 = AtomicU32::new(0);

pub fn enabled() -> bool {
    super::debug::flag(super::debug::V::Jitter)
}

/// The radical inverse of `i` in base `b`: 0...1.
fn halton(mut i: u32, b: u32) -> f32 {
    let (mut f, mut r) = (1.0, 0.0);
    while i > 0 {
        f /= b as f32;
        r += f * (i % b) as f32;
        i /= b;
    }
    r
}

/// The offset of picture `n` in pixels (x right, y up as GL's framebuffer), -0.5...0.5.
fn at(n: u32) -> [f32; 2] {
    // from 1: Halton's point 0 is (0, 0), a corner
    let i = n % PHASES + 1;
    [halton(i, 2) - 0.5, halton(i, 3) - 0.5]
}

/// This picture's offset (0 while off).
pub fn now() -> [f32; 2] {
    if enabled() { at(INDEX.load(Ordering::Relaxed)) } else { [0.0; 2] }
}

/// The last picture's offset (0 while off).
pub fn last() -> [f32; 2] {
    if enabled() { at(INDEX.load(Ordering::Relaxed).wrapping_sub(1)) } else { [0.0; 2] }
}

/// A picture was shown: the next point.
pub fn next() {
    INDEX.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The points lie within a pixel, differ, and cover both halves of each axis.
    #[test]
    fn points() {
        let p: Vec<[f32; 2]> = (0..PHASES).map(at).collect();
        assert!(p.iter().all(|q| q.iter().all(|v| (-0.5..0.5).contains(v))));
        for i in 0..p.len() {
            for j in 0..i {
                assert!(p[i] != p[j]);
            }
        }
        for a in 0..2 {
            assert!(p.iter().any(|q| q[a] < 0.0) && p.iter().any(|q| q[a] > 0.0));
        }
        assert_eq!(at(0), [0.0, 1.0 / 3.0 - 0.5]);
    }
}
