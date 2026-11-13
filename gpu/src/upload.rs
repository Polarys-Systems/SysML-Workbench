use crate::memory::{format_bytes, subresource_range};
use crate::{Buffer, Command, Device, Error, MemoryKind, Result, Texture, TimingSection, fail_if, vk};
use ash::vk::Handle;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

/// A color mip rectangle. A zero extent selects the whole mip. All uploads of one
/// texture in a batch must use the same `final_layout`.
#[derive(Clone, Copy, Debug)]
pub struct TextureUpload {
    pub mip_level: u32,
    pub offset: vk::Offset2D,
    pub extent: vk::Extent2D,
    /// `GENERAL` or `SHADER_READ_ONLY_OPTIMAL`.
    pub final_layout: vk::ImageLayout,
    /// The later use of the texture.
    pub consumer_stages: vk::PipelineStageFlags2,
    pub consumer_access: vk::AccessFlags2,
}

impl Default for TextureUpload {
    /// The whole mip 0, sampled from vertex/fragment shaders.
    fn default() -> Self {
        Self {
            mip_level: 0,
            offset: vk::Offset2D::default(),
            extent: vk::Extent2D::default(),
            final_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            consumer_stages: vk::PipelineStageFlags2::VERTEX_SHADER | vk::PipelineStageFlags2::FRAGMENT_SHADER,
            consumer_access: vk::AccessFlags2::SHADER_SAMPLED_READ,
        }
    }
}

enum Target {
    Buffer { raw: vk::Buffer, offset: u64 },
    Texture { image: vk::Image, mip_levels: u32, layout: Arc<AtomicI32>, region: TextureUpload },
}

struct Record {
    target: Target,
    staging_offset: u64,
    size: u64,
    stages: vk::PipelineStageFlags2,
    access: vk::AccessFlags2,
}

impl Record {
    fn key(&self) -> (bool, u64) {
        match &self.target {
            Target::Buffer { raw, .. } => (false, raw.as_raw()),
            Target::Texture { image, .. } => (true, image.as_raw()),
        }
    }

    /// Only records of the same resource can overlap.
    fn overlaps(&self, other: &Record) -> bool {
        match (&self.target, &other.target) {
            (Target::Buffer { offset: a, .. }, Target::Buffer { offset: b, .. }) => {
                *a < b + other.size && *b < a + self.size
            }
            (Target::Texture { region: a, .. }, Target::Texture { region: b, .. }) => {
                let span = |r: &TextureUpload| {
                    let (x, y) = (r.offset.x as u32, r.offset.y as u32);
                    (x, x + r.extent.width, y, y + r.extent.height)
                };
                let ((ax0, ax1, ay0, ay1), (bx0, bx1, by0, by1)) = (span(a), span(b));
                a.mip_level == b.mip_level && ax0 < bx1 && bx0 < ax1 && ay0 < by1 && by0 < ay1
            }
            _ => false,
        }
    }
}

/// Reusable bounded staging storage. Source bytes are copied while staging;
/// destinations must stay alive until the batch completes or is cancelled.
/// Use several batches and rotate when [`UploadBatch::poll`] reports busy.
pub struct UploadBatch {
    staging: Buffer,
    command: Command,
    used: u64,
    recording: bool,
    records: Vec<Record>,
    // Scratch reused across submissions.
    buffer_barriers: Vec<vk::BufferMemoryBarrier2<'static>>,
    image_barriers: Vec<vk::ImageMemoryBarrier2<'static>>,
    /// Layout state per texture, parallel to `image_barriers`.
    group_layouts: Vec<Arc<AtomicI32>>,
    previous: Vec<Option<usize>>,
    group_of: Vec<usize>,
    last: HashMap<(bool, u64), usize>,
}

impl Device {
    pub fn create_upload_batch(self: &Arc<Self>, capacity: u64) -> Result<UploadBatch> {
        fail_if(capacity == 0, Error::InvalidArgument("upload capacity is zero"))?;
        Ok(UploadBatch {
            staging: self.create_buffer(capacity, vk::BufferUsageFlags::TRANSFER_SRC, MemoryKind::Upload)?,
            command: self.create_command()?,
            used: 0,
            recording: false,
            records: Vec::new(),
            buffer_barriers: Vec::new(),
            image_barriers: Vec::new(),
            group_layouts: Vec::new(),
            previous: Vec::new(),
            group_of: Vec::new(),
            last: HashMap::new(),
        })
    }

