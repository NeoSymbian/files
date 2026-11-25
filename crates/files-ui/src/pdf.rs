// SPDX-License-Identifier: GPL-3.0-only
//! FA11 (files2-view-pdf): where the pages go and which pictures of them are wanted. The pages are laid out at the
//! view's width (16 dp each side, 12 dp between them; zoomed, z times as wide); the device draws whole pages near the
//! view at that width, one sharp tile of the view while zoomed, and the strip's thumbnails near the current page.
//! [`show_pdf`] hands the UI only the pages near the view.

use crate::{FilesApp, PSlot};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use slint::{Image, ModelRc, VecModel};

/// 16 dp beside the pages, 14 dp over the first, 12 dp between them, room for the pill under the last.
const SIDE: f32 = 16.0;
const TOP: f32 = 14.0;
const GAP: f32 = 12.0;
const BOTTOM: f32 = 52.0;
/// A thumbnail is 38 dp wide.
pub const THUMB_DP: f32 = 38.0;
/// How far one can zoom in.
pub const ZOOM_MAX: f32 = 4.0;
/// Thumbnails this far either side of the current page are wanted.
const THUMBS_NEAR: usize = 8;

/// A part of a scaled page: x, y, w, h px.
pub type Rect = (u32, u32, u32, u32);
/// The zoomed view's sharp tile: its page, its part of the page, its picture.
pub type Tile = (usize, Rect, Image);

/// A picture the view wants: a whole page at the layout's width, the zoomed view's tile, or a thumbnail. `scale` is
/// pixels per point, `rect` the part of the scaled page (x, y, w, h px).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Want {
    pub kind: Kind,
    pub page: usize,
    pub scale: f32,
    pub rect: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Page,
    Tile,
    Thumb,
}

/// The pages' layout and where the view is.
pub struct PdfViewer {
    /// Each page's size in points.
    pub sizes: Vec<(f32, f32)>,
    pub dp: f32,
    /// 1 (the screen's width) to [`ZOOM_MAX`].
    pub zoom: f32,
    view: (i32, i32),
    pub top: i32,
    pub left: i32,
    /// Each page's slot in the content, px.
    slots: Vec<(i32, i32, i32, i32)>,
    pub total: i32,
    pub total_w: i32,
}

impl PdfViewer {
    pub fn new(sizes: Vec<(f32, f32)>, dp: f32) -> PdfViewer {
        PdfViewer {
            sizes,
            dp,
            zoom: 1.0,
            view: (0, 0),
            top: 0,
            left: 0,
            slots: Vec::new(),
            total: 0,
            total_w: 0,
        }
    }

    pub fn pages(&self) -> usize {
        self.sizes.len()
    }

    /// Lays the pages out for a view of `w` × `h` px at the current zoom, keeping the same place on the current page.
    pub fn layout(&mut self, w: i32, h: i32) {
        let keep = self.anchor();
        self.view = (w, h);
        self.relayout();
        if let Some((p, f)) = keep {
            let (_, y, _, h) = self.slot(p);
            self.top = y + (f * h as f32) as i32;
        }
        self.clamp();
    }

    /// The slots at the view's width and the zoom.
    fn relayout(&mut self) {
        let (w, _) = self.view;
        let fit = (w as f32 - 2.0 * SIDE * self.dp).max(1.0);
        let width = (fit * self.zoom) as i32;
        self.total_w = (width as f32 + 2.0 * SIDE * self.dp).max(w as f32) as i32;
        let x = ((self.total_w - width) / 2).max(0);
        let gap = (GAP * self.dp) as i32;
        let mut y = (TOP * self.dp) as i32;
        self.slots.clear();
        for &(pw, ph) in &self.sizes {
            let ht = ((ph * width as f32 / pw.max(1.0)) as i32).max(1);
            self.slots.push((x, y, width, ht));
            y += ht + gap;
        }
        self.total = y - gap + (BOTTOM * self.dp) as i32;
    }

    /// The page's slot in the content, px.
    pub fn slot(&self, page: usize) -> (i32, i32, i32, i32) {
        self.slots.get(page).copied().unwrap_or_default()
    }

