//! Windows only: watches and writes the Windows clipboard for `clipboard`.
//!
//! Runs on its own thread with a message-only window: Windows tells that window about every
//! clipboard change (AddClipboardFormatListener), and the window owns the text vdview puts on the
//! clipboard. Text copied here goes to `on_copy`; text from the Mac comes in through `set`, and
//! the change that writing it causes is not reported back (it would bounce to the Mac). Content
//! that password managers mark as private (ExcludeClipboardContentFromMonitorProcessing) is not
//! reported.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::clipboard;

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

#[repr(C)]
struct WndClassExW {
    size: u32,
    style: u32,
    wnd_proc: unsafe extern "system" fn(isize, u32, usize, isize) -> isize,
    cls_extra: i32,
    wnd_extra: i32,
    instance: *mut c_void,
    icon: *mut c_void,
    cursor: *mut c_void,
    background: *mut c_void,
    menu_name: *const u16,
    class_name: *const u16,
    icon_small: *mut c_void,
}

const WM_QUIT: u32 = 0x0012;
const WM_CLIPBOARDUPDATE: u32 = 0x031D;
const WM_APP_CHANGED: u32 = 0x8000 + 1;
const WM_APP_SET: u32 = 0x8000 + 2;
const HWND_MESSAGE: isize = -3;
const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x0002;

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassExW(class: *const WndClassExW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: isize,
        menu: isize,
        instance: *mut c_void,
        param: *mut c_void,
    ) -> isize;
    fn DefWindowProcW(hwnd: isize, msg: u32, w: usize, l: isize) -> isize;
    fn DestroyWindow(hwnd: isize) -> i32;
    fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn PostThreadMessageW(thread: u32, msg: u32, w: usize, l: isize) -> i32;
    fn AddClipboardFormatListener(hwnd: isize) -> i32;
    fn RemoveClipboardFormatListener(hwnd: isize) -> i32;
    fn OpenClipboard(hwnd: isize) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn GetClipboardData(format: u32) -> *mut c_void;
    fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
    fn IsClipboardFormatAvailable(format: u32) -> i32;
    fn RegisterClipboardFormatW(name: *const u16) -> u32;
    fn GetClipboardSequenceNumber() -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn GetCurrentThreadId() -> u32;
    fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
    fn GlobalLock(mem: *mut c_void) -> *mut c_void;
    fn GlobalUnlock(mem: *mut c_void) -> i32;
    fn GlobalFree(mem: *mut c_void) -> *mut c_void;
    fn GlobalSize(mem: *mut c_void) -> usize;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn wnd_proc(hwnd: isize, msg: u32, w: usize, l: isize) -> isize {
    if msg == WM_CLIPBOARDUPDATE {
        // Handled by the loop below, which knows the state.
        unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_APP_CHANGED, 0, 0) };
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}

/// The clipboard thread; stopped when dropped.
pub struct Clipboard {
    thread: u32,
    /// Text from the Mac waiting to be written by the clipboard thread.
    inbox: Arc<Mutex<Option<String>>>,
}

impl Clipboard {
    /// `on_copy` runs on the clipboard thread with text copied on this PC (LF line ends).
    /// The window is not needed on Windows (Linux needs its Wayland connection).
    pub fn start(
        _window: &winit::window::Window,
        on_copy: impl Fn(String) + Send + 'static,
    ) -> Option<Clipboard> {
        Self::spawn(on_copy)
    }

