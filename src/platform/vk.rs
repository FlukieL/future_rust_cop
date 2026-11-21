//! Vulkan presents the window's pictures (PLAN-RTX.md step 1; `FCOP_VULKAN=1`). OpenGL still
//! draws everything, into an image both APIs share (GL_EXT_memory_object, GL_EXT_semaphore);
//! Vulkan copies it into the swapchain image and presents it. The GL context then lives on a
//! hidden window of its own (`gl::Gl::new_window`). Without a usable Vulkan device (no loader,
//! no interop, GL and Vulkan on different GPUs) GL shows the window as before.
//!
//! Pictures go round `SLOTS` shared images. Each picture: GL draws into the next one and
//! signals its `gl_done`; Vulkan waits for that and for the acquired swapchain image, copies
//! (flipped: GL's first row is the bottom), signals the swapchain image's `present_done`,
//! presents. Two pictures in flight (`frames`): before a picture is submitted, the fence of the
//! one two before it is waited for, so when GL draws into a slot again (`SLOTS` = 3 pictures
//! later) Vulkan's copy from it is done, and GL needs no semaphore wait (glWaitSemaphoreEXT
//! blocked ~300 µs a picture). glSignalSemaphoreEXT still blocks until the GPU finished the
//! picture (~350 µs; ~435 fps against GL's ~557 at the first mission's start, 1280x960).

use ash::{khr, vk};
use glow::HasContext;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use winit::window::Window;

/// `FCOP_VULKAN=1`: present through Vulkan.
pub fn wanted() -> bool {
    std::env::var("FCOP_VULKAN").is_ok_and(|v| v == "1")
}

#[cfg(windows)]
const MEM_HANDLE: vk::ExternalMemoryHandleTypeFlags = vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32;
#[cfg(unix)]
const MEM_HANDLE: vk::ExternalMemoryHandleTypeFlags = vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD;
#[cfg(windows)]
const SEM_HANDLE: vk::ExternalSemaphoreHandleTypeFlags = vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_WIN32;
#[cfg(unix)]
const SEM_HANDLE: vk::ExternalSemaphoreHandleTypeFlags = vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD;

// GL_EXT_memory_object, GL_EXT_semaphore and their _win32 / _fd parts
const GL_DEDICATED_MEMORY_OBJECT: u32 = 0x9581;
#[cfg(windows)]
const GL_HANDLE_TYPE: u32 = 0x9587; // GL_HANDLE_TYPE_OPAQUE_WIN32_EXT
#[cfg(unix)]
const GL_HANDLE_TYPE: u32 = 0x9586; // GL_HANDLE_TYPE_OPAQUE_FD_EXT
const GL_LAYOUT_TRANSFER_SRC: u32 = 0x9592;
const GL_NUM_DEVICE_UUIDS: u32 = 0x9596;
const GL_DEVICE_UUID: u32 = 0x9597;

/// The shared object handle as the GL import functions take it.
#[cfg(windows)]
type GlHandle = *mut c_void;
#[cfg(unix)]
type GlHandle = i32;

/// GL's interop functions (glow has none of them).
struct GlFns {
    create_memory_objects: unsafe extern "system" fn(i32, *mut u32),
    delete_memory_objects: unsafe extern "system" fn(i32, *const u32),
    memory_object_parameteriv: unsafe extern "system" fn(u32, u32, *const i32),
    import_memory: unsafe extern "system" fn(u32, u64, u32, GlHandle),
    tex_storage_mem_2d: unsafe extern "system" fn(u32, i32, u32, i32, i32, u32, u64),
    gen_semaphores: unsafe extern "system" fn(i32, *mut u32),
    import_semaphore: unsafe extern "system" fn(u32, u32, GlHandle),
    signal_semaphore: unsafe extern "system" fn(u32, u32, *const u32, u32, *const u32, *const u32),
    get_unsigned_byte_i: unsafe extern "system" fn(u32, u32, *mut u8),
}

