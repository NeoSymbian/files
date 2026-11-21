// SPDX-License-Identifier: Apache-2.0
//! EPUB 2 and 3: `META-INF/container.xml` → the package (title, manifest, spine) → the contents (EPUB 3's nav
//! document, else EPUB 2's NCX), and a chapter's XHTML as blocks ([`html`](crate::html)). A book is read a chapter at
//! a time. DRM (an `encryption.xml` that encrypts more than fonts) is refused; obfuscated fonts are fine (unused).

use crate::model::Doc;
use crate::xml::{Ev, Reader, text};
use crate::{Error, Package, Result, join};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use neo_format_archive::ReadAt;

/// At most this many chapters (spine items) and contents entries.
pub const MAX_SPINE: usize = 5000;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Book {
    pub title: String,
    /// The chapters' members, in reading order.
    pub spine: Vec<String>,
    /// The contents: a title, the chapter it opens, and how deep it is.
    pub toc: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub title: String,
    pub chapter: usize,
    pub depth: u8,
}

impl Book {
    /// The contents entry a chapter is under (its title for the footer), if any.
    pub fn title_of(&self, chapter: usize) -> Option<&str> {
        self.toc
            .iter()
            .rev()
            .find(|e| e.chapter <= chapter)
            .map(|e| e.title.as_str())
    }
}

fn xml<R: ReadAt>(p: &Package<R>, name: &str) -> Result<String> {
    let b = p.read(name)?;
    Ok(String::from(text(&b)?.as_ref()))
}

