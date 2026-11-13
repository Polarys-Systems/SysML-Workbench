use crate::{Buffer, Device, Error, Result, Sampler, Texture, fail_if, vk};
use std::sync::Arc;

const TYPES: [vk::DescriptorType; 6] = [
    vk::DescriptorType::SAMPLER,
    vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
    vk::DescriptorType::SAMPLED_IMAGE,
    vk::DescriptorType::STORAGE_IMAGE,
    vk::DescriptorType::UNIFORM_BUFFER,
    vk::DescriptorType::STORAGE_BUFFER,
];
const SETS_PER_POOL: u32 = 64;
const MIN_POOL_DESCRIPTORS: u32 = 128;

type Budget = [u32; TYPES.len()];

fn type_index(ty: vk::DescriptorType) -> Option<usize> {
    TYPES.iter().position(|&t| t == ty)
}

#[derive(Clone, Copy, Debug)]
pub struct BindingDesc {
    pub binding: u32,
    pub ty: vk::DescriptorType,
    pub stages: vk::ShaderStageFlags,
    pub count: u32,
}

/// Shared descriptor set layout. Cloning is cheap; pipelines and sets keep it alive.
#[derive(Clone)]
pub struct BindingLayout(pub(crate) Arc<LayoutInner>);

pub(crate) struct LayoutInner {
    device: Arc<Device>,
    pub raw: vk::DescriptorSetLayout,
    bindings: Vec<BindingDesc>,
    budget: Budget,
}

impl Drop for LayoutInner {
    fn drop(&mut self) {
        unsafe { self.device.raw.destroy_descriptor_set_layout(self.raw, None) };
    }
}

impl BindingLayout {
    pub fn raw(&self) -> vk::DescriptorSetLayout {
        self.0.raw
    }
    pub(crate) fn same(&self, other: &BindingLayout) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

pub(crate) struct Pool {
    pub raw: vk::DescriptorPool,
    live_sets: u32,
    available: Budget,
}

/// A descriptor set allocated from device-managed pools.
pub struct BindingSet {
    layout: BindingLayout,
    pub(crate) raw: vk::DescriptorSet,
    pool: usize,
}

impl BindingSet {
    pub fn raw(&self) -> vk::DescriptorSet {
        self.raw
    }
    pub fn layout(&self) -> &BindingLayout {
        &self.layout
    }
}

impl Drop for BindingSet {
    fn drop(&mut self) {
        let layout = &self.layout.0;
        let device = &layout.device;
        let mut state = device.state();
        let pool = &mut state.pools[self.pool];
        if unsafe { device.raw.free_descriptor_sets(pool.raw, &[self.raw]) }.is_ok() {
            pool.live_sets -= 1;
            pool.available.iter_mut().zip(layout.budget).for_each(|(a, b)| *a += b);
        }
    }
}

/// One descriptor. The variant must match the binding's descriptor type.
#[derive(Clone, Copy)]
pub enum BindingValue<'a> {
    /// `UNIFORM_BUFFER` or `STORAGE_BUFFER`.
    Buffer { buffer: &'a Buffer, offset: u64, size: u64 },
    /// `SAMPLED_IMAGE` or `STORAGE_IMAGE`; layout `GENERAL` or `SHADER_READ_ONLY_OPTIMAL`.
    Texture { texture: &'a Texture, layout: vk::ImageLayout },
    /// `SAMPLER`.
    Sampler(&'a Sampler),
    /// `COMBINED_IMAGE_SAMPLER`.
    TextureSampler { texture: &'a Texture, sampler: &'a Sampler, layout: vk::ImageLayout },
}

/// Writes a contiguous range of a binding's descriptor array.
pub struct BindingWrite<'a> {
    pub set: &'a BindingSet,
    pub binding: u32,
    pub first_element: u32,
    pub values: &'a [BindingValue<'a>],
}

impl Device {
    pub fn create_binding_layout(self: &Arc<Self>, bindings: &[BindingDesc]) -> Result<BindingLayout> {
        fail_if(bindings.is_empty(), Error::InvalidArgument("no bindings"))?;
        let stages = vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::COMPUTE;
        let mut budget = Budget::default();
        let mut native = Vec::with_capacity(bindings.len());
        for (i, b) in bindings.iter().enumerate() {
            let ty = type_index(b.ty).ok_or(Error::Unsupported("descriptor type"))?;
            fail_if(b.count == 0, Error::InvalidArgument("binding count is zero"))?;
            fail_if(b.stages.is_empty() || !stages.contains(b.stages), Error::InvalidArgument("binding stages"))?;
            fail_if(bindings[..i].iter().any(|e| e.binding == b.binding), Error::InvalidArgument("duplicate binding"))?;
            budget[ty] = budget[ty].checked_add(b.count).ok_or(Error::OutOfRange("descriptor count"))?;
            native.push(
                vk::DescriptorSetLayoutBinding::default()
                    .binding(b.binding)
                    .descriptor_type(b.ty)
                    .descriptor_count(b.count)
                    .stage_flags(b.stages),
            );
        }
        // Fixed-count bindings, updated before use: matches the ordinary pools below.
        let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&native);
        let mut support = vk::DescriptorSetLayoutSupport::default();
        unsafe { self.raw.get_descriptor_set_layout_support(&info, &mut support) };
        fail_if(support.supported == 0, Error::Unsupported("descriptor set layout"))?;
        let raw = self.vk(unsafe { self.raw.create_descriptor_set_layout(&info, None) })?;
        self.state().stats.descriptor_layout_creations += 1;
        Ok(BindingLayout(Arc::new(LayoutInner { device: self.clone(), raw, bindings: bindings.to_vec(), budget })))
    }

