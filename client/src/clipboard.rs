//! Text clipboard shared with the Mac (both ways, as soon as either side copies).
//!
//! Text travels inside Secure as ClipText messages: u32 total length ‖ u32 offset ‖ UTF-8 bytes,
//! at most `CHUNK` bytes each, in order. Line ends are LF on the wire. Up to `MAX_TEXT` bytes;
//! longer text is not sent. Nothing is sent on connecting, only later copies; text the other side
//! marked as secret (password managers) is not sent at all. The OS side lives in `clipwin`
//! (Windows) and `clipwl` (Linux, Wayland); elsewhere there is no clipboard yet.

#[cfg(windows)]
pub use crate::clipwin::Clipboard;
#[cfg(target_os = "linux")]
pub use crate::clipwl::Clipboard;

/// No clipboard on this platform yet.
#[cfg(not(any(windows, target_os = "linux")))]
pub struct Clipboard;

#[cfg(not(any(windows, target_os = "linux")))]
impl Clipboard {
    pub fn start(
        _window: &winit::window::Window,
        _on_copy: impl Fn(String) + Send + 'static,
    ) -> Option<Clipboard> {
        None
    }
    pub fn set(&self, _text: String) {}
}

/// Largest text sent or accepted, in UTF-8 bytes.
pub const MAX_TEXT: usize = 1 << 20;
/// Text bytes per ClipText message (keeps each one well inside a control message).
pub const CHUNK: usize = 60_000;

/// ClipText bodies for `text` (LF line ends); None when empty or too long.
pub fn chunks(text: &str) -> Option<Vec<Vec<u8>>> {
    let b = text.as_bytes();
    if b.is_empty() || b.len() > MAX_TEXT {
        return None;
    }
    let total = (b.len() as u32).to_be_bytes();
    Some(
        b.chunks(CHUNK)
            .enumerate()
            .map(|(i, c)| {
                let mut m = Vec::with_capacity(8 + c.len());
                m.extend_from_slice(&total);
                m.extend_from_slice(&((i * CHUNK) as u32).to_be_bytes());
                m.extend_from_slice(c);
                m
            })
            .collect(),
    )
}

/// Puts ClipText messages back together.
#[derive(Default)]
pub struct Assembler {
    total: usize,
    buf: Vec<u8>,
}

impl Assembler {
    /// Returns the text once the last piece arrived. A piece at offset 0 starts over; anything
    /// out of order drops the text being assembled.
    pub fn push(&mut self, body: &[u8]) -> Option<String> {
        if body.len() < 8 {
            return None;
        }
        let total = u32::from_be_bytes(body[0..4].try_into().unwrap()) as usize;
        let offset = u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize;
        let data = &body[8..];
        if offset == 0 {
            self.buf.clear();
            self.total = total;
        }
        if total == 0
            || total > MAX_TEXT
            || total != self.total
            || offset != self.buf.len()
            || offset + data.len() > total
        {
            self.buf = Vec::new();
            self.total = 0;
            return None;
        }
        self.buf.extend_from_slice(data);
        if self.buf.len() < total {
            return None;
        }
        self.total = 0;
        String::from_utf8(std::mem::take(&mut self.buf)).ok()
    }
}

/// For the wire: CR LF and lone CR become LF.
pub fn to_wire(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// From the wire, with the line ends Windows programs expect.
#[cfg(windows)]
pub fn from_wire(text: &str) -> String {
    to_wire(text).replace('\n', "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_in_pieces() {
        let text = "あいう\n".repeat(30_000); // 300 KB of multi-byte text: several pieces
        let parts = chunks(&text).unwrap();
        assert_eq!(parts.len(), text.len().div_ceil(CHUNK));
        let mut a = Assembler::default();
        let mut got = None;
        for p in &parts {
            assert!(got.is_none());
            got = a.push(p);
        }
        assert_eq!(got.as_deref(), Some(text.as_str()));
    }

    #[test]
    fn limits_and_order() {
        assert!(chunks("").is_none());
        assert!(chunks(&"x".repeat(MAX_TEXT + 1)).is_none());
        assert!(chunks(&"x".repeat(MAX_TEXT)).is_some());

        let parts = chunks(&"y".repeat(CHUNK * 2 + 1)).unwrap();
        let mut a = Assembler::default();
        assert_eq!(a.push(&parts[0]), None);
        assert_eq!(a.push(&parts[2]), None); // skipped a piece: dropped
        assert_eq!(a.push(&parts[1]), None); // and not resumed
        // A new text starts over cleanly.
        let short = chunks("hello").unwrap();
        assert_eq!(a.push(&short[0]).as_deref(), Some("hello"));
    }

    #[test]
    fn line_ends() {
        assert_eq!(to_wire("a\r\nb\rc\n"), "a\nb\nc\n");
        #[cfg(windows)]
        assert_eq!(from_wire("a\nb\r\n"), "a\r\nb\r\n");
    }
}
