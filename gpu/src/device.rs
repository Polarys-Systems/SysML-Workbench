use crate::command::{GpuTiming, TimingSection};
use crate::descriptors::Pool;
use crate::memory::Allocator;
use crate::{Error, Result, fail_if, vk};
use ash::{ext, khr};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle};
use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Anything that can host a Vulkan surface (a winit window, an SDL window, ...).
pub trait Window: HasWindowHandle + HasDisplayHandle {}
impl<T: HasWindowHandle + HasDisplayHandle + ?Sized> Window for T {}

/// Receives validation messages instead of the default stderr printer.
pub type DebugCallback = Box<
    dyn Fn(vk::DebugUtilsMessageSeverityFlagsEXT, vk::DebugUtilsMessageTypeFlagsEXT, &CStr)
        + Send
        + Sync,
>;

/// `DeviceDesc::default()` creates a headless device without validation.
#[derive(Default)]
pub struct DeviceDesc<'a> {
    /// Enables presentation; the window is only used during creation.
    pub window: Option<&'a dyn Window>,
    pub validation: bool,
    /// Requires `validation`.
    pub synchronization_validation: bool,
    pub debug_callback: Option<DebugCallback>,
    pub buffer_device_address: bool,
    pub scalar_block_layout: bool,
    pub sampler_anisotropy: bool,
    /// Optional; devices without timestamp support keep rendering without timings.
    pub gpu_timestamps: bool,
}

/// Counters for profiling. Command counters are added when a command is submitted.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub memory_allocations: u64,
    pub allocation_search_steps: u64,
    pub descriptor_pool_creations: u64,
    pub descriptor_layout_creations: u64,
    pub descriptor_update_calls: u64,
    pub upload_submissions: u64,
    pub uploaded_bytes: u64,
    pub upload_barrier_calls: u64,
    pub pipeline_binds: u64,
    pub dynamic_state_commands: u64,
    pub validation_errors: u64,
    pub draw_calls: u64,
    pub vertices_drawn: u64,
    pub indices_drawn: u64,
    pub reserved_bytes: u64,
    pub gpu_timings: [GpuTiming; TimingSection::COUNT],
}

#[derive(Clone, Copy)]
pub(crate) struct Features {
    pub buffer_device_address: bool,
    pub sampler_anisotropy: bool,
    pub gpu_timestamps: bool,
}

/// Mutable device-wide state behind one lock.
#[derive(Default)]
pub(crate) struct State {
    pub memory: Allocator,
    pub pools: Vec<Pool>,
    pub stats: Stats,
}

struct DebugState {
    errors: AtomicU64,
    callback: Option<DebugCallback>,
}