    fn spawn(on_copy: impl Fn(String) + Send + 'static) -> Option<Clipboard> {
        let inbox = Arc::new(Mutex::new(None::<String>));
        let thread_inbox = inbox.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                let hwnd = match create_window() {
                    Ok(h) => h,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                let _ = tx.send(Ok(unsafe { GetCurrentThreadId() }));
                let mut w = Watcher::new(hwnd);
                let mut msg = std::mem::MaybeUninit::<Msg>::uninit();
                while unsafe { GetMessageW(msg.as_mut_ptr(), 0, 0, 0) } > 0 {
                    let m = unsafe { msg.assume_init_ref() };
                    match m.message {
                        WM_APP_CHANGED => {
                            if let Some(text) = w.changed() {
                                on_copy(text);
                            }
                        }
                        WM_APP_SET => {
                            if let Some(text) = thread_inbox.lock().unwrap().take() {
                                w.write(&text);
                            }
                        }
                        _ => unsafe {
                            DispatchMessageW(m);
                        },
                    }
                }
                unsafe {
                    RemoveClipboardFormatListener(hwnd);
                    DestroyWindow(hwnd);
                }
            });
        let result = match spawned {
            Ok(_) => rx
                .recv()
                .unwrap_or_else(|_| Err(std::io::Error::other("clipboard thread ended"))),
            Err(e) => Err(e),
        };
        match result {
            Ok(thread) => Some(Clipboard { thread, inbox }),
            Err(e) => {
                eprintln!("cannot watch the clipboard ({e}); it is not shared with the Mac");
                None
            }
        }
    }

    /// Puts text from the Mac (LF line ends) on the clipboard.
    pub fn set(&self, text: String) {
        *self.inbox.lock().unwrap() = Some(text);
        unsafe { PostThreadMessageW(self.thread, WM_APP_SET, 0, 0) };
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        unsafe { PostThreadMessageW(self.thread, WM_QUIT, 0, 0) };
    }
}

fn create_window() -> std::io::Result<isize> {
    let class = wide("vdview-clipboard");
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WndClassExW {
        size: size_of::<WndClassExW>() as u32,
        style: 0,
        wnd_proc,
        cls_extra: 0,
        wnd_extra: 0,
        instance,
        icon: std::ptr::null_mut(),
        cursor: std::ptr::null_mut(),
        background: std::ptr::null_mut(),
        menu_name: std::ptr::null(),
        class_name: class.as_ptr(),
        icon_small: std::ptr::null_mut(),
    };
    // Fails harmlessly if the class already exists (a second Clipboard in this process).
    unsafe { RegisterClassExW(&wc) };
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            std::ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            0,
            instance,
            std::ptr::null_mut(),
        )
    };
    if hwnd == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { AddClipboardFormatListener(hwnd) } == 0 {
        let e = std::io::Error::last_os_error();
        unsafe { DestroyWindow(hwnd) };
        return Err(e);
    }
    Ok(hwnd)
}

/// State of the clipboard thread.
struct Watcher {
    hwnd: isize,
    /// Clipboard formats that mark content as not to be shared.
    private_formats: Vec<u32>,
    /// Sequence number of the last change handled (ours or someone else's).
    seen: u32,
}

impl Watcher {
    fn new(hwnd: isize) -> Watcher {
        let private_formats = [
            "ExcludeClipboardContentFromMonitorProcessing",
            "Clipboard Viewer Ignore",
        ]
        .iter()
        .map(|n| unsafe { RegisterClipboardFormatW(wide(n).as_ptr()) })
        .filter(|&f| f != 0)
        .collect();
        Watcher {
            hwnd,
            private_formats,
            // What is on the clipboard already is not sent; only later copies are.
            seen: unsafe { GetClipboardSequenceNumber() },
        }
    }

