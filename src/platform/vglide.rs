//! Glide's GPU side on the Vulkan renderer (PLAN-RTX.md step 6, `vkr.rs`): what `glide.rs`'s
//! `GlBack` does with OpenGL. The target (RGBA8, RGBA16F with tone mapping) drawn into, with
//! MSAA a multisampled one resolved at grBufferSwap; with the depth wanted the G-buffer
//! attachment (RG32F: depth, mask) and the depth buffer (D32F). The textures in two arrays
//! (rgb and coverage, the alpha) with mipmaps made by blits of the layers downloaded into;
//! the replacements in a third. Batches go into a vertex ring per picture in flight; one
//! pipeline per blending, depth and attachment state, made when first needed; the rest of a
//! batch's state as push constants. Vulkan's y points down: the target's first row is its top.

use super::glide::{Draw, HD_LAYERS, LAYERS, LEVELS, VERTEX};
use super::vkr::{Attachments, Buf, Img, Trash, Vkr, err, graphics_pipeline};
use ash::vk;
use std::collections::HashMap;

const VERT: &[u8] = include_bytes!("../../shaders/glide_vert.spv");
const FRAG: &[u8] = include_bytes!("../../shaders/glide_frag.spv");

/// The vertex ring of a picture in flight.
const RING: u64 = 16 << 20;

/// What a pipeline is made for.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    /// GL's blend factors, None: off
    blend: Option<(u32, u32)>,
    /// the depth attachment's writes (`Draw::gmode`); None without the attachment
    gmode: Option<i32>,
    /// the depth buffer: 0 none (or not attached), 1 tested, 2 the depth pass (written, no colour)
    depth: u8,
    points: bool,
    samples: u32,
    hdr: bool,
    attached: bool,
}

/// The target and what is drawn with it.
struct Target {
    w: u32,
    h: u32,
    hdr: bool,
    samples: u32,
    /// the picture shown (resolved into with MSAA)
    colour: Img,
    /// with MSAA: the picture drawn into
    ms: Option<Img>,
    /// the depth attachment and the depth buffer, when the depth is wanted
    gbuf: Option<(Img, Img)>,
}

pub struct Gpu {
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipelines: HashMap<Key, vk::Pipeline>,
    array: Img,
    alpha: Img,
    /// replacements (`hd.rs`), or a 1x1 stand-in
    hd: Img,
    hd_scale: u32,
    /// layers downloaded into since their mipmaps were made (bit per layer)
    stale: u64,
    hd_stale: u64,
    sampler: vk::Sampler,
    aniso: f32,
    ring: [Buf; 2],
    /// the picture the ring's offset is for, and the offset
    ring_at: (u64, u64),
    target: Option<Target>,
}

/// GL's blend factor as Vulkan's.
fn factor(f: u32) -> vk::BlendFactor {
    match f {
        glow::ZERO => vk::BlendFactor::ZERO,
        glow::SRC_COLOR => vk::BlendFactor::SRC_COLOR,
        glow::ONE_MINUS_SRC_COLOR => vk::BlendFactor::ONE_MINUS_SRC_COLOR,
        glow::DST_COLOR => vk::BlendFactor::DST_COLOR,
        glow::ONE_MINUS_DST_COLOR => vk::BlendFactor::ONE_MINUS_DST_COLOR,
        glow::SRC_ALPHA => vk::BlendFactor::SRC_ALPHA,
        glow::ONE_MINUS_SRC_ALPHA => vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        glow::DST_ALPHA => vk::BlendFactor::DST_ALPHA,
        glow::ONE_MINUS_DST_ALPHA => vk::BlendFactor::ONE_MINUS_DST_ALPHA,
        _ => vk::BlendFactor::ONE,
    }
}

fn colour_format(hdr: bool) -> vk::Format {
    if hdr { vk::Format::R16G16B16A16_SFLOAT } else { vk::Format::R8G8B8A8_UNORM }
}

const GBUF: vk::Format = vk::Format::R32G32_SFLOAT;
const DEPTH: vk::Format = vk::Format::D32_SFLOAT;

