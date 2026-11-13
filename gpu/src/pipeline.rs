use crate::memory::format_aspects;
use crate::{BindingLayout, Device, Error, MAX_COLOR_ATTACHMENTS, MAX_PUSH_RANGES, MAX_SET_LAYOUTS, Result, fail_if, vk};
use std::ffi::CStr;
use std::sync::Arc;

const SPIRV_MAGIC: u32 = 0x0723_0203;

#[derive(Clone, Copy)]
pub struct ColorTarget {
    pub format: vk::Format,
    pub blend: vk::PipelineColorBlendAttachmentState,
}

impl ColorTarget {
    /// Writes RGBA without blending.
    pub fn opaque(format: vk::Format) -> Self {
        let blend = vk::PipelineColorBlendAttachmentState::default()
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ZERO)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::RGBA);
        Self { format, blend }
    }
}

/// Graphics pipeline for dynamic rendering. Viewport, scissor, depth and stencil
/// state are dynamic and set on the [`crate::Command`].
pub struct PipelineDesc<'a> {
    /// SPIR-V words (use `ash::util::read_spv` to load a file).
    pub vertex_spirv: &'a [u32],
    /// Empty for depth-only pipelines.
    pub fragment_spirv: &'a [u32],
    pub vertex_entry: &'a CStr,
    pub fragment_entry: &'a CStr,
    /// All targets must share one blend state (independent blending is not enabled).
    pub colors: &'a [ColorTarget],
    pub depth_format: vk::Format,
    pub stencil_format: vk::Format,
    /// `None`: fill, no culling, counter-clockwise front faces.
    pub rasterization: Option<vk::PipelineRasterizationStateCreateInfo<'static>>,
    pub vertex_bindings: &'a [vk::VertexInputBindingDescription],
    pub vertex_attributes: &'a [vk::VertexInputAttributeDescription],
    pub topology: vk::PrimitiveTopology,
    pub layouts: &'a [&'a BindingLayout],
    pub push_ranges: &'a [vk::PushConstantRange],
}

impl Default for PipelineDesc<'_> {
    fn default() -> Self {
        Self {
            vertex_spirv: &[],
            fragment_spirv: &[],
            vertex_entry: c"main",
            fragment_entry: c"main",
            colors: &[],
            depth_format: vk::Format::UNDEFINED,
            stencil_format: vk::Format::UNDEFINED,
            rasterization: None,
            vertex_bindings: &[],
            vertex_attributes: &[],
            topology: vk::PrimitiveTopology::TRIANGLE_LIST,
            layouts: &[],
            push_ranges: &[],
        }
    }
}

/// Cloning is cheap; commands keep the bound pipeline alive while recording.
#[derive(Clone)]
pub struct Pipeline(pub(crate) Arc<PipelineInner>);

pub(crate) struct PipelineInner {
    device: Arc<Device>,
    pub raw: vk::Pipeline,
    pub layout: vk::PipelineLayout,
    pub set_layouts: Vec<BindingLayout>,
    pub push_ranges: Vec<vk::PushConstantRange>,
    pub colors: Vec<vk::Format>,
    pub depth_format: vk::Format,
    pub stencil_format: vk::Format,
}

impl Pipeline {
    pub fn raw(&self) -> vk::Pipeline {
        self.0.raw
    }
    pub fn layout(&self) -> vk::PipelineLayout {
        self.0.layout
    }
}

impl Drop for PipelineInner {
    fn drop(&mut self) {
        unsafe {
            self.device.raw.destroy_pipeline(self.raw, None);
            self.device.raw.destroy_pipeline_layout(self.layout, None);
        }
    }
}

fn blend_bytes(blend: &vk::PipelineColorBlendAttachmentState) -> &[u8] {
    // Plain repr(C) struct of 32-bit fields: no padding.
    unsafe { std::slice::from_raw_parts((blend as *const vk::PipelineColorBlendAttachmentState).cast::<u8>(), size_of_val(blend)) }
}

fn spirv_valid(code: &[u32]) -> bool {
    code.len() >= 5 && code[0] == SPIRV_MAGIC
}

