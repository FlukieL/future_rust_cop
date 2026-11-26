//! Windows launcher. Settings and backups are kept beside the launcher.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("The launcher currently supports Windows only. Run fcop directly on this platform.");
}

#[cfg(windows)]
mod windows {
    use fcop::launcher::{self, Settings};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Controls::*;
    use windows_sys::Win32::UI::Controls::Dialogs::*;
    use windows_sys::Win32::UI::HiDpi::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    const ISO: i32 = 101;
    const BROWSE_ISO: i32 = 102;
    const MUSIC: i32 = 103;
    const MUSIC_PATH: i32 = 104;
    const BROWSE_MUSIC: i32 = 105;
    const MODERN: i32 = 106;
    const MSAA: i32 = 107;
    const FILTER: i32 = 108;
    const VSYNC: i32 = 109;
    const FPS: i32 = 110;
    const RUMBLE: i32 = 111;
    const BACKUP: i32 = 113;
    const BACKUPS: i32 = 114;
    const RESTORE: i32 = 115;
    const OPEN_FOLDER: i32 = 116;
    const RESET_PAD: i32 = 117;
    const PLAY: i32 = 118;
    const STATUS: i32 = 119;
    const GAME_DATA: i32 = 120;
    const APPLY: i32 = 125;
    const RESOLUTION: i32 = 126;
    const FRAME_RATE: i32 = 127;
    const FULLSCREEN: i32 = 128;
    const SKIP_INTRO: i32 = 129;
    const RELOAD: i32 = 130;
    const TABS: i32 = 131;
    const EXPORT_TEXTURES: i32 = 132;
    const IMPORT_TEXTURES: i32 = 133;
    const EXPORT_SOUNDS: i32 = 134;
    const FINISHED: u32 = WM_APP + 1;
    const DISCOVERED: u32 = WM_APP + 2;
    const PROGRESS: u32 = WM_APP + 3;

    static DIR: OnceLock<PathBuf> = OnceLock::new();
    static BUSY: AtomicBool = AtomicBool::new(false);
    static GAME_SETTINGS_LOADED: AtomicBool = AtomicBool::new(false);
    static BRUSH: AtomicIsize = AtomicIsize::new(0);
    static TAB: AtomicIsize = AtomicIsize::new(0);
    static INPUT_BRUSH: AtomicIsize = AtomicIsize::new(0);
    static ANIMATION: AtomicIsize = AtomicIsize::new(0);

    // Win32 COLORREF values are stored as 0x00bbggrr.
    const BACKGROUND: u32 = 0x00301b0b;
    const INPUT_BACKGROUND: u32 = 0x00452510;
    const PANEL: u32 = 0x00512d12;
    const ACTIVE: u32 = 0x00844719;
    const EDGE: u32 = 0x00cc8c3d;
    const TEXT: u32 = 0x00ffecd1;
    const MUTED: u32 = 0x00ccad80;
    const TAB_NAMES: [&str; 5] = ["GENERAL", "VIDEO", "GAMEPAD", "SAVES", "TOOLS"];
    const TAB_CONTROLS: [&[i32]; 5] = [
        &[ISO, BROWSE_ISO, MUSIC, MUSIC_PATH, BROWSE_MUSIC, MODERN, SKIP_INTRO, 200],
        &[RESOLUTION, FRAME_RATE, MSAA, FILTER, VSYNC, FPS, FULLSCREEN, 201, 202, 203, 204],
        &[RUMBLE, RESET_PAD, 205],
        &[BACKUP, BACKUPS, RESTORE, OPEN_FOLDER, GAME_DATA, 206],
        &[EXPORT_TEXTURES, IMPORT_TEXTURES, EXPORT_SOUNDS, 207, 208],
    ];

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    unsafe fn set(hwnd: HWND, s: &str) {
        unsafe { SetWindowTextW(hwnd, wide(s).as_ptr()); }
    }

    unsafe fn text(hwnd: HWND) -> String {
        unsafe {
            let len = GetWindowTextLengthW(hwnd).max(0) as usize;
            let mut buf = vec![0u16; len + 1];
            let read = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
            String::from_utf16_lossy(&buf[..read])
        }
    }

    unsafe fn checked(hwnd: HWND, id: i32) -> bool {
        unsafe { SendMessageW(GetDlgItem(hwnd, id), BM_GETCHECK, 0, 0) == BST_CHECKED as isize }
    }

