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
                    let mut page: Vec<Item> = f.page(*pg).cloned().collect();
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

// ── books (files2-view-epub) ─────────────────────────────────────────────────────────────────────────────────────

use neo_format_document::epub::Entry;
use neo_format_document::layout::{Pos, page_of};

/// A reading theme: its name, paper, ink, muted text, the progress bar's track and fill.
pub struct ReadTheme {
    pub name: &'static str,
    pub paper: u32,
    pub ink: u32,
    pub muted: u32,
    pub track: u32,
    pub fill: u32,
}

pub const THEMES: [ReadTheme; 3] = [
    ReadTheme {
        name: "Sepia",
        paper: 0xf4ecd8,
        ink: 0x3b3226,
        muted: 0x8a7a62,
        track: 0xddd0b3,
        fill: 0xa0703c,
    },
    ReadTheme {
        name: "Light",
        paper: 0xffffff,
        ink: 0x1d1f23,
        muted: 0x6b6f76,
        track: 0xe3e5e8,
        fill: 0x3d8bfd,
    },
    ReadTheme {
        name: "Dark",
        paper: 0x111214,
        ink: 0xd6d3cc,
        muted: 0x7d838c,
        track: 0x2b2e33,
        fill: 0xc9a46a,
    },
];

/// Books start at this size ([`SIZES`]: 17 dp, the board's).
pub const BOOK_SIZE: usize = 2;

/// The layout of a book's page: `w` × `h` px of text at size `size`.
pub fn book_opts(dp: f32, size: usize, w: i32, h: i32) -> Opts {
    let body = (SIZES[size.min(SIZES.len() - 1)] * dp) as i32;
    Opts {
        width: w.max(100),
        body,
        twip: body as f32 / 240.0,
        absolute: false,
        gap: (10.0 * dp) as i32,
        max_image_h: h.max(100),
        image_scale: dp,
    }
}

/// How many pages a chapter has at `o` (the counting worker's step; the same layout the reader shows).
pub fn count_pages(doc: &Doc, fonts: &Fonts, o: Opts, h: i32, sizes: &[Option<(i32, i32)>]) -> u32 {
    let f = layout(doc, o, fonts, |id| sizes.get(id).copied().flatten());
    f.pages(h).len() as u32
}

/// Where a turn goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    /// Another page of this chapter.
    Page,
    /// The chapter `i` must be read (its first page, or its last when going back).
    Chapter(usize, bool),
    /// The book's first or last page already.
    None,
}

/// A book open in the reader: the chapter in hand, cut into pages, and the place in it.
pub struct BookReader {
    pub title: String,
    pub toc: Vec<Entry>,
    pub chapters: usize,
    pub chapter: usize,
    pub page: usize,
    /// Into [`SIZES`] and [`THEMES`].
    pub size: usize,
    pub theme: usize,
    /// Pages of each chapter at this size and page size (from the counting worker), when known.
    pub counts: Vec<Option<u32>>,
    doc: Doc,
    sizes: Vec<Option<(i32, i32)>>,
    flow: Flow,
    pages: Vec<Page>,
    fonts: Fonts,
    dp: f32,
    view: (i32, i32),
}

use alloc::string::String;

impl BookReader {
    pub fn new(title: String, toc: Vec<Entry>, chapters: usize, fonts: Fonts, dp: f32) -> Self {
        BookReader {
            title,
            toc,
            chapters,
            chapter: 0,
            page: 0,
            size: BOOK_SIZE,
            theme: 0,
            counts: alloc::vec![None; chapters],
            doc: Doc::default(),
            sizes: Vec::new(),
            flow: Flow::default(),
            pages: Vec::new(),
            fonts,
            dp,
            view: (0, 0),
        }
    }

    /// The layout options of this reader's pages.
    pub fn opts(&self) -> Opts {
        book_opts(self.dp, self.size, self.view.0, self.view.1)
    }

