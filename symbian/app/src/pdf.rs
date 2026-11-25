// SPDX-License-Identifier: GPL-3.0-only
//! FA11 PDFs (files2-view-pdf): a worker thread reads the whole file into a memory chunk of its own (at most 48 MB:
//! never the heap, which only grows), opens it with neo-format-document's PDF engine and draws what the view wants:
//! whole pages at the screen's width near the view, while zoomed one sharp tile of the view, and the strip's
//! thumbnails. The engine's document lives on the worker (hayro shares its parts with `Rc`); the UI thread lays the
//! pages out with files-ui's PdfViewer and shows the pictures as they come. The place (page, zoom) of each PDF is
//! kept in `C:\private\e0006001\pdfs.dat`.

use crate::fa7::Open7;
use crate::{FilesApp, State};
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use e32::chunk::Chunk;
use e32::dprintln;
use e32::sync::{Mutex, Semaphore};
use files_ops::store;
use files_ui::image::slint_image;
use files_ui::pdf::{Kind, PdfViewer, Pictures, Tile, Want, ZOOM_MAX, info, show_pdf};
use fs_e32::E32Fs;
use neo_format_document::Error;
use neo_format_document::pdf::{Entry, Pdf};
use neo_format_image::Pixels;
use neo_fs::{Fs as _, Open as FsOpen};
use neoapp::App;
use neoui_proto::TouchPhase;
use slint::platform::WindowEvent;
use slint::{ComponentHandle, Image, ModelRc, VecModel};

/// Where each PDF was left: `path \t size \t modified \t page \t zoom %`, newest first.
const PLACES: &str = "C:\\private\\e0006001\\pdfs.dat";
const PLACES_MAX: usize = 200;
/// The worker's stack: hayro interprets nested forms, patterns and Type3 glyphs recursively (it stops at 50 deep).
const STACK: i32 = 1024 * 1024;
/// The largest PDF read: it is held whole in a chunk while open.
pub(crate) const FILE_MAX: u64 = 48 << 20;
/// How often the UI looks for drawn pictures while the worker draws.
pub(crate) const POLL_US: i32 = 60_000;
/// A drawing this long means the worker died (a panic in a hostile file kills only its thread).
/// shortcut: a stuck-or-dead guess by time, until e32 can poll a thread's logon without blocking.
const STUCK_MS: u64 = 60_000;

pub(crate) struct Open {
    v: Option<PdfViewer>,
    path: String,
    size: u64,
    modified: i64,
    /// The document's number: pictures and answers of an older one are dropped.
    n: u32,
    pages: BTreeMap<usize, Image>,
    tile: Option<Tile>,
    thumbs: BTreeMap<usize, Image>,
    outline: Vec<Entry>,
    contents: bool,
    /// "" none, "password" or "page".
    ask: &'static str,
    /// A password was tried (a wrong one asks again, saying so).
    tried: bool,
    /// Where to go when the pages are known (the saved place).
    resume: Option<(usize, f32)>,
    fingers: [Option<(f32, f32)>; 2],
    /// A pinch: the fingers' first distance, the zoom then.
    pinch: Option<(f32, f32)>,
    swallow: bool,
}

pub(crate) enum Act {
    Resized(i32, i32),
    Scrolled(i32),
    ScrolledX(i32),
    Thumb(usize),
    Pill,
    Contents(usize),
    ContentsClosed,
    AskDone(String),
    AskCancelled,
}

