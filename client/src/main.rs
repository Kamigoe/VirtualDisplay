//! vdview: shows a VirtualDisplay stream from a Mac running `vdsend`.

mod clock;
mod control;
mod decoder;
mod osd;
mod protocol;
mod receiver;
mod render;
mod stats;

use std::net::{SocketAddr, ToSocketAddrs};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::monitor::MonitorHandle;
use winit::window::{Fullscreen, Window, WindowId};

use crate::clock::now_us;
use crate::control::Control;
use crate::decoder::Decoder;
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
                        monitor and the Mac encoder allow)
  --bitrate MBPS        average bitrate (default: sender's)
  --hwaccel NAME        auto | none | d3d11va | vaapi | vulkan | videotoolbox (default auto)
  --threads N           software decoding threads (default 0 = auto)
  --monitor N           show on monitor N (see the list printed at startup; default: primary)
  --windowed            start in a window instead of borderless fullscreen (F11 toggles)
  --vsync               present with FIFO (no tearing, +latency) instead of mailbox/immediate

keys while running:
  F2  8-bit <-> 10-bit      F3  4:2:0 <-> 4:4:4      F4  cycle refresh rate (max / 120 / 60 Hz)
  F11 fullscreen on/off     Esc leave fullscreen

self-test (no sender or display needed):
  vdview --snapshot IN.hevc OUT.ppm [--hwaccel NAME]
      decode the first picture of an Annex-B file and write what the renderer draws
  vdview --decode-test IN.hevc [--hwaccel NAME]
      decode a whole Annex-B stream picture by picture, as the live path does";

/// The M1's HEVC encoder sustains roughly this many pixels per second.
const ENCODER_PIXEL_RATE: f64 = 650e6;

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
            .find(|&r| r <= mhz && pixels * r as f64 / 1000.0 <= ENCODER_PIXEL_RATE)
            .unwrap_or(60_000);
        if max != mhz {
            println!(
                "note: monitor runs at {:.0} Hz but the Mac encoder tops out at {:.0} Hz for {pw}x{ph}",
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
    /// The sender answered a reconfiguration with a new stream.
    Restarted(Start),
    Closed(String),
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

struct Running {
    window: Arc<Window>,
    renderer: Renderer,
    control: Arc<Control>,
    stats: Arc<Stats>,
    start: Start,
    next_stats: Instant,
    /// What we last asked for; F2-F4 edit it and re-send it.
    hello: Hello,
    /// Refresh rates F4 cycles through (mHz, highest first).
    refresh_choices: Vec<u32>,
    /// Status panel: shown while pinned (F1), until `osd_until`, or while a switch is pending.
    osd_pinned: bool,
    osd_until: Option<Instant>,
    switching: bool,
    summary: stats::Summary,
}

const OSD_SHOW: Duration = Duration::from_secs(5);

impl Running {
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
        self.control.reconfigure(&self.hello);
        self.switching = true;
        self.flash_osd();
    }

    fn flash_osd(&mut self) {
        self.osd_until = Some(Instant::now() + OSD_SHOW);
        self.update_osd();
    }

    fn osd_visible(&self) -> bool {
        self.osd_pinned || self.switching || self.osd_until.is_some_and(|t| Instant::now() < t)
    }

    /// Re-renders (or hides) the status panel and redraws so it shows even on a static screen.
    fn update_osd(&mut self) {
        let lines = if self.osd_visible() {
            let s = &self.start;
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
                format!(
                    "F1 {}  F2 BIT DEPTH  F3 CHROMA  F4 REFRESH  F11 FULLSCREEN",
                    if self.osd_pinned { "HIDE" } else { "PIN" }
                ),
            ]
        } else {
            vec![]
        };
        let scale = (3.0 * self.window.scale_factor()).round() as usize;
        self.renderer.set_osd(lines, scale);
        self.window.request_redraw();
    }
}

struct App {
    args: Args,
    proxy: EventLoopProxy<UserEvent>,
    stop: Arc<AtomicBool>,
    running: Option<Running>,
    failed: bool,
}

const STATS_EVERY: Duration = Duration::from_secs(1);

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
        println!(
            "connecting to {addr}: requesting {}",
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
        let proxy = self.proxy.clone();
        let restart_proxy = self.proxy.clone();
        let (control, start) = Control::connect(
            addr,
            &hello,
            move |s| {
                let _ = restart_proxy.send_event(UserEvent::Restarted(s));
            },
            move |reason| {
                let _ = proxy.send_event(UserEvent::Closed(reason));
            },
        )?;
        println!("stream: {}", describe_start(&start));

        let mut attrs = Window::default_attributes().with_title("vdview");
        if self.args.windowed {
            let scale = monitor.as_ref().map_or(1.0, |m| m.scale_factor());
            attrs = attrs.with_inner_size(winit::dpi::LogicalSize::new(
                start.pixel_width as f64 / scale,
                start.pixel_height as f64 / scale,
            ));
        } else {
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(monitor.clone())));
        }
        let window = Arc::new(el.create_window(attrs)?);
        window.set_cursor_visible(self.args.windowed); // the Mac's cursor is part of the picture

        let (renderer, uploader) = Renderer::new(window.clone(), self.args.vsync)?;
        let stats = Arc::new(Stats::default());
        // When a stalled frame is finally repaired, every frame queued behind it is released at once;
        // decoding them (~6 ms each) catches up faster than resyncing on a large keyframe over Wi-Fi.
        let (tx, rx) = sync_channel(64);
        receiver::spawn(addr, control.clone(), stats.clone(), tx, self.stop.clone())?;

        // A fresh decoder per stream format: FFmpeg 6.1 crashes when a running VA-API decoder falls
        // back to software mid-stream (4:2:0 -> 4:4:4 on AMD), and 4:4:4 never decodes on VA-API
        // anyway, so open it in software directly.
        let (hwaccel, threads) = (self.args.hwaccel.clone(), self.args.threads);
        let open_for = move |s: &Start| {
            let hw = if s.chroma444 && hwaccel == "auto" {
                "none"
            } else {
                hwaccel.as_str()
            };
            Decoder::open(hw, threads)
        };
        let mut decoder = open_for(&start)?;
        let mut decoder_session = start.session_id;
        let (proxy, dstats, dcontrol) = (self.proxy.clone(), stats.clone(), control.clone());
        thread::Builder::new()
            .name("decode".into())
            .spawn(move || {
                // Last line of defence for the reference chain: never hand the decoder a picture
                // whose predecessor it has not seen. Mesa's VA-API driver segfaults on missing
                // references (seen on RX 9070 XT / Mesa 25.2.8), so after any gap or decode
                // error everything is dropped until the next keyframe.
                let mut expected: Option<u32> = None;
                let mut last_request = Instant::now() - Duration::from_secs(1);
                for frame in rx {
                    if frame.session != decoder_session {
                        let start = *dcontrol.current.lock().unwrap();
                        match open_for(&start) {
                            Ok(d) => decoder = d,
                            Err(e) => eprintln!("{e:#}"),
                        }
                        decoder_session = frame.session;
                        expected = None;
                    }
                    let in_sequence = frame.keyframe || expected == Some(frame.frame_id);
                    if !in_sequence {
                        if expected.is_some() {
                            eprintln!(
                                "decoder: expected frame {} but got {}; dropping until a keyframe",
                                expected.unwrap(),
                                frame.frame_id
                            );
                            expected = None;
                        }
                        if last_request.elapsed() >= Duration::from_millis(250) {
                            dcontrol.request_keyframe();
                            last_request = Instant::now();
                        }
                        continue;
                    }
                    expected = Some(frame.frame_id.wrapping_add(1));
                    let t0 = Instant::now();
                    match decoder.decode(&frame.data) {
                        Ok(Some(pic)) => {
                            uploader.upload(&pic, frame.capture_us);
                            dstats.frame_decoded(
                                t0.elapsed().as_micros() as u32,
                                &decoder.describe(),
                            );
                            let _ = proxy.send_event(UserEvent::FrameReady);
                        }
                        Ok(None) => {}
                        Err(e) => {
                            eprintln!("{e:#}; dropping until a keyframe");
                            expected = None;
                            dcontrol.request_keyframe();
                            last_request = Instant::now();
                        }
                    }
                }
            })?;

        let mut refresh_choices = vec![max_refresh, 120_000, 60_000, hello.refresh_mhz];
        refresh_choices.retain(|&r| r <= max_refresh);
        refresh_choices.sort_unstable_by(|a, b| b.cmp(a));
        refresh_choices.dedup();
        Ok(Running {
            window,
            renderer,
            control,
            stats,
            start,
            next_stats: Instant::now() + STATS_EVERY,
            hello,
            refresh_choices,
            osd_pinned: false,
            osd_until: Some(Instant::now() + OSD_SHOW),
            switching: false,
            summary: Default::default(),
        })
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
                if let Some(capture_us) = r.renderer.render() {
                    r.stats.frame_presented();
                    let clock = r.control.clock.lock().unwrap();
                    if let Some(local) = clock.server_to_client(capture_us) {
                        r.stats.latency(now_us().saturating_sub(local) as u32);
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        repeat: false,
                        ..
                    },
                ..
            } => match logical_key {
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
                _ => {}
            },
            _ => {}
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::FrameReady => {
                if let Some(r) = &self.running {
                    r.window.request_redraw();
                }
            }
            UserEvent::Restarted(start) => {
                println!("stream: {}", describe_start(&start));
                if let Some(r) = self.running.as_mut() {
                    r.start = start;
                    r.switching = false;
                    r.flash_osd();
                }
            }
            UserEvent::Closed(reason) => {
                eprintln!("sender closed the session: {reason}");
                el.exit();
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let Some(r) = self.running.as_mut() else {
            return;
        };
        let now = Instant::now();
        if now >= r.next_stats {
            let rtt = r.control.clock.lock().unwrap().rtt_us();
            let (line, summary) = r.stats.drain(STATS_EVERY.as_secs_f64(), rtt);
            println!("{line}");
            r.window
                .set_title(&format!("vdview {} | {line}", describe_start(&r.start)));
            r.summary = summary;
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
        self.stop.store(true, Ordering::Relaxed);
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
        stop: Arc::new(AtomicBool::new(false)),
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
