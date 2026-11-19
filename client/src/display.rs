//! Which of this PC's monitors vdview shows on, and which mode it asks the Mac for.
//!
//! - `candidates`: the modes offered for a monitor. First the sizes that match the monitor's
//!   shape ("looks like", as in the Mac's display settings: 1x, and HiDPI at 2x, 1.75x, 1.5x and
//!   1.25x text size), then common sizes that fit on it (black bars where the shape differs).
//!   Modes the Mac's encoder cannot keep up with at 60 Hz are left out.
//! - `Saved`: the last monitor chosen and the mode chosen for each monitor, kept in
//!   `display` next to the input secret, so the next start uses them.
//! - `Picker`: the menu (in the status panel) shown at start when several monitors are connected,
//!   and on F6: first the monitor (when there are several), then the mode.

use std::path::PathBuf;

use winit::keyboard::{Key, NamedKey};

/// A mode to ask the Mac for: logical size, HiDPI = 2x backing pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Res {
    pub width: u16,
    pub height: u16,
    pub hidpi: bool,
}

impl Res {
    /// Pixels the Mac encodes per picture.
    pub fn backing_pixels(&self) -> f64 {
        let k = if self.hidpi { 4.0 } else { 1.0 };
        self.width as f64 * self.height as f64 * k
    }

    pub fn label(&self) -> String {
        format!(
            "{}X{}{}",
            self.width,
            self.height,
            if self.hidpi { " HIDPI" } else { "" }
        )
    }

    fn parse(s: &str) -> Option<Res> {
        let mut words = s.split_whitespace();
        let (w, h) = words.next()?.split_once('x')?;
        let hidpi = match words.next() {
            Some("hidpi") => true,
            Some("1x") | None => false,
            _ => return None,
        };
        Some(Res {
            width: w.parse().ok()?,
            height: h.parse().ok()?,
            hidpi,
        })
    }

    fn to_text(self) -> String {
        format!(
            "{}x{} {}",
            self.width,
            self.height,
            if self.hidpi { "hidpi" } else { "1x" }
        )
    }
}

/// Text sizes offered as HiDPI "looks like" modes (2 = the monitor's own pixels, sharpest).
const HIDPI_SCALES: [f64; 4] = [2.0, 1.75, 1.5, 1.25];
/// Common logical sizes (1x, and HiDPI for the smaller ones).
const COMMON: [(u16, u16, bool); 8] = [
    (3840, 2160, false),
    (3440, 1440, false),
    (2560, 1440, false),
    (2560, 1080, false),
    (1920, 1080, false),
    (1280, 720, false),
    (1920, 1080, true),
    (1280, 720, true),
];

/// The Mac's encoder keeps up with this many backing pixels at 60 Hz.
pub fn fits_encoder(r: &Res) -> bool {
    r.backing_pixels() * 60.0 <= crate::ENCODER_PIXEL_RATE
}

fn even(v: f64) -> u16 {
    ((v / 2.0).round() * 2.0) as u16
}

/// Modes for a monitor of `pw` x `ph` pixels: (those matching its shape, common ones).
pub fn candidates(pw: u32, ph: u32) -> (Vec<Res>, Vec<Res>) {
    let mut matching = vec![Res {
        width: pw as u16,
        height: ph as u16,
        hidpi: false,
    }];
    for s in HIDPI_SCALES {
        matching.push(Res {
            width: even(pw as f64 / s),
            height: even(ph as f64 / s),
            hidpi: true,
        });
    }
    matching.retain(fits_encoder);
    matching.dedup();
    let mut common: Vec<Res> = COMMON
        .iter()
        .map(|&(width, height, hidpi)| Res {
            width,
            height,
            hidpi,
        })
        .filter(|r| {
            fits_encoder(r)
                && (r.width as u32) <= pw
                && (r.height as u32) <= ph
                && !matching.contains(r)
        })
        .collect();
    common.dedup();
    (matching, common)
}

