//! Frame import: turn captured host pixels into a `wl_buffer` for a bridged
//! surface.
//!
//! Two paths:
//!
//! * **SHM (always available):** the captured frame bytes are copied into a
//!   double-buffered shared-memory `wl_shm_pool` owned by the bridge. This is
//!   the portable fallback and the only path when the nested compositor or
//!   transport has no GPU (`--no-gpu` waypipe, software Weston).
//! * **dmabuf (feature = "dmabuf"):** a host GPU buffer (IOSurface-backed
//!   dmabuf on macOS, `AHardwareBuffer` on Android) is imported zero-copy via
//!   `zwp_linux_dmabuf_v1`. The platform shim provides the dmabuf fd + plane
//!   layout; the core only wires the protocol objects.
//!
//! Only the SHM path materializes protocol traffic here; dmabuf import is
//! delegated to [`DmabufImporter`] which the bridge wires against the
//! `zwp_linux_dmabuf_v1` global when present.

use crate::surface::PixelFormat;

/// Description of a single captured frame handed over the FFI boundary.
#[derive(Clone, Copy, Debug)]
pub struct FrameDesc {
    pub width: u32,
    pub height: u32,
    /// Bytes per row of the source data (>= width * bpp).
    pub stride: u32,
    pub format: PixelFormat,
}

impl FrameDesc {
    /// Minimum number of source bytes required for this frame.
    pub fn required_len(&self) -> usize {
        (self.stride as usize) * (self.height as usize)
    }

    /// Tightly-packed destination stride the SHM buffer uses.
    pub fn dst_stride(&self) -> u32 {
        self.width * self.format.bytes_per_pixel()
    }
}

/// Copies a source frame (with arbitrary `stride`) into a tightly-packed
/// destination buffer, row by row. Shared by the SHM path and any CPU dmabuf
/// fallback. Returns the number of bytes written.
pub fn pack_rows(desc: &FrameDesc, src: &[u8], dst: &mut [u8]) -> usize {
    let bpp = desc.format.bytes_per_pixel() as usize;
    let row_bytes = (desc.width as usize) * bpp;
    let src_stride = desc.stride as usize;
    let dst_stride = desc.dst_stride() as usize;
    let mut written = 0usize;
    for row in 0..(desc.height as usize) {
        let s = row * src_stride;
        let d = row * dst_stride;
        if s + row_bytes > src.len() || d + row_bytes > dst.len() {
            break;
        }
        dst[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
        written += row_bytes;
    }
    written
}

/// Plane layout for a dmabuf import, provided by the platform shim.
#[cfg(feature = "dmabuf")]
#[derive(Clone, Debug)]
pub struct DmabufPlane {
    /// Borrowed fd for the plane (the shim owns its lifetime; the bridge dup()s
    /// as needed before handing it to the compositor).
    pub fd: std::os::unix::io::RawFd,
    pub offset: u32,
    pub stride: u32,
    pub modifier: u64,
}

/// Parameters for importing a host GPU buffer as a dmabuf-backed `wl_buffer`.
#[cfg(feature = "dmabuf")]
#[derive(Clone, Debug)]
pub struct DmabufImport {
    pub width: u32,
    pub height: u32,
    /// DRM fourcc format code.
    pub fourcc: u32,
    pub planes: Vec<DmabufPlane>,
}
