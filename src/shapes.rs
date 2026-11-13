//! Anti-aliased rounded rectangles and lines, drawn as instanced quads whose data
//! the vertex shader fetches through a buffer device address.
//!
//! Shapes are grouped into layers. Each layer has its own transform
//! (`screen = layer * scale + offset`, applied on the GPU) and an optional clip
//! rectangle (scissor). Layers draw in the order they were started.
//!
//! Per frame: queue shapes, [`ShapeRenderer::prepare`] once (after
//! `begin_frame`), then [`ShapeRenderer::record`] once inside the render pass.

use gpu::{Buffer, ColorTarget, Command, Device, MemoryKind, Pipeline, PipelineDesc, vk};
use std::sync::Arc;

pub type Color = [f32; 4];
/// `[x0, y0, x1, y1]`
pub type Rect = [f32; 4];

const KIND_RECT: f32 = 0.0;
const KIND_LINE: f32 = 1.0;

/// Matches `Shape` in shaders/shape.vert (std430, 64 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct Shape {
    geom: [f32; 4],
    fill: [f32; 4],
    stroke: [f32; 4],
    params: [f32; 4],
}
// Plain f32 arrays, no padding.
unsafe impl bytemuck::Zeroable for Shape {}
unsafe impl bytemuck::Pod for Shape {}
const _: () = assert!(size_of::<Shape>() == 64);

/// Matches the shader's push constant block (32 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct Push {
    address: vk::DeviceAddress,
    pixel_to_ndc: [f32; 2],
    offset: [f32; 2],
    scale: f32,
    _pad: f32,
}
unsafe impl bytemuck::Zeroable for Push {}
unsafe impl bytemuck::Pod for Push {}
const _: () = assert!(size_of::<Push>() == 32);

/// Maps layer coordinates to screen pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub scale: f32,
    pub offset: [f32; 2],
}

impl Transform {
    pub const IDENTITY: Transform = Transform { scale: 1.0, offset: [0.0, 0.0] };
}

#[derive(Clone, Copy)]
struct Layer {
    first: u32,
    count: u32,
    transform: Transform,
    clip: Option<Rect>,
    /// Drawn in the overlay pass, above all base text (menus, popups).
    overlay: bool,
}

pub struct ShapeRenderer {
    buffers: Vec<Option<Buffer>>,
    pipeline: Pipeline,
    device: Arc<Device>,
    shapes: Vec<Shape>,
    layers: Vec<Layer>,
    slot: usize,
    prepared: Option<(usize, Vec<Layer>)>,
    /// Layers started from now on belong to the overlay pass.
    overlay: bool,
}

