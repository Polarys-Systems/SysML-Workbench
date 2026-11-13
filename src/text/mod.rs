//! Text rendering: stb_truetype rasterization into a shared glyph atlas,
//! instanced quads on the GPU. Supports any number of fonts and sizes at once.
//!
//! Per frame, after [`gpu::Swapchain::begin_frame`]:
//! 1. queue text with [`TextRenderer::draw`] / [`TextRenderer::draw_wrapped`],
//! 2. call [`TextRenderer::prepare`] once (outside a render pass; it may submit
//!    an atlas upload, which the queue orders before the frame's commands),
//! 3. call [`TextRenderer::record`] once inside the render pass.
//!
//! All coordinates are physical pixels, y down, origin at the target's top-left.
//! Multiply logical sizes by the window's scale factor for high-DPI output.

mod atlas;
mod font;
mod stb;

pub use font::{FontId, LineMetrics, PlacedGlyph};
pub use stb::FontFace;

use atlas::Atlas;
use font::FontSet;
use gpu::{
    BindingDesc, BindingLayout, BindingSet, BindingValue, BindingWrite, Buffer, ColorTarget, Command, Device,
    MemoryKind, Pipeline, PipelineDesc, Sampler, Texture, TextureDesc, TextureUpload, TimingSection, UploadBatch, vk,
};
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

/// Horizontal subpixel positions rasterized per glyph.
const SUBPIXEL_BINS: u32 = 4;
/// Font sizes are quantized to 1/SIZE_STEPS pixel so cache keys stay finite.
const SIZE_STEPS: f32 = 64.0;
const INITIAL_ATLAS: u32 = 1024;
const MAX_ATLAS: u32 = 4096;

#[derive(Clone, Copy, Debug)]
pub struct TextStyle {
    pub font: FontId,
    /// Em size in pixels.
    pub size: f32,
    /// sRGB RGBA, 0..1.
    pub color: [f32; 4],
}

impl TextStyle {
    pub fn new(font: FontId, size: f32, color: [f32; 4]) -> Self {
        Self { font, size, color }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    font: FontId,
    size: u32,
    glyph: u32,
    bin: u8,
}

/// Where a rasterized glyph lives in the atlas; `w == 0` for blank glyphs.
#[derive(Clone, Copy)]
struct CachedGlyph {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    /// Bitmap offset from the snapped pen position on the baseline.
    left: i16,
    top: i16,
}

struct AtlasFull;

#[repr(C)]
#[derive(Clone, Copy)]
struct Instance {
    rect: [f32; 4],
    uv: [f32; 4],
    color: [u8; 4],
}
// Plain f32/u8 fields with 4-byte alignment and no padding (36 bytes).
unsafe impl bytemuck::Zeroable for Instance {}
unsafe impl bytemuck::Pod for Instance {}
const _: () = assert!(size_of::<Instance>() == 36);

struct Draw {
    style: TextStyle,
    position: [f32; 2],
    max_width: Option<f32>,
    text: Range<usize>,
    clip: Option<[f32; 4]>,
}

pub struct TextRenderer {
    // GPU objects; frame commands referencing them are waited for by the swapchain.
    upload: UploadBatch,
    vertices: Vec<Option<Buffer>>,
    set: BindingSet,
    pipeline: Pipeline,
    _layout: BindingLayout,
    sampler: Sampler,
    texture: Texture,
    device: Arc<Device>,

    fonts: FontSet,
    atlas: Atlas,
    max_atlas: u32,
    glyphs: HashMap<GlyphKey, CachedGlyph>,