    unsafe fn check(hwnd: HWND, id: i32, value: bool) {
        unsafe { SendMessageW(GetDlgItem(hwnd, id), BM_SETCHECK, value as usize, 0); }
    }

    unsafe fn selection(hwnd: HWND, id: i32) -> i32 {
        unsafe { SendMessageW(GetDlgItem(hwnd, id), CB_GETCURSEL, 0, 0) as i32 }
    }

    unsafe fn select(hwnd: HWND, id: i32, index: usize) {
        unsafe { SendMessageW(GetDlgItem(hwnd, id), CB_SETCURSEL, index, 0); }
    }

    unsafe fn combo(hwnd: HWND, id: i32, values: &[&str], selected: usize) {
        unsafe {
            let control = GetDlgItem(hwnd, id);
            for value in values {
                SendMessageW(control, CB_ADDSTRING, 0, wide(value).as_ptr() as isize);
            }
            select(hwnd, id, selected);
        }
    }

    const MSAA_VALUES: [u32; 4] = [0, 2, 4, 8];
    const FILTER_VALUES: [u32; 6] = [0, 1, 2, 4, 8, 16];

    unsafe fn settings(hwnd: HWND) -> Settings {
        unsafe {
            Settings {
                iso: text(GetDlgItem(hwnd, ISO)),
                music_enabled: checked(hwnd, MUSIC),
                music_path: text(GetDlgItem(hwnd, MUSIC_PATH)),
                modern_ui: checked(hwnd, MODERN),
                ..Settings::default()
            }
        }
    }

    unsafe fn choose(owner: HWND, music: bool) -> Option<PathBuf> {
        unsafe {
            let filter = if music {
                "Music or disc image\0FE.MIS;*.iso;*.bin;*.img\0All files\0*.*\0\0"
            } else {
                "Disc image\0*.iso;*.bin;*.img\0All files\0*.*\0\0"
            };
            let filter: Vec<u16> = filter.encode_utf16().collect();
            let mut file = vec![0u16; 32768];
            let mut open: OPENFILENAMEW = std::mem::zeroed();
            open.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
            open.hwndOwner = owner;
            open.lpstrFilter = filter.as_ptr();
            open.lpstrFile = file.as_mut_ptr();
            open.nMaxFile = file.len() as u32;
            open.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
            if GetOpenFileNameW(&mut open) == 0 { return None; }
            let len = file.iter().position(|&c| c == 0).unwrap_or(file.len());
            Some(PathBuf::from(String::from_utf16_lossy(&file[..len])))
        }
    }

    fn send(hwnd: isize, message: u32, value: String) {
        let data = Box::into_raw(Box::new(value));
        if unsafe { PostMessageW(hwnd as HWND, message, 0, data as LPARAM) } == 0 {
            unsafe { drop(Box::from_raw(data)); }
        }
    }

    fn run_command(binary: &Path, args: &[&std::ffi::OsStr]) -> Result<(), String> {
        let output = Command::new(binary)
            .args(args)
            .env("FCOP_BATCH", "1")
            .env_remove("FCOP_COUNT")
            .output()
            .map_err(|e| format!("Cannot run {}: {e}", binary.display()))?;
        if output.status.success() {
            Ok(())
        } else {
            let error = String::from_utf8_lossy(&output.stderr);
            let tail: String = error.chars().rev().take(1200).collect::<String>().chars().rev().collect();
            Err(format!("Operation failed ({}): {}", output.status, tail.trim()))
        }
    }

    fn installed(root: &Path) -> Result<PathBuf, String> {
        let game = root.join("FCopLAPD");
        let exe = game.join("FCopLAPD.exe");
        fcop::setup::check(&exe)?;
        if !game.join("febmp.bin").is_file() || !game.join("missions").is_dir() {
            return Err("The existing installation is incomplete; it will not be overwritten.".into());
        }
        Ok(exe)
    }

    unsafe fn refresh_backups(hwnd: HWND) {
        unsafe {
            let control = GetDlgItem(hwnd, BACKUPS);
            SendMessageW(control, CB_RESETCONTENT, 0, 0);
            let root = DIR.get().unwrap().join(launcher::BACKUP_DIR);
            let names = launcher::list_backups(&root);
            for name in &names {
                SendMessageW(control, CB_ADDSTRING, 0, wide(name).as_ptr() as isize);
            }
            if !names.is_empty() { select(hwnd, BACKUPS, 0); }
        }
    }

