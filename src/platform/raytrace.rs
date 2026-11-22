//! Ray tracing on the Vulkan device (PLAN-RTX.md step 2, `FCOP_GBUFFER=rt`): the scene's
//! acceleration structures and a debug picture. Each model (or part) and terrain block gets a
//! BLAS the first time it is seen (cached until the mission changes; geometry from guest memory,
//! `scene::model_mesh` / `block_mesh`); each picture a TLAS of the view's instances. A compute
//! shader (`shaders/rt_debug.comp`, ray query) shoots one ray per pixel from the game's camera
//! and shades what it hits (terrain yellow, models green, faces in varying brightness); the
//! present shows that picture instead of the game's.
//!
//! The sun's shadow (`shadows::Mode::Rays`, PLAN-RTX.md step 5): `shadow` traces one ray per
//! pixel of a view toward the sun (`shaders/rt_shadow.comp`) from the place the game's depth
//! gives, in an RGBA32F image GL shares (`vk::Vk::sun_rays`); the ambient occlusion's rays too
//! (`ao::traced`).

use super::scene::{Frame, Instance};
use ash::{khr, vk};
use std::collections::HashMap;

/// The device extensions ray tracing needs (enabled by `vk.rs` when present).
pub const EXTENSIONS: [&std::ffi::CStr; 3] = [khr::acceleration_structure::NAME, khr::ray_query::NAME, khr::deferred_host_operations::NAME];

const DEBUG_SPV: &[u8] = include_bytes!("../../shaders/rt_debug.spv");
const SHADOW_SPV: &[u8] = include_bytes!("../../shaders/rt_shadow.spv");
const AO_BLUR_SPV: &[u8] = include_bytes!("../../shaders/rt_ao_blur.spv");
/// An instance's custom index: bit 23 a model (else terrain), below it its first material + 1.
const MODEL: u32 = 1 << 23;
/// The shaders' push constants (bytes).
const PUSH: u32 = 128;

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
    /// each BLAS and its instances' custom index: its triangles' first material + 1 (0: terrain,
    /// opaque)
    blas: HashMap<Key, Option<(Accel, u32)>>,
    generation: u32,
    /// the models' triangle materials (`scene::Material`) in BLAS order, how many are in the
    /// buffer, the buffer and its capacity
    mats: Vec<super::scene::Material>,
    mats_sent: usize,
    mat_buf: Option<(Buffer, usize)>,
    /// the mission's texture pages as coverage bits (`scene::Frame::pages`) and which ones
    pages_buf: Option<(Buffer, *const Vec<u32>)>,
    /// instance buffers, one per picture in flight (the debug picture's two, the sun's one),
    /// and their capacity
    instances: [Option<(Buffer, usize)>; 3],
    /// the TLAS, its scratch and its capacity in instances
    tlas: Option<(Accel, Buffer, usize)>,
    /// counts the TLAS made anew (descriptor sets name it)
    tlas_made: u64,
    _set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    debug: vk::Pipeline,
    shadow: vk::Pipeline,
    /// the traced AO's blur (`shaders/rt_ao_blur.comp`), after the sun's rays
    ao_blur: vk::Pipeline,
    /// the debug picture's and the sun's descriptor sets, and what each was written with (TLAS,
    /// storage image, pages, materials)
    sets: [(vk::DescriptorSet, Option<(u64, vk::ImageView, vk::Buffer, vk::Buffer)>); 2],
    /// the picture rays are shot into (storage image), its size
    image: Option<(vk::Image, vk::DeviceMemory, vk::ImageView, u32, u32)>,
    /// `FCOP_RT_TIME=1` (a test): GPU time of the sun's rays, logged every 2 s
    timing: Option<Timing>,
    /// this recording writes timestamps (`FCOP_RT_TIME`, the sun's rays only)
    timed: bool,
    /// the dynamic lights whose shadows the sun's shader traces too (`shadow`): a count, then
    /// place and reach of each (`LIGHTS` at most); written while no rays read it
    light_buf: Option<Buffer>,
    /// the BLAS the TLAS's instances named at its last build, in order, and the refits since
    refs: Vec<u64>,
    refits: u32,
}

