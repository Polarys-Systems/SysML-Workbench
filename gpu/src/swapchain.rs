use crate::{Command, Device, Error, FRAMES, Result, Texture, Window, fail_if, vk};
use std::sync::Arc;

/// The current frame: record into `command`, render into `target`, then call
/// [`Swapchain::end_frame`].
pub struct Frame<'a> {
    pub command: &'a mut Command,
    pub target: &'a Texture,
}

/// Swapchain images plus their "rendering complete" semaphores.
#[derive(Default)]
struct Chain {
    handle: vk::SwapchainKHR,
    format: vk::Format,
    extent: vk::Extent2D,
    images: Vec<Texture>,
    complete: Vec<vk::Semaphore>,
}

impl Chain {
    fn destroy(&mut self, device: &Device) {
        self.images.clear(); // views first
        unsafe {
            for &semaphore in &self.complete {
                device.raw.destroy_semaphore(semaphore, None);
            }
            if let Some(swapchain_fn) = &device.swapchain_fn {
                swapchain_fn.destroy_swapchain(self.handle, None);
            }
        }
        *self = Chain::default();
    }
}

/// FIFO (vsync) swapchain with [`FRAMES`] frames in flight. The window must
/// outlive the swapchain.
pub struct Swapchain {
    device: Arc<Device>,
    surface: vk::SurfaceKHR,
    chain: Chain,
    frames: Vec<Command>,
    acquired: [vk::Semaphore; FRAMES],
    acquire_pending: [bool; FRAMES],
    frame: usize,
    image: u32,
    active: bool,
    needs_rebuild: bool,
    window_size: (u32, u32),
}

impl Device {
    /// `width`/`height` are the window's size in pixels; they are only used when
    /// the surface does not report its own extent.
    pub fn create_swapchain(self: &Arc<Self>, window: &dyn Window, width: u32, height: u32) -> Result<Swapchain> {
        let surface_fn = self.surface_fn.as_ref().ok_or(Error::Unsupported("device was created without a window"))?;
        let display = window.display_handle().map_err(|_| Error::NoSurface)?.as_raw();
        let handle = window.window_handle().map_err(|_| Error::NoSurface)?.as_raw();
        let surface = unsafe { ash_window::create_surface(&self.entry, self.instance(), display, handle, None) }
            .map_err(|_| Error::NoSurface)?;
        // From here the swapchain's Drop cleans up partial construction.
        let mut swapchain = Swapchain {
            device: self.clone(),
            surface,
            chain: Chain::default(),
            frames: Vec::with_capacity(FRAMES),
            acquired: [vk::Semaphore::null(); FRAMES],
            acquire_pending: [false; FRAMES],
            frame: 0,
            image: 0,
            active: false,
            needs_rebuild: false,
            window_size: (width, height),
        };
        let supported = self.vk(unsafe {
            surface_fn.get_physical_device_surface_support(self.physical, self.queue_family, surface)
        })?;
        fail_if(!supported, Error::Unsupported("queue cannot present to this surface"))?;
        swapchain.chain = swapchain.build_chain(vk::SwapchainKHR::null())?;
        for i in 0..FRAMES {
            let mut command = self.create_command()?;
            command.frame_owned = true;
            swapchain.frames.push(command);
            swapchain.acquired[i] = self.create_semaphore()?;
        }
        Ok(swapchain)
    }

    fn create_semaphore(&self) -> Result<vk::Semaphore> {
        self.vk(unsafe { self.raw.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) })
    }
}

impl Swapchain {
    pub fn format(&self) -> vk::Format {
        self.chain.format
    }
    pub fn extent(&self) -> vk::Extent2D {
        self.chain.extent
    }