    /// Uploads a whole mip-0 texture and waits for completion.
    pub fn upload_texture(self: &Arc<Self>, texture: &Texture, data: &[u8]) -> Result<()> {
        let mut batch = self.create_upload_batch(data.len() as u64)?;
        batch.begin()?;
        batch.upload_texture(texture, &TextureUpload::default(), data)?;
        batch.submit()?;
        batch.wait()
    }
}

impl UploadBatch {
    /// `true` once the last submission finished.
    pub fn poll(&mut self) -> Result<bool> {
        self.command.poll()
    }

    pub fn wait(&mut self) -> Result<()> {
        self.command.wait()
    }

    /// Starts staging. The previous submission must have completed.
    pub fn begin(&mut self) -> Result<()> {
        fail_if(self.recording, Error::InvalidState("upload batch already recording"))?;
        fail_if(!self.command.poll()?, Error::Busy)?;
        self.recording = true;
        self.used = 0;
        self.records.clear();
        Ok(())
    }

    /// Discards staged uploads.
    pub fn cancel(&mut self) -> Result<()> {
        self.command.cancel()?;
        self.recording = false;
        self.used = 0;
        self.records.clear();
        Ok(())
    }

    fn stage(&mut self, data: &[u8], alignment: u64) -> Result<u64> {
        fail_if(!self.recording, Error::InvalidState("upload batch not begun"))?;
        let offset = self.used.next_multiple_of(alignment);
        fail_if(
            offset.checked_add(data.len() as u64).is_none_or(|end| end > self.staging.size()),
            Error::OutOfRange("upload batch capacity"),
        )?;
        self.staging.write(offset, data)?;
        self.used = offset + data.len() as u64;
        Ok(offset)
    }

    /// Stages a buffer write. `consumer_*` describe how the data is used afterwards.
    pub fn upload_buffer(
        &mut self,
        buffer: &Buffer,
        offset: u64,
        data: &[u8],
        consumer_stages: vk::PipelineStageFlags2,
        consumer_access: vk::AccessFlags2,
    ) -> Result<()> {
        fail_if(data.is_empty() || consumer_stages.is_empty(), Error::InvalidArgument("upload data/stages"))?;
        fail_if(buffer.raw() == self.staging.raw(), Error::InvalidArgument("cannot upload into the staging buffer"))?;
        fail_if(!buffer.usage().contains(vk::BufferUsageFlags::TRANSFER_DST), Error::InvalidArgument("buffer needs TRANSFER_DST"))?;
        fail_if(
            offset > buffer.size() || data.len() as u64 > buffer.size() - offset,
            Error::OutOfRange("buffer upload"),
        )?;
        let staging_offset = self.stage(data, 4)?;
        self.records.push(Record {
            target: Target::Buffer { raw: buffer.raw(), offset },
            staging_offset,
            size: data.len() as u64,
            stages: consumer_stages,
            access: consumer_access,
        });
        Ok(())
    }

