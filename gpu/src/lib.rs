//! Small Vulkan 1.3 rendering layer: one graphics queue, dynamic rendering and
//! synchronization2.
//!
//! Ownership rules:
//! * Every object keeps its [`Device`] alive (`Arc<Device>`), so destruction order
//!   is automatic. Dropping is destroying.
//! * Dropping a resource does not wait for the GPU. Wait for the commands that use
//!   it first ([`Command::wait`], [`UploadBatch::wait`], [`Device::wait_idle`]).
//!   Commands and swapchains wait for their own work when dropped.
//! * Uploads and recorded commands reference destinations by handle: keep those
//!   objects alive until the work completes or is cancelled.
//! * All recording happens on one thread at a time (types take `&mut self`).

mod command;
mod descriptors;
mod device;
mod memory;
mod pipeline;
mod swapchain;
mod upload;

pub use ash;
pub use ash::vk;

pub use command::{Attachment, Command, GpuTiming, RenderDesc, TimingSection};
pub use descriptors::{BindingDesc, BindingLayout, BindingSet, BindingValue, BindingWrite};
pub use device::{DebugCallback, Device, DeviceDesc, Stats, Window};
pub use memory::{Buffer, MemoryKind, Sampler, Texture, TextureDesc};
pub use pipeline::{ColorTarget, Pipeline, PipelineDesc};
pub use swapchain::{Frame, Swapchain};
pub use upload::{TextureUpload, UploadBatch};

pub const MAX_COLOR_ATTACHMENTS: usize = 8;
pub const MAX_SET_LAYOUTS: usize = 8;
pub const MAX_PUSH_RANGES: usize = 2;
/// Frames in flight per swapchain.
pub const FRAMES: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A parameter is invalid; the message names it.
    InvalidArgument(&'static str),
    /// The call is not allowed in the object's current state.
    InvalidState(&'static str),
    /// An offset, size or extent exceeds a resource or device limit.
    OutOfRange(&'static str),
    /// The device or this layer does not support the request.
    Unsupported(&'static str),
    NoDevice,
    LayerUnavailable,
    ExtensionUnavailable,
    NoSurface,
    /// The buffer is not host-visible.
    NotMapped,
    /// The swapchain must be resized/rebuilt with [`Swapchain::resize`].
    OutOfDate,
    /// The GPU has not finished yet.
    Busy,
    DeviceLost,
    OutOfHostMemory,
    OutOfDeviceMemory,
    /// Any other Vulkan error code.
    Vulkan(vk::Result),
}

impl From<vk::Result> for Error {
    fn from(result: vk::Result) -> Self {
        match result {
            vk::Result::ERROR_DEVICE_LOST => Error::DeviceLost,
            vk::Result::ERROR_OUT_OF_HOST_MEMORY => Error::OutOfHostMemory,
            vk::Result::ERROR_OUT_OF_DEVICE_MEMORY => Error::OutOfDeviceMemory,
            vk::Result::ERROR_OUT_OF_DATE_KHR => Error::OutOfDate,
            vk::Result::NOT_READY | vk::Result::TIMEOUT => Error::Busy,
            other => Error::Vulkan(other),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidArgument(what) => write!(f, "invalid argument: {what}"),
            Error::InvalidState(what) => write!(f, "invalid state: {what}"),
            Error::OutOfRange(what) => write!(f, "out of range: {what}"),
            Error::Unsupported(what) => write!(f, "unsupported: {what}"),
            Error::NoDevice => f.write_str("no suitable Vulkan 1.3 device"),
            Error::LayerUnavailable => f.write_str("validation layer unavailable"),
            Error::ExtensionUnavailable => f.write_str("instance extension unavailable"),
            Error::NoSurface => f.write_str("surface unavailable"),
            Error::NotMapped => f.write_str("buffer is not host-visible"),
            Error::OutOfDate => f.write_str("swapchain out of date"),
            Error::Busy => f.write_str("GPU work still pending"),
            Error::DeviceLost => f.write_str("device lost"),
            Error::OutOfHostMemory => f.write_str("host allocation failed"),
            Error::OutOfDeviceMemory => f.write_str("device allocation failed"),
            Error::Vulkan(result) => write!(f, "Vulkan error {result:?}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Returns `Err(error)` when `condition` holds. Keeps validation one line each.
#[inline]
pub(crate) fn fail_if(condition: bool, error: Error) -> Result<()> {
    if condition { Err(error) } else { Ok(()) }
}
