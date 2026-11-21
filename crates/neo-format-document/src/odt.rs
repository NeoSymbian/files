// SPDX-License-Identifier: Apache-2.0
//! OpenDocument text (.odt): `content.xml` with the styles of `styles.xml` and its own automatic styles (bold,
//! italic, underline, strike, size, colour, super/subscript, alignment, indents, page breaks), headings, lists,
//! tables and images, and the page layout. Notes, frames' captions and change tracking are not shown.

use crate::model::*;
use crate::xml::{Ev, Reader, text};
use crate::{Package, Result};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use neo_format_archive::ReadAt;

/// A length (`2.5cm`, `12pt`, `0.5in`, `10mm`) in twips.
pub(crate) fn length(s: &str) -> Option<i32> {
    let s = s.trim();
    let at = s.find(|c: char| c.is_ascii_alphabetic())?;
    let v: f32 = s[..at].parse().ok()?;
    let k = match &s[at..] {
        "cm" => 1440.0 / 2.54,
        "mm" => 144.0 / 2.54,
        "in" => 1440.0,
        "pt" => 20.0,
        "pc" => 240.0,
        "px" => 15.0,
        _ => return None,
    };
    Some((v * k) as i32)
}

#[derive(Clone, Default)]
struct OStyle {
    parent: Option<String>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    size: Option<u16>,
    colour: Option<u32>,
    script: Option<Script>,
    align: Option<Align>,
    indent: Option<i32>,
    first: Option<i32>,
    before: Option<i32>,
    after: Option<i32>,
    break_before: bool,
    /// a list style's levels: bullet or a number format
    levels: Vec<(bool, String)>,
    /// a paragraph style's outline level ("Heading 2")
    level: Option<u8>,
}

impl OStyle {
    fn over(&self, base: &OStyle) -> OStyle {
        OStyle {
            parent: None,
            bold: self.bold.or(base.bold),
            italic: self.italic.or(base.italic),
            underline: self.underline.or(base.underline),
            strike: self.strike.or(base.strike),
            size: self.size.or(base.size),
            colour: self.colour.or(base.colour),
            script: self.script.or(base.script),
            align: self.align.or(base.align),
            indent: self.indent.or(base.indent),
            first: self.first.or(base.first),
            before: self.before.or(base.before),
            after: self.after.or(base.after),
            break_before: self.break_before,
            levels: if self.levels.is_empty() {
                base.levels.clone()
            } else {
                self.levels.clone()
            },
            level: self.level.or(base.level),
        }
    }

    fn run(&self) -> Style {
        Style {
            bold: self.bold.unwrap_or(false),
            italic: self.italic.unwrap_or(false),
            underline: self.underline.unwrap_or(false),
            strike: self.strike.unwrap_or(false),
            mono: false,
            script: self.script.unwrap_or_default(),
            size: self.size.unwrap_or(0),
            colour: self.colour,
        }
    }

    fn props(&mut self, ev: &Ev) {
        let a = |k: &str| ev.attr("fo", k).or_else(|| ev.attr("style", k));
        if let Some(v) = a("font-weight") {
            self.bold = Some(v == "bold" || v.parse::<u32>().is_ok_and(|w| w >= 600));
        }
        if let Some(v) = a("font-style") {
            self.italic = Some(v == "italic" || v == "oblique");
        }
        if let Some(v) = a("text-underline-style") {
            self.underline = Some(v != "none");
        }
        if let Some(v) = a("text-line-through-style") {
            self.strike = Some(v != "none");
        }
        if let Some(v) = a("font-size").and_then(length) {
            self.size = Some((v / 10).clamp(2, 400) as u16);
        }
        if let Some(v) = a("color") {
            self.colour = crate::docx::hex(v);
        }
        if let Some(v) = a("text-position") {
            self.script = Some(match v.split(' ').next() {
                Some("super") => Script::Super,
                Some("sub") => Script::Sub,
                Some(p) if p.starts_with('-') => Script::Sub,
                Some(p)
                    if p.trim_end_matches('%')
                        .parse::<f32>()
                        .is_ok_and(|x| x > 0.0) =>
                {
                    Script::Super
                }
                _ => Script::Normal,
            });
        }
        if let Some(v) = a("text-align") {
            self.align = Some(match v {
                "center" => Align::Centre,
                "end" | "right" => Align::End,
                _ => Align::Start,
            });
        }
        if let Some(v) = a("margin-left").and_then(length) {
            self.indent = Some(v.clamp(0, 20000));
        }
        if let Some(v) = a("text-indent").and_then(length) {
            self.first = Some(v.clamp(-10000, 10000));
        }
        if let Some(v) = a("margin-top").and_then(length) {
            self.before = Some(v.clamp(0, 2000));
        }
        if let Some(v) = a("margin-bottom").and_then(length) {
            self.after = Some(v.clamp(0, 2000));
        }
        if a("break-before") == Some("page") {
            self.break_before = true;
        }
    }
}

