//! Sums up a frame log (`FCOP_FRAMELOG=file.csv`): `framelog <file.csv> [skip ms]`.
//! Frame times (as RivaTuner's: mean, 1% / 0.1% lows, longest), hitches (frames 1.5 times
//! the median and 4 ms over it, or longer; at high rates 1.5 times alone is noise) with when they came and how far apart (a regular stutter has a
//! steady spacing), and the motion: how far the game time shown moved on per picture
//! against the time between them (`step` / 0.3 ticks a ms less `frame_ms`; 0 when the
//! game moves in step with the clock). Pictures that move too much or too little stutter
//! even when they come on time.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("framelog <file.csv> [skip ms]");
        std::process::exit(1);
    };
    let skip: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1)
    });
    // (ms, frame_ms, swap_ms, step) of each picture but the first
    let mut rows: Vec<(f64, f64, f64, Option<f64>)> = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 7 {
            continue;
        }
        let num = |i: usize| f[i].parse::<f64>().ok();
        let (Some(ms), Some(frame), Some(swap)) = (num(0), num(1), num(2)) else { continue };
        if ms <= skip || frame <= 0.0 {
            continue;
        }
        rows.push((ms, frame, swap, num(5)));
    }
    if rows.is_empty() {
        eprintln!("no pictures (after {skip} ms)");
        return;
    }
    let span = rows.last().unwrap().0 - rows[0].0 + rows[0].1;
    let mut times: Vec<f64> = rows.iter().map(|r| r.1).collect();
    times.sort_by(f64::total_cmp);
    let n = times.len();
    let pct = |v: &[f64], p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    let median = pct(&times, 0.5);
    // a low: the mean frame rate of the slowest 1% / 0.1% of the frames
    let low = |share: f64| {
        let k = ((n as f64 * share).ceil() as usize).max(1);
        1000.0 * k as f64 / times[n - k..].iter().sum::<f64>()
    };
    println!("{n} pictures in {:.1} s{}", span / 1000.0, if skip > 0.0 { format!(" (from {skip} ms)") } else { String::new() });
    println!(
        "frame time: mean {:.2} ms ({:.1} fps), median {median:.2}, 99% {:.2}, 99.9% {:.2}, longest {:.2} ms",
        span / n as f64,
        1000.0 * n as f64 / span,
        pct(&times, 0.99),
        pct(&times, 0.999),
        times[n - 1]
    );
    println!("lows: 1% {:.1} fps, 0.1% {:.1} fps", low(0.01), low(0.001));
    let mut swaps: Vec<f64> = rows.iter().map(|r| r.2).collect();
    swaps.sort_by(f64::total_cmp);
    println!("showing (swap): median {:.2} ms, 99% {:.2}, longest {:.2}", pct(&swaps, 0.5), pct(&swaps, 0.99), swaps[n - 1]);

    let limit = (1.5 * median).max(median + 4.0);
    let hitches: Vec<(f64, f64)> = rows.iter().filter(|r| r.1 >= limit).map(|r| (r.0, r.1)).collect();
    println!("hitches (>= {limit:.2} ms: 1.5 x median, 4 ms over it): {}", hitches.len());
    for h in hitches.iter().take(20) {
        println!("  at {:9.1} ms: {:.2} ms", h.0, h.1);
    }
    if hitches.len() > 20 {
        println!("  ...");
    }
    if hitches.len() >= 3 {
        let mut gaps: Vec<f64> = hitches.windows(2).map(|w| w[1].0 - w[0].0).collect();
        gaps.sort_by(f64::total_cmp);
        println!(
            "  spacing: median {:.1} ms (shortest {:.1}, longest {:.1}); steady spacing = a periodic stutter",
            pct(&gaps, 0.5),
            gaps[0],
            gaps[gaps.len() - 1]
        );
    }

    // motion: game time shown per picture against the time between them
    let mut off: Vec<f64> = rows.iter().filter_map(|r| r.3.map(|s| s / 0.3 - r.1)).collect();
    if off.is_empty() {
        println!("motion: no game times in the log (not in a mission?)");
        return;
    }
    let still = rows.iter().filter(|r| r.3 == Some(0.0)).count();
    let mut abs: Vec<f64> = off.iter().map(|v| v.abs()).collect();
    abs.sort_by(f64::total_cmp);
    off.sort_by(f64::total_cmp);
    println!(
        "motion (moved on less time between, ms): median |{:.2}|, 99% |{:.2}|, longest |{:.2}|; range {:.2} ... {:.2}",
        pct(&abs, 0.5),
        pct(&abs, 0.99),
        abs[abs.len() - 1],
        off[0],
        off[off.len() - 1]
    );
    let uneven: Vec<&(f64, f64, f64, Option<f64>)> =
        rows.iter().filter(|r| r.3.is_some_and(|s| (s / 0.3 - r.1).abs() > (0.5 * median).max(2.0))).collect();
    println!("  pictures not moving with the clock (off by more than {:.2} ms): {} ({still} did not move at all)", (0.5 * median).max(2.0), uneven.len());
    for r in uneven.iter().take(20) {
        println!("  at {:9.1} ms: {:.2} ms after the last, game moved {:.2} ms", r.0, r.1, r.3.unwrap() / 0.3);
    }
    if uneven.len() > 20 {
        println!("  ...");
    }
    if uneven.len() >= 3 {
        let mut gaps: Vec<f64> = uneven.windows(2).map(|w| w[1].0 - w[0].0).collect();
        gaps.sort_by(f64::total_cmp);
        println!("  spacing: median {:.1} ms (shortest {:.1}, longest {:.1})", pct(&gaps, 0.5), gaps[0], gaps[gaps.len() - 1]);
    }
}
