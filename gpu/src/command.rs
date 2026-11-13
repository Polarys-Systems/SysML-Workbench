use crate::memory::{format_bytes, subresource_range};
use crate::pipeline::PipelineInner;
use crate::{
    BindingSet, Buffer, Device, Error, MAX_COLOR_ATTACHMENTS, MAX_SET_LAYOUTS, Pipeline, Result, Texture, fail_if, vk,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

/// GPU intervals (not CPU recording time). `Frame` is recorded automatically for
/// swapchain commands and excludes presentation/VSync. Each section can be timed
/// once per submission; nested intervals overlap and must not be summed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimingSection {
    Frame,
    RenderSetup,
    TextDraw,
    RenderFinish,
    UploadTotal,
    UploadCopies,
}

impl TimingSection {
    pub const COUNT: usize = 6;
    pub const ALL: [TimingSection; Self::COUNT] =
        [Self::Frame, Self::RenderSetup, Self::TextDraw, Self::RenderFinish, Self::UploadTotal, Self::UploadCopies];
    fn bit(self) -> u8 {
        1 << self as u8
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GpuTiming {
    pub milliseconds: f64,
    pub total_milliseconds: f64,
    /// Zero means no completed result yet, not a zero-duration interval.
    pub samples: u64,
}

/// A render target. Rendering uses the whole view and requires a single mip.
#[derive(Clone, Copy)]
pub struct Attachment<'a> {
    pub texture: &'a Texture,
    pub load: vk::AttachmentLoadOp,
    pub store: vk::AttachmentStoreOp,
    pub clear: vk::ClearValue,
}

impl<'a> Attachment<'a> {
    pub fn clear_color(texture: &'a Texture, rgba: [f32; 4]) -> Self {
        let clear = vk::ClearValue { color: vk::ClearColorValue { float32: rgba } };
        Self { texture, load: vk::AttachmentLoadOp::CLEAR, store: vk::AttachmentStoreOp::STORE, clear }
    }
    pub fn clear_depth(texture: &'a Texture, depth: f32, stencil: u32) -> Self {
        let clear = vk::ClearValue { depth_stencil: vk::ClearDepthStencilValue { depth, stencil } };
        Self { texture, load: vk::AttachmentLoadOp::CLEAR, store: vk::AttachmentStoreOp::STORE, clear }
    }
    /// Keeps the existing contents.
    pub fn load(texture: &'a Texture) -> Self {
        Self { texture, load: vk::AttachmentLoadOp::LOAD, store: vk::AttachmentStoreOp::STORE, clear: vk::ClearValue::default() }
    }
}

/// Depth and stencil may reference the same texture.
#[derive(Default)]
pub struct RenderDesc<'a> {
    pub area: vk::Rect2D,
    pub colors: &'a [Attachment<'a>],
    pub depth: Option<Attachment<'a>>,
    pub stencil: Option<Attachment<'a>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Idle,
    Recording,
    Rendering,
    Pending,
    Failed,
}

/// Counters merged into the device stats on submission.
#[derive(Default)]
struct Counters {
    pipeline_binds: u64,
    dynamic_state_commands: u64,
    draw_calls: u64,
    vertices_drawn: u64,
    indices_drawn: u64,
}

/// A reusable command buffer with its own fence. Image layouts are tracked per
/// command and committed to the textures when it is submitted.
pub struct Command {
    device: Arc<Device>,
    pool: vk::CommandPool,
    cb: vk::CommandBuffer,
    fence: vk::Fence,
    /// Null when timestamps are unavailable.
    queries: vk::QueryPool,
    phase: Phase,
    pub(crate) frame_owned: bool,
    // Cached state to skip redundant native calls.
    state_valid: bool,
    depth_enabled: bool,
    stencil_enabled: bool,
    pipeline: Option<Arc<PipelineInner>>,
    sets: [vk::DescriptorSet; MAX_SET_LAYOUTS],
    // Current render pass formats, checked against the bound pipeline.
    colors: Vec<vk::Format>,
    depth_format: vk::Format,
    stencil_format: vk::Format,
    index_bytes: u64,
    index_stride: u64,
    images: Vec<(Arc<AtomicI32>, vk::ImageLayout)>,
    timing_used: u8,
    timing_open: u8,
    counters: Counters,
}

impl Device {
    pub fn create_command(self: &Arc<Self>) -> Result<Command> {
        let mut command = Command {
            device: self.clone(),
            pool: vk::CommandPool::null(),
            cb: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
            queries: vk::QueryPool::null(),
            phase: Phase::Idle,
            frame_owned: false,
            state_valid: false,
            depth_enabled: false,
            stencil_enabled: false,
            pipeline: None,
            sets: [vk::DescriptorSet::null(); MAX_SET_LAYOUTS],
            colors: Vec::with_capacity(MAX_COLOR_ATTACHMENTS),
            depth_format: vk::Format::UNDEFINED,
            stencil_format: vk::Format::UNDEFINED,
            index_bytes: 0,
            index_stride: 0,
            images: Vec::new(),
            timing_used: 0,
            timing_open: 0,
            counters: Counters::default(),
        };
        // On error, Command's Drop destroys whatever was already created.
        let pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::TRANSIENT)
            .queue_family_index(self.queue_family);
        command.pool = self.vk(unsafe { self.raw.create_command_pool(&pool_info, None) })?;
        let cb_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(command.pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        command.cb = self.vk(unsafe { self.raw.allocate_command_buffers(&cb_info) })?[0];
        command.fence = self.vk(unsafe { self.raw.create_fence(&vk::FenceCreateInfo::default(), None) })?;
        if self.timestamps_supported() {
            let info = vk::QueryPoolCreateInfo::default()
                .query_type(vk::QueryType::TIMESTAMP)
                .query_count(2 * TimingSection::COUNT as u32);
            command.queries = self.vk(unsafe { self.raw.create_query_pool(&info, None) })?;
        }
        Ok(command)
    }
}

