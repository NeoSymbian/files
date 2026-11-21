// SPDX-License-Identifier: GPL-3.0-only
//! FA10 documents (files2-view-docx): a docx, odt or rtf read whole (it is small next to the heap), laid out by
//! files-ui's DocViewer at the advances Slint draws, in Page or Reflow mode, a window of it at a time.

use crate::audio::At;
use crate::fa7::Open7;
use crate::pics::DocPic;
use crate::{FilesApp, State};
use alloc::collections::VecDeque;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use e32::dprintln;
use files_ui::doc::{DocViewer, Fonts, Mode};
use neo_format_document::layout::Face;
use neo_format_document::{Doc, ImageSrc, Kind, Package};
use neo_fs::{Fs as _, Open as FsOpen};
use neoapp::App;
use neoui_proto::TouchPhase;
use slint::ComponentHandle;
use slint::platform::WindowEvent;

/// An rtf is read whole (its pictures are in its text); the zips are read a part at a time.
const RTF_MAX: u64 = 32 << 20;
/// A picture's header is looked for in this much of it (a JPEG's EXIF can push its size this far).
const HEAD: [usize; 2] = [64 << 10, 512 << 10];
/// The page view's zoom, at most.
const ZOOM_MAX: f32 = 4.0;

/// A pinch's start: the fingers' distance, the zoom, the midpoint and the scroll (left, top).
type Pinch = (f32, f32, (f32, f32), (i32, i32));
/// The pictures' own sizes (`None`: unknown until decoded).
pub(crate) type Sizes = Vec<Option<(i32, i32)>>;

/// A document's pictures in the window: decoded ones (with the size they were asked at), those asked and not back yet,
/// and those that can't be decoded. Pictures that leave the window are let go.
#[derive(Default)]
pub(crate) struct Pics {
    pics: Vec<Option<(slint::Image, u32)>>,
    pending: Vec<bool>,
    failed: Vec<bool>,
}

impl Pics {
    fn grow(&mut self, n: usize) {
        if self.pics.len() < n {
            self.pics.resize(n, None);
            self.pending.resize(n, false);
            self.failed.resize(n, false);
        }
    }

    /// The window wants `wanted` (id, drawn w, h): what to keep in the worker's queue, and what to ask (id, long side,
    /// at most the viewer's largest picture: a zoomed page doesn't decode a photo at full size).
    pub(crate) fn plan(&mut self, wanted: &[(usize, i32, i32)]) -> (Vec<usize>, Vec<(usize, u32)>) {
        self.grow(wanted.iter().map(|w| w.0 + 1).max().unwrap_or(0));
        for id in 0..self.pics.len() {
            if !wanted.iter().any(|w| w.0 == id) {
                self.pics[id] = None;
                self.pending[id] = false;
            }
        }
        let mut ask = Vec::new();
        for &(id, w, h) in wanted {
            let max = (w.max(h).max(1) as u32).min(crate::pics::SHOW_MAX);
            let fits = self.pics[id].as_ref().is_some_and(|p| p.1 * 5 >= max * 4);
            if fits || self.failed[id] || self.pending[id] {
                continue;
            }
            self.pending[id] = true;
            ask.push((id, max));
        }
        (wanted.iter().map(|w| w.0).collect(), ask)
    }

    /// A picture came back: kept (with the size the window wants now) when still wanted.
    pub(crate) fn arrived(
        &mut self,
        id: usize,
        px: Option<neo_format_image::Pixels>,
        wanted: &[(usize, i32, i32)],
    ) {
        self.grow(id + 1);
        self.pending[id] = false;
        match (px, wanted.iter().find(|w| w.0 == id)) {
            (Some(px), Some(&(_, w, h))) => {
                let max = (w.max(h).max(1) as u32).min(crate::pics::SHOW_MAX);
                self.pics[id] = Some((files_ui::image::slint_image(&px), max));
            }
            (Some(_), None) => {}
            (None, _) => self.failed[id] = true,
        }
    }

    pub(crate) fn image(&self, id: usize) -> Option<slint::Image> {
        self.pics
            .get(id)
            .and_then(|p| p.as_ref())
            .map(|p| p.0.clone())
    }
}

pub(crate) struct Open {
    v: DocViewer,
    /// The document's number: a picture decoded for an earlier one is dropped.
    n: u32,
    path: String,
    pics: Pics,
    info: String,
    /// The horizontal scroll (a zoomed page), px.
    left: i32,
    fingers: [Option<(f32, f32)>; 2],
    /// A pinch: the fingers' distance, the zoom, the midpoint and the scroll at its start.
    pinch: Option<Pinch>,
    swallow: bool,
}

