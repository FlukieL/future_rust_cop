//! NVIDIA Streamline (PLAN-RTX.md step 4): DLSS through its `sl.interposer.dll`, downloaded on the
//! player's choice (`sdk.rs`). With Vulkan the interposer is loaded in place of `vulkan-1.dll`
//! (`vk.rs`): its vkCreateInstance / vkCreateDevice add what the features need. `slInit` must run
//! before that (`init`).
//!
//! The declarations below follow Streamline's headers (MIT, NVIDIA): its structures begin with
//! a header (next in chain, a GUID naming the structure, its version), members only ever added
//! at the end.
//!
//! The interposer's signature is checked by Windows (WinVerifyTrust) before it is loaded; it
//! checks its plugins itself. On with `FCOP_DLSS=1` (decided at start; the setting in Advanced
//! Graphics comes later).

use std::ffi::{CStr, c_char, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The Streamline version the declarations follow (major << 48 | minor << 32 | patch << 16 |
/// 0xfedc), `sdk::VERSION`.
const SDK_VERSION: u64 = (2 << 48) | (14 << 32) | (16 << 16) | 0xfedc;

/// Features (`sl::Feature`).
pub const DLSS: u32 = 0;

/// Our project's id for NGX (a custom engine needs one; any GUID of our own).
const PROJECT_ID: &CStr = c"6b3e9f2a-41d7-4c8e-b5a0-2f9d7c1e8a34";

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid(u32, u16, u16, [u8; 8]);

/// Every structure's beginning: next in the chain, its type, its version.
#[repr(C)]
struct Header {
    next: *const c_void,
    kind: Guid,
    version: usize,
}

/// `sl::Preferences` (version 1).
#[repr(C)]
struct Preferences {
    head: Header,
    show_console: bool,
    log_level: u32,
    plugin_paths: *const *const u16,
    plugin_path_count: u32,
    logs_path: *const u16,
    allocate: *const c_void,
    release: *const c_void,
    log: Option<extern "C" fn(u32, *const c_char)>,
    flags: u64,
    features: *const u32,
    feature_count: u32,
    application_id: u32,
    engine: u32,
    engine_version: *const c_char,
    project_id: *const c_char,
    render_api: u32,
}

const PREFERENCES: Guid = Guid(0x1ca10965, 0xbf8e, 0x432b, [0x8d, 0xa1, 0x67, 0x16, 0xd8, 0x79, 0xfb, 0x14]);
/// `PreferenceFlags`: eDisableCLStateTracking | eAllowOTA | eLoadDownloadedPlugins (the default)
const FLAGS: u64 = 1 | 1 << 3 | 1 << 6;
/// `LogLevel::eDefault`, `EngineType::eCustom`, `RenderAPI::eVulkan`
const LOG_DEFAULT: u32 = 1;
const ENGINE_CUSTOM: u32 = 0;
const RENDER_VULKAN: u32 = 2;

/// `sl::AdapterInfo` (version 1).
#[repr(C)]
struct AdapterInfo {
    head: Header,
    luid: *const u8,
    luid_size: u32,
    vk_physical_device: *const c_void,
}

const ADAPTER_INFO: Guid = Guid(0x0677315f, 0xa746, 0x4492, [0x9f, 0x42, 0xcb, 0x61, 0x42, 0xc9, 0xc3, 0xd4]);

type Init = unsafe extern "C" fn(*const Preferences, u64) -> i32;
type IsSupported = unsafe extern "C" fn(u32, *const AdapterInfo) -> i32;
type Shutdown = unsafe extern "C" fn() -> i32;

/// `sl::Result` by number.
fn result(r: i32) -> &'static str {
    const NAMES: [&str; 40] = [
        "ok",
        "IO",
        "driver out of date",
        "OS out of date",
        "OS disabled hardware scheduling",
        "device not created",
        "no supported adapter found",
        "adapter not supported",
        "no plugins",
        "Vulkan API",
        "DXGI API",
        "D3D API",
        "NRD API",
        "NVAPI",
        "Reflex API",
        "NGX failed",
        "JSON parsing",
        "missing proxy",
        "missing resource state",
        "invalid integration",
        "missing input parameter",
        "not initialized",
        "compute failed",
        "init not called",
        "exception handler",
        "invalid parameter",
        "missing constants",
        "duplicated constants",
        "missing or invalid API",
        "common constants missing",
        "unsupported interface",
        "feature missing",
        "feature not supported",
        "feature missing hooks",
        "feature failed to load",
        "feature wrong priority",
        "feature missing dependency",
        "feature manager invalid state",
        "invalid state",
        "out of video memory (warning)",
    ];
    NAMES.get(r as usize).copied().unwrap_or("?")
}

struct Sl {
    _lib: libloading::Library,
    is_supported: IsSupported,
    shutdown: Shutdown,
}

// the functions are plain C entry points of a library loaded for good
unsafe impl Send for Sl {}
unsafe impl Sync for Sl {}

static SL: OnceLock<Option<Sl>> = OnceLock::new();

/// DLSS is wanted (from the start on).
pub fn wanted() -> bool {
    static W: OnceLock<bool> = OnceLock::new();
    *W.get_or_init(|| super::debug::flag(super::debug::V::Dlss))
}

extern "C" fn log(kind: u32, msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    let text = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    let level = ["", "warning: ", "error: "].get(kind as usize).copied().unwrap_or("");
    super::log!("streamline: {level}{}", text.trim_end());
}

fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain([0]).collect()
}

