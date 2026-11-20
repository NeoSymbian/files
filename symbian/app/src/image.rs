// SPDX-License-Identifier: GPL-3.0-only
//! FA5 on the UI thread: the image viewer (files2-view-image) and editor (files2-edit-image), and the thumbnails in
//! the rows. Decoding happens on the picture worker ([`pics`]); here are the folder's image list, zoom and pan (the
//! pinch read from raw touches), the editor's recipe with undo, and a small cache of thumbnails as Slint images for
//! the rows on screen.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use e32::dprintln;
use files_core::{self as model, Item};
use files_ui::image::{size_text, slint_image};
use files_ui::{FileRow, FilesApp, ImgGroup, StripItem};
use neo_format_image::gif::Gif;
use neo_format_image::ops::{Edit, UNIT};
use neo_format_image::shrink::{Turn, fit, turned};
use neo_format_image::{Format, Info, Pixels};
use neo_fs::Fs as _;
use neo_view::Family;
use neoapp::App;
use neoui_proto::TouchPhase;
use slint::platform::WindowEvent;
use slint::{ComponentHandle, Image, Model, ModelRc, Timer, TimerMode, VecModel};

use crate::pics::{self, Done, Want};
use crate::{State, View, fa3, note_with, now};

/// Thumbnails kept as Slint images (about 77 KB each at 160 px).
const CACHE_MAX: usize = 120;
/// The editor's preview: the long side.
const PREVIEW_MAX: u32 = 1080;
/// Zoom: double-tap goes to this, a pinch to at most `ZOOM_MAX`.
const ZOOM_TAP: f32 = 2.5;
const ZOOM_MAX: f32 = 6.0;
/// The smallest crop, in [`UNIT`]s of a side.
const CROP_MIN: u32 = 500;
/// Resize choices and their scale (0 = as cropped).
const RESIZE: [&str; 5] = [
    "Original size",
    "50 %",
    "25 %",
    "Fit 1920 px",
    "Fit 1280 px",
];

pub fn is_image(name: &str) -> bool {
    neo_view::family_of(name) == Family::Image
}

/// Thumbnails as images, the newest last; and what was asked of the worker.
#[derive(Default)]
pub struct Thumbs {
    cache: Vec<(String, Image)>,
    /// Paths the worker couldn't read (they keep their type tile).
    failed: Vec<String>,
    /// What was last asked (asked again only when it changes).
    asked: Vec<String>,
}

impl Thumbs {
    fn get(&self, path: &str) -> Option<Image> {
        self.cache
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, i)| i.clone())
    }

    fn put(&mut self, path: String, img: Image) {
        self.cache.retain(|(p, _)| *p != path);
        if self.cache.len() >= CACHE_MAX {
            self.cache.remove(0);
        }
        self.cache.push((path, img));
    }
}

/// The picture shown (once the worker decoded it).
pub struct Shown {
    pub info: Info,
    pub w: u32,
    pub h: u32,
    pub preview: bool,
}

struct Editing {
    /// The picture as shown (EXIF turn applied), small: the edits are drawn on it.
    base: Pixels,
    /// Undo history: the recipe and the Resize choice; `at` is the current one.
    steps: Vec<(Edit, usize)>,
    at: usize,
    cropping: bool,
    ratio: usize,
    /// A crop drag: the handle and the crop when it started.
    drag: Option<(i32, [u32; 4])>,
    /// The crop while a handle is dragged.
    pending: Option<[u32; 4]>,
    /// The copy's path (looked up once: the note shows it on every change).
    dst: String,
    saving: bool,
}

/// A pinch's start: the fingers' distance, the zoom, the pan and the midpoint.
type Pinch = (f32, f32, (f32, f32), (f32, f32));

pub struct Viewer {
    /// The folder's images in the folder's order: path, size, time.
    pub list: Vec<(String, u64, i64)>,
    pub at: usize,
    /// The worker request the viewer waits for.
    n: u32,
    pub shown: Option<Shown>,
    zoom: f32,
    pan: (f32, f32),
    drag0: Option<(f32, f32)>,
    chrome: bool,
    /// Fingers 0 and 1 (surface px), and a pinch: distance, zoom, pan and midpoint at its start.
    fingers: [Option<(f32, f32)>; 2],
    pinch: Option<Pinch>,
    /// Finger 0's events go nowhere until it lifts (it was part of a pinch).
    swallow: bool,
    gif: Option<Timer>,
    edit: Option<Editing>,
    pub from: View,
    asked_at: u64,
}

fn current(v: &Viewer) -> &str {
    v.list.get(v.at).map_or("", |e| e.0.as_str())
}