    unsafe fn show_tab(hwnd: HWND, tab: usize) {
        unsafe {
            TAB.store(tab as isize, Ordering::Relaxed);
            for (i, controls) in TAB_CONTROLS.iter().enumerate() {
                for &id in *controls {
                    ShowWindow(GetDlgItem(hwnd, id), if i == tab { SW_SHOW } else { SW_HIDE });
                }
            }
            ShowWindow(GetDlgItem(hwnd, RELOAD), if tab >= 3 { SW_HIDE } else { SW_SHOW });
        }
    }

    unsafe fn refresh_game_settings(hwnd: HWND) {
        unsafe {
            GAME_SETTINGS_LOADED.store(false, Ordering::Release);
            let game = DIR.get().unwrap().join("game_files").join("FCopLAPD");
            if !game.is_dir() { status(hwnd, "Install the game before editing game settings."); return; }
            let values = match launcher::game_registry(&game) {
                Ok(v) => v,
                Err(e) => { status(hwnd, &format!("Cannot read game settings: {e}")); return; }
            };
            let get = |key: &str, default| values.get(key).copied().unwrap_or(default);
            let size = get("rendersize", 0);
            set(GetDlgItem(hwnd, RESOLUTION), &if size == 0 { "window".into() } else { format!("{}x{}", size >> 16, size & 0xffff) });
            let rate = get("framerate", 0);
            select(hwnd, FRAME_RATE, fcop::platform::fps::CHOICES.iter().position(|&v| v == rate).unwrap_or(0));
            select(hwnd, MSAA, MSAA_VALUES.iter().position(|&v| v == get("msaa", 4)).unwrap_or(2));
            select(hwnd, FILTER, FILTER_VALUES.iter().position(|&v| v == get("texturefilter", 16)).unwrap_or(5));
            for (id, key, default) in [(VSYNC, "vsync", 1), (FPS, "showfps", 0), (RUMBLE, "controllervibration", 1), (FULLSCREEN, "fullscreen", 0), (SKIP_INTRO, "skipintro", 0)] {
                check(hwnd, id, get(key, default) & 0xff != 0);
            }
            GAME_SETTINGS_LOADED.store(true, Ordering::Release);
        }
    }

    unsafe fn save_settings(hwnd: HWND) -> Result<(), String> {
        unsafe {
            let updates = if GAME_SETTINGS_LOADED.load(Ordering::Acquire) {
                let input = text(GetDlgItem(hwnd, RESOLUTION));
                let size = if input.trim().eq_ignore_ascii_case("window") {
                    0
                } else {
                    let (w, h) = input.trim().split_once(['x', 'X']).ok_or("Resolution must be window or WIDTHxHEIGHT")?;
                    let w: u32 = w.trim().parse().map_err(|_| "Invalid resolution width")?;
                    let h: u32 = h.trim().parse().map_err(|_| "Invalid resolution height")?;
                    if !(320..=65535).contains(&w) || !(200..=65535).contains(&h) {
                        return Err("Resolution must be at least 320x200 and at most 65535x65535".into());
                    }
                    w << 16 | h
                };
                let choice = |id, values: &[u32]| -> Result<u32, String> {
                    usize::try_from(selection(hwnd, id)).ok().and_then(|i| values.get(i)).copied().ok_or_else(|| "Select a valid video option".into())
                };
                Some([
                    ("SkipIntro", checked(hwnd, SKIP_INTRO) as u32),
                    ("RenderSize", size),
                    ("FrameRate", choice(FRAME_RATE, fcop::platform::fps::CHOICES)?),
                    ("Msaa", choice(MSAA, &MSAA_VALUES)?),
                    ("TextureFilter", choice(FILTER, &FILTER_VALUES)?),
                    ("VSync", checked(hwnd, VSYNC) as u32),
                    ("ShowFps", checked(hwnd, FPS) as u32),
                    ("Fullscreen", checked(hwnd, FULLSCREEN) as u32),
                    ("ControllerVibration", checked(hwnd, RUMBLE) as u32),
                ])
            } else {
                None
            };
            let dir = DIR.get().unwrap();
            settings(hwnd).save(dir).map_err(|e| format!("Cannot save launcher settings: {e}"))?;
            if let Some(updates) = updates {
                let game = dir.join("game_files").join("FCopLAPD");
                launcher::write_game_settings(&game, &updates).map_err(|e| format!("Launcher settings saved; cannot save game settings: {e}"))?;
            } else if dir.join("game_files").join("FCopLAPD").is_dir() {
                return Err("Launcher settings saved; game settings could not be loaded. Reload them before saving.".into());
            }
            Ok(())
        }
    }