/// What vdview picks without a saved choice: the monitor's pixels, HiDPI at >= 175 % scale.
pub fn default_res(pw: u32, ph: u32, scale: f64) -> Res {
    if scale >= 1.75 {
        Res {
            width: even(pw as f64 / 2.0),
            height: even(ph as f64 / 2.0),
            hidpi: true,
        }
    } else {
        Res {
            width: pw as u16,
            height: ph as u16,
            hidpi: false,
        }
    }
}

/// The saved choice: last monitor, and a mode per monitor (by the name the OS gives it).
#[derive(Default, Debug, PartialEq)]
pub struct Saved {
    pub last: Option<String>,
    modes: Vec<(String, Res)>,
}

fn saved_path() -> Option<PathBuf> {
    Some(
        crate::secure::default_secret_path()?
            .parent()?
            .join("display"),
    )
}

impl Saved {
    pub fn load() -> Saved {
        saved_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| Saved::parse(&t))
            .unwrap_or_default()
    }

    fn parse(text: &str) -> Saved {
        let mut s = Saved::default();
        for line in text.lines() {
            let mut f = line.split('\t');
            match (f.next(), f.next(), f.next()) {
                (Some("last"), Some(name), None) => s.last = Some(name.to_string()),
                (Some("mode"), Some(name), Some(res)) => {
                    if let Some(r) = Res::parse(res) {
                        s.set_mode(name, r);
                    }
                }
                _ => {} // comments and anything unknown
            }
        }
        s
    }

    fn to_text(&self) -> String {
        let mut t =
            String::from("# vdview: last display and the mode chosen for each (F6 changes them)\n");
        if let Some(l) = &self.last {
            t += &format!("last\t{l}\n");
        }
        for (name, r) in &self.modes {
            t += &format!("mode\t{name}\t{}\n", r.to_text());
        }
        t
    }

    pub fn mode(&self, monitor: &str) -> Option<Res> {
        self.modes
            .iter()
            .find(|(n, _)| n == monitor)
            .map(|(_, r)| *r)
    }

    fn set_mode(&mut self, monitor: &str, r: Res) {
        match self.modes.iter_mut().find(|(n, _)| n == monitor) {
            Some(m) => m.1 = r,
            None => self.modes.push((monitor.to_string(), r)),
        }
    }

    /// Records a choice and writes the file (a failure is only reported).
    pub fn remember(&mut self, monitor: &str, r: Res) {
        self.last = Some(monitor.to_string());
        self.set_mode(monitor, r);
        let Some(path) = saved_path() else { return };
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&path, self.to_text()));
        if let Err(e) = written {
            eprintln!("cannot save the display choice to {}: {e}", path.display());
        }
    }
}

/// A monitor as the picker shows it.
#[derive(Clone, Debug)]
pub struct Monitor {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
    pub scale: f64,
}

impl Monitor {
    fn label(&self) -> String {
        format!(
            "{}  {}X{} @ {:.0}HZ  SCALE {:.0}%",
            self.name.to_uppercase(),
            self.width,
            self.height,
            self.refresh_mhz as f64 / 1000.0,
            self.scale * 100.0
        )
    }
}

enum Step {
    Monitor,
    Mode {
        matching: Vec<Res>,
        common: Vec<Res>,
    },
}

/// What a key did in the picker.
pub enum Picked {
    /// Still choosing (redraw).
    Open,
    /// Closed without a choice.
    Cancelled,
    /// Monitor (index into the list given) and mode.
    Chosen(usize, Res),
}

pub struct Picker {
    monitors: Vec<Monitor>,
    monitor: usize,
    step: Step,
    /// Highlighted line in the current step.
    at: usize,
    /// Shown at start: closing it means "use what is highlighted".
    at_start: bool,
}

impl Picker {
    /// `current`: the monitor highlighted first, `mode`: the mode highlighted on it (when there
    /// is only one monitor and the picker starts at the modes).
    pub fn new(
        monitors: Vec<Monitor>,
        current: usize,
        mode: Option<Res>,
        at_start: bool,
    ) -> Picker {
        let mut p = Picker {
            monitor: current.min(monitors.len().saturating_sub(1)),
            monitors,
            step: Step::Monitor,
            at: 0,
            at_start,
        };
        p.at = p.monitor;
        if p.monitors.len() < 2 {
            p.enter_mode_step(mode);
        }
        p
    }

