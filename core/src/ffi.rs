//! C ABI for the anowaW bridge.
//!
//! This is the surface the Wawona app links against in-process — from ObjC on
//! Apple platforms (`WWNAnowaWRunner`) and from JNI on Android (`android_jni.c`),
//! exactly the way `waypipe_main` is called today. All functions are
//! thread-affine: every call for a given `AnowawBridge*` must happen on the
//! same thread that created it (the shim dedicates one thread to the bridge).
//!
//! Return-code convention: `0` on success, small negative [`BridgeError::code`]
//! values on failure. Pointer-returning constructors return null on failure.

use std::ffi::{c_char, c_void, CStr};
use std::os::raw::c_int;

use crate::bridge::Bridge;
use crate::buffer::FrameDesc;
use crate::input::InputEvent;
use crate::surface::PixelFormat;
use crate::ANOWAW_ABI_VERSION;

/// Opaque handle to a running bridge, as seen by C.
pub type AnowawBridge = c_void;

fn cstr<'a>(p: *const c_char) -> &'a str {
    if p.is_null() {
        return "";
    }
    unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("")
}

/// Returns the ABI version the linked bridge implements. The Wawona side
/// asserts this equals its compiled-against constant before using the bridge.
#[no_mangle]
pub extern "C" fn anowaw_abi_version() -> u32 {
    ANOWAW_ABI_VERSION
}

/// Connect to the nested Weston compositor on `socket_name` (e.g. "wayland-1";
/// empty string = ambient `WAYLAND_DISPLAY`). Returns an opaque bridge pointer,
/// or null on failure.
///
/// # Safety
/// `socket_name` must be null or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn anowaw_start(socket_name: *const c_char) -> *mut AnowawBridge {
    let name = cstr(socket_name);
    match Bridge::connect(name) {
        Ok(bridge) => Box::into_raw(Box::new(bridge)) as *mut AnowawBridge,
        Err(e) => {
            log::error!("anowaw_start failed: {:?}", e);
            std::ptr::null_mut()
        }
    }
}

/// Register a host app and create its Wayland toplevel. Returns a nonzero app
/// handle, or 0 on failure.
///
/// # Safety
/// `bridge` must be a live pointer from [`anowaw_start`]; `app_id`/`title` must
/// be valid NUL-terminated C strings or null.
#[no_mangle]
pub unsafe extern "C" fn anowaw_bridge_app(
    bridge: *mut AnowawBridge,
    app_id: *const c_char,
    title: *const c_char,
    width: u32,
    height: u32,
) -> u64 {
    let Some(bridge) = (bridge as *mut Bridge).as_mut() else {
        return 0;
    };
    let _ = bridge.dispatch_pending();
    match bridge.bridge_app(cstr(app_id), cstr(title), width, height) {
        Ok(handle) => {
            let _ = bridge.dispatch_pending();
            handle
        }
        Err(e) => {
            log::error!("anowaw_bridge_app failed: {:?}", e);
            0
        }
    }
}

/// Upload a captured frame for `handle`. `data` points to `len` bytes laid out
/// as `stride * height`. `format` is a [`PixelFormat`] discriminant.
///
/// # Safety
/// `bridge` must be live; `data` must point to at least `len` valid bytes.
#[no_mangle]
pub unsafe extern "C" fn anowaw_push_frame(
    bridge: *mut AnowawBridge,
    handle: u64,
    data: *const u8,
    len: usize,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
) -> c_int {
    let Some(bridge) = (bridge as *mut Bridge).as_mut() else {
        return -100;
    };
    let Some(format) = PixelFormat::from_u32(format) else {
        return -101;
    };
    if data.is_null() {
        return -102;
    }
    let src = std::slice::from_raw_parts(data, len);
    let desc = FrameDesc { width, height, stride, format };
    let _ = bridge.dispatch_pending();
    match bridge.push_frame(handle, desc, src) {
        Ok(()) => 0,
        Err(e) => e.code(),
    }
}

/// Drain up to `cap` decoded input events into `out`. Returns the number
/// written (>= 0), or a negative error code.
///
/// # Safety
/// `bridge` must be live; `out` must point to `cap` writable [`InputEvent`]s.
#[no_mangle]
pub unsafe extern "C" fn anowaw_poll_input(
    bridge: *mut AnowawBridge,
    out: *mut InputEvent,
    cap: usize,
) -> c_int {
    let Some(bridge) = (bridge as *mut Bridge).as_mut() else {
        return -100;
    };
    if out.is_null() || cap == 0 {
        return 0;
    }
    // Pump the queue so fresh seat events are decoded before draining.
    if let Err(e) = bridge.dispatch_pending() {
        return e.code();
    }
    let slice = std::slice::from_raw_parts_mut(out, cap);
    bridge.poll_input(slice) as c_int
}

/// Returns 1 if the compositor requested this app's toplevel be closed, else 0.
///
/// # Safety
/// `bridge` must be live.
#[no_mangle]
pub unsafe extern "C" fn anowaw_close_requested(bridge: *mut AnowawBridge, handle: u64) -> c_int {
    let Some(bridge) = (bridge as *mut Bridge).as_mut() else {
        return 0;
    };
    let _ = bridge.dispatch_pending();
    bridge.close_requested(handle) as c_int
}

/// Tear down a bridged app's Wayland objects.
///
/// # Safety
/// `bridge` must be live.
#[no_mangle]
pub unsafe extern "C" fn anowaw_close_app(bridge: *mut AnowawBridge, handle: u64) {
    if let Some(bridge) = (bridge as *mut Bridge).as_mut() {
        bridge.close_app(handle);
        let _ = bridge.dispatch_pending();
    }
}

/// Pump the Wayland event queue once (non-blocking). Optional; the other
/// entry points already pump, but a dedicated shim thread may call this in a
/// loop for lowest input latency.
///
/// # Safety
/// `bridge` must be live.
#[no_mangle]
pub unsafe extern "C" fn anowaw_dispatch(bridge: *mut AnowawBridge) -> c_int {
    let Some(bridge) = (bridge as *mut Bridge).as_mut() else {
        return -100;
    };
    match bridge.dispatch_pending() {
        Ok(()) => 0,
        Err(e) => e.code(),
    }
}

/// Disconnect and free the bridge. The pointer is invalid after this call.
///
/// # Safety
/// `bridge` must be a live pointer from [`anowaw_start`] and not used again.
#[no_mangle]
pub unsafe extern "C" fn anowaw_stop(bridge: *mut AnowawBridge) {
    if !bridge.is_null() {
        drop(Box::from_raw(bridge as *mut Bridge));
    }
}