impl Command {
    pub fn device(&self) -> &Arc<Device> {
        &self.device
    }

    /// Native command buffer while recording, for raw Vulkan calls. Call
    /// [`Command::invalidate_state`] afterwards if you changed bound state.
    pub fn raw(&self) -> Option<vk::CommandBuffer> {
        self.recording().then_some(self.cb)
    }

    /// Forgets cached pipeline/descriptor/index/dynamic state.
    pub fn invalidate_state(&mut self) {
        self.state_valid = false;
        self.pipeline = None;
        self.sets = [vk::DescriptorSet::null(); MAX_SET_LAYOUTS];
        self.index_bytes = 0;
        self.index_stride = 0;
    }

    fn recording(&self) -> bool {
        matches!(self.phase, Phase::Recording | Phase::Rendering)
    }
    fn require_recording(&self) -> Result<()> {
        fail_if(!self.recording(), Error::InvalidState("command is not recording"))
    }
    fn require_outside_render(&self) -> Result<()> {
        fail_if(self.phase != Phase::Recording, Error::InvalidState("must be recording outside a render pass"))
    }
    fn require_not_frame(&self) -> Result<()> {
        fail_if(self.frame_owned, Error::InvalidArgument("frame commands are driven by the swapchain"))
    }

    // ---- lifecycle -------------------------------------------------------

    /// Returns `true` once the GPU has finished (or nothing was submitted).
    pub fn poll(&mut self) -> Result<bool> {
        fail_if(self.device.is_lost(), Error::DeviceLost)?;
        if self.phase != Phase::Pending {
            return Ok(true);
        }
        if !self.device.vk(unsafe { self.device.raw.get_fence_status(self.fence) })? {
            return Ok(false);
        }
        match self.collect_timings() {
            Err(Error::Busy) => Ok(false),
            Err(e) => Err(e),
            Ok(()) => {
                self.phase = Phase::Idle;
                Ok(true)
            }
        }
    }

    pub fn wait(&mut self) -> Result<()> {
        fail_if(self.device.is_lost(), Error::DeviceLost)?;
        if self.phase == Phase::Pending {
            self.device.vk(unsafe { self.device.raw.wait_for_fences(&[self.fence], true, u64::MAX) })?;
            self.collect_timings()?;
            self.phase = Phase::Idle;
        }
        Ok(())
    }

    pub fn begin(&mut self) -> Result<()> {
        self.require_not_frame()?;
        self.begin_internal()
    }

    /// Submits asynchronously. Rendering must be ended. Layouts commit on success.
    pub fn submit(&mut self) -> Result<()> {
        self.require_not_frame()?;
        self.submit_internal(vk::Semaphore::null(), vk::Semaphore::null())
    }

    /// Discards a recording (or a failed command) so it can be begun again.
    pub fn cancel(&mut self) -> Result<()> {
        self.require_not_frame()?;
        self.reset()
    }

