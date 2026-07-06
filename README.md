# wwn-anowaW

**anowaW** ("Wawona" reversed) is Wawona's *app bridge*: it renders a running
native **macOS (Cocoa/AppKit)** or **Android** application as a first-class
Wayland client *inside* Wawona's nested-Weston desktop. A phosh / GNOME / KDE /
niri session running under Wawona shows the host OS's apps as ordinary desktop
windows.

This repo contains **only the bridge** — the code that turns per-app host
surfaces into Wayland surfaces and routes input back to the source app. The
compositor, launcher, settings, and machine model live in the
[`Wawona`](https://github.com/Wawona/Wawona) integration repo.

## What it is (and is not)

anowaW is an **on-device capture-to-Wayland bridge**, not a scrcpy-style remote
display protocol:

- **macOS**: ScreenCaptureKit captures a single `SCWindow` into an `IOSurface`,
  which is imported as a `wl_buffer` (zero-copy dmabuf where possible, SHM
  fallback). Input decoded from the Wayland seat is injected back with
  `CGEvent` / Accessibility.
- **Android**: an app is launched onto an app-owned `VirtualDisplay`; its
  `Surface` (`AHardwareBuffer`) is imported as a `wl_buffer`. Input is injected
  through `InputManager`.

Conceptually this is the shape Waydroid uses (its HWC acts as a Wayland client
that maps Android surfaces to Wayland windows), but anowaW reuses the *host*
OS's own compositor (WindowServer / SurfaceFlinger) and bridges only per-app
surfaces, so there is no second Android userland to ship.

anowaW connects **as a client of the nested Weston** (`--backend=wayland`), not
Wawona's root Smithay compositor, so bridged apps appear inside the Linux
desktop rather than floating on the root surface.

## Layout

```
flake.nix                              registryFragment + lib.mkAnowaw
dependencies/libs/anowaw/
  anowaw-src.nix                       pins the in-repo Rust core crate
  macos.nix ios.nix android.nix ...    per-platform static-lib recipes
  Cargo.lock                           pinned lockfile (reproducible builds)
core/                                  Rust core (Wayland client, C FFI)
  Cargo.toml src/*.rs
platform/macos/                        ScreenCaptureKit capture + CGEvent inject
platform/android/                      VirtualDisplay capture + InputManager inject
.github/                              CI + patch-anchor verifier
```

## Use

```nix
inputs.wwn-anowaW.url = "github:Wawona/wwn-anowaW";

registry = wwn-toolchain.lib.baseRegistry // wwn-anowaW.registryFragment;

# In-process static lib for the Wawona app to link against:
anowaw = wwn-anowaW.lib.mkAnowaw { inherit pkgs; platform = "macos"; };
```

The static lib exports a C ABI (`anowaw_start`, `anowaw_bridge_app`,
`anowaw_push_frame`, `anowaw_poll_input`, `anowaw_stop`) called the same way
Wawona already calls `waypipe_main` from ObjC (`WWNWaypipeRunner.m`) and JNI
(`android_jni.c`).

## Standalone build

```sh
nix build .#anowaw-macos
nix build .#anowaw-ios
```

## Scope

- **v1: weston nested compositor only.** The bridge attaches to a nested Weston
  Wayland socket. Other nesting hosts (sway/niri/KDE) are out of scope for now.
- The desktop machine anowaW attaches to must be **local-only** and a **nested
  Wayland compositor** (never a plain Weston demo client, VM, container, or
  SSH/waypipe machine). Wawona enforces this filter.

## License

MIT for the Wawona Nix packaging, bridge core, and platform shims (see
`LICENSE`).
