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
const GAME_FILES: [&str; 9] = ["FCopLAPD.exe", "febmp.bin", "missions", "registry.txt", "controls.txt", "gamepad.txt", "save.cfg", COMPILED, SAVESTATE];
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
