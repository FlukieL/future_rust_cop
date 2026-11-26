//! The player's game files: the folder `game_files` next to fcop.exe (in a checkout: in the
//! current folder). The player copies the CD, an image of it or an installed game into it (or
//! puts the image next to fcop.exe: what the game needs is copied out of it); at every
//! start we find FCopLAPD.exe, check it is the one version we know, and keep only what the game
//! needs, in this layout (the CD's: the game folder and the videos next to it):
//!
//! ```text
//! game_files/FCopLAPD/   FCopLAPD.exe, febmp.bin, missions/ (and our registry.txt,
//!                        controls.txt, gamepad.txt, save.cfg: settings, key bindings, saves)
//! game_files/Video/      the videos (optional)
//! game_files/Textures/   replacement textures (optional, `platform::hd`)
//! game_files/PS1/        FE.MIS from the PlayStation disc (or an image of the disc in
//!                        game_files): its menu music (optional, `ps1`)
//! ```
//!
//! Everything else in `game_files` (the CD's installer, DirectX, the Mac version's MISSIONS
//! folder...) goes to the recycle bin (Linux: deleted), without asking.

use crate::iso::Iso;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const GAME: &str = "FCopLAPD";
const VIDEO: &str = "Video";
/// replacement textures (`platform::hd`), the player's
const TEXTURES: &str = "Textures";
/// the PlayStation version's menu music (`ps1`)
pub const PS1: &str = "PS1";
/// what stays in the game folder: the game's files and ours
const GAME_FILES: [&str; 12] = ["FCopLAPD.exe", "febmp.bin", "missions", "registry.txt", "controls.txt", "gamepad.txt", "gamepad-secondary.txt", "save.cfg", "autosave.cfg", "autosave.tmp", COMPILED, SAVESTATE];
/// our savestate (`platform::savestate`)
pub const SAVESTATE: &str = "savestate.bin";
/// the game's compiled code, kept for the next start (`jit::compile_cached`)
pub const COMPILED: &str = "compiled.bin";
/// our note in `game_files` (in the release)
const NOTE: &str = "README.txt";

/// The folder with the game files: `game_files` next to fcop.exe, else in the current folder.
/// When there is neither, the one next to fcop.exe (made when a CD image is found there).
pub fn root() -> PathBuf {
    match home().map(|d| d.join("game_files")) {
        Some(d) if d.is_dir() || !Path::new("game_files").is_dir() => d,
        _ => PathBuf::from("game_files"),
    }
}

/// The folder fcop.exe is in, or the app's folder (`app_folder`).
fn home() -> Option<PathBuf> {
    app_folder().or_else(|| std::env::current_exe().ok()?.parent().map(Path::to_path_buf))
}

/// In a macOS app (fcop.app/Contents/MacOS/fcop): ~/Library/Application Support/fcop. The
/// app is one sealed bundle to share; the player's files cannot go into it.
pub fn app_folder() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if !cfg!(target_os = "macos") || !exe.parent()?.ends_with("Contents/MacOS") {
        return None;
    }
    Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/fcop"))
}