    pub fn view(&self) -> (i32, i32) {
        self.view
    }

    pub fn fonts(&self) -> Fonts {
        self.fonts
    }

    pub fn dp(&self) -> f32 {
        self.dp
    }

    /// The pictures of the chapter in hand (their ids are what [`wanted`](Self::wanted) gives).
    pub fn images(&self) -> &[neo_format_document::ImageSrc] {
        &self.doc.images
    }

    /// Chapter `i` in hand (its blocks and its pictures' sizes), at `at`: a place in it, or its last page (`end`).
    pub fn set_chapter(
        &mut self,
        i: usize,
        doc: Doc,
        sizes: Vec<Option<(i32, i32)>>,
        at: Option<Pos>,
        end: bool,
    ) {
        self.chapter = i;
        self.doc = doc;
        self.sizes = sizes;
        self.relayout();
        self.page = match (at, end) {
            (_, true) => self.pages.len().saturating_sub(1),
            (Some(p), _) => page_of(&self.pages, p),
            _ => 0,
        };
    }

    /// The page area changed (or the size): lay the chapter out again, keeping the place.
    pub fn layout(&mut self, w: i32, h: i32) {
        let at = self.pos();
        let new = (w.max(100), h.max(100));
        if new != self.view {
            self.counts = alloc::vec![None; self.chapters];
        }
        self.view = new;
        self.relayout();
        self.page = page_of(&self.pages, at);
    }

    /// A new text size (counts start again).
    pub fn set_size(&mut self, size: usize) {
        let at = self.pos();
        self.size = size.min(SIZES.len() - 1);
        self.counts = alloc::vec![None; self.chapters];
        self.relayout();
        self.page = page_of(&self.pages, at);
    }

    fn relayout(&mut self) {
        let sizes = &self.sizes;
        self.flow = layout(&self.doc, self.opts(), &self.fonts, |id| {
            sizes.get(id).copied().flatten()
        });
        self.pages = self.flow.pages(self.view.1);
        self.counts[self.chapter] = Some(self.pages.len() as u32);
    }

    /// The place: the chapter and where its page starts.
    pub fn pos(&self) -> Pos {
        self.pages.get(self.page).map_or(Pos::default(), |p| p.at)
    }

    pub fn next_page(&mut self) -> Turn {
        if self.page + 1 < self.pages.len() {
            self.page += 1;
            Turn::Page
        } else if self.chapter + 1 < self.chapters {
            Turn::Chapter(self.chapter + 1, false)
        } else {
            Turn::None
        }
    }

    pub fn prev_page(&mut self) -> Turn {
        if self.page > 0 {
            self.page -= 1;
            Turn::Page
        } else if self.chapter > 0 {
            Turn::Chapter(self.chapter - 1, true)
        } else {
            Turn::None
        }
    }

    /// The page's items, from its top.
    pub fn page_items(&self) -> impl Iterator<Item = &Item> {
        let p = self.pages.get(self.page).copied().unwrap_or(Page {
            top: 0,
            end: 0,
            at: Pos::default(),
        });
        self.flow.page(p)
    }

    pub fn page_top(&self) -> i32 {
        self.pages.get(self.page).map_or(0, |p| p.top)
    }

    /// The pictures the page shows, and their drawn size.
    pub fn wanted(&self) -> Vec<(usize, i32, i32)> {
        self.page_items()
            .filter_map(|i| match *i {
                Item::Image { id, w, h, .. } => Some((id, w, h)),
                _ => None,
            })
            .collect()
    }

    /// How far into the book: by pages when every chapter is counted, else by chapters.
    pub fn progress(&self) -> f32 {
        let here = (self.page + 1) as f32 / self.pages.len().max(1) as f32;
        match self.total() {
            Some((before, total)) => (before + self.page as u32 + 1) as f32 / total.max(1) as f32,
            None => (self.chapter as f32 + here) / self.chapters.max(1) as f32,
        }
    }

