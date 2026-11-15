//! The Mac's mouse cursor, shown as vdview's own (OS) cursor while input goes to the Mac.
//!
//! The Mac normally draws its cursor into the video, so it moves 20-30 ms behind the mouse. While
//! forwarding, vdview asks the Mac to leave it out (CursorMode) and the Mac sends the cursor's
//! picture whenever it changes (CursorShape: arrow, I-beam, hand ...). vdview hands that picture
//! to the OS as the window's cursor, which then moves with no delay at all.

use anyhow::{Result, bail};

/// Largest side the Mac sends (keeps a shape inside one control message).
pub const MAX_SIDE: u16 = 120;

/// One cursor picture, in the Mac's virtual-display pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub width: u16,
    pub height: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    /// Straight (not premultiplied) sRGB RGBA, row by row.
    pub rgba: Vec<u8>,
}

/// A shape resampled for the window: what the OS cursor is built from.
#[derive(Clone, Debug, PartialEq)]
pub struct Scaled {
    pub width: u16,
    pub height: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    pub rgba: Vec<u8>,
}

impl Shape {
    /// Body of CursorShape: u16 width, height, hot_x, hot_y, then width * height * 4 bytes.
    pub fn decode(b: &[u8]) -> Result<Shape> {
        if b.len() < 8 {
            bail!("short CursorShape");
        }
        let u = |i: usize| u16::from_be_bytes([b[i], b[i + 1]]);
        let (width, height, hot_x, hot_y) = (u(0), u(2), u(4), u(6));
        if width > MAX_SIDE || height > MAX_SIDE {
            bail!("CursorShape too large ({width}x{height})");
        }
        let rgba = &b[8..];
        if rgba.len() != width as usize * height as usize * 4 {
            bail!("CursorShape size mismatch");
        }
        Ok(Shape {
            width,
            height,
            hot_x: hot_x.min(width.saturating_sub(1)),
            hot_y: hot_y.min(height.saturating_sub(1)),
            rgba: rgba.to_vec(),
        })
    }

    /// The Mac hides its cursor (an empty shape).
    pub fn hidden(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Resampled by `scale` (window pixels per Mac pixel). Several samples per output pixel when
    /// shrinking, so thin outlines do not drop out.
    pub fn scaled(&self, scale: f64) -> Scaled {
        let scale = scale.clamp(0.05, 8.0);
        if self.hidden() || (scale - 1.0).abs() < 1e-3 {
            return Scaled {
                width: self.width,
                height: self.height,
                hot_x: self.hot_x,
                hot_y: self.hot_y,
                rgba: self.rgba.clone(),
            };
        }
        let (sw, sh) = (self.width as usize, self.height as usize);
        let dw = ((sw as f64 * scale).round() as usize).clamp(1, 256);
        let dh = ((sh as f64 * scale).round() as usize).clamp(1, 256);
        let (fx, fy) = (sw as f64 / dw as f64, sh as f64 / dh as f64);
        let n = (fx.max(fy).ceil() as usize).clamp(1, 4);

        // Premultiplied copy, so transparent pixels do not bleed their colour into the edges.
        let pm: Vec<[f64; 4]> = self
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                let a = p[3] as f64 / 255.0;
                [p[0] as f64 * a, p[1] as f64 * a, p[2] as f64 * a, p[3] as f64]
            })
            .collect();
        let at = |x: isize, y: isize| -> [f64; 4] {
            if x < 0 || y < 0 || x >= sw as isize || y >= sh as isize {
                [0.0; 4]
            } else {
                pm[y as usize * sw + x as usize]
            }
        };
        let bilinear = |x: f64, y: f64| -> [f64; 4] {
            let (x, y) = (x - 0.5, y - 0.5);
            let (x0, y0) = (x.floor(), y.floor());
            let (tx, ty) = (x - x0, y - y0);
            let (x0, y0) = (x0 as isize, y0 as isize);
            let mut out = [0.0; 4];
            for (dx, dy, w) in [
                (0, 0, (1.0 - tx) * (1.0 - ty)),
                (1, 0, tx * (1.0 - ty)),
                (0, 1, (1.0 - tx) * ty),
                (1, 1, tx * ty),
            ] {
                let p = at(x0 + dx, y0 + dy);
                for c in 0..4 {
                    out[c] += p[c] * w;
                }
            }
            out
        };

