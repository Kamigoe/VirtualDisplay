//! Counters shared between threads, summarised once per second.

use std::sync::Mutex;

#[derive(Default)]
struct Inner {
    bytes: u64,
    packets: u64,
    frames_received: u64,
    frames_decoded: u64,
    frames_presented: u64,
    nacks: u64,
    frames_lost: u64,
    keyframe_requests: u64,
    decode_us: Vec<u32>,
    latency_us: Vec<u32>,
    decoder_path: String,
}

#[derive(Default)]
pub struct Stats(Mutex<Inner>);

impl Stats {
    pub fn packet(&self, bytes: usize) {
        let mut s = self.0.lock().unwrap();
        s.packets += 1;
        s.bytes += bytes as u64;
    }
    pub fn frame_received(&self) {
        self.0.lock().unwrap().frames_received += 1;
    }
    pub fn nack(&self) {
        self.0.lock().unwrap().nacks += 1;
    }
    pub fn frame_lost(&self) {
        self.0.lock().unwrap().frames_lost += 1;
    }
    pub fn keyframe_requested(&self) {
        self.0.lock().unwrap().keyframe_requests += 1;
    }
    pub fn frame_decoded(&self, decode_us: u32, path: &str) {
        let mut s = self.0.lock().unwrap();
        s.frames_decoded += 1;
        s.decode_us.push(decode_us);
        if s.decoder_path != path {
            s.decoder_path = path.to_string();
        }
    }
    pub fn frame_presented(&self) {
        self.0.lock().unwrap().frames_presented += 1;
    }
    /// Mac composite → presented on this machine.
    pub fn latency(&self, us: u32) {
        self.0.lock().unwrap().latency_us.push(us);
    }

    pub fn drain(&self, interval_s: f64, rtt_us: Option<u64>) -> String {
        let mut s = self.0.lock().unwrap();
        let line = format!(
            "{:5.1} fps (shown {:5.1}) | {:6.1} Mbps | latency p50 {} p95 {} | decode p50 {} [{}] | rtt {} | nack {} lost {} kf-req {}",
            s.frames_decoded as f64 / interval_s,
            s.frames_presented as f64 / interval_s,
            s.bytes as f64 * 8.0 / interval_s / 1e6,
            ms(pct(&mut s.latency_us, 0.5)),
            ms(pct(&mut s.latency_us, 0.95)),
            ms(pct(&mut s.decode_us, 0.5)),
            s.decoder_path,
            rtt_us.map_or("-".into(), |r| format!("{:.1}ms", r as f64 / 1000.0)),
            s.nacks,
            s.frames_lost,
            s.keyframe_requests,
        );
        let path = std::mem::take(&mut s.decoder_path);
        *s = Inner {
            decoder_path: path,
            ..Default::default()
        };
        line
    }
}

fn pct(v: &mut [u32], p: f64) -> Option<u32> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    Some(v[((v.len() as f64 * p) as usize).min(v.len() - 1)])
}

fn ms(v: Option<u32>) -> String {
    v.map_or("-".into(), |us| format!("{:.1}ms", us as f64 / 1000.0))
}