    fn build_chain(&self, old: vk::SwapchainKHR) -> Result<Chain> {
        let device = &self.device;
        let (surface_fn, swapchain_fn) = (device.surface_fn.as_ref().unwrap(), device.swapchain_fn.as_ref().unwrap());
        let (width, height) = self.window_size;
        fail_if(width == 0 || height == 0, Error::OutOfDate)?; // minimized
        let caps = device.vk(unsafe { surface_fn.get_physical_device_surface_capabilities(device.physical, self.surface) })?;
        fail_if(
            !caps.supported_usage_flags.contains(vk::ImageUsageFlags::COLOR_ATTACHMENT),
            Error::Unsupported("surface cannot be rendered to"),
        )?;
        let formats = device.vk(unsafe { surface_fn.get_physical_device_surface_formats(device.physical, self.surface) })?;
        let mut selected = *formats.first().ok_or(Error::Unsupported("surface has no formats"))?;
        if selected.format == vk::Format::UNDEFINED {
            selected.format = vk::Format::B8G8R8A8_SRGB;
        }
        if let Some(srgb) = formats.iter().find(|f| {
            matches!(f.format, vk::Format::B8G8R8A8_SRGB | vk::Format::R8G8B8A8_SRGB)
                && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
        }) {
            selected = *srgb;
        }
        let mut extent = caps.current_extent;
        if extent.width == u32::MAX {
            extent = vk::Extent2D {
                width: width.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: height.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            };
        }
        fail_if(extent.width == 0 || extent.height == 0, Error::OutOfDate)?;
        let mut image_count = caps.min_image_count + 1;
        if caps.max_image_count != 0 {
            image_count = image_count.min(caps.max_image_count);
        }
        use vk::CompositeAlphaFlagsKHR as Alpha;
        let alpha = [Alpha::OPAQUE, Alpha::PRE_MULTIPLIED, Alpha::POST_MULTIPLIED, Alpha::INHERIT]
            .into_iter()
            .find(|&a| caps.supported_composite_alpha.contains(a))
            .unwrap_or(Alpha::OPAQUE);
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(self.surface)
            .min_image_count(image_count)
            .image_format(selected.format)
            .image_color_space(selected.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(alpha)
            .present_mode(vk::PresentModeKHR::FIFO)
            .clipped(true)
            .old_swapchain(old);
        let mut chain = Chain { format: selected.format, extent, ..Chain::default() };
        let result = (|| {
            chain.handle = device.vk(unsafe { swapchain_fn.create_swapchain(&info, None) })?;
            for image in device.vk(unsafe { swapchain_fn.get_swapchain_images(chain.handle) })? {
                chain.images.push(Texture::swapchain_image(device, image, selected.format, extent)?);
                chain.complete.push(device.create_semaphore()?);
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok(chain),
            Err(e) => {
                chain.destroy(device);
                Err(e)
            }
        }
    }

    /// Consumes acquire signals that were never waited on (a failed submit does
    /// not consume them, and device idle alone cannot wait for them), then idles.
    fn drain_acquisitions(&mut self) -> Result<()> {
        let device = &self.device;
        for (pending, &semaphore) in self.acquire_pending.iter_mut().zip(&self.acquired) {
            if *pending {
                let wait = [vk::SemaphoreSubmitInfo::default()
                    .semaphore(semaphore)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
                let submit = vk::SubmitInfo2::default().wait_semaphore_infos(&wait);
                device.vk(unsafe { device.raw.queue_submit2(device.queue, &[submit], vk::Fence::null()) })?;
                *pending = false;
            }
        }
        device.wait_idle()
    }

    /// Rebuilds for a new window size. Call after resizes and whenever a frame
    /// call returns [`Error::OutOfDate`]; a zero size (minimized) keeps it out of date.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        fail_if(self.active, Error::InvalidState("a frame is active"))?;
        self.window_size = (width, height);
        self.needs_rebuild = true;
        self.drain_acquisitions()?;
        // The old swapchain is retired by the create call even if that fails.
        let mut old = std::mem::take(&mut self.chain);
        let built = self.build_chain(old.handle);
        old.destroy(&self.device);
        let mut reset = Ok(());
        for (command, semaphore) in self.frames.iter_mut().zip(&mut self.acquired) {
            let r = command.wait().and_then(|()| command.reset());
            unsafe { self.device.raw.destroy_semaphore(*semaphore, None) };
            *semaphore = vk::Semaphore::null();
            let r = r.and_then(|()| self.device.create_semaphore()).map(|s| *semaphore = s);
            reset = reset.and(r);
        }
        let mut chain = built?;
        if let Err(e) = reset {
            chain.destroy(&self.device);
            return Err(e);
        }
        self.chain = chain;
        self.needs_rebuild = false;
        self.frame = 0;
        Ok(())
    }

    /// Waits for this frame slot, begins its command and acquires an image.
    pub fn begin_frame(&mut self) -> Result<Frame<'_>> {
        fail_if(self.active, Error::InvalidState("a frame is already active"))?;
        fail_if(self.needs_rebuild || self.chain.handle == vk::SwapchainKHR::null(), Error::OutOfDate)?;
        let command = &mut self.frames[self.frame];
        if let Err(e) = command.wait().and_then(|()| command.begin_internal()) {
            self.needs_rebuild = true;
            return Err(e);
        }
        let swapchain_fn = self.device.swapchain_fn.as_ref().unwrap();
        let acquired = unsafe {
            swapchain_fn.acquire_next_image(self.chain.handle, u64::MAX, self.acquired[self.frame], vk::Fence::null())
        };
        match acquired {
            Ok((image, suboptimal)) => {
                self.image = image;
                self.active = true;
                self.acquire_pending[self.frame] = true;
                self.needs_rebuild = suboptimal;
                Ok(Frame { command: &mut self.frames[self.frame], target: &self.chain.images[image as usize] })
            }
            Err(e) => {
                let _ = command.reset();
                self.needs_rebuild = true;
                Err(self.device.error(e))
            }
        }
    }

    /// Transitions the target for presentation, submits and presents. Returns
    /// [`Error::OutOfDate`] when the swapchain should be resized before the next frame.
    pub fn end_frame(&mut self) -> Result<()> {
        fail_if(!self.active, Error::InvalidState("no active frame"))?;
        let image = self.image as usize;
        let command = &mut self.frames[self.frame];
        command.transition_texture(
            &self.chain.images[image],
            vk::ImageLayout::PRESENT_SRC_KHR,
            vk::PipelineStageFlags2::NONE,
            vk::AccessFlags2::NONE,
        )?;
        let submitted = command.submit_internal(self.acquired[self.frame], self.chain.complete[image]);
        self.active = false;
        if let Err(e) = submitted {
            self.needs_rebuild = true;
            return Err(e);
        }
        self.acquire_pending[self.frame] = false;
        let waits = [self.chain.complete[image]];
        let swapchains = [self.chain.handle];
        let indices = [self.image];
        let present =
            vk::PresentInfoKHR::default().wait_semaphores(&waits).swapchains(&swapchains).image_indices(&indices);
        let presented = unsafe { self.device.swapchain_fn.as_ref().unwrap().queue_present(self.device.queue, &present) };
        self.frame = (self.frame + 1) % FRAMES;
        match presented {
            Ok(false) => {}
            Ok(true) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.needs_rebuild = true,
            Err(e) => {
                self.needs_rebuild = true;
                return Err(self.device.error(e));
            }
        }
        fail_if(self.needs_rebuild, Error::OutOfDate)
    }

    /// Abandons the active frame's recording but still presents the acquired
    /// image (cleared to undefined contents) so the acquisition is consumed.
    pub fn cancel_frame(&mut self) -> Result<()> {
        fail_if(!self.active, Error::InvalidState("no active frame"))?;
        let command = &mut self.frames[self.frame];
        let mut result = command.reset().and_then(|()| command.begin_internal());
        if result.is_ok() {
            result = self.end_frame();
        }
        if result.is_err() {
            self.active = false;
            self.needs_rebuild = true;
        }
        result
    }
}

impl Drop for Swapchain {
    fn drop(&mut self) {
        if !self.device.is_lost() {
            let _ = self.drain_acquisitions();
        }
        self.chain.destroy(&self.device);
        unsafe {
            for &semaphore in &self.acquired {
                self.device.raw.destroy_semaphore(semaphore, None);
            }
            self.device.surface_fn.as_ref().unwrap().destroy_surface(self.surface, None);
        }
    }
}