/// Destroys the debug messenger and instance; dropped after the logical device.
struct InstanceOwner {
    instance: ash::Instance,
    debug: Option<(ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
}

impl Drop for InstanceOwner {
    fn drop(&mut self) {
        unsafe {
            if let Some((loader, messenger)) = &self.debug {
                loader.destroy_debug_utils_messenger(*messenger, None);
            }
            self.instance.destroy_instance(None);
        }
    }
}

pub struct Device {
    pub(crate) raw: ash::Device,
    pub(crate) physical: vk::PhysicalDevice,
    pub(crate) queue: vk::Queue,
    pub(crate) queue_family: u32,
    pub(crate) properties: vk::PhysicalDeviceProperties,
    pub(crate) memory_properties: vk::PhysicalDeviceMemoryProperties,
    pub(crate) features: Features,
    pub(crate) timestamp_valid_bits: u32,
    pub(crate) pipeline_cache: vk::PipelineCache,
    pub(crate) surface_fn: Option<khr::surface::Instance>,
    pub(crate) swapchain_fn: Option<khr::swapchain::Device>,
    state: Mutex<State>,
    lost: AtomicBool,
    // Drop order matters: instance before the debug state it points to, loader last.
    inst: InstanceOwner,
    debug_state: Box<DebugState>,
    pub(crate) entry: ash::Entry,
}

impl Device {
    pub fn new(desc: DeviceDesc) -> Result<Arc<Device>> {
        fail_if(
            desc.synchronization_validation && !desc.validation,
            Error::InvalidArgument("synchronization_validation requires validation"),
        )?;
        let entry = unsafe { ash::Entry::load() }.map_err(|_| Error::NoDevice)?;
        let display = match desc.window {
            Some(window) => Some(window.display_handle().map_err(|_| Error::NoSurface)?.as_raw()),
            None => None,
        };
        let debug_state = Box::new(DebugState { errors: AtomicU64::new(0), callback: desc.debug_callback });
        let inst = create_instance(&entry, desc.validation, desc.synchronization_validation, display, &debug_state)?;
        let instance = &inst.instance;
        let surface_fn = desc.window.map(|_| khr::surface::Instance::new(&entry, instance));

        // A temporary surface ensures the chosen queue can present to this kind of window.
        let surface = match (desc.window, display) {
            (Some(window), Some(display)) => {
                let handle = window.window_handle().map_err(|_| Error::NoSurface)?.as_raw();
                unsafe { ash_window::create_surface(&entry, instance, display, handle, None) }
                    .map_err(|_| Error::NoSurface)?
            }
            _ => vk::SurfaceKHR::null(),
        };
        let wanted = Wanted {
            buffer_device_address: desc.buffer_device_address,
            scalar_block_layout: desc.scalar_block_layout,
            sampler_anisotropy: desc.sampler_anisotropy,
            presentation: desc.window.is_some(),
        };
        let picked = pick_physical_device(instance, surface_fn.as_ref(), surface, &wanted);
        if let Some(surface_fn) = &surface_fn {
            unsafe { surface_fn.destroy_surface(surface, None) };
        }
        let Picked { physical, family, properties, descriptor_indexing } = picked?;

        let families = unsafe { instance.get_physical_device_queue_family_properties(physical) };
        let priorities = [1.0];
        let queues = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&priorities)];
        let mut f13 = vk::PhysicalDeviceVulkan13Features::default().synchronization2(true).dynamic_rendering(true);
        let mut f12 = vk::PhysicalDeviceVulkan12Features::default()
            .buffer_device_address(wanted.buffer_device_address)
            .scalar_block_layout(wanted.scalar_block_layout)
            .descriptor_indexing(descriptor_indexing);
        let base = vk::PhysicalDeviceFeatures::default().sampler_anisotropy(wanted.sampler_anisotropy);
        let extensions = [khr::swapchain::NAME.as_ptr()];
        let mut info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .enabled_features(&base)
            .push_next(&mut f12)
            .push_next(&mut f13);
        if wanted.presentation {
            info = info.enabled_extension_names(&extensions);
        }
        let raw = unsafe { instance.create_device(physical, &info, None) }.map_err(Error::from)?;
        let pipeline_cache = match unsafe { raw.create_pipeline_cache(&vk::PipelineCacheCreateInfo::default(), None) } {
            Ok(cache) => cache,
            Err(e) => {
                unsafe { raw.destroy_device(None) };
                return Err(e.into());
            }
        };
        Ok(Arc::new(Device {
            queue: unsafe { raw.get_device_queue(family, 0) },
            swapchain_fn: wanted.presentation.then(|| khr::swapchain::Device::new(instance, &raw)),
            memory_properties: unsafe { instance.get_physical_device_memory_properties(physical) },
            timestamp_valid_bits: families[family as usize].timestamp_valid_bits,
            features: Features {
                buffer_device_address: wanted.buffer_device_address,
                sampler_anisotropy: wanted.sampler_anisotropy,
                gpu_timestamps: desc.gpu_timestamps,
            },
            raw,
            physical,
            queue_family: family,
            properties,
            pipeline_cache,
            surface_fn,
            state: Mutex::default(),
            lost: AtomicBool::new(false),
            inst,
            debug_state,
            entry,
        }))
    }

    pub fn wait_idle(&self) -> Result<()> {
        self.vk(unsafe { self.raw.device_wait_idle() })
    }

    /// Native handles for interop with code that records raw Vulkan.
    pub fn raw(&self) -> &ash::Device {
        &self.raw
    }
    pub fn instance(&self) -> &ash::Instance {
        &self.inst.instance
    }
    pub fn physical_device(&self) -> vk::PhysicalDevice {
        self.physical
    }
    pub fn properties(&self) -> &vk::PhysicalDeviceProperties {
        &self.properties
    }

    pub fn stats(&self) -> Stats {
        let mut stats = self.state().stats;
        stats.validation_errors = self.debug_state.errors.load(Ordering::Relaxed);
        stats
    }

    pub fn timestamps_supported(&self) -> bool {
        self.features.gpu_timestamps
            && (1..=64).contains(&self.timestamp_valid_bits)
            && self.properties.limits.timestamp_period > 0.0
    }

    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }

    /// Converts a Vulkan error, remembering device loss.
    pub(crate) fn error(&self, result: vk::Result) -> Error {
        if result == vk::Result::ERROR_DEVICE_LOST {
            self.lost.store(true, Ordering::Relaxed);
        }
        Error::from(result)
    }

    pub(crate) fn vk<T>(&self, result: ash::prelude::VkResult<T>) -> Result<T> {
        result.map_err(|e| self.error(e))
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            let _ = self.raw.device_wait_idle();
            self.raw.destroy_pipeline_cache(self.pipeline_cache, None);
            let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
            for pool in &state.pools {
                self.raw.destroy_descriptor_pool(pool.raw, None);
            }
            state.memory.release_all(&self.raw);
            self.raw.destroy_device(None);
        }
    }
}