    draws: Vec<Draw>,
    text: String,
    /// Clip rectangle applied to draws queued from now on.
    clip: Option<[f32; 4]>,
    placed: Vec<PlacedGlyph>,
    instances: Vec<Instance>,
    /// Vertex buffer slot for the next `prepare`; `record` uses the previous one.
    slot: usize,
    /// (vertex slot, instances of the base pass, all instances).
    prepared: Option<(usize, u32, u32)>,
    /// Index of the first queued draw of the overlay pass, if one was started.
    overlay_from: Option<usize>,
    /// Instance index where the overlay pass starts.
    overlay_first: usize,
}

impl TextRenderer {
    pub fn new(device: &Arc<Device>, target_format: vk::Format) -> gpu::Result<Self> {
        let max_atlas = MAX_ATLAS.min(device.properties().limits.max_image_dimension2_d);
        let atlas = Atlas::new(INITIAL_ATLAS.min(max_atlas), INITIAL_ATLAS.min(max_atlas));
        let texture = create_atlas_texture(device, &atlas)?;
        let sampler = device.create_sampler(None)?;
        let layout = device.create_binding_layout(&[BindingDesc {
            binding: 0,
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            stages: vk::ShaderStageFlags::FRAGMENT,
            count: 1,
        }])?;
        let set = device.create_binding_set(&layout)?;
        write_atlas_binding(device, &set, &texture, &sampler)?;

        let vertex_spirv = read_spirv(include_bytes!(concat!(env!("OUT_DIR"), "/text.vert.spv")));
        let fragment_spirv = read_spirv(include_bytes!(concat!(env!("OUT_DIR"), "/text.frag.spv")));
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
            vertex_bindings: &[vk::VertexInputBindingDescription {
                binding: 0,
                stride: size_of::<Instance>() as u32,
                input_rate: vk::VertexInputRate::INSTANCE,
            }],
            vertex_attributes: &[
                vk::VertexInputAttributeDescription { location: 0, binding: 0, format: vk::Format::R32G32B32A32_SFLOAT, offset: 0 },
                vk::VertexInputAttributeDescription { location: 1, binding: 0, format: vk::Format::R32G32B32A32_SFLOAT, offset: 16 },
                vk::VertexInputAttributeDescription { location: 2, binding: 0, format: vk::Format::R8G8B8A8_UNORM, offset: 32 },
            ],
            topology: vk::PrimitiveTopology::TRIANGLE_STRIP,
            layouts: &[&layout],
            push_ranges: &[vk::PushConstantRange { stage_flags: vk::ShaderStageFlags::VERTEX, offset: 0, size: 8 }],
            ..Default::default()
        })?;
        let upload = device.create_upload_batch(atlas_bytes(&atlas))?;