struct Styles {
    by_name: BTreeMap<String, OStyle>,
    body: Option<u16>,
    page: Option<PageSetup>,
}

impl Styles {
    fn read(&mut self, xml: &str) -> Result<()> {
        let mut r = Reader::new(xml);
        let mut cur: Option<(String, OStyle)> = None;
        let mut default = false;
        let mut page: Option<PageSetup> = None;
        for ev in r.by_ref() {
            let ev = ev?;
            match &ev {
                Ev::Start { name, .. } => match name.local {
                    "style" | "list-style" if name.prefix == "style" || name.prefix == "text" => {
                        if let Some(n) = ev.attr("style", "name") {
                            let mut s = OStyle {
                                parent: ev.attr("style", "parent-style-name").map(String::from),
                                ..Default::default()
                            };
                            if let Some(l) = ev
                                .attr("style", "default-outline-level")
                                .and_then(|v| v.parse::<u8>().ok())
                            {
                                s.level = Some(l.clamp(1, 6));
                            }
                            cur = Some((String::from(n), s));
                        }
                    }
                    "default-style" => default = ev.attr("style", "family") == Some("paragraph"),
                    "text-properties" | "paragraph-properties" => {
                        if let Some((_, s)) = &mut cur {
                            s.props(&ev);
                        } else if default
                            && let Some(v) = ev.attr("fo", "font-size").and_then(length)
                        {
                            self.body = Some((v / 10).clamp(2, 400) as u16);
                        }
                    }
                    "list-level-style-bullet" | "list-level-style-number" => {
                        if let Some((_, s)) = &mut cur {
                            let bullet = name.local.ends_with("bullet");
                            let fmt = ev.attr("style", "num-format").unwrap_or("1");
                            let suffix = ev.attr("style", "num-suffix").unwrap_or(".");
                            let mut f = String::from(fmt);
                            f.push('|');
                            f.push_str(suffix);
                            s.levels.push((bullet, f));
                        }
                    }
                    "page-layout-properties" => {
                        let mut p = A4;
                        let l = |k: &str| ev.attr("fo", k).and_then(length);
                        if let (Some(w), Some(h)) = (l("page-width"), l("page-height")) {
                            p.width = w.clamp(1440, 50000);
                            p.height = h.clamp(1440, 50000);
                        }
                        for (i, k) in ["margin-top", "margin-right", "margin-bottom", "margin-left"]
                            .iter()
                            .enumerate()
                        {
                            if let Some(v) = l(k) {
                                p.margin[i] = v.clamp(0, 10000);
                            }
                        }
                        page.get_or_insert(p);
                    }
                    _ => {}
                },
                Ev::End(name) => match name.local {
                    "style" | "list-style" => {
                        if let Some((n, s)) = cur.take() {
                            self.by_name.insert(n, s);
                        }
                    }
                    "default-style" => default = false,
                    _ => {}
                },
                _ => {}
            }
        }
        if self.page.is_none() {
            self.page = page;
        }
        Ok(())
    }

    /// A style with its parents.
    fn get(&self, name: Option<&str>) -> OStyle {
        let Some(mut s) = name.and_then(|n| self.by_name.get(n)).cloned() else {
            return OStyle::default();
        };
        let mut parent = s.parent.clone();
        for _ in 0..8 {
            let Some(p) = parent.as_deref().and_then(|p| self.by_name.get(p)) else {
                break;
            };
            let bb = s.break_before;
            s = s.over(p);
            s.break_before = bb;
            parent = p.parent.clone();
        }
        s
    }
}

