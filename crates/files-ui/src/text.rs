// SPDX-License-Identifier: GPL-3.0-only
//! The rows the text surface draws (FA4), made from neo-format-text's model: a window of visual rows with coloured
//! spans, the selection and find marks, Markdown Preview lines and the table's cells. NEOFILES.EXE and the snapshot
//! tests use the same functions.

use crate::{CellRow, Mark, MdRow, MdSpan, TRow, TSpan, colour};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use neo_format_text::layout::Layout;
use neo_format_text::lex::{Highlighter, Kind};
use neo_format_text::md::{Block, Span, Style};
use neo_format_text::table::{Table, col_name};
use neo_format_text::{Doc, Pos};
use slint::{ModelRc, SharedString, VecModel};

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(alloc::rc::Rc::new(VecModel::from(v)))
}

/// files2-edit-code's palette.
pub fn kind_colour(k: Kind) -> slint::Color {
    colour(match k {
        Kind::Plain => 0xe6edf3,
        Kind::Keyword => 0xff7b72,
        Kind::Type => 0xffa657,
        Kind::Func => 0xd2a8ff,
        Kind::Str => 0xa5d6ff,
        Kind::Attr => 0x79c0ff,
        Kind::Comment => 0x8b949e,
        Kind::Punct => 0xb9bec7,
        Kind::Tag => 0x7ee787,
        Kind::Time => 0x7d838c,
        Kind::Source => 0x8cb8ff,
        Kind::Heading => 0x79c0ff,
    })
}