impl GlFns {
    fn load(get: &dyn Fn(&CStr) -> *const c_void) -> Result<GlFns, String> {
        macro_rules! f {
            ($n:literal) => {{
                let p = get($n);
                if p.is_null() {
                    return Err(format!("OpenGL has no {}", $n.to_str().unwrap()));
                }
                unsafe { std::mem::transmute::<*const c_void, _>(p) }
            }};
        }
        Ok(GlFns {
            create_memory_objects: f!(c"glCreateMemoryObjectsEXT"),
            delete_memory_objects: f!(c"glDeleteMemoryObjectsEXT"),
            memory_object_parameteriv: f!(c"glMemoryObjectParameterivEXT"),
            #[cfg(windows)]
            import_memory: f!(c"glImportMemoryWin32HandleEXT"),
            #[cfg(unix)]
            import_memory: f!(c"glImportMemoryFdEXT"),
            tex_storage_mem_2d: f!(c"glTexStorageMem2DEXT"),
            gen_semaphores: f!(c"glGenSemaphoresEXT"),
            #[cfg(windows)]
            import_semaphore: f!(c"glImportSemaphoreWin32HandleEXT"),
            #[cfg(unix)]
            import_semaphore: f!(c"glImportSemaphoreFdEXT"),
            signal_semaphore: f!(c"glSignalSemaphoreEXT"),
            get_unsigned_byte_i: f!(c"glGetUnsignedBytei_vEXT"),
        })
    }
}

#[cfg(windows)]
type MemFn = khr::external_memory_win32::Device;
#[cfg(unix)]
type MemFn = khr::external_memory_fd::Device;
#[cfg(windows)]
type SemFn = khr::external_semaphore_win32::Device;
#[cfg(unix)]
type SemFn = khr::external_semaphore_fd::Device;

/// Shared images GL draws into in turn.
const SLOTS: usize = 3;

/// A shared image and the semaphore that hands it from GL to Vulkan.
struct Slot {
    /// GL finished drawing: Vulkan's semaphore and GL's name for it
    gl_done: vk::Semaphore,
    gl_name: u32,
    shared: Option<Shared>,
}

struct Frame {
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    acquired: vk::Semaphore,
}

/// The image GL draws into, as both APIs see it.
struct Shared {
    image: vk::Image,
    memory: vk::DeviceMemory,
    w: u32,
    h: u32,
    gl_memory: u32,
    tex: glow::Texture,
    fbo: glow::Framebuffer,
}

pub struct Vk {
    _entry: ash::Entry,
    instance: ash::Instance,
    surface_fn: khr::surface::Instance,
    surface: vk::SurfaceKHR,
    phys: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    family: u32,
    swapchain_fn: khr::swapchain::Device,
    mem_fn: MemFn,
    swapchain: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    /// signalled by the copy into each swapchain image, waited for by its present
    present_done: Vec<vk::Semaphore>,
    extent: vk::Extent2D,
    /// the swapchain is to be made again before the next picture (resized, V-Sync switched)
    stale: Cell<bool>,
    vsync: Cell<bool>,
    _pool: vk::CommandPool,
    frames: [Frame; 2],
    next: usize,
    slots: Vec<Slot>,
    /// the slot of the picture being drawn
    cur: usize,
    gl: GlFns,
}

/// FCOP_VK_VALIDATE=1: the validation layer's messages to our log.
unsafe extern "system" fn validation_message(
    _: vk::DebugUtilsMessageSeverityFlagsEXT,
    _: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _: *mut c_void,
) -> vk::Bool32 {
    if let Some(m) = unsafe { data.as_ref() }.and_then(|d| unsafe { d.message_as_c_str() }) {
        super::log!("vulkan: {}", m.to_string_lossy());
    }
    vk::FALSE
}

fn err(what: &str) -> impl Fn(vk::Result) -> String + '_ {
    move |e| format!("{what}: {e}")
}

