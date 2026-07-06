//! The bridge: a Wayland client of the nested Weston compositor that presents
//! each host app as an `xdg_toplevel`, uploads captured frames, and decodes
//! seat input for the platform shim to re-inject.
//!
//! Implemented directly on `wayland-client` (no smithay-client-toolkit: SCTK
//! does not compile for Apple targets because rustix gates its pipe APIs out
//! on macOS/iOS). Everything here is plain protocol dispatch, portable across
//! macOS, iOS, and Android.

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::AsFd;

use memmap2::MmapMut;
use wayland_client::{
    delegate_noop,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm,
        wl_shm_pool, wl_surface, wl_touch,
    },
    backend::ObjectId,
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

use crate::buffer::{pack_rows, FrameDesc};
use crate::input::{InputEvent, InputKind};
use crate::surface::{AppHandle, BridgedApp, PixelFormat};

/// Errors surfaced across the FFI boundary as small negative codes.
#[derive(Debug)]
pub enum BridgeError {
    /// Could not connect to the nested compositor socket.
    Connect(String),
    /// A required Wayland global (compositor/shm/xdg_wm_base) was missing.
    MissingGlobal(&'static str),
    /// The referenced app handle is unknown.
    UnknownApp(AppHandle),
    /// A frame's byte length did not match its descriptor.
    ShortFrame,
    /// SHM pool allocation/resize failed.
    Shm(String),
    /// Event dispatch failed (connection lost).
    Dispatch(String),
}

impl BridgeError {
    pub fn code(&self) -> i32 {
        match self {
            BridgeError::Connect(_) => -1,
            BridgeError::MissingGlobal(_) => -2,
            BridgeError::UnknownApp(_) => -3,
            BridgeError::ShortFrame => -4,
            BridgeError::Shm(_) => -5,
            BridgeError::Dispatch(_) => -6,
        }
    }
}

fn map_format(format: PixelFormat) -> wl_shm::Format {
    match format {
        PixelFormat::Bgra8888 => wl_shm::Format::Argb8888,
        PixelFormat::Bgrx8888 => wl_shm::Format::Xrgb8888,
        PixelFormat::Rgba8888 => wl_shm::Format::Abgr8888,
        PixelFormat::Rgbx8888 => wl_shm::Format::Xbgr8888,
    }
}

/// Creates an anonymous, unlinked file for SHM pools. Portable across macOS /
/// iOS / Android (no memfd_create on Apple platforms).
fn anon_shm_file(len: u64) -> Result<File, BridgeError> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    for attempt in 0..64 {
        let path = dir.join(format!(
            "anowaw-shm-{}-{}",
            std::process::id(),
            attempt + (len as usize % 977)
        ));
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                // Unlink immediately: the fd keeps the storage alive.
                let _ = std::fs::remove_file(&path);
                file.set_len(len).map_err(|e| BridgeError::Shm(e.to_string()))?;
                return Ok(file);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(BridgeError::Shm(e.to_string())),
        }
    }
    Err(BridgeError::Shm("could not create anonymous shm file".into()))
}

/// Number of swap slots per app pool (double buffering).
const SLOTS: usize = 2;

/// User data attached to each `wl_buffer` so Release events find their slot.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BufferData {
    handle: AppHandle,
    slot: usize,
}

/// A double-buffered SHM pool sized for one app's frames.
struct ShmSlots {
    pool: wl_shm_pool::WlShmPool,
    _file: File,
    map: MmapMut,
    width: u32,
    height: u32,
    format: PixelFormat,
    slot_size: usize,
    buffers: [Option<wl_buffer::WlBuffer>; SLOTS],
    busy: [bool; SLOTS],
}

impl ShmSlots {
    fn destroy(&mut self) {
        for b in self.buffers.iter_mut().flatten() {
            b.destroy();
        }
        self.buffers = [None, None];
        self.pool.destroy();
    }
}

/// Live protocol objects + metadata for one bridged app.
struct AppEntry {
    app: BridgedApp,
    surface: wl_surface::WlSurface,
    xdg_surface: xdg_surface::XdgSurface,
    toplevel: xdg_toplevel::XdgToplevel,
    slots: Option<ShmSlots>,
}

impl AppEntry {
    fn destroy(&mut self) {
        if let Some(slots) = self.slots.as_mut() {
            slots.destroy();
        }
        self.slots = None;
        self.toplevel.destroy();
        self.xdg_surface.destroy();
        self.surface.destroy();
    }
}