/// Set up the game files in `root` and return FCopLAPD.exe's path, or what the player must do.
pub fn game_exe(root: &Path) -> Result<PathBuf, String> {
    let copy_in = if app_folder().is_some() {
        format!(
            "Put an image of your Future Cop: L.A.P.D. CD (.iso or .bin), or a copy of the CD (or your installed game folder), into this folder:\n\n{}\n\nThen start fcop again.",
            root.display()
        )
    } else {
        format!(
            "Put an image of your Future Cop: L.A.P.D. CD (.iso or .bin) next to fcop.exe, or copy the CD (or your installed game folder) into this folder:\n\n{}\n\nThen start fcop.exe again.",
            root.display()
        )
    };
    let game = root.join(GAME);
    let mut exe = find(&game, "FCopLAPD.exe", 0).or_else(|| find(root, "FCopLAPD.exe", 6));
    if exe.is_none() {
        // a CD image in game_files (it goes to the recycle bin afterwards), or next to fcop.exe
        // (it stays there)
        let pc = |iso: &mut Iso| !is_ps1(iso);
        if let Some(image) = find_image(root, 6, pc).or_else(|| home().and_then(|h| find_image(&h, 0, pc))) {
            from_image(&image, root)?;
            exe = find(&game, "FCopLAPD.exe", 0);
        }
    }
    let Some(exe) = exe else {
        return Err(format!("Future Cop's game files were not found.\n\n{copy_in}"));
    };
    check(&exe)?;
    let from = exe.parent().unwrap().to_path_buf();
    if !same(&from, &game) {
        std::fs::create_dir_all(&game).map_err(|e| format!("{}: {e}", game.display()))?;
        for name in GAME_FILES {
            move_in(&from, name, &game.join(name))?;
        }
    }
    for need in ["FCopLAPD.exe", "febmp.bin", "missions"] {
        if named(&game, need).is_none() {
            return Err(format!("{need} is missing next to FCopLAPD.exe in {}.\n\n{copy_in}", from.display()));
        }
    }
    let video = root.join(VIDEO);
    if !video.is_dir() {
        match find(root, "intro.mpg", 6).and_then(|p| p.parent().map(Path::to_path_buf)) {
            Some(v) if !same(&v, &game) => move_into(&v, &video)?,
            _ => eprintln!("setup: the game's videos (the CD's Video folder) were not found; the game runs without them"),
        }
    }
    let keep = ps1_music(root);
    writable(&game);
    writable(&video);
    // what the game does not need
    let mut rest = others(root, &[GAME, VIDEO, TEXTURES, PS1, NOTE]);
    rest.extend(others(&game, &GAME_FILES));
    rest.extend(others(&root.join(PS1), &[crate::ps1::FILE]));
    for p in rest.into_iter().filter(|p| keep.as_ref().is_none_or(|k| !same(p, k))) {
        eprintln!("setup: not needed, removed: {}", p.display());
        if let Err(e) = trash(&p) {
            eprintln!("setup: cannot remove {}: {e}", p.display());
        }
    }
    Ok(game.join(named(&game, "FCopLAPD.exe").unwrap()))
}

/// The PlayStation version's menu music: FE.MIS into `game_files/PS1`, wherever it is found:
/// the PS1 disc's files (anywhere in `root`) or an image of it (in `root`, then removed with
/// the rest, or next to fcop.exe, kept). Optional: what fails is only logged. Returns an image
/// in `root` that could not be read, to be kept.
fn ps1_music(root: &Path) -> Option<PathBuf> {
    let dir = root.join(PS1);
    let to = dir.join(crate::ps1::FILE);
    if let Some(n) = named(&dir, crate::ps1::FILE) {
        // the name read is FE.MIS (Linux: case matters)
        if n != crate::ps1::FILE {
            if let Err(e) = std::fs::rename(dir.join(&n), &to) {
                eprintln!("setup: cannot rename {}: {e}", dir.join(&n).display());
            }
        }
        return None;
    }
    if let Some(p) = find(root, crate::ps1::FILE, 6) {
        if let Err(e) = std::fs::create_dir_all(&dir).map_err(|e| e.to_string()).and_then(|_| move_into(&p, &to)) {
            eprintln!("setup: {e}");
        }
        return None;
    }
    let image = find_image(root, 6, is_ps1).or_else(|| home().and_then(|h| find_image(&h, 0, is_ps1)))?;
    let copied = (|| {
        let mut iso = Iso::open(&image)?;
        let top = iso.root();
        let (_, fe) = iso.find(&top, crate::ps1::FILE, 2)?;
        let mut bytes = Vec::new();
        iso.read(&fe, &mut bytes).ok()?;
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(&to, bytes).ok()
    })();
    match copied {
        Some(()) => {
            eprintln!("setup: the PlayStation menu music taken from {}", image.display());
            None
        }
        None => {
            eprintln!("setup: cannot take {} from {}", crate::ps1::FILE, image.display());
            Some(image)
        }
    }
}