    fn enter_mode_step(&mut self, highlight: Option<Res>) {
        let m = &self.monitors[self.monitor];
        let (matching, common) = candidates(m.width, m.height);
        let want = highlight.unwrap_or_else(|| default_res(m.width, m.height, m.scale));
        self.at = matching
            .iter()
            .chain(&common)
            .position(|r| *r == want)
            .unwrap_or(0);
        self.step = Step::Mode { matching, common };
    }

    fn len(&self) -> usize {
        match &self.step {
            Step::Monitor => self.monitors.len(),
            Step::Mode { matching, common } => matching.len() + common.len(),
        }
    }

    /// Handles a key; `mode_of` gives the mode to highlight for a monitor (saved or current).
    pub fn key(&mut self, key: &Key, mode_of: impl Fn(&Monitor) -> Option<Res>) -> Picked {
        let n = self.len();
        match key {
            Key::Named(NamedKey::ArrowUp) => self.at = (self.at + n - 1) % n.max(1),
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::Tab) => {
                self.at = (self.at + 1) % n.max(1)
            }
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::ArrowRight) => match &self.step {
                Step::Monitor => {
                    self.monitor = self.at;
                    let hl = mode_of(&self.monitors[self.monitor]);
                    self.enter_mode_step(hl);
                }
                Step::Mode { matching, common } => {
                    if let Some(r) = matching.iter().chain(common).nth(self.at) {
                        return Picked::Chosen(self.monitor, *r);
                    }
                }
            },
            Key::Named(NamedKey::Escape)
            | Key::Named(NamedKey::ArrowLeft)
            | Key::Named(NamedKey::Backspace) => match self.step {
                Step::Mode { .. } if self.monitors.len() >= 2 => {
                    self.step = Step::Monitor;
                    self.at = self.monitor;
                }
                _ if self.at_start => {
                    // At start there is no "before": go with what is highlighted.
                    let m = &self.monitors[self.monitor];
                    let r = mode_of(m).unwrap_or_else(|| default_res(m.width, m.height, m.scale));
                    return Picked::Chosen(self.monitor, r);
                }
                _ => return Picked::Cancelled,
            },
            _ => {}
        }
        Picked::Open
    }

    /// Status panel lines.
    pub fn lines(&self) -> Vec<String> {
        let mark = |i: usize| if i == self.at { "> " } else { "  " };
        let mut l = Vec::new();
        match &self.step {
            Step::Monitor => {
                l.push("SHOW THE MAC ON WHICH DISPLAY?".to_string());
                for (i, m) in self.monitors.iter().enumerate() {
                    l.push(format!("{}{}", mark(i), m.label()));
                }
                l.push(format!(
                    "UP/DOWN: CHOOSE   ENTER: NEXT   ESC: {}",
                    if self.at_start {
                        "KEEP THIS ONE"
                    } else {
                        "CANCEL"
                    }
                ));
            }
            Step::Mode { matching, common } => {
                l.push(format!(
                    "MAC RESOLUTION ON {}",
                    self.monitors[self.monitor].label()
                ));
                l.push("MATCHING THIS DISPLAY (LARGER TEXT FURTHER DOWN):".to_string());
                for (i, r) in matching.iter().enumerate() {
                    l.push(format!("{}{}", mark(i), r.label()));
                }
                if !common.is_empty() {
                    l.push("COMMON SIZES (BLACK BARS WHERE THE SHAPE DIFFERS):".to_string());
                    for (i, r) in common.iter().enumerate() {
                        l.push(format!("{}{}", mark(matching.len() + i), r.label()));
                    }
                }
                l.push(format!(
                    "UP/DOWN: CHOOSE   ENTER: USE IT   ESC: {}",
                    if self.monitors.len() >= 2 {
                        "BACK"
                    } else if self.at_start {
                        "KEEP THE DEFAULT"
                    } else {
                        "CANCEL"
                    }
                ));
            }
        }
        l
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_respect_the_encoder() {
        // 3440x1440: 1x, HiDPI 2x / 1.75x / 1.5x fit; 1.25x (5504x2304 backing) does not.
        let (m, c) = candidates(3440, 1440);
        let labels: Vec<String> = m.iter().map(Res::label).collect();
        assert_eq!(
            labels,
            [
                "3440X1440",
                "1720X720 HIDPI",
                "1966X822 HIDPI",
                "2294X960 HIDPI"
            ]
        );
        assert!(m.iter().chain(&c).all(fits_encoder));
        // Common sizes fit on the monitor and do not repeat the matching ones.
        assert!(c.iter().all(|r| r.width <= 3440 && r.height <= 1440));
        assert!(!c.contains(&m[0]));
        assert!(c.contains(&Res {
            width: 1920,
            height: 1080,
            hidpi: true
        }));

        // 4K: HiDPI 2x is the monitor's own pixels; 1.5x (5120x2880 backing) is too much.
        let (m, _) = candidates(3840, 2160);
        assert!(m.contains(&Res {
            width: 1920,
            height: 1080,
            hidpi: true
        }));
        assert!(!m.contains(&Res {
            width: 2560,
            height: 1440,
            hidpi: true
        }));
    }

    #[test]
    fn default_follows_the_scale() {
        assert_eq!(default_res(3840, 2160, 2.0).label(), "1920X1080 HIDPI");
        assert_eq!(default_res(3440, 1440, 1.0).label(), "3440X1440");
    }

    #[test]
    fn saved_round_trip() {
        let mut s = Saved {
            last: Some("DP-3".into()),
            ..Default::default()
        };
        s.set_mode(
            "DP-3",
            Res {
                width: 1720,
                height: 720,
                hidpi: true,
            },
        );
        s.set_mode(
            r"\\.\DISPLAY1",
            Res {
                width: 2560,
                height: 1440,
                hidpi: false,
            },
        );
        s.set_mode(
            "DP-3",
            Res {
                width: 3440,
                height: 1440,
                hidpi: false,
            },
        ); // replaces
        let back = Saved::parse(&s.to_text());
        assert_eq!(back, s);
        assert_eq!(back.mode("DP-3").unwrap().label(), "3440X1440");
        assert_eq!(Saved::parse("junk\nmode\tX\tbad\n"), Saved::default());
    }

    fn monitors() -> Vec<Monitor> {
        let m = |name: &str, w, h, scale| Monitor {
            name: name.into(),
            width: w,
            height: h,
            refresh_mhz: 60_000,
            scale,
        };
        vec![m("DP-3", 3440, 1440, 1.0), m("HDMI-1", 3840, 2160, 2.0)]
    }

    #[test]
    fn picker_steps() {
        let down = Key::Named(NamedKey::ArrowDown);
        let enter = Key::Named(NamedKey::Enter);
        let esc = Key::Named(NamedKey::Escape);
        let none = |_: &Monitor| None;

        // Start: monitor step, then the mode step highlights the default for that monitor.
        let mut p = Picker::new(monitors(), 0, None, true);
        assert!(p.lines()[1].starts_with("> DP-3"));
        p.key(&down, none);
        p.key(&enter, none);
        assert!(p.lines().iter().any(|l| l == "> 1920X1080 HIDPI"));
        // Esc goes back to the monitors; Esc there (at start) takes the highlighted one.
        p.key(&esc, none);
        assert!(matches!(
            p.key(&esc, none),
            Picked::Chosen(
                1,
                Res {
                    width: 1920,
                    height: 1080,
                    hidpi: true
                }
            )
        ));

        // F6 with one monitor: straight to the modes, current mode highlighted; Esc cancels,
        // Down + Enter picks the next one.
        let current = Res {
            width: 1720,
            height: 720,
            hidpi: true,
        };
        let mut p = Picker::new(monitors()[..1].to_vec(), 0, Some(current), false);
        assert!(p.lines()[0].starts_with("MAC RESOLUTION"));
        assert!(p.lines().iter().any(|l| l == "> 1720X720 HIDPI"));
        assert!(matches!(p.key(&esc, none), Picked::Cancelled));
        p.key(&down, none);
        assert!(matches!(
            p.key(&enter, none),
            Picked::Chosen(
                0,
                Res {
                    width: 1966,
                    height: 822,
                    hidpi: true
                }
            )
        ));
    }
}