/// The most lights `shadow` traces (bits of the mask's G / 2).
const LIGHTS: usize = 8;
/// The TLAS is built anew at least every this many pictures (refitted in between).
const REBUILD: u32 = 30;
const LIGHT_BYTES: u64 = 16 * (1 + LIGHTS as u64);
/// A light's rays end this share of its reach short of it: its place is pulled toward the camera
/// by up to 0.2 of it (`lights::view`), and what holds it (a robot's light bar, the robot) must not
/// shadow it (0.08 left the ground around a robot dark in its own police lights).
const LIGHT_SKIP: f32 = 0.3;

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
                vk::DescriptorSetLayoutBinding::default().binding(2).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE),
                vk::DescriptorSetLayoutBinding::default().binding(3).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE),
                vk::DescriptorSetLayoutBinding::default().binding(4).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE),
            ];
            let set_layout = device.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None).map_err(err("descriptor set layout"))?;
            let ranges = [vk::PushConstantRange::default().stage_flags(vk::ShaderStageFlags::COMPUTE).size(PUSH)];
            let layouts = [set_layout];
            let layout = device.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts).push_constant_ranges(&ranges), None).map_err(err("pipeline layout"))?;
            let pipeline = |spv: &[u8]| -> Result<vk::Pipeline, String> {
                let code = ash::util::read_spv(&mut std::io::Cursor::new(spv)).map_err(|e| format!("shader: {e}"))?;
                let module = device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None).map_err(err("shader module"))?;
                let stage = vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::COMPUTE).module(module).name(c"main");
                let p = device.create_compute_pipelines(vk::PipelineCache::null(), &[vk::ComputePipelineCreateInfo::default().stage(stage).layout(layout)], None);
                device.destroy_shader_module(module, None);
                Ok(p.map_err(|e| format!("compute pipeline: {}", e.1))?[0])
            };
            let (debug, shadow, ao_blur) = (pipeline(DEBUG_SPV)?, pipeline(SHADOW_SPV)?, pipeline(AO_BLUR_SPV)?);
            let sizes = [
                vk::DescriptorPoolSize { ty: vk::DescriptorType::ACCELERATION_STRUCTURE_KHR, descriptor_count: 2 },
                vk::DescriptorPoolSize { ty: vk::DescriptorType::STORAGE_IMAGE, descriptor_count: 2 },
                vk::DescriptorPoolSize { ty: vk::DescriptorType::STORAGE_BUFFER, descriptor_count: 6 },
            ];
            let pool = device.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(2).pool_sizes(&sizes), None).map_err(err("descriptor pool"))?;
            let both = [set_layout, set_layout];
            let sets = device.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(pool).set_layouts(&both)).map_err(err("descriptor set"))?;
            // (always made: the debug menu turns the times on while running, `timed`)
            let timing = {
                let pool = device.create_query_pool(&vk::QueryPoolCreateInfo::default().query_type(vk::QueryType::TIMESTAMP).query_count(4), None).map_err(err("query pool"))?;
                let period = instance.get_physical_device_properties(phys).limits.timestamp_period as f64;
                Some(Timing { pool, period, pending: false, sum: [0.0; 3], count: 0, instances: (0, 0), since: std::time::Instant::now() })
            };
            super::log!("raytrace: ready");
            let mut rt = Rt {
                device: device.clone(),
                accel_fn,
                memory,
                blas: HashMap::new(),
                generation: u32::MAX,
                mats: Vec::new(),
                mats_sent: 0,
                mat_buf: None,
                pages_buf: None,
                instances: [None, None, None],
                tlas: None,
                tlas_made: 0,
                _set_layout: set_layout,
                layout,
                debug,
                shadow,
                ao_blur,
                sets: [(sets[0], None), (sets[1], None)],
                image: None,
                timing,
                timed: false,
                light_buf: None,
                refs: Vec::new(),
                refits: 0,
            };
            rt.light_buf = Some(rt.buffer(LIGHT_BYTES, vk::BufferUsageFlags::STORAGE_BUFFER, true)?);
            Ok(rt)
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

    /// Storage and scratch (for a build and an update) for an acceleration structure of `kind`
    /// with `geometries` (room for `max` primitives each); the build of `counts` recorded into `cmd`.
    fn build(&self, cmd: vk::CommandBuffer, kind: vk::AccelerationStructureTypeKHR, flags: vk::BuildAccelerationStructureFlagsKHR, geometries: &[vk::AccelerationStructureGeometryKHR], counts: &[u32], max: &[u32], mut bufs: Vec<Buffer>) -> Result<Accel, String> {
        unsafe {
            let mut info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
                .ty(kind)
                .flags(flags)
                .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
                .geometries(geometries);
            let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
            self.accel_fn.get_acceleration_structure_build_sizes(vk::AccelerationStructureBuildTypeKHR::DEVICE, &info, max, &mut sizes);
            let storage = self.buffer(sizes.acceleration_structure_size, vk::BufferUsageFlags::ACCELERATION_STRUCTURE_STORAGE_KHR, false)?;
            let accel = self
                .accel_fn
                .create_acceleration_structure(&vk::AccelerationStructureCreateInfoKHR::default().buffer(storage.buf).size(sizes.acceleration_structure_size).ty(kind), None)
                .map_err(err("acceleration structure"))?;
            // scratch addresses aligned to 256 (more than any device asks)
            let scratch = self.buffer(sizes.build_scratch_size.max(sizes.update_scratch_size) + 256, vk::BufferUsageFlags::STORAGE_BUFFER, false)?;
            info = info.dst_acceleration_structure(accel).scratch_data(vk::DeviceOrHostAddressKHR { device_address: (scratch.addr + 255) & !255 });
            let ranges: Vec<_> = counts.iter().map(|&c| vk::AccelerationStructureBuildRangeInfoKHR::default().primitive_count(c)).collect();
            self.accel_fn.cmd_build_acceleration_structures(cmd, &[info], &[&ranges]);
            let addr = self.accel_fn.get_acceleration_structure_device_address(&vk::AccelerationStructureDeviceAddressInfoKHR::default().acceleration_structure(accel));
            bufs.push(storage);
            bufs.push(scratch);
            Ok(Accel { accel, addr, bufs })
        }
    }

    /// A BLAS of triangles (positions, indices), or none when there are none: the first
    /// `opaque` triangles one opaque geometry, the rest a second one whose hits the rays test
    /// by their texel (`shaders/rt_shadow.comp`; its primitive indices count from 0).
    fn blas(&self, cmd: vk::CommandBuffer, pos: &[[f32; 3]], idx: &[u32], opaque: u32) -> Result<Option<Accel>, String> {
        if idx.is_empty() {
            return Ok(None);
        }
        let input = vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR;
        let (vb, ib) = (self.upload(pos, input)?, self.upload(idx, input)?);
        let n = idx.len() as u32 / 3;
        let mut geometries = Vec::new();
        let mut counts = Vec::new();
        for (first, count, flags) in [(0, opaque, vk::GeometryFlagsKHR::OPAQUE), (opaque, n - opaque, vk::GeometryFlagsKHR::empty())] {
            if count == 0 {
                continue;
            }
            let tri = vk::AccelerationStructureGeometryTrianglesDataKHR::default()
                .vertex_format(vk::Format::R32G32B32_SFLOAT)
                .vertex_data(vk::DeviceOrHostAddressConstKHR { device_address: vb.addr })
                .vertex_stride(12)
                .max_vertex(pos.len() as u32 - 1)
                .index_type(vk::IndexType::UINT32)
                .index_data(vk::DeviceOrHostAddressConstKHR { device_address: ib.addr + 12 * first as u64 });
            geometries.push(vk::AccelerationStructureGeometryKHR::default().geometry_type(vk::GeometryTypeKHR::TRIANGLES).geometry(vk::AccelerationStructureGeometryDataKHR { triangles: tri }).flags(flags));
            counts.push(count);
        }
        self.build(cmd, vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL, vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE, &geometries, &counts, &counts, vec![vb, ib]).map(Some)
    }

    /// A mesh's BLAS (`scene::model_mesh` / `block_mesh`) and its instances' custom index: `kind`
    /// (`MODEL` or 0) | the first material of its triangles with keyed texels + 1.
    fn mesh(&mut self, cmd: vk::CommandBuffer, (pos, idx, mats): (Vec<[f32; 3]>, Vec<u32>, Vec<super::scene::Material>), kind: u32) -> Result<Option<(Accel, u32)>, String> {
        // opaque triangles first, then those with keyed texels and their materials
        let keyed = |m: &super::scene::Material| m[1] >> 16 != super::scene::OPAQUE;
        let mut sorted = Vec::with_capacity(idx.len());
        for pass in [false, true] {
            for (t, m) in idx.chunks(3).zip(&mats) {
                if keyed(m) == pass {
                    sorted.extend_from_slice(t);
                }
            }
        }
        let opaque = mats.iter().filter(|m| !keyed(m)).count() as u32;
        let a = self.blas(cmd, &pos, &sorted, opaque)?;
        let index = kind | (self.mats.len() as u32 + 1);
        self.mats.extend(mats.into_iter().filter(keyed));
        Ok(a.map(|a| (a, index)))
    }

    fn barrier(&self, cmd: vk::CommandBuffer, src: (vk::PipelineStageFlags, vk::AccessFlags), dst: (vk::PipelineStageFlags, vk::AccessFlags)) {
        let b = [vk::MemoryBarrier::default().src_access_mask(src.1).dst_access_mask(dst.1)];
        unsafe { self.device.cmd_pipeline_barrier(cmd, src.0, dst.0, vk::DependencyFlags::empty(), &b, &[], &[]) };
    }

    /// The acceleration structures of `frame` built (BLAS of what is new, the TLAS anew), recorded
    /// into `cmd` with the instances in buffer `slot` (free: its last use is done), ready for
    /// compute shaders.
    fn scene(&mut self, cmd: vk::CommandBuffer, slot: usize, frame: &Frame) -> Result<(), String> {
        let build = vk::PipelineStageFlags::ACCELERATION_STRUCTURE_BUILD_KHR;
        let (aw, ar) = (vk::AccessFlags::ACCELERATION_STRUCTURE_WRITE_KHR, vk::AccessFlags::ACCELERATION_STRUCTURE_READ_KHR);
        let compute = vk::PipelineStageFlags::COMPUTE_SHADER;
        if frame.generation != self.generation {
            // a new mission: its geometry is elsewhere
            unsafe { self.device.device_wait_idle().map_err(err("wait"))? };
            for (_, a) in std::mem::take(&mut self.blas) {
                if let Some((a, _)) = a {
                    self.free_accel(a);
                }
            }
            self.mats.clear();
            self.mats_sent = 0;
            // (freed BLAS addresses come back for others)
            self.refs.clear();
            self.generation = frame.generation;
        }
        // the mission's pages (the device is idle when they change: a new mission)
        if self.pages_buf.as_ref().is_none_or(|p| p.1 != std::sync::Arc::as_ptr(&frame.pages)) {
            unsafe { self.device.device_wait_idle().map_err(err("wait"))? };
            if let Some((b, _)) = self.pages_buf.take() {
                self.free(b);
            }
            let b = self.upload(&frame.pages[..], vk::BufferUsageFlags::STORAGE_BUFFER)?;
            self.pages_buf = Some((b, std::sync::Arc::as_ptr(&frame.pages)));
        }
        // the last picture's rays and builds are done with what is built now
        self.barrier(cmd, (build | compute, aw | ar | vk::AccessFlags::SHADER_READ), (build, aw | ar));
        let mut instances = Vec::new();
        let mut built = 0;
        for (sect, b) in &frame.blocks {
            let key = Key::Block(*sect);
            if !self.blas.contains_key(&key) {
                let a = self.mesh(cmd, super::scene::block_mesh(*sect, b), 0)?;
                self.blas.insert(key, a);
                built += 1;
            }
            if let Some(Some((a, index))) = self.blas.get(&key) {
                let o = b.origin.map(|x| x as f32);
                instances.push(instance([1.0, 0.0, 0.0, o[0], 0.0, 1.0, 0.0, o[1], 0.0, 0.0, 1.0, o[2]], *index, a.addr));
            }
        }
        for i in &frame.models {
            let key = Key::Model(i.list, i.first, i.count);
            if !self.blas.contains_key(&key) {
                let a = self.mesh(cmd, super::scene::model_mesh(i), MODEL)?;
                self.blas.insert(key, a);
                built += 1;
            }
            if let Some(Some((a, index))) = self.blas.get(&key) {
                instances.push(instance(model_transform(i), *index, a.addr));
            }
        }
        if built > 0 {
            self.barrier(cmd, (build, aw), (build, ar));
        }
        if let Some(t) = self.timing.as_mut().filter(|_| self.timed) {
            t.instances = (frame.blocks.len(), instances.len() - frame.blocks.len().min(instances.len()));
        }
        // new materials: behind those the rays in flight read, or all into a larger buffer
        if self.mat_buf.as_ref().is_none_or(|b| b.1 < self.mats.len()) {
            unsafe { self.device.device_wait_idle().map_err(err("wait"))? };
            if let Some((b, _)) = self.mat_buf.take() {
                self.free(b);
            }
            let cap = self.mats.len().max(4096).next_power_of_two();
            self.mat_buf = Some((self.buffer((cap * 8) as u64, vk::BufferUsageFlags::STORAGE_BUFFER, true)?, cap));
            self.mats_sent = 0;
        }
        if self.mats_sent < self.mats.len() {
            let (b, _) = self.mat_buf.as_ref().unwrap();
            let new = &self.mats[self.mats_sent..];
            unsafe { std::ptr::copy_nonoverlapping(new.as_ptr() as *const u8, b.ptr.add(self.mats_sent * 8), new.len() * 8) };
            self.mats_sent = self.mats.len();
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
        if let Some(t) = self.timing.as_ref().filter(|_| self.timed) {
            unsafe { self.device.cmd_write_timestamp(cmd, vk::PipelineStageFlags::ALL_COMMANDS, t.pool, 3) };
        }
        // The TLAS in the same storage each picture: refitted (UPDATE: new transforms, the tree
        // kept) while the instances name the same BLAS in the same order as at its last build,
        // which they mostly do (the game's lists keep their order), else built anew; anew every
        // `REBUILD` pictures too, as moved things loosen a refitted tree. A larger one when it grew.
        // (A build took ~98 µs a view at the first mission's start, ~310 instances; a refit ~30.)
        let refs: Vec<u64> = instances.iter().map(|i| unsafe { i.acceleration_structure_reference.device_handle }).collect();
        let n = instances.len() as u32;
        let top = vk::AccelerationStructureTypeKHR::TOP_LEVEL;
        let flags = vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE | vk::BuildAccelerationStructureFlagsKHR::ALLOW_UPDATE;
        match &self.tlas {
            Some((t, scratch, cap)) if *cap >= instances.len() => {
                let geometries = [geometry];
                self.refits += 1;
                let update = self.refits < REBUILD && refs == self.refs;
                if !update {
                    self.refs = refs;
                    self.refits = 0;
                }
                let info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
                    .ty(top)
                    .flags(flags)
                    .mode(if update { vk::BuildAccelerationStructureModeKHR::UPDATE } else { vk::BuildAccelerationStructureModeKHR::BUILD })
                    .src_acceleration_structure(if update { t.accel } else { vk::AccelerationStructureKHR::null() })
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
                let mut tlas = self.build(cmd, top, flags, &[geometry], &[n], &[cap as u32], vec![])?;
                let scratch = tlas.bufs.pop().unwrap();
                (self.refs, self.refits) = (refs, 0);
                self.tlas = Some((tlas, scratch, cap));
                self.tlas_made += 1;
            }
        }
        self.barrier(cmd, (build, aw), (compute, ar));
        Ok(())
    }

    /// Descriptor set `k` (0 the debug picture's, 1 the sun's) for the TLAS and storage image
    /// `view`, written when either changed.
    fn set(&mut self, k: usize, view: vk::ImageView) -> vk::DescriptorSet {
        let (set, bound) = self.sets[k];
        let (pages, mats) = (self.pages_buf.as_ref().unwrap().0.buf, self.mat_buf.as_ref().unwrap().0.buf);
        let lights = self.light_buf.as_ref().unwrap().buf;
        let now = (self.tlas_made, view, pages, mats);
        if bound != Some(now) {
            let accels = [self.tlas.as_ref().unwrap().0.accel];
            let mut write_as = vk::WriteDescriptorSetAccelerationStructureKHR::default().acceleration_structures(&accels);
            let images = [vk::DescriptorImageInfo::default().image_view(view).image_layout(vk::ImageLayout::GENERAL)];
            let page_info = [vk::DescriptorBufferInfo::default().buffer(pages).range(vk::WHOLE_SIZE)];
            let mat_info = [vk::DescriptorBufferInfo::default().buffer(mats).range(vk::WHOLE_SIZE)];
            let light_info = [vk::DescriptorBufferInfo::default().buffer(lights).range(vk::WHOLE_SIZE)];
            let writes = [
                vk::WriteDescriptorSet::default().dst_set(set).dst_binding(0).descriptor_type(vk::DescriptorType::ACCELERATION_STRUCTURE_KHR).descriptor_count(1).push_next(&mut write_as),
                vk::WriteDescriptorSet::default().dst_set(set).dst_binding(1).descriptor_type(vk::DescriptorType::STORAGE_IMAGE).image_info(&images),
                vk::WriteDescriptorSet::default().dst_set(set).dst_binding(2).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&page_info),
                vk::WriteDescriptorSet::default().dst_set(set).dst_binding(3).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&mat_info),
                vk::WriteDescriptorSet::default().dst_set(set).dst_binding(4).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&light_info),
            ];
            unsafe { self.device.update_descriptor_sets(&writes, &[]) };
            self.sets[k].1 = Some(now);
        }
        set
    }

    /// The sun's shadow of a view (`frame`) into `view`, an RGBA32F storage image of w x h in
    /// GENERAL layout holding the view's depth (R): G = 1 where the ray toward the sun (`sun`,
    /// camera space with x narrowed by widescreen's k, as the GL passes have it) is blocked.
    /// The rays toward `lights` (place in that space, reach; the first `LIGHTS`) too, from
    /// pixels within their reach: G gets 2 * a bit per light whose ray is blocked. With `ao` the
    /// AO's rays (`ao::rt_rays`, as long as its radius): B the share open.
    /// Recorded into `cmd`; the one recorded before is done.
    #[allow(clippy::too_many_arguments)]
    pub fn shadow(&mut self, cmd: vk::CommandBuffer, view: vk::ImageView, w: u32, h: u32, frame: &Frame, sun: [f32; 3], lights: &[[f32; 4]], ao: bool) -> Result<(), String> {
        // (the rays recorded before are done: the buffer is free)
        let n = lights.len().min(LIGHTS);
        let mut data = [0f32; 4 * (1 + LIGHTS)];
        data[0] = n as f32;
        data[1] = LIGHT_SKIP;
        for (i, l) in lights[..n].iter().enumerate() {
            data[4 + 4 * i..8 + 4 * i].copy_from_slice(l);
        }
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, self.light_buf.as_ref().unwrap().ptr, LIGHT_BYTES as usize) };
        self.timed = super::debug::flag(super::debug::V::RtTime);
        if !self.timed && let Some(t) = &mut self.timing {
            // (turned off: the last times are never read)
            t.pending = false;
        }
        if let Some(t) = self.timing.as_mut().filter(|_| self.timed) {
            unsafe { t.begin(&self.device, cmd) };
        }
        self.scene(cmd, 2, frame)?;
        if let Some(t) = self.timing.as_ref().filter(|_| self.timed) {
            unsafe { self.device.cmd_write_timestamp(cmd, vk::PipelineStageFlags::ALL_COMMANDS, t.pool, 1) };
        }
        let set = self.set(1, view);
        let mut push = camera_push(frame, h);
        push[24..27].copy_from_slice(&[sun[0] / frame.k, sun[1], sun[2]]);
        let (radius, rays) = soft();
        push[28] = radius.to_radians().tan();
        push[29] = rays as f32;
        if ao {
            push[30] = super::ao::RADIUS;
            push[31] = super::ao::rt_rays() as f32;
        }
        unsafe {
            let d = &self.device;
            d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, self.shadow);
            d.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::COMPUTE, self.layout, 0, &[set], &[]);
            d.cmd_push_constants(cmd, self.layout, vk::ShaderStageFlags::COMPUTE, 0, std::slice::from_raw_parts(push.as_ptr() as *const u8, PUSH as usize));
            d.cmd_dispatch(cmd, w.div_ceil(8), h.div_ceil(8), 1);
            if ao {
                // the AO's noise blurred: across (B into A), then down (A into B)
                let compute = vk::PipelineStageFlags::COMPUTE_SHADER;
                let barrier = [vk::MemoryBarrier::default().src_access_mask(vk::AccessFlags::SHADER_WRITE).dst_access_mask(vk::AccessFlags::SHADER_READ | vk::AccessFlags::SHADER_WRITE)];
                d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, self.ao_blur);
                for dir in [[1i32, 0], [0, 1]] {
                    d.cmd_pipeline_barrier(cmd, compute, compute, vk::DependencyFlags::empty(), &barrier, &[], &[]);
                    d.cmd_push_constants(cmd, self.layout, vk::ShaderStageFlags::COMPUTE, 0, std::slice::from_raw_parts(dir.as_ptr() as *const u8, 8));
                    d.cmd_dispatch(cmd, w.div_ceil(8), h.div_ceil(8), 1);
                }
            }
            if let Some(t) = self.timing.as_ref().filter(|_| self.timed) {
                d.cmd_write_timestamp(cmd, vk::PipelineStageFlags::COMPUTE_SHADER, t.pool, 2);
            }
        }
        Ok(())
    }

    /// The picture of `frame` into an image of w x h, recorded into `cmd` (picture in flight
    /// `slot`); the image is left in TRANSFER_SRC_OPTIMAL for the present to copy.
    pub fn record(&mut self, cmd: vk::CommandBuffer, slot: usize, w: u32, h: u32, frame: &Frame) -> Result<vk::Image, String> {
        let compute = vk::PipelineStageFlags::COMPUTE_SHADER;
        self.timed = false;
        self.scene(cmd, slot, frame)?;
        if self.image.as_ref().is_none_or(|i| (i.3, i.4) != (w, h)) {
            self.make_image(w, h)?;
        }
        let (image, _, view, _, _) = *self.image.as_ref().unwrap();
        let set = self.set(0, view);
        let range = vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1);
        let to = |old, new, src, dst| [vk::ImageMemoryBarrier::default().image(image).old_layout(old).new_layout(new).src_access_mask(src).dst_access_mask(dst).subresource_range(range)];
        let mut push = camera_push(frame, h);
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
            d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, self.debug);
            d.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::COMPUTE, self.layout, 0, &[set], &[]);
            let bytes = std::slice::from_raw_parts(push.as_ptr() as *const u8, PUSH as usize);
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
            Ok(())
        }
    }
}

