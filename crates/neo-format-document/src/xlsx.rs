// SPDX-License-Identifier: Apache-2.0
//! Excel (.xlsx) read-only: the workbook's sheets (chartsheets named, not drawn), shared strings, number formats
//! (General, decimals, grouping, percent, dates and times; [Red] and the like ignored), and a sheet's cells with
//! their cached values and formulas (a shared formula shifted from its master; nothing is calculated), column
//! widths and merged cells. Cells are kept compactly and sorted, at most [`MAX_CELLS`] a sheet.

use crate::xml::{Ev, Reader};
use crate::{Error, Package, Result, join};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use neo_format_archive::ReadAt;

/// At most this many cells of a sheet are read (the rest: [`Sheet::truncated`]).
pub const MAX_CELLS: usize = 200_000;
pub const MAX_STRINGS: usize = 1_000_000;
/// At most this many merged areas are kept (each cell drawn looks them up).
pub const MAX_MERGES: usize = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub struct SheetRef {
    pub name: String,
    /// The worksheet's part; `None` for a chartsheet.
    pub part: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Workbook {
    pub sheets: Vec<SheetRef>,
    pub strings: Vec<Box<str>>,
    /// Each cell style's number format code.
    pub formats: Vec<Box<str>>,
    /// Each cell style's font is bold.
    pub bold: Vec<bool>,
    pub date1904: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Empty,
    Num(f64),
    /// Into [`Workbook::strings`].
    Shared(u32),
    Text(Box<str>),
    Bool(bool),
    Error(Box<str>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub row: u32,
    pub col: u32,
    pub value: Value,
    pub formula: Option<Box<str>>,
    pub style: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sheet {
    /// Sorted by (row, col), from 0.
    pub cells: Vec<Cell>,
    /// (first col, last col, width in characters)
    pub widths: Vec<(u32, u32, f32)>,
    /// (row0, col0, row1, col1), inclusive
    pub merges: Vec<(u32, u32, u32, u32)>,
    pub rows: u32,
    pub cols: u32,
    pub truncated: bool,
}

impl Sheet {
    pub fn get(&self, row: u32, col: u32) -> Option<&Cell> {
        self.cells
            .binary_search_by(|c| (c.row, c.col).cmp(&(row, col)))
            .ok()
            .map(|i| &self.cells[i])
    }

    /// The cells of rows `r0..r1` (sorted).
    pub fn rows_in(&self, r0: u32, r1: u32) -> &[Cell] {
        let a = self.cells.partition_point(|c| c.row < r0);
        let b = self.cells.partition_point(|c| c.row < r1);
        &self.cells[a..b]
    }

    /// A column's width in characters (8.43, Excel's default, when the file says nothing).
    pub fn width(&self, col: u32) -> f32 {
        self.widths
            .iter()
            .find(|w| w.0 <= col && col <= w.1)
            .map_or(8.43, |w| w.2)
    }

    /// The merge a cell starts, or `None`; `Some(None)` for a cell hidden under another's merge.
    pub fn merge_at(&self, row: u32, col: u32) -> Option<Option<(u32, u32)>> {
        let m = self
            .merges
            .iter()
            .find(|m| m.0 <= row && row <= m.2 && m.1 <= col && col <= m.3)?;
        Some((m.0 == row && m.1 == col).then_some((m.2, m.3)))
    }
}

/// `0` → "A", `27` → "AB".
pub fn col_name(mut c: u32) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (c % 26) as u8) as char);
        if c < 26 {
            return s;
        }
        c = c / 26 - 1;
    }
}

/// "E7" → (row 6, col 4).
pub fn parse_ref(r: &str) -> Option<(u32, u32)> {
    let r = r.trim_start_matches('$');
    let letters = r.bytes().take_while(|b| b.is_ascii_alphabetic()).count();
    if letters == 0 || letters > 3 {
        return None;
    }
    let mut col = 0u32;
    for b in r[..letters].bytes() {
        col = col * 26 + (b.to_ascii_uppercase() - b'A' + 1) as u32;
    }
    let row: u32 = r[letters..].trim_start_matches('$').parse().ok()?;
    (row > 0 && row <= 1_048_576 && col <= 16_384).then(|| (row - 1, col - 1))
}

fn xml<R: ReadAt>(p: &Package<R>, name: &str) -> Result<String> {
    p.text(name)
}

