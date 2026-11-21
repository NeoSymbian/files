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
            Item::Image { x, y, w, h, id } => match image(*id) {
                Some(image) => images.push(DImage {
                    x: *x,
                    y: y - dy,
                    w: *w,
                    h: *h,
                    image,
                }),
                // still decoding (or it can't be): a quiet box where it goes
                None => rects.push(DRect {
                    x: *x,
                    y: y - dy,
                    w: *w,
                    h: *h,
                    colour: colour(PLACE),
                }),
            },
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

use neo_format_document::layout::{Flow, Opts, Page, layout, shift};
use neo_format_document::{Doc, PageSetup};

/// How a document shows (files2-view-docx's pill).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The file's pages at its page size, on grey.
    Page = 0,
    /// One long sheet at the reader's text size.
    Reflow = 1,
}

/// Text sizes the reader steps through (dp); documents start at 15, books at 17.
pub const SIZES: [f32; 7] = [13.0, 15.0, 17.0, 19.0, 22.0, 26.0, 30.0];

pub const PAPER: u32 = 0xf7f5f0;
/// Where a picture goes while it decodes.
pub const PLACE: u32 = 0xe6e2d8;
pub const INK: u32 = 0x33373d;

/// A document as DocView shows it: laid out at the view's width, in Page or Reflow mode, a window of it at a time.
pub struct DocViewer {
    pub doc: Doc,
    pub mode: Mode,
    /// Into [`SIZES`].
    pub size: usize,
    /// The page view's zoom, 1.0 = the page's width fits the view.
    pub zoom: f32,
    fonts: Fonts,
    dp: f32,
    view: (i32, i32),
    flow: Flow,
    /// The content's width, px (wider than the view when a page is zoomed).
    width: i32,
    /// The images' own sizes (`None`: not shown).
    sizes: Vec<Option<(i32, i32)>>,
    pages: Vec<Page>,
    /// Where the window starts, px.
    pub window_top: i32,
    /// The scroll position, px.
    pub top: i32,
}

impl DocViewer {
    pub fn new(doc: Doc, fonts: Fonts, dp: f32, mode: Mode) -> Self {
        let n = doc.images.len();
        DocViewer {
            doc,
            mode,
            size: 1,
            zoom: 1.0,
            fonts,
            dp,
            view: (0, 0),
            flow: Flow::default(),
            width: 0,
            sizes: alloc::vec![None; n],
            pages: Vec::new(),
            window_top: 0,
            top: 0,
        }
    }

    /// An image's own size, before (or after) a layout.
    pub fn set_size(&mut self, id: usize, size: Option<(i32, i32)>) {
        if let Some(s) = self.sizes.get_mut(id) {
            *s = size;
        }
    }

    pub fn pages(&self) -> usize {
        self.pages.len()
    }

    /// Lay out again for a view of `w` × `h` px (after a size, mode or zoom change, or a new view size).
    pub fn layout(&mut self, w: i32, h: i32) {
        self.view = (w.max(100), h.max(100));
        let dp = self.dp;
        let gap = (12.0 * dp) as i32;
        let sizes = self.sizes.clone();
        let size = move |id: usize| sizes.get(id).copied().flatten();
        match self.mode {
            Mode::Reflow => {
                let pad = (22.0 * dp) as i32;
                let body = (SIZES[self.size] * dp) as i32;
                let body_hp = if self.doc.body == 0 {
                    24
                } else {
                    self.doc.body
                };
                let o = Opts {
                    width: self.view.0 - 2 * pad,
                    body,
                    twip: body as f32 / (body_hp as f32 * 10.0),
                    absolute: false,
                    gap,
                    max_image_h: 0,
                    image_scale: dp / 2.0,
                };
                let mut f = layout(&self.doc, o, &self.fonts, size);
                shift(&mut f.items, pad, pad);
                let height = f.height + 2 * pad;
                self.flow = Flow::from_items(f.items, height);
                self.width = self.view.0;
                self.pages.clear();
            }
            Mode::Page => {
                let p: PageSetup = self.doc.page;
                let sheet_w = ((self.view.0 - 2 * gap) as f32 * self.zoom) as i32;
                let twip = sheet_w as f32 / p.width as f32;
                let o = Opts {
                    width: (p.text_width() as f32 * twip) as i32,
                    body: (self.doc.body.max(2) as f32 * 10.0 * twip) as i32,
                    twip,
                    absolute: true,
                    gap: (120.0 * twip) as i32,
                    max_image_h: (p.text_height() as f32 * twip) as i32,
                    image_scale: twip * 15.0,
                };
                let f = layout(&self.doc, o, &self.fonts, size);
                let text_h = (p.text_height() as f32 * twip) as i32;
                self.pages = f.pages(text_h);
                let sheet_h = (p.height as f32 * twip) as i32;
                let (ml, mt) = (
                    (p.margin[3] as f32 * twip) as i32,
                    (p.margin[0] as f32 * twip) as i32,
                );
                let x0 = ((self.view.0 - sheet_w) / 2).max(gap);
                let mut items = Vec::new();
                for (i, pg) in self.pages.iter().enumerate() {
                    let y = gap + i as i32 * (sheet_h + gap);
                    items.push(Item::Rect {
                        x: x0,
                        y,
                        w: sheet_w,
                        h: sheet_h,
                        colour: 0xffffff,
                    });
                    let mut page: Vec<Item> = f.page(pg).cloned().collect();
                    shift(&mut page, x0 + ml, y + mt - pg.top);
                    items.extend(page);
                }
                let total = gap + self.pages.len() as i32 * (sheet_h + gap);
                self.flow = Flow::from_items(items, total);
                self.width = (2 * x0 + sheet_w).max(self.view.0);
            }
        }
        self.top = self.top.min((self.flow.height - self.view.1).max(0));
        self.window_top = i32::MIN;
    }