    /// Pages before this chapter and in the whole book, once counted.
    fn total(&self) -> Option<(u32, u32)> {
        let all: Option<Vec<u32>> = self.counts.iter().copied().collect();
        let all = all?;
        Some((all[..self.chapter].iter().sum(), all.iter().sum()))
    }

    /// The footer: "Chapter 3 · 42 %" and "Page 118 of 280" (in the chapter until the book is counted).
    pub fn footer(&self) -> (String, String) {
        let name = self
            .toc
            .iter()
            .rev()
            .find(|e| e.chapter <= self.chapter)
            .map(|e| e.title.clone());
        let name = name
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| alloc::format!("Chapter {}", self.chapter + 1));
        let pct = (self.progress() * 100.0) as i32;
        let page = match self.total() {
            Some((before, total)) => {
                alloc::format!("Page {} of {}", before + self.page as u32 + 1, total)
            }
            None => alloc::format!(
                "Page {} of {} in the chapter",
                self.page + 1,
                self.pages.len()
            ),
        };
        (alloc::format!("{name} · {pct} %"), page)
    }

    /// The contents' entry the reader is in.
    pub fn toc_current(&self) -> usize {
        self.toc
            .iter()
            .rposition(|e| e.chapter <= self.chapter)
            .unwrap_or(0)
    }
}

/// Put the book's page on the screen (view "book"); `image(id)` gives a decoded picture.
pub fn show_book(ui: &crate::FilesApp, r: &BookReader, image: impl Fn(usize) -> Option<Image>) {
    let t = &THEMES[r.theme.min(THEMES.len() - 1)];
    let (texts, images, rects) = models(r.page_items(), r.page_top(), t.ink, image);
    ui.set_book_texts(texts);
    ui.set_book_images(images);
    ui.set_book_rects(rects);
    ui.set_book_title(r.title.as_str().into());
    let (left, right) = r.footer();
    ui.set_book_where(left.into());
    ui.set_book_page(right.into());
    ui.set_book_progress(r.progress());
    ui.set_book_paper(colour(t.paper));
    ui.set_book_muted(colour(t.muted));
    ui.set_book_track(colour(t.track));
    ui.set_book_fill(colour(t.fill));
    ui.set_book_theme_name(t.name.into());
    let titles: Vec<slint::SharedString> = r.toc.iter().map(|e| e.title.as_str().into()).collect();
    let depths: Vec<i32> = r.toc.iter().map(|e| e.depth as i32).collect();
    ui.set_book_contents(ModelRc::new(VecModel::from(titles)));
    ui.set_book_contents_depth(ModelRc::new(VecModel::from(depths)));
    ui.set_book_contents_current(r.toc_current() as i32);
}

// ── workbooks (files2-view-xlsx) ─────────────────────────────────────────────────────────────────────────────────

use crate::DCell;
use neo_format_document::xlsx::{self, Sheet, Value, Workbook};

/// What the grid draws: cells, lines, the column letters, the row numbers, and the selection (x, y, w, h).
pub type SheetModels = (
    Vec<DCell>,
    Vec<DRect>,
    Vec<DCell>,
    Vec<DCell>,
    (i32, i32, i32, i32),
);

/// The grid's sizes, dp: a character of a column's width, a row, the header row.
const CHAR_DP: f32 = 6.0;
const ROW_DP: f32 = 29.0;

/// A workbook in the grid: the sheet in hand, the selection, a window of cells around the scroll position.
pub struct SheetViewer {
    pub wb: Workbook,
    pub index: usize,
    /// `None` for a chartsheet.
    pub sheet: Option<Sheet>,
    /// (first row, first col, last row, last col), inclusive.
    pub sel: (u32, u32, u32, u32),
    dp: f32,
    view: (i32, i32),
    /// Column edges, px (one more than the columns).
    xs: Vec<i32>,
    row_h: i32,
    rows: u32,
    /// The window: rows and columns drawn.
    win: (u32, u32, u32, u32),
    pub scroll: (i32, i32),
}

