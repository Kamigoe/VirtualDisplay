//! Local monotonic clock and sender clock-offset estimation (NTP-style, min-RTT filtered).

use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::Instant;

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Microseconds on the viewer's monotonic clock (`client_us`).
pub fn now_us() -> u64 {
    epoch().elapsed().as_micros() as u64
}

#[derive(Clone, Copy)]
struct Sample {
    rtt_us: u64,
    /// server_us - client_us
    offset_us: i64,
}

#[derive(Default)]
pub struct ClockSync {
    samples: VecDeque<Sample>,
}

impl ClockSync {
    const WINDOW: usize = 16;

    pub fn add_sample(&mut self, sent_client_us: u64, server_us: u64, recv_client_us: u64) {
        let rtt_us = recv_client_us.saturating_sub(sent_client_us);
        let midpoint = sent_client_us + rtt_us / 2;
        self.samples.push_back(Sample {
            rtt_us,
            offset_us: server_us as i64 - midpoint as i64,
        });
        if self.samples.len() > Self::WINDOW {
            self.samples.pop_front();
        }
    }

    /// The sample with the smallest RTT has the least asymmetric-delay error.
    fn best(&self) -> Option<Sample> {
        self.samples.iter().copied().min_by_key(|s| s.rtt_us)
    }

    pub fn rtt_us(&self) -> Option<u64> {
        self.samples.back().map(|s| s.rtt_us)
    }

    /// Converts a sender timestamp to the viewer clock.
    pub fn server_to_client(&self, server_us: u64) -> Option<u64> {
        let s = self.best()?;
        Some((server_us as i64 - s.offset_us).max(0) as u64)
    }
}
