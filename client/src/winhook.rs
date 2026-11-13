//! Windows only: keeps the Windows key and Alt away from Windows while input goes to the Mac.
//!
//! Windows acts on Win (Start menu, Win+C Copilot, Win+D ...), Alt+Tab, Alt+F4, Alt+Space and
//! Ctrl+Esc before any application sees them. A low-level keyboard hook sees keys first: while
//! vdview is in front and forwarding is on, it swallows Win, Alt (and Esc while Ctrl is held) and
//! sends them to the Mac itself. Windows then never learns that Win or Alt is down, so the other
//! key of a combination (C, Tab, F4 ...) reaches vdview as an ordinary key and goes the usual way.
//!
//! Keys pass through untouched when forwarding is off, another window is in front, or the key is
//! mapped to `none` (e.g. `--map-win none` leaves the Windows key to Windows). A key whose press
//! was swallowed also has its release swallowed, and vice versa, so neither side sees half a key.
//! Ctrl+Alt+Del and Win+L are handled below the hook and cannot be taken.
//!
//! Two traps (both found the hard way, 2026-11-13):
//! - Windows stops calling a process's low-level keyboard hook once that process registers for raw
//!   keyboard input, which winit does for its device events. vdview turns those off
//!   (`DeviceEvents::Never`); it does not use them.
//! - The hook runs on its own thread with a plain message loop, so a busy event loop (rendering,
//!   a mode switch) can never delay keys or get the hook removed for being slow.
//!
//! `VDVIEW_HOOK_TEST=1` also takes keys synthesized by other programs and logs (key codes only)
//! what is swallowed, for automated tests.

use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use winit::keyboard::KeyCode;

use crate::control::Control;
use crate::input::{self, MacKey, Modifiers};
use crate::protocol;

type Hhook = *mut c_void;

#[repr(C)]
struct KbdLlHookStruct {
    vk_code: u32,
    scan_code: u32,
    flags: u32,
    time: u32,
    extra_info: usize,
}

/// Win32 MSG (only received, never inspected).
#[repr(C)]
struct Msg {
    hwnd: isize,
    message: u32,
    w: usize,
    l: isize,
    time: u32,
    pt: [i32; 2],
    private: u32,
}

const WH_KEYBOARD_LL: i32 = 13;
const HC_ACTION: i32 = 0;
const WM_QUIT: u32 = 0x0012;
const WM_KEYDOWN: usize = 0x0100;
const WM_SYSKEYDOWN: usize = 0x0104;
const LLKHF_INJECTED: u32 = 0x10;

const VK_ESCAPE: u32 = 0x1B;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;

#[link(name = "user32")]
unsafe extern "system" {
    fn SetWindowsHookExW(
        id: i32,
        proc: unsafe extern "system" fn(i32, usize, isize) -> isize,
        module: *mut c_void,
        thread: u32,
    ) -> Hhook;
    fn UnhookWindowsHookEx(hook: Hhook) -> i32;
    fn CallNextHookEx(hook: Hhook, code: i32, w: usize, l: isize) -> isize;
    fn GetForegroundWindow() -> isize;
    fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
    fn PostThreadMessageW(thread: u32, msg: u32, w: usize, l: isize) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn GetCurrentThreadId() -> u32;
}

struct State {
    hwnd: isize,
    modifiers: Modifiers,
    /// Where swallowed keys go; None while not forwarding.
    control: Option<Arc<Control>>,
    /// Keys whose press was swallowed (their release must be too).
    swallowed: HashSet<u32>,
    /// Left / right Ctrl held (for Ctrl+Esc).
    ctrl: [bool; 2],
}

/// Shared by the hook thread and the event loop; each holds it only briefly.
static STATE: Mutex<Option<State>> = Mutex::new(None);
static TEST: AtomicBool = AtomicBool::new(false);

/// The installed hook (on its own thread); removed when dropped.
pub struct Hook {
    thread: u32,
}