impl ShapeRenderer {
    /// The device must have been created with `buffer_device_address`.
    pub fn new(device: &Arc<Device>, target_format: vk::Format) -> gpu::Result<Self> {
        let vertex_spirv = read_spirv(include_bytes!(concat!(env!("OUT_DIR"), "/shape.vert.spv")));
        let fragment_spirv = read_spirv(include_bytes!(concat!(env!("OUT_DIR"), "/shape.frag.spv")));
        let mut color = ColorTarget::opaque(target_format);
        color.blend = color
            .blend
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA);
        let pipeline = device.create_pipeline(&PipelineDesc {
            vertex_spirv: &vertex_spirv,
            fragment_spirv: &fragment_spirv,
            colors: &[color],
            topology: vk::PrimitiveTopology::TRIANGLE_STRIP,
            push_ranges: &[vk::PushConstantRange {
                stage_flags: vk::ShaderStageFlags::VERTEX,
                offset: 0,
                size: size_of::<Push>() as u32,
            }],
            ..Default::default()
        })?;
        Ok(Self {
            buffers: (0..gpu::FRAMES).map(|_| None).collect(),
            pipeline,
            device: device.clone(),
            shapes: Vec::new(),
            layers: Vec::new(),
            slot: 0,
            prepared: None,
            overlay: false,
        })
    }

    /// Starts a new layer; following shapes use its transform and clip.
    pub fn begin_layer(&mut self, transform: Transform, clip: Option<Rect>) {
        let first = self.shapes.len() as u32;
        self.layers.push(Layer { first, count: 0, transform, clip, overlay: self.overlay });
    }

    /// Layers started after this call draw in the overlay pass (see [`Self::record`]).
    pub fn begin_overlay(&mut self) {
        self.overlay = true;
        self.begin_layer(Transform::IDENTITY, None);
    }

    fn push(&mut self, shape: Shape) {
        if self.layers.is_empty() {
            self.begin_layer(Transform::IDENTITY, None);
        }
        self.shapes.push(shape);
        self.layers.last_mut().unwrap().count += 1;
    }

    /// Filled rectangle with optional rounded corners.
    pub fn rect(&mut self, rect: Rect, fill: Color, radius: f32) {
        self.push(Shape { geom: rect, fill, stroke: fill, params: [KIND_RECT, radius, 0.0, 0.0] });
    }

    /// Rectangle with a border of `border` width drawn inside its edge.
    pub fn bordered_rect(&mut self, rect: Rect, fill: Color, stroke: Color, radius: f32, border: f32) {
        self.push(Shape { geom: rect, fill, stroke, params: [KIND_RECT, radius, border, 0.0] });
    }

    /// Line segment with round caps; `dash > 0` draws dashes of that length.
    pub fn line(&mut self, a: [f32; 2], b: [f32; 2], color: Color, thickness: f32, dash: f32) {
        self.push(Shape {
            geom: [a[0], a[1], b[0], b[1]],
            fill: color,
            stroke: color,
            params: [KIND_LINE, thickness, dash, 0.0],
        });
    }

    /// Drops the queued shapes (e.g. when a frame is skipped).
    pub fn discard(&mut self) {
        self.shapes.clear();
        self.layers.clear();
        self.overlay = false;
    }

    /// Writes this frame's shapes to the GPU. Clears the queue.
    pub fn prepare(&mut self) -> gpu::Result<()> {
        let slot = self.slot;
        self.slot = (slot + 1) % gpu::FRAMES;
        if !self.shapes.is_empty() {
            let bytes: &[u8] = bytemuck::cast_slice(&self.shapes);
            // The swapchain waited for this slot's previous frame in begin_frame.
            let buffer = &mut self.buffers[slot];
            if buffer.as_ref().is_none_or(|b| b.size() < bytes.len() as u64) {
                let size = (bytes.len() as u64).next_power_of_two().max(64 * 1024);
                let usage = vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
                *buffer = Some(self.device.create_buffer(size, usage, MemoryKind::Upload)?);
            }
            buffer.as_mut().unwrap().write(0, bytes)?;
        }
        let layers = self.layers.drain(..).filter(|l| l.count > 0).collect();
        self.shapes.clear();
        self.overlay = false;
        self.prepared = Some((slot, layers));
        Ok(())
    }

    /// [`Self::prepare`] for a one-off render outside the frame cycle (an
    /// export). The GPU must be idle; the frame slot rotation is kept so the
    /// next frame reuses a buffer the swapchain has waited for.
    pub fn prepare_offscreen(&mut self) -> gpu::Result<()> {
        let slot = self.slot;
        let result = self.prepare();
        self.slot = slot;
        result
    }

    /// Draws the base (`overlay == false`) or the overlay layers of what the
    /// last [`Self::prepare`] built, then restores the full-target viewport and
    /// scissor for whatever is recorded next.
    pub fn record(&mut self, cmd: &mut Command, target: vk::Extent2D, overlay: bool) -> gpu::Result<()> {
        let Some((slot, layers)) = &self.prepared else {
            return Err(gpu::Error::InvalidState("ShapeRenderer::prepare was not called"));
        };
        let slot = *slot;
        let layers: Vec<Layer> = layers.iter().filter(|l| l.overlay == overlay).copied().collect();
        if layers.is_empty() || target.width == 0 || target.height == 0 {
            return Ok(());
        }
        let address = self.buffers[slot].as_ref().map_or(0, Buffer::address);
        let full = vk::Rect2D { offset: vk::Offset2D::default(), extent: target };
        let viewport = vk::Viewport {
            x: 0.0,
            y: 0.0,
            width: target.width as f32,
            height: target.height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        cmd.bind_pipeline(&self.pipeline)?;
        for layer in &layers {
            let scissor = layer.clip.map_or(full, |c| clip_rect(c, target));
            if scissor.extent.width == 0 || scissor.extent.height == 0 {
                continue;
            }
            cmd.set_viewport(viewport, scissor)?;
            let push = Push {
                address,
                pixel_to_ndc: [2.0 / target.width as f32, 2.0 / target.height as f32],
                offset: layer.transform.offset,
                scale: layer.transform.scale,
                _pad: 0.0,
            };
            cmd.push_constants(vk::ShaderStageFlags::VERTEX, 0, bytemuck::bytes_of(&push))?;
            // gl_InstanceIndex starts at `first`, indexing the layer's shapes.
            cmd.draw(4, layer.count, 0, layer.first)?;
        }
        cmd.set_viewport(viewport, full)
    }
}

/// Converts a pixel rectangle to a scissor clamped to the target.
fn clip_rect(c: Rect, target: vk::Extent2D) -> vk::Rect2D {
    let x0 = c[0].floor().clamp(0.0, target.width as f32) as u32;
    let y0 = c[1].floor().clamp(0.0, target.height as f32) as u32;
    let x1 = c[2].ceil().clamp(0.0, target.width as f32) as u32;
    let y1 = c[3].ceil().clamp(0.0, target.height as f32) as u32;
    vk::Rect2D {
        offset: vk::Offset2D { x: x0 as i32, y: y0 as i32 },
        extent: vk::Extent2D { width: x1.saturating_sub(x0), height: y1.saturating_sub(y0) },
    }
}

fn read_spirv(bytes: &[u8]) -> Vec<u32> {
    gpu::ash::util::read_spv(&mut std::io::Cursor::new(bytes)).expect("invalid SPIR-V")
}
