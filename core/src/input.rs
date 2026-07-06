//! Input events decoded from the nested compositor's `wl_seat`, queued for the
//! platform shim to inject back into the source app.
//!
//! The bridge does not know how to inject input on a given OS — that is the
//! shim's job (`CGEvent`/Accessibility on macOS, `InputManager` on Android).
//! The core only decodes Wayland seat events into a neutral, C-ABI-friendly
//! form and hands them out via [`crate::ffi::anowaw_poll_input`].

/// Kind of input event. `repr(u32)` so it crosses the C ABI unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum InputKind {
    /// Pointer moved. `x`,`y` are surface-local logical coordinates.
    PointerMotion = 0,
    /// Pointer button. `code` is a Linux `BTN_*` code, `value` is 1 (press) / 0 (release).
    PointerButton = 1,
    /// Pointer scroll. `x`,`y` carry horizontal/vertical axis deltas (fixed→f32).
    PointerAxis = 2,
    /// Keyboard key. `code` is a Linux `KEY_*` evdev code, `value` is 1/0.
    Key = 3,
    /// Touch down/motion/up. `code` is the touch id, `value` is the phase
    /// (0=down,1=motion,2=up), `x`,`y` are surface-local coordinates.
    Touch = 4,
    /// Keyboard modifier state changed. `code` packs the depressed modifier mask.
    Modifiers = 5,
    /// Pointer entered/left the surface. `value` is 1 (enter) / 0 (leave).
    PointerFocus = 6,
}

/// A single decoded input event. Flat, `repr(C)`, safe to memcpy across FFI.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct InputEvent {
    /// Which bridged app this event targets.
    pub handle: u64,
    /// Event discriminant (see [`InputKind`]).
    pub kind: u32,
    /// Linux evdev key/button code, touch id, or modifier mask depending on `kind`.
    pub code: u32,
    /// Press/release/phase/enter-leave value depending on `kind`.
    pub value: i32,
    /// Surface-local X (logical px) or horizontal axis delta.
    pub x: f64,
    /// Surface-local Y (logical px) or vertical axis delta.
    pub y: f64,
    /// Event timestamp in milliseconds (compositor clock).
    pub time_ms: u32,
    /// Reserved for alignment / future use.
    pub _reserved: u32,
}

impl InputEvent {
    pub fn new(handle: u64, kind: InputKind) -> Self {
        Self {
            handle,
            kind: kind as u32,
            code: 0,
            value: 0,
            x: 0.0,
            y: 0.0,
            time_ms: 0,
            _reserved: 0,
        }
    }
}
