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
/// `PreferenceFlags`: eDisableCLStateTracking | eAllowOTA | eLoadDownloadedPlugins (the default),
/// eUseFrameBasedResourceTagging (`slSetTagForFrame`)
const FLAGS: u64 = 1 | 1 << 3 | 1 << 6 | 1 << 7;
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

/// `sl::ViewportHandle` (version 1): our view's number.
#[repr(C)]
struct Viewport {
    head: Header,
    value: u32,
}

const VIEWPORT: Guid = Guid(0x171b6435, 0x9b3c, 0x4fc8, [0x99, 0x94, 0xfb, 0xe5, 0x25, 0x69, 0xaa, 0xa4]);

/// `sl::Boolean`
const FALSE: u8 = 0;
const TRUE: u8 = 1;

/// `sl::Constants` (version 2). Matrices row-major, without jitter.
#[repr(C)]
struct Constants {
    head: Header,
    view_to_clip: [f32; 16],
    clip_to_view: [f32; 16],
    clip_to_lens_clip: [f32; 16],
    clip_to_prev_clip: [f32; 16],
    prev_clip_to_clip: [f32; 16],
    jitter: [f32; 2],
    mvec_scale: [f32; 2],
    pinhole_offset: [f32; 2],
    camera_pos: [f32; 3],
    camera_up: [f32; 3],
    camera_right: [f32; 3],
    camera_fwd: [f32; 3],
    near: f32,
    far: f32,
    fov: f32,
    aspect: f32,
    mvec_invalid: f32,
    depth_inverted: u8,
    camera_motion_included: u8,
    mvec_3d: u8,
    reset: u8,
    orthographic: u8,
    mvec_dilated: u8,
    mvec_jittered: u8,
    min_depth_separation: f32,
}

const CONSTANTS: Guid = Guid(0xdcd35ad7, 0x4e4a, 0x4bad, [0xa9, 0x0c, 0xe0, 0xc4, 0x9e, 0xb2, 0x3a, 0xfe]);

/// `sl::Resource` (version 1), for Vulkan: image, memory, view, layout and the image's description.
#[repr(C)]
struct Resource {
    head: Header,
    kind: u8,
    native: u64,
    memory: u64,
    view: u64,
    state: u32,
    width: u32,
    height: u32,
    format: u32,
    mips: u32,
    layers: u32,
    gpu_address: u64,
    flags: u32,
    usage: u32,
    internal: u16,
    reserved: u16,
}

const RESOURCE: Guid = Guid(0x3a9d70cf, 0x2418, 0x4b72, [0x83, 0x91, 0x13, 0xf8, 0x72, 0x1c, 0x72, 0x61]);

/// `sl::ResourceTag` (version 1).
#[repr(C)]
struct ResourceTag {
    head: Header,
    resource: *const Resource,
    kind: u32,
    lifecycle: u32,
    /// top, left, width, height
    extent: [u32; 4],
}

const RESOURCE_TAG: Guid = Guid(0x4c6a5aad, 0xb445, 0x496c, [0x87, 0xff, 0x1a, 0xf3, 0x84, 0x5b, 0xe6, 0x53]);

/// Buffer types (`kBufferType...`)
const BUFFER_DEPTH: u32 = 0;
const BUFFER_MVEC: u32 = 1;
const BUFFER_COLOUR_IN: u32 = 3;
const BUFFER_COLOUR_OUT: u32 = 4;
/// `ResourceLifecycle::eValidUntilEvaluate`
const VALID_UNTIL_EVALUATE: u32 = 2;

/// `sl::DLSSOptions` (version 3).
#[repr(C)]
struct DlssOptions {
    head: Header,
    mode: u32,
    width: u32,
    height: u32,
    sharpness: f32,
    pre_exposure: f32,
    exposure_scale: f32,
    hdr: u8,
    invert_x: u8,
    invert_y: u8,
    presets: [u32; 6],
    auto_exposure: u8,
    alpha_upscaling: u8,
}

const DLSS_OPTIONS: Guid = Guid(0x6ac826e4, 0x4c61, 0x4101, [0xa9, 0x2d, 0x63, 0x8d, 0x42, 0x10, 0x57, 0xb8]);

/// `sl::DLSSMode`
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum Mode {
    Off = 0,
    Performance = 1,
    Balanced = 2,
    Quality = 3,
    UltraPerformance = 4,
    Dlaa = 6,
}

