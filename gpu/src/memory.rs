use crate::{Device, Error, Result, fail_if, vk};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

const BLOCK_SIZE: u64 = 16 * 1024 * 1024;

/// Host-visible kinds are coherent and persistently mapped. Synchronize with the
/// GPU before reading or writing them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    /// CPU writes, GPU reads (staging, dynamic data).
    Upload,
    /// GPU only.
    Device,
    /// GPU writes, CPU reads.
    Readback,
}

/// Mapped host pointer; access is guarded by `&mut` on the owning buffer.
#[derive(Clone, Copy)]
struct Mapped(NonNull<u8>);
unsafe impl Send for Mapped {}
unsafe impl Sync for Mapped {}

#[derive(Clone, Copy)]
pub(crate) struct Allocation {
    block: usize,
    offset: u64,
}

struct Block {
    memory: vk::DeviceMemory,
    size: u64,
    mapped: Option<Mapped>,
    memory_type: u32,
    image: bool,
    dedicated: bool,
    /// Live `(offset, end)` ranges sorted by offset.
    ranges: Vec<(u64, u64)>,
}

impl Block {
    /// First-fit search; returns the aligned offset and the insertion index.
    fn find(&self, size: u64, alignment: u64, steps: &mut u64) -> Option<(u64, usize)> {
        let mut start = 0u64;
        for (i, &(offset, end)) in self.ranges.iter().chain([(self.size, self.size)].iter()).enumerate() {
            *steps += 1;
            let aligned = start.next_multiple_of(alignment.max(1));
            if aligned.checked_add(size)? <= offset {
                return Some((aligned, i));
            }
            start = end;
        }
        None
    }
}

/// Suballocates 16 MiB blocks per memory type; large or driver-requested
/// resources get dedicated allocations.
#[derive(Default)]
pub(crate) struct Allocator {
    blocks: Vec<Option<Block>>,
}

impl Allocator {
    pub(crate) fn release_all(&mut self, device: &ash::Device) {
        for block in self.blocks.drain(..).flatten() {
            release_block(device, block, &mut 0);
        }
    }
}

fn release_block(device: &ash::Device, block: Block, reserved: &mut u64) {
    unsafe {
        if block.mapped.is_some() {
            device.unmap_memory(block.memory);
        }
        device.free_memory(block.memory, None);
    }
    *reserved = reserved.saturating_sub(block.size);
}

enum Target {
    Buffer(vk::Buffer),
    Image(vk::Image),
}

impl Device {
    fn memory_type(&self, mask: u32, kind: MemoryKind) -> Option<u32> {
        use vk::MemoryPropertyFlags as F;
        let required = if kind == MemoryKind::Device { F::DEVICE_LOCAL } else { F::HOST_VISIBLE | F::HOST_COHERENT };
        let props = &self.memory_properties;
        let mut best: Option<(i32, u32)> = None;
        for (i, ty) in props.memory_types[..props.memory_type_count as usize].iter().enumerate() {
            let flags = ty.property_flags;
            if mask & (1 << i) == 0 || !flags.contains(required) {
                continue;
            }
            let score = match kind {
                MemoryKind::Readback if flags.contains(F::HOST_CACHED) => 2,
                MemoryKind::Upload if flags.contains(F::DEVICE_LOCAL) => 1,
                MemoryKind::Device if !flags.contains(F::HOST_VISIBLE) => 1,
                _ => 0,
            };
            if best.is_none_or(|(s, _)| score > s) {
                best = Some((score, i as u32));
            }
        }
        best.map(|(_, i)| i)
    }