impl Device {
    fn validate_pipeline(&self, desc: &PipelineDesc) -> Result<()> {
        let limits = &self.properties.limits;
        fail_if(!spirv_valid(desc.vertex_spirv), Error::InvalidArgument("vertex SPIR-V"))?;
        fail_if(
            !desc.fragment_spirv.is_empty() && !spirv_valid(desc.fragment_spirv),
            Error::InvalidArgument("fragment SPIR-V"),
        )?;
        fail_if(
            desc.colors.len() > MAX_COLOR_ATTACHMENTS.min(limits.max_color_attachments as usize),
            Error::OutOfRange("color target count"),
        )?;
        fail_if(
            desc.layouts.len() > MAX_SET_LAYOUTS.min(limits.max_bound_descriptor_sets as usize),
            Error::OutOfRange("set layout count"),
        )?;
        fail_if(desc.push_ranges.len() > MAX_PUSH_RANGES, Error::OutOfRange("push constant range count"))?;

        let mut seen = vk::ShaderStageFlags::empty();
        let push_stages = vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT;
        for r in desc.push_ranges {
            fail_if(
                r.size == 0 || r.size % 4 != 0 || r.offset % 4 != 0,
                Error::InvalidArgument("push constant range alignment"),
            )?;
            fail_if(
                r.stage_flags.is_empty() || !push_stages.contains(r.stage_flags) || seen.intersects(r.stage_flags),
                Error::InvalidArgument("push constant range stages"),
            )?;
            fail_if(
                u64::from(r.offset) + u64::from(r.size) > u64::from(limits.max_push_constants_size),
                Error::OutOfRange("push constant range"),
            )?;
            seen |= r.stage_flags;
        }

        for color in desc.colors {
            fail_if(format_aspects(color.format) != vk::ImageAspectFlags::COLOR || color.format == vk::Format::UNDEFINED, Error::InvalidArgument("color target format"))?;
            let features =
                unsafe { self.instance().get_physical_device_format_properties(self.physical, color.format) }.optimal_tiling_features;
            let mut required = vk::FormatFeatureFlags::COLOR_ATTACHMENT;
            if color.blend.blend_enable != 0 {
                required |= vk::FormatFeatureFlags::COLOR_ATTACHMENT_BLEND;
            }
            fail_if(!features.contains(required), Error::Unsupported("color target format features"))?;
            fail_if(
                blend_bytes(&color.blend) != blend_bytes(&desc.colors[0].blend),
                Error::Unsupported("independent blend states"),
            )?;
        }
        let depth = desc.depth_format != vk::Format::UNDEFINED;
        let stencil = desc.stencil_format != vk::Format::UNDEFINED;
        fail_if(
            (depth && !format_aspects(desc.depth_format).contains(vk::ImageAspectFlags::DEPTH))
                || (stencil && !format_aspects(desc.stencil_format).contains(vk::ImageAspectFlags::STENCIL))
                || (depth && stencil && desc.depth_format != desc.stencil_format),
            Error::InvalidArgument("depth/stencil format"),
        )?;
        if let Some(r) = &desc.rasterization {
            fail_if(
                !r.p_next.is_null()
                    || !r.flags.is_empty()
                    || r.depth_clamp_enable != 0
                    || r.depth_bias_clamp != 0.0
                    || r.polygon_mode != vk::PolygonMode::FILL
                    || r.line_width != 1.0,
                Error::Unsupported("rasterization state (needs features this device layer does not enable)"),
            )?;
        }
        fail_if(
            !(0..=vk::PrimitiveTopology::TRIANGLE_FAN.as_raw()).contains(&desc.topology.as_raw()),
            Error::Unsupported("topology"),
        )?;

        let (bindings, attributes) = (desc.vertex_bindings, desc.vertex_attributes);
        fail_if(
            bindings.len() > limits.max_vertex_input_bindings as usize
                || attributes.len() > limits.max_vertex_input_attributes as usize,
            Error::OutOfRange("vertex input count"),
        )?;
        for (i, b) in bindings.iter().enumerate() {
            fail_if(
                b.binding >= limits.max_vertex_input_bindings
                    || b.stride > limits.max_vertex_input_binding_stride
                    || !matches!(b.input_rate, vk::VertexInputRate::VERTEX | vk::VertexInputRate::INSTANCE)
                    || bindings[..i].iter().any(|e| e.binding == b.binding),
                Error::InvalidArgument("vertex binding"),
            )?;
        }
        for (i, a) in attributes.iter().enumerate() {
            fail_if(
                !bindings.iter().any(|b| b.binding == a.binding)
                    || a.location >= limits.max_vertex_input_attributes
                    || a.offset > limits.max_vertex_input_attribute_offset
                    || attributes[..i].iter().any(|e| e.location == a.location),
                Error::InvalidArgument("vertex attribute"),
            )?;
            let features = unsafe { self.instance().get_physical_device_format_properties(self.physical, a.format) };
            fail_if(
                !features.buffer_features.contains(vk::FormatFeatureFlags::VERTEX_BUFFER),
                Error::Unsupported("vertex attribute format"),
            )?;
        }
        Ok(())
    }

