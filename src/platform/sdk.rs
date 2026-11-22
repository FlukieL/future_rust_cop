//! NVIDIA's Streamline and DLSS files (PLAN-RTX.md step 4), downloaded on the player's choice,
//! never shipped: NVIDIA's licence for the DLSS DLLs does not let an open source project pass
//! them on (Streamline itself is MIT). The player accepts NVIDIA's licence text first
//! (`licence`), then `fetch` takes only the files needed from NVIDIA's official Streamline
//! release zip on GitHub (276 MB) by HTTP range requests: the zip's central directory from its
//! end, then each entry (~37 MB packed). Each is inflated, checked against its SHA-256 here
//! (version and hashes fixed, updated by hand) and written into `game_files/DLSS/`
//! (`setup::DLSS`, kept there). A DLL replaced later (DLSS Swapper, the NVIDIA App: DLSS 5 by
//! managers) is no error: `present` only asks whether the files are there.
//!
//! Streamline is Windows only, so is this.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::PathBuf;

/// The Streamline release taken.
pub const VERSION: &str = "2.14.16";
const URL: &str = "https://github.com/NVIDIA-RTX/Streamline/releases/download/v2.14.16/streamline-sdk-v2.14.16.zip";

/// NVIDIA's licence for the DLSS DLLs, shown before the download.
pub const LICENCE: &str = "bin/x64/nvngx_dlss.license.txt";

/// The files taken (path in the zip) and their SHA-256.
pub const FILES: &[(&str, &str)] = &[
    ("license.txt", "7b6f23e7d6f3ad6292f9308d2b42cdc3d82ae4e9b2abb55f230279d83bedd43d"),
    (LICENCE, "3027f23ca5a46dd9cb8183fbd522983a86f64d7daac5982912bf9f214671f294"),
    ("bin/x64/reflex.license.txt", "ebf83c07fb3b2939908c3795d887afde3161c89a28ba391724efc784ce1bdabe"),
    ("bin/x64/sl.interposer.dll", "b1d7395aebd7b9722841d64dcf7433680020b6a8e421f851c16d5ba0c2182edf"),
    ("bin/x64/sl.common.dll", "38eea3b4209afc455636ea372b5933d65fcc13becec6c84d101dcfc4d8648f75"),
    ("bin/x64/sl.dlss.dll", "18871eea87c90345da97d8109faa81ea7d5840e8d99b51b55b0e46cfa177f84e"),
    ("bin/x64/sl.dlss_g.dll", "3ed9538df724993bea8a050195a6a78b9c8c8a566d5b05ade477aab3c390e90f"),
    ("bin/x64/sl.reflex.dll", "443577e869352436ff7ec8f4d9c90418d3e95368436ce219261e0f4f07a38122"),
    ("bin/x64/sl.pcl.dll", "5698c38e722318681f215c9fe68e45b9bedd51378acc699d8eee26e961e57770"),
    ("bin/x64/NvLowLatencyVk.dll", "2a77dc3e1c724b7eea5755be0ae7423752e79a2459fae72181a9f00e3507e5d6"),
    ("bin/x64/nvngx_dlss.dll", "d00b5b0ab29c3ca101b70c3a5ecb3a01b7ef33766a3383a28a18623083e6123e"),
    ("bin/x64/nvngx_dlssg.dll", "8d59d49b36410dd6b62fef2503a5871d347415bd85d943fa0848563a95dd50c7"),
];

/// Where the files go: `game_files/DLSS`.
pub fn dir() -> PathBuf {
    crate::setup::root().join(crate::setup::DLSS)
}

/// A file's name in `dir`: the root `license.txt` is Streamline's.
fn name(path: &str) -> &str {
    if path == "license.txt" { "streamline.license.txt" } else { path.rsplit('/').next().unwrap_or(path) }
}

/// Whether every file is there.
pub fn present() -> bool {
    let d = dir();
    FILES.iter().all(|(p, _)| d.join(name(p)).is_file())
}

/// An entry of the zip's central directory.
struct Entry {
    path: String,
    method: u16,
    packed: u64,
    size: u64,
    /// its local header
    at: u64,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().user_agent("fcop").build().into()
}

/// Bytes `a..b` of the zip; `progress(n)` after each piece read (n bytes).
fn range(agent: &ureq::Agent, a: u64, b: u64, progress: &mut dyn FnMut(u64)) -> Result<Vec<u8>, String> {
    let mut r = agent.get(URL).header("Range", format!("bytes={a}-{}", b - 1)).call().map_err(|e| format!("download: {e}"))?;
    if r.status() != 206 {
        return Err(format!("download: the server answered {} to a range request", r.status()));
    }
    let mut body = r.body_mut().with_config().limit(u64::MAX).reader();
    let mut out = Vec::with_capacity((b - a) as usize);
    let mut buf = vec![0; 1 << 16];
    loop {
        let n = body.read(&mut buf).map_err(|e| format!("download: {e}"))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        progress(n as u64);
    }
    if out.len() as u64 != b - a {
        return Err(format!("download: {} bytes instead of {}", out.len(), b - a));
    }
    Ok(out)
}

fn u16_at(d: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([d[i], d[i + 1]])
}

fn u32_at(d: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(d[i..i + 4].try_into().unwrap())
}