/// Whether the file carries a valid signature Windows trusts.
fn signed(path: &Path) -> bool {
    use windows_sys::Win32::Security::WinTrust::*;
    let name = wide(path);
    let mut file = WINTRUST_FILE_INFO { cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32, pcwszFilePath: name.as_ptr(), hFile: std::ptr::null_mut(), pgKnownSubject: std::ptr::null_mut() };
    unsafe {
        let mut data: WINTRUST_DATA = std::mem::zeroed();
        data.cbStruct = size_of::<WINTRUST_DATA>() as u32;
        data.dwUIChoice = WTD_UI_NONE;
        data.fdwRevocationChecks = WTD_REVOKE_NONE;
        data.dwUnionChoice = WTD_CHOICE_FILE;
        data.Anonymous.pFile = &mut file;
        data.dwStateAction = WTD_STATEACTION_VERIFY;
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        let r = WinVerifyTrust(std::ptr::null_mut(), &mut action, &mut data as *mut _ as *mut c_void);
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(std::ptr::null_mut(), &mut action, &mut data as *mut _ as *mut c_void);
        r == 0
    }
}

/// Streamline started (`slInit`): the interposer's path, which `vk.rs` loads as the Vulkan
/// loader. None while DLSS is not wanted, its files are missing or Streamline failed (logged).
pub fn init() -> Option<PathBuf> {
    if !wanted() {
        return None;
    }
    if !super::sdk::present() {
        super::log!("streamline: DLSS wanted, its files are not in {}", super::sdk::dir().display());
        return None;
    }
    // a full path, without the `\\?\` canonicalize puts before it: Streamline found its plugins
    // by such a path but took them for unsigned, and NGX did not find DLSS
    let dir = std::fs::canonicalize(super::sdk::dir()).ok()?;
    let dir = PathBuf::from(dir.to_string_lossy().strip_prefix(r"\\?\").map(str::to_owned).unwrap_or_else(|| dir.to_string_lossy().into_owned()));
    let path = dir.join("sl.interposer.dll");
    if !signed(&path) {
        super::log!("streamline: error: {} has no valid signature", path.display());
        return None;
    }
    let sl = unsafe { start(&dir, &path) };
    let ok = sl.is_some();
    let _ = SL.set(sl);
    ok.then_some(path)
}

unsafe fn start(dir: &Path, path: &Path) -> Option<Sl> {
    unsafe {
        let lib = libloading::Library::new(path).map_err(|e| super::log!("streamline: error: {}: {e}", path.display())).ok()?;
        let sym = |name: &[u8]| lib.get::<*const c_void>(name).map(|s| *s).map_err(|e| super::log!("streamline: error: {e}")).ok();
        let init: Init = std::mem::transmute(sym(b"slInit\0")?);
        let is_supported: IsSupported = std::mem::transmute(sym(b"slIsFeatureSupported\0")?);
        let shutdown: Shutdown = std::mem::transmute(sym(b"slShutdown\0")?);
        // kept for good: Streamline may hold on to them
        let plugins: &'static [u16] = Box::leak(wide(dir).into_boxed_slice());
        let paths: &'static [*const u16; 1] = Box::leak(Box::new([plugins.as_ptr()]));
        let features: &'static [u32] = &[DLSS];
        let prefs = Preferences {
            head: Header { next: std::ptr::null(), kind: PREFERENCES, version: 1 },
            show_console: false,
            log_level: LOG_DEFAULT,
            plugin_paths: paths.as_ptr(),
            plugin_path_count: 1,
            logs_path: plugins.as_ptr(),
            allocate: std::ptr::null(),
            release: std::ptr::null(),
            log: Some(log),
            flags: FLAGS,
            features: features.as_ptr(),
            feature_count: features.len() as u32,
            application_id: 0,
            engine: ENGINE_CUSTOM,
            engine_version: c"1".as_ptr(),
            project_id: PROJECT_ID.as_ptr(),
            render_api: RENDER_VULKAN,
        };
        let r = init(&prefs, SDK_VERSION);
        if r != 0 {
            super::log!("streamline: error: slInit: {}", result(r));
            return None;
        }
        super::log!("streamline: started ({})", super::sdk::VERSION);
        Some(Sl { _lib: lib, is_supported, shutdown })
    }
}

/// Whether the Vulkan device `phys` can run DLSS (logged when not).
pub fn supported(phys: *const c_void) -> bool {
    let Some(Some(sl)) = SL.get() else { return false };
    let info = AdapterInfo { head: Header { next: std::ptr::null(), kind: ADAPTER_INFO, version: 1 }, luid: std::ptr::null(), luid_size: 0, vk_physical_device: phys };
    let r = unsafe { (sl.is_supported)(DLSS, &info) };
    if r != 0 {
        super::log!("streamline: DLSS not supported: {}", result(r));
    }
    r == 0
}

/// At exit.
pub fn shutdown() {
    if let Some(Some(sl)) = SL.get() {
        unsafe { (sl.shutdown)() };
    }
}