/// The shaders' push constants for the camera of `frame` and a picture h pixels high: camera
/// space to world (mat4 of columns), the camera's place, focal length and widescreen's k; the
/// rest (from 22) is each shader's own.
fn camera_push(frame: &Frame, h: u32) -> [f32; 32] {
    let (c, cp) = frame.camera;
    let mut push = [0f32; 32];
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
    push
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

/// GPU timestamps around the sun's rays (`FCOP_RT_TIME=1`): the scene's build, the rays.
struct Timing {
    pool: vk::QueryPool,
    /// ns a tick
    period: f64,
    /// timestamps written by the last recording (done when the next one starts: `sun_rays` waits)
    pending: bool,
    sum: [f64; 3],
    count: u32,
    /// the last scene's terrain blocks and model instances
    instances: (usize, usize),
    since: std::time::Instant,
}

impl Timing {
    /// Takes the last recording's times and starts this one's.
    unsafe fn begin(&mut self, d: &ash::Device, cmd: vk::CommandBuffer) {
        unsafe {
            if self.pending {
                let mut t = [0u64; 4];
                if d.get_query_pool_results(self.pool, 0, &mut t, vk::QueryResultFlags::TYPE_64).is_ok() {
                    self.sum[0] += (t[1] - t[0]) as f64 * self.period;
                    self.sum[1] += (t[2] - t[1]) as f64 * self.period;
                    self.sum[2] += (t[1] - t[3]) as f64 * self.period;
                    self.count += 1;
                }
            }
            if self.since.elapsed().as_secs_f64() >= 2.0 && self.count > 0 {
                let n = self.count as f64 * 1000.0;
                super::log!("raytrace: µs a view: scene {:.0} (TLAS {:.0}), sun's rays {:.0} ({} blocks, {} models)", self.sum[0] / n, self.sum[2] / n, self.sum[1] / n, self.instances.0, self.instances.1);
                (self.sum, self.count, self.since) = ([0.0; 3], 0, std::time::Instant::now());
            }
            d.cmd_reset_query_pool(cmd, self.pool, 0, 4);
            d.cmd_write_timestamp(cmd, vk::PipelineStageFlags::TOP_OF_PIPE, self.pool, 0);
            self.pending = true;
        }
    }
}

/// Soft edges of the sun's shadow: the sun disc's radius in degrees (0 hard) and the rays a pixel
/// shoots into it, as chosen in Advanced Graphics (`shadows::rt_choice`; applies at once; default
/// 1 degree and 4: the real sun's radius is 0.27 degrees, nearly as hard as one ray; the noise
/// between them is filtered in the AO's last pass). `FCOP_RT_SOFT=<degrees>`, `FCOP_RT_RAYS=<n>`
/// (1...16) override (tests).
fn soft() -> (f32, u32) {
    static ENV: std::sync::OnceLock<(Option<f32>, Option<u32>)> = std::sync::OnceLock::new();
    let env = *ENV.get_or_init(|| {
        let var = |n: &str| std::env::var(n).ok().and_then(|v| v.parse::<f32>().ok());
        (var("FCOP_RT_SOFT").map(|r| r.max(0.0)), var("FCOP_RT_RAYS").map(|n| (n as u32).clamp(1, 16)))
    });
    let (s, r) = super::shadows::rt_choice();
    let radius = env.0.unwrap_or(super::shadows::RT_SOFTNESS[s].0);
    let rays = if radius == 0.0 { 1 } else { env.1.unwrap_or(super::shadows::RT_RAYS[r].0) };
    (radius, rays)
}

/// `FCOP_RT_HIGH=<model units>` (a test): the rays' picture from that high above the camera.
fn high() -> Option<f32> {
    Some(super::debug::num(super::debug::V::RtHigh)).filter(|&h| h > 0.0)
}