impl Gpu {
    pub fn open(v: &mut Vkr) -> Result<Gpu, String> {
        unsafe {
            let d = &v.device;
            let bindings: Vec<_> = (0..3)
                .map(|b| {
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(b)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .descriptor_count(1)
                        .stage_flags(vk::ShaderStageFlags::FRAGMENT)
                })
                .collect();
            let set_layout = d
                .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().flags(vk::DescriptorSetLayoutCreateFlags::PUSH_DESCRIPTOR_KHR).bindings(&bindings), None)
                .map_err(err("descriptor set layout"))?;
            let ranges = [vk::PushConstantRange::default().stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT).size(80)];
            let sets = [set_layout];
            let layout = d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&sets).push_constant_ranges(&ranges), None).map_err(err("pipeline layout"))?;
            let usage = vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC;
            let one = vk::SampleCountFlags::TYPE_1;
            let array = v.image(256, 256, vk::Format::R8G8B8A8_UNORM, usage, LEVELS as u32, LAYERS, one, true)?;
            let alpha = v.image(256, 256, vk::Format::R8G8_UNORM, usage, LEVELS as u32, LAYERS, one, true)?;
            let hd_scale = super::hd::scale();
            let hd = match hd_scale {
                0 => v.image(1, 1, vk::Format::R8G8B8A8_UNORM, usage, 1, 1, one, true)?,
                k => v.image(256 * k, 256 * k, vk::Format::R8G8B8A8_UNORM, usage, LEVELS as u32 + k.trailing_zeros(), HD_LAYERS, one, true)?,
            };
            // all empty to start with
            for img in [&array, &alpha, &hd] {
                v.transition(img, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
                let cmd = v.cmd();
                let range = vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(img.levels).layer_count(img.layers);
                v.device.cmd_clear_color_image(cmd, img.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &vk::ClearColorValue { float32: [0.0; 4] }, &[range]);
                v.transition(img, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
            }
            let ring = [v.buffer(RING, vk::BufferUsageFlags::VERTEX_BUFFER, true)?, v.buffer(RING, vk::BufferUsageFlags::VERTEX_BUFFER, true)?];
            let sampler = sampler(v, 1.0)?;
            Ok(Gpu {
                set_layout,
                layout,
                pipelines: HashMap::new(),
                array,
                alpha,
                hd,
                hd_scale,
                stale: 0,
                hd_stale: 0,
                sampler,
                aniso: 1.0,
                ring,
                ring_at: (u64::MAX, 0),
                target: None,
            })
        }
    }

    pub fn has_depth(&self) -> bool {
        self.target.as_ref().is_some_and(|t| t.gbuf.is_some())
    }

    pub fn has_hd(&self) -> bool {
        self.hd_scale != 0
    }

    /// The target at w x h, its format, samples and depth as wanted (made anew when one changed).
    pub fn resize(&mut self, v: &mut Vkr, (w, h): (u32, u32), hdr: bool, samples: u32, depth: bool) -> Result<(), String> {
        // the most samples the device has for every attachment, at most those wanted
        let l = &v.limits;
        let can = l.framebuffer_color_sample_counts & l.framebuffer_depth_sample_counts;
        let samples = [8u32, 4, 2, 1].into_iter().find(|&n| n <= samples && can.contains(vk::SampleCountFlags::from_raw(n))).unwrap_or(1);
        if let Some(t) = &self.target {
            if (t.w, t.h, t.hdr, t.samples, t.gbuf.is_some()) == (w, h, hdr, samples, depth) {
                return Ok(());
            }
        }
        v.end_rendering();
        if let Some(t) = self.target.take() {
            v.trash(Trash::Img(t.colour));
            if let Some(m) = t.ms {
                v.trash(Trash::Img(m));
            }
            if let Some((g, z)) = t.gbuf {
                v.trash(Trash::Img(g));
                v.trash(Trash::Img(z));
            }
        }
        let one = vk::SampleCountFlags::TYPE_1;
        let n = vk::SampleCountFlags::from_raw(samples);
        let attach = vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC;
        let colour = v.image(w, h, colour_format(hdr), attach | vk::ImageUsageFlags::SAMPLED, 1, 1, one, false)?;
        let ms = if samples > 1 { Some(v.image(w, h, colour_format(hdr), attach, 1, 1, n, false)?) } else { None };
        let gbuf = if depth {
            let g = v.image(w, h, GBUF, attach | vk::ImageUsageFlags::SAMPLED, 1, 1, n, false)?;
            let z = v.image(w, h, DEPTH, vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED, 1, 1, n, false)?;
            Some((g, z))
        } else {
            None
        };
        let t = Target { w, h, hdr, samples, colour, ms, gbuf };
        // black, and the depth emptied
        for img in [Some(&t.colour), t.ms.as_ref()].into_iter().flatten() {
            clear_colour(v, img, [0.0, 0.0, 0.0, 1.0]);
        }
        clear_depth(v, &t);
        // shown until the first picture of its own
        v.transition(&t.colour, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        v.show_scene(t.colour.view, w, h, 1.0);
        self.target = Some(t);
        Ok(())
    }

    /// Rows of a texture layer: rgb premultiplied by coverage and the coverage, and the alpha.
    #[allow(clippy::too_many_arguments)]
    pub fn upload(&mut self, v: &mut Vkr, layer: u32, r0: u32, w: u32, rows: u32, rgb: &[u8], alpha: &[u8]) {
        for (img, px) in [(&self.array, rgb), (&self.alpha, alpha)] {
            if let Err(e) = v.upload(img, layer, (0, r0, w, rows), px) {
                super::log!("error: vulkan: texture: {e}");
            }
        }
        self.stale |= 1 << layer;
    }

    /// A replacement (straight alpha) into rows from `r0` (of the original) of `hd_layer`.
    pub fn hd_put(&mut self, v: &mut Vkr, hd_layer: u32, r0: u32, img: &super::hd::Image) {
        let px: Vec<u8> = img
            .rgba
            .chunks(4)
            .flat_map(|p| {
                let a = p[3] as u32;
                let m = |c: u8| ((c as u32 * a + 127) / 255) as u8;
                [m(p[0]), m(p[1]), m(p[2]), p[3]]
            })
            .collect();
        if let Err(e) = v.upload(&self.hd, hd_layer, (0, r0 * self.hd_scale, img.w, img.h), &px) {
            super::log!("error: vulkan: replacement: {e}");
        }
        self.hd_stale |= 1 << hd_layer;
    }

    /// Rows `r0..=r1` of the original texture (`w` wide, in `layer`) scaled up into the
    /// replacement's `hd_layer`.
    pub fn hd_scale_up(&mut self, v: &mut Vkr, layer: u32, hd_layer: u32, w: u32, (r0, r1): (u32, u32)) {
        let k = self.hd_scale;
        v.transition(&self.array, vk::ImageLayout::GENERAL);
        v.transition(&self.hd, vk::ImageLayout::GENERAL);
        let cmd = v.cmd();
        let sub = |l| vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).base_array_layer(l).layer_count(1);
        let o = |x: u32, y: u32| vk::Offset3D { x: x as i32, y: y as i32, z: 0 };
        let o1 = |x: u32, y: u32| vk::Offset3D { x: x as i32, y: y as i32, z: 1 };
        let blit = vk::ImageBlit::default()
            .src_subresource(sub(layer))
            .src_offsets([o(0, r0), o1(w, r1 + 1)])
            .dst_subresource(sub(hd_layer))
            .dst_offsets([o(0, r0 * k), o1(w * k, (r1 + 1) * k)]);
        unsafe { v.device.cmd_blit_image(cmd, self.array.image, vk::ImageLayout::GENERAL, self.hd.image, vk::ImageLayout::GENERAL, &[blit], vk::Filter::LINEAR) };
        self.hd_stale |= 1 << hd_layer;
    }

    /// The pipeline for a key, made at its first use.
    fn pipeline(&mut self, v: &Vkr, k: Key) -> Result<vk::Pipeline, String> {
        if let Some(&p) = self.pipelines.get(&k) {
            return Ok(p);
        }
        let all = vk::ColorComponentFlags::RGBA;
        let none = vk::ColorComponentFlags::empty();
        let blend = |b: Option<(u32, u32)>, mask| match b {
            None => vk::PipelineColorBlendAttachmentState::default().color_write_mask(mask),
            Some((s, d)) => vk::PipelineColorBlendAttachmentState::default()
                .blend_enable(true)
                .src_color_blend_factor(factor(s))
                .dst_color_blend_factor(factor(d))
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(factor(s))
                .dst_alpha_blend_factor(factor(d))
                .alpha_blend_op(vk::BlendOp::ADD)
                .color_write_mask(mask),
        };
        let depth_pass = k.depth == 2;
        let mut blends = vec![blend(k.blend, if depth_pass { none } else { all })];
        let mut formats = vec![colour_format(k.hdr)];
        if let Some(g) = k.gmode {
            // as GL's per-buffer masks: depth (R) and mask (A) blended as the colour, the
            // additive and blended ones' mask (G) not blended, the rest untouched
            let (b, mask) = match g {
                _ if depth_pass => (None, none),
                1 => (k.blend, vk::ColorComponentFlags::R | vk::ColorComponentFlags::A),
                2 | 3 => (None, vk::ColorComponentFlags::G),
                _ => (None, none),
            };
            blends.push(blend(b, mask));
            formats.push(GBUF);
        }
        let ds = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(k.depth != 0)
            .depth_write_enable(depth_pass)
            .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
        let vb = [vk::VertexInputBindingDescription::default().binding(0).stride(4 * VERTEX as u32).input_rate(vk::VertexInputRate::VERTEX)];
        let a = |location, format, offset| vk::VertexInputAttributeDescription::default().location(location).binding(0).format(format).offset(offset);
        let f2 = vk::Format::R32G32_SFLOAT;
        let f4 = vk::Format::R32G32B32A32_SFLOAT;
        let attrs = [
            a(0, f2, 0),
            a(1, f4, 8),
            a(2, f2, 24),
            a(3, f4, 32),
            a(4, vk::Format::R32G32B32A32_UINT, 48),
            a(5, f4, 64),
            a(6, vk::Format::R32_SFLOAT, 80),
            a(7, vk::Format::R32_UINT, 84),
        ];
        let topology = if k.points { vk::PrimitiveTopology::POINT_LIST } else { vk::PrimitiveTopology::TRIANGLE_LIST };
        let depth_format = if k.attached { DEPTH } else { vk::Format::UNDEFINED };
        let p = graphics_pipeline(&v.device, self.layout, VERT, FRAG, &vb, &attrs, topology, &blends, &formats, depth_format, vk::SampleCountFlags::from_raw(k.samples), Some(ds))?;
        self.pipelines.insert(k, p);
        Ok(p)
    }

    /// Mipmaps for the layers in `mask` of `img`, by blits level to level; then all of it is
    /// ready to sample.
    fn mipmaps(v: &mut Vkr, img: &Img, mask: u64) {
        v.transition(img, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
        let cmd = v.cmd();
        let d = &v.device;
        let barrier = |level: u32, layer: u32, from, to, src, dst| {
            vk::ImageMemoryBarrier::default()
                .image(img.image)
                .old_layout(from)
                .new_layout(to)
                .src_access_mask(src)
                .dst_access_mask(dst)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .subresource_range(vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).base_mip_level(level).level_count(1).base_array_layer(layer).layer_count(1))
        };
        let t = vk::PipelineStageFlags::TRANSFER;
        let (dst_l, src_l) = (vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
        let (w_acc, r_acc) = (vk::AccessFlags::TRANSFER_WRITE, vk::AccessFlags::TRANSFER_READ);
        for layer in (0..img.layers).filter(|l| mask >> l & 1 != 0) {
            for level in 1..img.levels {
                let (sw, sh) = ((img.w >> (level - 1)).max(1) as i32, (img.h >> (level - 1)).max(1) as i32);
                let (dw, dh) = ((img.w >> level).max(1) as i32, (img.h >> level).max(1) as i32);
                unsafe {
                    d.cmd_pipeline_barrier(cmd, t, t, vk::DependencyFlags::empty(), &[], &[], &[barrier(level - 1, layer, dst_l, src_l, w_acc, r_acc)]);
                    let sub = |l| vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).mip_level(l).base_array_layer(layer).layer_count(1);
                    let blit = vk::ImageBlit::default()
                        .src_subresource(sub(level - 1))
                        .src_offsets([vk::Offset3D::default(), vk::Offset3D { x: sw, y: sh, z: 1 }])
                        .dst_subresource(sub(level))
                        .dst_offsets([vk::Offset3D::default(), vk::Offset3D { x: dw, y: dh, z: 1 }]);
                    d.cmd_blit_image(cmd, img.image, src_l, img.image, dst_l, &[blit], vk::Filter::LINEAR);
                    // back, so that the whole image is in one layout again
                    d.cmd_pipeline_barrier(cmd, t, t, vk::DependencyFlags::empty(), &[], &[], &[barrier(level - 1, layer, src_l, dst_l, r_acc, w_acc)]);
                }
            }
        }
        v.transition(img, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
    }

    /// The attachments drawn into, rendering into them begun.
    fn begin(&self, v: &mut Vkr) -> Option<()> {
        let t = self.target.as_ref()?;
        let drawn = t.ms.as_ref().unwrap_or(&t.colour);
        let (g, z) = match &t.gbuf {
            Some((g, z)) => (g.view, z.view),
            None => (vk::ImageView::null(), vk::ImageView::null()),
        };
        let a = Attachments { colour: [drawn.view, g], depth: z, w: t.w, h: t.h };
        let mut imgs = vec![drawn];
        if let Some((g, z)) = &t.gbuf {
            imgs.push(g);
            imgs.push(z);
        }
        v.begin_rendering(a, &imgs);
        Some(())
    }

    /// The scissor box of a drawing in Vulkan's terms (from the top), None when empty.
    fn scissor(d: &Draw) -> Option<vk::Rect2D> {
        let [x, y, w, h] = d.scissor;
        let (w, h) = (w.max(0) as u32, h.max(0) as u32);
        let top = d.target.1 as i32 - y - h as i32;
        let x0 = x.max(0);
        let y0 = top.max(0);
        let w = (w as i32 - (x0 - x)).min(d.target.0 as i32 - x0);
        let h = (h as i32 - (y0 - top)).min(d.target.1 as i32 - y0);
        (w > 0 && h > 0).then(|| vk::Rect2D { offset: vk::Offset2D { x: x0, y: y0 }, extent: vk::Extent2D { width: w as u32, height: h as u32 } })
    }

    /// Clears the clip window's box: the colour, and the depth attachment and buffer.
    pub(super) fn clear(&mut self, v: &mut Vkr, d: &Draw, colour: [f32; 4]) {
        let Some(rect) = Self::scissor(d) else { return };
        if self.begin(v).is_none() {
            return;
        }
        let cmd = v.cmd();
        let mut clears = vec![vk::ClearAttachment { aspect_mask: vk::ImageAspectFlags::COLOR, color_attachment: 0, clear_value: vk::ClearValue { color: vk::ClearColorValue { float32: colour } } }];
        if self.has_depth() {
            clears.push(vk::ClearAttachment { aspect_mask: vk::ImageAspectFlags::COLOR, color_attachment: 1, clear_value: vk::ClearValue { color: vk::ClearColorValue { float32: [0.0; 4] } } });
            clears.push(vk::ClearAttachment { aspect_mask: vk::ImageAspectFlags::DEPTH, color_attachment: 0, clear_value: vk::ClearValue { depth_stencil: vk::ClearDepthStencilValue { depth: 1.0, stencil: 0 } } });
        }
        let r = vk::ClearRect { rect, base_array_layer: 0, layer_count: 1 };
        unsafe { v.device.cmd_clear_attachments(cmd, &clears, &[r]) };
    }

    /// Draws a batch.
    pub(super) fn draw(&mut self, v: &mut Vkr, d: &Draw, batch: &[u32]) {
        let Some(t) = &self.target else { return };
        let Some(rect) = Self::scissor(d) else { return };
        let (samples, hdr, attached) = (t.samples, t.hdr, t.gbuf.is_some());
        // the textures: mipmaps of the layers downloaded into (replacements always have them)
        let levels = d.mipmaps || self.hd_scale != 0;
        if levels && self.stale != 0 {
            Self::mipmaps(v, &self.array, self.stale);
            Self::mipmaps(v, &self.alpha, self.stale);
            self.stale = 0;
        }
        if self.hd_stale != 0 {
            Self::mipmaps(v, &self.hd, self.hd_stale);
            self.hd_stale = 0;
        }
        for img in [&self.array, &self.alpha, &self.hd] {
            v.transition(img, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        }
        let aniso = if v.limits.max_sampler_anisotropy > 1.0 { d.aniso.min(v.limits.max_sampler_anisotropy) } else { 1.0 };
        if aniso != self.aniso {
            match sampler(v, aniso) {
                Ok(s) => {
                    v.trash(Trash::Sampler(std::mem::replace(&mut self.sampler, s)));
                    self.aniso = aniso;
                }
                Err(e) => super::log!("error: vulkan: {e}"),
            }
        }
        // the vertices into this picture's ring (begun first: the ring was free when its fence was)
        v.cmd();
        let bytes = unsafe { std::slice::from_raw_parts(batch.as_ptr() as *const u8, batch.len() * 4) };
        if self.ring_at.0 != v.pictures {
            self.ring_at = (v.pictures, 0);
        }
        let at = self.ring_at.1;
        if at + bytes.len() as u64 > RING {
            super::log!("warn: vulkan: Glide's vertex ring is full this picture");
            return;
        }
        let ring = &self.ring[(v.pictures & 1) as usize];
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ring.ptr.add(at as usize), bytes.len()) };
        self.ring_at.1 = (at + bytes.len() as u64).next_multiple_of(4 * VERTEX as u64);
        let ring = ring.buf;
        let key = |depth| Key {
            blend: if depth == 2 { None } else { d.blend },
            gmode: attached.then_some(d.gmode),
            depth: if attached { depth } else { 0 },
            points: d.topology == glow::POINTS,
            samples,
            hdr,
            attached,
        };
        let colour_pass = match self.pipeline(v, key(if d.zk != 0 { 1 } else { 0 })) {
            Ok(p) => p,
            Err(e) => return super::log!("error: vulkan: {e}"),
        };
        let depth_pass = if d.zwrite && attached { self.pipeline(v, key(2)).ok() } else { None };
        if self.begin(v).is_none() {
            return;
        }
        let cmd = v.cmd();
        let dev = &v.device;
        let [vx, vy, vw, vh] = d.viewport;
        let mut pc = [0u32; 20];
        let f = |x: f32| x.to_bits();
        pc[0] = f(d.space[0]);
        pc[1] = f(d.space[1]);
        pc[2] = f(d.jitter[0]);
        pc[3] = f(d.jitter[1]);
        pc[4] = f(d.key[0]);
        pc[5] = f(d.key[1]);
        pc[6] = f(d.key[2]);
        pc[8] = d.clamp[0] as u32;
        pc[9] = d.clamp[1] as u32;
        pc[10] = d.mipmaps as u32;
        pc[11] = d.chroma as u32;
        pc[12] = d.linear as u32;
        pc[13] = d.edge as u32;
        pc[14] = d.gmode as u32;
        pc[15] = (d.zk != 0) as u32;
        pc[16] = f(if d.zk != 0 && !d.zwrite { super::zbuffer::TEST_BIAS } else { 0.0 });
        pc[17] = 0;
        pc[18] = d.show_effects as u32;
        let push = |pc: &[u32; 20]| unsafe {
            let b = std::slice::from_raw_parts(pc.as_ptr() as *const u8, 80);
            dev.cmd_push_constants(cmd, self.layout, vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT, 0, b);
        };
        let count = (batch.len() / VERTEX) as u32;
        unsafe {
            dev.cmd_set_viewport(cmd, 0, &[vk::Viewport { x: vx, y: vy, width: vw, height: vh, min_depth: 0.0, max_depth: 1.0 }]);
            dev.cmd_set_scissor(cmd, 0, &[rect]);
            let info = |view| [vk::DescriptorImageInfo::default().sampler(self.sampler).image_view(view).image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let (i0, i1, i2) = (info(self.array.view), info(self.alpha.view), info(self.hd.view));
            let ty = vk::DescriptorType::COMBINED_IMAGE_SAMPLER;
            let writes = [
                vk::WriteDescriptorSet::default().dst_binding(0).descriptor_type(ty).image_info(&i0),
                vk::WriteDescriptorSet::default().dst_binding(1).descriptor_type(ty).image_info(&i1),
                vk::WriteDescriptorSet::default().dst_binding(2).descriptor_type(ty).image_info(&i2),
            ];
            v.push_fn.cmd_push_descriptor_set(cmd, vk::PipelineBindPoint::GRAPHICS, self.layout, 0, &writes);
            dev.cmd_bind_vertex_buffers(cmd, 0, &[ring], &[at]);
            if let Some(p) = depth_pass {
                // first the depth alone, where the alpha is at least 0.75 when it counts
                // (keyed holes and soft edges leave what is behind visible)
                let mut z = pc;
                z[16] = f(-super::zbuffer::slack());
                z[17] = d.zonly as u32;
                dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, p);
                push(&z);
                dev.cmd_draw(cmd, count, 1, 0, 0);
            }
            dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, colour_pass);
            push(&pc);
            dev.cmd_draw(cmd, count, 1, 0, 0);
        }
    }

    /// grBufferSwap: the picture resolved (MSAA) into the image shown, the depth emptied for the
    /// next one; the image shown, ready to sample.
    pub fn finish(&mut self, v: &mut Vkr) -> Option<vk::ImageView> {
        let t = self.target.as_ref()?;
        v.end_rendering();
        if let Some(ms) = &t.ms {
            v.transition(ms, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
            v.transition(&t.colour, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
            let cmd = v.cmd();
            let sub = vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1);
            let region = vk::ImageResolve::default().src_subresource(sub).dst_subresource(sub).extent(vk::Extent3D { width: t.w, height: t.h, depth: 1 });
            unsafe { v.device.cmd_resolve_image(cmd, ms.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, t.colour.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[region]) };
        }
        clear_depth(v, t);
        v.transition(&t.colour, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        Some(t.colour.view)
    }

    pub fn close(&mut self, v: &mut Vkr) {
        v.end_rendering();
        if let Some(t) = self.target.take() {
            v.trash(Trash::Img(t.colour));
            if let Some(m) = t.ms {
                v.trash(Trash::Img(m));
            }
            if let Some((g, z)) = t.gbuf {
                v.trash(Trash::Img(g));
                v.trash(Trash::Img(z));
            }
        }
        for (_, p) in self.pipelines.drain() {
            v.trash(Trash::Pipeline(p));
        }
        let dummy = || Img {
            image: vk::Image::null(),
            memory: vk::DeviceMemory::null(),
            view: vk::ImageView::null(),
            w: 0,
            h: 0,
            format: vk::Format::UNDEFINED,
            levels: 0,
            layers: 0,
            samples: vk::SampleCountFlags::TYPE_1,
            layout: std::cell::Cell::new(vk::ImageLayout::UNDEFINED),
        };
        for img in [std::mem::replace(&mut self.array, dummy()), std::mem::replace(&mut self.alpha, dummy()), std::mem::replace(&mut self.hd, dummy())] {
            v.trash(Trash::Img(img));
        }
        for b in std::mem::replace(&mut self.ring, [dummy_buf(), dummy_buf()]) {
            v.trash(Trash::Buf(b));
        }
        v.trash(Trash::Sampler(self.sampler));
        // (the layouts are few and small: kept)
        let _ = self.set_layout;
    }
}

fn dummy_buf() -> Buf {
    Buf { buf: vk::Buffer::null(), memory: vk::DeviceMemory::null(), size: 0, ptr: std::ptr::null_mut() }
}

/// The textures' sampler: trilinear, clamped, with this anisotropy (1: none).
fn sampler(v: &Vkr, aniso: f32) -> Result<vk::Sampler, String> {
    unsafe {
        v.device
            .create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .anisotropy_enable(aniso > 1.0)
                    .max_anisotropy(aniso)
                    .max_lod(vk::LOD_CLAMP_NONE),
                None,
            )
            .map_err(err("sampler"))
    }
}

fn clear_colour(v: &mut Vkr, img: &Img, c: [f32; 4]) {
    v.transition(img, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
    let cmd = v.cmd();
    let range = vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1);
    unsafe { v.device.cmd_clear_color_image(cmd, img.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &vk::ClearColorValue { float32: c }, &[range]) };
}

/// The depth attachment and buffer emptied (no depth: nothing to occlude).
fn clear_depth(v: &mut Vkr, t: &Target) {
    let Some((g, z)) = &t.gbuf else { return };
    clear_colour(v, g, [0.0; 4]);
    v.transition(z, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
    let cmd = v.cmd();
    let range = vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::DEPTH).level_count(1).layer_count(1);
    unsafe { v.device.cmd_clear_depth_stencil_image(cmd, z.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &vk::ClearDepthStencilValue { depth: 1.0, stencil: 0 }, &[range]) };
}