    unsafe fn fill(dc: HDC, rect: &RECT, color: u32) {
        unsafe {
            let brush = CreateSolidBrush(color);
            FillRect(dc, rect, brush);
            DeleteObject(brush as HGDIOBJ);
        }
    }

    unsafe fn draw_control(item: &DRAWITEMSTRUCT) {
        unsafe {
            let dc = item.hDC;
            let rect = item.rcItem;
            let disabled = item.itemState & ODS_DISABLED != 0;
            let selected = item.itemState & ODS_SELECTED != 0;
            let tab = item.CtlType == ODT_TAB;
            let active = selected;
            let color = if disabled { BACKGROUND } else if active || item.CtlID == PLAY as u32 { ACTIVE } else { PANEL };
            fill(dc, &rect, color);
            let border = CreateSolidBrush(if disabled { PANEL } else { EDGE });
            FrameRect(dc, &rect, border);
            DeleteObject(border as HGDIOBJ);
            if tab && active {
                let stripe = RECT { left: rect.left + 1, top: rect.bottom - 3, right: rect.right - 1, bottom: rect.bottom - 1 };
                fill(dc, &stripe, 0x00ffcc67);
            }
            let caption = if tab {
                TAB_NAMES.get(item.itemID as usize).copied().unwrap_or("").to_string()
            } else {
                text(item.hwndItem)
            };
            let mut label = rect;
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(dc, if disabled { MUTED } else { TEXT });
            let font = SendMessageW(item.hwndItem, WM_GETFONT, 0, 0) as HFONT;
            let previous = if font.is_null() { std::ptr::null_mut() } else { SelectObject(dc, font as HGDIOBJ) };
            DrawTextW(dc, wide(&caption).as_ptr(), -1, &mut label, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
            if !previous.is_null() { SelectObject(dc, previous); }
            if item.itemState & ODS_FOCUS != 0 {
                let focus = RECT { left: rect.left + 3, top: rect.top + 3, right: rect.right - 3, bottom: rect.bottom - 3 };
                DrawFocusRect(dc, &focus);
            }
        }
    }

    unsafe fn status(hwnd: HWND, value: &str) {
        unsafe { set(GetDlgItem(hwnd, STATUS), value); }
    }

    unsafe fn confirm(hwnd: HWND, value: &str) -> bool {
        unsafe { MessageBoxW(hwnd, wide(value).as_ptr(), wide("Future Cop launcher").as_ptr(), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) == IDYES }
    }

    unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        unsafe {
            match msg {
                WM_COMMAND => {
                    let id = (wp & 0xffff) as i32;
                    if BUSY.load(Ordering::Acquire) { return 0; }
                    match id {
                        RELOAD => {
                            if confirm(hwnd, "Reload game settings? Unapplied changes in all sections will be discarded.") {
                                refresh_game_settings(hwnd);
                            }
                            0
                        }
                        APPLY => {
                            match save_settings(hwnd) {
                                Ok(()) => status(hwnd, "Settings saved."),
                                Err(e) => status(hwnd, &e),
                            }
                            0
                        }
                        BROWSE_ISO | BROWSE_MUSIC => {
                            if let Some(path) = choose(hwnd, id == BROWSE_MUSIC) {
                                set(GetDlgItem(hwnd, if id == BROWSE_ISO { ISO } else { MUSIC_PATH }), &path.to_string_lossy());
                            }
                            0
                        }
                        PLAY => {
                            if BUSY.swap(true, Ordering::AcqRel) { return 0; }
                            let options = settings(hwnd);
                            let dir = DIR.get().unwrap().clone();
                            if let Err(e) = options.save(&dir) {
                                BUSY.store(false, Ordering::Release);
                                status(hwnd, &format!("Cannot save launcher settings: {e}"));
                                return 0;
                            }
                            EnableWindow(GetDlgItem(hwnd, PLAY), 0);
                            status(hwnd, "Preparing game files...");
                            let window = hwnd as isize;
                            std::thread::spawn(move || {
                                let result = (|| -> Result<(), String> {
                                    let game_binary = dir.join("fcop.exe");
                                    if !game_binary.is_file() {
                                        return Err(format!("Game executable not found: {}", game_binary.display()));
                                    }
                                    let root = dir.join("game_files");
                                    let exe = if root.join("FCopLAPD").exists() {
                                        installed(&root)?
                                    } else {
                                        if options.iso.trim().is_empty() {
                                            return Err("Select a PC game disc image first.".into());
                                        }
                                        fcop::setup::install_launcher_image(Path::new(&options.iso), &root)?
                                    };
                                    if options.music_enabled && !root.join("PS1").join("FE.MIS").is_file() {
                                        if options.music_path.trim().is_empty() {
                                            return Err("Select a PS1 disc or FE.MIS, or disable menu music.".into());
                                        }
                                        fcop::setup::install_launcher_music(Path::new(&options.music_path), &root)?;
                                    }
                                    send(window, PROGRESS, "Checking compiled game...".into());
                                    let validate = || run_command(&game_binary, &["--validate-cache".as_ref(), exe.as_os_str()]);
                                    if validate().is_err() {
                                        send(window, PROGRESS, "Compiling game; this may take a while...".into());
                                        run_command(&game_binary, &["--prepare".as_ref(), exe.as_os_str()])?;
                                        send(window, PROGRESS, "Validating compiled game...".into());
                                        validate().map_err(|e| format!("Compilation did not produce a usable cache: {e}"))?;
                                    }
                                    let mut cmd = Command::new(game_binary);
                                    cmd.arg("--cache-only").arg(exe).env_remove("FCOP_COUNT");
                                    options.apply_env(&mut cmd);
                                    cmd.spawn().map_err(|e| format!("Cannot start the game: {e}"))?;
                                    Ok(())
                                })();
                                send(window, FINISHED, result.err().unwrap_or_else(|| "LAUNCHED".into()));
                            });
                            0
                        }
                        EXPORT_TEXTURES | IMPORT_TEXTURES | EXPORT_SOUNDS => {
                            let command = match id {
                                EXPORT_TEXTURES => "export",
                                IMPORT_TEXTURES => "import",
                                _ => "export-sounds",
                            };
                            if id == IMPORT_TEXTURES && !confirm(hwnd, "Import edited textures into replacement pages? Existing generated replacement pages may be updated.") {
                                return 0;
                            }
                            if BUSY.swap(true, Ordering::AcqRel) { return 0; }
                            EnableWindow(GetDlgItem(hwnd, PLAY), 0);
                            status(hwnd, "Running tool...");
                            let dir = DIR.get().unwrap().clone();
                            let window = hwnd as isize;
                            std::thread::spawn(move || {
                                let root = dir.join("game_files");
                                let result = installed(&root).and_then(|exe| {
                                    let missions = exe.parent().unwrap().join("missions");
                                    match command {
                                        "export" => fcop::export::export(&missions, &root.join("Textures")),
                                        "import" => fcop::export::import(&missions, &root.join("Textures")),
                                        _ => fcop::sounds::export(&missions, &root.join("Sounds")),
                                    }
                                });
                                send(window, FINISHED, result.err().unwrap_or_else(|| format!("{command} completed. See game_files for output.")));
                            });
                            0
                        }
                        BACKUP | RESTORE | RESET_PAD => {
                            let dir = DIR.get().unwrap();
                            let game = dir.join("game_files").join("FCopLAPD");
                            let backups = dir.join(launcher::BACKUP_DIR);
                            let result = if id == BACKUP {
                                launcher::backup_saves(&game, &backups)
                                    .map(|p| format!("Backup created: {}", p.display()))
                            } else if id == RESTORE {
                                let name = text(GetDlgItem(hwnd, BACKUPS));
                                if name.is_empty() {
                                    status(hwnd, "Select a backup to restore.");
                                    return 0;
                                }
                                if !confirm(hwnd, "Close the game first. Restore this backup? Current save and settings files will be backed up first.") {
                                    return 0;
                                }
                                launcher::restore_backup(&backups, &name, &game)
                                    .map(|n| format!("Restored {n} files; previous files were backed up."))
                            } else {
                                if !confirm(hwnd, "Close the game first. Reset gamepad bindings to defaults? Existing files will be backed up first.") {
                                    return 0;
                                }
                                launcher::reset_gamepad_bindings(&game, &backups)
                                    .map(|n| format!("Reset {n} gamepad binding files."))
                            };
                            match result {
                                Ok(message) => status(hwnd, &message),
                                Err(e) => status(hwnd, &format!("Operation failed: {e}")),
                            }
                            refresh_backups(hwnd);
                            if id == RESTORE { refresh_game_settings(hwnd); }
                            0
                        }
                        GAME_DATA => {
                            let game = DIR.get().unwrap().join("game_files").join("FCopLAPD");
                            let report = launcher::game_data_report(&game)
                                .unwrap_or_else(|e| format!("Cannot read game data: {e}"));
                            MessageBoxW(hwnd, wide(&report).as_ptr(), wide("Game settings and files").as_ptr(), MB_OK | MB_ICONINFORMATION);
                            0
                        }
                        OPEN_FOLDER => {
                            let game = DIR.get().unwrap().join("game_files").join("FCopLAPD");
                            if !game.is_dir() {
                                status(hwnd, "The game folder does not exist yet.");
                            } else if let Err(e) = Command::new("explorer.exe").arg(game).spawn() {
                                status(hwnd, &format!("Cannot open game folder: {e}"));
                            }
                            0
                        }
                        _ => DefWindowProcW(hwnd, msg, wp, lp),
                    }
                }
                WM_NOTIFY => {
                    let header = &*(lp as *const NMHDR);
                    if header.idFrom == TABS as usize && header.code == TCN_SELCHANGE {
                        let tab = SendMessageW(GetDlgItem(hwnd, TABS), TCM_GETCURSEL, 0, 0);
                        if (0..TAB_CONTROLS.len() as isize).contains(&tab) { show_tab(hwnd, tab as usize); }
                        0
                    } else {
                        DefWindowProcW(hwnd, msg, wp, lp)
                    }
                }
                PROGRESS => {
                    let value = Box::from_raw(lp as *mut String);
                    status(hwnd, &value);
                    0
                }
                FINISHED => {
                    let value = Box::from_raw(lp as *mut String);
                    if value.as_str() == "LAUNCHED" {
                        DestroyWindow(hwnd);
                    } else {
                        status(hwnd, &value);
                        BUSY.store(false, Ordering::Release);
                        EnableWindow(GetDlgItem(hwnd, PLAY), 1);
                    }
                    0
                }
                DISCOVERED => {
                    let value = Box::from_raw(lp as *mut String);
                    if text(GetDlgItem(hwnd, ISO)).is_empty() {
                        set(GetDlgItem(hwnd, ISO), &value);
                    }
                    0
                }
                WM_DRAWITEM => {
                    if lp != 0 {
                        let item = &*(lp as *const DRAWITEMSTRUCT);
                        if item.CtlType == ODT_TAB || item.CtlType == ODT_BUTTON {
                            draw_control(item);
                            return 1;
                        }
                    }
                    DefWindowProcW(hwnd, msg, wp, lp)
                }
                WM_PAINT => {
                    let mut ps: PAINTSTRUCT = std::mem::zeroed();
                    let dc = BeginPaint(hwnd, &mut ps);
                    let dpi = GetDpiForWindow(hwnd) as i32;
                    let px = |n: i32| n * dpi / 96;
                    let line = RECT { left: px(20), top: px(36), right: px(630), bottom: px(38) };
                    fill(dc, &line, PANEL);
                    let pos = ANIMATION.load(Ordering::Relaxed) as i32 % 100;
                    let x = px(20 + pos * 5);
                    let highlight = RECT { left: x, top: line.top, right: (x + px(90)).min(line.right), bottom: line.bottom };
                    fill(dc, &highlight, EDGE);
                    EndPaint(hwnd, &ps);
                    0
                }
                WM_TIMER if wp == 1 => {
                    ANIMATION.fetch_add(1, Ordering::Relaxed);
                    let dpi = GetDpiForWindow(hwnd) as i32;
                    let area = RECT { left: 20 * dpi / 96, top: 36 * dpi / 96, right: 630 * dpi / 96, bottom: 38 * dpi / 96 };
                    InvalidateRect(hwnd, &area, 0);
                    0
                }
                WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
                    let dc = wp as HDC;
                    SetTextColor(dc, TEXT);
                    SetBkColor(dc, BACKGROUND);
                    SetBkMode(dc, TRANSPARENT as i32);
                    BRUSH.load(Ordering::Acquire) as LRESULT
                }
                WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
                    let dc = wp as HDC;
                    SetTextColor(dc, TEXT);
                    SetBkColor(dc, INPUT_BACKGROUND);
                    INPUT_BRUSH.load(Ordering::Acquire) as LRESULT
                }
                WM_CLOSE => {
                    if BUSY.load(Ordering::Acquire) {
                        status(hwnd, "Please wait for the current operation to finish.");
                        return 0;
                    }
                    if let Err(e) = settings(hwnd).save(DIR.get().unwrap()) {
                        status(hwnd, &format!("Cannot save launcher settings: {e}"));
                        if !confirm(hwnd, "Could not save launcher settings. Close without saving?") {
                            return 0;
                        }
                    }
                    DestroyWindow(hwnd);
                    0
                }
                WM_DESTROY => { KillTimer(hwnd, 1); PostQuitMessage(0); 0 }
                _ => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
    }