pub(crate) fn bind(ui: &FilesApp, q: &Rc<RefCell<VecDeque<crate::Action>>>) {
    let push = |q: &Rc<RefCell<VecDeque<crate::Action>>>| {
        let q = q.clone();
        move |a: Act| q.borrow_mut().push_back(crate::Action::Pdf(a))
    };
    let p = push(q);
    ui.on_pdf_resized(move |w, h| p(Act::Resized(w, h)));
    let p = push(q);
    ui.on_pdf_scrolled(move |px| p(Act::Scrolled(px)));
    let p = push(q);
    ui.on_pdf_scrolled_x(move |px| p(Act::ScrolledX(px)));
    let p = push(q);
    ui.on_pdf_thumb_picked(move |i| p(Act::Thumb(i.max(0) as usize)));
    let p = push(q);
    ui.on_pdf_pill_tapped(move || p(Act::Pill));
    let p = push(q);
    ui.on_pdf_contents_chosen(move |i| p(Act::Contents(i.max(0) as usize)));
    let p = push(q);
    ui.on_pdf_contents_closed(move || p(Act::ContentsClosed));
    let p = push(q);
    ui.on_pdf_ask_done(move |s| p(Act::AskDone(s.to_string())));
    let p = push(q);
    ui.on_pdf_ask_cancelled(move || p(Act::AskCancelled));
}

pub(crate) fn is_pdf(name: &str) -> bool {
    neo_view::ext_of(name).eq_ignore_ascii_case("pdf")
}

/// The pictures in hand, for files-ui.
struct Pics<'a>(&'a Open);

impl Pictures for Pics<'_> {
    fn page(&self, page: usize) -> Option<Image> {
        self.0.pages.get(&page).cloned()
    }
    fn tile(&self) -> Option<Tile> {
        self.0.tile.clone()
    }
    fn thumb(&self, page: usize) -> Option<Image> {
        self.0.thumbs.get(&page).cloned()
    }
}

pub(crate) fn open(
    _app: &App,
    ui: &FilesApp,
    st: &mut State,
    o: &Open7,
) -> Result<&'static str, String> {
    if o.size > FILE_MAX {
        return Err(String::from("It is too big to open (over 48 MB)"));
    }
    if st.pdfw.is_none() {
        st.pdfw = Worker::new().ok();
    }
    let w = st
        .pdfw
        .ok_or_else(|| String::from("No memory to open it"))?;
    // the viewer's area is measured with the viewer showing
    ui.set_view("pdf".into());
    let modified = st.fs.entry(&o.path).map(|e| e.modified).unwrap_or(0);
    st.doc_n = st.doc_n.wrapping_add(1);
    let n = st.doc_n;
    let resume = place(st, &o.path, o.size, modified);
    st.pdf = Some(Open {
        v: None,
        path: o.path.clone(),
        size: o.size,
        modified,
        n,
        pages: BTreeMap::new(),
        tile: None,
        thumbs: BTreeMap::new(),
        outline: Vec::new(),
        contents: false,
        ask: "",
        tried: false,
        resume,
        fingers: [None; 2],
        pinch: None,
        swallow: false,
    });
    w.open(n, o.path.clone(), String::new());
    ui.set_pdf_ask("".into());
    ui.set_pdf_ask_error("".into());
    ui.set_pdf_contents_open(false);
    ui.set_pdf_pages(ModelRc::default());
    ui.set_pdf_thumbs(ModelRc::default());
    ui.set_pdf_tile_on(false);
    ui.set_pdf_pill("".into());
    ui.set_pdf_info("Opening…".into());
    Ok("pdf")
}

/// The view's properties from the state.
fn show(ui: &FilesApp, p: &Open) {
    if let Some(v) = &p.v {
        show_pdf(ui, v, &Pics(p), &info(p.size, v.pages()));
    }
    ui.set_pdf_contents_open(p.contents);
    ui.set_pdf_ask(p.ask.into());
}

