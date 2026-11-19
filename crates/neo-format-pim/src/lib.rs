// SPDX-License-Identifier: Apache-2.0
//! Cards and events for Files (files-app.md FA7): vCard 2.1, 3.0 and 4.0 ([`vcard`]) and iCalendar ([`ical`]), read
//! and written so that nothing the forms don't edit changes.
//!
//! Both formats are content lines (`group.NAME;PARAM=a,b:value`, folded at 75 octets). A file is kept as its lines,
//! each with the exact text it was read from ([`Line::raw`]); an edit replaces only the lines it changes, with text
//! made in the file's own dialect. Writing is the lines' text joined, so an unedited file comes back byte for byte and
//! unknown properties (PHOTO, X-…, ADR, VTIMEZONE…) survive every edit.
//!
//! datetime.md's DTSRV reuses [`ical`] for its stores (its `pim-ics`).
#![no_std]

extern crate alloc;

pub mod date;
pub mod ical;
pub mod vcard;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No complete BEGIN:VCARD … END:VCARD.
    NoCards,
    /// No complete VEVENT or VTODO.
    NoEvents,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Error::NoCards => "No contacts in this file",
            Error::NoEvents => "No events in this file",
        })
    }
}

/// One parameter: `TYPE=CELL,HOME` (a vCard 2.1 bare `CELL` reads as `TYPE=CELL`). Names are upper case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub values: Vec<String>,
}

/// A content line (unfolded) and the text it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// The physical lines as read (folds and line ends included), or as made by an edit.
    pub raw: String,
    pub group: String,
    /// Upper case; empty for a line that isn't a property (kept as it is).
    pub name: String,
    pub params: Vec<Param>,
    /// The value after the first unquoted ':', unfolded but still escaped (or quoted-printable).
    pub value: String,
}

impl Line {
    /// Whether a parameter `name` has `value` (case-insensitive).
    pub fn has(&self, name: &str, value: &str) -> bool {
        self.params
            .iter()
            .any(|p| p.name == name && p.values.iter().any(|v| v.eq_ignore_ascii_case(value)))
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| p.values.first())
            .map(String::as_str)
    }

    fn qp(&self) -> bool {
        self.has("ENCODING", "QUOTED-PRINTABLE")
    }

    /// The value as text: quoted-printable decoded, escapes undone.
    pub fn text(&self) -> String {
        unescape(&self.decoded())
    }

    /// A structured value's parts (N, ORG, ADR), split at unescaped ';', each unescaped.
    pub fn parts(&self) -> Vec<String> {
        split_unescaped(&self.decoded(), ';')
            .iter()
            .map(|s| unescape(s))
            .collect()
    }

    fn decoded(&self) -> String {
        if !self.qp() {
            return self.value.clone();
        }
        let bytes = quoted_printable(&self.value);
        match String::from_utf8(bytes) {
            Ok(s) => s,
            // a Nokia CHARSET=ISO-8859-1 card: bytes are code points
            Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
        }
    }
}

/// The line end a file uses (CRLF unless its first line ends in a bare LF).
fn eol_of(text: &str) -> &'static str {
    match text.find('\n') {
        Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => "\r\n",
        Some(_) => "\n",
        None => "\r\n",
    }
}

/// The physical lines with their ends.
fn physical(text: &str) -> impl Iterator<Item = &str> {
    text.split_inclusive('\n')
}

fn trim_eol(s: &str) -> &str {
    s.trim_end_matches(['\r', '\n'])
}

/// Split `text` into content lines: a physical line starting with a space or tab continues the one before; so does
/// the next line after a quoted-printable value ending in a soft break ('=').
pub fn lines(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let mut raw = String::new();
    let mut logical = String::new();
    // decided once, from the line's first physical line (a rescan per fold would be quadratic)
    let mut qp = false;
    for p in physical(text) {
        let body = trim_eol(p);
        // a soft break first: the line after it is taken whole, a leading space included
        let soft = !raw.is_empty() && qp && logical.ends_with('=');
        let folded = !raw.is_empty() && (body.starts_with(' ') || body.starts_with('\t'));
        if soft {
            logical.pop();
            logical.push_str(body);
        } else if folded {
            logical.push_str(&body[1..]);
        } else {
            if !raw.is_empty() {
                out.push(parse_line(core::mem::take(&mut raw), &logical));
            }
            logical.clear();
            logical.push_str(body);
            qp = is_qp_head(body);
        }
        raw.push_str(p);
    }
    if !raw.is_empty() {
        out.push(parse_line(raw, &logical));
    }
    out
}

fn is_qp_head(logical: &str) -> bool {
    let head = match find_colon(logical) {
        Some(i) => &logical[..i],
        None => return false,
    };
    head.to_ascii_uppercase().contains("QUOTED-PRINTABLE")
}

/// The first ':' outside double quotes.
fn find_colon(s: &str) -> Option<usize> {
    let mut quoted = false;
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => return Some(i),
            _ => {}
        }
    }
    None
}