    /// Someone changed the clipboard: its text, unless it is ours, private or not text.
    fn changed(&mut self) -> Option<String> {
        let seq = unsafe { GetClipboardSequenceNumber() };
        if seq == self.seen {
            return None; // our own write, or already handled
        }
        self.seen = seq;
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) } == 0 {
            return None;
        }
        let _open = self.open()?;
        if self
            .private_formats
            .iter()
            .any(|&f| unsafe { IsClipboardFormatAvailable(f) } != 0)
        {
            return None;
        }
        let mem = unsafe { GetClipboardData(CF_UNICODETEXT) };
        if mem.is_null() {
            return None;
        }
        let p = unsafe { GlobalLock(mem) } as *const u16;
        if p.is_null() {
            return None;
        }
        let units = unsafe { GlobalSize(mem) } / 2;
        let all = unsafe { std::slice::from_raw_parts(p, units) };
        let len = all.iter().position(|&c| c == 0).unwrap_or(units);
        let text = String::from_utf16_lossy(&all[..len]);
        unsafe { GlobalUnlock(mem) };
        Some(clipboard::to_wire(&text))
    }

    fn write(&mut self, text: &str) {
        let units: Vec<u16> = clipboard::from_wire(text)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mem = unsafe { GlobalAlloc(GMEM_MOVEABLE, units.len() * 2) };
        if mem.is_null() {
            return;
        }
        let p = unsafe { GlobalLock(mem) } as *mut u16;
        if p.is_null() {
            unsafe { GlobalFree(mem) };
            return;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
            GlobalUnlock(mem);
        }
        let Some(open) = self.open() else {
            unsafe { GlobalFree(mem) };
            eprintln!("clipboard: busy; text from the Mac not pasted here");
            return;
        };
        let ok = unsafe { EmptyClipboard() } != 0
            && !unsafe { SetClipboardData(CF_UNICODETEXT, mem) }.is_null();
        if !ok {
            unsafe { GlobalFree(mem) }; // still ours: the clipboard did not take it
        }
        drop(open);
        // The change notification for this write must not go back to the Mac.
        self.seen = unsafe { GetClipboardSequenceNumber() };
    }

    /// Opens the clipboard, waiting a little if another program has it open.
    fn open(&self) -> Option<Open> {
        for _ in 0..20 {
            if unsafe { OpenClipboard(self.hwnd) } != 0 {
                return Some(Open);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }
}

/// Closes the clipboard when dropped.
struct Open;

impl Drop for Open {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes the real clipboard (restoring its text afterwards), so it only runs on request:
    /// `cargo test --release -- --ignored clipboard`.
    #[test]
    #[ignore]
    fn clipboard_round_trip() {
        let (tx, rx) = std::sync::mpsc::channel();
        let c = Clipboard::spawn(move |t| {
            let _ = tx.send(t);
        })
        .unwrap();
        let before = read_now();

        // Text from the Mac is written with CR LF and does not come back as a copy.
        c.set("from the Mac\nline 2 あ".into());
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(read_now().as_deref(), Some("from the Mac\r\nline 2 あ"));
        assert!(rx.try_recv().is_err());

        // A copy by another program is reported, with LF.
        write_now("copied on the PC\r\nok");
        let got = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got, "copied on the PC\nok");

        drop(c);
        if let Some(t) = before {
            write_now(&t);
        }
    }

    fn read_now() -> Option<String> {
        let w = Watcher {
            hwnd: 0,
            private_formats: vec![],
            seen: u32::MAX,
        };
        let _o = w.open()?;
        let mem = unsafe { GetClipboardData(CF_UNICODETEXT) };
        if mem.is_null() {
            return None;
        }
        let p = unsafe { GlobalLock(mem) } as *const u16;
        let units = unsafe { GlobalSize(mem) } / 2;
        let all = unsafe { std::slice::from_raw_parts(p, units) };
        let len = all.iter().position(|&c| c == 0).unwrap_or(units);
        let s = String::from_utf16_lossy(&all[..len]);
        unsafe { GlobalUnlock(mem) };
        Some(s)
    }

    /// Writes like another program would (from a thread without our window).
    fn write_now(text: &str) {
        std::thread::scope(|s| {
            s.spawn(|| {
                let hwnd = create_window().unwrap();
                let mut w = Watcher {
                    hwnd,
                    private_formats: vec![],
                    seen: 0,
                };
                w.write(&clipboard::to_wire(text));
                unsafe {
                    RemoveClipboardFormatListener(hwnd);
                    DestroyWindow(hwnd);
                }
            });
        });
    }
}
