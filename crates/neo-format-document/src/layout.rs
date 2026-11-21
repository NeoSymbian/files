// SPDX-License-Identifier: Apache-2.0
//! Lines and pages: blocks laid out at a width into positioned text runs, images and rectangles, at the advances the
//! screen draws ([`Measure`]: the caller's, from the same fonts and rounding as Slint), then cut into pages at line
//! and row boundaries. Left-aligned, centred or end-aligned; no justification, hyphenation or right-to-left text.

use crate::model::*;
use alloc::string::String;
use alloc::vec::Vec;
use unicode_linebreak::{BreakOpportunity, linebreaks};

/// The faces the viewer draws (DejaVu Sans, its Bold and Oblique, and DejaVu Sans Mono).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    Mono = 3,
}

/// Glyph widths as the screen draws them.
pub trait Measure {
    /// The advance of `c` at `px`, `None` when the face has no glyph for it.
    fn advance(&self, c: char, face: Face, px: i32) -> Option<i32>;
    /// From a text's top to its baseline at `px`.
    fn ascent(&self, face: Face, px: i32) -> i32;
}

/// Drawn in place of a character the fonts don't have.
pub const MISSING: char = '□';

#[derive(Clone, Copy, Debug)]
pub struct Opts {
    /// The width text runs in, px.
    pub width: i32,
    /// The body text size, px.
    pub body: i32,
    /// px per twip, for indents, spacing and the file's sizes.
    pub twip: f32,
    /// Page mode: text sizes as the file gives them (in points × `twip` × 20); else relative to `body`.
    pub absolute: bool,
    /// The gap between paragraphs that don't say, px.
    pub gap: i32,
    /// The tallest an image may be, px (a page's height; 0 = no limit).
    pub max_image_h: i32,
    /// An image's own pixels are drawn this big (dp).
    pub image_scale: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub block: u32,
    pub ch: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Text {
        x: i32,
        y: i32,
        px: i32,
        face: Face,
        text: String,
        colour: Option<u32>,
        underline: bool,
        strike: bool,
    },
    Image {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        id: usize,
    },
    Rect {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        colour: u32,
    },
}

impl Item {
    pub fn y(&self) -> i32 {
        match *self {
            Item::Text { y, .. } | Item::Image { y, .. } | Item::Rect { y, .. } => y,
        }
    }
    fn shift(&mut self, dx: i32, dy: i32) {
        match self {
            Item::Text { x, y, .. } | Item::Image { x, y, .. } | Item::Rect { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
        }
    }
}

/// A place a page may start: a line's or a row's top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Break {
    pub y: i32,
    pub at: Pos,
    /// A page break in the file: the page ends here.
    pub forced: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Flow {
    /// By `y`.
    pub items: Vec<Item>,
    pub height: i32,
    pub breaks: Vec<Break>,
    /// The tallest item (a window looks this far above its top).
    tallest: i32,
}

/// A page: `[top, end)` of the flow, starting at `at`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    pub top: i32,
    pub end: i32,
    pub at: Pos,
}

impl Flow {
    /// The items that show in `[top, end)`.
    pub fn window(&self, top: i32, end: i32) -> impl Iterator<Item = &Item> {
        let from = self.items.partition_point(|i| i.y() < top - self.tallest);
        self.items[from..]
            .iter()
            .take_while(move |i| i.y() < end)
            .filter(move |i| {
                let h = match **i {
                    Item::Text { px, .. } => px * 2,
                    Item::Image { h, .. } | Item::Rect { h, .. } => h,
                };
                i.y() + h > top
            })
    }

