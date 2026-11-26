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

/// The colour sums (x 256): limited-range luma x 298, and each chroma value's part of red, green and blue. Plain
/// arithmetic, no tables, so the turned path's loops vectorize.
struct Colour {
    rv: i32,
    gu: i32,
    gv: i32,
    bu: i32,
}

impl Colour {
    fn new(hd: bool) -> Self {
        // BT.709 for HD, BT.601 below
        let (rv, gu, gv, bu) = if hd {
            (459, 55, 136, 541)
        } else {
            (409, 100, 208, 516)
        };
        Colour { rv, gu, gv, bu }
    }

    #[inline(always)]
    fn px(&self, y: u32, u: u8, v: u8) -> u32 {
        let l = (y as i32 - 16) * 298 + 128;
        let (u, v) = (i32::from(u) - 128, i32::from(v) - 128);
        let r = (l + self.rv * v) >> 8;
        let g = (l - self.gu * u - self.gv * v) >> 8;
        let b = (l + self.bu * u) >> 8;
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
    draw_rows(f, turn, dst, stride, w, h, 0..h);
}

/// The screen rows `part` of [`draw`]'s rectangle only, `dst` starting at row `part.start`: two threads can draw one
/// picture into disjoint rows (`split_at_mut`), with the same bytes as one `draw`.
pub fn draw_rows<P: Px>(
    f: &Frame,
    turn: Turn,
    dst: &mut [P],
    stride: usize,
    w: u32,
    h: u32,
    part: core::ops::Range<u32>,
) {
    let part = part.start.min(h) as usize..part.end.min(h) as usize;
    if w == 0 || part.is_empty() || f.width == 0 || f.height == 0 {
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
            for (dy, ty) in rows[part].iter().enumerate() {
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
            // In bands of BAND screen rows (BAND source columns): every source row is blended along x once, at the
            // band's columns; each screen column is then a vertical blend of two of those, BAND pixels side by side,
            // which the compiler vectorizes. The same sums as `luma`, so the same bytes. Gathering four samples per
            // pixel took 37 ms for 1080x1920 on an A75 (Pixel, 2026-11-26).
            const BAND: usize = 32;
            let fh = f.height as usize;
            let mut hb = alloc::vec![0u16; fh * BAND];
            let (mut ub, mut vb) = (alloc::vec![0u8; ch * BAND], alloc::vec![0u8; ch * BAND]);
            let mut out = [0u32; BAND];
            for (b, band) in rows[part].chunks(BAND).enumerate() {
                let n = band.len();
                for r in 0..fh {
                    let src = &f.y[r * ys..];
                    for (hv, tx) in hb[r * BAND..r * BAND + n].iter_mut().zip(band) {
                        // at most 255 x 256: fits
                        *hv = (u32::from(src[tx.i0 as usize]) * (256 - tx.w)
                            + u32::from(src[tx.i1 as usize]) * tx.w)
                            as u16;
                    }
                }
                for r in 0..ch {
                    for (k, tx) in band.iter().enumerate() {
                        let x = (tx.i0 as usize / 2).min(cw - 1);
                        ub[r * BAND + k] = f.u[r * us + x];
                        vb[r * BAND + k] = f.v[r * vs + x];
                    }
                }
                for (dx, ty) in ys_.iter().enumerate() {
                    let t0 = &hb[ty.i0 as usize * BAND..][..n];
                    let t1 = &hb[ty.i1 as usize * BAND..][..n];
                    let cr = (ty.i0 as usize / 2).min(ch - 1) * BAND;
                    let (u, v) = (&ub[cr..cr + n], &vb[cr..cr + n]);
                    let wy = ty.w;
                    for (k, o) in out[..n].iter_mut().enumerate() {
                        let l = (u32::from(t0[k]) * (256 - wy) + u32::from(t1[k]) * wy) >> 16;
                        *o = c.px(l, u[k], v[k]);
                    }
                    for (k, o) in out[..n].iter().enumerate() {
                        dst[(b * BAND + k) * stride + dx] = P::from_xrgb(*o);
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

    /// The per-pixel conversion `draw` started from (colour tables, four luma samples gathered per pixel).
    fn reference(f: &Frame, turn: Turn, dst: &mut [u32], stride: usize, w: u32, h: u32) {
        let hd = f.height >= 720 || f.width >= 1280;
        let (rv, gu, gv, bu) = if hd {
            (459, 55, 136, 541)
        } else {
            (409, 100, 208, 516)
        };
        let (mut ty_, mut trv, mut tgu, mut tgv, mut tbu) =
            ([0i32; 256], [0; 256], [0; 256], [0; 256], [0; 256]);
        for i in 0..256 {
            let d = i as i32 - 128;
            ty_[i] = (i as i32 - 16) * 298 + 128;
            (trv[i], tgu[i], tgv[i], tbu[i]) = (rv * d, gu * d, gv * d, bu * d);
        }
        let px = |y: u32, u: u8, v: u8| {
            let l = ty_[y as usize];
            let r = (l + trv[v as usize]) >> 8;
            let g = (l - tgu[u as usize] - tgv[v as usize]) >> 8;
            let b = (l + tbu[u as usize]) >> 8;
            (r.clamp(0, 255) as u32) << 16 | (g.clamp(0, 255) as u32) << 8 | b.clamp(0, 255) as u32
        };
        let (cw, ch) = (f.chroma_width() as usize, f.chroma_height() as usize);
        let at = |x: &Tap, y: &Tap| {
            let (r0, r1) = (y.i0 as usize * f.y_stride, y.i1 as usize * f.y_stride);
            let s = |i: usize| u32::from(f.y[i]);
            let top = s(r0 + x.i0 as usize) * (256 - x.w) + s(r0 + x.i1 as usize) * x.w;
            let bottom = s(r1 + x.i0 as usize) * (256 - x.w) + s(r1 + x.i1 as usize) * x.w;
            let i = (y.i0 as usize / 2).min(ch - 1) * f.uv_stride + (x.i0 as usize / 2).min(cw - 1);
            px((top * (256 - y.w) + bottom * y.w) >> 16, f.u[i], f.v[i])
        };
        if matches!(turn, Turn::None | Turn::Half) {
            let rev = turn == Turn::Half;
            let (xs, rows) = (taps(w, f.width, rev), taps(h, f.height, rev));
            for (dy, ty) in rows.iter().enumerate() {
                for (dx, tx) in xs.iter().enumerate() {
                    dst[dy * stride + dx] = at(tx, ty);
                }
            }
        } else {
            let q = turn == Turn::Quarter;
            let (ys, rows) = (taps(w, f.height, q), taps(h, f.width, !q));
            for (dy, tx) in rows.iter().enumerate() {
                for (dx, ty) in ys.iter().enumerate() {
                    dst[dy * stride + dx] = at(tx, ty);
                }
            }
        }
    }

    /// Random pictures of odd and even sizes, scaled up and down, every turn: the same bytes as the reference.
    #[test]
    fn same_bytes_as_the_reference() {
        let mut seed = 7u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed >> 8
        };
        for (pw, ph, w, h) in [
            (64, 36, 64, 36),
            (37, 23, 80, 51),
            (128, 72, 45, 70),
            (160, 90, 300, 169),
            (33, 65, 17, 9),
        ] {
            let f = Frame {
                width: pw,
                height: ph,
                y: (0..(pw + 3) * ph).map(|_| rnd() as u8).collect(),
                u: (0..(pw.div_ceil(2) + 1) * ph.div_ceil(2))
                    .map(|_| rnd() as u8)
                    .collect(),
                v: (0..(pw.div_ceil(2) + 1) * ph.div_ceil(2))
                    .map(|_| rnd() as u8)
                    .collect(),
                y_stride: pw as usize + 3,
                uv_stride: pw.div_ceil(2) as usize + 1,
                pts_us: 0,
            };
            for t in [Turn::None, Turn::Quarter, Turn::Half, Turn::ThreeQuarters] {
                let stride = w as usize + 5;
                let (mut got, mut want) =
                    (vec![0; stride * h as usize], vec![0; stride * h as usize]);
                draw(&f, t, &mut got, stride, w, h);
                reference(&f, t, &mut want, stride, w, h);
                assert!(got == want, "{pw}x{ph} -> {w}x{h} {t:?}");
            }
        }
    }

    /// Drawn in two parts at any row, the same bytes as drawn whole.
    #[test]
    fn parts_make_the_whole() {
        let f = Frame {
            width: 64,
            height: 36,
            y: (0..64 * 36).map(|i| (i * 7 % 251) as u8).collect(),
            u: (0..32 * 18).map(|i| (i * 13 % 251) as u8).collect(),
            v: (0..32 * 18).map(|i| (i * 29 % 251) as u8).collect(),
            y_stride: 64,
            uv_stride: 32,
            pts_us: 0,
        };
        let (w, h, stride) = (50u32, 90u32, 53usize);
        for t in [Turn::None, Turn::Quarter, Turn::Half, Turn::ThreeQuarters] {
            let mut whole = vec![0u32; stride * h as usize];
            draw(&f, t, &mut whole, stride, w, h);
            for k in [1, 31, 32, 45, 89] {
                let mut parts = vec![0u32; stride * h as usize];
                let (top, bottom) = parts.split_at_mut(k as usize * stride);
                draw_rows(&f, t, top, stride, w, h, 0..k);
                draw_rows(&f, t, bottom, stride, w, h, k..h);
                assert!(parts == whole, "{t:?} split at {k}");
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