/// Wayland dispatch state, owned by the [`Bridge`].
pub(crate) struct State {
    compositor: wl_compositor::WlCompositor,
    shm: wl_shm::WlShm,
    wm_base: xdg_wm_base::XdgWmBase,

    apps: HashMap<AppHandle, AppEntry>,
    /// wl_surface object id → app handle, for routing seat/xdg events.
    surface_to_handle: HashMap<ObjectId, AppHandle>,
    /// xdg_surface object id → app handle (configure routing).
    xdg_to_handle: HashMap<ObjectId, AppHandle>,
    /// xdg_toplevel object id → app handle (configure/close routing).
    toplevel_to_handle: HashMap<ObjectId, AppHandle>,

    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    touch: Option<wl_touch::WlTouch>,
    keyboard_focus: Option<AppHandle>,
    pointer_focus: Option<AppHandle>,
    touch_focus: Option<AppHandle>,

    /// Decoded input events awaiting `anowaw_poll_input`.
    input_queue: Vec<InputEvent>,
}

impl State {
    fn push_event(&mut self, handle: AppHandle, kind: InputKind, f: impl FnOnce(&mut InputEvent)) {
        let mut ev = InputEvent::new(handle, kind);
        f(&mut ev);
        self.input_queue.push(ev);
    }
}

/// A running bridge instance. One per nested-Weston desktop session.
pub struct Bridge {
    conn: Connection,
    event_queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
}

impl Bridge {
    /// Connect to the nested Weston compositor listening on `socket_name`
    /// (e.g. `"wayland-1"`; empty = ambient `WAYLAND_DISPLAY`). The socket is
    /// resolved against `$XDG_RUNTIME_DIR` like every other Wawona client.
    pub fn connect(socket_name: &str) -> Result<Self, BridgeError> {
        if !socket_name.is_empty() {
            // Target the nested compositor explicitly rather than inheriting
            // WAYLAND_DISPLAY (which points at Wawona's root Smithay socket).
            std::env::set_var("WAYLAND_DISPLAY", socket_name);
        }
        let conn =
            Connection::connect_to_env().map_err(|e| BridgeError::Connect(e.to_string()))?;

        let (globals, event_queue) = registry_queue_init::<State>(&conn)
            .map_err(|e| BridgeError::Connect(e.to_string()))?;
        let qh = event_queue.handle();

        let compositor: wl_compositor::WlCompositor = globals
            .bind(&qh, 4..=6, ())
            .map_err(|_| BridgeError::MissingGlobal("wl_compositor"))?;
        let shm: wl_shm::WlShm = globals
            .bind(&qh, 1..=1, ())
            .map_err(|_| BridgeError::MissingGlobal("wl_shm"))?;
        let wm_base: xdg_wm_base::XdgWmBase = globals
            .bind(&qh, 1..=6, ())
            .map_err(|_| BridgeError::MissingGlobal("xdg_wm_base"))?;
        // Seats arrive through the registry; bind every advertised one.
        for global in globals.contents().clone_list() {
            if global.interface == "wl_seat" {
                let _seat: wl_seat::WlSeat = globals.registry().bind(
                    global.name,
                    global.version.min(7),
                    &qh,
                    (),
                );
            }
        }

        let state = State {
            compositor,
            shm,
            wm_base,
            apps: HashMap::new(),
            surface_to_handle: HashMap::new(),
            xdg_to_handle: HashMap::new(),
            toplevel_to_handle: HashMap::new(),
            keyboard: None,
            pointer: None,
            touch: None,
            keyboard_focus: None,
            pointer_focus: None,
            touch_focus: None,
            input_queue: Vec::new(),
        };

        let mut bridge = Self { conn, event_queue, qh, state };
        // Complete initial roundtrip so seat capabilities are known.
        bridge
            .event_queue
            .roundtrip(&mut bridge.state)
            .map_err(|e| BridgeError::Connect(e.to_string()))?;
        Ok(bridge)
    }