/// Asks the worker for the pictures the view wants that aren't in hand; forgets those far from the view.
fn request(st: &mut State) {
    let Some(p) = st.pdf.as_mut() else { return };
    let (Some(v), Some(w)) = (p.v.as_ref(), st.pdfw) else {
        return;
    };
    let wants = v.wanted();
    let near: Vec<usize> = wants
        .iter()
        .filter(|w| w.kind == Kind::Page)
        .map(|w| w.page)
        .collect();
    let thumbs: Vec<usize> = wants
        .iter()
        .filter(|w| w.kind == Kind::Thumb)
        .map(|w| w.page)
        .collect();
    p.pages.retain(|k, _| near.contains(k));
    p.thumbs.retain(|k, _| thumbs.contains(k));
    if p.tile
        .as_ref()
        .is_some_and(|(page, rect, _)| v.zoom <= 1.0 || v.tile_rect(*page) != Some(*rect))
    {
        p.tile = None;
    }
    let missing: Vec<Want> = wants
        .into_iter()
        .filter(|w| match w.kind {
            Kind::Page => !p.pages.contains_key(&w.page),
            Kind::Thumb => !p.thumbs.contains_key(&w.page),
            Kind::Tile => p
                .tile
                .as_ref()
                .is_none_or(|(page, rect, _)| (*page, *rect) != (w.page, w.rect)),
        })
        .collect();
    w.want(p.n, missing);
}

/// The worker's answers: the document opened (or why not), pictures drawn.
pub(crate) fn poll(app: &App, ui: &FilesApp, st: &mut State) {
    let Some(w) = st.pdfw else { return };
    let (done, stuck) = w.take();
    let mut any = false;
    for d in done {
        let Some(p) = st.pdf.as_mut() else { continue };
        match d {
            Done::Opened(n, _) | Done::Drawn(n, ..) if n != p.n => {}
            Done::Opened(_, Ok(o)) => {
                let dp = ui.global::<files_ui::Theme>().get_dp();
                let mut v = PdfViewer::new(o.sizes, dp);
                if let Some((page, zoom)) = p.resume.take() {
                    v.zoom = zoom.clamp(1.0, ZOOM_MAX);
                    v.layout(ui.get_pdf_view_w(), ui.get_pdf_view_h());
                    v.scroll(v.top_of(page.min(v.pages() - 1)), 0);
                } else {
                    v.layout(ui.get_pdf_view_w(), ui.get_pdf_view_h());
                }
                dprintln!("files: pdf {} pages, opened in {} ms", v.pages(), o.ms);
                let (top, left) = (v.top, v.left);
                p.v = Some(v);
                p.outline = o.outline;
                p.ask = "";
                let titles: Vec<slint::SharedString> =
                    p.outline.iter().map(|e| e.title.as_str().into()).collect();
                let depth: Vec<i32> = p.outline.iter().map(|e| e.depth as i32).collect();
                ui.set_pdf_contents(ModelRc::new(VecModel::from(titles)));
                ui.set_pdf_contents_depth(ModelRc::new(VecModel::from(depth)));
                ui.set_pdf_ask_error("".into());
                show(ui, p);
                ui.invoke_pdf_scroll_to(top);
                ui.invoke_pdf_scroll_x_to(left);
                crate::fa7::refresh_menu(app, ui, st);
                any = true;
            }
            Done::Opened(_, Err(Error::Password)) => {
                p.ask = "password";
                ui.set_pdf_ask_text("".into());
                ui.set_pdf_ask_error(if p.tried { "Wrong password" } else { "" }.into());
                ui.set_pdf_info("Protected".into());
                show(ui, p);
            }
            Done::Opened(_, Err(e)) => {
                close(st);
                crate::fa7::hex_because(app, ui, st, e.reason());
                return;
            }
            Done::Drawn(_, want, px) => {
                let image = slint_image(&px);
                match want.kind {
                    Kind::Page => {
                        p.pages.insert(want.page, image);
                    }
                    Kind::Thumb => {
                        p.thumbs.insert(want.page, image);
                    }
                    Kind::Tile => p.tile = Some((want.page, want.rect, image)),
                }
                any = true;
            }
        }
    }
    if stuck {
        // a slow worker is told to stop; a dead one is forgotten either way: the next PDF starts a new one
        dprintln!("files: pdf worker stopped answering");
        if let Some(w) = st.pdfw.take() {
            w.close();
        }
        if st.pdf.is_none() {
            return;
        }
        close(st);
        crate::fa7::hex_because(app, ui, st, "It couldn't be drawn");
        return;
    }
    if any {
        request(st);
        if let Some(p) = st.pdf.as_ref() {
            show(ui, p);
        }
    }
}

