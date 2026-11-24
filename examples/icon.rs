//! The macOS app's icon (`tools/macos-app.sh`): a star on a shield, in the start window's
//! colours. `cargo run --release --example icon -- icon.png` writes it, 1024x1024.
type Rgb = [f32; 3];

const S: usize = 1024;
/// samples a pixel per axis (edges smoothed)
const N: usize = 4;

fn rgb(c: u32) -> Rgb {
    [(c >> 16 & 255) as f32, (c >> 8 & 255) as f32, (c & 255) as f32]
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t.clamp(0.0, 1.0))
}

/// Inside the rounded square of macOS icons (824 of 1024, corners 185).
fn in_square(x: f32, y: f32) -> bool {
    let (h, r) = (412.0, 185.0);
    let dx = ((x - 512.0).abs() - (h - r)).max(0.0);
    let dy = ((y - 512.0).abs() - (h - r)).max(0.0);
    dx * dx + dy * dy <= r * r
}

/// Inside the shield, made `k` times as large about its middle: straight sides, a top curved
/// down at the corners, a pointed bottom.
fn in_shield(x: f32, y: f32, k: f32) -> bool {
    let (w, top, mid, bottom) = (240.0, 240.0, 520.0, 820.0);
    let centre = (top + bottom) / 2.0;
    let (dx, y) = ((x - 512.0).abs() / k, (y - centre) / k + centre);
    if dx > w || y > bottom || y < top + 30.0 * (dx / w).powi(2) {
        return false;
    }
    let t = ((y - mid) / (bottom - mid)).max(0.0);
    dx <= w * (1.0 - t * t)
}

/// Inside a polygon (even-odd rule).
fn in_polygon(p: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut inside = false;
    for i in 0..p.len() {
        let (a, b) = (p[i], p[(i + p.len() - 1) % p.len()]);
        if (a.1 > y) != (b.1 > y) && x < a.0 + (y - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            inside = !inside;
        }
    }
    inside
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: icon <out.png>");
    // five points, the first up; inner radius of a regular star
    let star: Vec<(f32, f32)> = (0..10)
        .map(|i| {
            let a = std::f32::consts::PI * (i as f32 / 5.0 - 0.5);
            let r = if i % 2 == 0 { 150.0 } else { 150.0 * 0.382 };
            (512.0 + r * a.cos(), 515.0 + r * a.sin())
        })
        .collect();
    let mut rgba = vec![0u8; S * S * 4];
    for py in 0..S {
        for px in 0..S {
            let (mut sum, mut alpha) = ([0.0; 3], 0.0);
            for sy in 0..N {
                for sx in 0..N {
                    let x = px as f32 + (sx as f32 + 0.5) / N as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / N as f32;
                    if !in_square(x, y) {
                        continue;
                    }
                    let c = if in_polygon(&star, x, y) {
                        rgb(0xe8_f0ff)
                    } else if in_shield(x, y, 0.9) {
                        mix(rgb(0x3c_64d2), rgb(0x1e_3c96), (y - 260.0) / 540.0)
                    } else if in_shield(x, y, 1.0) {
                        rgb(0x7f_d0ff)
                    } else {
                        let back = mix(rgb(0x1e_3c96), rgb(0x0a_1440), (y - 100.0) / 824.0);
                        // the shield's shadow
                        if in_shield(x, y - 16.0, 1.0) { back.map(|v| v * 0.55) } else { back }
                    };
                    for i in 0..3 {
                        sum[i] += c[i];
                    }
                    alpha += 1.0;
                }
            }
            let o = (py * S + px) * 4;
            if alpha > 0.0 {
                for i in 0..3 {
                    rgba[o + i] = (sum[i] / alpha).round() as u8;
                }
                rgba[o + 3] = (255.0 * alpha / (N * N) as f32).round() as u8;
            }
        }
    }
    let f = std::io::BufWriter::new(std::fs::File::create(&path).expect("cannot create the file"));
    let mut e = png::Encoder::new(f, S as u32, S as u32);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().and_then(|mut w| w.write_image_data(&rgba)).expect("cannot write the icon");
}