/// Information verified from a PC game image before installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameImageInfo {
    pub version: &'static str,
    pub has_videos: bool,
}

/// Inspect a PC disc image without creating, moving, or deleting any files.
pub fn inspect_game_image(path: &Path) -> Result<GameImageInfo, String> {
    let mut iso = Iso::open(path).ok_or_else(|| format!("Cannot open {} as a disc image", path.display()))?;
    let root = iso.root();
    let (dir, exe) = iso.find(&root, "FCopLAPD.exe", 6)
        .ok_or_else(|| format!("{} does not contain FCopLAPD.exe", path.display()))?;
    let mut bytes = Vec::new();
    iso.read(&exe, &mut bytes).map_err(|e| format!("Cannot read FCopLAPD.exe from {}: {e}", path.display()))?;
    let hash = hex(&Sha256::digest(&bytes));
    let version = crate::ver::known(&hash).ok_or_else(|| unknown(&format!("FCopLAPD.exe on {}", path.display()), &hash))?;
    if !iso.get(&dir, "febmp.bin").is_some_and(|entry| !entry.dir)
        || !iso.get(&dir, "missions").is_some_and(|entry| entry.dir)
    {
        return Err(format!("{} does not contain the required game files", path.display()));
    }
    Ok(GameImageInfo {
        version: version.name,
        has_videos: iso.find(&root, "intro.mpg", 6).is_some(),
    })
}

/// Install a validated PC image into a managed root without overwriting an installation or
/// deleting the source. Partial work is confined to a unique staging directory.
pub fn install_launcher_image(image: &Path, root: &Path) -> Result<PathBuf, String> {
    inspect_game_image(image)?;
    let game = root.join(GAME);
    if game.exists() {
        return Err(format!("{} already exists; the launcher will not overwrite it", game.display()));
    }
    std::fs::create_dir_all(root).map_err(|e| format!("Cannot create {}: {e}", root.display()))?;
    let mut staging = None;
    for n in 0..1000u32 {
        let candidate = root.join(format!(".fcop-install-{}-{n}", std::process::id()));
        match std::fs::create_dir(&candidate) {
            Ok(()) => { staging = Some(candidate); break; }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Cannot create staging directory: {e}")),
        }
    }
    let staging = staging.ok_or_else(|| "Cannot create a unique staging directory".to_string())?;
    let result = (|| {
        let mut iso = Iso::open(image).ok_or_else(|| format!("Cannot open {}", image.display()))?;
        let top = iso.root();
        let (dir, exe) = iso.find(&top, "FCopLAPD.exe", 6).ok_or_else(|| "Game executable not found".to_string())?;
        let bitmap = iso.get(&dir, "febmp.bin").filter(|e| !e.dir).ok_or_else(|| "febmp.bin not found".to_string())?;
        let missions = iso.get(&dir, "missions").filter(|e| e.dir).ok_or_else(|| "missions folder not found".to_string())?;
        let staged_game = staging.join(GAME);
        std::fs::create_dir(&staged_game).map_err(|e| e.to_string())?;
        for (entry, name) in [(&exe, "FCopLAPD.exe"), (&bitmap, "febmp.bin")] {
            let mut bytes = Vec::new();
            iso.read(entry, &mut bytes).map_err(|e| e.to_string())?;
            if bytes.len() != entry.size as usize {
                return Err(format!("{name} is truncated in the image"));
            }
            std::fs::write(staged_game.join(name), bytes).map_err(|e| e.to_string())?;
        }
        // Revalidate what was extracted, not merely the earlier preflight.
        check(&staged_game.join("FCopLAPD.exe"))?;
        iso.extract_new(&missions, &staged_game.join("missions")).map_err(|e| format!("Cannot extract missions: {e}"))?;
        let videos = iso.find(&top, "intro.mpg", 6).map(|(dir, _)| dir);
        if let Some(dir) = videos.filter(|_| !root.join(VIDEO).exists()) {
            iso.extract_new(&dir, &staging.join(VIDEO)).map_err(|e| format!("Cannot extract videos: {e}"))?;
        }
        // Keep a failed video publish from leaving a game that cannot be retried.
        if staging.join(VIDEO).exists() {
            std::fs::rename(staging.join(VIDEO), root.join(VIDEO)).map_err(|e| format!("Cannot install videos: {e}"))?;
        }
        std::fs::rename(&staged_game, &game).map_err(|e| format!("Cannot install game: {e}"))?;
        Ok(game.join("FCopLAPD.exe"))
    })();
    if let Err(e) = std::fs::remove_dir_all(&staging) {
        eprintln!("warn: cannot remove staging directory {}: {e}", staging.display());
    }
    result
}