pub(crate) fn act(app: &App, ui: &FilesApp, st: &mut State, a: Act) {
    let Some(p) = st.pdf.as_mut() else { return };
    let pages = p.v.as_ref().map_or(0, |v| v.pages());
    match a {
        Act::Resized(w, h) => {
            let Some(v) = p.v.as_mut() else { return };
            v.layout(w, h);
            // the pictures were drawn for the old width
            p.pages.clear();
            p.thumbs.clear();
            p.tile = None;
            let (top, left) = (v.top, v.left);
            request(st);
            if let Some(p) = st.pdf.as_ref() {
                show(ui, p);
            }
            ui.invoke_pdf_scroll_to(top);
            ui.invoke_pdf_scroll_x_to(left);
        }
        Act::Scrolled(px) | Act::ScrolledX(px) => {
            let Some(v) = p.v.as_mut() else { return };
            let before = (v.near(), v.current());
            let (top, left) = match a {
                Act::Scrolled(_) => (px, v.left),
                _ => (v.top, px),
            };
            let changed = v.scroll(top, left);
            let moved = changed || before.0 != v.near();
            if changed {
                save_place(st);
            }
            // zoomed, the tile is for one place of the view: a new one is drawn where it stops
            if moved
                || st
                    .pdf
                    .as_ref()
                    .is_some_and(|p| p.v.as_ref().is_some_and(|v| v.zoom > 1.0))
            {
                request(st);
                if let Some(p) = st.pdf.as_ref() {
                    show(ui, p);
                }
            }
        }
        Act::Thumb(i) | Act::Contents(i) => {
            let page = match a {
                Act::Contents(_) => {
                    p.contents = false;
                    p.outline.get(i).and_then(|e| e.page)
                }
                _ => Some(i),
            };
            if let (Some(page), Some(v)) = (page, p.v.as_ref()) {
                ui.invoke_pdf_scroll_to(v.top_of(page.min(pages.saturating_sub(1))));
            }
            show(ui, p);
        }
        Act::ContentsClosed => {
            p.contents = false;
            show(ui, p);
        }
        Act::Pill => {
            if p.v.is_some() {
                p.ask = "page";
                ui.set_pdf_ask_text("".into());
                ui.set_pdf_ask_error("".into());
                show(ui, p);
            }
        }
        Act::AskDone(text) => match p.ask {
            "password" => {
                p.tried = true;
                p.ask = "";
                ui.set_pdf_ask_error("".into());
                ui.set_pdf_info("Opening…".into());
                if let Some(w) = st.pdfw {
                    w.open(p.n, p.path.clone(), text);
                }
                show(ui, p);
            }
            "page" => match text.trim().parse::<usize>() {
                Ok(n) if (1..=pages).contains(&n) => {
                    p.ask = "";
                    if let Some(v) = p.v.as_ref() {
                        ui.invoke_pdf_scroll_to(v.top_of(n - 1));
                    }
                    show(ui, p);
                }
                _ => ui.set_pdf_ask_error(format!("A page from 1 to {pages}").into()),
            },
            _ => {}
        },
        Act::AskCancelled => {
            if p.ask == "password" && p.v.is_none() {
                p.ask = "";
                ui.set_pdf_ask("".into());
                // nothing to show without the password: the bytes, saying why
                close(st);
                crate::fa7::hex_because(app, ui, st, Error::Password.reason());
            } else {
                p.ask = "";
                show(ui, p);
            }
        }
    }
}