/// The zip's size (from a first range answer's Content-Range) and its central directory.
fn directory(agent: &ureq::Agent) -> Result<Vec<Entry>, String> {
    // the size: Content-Range "bytes 0-0/<size>"
    let r = agent.get(URL).header("Range", "bytes=0-0").call().map_err(|e| format!("download: {e}"))?;
    let size: u64 = r
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit('/').next())
        .and_then(|v| v.parse().ok())
        .ok_or("download: the zip's size is not known")?;
    // the end of central directory record: within the last 22 + 65535 bytes
    let from = size.saturating_sub(22 + 65535);
    let tail = range(agent, from, size, &mut |_| {})?;
    let e = (0..=tail.len().saturating_sub(22)).rev().find(|&i| tail[i..i + 4] == *b"PK\x05\x06").ok_or("download: no zip directory")?;
    let (count, cd_size, cd_at) = (u16_at(&tail, e + 10) as usize, u32_at(&tail, e + 12) as u64, u32_at(&tail, e + 16) as u64);
    let cd = if cd_at >= from { tail[(cd_at - from) as usize..][..cd_size as usize].to_vec() } else { range(agent, cd_at, cd_at + cd_size, &mut |_| {})? };
    let mut out = Vec::with_capacity(count);
    let mut i = 0;
    while out.len() < count && i + 46 <= cd.len() && cd[i..i + 4] == *b"PK\x01\x02" {
        let (n, x, c) = (u16_at(&cd, i + 28) as usize, u16_at(&cd, i + 30) as usize, u16_at(&cd, i + 32) as usize);
        out.push(Entry {
            path: String::from_utf8_lossy(&cd[i + 46..i + 46 + n]).into_owned(),
            method: u16_at(&cd, i + 10),
            packed: u32_at(&cd, i + 20) as u64,
            size: u32_at(&cd, i + 24) as u64,
            at: u32_at(&cd, i + 42) as u64,
        });
        i += 46 + n + x + c;
    }
    Ok(out)
}

/// An entry's contents, inflated.
fn contents(agent: &ureq::Agent, e: &Entry, progress: &mut dyn FnMut(u64)) -> Result<Vec<u8>, String> {
    let head = range(agent, e.at, e.at + 30, &mut |_| {})?;
    if head[..4] != *b"PK\x03\x04" {
        return Err(format!("download: {} has no local header", e.path));
    }
    let start = e.at + 30 + u16_at(&head, 26) as u64 + u16_at(&head, 28) as u64;
    let packed = range(agent, start, start + e.packed, progress)?;
    let data = match e.method {
        0 => packed,
        8 => {
            let mut out = Vec::with_capacity(e.size as usize);
            flate2::read::DeflateDecoder::new(&packed[..]).read_to_end(&mut out).map_err(|err| format!("download: {}: {err}", e.path))?;
            out
        }
        m => return Err(format!("download: {} packed by method {m}", e.path)),
    };
    if data.len() as u64 != e.size {
        return Err(format!("download: {} is {} bytes instead of {}", e.path, data.len(), e.size));
    }
    Ok(data)
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// NVIDIA's licence text for the DLSS DLLs (shown before `fetch`), from the zip.
pub fn licence() -> Result<String, String> {
    let agent = agent();
    let dir = directory(&agent)?;
    let e = dir.iter().find(|e| e.path == LICENCE).ok_or_else(|| format!("download: {LICENCE} is not in the zip"))?;
    Ok(String::from_utf8_lossy(&contents(&agent, e, &mut |_| {})?).into_owned())
}

/// Download every file into `dir`; `progress(done, total)` in packed bytes. A file whose hash
/// differs is not written (`check` false: written anyway, its hash logged; for a new version).
pub fn fetch(check: bool, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    let agent = agent();
    let dir_entries = directory(&agent)?;
    let wanted: Vec<(&Entry, &str)> = FILES
        .iter()
        .map(|(p, h)| dir_entries.iter().find(|e| e.path == *p).map(|e| (e, *h)).ok_or_else(|| format!("download: {p} is not in the zip")))
        .collect::<Result<_, _>>()?;
    let total: u64 = wanted.iter().map(|w| w.0.packed).sum();
    let out = dir();
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut done = 0;
    for (e, hash) in wanted {
        let data = contents(&agent, e, &mut |n| {
            done += n;
            progress(done, total);
        })?;
        let got = hex(&Sha256::digest(&data));
        if got != hash {
            if check {
                return Err(format!("download: {} is not the file expected (SHA-256 {got})", e.path));
            }
            super::log!("sdk: {} SHA-256 {got}", e.path);
        }
        let to = out.join(name(&e.path));
        let tmp = to.with_extension("part");
        std::fs::write(&tmp, &data).and_then(|_| std::fs::rename(&tmp, &to)).map_err(|err| format!("{}: {err}", to.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Downloads the files into `game_files/DLSS` (network, ~37 MB): `cargo test --release --lib
    /// sdk -- --ignored --nocapture`; with FCOP_SDK_HASHES=1 their hashes are only printed.
    #[test]
    #[ignore]
    fn download() {
        let check = std::env::var("FCOP_SDK_HASHES").is_err();
        let mut last = 0;
        fetch(check, &mut |d, t| {
            if d * 10 / t != last {
                last = d * 10 / t;
                eprintln!("{}%", last * 10);
            }
        })
        .unwrap();
        assert!(present());
        assert!(licence().unwrap().contains("NVIDIA"));
    }
}
