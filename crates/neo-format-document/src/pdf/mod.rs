// SPDX-License-Identifier: Apache-2.0
//! PDF (files-app.md FA11): read by hayro-syntax (our no_std fork, NeoSymbian/hayro), a page's content interpreted by
//! hayro-interpret and drawn with tiny-skia by [`canvas`]. The file's bytes stay in the caller's buffer (on the phone
//! a chunk of their own); pages are drawn one at a time, whole or a window of one (a zoomed view).
//!
//! Left out (named in files-app.md): text selection and search, links, forms, colour management (ICC profiles use
//! their alternate space), the CJK predefined CMaps other than Identity (their text isn't drawn), hinting.

mod canvas;

use crate::{Error, Result};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::Cell;
use core::sync::atomic::{AtomicU32, Ordering};
use hayro_interpret::hayro_syntax::object::dict::keys;
use hayro_interpret::hayro_syntax::object::{
    Array, Dict, MaybeRef, Name, Object, ObjectIdentifier,
};
use hayro_interpret::hayro_syntax::{LoadPdfError, Pdf as Syntax};
use hayro_interpret::kurbo::{Affine, Rect};
use hayro_interpret::util::{RectExt, TransformExt};
use hayro_interpret::{Context, InterpreterCache, InterpreterSettings, interpret_page};
use neo_format_image::Pixels;

/// At most this many pixels in one drawing (a page or a window of one).
pub const MAX_PIXELS: u64 = 12_000_000;
/// A page side is at most this many points (200 inches, the PDF limit) as the viewer lays it out.
pub const MAX_SIDE: f32 = 14_400.0;
/// The outline (Contents) keeps at most this many entries, this deep.
const MAX_OUTLINE: usize = 2000;
const MAX_DEPTH: usize = 8;

/// An open PDF.
pub struct Pdf {
    doc: Syntax,
    /// Each page's size in points as shown (after the crop box and `/Rotate`).
    sizes: Vec<(f32, f32)>,
}

/// One entry of the outline: its title, the page it opens (from 0) and how deep it is (from 0).
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub title: String,
    pub page: Option<usize>,
    pub depth: usize,
}

/// A drawn page or window, and whether part of it couldn't be drawn.
pub struct Drawn {
    pub px: Pixels,
    pub partial: bool,
}

impl Pdf {
    /// Opens `data` (a whole PDF file). An empty `password` opens files that need none (or only an owner password).
    /// The document is used on the thread that opened it (hayro-syntax without std shares its parts with `Rc`).
    pub fn open<T: AsRef<[u8]> + 'static>(data: T, password: &str) -> Result<Pdf> {
        let doc = Syntax::new_with_password(Rc::new(data), password).map_err(|e| match e {
            LoadPdfError::Decryption(d) => match d {
                hayro_interpret::hayro_syntax::DecryptionError::PasswordProtected => {
                    Error::Password
                }
                hayro_interpret::hayro_syntax::DecryptionError::UnsupportedAlgorithm => {
                    Error::Bad("Its encryption isn't supported")
                }
                _ => Error::Bad("Its encryption is damaged"),
            },
            LoadPdfError::Invalid => Error::Bad("The file is damaged"),
        })?;
        // Sizes the viewer can lay out: 1 pt to 200 inches a side (a broken MediaBox gets Letter).
        let fit = |v: f32, or: f32| {
            if v.is_finite() && v >= 1.0 {
                v.min(MAX_SIDE)
            } else {
                or
            }
        };
        let sizes: Vec<(f32, f32)> = doc
            .pages()
            .iter()
            .map(|p| p.render_dimensions())
            .map(|(w, h)| (fit(w, 612.0), fit(h, 792.0)))
            .collect();
        if sizes.is_empty() {
            return Err(Error::Bad("It has no pages"));
        }
        Ok(Pdf { doc, sizes })
    }

    pub fn pages(&self) -> usize {
        self.sizes.len()
    }

    /// The page's size in points (1/72 inch) as shown.
    pub fn size(&self, page: usize) -> (f32, f32) {
        self.sizes.get(page).copied().unwrap_or((612.0, 792.0))
    }

    /// The document's title, if it has one.
    pub fn title(&self) -> Option<String> {
        let t = self.doc.metadata().title.as_deref()?;
        let t = text(t);
        (!t.trim().is_empty()).then_some(t)
    }

    /// The outline (bookmarks), depth first.
    pub fn outline(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        let xref = self.doc.xref();
        let Some(catalog) = xref.get::<Dict<'_>>(xref.root_id()) else {
            return out;
        };
        let Some(first) = catalog
            .get::<Dict<'_>>(keys::OUTLINES)
            .and_then(|o| o.get::<Dict<'_>>(keys::FIRST))
        else {
            return out;
        };
        let pages: Vec<Option<ObjectIdentifier>> =
            self.doc.pages().iter().map(|p| p.raw().obj_id()).collect();
        let names = named_dests(&catalog);
        // Depth first with our own stack: a hostile outline can't overflow ours, and loops end at the entry limit.
        let mut stack: Vec<(Dict<'_>, usize)> = Vec::from([(first, 0)]);
        let mut seen = Vec::new();
        while let Some((item, depth)) = stack.pop() {
            if out.len() >= MAX_OUTLINE {
                break;
            }
            if let Some(id) = item.obj_id() {
                if seen.contains(&id) {
                    continue;
                }
                seen.push(id);
            }
            let title = item
                .get::<hayro_interpret::hayro_syntax::object::String<'_>>(keys::TITLE)
                .map(|s| text(s.as_bytes()));
            let page = dest_page(&item, &pages, &names);
            out.push(Entry {
                title: title.unwrap_or_default(),
                page,
                depth,
            });
            if let Some(next) = item.get::<Dict<'_>>(keys::NEXT) {
                stack.push((next, depth));
            }
            if depth + 1 < MAX_DEPTH
                && let Some(child) = item.get::<Dict<'_>>(keys::FIRST)
            {
                stack.push((child, depth + 1));
            }
        }
        out
    }

    /// A drawer of this document's pages (fonts and other parsed objects are kept between pages).
    pub fn renderer(&self) -> Renderer<'_> {
        Renderer {
            pdf: self,
            cache: InterpreterCache::new(),
            glyphs: Rc::default(),
        }
    }
}