impl SheetViewer {
    pub fn new(wb: Workbook, dp: f32) -> Self {
        SheetViewer {
            wb,
            index: 0,
            sheet: None,
            sel: (0, 0, 0, 0),
            dp,
            view: (0, 0),
            xs: Vec::new(),
            row_h: 0,
            rows: 0,
            win: (0, 0, 0, 0),
            scroll: (0, 0),
        }
    }

    /// Sheet `i` in hand (or `None`: a chartsheet), its first cell selected, for a grid of `w` × `h` px.
    pub fn set_sheet(&mut self, i: usize, sheet: Option<Sheet>, w: i32, h: i32) {
        self.index = i;
        self.sheet = sheet;
        self.sel = (0, 0, 0, 0);
        self.scroll = (0, 0);
        self.view = (w.max(100), h.max(100));
        self.row_h = (ROW_DP * self.dp) as i32;
        let (rows, cols) = self.sheet.as_ref().map_or((0, 0), |s| (s.rows, s.cols));
        let min_w = (24.0 * self.dp) as i32;
        let mut xs = alloc::vec![0];
        let mut c = 0;
        // the columns used, then enough empty ones to fill the view
        while c < cols.max(1) || *xs.last().unwrap_or(&0) < self.view.0 {
            let w = self.sheet.as_ref().map_or(8.43, |s| s.width(c));
            let px = ((w * CHAR_DP * self.dp) as i32).max(min_w);
            xs.push(xs.last().unwrap_or(&0) + px);
            c += 1;
            if c >= 16_384 {
                break;
            }
        }
        self.xs = xs;
        self.rows = rows.max((self.view.1 / self.row_h.max(1)) as u32 + 1);
        self.win = (u32::MAX, 0, 0, 0);
        self.window();
    }

    pub fn cols(&self) -> u32 {
        self.xs.len() as u32 - 1
    }

    pub fn total(&self) -> (i32, i32) {
        (*self.xs.last().unwrap_or(&0), self.rows as i32 * self.row_h)
    }

    /// The row numbers' width: the digits of the last row.
    pub fn head(&self) -> (i32, i32) {
        let digits = self.rows.max(1).ilog10() as f32 + 1.0;
        (
            ((8.0 + 7.0 * digits) * self.dp).max(26.0 * self.dp) as i32,
            self.row_h,
        )
    }

    fn col_at(&self, x: i32) -> u32 {
        (self.xs.partition_point(|e| *e <= x).saturating_sub(1) as u32)
            .min(self.cols().saturating_sub(1))
    }

    fn row_at(&self, y: i32) -> u32 {
        ((y.max(0) / self.row_h.max(1)) as u32).min(self.rows.saturating_sub(1))
    }

    /// Scrolled to (x, y): whether the window must move.
    pub fn scrolled(&mut self, x: i32, y: i32) -> bool {
        self.scroll = (x.max(0), y.max(0));
        let (r0, r1, c0, c1) = (
            self.row_at(y),
            self.row_at(y + self.view.1),
            self.col_at(x),
            self.col_at(x + self.view.0),
        );
        if self.win.0 != u32::MAX
            && r0 >= self.win.0
            && r1 <= self.win.1
            && c0 >= self.win.2
            && c1 <= self.win.3
        {
            return false;
        }
        self.window();
        true
    }

    fn window(&mut self) {
        let (x, y) = self.scroll;
        let (vw, vh) = self.view;
        let r0 = self.row_at(y - vh);
        let r1 = self.row_at(y + 2 * vh);
        let c0 = self.col_at(x - vw);
        let c1 = self.col_at(x + 2 * vw);
        self.win = (r0, r1, c0, c1);
    }

