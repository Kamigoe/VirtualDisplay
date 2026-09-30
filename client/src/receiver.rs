//! UDP receive path: reassembles frames, delivers them in order, and recovers from loss with
//! Nack (retransmission) first and a keyframe request as the fallback (docs/protocol.md).

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Result;
use socket2::{Domain, Protocol, Socket, Type};

use crate::control::Control;
use crate::protocol::{self, FLAG_KEYFRAME, PAYLOAD_SIZE, VideoHeader};
use crate::stats::Stats;

pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub capture_us: u64,
}

struct Assembly {
    keyframe: bool,
    capture_us: u64,
    data: Vec<u8>,
    have: Vec<bool>,
    received: usize,
    last_seen: Instant,
    nack_rounds: u32,
    last_nack: Option<Instant>,
}

impl Assembly {
    fn new(h: &VideoHeader, now: Instant) -> Self {
        Assembly {
            keyframe: h.flags & FLAG_KEYFRAME != 0,
            capture_us: h.capture_us,
            data: vec![0; h.frame_bytes as usize],
            have: vec![false; h.packet_count as usize],
            received: 0,
            last_seen: now,
            nack_rounds: 0,
            last_nack: None,
        }
    }
    fn complete(&self) -> bool {
        self.received == self.have.len()
    }
    fn missing(&self) -> Vec<u16> {
        self.have
            .iter()
            .enumerate()
            .filter(|(_, h)| !**h)
            .map(|(i, _)| i as u16)
            .collect()
    }
}

/// Frame ids wrap; compare them as signed distances.
fn newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

/// Idle wakeup period for keepalives and loss timers.
const TICK: Duration = Duration::from_millis(2);
/// No new packet for this long on an incomplete frame => ask for the rest.
const QUIET_BEFORE_NACK: Duration = Duration::from_millis(3);
const MAX_NACK_ROUNDS: u32 = 2;
const KEYFRAME_RETRY: Duration = Duration::from_millis(250);
const KEEPALIVE_EVERY: Duration = Duration::from_secs(1);
const PING_EVERY: Duration = Duration::from_millis(500);
const RECV_BUFFER: usize = 8 << 20;