/// Draws pages, keeping what it parsed (fonts, colour spaces, glyph outlines) between them.
pub struct Renderer<'a> {
    pdf: &'a Pdf,
    cache: InterpreterCache<'a>,
    glyphs: Rc<canvas::GlyphCache>,
}

impl<'a> Renderer<'a> {
    /// Draws `page` at `scale` pixels per point, only the window `(x, y, w, h)` of the scaled page (in pixels), as
    /// RGB on white.
    pub fn render(
        &mut self,
        page: usize,
        scale: f32,
        (x, y, w, h): (u32, u32, u32, u32),
    ) -> Result<Drawn> {
        let p = self
            .pdf
            .doc
            .pages()
            .get(page)
            .ok_or(Error::Bad("No such page"))?;
        if w == 0
            || h == 0
            || u64::from(w) * u64::from(h) > MAX_PIXELS
            || !(scale > 0.0 && scale.is_finite())
        {
            return Err(Error::TooBig);
        }
        let transform = Affine::translate((-f64::from(x), -f64::from(y)))
            * Affine::scale(f64::from(scale))
            * p.initial_transform(true).to_kurbo();
        let target = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
        let warnings = Arc::new(AtomicU32::new(0));
        let counted = warnings.clone();
        let settings = InterpreterSettings {
            warning_sink: Arc::new(move |_| {
                counted.fetch_add(1, Ordering::Relaxed);
            }),
            ..InterpreterSettings::default()
        };
        let mut context = Context::new(transform, target, &self.cache, p.xref(), settings);
        let partial = Rc::new(Cell::new(false));
        let mut canvas = canvas::Canvas::new(
            w,
            h,
            tiny_skia::Color::WHITE,
            self.glyphs.clone(),
            partial.clone(),
        )
        .ok_or(Error::TooBig)?;
        let crop = transform.transform_rect_bbox(p.intersected_crop_box().to_kurbo());
        let clip = !crop.contains_rect(target);
        if clip {
            hayro_interpret::Device::push_clip_rect(&mut canvas, &crop);
        }
        interpret_page(p, &mut context, &mut canvas);
        if clip {
            hayro_interpret::Device::pop_clip(&mut canvas);
        }
        let pix = canvas.finish();
        // Opaque white under everything: the premultiplied RGBA is the RGB.
        let mut rgb = Vec::new();
        rgb.try_reserve_exact(w as usize * h as usize * 3)
            .map_err(|_| Error::TooBig)?;
        for p in pix.data().as_chunks::<4>().0 {
            rgb.extend_from_slice(&p[..3]);
        }
        let partial = partial.get() || warnings.load(Ordering::Relaxed) > 0;
        Ok(Drawn {
            px: Pixels {
                w,
                h,
                ch: 3,
                px: rgb,
            },
            partial,
        })
    }
}