/// Digits of the last line number (the gutter's width).
pub fn gutter(doc: &Doc) -> i32 {
    let mut n = doc.lines.len();
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

/// The rows `first..first + n`, coloured; tabs show as four spaces (the layout counts them so).
pub fn text_rows(
    doc: &Doc,
    hl: &mut Highlighter,
    layout: &Layout,
    first: usize,
    n: usize,
) -> Vec<TRow> {
    let mut out = Vec::with_capacity(n);
    let mut cur: (usize, neo_format_text::lex::Spans) = (usize::MAX, Vec::new());
    for (k, row) in layout.window(doc, first, n).into_iter().enumerate() {
        let text = &doc.lines[row.line].text;
        if cur.0 != row.line {
            cur = (row.line, hl.spans(doc, row.line));
        }
        let spans = &cur.1;
        // the row's byte range
        let byte = |c: usize| text.char_indices().nth(c).map_or(text.len(), |(b, _)| b);
        let (rb, re) = (byte(row.from), byte(row.to));
        let mut ts = Vec::new();
        for (r, kind) in spans {
            let (a, b) = (r.start.max(rb), r.end.min(re));
            if a >= b {
                continue;
            }
            let piece = &text[a..b];
            // spaces draw nothing; the spans after them are placed by column
            if piece.chars().all(|c| c == ' ' || c == '\t') {
                continue;
            }
            ts.push(TSpan {
                text: piece.replace('\t', "    ").as_str().into(),
                colour: kind_colour(*kind),
                x: neo_format_text::layout::x_of(&text[rb..a], usize::MAX) as i32,
            });
        }
        out.push(TRow {
            row: (first + k) as i32,
            no: if row.part == 0 {
                format!("{}", row.line + 1).as_str().into()
            } else {
                SharedString::new()
            },
            spans: model(ts),
        });
    }
    out
}

/// Marks for `ranges` (each `(from, to, kind)`) on the rows `first..first + n`.
pub fn marks(
    doc: &Doc,
    layout: &Layout,
    ranges: &[(Pos, Pos, i32)],
    first: usize,
    n: usize,
) -> Vec<Mark> {
    let mut out = Vec::new();
    let last = first + n;
    for &(a, b, kind) in ranges {
        let (ra, xa) = layout.place(doc, a);
        let (rb, xb) = layout.place(doc, b);
        if rb < first || ra >= last {
            continue;
        }
        let cols = if layout.cols == 0 { 4096 } else { layout.cols };
        for r in ra.max(first)..=rb.min(last - 1) {
            let x0 = if r == ra { xa } else { 0 };
            let x1 = if r == rb { xb } else { cols.max(x0 + 1) };
            out.push(Mark {
                row: r as i32,
                x: x0 as i32,
                w: (x1.saturating_sub(x0)).max(if a == b { 0 } else { 1 }) as i32,
                kind,
            });
        }
    }
    out
}

// ── Markdown Preview ─────────────────────────────────────────────────────────────────────────────────────────────

/// Rough advance of a char in em (DejaVu Sans; mono 0.6). The Rust side can't measure Slint text, so Preview wraps
/// by this estimate, on the narrow side so lines rarely overrun.
// shortcut: estimated widths; measure with the renderer's font metrics when Slint exposes them without std
fn advance(c: char, mono: bool, bold: bool) -> f32 {
    if mono {
        return 0.61;
    }
    let base = if c == ' ' {
        0.32
    } else if c.is_ascii_uppercase() || c.is_ascii_digit() {
        0.68
    } else if "iljt.,:;'|!".contains(c) {
        0.3
    } else if "mwMW".contains(c) {
        0.9
    } else {
        0.58
    };
    if bold { base * 1.08 } else { base }
}

fn span_style(s: &Span, kind: i32) -> MdSpan {
    let link = s.style == Style::Link;
    MdSpan {
        text: SharedString::new(),
        colour: colour(if link { 0x58a6ff } else { 0xe6edf3 }),
        bold: s.style == Style::Bold,
        italic: s.style == Style::Italic,
        mono: s.style == Style::Code || kind == 4,
        gap: false,
    }
}

/// Wraps `spans` into lines at most `width_em` wide (at spaces; a word longer than a line is cut).
fn wrap(spans: &[Span], kind: i32, width_em: f32) -> Vec<Vec<MdSpan>> {
    let mut lines: Vec<Vec<MdSpan>> = alloc::vec![Vec::new()];
    let mut used = 0.0f32;
    let mut pending = String::new();
    for s in spans {
        let st = span_style(s, kind);
        let mut word = String::new();
        let mut word_w = 0.0f32;
        let flush =
            |word: &mut String, word_w: &mut f32, lines: &mut Vec<Vec<MdSpan>>, used: &mut f32| {
                if word.is_empty() {
                    return;
                }
                if *used + *word_w > width_em && *used > 0.0 {
                    lines.push(Vec::new());
                    *used = 0.0;
                    let t = word.trim_start();
                    *word_w -= (word.len() - t.len()) as f32 * 0.32;
                    *word = String::from(t);
                }
                let line = lines.last_mut().expect("one line");
                match line.last_mut() {
                    Some(last)
                        if last.bold == st.bold
                            && last.italic == st.italic
                            && last.mono == st.mono
                            && last.colour == st.colour =>
                    {
                        let mut t = String::from(last.text.as_str());
                        t.push_str(word);
                        last.text = t.as_str().into();
                    }
                    _ => {
                        // a leading space becomes the span's gap (Slint drops it from the text)
                        let t = word.trim_start_matches(' ');
                        let mut m = st.clone();
                        m.gap = t.len() != word.len() && !line.is_empty();
                        m.text = t.into();
                        line.push(m);
                    }
                }
                *used += *word_w;
                word.clear();
                *word_w = 0.0;
            };
        // spaces left at the end of the last span start this one (they become its gap)
        word.push_str(&pending);
        word_w += pending.len() as f32 * 0.32;
        pending.clear();
        for c in s.text.chars() {
            if c == ' ' && !word.is_empty() && !word.ends_with(' ') {
                flush(&mut word, &mut word_w, &mut lines, &mut used);
            }
            word.push(c);
            word_w += advance(c, st.mono, st.bold);
        }
        let body = word.trim_end_matches(' ').len();
        pending = String::from(&word[body..]);
        word.truncate(body);
        word_w -= pending.len() as f32 * 0.32;
        flush(&mut word, &mut word_w, &mut lines, &mut used);
    }
    lines
}

/// Preview lines for a page `width_dp` wide (16 dp margins), from neo-format-text's blocks.
pub fn md_rows(blocks: &[Block], width_dp: f32) -> Vec<MdRow> {
    let mut out = Vec::new();
    let text_w = width_dp - 32.0;
    let row =
        |kind: i32, indent: i32, task: i32, bullet: bool, first: bool, spans: Vec<MdSpan>| MdRow {
            kind,
            indent,
            task,
            bullet,
            first,
            src: -1,
            spans: model(spans),
        };
    for (k, b) in blocks.iter().enumerate() {
        // a gap between blocks (none between list items or code lines that follow each other)
        let same = k > 0
            && matches!(
                (&blocks[k - 1], b),
                (Block::Item { .. }, Block::Item { .. })
            );
        if k > 0 && !same {
            out.push(row(7, 0, -1, false, true, Vec::new()));
        }
        match b {
            Block::Heading(level, spans) => {
                let kind = (*level).min(3) as i32;
                let size = [24.0, 19.0, 16.5][kind as usize - 1];
                for (i, l) in wrap(spans, kind, text_w / size).into_iter().enumerate() {
                    out.push(row(kind, 0, -1, false, i == 0, l));
                }
            }
            Block::Para(spans) | Block::Quote(spans) => {
                let kind = if matches!(b, Block::Quote(_)) { 6 } else { 0 };
                for (i, l) in wrap(
                    spans,
                    kind,
                    (text_w - if kind == 6 { 12.0 } else { 0.0 }) / 15.0,
                )
                .into_iter()
                .enumerate()
                {
                    out.push(row(kind, 0, -1, false, i == 0, l));
                }
            }
            Block::Item {
                depth,
                task,
                spans,
                line,
            } => {
                let indent = *depth as i32;
                let w = (text_w - (indent as f32 * 18.0 + 26.0)) / 15.0;
                let t = task.map_or(-1, |d| d as i32);
                for (i, l) in wrap(spans, 0, w).into_iter().enumerate() {
                    let mut r = row(0, indent, t, true, i == 0, l);
                    r.src = *line as i32;
                    out.push(r);
                }
            }
            Block::Code(lines) => {
                for l in lines {
                    let span = Span {
                        text: l.clone(),
                        style: Style::Code,
                    };
                    let mut s = span_style(&span, 4);
                    s.text = l.replace('\t', "    ").as_str().into();
                    s.colour = colour(0xd2a8ff);
                    out.push(row(4, 0, -1, false, true, alloc::vec![s]));
                }
            }
            Block::Rule => out.push(row(5, 0, -1, false, true, Vec::new())),
        }
    }
    out
}

// ── the table ────────────────────────────────────────────────────────────────────────────────────────────────────

/// Column places and widths from their longest cell (about 8.6 dp a char, 56–180 dp), on a page at `dp`.
pub fn table_cols(t: &Table, dp: f32) -> Vec<crate::Col> {
    let mut x = 0.0;
    (0..t.cols().max(1))
        .map(|c| {
            let chars = (0..t.rows())
                .map(|r| t.cell(r, c).chars().count())
                .max()
                .unwrap_or(0);
            let w = (chars as f32 * 8.6 + 20.0).clamp(56.0, 180.0) * dp;
            let col = crate::Col { x, w };
            x += w;
            col
        })
        .collect()
}

pub fn table_rows(t: &Table) -> (Vec<SharedString>, Vec<CellRow>) {
    let cols = t.cols().max(1);
    let heads = (0..cols).map(|c| col_name(c).as_str().into()).collect();
    let rows = (0..t.rows())
        .map(|r| CellRow {
            no: format!("{}", r + 1).as_str().into(),
            cells: model((0..cols).map(|c| t.cell(r, c).into()).collect()),
        })
        .collect();
    (heads, rows)
}
