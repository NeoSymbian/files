// SPDX-License-Identifier: Apache-2.0
//! Rich Text Format (.rtf), our own reader: groups and control words; text in the document's code page (1251 or
//! 1252; a font's `\fcharset204` means 1251) or as `\uN`; bold, italic, underline, strike, size, colour,
//! super/subscript; alignment and indents; tables (`\intbl`, `\cell`, `\row`); page breaks; PNG and JPEG pictures.
//! Headers, footers, footnotes, fields' instructions and every `\*` destination are skipped.

use crate::model::*;
use crate::{Error, Result};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// Group nesting deeper than this is refused.
pub const MAX_DEPTH: usize = 200;

/// Windows-1252's 0x80–0x9F (the rest of the upper half is Latin-1).
const CP1252: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}',
    '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
];

/// Windows-1251's 0x80–0xBF (0xC0–0xFF are А–я).
const CP1251: [char; 64] = [
    'Ђ', 'Ѓ', '‚', 'ѓ', '„', '…', '†', '‡', '€', '‰', 'Љ', '‹', 'Њ', 'Ќ', 'Ћ', 'Џ', 'ђ', '‘', '’',
    '“', '”', '•', '–', '—', '\u{98}', '™', 'љ', '›', 'њ', 'ќ', 'ћ', 'џ', '\u{a0}', 'Ў', 'ў', 'Ј',
    '¤', 'Ґ', '¦', '§', 'Ё', '©', 'Є', '«', '¬', '\u{ad}', '®', 'Ї', '°', '±', 'І', 'і', 'ґ', 'µ',
    '¶', '·', 'ё', '№', 'є', '»', 'ј', 'Ѕ', 'ѕ', 'ї',
];

fn byte(b: u8, cp: u16) -> char {
    match (cp, b) {
        (_, 0..=0x7f) => b as char,
        (1251, 0xc0..=0xff) => char::from_u32(0x410 + (b - 0xc0) as u32).unwrap_or('?'),
        (1251, _) => CP1251[(b - 0x80) as usize],
        (_, 0x80..=0x9f) => CP1252[(b - 0x80) as usize],
        _ => b as char,
    }
}

#[derive(Clone, Copy, Default)]
struct State {
    style: Style,
    colour: usize,
    /// characters to skip after a `\uN`
    uc: u8,
    skip: bool,
    font: i32,
    /// what the group's text is: 0 text, 1 colour table, 2 font table, 3 a picture
    dest: u8,
}

#[derive(Default)]
struct ParaProps {
    align: Align,
    indent: i32,
    first: i32,
    before: Option<i32>,
    after: Option<i32>,
    in_table: bool,
}

