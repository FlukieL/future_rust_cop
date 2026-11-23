//! The Vulkan renderer's context (PLAN-RTX.md step 6, `FCOP_RENDERER=vulkan`): the window shown
//! by Vulkan alone, no OpenGL. Instance, device, queue and swapchain; two pictures in flight,
//! each with one command buffer that Glide (`vglide.rs`) records into and the composition
//! ends: the shown picture (a 2D frame or Glide's target), the video, the dialog overlay and
//! the stats, letterboxed into an image as large as the window (`window`), blitted into the
//! swapchain image (any format; FCOP_SHOT reads `window` back).
//!
//! Uploads go through a staging ring per picture in flight; images and buffers have memory of
//! their own (there are few). Dynamic rendering: `begin_rendering` / `end_rendering` keep
//! track of the one active, so transfers (uploads, mipmaps) end it first.

use ash::{khr, vk};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::cell::Cell;
use std::ffi::{c_char, c_void};
use winit::window::Window;

const BLIT_VERT: &[u8] = include_bytes!("../../shaders/blit_vert.spv");
const BLIT_FRAG: &[u8] = include_bytes!("../../shaders/blit_frag.spv");

/// The Vulkan renderer instead of OpenGL: "Vulkan" in Preferences' Renderer list (REG_DWORD
/// `Vulkan`, from the next start), `FCOP_RENDERER=vulkan` / `=gl` overrides. Decided once.
pub fn wanted() -> bool {
    static W: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *W.get_or_init(|| match std::env::var("FCOP_RENDERER") {
        Ok(v) if v.eq_ignore_ascii_case("vulkan") => true,
        Ok(v) if v.eq_ignore_ascii_case("gl") => false,
        _ => chosen(),
    })
}

/// The player's choice (REG_DWORD `Vulkan`, default off).
pub fn chosen() -> bool {
    super::advapi32::value("", "Vulkan").and_then(|(_, d)| Some(u32::from_le_bytes(d.get(..4)?.try_into().ok()?))).is_some_and(|v| v != 0)
}

/// Preferences' OK: used from the next start.
pub fn choose(on: bool) {
    super::advapi32::set_value("", "Vulkan", 4, &(on as u32).to_le_bytes());
}

pub fn err(what: &str) -> impl Fn(vk::Result) -> String + '_ {
    move |e| format!("{what}: {e}")
}

/// An image with its own memory and a view of all its levels and layers.
pub struct Img {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
    pub w: u32,
    pub h: u32,
    pub format: vk::Format,
    pub levels: u32,
    pub layers: u32,
    pub samples: vk::SampleCountFlags,
    /// its layout now (as the commands recorded so far leave it)
    pub layout: Cell<vk::ImageLayout>,
}

/// A buffer with its own memory, mapped when host visible.
pub struct Buf {
    pub buf: vk::Buffer,
    pub memory: vk::DeviceMemory,
    pub size: u64,
    pub ptr: *mut u8,
}

/// Staging memory of a picture in flight: chunks, filled from the start again when its fence
/// was waited.
struct Staging {
    chunks: Vec<Buf>,
    /// the chunk and offset of the next upload
    at: (usize, u64),
}

const STAGING_CHUNK: u64 = 16 << 20;

struct Frame {
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    acquired: vk::Semaphore,
    staging: Staging,
    /// objects to destroy once this picture is done
    trash: Vec<Trash>,
}

/// What is destroyed when the picture that last used it is done.
pub enum Trash {
    Img(Img),
    Buf(Buf),
    Pipeline(vk::Pipeline),
    Sampler(vk::Sampler),
}

/// What the composition shows below the video and the overlay.
#[derive(Clone, Copy)]
enum Shown {
    /// the 2D frame (`frame`)
    Frame,
    /// an image someone rendered (Glide's target), its view, size and gamma
    Scene(vk::ImageView, u32, u32, f32),
}

/// The attachments of the rendering active (what `begin_rendering` was given), to tell whether
/// a new one is needed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Attachments {
    pub colour: [vk::ImageView; 2],
    pub depth: vk::ImageView,
    pub w: u32,
    pub h: u32,
}

pub struct Vkr {
    _entry: ash::Entry,
    pub instance: ash::Instance,
    surface_fn: khr::surface::Instance,
    surface: vk::SurfaceKHR,
    pub phys: vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub family: u32,
    swapchain_fn: khr::swapchain::Device,
    pub push_fn: khr::push_descriptor::Device,
    memory: vk::PhysicalDeviceMemoryProperties,
    pub limits: vk::PhysicalDeviceLimits,
    swapchain: vk::SwapchainKHR,
    swap_format: vk::Format,
    images: Vec<vk::Image>,
    present_done: Vec<vk::Semaphore>,
    extent: vk::Extent2D,
    stale: Cell<bool>,
    vsync: Cell<bool>,
    _pool: vk::CommandPool,
    frames: [Frame; 2],
    /// the picture being recorded, and whether its command buffer was begun
    cur: usize,
    recording: bool,
    /// the rendering active in the command buffer
    rendering: Option<Attachments>,
    /// pictures submitted (the next one's number)
    pub pictures: u64,
    // the composition
    blit_layout: vk::PipelineLayout,
    _blit_set: vk::DescriptorSetLayout,
    /// the composition's pipelines: opaque (the picture, the video), blended (overlay, stats)
    blit: [vk::Pipeline; 2],
    pub nearest: vk::Sampler,
    pub linear: vk::Sampler,
    window: Option<Img>,
    frame: Option<Img>,
    frame_size: (u32, u32),
    video: Option<Img>,
    video_size: Option<(u32, u32)>,
    overlay: Option<Img>,
    has_overlay: bool,
    stats: Option<Img>,
    shown: Shown,
    /// FCOP_SHOT's read back
    readback: Option<Buf>,
}