/// Opens `path` in the viewer, with the images of its folder (the open folder's order when it is that folder).
pub fn open(app: &App, ui: &FilesApp, st: &mut State, path: &str) {
    let Some(dir) = neo_fs::path::parent(path) else {
        return;
    };
    let list = crate::folder_files(st, &dir, is_image);
    let at = list.iter().position(|e| e.0 == path).unwrap_or(0);
    let list = if list.is_empty() {
        alloc::vec![(String::from(path), 0, 0)]
    } else {
        list
    };
    let from = if matches!(st.view, View::Image | View::ImageEdit) {
        st.viewer.as_ref().map_or(View::Folder, |v| v.from)
    } else {
        st.view
    };
    st.dir = dir;
    st.file = String::from(path);
    st.view = View::Image;
    st.viewer = Some(Viewer {
        list,
        at,
        n: 0,
        shown: None,
        zoom: 1.0,
        pan: (0.0, 0.0),
        drag0: None,
        chrome: true,
        fingers: [None; 2],
        pinch: None,
        swallow: false,
        gif: None,
        edit: None,
        from,
        asked_at: 0,
    });
    ui.set_view("image".into());
    ui.set_img_hint(!crate::flag(app, "files.hint.zoom", false));
    go(ui, st, at);
}

/// Shows image `at` of the list: its thumbnail at once, the picture when the worker has it.
fn go(ui: &FilesApp, st: &mut State, at: usize) {
    let Some(v) = st.viewer.as_mut() else { return };
    let at = at.min(v.list.len().saturating_sub(1));
    v.at = at;
    v.n += 1;
    v.shown = None;
    v.gif = None;
    v.zoom = 1.0;
    v.pan = (0.0, 0.0);
    v.asked_at = e32::time::millis();
    let (path, size, modified) = v.list[at].clone();
    st.pics.show(v.n, &path);
    st.file = path.clone();
    fa3::opened(st, &path);
    ui.set_name(neo_fs::path::name_of(&path).into());
    ui.set_img(st.thumbs.get(&path).unwrap_or_default());
    ui.set_img_note("".into());
    ui.set_img_info(info_text(None, size, modified).as_str().into());
    let n = st.viewer.as_ref().map_or(0, |v| v.list.len());
    ui.set_img_counter(if n > 1 {
        format!(
            "{} / {}",
            files_ui::thousands(at as u64 + 1),
            files_ui::thousands(n as u64)
        )
        .into()
    } else {
        "".into()
    });
    ui.set_img_editable(
        st.drives.iter().any(|&(d, ro)| path.starts_with(d) && !ro)
            && !crate::archive::in_cache(&path),
    );
    show_view(ui, st);
    strip(ui, st);
}

/// "29 Oct 2026 · 4032 × 3024 · 3.1 MB" (the date taken when the file says).
fn info_text(info: Option<&Info>, size: u64, modified: i64) -> String {
    let date = info
        .and_then(|i| i.taken.as_deref())
        .and_then(exif_date)
        .unwrap_or_else(|| {
            let d = model::full_date_text(modified);
            String::from(d.split(',').next().unwrap_or(""))
        });
    let mut s = date;
    if let Some(i) = info {
        let (w, h) = i.shown();
        s.push_str(" · ");
        s.push_str(&size_text(w, h));
    }
    if size > 0 {
        s.push_str(" · ");
        s.push_str(&model::size_text(size));
    }
    s
}

/// "2026:10:29 18:42:10" → "29 Oct 2026".
fn exif_date(t: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let y: u32 = t.get(0..4)?.parse().ok()?;
    let m: usize = t.get(5..7)?.parse().ok()?;
    let d: u32 = t.get(8..10)?.parse().ok()?;
    (y > 1900 && (1..=12).contains(&m)).then(|| format!("{d} {} {y}", MONTHS[m - 1]))
}

/// The strip: three either side of the current image.
fn strip(ui: &FilesApp, st: &mut State) {
    let Some(v) = st.viewer.as_ref() else { return };
    let a = v.at.saturating_sub(3);
    let e = (v.at + 4).min(v.list.len());
    let items: Vec<StripItem> = (a..e)
        .map(|k| {
            let img = st.thumbs.get(&v.list[k].0);
            StripItem {
                has: img.is_some(),
                img: img.unwrap_or_default(),
            }
        })
        .collect();
    ui.set_img_current((v.at - a) as i32);
    ui.set_img_strip(ModelRc::from(Rc::new(VecModel::from(items))));
}

fn show_view(ui: &FilesApp, st: &State) {
    let Some(v) = st.viewer.as_ref() else { return };
    ui.set_img_zoom(v.zoom);
    ui.set_img_pan_x(v.pan.0);
    ui.set_img_pan_y(v.pan.1);
    ui.set_img_chrome(v.chrome);
}

/// The picture's size on screen at zoom 1.
fn fitted(ui: &FilesApp, st: &State) -> (f32, f32) {
    let f = ui.get_img_fit();
    st.viewer
        .as_ref()
        .and_then(|v| v.shown.as_ref())
        .map_or((0.0, 0.0), |s| (s.w as f32 * f, s.h as f32 * f))
}

/// Keeps the picture over the screen when zoomed (centred when it is smaller).
fn clamp_pan(app: &App, ui: &FilesApp, st: &mut State) {
    let (fw, fh) = fitted(ui, st);
    let (sw, sh) = (app.width as f32, app.height() as f32);
    let Some(v) = st.viewer.as_mut() else { return };
    let mx = ((fw * v.zoom - sw) / 2.0).max(0.0);
    let my = ((fh * v.zoom - sh) / 2.0).max(0.0);
    v.pan = (v.pan.0.clamp(-mx, mx), v.pan.1.clamp(-my, my));
}