/// The ⋮ menu's PDF items: Contents (when there are bookmarks) and Go to page…
pub(crate) fn menu_items(st: &State) -> &'static [&'static str] {
    match st.pdf.as_ref() {
        Some(p) if p.v.is_some() && !p.outline.is_empty() => &["Contents", "Go to page…"],
        Some(p) if p.v.is_some() => &["Go to page…"],
        _ => &[],
    }
}

pub(crate) fn menu(ui: &FilesApp, st: &mut State, m: &str) -> bool {
    let Some(p) = st.pdf.as_mut().filter(|p| p.v.is_some()) else {
        return false;
    };
    match m {
        "Contents" => {
            p.contents = true;
            ui.set_pdf_contents_current(contents_current(p));
        }
        "Go to page…" => {
            p.ask = "page";
            ui.set_pdf_ask_text("".into());
            ui.set_pdf_ask_error("".into());
        }
        _ => return false,
    }
    show(ui, p);
    true
}

/// The outline entry for the current page: the last one at or before it (-1: none).
fn contents_current(p: &Open) -> i32 {
    let cur = p.v.as_ref().map_or(0, |v| v.current());
    p.outline
        .iter()
        .rposition(|e| e.page.is_some_and(|pg| pg <= cur))
        .map_or(-1, |i| i as i32)
}

/// The view's pictures let go (Back, the hex view): Slint would keep the last ones alive.
pub(crate) fn clear(ui: &FilesApp) {
    ui.set_pdf_pages(ModelRc::default());
    ui.set_pdf_thumbs(ModelRc::default());
    ui.set_pdf_tile_on(false);
    ui.set_pdf_tile(Default::default());
    ui.set_pdf_contents_open(false);
    ui.set_pdf_ask("".into());
}

/// Back: the Contents or an ask close first.
pub(crate) fn back(ui: &FilesApp, st: &mut State) -> bool {
    let Some(p) = st.pdf.as_mut() else {
        return false;
    };
    if p.contents || (p.ask == "page") {
        p.contents = false;
        p.ask = "";
        show(ui, p);
        return true;
    }
    false
}

pub(crate) fn close(st: &mut State) {
    if st.pdf.is_none() {
        return;
    }
    save_place(st);
    st.pdf = None;
    if let Some(w) = st.pdfw {
        w.close();
    }
}

/// Two fingers zoom (1× to 4×) about their middle: the pages stretch while they move, the sharp tile comes after.
pub(crate) fn touch(
    app: &App,
    ui: &FilesApp,
    st: &mut State,
    id: u8,
    x: i32,
    y: i32,
    phase: TouchPhase,
) -> bool {
    let Some(p) = st
        .pdf
        .as_mut()
        .filter(|p| p.v.is_some() && p.ask.is_empty() && !p.contents)
    else {
        return false;
    };
    let id = id as usize;
    if id > 1 {
        return true;
    }
    let at = (x as f32, y as f32);
    p.fingers[id] = if phase == TouchPhase::Up {
        None
    } else {
        Some(at)
    };
    let dist = |a: (f32, f32), b: (f32, f32)| {
        libm::sqrtf((a.0 - b.0) * (a.0 - b.0) + (a.1 - b.1) * (a.1 - b.1))
    };
    // the view starts under the 48 dp header (taken as the surface's top: close enough for a pinch's middle)
    let header = (48.0 * ui.global::<files_ui::Theme>().get_dp()) as i32;
    let zoom_to = |p: &mut Open| {
        if let (Some(a), Some(b), Some((d0, z0))) = (p.fingers[0], p.fingers[1], p.pinch)
            && let Some(v) = p.v.as_mut()
        {
            let z = (z0 * dist(a, b) / d0).clamp(1.0, ZOOM_MAX);
            let mid = (
                ((a.0 + b.0) / 2.0) as i32,
                ((a.1 + b.1) / 2.0) as i32 - header,
            );
            v.zoom_at(if z < 1.05 { 1.0 } else { z }, mid.0, mid.1);
        }
    };
    match (id, phase) {
        (1, TouchPhase::Down) => {
            if let (Some(f0), Some(v)) = (p.fingers[0], p.v.as_ref()) {
                p.pinch = Some((dist(f0, at).max(1.0), v.zoom));
                p.swallow = true;
                app.window.dispatch_event(WindowEvent::PointerExited);
            }
            true
        }
        (_, TouchPhase::Move) if p.pinch.is_some() => {
            zoom_to(p);
            if let Some(v) = p.v.as_ref() {
                let (top, left) = (v.top, v.left);
                show(ui, p);
                ui.invoke_pdf_scroll_to(top);
                ui.invoke_pdf_scroll_x_to(left);
            }
            true
        }
        (_, TouchPhase::Up) if p.pinch.is_some() => {
            // the lifted finger's last place counts: the zoom is read before it is forgotten
            p.fingers[id] = Some(at);
            zoom_to(p);
            p.fingers[id] = None;
            p.pinch = None;
            if id == 0 {
                p.swallow = false;
            }
            save_place(st);
            request(st);
            if let Some(p) = st.pdf.as_ref() {
                show(ui, p);
            }
            true
        }
        (_, TouchPhase::Up) if p.swallow => {
            if id == 0 {
                p.swallow = false;
            }
            true
        }
        _ => p.swallow,
    }
}