    pub fn run() {
        unsafe {
            let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) else {
                eprintln!("Cannot locate launcher directory");
                return;
            };
            let saved = Settings::load(&dir);
            let _ = DIR.set(dir.clone());
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let scale = GetDpiForSystem() as i32;
            let px = |n: i32| n * scale / 96;
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("FcopLauncher");
            let brush = CreateSolidBrush(BACKGROUND);
            let input_brush = CreateSolidBrush(INPUT_BACKGROUND);
            BRUSH.store(brush as isize, Ordering::Release);
            INPUT_BRUSH.store(input_brush as isize, Ordering::Release);
            let wc = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: brush,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassW(&wc);
            let common = INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_TAB_CLASSES };
            InitCommonControlsEx(&common);
            let hwnd = CreateWindowExW(
                0, class.as_ptr(), wide("Future Cop: L.A.P.D. - Launcher").as_ptr(),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                CW_USEDEFAULT, CW_USEDEFAULT, px(680), px(700),
                std::ptr::null_mut(), std::ptr::null_mut(), instance, std::ptr::null(),
            );
            if hwnd.is_null() { eprintln!("Cannot create launcher window"); return; }
            let font = CreateFontW(-px(15), 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, wide("Segoe UI").as_ptr());
            let title_font = CreateFontW(-px(21), 0, 0, 0, 700, 0, 0, 0, 1, 0, 0, 5, 0, wide("Segoe UI Semibold").as_ptr());
            let child = |kind: &str, label: &str, style: u32, id: i32, x: i32, y: i32, w: i32, h: i32| {
                let handle = CreateWindowExW(0, wide(kind).as_ptr(), wide(label).as_ptr(),
                    WS_CHILD | WS_VISIBLE | style, px(x), px(y), px(w), px(h),
                    hwnd, id as isize as HMENU, instance, std::ptr::null());
                SendMessageW(handle, WM_SETFONT, font as WPARAM, 1);
                handle
            };
            let button = |s: &str, id, x, y, w| child("BUTTON", s, WS_TABSTOP | BS_OWNERDRAW as u32, id, x, y, w, 26);
            let dropdown = |id, x, y, w| child("COMBOBOX", "", WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32, id, x, y, w, 200);