/// Zooms to `z` keeping the picture point under `p` (surface px) where it is.
fn zoom_at(app: &App, st: &mut State, z: f32, p: (f32, f32)) {
    let c = (app.width as f32 / 2.0, app.height() as f32 / 2.0);
    let Some(v) = st.viewer.as_mut() else { return };
    let z = z.clamp(1.0, ZOOM_MAX);
    let k = z / v.zoom;
    v.pan = (
        p.0 - c.0 - (p.0 - c.0 - v.pan.0) * k,
        p.1 - c.1 - (p.1 - c.1 - v.pan.1) * k,
    );
    v.zoom = z;
}

pub fn tapped(ui: &FilesApp, st: &mut State) {
    if let Some(v) = st.viewer.as_mut() {
        v.chrome = !v.chrome;
    }
    show_view(ui, st);
}

pub fn double_tapped(app: &App, ui: &FilesApp, st: &mut State, x: f32, y: f32) {
    let Some(v) = st.viewer.as_mut() else { return };
    // the two taps toggled the chrome twice: it is as it was
    if v.zoom > 1.01 {
        v.zoom = 1.0;
        v.pan = (0.0, 0.0);
    } else {
        zoom_at(app, st, ZOOM_TAP, (x, y));
        clamp_pan(app, ui, st);
    }
    hint_seen(app, ui);
    show_view(ui, st);
}

fn hint_seen(app: &App, ui: &FilesApp) {
    if ui.get_img_hint() {
        ui.set_img_hint(false);
        crate::save(
            app,
            "files.hint.zoom",
            neoui_proto::settings::Value::Bool(true),
        );
    }
}

pub fn dragged(app: &App, ui: &FilesApp, st: &mut State, dx: f32, dy: f32) {
    let Some(v) = st.viewer.as_mut() else { return };
    if v.pinch.is_some() {
        return;
    }
    let p0 = *v.drag0.get_or_insert(v.pan);
    if v.zoom > 1.01 {
        v.pan = (p0.0 + dx, p0.1 + dy);
        clamp_pan(app, ui, st);
    } else {
        // the picture follows the finger sideways
        v.pan = (dx, 0.0);
    }
    show_view(ui, st);
}

pub fn released(app: &App, ui: &FilesApp, st: &mut State, dx: f32, _dy: f32) {
    let Some(v) = st.viewer.as_mut() else { return };
    v.drag0 = None;
    if v.zoom > 1.01 {
        return;
    }
    v.pan = (0.0, 0.0);
    let far = dx.abs() > app.width as f32 * 0.25;
    let at = v.at;
    let n = v.list.len();
    if far && dx < 0.0 && at + 1 < n {
        go(ui, st, at + 1);
    } else if far && dx > 0.0 && at > 0 {
        go(ui, st, at - 1);
    } else {
        show_view(ui, st);
    }
}

pub fn strip_picked(ui: &FilesApp, st: &mut State, i: usize) {
    let Some(v) = st.viewer.as_ref() else { return };
    let to = v.at.saturating_sub(3) + i;
    if to != v.at {
        go(ui, st, to);
    }
}

/// A touch while the viewer shows: a second finger makes a pinch (Slint sees one pointer, so its press is cancelled
/// and the fingers are read here). True when the event is used up.
pub fn touch(
    app: &App,
    ui: &FilesApp,
    st: &mut State,
    id: u8,
    x: i32,
    y: i32,
    phase: TouchPhase,
) -> bool {
    let Some(v) = st.viewer.as_mut() else {
        return false;
    };
    let id = id as usize;
    if id > 1 {
        return true;
    }
    let p = (x as f32, y as f32);
    v.fingers[id] = if phase == TouchPhase::Up {
        None
    } else {
        Some(p)
    };
    match (id, phase) {
        (1, TouchPhase::Down) => {
            if let Some(f0) = v.fingers[0] {
                let d = dist(f0, p).max(1.0);
                v.pinch = Some((d, v.zoom, v.pan, mid(f0, p)));
                v.swallow = true;
                v.drag0 = None;
                app.window.dispatch_event(WindowEvent::PointerExited);
            }
            true
        }
        (_, TouchPhase::Move) if v.pinch.is_some() => {
            if let (Some(a), Some(b), Some((d0, z0, pan0, m0))) =
                (v.fingers[0], v.fingers[1], v.pinch)
            {
                let c = (app.width as f32 / 2.0, app.height() as f32 / 2.0);
                let z = (z0 * dist(a, b) / d0).clamp(1.0, ZOOM_MAX);
                let k = z / z0;
                let m = mid(a, b);
                // the picture point under the first midpoint follows the midpoint
                v.pan = (
                    m.0 - c.0 - (m0.0 - c.0 - pan0.0) * k,
                    m.1 - c.1 - (m0.1 - c.1 - pan0.1) * k,
                );
                v.zoom = z;
                clamp_pan(app, ui, st);
                hint_seen(app, ui);
                show_view(ui, st);
            }
            true
        }
        (_, TouchPhase::Up) if v.pinch.is_some() || v.swallow => {
            v.pinch = None;
            if v.zoom < 1.05 {
                v.zoom = 1.0;
                v.pan = (0.0, 0.0);
            }
            if id == 0 {
                v.swallow = false;
            }
            show_view(ui, st);
            true
        }
        (1, _) => true,
        (0, _) => v.swallow,
        _ => false,
    }
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    libm::sqrtf((a.0 - b.0) * (a.0 - b.0) + (a.1 - b.1) * (a.1 - b.1))
}

