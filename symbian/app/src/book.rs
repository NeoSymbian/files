// SPDX-License-Identifier: GPL-3.0-only
//! FA10 books (files2-view-epub): an EPUB read a chapter at a time through the open file, laid out into pages by
//! files-ui's BookReader; a counting worker lays every chapter out at the same size to number the pages; the place
//! in each book is kept in `C:\private\e0006001\books.dat`, the text size and theme in `files.reader.*`.

use crate::audio::At;
use crate::fa7::Open7;
use crate::pics::DocPic;
use crate::{FilesApp, State};
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use e32::dprintln;
use e32::sync::{Mutex, Semaphore};
use files_ops::store;
use files_ui::doc::{BookReader, Fonts, SIZES, THEMES, Turn, book_opts, count_pages};
use fs_e32::E32Fs;
use neo_format_archive::Member;
use neo_format_document::epub::{self, Book};
use neo_format_document::layout::Pos;
use neo_format_document::{Doc, ImageSrc, Package};
use neo_fs::{Fs as _, Open as FsOpen};
use neoapp::App;
use neoui_proto::settings::Value;
use slint::ComponentHandle;

/// Where each book was left: `path \t size \t modified \t chapter \t block \t char`, newest first.
const BOOKS: &str = "C:\\private\\e0006001\\books.dat";
const BOOKS_MAX: usize = 200;
const SIZE_KEY: &str = "files.reader.size";
const THEME_KEY: &str = "files.reader.theme";
/// The counting worker's stack (the XML and the layout).
const STACK: i32 = 256 * 1024;
/// How often the UI looks for counted chapters while the worker counts.
pub(crate) const POLL_US: i32 = 300_000;

pub(crate) struct Open {
    r: BookReader,
    book: Book,
    members: Vec<Member>,
    path: String,
    size: u64,
    modified: i64,
    n: u32,
    /// The counting job the page counts come from.
    counted: u32,
    /// The pictures of the chapter in hand (decoded at their drawn size), and those that can't be.
    pics: crate::doc::Pics,
    controls: bool,
    contents: bool,
}

pub(crate) enum Act {
    Resized(i32, i32),
    Turn(i32),
    ToggleControls,
    Control(usize),
    Contents(usize),
    ContentsClosed,
}

pub(crate) fn bind(ui: &FilesApp, q: &Rc<RefCell<VecDeque<crate::Action>>>) {
    let push = |q: &Rc<RefCell<VecDeque<crate::Action>>>| {
        let q = q.clone();
        move |a: Act| q.borrow_mut().push_back(crate::Action::Book(a))
    };
    let p = push(q);
    ui.on_book_resized(move |w, h| p(Act::Resized(w, h)));
    let p = push(q);
    ui.on_book_turn(move |d| p(Act::Turn(d)));
    let p = push(q);
    ui.on_book_toggle_controls(move || p(Act::ToggleControls));
    let p = push(q);
    ui.on_book_control(move |i| p(Act::Control(i.max(0) as usize)));
    let p = push(q);
    ui.on_book_contents_chosen(move |i| p(Act::Contents(i.max(0) as usize)));
    let p = push(q);
    ui.on_book_contents_closed(move || p(Act::ContentsClosed));
}

pub(crate) fn is_book(name: &str) -> bool {
    neo_format_document::Kind::of_ext(neo_view::ext_of(name))
        == Some(neo_format_document::Kind::Epub)
}

fn package<'a>(
    fs: &'a E32Fs,
    path: &str,
    members: Option<Vec<Member>>,
) -> Result<Package<At<fs_e32::E32File<'a>>>, String> {
    let io = |e: neo_fs::Error| files_core::error_text(e.code());
    let f = fs.open(path, FsOpen::Read).map_err(io)?;
    let size = neo_fs::File::size(&f).map_err(io)?;
    let file = At(f, size);
    match members {
        Some(m) => Ok(Package::with(file, m)),
        None => Package::open(file).map_err(|e| String::from(e.reason())),
    }
}

/// A chapter and its pictures' sizes (from their first bytes).
fn chapter<R: neo_format_archive::ReadAt>(
    p: &Package<R>,
    book: &Book,
    i: usize,
) -> Result<(Doc, crate::doc::Sizes), String> {
    let doc = epub::chapter(p, book, i).map_err(|e| String::from(e.reason()))?;
    let sizes = doc
        .images
        .iter()
        .map(|src| {
            let ImageSrc::Member(m) = src else {
                return None;
            };
            [64usize << 10, 512 << 10].iter().find_map(|&n| {
                neo_format_image::probe(&p.head(m, n).ok()?).map(|i| (i.w as i32, i.h as i32))
            })
        })
        .collect();
    Ok((doc, sizes))
}

