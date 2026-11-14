//! Diagram export: renders what the panes queued into the shape and text
//! renderers into an offscreen image, reads it back and writes a PNG.

use crate::shapes::ShapeRenderer;
use crate::text::TextRenderer;
use gpu::{Attachment, Device, MemoryKind, RenderDesc, TextureDesc, vk};
use std::error::Error;
use std::io::BufWriter;
use std::path::Path;
use std::sync::Arc;

/// Largest image side the device can render and the exporter allows.
pub fn max_side(device: &Device) -> u32 {
    let limits = &device.properties().limits;
    limits.max_image_dimension2_d.min(limits.max_framebuffer_width).min(limits.max_framebuffer_height).min(16384)
}

/// Renders the queued shapes and text (base pass only) into a `size` image
/// cleared to the linear color `clear`, in the window's `format` so the
/// existing pipelines apply, and saves it as an opaque RGB PNG.
///
/// Waits for the GPU. Drops whatever was queued, also on failure.
pub fn write_png(
    device: &Arc<Device>,
    shapes: &mut ShapeRenderer,
    text: &mut TextRenderer,
    format: vk::Format,
    size: [u32; 2],
    clear: [f32; 4],
    path: &Path,
) -> Result<(), Box<dyn Error>> {
    let result = render(device, shapes, text, format, size, clear).and_then(|rgb| encode(path, size, &rgb));
    shapes.discard();
    text.discard();
    result
}

fn render(
    device: &Arc<Device>,
    shapes: &mut ShapeRenderer,
    text: &mut TextRenderer,
    format: vk::Format,
    [width, height]: [u32; 2],
    clear: [f32; 4],
) -> Result<Vec<u8>, Box<dyn Error>> {
    let bgra = match format {
        vk::Format::B8G8R8A8_SRGB | vk::Format::B8G8R8A8_UNORM => true,
        vk::Format::R8G8B8A8_SRGB | vk::Format::R8G8B8A8_UNORM => false,
        other => return Err(format!("the window's pixel format {other:?} cannot be exported").into()),
    };
    // Frames in flight still use the renderers' buffers; prepare_offscreen
    // rewrites one of them.
    device.wait_idle()?;
    let target = device.create_texture(&TextureDesc {
        width,
        height,
        format,
        usage: vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
        mip_levels: 1,
    })?;
    let bytes = u64::from(width) * u64::from(height) * 4;
    let readback = device.create_buffer(bytes, vk::BufferUsageFlags::TRANSFER_DST, MemoryKind::Readback)?;
    shapes.prepare_offscreen()?;
    text.prepare_offscreen()?;

    let mut cmd = device.create_command()?;
    cmd.begin()?;
    let extent = target.extent();
    cmd.begin_render(&RenderDesc {
        area: target.rect(),
        colors: &[Attachment::clear_color(&target, clear)],
        ..Default::default()
    })?;
    shapes.record(&mut cmd, extent, crate::shapes::PASS_CANVAS)?;
    text.record(&mut cmd, extent, crate::shapes::PASS_CANVAS)?;
    cmd.end_render()?;
    cmd.copy_texture_to_buffer(&target, &readback, 0)?;
    cmd.submit()?;
    cmd.wait()?;

    let pixels = readback.mapped().ok_or("the readback buffer is not mapped")?;
    let mut rgb = Vec::with_capacity((bytes / 4 * 3) as usize);
    for &[c0, c1, c2, _] in pixels.as_chunks::<4>().0 {
        let [r, g, b] = if bgra { [c2, c1, c0] } else { [c0, c1, c2] };
        rgb.extend_from_slice(&[r, g, b]);
    }
    Ok(rgb)
}

fn encode(path: &Path, [width, height]: [u32; 2], rgb: &[u8]) -> Result<(), Box<dyn Error>> {
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgb)?;
    writer.finish()?;
    Ok(())
}