pub fn spawn(
    server: SocketAddr,
    session_id: u32,
    control: Arc<Control>,
    stats: Arc<Stats>,
    out: SyncSender<EncodedFrame>,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<()>> {
    let socket = Socket::new(
        Domain::for_address(server),
        Type::DGRAM,
        Some(Protocol::UDP),
    )?;
    // A 4K keyframe is ~1000 datagrams arriving back to back; OS defaults (64 KiB on Windows) overflow.
    let _ = socket.set_recv_buffer_size(RECV_BUFFER);
    let got = socket.recv_buffer_size().unwrap_or(0);
    if got < RECV_BUFFER / 2 {
        eprintln!(
            "warning: UDP receive buffer is only {} KiB (wanted {} KiB); on Linux raise net.core.rmem_max",
            got / 1024,
            RECV_BUFFER / 1024
        );
    }
    let bind: SocketAddr = if server.is_ipv4() {
        "0.0.0.0:0".parse()?
    } else {
        "[::]:0".parse()?
    };
    socket.bind(&bind.into())?;
    let socket: UdpSocket = socket.into();
    socket.connect(server)?; // only accept datagrams from the sender
    socket.set_read_timeout(Some(TICK))?;

    let mut rx = Receiver {
        socket,
        session_id,
        control,
        stats,
        out,
        frames: BTreeMap::new(),
        next_expected: 0,
        waiting_for_keyframe: true,
        last_keyframe_request: None,
        stall_since: None,
        retry_after: Duration::from_millis(8),
    };
    Ok(thread::Builder::new()
        .name("udp-rx".into())
        .spawn(move || rx.run(&stop))?)
}

struct Receiver {
    socket: UdpSocket,
    session_id: u32,
    control: Arc<Control>,
    stats: Arc<Stats>,
    out: SyncSender<EncodedFrame>,
    /// In-progress frames keyed by id relative to `next_expected` ordering.
    frames: BTreeMap<u32, Assembly>,
    next_expected: u32,
    waiting_for_keyframe: bool,
    last_keyframe_request: Option<Instant>,
    /// When we first saw a frame newer than `next_expected` while it had not arrived at all.
    stall_since: Option<Instant>,
    retry_after: Duration,
}

impl Receiver {
    fn run(&mut self, stop: &AtomicBool) {
        let mut buf = [0u8; 2048];
        let mut last_keepalive: Option<Instant> = None;
        let mut last_ping: Option<Instant> = None;
        while !stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            if last_keepalive.is_none_or(|t| now - t >= KEEPALIVE_EVERY) {
                let _ = self.socket.send(&protocol::keepalive(self.session_id));
                last_keepalive = Some(now);
            }
            if last_ping.is_none_or(|t| now - t >= PING_EVERY) {
                self.control.ping();
                last_ping = Some(now);
                if let Some(rtt) = self.control.clock.lock().unwrap().rtt_us() {
                    self.retry_after =
                        Duration::from_micros(2 * rtt + 2000).max(Duration::from_millis(8));
                }
            }
            match self.socket.recv(&mut buf) {
                Ok(n) => self.on_datagram(&buf[..n]),
                // TimedOut/WouldBlock: idle tick. ConnectionReset: Windows reports ICMP port-unreachable here.
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::ConnectionReset
                    ) => {}
                Err(e) => {
                    eprintln!("udp receive error: {e}");
                    thread::sleep(TICK);
                }
            }
            self.check_loss(Instant::now());
        }
    }

    fn on_datagram(&mut self, p: &[u8]) {
        let Some(h) = VideoHeader::parse(p) else {
            return;
        };
        if h.session_id != self.session_id {
            return;
        }
        self.stats.packet(p.len());
        if !self.waiting_for_keyframe && !newer(h.frame_id, self.next_expected.wrapping_sub(1)) {
            return; // already delivered or given up
        }
        let now = Instant::now();
        let a = self
            .frames
            .entry(h.frame_id)
            .or_insert_with(|| Assembly::new(&h, now));
        let idx = h.packet_index as usize;
        if a.have.len() != h.packet_count as usize
            || a.data.len() != h.frame_bytes as usize
            || idx >= a.have.len()
        {
            return; // inconsistent header; ignore
        }
        a.last_seen = now;
        if !a.have[idx] {
            let payload = &p[protocol::VIDEO_HEADER_SIZE..];
            let off = idx * PAYLOAD_SIZE;
            let len = payload.len().min(a.data.len().saturating_sub(off));
            a.data[off..off + len].copy_from_slice(&payload[..len]);
            a.have[idx] = true;
            a.received += 1;
        }
        if a.complete() {
            self.stats.frame_received();
            self.deliver_ready();
        }
    }

    fn deliver_ready(&mut self) {
        loop {
            if self.waiting_for_keyframe {
                let key = self
                    .frames
                    .iter()
                    .rev()
                    .find(|(_, a)| a.keyframe && a.complete())
                    .map(|(id, _)| *id);
                let Some(id) = key else { return };
                self.waiting_for_keyframe = false;
                self.next_expected = id;
            }
            let id = self.next_expected;
            match self.frames.get(&id) {
                Some(a) if a.complete() => {
                    let a = self.frames.remove(&id).unwrap();
                    self.next_expected = id.wrapping_add(1);
                    self.stall_since = None;
                    self.frames.retain(|k, _| newer(*k, id));
                    match self.out.try_send(EncodedFrame {
                        data: a.data,
                        capture_us: a.capture_us,
                    }) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => {
                            // Decoder is behind; the reference chain is broken from here on.
                            self.lose_sync(Instant::now());
                            return;
                        }
                        Err(TrySendError::Disconnected(_)) => return,
                    }
                }
                _ => return,
            }
        }
    }

    fn check_loss(&mut self, now: Instant) {
        if self.waiting_for_keyframe {
            if self
                .last_keyframe_request
                .is_none_or(|t| now - t >= KEYFRAME_RETRY)
            {
                self.request_keyframe(now);
            }
            // Bound memory while waiting.
            while self.frames.len() > 64 {
                let first = *self.frames.keys().next().unwrap();
                self.frames.remove(&first);
            }
            return;
        }
        let id = self.next_expected;
        let has_newer = self.frames.keys().any(|k| newer(*k, id));
        let retry_after = self.retry_after;
        match self.frames.get_mut(&id) {
            Some(a) if !a.complete() => {
                let due = match a.last_nack {
                    None => has_newer || now - a.last_seen >= QUIET_BEFORE_NACK,
                    Some(t) => now - t >= retry_after,
                };
                if !due {
                    return;
                }
                if a.nack_rounds >= MAX_NACK_ROUNDS {
                    self.lose_sync(now);
                    return;
                }
                a.nack_rounds += 1;
                a.last_nack = Some(now);
                let missing = a.missing();
                for chunk in missing.chunks(600) {
                    let _ = self
                        .socket
                        .send(&protocol::nack(self.session_id, id, chunk));
                }
                self.stats.nack();
            }
            None if has_newer => {
                // Every packet of this frame was lost, so we cannot even Nack it.
                let since = *self.stall_since.get_or_insert(now);
                if now - since >= retry_after {
                    self.lose_sync(now);
                }
            }
            _ => {}
        }
    }

    fn lose_sync(&mut self, now: Instant) {
        self.stats.frame_lost();
        self.waiting_for_keyframe = true;
        self.stall_since = None;
        self.frames.retain(|_, a| a.keyframe);
        self.request_keyframe(now);
    }

    fn request_keyframe(&mut self, now: Instant) {
        self.control.request_keyframe();
        self.stats.keyframe_requested();
        self.last_keyframe_request = Some(now);
    }
}
