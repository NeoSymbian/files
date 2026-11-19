// SPDX-License-Identifier: Apache-2.0
//! Turning and scaling, in integers (the user side is soft-float): EXIF orientation and the editor's rotate / flip as
//! one [`Turn`], a box filter for big reductions ([`fit`], [`boxed`]) and bilinear for the last step ([`resize`]).

use crate::Pixels;
use alloc::vec;

/// A mirror (left↔right) first, then quarter turns clockwise: the eight EXIF orientations and every rotate / flip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Turn {
    pub rot: u8,
    pub flip: bool,
}

impl Turn {
    pub const NONE: Turn = Turn {
        rot: 0,
        flip: false,
    };

    /// EXIF orientation 1–8 (anything else is 1).
    pub fn exif(o: u8) -> Turn {
        let (rot, flip) = match o {
            2 => (0, true),
            3 => (2, false),
            4 => (2, true),
            5 => (3, true),
            6 => (1, false),
            7 => (1, true),
            8 => (3, false),
            _ => (0, false),
        };
        Turn { rot, flip }
    }

    /// `self`, then `next`.
    pub fn then(self, next: Turn) -> Turn {
        if next.flip {
            Turn {
                rot: (next.rot + 4 - self.rot % 4) % 4,
                flip: !self.flip,
            }
        } else {
            Turn {
                rot: (self.rot + next.rot) % 4,
                flip: self.flip,
            }
        }
    }

    pub fn swaps(self) -> bool {
        self.rot % 2 == 1
    }

    /// The size after the turn of a `w`×`h` image.
    pub fn size(self, w: u32, h: u32) -> (u32, u32) {
        if self.swaps() { (h, w) } else { (w, h) }
    }

    /// The stored pixel shown at `(x, y)`; `w`×`h` is the stored size.
    #[inline]
    pub fn src(self, x: u32, y: u32, w: u32, h: u32) -> (u32, u32) {
        let (px, py) = match self.rot % 4 {
            0 => (x, y),
            1 => (y, h - 1 - x),
            2 => (w - 1 - x, h - 1 - y),
            _ => (w - 1 - y, x),
        };
        (if self.flip { w - 1 - px } else { px }, py)
    }

    /// Where the stored pixel `(x, y)` shows.
    #[inline]
    pub fn dst(self, x: u32, y: u32, w: u32, h: u32) -> (u32, u32) {
        let px = if self.flip { w - 1 - x } else { x };
        match self.rot % 4 {
            0 => (px, y),
            1 => (h - 1 - y, px),
            2 => (w - 1 - px, h - 1 - y),
            _ => (y, w - 1 - px),
        }
    }
}

/// Anything that hands out rows: [`Pixels`], or a turned and cropped view of them ([`crate::ops::View`]).
pub trait Rows {
    fn w(&self) -> u32;
    fn h(&self) -> u32;
    fn ch(&self) -> u8;
    /// Row `y` into `out` (`w × ch` bytes).
    fn row(&self, y: u32, out: &mut [u8]);
}

impl<B: AsRef<[u8]>> Rows for Pixels<B> {
    fn w(&self) -> u32 {
        self.w
    }
    fn h(&self) -> u32 {
        self.h
    }
    fn ch(&self) -> u8 {
        self.ch
    }
    fn row(&self, y: u32, out: &mut [u8]) {
        let n = self.w as usize * self.ch as usize;
        out.copy_from_slice(&self.px.as_ref()[y as usize * n..(y as usize + 1) * n]);
    }
}

/// Averages `k`×`k` blocks (the edges take what is there): `ceil(w/k)`×`ceil(h/k)`, read row by row.
pub fn boxed(src: &impl Rows, k: u32) -> Pixels {
    boxed_turned(src, k, Turn::NONE)
}

/// [`boxed`], each block written where `turn` puts it (no second buffer for the turn).
pub fn boxed_turned(src: &impl Rows, k: u32, turn: Turn) -> Pixels {
    let (w, h, ch) = (src.w(), src.h(), src.ch() as usize);
    let k = k.max(1);
    let (gw, gh) = (w.div_ceil(k), h.div_ceil(k));
    let (tw, th) = turn.size(gw, gh);
    let mut out = Pixels::new(tw, th, ch as u8);
    let mut row = vec![0u8; w as usize * ch];
    let mut sums = vec![0u32; gw as usize * ch];
    for by in 0..gh {
        sums.fill(0);
        let y0 = by * k;
        let y1 = (y0 + k).min(h);
        for y in y0..y1 {
            src.row(y, &mut row);
            for x in 0..w as usize {
                let s = (x / k as usize) * ch;
                for c in 0..ch {
                    sums[s + c] += row[x * ch + c] as u32;
                }
            }
        }
        let rows = y1 - y0;
        for bx in 0..gw {
            let cols = k.min(w - bx * k);
            let n = rows * cols;
            let (dx, dy) = turn.dst(bx, by, gw, gh);
            let o = (dy as usize * tw as usize + dx as usize) * ch;
            for c in 0..ch {
                let i = bx as usize * ch + c;
                out.px[o + c] = ((sums[i] + n / 2) / n) as u8;
            }
        }
    }
    out
}