    /// Register a new host app and create its Wayland toplevel. Returns the
    /// handle the shim uses for frame push / input poll / close.
    pub fn bridge_app(
        &mut self,
        app_id: &str,
        title: &str,
        width: u32,
        height: u32,
    ) -> Result<AppHandle, BridgeError> {
        let app = BridgedApp::new(app_id.to_string(), title.to_string(), width, height);
        let handle = app.handle;

        let surface = self.state.compositor.create_surface(&self.qh, ());
        let xdg_surface = self
            .state
            .wm_base
            .get_xdg_surface(&surface, &self.qh, ());
        let toplevel = xdg_surface.get_toplevel(&self.qh, ());
        toplevel.set_title(title.to_string());
        toplevel.set_app_id(format!("anowaw.{app_id}"));
        surface.commit();

        self.state.surface_to_handle.insert(surface.id(), handle);
        self.state.xdg_to_handle.insert(xdg_surface.id(), handle);
        self.state.toplevel_to_handle.insert(toplevel.id(), handle);
        self.state.apps.insert(
            handle,
            AppEntry { app, surface, xdg_surface, toplevel, slots: None },
        );

        // Pump so the initial configure lands promptly.
        self.dispatch_pending()?;
        Ok(handle)
    }

    fn ensure_slots(
        state: &mut State,
        qh: &QueueHandle<State>,
        handle: AppHandle,
        desc: &FrameDesc,
    ) -> Result<(), BridgeError> {
        let entry = state
            .apps
            .get_mut(&handle)
            .ok_or(BridgeError::UnknownApp(handle))?;

        let needs_new = match entry.slots.as_ref() {
            Some(s) => s.width != desc.width || s.height != desc.height || s.format != desc.format,
            None => true,
        };
        if !needs_new {
            return Ok(());
        }
        if let Some(mut old) = entry.slots.take() {
            old.destroy();
        }

        let slot_size = (desc.dst_stride() as usize) * (desc.height as usize);
        let total = slot_size * SLOTS;
        let file = anon_shm_file(total as u64)?;
        let map = unsafe { MmapMut::map_mut(&file) }.map_err(|e| BridgeError::Shm(e.to_string()))?;
        let pool = state
            .shm
            .create_pool(file.as_fd(), total as i32, qh, ());

        let mut buffers: [Option<wl_buffer::WlBuffer>; SLOTS] = [None, None];
        for (slot, buf) in buffers.iter_mut().enumerate() {
            *buf = Some(pool.create_buffer(
                (slot * slot_size) as i32,
                desc.width as i32,
                desc.height as i32,
                desc.dst_stride() as i32,
                map_format(desc.format),
                qh,
                BufferData { handle, slot },
            ));
        }

        let entry = state.apps.get_mut(&handle).unwrap();
        entry.slots = Some(ShmSlots {
            pool,
            _file: file,
            map,
            width: desc.width,
            height: desc.height,
            format: desc.format,
            slot_size,
            buffers,
            busy: [false; SLOTS],
        });
        Ok(())
    }

    /// Upload a captured frame (copied into an SHM `wl_buffer`) and present it
    /// on the app's surface.
    pub fn push_frame(
        &mut self,
        handle: AppHandle,
        desc: FrameDesc,
        src: &[u8],
    ) -> Result<(), BridgeError> {
        if src.len() < desc.required_len() {
            return Err(BridgeError::ShortFrame);
        }
        {
            let entry = self
                .state
                .apps
                .get(&handle)
                .ok_or(BridgeError::UnknownApp(handle))?;
            // Not yet configured: drop the frame but keep the connection alive.
            if !entry.app.configured {
                return Ok(());
            }
        }

        Self::ensure_slots(&mut self.state, &self.qh, handle, &desc)?;

        let entry = self.state.apps.get_mut(&handle).unwrap();
        let buffer = {
            let slots = entry.slots.as_mut().unwrap();

            // Pick a free slot; if all are held by the compositor, skip the frame.
            let Some(slot) = (0..SLOTS).find(|&s| !slots.busy[s]) else {
                return Ok(());
            };

            let off = slot * slots.slot_size;
            let dst = &mut slots.map[off..off + slots.slot_size];
            pack_rows(&desc, src, dst);

            slots.busy[slot] = true;
            slots.buffers[slot].as_ref().unwrap().clone()
        };
        entry.surface.attach(Some(&buffer), 0, 0);
        entry
            .surface
            .damage_buffer(0, 0, desc.width as i32, desc.height as i32);
        entry.surface.commit();

        self.dispatch_pending()
    }