pub(crate) fn open(
    app: &App,
    ui: &FilesApp,
    st: &mut State,
    o: &Open7,
) -> Result<&'static str, String> {
    // the viewer's area is measured with the viewer showing (the folder's footer tabs make it shorter)
    ui.set_view("book".into());
    let p = package(&st.fs, &o.path, None)?;
    let book = epub::open(&p).map_err(|e| String::from(e.reason()))?;
    let modified = st.fs.entry(&o.path).map(|e| e.modified).unwrap_or(0);
    let (chapter_i, at) = place(st, &o.path, o.size, modified).unwrap_or((0, Pos::default()));
    let chapter_i = chapter_i.min(book.spine.len() - 1);
    let (chapter_i, at, (doc, sizes)) = match chapter(&p, &book, chapter_i) {
        Ok(c) => (chapter_i, at, c),
        Err(_) if chapter_i > 0 => (0, Pos::default(), chapter(&p, &book, 0)?),
        Err(e) => return Err(e),
    };
    let members = p.members.clone();
    drop(p);
    let fonts = crate::doc::fonts(ui, st);
    let dp = ui.global::<files_ui::Theme>().get_dp();
    let mut r = BookReader::new(
        book.title.clone(),
        book.toc.clone(),
        book.spine.len(),
        fonts,
        dp,
    );
    r.size = setting(app, SIZE_KEY, files_ui::doc::BOOK_SIZE, SIZES.len());
    r.theme = setting(app, THEME_KEY, 0, THEMES.len());
    r.layout(ui.get_book_page_w(), ui.get_book_page_h());
    dprintln!(
        "files: book page {}x{} px, dp {}, window {}",
        ui.get_book_page_w(),
        ui.get_book_page_h(),
        dp,
        ui.window().size().height
    );
    r.set_chapter(chapter_i, doc, sizes, Some(at), false);
    st.doc_n = st.doc_n.wrapping_add(1);
    let b = Open {
        r,
        book,
        members,
        path: o.path.clone(),
        size: o.size,
        modified,
        n: st.doc_n,
        counted: 0,
        pics: crate::doc::Pics::default(),
        controls: false,
        contents: false,
    };
    dprintln!(
        "files: book {} chapters, at {} {:?}",
        b.book.spine.len(),
        chapter_i,
        at
    );
    st.book = Some(b);
    count(st);
    refresh(ui, st);
    Ok("book")
}

fn setting(app: &App, key: &str, default: usize, n: usize) -> usize {
    match app.setting_raw(key) {
        Ok(Value::Int(i)) if (0..n as i64).contains(&(i as i64)) => i as usize,
        _ => default,
    }
}

/// The page on screen, and its pictures asked of the worker.
fn refresh(ui: &FilesApp, st: &mut State) {
    let Some(b) = st.book.as_mut() else { return };
    let (keep, ask) = b.pics.plan(&b.r.wanted());
    let images = b.r.images();
    let ask = ask
        .into_iter()
        .filter_map(|(id, max)| match images.get(id) {
            Some(ImageSrc::Member(m)) => Some(DocPic {
                id,
                path: b.path.clone(),
                member: Some(m.clone()),
                bytes: None,
                max,
            }),
            _ => None,
        })
        .collect();
    st.pics.doc(b.n, &keep, ask);
    show(ui, b);
}

fn show(ui: &FilesApp, b: &Open) {
    let pics = &b.pics;
    files_ui::doc::show_book(ui, &b.r, |id| pics.image(id));
    ui.set_book_controls(b.controls);
    ui.set_book_contents_open(b.contents);
}

/// The book's picture `n`/`id`; another document's comes back.
pub(crate) fn picture(
    ui: &FilesApp,
    st: &mut State,
    n: u32,
    id: usize,
    px: Option<neo_format_image::Pixels>,
) -> Option<Option<neo_format_image::Pixels>> {
    let Some(b) = st.book.as_mut().filter(|b| b.n == n) else {
        return Some(px);
    };
    b.pics.arrived(id, px, &b.r.wanted());
    show(ui, b);
    None
}

/// A chapter that can't be read, as one centred line.
fn broken(why: &str) -> Doc {
    let mut p = neo_format_document::Para {
        align: neo_format_document::Align::Centre,
        ..Default::default()
    };
    p.push(
        &alloc::format!("This chapter can't be read: {why}"),
        neo_format_document::Style {
            italic: true,
            ..Default::default()
        },
    );
    Doc {
        blocks: alloc::vec![neo_format_document::Block::Para(p)],
        ..Default::default()
    }
}