/// `%PDF-` in the first kilobyte.
pub fn is_pdf(head: &[u8]) -> bool {
    head[..head.len().min(1024)]
        .windows(5)
        .any(|w| w == b"%PDF-")
}

/// A PDF text string: UTF-16BE or UTF-8 with their byte order marks, otherwise PDFDocEncoding (Latin-1 for the
/// letters; its few differences are punctuation).
fn text(b: &[u8]) -> String {
    if let Some(rest) = b.strip_prefix(&[0xfe, 0xff]) {
        let units = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes([c[0], c[1]]));
        char::decode_utf16(units)
            .map(|c| c.unwrap_or('\u{fffd}'))
            .collect()
    } else if let Some(rest) = b.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        b.iter().map(|&c| char::from(c)).collect()
    }
}

/// The catalog's named destinations: the old `/Dests` dictionary and the `/Names /Dests` name tree (flattened, at
/// most a few thousand names).
fn named_dests<'a>(catalog: &Dict<'a>) -> Vec<(Vec<u8>, Object<'a>)> {
    let mut out = Vec::new();
    if let Some(d) = catalog.get::<Dict<'a>>(keys::DESTS) {
        for (k, v) in d.entries() {
            if let Some(v) = resolve(&d, &k, v) {
                out.push((k.as_ref().to_vec(), v));
            }
        }
    }
    if let Some(tree) = catalog
        .get::<Dict<'a>>(keys::NAMES)
        .and_then(|n| n.get::<Dict<'a>>(keys::DESTS))
    {
        let mut stack = Vec::from([tree]);
        let mut nodes = 0;
        while let Some(node) = stack.pop() {
            nodes += 1;
            if nodes > 512 || out.len() > 4096 {
                break;
            }
            if let Some(names) = node.get::<Array<'a>>(keys::NAMES) {
                let mut it = names.iter::<Object<'a>>();
                while let (Some(k), Some(v)) = (it.next(), it.next()) {
                    if let Object::String(k) = k {
                        out.push((k.as_bytes().to_vec(), v));
                    }
                }
            }
            if let Some(kids) = node.get::<Array<'a>>(keys::KIDS) {
                stack.extend(kids.iter::<Dict<'a>>());
            }
        }
    }
    out
}

fn resolve<'a>(d: &Dict<'a>, k: &Name<'_>, _v: MaybeRef<Object<'a>>) -> Option<Object<'a>> {
    d.get::<Object<'a>>(k.as_ref())
}

/// The page an outline item opens: its `/Dest`, or its `/A` GoTo action's `/D`; an array `[page …]`, a name or a
/// string looked up in the named destinations, or a dictionary with `/D`.
fn dest_page(
    item: &Dict<'_>,
    pages: &[Option<ObjectIdentifier>],
    names: &[(Vec<u8>, Object<'_>)],
) -> Option<usize> {
    let dest = item.get::<Object<'_>>(keys::DEST).or_else(|| {
        item.get::<Dict<'_>>(keys::A)
            .and_then(|a| a.get::<Object<'_>>(keys::D))
    })?;
    let mut dest = dest;
    for _ in 0..4 {
        dest = match dest {
            Object::Array(a) => {
                let first = a.raw_iter().next()?;
                return match first {
                    MaybeRef::Ref(r) => pages.iter().position(|p| *p == Some(r.into())),
                    MaybeRef::NotRef(Object::Number(n)) => {
                        // Remote-style destinations name the page by number.
                        let n = n.as_f64();
                        (n >= 0.0 && (n as usize) < pages.len()).then_some(n as usize)
                    }
                    _ => None,
                };
            }
            Object::Name(n) => names
                .iter()
                .find(|(k, _)| k.as_slice() == n.as_ref())?
                .1
                .clone(),
            Object::String(s) => names
                .iter()
                .find(|(k, _)| k.as_slice() == s.as_bytes())?
                .1
                .clone(),
            Object::Dict(d) => d.get::<Object<'_>>(keys::D)?,
            _ => return None,
        };
    }
    None
}