/// Read a .odt.
pub fn read<R: ReadAt>(p: &Package<R>) -> Result<Doc> {
    let mut styles = Styles {
        by_name: BTreeMap::new(),
        body: None,
        page: None,
    };
    if p.has("styles.xml")
        && let Ok(b) = p.read("styles.xml")
    {
        styles.read(&text(&b)?)?;
    }
    let content = p.read("content.xml")?;
    let content = text(&content)?;
    styles.read(&content)?; // its automatic styles
    let mut doc = Doc {
        body: styles.body.unwrap_or(24),
        page: styles.page.unwrap_or(A4),
        ..Default::default()
    };
    let mut w = Walker {
        styles: &styles,
        doc: &mut doc,
        lists: Vec::new(),
    };
    let mut r = Reader::new(&content);
    let mut in_text = false;
    while let Some(ev) = r.next() {
        let ev = ev?;
        match &ev {
            Ev::Start { name, .. } if name.is("office", "text") => in_text = true,
            Ev::Start { .. } if in_text => w.block(&ev, &mut r, None)?,
            Ev::End(name) if name.is("office", "text") => in_text = false,
            _ => {}
        }
    }
    doc.trim();
    Ok(doc)
}

struct Walker<'a> {
    styles: &'a Styles,
    doc: &'a mut Doc,
    /// open lists: their style's levels, the item counter, and whether the item's first paragraph is still to come
    lists: Vec<(OStyle, u32, bool)>,
}

