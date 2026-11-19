//! Linux (Wayland) only: watches and writes the desktop's clipboard for `clipboard`.
//!
//! GNOME has no protocol for reading the clipboard in the background (wlr / ext data-control), so
//! this is an ordinary `wl_data_device` client on winit's own Wayland connection (its
//! `wl_display`, on a separate event queue dispatched from the event loop, like `wlinhibit`):
//!
//! - PC → Mac: the compositor announces the clipboard (`selection`) whenever vdview gets keyboard
//!   focus and when it changes while vdview has focus. Text copied in another program therefore
//!   goes to the Mac when vdview comes to the front, before it can be pasted there. The same text
//!   announced again (every focus) is not sent again; neither is what was on the clipboard when
//!   vdview started (the first announcement), nor what vdview itself put there.
//! - Mac → PC: text is offered as vdview's own selection right away while vdview has focus
//!   (Wayland only lets the focused client set it), otherwise as soon as it gets focus again.
//!
//! The text itself goes through pipes, read and written on short-lived threads so the event loop
//! never waits for another program. Content that password managers mark with the MIME type
//! `x-kde-passwordManagerHint` (KeePassXC and others) is not read. X11 sessions are not handled.
//!
//! Trap (found the hard way, 2026-11-20): GNOME hands over non-blocking pipes, so writing more
//! than the pipe's buffer (64 KiB) stopped with EAGAIN; the writer makes its end blocking first.
//! `VDVIEW_CLIPBOARD_DEBUG=1` logs focus, announcements and transfers (sizes and types only).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use wayland_client::backend::Backend;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_keyboard, wl_pointer,
    wl_registry, wl_seat,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, event_created_child,
};
use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use winit::window::Window;

use crate::clipboard;

/// Text MIME types, most preferred first; all of them are offered when writing.
const TEXT_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];
const SECRET_HINT: &str = "x-kde-passwordManagerHint";

type OnCopy = Arc<Mutex<Box<dyn Fn(String) + Send>>>;

/// `VDVIEW_CLIPBOARD_DEBUG=1`: logs what happens (sizes and MIME types only, never the text).
fn debug() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VDVIEW_CLIPBOARD_DEBUG").is_some())
}

macro_rules! dbg_log {
    ($($a:tt)*) => { if debug() { eprintln!("clipboard debug: {}", format!($($a)*)); } };
}

/// Watches the clipboard; see the module comment. Call `update` every turn of the event loop and
/// drop it before winit closes its connection (`exiting`).
pub struct Clipboard {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    /// Text from the Mac, waiting for `update`.
    inbox: Mutex<Option<String>>,
}

struct State {
    manager: wl_data_device_manager::WlDataDeviceManager,
    device: wl_data_device::WlDataDevice,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    /// Latest input serial (key, button or keyboard focus), needed to set the selection.
    serial: Option<u32>,
    focused: bool,
    /// Text from the Mac to offer once vdview has focus.
    pending: Option<String>,
    /// vdview's own selection, while it is the clipboard.
    source: Option<wl_data_source::WlDataSource>,
    /// The clipboard as last announced (to destroy when replaced).
    offer: Option<wl_data_offer::WlDataOffer>,
    shared: Arc<Mutex<Shared>>,
    on_copy: OnCopy,
}

/// Shared with the reader threads.
#[derive(Default)]
struct Shared {
    /// Hash of the text on this PC's clipboard as far as known: last read, or last offered by
    /// vdview. Text announced again with the same hash is not a new copy.
    last: Option<u64>,
    /// Counts announcements and vdview's own offers; a read that finishes after a newer one
    /// started is dropped.
    latest: u64,
}

/// MIME types of an offer, as announced.
#[derive(Default)]
struct Mimes(Mutex<Vec<String>>);