impl Vkr {
    /// Vulkan on the game's window.
    pub fn new(win: &Window) -> Result<Vkr, String> {
        unsafe {
            let entry = ash::Entry::load().map_err(|e| format!("no Vulkan loader: {e}"))?;
            let version = entry.try_enumerate_instance_version().ok().flatten().unwrap_or(vk::API_VERSION_1_0);
            if version < vk::API_VERSION_1_3 {
                return Err("Vulkan 1.3 is needed".into());
            }
            let display = win.display_handle().map_err(|e| format!("display handle: {e}"))?.as_raw();
            let window = win.window_handle().map_err(|e| format!("window handle: {e}"))?.as_raw();
            let mut exts = ash_window::enumerate_required_extensions(display).map_err(err("surface extensions"))?.to_vec();
            let validation = c"VK_LAYER_KHRONOS_validation";
            let layers: Vec<*const c_char> = if std::env::var("FCOP_VK_VALIDATE").is_ok_and(|v| v == "1")
                && entry.enumerate_instance_layer_properties().unwrap_or_default().iter().any(|l| l.layer_name_as_c_str() == Ok(validation))
            {
                vec![validation.as_ptr()]
            } else {
                vec![]
            };
            if !layers.is_empty() {
                exts.push(ash::ext::debug_utils::NAME.as_ptr());
            }
            let app = vk::ApplicationInfo::default().application_name(c"fcop").api_version(vk::API_VERSION_1_3);
            let instance = entry
                .create_instance(&vk::InstanceCreateInfo::default().application_info(&app).enabled_extension_names(&exts).enabled_layer_names(&layers), None)
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

            let dev_exts = [khr::swapchain::NAME, khr::push_descriptor::NAME];
            let mut best: Option<(u32, vk::PhysicalDevice, u32, String)> = None;
            for phys in instance.enumerate_physical_devices().map_err(err("physical devices"))? {
                let p = instance.get_physical_device_properties(phys);
                let name = p.device_name_as_c_str().map_or("?".into(), |n| n.to_string_lossy().into_owned());
                if p.api_version < vk::API_VERSION_1_3 {
                    super::log!("vulkan: {name}: no Vulkan 1.3");
                    continue;
                }
                let has = instance.enumerate_device_extension_properties(phys).unwrap_or_default();
                if !dev_exts.iter().all(|e| has.iter().any(|h| h.extension_name_as_c_str() == Ok(*e))) {
                    super::log!("vulkan: {name}: swapchain or push descriptors missing");
                    continue;
                }
                let Some(family) = instance.get_physical_device_queue_family_properties(phys).iter().enumerate().position(|(i, q)| {
                    q.queue_flags.contains(vk::QueueFlags::GRAPHICS) && surface_fn.get_physical_device_surface_support(phys, i as u32, surface).unwrap_or(false)
                }) else {
                    continue;
                };
                let rank = match p.device_type {
                    vk::PhysicalDeviceType::DISCRETE_GPU => 0,
                    vk::PhysicalDeviceType::INTEGRATED_GPU => 1,
                    _ => 2,
                };
                if best.as_ref().is_none_or(|b| rank < b.0) {
                    best = Some((rank, phys, family as u32, name));
                }
            }
            let (_, phys, family, name) = best.ok_or("no Vulkan 1.3 device that can show the window")?;
            super::log!("vulkan: {name} (the Vulkan renderer)");

            let has = instance.enumerate_device_extension_properties(phys).unwrap_or_default();
            let mut ext_ptrs: Vec<*const c_char> = dev_exts.iter().map(|e| e.as_ptr()).collect();
            let mut f13 = vk::PhysicalDeviceVulkan13Features::default().dynamic_rendering(true);
            let mut f12 = vk::PhysicalDeviceVulkan12Features::default();
            let mut accel = vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default().acceleration_structure(true);
            let mut query = vk::PhysicalDeviceRayQueryFeaturesKHR::default().ray_query(true);
            let supported = instance.get_physical_device_features(phys);
            let features = vk::PhysicalDeviceFeatures::default()
                .sampler_anisotropy(supported.sampler_anisotropy == vk::TRUE)
                .independent_blend(supported.independent_blend == vk::TRUE);
            let prio = [1.0];
            let queues = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&prio)];
            let mut info = vk::DeviceCreateInfo::default().queue_create_infos(&queues).enabled_features(&features).push_next(&mut f13);
            // ray tracing (the sun's rays, later on our images): when the device has it
            let rt = super::raytrace::EXTENSIONS.iter().all(|e| has.iter().any(|h| h.extension_name_as_c_str() == Ok(*e)));
            if rt {
                ext_ptrs.extend(super::raytrace::EXTENSIONS.iter().map(|e| e.as_ptr()));
                f12 = f12.buffer_device_address(true);
                info = info.push_next(&mut accel).push_next(&mut query);
            }
            let info = info.push_next(&mut f12).enabled_extension_names(&ext_ptrs);
            let device = instance.create_device(phys, &info, None).map_err(err("vkCreateDevice"))?;
            let queue = device.get_device_queue(family, 0);
            let swapchain_fn = khr::swapchain::Device::new(&instance, &device);
            let push_fn = khr::push_descriptor::Device::new(&instance, &device);
            let memory = instance.get_physical_device_memory_properties(phys);
            let limits = instance.get_physical_device_properties(phys).limits;

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
                    staging: Staging { chunks: Vec::new(), at: (0, 0) },
                    trash: Vec::new(),
                })
            };
            let frames = [frame(cmds[0])?, frame(cmds[1])?];

            // the composition's pipeline: a sampled picture as a rectangle
            let binding = [vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
            let blit_set = device
                .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().flags(vk::DescriptorSetLayoutCreateFlags::PUSH_DESCRIPTOR_KHR).bindings(&binding), None)
                .map_err(err("descriptor set layout"))?;
            let ranges = [vk::PushConstantRange::default().stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT).size(24)];
            let sets = [blit_set];
            let blit_layout = device.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&sets).push_constant_ranges(&ranges), None).map_err(err("pipeline layout"))?;
            let blend = vk::PipelineColorBlendAttachmentState::default()
                .blend_enable(true)
                .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
                .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ZERO)
                .dst_alpha_blend_factor(vk::BlendFactor::ONE)
                .alpha_blend_op(vk::BlendOp::ADD)
                // the window stays opaque (its alpha is never written)
                .color_write_mask(vk::ColorComponentFlags::R | vk::ColorComponentFlags::G | vk::ColorComponentFlags::B);
            let pipe = |blend: vk::PipelineColorBlendAttachmentState| {
                graphics_pipeline(&device, blit_layout, BLIT_VERT, BLIT_FRAG, &[], &[], vk::PrimitiveTopology::TRIANGLE_STRIP, &[blend], &[vk::Format::R8G8B8A8_UNORM], vk::Format::UNDEFINED, vk::SampleCountFlags::TYPE_1, None)
            };
            let blit = [pipe(blend.blend_enable(false))?, pipe(blend)?];
            let sampler = |f: vk::Filter| {
                device
                    .create_sampler(
                        &vk::SamplerCreateInfo::default()
                            .mag_filter(f)
                            .min_filter(f)
                            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                            .max_lod(0.25),
                        None,
                    )
                    .map_err(err("sampler"))
            };
            let (nearest, linear) = (sampler(vk::Filter::NEAREST)?, sampler(vk::Filter::LINEAR)?);
            let mut v = Vkr {
                _entry: entry,
                instance,
                surface_fn,
                surface,
                phys,
                device,
                queue,
                family,
                swapchain_fn,
                push_fn,
                memory,
                limits,
                swapchain: vk::SwapchainKHR::null(),
                swap_format: vk::Format::UNDEFINED,
                images: vec![],
                present_done: vec![],
                extent: vk::Extent2D::default(),
                stale: Cell::new(true),
                vsync: Cell::new(super::fps::vsync()),
                _pool: pool,
                frames,
                cur: 0,
                recording: false,
                rendering: None,
                pictures: 0,
                blit_layout,
                _blit_set: blit_set,
                blit,
                nearest,
                linear,
                window: None,
                frame: None,
                frame_size: (640, 480),
                video: None,
                video_size: None,
                overlay: None,
                has_overlay: false,
                stats: None,
                shown: Shown::Frame,
                readback: None,
            };
            let size = win.inner_size();
            v.build_swapchain(size.width, size.height)?;
            v.frame(&vec![0; 640 * 480], 640, 480);
            Ok(v)
        }
    }

    fn memory_type(&self, bits: u32, want: vk::MemoryPropertyFlags) -> Result<u32, String> {
        (0..self.memory.memory_type_count)
            .find(|&i| bits & (1 << i) != 0 && self.memory.memory_types[i as usize].property_flags.contains(want))
            .ok_or_else(|| format!("no memory type {want:?}"))
    }

    /// A 2D image (array) in device memory with a view of all of it.
    #[allow(clippy::too_many_arguments)]
    pub fn image(&self, w: u32, h: u32, format: vk::Format, usage: vk::ImageUsageFlags, levels: u32, layers: u32, samples: vk::SampleCountFlags, array: bool) -> Result<Img, String> {
        unsafe {
            let d = &self.device;
            let info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(format)
                .extent(vk::Extent3D { width: w, height: h, depth: 1 })
                .mip_levels(levels)
                .array_layers(layers)
                .samples(samples)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(usage)
                .initial_layout(vk::ImageLayout::UNDEFINED);
            let image = d.create_image(&info, None).map_err(err("image"))?;
            let req = d.get_image_memory_requirements(image);
            let kind = self.memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?;
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let memory = d.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(kind).push_next(&mut dedicated), None).map_err(err("image memory"))?;
            d.bind_image_memory(image, memory, 0).map_err(err("bind image memory"))?;
            let aspect = aspect_of(format);
            let view = d
                .create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
                        .view_type(if array { vk::ImageViewType::TYPE_2D_ARRAY } else { vk::ImageViewType::TYPE_2D })
                        .format(format)
                        .subresource_range(vk::ImageSubresourceRange::default().aspect_mask(aspect).level_count(levels).layer_count(layers)),
                    None,
                )
                .map_err(err("image view"))?;
            Ok(Img { image, memory, view, w, h, format, levels, layers, samples, layout: Cell::new(vk::ImageLayout::UNDEFINED) })
        }
    }

    /// A buffer; host visible (mapped, coherent) or in device memory.
    pub fn buffer(&self, size: u64, usage: vk::BufferUsageFlags, host: bool) -> Result<Buf, String> {
        unsafe {
            let d = &self.device;
            let buf = d.create_buffer(&vk::BufferCreateInfo::default().size(size.max(16)).usage(usage), None).map_err(err("buffer"))?;
            let req = d.get_buffer_memory_requirements(buf);
            let want = if host { vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT } else { vk::MemoryPropertyFlags::DEVICE_LOCAL };
            let kind = self.memory_type(req.memory_type_bits, want)?;
            let memory = d.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(kind), None).map_err(err("buffer memory"))?;
            d.bind_buffer_memory(buf, memory, 0).map_err(err("bind buffer memory"))?;
            let ptr = if host { d.map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()).map_err(err("map"))? as *mut u8 } else { std::ptr::null_mut() };
            Ok(Buf { buf, memory, size, ptr })
        }
    }

    /// Destroyed once the picture being recorded is done.
    pub fn trash(&mut self, t: Trash) {
        self.frames[self.cur].trash.push(t);
    }

    fn destroy(&self, t: Trash) {
        unsafe {
            let d = &self.device;
            match t {
                Trash::Img(i) => {
                    d.destroy_image_view(i.view, None);
                    d.destroy_image(i.image, None);
                    d.free_memory(i.memory, None);
                }
                Trash::Buf(b) => {
                    d.destroy_buffer(b.buf, None);
                    d.free_memory(b.memory, None);
                }
                Trash::Pipeline(p) => d.destroy_pipeline(p, None),
                Trash::Sampler(s) => d.destroy_sampler(s, None),
            }
        }
    }

    /// The command buffer of the picture being recorded (begun at its first use: the fence of
    /// the picture two before is waited for, its staging and trash freed).
    pub fn cmd(&mut self) -> vk::CommandBuffer {
        let f = &mut self.frames[self.cur];
        if !self.recording {
            unsafe {
                let _ = self.device.wait_for_fences(&[f.fence], true, u64::MAX);
                let _ = self.device.reset_fences(&[f.fence]);
                f.staging.at = (0, 0);
                let trash = std::mem::take(&mut f.trash);
                let cmd = f.cmd;
                let _ = self.device.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT));
                self.recording = true;
                for t in trash {
                    self.destroy(t);
                }
                return cmd;
            }
        }
        f.cmd
    }

    /// Rendering into these attachments (begun when another or none is active; contents kept).
    pub fn begin_rendering(&mut self, a: Attachments, layouts: &[&Img]) {
        if self.rendering == Some(a) {
            return;
        }
        self.end_rendering();
        let cmd = self.cmd();
        for img in layouts {
            let to = if aspect_of(img.format).contains(vk::ImageAspectFlags::DEPTH) {
                vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL
            } else {
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL
            };
            self.transition(img, to);
        }
        let colour: Vec<vk::RenderingAttachmentInfo> = a
            .colour
            .iter()
            .filter(|v| **v != vk::ImageView::null())
            .map(|&v| {
                vk::RenderingAttachmentInfo::default()
                    .image_view(v)
                    .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                    .load_op(vk::AttachmentLoadOp::LOAD)
                    .store_op(vk::AttachmentStoreOp::STORE)
            })
            .collect();
        let depth = vk::RenderingAttachmentInfo::default()
            .image_view(a.depth)
            .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::LOAD)
            .store_op(vk::AttachmentStoreOp::STORE);
        let mut info = vk::RenderingInfo::default()
            .render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent: vk::Extent2D { width: a.w, height: a.h } })
            .layer_count(1)
            .color_attachments(&colour);
        if a.depth != vk::ImageView::null() {
            info = info.depth_attachment(&depth);
        }
        unsafe { self.device.cmd_begin_rendering(cmd, &info) };
        self.rendering = Some(a);
    }

    pub fn end_rendering(&mut self) {
        if self.rendering.take().is_some() {
            let cmd = self.frames[self.cur].cmd;
            unsafe { self.device.cmd_end_rendering(cmd) };
        }
    }

    /// Records a layout change of all of `img` (outside rendering), with a full barrier for
    /// what it was and will be used for.
    pub fn transition(&mut self, img: &Img, to: vk::ImageLayout) {
        if img.layout.get() == to {
            return;
        }
        self.end_rendering();
        let cmd = self.cmd();
        let range = vk::ImageSubresourceRange::default().aspect_mask(aspect_of(img.format)).level_count(img.levels).layer_count(img.layers);
        let b = vk::ImageMemoryBarrier::default()
            .image(img.image)
            .old_layout(img.layout.get())
            .new_layout(to)
            .src_access_mask(access_of(img.layout.get()))
            .dst_access_mask(access_of(to))
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .subresource_range(range);
        unsafe { self.device.cmd_pipeline_barrier(cmd, vk::PipelineStageFlags::ALL_COMMANDS, vk::PipelineStageFlags::ALL_COMMANDS, vk::DependencyFlags::empty(), &[], &[], &[b]) };
        img.layout.set(to);
    }

    /// Room for `bytes` in this picture's staging memory: the buffer and the offset.
    fn stage(&mut self, bytes: &[u8]) -> Result<(vk::Buffer, u64), String> {
        // the picture begun first: that frees its staging memory (its fence waited)
        self.cmd();
        let n = bytes.len() as u64;
        let size = STAGING_CHUNK.max(n.next_power_of_two());
        let s = &self.frames[self.cur].staging;
        let (mut c, mut at) = s.at;
        // the next chunk with room, made when there is none
        while c < s.chunks.len() && at + n > s.chunks[c].size {
            c += 1;
            at = 0;
        }
        if c == self.frames[self.cur].staging.chunks.len() {
            let b = self.buffer(size, vk::BufferUsageFlags::TRANSFER_SRC, true)?;
            self.frames[self.cur].staging.chunks.push(b);
        }
        let s = &mut self.frames[self.cur].staging;
        let b = &s.chunks[c];
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), b.ptr.add(at as usize), bytes.len()) };
        // offsets kept at 16 bytes (any texel size)
        s.at = (c, (at + n).next_multiple_of(16));
        Ok((b.buf, at))
    }

    /// Texels into a region of level 0 of `img`'s `layer` (it is then in TRANSFER_DST).
    pub fn upload(&mut self, img: &Img, layer: u32, (x, y, w, h): (u32, u32, u32, u32), bytes: &[u8]) -> Result<(), String> {
        let (buf, at) = self.stage(bytes)?;
        self.transition(img, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
        let cmd = self.cmd();
        let region = vk::BufferImageCopy::default()
            .buffer_offset(at)
            .image_subresource(vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).base_array_layer(layer).layer_count(1))
            .image_offset(vk::Offset3D { x: x as i32, y: y as i32, z: 0 })
            .image_extent(vk::Extent3D { width: w, height: h, depth: 1 });
        unsafe { self.device.cmd_copy_buffer_to_image(cmd, buf, img.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[region]) };
        Ok(())
    }

    /// `img` made anew when its size differs (the old one trashed), then the pixels uploaded and
    /// made ready to sample.
    fn picture(&mut self, slot: fn(&mut Vkr) -> &mut Option<Img>, px: &[u32], w: u32, h: u32) {
        if slot(self).as_ref().is_none_or(|i| (i.w, i.h) != (w, h)) {
            let usage = vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST;
            match self.image(w, h, vk::Format::R8G8B8A8_UNORM, usage, 1, 1, vk::SampleCountFlags::TYPE_1, false) {
                Ok(i) => {
                    if let Some(old) = slot(self).replace(i) {
                        self.trash(Trash::Img(old));
                    }
                }
                Err(e) => return super::log!("error: vulkan: {e}"),
            }
        }
        let img = slot(self).take().unwrap();
        let bytes = unsafe { std::slice::from_raw_parts(px.as_ptr() as *const u8, px.len() * 4) };
        if let Err(e) = self.upload(&img, 0, (0, 0, w, h), bytes) {
            super::log!("error: vulkan: {e}");
        }
        self.transition(&img, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        *slot(self) = Some(img);
    }

    /// Show a 2D frame of 0RGB pixels.
    pub fn frame(&mut self, px: &[u32], w: u32, h: u32) {
        self.picture(|v| &mut v.frame, px, w, h);
        self.frame_size = (w, h);
        self.shown = Shown::Frame;
    }

    pub fn overlay(&mut self, o: Option<(&[u32], u32, u32)>) {
        self.has_overlay = o.is_some();
        if let Some((px, w, h)) = o {
            self.picture(|v| &mut v.overlay, px, w, h);
        }
    }

    /// Show a video frame (0RGB, w x h), or no video.
    pub fn video(&mut self, v: Option<(&[u32], u32, u32)>) {
        self.video_size = v.map(|v| (v.1, v.2));
        if let Some((px, w, h)) = v {
            self.picture(|v| &mut v.video, px, w, h);
        }
    }

    /// Show an image rendered (Glide's target, in SHADER_READ_ONLY by then), w x h, with a gamma.
    pub fn show_scene(&mut self, view: vk::ImageView, w: u32, h: u32, gamma: f32) {
        self.shown = Shown::Scene(view, w, h, gamma);
    }

    /// The size of the picture shown (input positions are in its pixels).
    pub fn shown_size(&self) -> (u32, u32) {
        match self.shown {
            Shown::Frame => self.frame_size,
            Shown::Scene(_, w, h, _) => (w, h),
        }
    }

    pub fn resize(&self, _w: u32, _h: u32) {
        self.stale.set(true);
    }

    pub fn set_vsync(&self, on: bool) {
        if self.vsync.replace(on) != on {
            self.stale.set(true);
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
                self.extent = extent;
                return Ok(());
            }
            let formats = self.surface_fn.get_physical_device_surface_formats(self.phys, self.surface).map_err(err("surface formats"))?;
            // UNORM: the shown gamma is in the picture already
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
                .image_usage(vk::ImageUsageFlags::TRANSFER_DST)
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
            self.swap_format = format.format;
            super::log!("vulkan: swapchain {}x{}, {} images, {:?}, {:?}", extent.width, extent.height, self.images.len(), format.format, mode);
            Ok(())
        }
    }

    /// A picture as a rectangle of the window image (clip space, y0 the top).
    fn quad(&mut self, view: vk::ImageView, sampler: vk::Sampler, rect: [f32; 4], words: bool, gamma: f32, blend: bool) {
        let cmd = self.frames[self.cur].cmd;
        unsafe {
            self.device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.blit[blend as usize]);
            let info = [vk::DescriptorImageInfo::default().sampler(sampler).image_view(view).image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let write = vk::WriteDescriptorSet::default().dst_binding(0).descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER).image_info(&info);
            self.push_fn.cmd_push_descriptor_set(cmd, vk::PipelineBindPoint::GRAPHICS, self.blit_layout, 0, &[write]);
            let mut pc = [0u8; 24];
            for (k, v) in rect.iter().enumerate() {
                pc[4 * k..4 * k + 4].copy_from_slice(&v.to_le_bytes());
            }
            pc[16..20].copy_from_slice(&(words as i32).to_le_bytes());
            pc[20..24].copy_from_slice(&gamma.to_le_bytes());
            self.device.cmd_push_constants(cmd, self.blit_layout, vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT, 0, &pc);
            self.device.cmd_draw(cmd, 4, 1, 0, 0);
        }
    }

    /// Draw the shown picture, the video and the overlay into the window (w x h), letterboxed,
    /// and present it; with `shot`, read back the picture's area as 0RGB pixels.
    pub fn draw(&mut self, w: u32, h: u32, shot: bool) -> Option<(Vec<u32>, u32, u32)> {
        let stats = super::stats::on().then(super::stats::pixels);
        if self.stale.get() {
            if let Err(e) = self.build_swapchain(w, h) {
                super::log!("error: vulkan: {e}");
            }
        }
        let (w, h) = (self.extent.width, self.extent.height);
        if w == 0 || h == 0 {
            // minimized: what was recorded still runs
            self.submit(None);
            return None;
        }
        if self.window.as_ref().is_none_or(|i| (i.w, i.h) != (w, h)) {
            let usage = vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC;
            match self.image(w, h, vk::Format::R8G8B8A8_UNORM, usage, 1, 1, vk::SampleCountFlags::TYPE_1, false) {
                Ok(i) => {
                    if let Some(old) = self.window.replace(i) {
                        self.trash(Trash::Img(old));
                    }
                }
                Err(e) => {
                    super::log!("error: vulkan: {e}");
                    return None;
                }
            }
        }
        if let Some(px) = stats.clone().flatten() {
            self.picture(|v| &mut v.stats, &px, super::stats::W as u32, super::stats::H as u32);
        }
        let (fw, fh) = self.shown_size();
        let (ox, oy, s) = super::window::fit(fw, fh, w, h);
        let to_x = |x: f64| (x / w as f64 * 2.0 - 1.0) as f32;
        let to_y = |y: f64| (y / h as f64 * 2.0 - 1.0) as f32;
        let (x0, y0, x1, y1) = (to_x(ox), to_y(oy), to_x(ox + fw as f64 * s), to_y(oy + fh as f64 * s));
        let window = self.window.take().unwrap();
        self.end_rendering();
        let cmd = self.cmd();
        self.transition(&window, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        unsafe {
            let attach = [vk::RenderingAttachmentInfo::default()
                .image_view(window.view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(vk::ClearValue { color: vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 1.0] } })];
            let area = vk::Rect2D { offset: vk::Offset2D::default(), extent: vk::Extent2D { width: w, height: h } };
            self.device.cmd_begin_rendering(cmd, &vk::RenderingInfo::default().render_area(area).layer_count(1).color_attachments(&attach));
            self.device.cmd_set_viewport(cmd, 0, &[vk::Viewport { x: 0.0, y: 0.0, width: w as f32, height: h as f32, min_depth: 0.0, max_depth: 1.0 }]);
            self.device.cmd_set_scissor(cmd, 0, &[area]);
        }
        match self.shown {
            Shown::Frame => {
                if let Some(f) = &self.frame {
                    let v = f.view;
                    self.quad(v, self.nearest, [x0, y0, x1, y1], true, 1.0, false);
                }
            }
            Shown::Scene(view, _, _, gamma) => self.quad(view, self.linear, [x0, y0, x1, y1], false, gamma, false),
        }
        // dialogs stay 4:3 over a wide picture
        let (sx, sw) = super::window::overlay_span(fw, fh);
        let (ox0, ox1) = (x0 + (x1 - x0) * sx as f32 / fw as f32, x0 + (x1 - x0) * (sx + sw) as f32 / fw as f32);
        if let (Some((vw, vh)), Some(v)) = (self.video_size, self.video.as_ref().map(|v| v.view)) {
            unsafe {
                let clear = vk::ClearAttachment { aspect_mask: vk::ImageAspectFlags::COLOR, color_attachment: 0, clear_value: vk::ClearValue { color: vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 1.0] } } };
                let rect = vk::ClearRect { rect: vk::Rect2D { offset: vk::Offset2D::default(), extent: vk::Extent2D { width: w, height: h } }, base_array_layer: 0, layer_count: 1 };
                self.device.cmd_clear_attachments(cmd, &[clear], &[rect]);
            }
            let (vx, vy, s) = super::window::fit(vw, vh, w, h);
            self.quad(v, self.linear, [to_x(vx), to_y(vy), to_x(vx + vw as f64 * s), to_y(vy + vh as f64 * s)], true, 1.0, false);
        }
        if self.has_overlay {
            if let Some(o) = self.overlay.as_ref().map(|o| o.view) {
                self.quad(o, self.linear, [ox0, y0, ox1, y1], true, 1.0, true);
            }
        }
        if stats.is_some() {
            if let Some(t) = self.stats.as_ref().map(|t| t.view) {
                let scale = if h >= 900 { 2.0 } else { 1.0 };
                let (sx0, sy0) = (to_x(8.0), to_y(8.0));
                let (sx1, sy1) = (to_x(8.0 + super::stats::W as f64 * scale), to_y(8.0 + super::stats::H as f64 * scale));
                self.quad(t, self.nearest, [sx0, sy0, sx1, sy1], true, 1.0, true);
            }
        }
        unsafe { self.device.cmd_end_rendering(cmd) };
        self.transition(&window, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
        // FCOP_SHOT: the picture's area of the window into a host buffer
        let area = shot.then(|| {
            let (rx, ry) = (ox.round().max(0.0) as u32, oy.round().max(0.0) as u32);
            let (rw, rh) = (((fw as f64 * s).round() as u32).min(w - rx), ((fh as f64 * s).round() as u32).min(h - ry));
            (rx, ry, rw.max(1), rh.max(1))
        });
        if let Some((rx, ry, rw, rh)) = area {
            let bytes = rw as u64 * rh as u64 * 4;
            if self.readback.as_ref().is_none_or(|b| b.size < bytes) {
                match self.buffer(bytes, vk::BufferUsageFlags::TRANSFER_DST, true) {
                    Ok(b) => {
                        if let Some(old) = self.readback.replace(b) {
                            self.trash(Trash::Buf(old));
                        }
                    }
                    Err(e) => super::log!("error: vulkan: {e}"),
                }
            }
            if let Some(b) = &self.readback {
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1))
                    .image_offset(vk::Offset3D { x: rx as i32, y: ry as i32, z: 0 })
                    .image_extent(vk::Extent3D { width: rw, height: rh, depth: 1 });
                unsafe { self.device.cmd_copy_image_to_buffer(cmd, window.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, b.buf, &[region]) };
            }
        }
        self.submit(Some(&window));
        self.window = Some(window);
        let (rx, ry, rw, rh) = area?;
        let _ = ry;
        let _ = rx;
        // the picture just submitted: wait for it
        let done = self.frames[self.cur ^ 1].fence;
        unsafe {
            let _ = self.device.wait_for_fences(&[done], true, u64::MAX);
        }
        let b = self.readback.as_ref()?;
        let src = unsafe { std::slice::from_raw_parts(b.ptr, (rw * rh * 4) as usize) };
        let px = src.chunks_exact(4).map(|p| (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32).collect();
        Some((px, rw, rh))
    }

    /// Ends the picture's command buffer (with `window`: blitted into a swapchain image and
    /// presented) and submits it; the next picture is recorded in the other command buffer.
    fn submit(&mut self, window: Option<&Img>) {
        self.end_rendering();
        let cmd = self.cmd();
        let f = &self.frames[self.cur];
        let (fence, acquired) = (f.fence, f.acquired);
        let mut img = None;
        if let Some(win) = window {
            match unsafe { self.swapchain_fn.acquire_next_image(self.swapchain, u64::MAX, acquired, vk::Fence::null()) } {
                Ok((i, suboptimal)) => {
                    self.stale.set(self.stale.get() || suboptimal);
                    img = Some(i as usize);
                }
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.stale.set(true),
                Err(e) => super::log!("error: vulkan: acquire: {e}"),
            }
            if let Some(i) = img {
                let range = vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1);
                let barrier = |old, new, src, dst| {
                    vk::ImageMemoryBarrier::default()
                        .image(self.images[i])
                        .old_layout(old)
                        .new_layout(new)
                        .src_access_mask(src)
                        .dst_access_mask(dst)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .subresource_range(range)
                };
                let layers = vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1);
                let o = |x: u32, y: u32, z| vk::Offset3D { x: x as i32, y: y as i32, z };
                let (w, h) = (win.w.min(self.extent.width), win.h.min(self.extent.height));
                let blit = vk::ImageBlit::default().src_subresource(layers).src_offsets([o(0, 0, 0), o(w, h, 1)]).dst_subresource(layers).dst_offsets([o(0, 0, 0), o(w, h, 1)]);
                unsafe {
                    let d = &self.device;
                    d.cmd_pipeline_barrier(
                        cmd,
                        vk::PipelineStageFlags::TOP_OF_PIPE,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[],
                        &[barrier(vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::AccessFlags::empty(), vk::AccessFlags::TRANSFER_WRITE)],
                    );
                    d.cmd_blit_image(cmd, win.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, self.images[i], vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[blit], vk::Filter::NEAREST);
                    d.cmd_pipeline_barrier(
                        cmd,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[],
                        &[barrier(vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::PRESENT_SRC_KHR, vk::AccessFlags::TRANSFER_WRITE, vk::AccessFlags::empty())],
                    );
                }
            }
        }
        unsafe {
            let d = &self.device;
            let _ = d.end_command_buffer(cmd);
            let waits: Vec<vk::Semaphore> = img.map(|_| acquired).into_iter().collect();
            let signals: Vec<vk::Semaphore> = img.map(|i| self.present_done[i]).into_iter().collect();
            let stages = vec![vk::PipelineStageFlags::TRANSFER; waits.len()];
            let cmds = [cmd];
            let submit = vk::SubmitInfo::default().wait_semaphores(&waits).wait_dst_stage_mask(&stages).command_buffers(&cmds).signal_semaphores(&signals);
            if let Err(e) = d.queue_submit(self.queue, &[submit], fence) {
                super::log!("error: vulkan: submit: {e}");
            }
            self.recording = false;
            self.cur ^= 1;
            self.pictures += 1;
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
}