fn mid(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
}

// ── the worker's results ──────────────────────────────────────────────────────────────────────────────────────────

/// Takes what the worker made: the viewer's picture, thumbnails (into the rows), a finished save.
pub fn poll(app: &App, ui: &FilesApp, st: &mut State) {
    let mut thumbs = false;
    for d in st.pics.take() {
        match d {
            Done::Thumb(path, Some(px)) => {
                st.thumbs.put(path, slint_image(&px));
                thumbs = true;
            }
            Done::Thumb(path, None) => {
                st.thumbs.failed.push(path);
            }
            Done::Show(n, path, r) => shown(app, ui, st, n, &path, r),
            Done::Saved(path, r) => saved(app, ui, st, &path, r),
        }
    }
    if thumbs {
        patch_rows(ui, st);
        if st.view == View::Image {
            strip(ui, st);
        }
    }
}

fn shown(
    app: &App,
    ui: &FilesApp,
    st: &mut State,
    n: u32,
    path: &str,
    r: Result<pics::Shown, i32>,
) {
    let Some(v) = st.viewer.as_mut() else { return };
    if n != v.n {
        return;
    }
    let ms = e32::time::millis() - v.asked_at;
    let size = v.list.get(v.at).map_or(0, |e| e.1);
    let modified = v.list.get(v.at).map_or(0, |e| e.2);
    match r {
        Ok(s) => {
            dprintln!("files: view {} {} {} {}", path, s.info.w, s.info.h, ms);
            ui.set_img(slint_image(&s.px));
            ui.set_img_info(
                info_text(Some(&s.info), s.bytes.max(size), modified)
                    .as_str()
                    .into(),
            );
            ui.set_img_note(
                if s.preview {
                    "Shown at preview size"
                } else {
                    ""
                }
                .into(),
            );
            v.shown = Some(Shown {
                w: s.px.w,
                h: s.px.h,
                info: s.info,
                preview: s.preview,
            });
            if let Some(b) = s.gif {
                animate(ui, v, b);
            }
        }
        Err(e) => {
            dprintln!("files: view {} err {}", path, e);
            let text = match e {
                -2 => "Too big to open",
                -1 | -3 => "Can't open this image",
                _ => "Can't read this file",
            };
            note_with(app, text, "");
        }
    }
}

/// Steps an animated GIF with a timer, each frame copied into the picture.
fn animate(ui: &FilesApp, v: &mut Viewer, bytes: Vec<u8>) {
    let Ok(g) = Gif::new(bytes) else { return };
    let g = Rc::new(RefCell::new(g));
    let weak = ui.as_weak();
    let timer = Timer::default();
    fn step(g: &Rc<RefCell<Gif<Vec<u8>>>>, weak: &slint::Weak<FilesApp>) -> u32 {
        let mut gif = g.borrow_mut();
        let delay = match gif.step() {
            Ok(Some(d)) => d,
            _ => {
                gif.rewind();
                gif.step().ok().flatten().unwrap_or(100)
            }
        };
        if let Some(ui) = weak.upgrade() {
            ui.set_img(slint_image(gif.canvas()));
        }
        delay
    }
    let first = step(&g, &weak);
    let t = Rc::new(timer);
    let t2 = Rc::downgrade(&t);
    t.start(
        TimerMode::Repeated,
        core::time::Duration::from_millis(first as u64),
        move || {
            let d = step(&g, &weak);
            if let Some(t) = t2.upgrade() {
                t.set_interval(core::time::Duration::from_millis(d as u64));
            }
        },
    );
    // the viewer keeps the timer; dropping it (another image, Back) stops the animation
    if let Ok(t) = Rc::try_unwrap(t) {
        v.gif = Some(t);
    }
}

// ── the editor ───────────────────────────────────────────────────────────────────────────────────────────────────

pub fn edit(ui: &FilesApp, st: &mut State) {
    let Some(v) = st.viewer.as_mut() else { return };
    let Some(s) = v.shown.as_ref() else { return };
    if s.preview {
        return;
    }
    // the picture as shown, small: what the edits are drawn on
    let img = ui.get_img();
    let px = if let Some(b) = img.to_rgb8() {
        Pixels {
            w: b.width(),
            h: b.height(),
            ch: 3,
            px: b.as_bytes().to_vec(),
        }
    } else if let Some(b) = img.to_rgba8() {
        Pixels {
            w: b.width(),
            h: b.height(),
            ch: 4,
            px: b.as_bytes().to_vec(),
        }
    } else {
        return;
    };
    let base = fit(&px, Turn::NONE, PREVIEW_MAX);
    let jpeg = s.info.format == Format::Jpeg;
    let src = String::from(current(v));
    let dst = copy_name(st, &src, jpeg);
    let Some(v) = st.viewer.as_mut() else { return };
    v.gif = None;
    v.edit = Some(Editing {
        base,
        steps: alloc::vec![(Edit::NONE, 0)],
        at: 0,
        cropping: true,
        ratio: 0,
        drag: None,
        pending: None,
        dst,
        saving: false,
    });
    st.view = View::ImageEdit;
    ui.set_view("image-edit".into());
    ui.set_resize_choices(ModelRc::from(Rc::new(VecModel::from(
        RESIZE
            .iter()
            .map(|s| (*s).into())
            .collect::<Vec<slint::SharedString>>(),
    ))));
    show_edit(ui, st, true);
}

