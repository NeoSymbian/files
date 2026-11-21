// SPDX-License-Identifier: Apache-2.0
//! XHTML (an EPUB chapter) as blocks: paragraphs and headings, lists, quotes, preformatted text, rules, tables and
//! pictures; bold, italic, underline, strike, super/subscript and code; an element's inline `style` for
//! font-weight, font-style, text-align and `display: none`. Stylesheets aren't read; white space collapses as in
//! HTML.

use crate::model::*;
use crate::xml::{Ev, Name, Reader};
use crate::{Result, join};
use alloc::string::String;
use alloc::vec::Vec;

/// Read `xhtml`; `path` is its member (picture links are relative to it).
pub fn read(xhtml: &str, path: &str) -> Result<Doc> {
    let mut h = Html {
        path,
        doc: Doc::default(),
        para: None,
        styles: Vec::new(),
        lists: Vec::new(),
        quote: 0,
        pre: 0,
        align: Vec::new(),
        table: None,
        space: true,
    };
    let mut r = Reader::new(xhtml);
    while let Some(ev) = r.next() {
        let ev = ev?;
        match &ev {
            Ev::Start { name, .. } => {
                if matches!(
                    name.local,
                    "head" | "script" | "style" | "noscript" | "title"
                ) {
                    r.skip_element()?;
                    continue;
                }
                let css = ev.attr("", "style").unwrap_or("");
                if css.replace(' ', "").contains("display:none") {
                    r.skip_element()?;
                    continue;
                }
                h.start(*name, &ev, css);
            }
            Ev::End(name) => h.end(*name),
            Ev::Text(t) => h.text(t),
        }
    }
    h.flush();
    if let Some(t) = h.table.take() {
        h.doc.blocks.push(Block::Table(t.0));
    }
    h.doc.trim();
    Ok(h.doc)
}

struct Html<'a> {
    path: &'a str,
    doc: Doc,
    para: Option<Para>,
    /// The inline style of each open element (pushed for every element, so ends pop in step).
    styles: Vec<Style>,
    /// Open lists: ordered, and the item counter.
    lists: Vec<(bool, u32)>,
    quote: u32,
    pre: u32,
    /// Alignment of each open block element.
    align: Vec<Option<Align>>,
    /// A table being read: the table, and the cell being filled.
    table: Option<(Table, Option<Cell>)>,
    /// The text so far ends in white space (or nothing): the next space collapses.
    space: bool,
}

fn block(local: &str) -> bool {
    matches!(
        local,
        "p" | "div"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "li"
            | "blockquote"
            | "pre"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "aside"
            | "figure"
            | "figcaption"
            | "dt"
            | "dd"
            | "body"
            | "center"
            | "address"
            | "main"
            | "nav"
    )
}