type Init = unsafe extern "C" fn(*const Preferences, u64) -> i32;
type IsSupported = unsafe extern "C" fn(u32, *const AdapterInfo) -> i32;
type Shutdown = unsafe extern "C" fn() -> i32;
type NewFrameToken = unsafe extern "C" fn(*mut *const c_void, *const u32) -> i32;
type SetConstants = unsafe extern "C" fn(*const Constants, *const c_void, *const Viewport) -> i32;
type SetTagForFrame = unsafe extern "C" fn(*const c_void, *const Viewport, *const ResourceTag, u32, *mut c_void) -> i32;
type Evaluate = unsafe extern "C" fn(u32, *const c_void, *const *const Header, u32, *mut c_void) -> i32;
type FeatureFunction = unsafe extern "C" fn(u32, *const c_char, *mut *const c_void) -> i32;
type DlssSetOptions = unsafe extern "C" fn(*const Viewport, *const DlssOptions) -> i32;

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
    new_frame_token: NewFrameToken,
    set_constants: SetConstants,
    set_tag_for_frame: SetTagForFrame,
    evaluate: Evaluate,
    feature_function: FeatureFunction,
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
        let new_frame_token: NewFrameToken = std::mem::transmute(sym(b"slGetNewFrameToken\0")?);
        let set_constants: SetConstants = std::mem::transmute(sym(b"slSetConstants\0")?);
        let set_tag_for_frame: SetTagForFrame = std::mem::transmute(sym(b"slSetTagForFrame\0")?);
        let evaluate: Evaluate = std::mem::transmute(sym(b"slEvaluateFeature\0")?);
        let feature_function: FeatureFunction = std::mem::transmute(sym(b"slGetFeatureFunction\0")?);
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
        Some(Sl { _lib: lib, is_supported, shutdown, new_frame_token, set_constants, set_tag_for_frame, evaluate, feature_function })
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

/// A Vulkan image as Streamline sees it (always in GENERAL layout).
#[derive(Clone, Copy)]
pub struct Image {
    pub image: u64,
    pub memory: u64,
    pub view: u64,
    pub w: u32,
    pub h: u32,
    pub format: u32,
    pub usage: u32,
}

impl Image {
    fn resource(&self) -> Resource {
        Resource {
            head: Header { next: std::ptr::null(), kind: RESOURCE, version: 1 },
            kind: 0,
            native: self.image,
            memory: self.memory,
            view: self.view,
            // VK_IMAGE_LAYOUT_GENERAL
            state: 1,
            width: self.w,
            height: self.h,
            format: self.format,
            mips: 1,
            layers: 1,
            gpu_address: 0,
            flags: 0,
            usage: self.usage,
            internal: 0,
            reserved: 0,
        }
    }
}

/// What DLSS gets for a view besides its images.
pub struct Frame {
    /// the picture's number (one frame token for all its views)
    pub index: u32,
    /// the view's number (Streamline's viewport)
    pub view: u32,
    /// the view's box in the input images: x, y (rows from the images' start), width, height
    pub rect: [u32; 4],
    /// its box in the output image
    pub out: [u32; 4],
    pub mode: Mode,
    /// the subpixel offset in pixels
    pub jitter: [f32; 2],
    /// pixels per unit at z = 1
    pub focal: f32,
    /// depth = near / z
    pub near: f32,
    pub hdr: bool,
    /// nothing to take from the last picture (the first, after a pause)
    pub reset: bool,
}

fn check(what: &str, r: i32) -> Result<(), String> {
    if r == 0 { Ok(()) } else { Err(format!("{what}: {}", result(r))) }
}

/// Row-major 4x4 matrices with row vectors (v * M), as Streamline's own maths.
#[rustfmt::skip]
const IDENTITY: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

