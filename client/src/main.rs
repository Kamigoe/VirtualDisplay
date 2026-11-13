//! vdview: shows a VirtualDisplay stream from a Mac running `vdsend`.

mod clock;
mod control;
mod decoder;
mod input;
mod osd;
mod protocol;
mod receiver;
mod render;
mod secure;
mod stats;

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use winit::monitor::MonitorHandle;
use winit::window::{Fullscreen, Window, WindowId};

use crate::clock::now_us;
use crate::control::{Closed, Control, InputLink};
use crate::decoder::Decoder;
use crate::input::{MacKey, Modifiers, Role};
use crate::protocol::{Hello, Start};
use crate::render::Renderer;
use crate::stats::Stats;

const USAGE: &str = "\
usage: vdview <mac-host>[:port] [options]
  --mode WxH@HZ         request this logical mode (default: match this PC's primary monitor)
  --hidpi / --no-hidpi  2x backing store on the Mac (default: on when the monitor scale is >= 175%)
  --server-mode         keep whatever mode the Mac is already using
  --chroma 420|444      chroma subsampling (default 444: sharp small text/UI, 8-bit only, decoded
                        on the CPU on AMD GPUs; 420 allows 10-bit for smoother gradients)
  --bit-depth 8|10      sample bit depth for 4:2:0 (default 10; reduces banding in gradients)
  --refresh HZ|max      refresh rate when matching the monitor (default 60; max = highest the
                        monitor and the Mac allow, at most 120 Hz)
  --bitrate MBPS        average bitrate (default: sender's)
  --hwaccel NAME        auto | none | d3d11va | vaapi | vulkan | videotoolbox (default auto)
  --threads N           software decoding threads (default 0 = auto)
  --monitor N           show on monitor N (see the list printed at startup; default: primary)
  --windowed            start in a window instead of borderless fullscreen (F11 toggles)
  --vsync               present with FIFO (no tearing, +latency) instead of mailbox/immediate

input (keyboard and mouse to the Mac; needs the same secret on both sides, see README):
  --input-secret PATH   secret file (default: %APPDATA%\\vdview\\input-secret on Windows,
                        ~/.config/vdview/input-secret elsewhere)
  --map-ctrl ROLE       what Ctrl is on the Mac: cmd | ctrl | alt | shift | none (default cmd)
  --map-alt ROLE        what Alt is on the Mac (default alt = Option)
  --map-win ROLE        what the Windows key is on the Mac (default ctrl)
  vdview --set-input-secret [--input-secret PATH]
      read the secret (64 hex digits, as printed by make-input-secret.sh) from standard input
      and save it

keys while running:
  F8  input to the Mac on/off (starts on). While on, every other key goes to the Mac, so the
      keys below work only while it is off.
  F2  8-bit <-> 10-bit      F3  4:2:0 <-> 4:4:4      F4  cycle refresh rate (max / 120 / 60 Hz)
  F5  HiDPI (2x) <-> 1x     F11 fullscreen on/off     Esc leave fullscreen

self-test (no sender or display needed):
  vdview --snapshot IN.hevc OUT.ppm [--hwaccel NAME]
      decode the first picture of an Annex-B file and write what the renderer draws
  vdview --decode-test IN.hevc [--hwaccel NAME]
      decode a whole Annex-B stream picture by picture, as the live path does";

/// The M1's HEVC encoder sustains roughly this many pixels per second.
const ENCODER_PIXEL_RATE: f64 = 650e6;
/// Highest refresh rate requested when matching the monitor (mHz). macOS accepts up to 240 Hz for
/// the virtual display, but ScreenCaptureKit delivered ~110 fps at 120-180 Hz and only ~55 fps at
/// 240 Hz (FHD, 2026-11-12), so asking for more than 120 Hz gains nothing or makes it worse.
/// `--mode` still requests any rate, for experiments.
const MAX_REFRESH_MHZ: u32 = 120_000;

struct Args {
    server: String,
    mode: Option<(u16, u16, u32)>,
    /// Requested refresh in mHz; None = the highest the monitor and encoder allow.
    refresh: Option<u32>,
    hidpi: Option<bool>,
    server_mode: bool,
    chroma444: bool,
    bit_depth: u8,
    bitrate_kbps: u32,
    hwaccel: String,
    threads: i32,
    monitor: Option<usize>,
    windowed: bool,
    vsync: bool,
    input_secret: Option<PathBuf>,
    modifiers: Modifiers,
}

impl Args {
    fn parse() -> Result<Args> {
        let mut it = std::env::args().skip(1);
        let mut a = Args {
            server: String::new(),
            mode: None,
            refresh: Some(60_000),
            hidpi: None,
            server_mode: false,
            chroma444: true,
            bit_depth: 10,
            bitrate_kbps: 0,
            hwaccel: "auto".into(),
            threads: 0,
            monitor: None,
            windowed: false,
            vsync: false,
            input_secret: None,
            modifiers: Modifiers::default(),
        };
        while let Some(arg) = it.next() {
            let mut value = || it.next().ok_or_else(|| anyhow!("{arg} needs a value"));
            match arg.as_str() {
                "--mode" => {
                    let v = value()?;
                    let (wh, hz) = v.split_once('@').unwrap_or((&v, "60"));
                    let (w, h) = wh
                        .split_once('x')
                        .ok_or_else(|| anyhow!("bad --mode {v}"))?;
                    let hz: f64 = hz.parse()?;
                    a.mode = Some((w.parse()?, h.parse()?, (hz * 1000.0).round() as u32));
                }
                "--refresh" => {
                    let v = value()?;
                    a.refresh = if v == "max" {
                        None
                    } else {
                        Some((v.parse::<f64>()? * 1000.0).round() as u32)
                    };
                }
                "--hidpi" => a.hidpi = Some(true),
                "--no-hidpi" => a.hidpi = Some(false),
                "--server-mode" => a.server_mode = true,
                "--chroma" => {
                    a.chroma444 = match value()?.as_str() {
                        "420" => false,
                        "444" => true,
                        other => bail!("--chroma must be 420 or 444, not {other}"),
                    }
                }
                "--bit-depth" => {
                    a.bit_depth = match value()?.as_str() {
                        "8" => 8,
                        "10" => 10,
                        other => bail!("--bit-depth must be 8 or 10, not {other}"),
                    }
                }
                "--bitrate" => a.bitrate_kbps = (value()?.parse::<f64>()? * 1000.0) as u32,
                "--hwaccel" => a.hwaccel = value()?,
                "--threads" => a.threads = value()?.parse()?,
                "--monitor" => a.monitor = Some(value()?.parse()?),
                "--windowed" => a.windowed = true,
                "--vsync" => a.vsync = true,
                "--input-secret" => a.input_secret = Some(value()?.into()),
                "--map-ctrl" => a.modifiers.ctrl = Role::parse(&value()?)?,
                "--map-alt" => a.modifiers.alt = Role::parse(&value()?)?,
                "--map-win" => a.modifiers.win = Role::parse(&value()?)?,
                "-h" | "--help" => {
                    println!("{USAGE}");
                    std::process::exit(0);
                }
                s if s.starts_with('-') => bail!("unknown option {s}"),
                s if a.server.is_empty() => a.server = s.to_string(),
                s => bail!("unexpected argument {s}"),
            }
        }
        if a.server.is_empty() {
            bail!("missing <mac-host>");
        }
        Ok(a)
    }

    fn server_addr(&self) -> Result<SocketAddr> {
        let with_port = if self.server.contains(':')
            && !self.server.starts_with('[')
            && self.server.matches(':').count() == 1
            || self.server.contains("]:")
        {
            self.server.clone()
        } else {
            format!("{}:{}", self.server, protocol::DEFAULT_PORT)
        };
        let addrs: Vec<SocketAddr> = with_port
            .to_socket_addrs()
            .with_context(|| format!("resolving {with_port}"))?
            .collect();
        addrs
            .iter()
            .find(|a| a.is_ipv4())
            .or(addrs.first())
            .copied()
            .ok_or_else(|| anyhow!("no address for {with_port}"))
    }

    /// Mode request: explicit `--mode`, else mirror the local monitor (clamped to the encoder's capacity).
    /// Returns the request plus the highest refresh rate (mHz) F4 may switch to.
    fn hello(&self, monitor: Option<&MonitorHandle>) -> (Hello, u32) {
        let mut h = Hello {
            width: 0,
            height: 0,
            hidpi: false,
            refresh_mhz: 60_000,
            chroma444: self.chroma444,
            bitrate_kbps: self.bitrate_kbps,
            bit_depth: self.bit_depth,
        };
        if self.server_mode {
            return (h, h.refresh_mhz);
        }
        let (pw, ph, scale, mhz) = monitor
            .map(|m| {
                (
                    m.size().width,
                    m.size().height,
                    m.scale_factor(),
                    m.refresh_rate_millihertz().unwrap_or(60_000),
                )
            })
            .unwrap_or((1920, 1080, 1.0, 60_000));
        h.hidpi = self.hidpi.unwrap_or(scale >= 1.75);
        if let Some((w, hh, hz)) = self.mode {
            (h.width, h.height, h.refresh_mhz) = (w, hh, hz);
            return (h, hz);
        }
        let div = if h.hidpi { 2 } else { 1 };
        h.width = (pw / div) as u16;
        h.height = (ph / div) as u16;
        let pixels = (pw * ph) as f64;
        let max = [mhz, 120_000, 100_000, 60_000]
            .into_iter()
            .find(|&r| {
                r <= mhz && r <= MAX_REFRESH_MHZ && pixels * r as f64 / 1000.0 <= ENCODER_PIXEL_RATE
            })
            .unwrap_or(60_000);
        if max != mhz {
            println!(
                "note: monitor runs at {:.0} Hz; requesting at most {:.0} Hz for {pw}x{ph} \
                 (Mac capture/encoder limits)",
                mhz as f64 / 1000.0,
                max as f64 / 1000.0
            );
        }
        h.refresh_mhz = self.refresh.map_or(max, |r| r.min(max));
        (h, max)
    }
}

enum UserEvent {
    FrameReady,
    /// A connection attempt (generation `gen`) succeeded.
    Connected(u64, Arc<Control>, Start),
    /// A connection attempt failed; the connector retries by itself.
    ConnectFailed(u64, String),
    /// The sender answered a reconfiguration with a new stream.
    Restarted(u64, Start),
    /// The connection of generation `gen` ended.
    Closed(u64, Closed),
    /// The input channel of generation `gen` changed state.
    Input(u64, InputLink),
}

fn describe_start(s: &Start) -> String {
    format!(
        "{}x{} @ {:.2} Hz, HEVC {} {}-bit",
        s.pixel_width,
        s.pixel_height,
        s.refresh_mhz as f64 / 1000.0,
        if s.chroma444 { "4:4:4" } else { "4:2:0" },
        s.bit_depth
    )
}

const OSD_SHOW: Duration = Duration::from_secs(5);
const RECONNECT_EVERY: Duration = Duration::from_secs(2);
const STATS_EVERY: Duration = Duration::from_secs(1);
/// A sender that has not answered the input handshake by then is too old to know it.
const INPUT_REPLY_WITHIN: Duration = Duration::from_secs(3);

/// The window and renderer live for the whole run; connections come and go underneath them.
struct Running {
    window: Arc<Window>,
    renderer: Renderer,
    uploader: render::Uploader,
    proxy: EventLoopProxy<UserEvent>,
    addr: SocketAddr,
    hwaccel: String,
    threads: i32,
    stats: Arc<Stats>,
    next_stats: Instant,
    /// What we last asked for; F2-F4 edit it and re-send it (and reconnects reuse it).
    hello: Hello,
    /// Refresh rates F4 cycles through (mHz, highest first).
    refresh_choices: Vec<u32>,

    /// Connection generation: events from older connections are ignored.
    generation: Arc<AtomicU64>,
    control: Option<Arc<Control>>,
    start: Option<Start>,
    /// Stops the receiver (and so the decoder) of the current connection.
    session_stop: Arc<AtomicBool>,
    connect_error: Option<String>,
    connect_attempts: u32,
    /// Set when the sender switched to another viewer: no automatic reconnect (two viewers would
    /// keep taking the sender from each other); Enter reconnects.
    replaced_by: Option<String>,

    /// Input secret, or why there is none (input then stays off).
    secret: Result<[u8; secure::SECRET_LEN], String>,
    modifiers: Modifiers,
    /// F8: forward keyboard and mouse to the Mac.
    input_on: bool,
    /// State of this connection's input channel; None until the sender answers.
    input_link: Option<InputLink>,
    /// Latest pointer position (window pixels) not yet sent.
    pointer: Option<(f64, f64)>,
    connected_at: Instant,
    /// The console already said that the sender did not answer the input handshake.
    input_silence_noted: bool,

    /// Status panel: shown while pinned (F1), until `osd_until`, or while a switch is pending.
    osd_pinned: bool,
    osd_until: Option<Instant>,
    switching: bool,
    summary: stats::Summary,
}

impl Running {
    /// Starts (re)connecting in the background until it succeeds or a newer attempt replaces it.
    fn connect(&mut self) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let (proxy, addr, hello, current, secret) = (
            self.proxy.clone(),
            self.addr,
            self.hello,
            self.generation.clone(),
            self.secret.clone().ok(),
        );
        self.connect_attempts = 0;
        let spawned = thread::Builder::new()
            .name("connect".into())
            .spawn(move || {
                while current.load(Ordering::SeqCst) == generation {
                    let (restart_proxy, input_proxy, close_proxy) =
                        (proxy.clone(), proxy.clone(), proxy.clone());
                    match Control::connect(
                        addr,
                        &hello,
                        secret,
                        move |s| {
                            let _ = restart_proxy.send_event(UserEvent::Restarted(generation, s));
                        },
                        move |link| {
                            let _ = input_proxy.send_event(UserEvent::Input(generation, link));
                        },
                        move |reason| {
                            let _ = close_proxy.send_event(UserEvent::Closed(generation, reason));
                        },
                    ) {
                        Ok((control, start)) => {
                            let _ =
                                proxy.send_event(UserEvent::Connected(generation, control, start));
                            return;
                        }
                        Err(e) => {
                            if proxy
                                .send_event(UserEvent::ConnectFailed(generation, format!("{e:#}")))
                                .is_err()
                            {
                                return; // event loop gone
                            }
                            thread::sleep(RECONNECT_EVERY);
                        }
                    }
                }
            });
        if let Err(e) = spawned {
            eprintln!("cannot start connector: {e}");
        }
        self.update_osd();
    }

    /// Spawns the receiver and decoder for a fresh connection.
    fn begin_session(&mut self, control: Arc<Control>, start: Start) -> Result<()> {
        self.session_stop.store(true, Ordering::Relaxed);
        self.session_stop = Arc::new(AtomicBool::new(false));
        // When a stalled frame is finally repaired, every frame queued behind it is released at once;
        // decoding them (~6 ms each) catches up faster than resyncing on a large keyframe over Wi-Fi.
        let (tx, rx) = sync_channel(64);
        receiver::spawn(
            self.addr,
            control.clone(),
            self.stats.clone(),
            tx,
            self.session_stop.clone(),
        )?;
        spawn_decoder(
            rx,
            self.hwaccel.clone(),
            self.threads,
            &start,
            control.clone(),
            self.uploader.clone(),
            self.stats.clone(),
            self.proxy.clone(),
        )?;
        self.control = Some(control);
        self.start = Some(start);
        self.connect_error = None;
        self.input_link = None;
        self.connected_at = Instant::now();
        self.input_silence_noted = false;
        Ok(())
    }

    fn end_session(&mut self) {
        self.release_all(); // the sender also releases everything when the link drops
        self.session_stop.store(true, Ordering::Relaxed);
        if let Some(c) = self.control.take() {
            c.shutdown();
        }
        self.input_link = None;
    }

    /// Keyboard and mouse currently go to the Mac.
    fn forwarding(&self) -> bool {
        self.input_on
            && self.control.is_some()
            && matches!(self.input_link, Some(InputLink::Ready { .. }))
    }

    fn send_input(&self, inner: u8, body: &[u8]) {
        if let Some(c) = &self.control {
            c.send_input(inner, body);
        }
    }

    /// Lets go of every key and button on the Mac (forwarding stopped or focus lost).
    fn release_all(&self) {
        if self.forwarding() {
            self.send_input(protocol::INPUT_RELEASE_ALL, &[]);
        }
    }

    /// Sends the latest pointer position, if it changed.
    fn flush_pointer(&mut self) {
        let Some(pos) = self.pointer.take() else {
            return;
        };
        let Some(s) = &self.start else { return };
        let size = self.window.inner_size();
        if let Some((x, y)) = input::pointer_position(
            pos,
            (size.width, size.height),
            (s.pixel_width, s.pixel_height),
        ) {
            let (x, y) = (x.to_be_bytes(), y.to_be_bytes());
            self.send_input(protocol::INPUT_POINTER, &[x[0], x[1], y[0], y[1]]);
        }
    }

    fn toggle_input(&mut self) {
        if self.input_on {
            self.release_all();
        }
        self.input_on = !self.input_on;
        println!("input to the Mac: {}", self.input_label());
        self.flash_osd();
    }

    /// Short state of input forwarding, for the title and the console.
    fn input_label(&self) -> String {
        match (&self.secret, &self.input_link) {
            (Err(_), _) => "off (no input secret)".into(),
            _ if self.control.is_none() => "not connected".into(),
            (_, None) if self.connected_at.elapsed() < INPUT_REPLY_WITHIN => {
                "authenticating".into()
            }
            (_, None) => "no reply (vdsend too old?)".into(),
            (_, Some(InputLink::Failed(why))) => format!("failed: {why}"),
            (_, Some(InputLink::Ready { permitted: false })) => {
                "the Mac needs the accessibility permission for vdsend".into()
            }
            (_, Some(InputLink::Ready { .. })) if self.input_on => "on (F8 turns it off)".into(),
            (_, Some(InputLink::Ready { .. })) => "off (F8 turns it on)".into(),
        }
    }

    fn reconfigure(&mut self, what: &str) {
        println!(
            "requesting {what}: {}x{}{} @ {:.0} Hz, {} {}-bit",
            self.hello.width,
            self.hello.height,
            if self.hello.hidpi { " HiDPI" } else { "" },
            self.hello.refresh_mhz as f64 / 1000.0,
            if self.hello.chroma444 {
                "4:4:4"
            } else {
                "4:2:0"
            },
            if self.hello.bit_depth == 10 { 10 } else { 8 }
        );
        match &self.control {
            Some(c) => {
                c.reconfigure(&self.hello);
                self.switching = true;
            }
            // Not connected: the next connection attempt uses the new settings.
            None if self.replaced_by.is_none() => self.connect(),
            None => {} // taken over: wait for Enter
        }
        self.flash_osd();
    }

    fn flash_osd(&mut self) {
        self.osd_until = Some(Instant::now() + OSD_SHOW);
        self.update_osd();
    }

    fn osd_visible(&self) -> bool {
        self.osd_pinned
            || self.switching
            || self.control.is_none()
            || self.osd_until.is_some_and(|t| Instant::now() < t)
    }

    /// Re-renders (or hides) the status panel and redraws so it shows even on a static screen.
    fn update_osd(&mut self) {
        let keys = if self.forwarding() {
            "F8 INPUT OFF (THEN F1-F5 AND F11 CONTROL VDVIEW)".to_string()
        } else {
            format!(
                "F1 {}  F2 BIT DEPTH  F3 CHROMA  F4 REFRESH  F5 HIDPI  F8 INPUT  F11 FULLSCREEN",
                if self.osd_pinned { "HIDE" } else { "PIN" }
            )
        };
        let input_line = format!("INPUT {}", self.input_label().to_uppercase())
            .chars()
            .take(90)
            .collect::<String>();
        let lines = if !self.osd_visible() {
            vec![]
        } else if let Some(by) = &self.replaced_by {
            vec![
                format!("TAKEN OVER BY ANOTHER VIEWER ({})", by.to_uppercase()),
                "PRESS ENTER TO RECONNECT (TAKES THE MAC BACK)".to_string(),
                keys,
            ]
        } else if self.control.is_none() {
            let mut l = vec![format!(
                "{} {}...   ATTEMPT {}",
                if self.start.is_some() {
                    "RECONNECTING TO"
                } else {
                    "CONNECTING TO"
                },
                self.addr,
                self.connect_attempts.max(1)
            )];
            if let Some(e) = &self.connect_error {
                l.push(e.chars().take(90).collect());
            }
            l.push(keys);
            l
        } else {
            let s = self.start.as_ref().unwrap();
            let ms = |v: Option<f64>| v.map_or("-".to_string(), |v| format!("{v:.1}MS"));
            vec![
                format!(
                    "{}X{} @ {:.0}HZ   {}  {}-BIT{}",
                    s.pixel_width,
                    s.pixel_height,
                    s.refresh_mhz as f64 / 1000.0,
                    if s.chroma444 { "4:4:4" } else { "4:2:0" },
                    s.bit_depth,
                    if self.switching {
                        "   SWITCHING..."
                    } else {
                        ""
                    }
                ),
                format!(
                    "{:.0} FPS   LATENCY {}   DECODE {} {}   {:.1} MBPS{}",
                    self.summary.fps,
                    ms(self.summary.latency_ms),
                    ms(self.summary.decode_ms),
                    self.summary.decoder,
                    self.summary.mbps,
                    if self.summary.lost > 0 {
                        format!("   LOST {}", self.summary.lost)
                    } else {
                        String::new()
                    }
                ),
                input_line,
                keys,
            ]
        };
        let scale = (3.0 * self.window.scale_factor()).round() as usize;
        self.renderer.set_osd(lines, scale);
        self.window.request_redraw();
    }

    /// Sends a key press or release (with the OS's auto-repeat) to the Mac.
    fn forward_key(&self, e: &KeyEvent) {
        let PhysicalKey::Code(code) = e.physical_key else {
            return;
        };
        let down = e.state == ElementState::Pressed;
        match input::mac_key(code, &self.modifiers) {
            Some(MacKey::Code(c)) => {
                self.send_input(protocol::INPUT_KEY, &input::key_body(down, c, e.repeat))
            }
            Some(MacKey::ImeToggle) if down && !e.repeat => {
                self.send_input(protocol::INPUT_IME_TOGGLE, &[])
            }
            _ => {}
        }
    }

    /// vdview's own keys (only while input forwarding is off).
    fn key_command(&mut self, key: Key) {
        let r = self;
        match key {
            Key::Named(NamedKey::F11) => {
                let full = r.window.fullscreen().is_none();
                r.window.set_fullscreen(
                    full.then(|| Fullscreen::Borderless(r.window.current_monitor())),
                );
                r.window.set_cursor_visible(!full);
            }
            Key::Named(NamedKey::Escape) if r.window.fullscreen().is_some() => {
                r.window.set_fullscreen(None);
                r.window.set_cursor_visible(true);
            }
            Key::Named(NamedKey::Enter) if r.replaced_by.is_some() => {
                r.replaced_by = None;
                println!("reconnecting (taking the sender back)");
                r.connect();
            }
            Key::Named(NamedKey::F1) => {
                r.osd_pinned = !r.osd_pinned;
                r.osd_until = None;
                r.update_osd();
            }
            Key::Named(NamedKey::F2) => {
                if r.hello.chroma444 {
                    println!("4:4:4 is 8-bit only on the Mac's encoder");
                } else {
                    r.hello.bit_depth = if r.hello.bit_depth == 10 { 8 } else { 10 };
                    r.reconfigure("bit depth");
                }
            }
            Key::Named(NamedKey::F3) => {
                r.hello.chroma444 = !r.hello.chroma444;
                r.reconfigure("chroma");
            }
            Key::Named(NamedKey::F4) => {
                if r.hello.width == 0 {
                    println!("refresh switching needs a requested mode (not --server-mode)");
                } else if r.refresh_choices.len() > 1 {
                    let i = r
                        .refresh_choices
                        .iter()
                        .position(|&c| c == r.hello.refresh_mhz)
                        .unwrap_or(0);
                    r.hello.refresh_mhz = r.refresh_choices[(i + 1) % r.refresh_choices.len()];
                    r.reconfigure("refresh rate");
                }
            }
            // Same backing pixels either way (e.g. 3840x2160 1x <-> 1920x1080 HiDPI), so the
            // pixel rate and the refresh choices stay valid.
            Key::Named(NamedKey::F5) => {
                let h = &mut r.hello;
                if h.width == 0 {
                    println!("HiDPI switching needs a requested mode (not --server-mode)");
                } else if h.hidpi {
                    (h.width, h.height, h.hidpi) = (h.width * 2, h.height * 2, false);
                    r.reconfigure("1x");
                } else if h.width % 2 == 0 && h.height % 2 == 0 && h.width >= 640 && h.height >= 480
                {
                    (h.width, h.height, h.hidpi) = (h.width / 2, h.height / 2, true);
                    r.reconfigure("HiDPI");
                } else {
                    println!(
                        "{}x{} is too small or odd-sized for HiDPI",
                        h.width, h.height
                    );
                }
            }
            _ => {}
        }
    }
}

/// Decode thread for one connection; ends when the receiver drops its sender.
#[allow(clippy::too_many_arguments)]
fn spawn_decoder(
    rx: std::sync::mpsc::Receiver<receiver::EncodedFrame>,
    hwaccel: String,
    threads: i32,
    start: &Start,
    control: Arc<Control>,
    uploader: render::Uploader,
    stats: Arc<Stats>,
    proxy: EventLoopProxy<UserEvent>,
) -> Result<()> {
    // A fresh decoder per stream format: FFmpeg 6.1 crashes when a running VA-API decoder falls
    // back to software mid-stream (4:2:0 -> 4:4:4 on AMD), and 4:4:4 never decodes on VA-API
    // anyway, so open it in software directly.
    let open_for = move |s: &Start| {
        let hw = if s.chroma444 && hwaccel == "auto" {
            "none"
        } else {
            hwaccel.as_str()
        };
        Decoder::open(hw, threads)
    };
    let mut decoder = open_for(start)?;
    let mut decoder_session = start.session_id;
    thread::Builder::new()
        .name("decode".into())
        .spawn(move || {
            // Last line of defence for the reference chain: never hand the decoder a picture whose
            // predecessor it has not seen. Mesa's VA-API driver segfaults on missing references (seen
            // on RX 9070 XT / Mesa 25.2.8), so after any gap or decode error everything is dropped
            // until the next keyframe.
            let mut expected: Option<u32> = None;
            let mut last_request = Instant::now() - Duration::from_secs(1);
            for frame in rx {
                if frame.session != decoder_session {
                    let start = *control.current.lock().unwrap();
                    match open_for(&start) {
                        Ok(d) => decoder = d,
                        Err(e) => eprintln!("{e:#}"),
                    }
                    decoder_session = frame.session;
                    expected = None;
                }
                let in_sequence = frame.keyframe || expected == Some(frame.frame_id);
                if !in_sequence {
                    if let Some(e) = expected.take() {
                        eprintln!(
                            "decoder: expected frame {e} but got {}; dropping until a keyframe",
                            frame.frame_id
                        );
                    }
                    if last_request.elapsed() >= Duration::from_millis(250) {
                        control.request_keyframe();
                        last_request = Instant::now();
                    }
                    continue;
                }
                expected = Some(frame.frame_id.wrapping_add(1));
                let t0 = Instant::now();
                match decoder.decode(&frame.data) {
                    Ok(Some(pic)) => {
                        uploader.upload(
                            &pic,
                            render::FrameTimes {
                                capture_us: frame.capture_us,
                                received_us: frame.received_us,
                                ready_us: 0,
                            },
                        );
                        stats.frame_decoded(t0.elapsed().as_micros() as u32, &decoder.describe());
                        let _ = proxy.send_event(UserEvent::FrameReady);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        eprintln!("{e:#}; dropping until a keyframe");
                        expected = None;
                        control.request_keyframe();
                        last_request = Instant::now();
                    }
                }
            }
        })?;
    Ok(())
}

struct App {
    args: Args,
    proxy: EventLoopProxy<UserEvent>,
    running: Option<Running>,
    failed: bool,
}

impl App {
    fn launch(&mut self, el: &ActiveEventLoop) -> Result<Running> {
        let monitors: Vec<MonitorHandle> = el.available_monitors().collect();
        for (i, m) in monitors.iter().enumerate() {
            println!(
                "monitor {i}: {} {}x{} @ {:.2} Hz, scale {:.2}",
                m.name().unwrap_or_default(),
                m.size().width,
                m.size().height,
                m.refresh_rate_millihertz().unwrap_or(0) as f64 / 1000.0,
                m.scale_factor()
            );
        }
        let monitor = match self.args.monitor {
            Some(i) => Some(
                monitors
                    .get(i)
                    .cloned()
                    .ok_or_else(|| anyhow!("no monitor {i}"))?,
            ),
            None => el.primary_monitor().or_else(|| monitors.first().cloned()),
        };
        let (hello, max_refresh) = self.args.hello(monitor.as_ref());
        let addr = self.args.server_addr()?;
        let secret = match self
            .args
            .input_secret
            .clone()
            .or_else(secure::default_secret_path)
        {
            None => Err("no place for the input secret (set --input-secret)".to_string()),
            Some(p) if !p.exists() && self.args.input_secret.is_none() => {
                Err(format!("no input secret at {}", p.display()))
            }
            Some(p) => secure::load_secret(&p).map_err(|e| format!("{e:#}")),
        };
        if let Err(why) = &secret {
            println!("note: keyboard and mouse stay on this PC: {why} (see vdview --help)");
        }
        println!(
            "sender {addr}: requesting {}",
            if hello.width == 0 {
                "the sender's current mode".to_string()
            } else {
                format!(
                    "{}x{}{} @ {:.2} Hz, chroma {}",
                    hello.width,
                    hello.height,
                    if hello.hidpi { " HiDPI" } else { "" },
                    hello.refresh_mhz as f64 / 1000.0,
                    if hello.chroma444 { "4:4:4" } else { "4:2:0" }
                )
            }
        );

        let mut attrs = Window::default_attributes().with_title("vdview");
        if self.args.windowed {
            let (pw, ph) = if hello.width == 0 {
                (1280, 720)
            } else {
                let k = if hello.hidpi { 2 } else { 1 };
                (hello.width as u32 * k, hello.height as u32 * k)
            };
            let scale = monitor.as_ref().map_or(1.0, |m| m.scale_factor());
            attrs = attrs.with_inner_size(winit::dpi::LogicalSize::new(
                pw as f64 / scale,
                ph as f64 / scale,
            ));
        } else {
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(monitor.clone())));
        }
        let window = Arc::new(el.create_window(attrs)?);
        window.set_cursor_visible(self.args.windowed); // the Mac's cursor is part of the picture
        let (renderer, uploader) = Renderer::new(window.clone(), self.args.vsync)?;

        let mut refresh_choices = vec![max_refresh, 120_000, 60_000, hello.refresh_mhz];
        refresh_choices.retain(|&r| r <= max_refresh);
        refresh_choices.sort_unstable_by(|a, b| b.cmp(a));
        refresh_choices.dedup();
        let mut r = Running {
            window,
            renderer,
            uploader,
            proxy: self.proxy.clone(),
            addr,
            hwaccel: self.args.hwaccel.clone(),
            threads: self.args.threads,
            stats: Arc::new(Stats::default()),
            next_stats: Instant::now() + STATS_EVERY,
            hello,
            refresh_choices,
            generation: Arc::new(AtomicU64::new(0)),
            control: None,
            start: None,
            session_stop: Arc::new(AtomicBool::new(false)),
            connect_error: None,
            connect_attempts: 0,
            replaced_by: None,
            secret,
            modifiers: self.args.modifiers,
            input_on: true,
            input_link: None,
            pointer: None,
            connected_at: Instant::now(),
            input_silence_noted: false,
            osd_pinned: false,
            osd_until: Some(Instant::now() + OSD_SHOW),
            switching: false,
            summary: Default::default(),
        };
        r.connect();
        Ok(r)
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.running.is_some() {
            return;
        }
        match self.launch(el) {
            Ok(r) => self.running = Some(r),
            Err(e) => {
                eprintln!("error: {e:#}");
                self.failed = true;
                el.exit();
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(r) = self.running.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => {
                r.renderer.resize(size.width, size.height);
                r.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if let Some(t) = r.renderer.render() {
                    r.stats.frame_presented();
                    if let Some(c) = &r.control {
                        let clock = c.clock.lock().unwrap();
                        if let Some(local) = clock.server_to_client(t.capture_us) {
                            let now = now_us();
                            r.stats.latency(now.saturating_sub(local) as u32);
                            r.stats.stages(
                                t.received_us.saturating_sub(local) as u32,
                                t.ready_us.saturating_sub(t.received_us) as u32,
                                now.saturating_sub(t.ready_us) as u32,
                            );
                        }
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => r.pointer = Some((position.x, position.y)),
            WindowEvent::MouseInput { state, button, .. } if r.forwarding() => {
                if let Some(b) = input::button_index(button) {
                    r.flush_pointer();
                    let down = state == ElementState::Pressed;
                    r.send_input(protocol::INPUT_BUTTON, &[b, down as u8]);
                }
            }
            WindowEvent::MouseWheel { delta, .. } if r.forwarding() => {
                if let Some(body) = input::scroll_body(delta) {
                    r.flush_pointer();
                    r.send_input(protocol::INPUT_SCROLL, &body);
                }
            }
            WindowEvent::Focused(false) => r.release_all(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.physical_key == PhysicalKey::Code(KeyCode::F8) {
                    if event.state == ElementState::Pressed && !event.repeat {
                        r.toggle_input();
                    }
                    return;
                }
                if r.forwarding() {
                    r.forward_key(&event);
                    return;
                }
                if event.state != ElementState::Pressed || event.repeat {
                    return;
                }
                r.key_command(event.logical_key);
            }
            _ => {}
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, event: UserEvent) {
        let Some(r) = self.running.as_mut() else {
            return;
        };
        let current = r.generation.load(Ordering::SeqCst);
        match event {
            UserEvent::FrameReady => r.window.request_redraw(),
            UserEvent::Connected(g, control, start) if g == current => {
                println!("connected; stream: {}", describe_start(&start));
                if let Err(e) = r.begin_session(control.clone(), start) {
                    eprintln!("cannot start session: {e:#}");
                    control.shutdown();
                    r.connect();
                    return;
                }
                r.switching = false;
                r.flash_osd();
            }
            UserEvent::Connected(_, control, _) => control.shutdown(), // superseded attempt
            UserEvent::ConnectFailed(g, reason) if g == current => {
                r.connect_attempts += 1;
                if r.connect_attempts == 1 || r.connect_attempts % 15 == 0 {
                    eprintln!("connect failed (attempt {}): {reason}", r.connect_attempts);
                }
                r.connect_error = Some(reason);
                r.update_osd();
            }
            UserEvent::Restarted(g, start) if g == current => {
                println!("stream: {}", describe_start(&start));
                r.start = Some(start);
                r.switching = false;
                r.flash_osd();
            }
            UserEvent::Closed(g, Closed::Lost(reason)) if g == current => {
                eprintln!("connection lost: {reason}; reconnecting");
                r.end_session();
                r.switching = false;
                r.connect();
            }
            UserEvent::Closed(g, Closed::Replaced(by)) if g == current => {
                eprintln!("the sender switched to another viewer ({by}); press Enter to reconnect");
                r.end_session();
                r.switching = false;
                r.window.set_title(&format!(
                    "vdview | taken over by another viewer ({by}) - press Enter to reconnect"
                ));
                r.replaced_by = Some(by);
                r.update_osd();
            }
            UserEvent::Input(g, link) if g == current => {
                r.input_link = Some(link);
                println!("input to the Mac: {}", r.input_label());
                r.flash_osd();
            }
            _ => {} // events from an older connection
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let Some(r) = self.running.as_mut() else {
            return;
        };
        if r.forwarding() {
            r.flush_pointer(); // at most one pointer message per turn of the event loop
        }
        let now = Instant::now();
        if now >= r.next_stats {
            let rtt = r
                .control
                .as_ref()
                .and_then(|c| c.clock.lock().unwrap().rtt_us());
            let (line, summary) = r.stats.drain(STATS_EVERY.as_secs_f64(), rtt);
            if r.control.is_some() {
                println!("{line}");
                let title = format!(
                    "vdview {} | input {} | {line}",
                    r.start.as_ref().map_or(String::new(), describe_start),
                    r.input_label()
                );
                r.window.set_title(&title);
            }
            r.summary = summary;
            if r.control.is_some()
                && r.secret.is_ok()
                && r.input_link.is_none()
                && !r.input_silence_noted
                && r.connected_at.elapsed() >= INPUT_REPLY_WITHIN
            {
                println!("input to the Mac: {}", r.input_label());
                r.input_silence_noted = true;
            }
            r.next_stats = now + STATS_EVERY;
            r.update_osd();
        }
        if r.osd_until.is_some_and(|t| now >= t) {
            r.osd_until = None;
            r.update_osd();
        }
        let wake = r.osd_until.map_or(r.next_stats, |t| t.min(r.next_stats));
        el.set_control_flow(ControlFlow::WaitUntil(wake));
    }

    fn exiting(&mut self, _: &ActiveEventLoop) {
        if let Some(r) = self.running.as_mut() {
            r.generation.fetch_add(1, Ordering::SeqCst); // stop any connector
            r.end_session();
        }
    }
}

/// `--snapshot IN.hevc OUT.ppm`: decode + offscreen render one picture.
fn snapshot(input: &str, output: &str, hwaccel: &str) -> Result<()> {
    let data = std::fs::read(input).with_context(|| format!("reading {input}"))?;
    let mut decoder = Decoder::open(hwaccel, 0)?;
    // A lone access unit sits in the decoder until it knows the picture is complete; an empty
    // packet (flush) is not supported by the shim, so feed the AU twice.
    let (w, h, layout, rgba) = {
        let pic = match decoder.decode(&data)? {
            Some(p) => p,
            None => decoder
                .decode(&data)?
                .ok_or_else(|| anyhow!("decoder produced no picture"))?,
        };
        (pic.width, pic.height, pic.layout, render::snapshot(&pic)?)
    };
    println!("decoded {w}x{h} {layout:?} via {}", decoder.describe());
    let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
    for px in rgba.chunks(4) {
        ppm.extend_from_slice(&px[..3]);
    }
    std::fs::write(output, ppm).with_context(|| format!("writing {output}"))?;
    println!("wrote {output}");
    Ok(())
}

/// Splits an Annex-B HEVC stream into access units (one coded picture each): a new unit starts
/// at a VPS or at the first slice segment of a picture.
fn split_access_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = vec![];
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let sc = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            starts.push((sc, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut aus = vec![];
    let mut au_start: Option<usize> = None;
    for &(sc, nal) in &starts {
        let Some(&b0) = data.get(nal) else { break };
        let kind = (b0 >> 1) & 0x3f;
        let first_slice = kind < 32 && data.get(nal + 2).is_some_and(|b| b & 0x80 != 0);
        if kind == 32 || first_slice {
            // A VPS directly followed by its picture's slice belongs to the same unit.
            if let Some(st) = au_start {
                let has_slice = starts
                    .iter()
                    .filter(|(s2, n2)| {
                        *s2 >= st && *s2 < sc && data.get(*n2).is_some_and(|b| (b >> 1) & 0x3f < 32)
                    })
                    .count()
                    > 0;
                if has_slice {
                    aus.push(&data[st..sc]);
                    au_start = Some(sc);
                }
            } else {
                au_start = Some(sc);
            }
        }
    }
    if let Some(st) = au_start {
        aus.push(&data[st..]);
    }
    aus
}

/// `--decode-test IN.hevc`: feed every access unit through the live decode path.
fn decode_test(input: &str, hwaccel: &str) -> Result<()> {
    let data = std::fs::read(input).with_context(|| format!("reading {input}"))?;
    let aus = split_access_units(&data);
    let mut decoder = Decoder::open(hwaccel, 0)?;
    let t0 = Instant::now();
    let (mut pictures, mut errors) = (0usize, 0usize);
    for (i, au) in aus.iter().enumerate() {
        match decoder.decode(au) {
            Ok(Some(_)) => pictures += 1,
            Ok(None) => {}
            Err(e) => {
                errors += 1;
                eprintln!("access unit {i}: {e:#}");
            }
        }
    }
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    println!(
        "{} access units, {pictures} pictures, {errors} errors via {} ({:.2} ms/picture)",
        aus.len(),
        decoder.describe(),
        ms / aus.len().max(1) as f64
    );
    Ok(())
}

/// `--set-input-secret [--input-secret PATH]`: saves the secret typed or piped on stdin (kept off
/// the command line so that it does not end up in the shell's history).
fn set_input_secret(argv: &[String]) -> Result<()> {
    let path = match argv.iter().position(|a| a == "--input-secret") {
        Some(i) => PathBuf::from(
            argv.get(i + 1)
                .ok_or_else(|| anyhow!("--input-secret needs a value"))?,
        ),
        None => secure::default_secret_path()
            .ok_or_else(|| anyhow!("no default place for the secret; use --input-secret"))?,
    };
    eprintln!("paste the input secret (64 hex digits from make-input-secret.sh) and press Enter:");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let secret = secure::parse_secret(&line)?;
    secure::save_secret(&path, &secret)?;
    println!("saved {}", path.display());
    Ok(())
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("--decode-test") {
        let Some(input) = argv.get(2) else {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        };
        let hwaccel = argv
            .iter()
            .position(|a| a == "--hwaccel")
            .and_then(|i| argv.get(i + 1))
            .map_or("auto", String::as_str);
        return match decode_test(input, hwaccel) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("vdview: {e:#}");
                ExitCode::FAILURE
            }
        };
    }
    if argv.get(1).map(String::as_str) == Some("--snapshot") {
        let (Some(input), Some(output)) = (argv.get(2), argv.get(3)) else {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        };
        let hwaccel = argv
            .iter()
            .position(|a| a == "--hwaccel")
            .and_then(|i| argv.get(i + 1))
            .map_or("auto", String::as_str);
        return match snapshot(input, output, hwaccel) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("vdview: {e:#}");
                ExitCode::FAILURE
            }
        };
    }
    if argv.get(1).map(String::as_str) == Some("--set-input-secret") {
        return match set_input_secret(&argv) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("vdview: {e:#}");
                ExitCode::FAILURE
            }
        };
    }
    let args = match Args::parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("vdview: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("vdview: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut app = App {
        args,
        proxy: event_loop.create_proxy(),
        running: None,
        failed: false,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("vdview: {e}");
        return ExitCode::FAILURE;
    }
    if app.failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