pub(crate) enum Act {
    Resized(i32, i32),
    Mode(i32),
    Scrolled(i32),
    ScrolledX(i32),
}

pub(crate) fn bind(ui: &FilesApp, q: &Rc<RefCell<VecDeque<crate::Action>>>) {
    let push = |q: &Rc<RefCell<VecDeque<crate::Action>>>| {
        let q = q.clone();
        move |a: Act| q.borrow_mut().push_back(crate::Action::Doc(a))
    };
    let p = push(q);
    ui.on_doc_resized(move |w, h| p(Act::Resized(w, h)));
    let p = push(q);
    ui.on_doc_mode_picked(move |i| p(Act::Mode(i)));
    let p = push(q);
    ui.on_doc_scrolled(move |px| p(Act::Scrolled(px)));
    let p = push(q);
    ui.on_doc_scrolled_x(move |px| p(Act::ScrolledX(px)));
}

/// The documents this viewer reads, by name (the bytes are checked when it opens).
pub(crate) fn is_doc(name: &str) -> bool {
    matches!(
        Kind::of_ext(neo_view::ext_of(name)),
        Some(Kind::Docx | Kind::Odt | Kind::Rtf)
    )
}

/// The glyph advances, calibrated against Slint once per run.
pub(crate) fn fonts(ui: &FilesApp, st: &mut State) -> Fonts {
    *st.fonts.get_or_insert_with(|| {
        let f = Fonts::calibrate(|text, face: Face, px| {
            ui.set_probe_text(text.into());
            ui.set_probe_px(px);
            ui.set_probe_face(face as i32);
            ui.get_probe_width() as i32
        });
        if f.is_none() {
            dprintln!("files: doc fonts not calibrated");
        }
        f.unwrap_or(Fonts::guess())
    })
}

/// The document, read through an open file: a docx's or odt's parts one at a time, its pictures' sizes from their
/// first bytes (they are decoded later, on the worker, when they show).
fn read(st: &State, path: &str, size: u64) -> Result<(Doc, Sizes), String> {
    let io = |e: neo_fs::Error| files_core::error_text(e.code());
    let f = st.fs.open(path, FsOpen::Read).map_err(io)?;
    let file = At(f, size);
    let kind = neo_format_document::detect(&file)
        .filter(|k| matches!(k, Kind::Docx | Kind::Odt | Kind::Rtf))
        .ok_or_else(|| String::from("It isn't a document we read"))?;
    let reason = |e: neo_format_document::Error| String::from(e.reason());
    if kind == Kind::Rtf {
        if size > RTF_MAX {
            return Err(String::from(neo_format_document::Error::TooBig.reason()));
        }
        let doc = neo_format_document::read(kind, &file).map_err(reason)?;
        let sizes = doc
            .images
            .iter()
            .map(|src| match src {
                ImageSrc::Bytes(b) => neo_format_image::probe(b).map(|i| (i.w as i32, i.h as i32)),
                ImageSrc::Member(_) => None,
            })
            .collect();
        return Ok((doc, sizes));
    }
    let p = Package::open(file).map_err(reason)?;
    let doc = match kind {
        Kind::Docx => neo_format_document::docx::read(&p),
        _ => neo_format_document::odt::read(&p),
    }
    .map_err(reason)?;
    let sizes = doc
        .images
        .iter()
        .map(|src| {
            let ImageSrc::Member(m) = src else {
                return None;
            };
            HEAD.iter().find_map(|&n| {
                let head = p.head(m, n).ok()?;
                neo_format_image::probe(&head).map(|i| (i.w as i32, i.h as i32))
            })
        })
        .collect();
    Ok((doc, sizes))
}

pub(crate) fn open(ui: &FilesApp, st: &mut State, o: &Open7) -> Result<&'static str, String> {
    // the viewer's area is measured with the viewer showing (the folder's footer tabs make it shorter)
    ui.set_view("doc".into());
    let (doc, sizes) = read(st, &o.path, o.size)?;
    let fonts = fonts(ui, st);
    let dp = ui.global::<files_ui::Theme>().get_dp();
    let mut v = DocViewer::new(doc, fonts, dp, Mode::Reflow);
    for (i, s) in sizes.into_iter().enumerate() {
        v.set_size(i, s);
    }
    v.layout(ui.get_doc_view_w(), ui.get_doc_view_h());
    v.scroll(0);
    dprintln!(
        "files: doc {} blocks, {} pictures, {} px",
        v.doc.blocks.len(),
        v.doc.images.len(),
        v.total()
    );
    st.doc_n = st.doc_n.wrapping_add(1);
    let d = Open {
        v,
        n: st.doc_n,
        path: o.path.clone(),
        pics: Pics::default(),
        info: files_ui::doc::info(neo_view::family_of(&o.name), o.size),
        left: 0,
        fingers: [None; 2],
        pinch: None,
        swallow: false,
    };
    st.doc = Some(d);
    refresh(ui, st);
    ui.invoke_doc_scroll_to(0);
    ui.invoke_doc_scroll_x_to(0);
    Ok("doc")
}