    /// The cell at (x, y) px in the grid selected.
    pub fn tap(&mut self, x: i32, y: i32) {
        let (r, c) = (self.row_at(y), self.col_at(x));
        // a tap on a merged area selects the merge
        let m = self.sheet.as_ref().and_then(|s| {
            s.merges
                .iter()
                .find(|m| m.0 <= r && r <= m.2 && m.1 <= c && c <= m.3)
                .copied()
        });
        self.sel = m.unwrap_or((r, c, r, c));
    }

    /// A column letter (or a row number) tapped: the whole column (row) selected.
    pub fn head_tap(&mut self, column: bool, at: i32) {
        let last_r = self.sheet.as_ref().map_or(0, |s| s.rows.saturating_sub(1));
        let last_c = self.sheet.as_ref().map_or(0, |s| s.cols.saturating_sub(1));
        self.sel = if column {
            let c = self.col_at(at);
            (0, c, last_r, c)
        } else {
            let r = self.row_at(at);
            (r, 0, r, last_c)
        };
    }

    /// "E7", or "B2:E6".
    pub fn ref_name(&self) -> String {
        let (r0, c0, r1, c1) = self.sel;
        let one = |r: u32, c: u32| alloc::format!("{}{}", xlsx::col_name(c), r + 1);
        if (r0, c0) == (r1, c1) {
            one(r0, c0)
        } else {
            alloc::format!("{}:{}", one(r0, c0), one(r1, c1))
        }
    }

    /// The formula bar: the selected cell's formula or value.
    pub fn formula(&self) -> String {
        let (r0, c0, ..) = self.sel;
        self.sheet
            .as_ref()
            .and_then(|s| s.get(r0, c0))
            .map(|c| xlsx::formula_bar(&self.wb, c))
            .unwrap_or_default()
    }

    /// "Sum 1 038 500": the numbers in the selection (none: nothing).
    pub fn sum(&self) -> String {
        let Some(s) = self.sheet.as_ref() else {
            return String::new();
        };
        let (r0, c0, r1, c1) = self.sel;
        let (mut sum, mut n) = (0.0f64, 0);
        for c in s.rows_in(r0, r1 + 1) {
            if (c0..=c1).contains(&c.col)
                && let Value::Num(v) = c.value
            {
                sum += v;
                n += 1;
            }
        }
        if n == 0 {
            return String::new();
        }
        let whole = sum == (sum as i64) as f64;
        alloc::format!(
            "Sum {}",
            xlsx::number(sum, if whole { "#,##0" } else { "#,##0.00" }, false)
        )
    }

    pub fn status(&self) -> String {
        let n = self.wb.sheets.len();
        let mut s = alloc::format!(
            "Read-only · {n} {}",
            if n == 1 { "sheet" } else { "sheets" }
        );
        if self.sheet.as_ref().is_some_and(|s| s.truncated) {
            s.push_str(" · the first 200,000 cells");
        }
        s
    }

