// SPDX-License-Identifier: Apache-2.0
//! A decoded picture (8-bit 4:2:0) into an XRGB8888 rectangle, in one pass: colour conversion, a bilinear scale to the
//! rectangle and a quarter turn (rotate, FA12). Luma is filtered; chroma, already half size, is taken nearest.
//! Limited-range BT.709 for HD sizes (720 lines and up), BT.601 below, as players guess without colour metadata.

use super::video::Frame;
use alloc::vec::Vec;

/// How the picture is turned on its way to the screen, clockwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Turn {
    #[default]
    None,
    Quarter,
    Half,
    ThreeQuarters,
}

impl Turn {
    /// The picture's size on screen before scaling: width and height swap on a quarter turn.
    pub fn size(self, w: u32, h: u32) -> (u32, u32) {
        match self {
            Turn::Quarter | Turn::ThreeQuarters => (h, w),
            _ => (w, h),
        }
    }
}

/// The largest rectangle of the picture's shape that fits `(w, h)`, centred: (x, y, width, height).
pub fn fit(picture: (u32, u32), turn: Turn, w: u32, h: u32) -> (u32, u32, u32, u32) {
    let (pw, ph) = turn.size(picture.0.max(1), picture.1.max(1));
    let (fw, fh) = if u64::from(pw) * u64::from(h) > u64::from(ph) * u64::from(w) {
        (w, (u64::from(ph) * u64::from(w) / u64::from(pw)) as u32)
    } else {
        ((u64::from(pw) * u64::from(h) / u64::from(ph)) as u32, h)
    };
    ((w - fw) / 2, (h - fh) / 2, fw.max(1), fh.max(1))
}

/// A bilinear tap along one axis: the two samples and the second one's weight (0..256).
#[derive(Clone, Copy)]
struct Tap {
    i0: u32,
    i1: u32,
    w: u32,
}

/// The taps for `dst` steps over `src` samples, centres lined up; `reverse` reads the axis backwards.
fn taps(dst: u32, src: u32, reverse: bool) -> Vec<Tap> {
    let (d, n) = (u64::from(dst.max(1)), u64::from(src.max(1)));
    let last = src.max(1) - 1;
    (0..d)
        .map(|i| {
            // (i + 0.5) * src / dst - 0.5, in 1/256ths
            let p = ((2 * i + 1) * n * 256 / (2 * d)).saturating_sub(128);
            let (i0, w) = (((p >> 8) as u32).min(last), (p & 255) as u32);
            let i1 = (i0 + 1).min(last);
            if reverse {
                Tap {
                    i0: last - i0,
                    i1: last - i1,
                    w,
                }
            } else {
                Tap { i0, i1, w }
            }
        })
        .collect()
}

/// The colour tables: limited-range luma x 298, and each chroma value's part of red, green and blue (x 256).
struct Colour {
    y: [i32; 256],
    rv: [i32; 256],
    g: [[i32; 256]; 2],
    bu: [i32; 256],
}

impl Colour {
    fn new(hd: bool) -> Self {
        // (r from v, g from u, g from v, b from u) x 256: BT.709 for HD, BT.601 below
        let (rv, gu, gv, bu) = if hd {
            (459, 55, 136, 541)
        } else {
            (409, 100, 208, 516)
        };
        let mut c = Colour {
            y: [0; 256],
            rv: [0; 256],
            g: [[0; 256]; 2],
            bu: [0; 256],
        };
        for i in 0..256 {
            let d = i as i32 - 128;
            c.y[i] = (i as i32 - 16) * 298 + 128;
            c.rv[i] = rv * d;
            c.g[0][i] = gu * d;
            c.g[1][i] = gv * d;
            c.bu[i] = bu * d;
        }
        c
    }

    #[inline(always)]
    fn px(&self, y: u32, u: u8, v: u8) -> u32 {
        let l = self.y[y as usize];
        let r = (l + self.rv[v as usize]) >> 8;
        let g = (l - self.g[0][u as usize] - self.g[1][v as usize]) >> 8;
        let b = (l + self.bu[u as usize]) >> 8;
        (r.clamp(0, 255) as u32) << 16 | (g.clamp(0, 255) as u32) << 8 | b.clamp(0, 255) as u32
    }
}

