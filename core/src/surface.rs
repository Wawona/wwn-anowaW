//! Per-bridged-app Wayland surface state.

use std::sync::atomic::{AtomicU64, Ordering};

/// Pixel format of a captured host frame. Matches the small set the bridge can
/// map to a Wayland `wl_shm` / dmabuf format without conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum PixelFormat {
    /// 32-bit BGRA, little-endian (macOS IOSurface native, Android RGBA swizzle).
    Bgra8888 = 0,
    /// 32-bit BGRX (opaque).
    Bgrx8888 = 1,
    /// 32-bit RGBA.
    Rgba8888 = 2,
    /// 32-bit RGBX (opaque).
    Rgbx8888 = 3,
}

impl PixelFormat {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Bgra8888),
            1 => Some(Self::Bgrx8888),
            2 => Some(Self::Rgba8888),
            3 => Some(Self::Rgbx8888),
            _ => None,
        }
    }

    /// Corresponding `wl_shm` format code (from the Wayland `wl_shm::format` enum).
    pub fn wl_shm_format(self) -> u32 {
        // 0 = ARGB8888, 1 = XRGB8888 are guaranteed; the rest are advertised by
        // most compositors. Weston (nested) supports the DRM fourccs below.
        match self {
            PixelFormat::Bgra8888 => 0,          // WL_SHM_FORMAT_ARGB8888
            PixelFormat::Bgrx8888 => 1,          // WL_SHM_FORMAT_XRGB8888
            PixelFormat::Rgba8888 => 0x34324152, // DRM_FORMAT_ABGR8888 fourcc
            PixelFormat::Rgbx8888 => 0x34324258, // DRM_FORMAT_XBGR8888 fourcc
        }
    }

    pub fn bytes_per_pixel(self) -> u32 {
        4
    }
}

/// Opaque, process-unique handle the platform shim uses to address a bridged
/// app. Never reused within a process lifetime.
pub type AppHandle = u64;

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_handle() -> AppHandle {
    NEXT_HANDLE.fetch_add(1, Ordering::Relaxed)
}

/// A single host application bridged as a Wayland toplevel.
///
/// Holds the desired geometry and identity; the live `wl_surface` /
/// `xdg_toplevel` objects live inside [`crate::bridge::Bridge`] so all Wayland
/// protocol access stays on the bridge's event-queue thread.
#[derive(Clone, Debug)]
pub struct BridgedApp {
    pub handle: AppHandle,
    /// Stable identity of the host app (macOS bundle id / Android package name).
    pub app_id: String,
    /// Human-readable window title.
    pub title: String,
    pub width: u32,
    pub height: u32,
    /// True once the toplevel has been configured by the nested compositor and
    /// is ready to receive frames.
    pub configured: bool,
    /// True after the compositor (or user) asked the toplevel to close; the
    /// platform shim should tear down the host app and call
    /// [`crate::ffi::anowaw_close_app`].
    pub close_requested: bool,
}

impl BridgedApp {
    pub(crate) fn new(app_id: String, title: String, width: u32, height: u32) -> Self {
        Self {
            handle: next_handle(),
            app_id,
            title,
            width: width.max(1),
            height: height.max(1),
            configured: false,
            close_requested: false,
        }
    }
}
