//! Ray tracing on the Vulkan device (PLAN-RTX.md step 2, `FCOP_GBUFFER=rt`): the scene's
//! acceleration structures and a debug picture. Each model (or part) and terrain block gets a
//! BLAS the first time it is seen (cached until the mission changes; geometry from guest memory,
//! `scene::model_mesh` / `block_mesh`); each picture a TLAS of the view's instances. A compute
//! shader (`shaders/rt_debug.comp`, ray query) shoots one ray per pixel from the game's camera
//! and shades what it hits (terrain yellow, models green, faces in varying brightness); the
//! present shows that picture instead of the game's.

use super::scene::{Frame, Instance};
use ash::{khr, vk};
use std::collections::HashMap;

/// The device extensions ray tracing needs (enabled by `vk.rs` when present).
pub const EXTENSIONS: [&std::ffi::CStr; 3] = [khr::acceleration_structure::NAME, khr::ray_query::NAME, khr::deferred_host_operations::NAME];

const SHADER: &[u8] = include_bytes!("../../shaders/rt_debug.spv");

/// A buffer with its memory and device address (mapped when host visible).
struct Buffer {
    buf: vk::Buffer,
    mem: vk::DeviceMemory,
    addr: u64,
    ptr: *mut u8,
}

struct Accel {
    accel: vk::AccelerationStructureKHR,
    addr: u64,
    /// storage, and the inputs and scratch of its build (kept: freed with it)
    bufs: Vec<Buffer>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    /// a model's vertex list, first vertex, count
    Model(u32, u32, u32),
    /// a terrain block's sect
    Block(u32),
}

pub struct Rt {
    device: ash::Device,
    accel_fn: khr::acceleration_structure::Device,
    memory: vk::PhysicalDeviceMemoryProperties,
    blas: HashMap<Key, Option<Accel>>,
    generation: u32,
    /// instance buffers, one per picture in flight, and their capacity
    instances: [Option<(Buffer, usize)>; 2],
    /// the TLAS, its scratch and its capacity in instances
    tlas: Option<(Accel, Buffer, usize)>,
    _set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    set: vk::DescriptorSet,
    /// the picture rays are shot into (storage image), its size
    image: Option<(vk::Image, vk::DeviceMemory, vk::ImageView, u32, u32)>,
    /// the descriptor set needs writing (TLAS or image made anew)
    stale: bool,
}

fn err(what: &str) -> impl Fn(vk::Result) -> String + '_ {
    move |e| format!("{what}: {e}")
}