        Ok(Self {
            upload,
            vertices: (0..gpu::FRAMES).map(|_| None).collect(),
            set,
            pipeline,
            _layout: layout,
            sampler,
            texture,
            device: device.clone(),
            fonts: FontSet::default(),
            atlas,
            max_atlas,
            glyphs: HashMap::new(),
            draws: Vec::new(),
            text: String::new(),
            clip: None,
            placed: Vec::new(),
            instances: Vec::new(),
            slot: 0,
            prepared: None,
            overlay_from: None,
            overlay_first: 0,
        })
    }

    // ---- fonts ---------------------------------------------------------------

    /// Adds face `index` of a font file's bytes; `None` if it cannot be parsed.
    pub fn add_font(&mut self, data: Vec<u8>, index: u32) -> Option<FontId> {
        self.fonts.add(FontFace::from_bytes(data, index)?)
    }

    /// Loads the first face of a trusted .ttf/.otf/.ttc file.
    pub fn load_font(&mut self, path: impl AsRef<Path>) -> std::io::Result<FontId> {
        let path = path.as_ref();
        let data = std::fs::read(path)?;
        self.add_font(data, 0).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("unsupported font: {}", path.display()))
        })
    }

    pub fn line_metrics(&self, font: FontId, size: f32) -> LineMetrics {
        self.fonts.line_metrics(font, quantize(size))
    }

    /// Size `[width, height]` of the laid-out text box.
    pub fn measure(&mut self, font: FontId, size: f32, text: &str, max_width: Option<f32>) -> [f32; 2] {
        self.placed.clear();
        self.fonts.layout(font, quantize(size), text, max_width, &mut self.placed)
    }

    // ---- per frame -------------------------------------------------------------

    /// Queues one text box with its top-left corner at `position`.
    pub fn draw(&mut self, style: TextStyle, position: [f32; 2], text: &str) {
        self.queue(style, position, None, text);
    }

    /// Like [`Self::draw`], wrapping lines at spaces to fit `max_width`.
    #[allow(dead_code)] // renderer API; the current UI clips instead of wrapping
    pub fn draw_wrapped(&mut self, style: TextStyle, position: [f32; 2], max_width: f32, text: &str) {
        self.queue(style, position, Some(max_width), text);
    }

    /// Clips text queued after this call to `[x0, y0, x1, y1]` (pixels);
    /// `None` removes the clip. Glyphs are cropped exactly at the edges.
    pub fn set_clip(&mut self, clip: Option<[f32; 4]>) {
        self.clip = clip;
    }

    fn queue(&mut self, mut style: TextStyle, position: [f32; 2], max_width: Option<f32>, text: &str) {
        if text.is_empty() || style.size.is_nan() || style.size <= 0.0 || style.color[3] <= 0.0 {
            return;
        }
        style.size = quantize(style.size);
        let start = self.text.len();
        self.text.push_str(text);
        let clip = self.clip;
        self.draws.push(Draw { style, position, max_width, text: start..self.text.len(), clip });
    }

    /// Drops the queued text (e.g. when a frame is skipped).
    pub fn discard(&mut self) {
        self.draws.clear();
        self.text.clear();
        self.clip = None;
        self.overlay_from = None;
    }

    /// Rasterizes missing glyphs, uploads atlas changes and fills this frame's
    /// vertex buffer. Clears the queue. Call outside a render pass.
    pub fn prepare(&mut self) -> gpu::Result<()> {
        let mut cleared = false;
        loop {
            // After the atlas was cleared at its largest size, drop what still
            // does not fit instead of looping.
            let partial = cleared;
            if self.build_instances(partial).is_ok() {
                break;
            }
            if self.atlas.width() < self.max_atlas {
                self.grow_atlas()?;
            } else {
                self.atlas.clear();
                self.glyphs.clear();
                cleared = true;
            }
        }
        self.draws.clear();
        self.text.clear();
        self.clip = None;
        self.overlay_from = None;
        self.upload_atlas()?;

        let slot = self.slot;
        self.slot = (slot + 1) % gpu::FRAMES;
        let count = self.instances.len() as u32;
        if count > 0 {
            let bytes: &[u8] = bytemuck::cast_slice(&self.instances);
            // The swapchain waited for this slot's previous frame in begin_frame,
            // so its buffer can be rewritten or replaced.
            let buffer = &mut self.vertices[slot];
            if buffer.as_ref().is_none_or(|b| b.size() < bytes.len() as u64) {
                let size = (bytes.len() as u64).next_power_of_two().max(16 * 1024);
                *buffer = Some(self.device.create_buffer(size, vk::BufferUsageFlags::VERTEX_BUFFER, MemoryKind::Upload)?);
            }
            buffer.as_mut().unwrap().write(0, bytes)?;
        }
        self.prepared = Some((slot, self.overlay_first as u32, count));
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

    /// Text queued after this call draws in the overlay pass, above the
    /// overlay shapes (see [`crate::shapes::ShapeRenderer::begin_overlay`]).
    pub fn begin_overlay(&mut self) {
        if self.overlay_from.is_none() {
            self.overlay_from = Some(self.draws.len());
        }
    }

    /// Draws the base (`overlay == false`) or the overlay text of what the last
    /// [`Self::prepare`] built. Call inside the render pass whose single color
    /// target has the format given to [`Self::new`].
    pub fn record(&mut self, cmd: &mut Command, target: vk::Extent2D, overlay: bool) -> gpu::Result<()> {
        let Some((slot, split, total)) = self.prepared else {
            return Err(gpu::Error::InvalidState("TextRenderer::prepare was not called"));
        };
        let (first, count) = if overlay { (split, total - split) } else { (0, split) };
        if count == 0 || target.width == 0 || target.height == 0 {
            return Ok(());
        }
        let vertices = self.vertices[slot].as_ref().unwrap();
        // A timing section may be used once per frame: time the base pass only.
        if !overlay {
            cmd.begin_timing(TimingSection::TextDraw)?;
        }
        cmd.bind_pipeline(&self.pipeline)?;
        cmd.bind_set(0, &self.set)?;
        let pixel_to_ndc = [2.0 / target.width as f32, 2.0 / target.height as f32];
        cmd.push_constants(vk::ShaderStageFlags::VERTEX, 0, bytemuck::cast_slice(&pixel_to_ndc))?;
        cmd.bind_vertex_buffer(0, vertices, 0)?;
        cmd.draw(4, count, 0, first)?;
        if !overlay {
            cmd.end_timing(TimingSection::TextDraw)?;
        }
        Ok(())
    }

    // ---- internals -------------------------------------------------------------

    fn build_instances(&mut self, partial: bool) -> Result<(), AtlasFull> {
        self.instances.clear();
        let draws = std::mem::take(&mut self.draws);
        let split = self.overlay_from.unwrap_or(draws.len()).min(draws.len());
        let mut result = self.build_draws(&draws[..split], partial);
        self.overlay_first = self.instances.len();
        if result.is_ok() {
            result = self.build_draws(&draws[split..], partial);
        }
        self.draws = draws;
        result
    }

    fn build_draws(&mut self, draws: &[Draw], partial: bool) -> Result<(), AtlasFull> {
        let (inv_w, inv_h) = (1.0 / self.atlas.width() as f32, 1.0 / self.atlas.height() as f32);
        let mut placed = std::mem::take(&mut self.placed);
        let mut result = Ok(());
        'draws: for draw in draws {
            let TextStyle { font, size, color } = draw.style;
            let color = color.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
            placed.clear();
            self.fonts.layout(font, size, &self.text[draw.text.clone()], draw.max_width, &mut placed);
            let scale = self.fonts.scale(font, size);
            let size_key = (size * SIZE_STEPS) as u32;
            for g in &placed {
                let x = draw.position[0] + g.x;
                let pen_y = (draw.position[1] + g.baseline).round();
                // Skip glyphs that are clearly outside the clip before rasterizing them.
                if let Some(c) = draw.clip
                    && (pen_y + size < c[1] || pen_y - 2.0 * size > c[3] || x > c[2] || x + 2.0 * size < c[0])
                {
                    continue;
                }
                let pen_x = x.floor();
                let bin = (((x - pen_x) * SUBPIXEL_BINS as f32) as u32).min(SUBPIXEL_BINS - 1) as u8;
                let key = GlyphKey { font, size: size_key, glyph: g.glyph, bin };
                let cached = match self.glyph(key, scale) {
                    Ok(c) => c,
                    Err(AtlasFull) if partial => continue,
                    Err(e) => {
                        result = Err(e);
                        break 'draws;
                    }
                };
                if cached.w == 0 {
                    continue;
                }
                let x0 = pen_x + f32::from(cached.left);
                let y0 = pen_y + f32::from(cached.top);
                let (u0, v0) = (f32::from(cached.x) * inv_w, f32::from(cached.y) * inv_h);
                let mut rect = [x0, y0, x0 + f32::from(cached.w), y0 + f32::from(cached.h)];
                let mut uv = [u0, v0, u0 + f32::from(cached.w) * inv_w, v0 + f32::from(cached.h) * inv_h];
                if let Some(clip) = draw.clip
                    && !crop(&mut rect, &mut uv, clip)
                {
                    continue;
                }
                self.instances.push(Instance { rect, uv, color });
            }
        }
        self.placed = placed;
        result
    }

    fn glyph(&mut self, key: GlyphKey, scale: f32) -> Result<CachedGlyph, AtlasFull> {
        if let Some(&cached) = self.glyphs.get(&key) {
            return Ok(cached);
        }
        let face = self.fonts.face(key.font);
        let shift = f32::from(key.bin) / SUBPIXEL_BINS as f32;
        let [x0, y0, x1, y1] = face.bitmap_box(key.glyph, scale, shift);
        let (w, h) = ((x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32);
        let mut cached = CachedGlyph { x: 0, y: 0, w: 0, h: 0, left: 0, top: 0 };
        if w > 0 && h > 0 {
            let [x, y] = self.atlas.allocate(w, h).ok_or(AtlasFull)?;
            let stride = self.atlas.width();
            face.rasterize(key.glyph, scale, shift, self.atlas.region_mut(x, y, w, h), w, h, stride);
            cached = CachedGlyph { x: x as u16, y: y as u16, w: w as u16, h: h as u16, left: x0 as i16, top: y0 as i16 };
        }
        self.glyphs.insert(key, cached);
        Ok(cached)
    }

    /// Doubles the atlas. Waits for the GPU: the texture and descriptor set
    /// are replaced while earlier frames may still sample them.
    fn grow_atlas(&mut self) -> gpu::Result<()> {
        let size = (self.atlas.width() * 2).min(self.max_atlas);
        self.device.wait_idle()?;
        self.upload.wait()?;
        let atlas = Atlas::new(size, size);
        let texture = create_atlas_texture(&self.device, &atlas)?;
        write_atlas_binding(&self.device, &self.set, &texture, &self.sampler)?;
        self.upload = self.device.create_upload_batch(atlas_bytes(&atlas))?;
        self.texture = texture;
        self.atlas = atlas;
        self.glyphs.clear();
        Ok(())
    }

    fn upload_atlas(&mut self) -> gpu::Result<()> {
        let Some(([x, y, w, h], bytes)) = self.atlas.take_dirty() else {
            return Ok(());
        };
        // Same queue as the frames: the upload's barriers order it after the
        // frames that sampled the atlas before and before the ones that follow.
        self.upload.wait()?;
        self.upload.begin()?;
        let region = TextureUpload {
            offset: vk::Offset2D { x: x as i32, y: y as i32 },
            extent: vk::Extent2D { width: w, height: h },
            consumer_stages: vk::PipelineStageFlags2::FRAGMENT_SHADER,
            ..Default::default()
        };
        if let Err(e) = self.upload.upload_texture(&self.texture, &region, &bytes) {
            let _ = self.upload.cancel();
            return Err(e);
        }
        self.upload.submit()
    }
}

