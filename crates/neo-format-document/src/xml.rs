// SPDX-License-Identifier: Apache-2.0
//! XML as a stream of start, end and text events over `xmlparser`, bounded: at most [`MAX_DEPTH`] levels and
//! [`MAX_TOKENS`] tokens, and no entity declarations (no billion laughs). Entities are resolved here: the five of
//! XML, character references, and the HTML names EPUB 2 books use without declaring them.

use crate::{Error, Result};
use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;
use xmlparser::{ElementEnd, Token, Tokenizer};

pub const MAX_DEPTH: usize = 64;
pub const MAX_TOKENS: usize = 4_000_000;

/// A name without its namespace URI: `w:p` is `{ prefix: "w", local: "p" }`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Name<'a> {
    pub prefix: &'a str,
    pub local: &'a str,
}

impl Name<'_> {
    /// `w:p` matches `is("w", "p")`; an empty prefix matches any (XHTML and OPF use a default namespace).
    pub fn is(&self, prefix: &str, local: &str) -> bool {
        self.local == local && (prefix.is_empty() || self.prefix == prefix)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ev<'a> {
    Start {
        name: Name<'a>,
        attrs: Attrs<'a>,
    },
    /// Also sent right after the start of an empty element (`<br/>`).
    End(Name<'a>),
    Text(Cow<'a, str>),
}

impl<'a> Ev<'a> {
    /// An attribute's value by local name (and prefix, when not empty).
    pub fn attr(&self, prefix: &str, local: &str) -> Option<&str> {
        match self {
            Ev::Start { attrs, .. } => attrs
                .iter()
                .find(|(n, _)| n.is(prefix, local))
                .map(|(_, v)| v.as_ref()),
            _ => None,
        }
    }
}

/// An element's attributes, entities resolved.
pub type Attrs<'a> = Vec<(Name<'a>, Cow<'a, str>)>;

pub struct Reader<'a> {
    tok: Tokenizer<'a>,
    open: Option<(Name<'a>, Attrs<'a>)>,
    queued: Option<Ev<'a>>,
    stack: Vec<Name<'a>>,
    tokens: usize,
}

/// The text of `bytes` (UTF-8, a BOM allowed; UTF-16 with a BOM converted).
pub fn text(bytes: &[u8]) -> Result<Cow<'_, str>> {
    if let Some(b) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return core::str::from_utf8(b)
            .map(Cow::Borrowed)
            .map_err(|_| Error::Bad("The text isn't UTF-8"));
    }
    let utf16 = |le: bool| {
        let units = bytes[2..].as_chunks::<2>().0.iter().map(|c| {
            if le {
                u16::from_le_bytes(*c)
            } else {
                u16::from_be_bytes(*c)
            }
        });
        Cow::Owned(
            char::decode_utf16(units)
                .map(|c| c.unwrap_or('\u{fffd}'))
                .collect(),
        )
    };
    match bytes {
        [0xFF, 0xFE, ..] => Ok(utf16(true)),
        [0xFE, 0xFF, ..] => Ok(utf16(false)),
        _ => core::str::from_utf8(bytes)
            .map(Cow::Borrowed)
            .map_err(|_| Error::Bad("The text isn't UTF-8")),
    }
}

impl<'a> Reader<'a> {
    pub fn new(s: &'a str) -> Self {
        Reader {
            tok: Tokenizer::from(s),
            open: None,
            queued: None,
            stack: Vec::new(),
            tokens: 0,
        }
    }

    /// How deep the reader is (elements open).
    pub fn depth(&self) -> usize {
        self.stack.len()
    }
}

impl<'a> Iterator for Reader<'a> {
    type Item = Result<Ev<'a>>;

    /// The next event; `None` at the end. A broken document is [`Error::Bad`].
    fn next(&mut self) -> Option<Result<Ev<'a>>> {
        if let Some(ev) = self.queued.take() {
            return Some(Ok(ev));
        }
        loop {
            let t = match self.tok.next()? {
                Ok(t) => t,
                Err(_) => return Some(Err(Error::Bad("The document's XML is broken"))),
            };
            self.tokens += 1;
            if self.tokens > MAX_TOKENS {
                return Some(Err(Error::TooBig));
            }
            match t {
                Token::ElementStart { prefix, local, .. } => {
                    self.open = Some((
                        Name {
                            prefix: prefix.as_str(),
                            local: local.as_str(),
                        },
                        Vec::new(),
                    ));
                }
                Token::Attribute {
                    prefix,
                    local,
                    value,
                    ..
                } => {
                    if let Some((_, attrs)) = &mut self.open {
                        attrs.push((
                            Name {
                                prefix: prefix.as_str(),
                                local: local.as_str(),
                            },
                            resolve(value.as_str()),
                        ));
                    }
                }
                Token::ElementEnd { end, .. } => match end {
                    ElementEnd::Open => {
                        let (name, attrs) = self.open.take()?;
                        if self.stack.len() >= MAX_DEPTH {
                            return Some(Err(Error::TooBig));
                        }
                        self.stack.push(name);
                        return Some(Ok(Ev::Start { name, attrs }));
                    }
                    ElementEnd::Empty => {
                        let (name, attrs) = self.open.take()?;
                        self.queued = Some(Ev::End(name));
                        return Some(Ok(Ev::Start { name, attrs }));
                    }
                    ElementEnd::Close(prefix, local) => {
                        let name = Name {
                            prefix: prefix.as_str(),
                            local: local.as_str(),
                        };
                        if self.stack.pop() != Some(name) {
                            return Some(Err(Error::Bad("The document's XML is broken")));
                        }
                        return Some(Ok(Ev::End(name)));
                    }
                },
                Token::Text { text } => return Some(Ok(Ev::Text(resolve(text.as_str())))),
                Token::Cdata { text, .. } => {
                    return Some(Ok(Ev::Text(Cow::Borrowed(text.as_str()))));
                }
                Token::EntityDeclaration { .. } => {
                    return Some(Err(Error::Bad("The document declares entities")));
                }
                _ => {}
            }
        }
    }
}

