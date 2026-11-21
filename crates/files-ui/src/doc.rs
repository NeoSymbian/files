// SPDX-License-Identifier: GPL-3.0-only
//! Documents (FA10): the glyph advances Slint draws, for neo-format-document's layout, and laid-out items as the
//! models `DocItems` draws.
//!
//! Slint's software renderer keeps each glyph's advance at one SDF size `T` (26.6 fixed point, chosen by the compiler
//! from every font size in the UI) and draws it at `P` px as `(P · adv_T / T + 32) / 64` (i-slint-compiler
//! `embed_glyphs.rs`, i-slint-renderer-software `pixelfont.rs`). The build script measured the fonts' advances;
//! [`Fonts::calibrate`] finds `T` once from a probe run, so lines break where Slint's text really ends.

use crate::{DImage, DRect, DText, colour};
use alloc::vec::Vec;
use neo_format_document::layout::{Face, Item, Measure};
use slint::{Image, ModelRc, VecModel};

mod table {
    include!(concat!(env!("OUT_DIR"), "/advances.rs"));
}

/// The document faces at the renderer's SDF size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fonts {
    t: i32,
}

/// The calibration runs: every script, each face, two sizes.
const SAMPLE: &str = "Ծրագիր 2026 · Нагрузка Ελλάδα — Office “plan”";

impl Fonts {
    /// The advance of `c` stored at SDF size `t` (26.6), as the compiler rounds it.
    fn stored(c: char, face: Face, t: i32) -> Option<i32> {
        let i = table::ADVANCES.binary_search_by_key(&c, |e| e.0).ok()?;
        let (_, adv, ink) = table::ADVANCES[i];
        let f = face as usize;
        if adv[f] == 0xffff {
            return None;
        }
        let num = adv[f] as i64 * t as i64 * 64;
        let upem = table::UPEM[f] as i64;
        // a glyph with an outline is rounded, a blank one (space) truncated; exact, as UPEM is a power of two
        Some(if ink & (1 << f) != 0 {
            (num + upem / 2) / upem
        } else {
            num / upem
        } as i32)
    }

    fn width_at(s: &str, face: Face, px: i32, t: i32) -> i32 {
        s.chars()
            .map(|c| Self::stored(c, face, t).map_or(px, |x| (px * x / t + 32) / 64))
            .sum()
    }

    /// Find the SDF size from `probe` (Slint's width of a run in a face at px): the first `T` that gives every
    /// sample's width. `None` when none does (the fonts or Slint changed: the layout would drift).
    pub fn calibrate(probe: impl Fn(&str, Face, i32) -> i32) -> Option<Fonts> {
        // many sizes: a few give the same widths under two sizes of T
        let mut runs: Vec<(Face, i32, i32)> = Vec::new();
        for face in [Face::Regular, Face::Bold, Face::Italic, Face::Mono] {
            for px in [20, 27, 33, 39, 45, 51, 66, 78] {
                runs.push((face, px, probe(SAMPLE, face, px)));
            }
        }
        let mut found = (8..=512).filter(|&t| {
            runs.iter()
                .all(|&(f, px, w)| Self::width_at(SAMPLE, f, px, t) == w)
        });
        match (found.next(), found.next()) {
            (Some(t), None) => Some(Fonts { t }),
            _ => None,
        }
    }

    /// The SDF size the files-ui build has (what [`calibrate`](Self::calibrate) finds there); for a fallback.
    pub const fn guess() -> Fonts {
        Fonts { t: 16 }
    }

    /// A run's width as Slint draws it.
    pub fn width(&self, s: &str, face: Face, px: i32) -> i32 {
        Self::width_at(s, face, px, self.t)
    }
}

impl Measure for Fonts {
    fn advance(&self, c: char, face: Face, px: i32) -> Option<i32> {
        // the mono face lacks Armenian: those show as the box, as Slint has no mono glyph for them either
        Self::stored(c, face, self.t).map(|x| (px * x / self.t + 32) / 64)
    }
    fn ascent(&self, face: Face, px: i32) -> i32 {
        let f = face as usize;
        px * table::ASCENT[f] as i32 / table::UPEM[f] as i32
    }
}

/// Items as the three models `DocItems` draws, `ink` for text without a colour of its own; `image(id)` is a decoded
/// picture (or `None` while it decodes).
pub fn models<'a>(
    items: impl Iterator<Item = &'a Item>,
    dy: i32,
    ink: u32,
    image: impl Fn(usize) -> Option<Image>,
) -> (ModelRc<DText>, ModelRc<DImage>, ModelRc<DRect>) {
    let (mut texts, mut images, mut rects) = (Vec::new(), Vec::new(), Vec::new());
    for i in items {
        match i {
            Item::Text {
                x,
                y,
                px,
                face,
                text,
                colour: c,
                ..
            } => texts.push(DText {
                x: *x,
                y: y - dy,
                px: *px,
                face: *face as i32,
                text: text.as_str().into(),
                colour: colour(c.unwrap_or(ink)),
            }),
            Item::Image { x, y, w, h, id } => {
                if let Some(image) = image(*id) {
                    images.push(DImage {
                        x: *x,
                        y: y - dy,
                        w: *w,
                        h: *h,
                        image,
                    });
                }
            }
            Item::Rect {
                x,
                y,
                w,
                h,
                colour: c,
            } => rects.push(DRect {
                x: *x,
                y: y - dy,
                w: *w,
                h: *h,
                colour: colour(*c),
            }),
        }
    }
    (
        ModelRc::new(VecModel::from(texts)),
        ModelRc::new(VecModel::from(images)),
        ModelRc::new(VecModel::from(rects)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_has_the_scripts() {
        assert!(table::ADVANCES.windows(2).all(|w| w[0].0 < w[1].0));
        for c in ['A', 'ж', 'Ծ', 'ω', '—', '“', '€', '□', 'Ґ'] {
            assert!(Fonts::stored(c, Face::Regular, 16).is_some(), "{c}");
            assert!(Fonts::stored(c, Face::Bold, 16).is_some(), "{c}");
        }
        assert!(Fonts::stored('中', Face::Regular, 16).is_none());
    }
}