impl Vk {
    /// Vulkan on window `win` for the GL context `gl` (its functions by `get`).
    pub fn new(win: &Window, gl: &glow::Context, get: &dyn Fn(&CStr) -> *const c_void) -> Result<Vk, String> {
        let gl_fns = GlFns::load(get)?;
        unsafe {
            let entry = ash::Entry::load().map_err(|e| format!("no Vulkan loader: {e}"))?;
            let version = entry.try_enumerate_instance_version().ok().flatten().unwrap_or(vk::API_VERSION_1_0);
            if version < vk::API_VERSION_1_1 {
                return Err("Vulkan 1.1 is needed".into());
            }
            let display = win.display_handle().map_err(|e| format!("display handle: {e}"))?.as_raw();
            let window = win.window_handle().map_err(|e| format!("window handle: {e}"))?.as_raw();
            let exts = ash_window::enumerate_required_extensions(display).map_err(err("surface extensions"))?;
            // FCOP_VK_VALIDATE=1: the Khronos validation layer, when installed (its messages logged)
            let validation = c"VK_LAYER_KHRONOS_validation";
            let layers: Vec<*const c_char> = if std::env::var("FCOP_VK_VALIDATE").is_ok_and(|v| v == "1")
                && entry.enumerate_instance_layer_properties().unwrap_or_default().iter().any(|l| l.layer_name_as_c_str() == Ok(validation))
            {
                vec![validation.as_ptr()]
            } else {
                vec![]
            };
            let mut exts = exts.to_vec();
            if !layers.is_empty() {
                exts.push(ash::ext::debug_utils::NAME.as_ptr());
            }
            let exts = &exts[..];
            let app = vk::ApplicationInfo::default().application_name(c"fcop").api_version(version.min(vk::API_VERSION_1_3));
            let instance = entry
                .create_instance(&vk::InstanceCreateInfo::default().application_info(&app).enabled_extension_names(exts).enabled_layer_names(&layers), None)
                .map_err(err("vkCreateInstance"))?;
            if !layers.is_empty() {
                let du = ash::ext::debug_utils::Instance::new(&entry, &instance);
                let info = vk::DebugUtilsMessengerCreateInfoEXT::default()
                    .message_severity(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR)
                    .message_type(vk::DebugUtilsMessageTypeFlagsEXT::GENERAL | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE)
                    .pfn_user_callback(Some(validation_message));
                match du.create_debug_utils_messenger(&info, None) {
                    Ok(_) => super::log!("vulkan: validation on"),
                    Err(e) => super::log!("vulkan: validation messenger: {e}"),
                }
            }
            let surface_fn = khr::surface::Instance::new(&entry, &instance);
            let surface = ash_window::create_surface(&entry, &instance, display, window, None).map_err(err("surface"))?;

            // the GPU the GL context runs on: the shared image must be on it
            let gl_uuid = (gl.get_parameter_i32(GL_NUM_DEVICE_UUIDS) > 0).then(|| {
                let mut u = [0u8; 16];
                (gl_fns.get_unsigned_byte_i)(GL_DEVICE_UUID, 0, u.as_mut_ptr());
                u
            });
            let dev_exts = [
                khr::swapchain::NAME,
                #[cfg(windows)]
                khr::external_memory_win32::NAME,
                #[cfg(windows)]
                khr::external_semaphore_win32::NAME,
                #[cfg(unix)]
                khr::external_memory_fd::NAME,
                #[cfg(unix)]
                khr::external_semaphore_fd::NAME,
            ];
            let mut best: Option<(u32, vk::PhysicalDevice, u32, String)> = None;
            for phys in instance.enumerate_physical_devices().map_err(err("physical devices"))? {
                let mut id = vk::PhysicalDeviceIDProperties::default();
                let mut props = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
                instance.get_physical_device_properties2(phys, &mut props);
                let p = props.properties;
                let name = p.device_name_as_c_str().map_or("?".into(), |n| n.to_string_lossy().into_owned());
                if p.api_version < vk::API_VERSION_1_1 {
                    continue;
                }
                let has = instance.enumerate_device_extension_properties(phys).unwrap_or_default();
                if !dev_exts.iter().all(|e| has.iter().any(|h| h.extension_name_as_c_str() == Ok(*e))) {
                    super::log!("vulkan: {name}: interop or swapchain extensions missing");
                    continue;
                }
                let Some(family) = instance.get_physical_device_queue_family_properties(phys).iter().enumerate().position(|(i, q)| {
                    q.queue_flags.contains(vk::QueueFlags::GRAPHICS) && surface_fn.get_physical_device_surface_support(phys, i as u32, surface).unwrap_or(false)
                }) else {
                    continue;
                };
                let rank = match (gl_uuid, p.device_type) {
                    (Some(u), _) if u == id.device_uuid => 0,
                    (Some(_), _) => continue,
                    (None, vk::PhysicalDeviceType::DISCRETE_GPU) => 1,
                    (None, _) => 2,
                };
                if best.as_ref().is_none_or(|b| rank < b.0) {
                    best = Some((rank, phys, family as u32, name));
                }
            }
            let (_, phys, family, name) = best.ok_or("no Vulkan device that can share images with OpenGL and show the window")?;
            super::log!("vulkan: {name}");

            let prio = [1.0];
            let queues = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&prio)];
            let ext_ptrs: Vec<*const c_char> = dev_exts.iter().map(|e| e.as_ptr()).collect();
            let device = instance
                .create_device(phys, &vk::DeviceCreateInfo::default().queue_create_infos(&queues).enabled_extension_names(&ext_ptrs), None)
                .map_err(err("vkCreateDevice"))?;
            let queue = device.get_device_queue(family, 0);
            let swapchain_fn = khr::swapchain::Device::new(&instance, &device);
            let mem_fn = MemFn::new(&instance, &device);
            let sem_fn = SemFn::new(&instance, &device);

