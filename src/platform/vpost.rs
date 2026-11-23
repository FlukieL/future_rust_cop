//! The post passes on the Vulkan renderer (PLAN-RTX.md step 6): what `glide::post_3d` runs over
//! a view after its 3D picture, as GL's passes do. Each pass is a fragment shader over a triangle
//! covering the target, cut to the view's box by the scissor; its inputs as push descriptors
//! (bindings 0... of one layout), its values as push constants. `copy` takes the picture drawn
//! so far (resolved when multisampled) for passes that read it while drawing over it.

use super::vkr::{Attachments, Img, Trash, Vkr, err, graphics_pipeline};
use ash::vk;
use std::collections::HashMap;

const FULL_VERT: &[u8] = include_bytes!("../../shaders/full_vert.spv");

/// The passes' fragment shaders.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Frag {
    Tonemap,
    /// the AO's passes (`ao.rs`); true: the depth multisampled
    AoNear(bool),
    Ao(bool),
    AoBlur(bool),
    AoApply(bool),
    Lights(bool),
}

impl Frag {
    fn spv(self) -> &'static [u8] {
        match self {
            Frag::Tonemap => include_bytes!("../../shaders/tonemap_frag.spv"),
            Frag::AoNear(false) => include_bytes!("../../shaders/ao_near_frag.spv"),
            Frag::AoNear(true) => include_bytes!("../../shaders/ao_near_ms_frag.spv"),
            Frag::Ao(false) => include_bytes!("../../shaders/ao_frag.spv"),
            Frag::Ao(true) => include_bytes!("../../shaders/ao_ms_frag.spv"),
            Frag::AoBlur(false) => include_bytes!("../../shaders/ao_blur_frag.spv"),
            Frag::AoBlur(true) => include_bytes!("../../shaders/ao_blur_ms_frag.spv"),
            Frag::AoApply(false) => include_bytes!("../../shaders/ao_apply_frag.spv"),
            Frag::AoApply(true) => include_bytes!("../../shaders/ao_apply_ms_frag.spv"),
            Frag::Lights(false) => include_bytes!("../../shaders/lights_frag.spv"),
            Frag::Lights(true) => include_bytes!("../../shaders/lights_ms_frag.spv"),
        }
    }
}

/// How a pass's output meets what is there.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Blend {
    /// replaced
    None,
    /// multiplied (the AO, the shadows)
    Multiply,
    /// added
    Add,
    /// the picture times 1 + the output
    MulAdd,
}

/// Inputs a pass may have.
const BINDINGS: u32 = 8;
/// Bytes of push constants a pass may have.
const PUSH: u32 = 128;

pub struct Post {
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipelines: HashMap<(Frag, vk::Format, u32, Blend), vk::Pipeline>,
    /// the picture as it was before a pass (`copy`)
    copy: Option<Img>,
}