unsafe extern "system" fn debug_message(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    user: *mut c_void,
) -> vk::Bool32 {
    let state = unsafe { &*(user as *const DebugState) };
    if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        state.errors.fetch_add(1, Ordering::Relaxed);
    }
    let message = unsafe { data.as_ref() }.and_then(|d| unsafe { d.message_as_c_str() }).unwrap_or(c"");
    match &state.callback {
        Some(callback) => callback(severity, types, message),
        None => eprintln!("[Vulkan] {}", message.to_string_lossy()),
    }
    vk::FALSE
}

fn create_instance(
    entry: &ash::Entry,
    validation: bool,
    sync_validation: bool,
    display: Option<RawDisplayHandle>,
    debug_state: &DebugState,
) -> Result<InstanceOwner> {
    let version = unsafe { entry.try_enumerate_instance_version() }?.unwrap_or(vk::API_VERSION_1_0);
    fail_if(version < vk::API_VERSION_1_3, Error::Unsupported("Vulkan 1.3 loader required"))?;
    const LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";
    if validation {
        let layers = unsafe { entry.enumerate_instance_layer_properties() }.unwrap_or_default();
        fail_if(!layers.iter().any(|l| l.layer_name_as_c_str() == Ok(LAYER)), Error::LayerUnavailable)?;
    }

    let mut extensions: Vec<*const c_char> = match display {
        Some(display) => ash_window::enumerate_required_extensions(display)
            .map_err(|_| Error::ExtensionUnavailable)?
            .to_vec(),
        None => Vec::new(),
    };
    if validation {
        extensions.push(ext::debug_utils::NAME.as_ptr());
    }
    if sync_validation {
        extensions.push(ext::validation_features::NAME.as_ptr());
    }
    let mut available = unsafe { entry.enumerate_instance_extension_properties(None) }.unwrap_or_default();
    if validation {
        available.extend(unsafe { entry.enumerate_instance_extension_properties(Some(LAYER)) }.unwrap_or_default());
    }
    for &name in &extensions {
        let name = unsafe { CStr::from_ptr(name) };
        fail_if(!available.iter().any(|e| e.extension_name_as_c_str() == Ok(name)), Error::ExtensionUnavailable)?;
    }

    let mut debug_info = vk::DebugUtilsMessengerCreateInfoEXT::default()
        .message_severity(
            vk::DebugUtilsMessageSeverityFlagsEXT::WARNING | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
        )
        .message_type(
            vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
        )
        .pfn_user_callback(Some(debug_message))
        .user_data(debug_state as *const DebugState as *mut c_void);
    let sync = [vk::ValidationFeatureEnableEXT::SYNCHRONIZATION_VALIDATION];
    let mut sync_features = vk::ValidationFeaturesEXT::default().enabled_validation_features(&sync);
    let app = vk::ApplicationInfo::default().application_name(c"base graphics").api_version(vk::API_VERSION_1_3);
    let layers = [LAYER.as_ptr()];
    let mut info = vk::InstanceCreateInfo::default().application_info(&app).enabled_extension_names(&extensions);
    if validation {
        info = info.enabled_layer_names(&layers).push_next(&mut debug_info);
    }
    if sync_validation {
        info = info.push_next(&mut sync_features);
    }
    let instance = unsafe { entry.create_instance(&info, None) }?;
    let mut owner = InstanceOwner { instance, debug: None };
    if validation {
        let loader = ext::debug_utils::Instance::new(entry, &owner.instance);
        let messenger = unsafe { loader.create_debug_utils_messenger(&debug_info, None) }?;
        owner.debug = Some((loader, messenger));
    }
    Ok(owner)
}