    /// Allocates and binds memory for a buffer or image.
    fn allocate(
        &self,
        target: Target,
        kind: MemoryKind,
    ) -> Result<(Allocation, vk::DeviceMemory, u64, Option<Mapped>)> {
        let mut dedicated_req = vk::MemoryDedicatedRequirements::default();
        let mut req2 = vk::MemoryRequirements2::default().push_next(&mut dedicated_req);
        unsafe {
            match target {
                Target::Buffer(buffer) => self.raw.get_buffer_memory_requirements2(
                    &vk::BufferMemoryRequirementsInfo2::default().buffer(buffer),
                    &mut req2,
                ),
                Target::Image(image) => self
                    .raw
                    .get_image_memory_requirements2(&vk::ImageMemoryRequirementsInfo2::default().image(image), &mut req2),
            }
        }
        let req = req2.memory_requirements;
        let is_image = matches!(target, Target::Image(_));
        let dedicated = dedicated_req.requires_dedicated_allocation != 0 || req.size > BLOCK_SIZE;
        let ty = self.memory_type(req.memory_type_bits, kind).ok_or(Error::OutOfDeviceMemory)?;

        let mut state = self.state();
        let state = &mut *state;
        let blocks = &mut state.memory.blocks;
        if !dedicated {
            for (index, slot) in blocks.iter_mut().enumerate() {
                let Some(block) = slot else { continue };
                if block.dedicated || block.memory_type != ty || block.image != is_image {
                    continue;
                }
                if let Some((offset, at)) = block.find(req.size, req.alignment, &mut state.stats.allocation_search_steps) {
                    block.ranges.insert(at, (offset, offset + req.size));
                    let mapped = block.mapped.map(|m| Mapped(unsafe { m.0.add(offset as usize) }));
                    return Ok((Allocation { block: index, offset }, block.memory, offset, mapped));
                }
            }
        }

        // New block. Recycle one empty block first so idle memory does not pile up.
        if let Some(slot) = blocks.iter_mut().find(|b| b.as_ref().is_some_and(|b| b.ranges.is_empty())) {
            release_block(&self.raw, slot.take().unwrap(), &mut state.stats.reserved_bytes);
        }
        let index = blocks.iter().position(Option::is_none).unwrap_or_else(|| {
            blocks.push(None);
            blocks.len() - 1
        });
        let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default();
        match target {
            Target::Buffer(buffer) => dedicated_info = dedicated_info.buffer(buffer),
            Target::Image(image) => dedicated_info = dedicated_info.image(image),
        }
        let mut flags = vk::MemoryAllocateFlagsInfo::default();
        if self.features.buffer_device_address && !is_image {
            flags = flags.flags(vk::MemoryAllocateFlags::DEVICE_ADDRESS);
        }
        let size = if dedicated { req.size } else { BLOCK_SIZE };
        let mut info = vk::MemoryAllocateInfo::default().allocation_size(size).memory_type_index(ty).push_next(&mut flags);
        if dedicated {
            info = info.push_next(&mut dedicated_info);
        }
        let memory = self.vk(unsafe { self.raw.allocate_memory(&info, None) })?;
        state.stats.memory_allocations += 1;
        state.stats.reserved_bytes += size;
        let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let mut mapped = None;
        if !is_image && self.memory_properties.memory_types[ty as usize].property_flags.contains(host) {
            match unsafe { self.raw.map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()) } {
                Ok(ptr) => mapped = NonNull::new(ptr.cast::<u8>()).map(Mapped),
                Err(e) => {
                    unsafe { self.raw.free_memory(memory, None) };
                    state.stats.reserved_bytes -= size;
                    return Err(e.into());
                }
            }
        }
        blocks[index] =
            Some(Block { memory, size, mapped, memory_type: ty, image: is_image, dedicated, ranges: vec![(0, req.size)] });
        Ok((Allocation { block: index, offset: 0 }, memory, 0, mapped))
    }

    fn free(&self, allocation: Allocation) {
        let mut state = self.state();
        let state = &mut *state;
        let slot = &mut state.memory.blocks[allocation.block];
        let Some(block) = slot else { return };
        if let Ok(i) = block.ranges.binary_search_by_key(&allocation.offset, |r| r.0) {
            block.ranges.remove(i);
        }
        if block.dedicated {
            release_block(&self.raw, slot.take().unwrap(), &mut state.stats.reserved_bytes);
        }
    }

    pub fn create_buffer(self: &Arc<Self>, size: u64, usage: vk::BufferUsageFlags, memory: MemoryKind) -> Result<Buffer> {
        fail_if(size == 0, Error::InvalidArgument("buffer size is zero"))?;
        fail_if(usage.is_empty(), Error::InvalidArgument("buffer usage is empty"))?;
        fail_if(memory != MemoryKind::Device && size > isize::MAX as u64, Error::OutOfRange("mapped buffer size"))?;
        fail_if(
            usage.contains(vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS) && !self.features.buffer_device_address,
            Error::InvalidArgument("buffer_device_address was not enabled"),
        )?;
        let info = vk::BufferCreateInfo::default().size(size).usage(usage);
        let raw = self.vk(unsafe { self.raw.create_buffer(&info, None) })?;
        let bound = self.allocate(Target::Buffer(raw), memory).and_then(|(alloc, mem, offset, mapped)| {
            if let Err(e) = self.vk(unsafe { self.raw.bind_buffer_memory(raw, mem, offset) }) {
                self.free(alloc);
                return Err(e);
            }
            Ok((alloc, mapped))
        });
        let (alloc, mapped) = match bound {
            Ok(v) => v,
            Err(e) => {
                unsafe { self.raw.destroy_buffer(raw, None) };
                return Err(e);
            }
        };
        let address = if usage.contains(vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS) {
            unsafe { self.raw.get_buffer_device_address(&vk::BufferDeviceAddressInfo::default().buffer(raw)) }
        } else {
            0
        };
        Ok(Buffer { device: self.clone(), raw, size, usage, mapped, address, alloc })
    }

    pub fn create_texture(self: &Arc<Self>, desc: &TextureDesc) -> Result<Texture> {
        use vk::ImageUsageFlags as U;
        let allowed = U::SAMPLED | U::STORAGE | U::COLOR_ATTACHMENT | U::DEPTH_STENCIL_ATTACHMENT | U::TRANSFER_SRC | U::TRANSFER_DST;
        let consumers = U::SAMPLED | U::STORAGE | U::COLOR_ATTACHMENT | U::DEPTH_STENCIL_ATTACHMENT;
        fail_if(desc.width == 0 || desc.height == 0, Error::InvalidArgument("texture extent is zero"))?;
        fail_if(desc.format == vk::Format::UNDEFINED, Error::InvalidArgument("texture format is undefined"))?;
        fail_if(!allowed.contains(desc.usage), Error::InvalidArgument("unsupported texture usage"))?;
        fail_if(!desc.usage.intersects(consumers), Error::InvalidArgument("texture usage has no consumer"))?;
        let mips = desc.mip_levels.max(1);
        fail_if(mips > 32 - desc.width.max(desc.height).leading_zeros(), Error::OutOfRange("mip levels"))?;
        let limits = unsafe {
            self.instance().get_physical_device_image_format_properties(
                self.physical,
                desc.format,
                vk::ImageType::TYPE_2D,
                vk::ImageTiling::OPTIMAL,
                desc.usage,
                vk::ImageCreateFlags::empty(),
            )
        }
        .map_err(|e| match e {
            vk::Result::ERROR_FORMAT_NOT_SUPPORTED => Error::Unsupported("texture format/usage"),
            e => e.into(),
        })?;
        fail_if(
            desc.width > limits.max_extent.width || desc.height > limits.max_extent.height || mips > limits.max_mip_levels,
            Error::OutOfRange("texture extent"),
        )?;
        let extent = vk::Extent2D { width: desc.width, height: desc.height };
        let aspects = format_aspects(desc.format);
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(desc.format)
            .extent(extent.into())
            .mip_levels(mips)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(desc.usage);
        let image = self.vk(unsafe { self.raw.create_image(&info, None) })?;
        let mut texture = Texture {
            device: self.clone(),
            image,
            view: vk::ImageView::null(),
            format: desc.format,
            extent,
            aspects,
            mip_levels: mips,
            usage: desc.usage,
            layout: Arc::new(AtomicI32::new(vk::ImageLayout::UNDEFINED.as_raw())),
            alloc: None,
        };
        // From here `texture`'s Drop cleans up whatever has been created.
        let (alloc, memory, offset, _) = self.allocate(Target::Image(image), MemoryKind::Device)?;
        texture.alloc = Some(alloc);
        self.vk(unsafe { self.raw.bind_image_memory(image, memory, offset) })?;
        texture.view = self.create_view(image, desc.format, aspects, mips)?;
        Ok(texture)
    }

    pub(crate) fn create_view(
        &self,
        image: vk::Image,
        format: vk::Format,
        aspects: vk::ImageAspectFlags,
        mips: u32,
    ) -> Result<vk::ImageView> {
        let info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .subresource_range(subresource_range(aspects, mips));
        self.vk(unsafe { self.raw.create_image_view(&info, None) })
    }

    /// `None` creates a linear, clamp-to-edge sampler with all mips.
    pub fn create_sampler(self: &Arc<Self>, info: Option<&vk::SamplerCreateInfo>) -> Result<Sampler> {
        let default = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
            .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .max_lod(vk::LOD_CLAMP_NONE);
        let info = info.unwrap_or(&default);
        fail_if(!info.p_next.is_null() || !info.flags.is_empty(), Error::InvalidArgument("sampler extensions/flags"))?;
        if info.anisotropy_enable != 0 {
            fail_if(!self.features.sampler_anisotropy, Error::InvalidArgument("sampler_anisotropy was not enabled"))?;
            fail_if(
                !(1.0..=self.properties.limits.max_sampler_anisotropy).contains(&info.max_anisotropy),
                Error::OutOfRange("max_anisotropy"),
            )?;
        }
        let raw = self.vk(unsafe { self.raw.create_sampler(info, None) })?;
        Ok(Sampler { device: self.clone(), raw })
    }
}