    /// The items of a page: those starting in it (a page never cuts a line).
    pub fn page<'a>(&'a self, p: &Page) -> impl Iterator<Item = &'a Item> {
        let (top, end) = (p.top, p.end);
        let from = self.items.partition_point(|i| i.y() < top);
        self.items[from..].iter().take_while(move |i| i.y() < end)
    }

    /// Cut into pages of `height`: each ends at the last line or row top that fits, or at a page break.
    pub fn pages(&self, height: i32) -> Vec<Page> {
        let height = height.max(1);
        let mut out = Vec::new();
        let (mut top, mut at, mut i) = (0, Pos::default(), 0);
        loop {
            let limit = top + height;
            let mut stop: Option<Break> = None;
            while i < self.breaks.len() && self.breaks[i].y <= limit {
                let b = self.breaks[i];
                i += 1;
                if b.y > top {
                    stop = Some(b);
                    if b.forced {
                        break;
                    }
                }
            }
            if limit >= self.height && !stop.is_some_and(|b| b.forced) {
                out.push(Page {
                    top,
                    end: self.height.max(top + 1),
                    at,
                });
                return out;
            }
            // no line top fits (a line taller than the page): cut at the limit
            let (y, next) = stop.map_or((limit, at), |b| (b.y, b.at));
            out.push(Page { top, end: y, at });
            top = y;
            at = next;
        }
    }
}

/// The page `pos` is on.
pub fn page_of(pages: &[Page], pos: Pos) -> usize {
    pages.partition_point(|p| p.at <= pos).saturating_sub(1)
}

struct Ctx<'a, M: Measure, S: Fn(usize) -> Option<(i32, i32)>> {
    m: &'a M,
    o: Opts,
    body: u16,
    size: S,
}

/// Lay `doc`'s blocks out. `size` gives an image's own pixels (`None`: not drawn).
pub fn layout<M: Measure>(
    doc: &Doc,
    o: Opts,
    m: &M,
    size: impl Fn(usize) -> Option<(i32, i32)>,
) -> Flow {
    let c = Ctx {
        m,
        o,
        body: if doc.body == 0 { 24 } else { doc.body },
        size,
    };
    let mut f = Flow::default();
    let mut y = 0;
    for (b, block) in doc.blocks.iter().enumerate() {
        let b = b as u32;
        match block {
            Block::Para(p) => y = c.para(&mut f, p, b, 0, y, o.width),
            Block::Table(t) => y = c.table(&mut f, t, b, y),
            Block::Image(img) => y = c.image(&mut f, img, b, y),
            Block::Rule => {
                f.breaks.push(Break {
                    y,
                    at: Pos { block: b, ch: 0 },
                    forced: false,
                });
                let h = (o.body / 16).max(1);
                f.items.push(Item::Rect {
                    x: o.width / 4,
                    y: y + o.gap,
                    w: o.width / 2,
                    h,
                    colour: 0x9aa0a8,
                });
                y += 2 * o.gap + h;
            }
            Block::PageBreak => f.breaks.push(Break {
                y,
                at: Pos { block: b, ch: 0 },
                forced: true,
            }),
        }
    }
    f.height = y;
    f.items.sort_by_key(Item::y);
    f.tallest = f
        .items
        .iter()
        .map(|i| match *i {
            Item::Text { px, .. } => px * 2,
            Item::Image { h, .. } | Item::Rect { h, .. } => h,
        })
        .max()
        .unwrap_or(0);
    f
}

/// One character on a line.
#[derive(Clone, Copy)]
struct Ch {
    c: char,
    run: u16,
    w: i32,
}