// ── the place in each PDF ────────────────────────────────────────────────────────────────────────────────────────

fn places(st: &State) -> Vec<String> {
    store::read(&*st.fs, PLACES)
        .ok()
        .map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn place(st: &State, path: &str, size: u64, modified: i64) -> Option<(usize, f32)> {
    let key = format!("{path}\t{size}\t{modified}\t");
    let line = places(st).into_iter().find(|l| l.starts_with(&key))?;
    let mut f = line[key.len()..].split('\t').map(|v| v.parse::<u32>().ok());
    let (page, zoom) = (f.next()??, f.next()??);
    Some((page as usize, zoom as f32 / 100.0))
}

fn save_place(st: &State) {
    let Some(p) = st.pdf.as_ref() else { return };
    let Some(v) = p.v.as_ref() else { return };
    let key = format!("{}\t{}\t{}\t", p.path, p.size, p.modified);
    let (page, zoom) = v.place();
    let mut lines = places(st);
    lines.retain(|l| !l.starts_with(&key));
    lines.insert(0, format!("{key}{page}\t{}", (zoom * 100.0) as u32));
    lines.truncate(PLACES_MAX);
    let mut out = lines.join("\n");
    out.push('\n');
    if let Err(e) = store::write(&*st.fs, PLACES, out.as_bytes()) {
        dprintln!("files: pdfs.dat {}", e.code());
    }
}

// ── the worker ───────────────────────────────────────────────────────────────────────────────────────────────────

/// What the worker found when it opened a PDF.
pub(crate) struct Opened {
    sizes: Vec<(f32, f32)>,
    outline: Vec<Entry>,
    ms: u64,
}

pub(crate) enum Done {
    Opened(u32, Result<Opened, Error>),
    Drawn(u32, Want, Pixels),
}

#[derive(Default)]
struct Queue {
    /// Open this (document number, path, password); a new one replaces the open document.
    open: Option<(u32, String, String)>,
    /// The open document's number and the pictures wanted of it, most needed first.
    n: u32,
    wants: VecDeque<Want>,
    close: bool,
    done: Vec<Done>,
    started: bool,
    /// When the worker took its current job (ms), while it works on it.
    since: Option<u64>,
    /// The picture being drawn (not asked for again while it is).
    doing: Option<Want>,
}

/// The file's bytes in a chunk of their own (closed when the document is).
struct Bytes(Chunk, usize);

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: the chunk holds `self.1` bytes written once by `read_all`, and lives as long as `self`.
        unsafe { core::slice::from_raw_parts(self.0.base(), self.1) }
    }
}

