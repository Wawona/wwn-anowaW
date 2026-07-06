//! anowaW bridge core.
//!
//! anowaW ("Wawona" reversed) renders a native **macOS (Cocoa/AppKit)** or
//! **Android** application as a first-class Wayland client inside Wawona's
//! nested-Weston desktop.
//!
//! # Where this runs
//!
//! The core is a **Wayland *client*** (not a compositor). It connects to the
//! socket exposed by the *nested Weston* compositor that Wawona launches with
//! `--backend=wayland` (e.g. `wayland-1`), **not** Wawona's root Smithay
//! socket (`wayland-0`). That way bridged apps appear as ordinary windows
//! inside the phosh/GNOME/KDE/niri session running under Wawona.
//!
//! # Data flow
//!
//! ```text
//!  host app (AppKit / Android Activity)
//!         │  per-app frames                 ▲  input events
//!         ▼  (IOSurface / VirtualDisplay)   │  (pointer / keyboard)
//!   platform shim  ──anowaw_push_frame──►  Bridge  ──anowaw_poll_input──►  platform shim
//!                                            │                               │
//!                                     wl_surface + xdg_toplevel        CGEvent / InputManager
//!                                            ▼
//!                                    nested Weston (wayland-1)
//! ```
//!
//! The Rust core is platform-agnostic. Capture (host frame → bytes/dmabuf) and
//! injection (Wayland seat event → host input) are implemented by the macOS and
//! Android shims, which drive the core purely through the [`ffi`] C ABI.

pub mod bridge;
pub mod buffer;
pub mod ffi;
pub mod input;
pub mod surface;

pub use bridge::{Bridge, BridgeError};
pub use input::{InputEvent, InputKind};
pub use surface::{BridgedApp, PixelFormat};

/// Semantic version of the bridge ABI. Bumped on any breaking C ABI change so
/// the Wawona side can assert compatibility at link/load time.
pub const ANOWAW_ABI_VERSION: u32 = 1;