    pub(crate) fn reset(&mut self) -> Result<()> {
        fail_if(self.phase == Phase::Pending, Error::Busy)?;
        let result = self.device.vk(unsafe {
            self.device.raw.reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty())
        });
        self.phase = if result.is_ok() { Phase::Idle } else { Phase::Failed };
        self.images.clear();
        self.timing_used = 0;
        self.timing_open = 0;
        self.counters = Counters::default();
        self.invalidate_state();
        result
    }

    pub(crate) fn begin_internal(&mut self) -> Result<()> {
        fail_if(!self.poll()?, Error::Busy)?;
        fail_if(self.recording(), Error::InvalidState("command is already recording"))?;
        self.reset()?;
        let info = vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        if let Err(e) = self.device.vk(unsafe { self.device.raw.begin_command_buffer(self.cb, &info) }) {
            self.phase = Phase::Failed;
            return Err(e);
        }
        self.phase = Phase::Recording;
        if self.queries != vk::QueryPool::null() {
            // Per-command pools: in-flight frames never reset each other's queries.
            unsafe { self.device.raw.cmd_reset_query_pool(self.cb, self.queries, 0, 2 * TimingSection::COUNT as u32) };
        }
        if self.frame_owned {
            self.begin_timing(TimingSection::Frame)?;
        }
        Ok(())
    }

    pub(crate) fn submit_internal(&mut self, wait: vk::Semaphore, signal: vk::Semaphore) -> Result<()> {
        fail_if(self.phase != Phase::Recording, Error::InvalidState("command must be recording outside rendering"))?;
        // Frame covers the whole swapchain command including the present transition.
        let frame = TimingSection::Frame.bit();
        fail_if(self.timing_open & !frame != 0, Error::InvalidState("a GPU timing section is still open"))?;
        if self.timing_open & frame != 0 {
            self.end_timing(TimingSection::Frame)?;
        }
        let result = self.end_and_submit(wait, signal);
        if result.is_err() {
            self.phase = Phase::Failed;
            return result;
        }
        self.phase = Phase::Pending;
        for (state, layout) in &self.images {
            state.store(layout.as_raw(), Ordering::Relaxed);
        }
        let c = std::mem::take(&mut self.counters);
        let stats = &mut self.device.state().stats;
        stats.pipeline_binds += c.pipeline_binds;
        stats.dynamic_state_commands += c.dynamic_state_commands;
        stats.draw_calls += c.draw_calls;
        stats.vertices_drawn += c.vertices_drawn;
        stats.indices_drawn += c.indices_drawn;
        Ok(())
    }

    fn end_and_submit(&self, wait: vk::Semaphore, signal: vk::Semaphore) -> Result<()> {
        let d = &self.device;
        d.vk(unsafe { d.raw.end_command_buffer(self.cb) })?;
        d.vk(unsafe { d.raw.reset_fences(&[self.fence]) })?;
        let all = vk::PipelineStageFlags2::ALL_COMMANDS;
        let waits = [vk::SemaphoreSubmitInfo::default().semaphore(wait).stage_mask(all)];
        let signals = [vk::SemaphoreSubmitInfo::default().semaphore(signal).stage_mask(all)];
        let buffers = [vk::CommandBufferSubmitInfo::default().command_buffer(self.cb)];
        let mut submit = vk::SubmitInfo2::default().command_buffer_infos(&buffers);
        if wait != vk::Semaphore::null() {
            submit = submit.wait_semaphore_infos(&waits);
        }
        if signal != vk::Semaphore::null() {
            submit = submit.signal_semaphore_infos(&signals);
        }
        d.vk(unsafe { d.raw.queue_submit2(d.queue, &[submit], self.fence) })
    }

    // ---- image layouts and transfers ------------------------------------

    fn current_layout(&self, state: &Arc<AtomicI32>) -> vk::ImageLayout {
        match self.images.iter().find(|(s, _)| Arc::ptr_eq(s, state)) {
            Some(&(_, layout)) => layout,
            None => vk::ImageLayout::from_raw(state.load(Ordering::Relaxed)),
        }
    }

    pub(crate) fn track(&mut self, state: &Arc<AtomicI32>, layout: vk::ImageLayout) {
        match self.images.iter_mut().find(|(s, _)| Arc::ptr_eq(s, state)) {
            Some(entry) => entry.1 = layout,
            None => self.images.push((state.clone(), layout)),
        }
    }

    pub(crate) fn barrier(&self, dependency: &vk::DependencyInfo) {
        unsafe { self.device.raw.cmd_pipeline_barrier2(self.cb, dependency) };
    }

    /// Records a whole-image layout transition (outside rendering). `stages` and
    /// `access` describe the next use.
    pub fn transition_texture(
        &mut self,
        texture: &Texture,
        layout: vk::ImageLayout,
        stages: vk::PipelineStageFlags2,
        access: vk::AccessFlags2,
    ) -> Result<()> {
        use vk::{ImageLayout as L, ImageUsageFlags as U};
        self.require_outside_render()?;
        let required = match layout {
            L::GENERAL => U::empty(),
            L::TRANSFER_SRC_OPTIMAL => U::TRANSFER_SRC,
            L::TRANSFER_DST_OPTIMAL => U::TRANSFER_DST,
            L::SHADER_READ_ONLY_OPTIMAL => U::SAMPLED,
            L::ATTACHMENT_OPTIMAL if texture.aspects == vk::ImageAspectFlags::COLOR => U::COLOR_ATTACHMENT,
            L::ATTACHMENT_OPTIMAL => U::DEPTH_STENCIL_ATTACHMENT,
            L::PRESENT_SRC_KHR if texture.alloc.is_none() => U::empty(),
            L::PRESENT_SRC_KHR => return Err(Error::InvalidArgument("only swapchain images can be presented")),
            _ => return Err(Error::Unsupported("target layout")),
        };
        fail_if(!texture.usage.contains(required), Error::InvalidArgument("texture usage does not allow layout"))?;
        let old = self.current_layout(&texture.layout);
        self.track(&texture.layout, layout);
        let (src_stage, src_access) = if old == L::UNDEFINED {
            (vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE)
        } else {
            (vk::PipelineStageFlags2::ALL_COMMANDS, vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
        };
        let barrier = [vk::ImageMemoryBarrier2::default()
            .src_stage_mask(src_stage)
            .src_access_mask(src_access)
            .dst_stage_mask(stages)
            .dst_access_mask(access)
            .old_layout(old)
            .new_layout(layout)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(texture.image)
            .subresource_range(subresource_range(texture.aspects, texture.mip_levels))];
        self.barrier(&vk::DependencyInfo::default().image_memory_barriers(&barrier));
        Ok(())
    }

    /// GPU buffer copy with barriers against all prior and later use.
    pub fn copy_buffer(&mut self, src: &Buffer, src_offset: u64, dst: &Buffer, dst_offset: u64, size: u64) -> Result<()> {
        use vk::{AccessFlags2 as A, PipelineStageFlags2 as S};
        self.require_outside_render()?;
        fail_if(size == 0, Error::InvalidArgument("copy size is zero"))?;
        fail_if(
            !src.usage().contains(vk::BufferUsageFlags::TRANSFER_SRC) || !dst.usage().contains(vk::BufferUsageFlags::TRANSFER_DST),
            Error::InvalidArgument("buffer transfer usage"),
        )?;
        let in_range = |offset: u64, buffer: &Buffer| offset <= buffer.size() && size <= buffer.size() - offset;
        fail_if(!in_range(src_offset, src) || !in_range(dst_offset, dst), Error::OutOfRange("buffer copy"))?;
        fail_if(
            src.raw() == dst.raw() && src_offset < dst_offset + size && dst_offset < src_offset + size,
            Error::InvalidArgument("overlapping copy within one buffer"),
        )?;
        let barrier = |buffer: &Buffer, offset, src_s, src_a, dst_s, dst_a| {
            vk::BufferMemoryBarrier2::default()
                .src_stage_mask(src_s)
                .src_access_mask(src_a)
                .dst_stage_mask(dst_s)
                .dst_access_mask(dst_a)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .buffer(buffer.raw())
                .offset(offset)
                .size(size)
        };
        let before = [
            barrier(src, src_offset, S::ALL_COMMANDS, A::MEMORY_WRITE, S::TRANSFER, A::TRANSFER_READ),
            barrier(dst, dst_offset, S::ALL_COMMANDS, A::MEMORY_WRITE, S::TRANSFER, A::TRANSFER_WRITE),
        ];
        self.barrier(&vk::DependencyInfo::default().buffer_memory_barriers(&before));
        let region = vk::BufferCopy { src_offset, dst_offset, size };
        unsafe { self.device.raw.cmd_copy_buffer(self.cb, src.raw(), dst.raw(), &[region]) };
        let after = [barrier(
            dst,
            dst_offset,
            S::TRANSFER,
            A::TRANSFER_WRITE,
            S::ALL_COMMANDS | S::HOST,
            A::MEMORY_READ | A::MEMORY_WRITE | A::HOST_READ,
        )];
        self.barrier(&vk::DependencyInfo::default().buffer_memory_barriers(&after));
        Ok(())
    }

    /// Copies a whole single-mip color texture into `dst` at `offset` as tightly
    /// packed rows, with barriers against earlier rendering and later host
    /// reads. Leaves the texture in `TRANSFER_SRC_OPTIMAL`.
    pub fn copy_texture_to_buffer(&mut self, src: &Texture, dst: &Buffer, offset: u64) -> Result<()> {
        use vk::{AccessFlags2 as A, PipelineStageFlags2 as S};
        self.require_outside_render()?;
        fail_if(src.aspects != vk::ImageAspectFlags::COLOR, Error::Unsupported("readback of depth/stencil textures"))?;
        fail_if(src.mip_levels != 1, Error::Unsupported("readback of mipmapped textures"))?;
        fail_if(!dst.usage().contains(vk::BufferUsageFlags::TRANSFER_DST), Error::InvalidArgument("buffer transfer usage"))?;
        let texel = u64::from(format_bytes(src.format));
        fail_if(texel == 0, Error::Unsupported("texture readback format"))?;
        fail_if(!offset.is_multiple_of(texel.max(4)), Error::InvalidArgument("readback offset alignment"))?;
        let size = u64::from(src.extent.width) * u64::from(src.extent.height) * texel;
        fail_if(offset > dst.size() || size > dst.size() - offset, Error::OutOfRange("texture readback"))?;

        self.transition_texture(src, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, S::COPY, A::TRANSFER_READ)?;
        let barrier = |src_s, src_a, dst_s, dst_a| {
            [vk::BufferMemoryBarrier2::default()
                .src_stage_mask(src_s)
                .src_access_mask(src_a)
                .dst_stage_mask(dst_s)
                .dst_access_mask(dst_a)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .buffer(dst.raw())
                .offset(offset)
                .size(size)]
        };
        let before = barrier(S::ALL_COMMANDS, A::MEMORY_READ | A::MEMORY_WRITE, S::COPY, A::TRANSFER_WRITE);
        self.barrier(&vk::DependencyInfo::default().buffer_memory_barriers(&before));
        let region = vk::BufferImageCopy {
            buffer_offset: offset,
            buffer_row_length: 0,
            buffer_image_height: 0,
            image_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            },
            image_offset: vk::Offset3D::default(),
            image_extent: src.extent.into(),
        };
        unsafe {
            self.device.raw.cmd_copy_image_to_buffer(
                self.cb,
                src.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst.raw(),
                &[region],
            )
        };
        let after = barrier(S::COPY, A::TRANSFER_WRITE, S::ALL_COMMANDS | S::HOST, A::MEMORY_READ | A::HOST_READ);
        self.barrier(&vk::DependencyInfo::default().buffer_memory_barriers(&after));
        Ok(())
    }

    // ---- rendering --------------------------------------------------------

    fn check_attachment(&self, a: &Attachment, area: vk::Rect2D, aspect: vk::ImageAspectFlags) -> Result<()> {
        let t = a.texture;
        let usage = if aspect == vk::ImageAspectFlags::COLOR {
            vk::ImageUsageFlags::COLOR_ATTACHMENT
        } else {
            vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
        };
        let (x, y) = (area.offset.x as u32, area.offset.y as u32);
        fail_if(
            t.mip_levels != 1 || !t.aspects.intersects(aspect) || !t.usage.intersects(usage),
            Error::InvalidArgument("attachment texture"),
        )?;
        fail_if(
            x > t.extent.width
                || area.extent.width > t.extent.width - x
                || y > t.extent.height
                || area.extent.height > t.extent.height - y,
            Error::OutOfRange("render area exceeds attachment"),
        )?;
        let load_ok = match a.load {
            vk::AttachmentLoadOp::CLEAR | vk::AttachmentLoadOp::DONT_CARE => true,
            vk::AttachmentLoadOp::LOAD => self.current_layout(&t.layout) != vk::ImageLayout::UNDEFINED,
            _ => false,
        };
        fail_if(!load_ok, Error::InvalidArgument("attachment load op (LOAD needs defined contents)"))?;
        fail_if(
            !matches!(a.store, vk::AttachmentStoreOp::STORE | vk::AttachmentStoreOp::DONT_CARE),
            Error::InvalidArgument("attachment store op"),
        )
    }

    /// Transitions the attachments, begins dynamic rendering and sets a full-area
    /// viewport and scissor.
    pub fn begin_render(&mut self, desc: &RenderDesc) -> Result<()> {
        use vk::{AccessFlags2 as A, ImageAspectFlags as Asp, PipelineStageFlags2 as S};
        self.require_outside_render()?;
        let area = desc.area;
        let limits = &self.device.properties.limits;
        fail_if(
            desc.colors.len() > MAX_COLOR_ATTACHMENTS.min(limits.max_color_attachments as usize),
            Error::OutOfRange("color attachment count"),
        )?;
        fail_if(
            area.offset.x < 0 || area.offset.y < 0 || area.extent.width == 0 || area.extent.height == 0,
            Error::InvalidArgument("render area"),
        )?;
        fail_if(
            desc.colors.is_empty() && desc.depth.is_none() && desc.stencil.is_none(),
            Error::InvalidArgument("no attachments"),
        )?;
        fail_if(
            u64::from(area.offset.x as u32) + u64::from(area.extent.width) > u64::from(limits.max_framebuffer_width)
                || u64::from(area.offset.y as u32) + u64::from(area.extent.height) > u64::from(limits.max_framebuffer_height),
            Error::OutOfRange("render area exceeds framebuffer limits"),
        )?;
        for (i, a) in desc.colors.iter().enumerate() {
            self.check_attachment(a, area, Asp::COLOR)?;
            fail_if(
                desc.colors[..i].iter().any(|e| e.texture.image == a.texture.image),
                Error::InvalidArgument("duplicate color attachment"),
            )?;
        }
        if let Some(d) = &desc.depth {
            self.check_attachment(d, area, Asp::DEPTH)?;
        }
        if let Some(s) = &desc.stencil {
            self.check_attachment(s, area, Asp::STENCIL)?;
            fail_if(
                desc.depth.is_some_and(|d| d.texture.image != s.texture.image),
                Error::InvalidArgument("depth and stencil must share a texture"),
            )?;
        }
        let viewport = vk::Viewport {
            x: area.offset.x as f32,
            y: area.offset.y as f32,
            width: area.extent.width as f32,
            height: area.extent.height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        self.validate_viewport(&viewport, &area)?;

        let color_use = (S::COLOR_ATTACHMENT_OUTPUT, A::COLOR_ATTACHMENT_READ | A::COLOR_ATTACHMENT_WRITE);
        let depth_use = (
            S::EARLY_FRAGMENT_TESTS | S::LATE_FRAGMENT_TESTS,
            A::DEPTH_STENCIL_ATTACHMENT_READ | A::DEPTH_STENCIL_ATTACHMENT_WRITE,
        );
        for a in desc.colors {
            self.transition_texture(a.texture, vk::ImageLayout::ATTACHMENT_OPTIMAL, color_use.0, color_use.1)?;
        }
        let depth_stencil = desc.depth.or(desc.stencil);
        if let Some(a) = depth_stencil {
            self.transition_texture(a.texture, vk::ImageLayout::ATTACHMENT_OPTIMAL, depth_use.0, depth_use.1)?;
        }

        let info = |a: &Attachment| {
            vk::RenderingAttachmentInfo::default()
                .image_view(a.texture.view)
                .image_layout(vk::ImageLayout::ATTACHMENT_OPTIMAL)
                .load_op(a.load)
                .store_op(a.store)
                .clear_value(a.clear)
        };
        let mut colors = [vk::RenderingAttachmentInfo::default(); MAX_COLOR_ATTACHMENTS];
        for (slot, a) in colors.iter_mut().zip(desc.colors) {
            *slot = info(a);
        }
        let depth = desc.depth.as_ref().map(info);
        let stencil = desc.stencil.as_ref().map(info);
        let mut rendering =
            vk::RenderingInfo::default().render_area(area).layer_count(1).color_attachments(&colors[..desc.colors.len()]);
        if let Some(d) = &depth {
            rendering = rendering.depth_attachment(d);
        }
        if let Some(s) = &stencil {
            rendering = rendering.stencil_attachment(s);
        }
        unsafe { self.device.raw.cmd_begin_rendering(self.cb, &rendering) };
        self.phase = Phase::Rendering;
        self.colors.clear();
        self.colors.extend(desc.colors.iter().map(|a| a.texture.format));
        self.depth_format = desc.depth.map_or(vk::Format::UNDEFINED, |a| a.texture.format);
        self.stencil_format = desc.stencil.map_or(vk::Format::UNDEFINED, |a| a.texture.format);
        self.set_viewport(viewport, area)
    }

    pub fn end_render(&mut self) -> Result<()> {
        fail_if(self.phase != Phase::Rendering, Error::InvalidState("not rendering"))?;
        unsafe { self.device.raw.cmd_end_rendering(self.cb) };
        self.phase = Phase::Recording;
        Ok(())
    }

    fn validate_viewport(&self, v: &vk::Viewport, scissor: &vk::Rect2D) -> Result<()> {
        let limits = &self.device.properties.limits;
        let [lo, hi] = limits.viewport_bounds_range;
        let depth_ok = |d: f32| (0.0..=1.0).contains(&d);
        fail_if(
            ![v.x, v.y, v.width, v.height, v.min_depth, v.max_depth].iter().all(|f| f.is_finite())
                || v.width <= 0.0
                || v.height <= 0.0
                || v.width > limits.max_viewport_dimensions[0] as f32
                || v.height > limits.max_viewport_dimensions[1] as f32
                || v.x < lo
                || v.y < lo
                || v.x + v.width > hi
                || v.y + v.height > hi
                || !depth_ok(v.min_depth)
                || !depth_ok(v.max_depth),
            Error::OutOfRange("viewport"),
        )?;
        fail_if(
            scissor.offset.x < 0
                || scissor.offset.y < 0
                || i64::from(scissor.offset.x) + i64::from(scissor.extent.width) > i64::from(i32::MAX)
                || i64::from(scissor.offset.y) + i64::from(scissor.extent.height) > i64::from(i32::MAX),
            Error::OutOfRange("scissor"),
        )
    }

    pub fn set_viewport(&mut self, viewport: vk::Viewport, scissor: vk::Rect2D) -> Result<()> {
        self.require_recording()?;
        self.validate_viewport(&viewport, &scissor)?;
        unsafe {
            self.device.raw.cmd_set_viewport_with_count(self.cb, &[viewport]);
            self.device.raw.cmd_set_scissor_with_count(self.cb, &[scissor]);
        }
        Ok(())
    }

    // ---- pipeline state ----------------------------------------------------

    /// Sets every dynamic depth/stencil state once per recording: depth and stencil
    /// tests off, depth write on with LESS, stencil KEEP/ALWAYS with full masks.
    fn ensure_defaults(&mut self) {
        if self.state_valid {
            return;
        }
        let (d, cb, both) = (&self.device.raw, self.cb, vk::StencilFaceFlags::FRONT_AND_BACK);
        unsafe {
            d.cmd_set_depth_test_enable(cb, false);
            d.cmd_set_depth_write_enable(cb, true);
            d.cmd_set_depth_compare_op(cb, vk::CompareOp::LESS);
            d.cmd_set_stencil_test_enable(cb, false);
            d.cmd_set_stencil_op(cb, both, vk::StencilOp::KEEP, vk::StencilOp::KEEP, vk::StencilOp::KEEP, vk::CompareOp::ALWAYS);
            d.cmd_set_stencil_compare_mask(cb, both, !0);
            d.cmd_set_stencil_write_mask(cb, both, !0);
            d.cmd_set_stencil_reference(cb, both, 0);
        }
        self.depth_enabled = false;
        self.stencil_enabled = false;
        self.state_valid = true;
        self.counters.dynamic_state_commands += 8;
    }

    pub fn bind_pipeline(&mut self, pipeline: &Pipeline) -> Result<()> {
        self.require_recording()?;
        self.ensure_defaults();
        if self.pipeline.as_ref().is_none_or(|p| p.raw != pipeline.0.raw) {
            unsafe { self.device.raw.cmd_bind_pipeline(self.cb, vk::PipelineBindPoint::GRAPHICS, pipeline.0.raw) };
            self.counters.pipeline_binds += 1;
            self.sets = [vk::DescriptorSet::null(); MAX_SET_LAYOUTS];
            self.pipeline = Some(pipeline.0.clone());
        }
        Ok(())
    }

    fn bound_pipeline(&self) -> Result<&Arc<PipelineInner>> {
        self.require_recording()?;
        self.pipeline.as_ref().ok_or(Error::InvalidState("no pipeline bound"))
    }

    /// Binds `set` at `index`; its layout must be the pipeline's layout at that index.
    pub fn bind_set(&mut self, index: u32, set: &BindingSet) -> Result<()> {
        let pipeline = self.bound_pipeline()?;
        let expected = pipeline.set_layouts.get(index as usize).ok_or(Error::InvalidArgument("set index"))?;
        fail_if(!expected.same(set.layout()), Error::InvalidArgument("set layout does not match pipeline"))?;
        if self.sets[index as usize] != set.raw {
            unsafe {
                self.device.raw.cmd_bind_descriptor_sets(
                    self.cb,
                    vk::PipelineBindPoint::GRAPHICS,
                    pipeline.layout,
                    index,
                    &[set.raw],
                    &[],
                )
            };
            self.sets[index as usize] = set.raw;
        }
        Ok(())
    }

    pub fn push_constants(&mut self, stages: vk::ShaderStageFlags, offset: u32, data: &[u8]) -> Result<()> {
        let pipeline = self.bound_pipeline()?;
        fail_if(
            data.is_empty() || data.len() % 4 != 0 || offset % 4 != 0 || stages.is_empty(),
            Error::InvalidArgument("push constant data/offset/stages"),
        )?;
        let size = u32::try_from(data.len()).map_err(|_| Error::OutOfRange("push constant size"))?;
        let (start, end) = (u64::from(offset), u64::from(offset) + u64::from(size));
        let mut covered = vk::ShaderStageFlags::empty();
        for r in &pipeline.push_ranges {
            let (r_start, r_end) = (u64::from(r.offset), u64::from(r.offset) + u64::from(r.size));
            if start >= r_start && end <= r_end {
                covered |= r.stage_flags;
            }
            // Overlapping a range requires naming all of that range's stages.
            fail_if(
                start < r_end && r_start < end && !stages.contains(r.stage_flags),
                Error::InvalidArgument("push constant stages must include every overlapping range"),
            )?;
        }
        fail_if(!covered.contains(stages), Error::OutOfRange("push constants outside the pipeline's ranges"))?;
        unsafe { self.device.raw.cmd_push_constants(self.cb, pipeline.layout, stages, offset, data) };
        Ok(())
    }

    pub fn set_depth_test(&mut self, enabled: bool) -> Result<()> {
        self.require_recording()?;
        self.ensure_defaults();
        if self.depth_enabled != enabled {
            unsafe { self.device.raw.cmd_set_depth_test_enable(self.cb, enabled) };
            self.depth_enabled = enabled;
            self.counters.dynamic_state_commands += 1;
        }
        Ok(())
    }

    pub fn set_stencil_test(&mut self, enabled: bool) -> Result<()> {
        self.require_recording()?;
        self.ensure_defaults();
        if self.stencil_enabled != enabled {
            unsafe { self.device.raw.cmd_set_stencil_test_enable(self.cb, enabled) };
            self.stencil_enabled = enabled;
            self.counters.dynamic_state_commands += 1;
        }
        Ok(())
    }

    pub fn set_depth_state(&mut self, write: bool, compare: vk::CompareOp) -> Result<()> {
        self.require_recording()?;
        fail_if(!compare_valid(compare), Error::InvalidArgument("compare op"))?;
        self.ensure_defaults();
        unsafe {
            self.device.raw.cmd_set_depth_write_enable(self.cb, write);
            self.device.raw.cmd_set_depth_compare_op(self.cb, compare);
        }
        self.counters.dynamic_state_commands += 2;
        Ok(())
    }

    pub fn set_stencil_state(&mut self, front: vk::StencilOpState, back: vk::StencilOpState) -> Result<()> {
        self.require_recording()?;
        let op_valid = |op: vk::StencilOp| (0..=vk::StencilOp::DECREMENT_AND_WRAP.as_raw()).contains(&op.as_raw());
        for s in [&front, &back] {
            fail_if(
                !compare_valid(s.compare_op) || !op_valid(s.fail_op) || !op_valid(s.pass_op) || !op_valid(s.depth_fail_op),
                Error::InvalidArgument("stencil op state"),
            )?;
        }
        self.ensure_defaults();
        for (face, s) in [(vk::StencilFaceFlags::FRONT, front), (vk::StencilFaceFlags::BACK, back)] {
            let (d, cb) = (&self.device.raw, self.cb);
            unsafe {
                d.cmd_set_stencil_op(cb, face, s.fail_op, s.pass_op, s.depth_fail_op, s.compare_op);
                d.cmd_set_stencil_compare_mask(cb, face, s.compare_mask);
                d.cmd_set_stencil_write_mask(cb, face, s.write_mask);
                d.cmd_set_stencil_reference(cb, face, s.reference);
            }
        }
        self.counters.dynamic_state_commands += 8;
        Ok(())
    }

    pub fn bind_vertex_buffer(&mut self, binding: u32, buffer: &Buffer, offset: u64) -> Result<()> {
        self.require_recording()?;
        fail_if(
            !buffer.usage().contains(vk::BufferUsageFlags::VERTEX_BUFFER)
                || offset >= buffer.size()
                || binding >= self.device.properties.limits.max_vertex_input_bindings,
            Error::InvalidArgument("vertex buffer binding"),
        )?;
        unsafe { self.device.raw.cmd_bind_vertex_buffers(self.cb, binding, &[buffer.raw()], &[offset]) };
        Ok(())
    }

    pub fn bind_index_buffer(&mut self, buffer: &Buffer, offset: u64, ty: vk::IndexType) -> Result<()> {
        self.require_recording()?;
        let stride = match ty {
            vk::IndexType::UINT16 => 2,
            vk::IndexType::UINT32 => 4,
            _ => return Err(Error::Unsupported("index type")),
        };
        fail_if(
            !buffer.usage().contains(vk::BufferUsageFlags::INDEX_BUFFER) || !offset.is_multiple_of(stride) || offset >= buffer.size(),
            Error::InvalidArgument("index buffer binding"),
        )?;
        unsafe { self.device.raw.cmd_bind_index_buffer(self.cb, buffer.raw(), offset, ty) };
        self.index_bytes = buffer.size() - offset;
        self.index_stride = stride;
        Ok(())
    }

    fn check_draw(&self) -> Result<()> {
        fail_if(self.phase != Phase::Rendering, Error::InvalidState("draws need an active render pass"))?;
        let p = self.pipeline.as_ref().ok_or(Error::InvalidState("no pipeline bound"))?;
        fail_if(
            p.colors != self.colors || p.depth_format != self.depth_format || p.stencil_format != self.stencil_format,
            Error::InvalidState("pipeline formats do not match the render targets"),
        )?;
        fail_if(
            (self.depth_enabled && self.depth_format == vk::Format::UNDEFINED)
                || (self.stencil_enabled && self.stencil_format == vk::Format::UNDEFINED),
            Error::InvalidState("depth/stencil test enabled without an attachment"),
        )?;
        fail_if(
            self.sets[..p.set_layouts.len()].contains(&vk::DescriptorSet::null()),
            Error::InvalidState("not all binding sets are bound"),
        )
    }

    pub fn draw(&mut self, vertices: u32, instances: u32, first_vertex: u32, first_instance: u32) -> Result<()> {
        self.check_draw()?;
        unsafe { self.device.raw.cmd_draw(self.cb, vertices, instances, first_vertex, first_instance) };
        self.counters.draw_calls += 1;
        self.counters.vertices_drawn += u64::from(vertices) * u64::from(instances);
        Ok(())
    }

    pub fn draw_indexed(
        &mut self,
        indices: u32,
        instances: u32,
        first_index: u32,
        vertex_offset: i32,
        first_instance: u32,
    ) -> Result<()> {
        self.check_draw()?;
        fail_if(
            self.index_stride == 0 || (u64::from(first_index) + u64::from(indices)) * self.index_stride > self.index_bytes,
            Error::OutOfRange("indices exceed the bound index buffer"),
        )?;
        unsafe {
            self.device.raw.cmd_draw_indexed(self.cb, indices, instances, first_index, vertex_offset, first_instance)
        };
        self.counters.draw_calls += 1;
        self.counters.indices_drawn += u64::from(indices) * u64::from(instances);
        Ok(())
    }

    // ---- GPU timing ---------------------------------------------------------

    /// TOP/BOTTOM timestamps bracket GPU execution including barriers inside the
    /// interval. No-op when timestamps are unavailable.
    pub fn begin_timing(&mut self, section: TimingSection) -> Result<()> {
        self.require_recording()?;
        if self.queries == vk::QueryPool::null() {
            return Ok(());
        }
        fail_if(self.timing_used & section.bit() != 0, Error::InvalidState("timing section already used"))?;
        let query = 2 * section as u32;
        unsafe { self.device.raw.cmd_write_timestamp2(self.cb, vk::PipelineStageFlags2::TOP_OF_PIPE, self.queries, query) };
        self.timing_used |= section.bit();
        self.timing_open |= section.bit();
        Ok(())
    }

    pub fn end_timing(&mut self, section: TimingSection) -> Result<()> {
        self.require_recording()?;
        if self.queries == vk::QueryPool::null() {
            return Ok(());
        }
        fail_if(self.timing_open & section.bit() == 0, Error::InvalidState("timing section not open"))?;
        let query = 2 * section as u32 + 1;
        unsafe {
            self.device.raw.cmd_write_timestamp2(self.cb, vk::PipelineStageFlags2::BOTTOM_OF_PIPE, self.queries, query)
        };
        self.timing_open &= !section.bit();
        Ok(())
    }

    /// Called after the fence signalled; never waits.
    fn collect_timings(&mut self) -> Result<()> {
        if self.queries == vk::QueryPool::null() || self.timing_used == 0 {
            return Ok(());
        }
        let d = &self.device;
        let period = f64::from(d.properties.limits.timestamp_period);
        let mut elapsed = [None; TimingSection::COUNT];
        for section in TimingSection::ALL.into_iter().filter(|s| self.timing_used & s.bit() != 0) {
            let mut ticks = [0u64; 2];
            d.vk(unsafe {
                d.raw.get_query_pool_results(self.queries, 2 * section as u32, &mut ticks, vk::QueryResultFlags::TYPE_64)
            })?;
            elapsed[section as usize] = Some(timestamp_elapsed_ms(ticks[0], ticks[1], d.timestamp_valid_bits, period));
        }
        let timings = &mut d.state().stats.gpu_timings;
        for (timing, ms) in timings.iter_mut().zip(elapsed) {
            if let Some(ms) = ms {
                timing.milliseconds = ms;
                timing.total_milliseconds += ms;
                timing.samples += 1;
            }
        }
        self.timing_used = 0;
        Ok(())
    }
}