pub(crate) struct Worker {
    q: Mutex<Queue>,
    wake: Semaphore,
}

impl Worker {
    fn new() -> Result<&'static Worker, i32> {
        Ok(Box::leak(Box::new(Worker {
            q: Mutex::new(Queue::default())?,
            wake: Semaphore::new(0)?,
        })))
    }

    /// Starts the thread (once); false when it can't be.
    fn start(&'static self) -> bool {
        if core::mem::replace(&mut self.q.lock().started, true) {
            return true;
        }
        let r = e32::thread::spawn_with_stack(STACK, move || {
            let Ok(fs) = E32Fs::connect() else {
                self.fail(Error::Io(-1));
                return -1;
            };
            loop {
                // the job is taken in a statement of its own: the lock is free again while it is served
                let job = self.q.lock().open.take();
                match job {
                    Some((n, path, password)) => self.serve(&fs, n, &path, &password),
                    None => self.wake.wait(),
                }
            }
        });
        if r.is_err() {
            self.fail(Error::TooBig);
            return false;
        }
        true
    }

    /// No worker: the document waiting to open gets `e` (the UI shows the bytes), the next one starts a new thread.
    fn fail(&self, e: Error) {
        let mut q = self.q.lock();
        q.started = false;
        if let Some((n, ..)) = q.open.take() {
            q.done.push(Done::Opened(n, Err(e)));
        }
    }

    fn open(&'static self, n: u32, path: String, password: String) {
        {
            let mut q = self.q.lock();
            q.open = Some((n, path, password));
            q.n = n;
            q.wants.clear();
            q.close = false;
            q.done.clear();
        }
        if self.start() {
            self.wake.signal();
        }
    }

    fn want(&self, n: u32, wants: Vec<Want>) {
        let mut q = self.q.lock();
        if q.n != n {
            return;
        }
        let doing = q.doing;
        q.wants = wants.into_iter().filter(|w| Some(*w) != doing).collect();
        drop(q);
        self.wake.signal();
    }

    fn close(&self) {
        let mut q = self.q.lock();
        q.close = true;
        q.wants.clear();
        q.done.clear();
        drop(q);
        self.wake.signal();
    }

    /// The answers so far, and whether the worker has been on one job too long (it died).
    fn take(&self) -> (Vec<Done>, bool) {
        let mut q = self.q.lock();
        let stuck = q
            .since
            .is_some_and(|t| e32::time::millis().saturating_sub(t) > STUCK_MS);
        (core::mem::take(&mut q.done), stuck)
    }

    pub(crate) fn busy(&self) -> bool {
        let q = self.q.lock();
        q.since.is_some() || !q.done.is_empty() || !q.wants.is_empty() || q.open.is_some()
    }

    /// One document: opened, then pictures drawn as they are wanted until another is opened or it is closed.
    fn serve(&self, fs: &E32Fs, n: u32, path: &str, password: &str) {
        let t0 = e32::time::millis();
        self.q.lock().since = Some(t0);
        let pdf = read_all(fs, path).and_then(|b| Pdf::open(b, password));
        let pdf = match pdf {
            Ok(pdf) => pdf,
            Err(e) => {
                let mut q = self.q.lock();
                q.since = None;
                q.done.push(Done::Opened(n, Err(e)));
                return;
            }
        };
        let opened = Opened {
            sizes: (0..pdf.pages()).map(|i| pdf.size(i)).collect(),
            outline: pdf.outline(),
            ms: e32::time::millis() - t0,
        };
        {
            let mut q = self.q.lock();
            q.since = None;
            q.done.push(Done::Opened(n, Ok(opened)));
        }
        let mut r = pdf.renderer();
        loop {
            let want = {
                let mut q = self.q.lock();
                if q.open.is_some() || q.close {
                    q.close = false;
                    q.wants.clear();
                    return;
                }
                let w = q.wants.pop_front();
                q.since = w.map(|_| e32::time::millis());
                q.doing = w;
                w
            };
            let Some(want) = want else {
                self.wake.wait();
                continue;
            };
            let t = e32::time::millis();
            match r.render(want.page, want.scale, want.rect) {
                Ok(d) => {
                    if want.kind == Kind::Page {
                        dprintln!(
                            "files: pdf page {} {}x{} in {} ms{}",
                            want.page + 1,
                            want.rect.2,
                            want.rect.3,
                            e32::time::millis() - t,
                            if d.partial { " (partial)" } else { "" }
                        );
                    }
                    let mut q = self.q.lock();
                    q.since = None;
                    q.doing = None;
                    if q.n == n && !q.close {
                        q.done.push(Done::Drawn(n, want, d.px));
                    }
                }
                Err(e) => {
                    dprintln!("files: pdf page {} {}", want.page + 1, e.reason());
                    let mut q = self.q.lock();
                    q.since = None;
                    q.doing = None;
                }
            }
        }
    }
}

