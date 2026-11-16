//! `FCOP_STATS=1` (on with `FCOP_SAMPLE=1` too): frames a second and frame times, drawn by us
//! in the window's top left corner, so no overlay tool (RivaTuner) has to hook into our GL.
//! Counted are the new pictures shown (`picture`: Glide's and 2D frames), not the window's
//! repaints. The text is the last half second's: frames a second and the mean frame time,
//! then the longest and the 99th percentile frame time of the last second. The graph goes by
//! time, as RivaTuner's: one column per 1/60 s, 4 s across, a range of 0 to 20 ms, or 50, 100,
//! 200, 500, 1000 ms when a column shown is longer (labelled in its corner). A line as
//! RivaTuner's, every frame on its own: each column spans the frames ending in it and joins
//! the previous column's last; a frame longer than a column is a plateau over the columns it
//! lies within. Yellow above 8.3 ms (under 120 fps), red above 16.7 ms (under 60 fps).
//! Redrawn 60 times a second; not in FCOP_SHOT's pictures.
use super::dialog::draw::glyph;
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// "Show Frame Rate" in Advanced Graphics (REG_DWORD `ShowFps`, default off; `FCOP_STATS=0/1`).
static SHOW: super::light::Setting = super::light::Setting::with_default("ShowFps", "FCOP_STATS", false);

pub fn on() -> bool {
    static SAMPLE: OnceLock<bool> = OnceLock::new();
    SHOW.get() || *SAMPLE.get_or_init(|| std::env::var_os("FCOP_SAMPLE").is_some_and(|v| v != "0"))
}

/// The box: saved, and shown (or not) from the next picture on, counting afresh.
pub fn set_enabled(on: bool) {
    SHOW.set(on);
    let s = &mut *STATS.lock().unwrap();
    s.start = None;
    s.last = None;
    s.times.clear();
    s.total = 0.0;
    s.cols.clear();
    s.col = 0;
    s.text = [String::new(), String::new()];
    s.text_at = None;
    s.drawn = None;
}

pub fn enabled() -> bool {
    SHOW.get()
}

/// Columns of the graph, one pixel each: the picture's width.
pub const W: usize = 240;
const COL_MS: f32 = 1000.0 / 60.0;
/// The graph's range at least: 0 to 20 ms.
const TOP_MS: f32 = 20.0;
const LINE: usize = 10;
const GRAPH: usize = 48;
/// The picture's height: two lines of text, a gap, the graph.
pub const H: usize = 2 * LINE + 2 + GRAPH;

struct Stats {
    start: Option<Instant>,
    last: Option<Instant>,
    /// frame times in ms of the last second, oldest first, and their sum
    times: VecDeque<f32>,
    total: f32,
    /// the graph's columns, oldest first; `col`: the number of columns made so far
    cols: VecDeque<Col>,
    col: u64,
    text: [String; 2],
    text_at: Option<Instant>,
    drawn: Option<Instant>,
}

/// A column of the graph: the frames ending in it (count, shortest, longest, last), and the
/// longest frame it lies within.
#[derive(Clone, Copy, Default)]
struct Col {
    n: u32,
    min: f32,
    max: f32,
    last: f32,
    long: f32,
}

static STATS: Mutex<Stats> = Mutex::new(Stats {
    start: None,
    last: None,
    times: VecDeque::new(),
    total: 0.0,
    cols: VecDeque::new(),
    col: 0,
    text: [String::new(), String::new()],
    text_at: None,
    drawn: None,
});

/// A new picture is shown now.
pub fn picture() {
    if !on() {
        return;
    }
    let s = &mut *STATS.lock().unwrap();
    let now = Instant::now();
    let start = *s.start.get_or_insert(now);
    let col_of = |t: Instant| ((t - start).as_secs_f32() * 1000.0 / COL_MS) as u64;
    let c = col_of(now);
    while s.col <= c {
        s.cols.push_back(Col::default());
        s.col += 1;
        if s.cols.len() > W {
            s.cols.pop_front();
        }
    }
    let Some(t) = s.last.replace(now) else { return };
    let ms = (now - t).as_secs_f32() * 1000.0;
    s.times.push_back(ms);
    s.total += ms;
    while s.total - s.times[0] >= 1000.0 {
        s.total -= s.times.pop_front().unwrap();
    }
    // the column it ends in, and every column before it lasted
    let n = s.cols.len() as u64;
    let last = s.cols.back_mut().unwrap();
    last.min = if last.n == 0 { ms } else { last.min.min(ms) };
    last.max = last.max.max(ms);
    last.last = ms;
    last.n += 1;
    for k in col_of(t).max(s.col - n)..s.col - 1 {
        let i = (n - (s.col - k)) as usize;
        s.cols[i].long = s.cols[i].long.max(ms);
    }
}

