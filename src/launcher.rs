//! Launcher configuration, game registry settings, and save backups.
//! Launcher settings are kept beside the executable; game preferences are kept in
//! the game's `registry.txt`. The game should be closed before editing its preferences.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub const SETTINGS_FILE: &str = "fcop-launcher.cfg";
/// Backups go here, next to the launcher (not into `game_files`, which setup tidies).
pub const BACKUP_DIR: &str = "fcop-backups";
/// The files of the game folder a backup holds: saves, save state, settings and bindings.
pub const SAVE_FILES: [&str; 7] = ["save.cfg", "autosave.cfg", "savestate.bin", "registry.txt", "controls.txt", "gamepad.txt", "gamepad-secondary.txt"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub iso: String,
    pub music_enabled: bool,
    pub music_path: String,
    pub modern_ui: bool,
    /// anti-aliasing samples (0, 2, 4, 8)
    pub msaa: Option<u32>,
    /// texture filtering (0, 1, 2, 4, 8, 16)
    pub filtering: Option<u32>,
    pub vsync: Option<bool>,
    pub show_fps: Option<bool>,
    pub vibration: Option<bool>,
    pub swap_pads: Option<bool>,
}

fn flag(v: &str) -> Option<bool> {
    match v {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

fn text(v: Option<bool>) -> Option<&'static str> {
    v.map(|b| if b { "1" } else { "0" })
}

impl Settings {
    /// Read settings from the text of a settings file. Unknown lines and bad values are ignored
    /// (a bad value leaves the default).
    pub fn parse(s: &str) -> Settings {
        let mut r = Settings::default();
        for line in s.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim_end_matches('\r');
            match k.trim() {
                "iso" => r.iso = v.to_string(),
                "music_enabled" => r.music_enabled = flag(v).unwrap_or(false),
                "music_path" => r.music_path = v.to_string(),
                "modern_ui" => r.modern_ui = flag(v).unwrap_or(false),
                "msaa" => r.msaa = v.parse().ok().filter(|n| crate::platform::glide::MSAA_CHOICES.contains(n)),
                "filtering" => r.filtering = v.parse().ok().filter(|n| crate::platform::glide::FILTER_CHOICES.contains(n)),
                "vsync" => r.vsync = flag(v),
                "show_fps" => r.show_fps = flag(v),
                "vibration" => r.vibration = flag(v),
                "swap_pads" => r.swap_pads = flag(v),
                _ => {}
            }
        }
        r
    }

    pub fn to_text(&self) -> String {
        // a value cannot hold a line break
        let line = |s: &str| s.replace(['\r', '\n'], " ");
        let mut out = format!(
            "iso={}\nmusic_enabled={}\nmusic_path={}\nmodern_ui={}\n",
            line(&self.iso),
            self.music_enabled as u8,
            line(&self.music_path),
            self.modern_ui as u8
        );
        let mut opt = |k: &str, v: Option<String>| {
            if let Some(v) = v {
                out.push_str(&format!("{k}={v}\n"));
            }
        };
        opt("msaa", self.msaa.map(|n| n.to_string()));
        opt("filtering", self.filtering.map(|n| n.to_string()));
        opt("vsync", text(self.vsync).map(String::from));
        opt("show_fps", text(self.show_fps).map(String::from));
        opt("vibration", text(self.vibration).map(String::from));
        opt("swap_pads", text(self.swap_pads).map(String::from));
        out
    }

    /// The settings in `dir` (the defaults when there is no file or it cannot be read).
    pub fn load(dir: &Path) -> Settings {
        std::fs::read_to_string(dir.join(SETTINGS_FILE)).map(|s| Settings::parse(&s)).unwrap_or_default()
    }

    /// Write the settings to `dir`: to a temporary file first, so a failure cannot leave half a
    /// file.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let tmp = dir.join(format!("{SETTINGS_FILE}.tmp"));
        std::fs::write(&tmp, self.to_text())?;
        std::fs::rename(&tmp, dir.join(SETTINGS_FILE)).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    /// The environment the game is started with.
    pub fn apply_env(&self, cmd: &mut Command) {
        cmd.env("FCOP_MUSIC", if self.music_enabled { "1" } else { "0" });
        cmd.env("FCOP_MODERN_UI", if self.modern_ui { "1" } else { "0" });
        for name in ["FCOP_MSAA", "FCOP_FILTER", "FCOP_VSYNC", "FCOP_STATS", "FCOP_RUMBLE", "FCOP_SWAP_PADS", "FCOP_SKIP_INTRO", "FCOP_WINDOW"] {
            cmd.env_remove(name);
        }
    }
}

