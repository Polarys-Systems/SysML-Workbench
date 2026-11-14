//! Frosted glass: the canvas (diagram) is drawn into an offscreen image, which
//! is copied to the window and also shrunk and blurred. Glass shapes in later
//! passes sample the blurred copy behind them (see `shaders/shape.frag`).
//!
//! Per frame, outside any render pass:
//! 1. render the canvas pass into [`Backdrop::canvas`];
//! 2. [`Backdrop::record_blur`];
//! 3. in the window's render pass, [`Backdrop::record_composite`] first.

use gpu::{
    Attachment, BindingDesc, BindingLayout, BindingSet, BindingValue, BindingWrite, Command, Device, Pipeline,
    PipelineDesc, RenderDesc, Sampler, Texture, TextureDesc, vk,
};
use std::sync::Arc;

/// The blurred copy is this many times smaller than the window on each side.
/// `shaders/shape.frag` relies on the same factor.
pub const SHRINK: u32 = 4;

/// Matches `Push` in shaders/post.frag.
#[repr(C)]
#[derive(Clone, Copy)]
struct Push {
    uv_scale: [f32; 2],
    step: [f32; 2],
    mode: f32,
}
unsafe impl bytemuck::Zeroable for Push {}
unsafe impl bytemuck::Pod for Push {}

const COPY: f32 = 0.0;
const SHRINK_MODE: f32 = 1.0;
const BLUR: f32 = 2.0;

/// Window-sized canvas image plus two small images the blur ping-pongs between.
struct Images {
    canvas: Texture,
    small: [Texture; 2],
    /// Sampling the canvas, `small[0]` and `small[1]`.
    sets: [BindingSet; 3],
}

pub struct Backdrop {
    device: Arc<Device>,
    pipeline: Pipeline,
    layout: BindingLayout,
    sampler: Sampler,
    format: vk::Format,
    images: Images,
}

impl Backdrop {
    pub fn new(device: &Arc<Device>, format: vk::Format, extent: vk::Extent2D) -> gpu::Result<Self> {
        let layout = device.create_binding_layout(&[BindingDesc {
            binding: 0,
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            stages: vk::ShaderStageFlags::FRAGMENT,
            count: 1,
        }])?;
        let vertex = read_spirv(include_bytes!(concat!(env!("OUT_DIR"), "/post.vert.spv")));
        let fragment = read_spirv(include_bytes!(concat!(env!("OUT_DIR"), "/post.frag.spv")));
        let pipeline = device.create_pipeline(&PipelineDesc {
            vertex_spirv: &vertex,
            fragment_spirv: &fragment,
            colors: &[gpu::ColorTarget::opaque(format)],
            topology: vk::PrimitiveTopology::TRIANGLE_LIST,
            layouts: &[&layout],
            push_ranges: &[vk::PushConstantRange {
                stage_flags: vk::ShaderStageFlags::FRAGMENT,
                offset: 0,
                size: size_of::<Push>() as u32,
            }],
            ..Default::default()
        })?;
        let sampler = device.create_sampler(None)?;
        let images = create_images(device, &layout, &sampler, format, extent)?;
        Ok(Self { device: device.clone(), pipeline, layout, sampler, format, images })
    }

    /// Recreates the images for a new window size. Waits for the GPU, since
    /// frames in flight may still use the old ones.
    pub fn resize(&mut self, extent: vk::Extent2D) -> gpu::Result<()> {
        if extent == self.images.canvas.extent() || extent.width == 0 || extent.height == 0 {
            return Ok(());
        }
        self.device.wait_idle()?;
        self.images = create_images(&self.device, &self.layout, &self.sampler, self.format, extent)?;
        Ok(())
    }

    /// The image the canvas pass renders into.
    pub fn canvas(&self) -> &Texture {
        &self.images.canvas
    }

    /// The blurred canvas glass shapes sample (in `SHADER_READ_ONLY_OPTIMAL`
    /// outside [`Self::record_blur`]).
    pub fn blurred(&self) -> &Texture {
        &self.images.small[0]
    }

    pub fn sampler(&self) -> &Sampler {
        &self.sampler
    }

