//! D3D12.X on host D3D12.
//!
//! D3D12.X records commands from code inlined into the title, which writes
//! packets into the command list while its put pointer is below a limit and
//! calls the list's method otherwise. Our lists keep the limits at zero, so
//! every command arrives here as a call, to be run on host D3D12. The
//! title's GPU addresses are CPU addresses (console memory is unified).
//!
//! [`device`] is the device object titles create, [`dxgi`] its adapter side,
//! [`resource`] heaps and resources, [`fence`] fences, [`queue`] command
//! queues, [`list`] command allocators and lists, [`descriptor`]
//! descriptor heaps, [`root`] root signatures, [`query`] query heaps,
//! [`signature`] command signatures, [`pipeline`] pipeline states,
//! [`frame`] frame pacing, [`object`] what all console graphics objects
//! share, [`console`] which console model the title sees. Planned:
//! `present` (swap chain).

#[cfg(all(windows, feature = "diagnostics"))]
mod capture;
pub mod console;
#[cfg(all(windows, feature = "diagnostics"))]
mod copystat;
pub mod descriptor;
pub mod device;
#[cfg(windows)]
mod draw;
pub mod dxgi;
pub mod fence;
pub mod frame;
#[cfg(windows)]
mod host;
#[cfg(windows)]
mod indirect;
pub mod list;
pub mod object;
pub mod pipeline;
#[cfg(windows)]
mod primitive;
pub mod query;
pub mod queue;
pub mod resource;
pub mod root;
#[cfg(windows)]
mod sampler;
#[cfg(windows)]
pub mod shader_cache;
#[cfg(windows)]
mod shadow;
pub mod signature;
#[cfg(windows)]
mod texture;
pub mod upscale;
#[cfg(windows)]
mod watch;

#[cfg(windows)]
pub use host::on_failure;
#[cfg(windows)]
pub use watch::{title_memory_remapped, writing as kernel_will_write};

/// Host HDR display luminance range in nits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HdrDisplay {
    pub min_luminance: f32,
    pub max_luminance: f32,
    pub max_full_frame_luminance: f32,
}

/// The last presented frame converted to 8-bit RGBA pixels.
#[derive(Debug, Clone)]
pub struct PresentedFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub hdr: bool,
}

/// Enables or disables HDR swap-chain presentation; returns the primary
/// display's HDR luminance range when `enable` is true and the host output
/// is in an HDR color space.
pub fn set_hdr_mode(enable: bool) -> Option<HdrDisplay> {
    #[cfg(windows)]
    {
        host::set_hdr_mode(enable)
    }
    #[cfg(not(windows))]
    {
        let _ = enable;
        None
    }
}

/// Reads back the most recently presented swap-chain frame on demand.
pub fn last_frame() -> Option<PresentedFrame> {
    #[cfg(windows)]
    {
        host::last_frame()
    }
    #[cfg(not(windows))]
    {
        None
    }
}