    pub fn total(&self) -> i32 {
        self.flow.height
    }

    /// The content's width (the view's, or a zoomed page's).
    pub fn width(&self) -> i32 {
        self.width
    }

    /// The pictures the window shows, and the size each is drawn at (px): what to decode, at most that big.
    pub fn wanted(&self) -> Vec<(usize, i32, i32)> {
        let mut out: Vec<(usize, i32, i32)> = Vec::new();
        for i in self.window() {
            if let Item::Image { id, w, h, .. } = *i {
                match out.iter_mut().find(|o| o.0 == id) {
                    Some(o) => *o = (id, o.1.max(w), o.2.max(h)),
                    None => out.push((id, w, h)),
                }
            }
        }
        out
    }

    /// Scrolled to `top`: whether the window must move (the caller then sets new models).
    pub fn scroll(&mut self, top: i32) -> bool {
        self.top = top.max(0);
        let h = self.view.1;
        let need = self.window_top == i32::MIN
            || top < self.window_top
            || top + h > self.window_top + 3 * h;
        if need {
            self.window_top = (top - h).max(0);
        }
        need
    }

    /// The items of the window, relative to its top.
    pub fn window(&self) -> impl Iterator<Item = &Item> {
        let t = self.window_top.max(0);
        self.flow.window(t, t + 4 * self.view.1)
    }

    /// "12 %": how far down the view is.
    pub fn percent(&self) -> alloc::string::String {
        let room = (self.flow.height - self.view.1).max(1);
        alloc::format!(
            "{} %",
            (self.top.max(0) as i64 * 100 / room as i64).clamp(0, 100)
        )
    }

    /// The models for DocView: `image(id)` gives a decoded picture.
    pub fn models(
        &self,
        image: impl Fn(usize) -> Option<Image>,
    ) -> (ModelRc<DText>, ModelRc<DImage>, ModelRc<DRect>) {
        models(self.window(), self.window_top.max(0), INK, image)
    }
}

/// Put `v` on the screen (view "doc"); `image(id)` gives a decoded picture.
pub fn show(
    ui: &crate::FilesApp,
    v: &DocViewer,
    info: &str,
    image: impl Fn(usize) -> Option<Image>,
) {
    let (t, i, r) = v.models(image);
    ui.set_doc_mode(v.mode as i32);
    ui.set_doc_texts(t);
    ui.set_doc_images(i);
    ui.set_doc_rects(r);
    ui.set_doc_total(v.total());
    ui.set_doc_total_w(v.width());
    ui.set_doc_window_top(v.window_top.max(0));
    ui.set_doc_paper(colour(PAPER));
    ui.set_doc_info(info.into());
    ui.set_doc_percent(v.percent().into());
}

/// "Word document · 48 KB".
pub fn info(family: neo_view::Family, size: u64) -> alloc::string::String {
    alloc::format!("{} · {}", family.name(), files_core::size_text(size))
}