impl<M: Measure, S: Fn(usize) -> Option<(i32, i32)>> Ctx<'_, M, S> {
    fn tw(&self, twips: i32) -> i32 {
        (twips as f32 * self.o.twip) as i32
    }

    fn face(s: &Style) -> Face {
        if s.mono {
            Face::Mono
        } else if s.bold {
            Face::Bold
        } else if s.italic {
            Face::Italic
        } else {
            Face::Regular
        }
    }

    /// A run's px: the file's size (Page mode), or relative to the body; headings grow when the file says nothing.
    fn px(&self, s: &Style, kind: &ParaKind) -> i32 {
        let o = &self.o;
        let base = if s.size == 0 {
            let k = match kind {
                ParaKind::Heading(1) => 1.6,
                ParaKind::Heading(2) => 1.4,
                ParaKind::Heading(3) => 1.2,
                ParaKind::Heading(4) => 1.1,
                _ => 1.0,
            };
            if o.absolute {
                self.body as f32 * 10.0 * o.twip * k
            } else {
                o.body as f32 * k
            }
        } else if o.absolute {
            s.size as f32 * 10.0 * o.twip
        } else {
            o.body as f32 * s.size as f32 / self.body as f32
        };
        let px = if s.script == Script::Normal {
            base
        } else {
            base * 0.7
        };
        (px as i32).max(4)
    }

    fn para(&self, f: &mut Flow, p: &Para, block: u32, x0: i32, mut y: i32, width: i32) -> i32 {
        let o = &self.o;
        let heading = matches!(p.kind, ParaKind::Heading(_));
        let (before, after) = match p.kind {
            ParaKind::Heading(_) => (o.gap * 3 / 2, o.gap / 2),
            ParaKind::Item { .. } => (o.gap / 4, o.gap / 4),
            _ => (o.gap / 2, o.gap / 2),
        };
        y += p.before.map_or(before, |t| self.tw(t));
        let styles: Vec<Style> = p
            .runs
            .iter()
            .map(|r| {
                let mut s = r.style;
                s.bold |= heading;
                s.mono |= p.kind == ParaKind::Pre;
                s
            })
            .collect();
        let body_px = styles.first().map_or(o.body, |s| self.px(s, &p.kind));
        let step = body_px * 3 / 2;
        let mut indent = self.tw(p.indent);
        if p.kind == ParaKind::Quote {
            indent += step;
        }
        let mut marker = None;
        if let ParaKind::Item { depth, marker: m } = &p.kind {
            let at = indent + *depth as i32 * step;
            let mw: i32 = m
                .chars()
                .map(|c| {
                    self.m
                        .advance(c, Face::Regular, body_px)
                        .unwrap_or(body_px / 2)
                })
                .sum();
            let box_w = mw.max(step) + body_px / 3;
            marker = Some((at, m.as_str()));
            indent = at + box_w;
        }
        let colour = if p.kind == ParaKind::Quote {
            Some(0x5b6472)
        } else {
            None
        };

        // characters, with a glyph for each (or the box)
        let mut text = String::new();
        let mut chars: Vec<Ch> = Vec::new();
        for (i, r) in p.runs.iter().enumerate() {
            let s = &styles[i];
            let (face, px) = (Self::face(s), self.px(s, &p.kind));
            for c in r.text.chars() {
                let c = if c == '\t' { ' ' } else { c };
                let (c, w) = match self.m.advance(c, face, px) {
                    Some(w) => (c, w),
                    None if c == '\n' || c == '\u{ad}' || c == '\u{200b}' => (c, 0),
                    None => (MISSING, self.m.advance(MISSING, face, px).unwrap_or(px / 2)),
                };
                text.push(c);
                chars.push(Ch {
                    c,
                    run: i as u16,
                    w,
                });
            }
        }
        if chars.is_empty() {
            // an empty paragraph keeps a line's height (docx uses them as spacing)
            let lh = body_px * 14 / 10;
            f.breaks.push(Break {
                y,
                at: Pos { block, ch: 0 },
                forced: false,
            });
            return y + lh + p.after.map_or(after, |t| self.tw(t));
        }

        // break opportunities by char index
        let mut opp = alloc::vec![None; chars.len() + 1];
        let byte_to_char: Vec<usize> = {
            let mut v = alloc::vec![0; text.len() + 1];
            for (ci, (bi, _)) in text.char_indices().enumerate() {
                v[bi] = ci;
            }
            v[text.len()] = chars.len();
            v
        };
        for (bi, b) in linebreaks(&text) {
            opp[byte_to_char[bi]] = Some(b == BreakOpportunity::Mandatory);
        }

        let first = self.tw(p.first);
        let mut start = 0;
        let mut line_no = 0;
        while start < chars.len() {
            let x_in = indent + if line_no == 0 { first } else { 0 };
            let avail = (width - x_in).max(body_px * 2);
            // greedy: the furthest opportunity that fits (trailing spaces don't count)
            let mut w = 0;
            let mut end = start;
            let mut fit = None;
            let mut i = start;
            while i < chars.len() {
                w += chars[i].w;
                i += 1;
                if let Some(m) = opp[i] {
                    let ink: i32 = w - chars[start..i]
                        .iter()
                        .rev()
                        .take_while(|c| c.c == ' ' || c.c == '\n')
                        .map(|c| c.w)
                        .sum::<i32>();
                    if ink > avail && fit.is_some() {
                        break;
                    }
                    fit = Some(i);
                    if ink > avail {
                        break;
                    }
                    if m {
                        break;
                    }
                }
            }
            if let Some(e) = fit {
                end = e;
                // a word longer than the line: cut it by characters
                let ink: i32 = chars[start..end]
                    .iter()
                    .filter(|c| c.c != ' ')
                    .map(|c| c.w)
                    .sum();
                if ink > avail {
                    let mut w = 0;
                    end = start;
                    while end < chars.len() && (end == start || w + chars[end].w <= avail) {
                        w += chars[end].w;
                        end += 1;
                    }
                }
            }
            if end == start {
                end = chars.len();
            }
            y = self.line(
                f,
                &chars[start..end],
                &styles,
                p,
                block,
                start as u32,
                x0 + x_in,
                y,
                avail,
                colour,
                if line_no == 0 {
                    marker.map(|(a, m)| (x0 + a, m, body_px))
                } else {
                    None
                },
            );
            start = end;
            line_no += 1;
        }
        y + p.after.map_or(after, |t| self.tw(t))
    }

    #[allow(clippy::too_many_arguments)]
    fn line(
        &self,
        f: &mut Flow,
        chars: &[Ch],
        styles: &[Style],
        p: &Para,
        block: u32,
        ch: u32,
        x0: i32,
        top: i32,
        avail: i32,
        colour: Option<u32>,
        marker: Option<(i32, &str, i32)>,
    ) -> i32 {
        f.breaks.push(Break {
            y: top,
            at: Pos { block, ch },
            forced: false,
        });
        let px_of = |run: u16| self.px(&styles[run as usize], &p.kind);
        let max_px = chars
            .iter()
            .map(|c| px_of(c.run))
            .max()
            .unwrap_or(self.o.body);
        let lh = max_px * 14 / 10;
        let max_ascent = chars
            .iter()
            .map(|c| {
                self.m
                    .ascent(Self::face(&styles[c.run as usize]), px_of(c.run))
            })
            .max()
            .unwrap_or(max_px);
        let base = top + (lh - max_px * 117 / 100) / 2 + max_ascent;
        // the ink's width, for alignment
        let ink: i32 = {
            let trail: i32 = chars
                .iter()
                .rev()
                .take_while(|c| c.c == ' ' || c.c == '\n')
                .map(|c| c.w)
                .sum();
            chars.iter().map(|c| c.w).sum::<i32>() - trail
        };
        let shift = match p.align {
            Align::Start => 0,
            Align::Centre => (avail - ink).max(0) / 2,
            Align::End => (avail - ink).max(0),
        };
        if let Some((x, m, px)) = marker {
            f.items.push(Item::Text {
                x,
                y: base - self.m.ascent(Face::Regular, px),
                px,
                face: Face::Regular,
                text: String::from(m),
                colour,
                underline: false,
                strike: false,
            });
        }
        let mut x = x0 + shift;
        let mut i = 0;
        while i < chars.len() {
            let run = chars[i].run;
            let mut j = i;
            let mut s = String::new();
            let start_x = x;
            let mut lead = 0;
            while j < chars.len() && chars[j].run == run {
                let c = chars[j].c;
                if s.is_empty() && c == ' ' {
                    lead += chars[j].w; // Slint drops a run's leading spaces: start after them
                } else if c != '\n' && c != '\u{ad}' {
                    s.push(c);
                }
                x += chars[j].w;
                j += 1;
            }
            let trimmed = s.trim_end();
            if !trimmed.is_empty() {
                let st = &styles[run as usize];
                let face = Self::face(st);
                let px = px_of(run);
                let dy = match st.script {
                    Script::Normal => 0,
                    Script::Super => -max_px / 3,
                    Script::Sub => max_px / 5,
                };
                f.items.push(Item::Text {
                    x: start_x + lead,
                    y: base - self.m.ascent(face, px) + dy,
                    px,
                    face,
                    text: String::from(trimmed),
                    colour: st.colour.or(colour),
                    underline: st.underline,
                    strike: st.strike,
                });
            }
            i = j;
        }
        top + lh
    }

    fn table(&self, f: &mut Flow, t: &Table, block: u32, mut y: i32) -> i32 {
        let o = &self.o;
        let cols = t
            .rows
            .iter()
            .map(|r| r.iter().map(|c| c.span.max(1) as usize).sum::<usize>())
            .max()
            .unwrap_or(0);
        if cols == 0 {
            return y;
        }
        let line = (o.body / 16).max(1);
        let pad = o.body * 2 / 5;
        let widths: Vec<i32> = if t.widths.len() == cols && t.widths.iter().all(|w| *w > 0) {
            let total: i64 = t.widths.iter().map(|w| *w as i64).sum();
            t.widths
                .iter()
                .map(|w| (*w as i64 * o.width as i64 / total) as i32)
                .collect()
        } else {
            alloc::vec![o.width / cols as i32; cols]
        };
        let colour = 0xd9d6cf;
        y += o.gap / 2;
        for row in &t.rows {
            f.breaks.push(Break {
                y,
                at: Pos { block, ch: 0 },
                forced: false,
            });
            f.items.push(Item::Rect {
                x: 0,
                y,
                w: o.width,
                h: line,
                colour,
            });
            let mut cell_flow = Flow::default();
            let mut bottom = y;
            let mut col = 0;
            let mut x = 0;
            for cell in row {
                let span = (cell.span.max(1) as usize).min(cols - col.min(cols));
                if span == 0 {
                    break;
                }
                let w: i32 = widths[col..col + span].iter().sum();
                let mut cy = y + line + pad / 2;
                for p in &cell.paras {
                    let mut p = p.clone();
                    if cell.header {
                        for r in &mut p.runs {
                            r.style.bold = true;
                        }
                    }
                    p.before = Some(0);
                    p.after = Some(0);
                    cy = self.para(&mut cell_flow, &p, block, x + pad, cy, w - 2 * pad);
                }
                bottom = bottom.max(cy + pad / 2);
                if col > 0 {
                    f.items.push(Item::Rect {
                        x,
                        y,
                        w: line,
                        h: 0,
                        colour,
                    });
                }
                x += w;
                col += span;
            }
            // the column lines run the row's height
            for i in &mut f.items {
                if let Item::Rect { h, w, .. } = i
                    && *h == 0
                    && *w == line
                {
                    *h = bottom - y;
                }
            }
            f.items.extend(cell_flow.items);
            y = bottom;
        }
        f.items.push(Item::Rect {
            x: 0,
            y,
            w: o.width,
            h: line,
            colour,
        });
        y + line + o.gap / 2
    }

    fn image(&self, f: &mut Flow, img: &Image, block: u32, y: i32) -> i32 {
        let o = &self.o;
        let Some((nw, nh)) = (self.size)(img.id) else {
            return y;
        };
        if nw <= 0 || nh <= 0 {
            return y;
        }
        let (mut w, mut h) = if img.width > 0 && img.height > 0 && o.absolute {
            (self.tw(img.width), self.tw(img.height))
        } else {
            (
                (nw as f32 * o.image_scale) as i32,
                (nh as f32 * o.image_scale) as i32,
            )
        };
        if w > o.width {
            h = (h as i64 * o.width as i64 / w as i64) as i32;
            w = o.width;
        }
        if o.max_image_h > 0 && h > o.max_image_h {
            w = (w as i64 * o.max_image_h as i64 / h as i64) as i32;
            h = o.max_image_h;
        }
        let y = y + o.gap / 2;
        f.breaks.push(Break {
            y,
            at: Pos { block, ch: 0 },
            forced: false,
        });
        f.items.push(Item::Image {
            x: (o.width - w) / 2,
            y,
            w: w.max(1),
            h: h.max(1),
            id: img.id,
        });
        y + h + o.gap / 2
    }
}

