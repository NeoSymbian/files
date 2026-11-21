// SPDX-License-Identifier: Apache-2.0
//! Word (.docx, Office Open XML): `word/document.xml` with its styles (heading levels, run defaults), numbering
//! (bullets and numbers) and relationships (images in `word/media`), tables, page breaks and the page setup
//! (`w:sectPr`). Headers, footers, footnotes, comments and deleted text are not shown; text boxes show their text
//! where they are.

use crate::model::*;
use crate::xml::{Ev, Reader, text};
use crate::{Package, Result, join};
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use neo_format_archive::ReadAt;

/// A paragraph style: its heading level, alignment and run defaults (after `basedOn`).
#[derive(Clone, Default)]
struct PStyle {
    based: Option<String>,
    level: Option<u8>,
    align: Option<Align>,
    run: RunProps,
}

/// Run properties as the file sets them (`None` = inherit).
#[derive(Clone, Copy, Default)]
struct RunProps {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    size: Option<u16>,
    colour: Option<Option<u32>>,
    script: Option<Script>,
}

impl RunProps {
    fn over(self, base: RunProps) -> RunProps {
        RunProps {
            bold: self.bold.or(base.bold),
            italic: self.italic.or(base.italic),
            underline: self.underline.or(base.underline),
            strike: self.strike.or(base.strike),
            size: self.size.or(base.size),
            colour: self.colour.or(base.colour),
            script: self.script.or(base.script),
        }
    }
    fn style(self) -> Style {
        Style {
            bold: self.bold.unwrap_or(false),
            italic: self.italic.unwrap_or(false),
            underline: self.underline.unwrap_or(false),
            strike: self.strike.unwrap_or(false),
            mono: false,
            script: self.script.unwrap_or_default(),
            size: self.size.unwrap_or(0),
            colour: self.colour.flatten(),
        }
    }
    /// One `w:rPr` child.
    fn set(&mut self, ev: &Ev) {
        let Ev::Start { name, .. } = ev else { return };
        let on = || !matches!(ev.attr("w", "val"), Some("0" | "false" | "off" | "none"));
        match name.local {
            "b" => self.bold = Some(on()),
            "i" => self.italic = Some(on()),
            "u" => self.underline = Some(on()),
            "strike" | "dstrike" => self.strike = Some(on()),
            "sz" => {
                self.size = ev
                    .attr("w", "val")
                    .and_then(|v| v.parse::<u16>().ok())
                    .map(|v| v.clamp(2, 400))
            }
            "color" => self.colour = Some(ev.attr("w", "val").and_then(hex)),
            "vertAlign" => {
                self.script = Some(match ev.attr("w", "val") {
                    Some("superscript") => Script::Super,
                    Some("subscript") => Script::Sub,
                    _ => Script::Normal,
                })
            }
            _ => {}
        }
    }
}

/// `1F4E8C` → 0x1f4e8c; `auto` → none.
pub(crate) fn hex(s: &str) -> Option<u32> {
    let s = s.trim_start_matches('#');
    (s.len() == 6)
        .then(|| u32::from_str_radix(s, 16).ok())
        .flatten()
}

fn int(ev: &Ev, local: &str) -> Option<i32> {
    ev.attr("w", local)
        .and_then(|v| v.parse::<f32>().ok())
        .map(|v| v as i32)
}

#[derive(Clone)]
struct Level {
    format: String,
    text: String,
    start: u32,
}

struct Numbering {
    /// numId → abstractNumId
    nums: BTreeMap<String, String>,
    /// abstractNumId → levels
    abstracts: BTreeMap<String, Vec<Level>>,
    counters: BTreeMap<(String, u8), u32>,
}