    /// Shrinks and blurs the rendered canvas into [`Self::blurred`]. `radius`
    /// is the blur radius in window pixels (0: only shrink).
    pub fn record_blur(&self, cmd: &mut Command, radius: f32) -> gpu::Result<()> {
        let im = &self.images;
        let read = |cmd: &mut Command, t: &Texture| {
            cmd.transition_texture(
                t,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
            )
        };
        let canvas = im.canvas.extent();
        let small = im.small[0].extent();
        read(cmd, &im.canvas)?;
        // Small pixel i covers canvas pixels SHRINK*i ..; the last may hang over the edge.
        let uv_scale = [
            (small.width * SHRINK) as f32 / canvas.width as f32,
            (small.height * SHRINK) as f32 / canvas.height as f32,
        ];
        self.pass(cmd, &im.small[0], &im.sets[0], Push { uv_scale, step: [0.0; 2], mode: SHRINK_MODE })?;
        read(cmd, &im.small[0])?;
        // The 13-tap kernel reaches about 5.2 taps; spread the taps for larger radii
        // and repeat the pair of passes so the spacing does not show.
        let reach = radius / SHRINK as f32;
        if reach > 0.5 {
            let rounds = if reach > 8.0 { 2 } else { 1 };
            let spread = reach / (5.2 * (rounds as f32).sqrt());
            let texel = [1.0 / small.width as f32, 1.0 / small.height as f32];
            for _ in 0..rounds {
                let h = Push { uv_scale: [1.0; 2], step: [texel[0] * spread, 0.0], mode: BLUR };
                self.pass(cmd, &im.small[1], &im.sets[1], h)?;
                read(cmd, &im.small[1])?;
                let v = Push { uv_scale: [1.0; 2], step: [0.0, texel[1] * spread], mode: BLUR };
                self.pass(cmd, &im.small[0], &im.sets[2], v)?;
                read(cmd, &im.small[0])?;
            }
        }
        Ok(())
    }

    /// Draws the canvas over the whole target (inside the window's render pass).
    pub fn record_composite(&self, cmd: &mut Command) -> gpu::Result<()> {
        cmd.bind_pipeline(&self.pipeline)?;
        cmd.bind_set(0, &self.images.sets[0])?;
        let push = Push { uv_scale: [1.0; 2], step: [0.0; 2], mode: COPY };
        cmd.push_constants(vk::ShaderStageFlags::FRAGMENT, 0, bytemuck::bytes_of(&push))?;
        cmd.draw(3, 1, 0, 0)
    }

    /// One full-target pass into `target`, sampling through `set`.
    fn pass(&self, cmd: &mut Command, target: &Texture, set: &BindingSet, push: Push) -> gpu::Result<()> {
        cmd.begin_render(&RenderDesc {
            area: target.rect(),
            // Every pixel is overwritten: the old contents need not be loaded.
            colors: &[Attachment { load: vk::AttachmentLoadOp::DONT_CARE, ..Attachment::load(target) }],
            ..Default::default()
        })?;
        cmd.bind_pipeline(&self.pipeline)?;
        cmd.bind_set(0, set)?;
        cmd.push_constants(vk::ShaderStageFlags::FRAGMENT, 0, bytemuck::bytes_of(&push))?;
        cmd.draw(3, 1, 0, 0)?;
        cmd.end_render()
    }
}

fn create_images(
    device: &Arc<Device>,
    layout: &BindingLayout,
    sampler: &Sampler,
    format: vk::Format,
    extent: vk::Extent2D,
) -> gpu::Result<Images> {
    let usage = vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED;
    let texture = |width: u32, height: u32| {
        device.create_texture(&TextureDesc { width: width.max(1), height: height.max(1), format, usage, mip_levels: 1 })
    };
    let (w, h) = (extent.width.max(1), extent.height.max(1));
    let canvas = texture(w, h)?;
    let small = [texture(w.div_ceil(SHRINK), h.div_ceil(SHRINK))?, texture(w.div_ceil(SHRINK), h.div_ceil(SHRINK))?];
    // Every image starts readable, so a binding is valid even before the first frame.
    let mut cmd = device.create_command()?;
    cmd.begin()?;
    for t in [&canvas, &small[0], &small[1]] {
        cmd.transition_texture(
            t,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::PipelineStageFlags2::FRAGMENT_SHADER,
            vk::AccessFlags2::SHADER_SAMPLED_READ,
        )?;
    }
    cmd.submit()?;
    cmd.wait()?;
    let set = |t: &Texture| -> gpu::Result<BindingSet> {
        let set = device.create_binding_set(layout)?;
        device.write_bindings(&[BindingWrite {
            set: &set,
            binding: 0,
            first_element: 0,
            values: &[BindingValue::TextureSampler { texture: t, sampler, layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL }],
        }])?;
        Ok(set)
    };
    let sets = [set(&canvas)?, set(&small[0])?, set(&small[1])?];
    Ok(Images { canvas, small, sets })
}

fn read_spirv(bytes: &[u8]) -> Vec<u32> {
    gpu::ash::util::read_spv(&mut std::io::Cursor::new(bytes)).expect("invalid SPIR-V")
}