impl Rt {
    pub fn new(instance: &ash::Instance, device: &ash::Device, phys: vk::PhysicalDevice) -> Result<Rt, String> {
        unsafe {
            let accel_fn = khr::acceleration_structure::Device::new(instance, device);
            let memory = instance.get_physical_device_memory_properties(phys);
            let bindings = [
                vk::DescriptorSetLayoutBinding::default()
                    .binding(0)
                    .descriptor_type(vk::DescriptorType::ACCELERATION_STRUCTURE_KHR)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
                vk::DescriptorSetLayoutBinding::default().binding(1).descriptor_type(vk::DescriptorType::STORAGE_IMAGE).descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE),
            ];
            let set_layout = device.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None).map_err(err("descriptor set layout"))?;
            let ranges = [vk::PushConstantRange::default().stage_flags(vk::ShaderStageFlags::COMPUTE).size(96)];
            let layouts = [set_layout];
            let layout = device.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts).push_constant_ranges(&ranges), None).map_err(err("pipeline layout"))?;
            let code = ash::util::read_spv(&mut std::io::Cursor::new(SHADER)).map_err(|e| format!("shader: {e}"))?;
            let module = device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None).map_err(err("shader module"))?;
            let stage = vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::COMPUTE).module(module).name(c"main");
            let pipeline = device
                .create_compute_pipelines(vk::PipelineCache::null(), &[vk::ComputePipelineCreateInfo::default().stage(stage).layout(layout)], None)
                .map_err(|e| format!("compute pipeline: {}", e.1))?[0];
            device.destroy_shader_module(module, None);
            let sizes = [
                vk::DescriptorPoolSize { ty: vk::DescriptorType::ACCELERATION_STRUCTURE_KHR, descriptor_count: 1 },
                vk::DescriptorPoolSize { ty: vk::DescriptorType::STORAGE_IMAGE, descriptor_count: 1 },
            ];
            let pool = device.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes), None).map_err(err("descriptor pool"))?;
            let set = device.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(pool).set_layouts(&layouts)).map_err(err("descriptor set"))?[0];
            super::log!("raytrace: ready");
            Ok(Rt {
                device: device.clone(),
                accel_fn,
                memory,
                blas: HashMap::new(),
                generation: u32::MAX,
                instances: [None, None],
                tlas: None,
                _set_layout: set_layout,
                layout,
                pipeline,
                set,
                image: None,
                stale: true,
            })
        }
    }

    fn buffer(&self, size: u64, usage: vk::BufferUsageFlags, host: bool) -> Result<Buffer, String> {
        unsafe {
            let d = &self.device;
            let buf = d
                .create_buffer(&vk::BufferCreateInfo::default().size(size.max(16)).usage(usage | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS), None)
                .map_err(err("buffer"))?;
            let req = d.get_buffer_memory_requirements(buf);
            let want = if host { vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT } else { vk::MemoryPropertyFlags::DEVICE_LOCAL };
            let kind = (0..self.memory.memory_type_count)
                .find(|&i| req.memory_type_bits & (1 << i) != 0 && self.memory.memory_types[i as usize].property_flags.contains(want))
                .ok_or("no memory type for a buffer")?;
            let mut flags = vk::MemoryAllocateFlagsInfo::default().flags(vk::MemoryAllocateFlags::DEVICE_ADDRESS);
            let mem = d.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(kind).push_next(&mut flags), None).map_err(err("buffer memory"))?;
            d.bind_buffer_memory(buf, mem, 0).map_err(err("bind buffer memory"))?;
            let addr = d.get_buffer_device_address(&vk::BufferDeviceAddressInfo::default().buffer(buf));
            let ptr = if host { d.map_memory(mem, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()).map_err(err("map"))? as *mut u8 } else { std::ptr::null_mut() };
            Ok(Buffer { buf, mem, addr, ptr })
        }
    }

    fn free(&self, b: Buffer) {
        unsafe {
            self.device.destroy_buffer(b.buf, None);
            self.device.free_memory(b.mem, None);
        }
    }

    fn free_accel(&self, a: Accel) {
        unsafe { self.accel_fn.destroy_acceleration_structure(a.accel, None) };
        for b in a.bufs {
            self.free(b);
        }
    }

    /// A host buffer holding `data`.
    fn upload<T: Copy>(&self, data: &[T], usage: vk::BufferUsageFlags) -> Result<Buffer, String> {
        let size = std::mem::size_of_val(data) as u64;
        let b = self.buffer(size, usage, true)?;
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, b.ptr, size as usize) };
        Ok(b)
    }

    /// Storage and scratch for an acceleration structure of `kind` with `geometry` (room for
    /// `max` primitives); the build of `count` recorded into `cmd`.
    fn build(&self, cmd: vk::CommandBuffer, kind: vk::AccelerationStructureTypeKHR, geometry: vk::AccelerationStructureGeometryKHR, count: u32, max: u32, mut bufs: Vec<Buffer>) -> Result<Accel, String> {
        unsafe {
            let geometries = [geometry];
            let mut info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
                .ty(kind)
                .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE)
                .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
                .geometries(&geometries);
            let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
            self.accel_fn.get_acceleration_structure_build_sizes(vk::AccelerationStructureBuildTypeKHR::DEVICE, &info, &[max], &mut sizes);
            let storage = self.buffer(sizes.acceleration_structure_size, vk::BufferUsageFlags::ACCELERATION_STRUCTURE_STORAGE_KHR, false)?;
            let accel = self
                .accel_fn
                .create_acceleration_structure(&vk::AccelerationStructureCreateInfoKHR::default().buffer(storage.buf).size(sizes.acceleration_structure_size).ty(kind), None)
                .map_err(err("acceleration structure"))?;
            // scratch addresses aligned to 256 (more than any device asks)
            let scratch = self.buffer(sizes.build_scratch_size + 256, vk::BufferUsageFlags::STORAGE_BUFFER, false)?;
            info = info.dst_acceleration_structure(accel).scratch_data(vk::DeviceOrHostAddressKHR { device_address: (scratch.addr + 255) & !255 });
            let range = [vk::AccelerationStructureBuildRangeInfoKHR::default().primitive_count(count)];
            self.accel_fn.cmd_build_acceleration_structures(cmd, &[info], &[&range]);
            let addr = self.accel_fn.get_acceleration_structure_device_address(&vk::AccelerationStructureDeviceAddressInfoKHR::default().acceleration_structure(accel));
            bufs.push(storage);
            bufs.push(scratch);
            Ok(Accel { accel, addr, bufs })
        }
    }

    /// A BLAS of triangles (positions, indices), or none when there are none.
    fn blas(&self, cmd: vk::CommandBuffer, (pos, idx): (Vec<[f32; 3]>, Vec<u32>)) -> Result<Option<Accel>, String> {
        if idx.is_empty() {
            return Ok(None);
        }
        let input = vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR;
        let (vb, ib) = (self.upload(&pos, input)?, self.upload(&idx, input)?);
        let tri = vk::AccelerationStructureGeometryTrianglesDataKHR::default()
            .vertex_format(vk::Format::R32G32B32_SFLOAT)
            .vertex_data(vk::DeviceOrHostAddressConstKHR { device_address: vb.addr })
            .vertex_stride(12)
            .max_vertex(pos.len() as u32 - 1)
            .index_type(vk::IndexType::UINT32)
            .index_data(vk::DeviceOrHostAddressConstKHR { device_address: ib.addr });
        let geometry = vk::AccelerationStructureGeometryKHR::default()
            .geometry_type(vk::GeometryTypeKHR::TRIANGLES)
            .geometry(vk::AccelerationStructureGeometryDataKHR { triangles: tri })
            .flags(vk::GeometryFlagsKHR::OPAQUE);
        let n = idx.len() as u32 / 3;
        self.build(cmd, vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL, geometry, n, n, vec![vb, ib]).map(Some)
    }

    fn barrier(&self, cmd: vk::CommandBuffer, src: (vk::PipelineStageFlags, vk::AccessFlags), dst: (vk::PipelineStageFlags, vk::AccessFlags)) {
        let b = [vk::MemoryBarrier::default().src_access_mask(src.1).dst_access_mask(dst.1)];
        unsafe { self.device.cmd_pipeline_barrier(cmd, src.0, dst.0, vk::DependencyFlags::empty(), &b, &[], &[]) };
    }

    /// The picture of `frame` into an image of w x h, recorded into `cmd` (picture in flight
    /// `slot`); the image is left in TRANSFER_SRC_OPTIMAL for the present to copy.
    pub fn record(&mut self, cmd: vk::CommandBuffer, slot: usize, w: u32, h: u32, frame: &Frame) -> Result<vk::Image, String> {
        let build = vk::PipelineStageFlags::ACCELERATION_STRUCTURE_BUILD_KHR;
        let (aw, ar) = (vk::AccessFlags::ACCELERATION_STRUCTURE_WRITE_KHR, vk::AccessFlags::ACCELERATION_STRUCTURE_READ_KHR);
        let compute = vk::PipelineStageFlags::COMPUTE_SHADER;
        if frame.generation != self.generation {
            // a new mission: its geometry is elsewhere
            unsafe { self.device.device_wait_idle().map_err(err("wait"))? };
            for (_, a) in std::mem::take(&mut self.blas) {
                if let Some(a) = a {
                    self.free_accel(a);
                }
            }
            self.generation = frame.generation;
        }
        // the last picture's rays and builds are done with what is built now
        self.barrier(cmd, (build | compute, aw | ar | vk::AccessFlags::SHADER_READ), (build, aw | ar));
        let mut instances = Vec::new();
        let mut built = 0;
        for (sect, b) in &frame.blocks {
            let key = Key::Block(*sect);
            if !self.blas.contains_key(&key) {
                let a = self.blas(cmd, super::scene::block_mesh(*sect, b))?;
                self.blas.insert(key, a);
                built += 1;
            }
            if let Some(Some(a)) = self.blas.get(&key) {
                let o = b.origin.map(|x| x as f32);
                instances.push(instance([1.0, 0.0, 0.0, o[0], 0.0, 1.0, 0.0, o[1], 0.0, 0.0, 1.0, o[2]], 0, a.addr));
            }
        }
        for i in &frame.models {
            let key = Key::Model(i.list, i.first, i.count);
            if !self.blas.contains_key(&key) {
                let a = self.blas(cmd, super::scene::model_mesh(i))?;
                self.blas.insert(key, a);
                built += 1;
            }
            if let Some(Some(a)) = self.blas.get(&key) {
                instances.push(instance(model_transform(i), 1, a.addr));
            }
        }
        if built > 0 {
            self.barrier(cmd, (build, aw), (build, ar));
        }
        // the instances into this picture's buffer
        if self.instances[slot].as_ref().is_none_or(|b| b.1 < instances.len()) {
            if let Some((b, _)) = self.instances[slot].take() {
                unsafe { self.device.device_wait_idle().map_err(err("wait"))? };
                self.free(b);
            }
            let cap = instances.len().max(256).next_power_of_two();
            let usage = vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR;
            self.instances[slot] = Some((self.buffer((cap * std::mem::size_of::<vk::AccelerationStructureInstanceKHR>()) as u64, usage, true)?, cap));
        }
        let (ib, _) = self.instances[slot].as_ref().unwrap();
        unsafe { std::ptr::copy_nonoverlapping(instances.as_ptr() as *const u8, ib.ptr, std::mem::size_of_val(&instances[..])) };
        let geometry = vk::AccelerationStructureGeometryKHR::default().geometry_type(vk::GeometryTypeKHR::INSTANCES).geometry(vk::AccelerationStructureGeometryDataKHR {
            instances: vk::AccelerationStructureGeometryInstancesDataKHR::default().data(vk::DeviceOrHostAddressConstKHR { device_address: ib.addr }),
        });
        // the TLAS built anew each picture into the same storage; a larger one when it grew
        let n = instances.len() as u32;
        let top = vk::AccelerationStructureTypeKHR::TOP_LEVEL;
        match &self.tlas {
            Some((t, scratch, cap)) if *cap >= instances.len() => {
                let geometries = [geometry];
                let info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
                    .ty(top)
                    .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE)
                    .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
                    .geometries(&geometries)
                    .dst_acceleration_structure(t.accel)
                    .scratch_data(vk::DeviceOrHostAddressKHR { device_address: (scratch.addr + 255) & !255 });
                let range = [vk::AccelerationStructureBuildRangeInfoKHR::default().primitive_count(n)];
                unsafe { self.accel_fn.cmd_build_acceleration_structures(cmd, &[info], &[&range]) };
            }
            _ => {
                if let Some((old, scratch, _)) = self.tlas.take() {
                    unsafe { self.device.device_wait_idle().map_err(err("wait"))? };
                    self.free_accel(old);
                    self.free(scratch);
                }
                let cap = instances.len().max(256).next_power_of_two();
                let mut tlas = self.build(cmd, top, geometry, n, cap as u32, vec![])?;
                let scratch = tlas.bufs.pop().unwrap();
                self.tlas = Some((tlas, scratch, cap));
                self.stale = true;
            }
        }
        self.barrier(cmd, (build, aw), (compute, ar));

        if self.image.as_ref().is_none_or(|i| (i.3, i.4) != (w, h)) {
            self.make_image(w, h)?;
        }
        let (image, _, view, _, _) = *self.image.as_ref().unwrap();
        if self.stale {
            let accels = [self.tlas.as_ref().unwrap().0.accel];
            let mut write_as = vk::WriteDescriptorSetAccelerationStructureKHR::default().acceleration_structures(&accels);
            let images = [vk::DescriptorImageInfo::default().image_view(view).image_layout(vk::ImageLayout::GENERAL)];
            let writes = [
                vk::WriteDescriptorSet::default().dst_set(self.set).dst_binding(0).descriptor_type(vk::DescriptorType::ACCELERATION_STRUCTURE_KHR).descriptor_count(1).push_next(&mut write_as),
                vk::WriteDescriptorSet::default().dst_set(self.set).dst_binding(1).descriptor_type(vk::DescriptorType::STORAGE_IMAGE).image_info(&images),
            ];
            unsafe { self.device.update_descriptor_sets(&writes, &[]) };
            self.stale = false;
        }
        let range = vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1);
        let to = |old, new, src, dst| [vk::ImageMemoryBarrier::default().image(image).old_layout(old).new_layout(new).src_access_mask(src).dst_access_mask(dst).subresource_range(range)];
        let (c, cp) = frame.camera;
        let mut push = [0f32; 24];
        for i in 0..3 {
            for j in 0..3 {
                // column i of the camera-to-world matrix: row j of C, element i
                push[4 * i + j] = c[3 * j + i] as f32;
            }
        }
        push[15] = 1.0;
        push[16..19].copy_from_slice(&cp.map(|x| x as f32));
        push[20] = 4096.0 * h as f32 / 1920.0;
        push[21] = frame.k;
        // the debug shading's fog: darker with distance
        push[22] = 30000.0;
        // FCOP_RT_HIGH=<model units> (a test): seen from that high above, straight down (camera
        // x to world x, its up to world z, ahead to world -y)
        if let Some(high) = high() {
            push[..12].copy_from_slice(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0]);
            push[17] += high;
            push[22] = 3.0 * high;
        }
        unsafe {
            let d = &self.device;
            d.cmd_pipeline_barrier(cmd, vk::PipelineStageFlags::TOP_OF_PIPE, compute, vk::DependencyFlags::empty(), &[], &[], &to(vk::ImageLayout::UNDEFINED, vk::ImageLayout::GENERAL, vk::AccessFlags::empty(), vk::AccessFlags::SHADER_WRITE));
            d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, self.pipeline);
            d.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::COMPUTE, self.layout, 0, &[self.set], &[]);
            let bytes = std::slice::from_raw_parts(push.as_ptr() as *const u8, 96);
            d.cmd_push_constants(cmd, self.layout, vk::ShaderStageFlags::COMPUTE, 0, bytes);
            d.cmd_dispatch(cmd, w.div_ceil(8), h.div_ceil(8), 1);
            d.cmd_pipeline_barrier(
                cmd,
                compute,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &to(vk::ImageLayout::GENERAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::AccessFlags::SHADER_WRITE, vk::AccessFlags::TRANSFER_READ),
            );
        }
        Ok(image)
    }

    fn make_image(&mut self, w: u32, h: u32) -> Result<(), String> {
        unsafe {
            let d = &self.device;
            if let Some((image, mem, view, _, _)) = self.image.take() {
                d.device_wait_idle().map_err(err("wait"))?;
                d.destroy_image_view(view, None);
                d.destroy_image(image, None);
                d.free_memory(mem, None);
            }
            let info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
                .extent(vk::Extent3D { width: w, height: h, depth: 1 })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC);
            let image = d.create_image(&info, None).map_err(err("ray picture"))?;
            let req = d.get_image_memory_requirements(image);
            let kind = (0..self.memory.memory_type_count)
                .find(|&i| req.memory_type_bits & (1 << i) != 0 && self.memory.memory_types[i as usize].property_flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL))
                .ok_or("no memory for the ray picture")?;
            let mem = d.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(kind), None).map_err(err("ray picture memory"))?;
            d.bind_image_memory(image, mem, 0).map_err(err("bind ray picture"))?;
            let range = vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1);
            let view = d
                .create_image_view(&vk::ImageViewCreateInfo::default().image(image).view_type(vk::ImageViewType::TYPE_2D).format(vk::Format::R8G8B8A8_UNORM).subresource_range(range), None)
                .map_err(err("ray picture view"))?;
            self.image = Some((image, mem, view, w, h));
            self.stale = true;
            Ok(())
        }
    }
}