/// Edits made (Back asks before dropping them).
pub fn edited(st: &State) -> bool {
    st.viewer
        .as_ref()
        .and_then(|v| v.edit.as_ref())
        .is_some_and(|e| e.at > 0)
}

fn now_edit(e: &Editing) -> (Edit, usize) {
    e.steps[e.at]
}

/// The new name: `sunset-edit.jpg`, `sunset-edit 2.jpg`… (PNG unless the original is a JPEG).
fn copy_name(st: &State, path: &str, jpeg: bool) -> String {
    let dir = neo_fs::path::parent(path).unwrap_or_default();
    let name = neo_fs::path::name_of(path);
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let ext = if jpeg {
        name.rsplit_once('.').map_or("jpg", |(_, e)| e)
    } else {
        "png"
    };
    let mut k = 1;
    loop {
        let n = if k == 1 {
            format!("{dir}{stem}-edit.{ext}")
        } else {
            format!("{dir}{stem}-edit {k}.{ext}")
        };
        if st.fs.entry(&n).is_err() {
            return n;
        }
        k += 1;
    }
}

/// The saved size for the Resize choice `r` of what the recipe crops.
fn resized(info: &Info, e: &Edit, r: usize) -> Option<(u32, u32)> {
    let base = Turn::exif(info.orientation);
    let (w, h) = Edit { size: None, ..*e }.out_size(base, info.w, info.h);
    let scale = |k: f32| {
        Some((
            ((w as f32 * k) as u32).max(1),
            ((h as f32 * k) as u32).max(1),
        ))
    };
    match r {
        1 => scale(0.5),
        2 => scale(0.25),
        3 | 4 => {
            let m = if r == 3 { 1920.0 } else { 1280.0 };
            let k = (m / w.max(h) as f32).min(1.0);
            if k >= 1.0 { None } else { scale(k) }
        }
        _ => None,
    }
}

fn show_edit(ui: &FilesApp, st: &mut State, redraw: bool) {
    let Some(v) = st.viewer.as_ref() else { return };
    let (Some(e), Some(s)) = (v.edit.as_ref(), v.shown.as_ref()) else {
        return;
    };
    let (cur, r) = now_edit(e);
    let cur = Edit {
        crop: e.pending.unwrap_or(cur.crop),
        ..cur
    };
    if redraw {
        ui.set_img(slint_image(&turned(&e.base, cur.turn)));
    }
    let (bw, bh) = (ui.get_img_box_w(), ui.get_img_box_h());
    let [x, y, w, h] = cur.crop;
    let u = |v: u32, n: f32| v as f32 * n / UNIT as f32;
    ui.set_crop_x(u(x, bw));
    ui.set_crop_y(u(y, bh));
    ui.set_crop_w(u(w, bw));
    ui.set_crop_h(u(h, bh));
    let size = Edit {
        size: resized(&s.info, &cur, r),
        ..cur
    }
    .out_size(Turn::exif(s.info.orientation), s.info.w, s.info.h);
    ui.set_crop_label(size_text(size.0, size.1).as_str().into());
    ui.set_crop_ratio(e.ratio as i32);
    ui.set_img_cropping(e.cropping);
    ui.set_img_can_undo(e.at > 0);
    ui.set_img_can_redo(e.at + 1 < e.steps.len());
    ui.set_img_saving(e.saving);
    ui.set_resize_current(r as i32);
    ui.set_img_save_note(
        format!(
            "Saves {};\nthe original stays.",
            neo_fs::path::name_of(&e.dst)
        )
        .as_str()
        .into(),
    );
}

fn push(ui: &FilesApp, st: &mut State, step: (Edit, usize), redraw: bool) {
    let Some(e) = st.viewer.as_mut().and_then(|v| v.edit.as_mut()) else {
        return;
    };
    if e.steps[e.at] == step {
        return;
    }
    e.steps.truncate(e.at + 1);
    e.steps.push(step);
    e.at += 1;
    show_edit(ui, st, redraw);
}

pub fn undo(ui: &FilesApp, st: &mut State, redo: bool) {
    let Some(e) = st.viewer.as_mut().and_then(|v| v.edit.as_mut()) else {
        return;
    };
    if redo && e.at + 1 < e.steps.len() {
        e.at += 1;
    } else if !redo && e.at > 0 {
        e.at -= 1;
    } else {
        return;
    }
    show_edit(ui, st, true);
}

pub fn rotate(ui: &FilesApp, st: &mut State, flip: bool) {
    let Some(e) = st.viewer.as_ref().and_then(|v| v.edit.as_ref()) else {
        return;
    };
    let (cur, r) = now_edit(e);
    let next = if flip { cur.flip() } else { cur.rotate() };
    push(ui, st, (next, r), true);
}