    /// The window's cells, lines and headers, and the selection's rectangle, px in the grid.
    pub fn models(&self) -> SheetModels {
        let (r0, r1, c0, c1) = self.win;
        let (r0, r1, c0, c1) = if r0 == u32::MAX {
            (0, 0, 0, 0)
        } else {
            (r0, r1, c0, c1)
        };
        let rh = self.row_h;
        let x_of = |c: u32| self.xs[(c as usize).min(self.xs.len() - 1)];
        let mut cells = Vec::new();
        if let Some(s) = &self.sheet {
            for c in s.rows_in(r0, r1 + 1) {
                if c.col < c0 || c.col > c1 {
                    continue;
                }
                let (w, h) = match s.merge_at(c.row, c.col) {
                    Some(None) => continue,
                    Some(Some((er, ec))) => {
                        (x_of(ec + 1) - x_of(c.col), (er - c.row + 1) as i32 * rh)
                    }
                    None => (x_of(c.col + 1) - x_of(c.col), rh),
                };
                let text = xlsx::display(&self.wb, c);
                if text.is_empty() {
                    continue;
                }
                let align = match (&c.value, s.merge_at(c.row, c.col)) {
                    (_, Some(Some(_))) => 2,
                    (Value::Num(_), _) => 1,
                    (Value::Bool(_) | Value::Error(_), _) => 2,
                    _ => 0,
                };
                let bold = self.wb.bold.get(c.style as usize).copied().unwrap_or(false);
                cells.push(DCell {
                    x: x_of(c.col),
                    y: c.row as i32 * rh,
                    w,
                    h,
                    text: text.as_str().into(),
                    align,
                    bold,
                });
            }
        }
        // grid lines over the window
        let line = (self.dp / 3.0).max(1.0) as i32;
        let ink = colour(0x242629);
        let (top, bottom) = (r0 as i32 * rh, (r1 + 1) as i32 * rh);
        let (left, right) = (x_of(c0), x_of(c1 + 1));
        let mut lines = Vec::new();
        for c in c0..=c1 + 1 {
            lines.push(DRect {
                x: x_of(c) - line,
                y: top,
                w: line,
                h: bottom - top,
                colour: ink,
            });
        }
        for r in r0..=r1 + 1 {
            lines.push(DRect {
                x: left,
                y: r as i32 * rh - line,
                w: right - left,
                h: line,
                colour: ink,
            });
        }
        let cols: Vec<DCell> = (c0..=c1)
            .map(|c| DCell {
                x: x_of(c),
                y: 0,
                w: x_of(c + 1) - x_of(c),
                h: rh,
                text: xlsx::col_name(c).as_str().into(),
                align: 2,
                bold: true,
            })
            .collect();
        let hw = self.head().0;
        let rows: Vec<DCell> = (r0..=r1)
            .map(|r| DCell {
                x: 0,
                y: r as i32 * rh,
                w: hw,
                h: rh,
                text: alloc::format!("{}", r + 1).as_str().into(),
                align: 2,
                bold: true,
            })
            .collect();
        let (sr0, sc0, sr1, sc1) = self.sel;
        let sel = if self.sheet.is_some() {
            (
                x_of(sc0),
                sr0 as i32 * rh,
                x_of(sc1 + 1) - x_of(sc0),
                (sr1 - sr0 + 1) as i32 * rh,
            )
        } else {
            (0, 0, 0, 0)
        };
        (cells, lines, cols, rows, sel)
    }
}

/// Put the sheet on the screen (view "sheet").
pub fn show_sheet(ui: &crate::FilesApp, v: &SheetViewer) {
    let (cells, lines, cols, rows, sel) = v.models();
    ui.set_sheet_cells(ModelRc::new(VecModel::from(cells)));
    ui.set_sheet_lines(ModelRc::new(VecModel::from(lines)));
    ui.set_sheet_col_heads(ModelRc::new(VecModel::from(cols)));
    ui.set_sheet_row_heads(ModelRc::new(VecModel::from(rows)));
    ui.set_sheet_sel_x(sel.0);
    ui.set_sheet_sel_y(sel.1);
    ui.set_sheet_sel_w(sel.2);
    ui.set_sheet_sel_h(sel.3);
    let (tw, th) = v.total();
    ui.set_sheet_total_w(tw);
    ui.set_sheet_total_h(th);
    let (hw, hh) = v.head();
    ui.set_sheet_head_w(hw);
    ui.set_sheet_head_h(hh);
    ui.set_sheet_ref(v.ref_name().into());
    ui.set_sheet_formula(v.formula().into());
    ui.set_sheet_status(v.status().into());
    ui.set_sheet_sum(v.sum().into());
    ui.set_sheet_note(
        if v.sheet.is_none() {
            "Charts aren't shown"
        } else {
            ""
        }
        .into(),
    );
    let names: Vec<slint::SharedString> =
        v.wb.sheets.iter().map(|s| s.name.as_str().into()).collect();
    ui.set_sheet_names(ModelRc::new(VecModel::from(names)));
    ui.set_sheet_index(v.index as i32);
}