impl Drop for Command {
    fn drop(&mut self) {
        let d = &self.device.raw;
        unsafe {
            if self.phase == Phase::Pending && !self.device.is_lost() {
                let _ = d.wait_for_fences(&[self.fence], true, u64::MAX);
            }
            d.destroy_query_pool(self.queries, None);
            d.destroy_fence(self.fence, None);
            d.destroy_command_pool(self.pool, None);
        }
    }
}

fn compare_valid(op: vk::CompareOp) -> bool {
    (0..=vk::CompareOp::ALWAYS.as_raw()).contains(&op.as_raw())
}

/// Masks the integer difference to the queue's valid bits (handles wraparound)
/// before converting, so huge absolute counters keep exact deltas.
pub(crate) fn timestamp_elapsed_ms(start: u64, end: u64, valid_bits: u32, period_ns: f64) -> f64 {
    if !(1..=64).contains(&valid_bits) || period_ns <= 0.0 {
        return 0.0;
    }
    let mask = if valid_bits == 64 { u64::MAX } else { (1 << valid_bits) - 1 };
    (end.wrapping_sub(start) & mask) as f64 * period_ns / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::timestamp_elapsed_ms as ms;

    #[test]
    fn timestamp_conversion_handles_period_and_wraparound() {
        assert_eq!(ms(100, 2100, 64, 2.5), 0.005);
        assert_eq!(ms(0xffff_fff0, 0x10, 32, 1.0), 0.000032);
        assert_eq!(ms(u64::MAX - 15, 16, 64, 1.0), 0.000032);
        assert_eq!(ms(0x1234_fff0, 0x5678_0010, 16, 1.0), 0.000032);
        assert_eq!(ms((1 << 60) + 3, (1 << 60) + 10, 64, 1.0), 0.000007);
        assert_eq!(ms(10, 10, 64, 1.0), 0.0);
        assert_eq!(ms(0, 10, 0, 1.0), 0.0);
        assert_eq!(ms(0, 10, 65, 1.0), 0.0);
        assert_eq!(ms(0, 10, 64, 0.0), 0.0);
    }
}