    /// Stages tightly packed texels for a color mip rectangle.
    pub fn upload_texture(&mut self, texture: &Texture, region: &TextureUpload, data: &[u8]) -> Result<()> {
        use vk::ImageLayout as L;
        let mut region = *region;
        fail_if(
            texture.aspects != vk::ImageAspectFlags::COLOR || !texture.usage.contains(vk::ImageUsageFlags::TRANSFER_DST),
            Error::InvalidArgument("texture must be color with TRANSFER_DST"),
        )?;
        fail_if(
            region.mip_level >= texture.mip_levels
                || region.offset.x < 0
                || region.offset.y < 0
                || region.consumer_stages.is_empty()
                || !matches!(region.final_layout, L::GENERAL | L::SHADER_READ_ONLY_OPTIMAL)
                || (region.final_layout == L::SHADER_READ_ONLY_OPTIMAL && !texture.usage.contains(vk::ImageUsageFlags::SAMPLED)),
            Error::InvalidArgument("texture upload region"),
        )?;
        let width = (texture.extent.width >> region.mip_level).max(1);
        let height = (texture.extent.height >> region.mip_level).max(1);
        if region.extent.width == 0 && region.extent.height == 0 {
            region.extent = vk::Extent2D { width, height };
        }
        let (x, y) = (region.offset.x as u32, region.offset.y as u32);
        fail_if(
            region.extent.width == 0
                || region.extent.height == 0
                || x > width
                || region.extent.width > width - x
                || y > height
                || region.extent.height > height - y,
            Error::OutOfRange("texture upload region"),
        )?;
        let texel = format_bytes(texture.format);
        fail_if(texel == 0, Error::Unsupported("texture upload format"))?;
        let expected = u64::from(region.extent.width) * u64::from(region.extent.height) * u64::from(texel);
        fail_if(data.len() as u64 != expected, Error::InvalidArgument("texture data size does not match region"))?;
        fail_if(
            self.records.iter().any(|r| {
                matches!(&r.target, Target::Texture { image, region: other, .. }
                    if *image == texture.image && other.final_layout != region.final_layout)
            }),
            Error::InvalidArgument("conflicting final layouts for one texture"),
        )?;
        let staging_offset = self.stage(data, u64::from(texel.max(4)))?;
        self.records.push(Record {
            target: Target::Texture {
                image: texture.image,
                mip_levels: texture.mip_levels,
                layout: texture.layout.clone(),
                region,
            },
            staging_offset,
            size: expected,
            stages: region.consumer_stages,
            access: region.consumer_access,
        });
        Ok(())
    }

    /// Records grouped barriers and copies and submits asynchronously.
    /// On failure the staged uploads are discarded.
    pub fn submit(&mut self) -> Result<()> {
        fail_if(!self.recording || self.records.is_empty(), Error::InvalidState("nothing staged"))?;
        let result = self.record_and_submit();
        self.recording = false;
        if result.is_err() {
            let _ = self.command.reset();
        }
        result
    }