/// Move a flow's items by (dx, dy) (a table cell, a page's margins).
pub fn shift(items: &mut [Item], dx: i32, dy: i32) {
    for i in items {
        i.shift(dx, dy);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every glyph 10 px wide at px 20 (scaled), '□' too; no 'ж'.
    pub struct Fixed;
    impl Measure for Fixed {
        fn advance(&self, c: char, _: Face, px: i32) -> Option<i32> {
            (c != 'ж' && c != '\n').then_some(px / 2)
        }
        fn ascent(&self, _: Face, px: i32) -> i32 {
            px
        }
    }

    fn opts(width: i32) -> Opts {
        Opts {
            width,
            body: 20,
            twip: 0.05,
            absolute: false,
            gap: 10,
            max_image_h: 0,
            image_scale: 1.0,
        }
    }

    fn para(s: &str) -> Block {
        let mut p = Para::default();
        p.push(s, Style::default());
        Block::Para(p)
    }

    fn texts(f: &Flow) -> Vec<(i32, i32, &str)> {
        f.items
            .iter()
            .filter_map(|i| match i {
                Item::Text { x, y, text, .. } => Some((*x, *y, text.as_str())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn wraps_at_spaces_and_cuts_long_words() {
        let doc = Doc {
            blocks: alloc::vec![para("aaa bbb ccc"), para("xxxxxxxxxxxx")],
            ..Default::default()
        };
        let f = layout(&doc, opts(75), &Fixed, |_| None);
        let t: Vec<&str> = texts(&f).iter().map(|t| t.2).collect();
        // 7 chars of 10 px fit in 75: "aaa bbb" then "ccc"; the long word in 7s
        assert_eq!(t, ["aaa bbb", "ccc", "xxxxxxx", "xxxxx"]);
        assert!(f.breaks.len() == 4);
    }

    #[test]
    fn styled_runs_place_by_advance_and_skip_leading_spaces() {
        let mut p = Para::default();
        p.push("ab", Style::default());
        p.push(
            " cd",
            Style {
                bold: true,
                ..Default::default()
            },
        );
        p.push("жe", Style::default());
        let doc = Doc {
            blocks: alloc::vec![Block::Para(p)],
            ..Default::default()
        };
        let f = layout(&doc, opts(500), &Fixed, |_| None);
        let t = texts(&f);
        assert_eq!(t[0].2, "ab");
        assert_eq!((t[1].0, t[1].2), (30, "cd")); // after "ab" (20) and the space (10)
        assert_eq!((t[2].0, t[2].2), (50, "□e"));
        assert!(matches!(
            f.items[1],
            Item::Text {
                face: Face::Bold,
                ..
            }
        ));
    }

    #[test]
    fn centre_end_and_markers() {
        let mut c = Para {
            align: Align::Centre,
            ..Default::default()
        };
        c.push("ab", Style::default());
        let mut e = Para {
            align: Align::End,
            ..Default::default()
        };
        e.push("ab", Style::default());
        let mut li = Para::new(ParaKind::Item {
            depth: 0,
            marker: String::from("•"),
        });
        li.push("x", Style::default());
        let doc = Doc {
            blocks: alloc::vec![Block::Para(c), Block::Para(e), Block::Para(li)],
            ..Default::default()
        };
        let t = layout(&doc, opts(100), &Fixed, |_| None);
        let t = texts(&t);
        assert_eq!(t[0].0, 40);
        assert_eq!(t[1].0, 80);
        assert_eq!(t[2].2, "•");
        assert!(t[3].0 > t[2].0);
    }

    #[test]
    fn pages_end_at_lines_and_page_breaks() {
        let blocks: Vec<Block> = (0..10).map(|_| para("a")).collect();
        let mut doc = Doc {
            blocks,
            ..Default::default()
        };
        doc.blocks.insert(3, Block::PageBreak);
        let f = layout(&doc, opts(100), &Fixed, |_| None);
        let pages = f.pages(100);
        // a line is 28 + 10 gap: 2 lines a page, and the break after the third paragraph
        assert!(pages.len() >= 5, "{pages:?}");
        assert_eq!(pages[1].at.block, 2);
        assert_eq!(pages[2].at.block, 3);
        for p in &pages {
            assert!(p.end > p.top);
            for i in f.page(p) {
                assert!(i.y() >= p.top && i.y() < p.end);
            }
        }
        assert_eq!(pages.last().unwrap().end, f.height);
        assert_eq!(
            page_of(&pages, Pos { block: 5, ch: 0 }),
            pages
                .iter()
                .rposition(|p| p.at <= Pos { block: 5, ch: 0 })
                .unwrap()
        );
        // nothing is lost: every item is on exactly one page
        let n: usize = pages.iter().map(|p| f.page(p).count()).sum();
        assert_eq!(n, f.items.len());
    }

    #[test]
    fn tables_and_images() {
        let cell = |s: &str| Cell {
            paras: alloc::vec![{
                let mut p = Para::default();
                p.push(s, Style::default());
                p
            }],
            span: 1,
            header: false,
        };
        let t = Table {
            rows: alloc::vec![
                alloc::vec![cell("a"), cell("b")],
                alloc::vec![Cell {
                    span: 2,
                    ..cell("wide")
                }]
            ],
            widths: alloc::vec![],
        };
        let mut doc = Doc {
            blocks: alloc::vec![Block::Table(t)],
            ..Default::default()
        };
        let img = doc.image(ImageSrc::Member(String::from("i.png")), 0, 0);
        doc.blocks.push(img);
        let f = layout(&doc, opts(200), &Fixed, |_| Some((400, 100)));
        let t = texts(&f);
        assert_eq!(
            t.iter().map(|t| t.2).collect::<Vec<_>>(),
            ["a", "b", "wide"]
        );
        assert!(t[1].0 >= 100);
        let img = f
            .items
            .iter()
            .find_map(|i| match i {
                Item::Image { w, h, .. } => Some((*w, *h)),
                _ => None,
            })
            .unwrap();
        assert_eq!(img, (200, 50));
        let lines = f
            .items
            .iter()
            .filter(|i| matches!(i, Item::Rect { h, .. } if *h > 2))
            .count();
        assert_eq!(lines, 1); // one column line, in the first row
    }

    #[test]
    fn window_finds_what_shows() {
        let doc = Doc {
            blocks: (0..100).map(|_| para("a")).collect(),
            ..Default::default()
        };
        let f = layout(&doc, opts(100), &Fixed, |_| None);
        let w: Vec<&Item> = f.window(380, 500).collect();
        assert!(!w.is_empty());
        assert!(w.iter().all(|i| i.y() < 500 && i.y() + 40 > 380));
    }
}