/// A model instance's transform for Vulkan (3x4, rows, column vectors): world = v * R + offset
/// in our row-vector terms.
fn model_transform(i: &Instance) -> [f32; 12] {
    let r = &i.matrix;
    std::array::from_fn(|k| {
        let (row, col) = (k / 4, k % 4);
        (if col == 3 { i.offset[row] } else { r[3 * col + row] }) as f32
    })
}

fn instance(t: [f32; 12], kind: u32, blas: u64) -> vk::AccelerationStructureInstanceKHR {
    vk::AccelerationStructureInstanceKHR {
        transform: vk::TransformMatrixKHR { matrix: t },
        instance_custom_index_and_mask: vk::Packed24_8::new(kind, 0xff),
        instance_shader_binding_table_record_offset_and_flags: vk::Packed24_8::new(0, vk::GeometryInstanceFlagsKHR::TRIANGLE_FACING_CULL_DISABLE.as_raw() as u8),
        acceleration_structure_reference: vk::AccelerationStructureReferenceKHR { device_handle: blas },
    }
}

/// `FCOP_RT_HIGH=<model units>` (a test): the rays' picture from that high above the camera.
fn high() -> Option<f32> {
    static H: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
    *H.get_or_init(|| std::env::var("FCOP_RT_HIGH").ok().and_then(|v| v.parse().ok()))
}