    /// Pixels per point of the page as laid out.
    pub fn scale(&self, page: usize) -> f32 {
        let (_, _, w, _) = self.slot(page);
        w as f32 / self.sizes.get(page).map_or(1.0, |s| s.0.max(1.0))
    }

    /// The page at the middle of the view.
    pub fn current(&self) -> usize {
        let mid = self.top + self.view.1 / 2;
        let gap = (GAP * self.dp) as i32;
        self.slots
            .iter()
            .position(|&(_, y, _, h)| mid < y + h + gap)
            .unwrap_or(self.pages().saturating_sub(1))
    }

    /// The pages whose slots show in the view (and one more each side).
    pub fn near(&self) -> core::ops::Range<usize> {
        let (top, end) = (self.top, self.top + self.view.1);
        let first = self
            .slots
            .iter()
            .position(|&(_, y, _, h)| y + h > top)
            .unwrap_or(0);
        let last = self
            .slots
            .iter()
            .rposition(|&(_, y, _, _)| y < end)
            .unwrap_or(first);
        first.saturating_sub(1)..(last + 2).min(self.pages())
    }

    /// The view scrolled to `top` (and sideways to `left`); true when the current page changed.
    pub fn scroll(&mut self, top: i32, left: i32) -> bool {
        let was = self.current();
        self.top = top;
        self.left = left;
        self.clamp();
        was != self.current()
    }

    /// Where to scroll to show the page's top.
    pub fn top_of(&self, page: usize) -> i32 {
        self.slot(page).1 - (TOP * self.dp) as i32
    }

    /// The pictures wanted now, most needed first: the current page, its neighbours, the zoomed tile, thumbnails.
    pub fn wanted(&self) -> Vec<Want> {
        let mut out = Vec::new();
        let cur = self.current();
        let fit = self.fit_scale_ratio();
        let mut pages: Vec<usize> = self.near().collect();
        pages.sort_by_key(|&p| p.abs_diff(cur));
        for p in pages {
            // Whole pages at the screen's width: zoomed, they stretch until the tile comes.
            let s = self.scale(p) * fit;
            let (_, _, w, h) = self.slot(p);
            let size = ((w as f32 * fit) as u32, (h as f32 * fit) as u32);
            out.push(Want {
                kind: Kind::Page,
                page: p,
                scale: s,
                rect: (0, 0, size.0.max(1), size.1.max(1)),
            });
        }
        if self.zoom > 1.0
            && let Some(rect) = self.tile_rect(cur)
        {
            out.insert(
                1,
                Want {
                    kind: Kind::Tile,
                    page: cur,
                    scale: self.scale(cur),
                    rect,
                },
            );
        }
        let thumb = THUMB_DP * self.dp;
        let n = self.pages();
        let mut thumbs: Vec<usize> =
            (cur.saturating_sub(THUMBS_NEAR)..(cur + THUMBS_NEAR + 1).min(n)).collect();
        thumbs.sort_by_key(|&p| p.abs_diff(cur));
        for p in thumbs {
            let (pw, ph) = self.sizes[p];
            let s = thumb / pw.max(1.0);
            out.push(Want {
                kind: Kind::Thumb,
                page: p,
                scale: s,
                rect: (0, 0, thumb as u32, ((ph * s) as u32).max(1)),
            });
        }
        out
    }

    /// The part of the page's slot the view shows, in the slot's px (`None` when it doesn't show).
    pub fn tile_rect(&self, page: usize) -> Option<Rect> {
        let (x, y, w, h) = self.slot(page);
        let (vx0, vy0) = (self.left, self.top);
        let (vx1, vy1) = (vx0 + self.view.0, vy0 + self.view.1);
        let (x0, y0) = (vx0.max(x), vy0.max(y));
        let (x1, y1) = (vx1.min(x + w), vy1.min(y + h));
        (x1 > x0 && y1 > y0).then(|| {
            (
                (x0 - x) as u32,
                (y0 - y) as u32,
                (x1 - x0) as u32,
                (y1 - y0) as u32,
            )
        })
    }

