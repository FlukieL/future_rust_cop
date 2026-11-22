//! Motion vectors (PLAN-RTX.md step 3, for DLSS and temporal filters): per pixel of a view, where
//! the point it shows was on the screen in the last picture shown, as pixels (last minus now).
//!
//! First part: the camera's motion. Each view's camera (`shadows::camera`: its matrix C at
//! +0x28 and place c at +0x7c, camera space = (world - c / 16) * C as `scene.rs` has it; the
//! matrix at +0x58 with the translation at +0x4c did not fit together while turning) is kept from picture to picture; the frames
//! drawn in between (`fps.rs`) set the interpolated camera before they draw, so the vectors are
//! between the pictures shown, not between the game's frames. A pixel's place in this view's
//! space comes from the depth (`ao::COMMON`'s P), back to the world and into the last
//! picture's view: right for everything that stands still (terrain, buildings). Moving things
//! (robots, vehicles) are the second part (not yet). Pixels without a depth (nothing drawn
//! there) count as infinitely far: only the camera's turn moves them.
//!
//! Made when wanted (`wanted`: `FCOP_GBUFFER=motion` shows them, coloured by direction,
//! brighter with length; DLSS will take them).

use super::gl::compile;
use glow::HasContext;

/// Whether the vectors are made (the depth then too: `ao::depth_wanted`).
pub fn wanted() -> bool {
    showing() || checking()
}

fn showing() -> bool {
    super::debug::choice(super::debug::V::GBuffer) == "motion"
}

/// `FCOP_GBUFFER=reproject`: the last picture moved by the vectors, less this one (4 times,
/// as brightness): black where they are right (moving things are not yet).
fn checking() -> bool {
    super::debug::choice(super::debug::V::GBuffer) == "reproject"
}

/// A view's camera: rows of its matrix (right, up, forward in the world), its translation
/// (object units: camera space = rows * world + t), widescreen's k.
#[derive(Clone, Copy, PartialEq)]
pub struct Camera {
    pub rot: [[f32; 3]; 3],
    pub t: [f32; 3],
    pub k: f32,
}

/// From this picture's camera to the last's: c0 = m * c1 + off (columns of m), in f64.
/// m = R0 * R1^-1 with the true inverse: the game's matrices are not quite orthonormal (16.16,
/// and the pictures between frames interpolate them linearly, which shrinks them while
/// turning): R1^T as the inverse moved still pictures by 0.4 pixels, turning ones by up to 14.
fn between(now: &Camera, last: &Camera) -> ([f32; 9], [f32; 3]) {
    let r = |c: &Camera| c.rot.map(|row| row.map(|x| x as f64));
    let (r0, r1) = (r(last), r(now));
    // the inverse of r1 by cofactors (rows of the adjugate's transpose)
    let a = r1;
    let cof = |i: usize, j: usize| {
        let (i1, i2, j1, j2) = ((i + 1) % 3, (i + 2) % 3, (j + 1) % 3, (j + 2) % 3);
        a[i1][j1] * a[i2][j2] - a[i1][j2] * a[i2][j1]
    };
    let det = (0..3).map(|j| a[0][j] * cof(0, j)).sum::<f64>();
    let inv: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| cof(j, i) / det));
    let m: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| r0[i][k] * inv[k][j]).sum()));
    let off = [0, 1, 2].map(|i| last.t[i] as f64 - (0..3).map(|j| m[i][j] * now.t[j] as f64).sum::<f64>());
    // column-major for GLSL
    (std::array::from_fn(|k| m[k % 3][k / 3] as f32), off.map(|x| x as f32))
}

const MOTION_FS: &str = r#"
uniform mat3 m;
uniform vec3 off;
uniform vec2 ks;      // widescreen's k now, in the last picture
out vec4 o;
void main() {
    ivec2 q = ivec2(gl_FragCoord.xy);
    float z = G(q).r;
    vec2 s = vec2(q) + 0.5;
    // camera space (unscaled x); no depth: infinitely far
    vec3 c = z > 0.0 ? P(s, z) : P(s, 1.0) * 1.0e6;
    c.x /= ks.x;
    vec3 c0 = m * c + (z > 0.0 ? off : vec3(0.0));
    vec2 s0 = c0.z > 1.0 ? vec2(c0.x * ks.y, c0.y) * focal / c0.z + vec2(size) * 0.5 : s;
    o = vec4(s0 - s, 0.0, 1.0);
}
"#;