    /// Drain decoded input events into `out`, returning how many were written.
    pub fn poll_input(&mut self, out: &mut [InputEvent]) -> usize {
        let n = self.state.input_queue.len().min(out.len());
        for (i, ev) in self.state.input_queue.drain(..n).enumerate() {
            out[i] = ev;
        }
        n
    }

    /// True if the compositor asked this app's toplevel to close.
    pub fn close_requested(&self, handle: AppHandle) -> bool {
        self.state
            .apps
            .get(&handle)
            .map(|e| e.app.close_requested)
            .unwrap_or(false)
    }

    /// Destroy a bridged app's Wayland objects.
    pub fn close_app(&mut self, handle: AppHandle) {
        if let Some(mut entry) = self.state.apps.remove(&handle) {
            self.state.surface_to_handle.remove(&entry.surface.id());
            self.state.xdg_to_handle.remove(&entry.xdg_surface.id());
            self.state.toplevel_to_handle.remove(&entry.toplevel.id());
            entry.destroy();
            let _ = self.dispatch_pending();
        }
    }

    /// Pump the Wayland event queue without blocking. Called by the FFI layer
    /// around every interaction so configure/seat events are processed.
    pub fn dispatch_pending(&mut self) -> Result<(), BridgeError> {
        self.event_queue
            .flush()
            .map_err(|e| BridgeError::Dispatch(e.to_string()))?;
        // Read any pending wire data without blocking, then dispatch it.
        if let Some(guard) = self.conn.prepare_read() {
            let _ = guard.read();
        }
        self.event_queue
            .dispatch_pending(&mut self.state)
            .map_err(|e| BridgeError::Dispatch(e.to_string()))?;
        Ok(())
    }

    /// Block until at least one event is processed (shim pump-loop use).
    pub fn dispatch_blocking(&mut self) -> Result<(), BridgeError> {
        self.event_queue
            .blocking_dispatch(&mut self.state)
            .map_err(|e| BridgeError::Dispatch(e.to_string()))?;
        Ok(())
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

// ── Registry (initial global list + later events) ───────────────────────────

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        // Bind seats that appear after startup (hotplug).
        if let wl_registry::Event::Global { name, interface, version } = event {
            if interface == "wl_seat" {
                let _seat: wl_seat::WlSeat = registry.bind(name, version.min(7), qh, ());
            }
        }
    }
}

// ── xdg_wm_base ping/pong ────────────────────────────────────────────────────

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        _state: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

// ── xdg_surface configure ────────────────────────────────────────────────────

impl Dispatch<xdg_surface::XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        xdg_surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            if let Some(&handle) = state.xdg_to_handle.get(&xdg_surface.id()) {
                if let Some(entry) = state.apps.get_mut(&handle) {
                    entry.app.configured = true;
                }
            }
        }
    }
}

// ── xdg_toplevel configure/close ─────────────────────────────────────────────

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
    fn event(
        state: &mut Self,
        toplevel: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let Some(&handle) = state.toplevel_to_handle.get(&toplevel.id()) else {
            return;
        };
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                if let Some(entry) = state.apps.get_mut(&handle) {
                    if width > 0 && height > 0 {
                        entry.app.width = width as u32;
                        entry.app.height = height as u32;
                    }
                }
            }
            xdg_toplevel::Event::Close => {
                if let Some(entry) = state.apps.get_mut(&handle) {
                    entry.app.close_requested = true;
                }
            }
            _ => {}
        }
    }
}

// ── Buffer release (frees a swap slot) ──────────────────────────────────────

impl Dispatch<wl_buffer::WlBuffer, BufferData> for State {
    fn event(
        state: &mut Self,
        _buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        data: &BufferData,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            if let Some(entry) = state.apps.get_mut(&data.handle) {
                if let Some(slots) = entry.slots.as_mut() {
                    if data.slot < SLOTS {
                        slots.busy[data.slot] = false;
                    }
                }
            }
        }
    }
}

// ── Seat capabilities ────────────────────────────────────────────────────────

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Touch) && state.touch.is_none() {
                state.touch = Some(seat.get_touch(qh, ()));
            }
        }
    }
}