impl Clipboard {
    /// `on_copy` runs on a reader thread with text copied on this PC (LF line ends). None on X11.
    pub fn start(window: &Window, on_copy: impl Fn(String) + Send + 'static) -> Option<Clipboard> {
        let display = match window.display_handle().ok()?.as_raw() {
            RawDisplayHandle::Wayland(d) => d.display,
            _ => {
                eprintln!("clipboard: not a Wayland session; it is not shared with the Mac");
                return None;
            }
        };
        // Safety: the pointer comes from winit, whose connection outlives this (it is dropped in
        // `exiting`, before winit closes the connection).
        let conn = Connection::from_backend(unsafe {
            Backend::from_foreign_display(display.as_ptr().cast())
        });
        let fail = |what: String| {
            eprintln!("cannot watch the clipboard ({what}); it is not shared with the Mac");
        };
        let (globals, queue) = registry_queue_init::<State>(&conn)
            .map_err(|e| fail(e.to_string()))
            .ok()?;
        let qh = queue.handle();
        let manager = globals
            .bind::<wl_data_device_manager::WlDataDeviceManager, _, _>(&qh, 1..=3, ())
            .map_err(|e| fail(e.to_string()))
            .ok()?;
        let seat = globals
            .bind::<wl_seat::WlSeat, _, _>(&qh, 1..=5, ())
            .map_err(|e| fail(e.to_string()))
            .ok()?;
        let device = manager.get_data_device(&seat, &qh, ());
        let state = State {
            manager,
            device,
            keyboard: None,
            pointer: None,
            serial: None,
            focused: false,
            pending: None,
            source: None,
            offer: None,
            shared: Arc::default(),
            on_copy: Arc::new(Mutex::new(Box::new(on_copy))),
        };
        let _ = conn.flush();
        Some(Clipboard {
            conn,
            queue,
            state,
            inbox: Mutex::new(None),
        })
    }

    /// Puts text from the Mac (LF line ends) on the clipboard (at the next `update`).
    pub fn set(&self, text: String) {
        *self.inbox.lock().unwrap() = Some(text);
    }

    /// Handles what arrived from the compositor and from the Mac.
    pub fn update(&mut self) {
        if let Some(text) = self.inbox.lock().unwrap().take() {
            self.state.pending = Some(text);
        }
        // winit reads the socket; this only handles what arrived for this queue.
        let _ = self.queue.dispatch_pending(&mut self.state);
        self.state.offer_pending(&self.queue.handle());
        let _ = self.conn.flush();
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        let s = &mut self.state;
        if let Some(o) = s.offer.take() {
            o.destroy();
        }
        if let Some(src) = s.source.take() {
            src.destroy();
        }
        if let Some(k) = s.keyboard.take() {
            k.release();
        }
        if let Some(p) = s.pointer.take() {
            p.release();
        }
        s.device.release();
        let _ = self.conn.flush();
    }
}

fn hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

impl State {
    /// Offers the Mac's text as vdview's selection, once vdview has focus.
    fn offer_pending(&mut self, qh: &QueueHandle<State>) {
        let (true, Some(serial)) = (self.focused, self.serial) else {
            return;
        };
        let Some(text) = self.pending.take() else {
            return;
        };
        {
            let mut s = self.shared.lock().unwrap();
            s.last = Some(hash(&text));
            s.latest += 1; // a read still running is older than this
        }
        let source = self
            .manager
            .create_data_source(qh, Arc::new(text.into_bytes()));
        for t in TEXT_TYPES {
            source.offer(t.into());
        }
        dbg_log!(
            "offer {} bytes as our selection (serial {serial})",
            source.data::<Arc<Vec<u8>>>().map_or(0, |t| t.len())
        );
        self.device.set_selection(Some(&source), serial);
        if let Some(old) = self.source.replace(source) {
            old.destroy();
        }
    }

    /// The compositor announced the clipboard (on focus, and on every change while focused).
    fn selection(&mut self, offer: Option<wl_data_offer::WlDataOffer>) {
        if let Some(old) = self.offer.take() {
            old.destroy();
        }
        let latest = {
            let mut s = self.shared.lock().unwrap();
            s.latest += 1;
            s.latest
        };
        let Some(offer) = offer else { return };
        // The source is cancelled before anyone else's selection is announced, so while it
        // lives the announcement is vdview's own text.
        let ours = self.source.is_some();
        let mimes = offer
            .data::<Mimes>()
            .map(|m| m.0.lock().unwrap().clone())
            .unwrap_or_default();
        let mime = TEXT_TYPES
            .iter()
            .find(|t| mimes.iter().any(|m| m == *t))
            .copied();
        let secret = mimes.iter().any(|m| m == SECRET_HINT);
        dbg_log!("selection #{latest}: ours {ours}, secret {secret}, types {mimes:?}");
        if !ours
            && !secret
            && let Some(mime) = mime
        {
            self.read(&offer, mime, latest);
        }
        self.offer = Some(offer);
    }