/// Copy optional PlayStation menu music into the managed root. Existing music is never
/// overwritten and the selected source is only read.
pub fn install_launcher_music(source: &Path, root: &Path) -> Result<PathBuf, String> {
    use std::io::Write;

    let bytes = if source.file_name().is_some_and(|name| name.eq_ignore_ascii_case(crate::ps1::FILE)) {
        std::fs::read(source).map_err(|e| format!("Cannot read {}: {e}", source.display()))?
    } else {
        let mut iso = Iso::open(source).ok_or_else(|| format!("Cannot open {} as a disc image", source.display()))?;
        if !is_ps1(&mut iso) {
            return Err(format!("{} does not contain PlayStation menu music", source.display()));
        }
        let top = iso.root();
        let cw = iso.get(&top, "CW").ok_or_else(|| "PlayStation CW directory not found".to_string())?;
        let entry = iso.get(&cw, crate::ps1::FILE).filter(|entry| !entry.dir)
            .ok_or_else(|| "PlayStation FE.MIS not found".to_string())?;
        let mut bytes = Vec::new();
        iso.read(&entry, &mut bytes).map_err(|e| format!("Cannot read FE.MIS: {e}"))?;
        if bytes.len() != entry.size as usize {
            return Err("The PlayStation music file is truncated".into());
        }
        bytes
    };
    if crate::ps1::menu_music(&bytes).is_none() {
        return Err(format!("{} does not contain playable menu music", source.display()));
    }
    let dir = root.join(PS1);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    let path = dir.join(crate::ps1::FILE);
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)
        .map_err(|e| format!("Cannot create {} (existing music is not overwritten): {e}", path.display()))?;
    if let Err(e) = file.write_all(&bytes).and_then(|_| file.flush()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(format!("Cannot write {}: {e}", path.display()));
    }
    Ok(path)
}

/// Find valid PC and PS1 images in one directory, without changing any files.
/// When multiple images are present, the first valid filename in sorted order is used.
pub fn discover_launcher_images(dir: &Path) -> Result<(Option<PathBuf>, Option<PathBuf>), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("Cannot read {}: {e}", dir.display()))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("Cannot read an entry in {}: {e}", dir.display()))?;
        let ty = entry.file_type().map_err(|e| format!("Cannot inspect {}: {e}", entry.path().display()))?;
        if ty.is_file() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()).is_some_and(|ext| {
                ["iso", "bin", "img"].iter().any(|candidate| ext.eq_ignore_ascii_case(candidate))
            }) {
                paths.push(path);
            }
        }
    }
    paths.sort();
    let mut pc = None;
    let mut ps1 = None;
    for path in paths {
        if pc.is_none() && inspect_game_image(&path).is_ok() {
            pc = Some(path);
        } else if ps1.is_none() && inspect_ps1_music_image(&path).is_ok() {
            ps1 = Some(path);
        }
        if pc.is_some() && ps1.is_some() {
            break;
        }
    }
    Ok((pc, ps1))
}