impl Hook {
    pub fn install(hwnd: isize, modifiers: Modifiers) -> Option<Hook> {
        TEST.store(
            std::env::var_os("VDVIEW_HOOK_TEST").is_some(),
            Ordering::Relaxed,
        );
        *STATE.lock().unwrap() = Some(State {
            hwnd,
            modifiers,
            control: None,
            swallowed: HashSet::new(),
            ctrl: [false; 2],
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("keyboard-hook".into())
            .spawn(move || {
                let hook = unsafe {
                    SetWindowsHookExW(
                        WH_KEYBOARD_LL,
                        hook_proc,
                        GetModuleHandleW(std::ptr::null()),
                        0,
                    )
                };
                if hook.is_null() {
                    let _ = tx.send(Err(std::io::Error::last_os_error()));
                    return;
                }
                let _ = tx.send(Ok(unsafe { GetCurrentThreadId() }));
                // Windows runs the hook inside GetMessage; it returns 0 on WM_QUIT (from drop).
                let mut msg = std::mem::MaybeUninit::<Msg>::uninit();
                while unsafe { GetMessageW(msg.as_mut_ptr(), 0, 0, 0) } > 0 {}
                unsafe { UnhookWindowsHookEx(hook) };
            });
        let result = match spawned {
            Ok(_) => rx
                .recv()
                .unwrap_or_else(|_| Err(std::io::Error::other("hook thread ended"))),
            Err(e) => Err(e),
        };
        match result {
            Ok(thread) => Some(Hook { thread }),
            Err(e) => {
                eprintln!(
                    "cannot install the keyboard hook ({e}); the Windows key and Alt+Tab stay with Windows"
                );
                *STATE.lock().unwrap() = None;
                None
            }
        }
    }

    /// `control` = where to send while forwarding to the Mac, None while not forwarding.
    pub fn update(&self, control: Option<&Arc<Control>>) {
        if let Some(s) = STATE.lock().unwrap().as_mut() {
            let same = match (&s.control, control) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            };
            if !same {
                s.control = control.cloned();
            }
        }
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        unsafe { PostThreadMessageW(self.thread, WM_QUIT, 0, 0) };
        *STATE.lock().unwrap() = None;
    }
}

unsafe extern "system" fn hook_proc(code: i32, w: usize, l: isize) -> isize {
    if code == HC_ACTION {
        let k = unsafe { &*(l as *const KbdLlHookStruct) };
        if handle(w == WM_KEYDOWN || w == WM_SYSKEYDOWN, k) {
            return 1; // swallowed: Windows and vdview's window never see it
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, w, l) }
}

/// Returns true to swallow the key.
fn handle(down: bool, k: &KbdLlHookStruct) -> bool {
    let test = TEST.load(Ordering::Relaxed);
    if k.flags & LLKHF_INJECTED != 0 && !test {
        return false; // synthesized by some program: leave it alone
    }
    let Ok(mut guard) = STATE.lock() else {
        return false;
    };
    let Some(s) = guard.as_mut() else {
        return false;
    };
    let code = match k.vk_code {
        VK_LCONTROL | VK_RCONTROL => {
            s.ctrl[(k.vk_code == VK_RCONTROL) as usize] = down;
            return false; // Ctrl itself reaches vdview normally
        }
        VK_LWIN => KeyCode::SuperLeft,
        VK_RWIN => KeyCode::SuperRight,
        VK_LMENU => KeyCode::AltLeft,
        VK_RMENU => KeyCode::AltRight,
        VK_ESCAPE if !down || s.ctrl.contains(&true) => KeyCode::Escape,
        _ => return false,
    };
    let mac = input::mac_key(code, &s.modifiers);
    let held = s.swallowed.contains(&k.vk_code);
    if down {
        let take = held
            || (s.control.is_some() && mac.is_some() && unsafe { GetForegroundWindow() } == s.hwnd);
        if !take {
            return false;
        }
        s.swallowed.insert(k.vk_code);
    } else if !s.swallowed.remove(&k.vk_code) {
        return false; // Windows saw the press, so it gets the release too
    }
    if test {
        eprintln!("keyboard hook: swallowed vk {:02x} down {down}", k.vk_code);
    }
    // After forwarding stopped `control` is None: the release is only swallowed (the Mac already
    // let go of everything when forwarding was turned off).
    if let (Some(c), Some(MacKey::Code(mac))) = (&s.control, mac) {
        c.send_input(
            protocol::INPUT_KEY,
            &input::key_body(down, mac, down && held),
        );
    }
    true
}