const BUILTIN: [(u32, &str); 21] = [
    (0, "General"),
    (1, "0"),
    (2, "0.00"),
    (3, "#,##0"),
    (4, "#,##0.00"),
    (9, "0%"),
    (10, "0.00%"),
    (11, "0.00E+00"),
    (12, "# ?/?"),
    (14, "dd.mm.yyyy"),
    (15, "d-mmm-yy"),
    (16, "d-mmm"),
    (17, "mmm-yy"),
    (18, "h:mm AM/PM"),
    (19, "h:mm:ss AM/PM"),
    (20, "h:mm"),
    (21, "h:mm:ss"),
    (22, "dd.mm.yyyy h:mm"),
    (45, "mm:ss"),
    (46, "[h]:mm:ss"),
    (49, "@"),
];

/// Open a workbook: its sheets, strings and number formats.
pub fn open<R: ReadAt>(p: &Package<R>) -> Result<Workbook> {
    let mut wb = Workbook::default();
    let book = xml(p, "xl/workbook.xml")?;
    let rels = if p.has("xl/_rels/workbook.xml.rels") {
        xml(p, "xl/_rels/workbook.xml.rels")?
    } else {
        String::new()
    };
    // rId → (target, is a chartsheet)
    let mut targets: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for ev in Reader::new(&rels) {
        let ev = ev?;
        if let Ev::Start { name, .. } = &ev
            && name.local == "Relationship"
            && let (Some(id), Some(t)) = (ev.attr("", "Id"), ev.attr("", "Target"))
        {
            let chart = ev
                .attr("", "Type")
                .is_some_and(|t| t.ends_with("/chartsheet"));
            targets.insert(String::from(id), (join("xl/workbook.xml", t), chart));
        }
    }
    for ev in Reader::new(&book) {
        let ev = ev?;
        let Ev::Start { name, .. } = &ev else {
            continue;
        };
        match name.local {
            "workbookPr" => wb.date1904 = matches!(ev.attr("", "date1904"), Some("1" | "true")),
            "sheet" => {
                let n = String::from(ev.attr("", "name").unwrap_or("Sheet"));
                let t = ev.attr("r", "id").and_then(|id| targets.get(id));
                let part = match t {
                    Some((t, false)) => Some(t.clone()),
                    _ => None,
                };
                wb.sheets.push(SheetRef { name: n, part });
            }
            _ => {}
        }
    }
    if wb.sheets.is_empty() {
        return Err(Error::Bad("The workbook has no sheets"));
    }
    if p.has("xl/sharedStrings.xml") {
        let s = xml(p, "xl/sharedStrings.xml")?;
        let mut r = Reader::new(&s);
        let (mut cur, mut in_si, mut in_t, mut skip) = (String::new(), false, false, 0);
        for ev in r.by_ref() {
            match ev? {
                Ev::Start { name, .. } => match name.local {
                    "si" => {
                        in_si = true;
                        cur.clear();
                    }
                    "t" if in_si && skip == 0 => in_t = true,
                    // phonetic runs aren't the text
                    "rPh" | "phoneticPr" => skip += 1,
                    _ => {}
                },
                Ev::End(name) => match name.local {
                    "si" => {
                        if wb.strings.len() >= MAX_STRINGS {
                            return Err(Error::TooBig);
                        }
                        wb.strings.push(cur.as_str().into());
                        in_si = false;
                    }
                    "t" => in_t = false,
                    "rPh" | "phoneticPr" => skip -= 1,
                    _ => {}
                },
                Ev::Text(t) if in_t => cur.push_str(&t),
                _ => {}
            }
        }
    }
    if p.has("xl/styles.xml") {
        let s = xml(p, "xl/styles.xml")?;
        let mut custom: BTreeMap<u32, String> = BTreeMap::new();
        let mut in_xfs = false;
        let (mut in_fonts, mut fonts): (bool, Vec<bool>) = (false, Vec::new());
        for ev in Reader::new(&s) {
            let ev = ev?;
            match &ev {
                Ev::Start { name, .. } => match name.local {
                    "numFmt" => {
                        if let (Some(id), Some(code)) = (
                            ev.attr("", "numFmtId").and_then(|v| v.parse().ok()),
                            ev.attr("", "formatCode"),
                        ) {
                            custom.insert(id, String::from(code));
                        }
                    }
                    "cellXfs" => in_xfs = true,
                    "fonts" => in_fonts = true,
                    "font" if in_fonts => fonts.push(false),
                    "b" if in_fonts => {
                        if let Some(f) = fonts.last_mut() {
                            *f = !matches!(ev.attr("", "val"), Some("0" | "false"));
                        }
                    }
                    "xf" if in_xfs => {
                        let id: u32 = ev
                            .attr("", "numFmtId")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        let code = custom
                            .get(&id)
                            .map(String::as_str)
                            .or_else(|| BUILTIN.iter().find(|b| b.0 == id).map(|b| b.1))
                            .unwrap_or("General");
                        wb.formats.push(code.into());
                        let font: usize = ev
                            .attr("", "fontId")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        wb.bold.push(fonts.get(font).copied().unwrap_or(false));
                    }
                    _ => {}
                },
                Ev::End(name) if name.local == "cellXfs" => in_xfs = false,
                Ev::End(name) if name.local == "fonts" => in_fonts = false,
                _ => {}
            }
        }
    }
    Ok(wb)
}