pub fn crop_mode(ui: &FilesApp, st: &mut State) {
    if let Some(e) = st.viewer.as_mut().and_then(|v| v.edit.as_mut()) {
        e.cropping = !e.cropping;
    }
    show_edit(ui, st, false);
}

pub fn resize_chosen(ui: &FilesApp, st: &mut State, i: usize) {
    let Some(e) = st.viewer.as_ref().and_then(|v| v.edit.as_ref()) else {
        return;
    };
    let (cur, _) = now_edit(e);
    push(ui, st, (cur, i.min(RESIZE.len() - 1)), false);
}

/// The shown picture's size in px (the crop's units scale by it for a ratio).
fn shown_size(st: &State) -> (u32, u32) {
    let Some(v) = st.viewer.as_ref() else {
        return (1, 1);
    };
    let (Some(e), Some(s)) = (v.edit.as_ref(), v.shown.as_ref()) else {
        return (1, 1);
    };
    let (w, h) = Turn::exif(s.info.orientation).size(s.info.w, s.info.h);
    now_edit(e).0.turn.size(w, h)
}

/// The ratio chips: the largest centred box of that shape (Original: the whole picture).
pub fn ratio_picked(ui: &FilesApp, st: &mut State, i: usize) {
    let (w, h) = shown_size(st);
    let Some(e) = st.viewer.as_mut().and_then(|v| v.edit.as_mut()) else {
        return;
    };
    e.ratio = i;
    let (cur, r) = now_edit(e);
    let shape = match i {
        2 => Some((1.0, 1.0)),
        3 => Some((4.0, 3.0)),
        4 => Some((16.0, 9.0)),
        _ => None,
    };
    let crop = match (i, shape) {
        (1, _) => [0, 0, UNIT, UNIT],
        (_, Some((a, b))) => fit_ratio(w, h, a / b),
        _ => cur.crop,
    };
    push(ui, st, (Edit { crop, ..cur }, r), false);
}

/// The largest box of aspect `ratio` (w/h in px) centred in a `w`×`h` picture, in units.
fn fit_ratio(w: u32, h: u32, ratio: f32) -> [u32; 4] {
    let (w, h) = (w as f32, h as f32);
    let (bw, bh) = if w / h > ratio {
        (h * ratio, h)
    } else {
        (w, w / ratio)
    };
    let cw = (bw / w * UNIT as f32) as u32;
    let ch = (bh / h * UNIT as f32) as u32;
    [(UNIT - cw) / 2, (UNIT - ch) / 2, cw, ch]
}

pub fn crop_start(st: &mut State, handle: i32) {
    if let Some(e) = st.viewer.as_mut().and_then(|v| v.edit.as_mut()) {
        e.drag = Some((handle, now_edit(e).0.crop));
    }
}

/// A handle dragged `dx`, `dy` (px on screen) since its press.
pub fn crop_drag(ui: &FilesApp, st: &mut State, handle: i32, dx: f32, dy: f32) {
    let (bw, bh) = (ui.get_img_box_w().max(1.0), ui.get_img_box_h().max(1.0));
    let (pw, ph) = shown_size(st);
    let Some(e) = st.viewer.as_mut().and_then(|v| v.edit.as_mut()) else {
        return;
    };
    let Some((h0, [x0, y0, w0, hh0])) = e.drag else {
        return;
    };
    if h0 != handle {
        return;
    }
    let du = (dx / bw * UNIT as f32) as i64;
    let dv = (dy / bh * UNIT as f32) as i64;
    let (x0, y0, w0, hh0) = (x0 as i64, y0 as i64, w0 as i64, hh0 as i64);
    let (u, m) = (UNIT as i64, CROP_MIN as i64);
    let (mut x, mut y, mut w, mut h) = (x0, y0, w0, hh0);
    if handle == 0 {
        x = (x0 + du).clamp(0, u - w0);
        y = (y0 + dv).clamp(0, u - hh0);
    } else {
        let left = matches!(handle, 1 | 3 | 7);
        let right = matches!(handle, 2 | 4 | 8);
        let top = matches!(handle, 1 | 2 | 5);
        let bottom = matches!(handle, 3 | 4 | 6);
        if left {
            x = (x0 + du).clamp(0, x0 + w0 - m);
            w = x0 + w0 - x;
        }
        if right {
            w = (w0 + du).clamp(m, u - x0);
        }
        if top {
            y = (y0 + dv).clamp(0, y0 + hh0 - m);
            h = y0 + hh0 - y;
        }
        if bottom {
            h = (hh0 + dv).clamp(m, u - y0);
        }
        // a fixed shape: the height follows the width (the width the height on the top and bottom edges)
        let shape = match e.ratio {
            2 => Some(1.0f32),
            3 => Some(4.0 / 3.0),
            4 => Some(16.0 / 9.0),
            _ => None,
        };
        if let Some(r) = shape {
            // units to px: w·pw/U ÷ (h·ph/U) = r
            let k = pw as f32 / (ph as f32 * r);
            if matches!(handle, 5 | 6) {
                w = ((h as f32 / k) as i64).clamp(m, u - x);
            } else {
                h = ((w as f32 * k) as i64).clamp(m, u - y);
                w = ((h as f32 / k) as i64).min(w);
            }
            if top {
                y = y0 + hh0 - h;
            }
            if left {
                x = x0 + w0 - w;
            }
        }
    }
    // drawn now; one undo step when the finger lifts
    e.pending = Some([x as u32, y as u32, w as u32, h as u32]);
    show_edit(ui, st, false);
}