impl Walker<'_> {
    /// A block-level element just started; `into`: a table cell's paragraphs.
    fn block(&mut self, ev: &Ev, r: &mut Reader, mut into: Option<&mut Vec<Para>>) -> Result<()> {
        let Ev::Start { name, .. } = ev else {
            return Ok(());
        };
        match (name.prefix, name.local) {
            ("text", "p" | "h") => {
                let s = self.styles.get(ev.attr("text", "style-name"));
                let mut p = Para {
                    align: s.align.unwrap_or_default(),
                    indent: s.indent.unwrap_or(0),
                    first: s.first.unwrap_or(0),
                    before: s.before,
                    after: s.after,
                    ..Default::default()
                };
                if name.local == "h" {
                    let l = ev
                        .attr("text", "outline-level")
                        .and_then(|v| v.parse::<u8>().ok())
                        .or(s.level)
                        .unwrap_or(1);
                    p.kind = ParaKind::Heading(l.clamp(1, 6));
                }
                if let Some(item) = self.item_marker() {
                    p.kind = item;
                    p.indent = 0;
                    p.first = 0;
                }
                let mut images = Vec::new();
                self.inline(r, &mut p, s.run(), &mut images)?;
                match into {
                    Some(v) => v.push(p),
                    None => {
                        if s.break_before && !self.doc.blocks.is_empty() {
                            self.doc.blocks.push(Block::PageBreak);
                        }
                        if !(p.is_empty() && !images.is_empty()) {
                            self.doc.blocks.push(Block::Para(p));
                        }
                        self.doc.blocks.extend(images);
                    }
                }
            }
            ("text", "list") => {
                let s = match ev.attr("text", "style-name") {
                    Some(n) => self.styles.get(Some(n)),
                    None => self.lists.last().map(|l| l.0.clone()).unwrap_or_default(),
                };
                self.lists.push((s, 0, false));
                let depth = r.depth();
                while r.depth() >= depth {
                    let Some(e) = r.next() else { break };
                    let e = e?;
                    if let Ev::Start { name, .. } = &e {
                        if name.is("text", "list-item") {
                            if let Some(l) = self.lists.last_mut() {
                                l.1 += 1;
                                l.2 = true;
                            }
                        } else {
                            self.block(&e, r, into.as_deref_mut())?;
                        }
                    }
                }
                self.lists.pop();
            }
            ("table", "table") if into.is_none() => {
                let t = self.table(r)?;
                self.doc.blocks.push(Block::Table(t));
            }
            ("text", "soft-page-break") => {}
            ("text", "section" | "index-body" | "table-of-content") => {}
            _ => {
                if name.prefix == "text"
                    && (name.local == "sequence-decls" || name.local == "tracked-changes")
                {
                    r.skip_element()?;
                }
            }
        }
        Ok(())
    }

    /// The marker of a list item's first paragraph (only the first paragraph of an item gets one).
    fn item_marker(&mut self) -> Option<ParaKind> {
        let depth = self.lists.len().checked_sub(1)?;
        let (style, n, pending) = self.lists.last_mut()?;
        if !core::mem::replace(pending, false) {
            return None;
        }
        let (bullet, fmt) = style
            .levels
            .get(depth)
            .cloned()
            .unwrap_or((true, String::from("1|.")));
        let marker = if bullet {
            String::from("•")
        } else {
            let (f, suffix) = fmt.split_once('|').unwrap_or(("1", "."));
            let word = match f {
                "a" => "lowerLetter",
                "A" => "upperLetter",
                "i" => "lowerRoman",
                "I" => "upperRoman",
                _ => "decimal",
            };
            let mut m = crate::docx::counter(*n, word);
            m.push_str(suffix);
            m
        };
        Some(ParaKind::Item {
            depth: depth.min(8) as u8,
            marker,
        })
    }

    /// The inline content of a `text:p`/`text:h` up to its end.
    fn inline(
        &mut self,
        r: &mut Reader,
        p: &mut Para,
        base: Style,
        images: &mut Vec<Block>,
    ) -> Result<()> {
        let depth = r.depth();
        let mut stack: Vec<Style> = Vec::new();
        let mut frame = (0, 0);
        while r.depth() >= depth {
            let Some(ev) = r.next() else { break };
            let ev = ev?;
            let cur = stack.last().copied().unwrap_or(base);
            match &ev {
                Ev::Start { name, .. } => match (name.prefix, name.local) {
                    ("text", "span") => {
                        let s = self.styles.get(ev.attr("text", "style-name"));
                        let mut st = cur;
                        let run = s.run();
                        st.bold = s.bold.unwrap_or(st.bold);
                        st.italic = s.italic.unwrap_or(st.italic);
                        st.underline = s.underline.unwrap_or(st.underline);
                        st.strike = s.strike.unwrap_or(st.strike);
                        if s.size.is_some() {
                            st.size = run.size;
                        }
                        if s.colour.is_some() {
                            st.colour = run.colour;
                        }
                        if s.script.is_some() {
                            st.script = run.script;
                        }
                        stack.push(st);
                    }
                    ("text", "a") => stack.push(cur),
                    ("text", "s") => {
                        let n = ev
                            .attr("text", "c")
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(1)
                            .min(100);
                        p.push(&" ".repeat(n), cur);
                    }
                    ("text", "tab") => p.push("\t", cur),
                    ("text", "line-break") => p.push("\n", cur),
                    ("text", "note" | "annotation" | "bookmark-ref" | "ruby-text") => {
                        r.skip_element()?
                    }
                    ("draw", "frame") => {
                        let l = |k| ev.attr("svg", k).and_then(length).unwrap_or(0);
                        frame = (l("width"), l("height"));
                    }
                    ("draw", "image") => {
                        if let Some(href) = ev.attr("xlink", "href").filter(|h| !h.contains(':')) {
                            let src = String::from(href.trim_start_matches("./"));
                            images.push(self.doc.image(ImageSrc::Member(src), frame.0, frame.1));
                        }
                    }
                    _ => {}
                },
                Ev::End(name) => {
                    if (name.prefix == "text" && (name.local == "span" || name.local == "a"))
                        && !stack.is_empty()
                    {
                        stack.pop();
                    }
                }
                Ev::Text(t) => {
                    // ODF collapses white space like HTML: runs of spaces and new lines are one space
                    let mut s = String::with_capacity(t.len());
                    let mut space = p.text().ends_with(' ') || p.is_empty();
                    for c in t.chars() {
                        if c.is_whitespace() && c != '\u{a0}' {
                            if !space {
                                s.push(' ');
                            }
                            space = true;
                        } else {
                            s.push(c);
                            space = false;
                        }
                    }
                    p.push(&s, cur);
                }
            }
        }
        Ok(())
    }

    fn table(&mut self, r: &mut Reader) -> Result<Table> {
        let depth = r.depth();
        let mut t = Table::default();
        while r.depth() >= depth {
            let Some(ev) = r.next() else { break };
            let ev = ev?;
            let Ev::Start { name, .. } = &ev else {
                continue;
            };
            match (name.prefix, name.local) {
                ("table", "table-row") => t.rows.push(Vec::new()),
                ("table", "table-cell") => {
                    let span = ev
                        .attr("table", "number-columns-spanned")
                        .and_then(|v| v.parse::<u16>().ok())
                        .unwrap_or(1);
                    let mut cell = Cell {
                        span: span.clamp(1, 64),
                        ..Default::default()
                    };
                    let cd = r.depth();
                    while r.depth() >= cd {
                        let Some(e) = r.next() else { break };
                        let e = e?;
                        if matches!(&e, Ev::Start { .. }) {
                            self.block(&e, r, Some(&mut cell.paras))?;
                        }
                    }
                    if let Some(row) = t.rows.last_mut() {
                        row.push(cell);
                    }
                }
                ("table", "covered-table-cell") => {}
                _ => {}
            }
        }
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths() {
        assert_eq!(length("2.54cm"), Some(1440));
        assert_eq!(length("12pt"), Some(240));
        assert_eq!(length("0.5in"), Some(720));
        assert_eq!(length("12"), None);
    }
}