/// The window's models, and the pictures it wants asked of the worker (those that left it are let go).
fn refresh(ui: &FilesApp, st: &mut State) {
    let Some(d) = st.doc.as_mut() else { return };
    let (keep, ask) = d.pics.plan(&d.v.wanted());
    let ask = ask
        .into_iter()
        .map(|(id, max)| {
            let (member, bytes) = match &d.v.doc.images[id] {
                ImageSrc::Member(m) => (Some(m.clone()), None),
                // shortcut: an rtf's picture is copied for the worker (once per decode); rtf files with big photos are
                // rare, keep the bytes in the worker if they aren't
                ImageSrc::Bytes(b) => (None, Some(b.clone())),
            };
            DocPic {
                id,
                path: d.path.clone(),
                member,
                bytes,
                max,
            }
        })
        .collect();
    st.pics.doc(d.n, &keep, ask);
    let pics = &d.pics;
    files_ui::doc::show(ui, &d.v, &d.info, |id| pics.image(id));
}

/// A picture from the worker.
pub(crate) fn picture(
    ui: &FilesApp,
    st: &mut State,
    n: u32,
    id: usize,
    px: Option<neo_format_image::Pixels>,
) {
    let Some(px) = crate::book::picture(ui, st, n, id, px) else {
        return;
    };
    let Some(d) = st.doc.as_mut().filter(|d| d.n == n) else {
        return;
    };
    d.pics.arrived(id, px, &d.v.wanted());
    let pics = &d.pics;
    files_ui::doc::show(ui, &d.v, &d.info, |id| pics.image(id));
}

pub(crate) fn act(ui: &FilesApp, st: &mut State, a: Act) {
    let Some(d) = st.doc.as_mut() else { return };
    match a {
        Act::Resized(w, h) => {
            // the same share of the document stays in view
            let at = d.v.top as f32 / d.v.total().max(1) as f32;
            d.v.layout(w, h);
            let top = (at * d.v.total() as f32) as i32;
            d.v.scroll(top);
            refresh(ui, st);
            ui.invoke_doc_scroll_to(top);
        }
        Act::Mode(m) => {
            let mode = if m == 0 { Mode::Page } else { Mode::Reflow };
            if mode == d.v.mode {
                return;
            }
            // keep the same share of the document in view
            let at = d.v.top as f32 / d.v.total().max(1) as f32;
            d.v.mode = mode;
            d.v.zoom = 1.0;
            d.v.layout(ui.get_doc_view_w(), ui.get_doc_view_h());
            let top = (at * d.v.total() as f32) as i32;
            d.v.scroll(top);
            d.left = 0;
            refresh(ui, st);
            ui.invoke_doc_scroll_to(top);
            ui.invoke_doc_scroll_x_to(0);
        }
        Act::Scrolled(px) => {
            if d.v.scroll(px) {
                refresh(ui, st);
            } else {
                ui.set_doc_percent(d.v.percent().into());
            }
        }
        Act::ScrolledX(px) => d.left = px.max(0),
    }
}

