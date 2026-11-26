//! Future Cop: L.A.P.D. recompiled: compiles the player's FCopLAPD.exe and runs it.
//!
//!     fcop [path to FCopLAPD.exe]
//!     fcop export      the texture pages cut into their pictures (`export`)
//!     fcop import      changed pictures back into pages: replacement textures
//!     fcop export-sounds   the missions' sounds as WAV files (`sounds`)
//!
//! Without a path: the game files in `game_files` next to fcop.exe (see `setup`).
//!
//! A release build is a Windows program without a console window: its messages go to the
//! console it was started from, else to fcop.log next to fcop.exe.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use fcop::discover::{Code, Program};
use fcop::image::{Image, load_functions};
use fcop::jit::{compile_cached, load_cached_only};
use fcop::setup;
use fcop::smc::Smc;
use fcop::splash;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Without a console window of our own: messages to the console we were started from (or
/// wherever they were redirected), else to fcop.log next to fcop.exe.
#[cfg(windows)]
fn output() {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::System::Console::*;
    unsafe {
        if !GetStdHandle(STD_ERROR_HANDLE).is_null() {
            return;
        }
        let out = if AttachConsole(ATTACH_PARENT_PROCESS) != 0 {
            std::fs::OpenOptions::new().write(true).open("CONOUT$").ok()
        } else {
            let log = std::env::current_exe().ok().and_then(|p| Some(p.parent()?.join("fcop.log")));
            log.and_then(|p| std::fs::File::create(p).ok())
        };
        if let Some(f) = out {
            let h = f.into_raw_handle();
            SetStdHandle(STD_OUTPUT_HANDLE, h as _);
            SetStdHandle(STD_ERROR_HANDLE, h as _);
        }
    }
}

/// A macOS app started from Finder has no terminal: messages go to fcop.log in its folder.
#[cfg(unix)]
fn output() {
    use std::os::fd::AsRawFd;
    let Some(dir) = setup::app_folder() else { return };
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(f) = std::fs::File::create(dir.join("fcop.log")) {
        unsafe {
            libc::dup2(f.as_raw_fd(), 1);
            libc::dup2(f.as_raw_fd(), 2);
        }
    }
}

#[cfg(windows)]
fn open_launcher() -> bool {
    let Some(path) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("fcop-launcher.exe"))) else {
        return false;
    };
    std::process::Command::new(path).spawn().is_ok()
}

#[cfg(windows)]
fn load_launcher_settings() {
    let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) else { return };
    if !dir.join(fcop::launcher::SETTINGS_FILE).is_file() { return; }
    let settings = fcop::launcher::Settings::load(&dir);
    for (name, enabled) in [("FCOP_MUSIC", settings.music_enabled), ("FCOP_MODERN_UI", settings.modern_ui)] {
        if std::env::var_os(name).is_none() {
            // This occurs before threads are started.
            unsafe { std::env::set_var(name, if enabled { "1" } else { "0" }); }
        }
    }
}

fn main() {
    #[cfg(unix)]
    if std::env::args().nth(1).as_deref() == Some("--splash") {
        return splash::child();
    }
    output();
    if let Some(cmd @ ("export" | "import" | "export-sounds")) = std::env::args().nth(1).as_deref() {
        std::process::exit(tool(cmd));
    }
    #[cfg(windows)]
    if !matches!(std::env::args().nth(1).as_deref(), Some("--prepare" | "--validate-cache")) {
        load_launcher_settings();
    }
    // guest calls nest on the host stack like the original's: run on a thread with a large one
    // (macOS: windows only on the main thread, its stack made as large by build.rs)
    #[cfg(target_os = "macos")]
    let code = run();
    #[cfg(not(target_os = "macos"))]
    let code = std::thread::Builder::new().stack_size(256 << 20).spawn(run).unwrap().join().unwrap();
    fcop::platform::kernel32::terminate(code);
}