/// The aspect of a format: depth for depth formats, else colour.
pub fn aspect_of(f: vk::Format) -> vk::ImageAspectFlags {
    match f {
        vk::Format::D32_SFLOAT | vk::Format::D16_UNORM | vk::Format::X8_D24_UNORM_PACK32 => vk::ImageAspectFlags::DEPTH,
        vk::Format::D24_UNORM_S8_UINT | vk::Format::D32_SFLOAT_S8_UINT => vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
        _ => vk::ImageAspectFlags::COLOR,
    }
}

/// What a layout is used for, as a barrier's access mask.
fn access_of(l: vk::ImageLayout) -> vk::AccessFlags {
    match l {
        vk::ImageLayout::TRANSFER_DST_OPTIMAL => vk::AccessFlags::TRANSFER_WRITE,
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL => vk::AccessFlags::TRANSFER_READ,
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL => vk::AccessFlags::SHADER_READ,
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL => vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
        vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL => vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
        vk::ImageLayout::GENERAL => vk::AccessFlags::SHADER_READ | vk::AccessFlags::SHADER_WRITE | vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE,
        _ => vk::AccessFlags::empty(),
    }
}

/// A graphics pipeline for dynamic rendering (viewport and scissor dynamic).
#[allow(clippy::too_many_arguments)]
pub fn graphics_pipeline(
    device: &ash::Device,
    layout: vk::PipelineLayout,
    vert: &[u8],
    frag: &[u8],
    bindings: &[vk::VertexInputBindingDescription],
    attributes: &[vk::VertexInputAttributeDescription],
    topology: vk::PrimitiveTopology,
    blends: &[vk::PipelineColorBlendAttachmentState],
    formats: &[vk::Format],
    depth_format: vk::Format,
    samples: vk::SampleCountFlags,
    depth: Option<vk::PipelineDepthStencilStateCreateInfo>,
) -> Result<vk::Pipeline, String> {
    unsafe {
        let module = |spv: &[u8]| -> Result<vk::ShaderModule, String> {
            let code = ash::util::read_spv(&mut std::io::Cursor::new(spv)).map_err(|e| format!("shader: {e}"))?;
            device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None).map_err(err("shader module"))
        };
        let (vs, fs) = (module(vert)?, module(frag)?);
        let stages = [
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(vs).name(c"main"),
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(fs).name(c"main"),
        ];
        let input = vk::PipelineVertexInputStateCreateInfo::default().vertex_binding_descriptions(bindings).vertex_attribute_descriptions(attributes);
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(topology);
        let viewport = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default().polygon_mode(vk::PolygonMode::FILL).cull_mode(vk::CullModeFlags::NONE).line_width(1.0);
        let ms = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(samples);
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(blends);
        let dynamic = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dyn_info = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);
        let mut rendering = vk::PipelineRenderingCreateInfo::default().color_attachment_formats(formats).depth_attachment_format(depth_format);
        let ds = depth.unwrap_or_default();
        let info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&ms)
            .color_blend_state(&blend)
            .depth_stencil_state(&ds)
            .dynamic_state(&dyn_info)
            .layout(layout)
            .push_next(&mut rendering);
        let p = device.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None);
        device.destroy_shader_module(vs, None);
        device.destroy_shader_module(fs, None);
        p.map(|p| p[0]).map_err(|(_, e)| format!("graphics pipeline: {e}"))
    }
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
