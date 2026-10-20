//! TCP control channel: Hello/Start handshake, keyframe requests, and Ping/Pong clock sync.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::clock::{ClockSync, now_us};
use crate::protocol::{self, Hello, Start};

pub struct Control {
    writer: Mutex<TcpStream>,
    pub clock: Arc<Mutex<ClockSync>>,
    /// Current video session; changes when the sender answers a re-sent Hello with a new Start.
    pub session: Arc<AtomicU32>,
    /// Start message of the current session (format of the stream).
    pub current: Arc<Mutex<Start>>,
    /// When the last Pong arrived; the receiver declares the link dead if this gets stale.
    pub last_pong: Arc<Mutex<Instant>>,
}

impl Control {
    /// Connects, sends Hello and waits for Start. Spawns a reader thread for Pong/Start/Error;
    /// `on_restart` runs there for each later Start (after `reconfigure`), `on_closed` when the
    /// sender goes away.
    pub fn connect(
        addr: SocketAddr,
        hello: &Hello,
        on_restart: impl Fn(Start) + Send + 'static,
        on_closed: impl FnOnce(String) + Send + 'static,
    ) -> Result<(Arc<Control>, Start)> {
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
            .with_context(|| format!("connecting to {addr}"))?;
        stream.set_nodelay(true)?;
        stream.write_all(&protocol::control_message(
            protocol::MSG_HELLO,
            &hello.encode(),
        ))?;

        // The sender may need a few seconds to rebuild its virtual display.
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
        let (kind, body) = read_message(&mut stream).context("waiting for Start")?;
        let start = match kind {
            protocol::MSG_START => Start::decode(&body)?,
            protocol::MSG_ERROR => bail!("sender refused: {}", String::from_utf8_lossy(&body)),
            other => bail!("unexpected control message 0x{other:02x}"),
        };
        if start.codec != protocol::CODEC_HEVC {
            bail!("unsupported codec {}", start.codec);
        }
        stream.set_read_timeout(None)?;

        let control = Arc::new(Control {
            writer: Mutex::new(stream.try_clone()?),
            clock: Arc::new(Mutex::new(ClockSync::default())),
            session: Arc::new(AtomicU32::new(start.session_id)),
            current: Arc::new(Mutex::new(start)),
            last_pong: Arc::new(Mutex::new(Instant::now())),
        });

        let clock = control.clock.clone();
        let session = control.session.clone();
        let current = control.current.clone();
        let last_pong = control.last_pong.clone();
        thread::Builder::new()
            .name("control-rx".into())
            .spawn(move || {
                let reason = loop {
                    match read_message(&mut stream) {
                        Ok((protocol::MSG_PONG, body)) => {
                            let mut r = protocol::Reader(&body);
                            if let (Ok(sent), Ok(server)) = (r.u64(), r.u64()) {
                                clock.lock().unwrap().add_sample(sent, server, now_us());
                                *last_pong.lock().unwrap() = Instant::now();
                            }
                        }
                        Ok((protocol::MSG_START, body)) => match Start::decode(&body) {
                            Ok(s) => {
                                *current.lock().unwrap() = s;
                                session.store(s.session_id, Ordering::Relaxed);
                                on_restart(s);
                            }
                            Err(e) => eprintln!("bad Start: {e:#}"),
                        },
                        Ok((protocol::MSG_ERROR, body)) => {
                            break String::from_utf8_lossy(&body).into_owned();
                        }
                        Ok(_) => {}
                        Err(e) => break format!("control connection closed: {e}"),
                    }
                };
                on_closed(reason);
            })?;

        Ok((control, start))
    }

    /// Asks for a different mode/format on the running connection; answered by a new Start.
    pub fn reconfigure(&self, hello: &Hello) {
        self.send(protocol::MSG_HELLO, &hello.encode());
    }

    /// Closes the connection (the reader thread then reports it as closed).
    pub fn shutdown(&self) {
        let _ = self.writer.lock().unwrap().shutdown(Shutdown::Both);
    }

    pub fn request_keyframe(&self) {
        self.send(protocol::MSG_KEYFRAME_REQUEST, &[]);
    }

    pub fn ping(&self) {
        self.send(protocol::MSG_PING, &now_us().to_be_bytes());
    }

    fn send(&self, kind: u8, body: &[u8]) {
        // Errors surface through the reader thread noticing the closed connection.
        let _ = self
            .writer
            .lock()
            .unwrap()
            .write_all(&protocol::control_message(kind, body));
    }
}

fn read_message(s: &mut TcpStream) -> Result<(u8, Vec<u8>)> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > 64 * 1024 {
        bail!("bad control message length {len}");
    }
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf)?;
    let kind = buf.remove(0);
    Ok((kind, buf))
}
