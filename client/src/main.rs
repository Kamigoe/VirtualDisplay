//! vdview: shows a VirtualDisplay stream from a Mac running `vdsend`.

mod clock;
mod control;
mod decoder;
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
  --chroma 420|444      chroma subsampling (default 420; 444 is decoded on the CPU on AMD GPUs)
  --bitrate MBPS        average bitrate (default: sender's)
  --hwaccel NAME        auto | none | d3d11va | vaapi | vulkan | videotoolbox (default auto)
  --threads N           software decoding threads (default 0 = auto)
  --monitor N           show on monitor N (see the list printed at startup; default: primary)
  --windowed            start in a window instead of borderless fullscreen (F11 toggles)
  --vsync               present with FIFO (no tearing, +latency) instead of mailbox/immediate

self-test (no sender or display needed):
  vdview --snapshot IN.hevc OUT.ppm [--hwaccel NAME]
      decode the first picture of an Annex-B file and write what the renderer draws";

/// The M1's HEVC encoder sustains roughly this many pixels per second.
const ENCODER_PIXEL_RATE: f64 = 650e6;

struct Args {
    server: String,
    mode: Option<(u16, u16, u32)>,
    hidpi: Option<bool>,
    server_mode: bool,
    chroma444: bool,
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
            hidpi: None,
            server_mode: false,
            chroma444: false,
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
    fn hello(&self, monitor: Option<&MonitorHandle>) -> Hello {
        let mut h = Hello {
            width: 0,
            height: 0,
            hidpi: false,
            refresh_mhz: 60_000,
            chroma444: self.chroma444,
            bitrate_kbps: self.bitrate_kbps,
        };
        if self.server_mode {
            return h;
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
        } else {
            let div = if h.hidpi { 2 } else { 1 };
            h.width = (pw / div) as u16;
            h.height = (ph / div) as u16;
            let pixels = (pw * ph) as f64;
            h.refresh_mhz = [mhz, 120_000, 100_000, 60_000]
                .into_iter()
                .find(|&r| r <= mhz && pixels * r as f64 / 1000.0 <= ENCODER_PIXEL_RATE)
                .unwrap_or(60_000);
            if h.refresh_mhz != mhz {
                println!(
                    "note: monitor runs at {:.0} Hz but the Mac encoder tops out at {:.0} Hz for {pw}x{ph}",
                    mhz as f64 / 1000.0,
                    h.refresh_mhz as f64 / 1000.0
                );
            }
        }
        h
    }
}

enum UserEvent {
    FrameReady,
    Closed(String),
}

struct Running {
    window: Arc<Window>,
    renderer: Renderer,
    control: Arc<Control>,
    stats: Arc<Stats>,
    start: Start,
    decoder_name: String,
    next_stats: Instant,
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
        let hello = self.args.hello(monitor.as_ref());
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
        let (control, start) = Control::connect(addr, &hello, move |reason| {
            let _ = proxy.send_event(UserEvent::Closed(reason));
        })?;
        println!(
            "stream: {}x{} @ {:.2} Hz, HEVC {}",
            start.pixel_width,
            start.pixel_height,
            start.refresh_mhz as f64 / 1000.0,
            if start.chroma444 { "4:4:4" } else { "4:2:0" }
        );

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
        // A few frames of slack; if the decoder falls further behind, the receiver resyncs on a keyframe.
        let (tx, rx) = sync_channel(8);
        receiver::spawn(
            addr,
            start.session_id,
            control.clone(),
            stats.clone(),
            tx,
            self.stop.clone(),
        )?;

        let mut decoder = Decoder::open(&self.args.hwaccel, self.args.threads)?;
        let decoder_name = decoder.describe();
        let (proxy, dstats, dcontrol) = (self.proxy.clone(), stats.clone(), control.clone());
        thread::Builder::new()
            .name("decode".into())
            .spawn(move || {
                for frame in rx {
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
                            eprintln!("{e:#}");
                            dcontrol.request_keyframe();
                        }
                    }
                }
            })?;

        Ok(Running {
            window,
            renderer,
            control,
            stats,
            start,
            decoder_name,
            next_stats: Instant::now() + STATS_EVERY,
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
            let line = r.stats.drain(STATS_EVERY.as_secs_f64(), rtt);
            println!("{line}");
            r.window.set_title(&format!(
                "vdview {}x{}@{:.0} [{}] | {line}",
                r.start.pixel_width,
                r.start.pixel_height,
                r.start.refresh_mhz as f64 / 1000.0,
                r.decoder_name
            ));
            r.next_stats = now + STATS_EVERY;
        }
        el.set_control_flow(ControlFlow::WaitUntil(r.next_stats));
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

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
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