pub struct Buffer {
    device: Arc<Device>,
    raw: vk::Buffer,
    size: u64,
    usage: vk::BufferUsageFlags,
    mapped: Option<Mapped>,
    address: vk::DeviceAddress,
    alloc: Allocation,
}

impl Buffer {
    pub fn raw(&self) -> vk::Buffer {
        self.raw
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn usage(&self) -> vk::BufferUsageFlags {
        self.usage
    }
    /// Zero unless created with `SHADER_DEVICE_ADDRESS`.
    pub fn address(&self) -> vk::DeviceAddress {
        self.address
    }
    /// Host view of an `Upload`/`Readback` buffer.
    pub fn mapped(&self) -> Option<&[u8]> {
        self.mapped.map(|m| unsafe { std::slice::from_raw_parts(m.0.as_ptr(), self.size as usize) })
    }
    pub fn mapped_mut(&mut self) -> Option<&mut [u8]> {
        self.mapped.map(|m| unsafe { std::slice::from_raw_parts_mut(m.0.as_ptr(), self.size as usize) })
    }

    /// Copies `data` into a host-visible buffer at `offset`.
    pub fn write(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        let bytes = self.mapped_mut().ok_or(Error::NotMapped)?;
        let range = usize::try_from(offset).ok().and_then(|start| bytes.get_mut(start..start.checked_add(data.len())?));
        range.ok_or(Error::OutOfRange("buffer write"))?.copy_from_slice(data);
        Ok(())
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe { self.device.raw.destroy_buffer(self.raw, None) };
        self.device.free(self.alloc);
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TextureDesc {
    pub width: u32,
    pub height: u32,
    pub format: vk::Format,
    pub usage: vk::ImageUsageFlags,
    /// Zero selects one mip.
    pub mip_levels: u32,
}

/// A 2D image with a full view. Its layout is tracked automatically and becomes
/// visible here once the command that changed it has been submitted.
pub struct Texture {
    pub(crate) device: Arc<Device>,
    pub(crate) image: vk::Image,
    pub(crate) view: vk::ImageView,
    pub(crate) format: vk::Format,
    pub(crate) extent: vk::Extent2D,
    pub(crate) aspects: vk::ImageAspectFlags,
    pub(crate) mip_levels: u32,
    pub(crate) usage: vk::ImageUsageFlags,
    pub(crate) layout: Arc<AtomicI32>,
    /// `None` for swapchain images, which the swapchain owns.
    pub(crate) alloc: Option<Allocation>,
}

impl Texture {
    /// Wraps a swapchain image; only the view is owned.
    pub(crate) fn swapchain_image(
        device: &Arc<Device>,
        image: vk::Image,
        format: vk::Format,
        extent: vk::Extent2D,
    ) -> Result<Texture> {
        let view = device.create_view(image, format, vk::ImageAspectFlags::COLOR, 1)?;
        Ok(Texture {
            device: device.clone(),
            image,
            view,
            format,
            extent,
            aspects: vk::ImageAspectFlags::COLOR,
            mip_levels: 1,
            usage: vk::ImageUsageFlags::COLOR_ATTACHMENT,
            layout: Arc::new(AtomicI32::new(vk::ImageLayout::UNDEFINED.as_raw())),
            alloc: None,
        })
    }

    pub fn raw(&self) -> vk::Image {
        self.image
    }
    pub fn view(&self) -> vk::ImageView {
        self.view
    }
    pub fn format(&self) -> vk::Format {
        self.format
    }
    pub fn extent(&self) -> vk::Extent2D {
        self.extent
    }
    pub fn mip_levels(&self) -> u32 {
        self.mip_levels
    }
    pub fn usage(&self) -> vk::ImageUsageFlags {
        self.usage
    }
    /// Layout after the last submitted command that touched this texture.
    pub fn layout(&self) -> vk::ImageLayout {
        vk::ImageLayout::from_raw(self.layout.load(Ordering::Relaxed))
    }
    /// The whole texture as a render area.
    pub fn rect(&self) -> vk::Rect2D {
        vk::Rect2D { offset: vk::Offset2D::default(), extent: self.extent }
    }
}

impl Drop for Texture {
    fn drop(&mut self) {
        unsafe { self.device.raw.destroy_image_view(self.view, None) };
        if let Some(alloc) = self.alloc {
            unsafe { self.device.raw.destroy_image(self.image, None) };
            self.device.free(alloc);
        }
    }
}

/// Independently owned; may be shared among any number of bindings.
pub struct Sampler {
    device: Arc<Device>,
    raw: vk::Sampler,
}

impl Sampler {
    pub fn raw(&self) -> vk::Sampler {
        self.raw
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        unsafe { self.device.raw.destroy_sampler(self.raw, None) };
    }
}

pub(crate) fn subresource_range(aspects: vk::ImageAspectFlags, mips: u32) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange { aspect_mask: aspects, base_mip_level: 0, level_count: mips, base_array_layer: 0, layer_count: 1 }
}

/// Bytes per texel for the uncompressed color formats uploads support; 0 otherwise.
pub(crate) fn format_bytes(format: vk::Format) -> u32 {
    use vk::Format as F;
    match format {
        F::R8_UNORM | F::R8_UINT => 1,
        F::R8G8_UNORM | F::R16_UNORM | F::R16_SFLOAT => 2,
        F::R8G8B8A8_UNORM | F::B8G8R8A8_UNORM | F::R8G8B8A8_SRGB | F::B8G8R8A8_SRGB | F::R16G16_SFLOAT | F::R32_SFLOAT
        | F::R32_UINT => 4,
        F::R16G16B16A16_SFLOAT | F::R32G32_SFLOAT => 8,
        F::R32G32B32_SFLOAT => 12,
        F::R32G32B32A32_SFLOAT => 16,
        _ => 0,
    }
}

pub(crate) fn format_aspects(format: vk::Format) -> vk::ImageAspectFlags {
    use vk::{Format as F, ImageAspectFlags as A};
    match format {
        F::D16_UNORM | F::X8_D24_UNORM_PACK32 | F::D32_SFLOAT => A::DEPTH,
        F::S8_UINT => A::STENCIL,
        F::D16_UNORM_S8_UINT | F::D24_UNORM_S8_UINT | F::D32_SFLOAT_S8_UINT => A::DEPTH | A::STENCIL,
        _ => A::COLOR,
    }
}
