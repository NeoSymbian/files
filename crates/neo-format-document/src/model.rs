// SPDX-License-Identifier: Apache-2.0
//! The block model every document reader fills: paragraphs of styled runs, list items, tables, images, rules and
//! page breaks, with the page setup (docx sectPr, odt page layout, rtf \paperw; A4 when the file has none).
//! Lengths are twips (1/20 pt), as docx and rtf store them.

use alloc::string::String;
use alloc::vec::Vec;

/// A4 with 2.5 cm margins, in twips.
pub const A4: PageSetup = PageSetup {
    width: 11906,
    height: 16838,
    margin: [1417, 1417, 1417, 1417],
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageSetup {
    pub width: i32,
    pub height: i32,
    /// top, right, bottom, left
    pub margin: [i32; 4],
}

impl PageSetup {
    /// The width text runs in.
    pub fn text_width(&self) -> i32 {
        (self.width - self.margin[1] - self.margin[3]).max(1440)
    }
    pub fn text_height(&self) -> i32 {
        (self.height - self.margin[0] - self.margin[2]).max(1440)
    }
}

impl Default for PageSetup {
    fn default() -> Self {
        A4
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Doc {
    pub title: String,
    pub page: PageSetup,
    /// The body text size, half-points (docDefaults; 0 = 24, 12 pt): reflow draws other sizes relative to it.
    pub body: u16,
    pub blocks: Vec<Block>,
    /// What [`Block::Image`] points at.
    pub images: Vec<ImageSrc>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ImageSrc {
    /// A zip member (EPUB, docx, odt).
    Member(String),
    /// The bytes themselves (an rtf `\pict`).
    Bytes(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Para(Para),
    Table(Table),
    Image(Image),
    Rule,
    PageBreak,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Start,
    Centre,
    End,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ParaKind {
    #[default]
    Body,
    /// 1–6
    Heading(u8),
    /// A list item: depth from 0 and its marker ("•", "3.", "b)").
    Item {
        depth: u8,
        marker: String,
    },
    /// Preformatted: monospace, lines kept.
    Pre,
    Quote,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Para {
    pub kind: ParaKind,
    pub runs: Vec<Run>,
    pub align: Align,
    /// From the start edge, twips.
    pub indent: i32,
    /// The first line's extra indent, twips (negative: hanging).
    pub first: i32,
    /// Space before and after, twips; `None` = the viewer's paragraph gap.
    pub before: Option<i32>,
    pub after: Option<i32>,
}

impl Para {
    pub fn new(kind: ParaKind) -> Self {
        Para {
            kind,
            ..Default::default()
        }
    }
    /// The text of every run.
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
    pub fn is_empty(&self) -> bool {
        self.runs.iter().all(|r| r.text.is_empty())
    }
    /// Add text in `style`, joined to the last run when the style is the same.
    pub fn push(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        match self.runs.last_mut() {
            Some(r) if r.style == style => r.text.push_str(text),
            _ => self.runs.push(Run {
                text: String::from(text),
                style,
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub text: String,
    pub style: Style,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Script {
    #[default]
    Normal,
    Super,
    Sub,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub mono: bool,
    pub script: Script,
    /// Half-points (22 = 11 pt); 0 = the body size.
    pub size: u16,
    /// 0xRRGGBB; `None` = the text colour.
    pub colour: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    pub rows: Vec<Vec<Cell>>,
    /// Column widths, twips, when the file gives them (docx tblGrid).
    pub widths: Vec<i32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cell {
    pub paras: Vec<Para>,
    /// Columns it spans (1 or more).
    pub span: u16,
    pub header: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    /// Into [`Doc::images`].
    pub id: usize,
    /// The size the file asks for, twips; 0 = the picture's own.
    pub width: i32,
    pub height: i32,
}

impl Doc {
    /// An image block for `src`.
    pub fn image(&mut self, src: ImageSrc, width: i32, height: i32) -> Block {
        let id = self
            .images
            .iter()
            .position(|s| *s == src)
            .unwrap_or_else(|| {
                self.images.push(src);
                self.images.len() - 1
            });
        Block::Image(Image { id, width, height })
    }

    /// Drop empty paragraphs at the end (a docx's last `w:p`).
    pub fn trim(&mut self) {
        while matches!(self.blocks.last(), Some(Block::Para(p)) if p.is_empty()) {
            self.blocks.pop();
        }
    }
}