impl Post {
    pub fn new(v: &Vkr) -> Result<Post, String> {
        unsafe {
            let d = &v.device;
            let bindings: Vec<_> = (0..BINDINGS)
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
            let ranges = [vk::PushConstantRange::default().stage_flags(vk::ShaderStageFlags::FRAGMENT).size(PUSH)];
            let sets = [set_layout];
            let layout = d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&sets).push_constant_ranges(&ranges), None).map_err(err("pipeline layout"))?;
            Ok(Post { set_layout, layout, pipelines: HashMap::new(), copy: None })
        }
    }

    /// A pass's pipeline drawing into `format` with `samples`, made at its first use.
    pub fn pipeline(&mut self, v: &Vkr, frag: Frag, format: vk::Format, samples: u32, blend: Blend) -> Result<vk::Pipeline, String> {
        let key = (frag, format, samples, blend);
        if let Some(&p) = self.pipelines.get(&key) {
            return Ok(p);
        }
        let b = vk::PipelineColorBlendAttachmentState::default().color_write_mask(vk::ColorComponentFlags::RGBA);
        let with = |src, dst| {
            b.blend_enable(true)
                .src_color_blend_factor(src)
                .dst_color_blend_factor(dst)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ZERO)
                .dst_alpha_blend_factor(vk::BlendFactor::ONE)
                .alpha_blend_op(vk::BlendOp::ADD)
        };
        let state = match blend {
            Blend::None => b,
            Blend::Multiply => with(vk::BlendFactor::DST_COLOR, vk::BlendFactor::ZERO),
            Blend::Add => with(vk::BlendFactor::ONE, vk::BlendFactor::ONE),
            Blend::MulAdd => with(vk::BlendFactor::DST_COLOR, vk::BlendFactor::ONE),
        };
        let p = graphics_pipeline(&v.device, self.layout, FULL_VERT, frag.spv(), &[], &[], vk::PrimitiveTopology::TRIANGLE_LIST, &[state], &[format], vk::Format::UNDEFINED, vk::SampleCountFlags::from_raw(samples), None)?;
        self.pipelines.insert(key, p);
        Ok(p)
    }

    /// `rect` of the picture in `drawn` (resolved when multisampled) into the copy, which is
    /// then ready to sample: its view.
    pub fn copy(&mut self, v: &mut Vkr, drawn: &Img, rect: vk::Rect2D) -> Result<vk::ImageView, String> {
        if self.copy.as_ref().is_none_or(|c| (c.w, c.h, c.format) != (drawn.w, drawn.h, drawn.format)) {
            let usage = vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST;
            let c = v.image(drawn.w, drawn.h, drawn.format, usage, 1, 1, vk::SampleCountFlags::TYPE_1, false)?;
            if let Some(old) = self.copy.replace(c) {
                v.trash(Trash::Img(old));
            }
        }
        let copy = self.copy.as_ref().unwrap();
        v.transition(drawn, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
        v.transition(copy, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
        let cmd = v.cmd();
        let sub = vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1);
        let at = vk::Offset3D { x: rect.offset.x, y: rect.offset.y, z: 0 };
        let extent = vk::Extent3D { width: rect.extent.width, height: rect.extent.height, depth: 1 };
        unsafe {
            if drawn.samples != vk::SampleCountFlags::TYPE_1 {
                let r = vk::ImageResolve::default().src_subresource(sub).src_offset(at).dst_subresource(sub).dst_offset(at).extent(extent);
                v.device.cmd_resolve_image(cmd, drawn.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, copy.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[r]);
            } else {
                let r = vk::ImageCopy::default().src_subresource(sub).src_offset(at).dst_subresource(sub).dst_offset(at).extent(extent);
                v.device.cmd_copy_image(cmd, drawn.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, copy.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[r]);
            }
        }
        v.transition(copy, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        Ok(copy.view)
    }

    /// Runs a pass into `into` inside `rect`: `inputs` (binding, view, sampler; images ready to
    /// sample), `push` its constants.
    pub fn draw(&self, v: &mut Vkr, pipeline: vk::Pipeline, into: &Img, rect: vk::Rect2D, inputs: &[(u32, vk::ImageView, vk::Sampler)], push: &[u8]) {
        let a = Attachments { colour: [into.view, vk::ImageView::null()], depth: vk::ImageView::null(), w: into.w, h: into.h };
        v.begin_rendering(a, &[into]);
        let cmd = v.cmd();
        let infos: Vec<[vk::DescriptorImageInfo; 1]> =
            inputs.iter().map(|&(_, view, s)| [vk::DescriptorImageInfo::default().sampler(s).image_view(view).image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)]).collect();
        let writes: Vec<vk::WriteDescriptorSet> = inputs
            .iter()
            .zip(&infos)
            .map(|(&(b, _, _), i)| vk::WriteDescriptorSet::default().dst_binding(b).descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER).image_info(i))
            .collect();
        unsafe {
            let d = &v.device;
            d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
            d.cmd_set_viewport(cmd, 0, &[vk::Viewport { x: 0.0, y: 0.0, width: into.w as f32, height: into.h as f32, min_depth: 0.0, max_depth: 1.0 }]);
            d.cmd_set_scissor(cmd, 0, &[rect]);
            if !writes.is_empty() {
                v.push_fn.cmd_push_descriptor_set(cmd, vk::PipelineBindPoint::GRAPHICS, self.layout, 0, &writes);
            }
            if !push.is_empty() {
                let mut p = push.to_vec();
                p.resize(p.len().next_multiple_of(4), 0);
                d.cmd_push_constants(cmd, self.layout, vk::ShaderStageFlags::FRAGMENT, 0, &p);
            }
            d.cmd_draw(cmd, 3, 1, 0, 0);
        }
    }

    pub fn close(&mut self, v: &mut Vkr) {
        for (_, p) in self.pipelines.drain() {
            v.trash(Trash::Pipeline(p));
        }
        if let Some(c) = self.copy.take() {
            v.trash(Trash::Img(c));
        }
        let _ = (self.set_layout, self.layout);
    }
}

/// Push constants from 32-bit values (ints and floats as their bits).
pub fn words(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|x| x.to_le_bytes()).collect()
}