impl Html<'_> {
    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or_default()
    }

    /// Finish the paragraph being filled (into the table's cell when one is open).
    fn flush(&mut self) {
        if let Some(p) = self.para.take()
            && !p.is_empty()
        {
            match &mut self.table {
                Some((_, Some(cell))) => cell.paras.push(p),
                Some((t, None)) => {
                    // text between cells: a row of its own
                    t.rows.push(alloc::vec![Cell {
                        paras: alloc::vec![p],
                        span: 1,
                        header: false
                    }]);
                }
                None => self.doc.blocks.push(Block::Para(p)),
            }
        }
        self.space = true;
    }

    fn new_para(&mut self, kind: ParaKind) {
        self.flush();
        let align = self.align.iter().rev().find_map(|a| *a).unwrap_or_default();
        let kind = if kind == ParaKind::Body && self.pre > 0 {
            ParaKind::Pre
        } else if kind == ParaKind::Body && self.quote > 0 {
            ParaKind::Quote
        } else {
            kind
        };
        self.para = Some(Para {
            kind,
            align,
            ..Default::default()
        });
    }

    fn start(&mut self, name: Name, ev: &Ev, css: &str) {
        let mut st = self.style();
        match name.local {
            "b" | "strong" | "th" => st.bold = true,
            "i" | "em" | "cite" | "var" | "dfn" => st.italic = true,
            "u" | "ins" => st.underline = true,
            "s" | "strike" | "del" => st.strike = true,
            "sup" => st.script = Script::Super,
            "sub" => st.script = Script::Sub,
            "code" | "kbd" | "samp" | "tt" | "pre" => st.mono = true,
            _ => {}
        }
        for decl in css.split(';') {
            let Some((k, v)) = decl.split_once(':') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "font-weight" => {
                    st.bold =
                        v == "bold" || v == "bolder" || v.parse::<u32>().is_ok_and(|w| w >= 600)
                }
                "font-style" => st.italic = v == "italic" || v == "oblique",
                _ => {}
            }
        }
        self.styles.push(st);
        let align = css.split(';').find_map(|d| {
            let (k, v) = d.split_once(':')?;
            (k.trim() == "text-align").then(|| match v.trim() {
                "center" => Align::Centre,
                "right" | "end" => Align::End,
                _ => Align::Start,
            })
        });
        let align = align.or((name.local == "center").then_some(Align::Centre));
        if block(name.local) {
            self.align.push(align);
        }
        match name.local {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = name.local.as_bytes()[1] - b'0';
                self.new_para(ParaKind::Heading(level));
            }
            "ul" | "ol" => {
                self.flush();
                let start = ev
                    .attr("", "start")
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(1);
                self.lists
                    .push((name.local == "ol", start.saturating_sub(1)));
            }
            "li" => {
                let depth = self.lists.len().saturating_sub(1).min(8) as u8;
                let marker = match self.lists.last_mut() {
                    Some((true, n)) => {
                        *n = n.saturating_add(1);
                        alloc::format!("{n}.")
                    }
                    _ => String::from(if depth.is_multiple_of(2) {
                        "•"
                    } else {
                        "◦"
                    }),
                };
                self.new_para(ParaKind::Item { depth, marker });
            }
            "blockquote" => {
                self.quote += 1;
                self.new_para(ParaKind::Body);
            }
            "pre" => {
                self.pre += 1;
                self.new_para(ParaKind::Pre);
            }
            "hr" => {
                self.flush();
                self.doc.blocks.push(Block::Rule);
            }
            "br" => {
                if self.para.is_none() {
                    self.new_para(ParaKind::Body);
                }
                if let Some(p) = &mut self.para {
                    p.push("\n", st);
                }
                self.space = true;
            }
            "img" | "image" => {
                let src = ev
                    .attr("", "src")
                    .or_else(|| ev.attr("xlink", "href"))
                    .or_else(|| ev.attr("", "href"));
                if let Some(src) = src.filter(|s| !s.contains(':')) {
                    let in_para = self.para.as_ref().is_some_and(|p| !p.is_empty());
                    let mut kind = None;
                    if in_para {
                        kind = self.para.as_ref().map(|p| p.kind.clone());
                        self.flush();
                    }
                    let b = self.doc.image(ImageSrc::Member(join(self.path, src)), 0, 0);
                    match &mut self.table {
                        Some(_) => {} // pictures in tables: not shown
                        None => self.doc.blocks.push(b),
                    }
                    if let Some(k) = kind {
                        self.new_para(k);
                    }
                }
            }
            "table" => {
                self.flush();
                if self.table.is_none() {
                    self.table = Some((Table::default(), None));
                }
            }
            "tr" => {
                self.flush();
                if let Some((t, _)) = &mut self.table {
                    t.rows.push(Vec::new());
                }
            }
            "td" | "th" => {
                self.flush();
                if let Some((_, cell)) = &mut self.table {
                    let span = ev
                        .attr("", "colspan")
                        .and_then(|s| s.parse::<u16>().ok())
                        .unwrap_or(1)
                        .clamp(1, 64);
                    *cell = Some(Cell {
                        paras: Vec::new(),
                        span,
                        header: name.local == "th",
                    });
                }
            }
            l if block(l) => self.new_para(ParaKind::Body),
            _ => {}
        }
    }

    fn end(&mut self, name: Name) {
        self.styles.pop();
        if block(name.local) {
            self.align.pop();
        }
        match name.local {
            "ul" | "ol" => {
                self.flush();
                self.lists.pop();
            }
            "blockquote" => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
            }
            "pre" => {
                self.flush();
                self.pre = self.pre.saturating_sub(1);
            }
            "td" | "th" => {
                self.flush();
                if let Some((t, cell)) = &mut self.table
                    && let Some(c) = cell.take()
                {
                    match t.rows.last_mut() {
                        Some(row) => row.push(c),
                        None => t.rows.push(alloc::vec![c]),
                    }
                }
            }
            "table" => {
                self.flush();
                if let Some((t, _)) = self.table.take()
                    && !t.rows.is_empty()
                {
                    self.doc.blocks.push(Block::Table(t));
                }
            }
            l if block(l) => self.flush(),
            _ => {}
        }
    }

    fn text(&mut self, t: &str) {
        let st = self.style();
        if self.pre > 0 {
            if self.para.is_none() {
                self.new_para(ParaKind::Pre);
            }
            if let Some(p) = &mut self.para {
                p.push(&t.replace("\r\n", "\n"), st);
            }
            return;
        }
        let mut s = String::with_capacity(t.len());
        for c in t.chars() {
            if c.is_whitespace() && c != '\u{a0}' {
                if !self.space {
                    s.push(' ');
                }
                self.space = true;
            } else {
                s.push(c);
                self.space = false;
            }
        }
        if s.is_empty() || (s == " " && self.para.is_none()) {
            return;
        }
        if self.para.is_none() {
            // text straight in a <div> or <body>
            self.new_para(ParaKind::Body);
            s = String::from(s.trim_start());
            self.space = s.ends_with(' ');
        }
        if let Some(p) = &mut self.para {
            p.push(&s, st);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(d: &Doc) -> Vec<String> {
        d.blocks
            .iter()
            .map(|b| match b {
                Block::Para(p) => match &p.kind {
                    ParaKind::Heading(l) => alloc::format!("h{l} {}", p.text()),
                    ParaKind::Item { marker, depth } => {
                        alloc::format!("{depth}{marker} {}", p.text())
                    }
                    ParaKind::Quote => alloc::format!("> {}", p.text()),
                    ParaKind::Pre => alloc::format!("pre {}", p.text()),
                    _ => alloc::format!("p {}", p.text()),
                },
                Block::Table(t) => alloc::format!("table {}", t.rows.len()),
                Block::Image(_) => String::from("img"),
                Block::Rule => String::from("hr"),
                Block::PageBreak => String::from("page"),
            })
            .collect()
    }

    #[test]
    fn blocks_and_styles() {
        let x = r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>T</title>
<style>p{}</style></head><body><h2>Chapter&#160;3</h2>
<p>A  <b>bold</b>
 and <em>italic</em> word.</p><p style="text-align:center">Centre</p>
<ul><li>one</li><li>two<ol><li>a</li><li>b</li></ol></li></ul>
<blockquote><p>Quote</p></blockquote><pre>line 1
  line 2</pre><hr/><div>loose <span style="font-weight:bold">text</span></div>
<p>before<img src="../img/a.png"/>after</p><p style="display:none">hidden</p>
<table><tr><th>H</th><td>c</td></tr></table></body></html>"#;
        let d = read(x, "OEBPS/text/c3.xhtml").unwrap();
        assert_eq!(
            shape(&d),
            [
                "h2 Chapter\u{a0}3",
                "p A bold and italic word.",
                "p Centre",
                "0• one",
                "0• two",
                "11. a",
                "12. b",
                "> Quote",
                "pre line 1\n  line 2",
                "hr",
                "p loose text",
                "p before",
                "img",
                "p after",
                "table 1",
            ]
        );
        let Block::Para(p) = &d.blocks[1] else {
            panic!()
        };
        assert!(p.runs.iter().any(|r| r.text == "bold" && r.style.bold));
        assert!(p.runs.iter().any(|r| r.text == "italic" && r.style.italic));
        let Block::Para(c) = &d.blocks[2] else {
            panic!()
        };
        assert_eq!(c.align, Align::Centre);
        assert_eq!(
            d.images,
            [ImageSrc::Member(String::from("OEBPS/img/a.png"))]
        );
    }
}