/// The whole file in a chunk of its own.
fn read_all(fs: &E32Fs, path: &str) -> Result<Bytes, Error> {
    let f = fs
        .open(path, FsOpen::Read)
        .map_err(|e| Error::Io(e.code()))?;
    let size = neo_fs::File::size(&f).map_err(|e| Error::Io(e.code()))?;
    if size > FILE_MAX {
        return Err(Error::TooBig);
    }
    let n = size as usize;
    let c = Chunk::create_global(n.max(1)).map_err(|_| Error::TooBig)?;
    // SAFETY: the chunk is `n` committed bytes, ours alone until `Bytes` hands them out read-only.
    let buf = unsafe { core::slice::from_raw_parts_mut(c.base(), n) };
    let mut at = 0;
    while at < n {
        let got = neo_fs::File::read_at(&f, at as u64, &mut buf[at..])
            .map_err(|e| Error::Io(e.code()))?;
        if got == 0 {
            break;
        }
        at += got;
    }
    Ok(Bytes(c, at))
}

/// `C:\Data\fa11\`: the PDF fixtures, each with a broken copy as FA7's and FA10's.
#[cfg(feature = "test-files")]
pub(crate) fn test_files(st: &State) {
    const DIR: &str = "C:\\Data\\fa11\\";
    if st.fs.entry(&format!("{DIR}boarding-pass.pdf")).is_ok() {
        return;
    }
    let files: [(&str, &[u8]); 9] = [
        (
            "boarding-pass.pdf",
            include_bytes!("../../../crates/neo-format-document/tests/data/boarding-pass.pdf"),
        ),
        (
            "book-100.pdf",
            include_bytes!("../../../crates/neo-format-document/tests/data/book-100.pdf"),
        ),
        (
            "locked.pdf",
            include_bytes!("../../../crates/neo-format-document/tests/data/locked.pdf"),
        ),
        (
            "quartz.pdf",
            include_bytes!("../../../crates/neo-format-document/tests/data/quartz.pdf"),
        ),
        (
            "deep.pdf",
            include_bytes!("../../../crates/neo-format-document/tests/data/deep.pdf"),
        ),
        (
            "loop.pdf",
            include_bytes!("../../../crates/neo-format-document/tests/data/loop.pdf"),
        ),
        (
            "broken-xref.pdf",
            include_bytes!("../../../crates/neo-format-document/tests/data/broken-xref.pdf"),
        ),
        (
            "tiling.pdf",
            include_bytes!("../../../crates/neo-format-document/tests/data/tiling.pdf"),
        ),
        (
            "masks.pdf",
            include_bytes!("../../../crates/neo-format-document/tests/data/masks.pdf"),
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