/// The image turned and box-shrunk so its long side is at most `max`: the viewer's and the thumbnails' first step.
/// Reads `src` in order and writes each block where the turn puts it, so a 12 MP image is walked once.
pub fn fit<B: AsRef<[u8]>>(src: &Pixels<B>, turn: Turn, max: u32) -> Pixels {
    let k = src.w.max(src.h).div_ceil(max.max(1)).max(1);
    boxed_turned(src, k, turn)
}

/// `src` turned (a copy).
pub fn turned<B: AsRef<[u8]>>(src: &Pixels<B>, turn: Turn) -> Pixels {
    let sp = src.px.as_ref();
    let (tw, th) = turn.size(src.w, src.h);
    let mut out = Pixels::new(tw, th, src.ch);
    let c = src.ch as usize;
    for y in 0..src.h {
        for x in 0..src.w {
            let (dx, dy) = turn.dst(x, y, src.w, src.h);
            let s = (y as usize * src.w as usize + x as usize) * c;
            let d = (dy as usize * tw as usize + dx as usize) * c;
            out.px[d..d + c].copy_from_slice(&sp[s..s + c]);
        }
    }
    out
}

/// Bilinear to exactly `w`×`h` (16.16 fixed point, pixel centres aligned). For reductions over 2×, [`resize`].
pub fn bilinear<B: AsRef<[u8]>>(src: &Pixels<B>, w: u32, h: u32) -> Pixels {
    let sp = src.px.as_ref();
    let (w, h) = (w.max(1), h.max(1));
    let mut out = Pixels::new(w, h, src.ch);
    let c = src.ch as usize;
    let (sw, sh) = (src.w as i64, src.h as i64);
    // the source coordinate of the destination centre, in 1/65536
    let map = |d: u32, dn: u32, sn: i64| -> (usize, usize, u32) {
        let f =
            ((2 * d as i64 + 1) * sn * 65536 / (2 * dn as i64) - 32768).clamp(0, (sn - 1) * 65536);
        let i = (f >> 16) as usize;
        (i, (i + 1).min(sn as usize - 1), (f & 0xFFFF) as u32)
    };
    for y in 0..h {
        let (y0, y1, fy) = map(y, h, sh);
        for x in 0..w {
            let (x0, x1, fx) = map(x, w, sw);
            let p =
                |xx: usize, yy: usize, ch: usize| sp[(yy * src.w as usize + xx) * c + ch] as u32;
            for ch in 0..c {
                let top = p(x0, y0, ch) * (65536 - fx) + p(x1, y0, ch) * fx;
                let bot = p(x0, y1, ch) * (65536 - fx) + p(x1, y1, ch) * fx;
                // top and bot are < 2^24; scale them down before the second weight
                let v = ((top >> 8) as u64 * (65536 - fy) as u64
                    + (bot >> 8) as u64 * fy as u64
                    + (1 << 23))
                    >> 24;
                out.px[(y as usize * w as usize + x as usize) * c + ch] = v as u8;
            }
        }
    }
    out
}

/// Any size to exactly `w`×`h`: a box filter down to within 2× of it, then bilinear.
pub fn resize(src: &impl Rows, w: u32, h: u32) -> Pixels {
    let k = (src.w() / w.max(1)).min(src.h() / h.max(1)).max(1);
    let b = boxed(src, k);
    if b.w == w && b.h == h {
        b
    } else {
        bilinear(&b, w, h)
    }
}

/// A square thumbnail `side` px across: turned, the middle of the short side kept.
pub fn thumb<B: AsRef<[u8]>>(src: &Pixels<B>, turn: Turn, side: u32) -> Pixels {
    let k = (src.w.min(src.h) / side.max(1)).max(1);
    let g = boxed_turned(src, k, turn);
    let s = g.w.min(g.h);
    let (x0, y0) = ((g.w - s) / 2, (g.h - s) / 2);
    let c = g.ch as usize;
    let mut sq = Pixels::new(s, s, g.ch);
    for y in 0..s {
        let a = ((y + y0) as usize * g.w as usize + x0 as usize) * c;
        sq.px[y as usize * s as usize * c..(y as usize + 1) * s as usize * c]
            .copy_from_slice(&g.px[a..a + s as usize * c]);
    }
    if s == side {
        sq
    } else {
        bilinear(&sq, side, side)
    }
}