pub fn crop_done(ui: &FilesApp, st: &mut State) {
    let Some(e) = st.viewer.as_mut().and_then(|v| v.edit.as_mut()) else {
        return;
    };
    e.drag = None;
    let Some(crop) = e.pending.take() else { return };
    let (cur, r) = now_edit(e);
    push(ui, st, (Edit { crop, ..cur }, r), false);
}

pub fn reset(ui: &FilesApp, st: &mut State) {
    push(ui, st, (Edit::NONE, 0), true);
}

pub fn save(app: &App, ui: &FilesApp, st: &mut State) {
    let Some(v) = st.viewer.as_ref() else { return };
    let (Some(e), Some(s)) = (v.edit.as_ref(), v.shown.as_ref()) else {
        return;
    };
    if e.saving {
        return;
    }
    let src = String::from(current(v));
    let info = s.info.clone();
    let (cur, r) = now_edit(e);
    if st.drives.iter().any(|&(d, ro)| src.starts_with(d) && ro) {
        return note_with(app, "This drive is read-only", "");
    }
    let edit = Edit {
        size: resized(&info, &cur, r),
        ..cur
    };
    let dst = copy_name(st, &src, info.format == Format::Jpeg);
    if let Some(e) = st.viewer.as_mut().and_then(|v| v.edit.as_mut()) {
        e.saving = true;
    }
    st.pics.save(&src, &dst, edit);
    show_edit(ui, st, false);
}

