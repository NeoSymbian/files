// SPDX-License-Identifier: Apache-2.0
//! The byte-exact text model (files-app.md FA4): a file decodes into lines that keep their own line end, and
//! encodes back to the same bytes — BOM, CRLF/LF/CR mixes and a missing final newline included.

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    /// UTF-8 with the EF BB BF mark.
    Utf8Bom,
    /// UTF-16, little-endian, with the FF FE mark.
    Utf16Le,
    /// UTF-16, big-endian, with the FE FF mark.
    Utf16Be,
}

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf8Bom => "UTF-8 BOM",
            Encoding::Utf16Le => "UTF-16LE",
            Encoding::Utf16Be => "UTF-16BE",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnd {
    /// The last line (nothing after it).
    None,
    Lf,
    CrLf,
    Cr,
}

impl LineEnd {
    pub fn label(self) -> &'static str {
        match self {
            LineEnd::None | LineEnd::Lf => "LF",
            LineEnd::CrLf => "CRLF",
            LineEnd::Cr => "CR",
        }
    }

    fn text(self) -> &'static str {
        match self {
            LineEnd::None => "",
            LineEnd::Lf => "\n",
            LineEnd::CrLf => "\r\n",
            LineEnd::Cr => "\r",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub end: LineEnd,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    pub encoding: Encoding,
    /// Never empty: an empty file is one empty line.
    pub lines: Vec<Line>,
}

/// The bytes aren't text in any encoding we keep exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotText;

/// Splits on `\r\n`, `\n` and `\r`; the last piece ends with [`LineEnd::None`].
pub fn split_lines(s: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let b = s.as_bytes();
    let (mut start, mut i) = (0, 0);
    while i < b.len() {
        let end = match b[i] {
            b'\n' => Some((LineEnd::Lf, 1)),
            b'\r' if b.get(i + 1) == Some(&b'\n') => Some((LineEnd::CrLf, 2)),
            b'\r' => Some((LineEnd::Cr, 1)),
            _ => None,
        };
        if let Some((end, n)) = end {
            lines.push(Line {
                text: String::from(&s[start..i]),
                end,
            });
            i += n;
            start = i;
        } else {
            i += 1;
        }
    }
    lines.push(Line {
        text: String::from(&s[start..]),
        end: LineEnd::None,
    });
    lines
}

fn utf16(b: &[u8], le: bool) -> Result<String, NotText> {
    let (units, rest) = b.as_chunks::<2>();
    if !rest.is_empty() {
        return Err(NotText);
    }
    char::decode_utf16(units.iter().map(|u| {
        if le {
            u16::from_le_bytes(*u)
        } else {
            u16::from_be_bytes(*u)
        }
    }))
    .collect::<Result<String, _>>()
    .map_err(|_| NotText)
}

impl Doc {
    pub fn new() -> Doc {
        Doc {
            encoding: Encoding::Utf8,
            lines: alloc::vec![Line {
                text: String::new(),
                end: LineEnd::None,
            }],
        }
    }

    pub fn decode(b: &[u8]) -> Result<Doc, NotText> {
        let (encoding, text) = if let Some(r) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
            (
                Encoding::Utf8Bom,
                String::from(core::str::from_utf8(r).map_err(|_| NotText)?),
            )
        } else if let Some(r) = b.strip_prefix(&[0xFF, 0xFE]) {
            (Encoding::Utf16Le, utf16(r, true)?)
        } else if let Some(r) = b.strip_prefix(&[0xFE, 0xFF]) {
            (Encoding::Utf16Be, utf16(r, false)?)
        } else {
            (
                Encoding::Utf8,
                String::from(core::str::from_utf8(b).map_err(|_| NotText)?),
            )
        };
        Ok(Doc {
            encoding,
            lines: split_lines(&text),
        })
    }

    /// What can be shown of bytes that aren't text: invalid sequences become U+FFFD (read-only: it can't save back).
    pub fn lossy(b: &[u8]) -> Doc {
        Doc::decode(b).unwrap_or_else(|_| Doc {
            encoding: Encoding::Utf8,
            lines: split_lines(&String::from_utf8_lossy(b)),
        })
    }

    pub fn text(&self) -> String {
        let mut s = String::new();
        for l in &self.lines {
            s.push_str(&l.text);
            s.push_str(l.end.text());
        }
        s
    }

    pub fn encode(&self) -> Vec<u8> {
        let text = self.text();
        match self.encoding {
            Encoding::Utf8 => text.into_bytes(),
            Encoding::Utf8Bom => {
                let mut b = alloc::vec![0xEF, 0xBB, 0xBF];
                b.extend_from_slice(text.as_bytes());
                b
            }
            Encoding::Utf16Le | Encoding::Utf16Be => {
                let le = self.encoding == Encoding::Utf16Le;
                let mut b = if le {
                    alloc::vec![0xFF, 0xFE]
                } else {
                    alloc::vec![0xFE, 0xFF]
                };
                for u in text.encode_utf16() {
                    b.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() });
                }
                b
            }
        }
    }

    /// The line end most lines use (new lines get it); LF for a one-line file.
    pub fn main_end(&self) -> LineEnd {
        let mut n = [0usize; 3];
        for l in &self.lines {
            match l.end {
                LineEnd::Lf => n[0] += 1,
                LineEnd::CrLf => n[1] += 1,
                LineEnd::Cr => n[2] += 1,
                LineEnd::None => {}
            }
        }
        if n[1] > n[0] && n[1] >= n[2] {
            LineEnd::CrLf
        } else if n[2] > n[0] && n[2] > n[1] {
            LineEnd::Cr
        } else {
            LineEnd::Lf
        }
    }

    /// Indentation by tabs: the file has a line that starts with one.
    pub fn uses_tabs(&self) -> bool {
        self.lines.iter().any(|l| l.text.starts_with('\t'))
    }
}

impl Default for Doc {
    fn default() -> Self {
        Doc::new()
    }
}