/// Crops a glyph quad and its texture coordinates to `clip`; false if nothing is left.
fn crop(rect: &mut [f32; 4], uv: &mut [f32; 4], clip: [f32; 4]) -> bool {
    if rect[2] <= clip[0] || rect[0] >= clip[2] || rect[3] <= clip[1] || rect[1] >= clip[3] {
        return false;
    }
    let du = (uv[2] - uv[0]) / (rect[2] - rect[0]);
    let dv = (uv[3] - uv[1]) / (rect[3] - rect[1]);
    if rect[0] < clip[0] {
        uv[0] += (clip[0] - rect[0]) * du;
        rect[0] = clip[0];
    }
    if rect[2] > clip[2] {
        uv[2] -= (rect[2] - clip[2]) * du;
        rect[2] = clip[2];
    }
    if rect[1] < clip[1] {
        uv[1] += (clip[1] - rect[1]) * dv;
        rect[1] = clip[1];
    }
    if rect[3] > clip[3] {
        uv[3] -= (rect[3] - clip[3]) * dv;
        rect[3] = clip[3];
    }
    true
}

fn quantize(size: f32) -> f32 {
    (size * SIZE_STEPS).round().max(1.0) / SIZE_STEPS
}

fn atlas_bytes(atlas: &Atlas) -> u64 {
    u64::from(atlas.width()) * u64::from(atlas.height())
}

fn create_atlas_texture(device: &Arc<Device>, atlas: &Atlas) -> gpu::Result<Texture> {
    device.create_texture(&TextureDesc {
        width: atlas.width(),
        height: atlas.height(),
        format: vk::Format::R8_UNORM,
        usage: vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
        mip_levels: 1,
    })
}

fn write_atlas_binding(device: &Device, set: &BindingSet, texture: &Texture, sampler: &Sampler) -> gpu::Result<()> {
    device.write_bindings(&[BindingWrite {
        set,
        binding: 0,
        first_element: 0,
        values: &[BindingValue::TextureSampler {
            texture,
            sampler,
            layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }],
    }])
}

fn read_spirv(bytes: &[u8]) -> Vec<u32> {
    gpu::ash::util::read_spv(&mut std::io::Cursor::new(bytes)).expect("invalid SPIR-V")
}