fn run() -> u32 {
    // first, before other allocations take its place
    fcop::platform::reserve_memory();
    splash::show();
    splash::status("Looking for the game...");
    let mut args = std::env::args().skip(1);
    let first = args.next();
    let prepare = first.as_deref() == Some("--prepare");
    let validate = first.as_deref() == Some("--validate-cache");
    let explicit_cache = first.as_deref() == Some("--cache-only");
    let path = if prepare || validate || explicit_cache { args.next() } else { first };
    let cache_only = validate || explicit_cache || (cfg!(windows) && !prepare);
    if args.next().is_some() || ((prepare || validate || explicit_cache) && path.is_none()) {
        splash::fail("Usage: fcop [path to FCopLAPD.exe]\n       fcop --cache-only|--prepare|--validate-cache <path to installed FCopLAPD.exe>");
        return 2;
    }
    let found = match path {
        Some(p) => setup::check(Path::new(&p)).map(|_| PathBuf::from(p)),
        None if cfg!(windows) => {
            let exe = setup::root().join("FCopLAPD").join("FCopLAPD.exe");
            setup::check(&exe).map(|_| exe)
        }
        None => setup::game_exe(&setup::root()),
    };
    let exe = match found {
        Ok(e) => e,
        Err(text) => {
            #[cfg(windows)]
            if !prepare && !validate && !explicit_cache && open_launcher() {
                splash::close();
                return 0;
            }
            splash::fail(&text);
            return 2;
        }
    };
    // (loading it picks the version: `ver`)
    let img = Image::load(&exe).unwrap_or_else(|e| panic!("{}: {e}", exe.display()));
    let ranges = load_functions();
    if !prepare && !validate {
        fcop::platform::dinput::start();
    }
    splash::status(if cache_only { "Loading compiled game..." } else { "Compiling the game..." });
    let t = Instant::now();
    let mut code = Code::new(&img);
    let prog = Program::build(&mut code, &ranges);
    let smc = Smc::find(&prog, &mut code);
    let found = t.elapsed().as_millis();
    // the machine code is kept beside the EXE, for this program and this EXE
    let cache = exe.with_file_name(setup::COMPILED);
    let key = cache_key(&img);
    let hooks: Vec<_> = fcop::platform::HOOKS.iter().map(|&(a, h)| (a.at(), h)).collect();
    let compiled = if cache_only {
        match key.and_then(|key| load_cached_only(&prog, &smc, &mut code, &hooks, (cache.as_path(), key))) {
            Some(compiled) => compiled,
            None => {
                #[cfg(windows)]
                if !explicit_cache && !validate && open_launcher() {
                    splash::close();
                    return 0;
                }
                splash::fail("No compatible compiled game cache was found. Open the launcher to prepare it.");
                return 2;
            }
        }
    } else {
        compile_cached(&prog, &smc, &mut code, &hooks, key.map(|k| (cache.as_path(), k)))
    };
    if prepare || validate {
        drop(compiled);
        splash::close();
        return 0;
    }
    let st = &compiled.stats;
    if st.cached {
        eprintln!("compiled code of {} functions loaded in {} ms", st.functions, t.elapsed().as_millis());
    } else {
        eprintln!(
            "compiled {} functions in {} ms (finding them {found}, lifting {}, compiling {})",
            st.functions,
            t.elapsed().as_millis(),
            st.lift_ms,
            st.compile_ms
        );
    }
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let code = fcop::platform::run_game(&img, dir);
    drop(compiled);
    code
}

/// `fcop export` and `fcop import`: in `game_files/Textures`; `fcop export-sounds`: in
/// `game_files/Sounds`.
fn tool(cmd: &str) -> i32 {
    let root = setup::root();
    let exe = match setup::game_exe(&root) {
        Ok(e) => e,
        Err(text) => {
            eprintln!("{text}");
            return 2;
        }
    };
    let game = exe.parent().unwrap_or(Path::new("."));
    let missions = std::fs::read_dir(game)
        .ok()
        .and_then(|d| d.flatten().map(|e| e.path()).find(|p| p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("missions"))))
        .unwrap_or_else(|| game.join("missions"));
    let out = root.join(if cmd == "export-sounds" { "Sounds" } else { "Textures" });
    let done = match cmd {
        "import" => fcop::export::import(&missions, &out),
        "export-sounds" => fcop::sounds::export(&missions, &out),
        _ => fcop::export::export(&missions, &out),
    };
    match done {
        Ok(()) => {
            eprintln!("written to {}", out.display());
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// What the compiled code depends on: this program (any change to the recompiler) and the
/// game's EXE. None when our own file cannot be read.
fn cache_key(img: &Image) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let own = std::fs::read(std::env::current_exe().ok()?).ok()?;
    let mut h = Sha256::new();
    h.update(&own);
    h.update(&img.data);
    // the guest memory's base, a constant in the code when fixed
    h.update(fcop::lift::FIXED_BASE.load(std::sync::atomic::Ordering::Relaxed).to_le_bytes());
    Some(h.finalize().into())
}