// ── Keyboard ─────────────────────────────────────────────────────────────────

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _kbd: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { surface, .. } => {
                state.keyboard_focus = state.surface_to_handle.get(&surface.id()).copied();
            }
            wl_keyboard::Event::Leave { .. } => {
                state.keyboard_focus = None;
            }
            wl_keyboard::Event::Key { time, key, state: key_state, .. } => {
                if let Some(handle) = state.keyboard_focus {
                    let pressed =
                        matches!(key_state, WEnum::Value(wl_keyboard::KeyState::Pressed));
                    state.push_event(handle, InputKind::Key, |ev| {
                        ev.code = key; // evdev KEY_* code
                        ev.value = pressed as i32;
                        ev.time_ms = time;
                    });
                }
            }
            wl_keyboard::Event::Modifiers { mods_depressed, .. } => {
                if let Some(handle) = state.keyboard_focus {
                    state.push_event(handle, InputKind::Modifiers, |ev| {
                        ev.code = mods_depressed;
                    });
                }
            }
            _ => {}
        }
    }
}

// ── Pointer ──────────────────────────────────────────────────────────────────

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _ptr: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { surface, surface_x, surface_y, .. } => {
                state.pointer_focus = state.surface_to_handle.get(&surface.id()).copied();
                if let Some(handle) = state.pointer_focus {
                    state.push_event(handle, InputKind::PointerFocus, |ev| {
                        ev.value = 1;
                        ev.x = surface_x;
                        ev.y = surface_y;
                    });
                }
            }
            wl_pointer::Event::Leave { .. } => {
                if let Some(handle) = state.pointer_focus.take() {
                    state.push_event(handle, InputKind::PointerFocus, |ev| ev.value = 0);
                }
            }
            wl_pointer::Event::Motion { time, surface_x, surface_y } => {
                if let Some(handle) = state.pointer_focus {
                    state.push_event(handle, InputKind::PointerMotion, |ev| {
                        ev.x = surface_x;
                        ev.y = surface_y;
                        ev.time_ms = time;
                    });
                }
            }
            wl_pointer::Event::Button { time, button, state: btn_state, .. } => {
                if let Some(handle) = state.pointer_focus {
                    let pressed =
                        matches!(btn_state, WEnum::Value(wl_pointer::ButtonState::Pressed));
                    state.push_event(handle, InputKind::PointerButton, |ev| {
                        ev.code = button; // Linux BTN_* code
                        ev.value = pressed as i32;
                        ev.time_ms = time;
                    });
                }
            }
            wl_pointer::Event::Axis { time, axis, value } => {
                if let Some(handle) = state.pointer_focus {
                    let horizontal =
                        matches!(axis, WEnum::Value(wl_pointer::Axis::HorizontalScroll));
                    state.push_event(handle, InputKind::PointerAxis, |ev| {
                        if horizontal {
                            ev.x = value;
                        } else {
                            ev.y = value;
                        }
                        ev.time_ms = time;
                    });
                }
            }
            _ => {}
        }
    }
}

// ── Touch ────────────────────────────────────────────────────────────────────

impl Dispatch<wl_touch::WlTouch, ()> for State {
    fn event(
        state: &mut Self,
        _touch: &wl_touch::WlTouch,
        event: wl_touch::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_touch::Event::Down { time, surface, id, x, y, .. } => {
                state.touch_focus = state.surface_to_handle.get(&surface.id()).copied();
                if let Some(handle) = state.touch_focus {
                    state.push_event(handle, InputKind::Touch, |ev| {
                        ev.code = id as u32;
                        ev.value = 0; // down
                        ev.x = x;
                        ev.y = y;
                        ev.time_ms = time;
                    });
                }
            }
            wl_touch::Event::Motion { time, id, x, y } => {
                if let Some(handle) = state.touch_focus {
                    state.push_event(handle, InputKind::Touch, |ev| {
                        ev.code = id as u32;
                        ev.value = 1; // motion
                        ev.x = x;
                        ev.y = y;
                        ev.time_ms = time;
                    });
                }
            }
            wl_touch::Event::Up { time, id, .. } => {
                if let Some(handle) = state.touch_focus {
                    state.push_event(handle, InputKind::Touch, |ev| {
                        ev.code = id as u32;
                        ev.value = 2; // up
                        ev.time_ms = time;
                    });
                }
            }
            wl_touch::Event::Cancel => {
                state.touch_focus = None;
            }
            _ => {}
        }
    }
}

// ── Event-less / ignored objects ─────────────────────────────────────────────

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_surface::WlSurface);
