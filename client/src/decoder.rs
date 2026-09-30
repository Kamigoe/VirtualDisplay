//! Safe wrapper over the C shim in csrc/vd_decoder.c.

use std::ffi::{CStr, CString, c_char, c_int};
use std::ptr;

use anyhow::{Result, anyhow};

#[repr(C)]
struct RawDecoder {
    _private: [u8; 0],
}

#[repr(C)]
struct RawFrame {
    width: c_int,
    height: c_int,
    layout: c_int,
    full_range: c_int,
    data: [*const u8; 3],
    linesize: [c_int; 3],
}

unsafe extern "C" {
    fn vd_decoder_open(
        out: *mut *mut RawDecoder,
        hwaccel: *const c_char,
        threads: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn vd_decoder_decode(
        d: *mut RawDecoder,
        data: *const u8,
        size: c_int,
        out: *mut RawFrame,
    ) -> c_int;
    fn vd_decoder_describe(d: *const RawDecoder) -> *const c_char;
    fn vd_decoder_error_string(err: c_int, buf: *mut c_char, len: usize);
    fn vd_decoder_close(d: *mut RawDecoder);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Nv12,
    I420,
    I444,
}

pub struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: u32,
    pub width: u32,
    pub height: u32,
}

/// A decoded picture borrowing the decoder's buffers (valid until the next `decode`).
pub struct Picture<'a> {
    pub width: u32,
    pub height: u32,
    pub layout: Layout,
    pub full_range: bool,
    pub planes: Vec<Plane<'a>>,
}

pub struct Decoder(*mut RawDecoder);

// The decoder is used from one thread at a time.
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn open(hwaccel: &str, threads: i32) -> Result<Self> {
        let hw = CString::new(hwaccel)?;
        let mut err = [0 as c_char; 256];
        let mut d = ptr::null_mut();
        let r =
            unsafe { vd_decoder_open(&mut d, hw.as_ptr(), threads, err.as_mut_ptr(), err.len()) };
        if r < 0 {
            let msg = unsafe { CStr::from_ptr(err.as_ptr()) }.to_string_lossy();
            return Err(anyhow!("opening HEVC decoder: {msg}"));
        }
        Ok(Decoder(d))
    }

    pub fn decode(&mut self, data: &[u8]) -> Result<Option<Picture<'_>>> {
        let mut f = RawFrame {
            width: 0,
            height: 0,
            layout: 0,
            full_range: 0,
            data: [ptr::null(); 3],
            linesize: [0; 3],
        };
        let r = unsafe { vd_decoder_decode(self.0, data.as_ptr(), data.len() as c_int, &mut f) };
        if r < 0 {
            let mut buf = [0 as c_char; 256];
            unsafe { vd_decoder_error_string(r, buf.as_mut_ptr(), buf.len()) };
            return Err(anyhow!(
                "decode failed: {}",
                unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy()
            ));
        }
        if r == 0 {
            return Ok(None);
        }
        let (w, h) = (f.width as u32, f.height as u32);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (layout, dims): (Layout, &[(u32, u32, u32)]) = match f.layout {
            // (width in texels, height, bytes per texel)
            0 => (Layout::Nv12, &[(w, h, 1), (cw, ch, 2)]),
            1 => (Layout::I420, &[(w, h, 1), (cw, ch, 1), (cw, ch, 1)]),
            2 => (Layout::I444, &[(w, h, 1), (w, h, 1), (w, h, 1)]),
            other => return Err(anyhow!("unknown layout {other}")),
        };
        let planes = dims
            .iter()
            .enumerate()
            .map(|(i, &(pw, ph, bpp))| {
                let stride = f.linesize[i] as u32;
                assert!(stride >= pw * bpp, "unexpected plane stride");
                // Last row only needs its visible bytes.
                let len = (stride * (ph - 1) + pw * bpp) as usize;
                Plane {
                    data: unsafe { std::slice::from_raw_parts(f.data[i], len) },
                    stride,
                    width: pw,
                    height: ph,
                }
            })
            .collect();
        Ok(Some(Picture {
            width: w,
            height: h,
            layout,
            full_range: f.full_range != 0,
            planes,
        }))
    }

    pub fn describe(&self) -> String {
        unsafe { CStr::from_ptr(vd_decoder_describe(self.0)) }
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { vd_decoder_close(self.0) };
    }
}