impl Numbering {
    fn read(xml: &str) -> Result<Numbering> {
        let mut n = Numbering {
            nums: BTreeMap::new(),
            abstracts: BTreeMap::new(),
            counters: BTreeMap::new(),
        };
        let mut r = Reader::new(xml);
        let (mut abs, mut num): (Option<String>, Option<String>) = (None, None);
        let mut lvl: Option<Level> = None;
        for ev in r.by_ref() {
            let ev = ev?;
            match &ev {
                Ev::Start { name, .. } => match name.local {
                    "abstractNum" => abs = ev.attr("w", "abstractNumId").map(String::from),
                    "lvl" => {
                        lvl = Some(Level {
                            format: String::from("decimal"),
                            text: String::from("%1."),
                            start: 1,
                        })
                    }
                    "numFmt" => {
                        if let (Some(l), Some(v)) = (&mut lvl, ev.attr("w", "val")) {
                            l.format = String::from(v);
                        }
                    }
                    "lvlText" => {
                        if let (Some(l), Some(v)) = (&mut lvl, ev.attr("w", "val")) {
                            l.text = String::from(v);
                        }
                    }
                    "start" => {
                        if let (Some(l), Some(v)) = (&mut lvl, int(&ev, "val")) {
                            l.start = v.max(0) as u32;
                        }
                    }
                    "num" => num = ev.attr("w", "numId").map(String::from),
                    "abstractNumId" => {
                        if let (Some(id), Some(v)) = (&num, ev.attr("w", "val")) {
                            n.nums.insert(id.clone(), String::from(v));
                        }
                    }
                    _ => {}
                },
                Ev::End(name) if name.local == "lvl" => {
                    if let (Some(a), Some(l)) = (&abs, lvl.take()) {
                        n.abstracts.entry(a.clone()).or_default().push(l);
                    }
                }
                _ => {}
            }
        }
        Ok(n)
    }

    /// The marker of the next item at (num, level); deeper counters restart.
    fn marker(&mut self, num: &str, level: u8) -> Option<String> {
        let abs = self.nums.get(num)?;
        let levels = self.abstracts.get(abs)?;
        let l = levels.get(level as usize)?.clone();
        if l.format == "none" {
            return Some(String::new());
        }
        let start = l.start;
        let n = *self
            .counters
            .entry((String::from(num), level))
            .and_modify(|c| *c += 1)
            .or_insert(start);
        self.counters.retain(|(k, lv), _| k != num || *lv <= level);
        if l.format == "bullet" {
            return Some(String::from(match l.text.chars().next() {
                Some('o') => "◦",
                Some('\u{f0a7}' | '\u{f0fc}' | '§') => "▪",
                _ => "•",
            }));
        }
        // "%1." with every level's counter
        let mut out = String::new();
        let mut it = l.text.chars().peekable();
        while let Some(c) = it.next() {
            if c == '%'
                && let Some(d) = it.peek().and_then(|d| d.to_digit(10))
            {
                it.next();
                let lv = (d as u8).saturating_sub(1);
                let fmt = levels
                    .get(lv as usize)
                    .map_or("decimal", |l| l.format.as_str());
                let v = if lv == level {
                    n
                } else {
                    self.counters
                        .get(&(String::from(num), lv))
                        .copied()
                        .unwrap_or(1)
                };
                out.push_str(&counter(v, fmt));
            } else {
                out.push(c);
            }
        }
        Some(out)
    }
}

/// A list counter in a numbering format.
pub(crate) fn counter(n: u32, format: &str) -> String {
    match format {
        "lowerLetter" | "upperLetter" => {
            let mut s = String::new();
            let mut k = n.max(1);
            while k > 0 {
                k -= 1;
                s.insert(0, (b'a' + (k % 26) as u8) as char);
                k /= 26;
            }
            if format == "upperLetter" {
                s.to_uppercase()
            } else {
                s
            }
        }
        "lowerRoman" | "upperRoman" => {
            let mut s = String::new();
            let mut k = n.clamp(1, 3999);
            for (v, r) in [
                (1000, "m"),
                (900, "cm"),
                (500, "d"),
                (400, "cd"),
                (100, "c"),
                (90, "xc"),
                (50, "l"),
                (40, "xl"),
                (10, "x"),
                (9, "ix"),
                (5, "v"),
                (4, "iv"),
                (1, "i"),
            ] {
                while k >= v {
                    s.push_str(r);
                    k -= v;
                }
            }
            if format == "upperRoman" {
                s.to_uppercase()
            } else {
                s
            }
        }
        _ => n.to_string(),
    }
}

