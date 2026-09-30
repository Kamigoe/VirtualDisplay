//! Wire format of docs/protocol.md (v1). All integers are big-endian.

use anyhow::{Result, bail};

pub const VERSION: u16 = 1;
pub const DEFAULT_PORT: u16 = 7777;

pub const MSG_HELLO: u8 = 0x01;
pub const MSG_START: u8 = 0x02;
pub const MSG_KEYFRAME_REQUEST: u8 = 0x03;
pub const MSG_PING: u8 = 0x04;
pub const MSG_PONG: u8 = 0x05;
pub const MSG_ERROR: u8 = 0x7F;

pub const VIDEO_MAGIC: u8 = 0x56;
pub const KEEPALIVE_MAGIC: u8 = 0x4B;
pub const NACK_MAGIC: u8 = 0x4E;

pub const VIDEO_HEADER_SIZE: usize = 28;
pub const MAX_DATAGRAM: usize = 1400;
pub const PAYLOAD_SIZE: usize = MAX_DATAGRAM - VIDEO_HEADER_SIZE;

pub const FLAG_KEYFRAME: u8 = 1 << 0;

pub const CODEC_HEVC: u8 = 1;

#[derive(Clone, Copy, Debug)]
pub struct Hello {
    /// Logical size in points; 0 = keep the sender's current mode.
    pub width: u16,
    pub height: u16,
    pub hidpi: bool,
    pub refresh_mhz: u32,
    pub chroma444: bool,
    pub bitrate_kbps: u32,
}

impl Hello {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(20);
        b.extend_from_slice(&VERSION.to_be_bytes());
        b.extend_from_slice(&self.width.to_be_bytes());
        b.extend_from_slice(&self.height.to_be_bytes());
        b.push(self.hidpi as u8);
        b.extend_from_slice(&self.refresh_mhz.to_be_bytes());
        b.push(self.chroma444 as u8);
        b.extend_from_slice(&self.bitrate_kbps.to_be_bytes());
        b
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Start {
    pub session_id: u32,
    pub pixel_width: u16,
    pub pixel_height: u16,
    pub refresh_mhz: u32,
    pub codec: u8,
    pub chroma444: bool,
}

impl Start {
    pub fn decode(body: &[u8]) -> Result<Self> {
        let mut r = Reader(body);
        let version = r.u16()?;
        if version != VERSION {
            bail!("sender speaks protocol v{version}, this viewer speaks v{VERSION}");
        }
        Ok(Start {
            session_id: r.u32()?,
            pixel_width: r.u16()?,
            pixel_height: r.u16()?,
            refresh_mhz: r.u32()?,
            codec: r.u8()?,
            chroma444: r.u8()? == 1,
        })
    }
}

/// Frames a control message: u32 length + u8 type + body.
pub fn control_message(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(5 + body.len());
    m.extend_from_slice(&(body.len() as u32 + 1).to_be_bytes());
    m.push(kind);
    m.extend_from_slice(body);
    m
}

#[derive(Clone, Copy, Debug)]
pub struct VideoHeader {
    pub flags: u8,
    pub session_id: u32,
    pub frame_id: u32,
    pub packet_index: u16,
    pub packet_count: u16,
    pub frame_bytes: u32,
    pub capture_us: u64,
}

impl VideoHeader {
    pub fn parse(p: &[u8]) -> Option<Self> {
        if p.len() < VIDEO_HEADER_SIZE || p[0] != VIDEO_MAGIC || u16::from(p[1]) != VERSION {
            return None;
        }
        let mut r = Reader(&p[2..]);
        let flags = r.u8().ok()?;
        r.u8().ok()?;
        Some(VideoHeader {
            flags,
            session_id: r.u32().ok()?,
            frame_id: r.u32().ok()?,
            packet_index: r.u16().ok()?,
            packet_count: r.u16().ok()?,
            frame_bytes: r.u32().ok()?,
            capture_us: r.u64().ok()?,
        })
    }
}

pub fn keepalive(session_id: u32) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[0] = KEEPALIVE_MAGIC;
    b[1] = VERSION as u8;
    b[4..8].copy_from_slice(&session_id.to_be_bytes());
    b
}

pub fn nack(session_id: u32, frame_id: u32, missing: &[u16]) -> Vec<u8> {
    let mut b = Vec::with_capacity(12 + missing.len() * 2);
    b.push(NACK_MAGIC);
    b.push(VERSION as u8);
    b.extend_from_slice(&(missing.len() as u16).to_be_bytes());
    b.extend_from_slice(&session_id.to_be_bytes());
    b.extend_from_slice(&frame_id.to_be_bytes());
    for m in missing {
        b.extend_from_slice(&m.to_be_bytes());
    }
    b
}

pub struct Reader<'a>(pub &'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        if self.0.len() < N {
            bail!("message truncated");
        }
        let (head, rest) = self.0.split_at(N);
        self.0 = rest;
        Ok(head.try_into().unwrap())
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take()?))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take()?))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take()?))
    }
}