/// Read sheet `i` (a chartsheet: [`Error::NotDocument`]).
pub fn sheet<R: ReadAt>(p: &Package<R>, wb: &Workbook, i: usize) -> Result<Sheet> {
    let part = wb
        .sheets
        .get(i)
        .and_then(|s| s.part.clone())
        .ok_or(Error::NotDocument)?;
    let s = xml(p, &part)?;
    let mut sh = Sheet::default();
    let mut r = Reader::new(&s);
    // si → (master's row, col, formula)
    let mut shared: BTreeMap<u32, (u32, u32, String)> = BTreeMap::new();
    let mut cell: Option<Cell> = None;
    let mut kind = String::new();
    let (mut in_v, mut in_f, mut in_is_t) = (false, false, false);
    let (mut v, mut f) = (String::new(), String::new());
    let mut f_si: Option<(u32, bool)> = None;
    let mut next_row = 0u32;
    let mut next_col = 0u32;
    for ev in r.by_ref() {
        let ev = ev?;
        match &ev {
            Ev::Start { name, .. } => match name.local {
                "row" => {
                    next_row = ev
                        .attr("", "r")
                        .and_then(|v| v.parse::<u32>().ok())
                        .map_or(next_row, |r| r.saturating_sub(1))
                        .min(1_048_575);
                    next_col = 0;
                }
                "c" => {
                    let (row, col) = ev
                        .attr("", "r")
                        .and_then(parse_ref)
                        .unwrap_or((next_row, next_col));
                    // Excel's limits: a hostile file can't push the grid past them
                    let (row, col) = (row.min(1_048_575), col.min(16_383));
                    next_col = (col + 1).min(16_383);
                    kind = String::from(ev.attr("", "t").unwrap_or("n"));
                    let style = ev.attr("", "s").and_then(|v| v.parse().ok()).unwrap_or(0);
                    cell = Some(Cell {
                        row,
                        col,
                        value: Value::Empty,
                        formula: None,
                        style,
                    });
                    v.clear();
                    f.clear();
                    f_si = None;
                }
                "v" => in_v = true,
                "f" => {
                    in_f = true;
                    if ev.attr("", "t") == Some("shared") {
                        f_si = ev
                            .attr("", "si")
                            .and_then(|s| s.parse().ok())
                            .map(|si| (si, ev.attr("", "ref").is_some()));
                    }
                }
                "t" if kind == "inlineStr" => in_is_t = true,
                "col" => {
                    let a = |k| ev.attr("", k).and_then(|v| v.parse::<u32>().ok());
                    if let (Some(min), Some(max), Some(w)) = (
                        a("min"),
                        a("max"),
                        ev.attr("", "width").and_then(|v| v.parse::<f32>().ok()),
                    ) {
                        sh.widths.push((
                            min.saturating_sub(1),
                            max.saturating_sub(1).min(16_383),
                            w.clamp(0.5, 255.0),
                        ));
                    }
                }
                "mergeCell" => {
                    if sh.merges.len() < MAX_MERGES
                        && let Some((a, b)) = ev.attr("", "ref").and_then(|r| r.split_once(':'))
                        && let (Some(a), Some(b)) = (parse_ref(a), parse_ref(b))
                    {
                        sh.merges.push((a.0, a.1, b.0.max(a.0), b.1.max(a.1)));
                    }
                }
                _ => {}
            },
            Ev::Text(t) => {
                if in_v {
                    v.push_str(t);
                } else if in_f {
                    f.push_str(t);
                } else if in_is_t {
                    v.push_str(t);
                }
            }
            Ev::End(name) => match name.local {
                "v" => in_v = false,
                "f" => in_f = false,
                "t" => in_is_t = false,
                "c" => {
                    let Some(mut c) = cell.take() else { continue };
                    c.value = match kind.as_str() {
                        "s" => v
                            .trim()
                            .parse::<u32>()
                            .ok()
                            .filter(|&i| (i as usize) < wb.strings.len())
                            .map_or(Value::Empty, Value::Shared),
                        "str" | "inlineStr" => Value::Text(v.as_str().into()),
                        "b" => Value::Bool(v.trim() == "1"),
                        "e" => Value::Error(v.as_str().into()),
                        _ if v.is_empty() => Value::Empty,
                        _ => v.trim().parse::<f64>().map_or(Value::Empty, Value::Num),
                    };
                    let formula = match f_si {
                        Some((si, true)) => {
                            shared.insert(si, (c.row, c.col, f.clone()));
                            Some(f.clone())
                        }
                        Some((si, false)) => shared.get(&si).map(|(r0, c0, mf)| {
                            shift(mf, c.row as i64 - *r0 as i64, c.col as i64 - *c0 as i64)
                        }),
                        None => (!f.is_empty()).then(|| f.clone()),
                    };
                    c.formula = formula.map(|f| f.into_boxed_str());
                    if c.value != Value::Empty || c.formula.is_some() || c.style != 0 {
                        sh.rows = sh.rows.max(c.row + 1);
                        sh.cols = sh.cols.max(c.col + 1);
                        if sh.cells.len() >= MAX_CELLS {
                            sh.truncated = true;
                            break;
                        }
                        sh.cells.push(c);
                    }
                }
                _ => {}
            },
        }
    }
    sh.cells.sort_by_key(|c| (c.row, c.col));
    sh.cells.dedup_by_key(|c| (c.row, c.col));
    for m in &sh.merges {
        sh.rows = sh.rows.max(m.2 + 1);
        sh.cols = sh.cols.max(m.3 + 1);
    }
    Ok(sh)
}