            let pool = device
                .create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(family).flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER), None)
                .map_err(err("command pool"))?;
            let cmds = device
                .allocate_command_buffers(&vk::CommandBufferAllocateInfo::default().command_pool(pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(2))
                .map_err(err("command buffers"))?;
            let frame = |cmd| -> Result<Frame, String> {
                Ok(Frame {
                    cmd,
                    fence: device.create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None).map_err(err("fence"))?,
                    acquired: device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).map_err(err("semaphore"))?,
                })
            };
            let frames = [frame(cmds[0])?, frame(cmds[1])?];

            // the two semaphores GL and Vulkan share
            let shared_semaphore = || -> Result<(vk::Semaphore, u32), String> {
                let mut export = vk::ExportSemaphoreCreateInfo::default().handle_types(SEM_HANDLE);
                let s = device.create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut export), None).map_err(err("shared semaphore"))?;
                let mut name = 0;
                (gl_fns.gen_semaphores)(1, &mut name);
                #[cfg(windows)]
                {
                    let h = sem_fn
                        .get_semaphore_win32_handle(&vk::SemaphoreGetWin32HandleInfoKHR::default().semaphore(s).handle_type(SEM_HANDLE))
                        .map_err(err("semaphore handle"))?;
                    // left open: NVIDIA's GL takes an object by its handle only when it first uses
                    // it (a memory handle closed after the import gave GL memory of its own: a
                    // black window), and these live as long as the process
                    (gl_fns.import_semaphore)(name, GL_HANDLE_TYPE, h as GlHandle);
                }
                #[cfg(unix)]
                {
                    // GL owns an imported fd
                    let fd = sem_fn.get_semaphore_fd(&vk::SemaphoreGetFdInfoKHR::default().semaphore(s).handle_type(SEM_HANDLE)).map_err(err("semaphore fd"))?;
                    (gl_fns.import_semaphore)(name, GL_HANDLE_TYPE, fd);
                }
                Ok((s, name))
            };
            let mut slots = vec![];
            for _ in 0..SLOTS {
                let (gl_done, gl_name) = shared_semaphore()?;
                slots.push(Slot { gl_done, gl_name, shared: None });
            }
            let mut v = Vk {
                _entry: entry,
                instance,
                surface_fn,
                surface,
                phys,
                device,
                queue,
                family,
                swapchain_fn,
                mem_fn,
                swapchain: vk::SwapchainKHR::null(),
                images: vec![],
                present_done: vec![],
                extent: vk::Extent2D::default(),
                stale: Cell::new(true),
                vsync: Cell::new(super::fps::vsync()),
                _pool: pool,
                frames,
                next: 0,
                slots,
                cur: 0,
                gl: gl_fns,
            };
            let size = win.inner_size();
            v.build_swapchain(size.width, size.height)?;
            Ok(v)
        }
    }

    /// The swapchain made (again) for a window of w x h; an empty extent while minimized.
    fn build_swapchain(&mut self, w: u32, h: u32) -> Result<(), String> {
        unsafe {
            let _ = self.device.device_wait_idle();
            let caps = self.surface_fn.get_physical_device_surface_capabilities(self.phys, self.surface).map_err(err("surface capabilities"))?;
            let extent = if caps.current_extent.width != u32::MAX {
                caps.current_extent
            } else {
                vk::Extent2D {
                    width: w.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                    height: h.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
                }
            };
            self.stale.set(false);
            for s in self.present_done.drain(..) {
                self.device.destroy_semaphore(s, None);
            }
            if extent.width == 0 || extent.height == 0 {
                // minimized: nothing to show until it is restored
                self.extent = extent;
                return Ok(());
            }
            let formats = self.surface_fn.get_physical_device_surface_formats(self.phys, self.surface).map_err(err("surface formats"))?;
            // UNORM as GL's window (the shown gamma is in the picture already)
            let format = formats
                .iter()
                .find(|f| f.format == vk::Format::B8G8R8A8_UNORM)
                .or_else(|| formats.iter().find(|f| f.format == vk::Format::R8G8B8A8_UNORM))
                .or(formats.first())
                .copied()
                .ok_or("the surface has no formats")?;
            let modes = self.surface_fn.get_physical_device_surface_present_modes(self.phys, self.surface).map_err(err("present modes"))?;
            let mode = if self.vsync.get() {
                vk::PresentModeKHR::FIFO
            } else {
                [vk::PresentModeKHR::IMMEDIATE, vk::PresentModeKHR::MAILBOX].into_iter().find(|m| modes.contains(m)).unwrap_or(vk::PresentModeKHR::FIFO)
            };
            if !caps.supported_usage_flags.contains(vk::ImageUsageFlags::TRANSFER_DST) {
                return Err("swapchain images cannot be copied into".into());
            }
            let count = if caps.max_image_count == 0 { caps.min_image_count + 1 } else { (caps.min_image_count + 1).min(caps.max_image_count) };
            let alpha = [vk::CompositeAlphaFlagsKHR::OPAQUE, vk::CompositeAlphaFlagsKHR::INHERIT]
                .into_iter()
                .find(|a| caps.supported_composite_alpha.contains(*a))
                .unwrap_or(vk::CompositeAlphaFlagsKHR::from_raw(caps.supported_composite_alpha.as_raw() & caps.supported_composite_alpha.as_raw().wrapping_neg()));
            let old = self.swapchain;
            let info = vk::SwapchainCreateInfoKHR::default()
                .surface(self.surface)
                .min_image_count(count)
                .image_format(format.format)
                .image_color_space(format.color_space)
                .image_extent(extent)
                .image_array_layers(1)
                .image_usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::COLOR_ATTACHMENT)
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                .pre_transform(caps.current_transform)
                .composite_alpha(alpha)
                .present_mode(mode)
                .clipped(true)
                .old_swapchain(old);
            self.swapchain = self.swapchain_fn.create_swapchain(&info, None).map_err(err("swapchain"))?;
            if old != vk::SwapchainKHR::null() {
                self.swapchain_fn.destroy_swapchain(old, None);
            }
            self.images = self.swapchain_fn.get_swapchain_images(self.swapchain).map_err(err("swapchain images"))?;
            for _ in &self.images {
                self.present_done.push(self.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).map_err(err("semaphore"))?);
            }
            self.extent = extent;
            super::log!("vulkan: swapchain {}x{}, {} images, {:?}, {:?}", extent.width, extent.height, self.images.len(), format.format, mode);
            Ok(())
        }
    }

    /// Slot `k`'s shared image made (again) at w x h.
    fn make_shared(&mut self, k: usize, gl: &glow::Context, w: u32, h: u32) -> Result<(), String> {
        unsafe {
            gl.finish();
            let _ = self.device.device_wait_idle();
            if let Some(s) = self.slots[k].shared.take() {
                gl.delete_framebuffer(s.fbo);
                gl.delete_texture(s.tex);
                (self.gl.delete_memory_objects)(1, &s.gl_memory);
                self.device.destroy_image(s.image, None);
                self.device.free_memory(s.memory, None);
            }
            let mut ext = vk::ExternalMemoryImageCreateInfo::default().handle_types(MEM_HANDLE);
            let info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
                .extent(vk::Extent3D { width: w, height: h, depth: 1 })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .push_next(&mut ext);
            let image = self.device.create_image(&info, None).map_err(err("shared image"))?;
            let req = self.device.get_image_memory_requirements(image);
            let types = self.instance.get_physical_device_memory_properties(self.phys);
            let kind = (0..types.memory_type_count)
                .find(|&i| req.memory_type_bits & (1 << i) != 0 && types.memory_types[i as usize].property_flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL))
                .ok_or("no device memory for the shared image")?;
            let mut export = vk::ExportMemoryAllocateInfo::default().handle_types(MEM_HANDLE);
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let alloc = vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(kind).push_next(&mut export).push_next(&mut dedicated);
            let memory = self.device.allocate_memory(&alloc, None).map_err(err("shared memory"))?;
            self.device.bind_image_memory(image, memory, 0).map_err(err("bind shared memory"))?;

            #[cfg(windows)]
            let handle;
            let mut gl_memory = 0;
            (self.gl.create_memory_objects)(1, &mut gl_memory);
            (self.gl.memory_object_parameteriv)(gl_memory, GL_DEDICATED_MEMORY_OBJECT, &1);
            #[cfg(windows)]
            {
                let h = self
                    .mem_fn
                    .get_memory_win32_handle(&vk::MemoryGetWin32HandleInfoKHR::default().memory(memory).handle_type(MEM_HANDLE))
                    .map_err(err("memory handle"))?;
                (self.gl.import_memory)(gl_memory, req.size, GL_HANDLE_TYPE, h as GlHandle);
                handle = h;
            }
            #[cfg(unix)]
            {
                let fd = self.mem_fn.get_memory_fd(&vk::MemoryGetFdInfoKHR::default().memory(memory).handle_type(MEM_HANDLE)).map_err(err("memory fd"))?;
                (self.gl.import_memory)(gl_memory, req.size, GL_HANDLE_TYPE, fd);
            }
            let tex = gl.create_texture().map_err(|e| format!("GL texture: {e}"))?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            (self.gl.tex_storage_mem_2d)(glow::TEXTURE_2D, 1, glow::RGBA8, w as i32, h as i32, gl_memory, 0);
            // NVIDIA's GL takes the memory by the handle only here: closed after the import, GL
            // drew into memory of its own (black window, ~40 fps)
            #[cfg(windows)]
            windows_sys::Win32::Foundation::CloseHandle(handle as _);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
            let fbo = gl.create_framebuffer().map_err(|e| format!("GL framebuffer: {e}"))?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            if status != glow::FRAMEBUFFER_COMPLETE {
                return Err(format!("the shared image is no complete GL framebuffer ({status:#x})"));
            }
            self.slots[k].shared = Some(Shared { image, memory, w, h, gl_memory, tex, fbo });
            Ok(())
        }
    }

    /// Before GL draws a picture of w x h: the framebuffer to draw it into.
    pub fn begin(&mut self, gl: &glow::Context, w: u32, h: u32) -> Option<glow::Framebuffer> {
        self.cur = (self.cur + 1) % SLOTS;
        let k = self.cur;
        if self.slots[k].shared.as_ref().is_none_or(|s| (s.w, s.h) != (w, h)) {
            if let Err(e) = self.make_shared(k, gl, w, h) {
                super::log!("error: vulkan: {e}");
                return None;
            }
        }
        self.slots[k].shared.as_ref().map(|s| s.fbo)
    }

    /// After GL drew the picture: Vulkan copies it into the swapchain and presents it.
    pub fn end(&mut self, gl: &glow::Context) {
        let k = self.cur;
        let Some(s) = &self.slots[k].shared else { return };
        let (gl_done, gl_done_name) = (self.slots[k].gl_done, self.slots[k].gl_name);
        let (sw, sh, simage, tex) = (s.w, s.h, s.image, s.tex.0.get());
        unsafe {
            (self.gl.signal_semaphore)(gl_done_name, 0, std::ptr::null(), 1, &tex, &GL_LAYOUT_TRANSFER_SRC);
            gl.flush();
            let f = &self.frames[self.next];
            let (cmd, fence, acquired) = (f.cmd, f.fence, f.acquired);
            self.next ^= 1;
            let _ = self.device.wait_for_fences(&[fence], true, u64::MAX);
            if self.stale.get() {
                if let Err(e) = self.build_swapchain(sw, sh) {
                    super::log!("error: vulkan: {e}");
                }
            }
            let mut img = None;
            if self.extent.width > 0 && self.extent.height > 0 {
                match self.swapchain_fn.acquire_next_image(self.swapchain, u64::MAX, acquired, vk::Fence::null()) {
                    Ok((i, suboptimal)) => {
                        self.stale.set(self.stale.get() || suboptimal);
                        img = Some(i as usize);
                    }
                    Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.stale.set(true),
                    Err(e) => super::log!("error: vulkan: acquire: {e}"),
                }
            }
            let _ = self.device.reset_fences(&[fence]);
            let d = &self.device;
            let _ = d.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT));
            let range = vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1);
            let barrier = |image, old, new, src, dst, from, to| {
                vk::ImageMemoryBarrier::default()
                    .image(image)
                    .old_layout(old)
                    .new_layout(new)
                    .src_access_mask(src)
                    .dst_access_mask(dst)
                    .src_queue_family_index(from)
                    .dst_queue_family_index(to)
                    .subresource_range(range)
            };
            let ext = vk::QUEUE_FAMILY_EXTERNAL;
            let ign = vk::QUEUE_FAMILY_IGNORED;
            let src_layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;
            // the shared image comes from GL in the layout GL signalled
            let mut take = vec![barrier(simage, src_layout, src_layout, vk::AccessFlags::empty(), vk::AccessFlags::TRANSFER_READ, ext, self.family)];
            if let Some(i) = img {
                take.push(barrier(self.images[i], vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::AccessFlags::empty(), vk::AccessFlags::TRANSFER_WRITE, ign, ign));
            }
            d.cmd_pipeline_barrier(cmd, vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[], &take);
            if let Some(i) = img {
                let layers = vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1);
                let o = |x: u32, y: u32, z| vk::Offset3D { x: x as i32, y: y as i32, z };
                // the first row of GL's picture is its bottom
                let blit = vk::ImageBlit::default()
                    .src_subresource(layers)
                    .src_offsets([o(0, sh, 0), o(sw, 0, 1)])
                    .dst_subresource(layers)
                    .dst_offsets([o(0, 0, 0), o(self.extent.width, self.extent.height, 1)]);
                d.cmd_blit_image(cmd, simage, src_layout, self.images[i], vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[blit], vk::Filter::LINEAR);
            }
            let mut give = vec![barrier(simage, src_layout, src_layout, vk::AccessFlags::TRANSFER_READ, vk::AccessFlags::empty(), self.family, ext)];
            if let Some(i) = img {
                give.push(barrier(self.images[i], vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::PRESENT_SRC_KHR, vk::AccessFlags::TRANSFER_WRITE, vk::AccessFlags::empty(), ign, ign));
            }
            d.cmd_pipeline_barrier(cmd, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::BOTTOM_OF_PIPE, vk::DependencyFlags::empty(), &[], &[], &give);
            let _ = d.end_command_buffer(cmd);

            let mut waits = vec![gl_done];
            let mut signals = vec![];
            if let Some(i) = img {
                waits.push(acquired);
                signals.push(self.present_done[i]);
            }
            let stages = vec![vk::PipelineStageFlags::TRANSFER; waits.len()];
            let cmds = [cmd];
            let submit = vk::SubmitInfo::default().wait_semaphores(&waits).wait_dst_stage_mask(&stages).command_buffers(&cmds).signal_semaphores(&signals);
            if let Err(e) = d.queue_submit(self.queue, &[submit], fence) {
                super::log!("error: vulkan: submit: {e}");
                return;
            }
            if let Some(i) = img {
                let wait = [self.present_done[i]];
                let chains = [self.swapchain];
                let index = [i as u32];
                match self.swapchain_fn.queue_present(self.queue, &vk::PresentInfoKHR::default().wait_semaphores(&wait).swapchains(&chains).image_indices(&index)) {
                    Ok(false) => {}
                    Ok(true) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.stale.set(true),
                    Err(e) => super::log!("error: vulkan: present: {e}"),
                }
            }
        }
    }

    /// The window changed size: a new swapchain before the next picture.
    pub fn resized(&self) {
        self.stale.set(true);
    }

    /// V-Sync: FIFO, else IMMEDIATE (or MAILBOX).
    pub fn set_vsync(&self, on: bool) {
        if self.vsync.replace(on) != on {
            self.stale.set(true);
        }
    }
}