        let mut rgba = Vec::with_capacity(dw * dh * 4);
        for y in 0..dh {
            for x in 0..dw {
                let mut acc = [0.0; 4];
                for j in 0..n {
                    for i in 0..n {
                        let sx = (x as f64 + (i as f64 + 0.5) / n as f64) * fx;
                        let sy = (y as f64 + (j as f64 + 0.5) / n as f64) * fy;
                        let p = bilinear(sx, sy);
                        for c in 0..4 {
                            acc[c] += p[c];
                        }
                    }
                }
                let k = (n * n) as f64;
                let a = acc[3] / k;
                let un = |v: f64| {
                    if a < 0.5 {
                        0
                    } else {
                        (v / k / (a / 255.0)).round().clamp(0.0, 255.0) as u8
                    }
                };
                rgba.extend_from_slice(&[
                    un(acc[0]),
                    un(acc[1]),
                    un(acc[2]),
                    a.round().clamp(0.0, 255.0) as u8,
                ]);
            }
        }
        let hot = |h: u16, size: usize| ((h as f64 * scale).round() as usize).min(size - 1) as u16;
        Scaled {
            width: dw as u16,
            height: dh as u16,
            hot_x: hot(self.hot_x, dw),
            hot_y: hot(self.hot_y, dh),
            rgba,
        }
    }
}

/// Window pixels per video pixel (the video is fitted into the window keeping its aspect ratio).
pub fn video_scale((win_w, win_h): (u32, u32), (video_w, video_h): (u16, u16)) -> f64 {
    if video_w == 0 || video_h == 0 || win_w == 0 || win_h == 0 {
        return 1.0;
    }
    (win_w as f64 / video_w as f64).min(win_h as f64 / video_h as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(w: u16, h: u16, hx: u16, hy: u16, rgba: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        for v in [w, h, hx, hy] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        b.extend_from_slice(rgba);
        b
    }

    #[test]
    fn decode_checks_sizes() {
        let px = [255u8, 0, 0, 255].repeat(4);
        let s = Shape::decode(&body(2, 2, 1, 5, &px)).unwrap();
        assert_eq!((s.width, s.height, s.hot_x, s.hot_y), (2, 2, 1, 1)); // hotspot clamped
        assert!(Shape::decode(&body(2, 2, 0, 0, &px[..12])).is_err());
        assert!(Shape::decode(&body(121, 1, 0, 0, &[0; 484])).is_err());
        assert!(Shape::decode(&body(0, 0, 0, 0, &[])).unwrap().hidden());
    }

    #[test]
    fn scaling_keeps_colour_and_moves_the_hotspot() {
        // 4x4 opaque white with a transparent border column; hotspot at (2, 2).
        let mut px = Vec::new();
        for _y in 0..4 {
            for x in 0..4 {
                px.extend_from_slice(if x == 3 { &[0, 0, 0, 0] } else { &[255, 255, 255, 255] });
            }
        }
        let s = Shape::decode(&body(4, 4, 2, 2, &px)).unwrap();
        assert_eq!(s.scaled(1.0).rgba, s.rgba);
        let half = s.scaled(0.5);
        assert_eq!((half.width, half.height, half.hot_x, half.hot_y), (2, 2, 1, 1));
        assert_eq!(&half.rgba[..4], &[255, 255, 255, 255]); // inside: unchanged
        assert_eq!(&half.rgba[4..7], &[255, 255, 255]); // edge: colour kept, only alpha drops
        assert!(half.rgba[7] < 255 && half.rgba[7] > 0);
        let double = s.scaled(2.0);
        assert_eq!((double.width, double.hot_x), (8, 4));
    }

    #[test]
    fn video_scale_fits_the_window() {
        assert_eq!(video_scale((1720, 720), (3440, 1440)), 0.5);
        assert_eq!(video_scale((3440, 1440), (3440, 1440)), 1.0);
    }
}