/// DLSS for a view, recorded into the Vulkan command buffer `cmd`: `colour` (the lit 3D picture,
/// before bloom and tone mapping), `depth` (near / z, 0 where nothing), `mvec` (pixels from this
/// picture to the last), into `out`.
pub fn evaluate(cmd: *mut c_void, f: &Frame, colour: Image, depth: Image, mvec: Image, out: Image) -> Result<(), String> {
    let Some(Some(sl)) = SL.get() else { return Err("Streamline not started".into()) };
    static SET_OPTIONS: OnceLock<Option<DlssSetOptions>> = OnceLock::new();
    let set_options = *SET_OPTIONS.get_or_init(|| {
        let mut p = std::ptr::null();
        let r = unsafe { (sl.feature_function)(DLSS, c"slDLSSSetOptions".as_ptr(), &mut p) };
        (r == 0 && !p.is_null()).then(|| unsafe { std::mem::transmute::<*const c_void, DlssSetOptions>(p) })
    });
    let set_options = set_options.ok_or("no slDLSSSetOptions")?;
    let viewport = Viewport { head: Header { next: std::ptr::null(), kind: VIEWPORT, version: 1 }, value: f.view };
    // the projection: view space (x right, y up, z ahead) to clip space with the depth inverted
    // (near / z); the game's camera has no far plane
    let (w, h) = (f.rect[2] as f32, f.rect[3] as f32);
    let (sx, sy) = (2.0 * f.focal / w, 2.0 * f.focal / h);
    #[rustfmt::skip]
    let proj = [
        sx, 0.0, 0.0, 0.0,
        0.0, sy, 0.0, 0.0,
        0.0, 0.0, 0.0, 1.0,
        0.0, 0.0, f.near, 0.0,
    ];
    #[rustfmt::skip]
    let inv = [
        1.0 / sx, 0.0, 0.0, 0.0,
        0.0, 1.0 / sy, 0.0, 0.0,
        0.0, 0.0, 0.0, 1.0 / f.near,
        0.0, 0.0, 1.0, 0.0,
    ];
    let bool = |b: bool| if b { TRUE } else { FALSE };
    let c = Constants {
        head: Header { next: std::ptr::null(), kind: CONSTANTS, version: 2 },
        view_to_clip: proj,
        clip_to_view: inv,
        clip_to_lens_clip: IDENTITY,
        // the camera's motion is in the vectors
        clip_to_prev_clip: IDENTITY,
        prev_clip_to_clip: IDENTITY,
        jitter: f.jitter,
        mvec_scale: [1.0 / w, 1.0 / h],
        pinhole_offset: [0.0; 2],
        camera_pos: [0.0; 3],
        camera_up: [0.0, 1.0, 0.0],
        camera_right: [1.0, 0.0, 0.0],
        camera_fwd: [0.0, 0.0, 1.0],
        near: f.near,
        far: 100000.0,
        fov: 2.0 * (h * 0.5 / f.focal).atan(),
        aspect: w / h,
        mvec_invalid: f32::MAX,
        depth_inverted: TRUE,
        camera_motion_included: TRUE,
        mvec_3d: FALSE,
        reset: bool(f.reset),
        orthographic: FALSE,
        mvec_dilated: FALSE,
        mvec_jittered: FALSE,
        min_depth_separation: 40.0,
    };
    let o = DlssOptions {
        head: Header { next: std::ptr::null(), kind: DLSS_OPTIONS, version: 3 },
        mode: f.mode as u32,
        width: f.out[2],
        height: f.out[3],
        sharpness: 0.0,
        pre_exposure: 1.0,
        exposure_scale: 1.0,
        hdr: bool(f.hdr),
        invert_x: FALSE,
        invert_y: FALSE,
        presets: [0; 6],
        auto_exposure: FALSE,
        alpha_upscaling: FALSE,
    };
    let res = [colour.resource(), depth.resource(), mvec.resource(), out.resource()];
    let tag = |r: &Resource, kind, [x, y, w, h]: [u32; 4]| ResourceTag {
        head: Header { next: std::ptr::null(), kind: RESOURCE_TAG, version: 1 },
        resource: r,
        kind,
        lifecycle: VALID_UNTIL_EVALUATE,
        extent: [y, x, w, h],
    };
    let tags = [tag(&res[0], BUFFER_COLOUR_IN, f.rect), tag(&res[1], BUFFER_DEPTH, f.rect), tag(&res[2], BUFFER_MVEC, f.rect), tag(&res[3], BUFFER_COLOUR_OUT, f.out)];
    let inputs = [&viewport.head as *const Header];
    unsafe {
        let mut token = std::ptr::null();
        check("slGetNewFrameToken", (sl.new_frame_token)(&mut token, &f.index))?;
        check("slSetConstants", (sl.set_constants)(&c, token, &viewport))?;
        check("slDLSSSetOptions", set_options(&viewport, &o))?;
        check("slSetTagForFrame", (sl.set_tag_for_frame)(token, &viewport, tags.as_ptr(), tags.len() as u32, cmd))?;
        check("slEvaluateFeature", (sl.evaluate)(DLSS, token, inputs.as_ptr(), inputs.len() as u32, cmd))?;
    }
    Ok(())
}

/// At exit.
pub fn shutdown() {
    if let Some(Some(sl)) = SL.get() {
        unsafe { (sl.shutdown)() };
    }
}
