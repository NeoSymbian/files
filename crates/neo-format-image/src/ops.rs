// SPDX-License-Identifier: Apache-2.0
//! The image editor's operations (files2-edit-image) as a recipe: a turn, a crop and a final size. The editor shows
//! them on a screen-sized preview; saving applies them to the full image as a [`View`], read row by row, so only a
//! resize allocates a second (smaller) image.

use crate::Pixels;
use crate::shrink::{Rows, Turn, resize};

/// Crop units: 1/10000 of the turned image's side.
pub const UNIT: u32 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edit {
    pub turn: Turn,
    /// x, y, w, h in [`UNIT`]s of the turned image.
    pub crop: [u32; 4],
    /// The saved size, after the crop.
    pub size: Option<(u32, u32)>,
}

impl Default for Edit {
    fn default() -> Self {
        Edit::NONE
    }
}

impl Edit {
    pub const NONE: Edit = Edit {
        turn: Turn::NONE,
        crop: [0, 0, UNIT, UNIT],
        size: None,
    };

    pub fn is_none(&self) -> bool {
        *self == Edit::NONE
    }

    /// A quarter turn clockwise; the crop turns with the image.
    pub fn rotate(self) -> Edit {
        let [x, y, w, h] = self.crop;
        Edit {
            turn: self.turn.then(Turn {
                rot: 1,
                flip: false,
            }),
            crop: [UNIT - (y + h), x, h, w],
            size: self.size.map(|(w, h)| (h, w)),
        }
    }

    /// A mirror left↔right of what is shown.
    pub fn flip(self) -> Edit {
        let [x, y, w, h] = self.crop;
        Edit {
            turn: self.turn.then(Turn { rot: 0, flip: true }),
            crop: [UNIT - (x + w), y, w, h],
            size: self.size,
        }
    }

    /// The crop in pixels of a `w`×`h` turned image (at least 1×1).
    pub fn crop_px(&self, w: u32, h: u32) -> [u32; 4] {
        let px = |v: u32, n: u32| (v as u64 * n as u64 / UNIT as u64) as u32;
        let [x, y, cw, ch] = self.crop;
        let (x0, y0) = (px(x, w).min(w - 1), px(y, h).min(h - 1));
        let (x1, y1) = (
            px(x + cw, w).clamp(x0 + 1, w),
            px(y + ch, h).clamp(y0 + 1, h),
        );
        [x0, y0, x1 - x0, y1 - y0]
    }

    /// The saved size of a stored `w`×`h` image whose own orientation is `base`.
    pub fn out_size(&self, base: Turn, w: u32, h: u32) -> (u32, u32) {
        if let Some(s) = self.size {
            return s;
        }
        let (tw, th) = base.then(self.turn).size(w, h);
        let [_, _, cw, ch] = self.crop_px(tw, th);
        (cw, ch)
    }
}

/// The stored image `src` as the recipe shows it (before any resize): turned, then cropped.
pub struct View<'a> {
    src: &'a Pixels,
    turn: Turn,
    crop: [u32; 4],
}

impl<'a> View<'a> {
    /// `base`: the image's own orientation (EXIF), applied before the edit's turn.
    pub fn new(src: &'a Pixels, base: Turn, e: &Edit) -> View<'a> {
        let turn = base.then(e.turn);
        let (tw, th) = turn.size(src.w, src.h);
        View {
            src,
            turn,
            crop: e.crop_px(tw, th),
        }
    }
}

impl Rows for View<'_> {
    fn w(&self) -> u32 {
        self.crop[2]
    }
    fn h(&self) -> u32 {
        self.crop[3]
    }
    fn ch(&self) -> u8 {
        self.src.ch
    }
    fn row(&self, y: u32, out: &mut [u8]) {
        let c = self.src.ch as usize;
        let [x0, y0, w, _] = self.crop;
        for x in 0..w {
            let (sx, sy) = self.turn.src(x0 + x, y0 + y, self.src.w, self.src.h);
            let s = (sy as usize * self.src.w as usize + sx as usize) * c;
            out[x as usize * c..x as usize * c + c].copy_from_slice(&self.src.px[s..s + c]);
        }
    }
}

/// The recipe applied to `src` (a copy): the preview, and what a save writes when it resizes.
pub fn apply(src: &Pixels, base: Turn, e: &Edit) -> Pixels {
    let v = View::new(src, base, e);
    match e.size {
        Some((w, h)) => resize(&v, w, h),
        None => {
            let mut out = Pixels::new(v.w(), v.h(), v.ch());
            let n = v.w() as usize * v.ch() as usize;
            for y in 0..v.h() {
                v.row(y, &mut out.px[y as usize * n..(y as usize + 1) * n]);
            }
            out
        }
    }
}
