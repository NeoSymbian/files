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

/// Source positions for each destination step along one axis: the sample index and the weight of the next one (0..256).
fn steps(dst: u32, src: u32) -> Vec<(u32, u32)> {
    let (dst, src) = (u64::from(dst.max(1)), u64::from(src.max(1)));
    (0..dst)
        .map(|i| {
            // centres line up: (i + 0.5) * src / dst - 0.5, in 1/256ths
            let p = ((2 * i + 1) * src * 256 / (2 * dst)).saturating_sub(128);
            let (n, f) = ((p >> 8) as u32, (p & 255) as u32);
            if n + 1 >= src as u32 {
                (src as u32 - 1, 0)
            } else {
                (n, f)
            }
        })
        .collect()
}

/// Draws `f` scaled into the `w` x `h` rectangle at `dst` (rows `stride` pixels apart), turned by `turn`.
pub fn draw(f: &Frame, turn: Turn, dst: &mut [u32], stride: usize, w: u32, h: u32) {
    if w == 0 || h == 0 || f.width == 0 || f.height == 0 {
        return;
    }
    let hd = f.height >= 720 || f.width >= 1280;
    // (r from v, g from u, g from v, b from u) x 256, and luma x 298 / 256 for limited range
    let (rv, gu, gv, bu) = if hd {
        (459, 55, 136, 541)
    } else {
        (409, 100, 208, 516)
    };
    // a destination column steps along the source's x (no turn, half) or y (quarter turns)
    let (cols, rows) = match turn {
        Turn::None | Turn::Half => (steps(w, f.width), steps(h, f.height)),
        _ => (steps(w, f.height), steps(h, f.width)),
    };
    let (cw, ch) = (f.chroma_width() as usize, f.chroma_height() as usize);
    let luma = |x: u32, y: u32| u32::from(f.y[y as usize * f.y_stride + x as usize]);
    for (dy, &(rn, rf)) in rows.iter().enumerate() {
        let line = &mut dst[dy * stride..dy * stride + w as usize];
        for (dx, &(cn, cf)) in cols.iter().enumerate() {
            // the source point: (x, x weight, y, y weight)
            let (x, xf, y, yf) = match turn {
                Turn::None => (cn, cf, rn, rf),
                Turn::Half => (f.width - 1 - cn, 0, f.height - 1 - rn, 0),
                // clockwise: the screen's top row is the picture's left column, read bottom up
                Turn::Quarter => (rn, rf, f.height - 1 - cn, 0),
                Turn::ThreeQuarters => (f.width - 1 - rn, 0, cn, cf),
            };
            let x1 = (x + 1).min(f.width - 1);
            let y1 = (y + 1).min(f.height - 1);
            let top = luma(x, y) * (256 - xf) + luma(x1, y) * xf;
            let bottom = luma(x, y1) * (256 - xf) + luma(x1, y1) * xf;
            let yv = (top * (256 - yf) + bottom * yf) >> 16;
            let ci = (y as usize / 2).min(ch - 1) * f.uv_stride + (x as usize / 2).min(cw - 1);
            let (u, v) = (i32::from(f.u[ci]) - 128, i32::from(f.v[ci]) - 128);
            let l = (yv as i32 - 16) * 298;
            let r = (l + rv * v + 128) >> 8;
            let g = (l - gu * u - gv * v + 128) >> 8;
            let b = (l + bu * u + 128) >> 8;
            line[dx] = (r.clamp(0, 255) as u32) << 16
                | (g.clamp(0, 255) as u32) << 8
                | b.clamp(0, 255) as u32;
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