    pub fn create_binding_set(&self, layout: &BindingLayout) -> Result<BindingSet> {
        let budget = layout.0.budget;
        let layouts = [layout.0.raw];
        let allocate = |pool: vk::DescriptorPool| unsafe {
            self.raw.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default().descriptor_pool(pool).set_layouts(&layouts),
            )
        };
        let mut state = self.state();
        let state = &mut *state;
        let fits = |p: &Pool| p.live_sets < SETS_PER_POOL && p.available.iter().zip(budget).all(|(a, b)| *a >= b);
        let take = |p: &mut Pool| {
            p.live_sets += 1;
            p.available.iter_mut().zip(budget).for_each(|(a, b)| *a -= b);
        };
        for (index, pool) in state.pools.iter_mut().enumerate().filter(|(_, p)| fits(p)) {
            match allocate(pool.raw) {
                Ok(sets) => {
                    take(pool);
                    return Ok(BindingSet { layout: layout.clone(), raw: sets[0], pool: index });
                }
                Err(vk::Result::ERROR_OUT_OF_POOL_MEMORY | vk::Result::ERROR_FRAGMENTED_POOL) => continue,
                Err(e) => return self.vk(Err(e)),
            }
        }

        let sizes: [vk::DescriptorPoolSize; TYPES.len()] = std::array::from_fn(|i| vk::DescriptorPoolSize {
            ty: TYPES[i],
            descriptor_count: budget[i].max(MIN_POOL_DESCRIPTORS),
        });
        let info = vk::DescriptorPoolCreateInfo::default()
            .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
            .max_sets(SETS_PER_POOL)
            .pool_sizes(&sizes);
        let raw = self.vk(unsafe { self.raw.create_descriptor_pool(&info, None) })?;
        let set = match allocate(raw) {
            Ok(sets) => sets[0],
            Err(e) => {
                unsafe { self.raw.destroy_descriptor_pool(raw, None) };
                return self.vk(Err(e));
            }
        };
        let mut pool = Pool { raw, live_sets: 0, available: sizes.map(|s| s.descriptor_count) };
        take(&mut pool);
        state.pools.push(pool);
        state.stats.descriptor_pool_creations += 1;
        Ok(BindingSet { layout: layout.clone(), raw: set, pool: state.pools.len() - 1 })
    }

    /// Validates the whole batch, then applies it with one native update.
    /// The sets must not be in use by pending GPU work.
    pub fn write_bindings(&self, writes: &[BindingWrite]) -> Result<()> {
        use vk::DescriptorType as T;
        fail_if(writes.is_empty(), Error::InvalidArgument("no binding writes"))?;
        let limits = &self.properties.limits;
        let mut buffers = Vec::new();
        let mut images = Vec::new();
        let mut kinds = Vec::with_capacity(writes.len());
        for w in writes {
            let desc = w.set.layout.0.bindings.iter().find(|b| b.binding == w.binding);
            let desc = desc.ok_or(Error::InvalidArgument("binding not in layout"))?;
            fail_if(w.values.is_empty(), Error::InvalidArgument("no binding values"))?;
            fail_if(
                w.first_element > desc.count || w.values.len() as u64 > u64::from(desc.count - w.first_element),
                Error::OutOfRange("descriptor array range"),
            )?;
            let is_buffer = matches!(desc.ty, T::UNIFORM_BUFFER | T::STORAGE_BUFFER);
            kinds.push((desc.ty, is_buffer, if is_buffer { buffers.len() } else { images.len() }));
            for value in w.values {
                match (*value, desc.ty) {
                    (BindingValue::Buffer { buffer, offset, size }, T::UNIFORM_BUFFER | T::STORAGE_BUFFER) => {
                        let uniform = desc.ty == T::UNIFORM_BUFFER;
                        let (usage, align, max) = if uniform {
                            (vk::BufferUsageFlags::UNIFORM_BUFFER, limits.min_uniform_buffer_offset_alignment, limits.max_uniform_buffer_range)
                        } else {
                            (vk::BufferUsageFlags::STORAGE_BUFFER, limits.min_storage_buffer_offset_alignment, limits.max_storage_buffer_range)
                        };
                        fail_if(!buffer.usage().contains(usage), Error::InvalidArgument("buffer usage"))?;
                        fail_if(size == 0 || size > u64::from(max), Error::OutOfRange("descriptor buffer size"))?;
                        fail_if(offset % align.max(1) != 0, Error::InvalidArgument("descriptor buffer offset alignment"))?;
                        fail_if(
                            offset > buffer.size() || size > buffer.size() - offset,
                            Error::OutOfRange("descriptor buffer range"),
                        )?;
                        buffers.push(vk::DescriptorBufferInfo { buffer: buffer.raw(), offset, range: size });
                    }
                    (BindingValue::Sampler(sampler), T::SAMPLER) => {
                        images.push(vk::DescriptorImageInfo { sampler: sampler.raw(), ..Default::default() })
                    }
                    (BindingValue::Texture { texture, layout }, T::SAMPLED_IMAGE | T::STORAGE_IMAGE) => {
                        check_image(texture, layout, desc.ty == T::STORAGE_IMAGE)?;
                        images.push(vk::DescriptorImageInfo { image_view: texture.view, image_layout: layout, ..Default::default() })
                    }
                    (BindingValue::TextureSampler { texture, sampler, layout }, T::COMBINED_IMAGE_SAMPLER) => {
                        check_image(texture, layout, false)?;
                        images.push(vk::DescriptorImageInfo {
                            sampler: sampler.raw(),
                            image_view: texture.view,
                            image_layout: layout,
                        })
                    }
                    _ => return Err(Error::InvalidArgument("binding value does not match descriptor type")),
                }
            }
        }
        let native: Vec<_> = writes
            .iter()
            .zip(kinds)
            .map(|(w, (ty, is_buffer, start))| {
                let write = vk::WriteDescriptorSet::default()
                    .dst_set(w.set.raw)
                    .dst_binding(w.binding)
                    .dst_array_element(w.first_element)
                    .descriptor_type(ty);
                let range = start..start + w.values.len();
                if is_buffer { write.buffer_info(&buffers[range]) } else { write.image_info(&images[range]) }
            })
            .collect();
        unsafe { self.raw.update_descriptor_sets(&native, &[]) };
        self.state().stats.descriptor_update_calls += 1;
        Ok(())
    }
}

fn check_image(texture: &Texture, layout: vk::ImageLayout, storage: bool) -> Result<()> {
    let usage = if storage { vk::ImageUsageFlags::STORAGE } else { vk::ImageUsageFlags::SAMPLED };
    fail_if(!texture.usage.contains(usage), Error::InvalidArgument("texture usage"))?;
    fail_if(
        texture.aspects == vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
        Error::InvalidArgument("combined depth/stencil textures cannot be bound"),
    )?;
    let read_only = !storage && layout == vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
    fail_if(layout != vk::ImageLayout::GENERAL && !read_only, Error::InvalidArgument("descriptor image layout"))
}
