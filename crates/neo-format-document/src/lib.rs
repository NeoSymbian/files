// SPDX-License-Identifier: Apache-2.0
//! Documents for Files (files-app.md FA10): EPUB, docx, odt and rtf read into one block model ([`model`]), laid out
//! into lines and pages by [`layout`] at the widths the screen draws (the caller measures glyphs), and xlsx read into
//! cells. Everything is read-only, bounded (members, XML depth and tokens) and ours (the crates.io readers need std).
#![no_std]

extern crate alloc;

pub mod docx;
pub mod layout;
pub mod model;
pub mod odt;
pub mod rtf;
pub mod xml;

pub use model::*;

use neo_format_archive::zip;
use neo_format_archive::{Member, ReadAt};

/// What went wrong, as the hex view says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not this kind of document (the wrong zip, no `{\rtf`).
    NotDocument,
    /// Broken: the reason, for the hex view's heading.
    Bad(&'static str),
    /// Over a limit (a part, the XML, the cells).
    TooBig,
    /// DRM.
    Protected,
    /// The file system's error code.
    Io(i32),
}

impl Error {
    /// The hex view's reason.
    pub fn reason(self) -> &'static str {
        match self {
            Error::NotDocument => "It isn't a document we read",
            Error::Bad(why) => why,
            Error::TooBig => "It is too big to show",
            Error::Protected => "It is protected (DRM)",
            Error::Io(_) => "It couldn't be read",
        }
    }
}

impl From<neo_format_archive::Error> for Error {
    fn from(e: neo_format_archive::Error) -> Self {
        match e {
            neo_format_archive::Error::NotArchive => Error::NotDocument,
            neo_format_archive::Error::Corrupt => Error::Bad("The file is damaged"),
            neo_format_archive::Error::TooBig => Error::TooBig,
            neo_format_archive::Error::Io(n) => Error::Io(n),
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// At most this many bytes of one part (an XML file, a chapter) are read.
pub const MAX_PART: u64 = 32 << 20;

/// The zip members of a document, and the file they are in.
pub struct Package<R: ReadAt> {
    pub r: R,
    pub members: alloc::vec::Vec<Member>,
}

impl<R: ReadAt> Package<R> {
    pub fn open(r: R) -> Result<Self> {
        let members = zip::list(&r)?;
        Ok(Package { r, members })
    }

    /// A member by name: exact first, then ignoring ASCII case (Office part names are case-insensitive).
    pub fn find(&self, name: &str) -> Option<&Member> {
        let name = name.trim_start_matches('/');
        let file = |m: &&Member| m.kind == neo_format_archive::Kind::File;
        self.members
            .iter()
            .filter(file)
            .find(|m| m.name == name)
            .or_else(|| {
                self.members
                    .iter()
                    .filter(file)
                    .find(|m| m.name.eq_ignore_ascii_case(name))
            })
    }

    /// A member's bytes, at most [`MAX_PART`].
    pub fn read(&self, name: &str) -> Result<alloc::vec::Vec<u8>> {
        let m = self
            .find(name)
            .ok_or(Error::Bad("A part of the document is missing"))?;
        Ok(zip::read_member(&self.r, m, MAX_PART)?)
    }

    pub fn has(&self, name: &str) -> bool {
        self.find(name).is_some()
    }
}

/// The documents FA10 reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Epub,
    Docx,
    Odt,
    Rtf,
    Xlsx,
}

impl Kind {
    /// By extension (any case): the viewer is chosen by name, then [`detect`] checks the bytes.
    pub fn of_ext(ext: &str) -> Option<Kind> {
        [
            ("epub", Kind::Epub),
            ("docx", Kind::Docx),
            ("odt", Kind::Odt),
            ("rtf", Kind::Rtf),
            ("xlsx", Kind::Xlsx),
        ]
        .into_iter()
        .find(|(e, _)| e.eq_ignore_ascii_case(ext))
        .map(|(_, k)| k)
    }
}

/// What the bytes are: `{\rtf`, or a zip whose `mimetype` or parts say EPUB, ODF text, Word or Excel.
pub fn detect<R: ReadAt>(r: &R) -> Option<Kind> {
    let mut head = [0u8; 5];
    let n = r.read_at(0, &mut head).ok()?;
    if &head[..n] == b"{\\rtf" {
        return Some(Kind::Rtf);
    }
    let p = Package::open(r).ok()?;
    if let Some(m) = p.find("mimetype").filter(|m| m.size < 100) {
        let t = zip::read_member(p.r, m, 100).ok()?;
        match t.trim_ascii() {
            b"application/epub+zip" => return Some(Kind::Epub),
            b"application/vnd.oasis.opendocument.text" => return Some(Kind::Odt),
            _ => {}
        }
    }
    if p.has("[Content_Types].xml") {
        if p.has("word/document.xml") {
            return Some(Kind::Docx);
        }
        if p.has("xl/workbook.xml") {
            return Some(Kind::Xlsx);
        }
    }
    None
}

/// Read a docx, odt or rtf ([`Kind::Epub`] and [`Kind::Xlsx`] have their own readers: a book is read a chapter at a
/// time, a sheet as cells). An rtf is read whole, at most [`MAX_PART`].
pub fn read<R: ReadAt>(kind: Kind, r: R) -> Result<Doc> {
    match kind {
        Kind::Docx => docx::read(&Package::open(r)?),
        Kind::Odt => odt::read(&Package::open(r)?),
        Kind::Rtf => {
            let size = r.size();
            if size > MAX_PART {
                return Err(Error::TooBig);
            }
            let mut b = alloc::vec![0u8; size as usize];
            let n = r.read_at(0, &mut b)?;
            b.truncate(n);
            rtf::read(&b)
        }
        Kind::Epub | Kind::Xlsx => Err(Error::NotDocument),
    }
}

/// `dir` of `base` joined with `rel` (`OEBPS/text/c1.xhtml` + `../img/a.png` → `OEBPS/img/a.png`); a `#fragment` and
/// a `?query` are dropped, `%20`-style escapes decoded.
pub fn join(base: &str, rel: &str) -> alloc::string::String {
    use alloc::string::String;
    use alloc::vec::Vec;
    let rel = rel.split(['#', '?']).next().unwrap_or("");
    let rel = unescape(rel);
    let mut parts: Vec<&str> = if rel.starts_with('/') {
        Vec::new()
    } else {
        base.rsplit_once('/')
            .map(|(d, _)| d.split('/').collect())
            .unwrap_or_default()
    };
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let mut out = String::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(p);
    }
    out
}

fn unescape(s: &str) -> alloc::string::String {
    let b = s.as_bytes();
    let mut out = alloc::vec::Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    alloc::string::String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_paths() {
        assert_eq!(
            join("OEBPS/text/c1.xhtml", "../img/a%20b.png#x"),
            "OEBPS/img/a b.png"
        );
        assert_eq!(join("content.opf", "c1.xhtml"), "c1.xhtml");
        assert_eq!(join("word/document.xml", "media/i.png"), "word/media/i.png");
        assert_eq!(join("a/b.xml", "/x/y.png"), "x/y.png");
        assert_eq!(join("a.xml", "%"), "%");
    }
}