/// The display's new pixels (ARGB words, W x H) when they are due.
pub fn pixels() -> Option<Vec<u32>> {
    let s = &mut *STATS.lock().unwrap();
    let now = Instant::now();
    if s.drawn.is_some_and(|d| now - d < Duration::from_millis(16)) {
        return None;
    }
    s.drawn = Some(now);
    if s.text_at.is_none_or(|d| now - d >= Duration::from_millis(500)) {
        s.text_at = Some(now);
        text_of(s);
    }
    Some(render(s))
}

/// The text: frames a second and mean over the last half second, longest and 99th percentile
/// over the last second.
fn text_of(s: &mut Stats) {
    let (mut sum, mut n) = (0.0, 0);
    for &t in s.times.iter().rev() {
        if sum >= 500.0 {
            break;
        }
        sum += t;
        n += 1;
    }
    if n == 0 {
        return;
    }
    let mut second: Vec<f32> = s.times.iter().copied().collect();
    second.sort_unstable_by(f32::total_cmp);
    let max = second[second.len() - 1];
    let p99 = second[((second.len() - 1) as f32 * 0.99).round() as usize];
    s.text[0] = format!("{:.0} fps  {:.2} ms", n as f32 * 1000.0 / sum, sum / n as f32);
    s.text[1] = format!("max {max:.2}  99% {p99:.2} ms");
}

fn render(s: &Stats) -> Vec<u32> {
    let mut px = vec![0xa000_0000u32; W * H];
    text(&mut px, 2, 1, &s.text[0], 0xffff_ffff);
    text(&mut px, 2, 1 + LINE, &s.text[1], 0xffc0_c0c0);
    let cols: Vec<Col> = s.cols.iter().copied().collect();
    let skip = W - cols.len();
    // per column the frames ending in it (shortest, longest, last), or the frame it lies within
    let span = |c: &Col| if c.n == 0 { (c.long, c.long, c.long) } else { (c.min.min(c.long), c.max.max(c.long), c.last) };
    let top = top_ms(cols.iter().map(|c| span(c).1).fold(0.0, f32::max));
    let label = format!("{top:.0} ms");
    text(&mut px, W - 2 - text_width(&label), H - GRAPH, &label, 0xff80_8080);
    let y_of = |t: f32| H - 1 - ((t / top).min(1.0) * (GRAPH - 1) as f32).round() as usize;
    let mut prev: Option<f32> = None;
    for (x, c) in cols.iter().enumerate() {
        let (lo, hi, last) = span(c);
        if hi == 0.0 {
            prev = None;
            continue;
        }
        // a line as RivaTuner's: from the previous column's last frame through every frame here
        let (lo, hi) = prev.map_or((lo, hi), |p| (lo.min(p), hi.max(p)));
        prev = Some(last);
        for y in y_of(hi)..=y_of(lo) {
            let t = (H - 1 - y) as f32 / (GRAPH - 1) as f32 * top;
            px[y * W + x + skip] = if t > 1000.0 / 60.0 {
                0xffff_5050
            } else if t > 1000.0 / 120.0 {
                0xffff_e050
            } else {
                0xff60_ff60
            };
        }
    }
    px
}

/// The graph's range for the longest column shown: 20 ms, or the first step above it (low
/// frame rates filled the whole graph red).
fn top_ms(max: f32) -> f32 {
    [TOP_MS, 50.0, 100.0, 200.0, 500.0].into_iter().find(|&t| max <= t).unwrap_or(1000.0)
}

fn text_width(s: &str) -> usize {
    s.chars().map(|ch| glyph(ch).map_or(3, |(_, _, w)| w as usize + 1)).sum()
}

fn text(px: &mut [u32], x: usize, y: usize, s: &str, c: u32) {
    let mut x = x as i32;
    for ch in s.chars() {
        let Some((g, l, w)) = glyph(ch) else {
            x += 3;
            continue;
        };
        for (row, bits) in g.iter().enumerate() {
            for col in l..l + w {
                let px_x = x + col - l;
                if bits >> col & 1 != 0 && (0..W as i32).contains(&px_x) {
                    px[(y + row) * W + px_x as usize] = c;
                }
            }
        }
        x += w + 1;
    }
}