/// A pixel `draw` can write: XRGB8888 (`u32`, the app's surface) or packed RGB (`[u8; 3]`, a Slint image's bytes).
pub trait Px: Copy {
    fn from_xrgb(v: u32) -> Self;
}

impl Px for u32 {
    #[inline(always)]
    fn from_xrgb(v: u32) -> Self {
        v
    }
}

impl Px for [u8; 3] {
    #[inline(always)]
    fn from_xrgb(v: u32) -> Self {
        [(v >> 16) as u8, (v >> 8) as u8, v as u8]
    }
}

/// Draws `f` scaled into the `w` x `h` rectangle at `dst` (rows `stride` pixels apart), turned by `turn`.
pub fn draw<P: Px>(f: &Frame, turn: Turn, dst: &mut [P], stride: usize, w: u32, h: u32) {
    if w == 0 || h == 0 || f.width == 0 || f.height == 0 {
        return;
    }
    let c = Colour::new(f.height >= 720 || f.width >= 1280);
    let (cw, ch) = (
        f.chroma_width().max(1) as usize,
        f.chroma_height().max(1) as usize,
    );
    let (ys, us, vs) = (f.y_stride, f.uv_stride, f.uv_stride);
    let luma = |x: &Tap, y: &Tap| -> u32 {
        let r0 = y.i0 as usize * ys;
        let r1 = y.i1 as usize * ys;
        let (a, b) = (
            u32::from(f.y[r0 + x.i0 as usize]),
            u32::from(f.y[r0 + x.i1 as usize]),
        );
        let (cc, d) = (
            u32::from(f.y[r1 + x.i0 as usize]),
            u32::from(f.y[r1 + x.i1 as usize]),
        );
        let top = a * (256 - x.w) + b * x.w;
        let bottom = cc * (256 - x.w) + d * x.w;
        (top * (256 - y.w) + bottom * y.w) >> 16
    };
    let chroma = |x: &Tap, y: &Tap| -> (u8, u8) {
        let i = (y.i0 as usize / 2).min(ch - 1) * us + (x.i0 as usize / 2).min(cw - 1);
        let j = (y.i0 as usize / 2).min(ch - 1) * vs + (x.i0 as usize / 2).min(cw - 1);
        (f.u[i], f.v[j])
    };
    match turn {
        // each screen row is one source row
        Turn::None | Turn::Half => {
            let rev = turn == Turn::Half;
            let (xs, rows) = (taps(w, f.width, rev), taps(h, f.height, rev));
            for (dy, ty) in rows.iter().enumerate() {
                let line = &mut dst[dy * stride..dy * stride + w as usize];
                for (d, tx) in line.iter_mut().zip(&xs) {
                    let (u, v) = chroma(tx, ty);
                    *d = P::from_xrgb(c.px(luma(tx, ty), u, v));
                }
            }
        }
        // each screen row is one source column: clockwise, the top row is the picture's left column read bottom up
        Turn::Quarter | Turn::ThreeQuarters => {
            let q = turn == Turn::Quarter;
            let (ys_, rows) = (taps(w, f.height, q), taps(h, f.width, !q));
            // in bands of screen rows, each screen column inside: the source is read along its rows (a column at a
            // time it missed the cache on every pixel: 41 ms for 1080x1920 on an A75)
            const BAND: usize = 32;
            for (b, band) in rows.chunks(BAND).enumerate() {
                for (dx, ty) in ys_.iter().enumerate() {
                    for (k, tx) in band.iter().enumerate() {
                        let (u, v) = chroma(tx, ty);
                        dst[(b * BAND + k) * stride + dx] = P::from_xrgb(c.px(luma(tx, ty), u, v));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A 4x2 picture: left half white, right half black; chroma neutral.
    fn halves() -> Frame {
        Frame {
            width: 4,
            height: 2,
            y: vec![235, 235, 16, 16, 235, 235, 16, 16],
            u: vec![128, 128],
            v: vec![128, 128],
            y_stride: 4,
            uv_stride: 2,
            pts_us: 0,
        }
    }

    #[test]
    fn white_and_black_stay() {
        let mut d = vec![0; 8];
        draw(&halves(), Turn::None, &mut d, 4, 4, 2);
        assert_eq!(d[0], 0xffffff);
        assert_eq!(d[3], 0x000000);
    }

    #[test]
    fn a_quarter_turn_puts_the_left_on_top() {
        // 2 wide, 4 high on screen: clockwise, the picture's left (white) half becomes the top
        let mut d = vec![0; 8];
        draw(&halves(), Turn::Quarter, &mut d, 2, 2, 4);
        assert_eq!(d[0], 0xffffff);
        assert_eq!(d[7], 0x000000);
        let mut d = vec![0; 8];
        draw(&halves(), Turn::ThreeQuarters, &mut d, 2, 2, 4);
        assert_eq!((d[0], d[7]), (0x000000, 0xffffff));
    }

    #[test]
    fn colours() {
        // BT.601 red (Y 81, U 90, V 240)
        let f = Frame {
            width: 2,
            height: 2,
            y: vec![81; 4],
            u: vec![90],
            v: vec![240],
            y_stride: 2,
            uv_stride: 1,
            pts_us: 0,
        };
        let mut d = vec![0; 4];
        draw(&f, Turn::None, &mut d, 2, 2, 2);
        let (r, g, b) = (d[0] >> 16, d[0] >> 8 & 255, d[0] & 255);
        assert!(r > 245 && g < 10 && b < 10, "{r} {g} {b}");
    }

    #[test]
    fn fitting() {
        // 1280x720 on a 1080-wide, 1800-high area: full width, 607 high, centred
        assert_eq!(
            fit((1280, 720), Turn::None, 1080, 1800),
            (0, 596, 1080, 607)
        );
        // turned, it fills the height
        assert_eq!(
            fit((1280, 720), Turn::Quarter, 1080, 1800),
            (34, 0, 1012, 1800)
        );
    }

    /// The tables agree with a plain float conversion to within rounding.
    #[test]
    fn close_to_the_formula() {
        let (w, h) = (64u32, 36u32);
        let mut seed = 1u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 24) as u8
        };
        let f = Frame {
            width: w,
            height: h,
            y: (0..w * h).map(|_| rnd().clamp(16, 235)).collect(),
            u: (0..w * h / 4).map(|_| rnd().clamp(16, 240)).collect(),
            v: (0..w * h / 4).map(|_| rnd().clamp(16, 240)).collect(),
            y_stride: w as usize,
            uv_stride: w as usize / 2,
            pts_us: 0,
        };
        let mut d = vec![0; (w * h) as usize];
        draw(&f, Turn::None, &mut d, w as usize, w, h);
        for y in 0..h as usize {
            for x in 0..w as usize {
                let (l, u, v) = (
                    f.y[y * 64 + x] as f64,
                    f.u[y / 2 * 32 + x / 2] as f64,
                    f.v[y / 2 * 32 + x / 2] as f64,
                );
                let l = (l - 16.0) * 1.164;
                let want = [
                    l + 1.596 * (v - 128.0),
                    l - 0.391 * (u - 128.0) - 0.813 * (v - 128.0),
                    l + 2.018 * (u - 128.0),
                ];
                let got = [
                    d[y * 64 + x] >> 16 & 255,
                    d[y * 64 + x] >> 8 & 255,
                    d[y * 64 + x] & 255,
                ];
                for (g, w) in got.iter().zip(want) {
                    assert!(
                        (f64::from(*g) - w.clamp(0.0, 255.0)).abs() <= 2.0,
                        "{x},{y}: {got:?} {want:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn scaling_keeps_the_edges_inside() {
        let mut d = vec![0; 1080 * 607];
        let f = Frame {
            width: 1280,
            height: 720,
            y: vec![128; 1280 * 720],
            u: vec![128; 640 * 360],
            v: vec![128; 640 * 360],
            y_stride: 1280,
            uv_stride: 640,
            pts_us: 0,
        };
        for t in [Turn::None, Turn::Quarter, Turn::Half, Turn::ThreeQuarters] {
            let (w, h) = t.size(1080, 607);
            draw(&f, t, &mut d, w as usize, w, h);
        }
    }
}
