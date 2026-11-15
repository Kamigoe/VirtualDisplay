//! Linux (Wayland) only: keeps the desktop's shortcuts away while input goes to the Mac.
//!
//! GNOME acts on Super (Activities), Super+C, Alt+Tab, Alt+F4, Alt+Space, Ctrl+Alt+arrows ...
//! before vdview sees them. The keyboard-shortcuts-inhibit protocol
//! (`zwp_keyboard_shortcuts_inhibit_manager_v1`) asks the compositor to hand every key to
//! vdview's surface instead, as ordinary key events, which then go to the Mac the usual way.
//! vdview holds an inhibitor only while forwarding is on; the compositor itself applies it only
//! while vdview has keyboard focus, so another window in front gets its shortcuts back at once.
//! GNOME asks once whether vdview may do this, and Super+Esc always restores the shortcuts.
//!
//! The inhibitor covers all shortcuts at once, so it is only used when both the Windows (Super)
//! key and Alt go to the Mac: `--map-win none` or `--map-alt none` leaves every shortcut to the
//! desktop. GNOME never lets go of Ctrl+Alt+F1..F12 (virtual terminals) and Super+Esc.
//!
//! Keys are not split between the two sides: every key still reaches vdview's window (the
//! inhibitor only stops the desktop from acting on it), and when forwarding stops the Mac lets go
//! of everything, so a key pressed for the Mac and released after F8 does not linger anywhere.
//!
//! winit 0.30 does not expose the protocol, so this talks to the compositor itself through
//! winit's own Wayland connection (its `wl_display` and the window's `wl_surface`), on a separate
//! event queue that is dispatched from the event loop. X11 sessions are not handled.

use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat, wl_surface::WlSurface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1 as Manager,
    zwp_keyboard_shortcuts_inhibitor_v1::{self, ZwpKeyboardShortcutsInhibitorV1 as Inhibitor},
};
use winit::raw_window_handle::{
    HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
};
use winit::window::Window;

use crate::input::{Modifiers, Role};

/// Asks the compositor to keep its shortcuts while forwarding; see the module comment.
pub struct Shortcuts {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    manager: Manager,
    seat: wl_seat::WlSeat,
    surface: WlSurface,
    inhibitor: Option<Inhibitor>,
}

#[derive(Default)]
struct State {
    /// The compositor currently hands the shortcuts to vdview.
    active: bool,
}

impl Shortcuts {
    /// None on X11, without the protocol, or when Super or Alt stays with the desktop.
    pub fn new(window: &Window, modifiers: Modifiers) -> Option<Shortcuts> {
        if modifiers.win == Role::None || modifiers.alt == Role::None {
            return None;
        }
        let display = match window.display_handle().ok()?.as_raw() {
            RawDisplayHandle::Wayland(d) => d.display,
            _ => return None,
        };
        let surface = match window.window_handle().ok()?.as_raw() {
            RawWindowHandle::Wayland(w) => w.surface,
            _ => return None,
        };
        // Safety: both pointers come from winit, whose connection and window outlive this (it is
        // dropped in `exiting`, before winit closes the connection).
        let conn = Connection::from_backend(unsafe {
            Backend::from_foreign_display(display.as_ptr().cast())
        });
        let surface =
            unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.as_ptr().cast()) }
                .ok()
                .and_then(|id| WlSurface::from_id(&conn, id).ok())?;
        let (globals, queue) = registry_queue_init::<State>(&conn)
            .map_err(|e| eprintln!("shortcuts: cannot read the Wayland globals ({e})"))
            .ok()?;
        let qh = queue.handle();
        let manager = match globals.bind::<Manager, _, _>(&qh, 1..=1, ()) {
            Ok(m) => m,
            Err(e) => {
                eprintln!(
                    "the compositor cannot hand its shortcuts to vdview ({e}); Super and Alt+Tab stay with the desktop"
                );
                return None;
            }
        };
        let seat = globals
            .bind::<wl_seat::WlSeat, _, _>(&qh, 1..=1, ())
            .map_err(|e| eprintln!("shortcuts: no seat ({e})"))
            .ok()?;
        Some(Shortcuts {
            conn,
            queue,
            state: State::default(),
            manager,
            seat,
            surface,
            inhibitor: None,
        })
    }

    /// Call every turn of the event loop: `take` = forwarding to the Mac.
    pub fn update(&mut self, take: bool) {
        if take != self.inhibitor.is_some() {
            if take {
                let qh = self.queue.handle();
                self.inhibitor =
                    Some(
                        self.manager
                            .inhibit_shortcuts(&self.surface, &self.seat, &qh, ()),
                    );
            } else if let Some(i) = self.inhibitor.take() {
                i.destroy();
                if self.state.active {
                    self.state.active = false;
                    println!("desktop shortcuts: back to the desktop");
                }
            }
            let _ = self.conn.flush();
        }
        // winit reads the socket; this only handles what arrived for this queue.
        let _ = self.queue.dispatch_pending(&mut self.state);
    }
}

impl Drop for Shortcuts {
    fn drop(&mut self) {
        self.update(false);
    }
}

impl Dispatch<Inhibitor, ()> for State {
    fn event(
        state: &mut Self,
        _: &Inhibitor,
        event: zwp_keyboard_shortcuts_inhibitor_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let active = match event {
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Active => true,
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Inactive => false,
            _ => return,
        };
        if active != state.active {
            state.active = active;
            println!(
                "desktop shortcuts: {}",
                if active {
                    "go to the Mac (Super+Esc gives them back)"
                } else {
                    "back to the desktop"
                }
            );
        }
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

impl Dispatch<Manager, ()> for State {
    fn event(
        _: &mut Self,
        _: &Manager,
        _: <Manager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