fn saved(app: &App, ui: &FilesApp, st: &mut State, path: &str, r: Result<(u32, u32), i32>) {
    if let Some(e) = st.viewer.as_mut().and_then(|v| v.edit.as_mut()) {
        e.saving = false;
    }
    match r {
        Ok((w, h)) => {
            dprintln!("files: imgsave {} {} {}", path, w, h);
            note_with(app, &format!("Saved {}", neo_fs::path::name_of(path)), "");
            // the copy, in the viewer (its folder lists it now)
            if let Some(v) = st.viewer.as_mut() {
                v.edit = None;
            }
            let from = st.viewer.as_ref().map_or(View::Folder, |v| v.from);
            if from == View::Folder {
                st.items = st
                    .fs
                    .read_dir(&neo_fs::path::parent(path).unwrap_or_default())
                    .map(|v| {
                        v.into_iter()
                            .map(|e| Item {
                                is_dir: e.is_dir(),
                                name: e.name,
                                size: e.size,
                                modified: e.modified,
                                ..Item::default()
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                model::sort(&mut st.items, st.sort);
                st.checked = alloc::vec![false; st.items.len()];
            }
            open(app, ui, st, path);
        }
        Err(e) => {
            dprintln!("files: imgsave {} err {}", path, e);
            note_with(
                app,
                match e {
                    -2 => "Too big to edit",
                    -26 => "Not enough space",
                    _ => "Couldn't save the copy",
                },
                "",
            );
            show_edit(ui, st, false);
        }
    }
}

/// Back from the editor (to the viewer) or the viewer (to where it was opened from); true when handled here.
pub fn leave_editor(ui: &FilesApp, st: &mut State) {
    let Some(v) = st.viewer.as_mut() else { return };
    v.edit = None;
    st.view = View::Image;
    ui.set_view("image".into());
    let at = v.at;
    go(ui, st, at);
}

// ── thumbnails in the rows ───────────────────────────────────────────────────────────────────────────────────────

/// The rows on screen (with a screen either side) in the folder or a listing; rows are 64 dp, the grid 124 dp a
/// line of three.
fn window(app: &App, ui: &FilesApp, st: &State, n: usize) -> (usize, usize) {
    let dp = app.dp;
    let (top, h) = if st.view == View::Folder {
        (ui.get_folder_top(), ui.get_folder_h())
    } else {
        (ui.get_list_top(), ui.get_list_h())
    };
    let (row, per) = if st.view == View::Folder && st.grid {
        (124.0 * dp, 3)
    } else {
        (64.0 * dp, 1)
    };
    let first = ((top - h).max(0.0) / row) as usize * per;
    let last = ((top + 2.0 * h) / row) as usize * per + per;
    (first.min(n), last.min(n))
}

/// The image rows' paths, in row order, of what is showing (the type page's groups included).
fn image_rows(st: &State) -> Vec<(usize, String, u64, i64)> {
    match st.view {
        View::Folder => st
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| !i.is_dir && is_image(&i.name))
            .map(|(k, i)| (k, model::join_file(&st.dir, &i.name), i.size, i.modified))
            .collect(),
        View::Recent | View::Type | View::Review => st
            .listed
            .iter()
            .enumerate()
            .filter(|(_, l)| is_image(&l.item.name))
            .map(|(k, l)| (k, l.path.clone(), l.item.size, l.item.modified))
            .collect(),
        _ => Vec::new(),
    }
}

/// Puts the cached thumbnails into the rows on screen, takes them off the others, and asks the worker for the
/// missing ones. Called when rows change, after a scroll, and when the worker made some.
pub fn refresh(app: &App, ui: &FilesApp, st: &mut State) {
    let mut wants: Vec<Want> = Vec::new();
    if st.view == View::Type && !st.img_groups.is_empty() {
        for g in &st.img_groups {
            for &k in g.1.iter() {
                if let Some(l) = st.listed.get(k) {
                    wants.push(Want {
                        path: l.path.clone(),
                        size: l.item.size,
                        modified: l.item.modified,
                    });
                }
            }
        }
    } else if matches!(st.view, View::Image | View::ImageEdit) {
        if let Some(v) = st.viewer.as_ref() {
            let a = v.at.saturating_sub(3);
            let e = (v.at + 4).min(v.list.len());
            for (p, s, m) in &v.list[a..e] {
                wants.push(Want {
                    path: p.clone(),
                    size: *s,
                    modified: *m,
                });
            }
        }
    } else {
        let rows = image_rows(st);
        let n = match st.view {
            View::Folder => st.items.len(),
            _ => st.listed.len(),
        };
        let (a, e) = window(app, ui, st, n);
        for (k, p, s, m) in rows {
            if k >= a && k < e {
                wants.push(Want {
                    path: p,
                    size: s,
                    modified: m,
                });
            }
        }
    }
    wants.retain(|w| st.thumbs.get(&w.path).is_none() && !st.thumbs.failed.contains(&w.path));
    let asked: Vec<String> = wants.iter().map(|w| w.path.clone()).collect();
    if asked != st.thumbs.asked {
        st.thumbs.asked = asked;
        if !wants.is_empty() {
            st.pics.thumbs(wants);
        }
    }
    patch_rows(ui, st);
}

/// The rows' thumbnails from the cache (rows off screen lose theirs, so the memory goes with the cache).
pub fn patch_rows(ui: &FilesApp, st: &State) {
    if st.view == View::Type && !st.img_groups.is_empty() {
        let groups = ui.get_img_groups();
        for gi in 0..groups.row_count() {
            let Some(g) = groups.row_data(gi) else {
                continue;
            };
            let Some(idx) = st.img_groups.get(gi) else {
                continue;
            };
            for (i, &k) in idx.1.iter().enumerate() {
                let Some(mut r) = g.thumbs.row_data(i) else {
                    continue;
                };
                let img = st.listed.get(k).and_then(|l| st.thumbs.get(&l.path));
                if r.has_thumb != img.is_some() {
                    r.has_thumb = img.is_some();
                    r.thumb = img.unwrap_or_default();
                    g.thumbs.set_row_data(i, r);
                }
            }
        }
        return;
    }
    let rows = ui.get_rows();
    let paths: Vec<(usize, String)> = image_rows(st)
        .into_iter()
        .map(|(k, p, _, _)| (k, p))
        .collect();
    for (k, p) in paths {
        let Some(mut r) = rows.row_data(k) else {
            continue;
        };
        let img = st.thumbs.get(&p);
        if r.has_thumb != img.is_some() {
            r.has_thumb = img.is_some();
            r.thumb = img.unwrap_or_default();
            rows.set_row_data(k, r);
        }
    }
}

/// files2-type-images: the Images type by folder, eight a folder (the last "+N" opens it).
pub fn type_groups(ui: &FilesApp, st: &mut State) {
    let mut groups: Vec<(String, Vec<usize>, usize)> = Vec::new();
    for (k, l) in st.listed.iter().enumerate() {
        let dir = neo_fs::path::parent(&l.path).unwrap_or_default();
        match groups.iter_mut().find(|g| g.0 == dir) {
            Some(g) => {
                g.2 += 1;
                if g.1.len() < 8 {
                    g.1.push(k);
                }
            }
            None => groups.push((dir, alloc::vec![k], 1)),
        }
    }
    let now = now();
    let rows: Vec<ImgGroup> = groups
        .iter()
        .map(|(dir, idx, n)| ImgGroup {
            title: dir.trim_end_matches('\\').into(),
            count: files_ui::thousands(*n as u64).as_str().into(),
            more: if *n > idx.len() {
                format!("+{}", files_ui::thousands((*n - idx.len() + 1) as u64))
                    .as_str()
                    .into()
            } else {
                "".into()
            },
            thumbs: ModelRc::from(Rc::new(VecModel::from(
                idx.iter()
                    .map(|&k| files_ui::file_row(&st.listed[k].item, now, false))
                    .collect::<Vec<FileRow>>(),
            ))),
        })
        .collect();
    st.img_groups = groups.into_iter().map(|(d, idx, _)| (d, idx)).collect();
    ui.set_img_groups(ModelRc::from(Rc::new(VecModel::from(rows))));
    ui.set_rows(ModelRc::from(Rc::new(
        VecModel::from(Vec::<FileRow>::new()),
    )));
}
