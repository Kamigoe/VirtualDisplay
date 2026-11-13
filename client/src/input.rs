//! Turns this PC's keyboard and mouse events into the Mac's input messages.
//!
//! Keys go by physical position (winit `KeyCode`, the W3C `code`), translated to the Mac's
//! virtual key codes (`kVK_*`), so the Mac's own layout (JIS) and input method make the same
//! characters as when typing on it directly.

use anyhow::{Result, bail};
use winit::event::{MouseButton, MouseScrollDelta};
use winit::keyboard::KeyCode;

/// What a PC modifier key becomes on the Mac (`--map-ctrl` / `--map-alt` / `--map-win`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    Command,
    Control,
    Option,
    Shift,
    None,
}

impl Role {
    pub fn parse(s: &str) -> Result<Role> {
        Ok(match s {
            "cmd" | "command" => Role::Command,
            "ctrl" | "control" => Role::Control,
            "alt" | "option" | "opt" => Role::Option,
            "shift" => Role::Shift,
            "none" => Role::None,
            other => bail!("unknown modifier {other} (cmd, ctrl, alt, shift or none)"),
        })
    }

    /// Mac key code of the left / right variant.
    fn code(self, right: bool) -> Option<u16> {
        Some(match (self, right) {
            (Role::Command, false) => 55,
            (Role::Command, true) => 54,
            (Role::Shift, false) => 56,
            (Role::Shift, true) => 60,
            (Role::Option, false) => 58,
            (Role::Option, true) => 61,
            (Role::Control, false) => 59,
            (Role::Control, true) => 62,
            (Role::None, _) => return None,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Modifiers {
    pub ctrl: Role,
    pub alt: Role,
    pub win: Role,
}

impl Default for Modifiers {
    /// Same as mac-input-cast: the PC's Ctrl sits where the Mac's Command is used most.
    fn default() -> Self {
        Modifiers {
            ctrl: Role::Command,
            alt: Role::Option,
            win: Role::Control,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MacKey {
    Code(u16),
    /// 半角/全角: the Mac switches between 英数 and かな.
    ImeToggle,
}

/// Mac virtual key code for a physical key, or None for keys that have no counterpart.
pub fn mac_key(code: KeyCode, m: &Modifiers) -> Option<MacKey> {
    use KeyCode::*;
    let c: u16 = match code {
        KeyA => 0,
        KeyS => 1,
        KeyD => 2,
        KeyF => 3,
        KeyH => 4,
        KeyG => 5,
        KeyZ => 6,
        KeyX => 7,
        KeyC => 8,
        KeyV => 9,
        KeyB => 11,
        KeyQ => 12,
        KeyW => 13,
        KeyE => 14,
        KeyR => 15,
        KeyY => 16,
        KeyT => 17,
        Digit1 => 18,
        Digit2 => 19,
        Digit3 => 20,
        Digit4 => 21,
        Digit6 => 22,
        Digit5 => 23,
        Equal => 24, // JIS: ^
        Digit9 => 25,
        Digit7 => 26,
        Minus => 27,
        Digit8 => 28,
        Digit0 => 29,
        BracketRight => 30, // JIS: [
        KeyO => 31,
        KeyU => 32,
        BracketLeft => 33, // JIS: @
        KeyI => 34,
        KeyP => 35,
        Enter => 36,
        KeyL => 37,
        KeyJ => 38,
        Quote => 39, // JIS: :
        KeyK => 40,
        Semicolon => 41,
        Backslash => 42, // JIS: ]
        Comma => 43,
        Slash => 44,
        KeyN => 45,
        KeyM => 46,
        Period => 47,
        Tab => 48,
        Space => 49,
        Backspace => 51,
        Escape => 53,
        CapsLock => 57, // JIS PC keyboards: 英数 (Shift+英数 is Caps Lock)
        IntlYen => 93,
        IntlRo => 94, // JIS: ろ / _
        NonConvert | Lang2 => 102,
        Convert | KanaMode | Lang1 => 104,
        Backquote => return Some(MacKey::ImeToggle), // JIS: 半角/全角
        ControlLeft => m.ctrl.code(false)?,
        ControlRight => m.ctrl.code(true)?,
        AltLeft => m.alt.code(false)?,
        AltRight => m.alt.code(true)?,
        SuperLeft => m.win.code(false)?,
        SuperRight => m.win.code(true)?,
        ShiftLeft => 56,
        ShiftRight => 60,
        NumpadDecimal => 65,
        NumpadMultiply => 67,
        NumpadAdd => 69,
        NumLock => 71, // keypad Clear
        NumpadDivide => 75,
        NumpadEnter => 76,
        NumpadSubtract => 78,
        NumpadEqual => 81,
        Numpad0 => 82,
        Numpad1 => 83,
        Numpad2 => 84,
        Numpad3 => 85,
        Numpad4 => 86,
        Numpad5 => 87,
        Numpad6 => 88,
        Numpad7 => 89,
        Numpad8 => 91,
        Numpad9 => 92,
        NumpadComma => 95,
        F1 => 122,
        F2 => 120,
        F3 => 99,
        F4 => 118,
        F5 => 96,
        F6 => 97,
        F7 => 98,
        F8 => 100,
        F9 => 101,
        F10 => 109,
        F11 => 103,
        F12 => 111,
        F13 => 105,
        F14 => 107,
        F15 => 113,
        F16 => 106,
        F17 => 64,
        F18 => 79,
        F19 => 80,
        F20 => 90,
        Insert => 114, // Help
        Home => 115,
        PageUp => 116,
        Delete => 117,
        End => 119,
        PageDown => 121,
        ArrowLeft => 123,
        ArrowRight => 124,
        ArrowDown => 125,
        ArrowUp => 126,
        AudioVolumeMute => 74,
        AudioVolumeDown => 73,
        AudioVolumeUp => 72,
        _ => return None,
    };
    Some(MacKey::Code(c))
}

/// Body of a Key message.
pub fn key_body(down: bool, code: u16, repeat: bool) -> [u8; 4] {
    let c = code.to_be_bytes();
    [down as u8, c[0], c[1], repeat as u8]
}

pub fn button_index(b: MouseButton) -> Option<u8> {
    Some(match b {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::Back => 3,
        MouseButton::Forward => 4,
        MouseButton::Other(_) => return None,
    })
}

/// Body of a Scroll message: hundredths of a line for wheels, pixels for touchpads; +dy is up
/// (winit's convention too).
pub fn scroll_body(delta: MouseScrollDelta) -> Option<[u8; 5]> {
    let clamp = |v: f64| v.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
    let (dx, dy, unit) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (clamp(x as f64 * 100.0), clamp(y as f64 * 100.0), 0),
        MouseScrollDelta::PixelDelta(p) => (clamp(p.x), clamp(p.y), 1),
    };
    if dx == 0 && dy == 0 {
        return None;
    }
    let (x, y) = (dx.to_be_bytes(), dy.to_be_bytes());
    Some([x[0], x[1], y[0], y[1], unit])
}

/// Maps a window position (physical pixels) to the video's 0..=65535 range. The renderer draws
/// the video centered and aspect-fitted, so this mirrors its letterboxing; positions on the black
/// bars stick to the nearest edge.
pub fn pointer_position(
    (px, py): (f64, f64),
    (win_w, win_h): (u32, u32),
    (video_w, video_h): (u16, u16),
) -> Option<(u16, u16)> {
    if win_w == 0 || win_h == 0 || video_w == 0 || video_h == 0 {
        return None;
    }
    let (tw, th) = (win_w as f64, win_h as f64);
    let (va, sa) = (video_w as f64 / video_h as f64, tw / th);
    let (w, h) = if va > sa {
        (tw, tw / va)
    } else {
        (th * va, th)
    };
    let (x0, y0) = ((tw - w) / 2.0, (th - h) / 2.0);
    let norm = |v: f64, origin: f64, size: f64| {
        (((v - origin) / size).clamp(0.0, 1.0) * 65535.0).round() as u16
    };
    Some((norm(px, x0, w), norm(py, y0, h)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_follow_the_mapping() {
        let m = Modifiers::default();
        assert_eq!(mac_key(KeyCode::ControlLeft, &m), Some(MacKey::Code(55)));
        assert_eq!(mac_key(KeyCode::SuperLeft, &m), Some(MacKey::Code(59)));
        let none = Modifiers {
            win: Role::None,
            ..m
        };
        assert_eq!(mac_key(KeyCode::SuperLeft, &none), None);
        assert_eq!(mac_key(KeyCode::Backquote, &m), Some(MacKey::ImeToggle));
        assert_eq!(mac_key(KeyCode::IntlYen, &m), Some(MacKey::Code(93)));
    }

    #[test]
    fn pointer_follows_letterboxing() {
        // 16:9 video in a 4:3 window: bars top and bottom.
        assert_eq!(
            pointer_position((0.0, 0.0), (1600, 1200), (1920, 1080)),
            Some((0, 0))
        );
        assert_eq!(
            pointer_position((800.0, 600.0), (1600, 1200), (1920, 1080)),
            Some((32768, 32768))
        );
        assert_eq!(
            pointer_position((1599.0, 1199.0), (1600, 1200), (1920, 1080))
                .unwrap()
                .1,
            65535
        );
    }

    #[test]
    fn scroll_units() {
        assert_eq!(
            scroll_body(MouseScrollDelta::LineDelta(0.0, 1.0)),
            Some([0, 0, 0, 100, 0])
        );
        assert_eq!(scroll_body(MouseScrollDelta::LineDelta(0.0, 0.0)), None);
    }
}