const SHOW_FS: &str = r#"
uniform sampler2D mv;
out vec4 o;
vec3 hue(float h) {
    return clamp(abs(fract(h + vec3(0.0, 2.0, 1.0) / 3.0) * 6.0 - 3.0) - 1.0, 0.0, 1.0);
}
void main() {
    vec2 v = texelFetch(mv, ivec2(gl_FragCoord.xy), 0).rg;
    float len = length(v);
    // the direction as a hue, the length (up to 16 pixels) as brightness; still: grey
    vec3 c = len < 0.05 ? vec3(0.2) : hue(atan(v.y, v.x) / 6.2831853 + 0.5) * clamp(len / 16.0, 0.15, 1.0);
    o = vec4(c, 1.0);
}
"#;

const CHECK_FS: &str = r#"
uniform sampler2D mv;
uniform sampler2D now;
uniform sampler2D last;
out vec4 o;
void main() {
    ivec2 q = ivec2(gl_FragCoord.xy);
    vec2 s = (vec2(q) + 0.5 + texelFetch(mv, q, 0).rg) / vec2(size);
    vec3 c = texelFetch(now, q, 0).rgb;
    vec3 l = texture(last, s).rgb;
    o = vec4(any(lessThan(s, vec2(0.0))) || any(greaterThan(s, vec2(1.0))) ? vec3(0.0, 0.0, 0.3) : abs(c - l) * 4.0, 1.0);
}
"#;

pub struct Motion {
    /// making them, showing them; each single-sampled and multisampled depth
    progs: [[glow::Program; 2]; 3],
    vao: glow::VertexArray,
    /// the vectors (RG16F) and their framebuffer, their size
    target: Option<((u32, u32), glow::Texture, glow::Framebuffer)>,
    /// each view's camera in the last picture
    last: [Option<Camera>; 4],
    /// `checking`: this picture and the last (RGBA8 copies, each with a framebuffer)
    pictures: Option<((u32, u32), [(glow::Texture, glow::Framebuffer); 2])>,
}

impl Motion {
    pub fn new(gl: &glow::Context) -> Motion {
        let prog = |fs: &str, ms: bool| compile(gl, super::ao::VS, &format!("{}{}{fs}", if ms { "#define MS\n" } else { "" }, super::ao::COMMON));
        let progs = [MOTION_FS, SHOW_FS, CHECK_FS].map(|fs| [prog(fs, false), prog(fs, true)]);
        let vao = unsafe { gl.create_vertex_array().expect("GL vertex array") };
        Motion { progs, vao, target: None, last: [None; 4], pictures: None }
    }

    pub fn delete(&mut self, gl: &glow::Context) {
        unsafe {
            for p in self.progs.iter().flatten() {
                super::gl::delete_program(gl, *p);
            }
            gl.delete_vertex_array(self.vao);
            if let Some((_, t, f)) = self.target.take() {
                gl.delete_framebuffer(f);
                gl.delete_texture(t);
            }
            for (t, f) in self.pictures.take().into_iter().flat_map(|p| p.1) {
                gl.delete_framebuffer(f);
                gl.delete_texture(t);
            }
        }
    }

    /// The vectors of view `view` (its camera `cam`) into the texture (returned), from the depth
    /// attachment `depth` (multisampled or not) of a target of `size`; shown in `out` (the
    /// view's picture, within `clip`) with `FCOP_GBUFFER=motion`.
    #[allow(clippy::too_many_arguments)]
    pub fn run(&mut self, gl: &glow::Context, depth: glow::Texture, ms: bool, size: (u32, u32), out: glow::Framebuffer, clip: [i32; 4], view: usize, cam: Camera) -> glow::Texture {
        let last = self.last.get(view).copied().flatten().unwrap_or(cam);
        if let Some(l) = self.last.get_mut(view) {
            *l = Some(cam);
        }
        let (m, off) = between(&cam, &last);
        if self.target.as_ref().is_none_or(|t| t.0 != size) {
            if let Some((_, t, f)) = self.target.take() {
                unsafe {
                    gl.delete_framebuffer(f);
                    gl.delete_texture(t);
                }
            }
            unsafe {
                let t = super::gl::texture(gl, false);
                gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RG16F as i32, size.0 as i32, size.1 as i32, 0, glow::RG, glow::FLOAT, glow::PixelUnpackData::Slice(None));
                let f = gl.create_framebuffer().expect("GL framebuffer");
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
                gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(t), 0);
                self.target = Some((size, t, f));
            }
        }
        let (_, tex, fbo) = *self.target.as_ref().unwrap();
        let (w, h) = (size.0 as i32, size.1 as i32);
        let v = ms as usize;
        unsafe {
            gl.disable(glow::BLEND);
            gl.bind_vertex_array(Some(self.vao));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(if ms { glow::TEXTURE_2D_MULTISAMPLE } else { glow::TEXTURE_2D }, Some(depth));
            let common = |p: glow::Program| {
                gl.use_program(Some(p));
                let u = move |n: &'static str| super::gl::uniform(gl, p, n);
                gl.uniform_1_i32(u("gdepth").as_ref(), 0);
                gl.uniform_2_i32(u("size").as_ref(), w, h);
                gl.uniform_1_f32(u("focal").as_ref(), 4096.0 * h as f32 / 1920.0);
                u
            };
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.viewport(0, 0, w, h);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(clip[0], clip[1], clip[2], clip[3]);
            let u = common(self.progs[0][v]);
            gl.uniform_matrix_3_f32_slice(u("m").as_ref(), false, &m);
            gl.uniform_3_f32(u("off").as_ref(), off[0], off[1], off[2]);
            gl.uniform_2_f32(u("ks").as_ref(), cam.k, last.k);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            if showing() {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(out));
                gl.active_texture(glow::TEXTURE1);
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                let u = common(self.progs[1][v]);
                gl.uniform_1_i32(u("mv").as_ref(), 1);
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
                gl.bind_texture(glow::TEXTURE_2D, None);
                gl.active_texture(glow::TEXTURE0);
            }
            if checking() {
                self.check(gl, out, size, clip, tex, v);
            }
            gl.disable(glow::SCISSOR_TEST);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        tex
    }
}