/// A shared formula moved by (dr, dc): its relative references shift, `$` ones stay; strings and names don't change.
pub fn shift(f: &str, dr: i64, dc: i64) -> String {
    let b = f.as_bytes();
    let mut out = String::with_capacity(f.len() + 4);
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'"' {
            // a string literal, "" inside it
            let start = i;
            i += 1;
            while i < b.len() {
                if b[i] == b'"' {
                    if b.get(i + 1) == Some(&b'"') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            out.push_str(&f[start..i]);
            continue;
        }
        let prev_word =
            i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'.');
        if !prev_word && (c == b'$' || c.is_ascii_alphabetic()) {
            // [$]LETTERS[$]DIGITS, not followed by a letter, digit or "(" (a function, a name)
            let mut j = i;
            let col_abs = b[j] == b'$';
            if col_abs {
                j += 1;
            }
            let l0 = j;
            while j < b.len() && b[j].is_ascii_alphabetic() && j - l0 < 3 {
                j += 1;
            }
            let letters = &f[l0..j];
            let row_abs = j < b.len() && b[j] == b'$';
            let mut k = if row_abs { j + 1 } else { j };
            let d0 = k;
            while k < b.len() && b[k].is_ascii_digit() {
                k += 1;
            }
            let ok = !letters.is_empty()
                && k > d0
                && !(k < b.len() && (b[k].is_ascii_alphanumeric() || b[k] == b'(' || b[k] == b'_'));
            if ok && let Some((row, col)) = parse_ref(&format!("{letters}{}", &f[d0..k])) {
                let col = if col_abs { col as i64 } else { col as i64 + dc };
                let row = if row_abs { row as i64 } else { row as i64 + dr };
                if col < 0 || row < 0 {
                    out.push_str("#REF!");
                } else {
                    if col_abs {
                        out.push('$');
                    }
                    out.push_str(&col_name(col as u32));
                    if row_abs {
                        out.push('$');
                    }
                    out.push_str(&(row + 1).to_string());
                }
                i = k;
                continue;
            }
            // a word: copy it whole
            let mut j = i;
            while j < b.len()
                && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'$' || b[j] == b'.')
            {
                j += 1;
            }
            out.push_str(&f[i..j.max(i + 1)]);
            i = j.max(i + 1);
            continue;
        }
        let ch = f[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// What a cell shows: its value in its number format.
pub fn display(wb: &Workbook, c: &Cell) -> String {
    match &c.value {
        Value::Empty => String::new(),
        Value::Shared(i) => wb
            .strings
            .get(*i as usize)
            .map(|s| s.to_string())
            .unwrap_or_default(),
        Value::Text(t) | Value::Error(t) => t.to_string(),
        Value::Bool(b) => String::from(if *b { "TRUE" } else { "FALSE" }),
        Value::Num(n) => number(
            *n,
            wb.formats.get(c.style as usize).map_or("General", |f| f),
            wb.date1904,
        ),
    }
}

/// The value the formula bar shows: `=` and the formula, else the plain value.
pub fn formula_bar(wb: &Workbook, c: &Cell) -> String {
    match &c.formula {
        Some(f) => format!("={f}"),
        None => match c.value {
            Value::Num(n) => general(n),
            _ => display(wb, c),
        },
    }
}

/// A number in Excel's General format: no more than 11 significant characters.
pub fn general(n: f64) -> String {
    if !n.is_finite() {
        return String::from("#NUM!");
    }
    if n == (n as i64) as f64 && n.abs() < 1e11 {
        return format!("{}", n as i64);
    }
    let mut s = format!("{n:.9}");
    if s.contains('.') {
        s = String::from(s.trim_end_matches('0').trim_end_matches('.'));
    }
    s
}

/// `n` in the format `code` (the parts Files draws: sections, decimals, grouping, percent, literals, dates, times).
pub fn number(n: f64, code: &str, date1904: bool) -> String {
    let sections: Vec<&str> = split_sections(code);
    let (sec, neg_sign) = match (sections.len(), n) {
        (1, _) => (sections[0], n < 0.0),
        (_, n) if n > 0.0 => (sections[0], false),
        (_, n) if n < 0.0 => (sections[1], false),
        (2, _) => (sections[0], false),
        _ => (sections[2], false),
    };
    let v = if sections.len() > 1 && n < 0.0 { -n } else { n };
    let (sec, _colour) = strip_brackets(sec);
    if sec.eq_ignore_ascii_case("general") || sec.is_empty() {
        return general(n);
    }
    if sec == "@" {
        return general(n);
    }
    let lower = sec.to_ascii_lowercase();
    let datey = !lower.contains('0')
        && !lower.contains('#')
        && (lower.contains('y')
            || lower.contains('d')
            || lower.contains('h')
            || lower.contains('s')
            || lower.contains('m'));
    if datey {
        return date(v, &sec, date1904);
    }
    // literals before and after the digits
    let digits_at = sec.find(['0', '#', '?']).unwrap_or(sec.len());
    let digits_end = sec
        .rfind(['0', '#', '?', ',', '.'])
        .map_or(sec.len(), |i| i + 1)
        .max(digits_at);
    let (pre, body, post) = (
        &sec[..digits_at],
        &sec[digits_at..digits_end],
        &sec[digits_end..],
    );
    let percent = sec.contains('%');
    let mut v = if percent { v * 100.0 } else { v };
    // trailing commas scale by 1000 each
    let mut body = body;
    while body.ends_with(',') {
        v /= 1000.0;
        body = &body[..body.len() - 1];
    }
    let (int_part, frac_part) = body.split_once('.').unwrap_or((body, ""));
    let decimals = frac_part
        .chars()
        .filter(|c| matches!(c, '0' | '#' | '?'))
        .count()
        .min(10);
    let group = int_part.contains(',');
    let min_int = int_part.chars().filter(|c| *c == '0').count();
    let s = format!("{:.*}", decimals, v.abs());
    let (ip, fp) = s.split_once('.').unwrap_or((&s, ""));
    let mut ip = String::from(ip.trim_start_matches('0'));
    while ip.len() < min_int {
        ip.insert(0, '0');
    }
    if group {
        let mut g = String::new();
        let n = ip.len();
        for (k, ch) in ip.chars().enumerate() {
            if k > 0 && (n - k) % 3 == 0 {
                g.push('\u{a0}');
            }
            g.push(ch);
        }
        ip = g;
    }
    let mut out = String::new();
    if (neg_sign || (sections.len() == 1 && n < 0.0))
        && v.abs() > 0.0
        && (ip.trim_matches(['0', '\u{a0}']).len() + fp.trim_matches('0').len()) > 0
    {
        out.push('-');
    }
    out.push_str(&literal(pre));
    out.push_str(&ip);
    if decimals > 0 {
        out.push('.');
        out.push_str(fp);
    }
    out.push_str(&literal(post));
    out
}

fn split_sections(code: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut q, mut start) = (false, 0);
    for (i, c) in code.char_indices() {
        match c {
            '"' => q = !q,
            ';' if !q => {
                out.push(&code[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&code[start..]);
    out.truncate(3);
    out
}

/// A section without its `[Red]`/`[$€-407]`/`[h]` parts (a currency's symbol is kept), and the colour named.
fn strip_brackets(s: &str) -> (String, Option<String>) {
    let mut out = String::new();
    let mut colour = None;
    let mut rest = s;
    while let Some(a) = rest.find('[') {
        out.push_str(&rest[..a]);
        let Some(b) = rest[a..].find(']') else { break };
        let inner = &rest[a + 1..a + b];
        if let Some(cur) = inner.strip_prefix('$') {
            out.push('"');
            out.push_str(cur.split('-').next().unwrap_or(""));
            out.push('"');
        } else if inner.eq_ignore_ascii_case("h")
            || inner.eq_ignore_ascii_case("hh")
            || inner.eq_ignore_ascii_case("mm")
            || inner.eq_ignore_ascii_case("ss")
        {
            out.push_str(inner);
        } else if inner.chars().all(|c| c.is_ascii_alphabetic()) {
            colour = Some(String::from(inner));
        }
        rest = &rest[a + b + 1..];
    }
    out.push_str(rest);
    (out, colour)
}

/// The literal parts of a format: "quoted", \x, and plain characters (not `_x`/`*x` spacing).
fn literal(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    let mut q = false;
    while let Some(c) = it.next() {
        match c {
            '"' => q = !q,
            _ if q => out.push(c),
            '\\' => {
                if let Some(n) = it.next() {
                    out.push(n);
                }
            }
            '_' | '*' => {
                if it.next().is_some() && c == '_' {
                    out.push(' ');
                }
            }
            '%' => out.push('%'),
            _ => out.push(c),
        }
    }
    out
}

/// A serial date/time in a date format (1900 system with Excel's leap day, or 1904).
fn date(v: f64, code: &str, date1904: bool) -> String {
    let days = libm_floor(v) as i64;
    let secs = libm_round((v - days as f64) * 86_400.0) as i64;
    // 1904: serial 0 is 1904-01-01, 24107 days before 1970
    let (y, m, d) = if date1904 {
        civil(days - 24107)
    } else {
        civil_1900(days)
    };
    let (hh, mi, ss) = (secs / 3600 % 24, secs / 60 % 60, secs % 60);
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let lower = code.to_ascii_lowercase();
    let ampm = lower.contains("am/pm");
    let mut out = String::new();
    let b = code.as_bytes();
    let mut i = 0;
    let mut last_h = false;
    while i < b.len() {
        let c = b[i].to_ascii_lowercase();
        let run = b[i..]
            .iter()
            .take_while(|x| x.to_ascii_lowercase() == c)
            .count();
        match c {
            b'y' => out.push_str(&if run <= 2 {
                format!("{:02}", y % 100)
            } else {
                format!("{y}")
            }),
            b'd' => out.push_str(&if run == 1 {
                format!("{d}")
            } else {
                format!("{d:02}")
            }),
            b'm' => {
                // minutes after an hour or before seconds
                let next_s = lower[i + run..]
                    .trim_start_matches([':', ' '])
                    .starts_with('s');
                if last_h || next_s {
                    out.push_str(&if run == 1 {
                        format!("{mi}")
                    } else {
                        format!("{mi:02}")
                    });
                } else {
                    let name = MONTHS[(m - 1).clamp(0, 11) as usize];
                    out.push_str(&match run {
                        1 => format!("{m}"),
                        2 => format!("{m:02}"),
                        3 => String::from(&name[..3]),
                        _ => String::from(name),
                    });
                }
            }
            b'h' => {
                let h = if ampm { (hh + 11) % 12 + 1 } else { hh };
                out.push_str(&if run == 1 {
                    format!("{h}")
                } else {
                    format!("{h:02}")
                });
                last_h = true;
                i += run;
                continue;
            }
            b's' => out.push_str(&if run == 1 {
                format!("{ss}")
            } else {
                format!("{ss:02}")
            }),
            b'a' if lower[i..].starts_with("am/pm") => {
                out.push_str(if hh < 12 { "AM" } else { "PM" });
                i += 5;
                continue;
            }
            b'"' => {
                let end = code[i + 1..].find('"').map_or(code.len(), |e| i + 1 + e);
                out.push_str(&code[i + 1..end]);
                i = end + 1;
                continue;
            }
            b'\\' => {
                if let Some(ch) = code[i + 1..].chars().next() {
                    out.push(ch);
                    i += 1 + ch.len_utf8();
                    continue;
                }
            }
            _ => {
                let ch = code[i..].chars().next().unwrap_or(' ');
                out.push(ch);
                i += ch.len_utf8();
                if !matches!(ch, ':' | ' ') {
                    last_h = false;
                }
                continue;
            }
        }
        if c != b'm' && c != b's' {
            last_h = false;
        }
        i += run;
    }
    out
}

fn libm_floor(v: f64) -> f64 {
    let t = v as i64 as f64;
    if t > v { t - 1.0 } else { t }
}

fn libm_round(v: f64) -> f64 {
    libm_floor(v + 0.5)
}

/// Excel's 1900 serial (with its 29 February 1900) as a civil date.
fn civil_1900(serial: i64) -> (i64, i64, i64) {
    if serial == 60 {
        return (1900, 2, 29);
    }
    let s = if serial > 60 { serial - 1 } else { serial };
    // serial 1 = 1900-01-01; days since 1970-01-01 = s - 25568
    civil(s - 25568)
}

/// Days since 1970-01-01 as (year, month, day) (Howard Hinnant's algorithm).
fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_and_names() {
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(27), "AB");
        assert_eq!(col_name(16383), "XFD");
        assert_eq!(parse_ref("E7"), Some((6, 4)));
        assert_eq!(parse_ref("$AB$10"), Some((9, 27)));
        assert_eq!(parse_ref("E0"), None);
    }

    #[test]
    fn shared_formulas_shift() {
        assert_eq!(shift("SUM(B2:D2)", 1, 0), "SUM(B3:D3)");
        assert_eq!(shift("B9-B7", 0, 2), "D9-D7");
        assert_eq!(
            shift("$A$1+A1+\"B2\"&LOG10(C3)", 2, 1),
            "$A$1+B3+\"B2\"&LOG10(D5)"
        );
        assert_eq!(shift("Sheet2!A1*$B1", 1, 1), "Sheet2!B2*$B2");
        assert_eq!(shift("A1", -1, 0), "#REF!");
    }

    #[test]
    fn number_formats() {
        assert_eq!(number(180000.0, "#,##0", false), "180\u{a0}000");
        assert_eq!(
            number(1038500.0, "#,##0 \"₽\"", false),
            "1\u{a0}038\u{a0}500 ₽"
        );
        assert_eq!(number(0.425, "0.0%", false), "42.5%");
        assert_eq!(number(-12.5, "0.00", false), "-12.50");
        assert_eq!(number(-12.5, "0.00;[Red](0.00)", false), "(12.50)");
        assert_eq!(number(3.0, "General", false), "3");
        assert_eq!(number(1.5e6, "#,##0,\"K\"", false), "1\u{a0}500K");
        assert_eq!(number(46000.0, "dd.mm.yyyy", false), "09.12.2025");
        assert_eq!(number(0.0, "yyyy-mm-dd", true), "1904-01-01");
        assert_eq!(
            number(46000.75, "d mmm yyyy h:mm", false),
            "9 Dec 2025 18:00"
        );
        assert_eq!(number(0.5, "h:mm AM/PM", false), "12:00 PM");
        assert_eq!(number(60.0, "yyyy-mm-dd", false), "1900-02-29");
        assert_eq!(number(12.0, "[$€-407] 0.00", false), "€ 12.00");
        assert_eq!(general(1.0 / 3.0), "0.333333333");
    }
}
