// SPDX-License-Identifier: Apache-2.0
//! Our rasteriser (the owner's decision: no rasteriser crate). The signed-area coverage algorithm of font-rs and
//! ab_glyph_rasterizer, written here: every line adds, to the cells it crosses, the signed area it covers and, to the
//! cell after, the rest of its height; [`Raster::alpha`] takes the running sum over the whole buffer, so a closed
//! outline nets to zero at each row end. Curves are flattened into lines first. Any winding is ink (the sum is taken
//! as `|sum|` clamped to 1), which fonts never notice.
//!
//! Every write is bounds-checked and coordinates are clipped, so a corrupt outline draws garbage, never panics.

use alloc::vec;
use alloc::vec::Vec;

/// The flattening tolerance, in pixels.
const TOLERANCE: f32 = 0.3;
/// At most this many lines a curve (a corrupt font's control points can be far apart).
const MAX_SEGMENTS: usize = 64;

pub struct Raster {
    w: usize,
    h: usize,
    acc: Vec<f32>,
}

type P = (f32, f32);

impl Raster {
    pub fn new(w: usize, h: usize) -> Self {
        Raster {
            w,
            h,
            // +2: a line ending exactly on the right edge adds to the cell after the last one
            acc: vec![0.0; w * h + 2],
        }
    }

    pub fn width(&self) -> usize {
        self.w
    }

    pub fn height(&self) -> usize {
        self.h
    }

    fn add(&mut self, i: usize, v: f32) {
        if let Some(c) = self.acc.get_mut(i) {
            *c += v;
        }
    }

    pub fn line(&mut self, p0: P, p1: P) {
        if !(p0.0.is_finite() && p0.1.is_finite() && p1.0.is_finite() && p1.1.is_finite()) {
            return;
        }
        if (p0.1 - p1.1).abs() <= f32::EPSILON {
            return;
        }
        let (dir, p0, p1) = if p0.1 < p1.1 {
            (1.0, p0, p1)
        } else {
            (-1.0, p1, p0)
        };
        let dxdy = (p1.0 - p0.0) / (p1.1 - p0.1);
        let mut x = p0.0;
        if p0.1 < 0.0 {
            x -= p0.1 * dxdy;
        }
        let wf = self.w as f32;
        // `as usize` saturates: negative → 0
        let y0 = p0.1 as usize;
        let y1 = self.h.min(libm::ceilf(p1.1) as usize);
        for y in y0..y1 {
            let start = y * self.w;
            let dy = ((y + 1) as f32).min(p1.1) - (y as f32).max(p0.1);
            let xnext = x + dxdy * dy;
            let d = dy * dir;
            // clipped to the bitmap: ink left of it lands in column 0, right of it in the next row's column 0,
            // where the line's partner cancels it
            let (x0, x1) = if x < xnext { (x, xnext) } else { (xnext, x) };
            let (x0, x1) = (x0.clamp(0.0, wf), x1.clamp(0.0, wf));
            let x0floor = libm::floorf(x0);
            let x0i = x0floor as usize;
            let x1ceil = libm::ceilf(x1);
            let x1i = x1ceil as usize;
            if x1i <= x0i + 1 {
                let xmf = 0.5 * (x0 + x1) - x0floor;
                self.add(start + x0i, d - d * xmf);
                self.add(start + x0i + 1, d * xmf);
            } else {
                let s = 1.0 / (x1 - x0);
                let x0f = x0 - x0floor;
                let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
                let x1f = x1 - x1ceil + 1.0;
                let am = 0.5 * s * x1f * x1f;
                self.add(start + x0i, d * a0);
                if x1i == x0i + 2 {
                    self.add(start + x0i + 1, d * (1.0 - a0 - am));
                } else {
                    let a1 = s * (1.5 - x0f);
                    self.add(start + x0i + 1, d * (a1 - a0));
                    for xi in x0i + 2..x1i - 1 {
                        self.add(start + xi, d * s);
                    }
                    let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                    self.add(start + x1i - 1, d * (1.0 - a2 - am));
                }
                self.add(start + x1i, d * am);
            }
            x = xnext;
        }
    }

    pub fn quad(&mut self, p0: P, p1: P, p2: P) {
        // a quadratic's chord error with n steps is |p0 - 2p1 + p2| / (4n²)
        let dd = len((p0.0 - 2.0 * p1.0 + p2.0, p0.1 - 2.0 * p1.1 + p2.1));
        let n = segments(dd / (4.0 * TOLERANCE));
        let mut prev = p0;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let p = (
                u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0,
                u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1,
            );
            self.line(prev, p);
            prev = p;
        }
    }

    pub fn cubic(&mut self, p0: P, p1: P, p2: P, p3: P) {
        // a cubic's second derivative is at most 6·max|second difference|, its chord error that / (8n²)
        let dd = len((p0.0 - 2.0 * p1.0 + p2.0, p0.1 - 2.0 * p1.1 + p2.1))
            .max(len((p1.0 - 2.0 * p2.0 + p3.0, p1.1 - 2.0 * p2.1 + p3.1)));
        let n = segments(0.75 * dd / TOLERANCE);
        let mut prev = p0;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let (a, b, c, e) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            let p = (
                a * p0.0 + b * p1.0 + c * p2.0 + e * p3.0,
                a * p0.1 + b * p1.1 + c * p2.1 + e * p3.1,
            );
            self.line(prev, p);
            prev = p;
        }
    }

    /// The coverage, row by row, 0 (none) to 255 (full).
    pub fn alpha(self) -> Vec<u8> {
        let mut sum = 0.0f32;
        self.acc[..self.w * self.h]
            .iter()
            .map(|&a| {
                sum += a;
                (sum.abs().min(1.0) * 255.0 + 0.5) as u8
            })
            .collect()
    }
}