/// A touch while a document shows in Page mode: a second finger makes a pinch (Slint sees one pointer, so its press
/// is cancelled and the fingers are read here, as the image viewer does). The pages are laid out again at the new
/// zoom when the fingers lift; meanwhile the footer says the zoom. True when the event is used up.
pub(crate) fn touch(
    app: &App,
    ui: &FilesApp,
    st: &mut State,
    id: u8,
    x: i32,
    y: i32,
    phase: TouchPhase,
) -> bool {
    let Some(d) = st.doc.as_mut().filter(|d| d.v.mode == Mode::Page) else {
        return false;
    };
    let id = id as usize;
    if id > 1 {
        return true;
    }
    let p = (x as f32, y as f32);
    d.fingers[id] = if phase == TouchPhase::Up {
        None
    } else {
        Some(p)
    };
    let dist = |a: (f32, f32), b: (f32, f32)| {
        libm::sqrtf((a.0 - b.0) * (a.0 - b.0) + (a.1 - b.1) * (a.1 - b.1))
    };
    let zoom_now = |d: &Open| match (d.fingers[0], d.fingers[1], d.pinch) {
        (Some(a), Some(b), Some((d0, z0, ..))) => (z0 * dist(a, b) / d0).clamp(1.0, ZOOM_MAX),
        _ => d.v.zoom,
    };
    match (id, phase) {
        (1, TouchPhase::Down) => {
            if let Some(f0) = d.fingers[0] {
                let mid = ((f0.0 + p.0) / 2.0, (f0.1 + p.1) / 2.0);
                d.pinch = Some((dist(f0, p).max(1.0), d.v.zoom, mid, (d.left, d.v.top)));
                d.swallow = true;
                app.window.dispatch_event(WindowEvent::PointerExited);
            }
            true
        }
        (_, TouchPhase::Move) if d.pinch.is_some() => {
            ui.set_doc_percent(format!("Zoom {} %", (zoom_now(d) * 100.0) as i32).into());
            true
        }
        (_, TouchPhase::Up) if d.pinch.is_some() => {
            // the second finger's last place is its up: the zoom is read before it is forgotten
            d.fingers[id] = Some(p);
            let z = zoom_now(d);
            d.fingers[id] = None;
            let Some((_, z0, mid, (left0, top0))) = d.pinch.take() else {
                return true;
            };
            let k = z / z0;
            d.v.zoom = if z < 1.05 { 1.0 } else { z };
            d.v.layout(ui.get_doc_view_w(), ui.get_doc_view_h());
            // the point under the fingers stays under them (screen and view origins taken as one: close enough)
            let top = ((top0 as f32 + mid.1) * k - mid.1) as i32;
            let left = ((left0 as f32 + mid.0) * k - mid.0) as i32;
            d.v.scroll(top.max(0));
            d.left = left.max(0);
            if id == 0 {
                d.swallow = false;
            }
            refresh(ui, st);
            ui.invoke_doc_scroll_to(top);
            ui.invoke_doc_scroll_x_to(left);
            true
        }
        (_, TouchPhase::Up) if d.swallow => {
            if id == 0 {
                d.swallow = false;
            }
            true
        }
        (1, _) => true,
        (0, _) => d.swallow,
        _ => false,
    }
}

pub(crate) fn close(st: &mut State) {
    if st.doc.take().is_some() {
        st.pics.doc(0, &[], Vec::new());
    }
}

/// Every FA10 viewer closed (another view takes over: Back, the hex view).
pub(crate) fn close_all(st: &mut State) {
    close(st);
    crate::book::close(st);
    crate::sheet::close(st);
}

/// `C:\Data\fa10\`: the plan as docx, odt and rtf (and textutil's copies), each with a broken copy as FA7's.
#[cfg(feature = "test-files")]
pub(crate) fn test_files(st: &State) {
    use alloc::format;
    use files_ops::store;
    use neo_fs::Fs as _;
    const DIR: &str = "C:\\Data\\fa10\\";
    if st.fs.entry(&format!("{DIR}plan-hy.docx")).is_ok() {
        return;
    }
    let files: [(&str, &[u8]); 9] = [
        (
            "plan-hy.docx",
            include_bytes!("../../../crates/neo-format-document/tests/data/plan-hy.docx"),
        ),
        (
            "plan-hy.odt",
            include_bytes!("../../../crates/neo-format-document/tests/data/plan-hy.odt"),
        ),
        (
            "plan-hy.rtf",
            include_bytes!("../../../crates/neo-format-document/tests/data/plan-hy.rtf"),
        ),
        (
            "tu-plan.docx",
            include_bytes!("../../../crates/neo-format-document/tests/data/tu-plan.docx"),
        ),
        (
            "tu-plan.odt",
            include_bytes!("../../../crates/neo-format-document/tests/data/tu-plan.odt"),
        ),
        (
            "book-300.epub",
            include_bytes!("../../../crates/neo-format-document/tests/data/book-300.epub"),
        ),
        (
            "drm.epub",
            include_bytes!("../../../crates/neo-format-document/tests/data/drm.epub"),
        ),
        (
            "Бюджет 2026.xlsx",
            include_bytes!("../../../crates/neo-format-document/tests/data/budget-ru.xlsx"),
        ),
        (
            "tu-plan.rtf",
            include_bytes!("../../../crates/neo-format-document/tests/data/tu-plan.rtf"),
        ),
    ];
    for (name, bytes) in files {
        let _ = store::write(&*st.fs, &format!("{DIR}{name}"), bytes);
        let mut bad = bytes[..bytes.len() * 2 / 3].to_vec();
        for i in (7..bad.len()).step_by(97) {
            bad[i] ^= 0x5a;
        }
        let _ = store::write(&*st.fs, &format!("{DIR}broken-{name}"), &bad);
    }
}