struct Wanted {
    buffer_device_address: bool,
    scalar_block_layout: bool,
    sampler_anisotropy: bool,
    presentation: bool,
}

struct Picked {
    physical: vk::PhysicalDevice,
    family: u32,
    properties: vk::PhysicalDeviceProperties,
    descriptor_indexing: bool,
}

/// Picks the first discrete GPU that fits, else the first suitable one.
fn pick_physical_device(
    instance: &ash::Instance,
    surface_fn: Option<&khr::surface::Instance>,
    surface: vk::SurfaceKHR,
    wanted: &Wanted,
) -> Result<Picked> {
    let mut best: Option<(u32, Picked)> = None;
    for physical in unsafe { instance.enumerate_physical_devices() }? {
        let Some(picked) = suitable(instance, surface_fn, surface, physical, wanted) else { continue };
        let score = if picked.properties.device_type == vk::PhysicalDeviceType::DISCRETE_GPU { 2 } else { 1 };
        if best.as_ref().is_none_or(|(best_score, _)| score > *best_score) {
            best = Some((score, picked));
        }
    }
    best.map(|(_, picked)| picked).ok_or(Error::NoDevice)
}

fn suitable(
    instance: &ash::Instance,
    surface_fn: Option<&khr::surface::Instance>,
    surface: vk::SurfaceKHR,
    physical: vk::PhysicalDevice,
    wanted: &Wanted,
) -> Option<Picked> {
    let properties = unsafe { instance.get_physical_device_properties(physical) };
    if properties.api_version < vk::API_VERSION_1_3 {
        return None;
    }
    let mut f13 = vk::PhysicalDeviceVulkan13Features::default();
    let mut f12 = vk::PhysicalDeviceVulkan12Features::default();
    let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut f12).push_next(&mut f13);
    unsafe { instance.get_physical_device_features2(physical, &mut features) };
    let base = features.features;
    if f13.dynamic_rendering == 0
        || f13.synchronization2 == 0
        || (wanted.buffer_device_address && f12.buffer_device_address == 0)
        || (wanted.scalar_block_layout && f12.scalar_block_layout == 0)
        || (wanted.sampler_anisotropy && base.sampler_anisotropy == 0)
    {
        return None;
    }
    if wanted.presentation {
        let extensions = unsafe { instance.enumerate_device_extension_properties(physical) }.unwrap_or_default();
        if !extensions.iter().any(|e| e.extension_name_as_c_str() == Ok(khr::swapchain::NAME)) {
            return None;
        }
    }
    let families = unsafe { instance.get_physical_device_queue_family_properties(physical) };
    let family = families.iter().enumerate().position(|(i, family)| {
        let presents = match surface_fn {
            Some(surface_fn) if surface != vk::SurfaceKHR::null() => unsafe {
                surface_fn.get_physical_device_surface_support(physical, i as u32, surface)
            }
            .unwrap_or(false),
            _ => true,
        };
        family.queue_count != 0 && family.queue_flags.contains(vk::QueueFlags::GRAPHICS) && presents
    })?;
    Some(Picked { physical, family: family as u32, properties, descriptor_indexing: f12.descriptor_indexing != 0 })
}