/// Check for a PlayStation disc with the optional FE.MIS menu music, without changing it.
pub fn inspect_ps1_music_image(path: &Path) -> Result<(), String> {
    let mut iso = Iso::open(path).ok_or_else(|| format!("Cannot open {} as a disc image", path.display()))?;
    if is_ps1(&mut iso) {
        Ok(())
    } else {
        Err(format!("{} does not contain PlayStation menu music", path.display()))
    }
}

/// Whether an image is the PlayStation disc (SYSTEM.CNF at the top, FE.MIS in CW).
fn is_ps1(iso: &mut Iso) -> bool {
    let top = iso.root();
    iso.get(&top, "SYSTEM.CNF").is_some() && iso.get(&top, "CW").is_some_and(|cw| iso.get(&cw, crate::ps1::FILE).is_some())
}

/// The first CD image (.iso, .bin, .img with an ISO 9660 file system) in `dir` or below that
/// `want` takes.
fn find_image(dir: &Path, depth: u32, want: fn(&mut Iso) -> bool) -> Option<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    let image = |p: &Path| {
        let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
        matches!(ext.as_deref(), Some("iso" | "bin" | "img")) && Iso::open(p).is_some_and(|mut i| want(&mut i))
    };
    let mut files = entries.iter().filter(|e| e.file_type().is_ok_and(|t| t.is_file())).map(|e| e.path());
    if let Some(p) = files.find(|p| image(p)) {
        return Some(p);
    }
    if depth == 0 {
        return None;
    }
    entries.iter().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).find_map(|e| find_image(&e.path(), depth - 1, want))
}

/// Take what the game needs from a CD image into `root`'s layout: the videos, the game folder's
/// files, FCopLAPD.exe last (an interrupted start does it again). The image itself goes to the
/// recycle bin with the rest after that.
fn from_image(path: &Path, root: &Path) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("Cannot read the CD image {}: {e}", path.display());
    let mut iso = Iso::open(path).ok_or_else(|| format!("{} is not a CD image", path.display()))?;
    let top = iso.root();
    let Some((dir, exe)) = iso.find(&top, "FCopLAPD.exe", 6) else {
        return Err(format!("{} holds no FCopLAPD.exe: is it the Future Cop: L.A.P.D. CD?", path.display()));
    };
    let mut bytes = Vec::new();
    iso.read(&exe, &mut bytes).map_err(fail)?;
    // what is copied, for the progress shown
    let video_dir = iso.find(&top, "intro.mpg", 6).map(|(v, _)| v);
    let mut total = exe.size as u64;
    for e in [iso.get(&dir, "febmp.bin"), iso.get(&dir, "missions"), video_dir.clone()].into_iter().flatten() {
        total += if e.dir { iso.size(&e).map_err(fail)? } else { e.size as u64 };
    }
    let total = total.max(1);
    iso.copied = 0;
    iso.progress = Some(Box::new(move |n| {
        crate::splash::status(&format!("Copying the game from the CD image... {}%", n * 100 / total))
    }));
    let hash = hex(&Sha256::digest(&bytes));
    if crate::ver::known(&hash).is_none() {
        return Err(unknown(&format!("FCopLAPD.exe on {}", path.display()), &hash));
    }
    eprintln!("setup: taking the game from the CD image {}", path.display());
    let game = root.join(GAME);
    std::fs::create_dir_all(&game).map_err(fail)?;
    let video = root.join(VIDEO);
    if !video.is_dir() {
        if let Some(v) = &video_dir {
            iso.extract(v, &video).map_err(fail)?;
        }
    }
    for name in ["febmp.bin", "missions"] {
        match iso.get(&dir, name) {
            Some(e) if e.dir => iso.extract(&e, &game.join(name)).map_err(fail)?,
            Some(e) => {
                let mut out = std::io::BufWriter::new(std::fs::File::create(game.join(name)).map_err(fail)?);
                iso.read(&e, &mut out).map_err(fail)?;
            }
            None => return Err(format!("{name} is missing next to FCopLAPD.exe on {}.", path.display())),
        }
    }
    std::fs::write(game.join("FCopLAPD.exe"), &bytes).map_err(fail)
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn unknown(what: &str, hash: &str) -> String {
    let known: Vec<String> = crate::ver::VERSIONS.iter().map(|v| format!("{}: {}", v.name, v.sha256)).collect();
    format!(
        "{what} is a version of Future Cop this port does not know (the PC versions with these \
         SHA-256 are supported:\n{}).\n\nIts SHA-256: {hash}",
        known.join("\n")
    )
}