            let heading = child("STATIC", "FUTURE COP : L.A.P.D.", 0, 0, 20, 7, 600, 28);
            SendMessageW(heading, WM_SETFONT, title_font as WPARAM, 1);
            let tabs = child("SysTabControl32", "", WS_TABSTOP | TCS_OWNERDRAWFIXED as u32 | TCS_FIXEDWIDTH as u32, TABS, 10, 42, 640, 34);
            SendMessageW(tabs, TCM_SETITEMSIZE, 0, (px(126) as u32 | ((px(30) as u32) << 16)) as isize);
            for (index, title) in TAB_NAMES.iter().enumerate() {
                let mut title = wide(title);
                let item = TCITEMW { mask: TCIF_TEXT, pszText: title.as_mut_ptr(), ..std::mem::zeroed() };
                SendMessageW(tabs, TCM_INSERTITEMW, index, &item as *const TCITEMW as isize);
            }

            child("STATIC", "PC disc image", 0, 200, 20, 90, 200, 22);
            child("EDIT", &saved.iso, WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32, ISO, 20, 115, 490, 25);
            button("Browse...", BROWSE_ISO, 520, 115, 110);
            child("BUTTON", "Use PlayStation menu music", WS_TABSTOP | BS_AUTOCHECKBOX as u32, MUSIC, 20, 155, 380, 24);
            child("EDIT", &saved.music_path, WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32, MUSIC_PATH, 20, 185, 490, 25);
            button("Browse...", BROWSE_MUSIC, 520, 185, 110);
            child("BUTTON", "Modern dialogs (preview; menus remain classic)", WS_TABSTOP | BS_AUTOCHECKBOX as u32, MODERN, 20, 225, 600, 24);
            child("BUTTON", "Skip intro videos", WS_TABSTOP | BS_AUTOCHECKBOX as u32, SKIP_INTRO, 20, 260, 250, 24);
            check(hwnd, MUSIC, saved.music_enabled);
            check(hwnd, MODERN, saved.modern_ui);