impl Reader<'_> {
    /// Skip to the end of the element just started (its start was the last event).
    pub fn skip_element(&mut self) -> Result<()> {
        let depth = self.stack.len();
        while self.stack.len() >= depth {
            match self.next() {
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e),
                None => return Err(Error::Bad("The document's XML is broken")),
            }
        }
        Ok(())
    }

    /// The text inside the element just started, up to its end.
    pub fn inner_text(&mut self) -> Result<String> {
        let depth = self.stack.len();
        let mut s = String::new();
        while self.stack.len() >= depth {
            match self.next() {
                Some(Ok(Ev::Text(t))) => s.push_str(&t),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e),
                None => return Err(Error::Bad("The document's XML is broken")),
            }
        }
        Ok(s)
    }
}

/// Entities resolved: `&amp;`, `&#1329;`, `&#x2014;`, `&nbsp;`… An unknown one stays as it is.
pub fn resolve(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        // `;` is ASCII: look for it in bytes (12 bytes may end inside a character)
        let Some(end) = rest.as_bytes()[..rest.len().min(12)]
            .iter()
            .position(|&b| b == b';')
        else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let c = if let Some(n) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
            u32::from_str_radix(n, 16).ok().and_then(char::from_u32)
        } else if let Some(n) = name.strip_prefix('#') {
            n.parse().ok().and_then(char::from_u32)
        } else {
            named(name)
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn named(n: &str) -> Option<char> {
    Some(match n {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "shy" => '\u{ad}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "deg" => '°',
        "plusmn" => '±',
        "times" => '×',
        "divide" => '÷',
        "middot" => '·',
        "sect" => '§',
        "para" => '¶',
        "laquo" => '«',
        "raquo" => '»',
        "lsquo" => '‘',
        "rsquo" => '’',
        "sbquo" => '‚',
        "ldquo" => '“',
        "rdquo" => '”',
        "bdquo" => '„',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "bull" => '•',
        "prime" => '′',
        "Prime" => '″',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "iexcl" => '¡',
        "iquest" => '¿',
        "ensp" => '\u{2002}',
        "emsp" => '\u{2003}',
        "thinsp" => '\u{2009}',
        "zwnj" => '\u{200c}',
        "zwj" => '\u{200d}',
        "larr" => '←',
        "rarr" => '→',
        "frac12" => '½',
        "frac14" => '¼',
        "frac34" => '¾',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(s: &str) -> Result<Vec<Ev<'_>>> {
        Reader::new(s).collect()
    }

    #[test]
    fn events_and_entities() {
        // "&" before letters of two bytes each: the entity scan must not cut a character
        let v = all("<a b='R&amp;D Компания'>&amp;Ծրագիր &Ծրագիր</a>").unwrap();
        assert_eq!(v[0].attr("", "b"), Some("R&D Компания"));
        assert_eq!(v[1], Ev::Text(Cow::Owned(String::from("&Ծրագիր &Ծրագիր"))));
        let v =
            all("<w:p a='1 &amp; 2'><w:t>A&nbsp;&#x2014;&#1329;&bogus;</w:t><br/></w:p>").unwrap();
        assert_eq!(v[0].attr("", "a"), Some("1 & 2"));
        assert_eq!(v[2], Ev::Text(Cow::Owned(String::from("A\u{a0}—Ա&bogus;"))));
        assert!(matches!(v[5], Ev::End(n) if n.local == "br"));
        assert!(matches!(v[6], Ev::End(n) if n.is("w", "p")));
    }

    #[test]
    fn refuses_entities_depth_and_mismatch() {
        let bomb = "<!DOCTYPE x [<!ENTITY a \"aaaa\">]><x>&a;</x>";
        assert_eq!(
            all(bomb).unwrap_err(),
            Error::Bad("The document declares entities")
        );
        let deep: String = "<a>".repeat(MAX_DEPTH + 1);
        assert_eq!(all(&deep).unwrap_err(), Error::TooBig);
        assert!(all("<a><b></a></b>").is_err());
        // an XHTML doctype without declarations is fine
        assert!(
            all("<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.1//EN\" \"x.dtd\"><html/>").is_ok()
        );
    }

    #[test]
    fn utf16_and_bom() {
        assert_eq!(text(&[0xEF, 0xBB, 0xBF, b'<', b'a']).unwrap(), "<a");
        assert_eq!(text(&[0xFF, 0xFE, b'<', 0, b'a', 0]).unwrap(), "<a");
        assert!(text(&[0xC0]).is_err());
    }
}