/// Read DWORD values from the game's own root registry key without modifying it.
pub fn game_registry(game: &Path) -> std::io::Result<std::collections::BTreeMap<String, u32>> {
    let path = game.join("registry.txt");
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(e) => return Err(e),
    };
    let mut in_key = false;
    let mut values = std::collections::BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_key = section.eq_ignore_ascii_case(crate::platform::advapi32::FCOP_KEY);
        } else if in_key {
            if let Some((name, value)) = line.split_once('=') {
                if let Some(hex) = value.trim().strip_prefix("dword:") {
                    if let Ok(value) = u32::from_str_radix(hex, 16) {
                        values.insert(name.trim().to_ascii_lowercase(), value);
                    }
                }
            }
        }
    }
    Ok(values)
}

/// Update selected DWORDs in the game's root registry key, preserving unrelated lines and keys.
/// The game must be closed before calling this: it keeps its own in-memory copy.
pub fn write_game_settings(game: &Path, updates: &[(&str, u32)]) -> std::io::Result<()> {
    if !game.is_dir() {
        return Err(io_err(std::io::ErrorKind::NotFound, "game folder not found"));
    }
    let path = game.join("registry.txt");
    let input = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut out = String::new();
    let mut in_root = false;
    let mut inserted = false;
    for line in input.lines() {
        if let Some(section) = line.trim().strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_root = section.eq_ignore_ascii_case(crate::platform::advapi32::FCOP_KEY);
            out.push_str(line);
            out.push('\n');
            if in_root && !inserted {
                for &(key, value) in updates {
                    out.push_str(&format!("{key} = dword:{value:08x}\n"));
                }
                inserted = true;
            }
            continue;
        }
        let replaced = in_root && line.split_once('=').is_some_and(|(name, _)| {
            updates.iter().any(|(key, _)| key.eq_ignore_ascii_case(name.trim()))
        });
        if !replaced {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !inserted {
        out.push_str(&format!("\n[{}]\n", crate::platform::advapi32::FCOP_KEY));
        for &(key, value) in updates {
            out.push_str(&format!("{key} = dword:{value:08x}\n"));
        }
    }
    let tmp = game.join("registry.txt.launcher.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// A read-only report of the game's settings and its save and binding files.
pub fn game_data_report(game: &Path) -> std::io::Result<String> {
    if !game.is_dir() {
        return Err(io_err(std::io::ErrorKind::NotFound, "game folder not found"));
    }
    let values = game_registry(game)?;
    let mut out = format!("Game folder: {}\n\nSaved game settings:\n", game.display());
    for (name, key) in [
        ("Anti-aliasing (MSAA)", "msaa"), ("Texture filtering", "texturefilter"),
        ("V-Sync", "vsync"), ("Show FPS", "showfps"),
        ("Vibration", "controllervibration"), ("Swap controllers", "swapcontrollers"),
    ] {
        match values.get(key) {
            Some(value) => out.push_str(&format!("{name}: {value}\n")),
            None => out.push_str(&format!("{name}: not saved (game default)\n")),
        }
    }
    out.push_str("\nGame files:\n");
    for name in SAVE_FILES {
        match std::fs::metadata(game.join(name)) {
            Ok(m) if m.is_file() => out.push_str(&format!("{name}: {} bytes\n", m.len())),
            Ok(_) => out.push_str(&format!("{name}: not a regular file\n")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => out.push_str(&format!("{name}: not present\n")),
            Err(e) => out.push_str(&format!("{name}: cannot read: {e}\n")),
        }
    }
    out.push_str("\nSave-slot contents are not decoded. Close the game before backup or restore.");
    Ok(out)
}

// ---- saves ---------------------------------------------------------------------------------------

/// Copy `from` to `to` through a temporary file, so `to` is whole or unchanged.
fn copy_atomic(from: &Path, to: &Path) -> std::io::Result<()> {
    let tmp = to.with_extension("fcop-tmp");
    std::fs::copy(from, &tmp)?;
    std::fs::rename(&tmp, to).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn io_err(kind: std::io::ErrorKind, msg: &str) -> std::io::Error {
    std::io::Error::new(kind, msg)
}

/// A backup's name as `backup_saves` makes it: nothing that could leave the backup folder.
pub fn valid_backup_name(name: &str) -> bool {
    name.strip_prefix("backup-").is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '-'))
}

/// Copy the save and settings files of the game folder `game` into a new folder of `root`
/// (`backup-<seconds>`). Existing backups are never touched. Fails when there is nothing to
/// back up.
pub fn backup_saves(game: &Path, root: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(root)?;
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut dir = None;
    for n in 0..1000u32 {
        let name = if n == 0 { format!("backup-{secs}") } else { format!("backup-{secs}-{n}") };
        let p = root.join(name);
        match std::fs::create_dir(&p) {
            Ok(()) => {
                dir = Some(p);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    let dir = dir.ok_or_else(|| io_err(std::io::ErrorKind::Other, "cannot create a backup folder"))?;
    let mut copied = 0;
    for name in SAVE_FILES {
        let src = game.join(name);
        match std::fs::metadata(&src) {
            Ok(metadata) if metadata.is_file() => {
                if let Err(e) = copy_atomic(&src, &dir.join(name)) {
                    let _ = std::fs::remove_dir_all(&dir);
                    return Err(e);
                }
                copied += 1;
            }
            Ok(_) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(io_err(std::io::ErrorKind::InvalidData, "a backup source is not a file"));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        }
    }
    if copied == 0 {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(io_err(std::io::ErrorKind::UnexpectedEof, "there are no save or settings files to back up"));
    }
    Ok(dir)
}

/// The backups in `root`, newest first.
pub fn list_backups(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| valid_backup_name(n))
        .collect();
    let key = |name: &str| {
        let mut parts = name.strip_prefix("backup-").unwrap_or("").split('-');
        let seconds = parts.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        let sequence = parts.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
        (seconds, sequence)
    };
    names.sort_by_key(|name| std::cmp::Reverse(key(name)));
    names
}

/// Put the files of backup `name` (in `root`) back into the game folder `game`. The current
/// files are backed up first, so a restore can be undone. Returns how many files were restored.
pub fn restore_backup(root: &Path, name: &str, game: &Path) -> std::io::Result<usize> {
    if !valid_backup_name(name) {
        return Err(io_err(std::io::ErrorKind::InvalidInput, "not a backup name"));
    }
    let from = root.join(name);
    if !from.is_dir() {
        return Err(io_err(std::io::ErrorKind::NotFound, "the backup does not exist"));
    }
    if !game.is_dir() {
        return Err(io_err(std::io::ErrorKind::NotFound, "the game is not installed"));
    }
    if !SAVE_FILES.iter().any(|f| from.join(f).is_file()) {
        return Err(io_err(std::io::ErrorKind::InvalidData, "the backup holds no files"));
    }
    // nothing to keep is fine; any other error stops the restore
    match backup_saves(game, root) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
        Err(e) => return Err(e),
    }
    let mut restored = 0;
    for f in SAVE_FILES {
        let src = from.join(f);
        if src.is_file() {
            copy_atomic(&src, &game.join(f))?;
            restored += 1;
        }
    }
    Ok(restored)
}

/// Delete the gamepad bindings of the game folder `game` (the game then uses its defaults).
/// The files are backed up into `backups` first. Returns how many files were removed.
pub fn reset_gamepad_bindings(game: &Path, backups: &Path) -> std::io::Result<usize> {
    let files = ["gamepad.txt", "gamepad-secondary.txt"];
    if !files.iter().any(|f| game.join(f).is_file()) {
        return Ok(0);
    }
    backup_saves(game, backups)?;
    let mut removed = 0;
    for f in files {
        let p = game.join(f);
        if p.is_file() {
            std::fs::remove_file(&p)?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let d = std::env::temp_dir().join(format!("fcop-launcher-{tag}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_only_game_root_registry_and_reports_save_files() {
        let dir = temp("game-data");
        std::fs::write(dir.join("registry.txt"), format!(
            "[{}\\Settings]\nMsaa = dword:00000008\n[{}]\nMsaa = dword:00000004\nVSync = dword:00000000\n",
            crate::platform::advapi32::FCOP_KEY, crate::platform::advapi32::FCOP_KEY
        )).unwrap();
        std::fs::write(dir.join("save.cfg"), b"saved").unwrap();
        let values = game_registry(&dir).unwrap();
        assert_eq!(values.get("msaa"), Some(&4));
        assert_eq!(values.get("vsync"), Some(&0));
        let report = game_data_report(&dir).unwrap();
        assert!(report.contains("save.cfg: 5 bytes"));
        assert!(report.contains("autosave.cfg: not present"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn writes_game_settings_without_losing_other_sections_or_values() {
        let dir = temp("game-write");
        let key = crate::platform::advapi32::FCOP_KEY;
        let before = format!("[{key}]\nMsaa = dword:00000002\nGroup = sz:3Dfx\n[{key}\\Settings]\nResolution = dword:00000009\n");
        std::fs::write(dir.join("registry.txt"), before).unwrap();
        write_game_settings(&dir, &[("Msaa", 8), ("FrameRate", 144), ("RenderSize", (1920 << 16) | 1080)]).unwrap();
        let text = std::fs::read_to_string(dir.join("registry.txt")).unwrap();
        assert!(text.contains("Group = sz:3Dfx"));
        assert!(text.contains("Resolution = dword:00000009"));
        let values = game_registry(&dir).unwrap();
        assert_eq!(values.get("msaa"), Some(&8));
        assert_eq!(values.get("framerate"), Some(&144));
        assert_eq!(values.get("rendersize"), Some(&((1920 << 16) | 1080)));
        write_game_settings(&dir, &[("Msaa", 4)]).unwrap();
        assert_eq!(game_registry(&dir).unwrap().get("msaa"), Some(&4));
        let text = std::fs::read_to_string(dir.join("registry.txt")).unwrap();
        assert_eq!(text.lines().filter(|line| *line == format!("[{key}]")).count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn settings_survive_a_save_and_load() {
        let dir = temp("settings");
        let s = Settings {
            iso: r"C:\Games\Future Cop.iso".into(),
            music_enabled: true,
            music_path: "FE.MIS".into(),
            modern_ui: true,
            msaa: Some(8),
            filtering: Some(4),
            vsync: Some(false),
            show_fps: Some(true),
            vibration: None,
            swap_pads: Some(true),
        };
        s.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), s);
        let updated = Settings { iso: "new image.iso".into(), modern_ui: false, ..s };
        updated.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), updated);
        assert!(!dir.join(format!("{SETTINGS_FILE}.tmp")).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_or_bad_settings_give_defaults() {
        let dir = temp("defaults");
        assert_eq!(Settings::load(&dir), Settings::default());
        let s = Settings::parse("msaa=3\nfiltering=x\nvsync=maybe\nmodern_ui=2\nnonsense\nunknown=1\n");
        assert_eq!(s, Settings::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn line_breaks_cannot_split_a_value() {
        let s = Settings { iso: "a\nmodern_ui=1".into(), ..Default::default() };
        assert!(!Settings::parse(&s.to_text()).modern_ui);
    }

    #[test]
    fn env_only_holds_the_overrides_that_are_set() {
        let mut cmd = Command::new("x");
        Settings { msaa: Some(2), ..Default::default() }.apply_env(&mut cmd);
        let vars: Vec<(String, String)> =
            cmd.get_envs().filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string()))).collect();
        assert!(!vars.iter().any(|(k, _)| k == "FCOP_MSAA"));
        assert!(!vars.iter().any(|(k, _)| k == "FCOP_VSYNC" || k == "FCOP_FILTER"));
        assert!(cmd.get_envs().any(|(k, v)| k == "FCOP_VSYNC" && v.is_none()));
        assert!(cmd.get_envs().any(|(k, v)| k == "FCOP_FILTER" && v.is_none()));
    }

    #[test]
    fn backup_and_restore_round_trip_and_keep_the_current_files() {
        let root = temp("saves");
        let game = root.join("game");
        let backups = root.join("backups");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join("save.cfg"), "old").unwrap();
        std::fs::write(game.join("savestate.bin"), "state").unwrap();

        let first = backup_saves(&game, &backups).unwrap();
        assert_eq!(std::fs::read_to_string(first.join("save.cfg")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(first.join("savestate.bin")).unwrap(), "state");

        std::fs::write(game.join("save.cfg"), "new").unwrap();
        let name = first.file_name().unwrap().to_str().unwrap().to_string();
        assert_eq!(restore_backup(&backups, &name, &game).unwrap(), 2);
        assert_eq!(std::fs::read_to_string(game.join("save.cfg")).unwrap(), "old");
        // the files replaced were backed up first
        let all = list_backups(&backups);
        assert_eq!(all.len(), 2);
        let kept = all.iter().find(|n| **n != name).unwrap();
        assert_eq!(std::fs::read_to_string(backups.join(kept).join("save.cfg")).unwrap(), "new");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resetting_gamepad_bindings_backs_them_up_and_keeps_saves() {
        let root = temp("pad");
        let game = root.join("game");
        let backups = root.join("backups");
        std::fs::create_dir_all(&game).unwrap();
        assert_eq!(reset_gamepad_bindings(&game, &backups).unwrap(), 0);
        assert!(list_backups(&backups).is_empty());
        std::fs::write(game.join("gamepad.txt"), "bind").unwrap();
        std::fs::write(game.join("save.cfg"), "save").unwrap();
        assert_eq!(reset_gamepad_bindings(&game, &backups).unwrap(), 1);
        assert!(!game.join("gamepad.txt").exists());
        assert_eq!(std::fs::read_to_string(game.join("save.cfg")).unwrap(), "save");
        let b = list_backups(&backups);
        assert_eq!(b.len(), 1);
        assert_eq!(std::fs::read_to_string(backups.join(&b[0]).join("gamepad.txt")).unwrap(), "bind");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nothing_to_back_up_and_bad_names_are_refused() {
        let root = temp("refuse");
        let game = root.join("game");
        std::fs::create_dir_all(&game).unwrap();
        assert!(backup_saves(&game, &root.join("backups")).is_err());
        assert!(list_backups(&root.join("backups")).is_empty());
        for bad in ["", "backup-", "../backup-1", "backup-1/..", "backup-x", "other"] {
            assert!(!valid_backup_name(bad), "{bad:?}");
            assert!(restore_backup(&root, bad, &game).is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