/// Which version of FCopLAPD.exe this is, if we know it.
pub fn check(exe: &Path) -> Result<&'static crate::ver::Version, String> {
    let bytes = std::fs::read(exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    let hash = hex(&Sha256::digest(&bytes));
    crate::ver::known(&hash).ok_or_else(|| unknown(&exe.display().to_string(), &hash))
}

/// The entry of `dir` called `name`, in any case.
fn named(dir: &Path, name: &str) -> Option<std::ffi::OsString> {
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.file_name()).find(|n| n.to_string_lossy().eq_ignore_ascii_case(name))
}

/// The first file called `name` in `dir` or below it, at most `depth` folders down (links are
/// not followed).
fn find(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let mut dirs = Vec::new();
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            dirs.push(e.path());
        } else if t.is_file() && e.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
            return Some(e.path());
        }
    }
    if depth == 0 {
        return None;
    }
    dirs.sort();
    dirs.iter().find_map(|d| find(d, name, depth - 1))
}

fn same(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Move `from`/`name` (any case) to `to`, unless it is not there or `to` already is.
fn move_in(from: &Path, name: &str, to: &Path) -> Result<(), String> {
    match named(from, name) {
        Some(n) if !to.exists() => move_into(&from.join(n), to),
        _ => Ok(()),
    }
}

fn move_into(from: &Path, to: &Path) -> Result<(), String> {
    eprintln!("setup: {} -> {}", from.display(), to.display());
    std::fs::rename(from, to).map_err(|e| format!("Cannot move {} to {}: {e}", from.display(), to.display()))
}

/// The entries of `dir` other than `keep` (any case).
fn others(dir: &Path, keep: &[&str]) -> Vec<PathBuf> {
    let Ok(d) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = d
        .flatten()
        .filter(|e| !keep.iter().any(|k| e.file_name().to_string_lossy().eq_ignore_ascii_case(k)))
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

/// Files copied from a CD are read-only; the game writes into its folder.
fn writable(dir: &Path) {
    let Ok(d) = std::fs::read_dir(dir) else { return };
    for e in d.flatten() {
        let p = e.path();
        if let Ok(m) = std::fs::symlink_metadata(&p) {
            let mut perm = m.permissions();
            if perm.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                perm.set_readonly(false);
                let _ = std::fs::set_permissions(&p, perm);
            }
            if m.is_dir() {
                writable(&p);
            }
        }
    }
}

/// Move a file or folder to the recycle bin (elsewhere: delete it).
#[cfg(windows)]
fn trash(p: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::*;
    // a list of paths, each NUL-terminated, the list ending with another NUL
    let from: Vec<u16> = p.as_os_str().encode_wide().chain([0, 0]).collect();
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16,
        ..Default::default()
    };
    match unsafe { SHFileOperationW(&mut op) } {
        0 if op.fAnyOperationsAborted == 0 => Ok(()),
        e => Err(format!("error {e:#x}")),
    }
}

#[cfg(not(windows))]
fn trash(p: &Path) -> Result<(), String> {
    let r = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
    r.map_err(|e| e.to_string())
}