            child("STATIC", "Resolution (window or WIDTHxHEIGHT)", 0, 201, 20, 100, 280, 22);
            child("EDIT", "window", WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32, RESOLUTION, 320, 100, 180, 25);
            child("STATIC", "Frame rate limit", 0, 202, 20, 140, 250, 22);
            dropdown(FRAME_RATE, 320, 140, 180);
            combo(hwnd, FRAME_RATE, &["Original (37.5)", "60", "75", "100", "120", "144", "165", "240", "300", "Unlimited"], 0);
            child("STATIC", "Anti-aliasing", 0, 203, 20, 180, 250, 22);
            dropdown(MSAA, 320, 180, 180);
            combo(hwnd, MSAA, &["Off", "2x", "4x", "8x"], 2);
            child("STATIC", "Texture filtering", 0, 204, 20, 220, 250, 22);
            dropdown(FILTER, 320, 220, 180);
            combo(hwnd, FILTER, &["Off", "Trilinear", "2x", "4x", "8x", "16x"], 5);
            child("BUTTON", "V-Sync", WS_TABSTOP | BS_AUTOCHECKBOX as u32, VSYNC, 20, 265, 180, 24);
            child("BUTTON", "Show frame rate", WS_TABSTOP | BS_AUTOCHECKBOX as u32, FPS, 220, 265, 200, 24);
            child("BUTTON", "Fullscreen", WS_TABSTOP | BS_AUTOCHECKBOX as u32, FULLSCREEN, 20, 300, 180, 24);