/// Chapter `i` in hand: at its start, or its last page (`end`), or at a place.
fn go(ui: &FilesApp, st: &mut State, i: usize, end: bool, at: Option<Pos>) {
    let Some(b) = st.book.as_ref() else { return };
    let r = package(&st.fs, &b.path, Some(b.members.clone())).and_then(|p| chapter(&p, &b.book, i));
    let Some(b) = st.book.as_mut() else { return };
    // a chapter that can't be read is a page saying so: turning goes on past it
    let (doc, sizes) = r.unwrap_or_else(|why| {
        dprintln!("files: chapter {} {}", i, why);
        (broken(&why), Vec::new())
    });
    b.r.set_chapter(i, doc, sizes, at, end);
    // the last chapter's pictures go: they were another chapter's ids
    b.pics = crate::doc::Pics::default();
    st.doc_n = st.doc_n.wrapping_add(1);
    b.n = st.doc_n;
    save_place(st);
    refresh(ui, st);
}

pub(crate) fn act(app: &App, ui: &FilesApp, st: &mut State, a: Act) {
    let Some(b) = st.book.as_mut() else { return };
    match a {
        Act::Resized(w, h) => {
            if (w, h) != b.r.view() {
                // the page keeps its place; the pages are counted again at the new size
                b.r.layout(w, h);
                count(st);
                refresh(ui, st);
            }
        }
        Act::Turn(d) => {
            b.controls = false;
            let t = if d > 0 {
                b.r.next_page()
            } else {
                b.r.prev_page()
            };
            match t {
                Turn::Page => {
                    save_place(st);
                    refresh(ui, st);
                }
                Turn::Chapter(i, end) => go(ui, st, i, end, None),
                Turn::None => show(ui, b),
            }
        }
        Act::ToggleControls => {
            b.controls = !b.controls;
            show(ui, b);
        }
        Act::Control(0) => {
            b.controls = false;
            b.contents = true;
            show(ui, b);
        }
        Act::Control(i @ (1 | 2)) => {
            let size = if i == 1 {
                b.r.size.saturating_sub(1)
            } else {
                (b.r.size + 1).min(SIZES.len() - 1)
            };
            if size != b.r.size {
                b.r.set_size(size);
                crate::save(app, SIZE_KEY, Value::Int(size as i32));
                count(st);
            }
            refresh(ui, st);
        }
        Act::Control(_) => {
            b.r.theme = (b.r.theme + 1) % THEMES.len();
            crate::save(app, THEME_KEY, Value::Int(b.r.theme as i32));
            show(ui, b);
        }
        Act::Contents(i) => {
            b.contents = false;
            if let Some(e) = b.book.toc.get(i) {
                let c = e.chapter;
                go(ui, st, c, false, None);
            } else {
                show(ui, b);
            }
        }
        Act::ContentsClosed => {
            b.contents = false;
            show(ui, b);
        }
    }
}

/// The ⋮ menu's book items.
pub(crate) fn menu(app: &App, ui: &FilesApp, st: &mut State, m: &str) -> bool {
    let Some(b) = st.book.as_mut() else {
        return false;
    };
    match m {
        "Contents" => b.contents = true,
        "Text size" => b.controls = true,
        "Theme" => {
            b.r.theme = (b.r.theme + 1) % THEMES.len();
            crate::save(app, THEME_KEY, Value::Int(b.r.theme as i32));
        }
        _ => return false,
    }
    show(ui, b);
    true
}

/// Back: the Contents or the controls close first.
pub(crate) fn back(ui: &FilesApp, st: &mut State) -> bool {
    let Some(b) = st.book.as_mut() else {
        return false;
    };
    if b.contents || b.controls {
        b.contents = false;
        b.controls = false;
        show(ui, b);
        return true;
    }
    false
}

pub(crate) fn close(st: &mut State) {
    if st.book.is_none() {
        return;
    }
    save_place(st);
    st.book = None;
    st.pics.doc(0, &[], Vec::new());
    if let Some(c) = st.counter {
        c.cancel();
    }
}

// ── the place in each book ───────────────────────────────────────────────────────────────────────────────────────