    /// Reads the announced text on its own thread and sends it if it is a new copy.
    fn read(&self, offer: &wl_data_offer::WlDataOffer, mime: &str, latest: u64) {
        let (mut reader, writer) = match std::io::pipe() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("clipboard: {e}");
                return;
            }
        };
        // libwayland sends a copy of the descriptor; closing ours right away lets the reader see
        // the end once the other program closes its copy.
        offer.receive(mime.into(), std::os::fd::AsFd::as_fd(&writer));
        drop(writer);
        let shared = self.shared.clone();
        let on_copy = self.on_copy.clone();
        let _ = std::thread::Builder::new()
            .name("clipboard-read".into())
            .spawn(move || {
                let mut bytes = Vec::new();
                if reader.read_to_end(&mut bytes).is_err() || bytes.is_empty() {
                    return;
                }
                let text = clipboard::to_wire(&String::from_utf8_lossy(&bytes));
                let h = hash(&text);
                {
                    let mut s = shared.lock().unwrap();
                    if s.latest != latest || s.last == Some(h) {
                        return; // superseded, or announced again without a new copy
                    }
                    s.last = Some(h);
                    if latest == 1 {
                        return; // on the clipboard when vdview started
                    }
                }
                (on_copy.lock().unwrap())(text);
            });
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_data_device::WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::Selection { id } => state.selection(id),
            // Drag and drop: vdview takes no drops.
            wl_data_device::Event::Enter { id: Some(o), .. } => o.destroy(),
            _ => {}
        }
    }

    event_created_child!(State, wl_data_device::WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (wl_data_offer::WlDataOffer, Mimes::default()),
    ]);
}

impl Dispatch<wl_data_offer::WlDataOffer, Mimes> for State {
    fn event(
        _: &mut Self,
        _: &wl_data_offer::WlDataOffer,
        event: wl_data_offer::Event,
        mimes: &Mimes,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            mimes.0.lock().unwrap().push(mime_type);
        }
    }
}

impl Dispatch<wl_data_source::WlDataSource, Arc<Vec<u8>>> for State {
    fn event(
        state: &mut Self,
        source: &wl_data_source::WlDataSource,
        event: wl_data_source::Event,
        text: &Arc<Vec<u8>>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { fd, mime_type } => {
                dbg_log!("send {} bytes as {mime_type}", text.len());
                let text = text.clone();
                let _ = std::thread::Builder::new()
                    .name("clipboard-write".into())
                    .spawn(move || {
                        let mut file = std::fs::File::from(fd);
                        // The other side may hand over a non-blocking pipe (GNOME does): text
                        // longer than the pipe's buffer (64 KiB) would stop with EAGAIN.
                        let raw = std::os::fd::AsRawFd::as_raw_fd(&file);
                        unsafe {
                            let flags = libc::fcntl(raw, libc::F_GETFL);
                            if flags >= 0 && flags & libc::O_NONBLOCK != 0 {
                                libc::fcntl(raw, libc::F_SETFL, flags & !libc::O_NONBLOCK);
                            }
                        }
                        let r = file.write_all(&text);
                        dbg_log!("sent: {r:?}");
                    });
            }
            wl_data_source::Event::Cancelled => {
                dbg_log!("our selection cancelled");
                // Someone else copied: the clipboard is no longer vdview's text.
                source.destroy();
                if state.source.as_ref() == Some(source) {
                    state.source = None;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        // vdview's own keyboard and pointer, only for their serials and keyboard focus.
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { serial, .. } => {
                dbg_log!("focus in (serial {serial})");
                state.serial = Some(serial);
                state.focused = true;
            }
            wl_keyboard::Event::Leave { .. } => {
                dbg_log!("focus out");
                state.focused = false
            }
            wl_keyboard::Event::Key { serial, .. } => state.serial = Some(serial),
            _ => {} // the keymap's descriptor is closed when the event is dropped
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button { serial, .. } = event {
            state.serial = Some(serial);
        }
    }
}

impl Dispatch<wl_data_device_manager::WlDataDeviceManager, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_data_device_manager::WlDataDeviceManager,
        _: wl_data_device_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