fn split_quoted(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut quoted, mut start) = (false, 0);
    for (i, c) in s.char_indices() {
        if c == '"' {
            quoted = !quoted;
        } else if c == sep && !quoted {
            out.push(&s[start..i]);
            start = i + 1;
        }
    }
    out.push(&s[start..]);
    out
}

fn parse_line(raw: String, logical: &str) -> Line {
    let Some(colon) = find_colon(logical) else {
        return Line {
            raw,
            group: String::new(),
            name: String::new(),
            params: Vec::new(),
            value: String::new(),
        };
    };
    let mut head = split_quoted(&logical[..colon], ';').into_iter();
    let first = head.next().unwrap_or("");
    let (group, name) = match first.rfind('.') {
        Some(i) => (&first[..i], &first[i + 1..]),
        None => ("", first),
    };
    let params = head
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (n, v) = match p.split_once('=') {
                Some((n, v)) => (n.trim().to_ascii_uppercase(), v),
                None => (String::from("TYPE"), p),
            };
            // TYPE="work,voice" (4.0) is a list in quotes; other quoted values (CN="Smith, J") are one value
            let list = n == "TYPE";
            Param {
                values: split_quoted(v, ',')
                    .into_iter()
                    .flat_map(|v| {
                        let v = v.trim_matches('"');
                        if list {
                            v.split(',').map(str::to_string).collect()
                        } else {
                            alloc::vec![v.to_string()]
                        }
                    })
                    .collect(),
                name: n,
            }
        })
        .collect();
    Line {
        raw,
        group: group.to_string(),
        name: name.trim().to_ascii_uppercase(),
        params,
        value: logical[colon + 1..].to_string(),
    }
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

/// Quoted-printable to bytes (soft breaks are gone already: [`lines`] joined them).
fn quoted_printable(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'='
            && let (Some(h), Some(l)) = (
                b.get(i + 1).copied().and_then(hex),
                b.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Split at `sep` not preceded by a backslash escape (the parts stay escaped).
fn split_unescaped(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut esc = false;
    for c in s.chars() {
        if esc {
            cur.push('\\');
            cur.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == sep {
            out.push(core::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    if esc {
        cur.push('\\');
    }
    out.push(cur);
    out
}

/// `\n` `\N` → a line break; `\,` `\;` `\\` → the character; any other escape stays as it is.
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(c @ (',' | ';' | '\\' | ':')) => out.push(c),
            Some(c) => {
                out.push('\\');
                out.push(c);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The escaping of a text value (RFC 6350 and 5545; vCard 2.1 escapes only ';' and '\').
pub fn escape(s: &str, commas: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' if commas => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// A new line's text: `head:value` folded at 75 octets (never inside a character), with `eol`.
pub fn folded(head: &str, value: &str, eol: &str) -> String {
    let line = alloc::format!("{head}:{value}");
    let mut out = String::with_capacity(line.len() + 8);
    let mut width = 0;
    for c in line.chars() {
        let n = c.len_utf8();
        if width + n > 75 {
            out.push_str(eol);
            out.push(' ');
            width = 1;
        }
        out.push(c);
        width += n;
    }
    out.push_str(eol);
    out
}

/// A new vCard 2.1 line: plain when the value is printable ASCII, else UTF-8 quoted-printable with soft breaks (the
/// form phone's `vcard::export` writes and Nokia phones read).
pub fn qp_line(head: &str, value: &str, eol: &str) -> String {
    if value.bytes().all(|b| (b' '..=b'~').contains(&b)) {
        return alloc::format!("{head}:{value}{eol}");
    }
    let mut out = alloc::format!("{head};CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:");
    let mut width = out.len();
    for &b in value.as_bytes() {
        let n = if (b' '..=b'~').contains(&b) && b != b'=' {
            1
        } else {
            3
        };
        if width + n > 75 {
            out.push('=');
            out.push_str(eol);
            width = 0;
        }
        if n == 1 {
            out.push(b as char);
        } else {
            let _ = write!(out, "={b:02X}");
        }
        width += n;
    }
    out.push_str(eol);
    out
}

/// Make a line from its text (one content line, with its end).
fn made(raw: String) -> Line {
    lines(&raw).into_iter().next().unwrap_or(Line {
        raw,
        group: String::new(),
        name: String::new(),
        params: Vec::new(),
        value: String::new(),
    })
}

/// The parameters as written (`;TYPE=CELL;PREF=1`, or vCard 2.1's bare `;CELL;PREF` when `bare`), without ENCODING
/// and CHARSET (a new value sets its own).
fn params_text(params: &[Param], bare: bool) -> String {
    let mut out = String::new();
    for p in params {
        if p.name == "ENCODING" || p.name == "CHARSET" {
            continue;
        }
        if bare && p.name == "TYPE" {
            for v in &p.values {
                let _ = write!(out, ";{v}");
            }
            continue;
        }
        let _ = write!(out, ";{}=", p.name);
        for (i, v) in p.values.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            if v.contains([':', ';', ',']) {
                let _ = write!(out, "\"{v}\"");
            } else {
                out.push_str(v);
            }
        }
    }
    out
}

/// The whole text of `lines`.
fn joined(lines: &[Line]) -> String {
    lines.iter().map(|l| l.raw.as_str()).collect()
}