            child("BUTTON", "Controller vibration", WS_TABSTOP | BS_AUTOCHECKBOX as u32, RUMBLE, 20, 100, 300, 24);
            button("Reset gamepad bindings...", RESET_PAD, 20, 140, 230);
            child("STATIC", "Configure individual bindings in the game.", 0, 205, 20, 180, 400, 22);

            button("Back up files", BACKUP, 20, 100, 145);
            dropdown(BACKUPS, 175, 100, 265);
            button("Restore...", RESTORE, 450, 100, 115);
            button("Open game folder", OPEN_FOLDER, 20, 145, 180);
            button("Read game data...", GAME_DATA, 220, 145, 180);
            child("STATIC", "Close the game before backup or restore.", 0, 206, 20, 190, 500, 22);

            child("STATIC", "Texture and sound tools", 0, 207, 20, 100, 500, 22);
            button("Export textures", EXPORT_TEXTURES, 20, 145, 180);
            button("Import textures", IMPORT_TEXTURES, 220, 145, 180);
            button("Export sounds", EXPORT_SOUNDS, 420, 145, 180);
            child("STATIC", "Files are written to game_files/Textures or game_files/Sounds.", 0, 208, 20, 190, 590, 44);

            button("Reload game settings", RELOAD, 20, 480, 190);
            child("STATIC", "Ready.", 0, STATUS, 20, 515, 610, 54);
            child("BUTTON", "SAVE", WS_TABSTOP | BS_OWNERDRAW as u32, APPLY, 350, 585, 130, 32);
            child("BUTTON", "PLAY", WS_TABSTOP | BS_OWNERDRAW as u32, PLAY, 500, 585, 130, 32);
            refresh_backups(hwnd);
            refresh_game_settings(hwnd);
            show_tab(hwnd, 0);
            let mut animations: i32 = 1;
            if SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, &mut animations as *mut i32 as *mut _, 0) != 0 && animations != 0 {
                SetTimer(hwnd, 1, 80, None);
            }
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);

            if saved.iso.is_empty() {
                let window = hwnd as isize;
                std::thread::spawn(move || {
                    if let Ok((Some(pc), _)) = fcop::setup::discover_launcher_images(&dir) {
                        send(window, DISCOVERED, pc.to_string_lossy().into_owned());
                    }
                });
            }
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if IsDialogMessageW(hwnd, &msg) == 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            DeleteObject(font as HGDIOBJ);
            DeleteObject(title_font as HGDIOBJ);
            DeleteObject(input_brush as HGDIOBJ);
            DeleteObject(brush as HGDIOBJ);
        }
    }
}

#[cfg(windows)]
fn main() {
    windows::run();
}
