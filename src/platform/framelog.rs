//! `FCOP_FRAMELOG=file.csv`: a line for every 3D picture shown (Glide's grBufferSwap), to
//! measure stutter. The frame times alone do not show all of it: pictures between the game's
//! frames (`fps.rs`) show the game at an interpolated time, and a picture that shows the wrong
//! time moves unevenly although it came on time. So each line also holds the game time the
//! picture shows (in the game's ticks, 1/300 s) and how far it moved on since the picture
//! before. Smooth: `frame_ms` and `step` both even (`step` = `frame_ms` * 0.3 when the game
//! runs at its speed). `examples/framelog.rs <file.csv>` sums a log up.
//!
//! Columns: `ms` since the first picture (when it was handed to the window), `frame_ms` since
//! the picture before, `swap_ms` how long showing it took (the window's swap: waits there are
//! the driver's or V-Sync's), `clock` the game clock in ticks then, `shown` the game time the
//! picture shows, `step` shown less the picture before's, `kind` `frame` (the game's own
//! frame) or `between` (a picture drawn in between).
use std::io::Write;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Instant;

struct Log {
    file: std::fs::File,
    start: Option<Instant>,
    last: Option<(Instant, f64)>,
    /// what the render block being drawn shows: (clock, shown game time, in between)
    drawing: Option<(f64, f64, bool)>,
}

fn log() -> Option<&'static Mutex<Log>> {
    static LOG: OnceLock<Option<Mutex<Log>>> = OnceLock::new();
    LOG.get_or_init(|| {
        let path = std::env::var_os("FCOP_FRAMELOG").filter(|v| !v.is_empty() && v != "0")?;
        let mut file = match std::fs::File::create(&path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("FCOP_FRAMELOG: cannot write {}: {e}", std::path::Path::new(&path).display());
                return None;
            }
        };
        let _ = writeln!(file, "ms,frame_ms,swap_ms,clock,shown,step,kind");
        Some(Mutex::new(Log { file, start: None, last: None, drawing: None }))
    })
    .as_ref()
}

pub fn on() -> bool {
    log().is_some()
}

/// `fps::render_start`: the picture being drawn shows game time `shown` (ticks) at clock
/// `clock`; `between`: not the game's own frame.
pub fn drawing(clock: f64, shown: f64, between: bool) {
    if let Some(l) = log() {
        l.lock().unwrap().drawing = Some((clock, shown, between));
    }
}

/// A picture is being shown: `begin` when it was handed over, now when that returned.
pub fn shown(begin: Instant) {
    let Some(l) = log() else { return };
    let now = Instant::now();
    let l = &mut *l.lock().unwrap();
    let start = *l.start.get_or_insert(begin);
    // nothing known of what it shows (menus' 3D, a renderer without our hooks): empty
    let (clock, shown, between) = l.drawing.take().map_or((String::new(), None, false), |(c, s, b)| (format!("{c:.2}"), Some(s), b));
    let frame = l.last.map_or(0.0, |(t, _)| (begin - t).as_secs_f64() * 1000.0);
    let step = match (shown, l.last) {
        (Some(s), Some((_, p))) if p.is_finite() => format!("{:.2}", s - p),
        _ => String::new(),
    };
    // one write a line (writeln! on a file writes each piece by itself)
    let line = format!(
        "{:.3},{frame:.3},{:.3},{clock},{},{step},{}\n",
        (begin - start).as_secs_f64() * 1000.0,
        (now - begin).as_secs_f64() * 1000.0,
        shown.map_or(String::new(), |s| format!("{s:.2}")),
        if between { "between" } else { "frame" }
    );
    let _ = l.file.write_all(line.as_bytes());
    l.last = Some((begin, shown.unwrap_or(f64::NAN)));
}