    fn record_and_submit(&mut self) -> Result<()> {
        use vk::{AccessFlags2 as A, PipelineStageFlags2 as S};
        let cmd = &mut self.command;
        cmd.begin()?;
        cmd.begin_timing(TimingSection::UploadTotal)?;
        self.buffer_barriers.clear();
        self.image_barriers.clear();
        self.group_layouts.clear();
        self.previous.clear();
        self.group_of.clear();
        self.last.clear();

        // Pre-copy barriers: one per buffer range, one per texture.
        for (i, record) in self.records.iter().enumerate() {
            let previous = self.last.insert(record.key(), i);
            self.previous.push(previous);
            match &record.target {
                Target::Buffer { raw, offset } => {
                    self.group_of.push(usize::MAX);
                    self.buffer_barriers.push(
                        vk::BufferMemoryBarrier2::default()
                            .src_stage_mask(S::ALL_COMMANDS)
                            .src_access_mask(A::MEMORY_READ | A::MEMORY_WRITE)
                            .dst_stage_mask(S::TRANSFER)
                            .dst_access_mask(A::TRANSFER_WRITE)
                            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                            .buffer(*raw)
                            .offset(*offset)
                            .size(record.size),
                    );
                }
                Target::Texture { image, mip_levels, layout, .. } => {
                    let group = match previous {
                        Some(p) => self.group_of[p],
                        None => {
                            let old = vk::ImageLayout::from_raw(layout.load(Ordering::Relaxed));
                            let (src_s, src_a) = if old == vk::ImageLayout::UNDEFINED {
                                (S::NONE, A::NONE)
                            } else {
                                (S::ALL_COMMANDS, A::MEMORY_READ | A::MEMORY_WRITE)
                            };
                            self.image_barriers.push(
                                vk::ImageMemoryBarrier2::default()
                                    .src_stage_mask(src_s)
                                    .src_access_mask(src_a)
                                    .dst_stage_mask(S::TRANSFER)
                                    .dst_access_mask(A::TRANSFER_WRITE)
                                    .old_layout(old)
                                    .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                    .image(*image)
                                    .subresource_range(subresource_range(vk::ImageAspectFlags::COLOR, *mip_levels)),
                            );
                            self.group_layouts.push(layout.clone());
                            self.image_barriers.len() - 1
                        }
                    };
                    self.group_of.push(group);
                }
            }
        }
        cmd.barrier(
            &vk::DependencyInfo::default()
                .buffer_memory_barriers(&self.buffer_barriers)
                .image_memory_barriers(&self.image_barriers),
        );
        let mut barrier_calls = 1;

        cmd.begin_timing(TimingSection::UploadCopies)?;
        let (device, cb, staging) = (cmd.device().clone(), cmd.raw().unwrap(), self.staging.raw());
        let mut bytes = 0;
        for (i, record) in self.records.iter().enumerate() {
            // Order overlapping writes to the same resource.
            let mut p = self.previous[i];
            while let Some(j) = p {
                if record.overlaps(&self.records[j]) {
                    let ordered = [vk::MemoryBarrier2::default()
                        .src_stage_mask(S::TRANSFER)
                        .src_access_mask(A::TRANSFER_WRITE)
                        .dst_stage_mask(S::TRANSFER)
                        .dst_access_mask(A::TRANSFER_WRITE)];
                    cmd.barrier(&vk::DependencyInfo::default().memory_barriers(&ordered));
                    barrier_calls += 1;
                    break;
                }
                p = self.previous[j];
            }
            match &record.target {
                Target::Buffer { raw, offset } => {
                    let copy = vk::BufferCopy { src_offset: record.staging_offset, dst_offset: *offset, size: record.size };
                    unsafe { device.raw.cmd_copy_buffer(cb, staging, *raw, &[copy]) };
                }
                Target::Texture { image, region, .. } => {
                    let copy = vk::BufferImageCopy {
                        buffer_offset: record.staging_offset,
                        buffer_row_length: 0,
                        buffer_image_height: 0,
                        image_subresource: vk::ImageSubresourceLayers {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            mip_level: region.mip_level,
                            base_array_layer: 0,
                            layer_count: 1,
                        },
                        image_offset: vk::Offset3D { x: region.offset.x, y: region.offset.y, z: 0 },
                        image_extent: vk::Extent3D { width: region.extent.width, height: region.extent.height, depth: 1 },
                    };
                    unsafe {
                        device.raw.cmd_copy_buffer_to_image(cb, staging, *image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[copy])
                    };
                }
            }
            bytes += record.size;
        }
        cmd.end_timing(TimingSection::UploadCopies)?;

        // Post-copy barriers: TRANSFER writes -> each consumer, with final layouts.
        for b in &mut self.buffer_barriers {
            (b.src_stage_mask, b.src_access_mask) = (S::TRANSFER, A::TRANSFER_WRITE);
            (b.dst_stage_mask, b.dst_access_mask) = (S::NONE, A::NONE);
        }
        for b in &mut self.image_barriers {
            (b.src_stage_mask, b.src_access_mask) = (S::TRANSFER, A::TRANSFER_WRITE);
            (b.dst_stage_mask, b.dst_access_mask) = (S::NONE, A::NONE);
            b.old_layout = vk::ImageLayout::TRANSFER_DST_OPTIMAL;
        }
        let mut next_buffer = 0;
        for (record, &group) in self.records.iter().zip(&self.group_of) {
            let (stages, access) = match &record.target {
                Target::Buffer { .. } => {
                    next_buffer += 1;
                    let b = &mut self.buffer_barriers[next_buffer - 1];
                    (&mut b.dst_stage_mask, &mut b.dst_access_mask)
                }
                Target::Texture { region, .. } => {
                    let b = &mut self.image_barriers[group];
                    b.new_layout = region.final_layout;
                    (&mut b.dst_stage_mask, &mut b.dst_access_mask)
                }
            };
            *stages |= record.stages;
            *access |= record.access;
        }
        for (layout, barrier) in self.group_layouts.iter().zip(&self.image_barriers) {
            cmd.track(layout, barrier.new_layout);
        }
        cmd.barrier(
            &vk::DependencyInfo::default()
                .buffer_memory_barriers(&self.buffer_barriers)
                .image_memory_barriers(&self.image_barriers),
        );
        barrier_calls += 1;
        cmd.end_timing(TimingSection::UploadTotal)?;
        cmd.submit()?;

        let stats = &mut device.state().stats;
        stats.upload_barrier_calls += barrier_calls;
        stats.upload_submissions += 1;
        stats.uploaded_bytes += bytes;
        Ok(())
    }
}