fn places(st: &State) -> Vec<String> {
    store::read(&*st.fs, BOOKS)
        .ok()
        .map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn place(st: &State, path: &str, size: u64, modified: i64) -> Option<(usize, Pos)> {
    let key = format!("{path}\t{size}\t{modified}\t");
    let line = places(st).into_iter().find(|l| l.starts_with(&key))?;
    let mut f = line[key.len()..].split('\t').map(|v| v.parse::<u32>().ok());
    let (c, block, ch) = (f.next()??, f.next()??, f.next()??);
    Some((c as usize, Pos { block, ch }))
}

fn save_place(st: &State) {
    let Some(b) = st.book.as_ref() else { return };
    let key = format!("{}\t{}\t{}\t", b.path, b.size, b.modified);
    let at = b.r.pos();
    let mut lines = places(st);
    lines.retain(|l| !l.starts_with(&key));
    lines.insert(0, format!("{key}{}\t{}\t{}", b.r.chapter, at.block, at.ch));
    lines.truncate(BOOKS_MAX);
    let mut out = lines.join("\n");
    out.push('\n');
    if let Err(e) = store::write(&*st.fs, BOOKS, out.as_bytes()) {
        dprintln!("files: books.dat {}", e.code());
    }
}

// ── counting the pages on a worker ────────────────────────────────────────────────────────────────────────────────

struct Job {
    n: u32,
    path: String,
    fonts: Fonts,
    dp: f32,
    size: usize,
    w: i32,
    h: i32,
}

#[derive(Default)]
struct Queue {
    job: Option<Job>,
    /// (book number, chapter, pages)
    done: Vec<(u32, usize, u32)>,
    busy: bool,
    started: bool,
    /// The book closed: the running count stops.
    cancel: bool,
}

pub(crate) struct Counter {
    q: Mutex<Queue>,
    wake: Semaphore,
}

impl Counter {
    pub(crate) fn new() -> Result<&'static Counter, i32> {
        Ok(Box::leak(Box::new(Counter {
            q: Mutex::new(Queue::default())?,
            wake: Semaphore::new(0)?,
        })))
    }

    fn post(&'static self, job: Job) {
        let start = {
            let mut q = self.q.lock();
            q.job = Some(job);
            q.done.clear();
            !core::mem::replace(&mut q.started, true)
        };
        if start {
            let r = e32::thread::spawn_with_stack(STACK, move || {
                let Ok(fs) = E32Fs::connect() else { return -1 };
                loop {
                    self.wake.wait();
                    loop {
                        let job = {
                            let mut q = self.q.lock();
                            let j = q.job.take();
                            q.busy = j.is_some();
                            q.cancel = false;
                            j
                        };
                        let Some(job) = job else { break };
                        self.count(&fs, &job);
                    }
                }
            });
            if r.is_err() {
                self.q.lock().started = false;
            }
        }
        self.wake.signal();
    }

    /// Stop counting (the book closed).
    fn cancel(&self) {
        let mut q = self.q.lock();
        q.job = None;
        q.cancel = true;
        q.done.clear();
    }

    /// Every chapter laid out as the reader would; a newer job (or a cancel) stops this one.
    fn count(&self, fs: &E32Fs, job: &Job) {
        let t0 = e32::time::millis();
        let Ok(p) = package(fs, &job.path, None) else {
            return;
        };
        let Ok(book) = epub::open(&p) else { return };
        let o = book_opts(job.dp, job.size, job.w, job.h);
        for i in 0..book.spine.len() {
            {
                let q = self.q.lock();
                if q.job.is_some() || q.cancel {
                    return;
                }
            }
            let n = match chapter(&p, &book, i) {
                Ok((doc, sizes)) => count_pages(&doc, &job.fonts, o, job.h, &sizes),
                Err(_) => 1,
            };
            self.q.lock().done.push((job.n, i, n));
        }
        dprintln!(
            "files: counted {} chapters in {} ms",
            book.spine.len(),
            e32::time::millis() - t0
        );
    }

    pub(crate) fn busy(&self) -> bool {
        let q = self.q.lock();
        q.busy || q.job.is_some() || !q.done.is_empty()
    }

    fn take(&self) -> Vec<(u32, usize, u32)> {
        core::mem::take(&mut self.q.lock().done)
    }
}

/// Count the open book's pages at its size and page size (again after a size change).
fn count(st: &mut State) {
    let Some(b) = st.book.as_ref() else { return };
    let Some(c) = st.counter else { return };
    let (w, h) = b.r.view();
    st.doc_n = st.doc_n.wrapping_add(1);
    let n = st.doc_n;
    c.post(Job {
        n,
        path: b.path.clone(),
        fonts: b.r.fonts(),
        dp: b.r.dp(),
        size: b.r.size,
        w,
        h,
    });
    // the reader cleared its counts when the size changed; the worker's answers for this job fill them
    if let Some(b) = st.book.as_mut() {
        b.counted = n;
    }
}

/// Counted chapters into the reader (its footer then numbers the pages through the book).
pub(crate) fn poll(ui: &FilesApp, st: &mut State) {
    let Some(c) = st.counter else { return };
    let done = c.take();
    let Some(b) = st.book.as_mut() else { return };
    let mut any = false;
    for (n, i, pages) in done {
        if n == b.counted
            && let Some(slot) = b.r.counts.get_mut(i)
        {
            *slot = Some(pages);
            any = true;
        }
    }
    if any {
        show(ui, b);
    }
}