impl Motion {
    /// `checking`: this picture (copied from `out`) against the last moved by the vectors
    /// `mv`, shown in `out`; then this picture is the last.
    unsafe fn check(&mut self, gl: &glow::Context, out: glow::Framebuffer, size: (u32, u32), clip: [i32; 4], mv: glow::Texture, v: usize) {
        unsafe {
            if self.pictures.as_ref().is_none_or(|p| p.0 != size) {
                for (t, f) in self.pictures.take().into_iter().flat_map(|p| p.1) {
                    gl.delete_framebuffer(f);
                    gl.delete_texture(t);
                }
                let make = || {
                    let t = super::gl::texture(gl, true);
                    gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, size.0 as i32, size.1 as i32, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
                    let f = gl.create_framebuffer().expect("GL framebuffer");
                    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
                    gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(t), 0);
                    (t, f)
                };
                self.pictures = Some((size, [make(), make()]));
            }
            let [now, last] = self.pictures.as_ref().unwrap().1;
            let [x, y, w, h] = clip;
            gl.disable(glow::SCISSOR_TEST);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(out));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(now.1));
            gl.blit_framebuffer(x, y, x + w, y + h, x, y, x + w, y + h, glow::COLOR_BUFFER_BIT, glow::NEAREST);
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(out));
            gl.enable(glow::SCISSOR_TEST);
            for (unit, t) in [(1, mv), (2, now.0), (3, last.0)] {
                gl.active_texture(glow::TEXTURE0 + unit);
                gl.bind_texture(glow::TEXTURE_2D, Some(t));
            }
            let p = self.progs[2][v];
            gl.use_program(Some(p));
            let u = |n: &'static str| super::gl::uniform(gl, p, n);
            gl.uniform_1_i32(u("mv").as_ref(), 1);
            gl.uniform_2_i32(u("size").as_ref(), size.0 as i32, size.1 as i32);
            gl.uniform_1_i32(u("now").as_ref(), 2);
            gl.uniform_1_i32(u("last").as_ref(), 3);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            for unit in 1..4 {
                gl.active_texture(glow::TEXTURE0 + unit);
                gl.bind_texture(glow::TEXTURE_2D, None);
            }
            gl.active_texture(glow::TEXTURE0);
            // this picture is the last now
            let p = self.pictures.as_mut().unwrap();
            p.1.swap(0, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A point the cameras both see maps from the one's space into the other's.
    #[test]
    fn between_cameras() {
        let (s, c) = (0.3f32.sin(), 0.3f32.cos());
        // turned about y and moved
        let a = Camera { rot: [[c, 0.0, -s], [0.0, 1.0, 0.0], [s, 0.0, c]], t: [10.0, -5.0, 300.0], k: 1.0 };
        let b = Camera { rot: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], t: [-20.0, 0.0, 100.0], k: 1.0 };
        let world = [123.0f32, 45.0, 678.0];
        let view = |cam: &Camera| [0, 1, 2].map(|i| (0..3).map(|j| cam.rot[i][j] * world[j]).sum::<f32>() + cam.t[i]);
        let (m, off) = between(&a, &b);
        let ca = view(&a);
        let got = [0, 1, 2].map(|i| (0..3).map(|j| m[3 * j + i] * ca[j]).sum::<f32>() + off[i]);
        let want = view(&b);
        for i in 0..3 {
            assert!((got[i] - want[i]).abs() < 1e-3, "{got:?} {want:?}");
        }
    }
}