    /// Zooms to `z` (clamped) keeping the point (`fx`, `fy`) of the view over the same place of its page.
    pub fn zoom_at(&mut self, z: f32, fx: i32, fy: i32) {
        let z = z.clamp(1.0, ZOOM_MAX);
        if z == self.zoom || self.slots.is_empty() {
            return;
        }
        let (cx, cy) = (self.left + fx, self.top + fy);
        let gap = (GAP * self.dp) as i32;
        let p = self
            .slots
            .iter()
            .position(|&(_, y, _, h)| cy < y + h + gap)
            .unwrap_or(self.pages() - 1);
        let (x, y, w, h) = self.slot(p);
        let (u, v) = (
            (cx - x) as f32 / w.max(1) as f32,
            (cy - y) as f32 / h.max(1) as f32,
        );
        self.zoom = z;
        self.relayout();
        let (x, y, w, h) = self.slot(p);
        self.left = x + (u * w as f32) as i32 - fx;
        self.top = y + (v * h as f32) as i32 - fy;
        self.clamp();
    }

    /// "Page 1 / 2".
    pub fn pill(&self) -> String {
        format!("Page {} / {}", self.current() + 1, self.pages())
    }

    /// Where the reader is, to keep: the current page and the zoom.
    pub fn place(&self) -> (usize, f32) {
        (self.current(), self.zoom)
    }

    /// Whole pages are drawn at the screen's width: the ratio of that to the zoomed layout.
    fn fit_scale_ratio(&self) -> f32 {
        1.0 / self.zoom
    }

    /// The current page and how far down it the view's top is (a fraction), to keep over a new layout.
    fn anchor(&self) -> Option<(usize, f32)> {
        if self.slots.is_empty() {
            return None;
        }
        let p = self.current();
        let (_, y, _, h) = self.slot(p);
        Some((p, (self.top - y) as f32 / h.max(1) as f32))
    }

    fn clamp(&mut self) {
        self.top = self.top.clamp(0, (self.total - self.view.1).max(0));
        self.left = self.left.clamp(0, (self.total_w - self.view.0).max(0));
    }
}

/// What the device has drawn: whole pages, the tile and thumbnails, by page.
pub trait Pictures {
    fn page(&self, page: usize) -> Option<Image>;
    fn tile(&self) -> Option<Tile>;
    fn thumb(&self, page: usize) -> Option<Image>;
}

/// The view's properties: the pages near the view, the tile, the strip, the pill and the footer.
pub fn show_pdf(ui: &FilesApp, v: &PdfViewer, pics: &impl Pictures, info: &str) {
    let slots: Vec<PSlot> = v
        .near()
        .map(|p| {
            let (x, y, w, h) = v.slot(p);
            let image = pics.page(p);
            PSlot {
                x,
                y,
                w,
                h,
                ready: image.is_some(),
                image: image.unwrap_or_default(),
            }
        })
        .collect();
    ui.set_pdf_pages(ModelRc::new(VecModel::from(slots)));
    let tile = pics
        .tile()
        .filter(|(p, rect, _)| v.zoom > 1.0 && v.tile_rect(*p) == Some(*rect));
    ui.set_pdf_tile_on(tile.is_some());
    if let Some((p, (x, y, w, h), image)) = tile {
        let (sx, sy, ..) = v.slot(p);
        ui.set_pdf_tile(PSlot {
            x: sx + x as i32,
            y: sy + y as i32,
            w: w as i32,
            h: h as i32,
            image,
            ready: true,
        });
    }
    ui.set_pdf_total(v.total);
    ui.set_pdf_total_w(v.total_w);
    ui.set_pdf_pill(v.pill().into());
    ui.set_pdf_current(v.current() as i32);
    let thumbs: Vec<Image> = (0..v.pages())
        .map(|p| pics.thumb(p).unwrap_or_default())
        .collect();
    ui.set_pdf_thumbs(ModelRc::new(VecModel::from(thumbs)));
    ui.set_pdf_info(info.into());
}

/// "212 KB · 2 pages".
pub fn info(size: u64, pages: usize) -> String {
    format!(
        "{} · {} page{}",
        files_core::size_text(size),
        pages,
        if pages == 1 { "" } else { "s" }
    )
}