fn len(v: P) -> f32 {
    libm::sqrtf(v.0 * v.0 + v.1 * v.1)
}

/// Lines for a curve whose `n²` must reach `n2` (NaN or below 1 → 1).
fn segments(n2: f32) -> usize {
    (libm::ceilf(libm::sqrtf(n2)) as usize).clamp(1, MAX_SEGMENTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(r: &mut Raster, x0: f32, y0: f32, x1: f32, y1: f32) {
        r.line((x0, y0), (x1, y0));
        r.line((x1, y0), (x1, y1));
        r.line((x1, y1), (x0, y1));
        r.line((x0, y1), (x0, y0));
    }

    fn total(a: &[u8]) -> f32 {
        a.iter().map(|&v| v as f32 / 255.0).sum()
    }

    #[test]
    fn a_whole_pixel_square() {
        let mut r = Raster::new(10, 10);
        rect(&mut r, 2.0, 3.0, 6.0, 8.0);
        let a = r.alpha();
        for y in 0..10 {
            for x in 0..10 {
                let inside = (2..6).contains(&x) && (3..8).contains(&y);
                assert_eq!(a[y * 10 + x], if inside { 255 } else { 0 }, "{x},{y}");
            }
        }
        assert!((total(&a) - 20.0).abs() < 0.01);
    }

    #[test]
    fn half_pixel_edges() {
        let mut r = Raster::new(10, 10);
        rect(&mut r, 2.5, 2.5, 6.5, 6.5);
        let a = r.alpha();
        assert!((126..=129).contains(&a[2 * 10 + 4]), "top {}", a[24]);
        assert!((126..=129).contains(&a[4 * 10 + 2]), "left {}", a[42]);
        assert!((62..=66).contains(&a[2 * 10 + 2]), "corner {}", a[22]);
        assert_eq!(a[4 * 10 + 4], 255);
        assert!((total(&a) - 16.0).abs() < 0.05);
    }

    #[test]
    fn a_triangle_and_curves_cover_their_area() {
        let mut r = Raster::new(40, 40);
        r.line((5.0, 5.0), (35.0, 10.0));
        r.line((35.0, 10.0), (12.0, 33.0));
        r.line((12.0, 33.0), (5.0, 5.0));
        let area = 0.5 * ((35.0f32 - 5.0) * (33.0 - 5.0) - (12.0 - 5.0) * (10.0 - 5.0)).abs();
        let got = total(&r.alpha());
        assert!((got - area).abs() / area < 0.01, "{got} vs {area}");
        // a disc of radius 15 from four cubics (the 0.5523 circle): flattened within 0.3 px it is a 20-gon, 1.6% smaller

        let mut r = Raster::new(40, 40);
        let (c, rad, k) = (20.0f32, 15.0f32, 15.0 * 0.552_284_8);
        r.cubic(
            (c + rad, c),
            (c + rad, c + k),
            (c + k, c + rad),
            (c, c + rad),
        );
        r.cubic(
            (c, c + rad),
            (c - k, c + rad),
            (c - rad, c + k),
            (c - rad, c),
        );
        r.cubic(
            (c - rad, c),
            (c - rad, c - k),
            (c - k, c - rad),
            (c, c - rad),
        );
        r.cubic(
            (c, c - rad),
            (c + k, c - rad),
            (c + rad, c - k),
            (c + rad, c),
        );
        let want = core::f32::consts::PI * rad * rad;
        let got = total(&r.alpha());
        assert!((got - want).abs() / want < 0.02, "{got} vs {want}");
        // a parabolic segment closed by its chord: (2/3)·base·height (the curve's apex is half the control's height)
        let mut r = Raster::new(40, 40);
        r.quad((5.0, 30.0), (20.0, 0.0), (35.0, 30.0));
        r.line((35.0, 30.0), (5.0, 30.0));
        let want = 2.0 / 3.0 * 30.0 * 15.0;
        let got = total(&r.alpha());
        assert!((got - want).abs() / want < 0.02, "{got} vs {want}");
    }

    #[test]
    fn outside_and_nonsense_never_panic() {
        let mut r = Raster::new(8, 8);
        rect(&mut r, -100.0, -100.0, 300.0, 4.0);
        let a = r.alpha();
        assert!(a[..32].iter().all(|&v| v == 255) && a[32..].iter().all(|&v| v == 0));
        let mut r = Raster::new(8, 8);
        r.line((f32::NAN, 0.0), (1.0, 5.0));
        r.line((0.0, f32::INFINITY), (1.0, 5.0));
        r.cubic((1e30, -1e30), (-1e30, 1e30), (0.0, 0.0), (8.0, 8.0));
        r.quad((8.0, 0.0), (8.0, 8.0), (8.0, 0.0));
        r.line((8.0, 0.0), (8.0, 8.0));
        assert_eq!(r.alpha().len(), 64);
        Raster::new(0, 0).line((0.0, 0.0), (5.0, 5.0));
    }
}