    pub fn create_pipeline(self: &Arc<Self>, desc: &PipelineDesc) -> Result<Pipeline> {
        self.validate_pipeline(desc)?;
        let set_layouts: Vec<_> = desc.layouts.iter().map(|l| l.0.raw).collect();
        let layout_info =
            vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts).push_constant_ranges(desc.push_ranges);
        let layout = self.vk(unsafe { self.raw.create_pipeline_layout(&layout_info, None) })?;

        let mut stages = Vec::with_capacity(2);
        let mut result = Ok(());
        for (code, stage, entry) in [
            (desc.vertex_spirv, vk::ShaderStageFlags::VERTEX, desc.vertex_entry),
            (desc.fragment_spirv, vk::ShaderStageFlags::FRAGMENT, desc.fragment_entry),
        ] {
            if code.is_empty() {
                continue;
            }
            match self.vk(unsafe { self.raw.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(code), None) }) {
                Ok(module) => stages.push(vk::PipelineShaderStageCreateInfo::default().stage(stage).module(module).name(entry)),
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }

        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(desc.vertex_bindings)
            .vertex_attribute_descriptions(desc.vertex_attributes);
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(desc.topology);
        let viewport = vk::PipelineViewportStateCreateInfo::default();
        let raster = desc.rasterization.unwrap_or_else(|| {
            vk::PipelineRasterizationStateCreateInfo::default()
                .polygon_mode(vk::PolygonMode::FILL)
                .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
                .line_width(1.0)
        });
        let multisample =
            vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default();
        let blends: Vec<_> = desc.colors.iter().map(|c| c.blend).collect();
        let formats: Vec<_> = desc.colors.iter().map(|c| c.format).collect();
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blends);
        let dynamic_states = [
            vk::DynamicState::VIEWPORT_WITH_COUNT,
            vk::DynamicState::SCISSOR_WITH_COUNT,
            vk::DynamicState::DEPTH_TEST_ENABLE,
            vk::DynamicState::DEPTH_WRITE_ENABLE,
            vk::DynamicState::DEPTH_COMPARE_OP,
            vk::DynamicState::STENCIL_TEST_ENABLE,
            vk::DynamicState::STENCIL_OP,
            vk::DynamicState::STENCIL_COMPARE_MASK,
            vk::DynamicState::STENCIL_WRITE_MASK,
            vk::DynamicState::STENCIL_REFERENCE,
        ];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let mut rendering = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&formats)
            .depth_attachment_format(desc.depth_format)
            .stencil_attachment_format(desc.stencil_format);
        let info = vk::GraphicsPipelineCreateInfo::default()
            .push_next(&mut rendering)
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(layout)
            .base_pipeline_index(-1);
        let pipeline = result.and_then(|()| {
            unsafe { self.raw.create_graphics_pipelines(self.pipeline_cache, &[info], None) }
                .map(|p| p[0])
                .map_err(|(_, e)| self.error(e))
        });
        for stage in &stages {
            unsafe { self.raw.destroy_shader_module(stage.module, None) };
        }
        match pipeline {
            Ok(raw) => Ok(Pipeline(Arc::new(PipelineInner {
                device: self.clone(),
                raw,
                layout,
                set_layouts: desc.layouts.iter().map(|&l| l.clone()).collect(),
                push_ranges: desc.push_ranges.to_vec(),
                colors: formats,
                depth_format: desc.depth_format,
                stencil_format: desc.stencil_format,
            }))),
            Err(e) => {
                unsafe { self.raw.destroy_pipeline_layout(layout, None) };
                Err(e)
            }
        }
    }
}