/// The styles: paragraph styles by id, and the document's run defaults.
fn styles(xml: &str) -> Result<(BTreeMap<String, PStyle>, RunProps)> {
    let mut out = BTreeMap::new();
    let mut defaults = RunProps::default();
    let mut r = Reader::new(xml);
    let mut cur: Option<(String, PStyle)> = None;
    let mut in_defaults = false;
    let mut in_rpr = false;
    for ev in r.by_ref() {
        let ev = ev?;
        match &ev {
            Ev::Start { name, .. } => match name.local {
                "rPrDefault" => in_defaults = true,
                "style" => {
                    in_defaults = false;
                    cur = ev
                        .attr("w", "styleId")
                        .map(|id| (String::from(id), PStyle::default()));
                    if let Some((id, s)) = &mut cur {
                        let lower = id.to_ascii_lowercase();
                        if lower == "title" {
                            s.level = Some(1);
                        } else if let Some(n) = lower
                            .strip_prefix("heading")
                            .and_then(|n| n.parse::<u8>().ok())
                        {
                            s.level = Some(n.clamp(1, 6));
                        }
                    }
                }
                "name" => {
                    if let (Some((_, s)), Some(v)) = (&mut cur, ev.attr("w", "val")) {
                        let v = v.to_ascii_lowercase();
                        if let Some(n) = v
                            .strip_prefix("heading ")
                            .and_then(|n| n.parse::<u8>().ok())
                        {
                            s.level = Some(n.clamp(1, 6));
                        } else if v == "title" {
                            s.level = Some(1);
                        }
                    }
                }
                "basedOn" => {
                    if let (Some((_, s)), Some(v)) = (&mut cur, ev.attr("w", "val")) {
                        s.based = Some(String::from(v));
                    }
                }
                "outlineLvl" => {
                    if let (Some((_, s)), Some(v)) = (&mut cur, int(&ev, "val"))
                        && (0..6).contains(&v)
                    {
                        s.level = Some(v as u8 + 1);
                    }
                }
                "jc" => {
                    if let Some((_, s)) = &mut cur {
                        s.align = Some(align(ev.attr("w", "val")));
                    }
                }
                "rPr" => in_rpr = true,
                _ if in_rpr => {
                    if in_defaults {
                        defaults.set(&ev);
                    } else if let Some((_, s)) = &mut cur {
                        s.run.set(&ev);
                    }
                }
                _ => {}
            },
            Ev::End(name) => match name.local {
                "rPr" => in_rpr = false,
                "rPrDefault" => in_defaults = false,
                "style" => {
                    if let Some((id, s)) = cur.take() {
                        out.insert(id, s);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    // basedOn, resolved (at most 8 deep: a loop in the file stops there)
    let ids: Vec<String> = out.keys().cloned().collect();
    for id in ids {
        let mut s = out[&id].clone();
        let mut base = s.based.clone();
        for _ in 0..8 {
            let Some(b) = base.and_then(|b| out.get(&b)) else {
                break;
            };
            s.run = s.run.over(b.run);
            s.level = s.level.or(b.level);
            s.align = s.align.or(b.align);
            base = b.based.clone();
        }
        out.insert(id, s);
    }
    Ok((out, defaults))
}

fn align(v: Option<&str>) -> Align {
    match v {
        Some("center") => Align::Centre,
        Some("right" | "end") => Align::End,
        _ => Align::Start,
    }
}

/// Relationship ids to part names (`rId4` → `word/media/image1.png`).
fn rels(xml: &str, base: &str) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let mut r = Reader::new(xml);
    for ev in r.by_ref() {
        let ev = ev?;
        if let Ev::Start { name, .. } = &ev
            && name.local == "Relationship"
            && ev.attr("", "TargetMode") != Some("External")
            && let (Some(id), Some(t)) = (ev.attr("", "Id"), ev.attr("", "Target"))
        {
            out.insert(String::from(id), join(base, t));
        }
    }
    Ok(out)
}

/// Read a .docx.
pub fn read<R: ReadAt>(p: &Package<R>) -> Result<Doc> {
    let doc_xml = p.read("word/document.xml")?;
    let opt = |name: &str| if p.has(name) { p.read(name).ok() } else { None };
    let styles_xml = opt("word/styles.xml");
    let numbering_xml = opt("word/numbering.xml");
    let rels_xml = opt("word/_rels/document.xml.rels");
    let (styles, defaults) = match &styles_xml {
        Some(b) => styles(&text(b)?)?,
        None => (BTreeMap::new(), RunProps::default()),
    };
    let mut numbering = match &numbering_xml {
        Some(b) => Numbering::read(&text(b)?)?,
        None => Numbering {
            nums: BTreeMap::new(),
            abstracts: BTreeMap::new(),
            counters: BTreeMap::new(),
        },
    };
    let rels = match &rels_xml {
        Some(b) => rels(&text(b)?, "word/document.xml")?,
        None => BTreeMap::new(),
    };
    let xml = text(&doc_xml)?;
    let mut doc = Doc {
        body: defaults.size.unwrap_or(24),
        ..Default::default()
    };
    let mut w = Walker {
        styles: &styles,
        defaults,
        numbering: &mut numbering,
        rels: &rels,
        doc: &mut doc,
    };
    w.body(&mut Reader::new(&xml))?;
    doc.trim();
    Ok(doc)
}

struct Walker<'a> {
    styles: &'a BTreeMap<String, PStyle>,
    defaults: RunProps,
    numbering: &'a mut Numbering,
    rels: &'a BTreeMap<String, String>,
    doc: &'a mut Doc,
}

/// What a paragraph collects: the paragraph, images in it, a page break after it.
struct ParaOut {
    para: Para,
    images: Vec<Block>,
    page_break: bool,
    before_break: bool,
}

impl Walker<'_> {
    fn body(&mut self, r: &mut Reader) -> Result<()> {
        while let Some(ev) = r.next() {
            let ev = ev?;
            let Ev::Start { name, .. } = &ev else {
                continue;
            };
            match name.local {
                "p" if name.prefix == "w" => {
                    let out = self.para(r)?;
                    self.push(out);
                }
                "tbl" => {
                    let t = self.table(r)?;
                    self.doc.blocks.push(Block::Table(t));
                }
                "sectPr" => self.section(r)?,
                // the rest of the body: headers' references, bookmarks, sdt wrappers (walked into)
                _ => {}
            }
        }
        Ok(())
    }

    fn push(&mut self, out: ParaOut) {
        if out.before_break {
            self.doc.blocks.push(Block::PageBreak);
        }
        // a paragraph that only holds pictures or a page break isn't a line of its own
        let only_images = out.para.is_empty() && (!out.images.is_empty() || out.page_break);
        if !only_images {
            self.doc.blocks.push(Block::Para(out.para));
        }
        self.doc.blocks.extend(out.images);
        if out.page_break {
            self.doc.blocks.push(Block::PageBreak);
        }
    }

    fn section(&mut self, r: &mut Reader) -> Result<()> {
        let depth = r.depth();
        while r.depth() >= depth {
            let Some(ev) = r.next() else { break };
            let ev = ev?;
            if let Ev::Start { name, .. } = &ev {
                match name.local {
                    "pgSz" => {
                        if let (Some(w), Some(h)) = (int(&ev, "w"), int(&ev, "h")) {
                            self.doc.page.width = w.clamp(1440, 50000);
                            self.doc.page.height = h.clamp(1440, 50000);
                        }
                    }
                    "pgMar" => {
                        let m = &mut self.doc.page.margin;
                        for (i, k) in ["top", "right", "bottom", "left"].iter().enumerate() {
                            if let Some(v) = int(&ev, k) {
                                m[i] = v.abs().min(10000);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// A `w:p` (its start was the last event).
    fn para(&mut self, r: &mut Reader) -> Result<ParaOut> {
        let depth = r.depth();
        let mut out = ParaOut {
            para: Para::default(),
            images: Vec::new(),
            page_break: false,
            before_break: false,
        };
        let mut pstyle: Option<&PStyle> = None;
        let mut num: (Option<String>, u8) = (None, 0);
        let mut run = RunProps::default();
        let (mut in_ppr, mut in_rpr, mut in_run, mut in_text, mut deleted, mut fallback) =
            (false, false, false, false, 0, 0);
        let mut image_size = (0, 0);
        while r.depth() >= depth {
            let Some(ev) = r.next() else { break };
            let ev = ev?;
            match &ev {
                Ev::Start { name, .. } => {
                    if deleted > 0 || fallback > 0 {
                        if name.local == "del" {
                            deleted += 1;
                        }
                        if name.local == "Fallback" {
                            fallback += 1;
                        }
                        continue;
                    }
                    match name.local {
                        "pPr" => in_ppr = true,
                        "rPr" => in_rpr = true,
                        "pStyle" => {
                            pstyle = ev.attr("w", "val").and_then(|v| self.styles.get(v));
                        }
                        "jc" if in_ppr => out.para.align = align(ev.attr("w", "val")),
                        "ind" if in_ppr => {
                            out.para.indent = int(&ev, "left")
                                .or_else(|| int(&ev, "start"))
                                .unwrap_or(0)
                                .clamp(0, 20000);
                            out.para.first = int(&ev, "firstLine").unwrap_or(0)
                                - int(&ev, "hanging").unwrap_or(0);
                            out.para.first = out.para.first.clamp(-10000, 10000);
                        }
                        "spacing" if in_ppr => {
                            out.para.before = int(&ev, "before").map(|v| v.clamp(0, 2000));
                            out.para.after = int(&ev, "after").map(|v| v.clamp(0, 2000));
                        }
                        "ilvl" if in_ppr => num.1 = int(&ev, "val").unwrap_or(0).clamp(0, 8) as u8,
                        "numId" if in_ppr => num.0 = ev.attr("w", "val").map(String::from),
                        "pageBreakBefore" if in_ppr => {
                            out.before_break = !matches!(ev.attr("w", "val"), Some("0" | "false"))
                        }
                        "r" => {
                            in_run = true;
                            run = RunProps::default();
                        }
                        "t" | "delText" | "instrText" if in_run => in_text = name.local == "t",
                        "tab" if in_run => self.text(&mut out.para, "\t", pstyle, run),
                        "br" if in_run => match ev.attr("w", "type") {
                            Some("page") => out.page_break = true,
                            Some("column") => {}
                            _ => self.text(&mut out.para, "\n", pstyle, run),
                        },
                        "cr" if in_run => self.text(&mut out.para, "\n", pstyle, run),
                        "noBreakHyphen" if in_run => self.text(&mut out.para, "‑", pstyle, run),
                        "softHyphen" if in_run => self.text(&mut out.para, "\u{ad}", pstyle, run),
                        "del" => deleted = 1,
                        "Fallback" => fallback = 1,
                        "extent" if name.prefix == "wp" => {
                            let emu = |k| {
                                ev.attr("", k)
                                    .and_then(|v| v.parse::<i64>().ok())
                                    .unwrap_or(0)
                            };
                            image_size = (
                                (emu("cx") / 635).clamp(0, 50000) as i32,
                                (emu("cy") / 635).clamp(0, 50000) as i32,
                            );
                        }
                        "blip" => {
                            if let Some(part) =
                                ev.attr("r", "embed").and_then(|id| self.rels.get(id))
                            {
                                let b = self.doc.image(
                                    ImageSrc::Member(part.clone()),
                                    image_size.0,
                                    image_size.1,
                                );
                                out.images.push(b);
                            }
                        }
                        // the paragraph mark's own run properties (pPr/rPr) style nothing we show
                        _ if in_rpr && in_run => run.set(&ev),
                        _ => {}
                    }
                }
                Ev::End(name) => {
                    if deleted > 0 && name.local == "del" {
                        deleted -= 1;
                        continue;
                    }
                    if fallback > 0 && name.local == "Fallback" {
                        fallback -= 1;
                        continue;
                    }
                    match name.local {
                        "pPr" => in_ppr = false,
                        "rPr" => in_rpr = false,
                        "r" => in_run = false,
                        "t" => in_text = false,
                        _ => {}
                    }
                }
                Ev::Text(t) => {
                    if in_text && deleted == 0 && fallback == 0 {
                        self.text(&mut out.para, t, pstyle, run);
                    }
                }
            }
        }
        if let Some(s) = pstyle {
            if let Some(level) = s.level {
                out.para.kind = ParaKind::Heading(level);
            }
            if out.para.align == Align::Start {
                out.para.align = s.align.unwrap_or_default();
            }
        }
        if let (Some(id), level) = (&num.0, num.1)
            && let Some(m) = self.numbering.marker(id, level)
            && !m.is_empty()
        {
            out.para.kind = ParaKind::Item {
                depth: level,
                marker: m,
            };
            out.para.indent = 0;
            out.para.first = 0;
        }
        Ok(out)
    }

    fn text(&self, p: &mut Para, t: &str, pstyle: Option<&PStyle>, run: RunProps) {
        let base = pstyle.map_or(self.defaults, |s| s.run.over(self.defaults));
        let mut style = run.over(base).style();
        if style.size == self.defaults.size.unwrap_or(24)
            && pstyle.is_none_or(|s| s.run.size.is_none())
            && run.size.is_none()
        {
            style.size = 0; // the body size: reflow draws it at the reader's size
        }
        p.push(t, style);
    }

    /// A `w:tbl` (its start was the last event).
    fn table(&mut self, r: &mut Reader) -> Result<Table> {
        let depth = r.depth();
        let mut t = Table::default();
        let mut row: Option<Vec<Cell>> = None;
        let mut cell: Option<Cell> = None;
        let mut merged_away = false;
        while r.depth() >= depth {
            let Some(ev) = r.next() else { break };
            let ev = ev?;
            match &ev {
                Ev::Start { name, .. } => match name.local {
                    "gridCol" => t.widths.push(int(&ev, "w").unwrap_or(0).clamp(0, 50000)),
                    "tr" => row = Some(Vec::new()),
                    "tc" => {
                        cell = Some(Cell {
                            span: 1,
                            ..Default::default()
                        });
                        merged_away = false;
                    }
                    "gridSpan" => {
                        if let Some(c) = &mut cell {
                            c.span = int(&ev, "val").unwrap_or(1).clamp(1, 64) as u16;
                        }
                    }
                    "vMerge" => merged_away = ev.attr("w", "val").is_none_or(|v| v == "continue"),
                    "p" if name.prefix == "w" => {
                        let out = self.para(r)?;
                        if let Some(c) = &mut cell
                            && !merged_away
                        {
                            c.paras.push(out.para);
                        }
                    }
                    "tbl" => {
                        // a table in a cell: its text, row by row
                        let inner = self.table(r)?;
                        if let Some(c) = &mut cell {
                            for row in inner.rows {
                                for ic in row {
                                    c.paras.extend(ic.paras);
                                }
                            }
                        }
                    }
                    _ => {}
                },
                Ev::End(name) => match name.local {
                    "tc" => {
                        if let (Some(rw), Some(c)) = (&mut row, cell.take()) {
                            rw.push(c);
                        }
                    }
                    "tr" => {
                        if let Some(rw) = row.take() {
                            t.rows.push(rw);
                        }
                    }
                    _ => {}
                },
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
    fn counters() {
        assert_eq!(counter(28, "lowerLetter"), "ab");
        assert_eq!(counter(14, "upperRoman"), "XIV");
        assert_eq!(counter(3, "decimal"), "3");
    }
}