/// Open a book: its title, spine and contents.
pub fn open<R: ReadAt>(p: &Package<R>) -> Result<Book> {
    if p.has("META-INF/encryption.xml") {
        let enc = xml(p, "META-INF/encryption.xml")?;
        for ev in Reader::new(&enc) {
            let ev = ev?;
            if let Ev::Start { name, .. } = &ev
                && name.local == "EncryptionMethod"
                && !matches!(
                    ev.attr("", "Algorithm"),
                    Some("http://www.idpf.org/2008/embedding" | "http://ns.adobe.com/pdf/enc#RC")
                )
            {
                return Err(Error::Protected);
            }
        }
    }
    let container = xml(p, "META-INF/container.xml")?;
    let mut opf_path = None;
    for ev in Reader::new(&container) {
        let ev = ev?;
        if let Ev::Start { name, .. } = &ev
            && name.local == "rootfile"
            && let Some(f) = ev.attr("", "full-path")
        {
            opf_path = Some(String::from(f));
            break;
        }
    }
    let opf_path = opf_path.ok_or(Error::Bad("The book has no package"))?;
    let opf = xml(p, &opf_path)?;
    let mut book = Book::default();
    // id → (href, media type, properties)
    let mut manifest: BTreeMap<String, (String, String, String)> = BTreeMap::new();
    let mut spine_ids = Vec::new();
    let mut ncx_id = None;
    let mut r = Reader::new(&opf);
    while let Some(ev) = r.next() {
        let ev = ev?;
        let Ev::Start { name, .. } = &ev else {
            continue;
        };
        match name.local {
            "title" if book.title.is_empty() && name.prefix == "dc" => {
                book.title = String::from(r.inner_text()?.trim())
            }
            "item" => {
                if let (Some(id), Some(href)) = (ev.attr("", "id"), ev.attr("", "href")) {
                    manifest.insert(
                        String::from(id),
                        (
                            join(&opf_path, href),
                            String::from(ev.attr("", "media-type").unwrap_or("")),
                            String::from(ev.attr("", "properties").unwrap_or("")),
                        ),
                    );
                }
            }
            "spine" => ncx_id = ev.attr("", "toc").map(String::from),
            "itemref" => {
                if let Some(id) = ev.attr("", "idref")
                    && spine_ids.len() < MAX_SPINE
                {
                    spine_ids.push(String::from(id));
                }
            }
            _ => {}
        }
    }
    book.spine = spine_ids
        .iter()
        .filter_map(|id| manifest.get(id).map(|m| m.0.clone()))
        .collect();
    if book.spine.is_empty() {
        return Err(Error::Bad("The book has no chapters"));
    }
    let chapter_of = |href: &str, base: &str| {
        let path = join(base, href);
        book.spine.iter().position(|s| *s == path)
    };
    let nav = manifest
        .values()
        .find(|m| m.2.split_whitespace().any(|p| p == "nav"))
        .map(|m| m.0.clone());
    let ncx = ncx_id
        .and_then(|id| manifest.get(&id).map(|m| m.0.clone()))
        .or_else(|| {
            manifest
                .values()
                .find(|m| m.1 == "application/x-dtbncx+xml")
                .map(|m| m.0.clone())
        });
    let mut toc = Vec::new();
    if let Some(nav) = nav.filter(|n| p.has(n)) {
        // <nav epub:type="toc"><ol><li><a href>title</a><ol>…
        let doc = xml(p, &nav)?;
        let mut r = Reader::new(&doc);
        let (mut in_toc, mut toc_depth, mut ol) = (false, 0, 0u8);
        while let Some(ev) = r.next() {
            let ev = ev?;
            match &ev {
                Ev::Start { name, .. } if name.local == "nav" => {
                    let kind = ev.attr("epub", "type").unwrap_or("");
                    if kind.split_whitespace().any(|t| t == "toc")
                        || (!in_toc && toc.is_empty() && kind.is_empty())
                    {
                        in_toc = true;
                        toc_depth = r.depth();
                    }
                }
                Ev::Start { name, .. } if in_toc && name.local == "ol" => ol = ol.saturating_add(1),
                Ev::End(name) if in_toc && name.local == "ol" => ol = ol.saturating_sub(1),
                Ev::Start { name, .. } if in_toc && name.local == "a" => {
                    let href = String::from(ev.attr("", "href").unwrap_or(""));
                    let title = collapse(&r.inner_text()?);
                    if let Some(chapter) = chapter_of(&href, &nav)
                        && toc.len() < MAX_SPINE
                    {
                        toc.push(Entry {
                            title,
                            chapter,
                            depth: ol.saturating_sub(1),
                        });
                    }
                }
                Ev::End(name) if in_toc && name.local == "nav" && r.depth() < toc_depth => {
                    in_toc = false
                }
                _ => {}
            }
        }
    } else if let Some(ncx) = ncx.filter(|n| p.has(n)) {
        // <navPoint><navLabel><text>title</text></navLabel><content src/><navPoint>…
        let doc = xml(p, &ncx)?;
        let mut r = Reader::new(&doc);
        let (mut depth, mut title) = (0u8, String::new());
        while let Some(ev) = r.next() {
            let ev = ev?;
            match &ev {
                Ev::Start { name, .. } if name.local == "navPoint" => {
                    depth = depth.saturating_add(1)
                }
                Ev::End(name) if name.local == "navPoint" => depth = depth.saturating_sub(1),
                Ev::Start { name, .. } if name.local == "text" && depth > 0 => {
                    title = collapse(&r.inner_text()?)
                }
                Ev::Start { name, .. } if name.local == "content" => {
                    if let Some(chapter) = ev.attr("", "src").and_then(|s| chapter_of(s, &ncx))
                        && toc.len() < MAX_SPINE
                    {
                        toc.push(Entry {
                            title: core::mem::take(&mut title),
                            chapter,
                            depth: depth.saturating_sub(1),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    book.toc = toc;
    if book.title.is_empty() {
        book.title = String::from("Book");
    }
    Ok(book)
}

/// A chapter as blocks; its pictures are members of the book.
pub fn chapter<R: ReadAt>(p: &Package<R>, book: &Book, i: usize) -> Result<Doc> {
    let path = book.spine.get(i).ok_or(Error::Bad("No such chapter"))?;
    let b = p.read(path)?;
    crate::html::read(&text(&b)?, path)
}

fn collapse(s: &str) -> String {
    let mut out = String::new();
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}