/// Read an .rtf.
pub fn read(bytes: &[u8]) -> Result<Doc> {
    if !bytes.starts_with(b"{\\rtf") {
        return Err(Error::NotDocument);
    }
    let mut doc = Doc::default();
    let mut cp: u16 = 1252;
    let mut fonts: BTreeMap<i32, u16> = BTreeMap::new();
    let mut colours: Vec<Option<u32>> = Vec::new();
    let mut rgb = (0u32, 0u32, 0u32, false);
    let mut deff = 0;
    let mut stack: Vec<State> = Vec::new();
    let mut st = State {
        uc: 1,
        ..Default::default()
    };
    let mut pp = ParaProps::default();
    let mut para = Para::default();
    let mut table: Option<Table> = None;
    let mut row: Vec<Cell> = Vec::new();
    let mut cell: Vec<Para> = Vec::new();
    let mut skip_chars = 0u8;
    let mut ignorable = false;
    let mut pict: (Vec<u8>, Option<u8>, i32, i32, Option<u8>) = (Vec::new(), None, 0, 0, None);
    let mut surrogate: Option<u16> = None;
    let mut i = 0;

    let flush = |doc: &mut Doc,
                 para: &mut Para,
                 pp: &ParaProps,
                 table: &mut Option<Table>,
                 cell: &mut Vec<Para>,
                 end_cell: bool| {
        let mut p = core::mem::take(para);
        p.align = pp.align;
        p.indent = pp.indent;
        p.first = pp.first;
        p.before = pp.before;
        p.after = pp.after;
        if pp.in_table {
            cell.push(p);
            let _ = end_cell;
        } else {
            if let Some(t) = table.take() {
                doc.blocks.push(Block::Table(t));
            }
            doc.blocks.push(Block::Para(p));
        }
    };

    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'{' => {
                if stack.len() >= MAX_DEPTH {
                    return Err(Error::TooBig);
                }
                stack.push(st);
                i += 1;
            }
            b'}' => {
                if st.dest == 3 && !st.skip {
                    // a picture group ends: PNG and JPEG become images
                    if let Some(kind) = pict.1.take()
                        && kind > 0
                        && !pict.0.is_empty()
                    {
                        let data = core::mem::take(&mut pict.0);
                        let b = doc.image(ImageSrc::Bytes(data), pict.2, pict.3);
                        if !para.is_empty() {
                            flush(&mut doc, &mut para, &pp, &mut table, &mut cell, false);
                        }
                        doc.blocks.push(b);
                    }
                    pict = (Vec::new(), None, 0, 0, None);
                }
                if st.dest == 1 && rgb.3 {
                    colours.push(Some(rgb.0 << 16 | rgb.1 << 8 | rgb.2));
                }
                st = stack
                    .pop()
                    .ok_or(Error::Bad("The RTF's groups don't close"))?;
                i += 1;
                if stack.is_empty() {
                    break;
                }
            }
            b'\\' => {
                i += 1;
                let Some(&n) = bytes.get(i) else { break };
                if n.is_ascii_alphabetic() {
                    let start = i;
                    while i < bytes.len() && bytes[i].is_ascii_alphabetic() && i - start < 32 {
                        i += 1;
                    }
                    let word = core::str::from_utf8(&bytes[start..i]).unwrap_or("");
                    let ns = i;
                    if i < bytes.len() && bytes[i] == b'-' {
                        i += 1;
                    }
                    while i < bytes.len() && bytes[i].is_ascii_digit() && i - ns < 11 {
                        i += 1;
                    }
                    let arg: Option<i32> = core::str::from_utf8(&bytes[ns..i])
                        .ok()
                        .and_then(|s| s.parse().ok());
                    if i < bytes.len() && bytes[i] == b' ' {
                        i += 1;
                    }
                    if skip_chars > 0 {
                        skip_chars -= 1;
                        continue;
                    }
                    if core::mem::take(&mut ignorable) && !matches!(word, "pict" | "shppict") {
                        st.skip = true;
                        continue;
                    }
                    let on = arg != Some(0);
                    match word {
                        "ansicpg" => cp = if arg == Some(1251) { 1251 } else { 1252 },
                        "deff" => deff = arg.unwrap_or(0),
                        "fonttbl" => st.dest = 2,
                        "f" if st.dest == 2 => st.font = arg.unwrap_or(0),
                        "fcharset" if st.dest == 2 => {
                            if arg == Some(204) {
                                fonts.insert(st.font, 1251);
                            } else if arg == Some(0) {
                                fonts.insert(st.font, 1252);
                            }
                        }
                        "f" => st.font = arg.unwrap_or(deff),
                        "colortbl" => {
                            st.dest = 1;
                            colours.clear();
                        }
                        "red" if st.dest == 1 => {
                            rgb = (arg.unwrap_or(0).clamp(0, 255) as u32, rgb.1, rgb.2, true)
                        }
                        "green" if st.dest == 1 => rgb.1 = arg.unwrap_or(0).clamp(0, 255) as u32,
                        "blue" if st.dest == 1 => rgb.2 = arg.unwrap_or(0).clamp(0, 255) as u32,
                        "stylesheet" | "info" | "header" | "headerl" | "headerr" | "headerf"
                        | "footer" | "footerl" | "footerr" | "footerf" | "footnote" | "fldinst"
                        | "object" | "listtable" | "listoverridetable" | "revtbl" | "rsidtbl"
                        | "filetbl" | "xmlnstbl" | "themedata" | "colorschememapping"
                        | "datastore" | "latentstyles" | "generator" | "nonshppict"
                        | "bkmkstart" | "bkmkend" | "comment" | "pnseclvl" | "atnid"
                        | "annotation" => st.skip = true,
                        "pict" => {
                            st.dest = 3;
                            pict = (Vec::new(), Some(0), 0, 0, None);
                        }
                        "pngblip" if st.dest == 3 => pict.1 = Some(1),
                        "jpegblip" if st.dest == 3 => pict.1 = Some(2),
                        "picwgoal" if st.dest == 3 => pict.2 = arg.unwrap_or(0).clamp(0, 50000),
                        "pichgoal" if st.dest == 3 => pict.3 = arg.unwrap_or(0).clamp(0, 50000),
                        "par" | "sect" if !st.skip => {
                            flush(&mut doc, &mut para, &pp, &mut table, &mut cell, false)
                        }
                        "page" if !st.skip => {
                            if !para.is_empty() {
                                flush(&mut doc, &mut para, &pp, &mut table, &mut cell, false);
                            }
                            doc.blocks.push(Block::PageBreak);
                        }
                        "pard" => pp = ParaProps::default(),
                        "plain" => {
                            st.style = Style::default();
                            st.colour = 0;
                        }
                        "intbl" => pp.in_table = true,
                        "cell" if !st.skip => {
                            if !para.is_empty() || cell.is_empty() {
                                let keep = pp.in_table;
                                pp.in_table = true;
                                flush(&mut doc, &mut para, &pp, &mut table, &mut cell, true);
                                pp.in_table = keep;
                            }
                            row.push(Cell {
                                paras: core::mem::take(&mut cell),
                                span: 1,
                                header: false,
                            });
                        }
                        "row" if !st.skip => {
                            table
                                .get_or_insert_with(Table::default)
                                .rows
                                .push(core::mem::take(&mut row));
                        }
                        "qc" => pp.align = Align::Centre,
                        "qr" => pp.align = Align::End,
                        "ql" | "qj" => pp.align = Align::Start,
                        "li" => pp.indent = arg.unwrap_or(0).clamp(0, 20000),
                        "fi" => pp.first = arg.unwrap_or(0).clamp(-10000, 10000),
                        "sb" => pp.before = Some(arg.unwrap_or(0).clamp(0, 2000)),
                        "sa" => pp.after = Some(arg.unwrap_or(0).clamp(0, 2000)),
                        "paperw" => doc.page.width = arg.unwrap_or(A4.width).clamp(1440, 50000),
                        "paperh" => doc.page.height = arg.unwrap_or(A4.height).clamp(1440, 50000),
                        "margt" => doc.page.margin[0] = arg.unwrap_or(1440).clamp(0, 10000),
                        "margr" => doc.page.margin[1] = arg.unwrap_or(1440).clamp(0, 10000),
                        "margb" => doc.page.margin[2] = arg.unwrap_or(1440).clamp(0, 10000),
                        "margl" => doc.page.margin[3] = arg.unwrap_or(1440).clamp(0, 10000),
                        "b" => st.style.bold = on,
                        "i" => st.style.italic = on,
                        "ul" => st.style.underline = on,
                        "ulnone" => st.style.underline = false,
                        "strike" => st.style.strike = on,
                        "fs" => st.style.size = arg.unwrap_or(24).clamp(2, 400) as u16,
                        "cf" => st.colour = arg.unwrap_or(0).max(0) as usize,
                        "super" => st.style.script = Script::Super,
                        "sub" => st.style.script = Script::Sub,
                        "nosupersub" => st.style.script = Script::Normal,
                        "uc" => st.uc = arg.unwrap_or(1).clamp(0, 8) as u8,
                        "u" => {
                            let v = arg.unwrap_or(0);
                            let u = if v < 0 { (v + 65536) as u16 } else { v as u16 };
                            let ch = match (surrogate.take(), u) {
                                (_, 0xd800..=0xdbff) => {
                                    surrogate = Some(u);
                                    None
                                }
                                (Some(hi), 0xdc00..=0xdfff) => {
                                    char::decode_utf16([hi, u]).next().and_then(|r| r.ok())
                                }
                                _ => char::from_u32(u as u32),
                            };
                            if let Some(ch) = ch
                                && !st.skip
                                && st.dest == 0
                            {
                                let mut s = [0u8; 4];
                                text(&mut para, ch.encode_utf8(&mut s), &st, &colours);
                            }
                            skip_chars = st.uc;
                        }
                        "line" if !st.skip => text(&mut para, "\n", &st, &colours),
                        "tab" if !st.skip => text(&mut para, "\t", &st, &colours),
                        "emdash" => text(&mut para, "—", &st, &colours),
                        "endash" => text(&mut para, "–", &st, &colours),
                        "bullet" => text(&mut para, "•", &st, &colours),
                        "lquote" => text(&mut para, "‘", &st, &colours),
                        "rquote" => text(&mut para, "’", &st, &colours),
                        "ldblquote" => text(&mut para, "“", &st, &colours),
                        "rdblquote" => text(&mut para, "”", &st, &colours),
                        _ => {}
                    }
                } else {
                    i += 1;
                    match n {
                        b'\'' => {
                            let h = bytes
                                .get(i..i + 2)
                                .and_then(|h| core::str::from_utf8(h).ok())
                                .and_then(|h| u8::from_str_radix(h, 16).ok());
                            i += 2;
                            if skip_chars > 0 {
                                skip_chars -= 1;
                                continue;
                            }
                            if let Some(b) = h
                                && !st.skip
                                && st.dest == 0
                            {
                                let page = fonts.get(&st.font).copied().unwrap_or(cp);
                                let mut s = [0u8; 4];
                                text(&mut para, byte(b, page).encode_utf8(&mut s), &st, &colours);
                            }
                        }
                        b'*' => ignorable = true,
                        b'~' => text(&mut para, "\u{a0}", &st, &colours),
                        b'-' => text(&mut para, "\u{ad}", &st, &colours),
                        b'_' => text(&mut para, "‑", &st, &colours),
                        b'\r' | b'\n' if !st.skip => {
                            flush(&mut doc, &mut para, &pp, &mut table, &mut cell, false)
                        }
                        b'\\' | b'{' | b'}' if !st.skip && st.dest == 0 => {
                            if skip_chars > 0 {
                                skip_chars -= 1;
                            } else {
                                text(
                                    &mut para,
                                    if n == b'\\' {
                                        "\\"
                                    } else if n == b'{' {
                                        "{"
                                    } else {
                                        "}"
                                    },
                                    &st,
                                    &colours,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            b'\r' | b'\n' => i += 1,
            _ => {
                // a run of text
                let start = i;
                while i < bytes.len() && !matches!(bytes[i], b'{' | b'}' | b'\\' | b'\r' | b'\n') {
                    i += 1;
                }
                let mut run = &bytes[start..i];
                while skip_chars > 0 && !run.is_empty() {
                    run = &run[1..];
                    skip_chars -= 1;
                }
                if st.skip {
                    continue;
                }
                match st.dest {
                    0 => {
                        let page = fonts.get(&st.font).copied().unwrap_or(cp);
                        let s: String = run.iter().map(|&b| byte(b, page)).collect();
                        text(&mut para, &s, &st, &colours);
                    }
                    1 => {
                        for &b in run {
                            if b == b';' {
                                colours.push(if rgb.3 {
                                    Some(rgb.0 << 16 | rgb.1 << 8 | rgb.2)
                                } else {
                                    None
                                });
                                rgb = (0, 0, 0, false);
                            }
                        }
                    }
                    3 => {
                        for &b in run {
                            if let Some(d) = (b as char).to_digit(16) {
                                match pict.4.take() {
                                    Some(h) => pict.0.push(h << 4 | d as u8),
                                    None => pict.4 = Some(d as u8),
                                }
                            }
                        }
                        if pict.0.len() > crate::MAX_PART as usize {
                            return Err(Error::TooBig);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    if !para.is_empty() {
        flush(&mut doc, &mut para, &pp, &mut table, &mut cell, false);
    }
    if let Some(t) = table.take() {
        doc.blocks.push(Block::Table(t));
    }
    doc.trim();
    Ok(doc)
}

fn text(p: &mut Para, s: &str, st: &State, colours: &[Option<u32>]) {
    if st.skip || st.dest != 0 {
        return;
    }
    let mut style = st.style;
    style.colour = colours.get(st.colour).copied().flatten();
    if style.size == 24 {
        style.size = 0;
    }
    p.push(s, style);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paras(d: &Doc) -> Vec<String> {
        d.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Para(p) => Some(p.text()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn text_code_pages_and_unicode() {
        let r = br"{\rtf1\ansi\ansicpg1252{\fonttbl{\f0 Arial;}{\f1\fcharset204 Times;}}{\colortbl;\red31\green78\blue140;}
\pard\qc{\b Bold}\par
\f1 \'cf\'f0\'e8\f0  caf\'e9 \u1329?\u1330? \{x\}\par
\cf1 blue\cf0  \ldblquote q\rdblquote\par}";
        let d = read(r).unwrap();
        assert_eq!(paras(&d), ["Bold", "При café ԱԲ {x}", "blue “q”"]);
        let Block::Para(p) = &d.blocks[0] else {
            panic!()
        };
        assert_eq!(p.align, Align::Centre);
        assert!(p.runs[0].style.bold);
        let Block::Para(p) = &d.blocks[2] else {
            panic!()
        };
        assert_eq!(p.runs[0].style.colour, Some(0x1f4e8c));
    }

    #[test]
    fn tables_pages_and_skips() {
        let r = br"{\rtf1{\*\generator X;}{\info{\title T}}{\header H}A\par\page
\trowd\cellx1000\cellx2000\intbl a\cell b\cell\row
\trowd\cellx2000\intbl c\cell\row
\pard after\par}";
        let d = read(r).unwrap();
        assert!(matches!(d.blocks[0], Block::Para(ref p) if p.text() == "A"));
        assert!(matches!(d.blocks[1], Block::PageBreak));
        let Block::Table(t) = &d.blocks[2] else {
            panic!("{:?}", d.blocks)
        };
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0][1].paras[0].text(), "b");
        assert!(matches!(d.blocks[3], Block::Para(ref p) if p.text() == "after"));
    }

    #[test]
    fn refuses_garbage() {
        assert_eq!(read(b"hello").unwrap_err(), Error::NotDocument);
        let deep: Vec<u8> = b"{\\rtf1"
            .iter()
            .copied()
            .chain(core::iter::repeat_n(b'{', 300))
            .collect();
        assert_eq!(read(&deep).unwrap_err(), Error::TooBig);
        assert!(read(b"{\\rtf1 x}}}").is_ok());
    }
}
