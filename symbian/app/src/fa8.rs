// SPDX-License-Identifier: GPL-3.0-only
//! FA8, open with, share, pick (protocol 1.12): the Open with sheet (Files' own viewer, then the apps
//! `neoui_proto::open` lists for the type, with "Always"), the Share sheet (Copy path, Copy to clipboard, Open with…),
//! Files as the system file picker (pages `files::PICK` and `files::SAVE`, answered with `DONE_FILE`), and copied
//! files on the system clipboard (a paths clip), so a Copy survives Files closing.
//!
//! Files go to other apps by handle (`App::open_file`): an app sees a file in Files' private folder (an archive's
//! member) or one with a long path without being able to open it by name.

use crate::archive::{self, Purpose};
use crate::{Clip, State, View, is_text, note_with, show_clip, toast};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use e32::dprintln;
use e32::fs::{File as RFile, mode};
use files_ops::{Kind, store};
use files_ui::{FilesApp, WithRow};
use neo_fs::{Fs as _, path};
use neoapp::App;
use neoapp::open::{self as handlers, Handler, files};
use neoui_proto::ipc as proto;
use neoui_proto::settings::Value;
use slint::{ModelRc, VecModel};

/// The setting that names the app that always opens a type (`ext=key;`).
const ALWAYS: &str = "open.always";
/// Where the picker starts.
const START: &str = "C:\\Data\\";

/// What the FA8 callbacks ask.
pub(crate) enum Act {
    OpenWith,
    Share,
    WithPicked(usize),
    SharePicked(usize),
}

/// The open Open with sheet: the file, its extension, and the apps after Files' own row.
pub(crate) struct With {
    path: String,
    ext: String,
    apps: Vec<&'static Handler>,
}

/// The open Share sheet: the file, and its text when it fits the clipboard.
pub(crate) struct Share {
    path: String,
    text: Option<String>,
}

pub(crate) fn bind(ui: &FilesApp, q: &Rc<RefCell<alloc::collections::VecDeque<crate::Action>>>) {
    let push = |q: &Rc<RefCell<alloc::collections::VecDeque<crate::Action>>>| {
        let q = q.clone();
        move |a: Act| q.borrow_mut().push_back(crate::Action::Fa8(a))
    };
    let p = push(q);
    ui.on_open_with(move || p(Act::OpenWith));
    let p = push(q);
    ui.on_share(move || p(Act::Share));
    let p = push(q);
    ui.on_with_picked(move |i| p(Act::WithPicked(i.max(0) as usize)));
    let p = push(q);
    ui.on_share_picked(move |i| p(Act::SharePicked(i.max(0) as usize)));
}

pub(crate) fn act(app: &App, ui: &FilesApp, st: &mut State, a: Act) {
    match a {
        Act::OpenWith => {
            if let Some(p) = current(st) {
                open_with(ui, st, p);
            }
        }
        Act::Share => share(ui, st),
        Act::WithPicked(i) => with_picked(app, ui, st, i),
        Act::SharePicked(i) => share_picked(app, ui, st, i),
    }
}

/// The file in front: the text viewer's or editor's, the image viewer's, an FA7 viewer's.
pub(crate) fn current(st: &State) -> Option<String> {
    match st.view {
        View::Text | View::Edit => st.open.as_ref().map(|o| o.path.clone()),
        View::Image => Some(st.file.clone()).filter(|p| !p.is_empty()),
        View::Fa7 => st.fa7.as_ref().map(|o| o.path.clone()),
        _ => None,
    }
}

fn ext_of(p: &str) -> String {
    neo_view::ext_of(path::name_of(p)).to_ascii_lowercase()
}

/// The Open with sheet for `p` (files2-open-with).
pub(crate) fn open_with(ui: &FilesApp, st: &mut State, p: String) {
    let name = path::name_of(&p).to_string();
    let ext = ext_of(&p);
    let apps: Vec<&'static Handler> = handlers::handlers_for(&ext).collect();
    let family = neo_view::family_of(&name);
    let mut rows = Vec::from([WithRow {
        title: "Files".into(),
        sub: family.viewer().into(),
        colour: files_ui::colour(0xe2a92a),
    }]);
    rows.extend(apps.iter().map(|h| {
        WithRow {
            title: h.label.into(),
            sub: if h.by_path {
                "Gets the file's name"
            } else {
                "Gets the file"
            }
            .into(),
            colour: files_ui::colour(0x8a63d2),
        }
    }));
    ui.set_with_title(format!("Open {name} with").into());
    ui.set_with_rows(ModelRc::new(VecModel::from(rows)));
    ui.set_with_always(false);
    ui.set_share_open(false);
    ui.set_with_open(true);
    st.with = Some(With { path: p, ext, apps });
}

fn with_picked(app: &App, ui: &FilesApp, st: &mut State, i: usize) {
    let Some(w) = st.with.take() else { return };
    ui.set_with_open(false);
    let handler = i.checked_sub(1).and_then(|n| w.apps.get(n)).copied();
    if ui.get_with_always() {
        remember(app, &w.ext, handler.map(|h| h.key));
    }
    match handler {
        Some(h) => {
            launch(app, st, h, &w.path);
        }
        // Files: the viewer it is in, or (from a folder's selection) the file opens here
        None if current(st).as_deref() != Some(w.path.as_str()) => {
            crate::open_here(app, ui, st, &w.path)
        }
        None => {}
    }
}

/// Open `p` in `h`: by handle (`OPEN_FILE`), or by name for an app that reads a path.
fn launch(app: &App, st: &State, h: &Handler, p: &str) -> bool {
    let r = if h.by_path {
        if p.len() > proto::ARG_MAX {
            Err(e32::sys::err::KERR_BAD_NAME)
        } else {
            app.open(h.key, h.page, p)
        }
    } else {
        RFile::open(&st.fs.0, p, mode::READ | mode::SHARE_READERS_ONLY)
            .and_then(|f| app.open_file(h.key, h.page, &f))
    };
    match r {
        Ok(()) => {
            dprintln!("files: open with {} {}", h.key, p);
            true
        }
        Err(e) => {
            dprintln!("files: open with {} failed {}", h.key, e);
            toast(app, "Can't open it there", e);
            false
        }
    }
}

/// The handler "Always" names for `p`'s type, if it still takes it.
fn always_for(app: &App, p: &str) -> Option<&'static Handler> {
    let Ok(Value::Bytes(table)) = app.setting_raw(ALWAYS) else {
        return None;
    };
    let ext = ext_of(p);
    let key = handlers::always(&table, &ext)?;
    handlers::handlers_for(&ext).find(|h| h.key == key)
}

/// "Always" in the sheet: `key` opens `ext` from now on (`None`: Files does).
fn remember(app: &App, ext: &str, key: Option<&str>) {
    let table = match app.setting_raw(ALWAYS) {
        Ok(Value::Bytes(b)) => b,
        _ => Vec::new(),
    };
    let next = handlers::set_always(&table, ext, key);
    if let Err(e) = app.set_setting_raw(ALWAYS, &Value::Bytes(next)) {
        dprintln!("files: open.always not saved {}", e);
    }
}

/// A file tapped in Files: the app "Always" names opens it instead; true when one did.
pub(crate) fn open_always(app: &App, st: &State, p: &str) -> bool {
    always_for(app, p).is_some_and(|h| launch(app, st, h, p))
}

// ── Share ────────────────────────────────────────────────────────────────────────────────────────────────────────

fn share(ui: &FilesApp, st: &mut State) {
    let Some(p) = current(st) else { return };
    let name = path::name_of(&p).to_string();
    let small = st
        .fs
        .entry(&p)
        .is_ok_and(|e| e.size as usize <= proto::CLIP_MAX);
    let text = (is_text(&name) && small)
        .then(|| store::read(&*st.fs, &p).ok())
        .flatten()
        .and_then(|b| String::from_utf8(b).ok());
    ui.set_share_clip(
        if text.is_some() {
            "The text, to paste anywhere"
        } else {
            "The file, to paste in a folder"
        }
        .into(),
    );
    ui.set_share_title(format!("Share {name}").into());
    ui.set_with_open(false);
    ui.set_share_open(true);
    st.share = Some(Share { path: p, text });
}

fn share_picked(app: &App, ui: &FilesApp, st: &mut State, i: usize) {
    let Some(s) = st.share.take() else { return };
    ui.set_share_open(false);
    match i {
        0 => {
            if app.set_clipboard(&s.path) {
                note_with(app, "Path copied", "");
            }
        }
        1 => match &s.text {
            Some(t) if app.set_clipboard(t) => note_with(app, "Text copied", ""),
            _ => {
                st.clip = Some(Clip {
                    kind: Kind::Copy,
                    paths: Vec::from([s.path.clone()]),
                    from: path::parent(&s.path).unwrap_or_default(),
                });
                publish_clip(app, st);
                show_clip(ui, st);
                note_with(app, "Copied: paste it in a folder", "");
            }
        },
        2 => open_with(ui, st, s.path),
        _ => {}
    }
}

// ── the system clipboard holds copied files ──────────────────────────────────────────────────────────────────────

/// Files' clip goes on the system clipboard too (paths, one per line), so it outlives Files.
pub(crate) fn publish_clip(app: &App, st: &State) {
    let Some(c) = &st.clip else { return };
    let kind = match c.kind {
        Kind::Copy => proto::CLIP_PATHS_COPY,
        Kind::Move => proto::CLIP_PATHS_MOVE,
    };
    let paths: Vec<&str> = c.paths.iter().map(String::as_str).collect();
    // over 2 KB of paths: the clip stays Files' own
    let _ = app.set_clipboard_paths(kind, &paths);
}

/// The clip was used or cancelled: take copied files off the system clipboard (text stays).
pub(crate) fn clear_clip(app: &App) {
    if app.clipboard_kind() != proto::CLIP_TEXT {
        app.set_clipboard("");
    }
}

/// Files copied before Files last closed (or in another session) come back as the paste bar.
pub(crate) fn restore_clip(app: &App, ui: &FilesApp, st: &mut State) {
    if st.clip.is_some() {
        return;
    }
    let kind = match app.clipboard_kind() {
        proto::CLIP_PATHS_COPY => Kind::Copy,
        proto::CLIP_PATHS_MOVE => Kind::Move,
        _ => return,
    };
    let paths: Vec<String> = app
        .clipboard()
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(ToString::to_string)
        .collect();
    let Some(first) = paths.first() else { return };
    st.clip = Some(Clip {
        kind,
        from: path::parent(first).unwrap_or_default(),
        paths,
    });
    show_clip(ui, st);
}

// ── Files as the file picker ─────────────────────────────────────────────────────────────────────────────────────

/// Another app opened Files on `page` (`AppEvent::Open`): pick a file, or save one.
pub(crate) fn opened(app: &App, ui: &FilesApp, st: &mut State, page: u16) {
    let arg = app.arg().unwrap_or_default();
    dprintln!("files: opened on page {} {:?}", page, arg);
    match page {
        files::PICK => archive::open_picker(app, ui, st, Purpose::PickFile(arg), START.into()),
        files::SAVE => {
            let name = if arg.is_empty() {
                "New file.txt".to_string()
            } else {
                arg
            };
            archive::open_picker(app, ui, st, Purpose::SaveAs(name, true), START.into());
        }
        _ => {}
    }
}

/// Whether file `name` passes the picker's filter: empty = all; else comma-separated extensions or families
/// (`image`, `text`).
pub(crate) fn passes(filter: &str, name: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let ext = neo_view::ext_of(name);
    let family = neo_view::family_of(name).name().to_ascii_lowercase();
    filter.split(',').map(str::trim).any(|f| {
        !f.is_empty()
            && (ext.eq_ignore_ascii_case(f) || family.starts_with(&f.to_ascii_lowercase()))
    })
}

/// A file tapped in the picker: the app that asked gets it, open for reading.
pub(crate) fn picked(app: &App, st: &State, p: &str) {
    let r = RFile::open(&st.fs.0, p, mode::READ | mode::SHARE_READERS_ONLY)
        .and_then(|f| app.done_file(&f));
    match r {
        Ok(()) => dprintln!("files: picked {}", p),
        Err(e) => {
            toast(app, "Can't open it", e);
            let _ = app.done();
        }
    }
}

/// Save as's name in folder `dir`: for the app that asked (an empty file it writes), or the editor's text.
pub(crate) fn save_as(
    app: &App,
    ui: &FilesApp,
    st: &mut State,
    dir: &str,
    name: &str,
    caller: bool,
) {
    let name = name.trim();
    if name.is_empty() || name.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|']) {
        note_with(app, "Give it a name without \\ / : * ? \" < > |", "");
        return;
    }
    let p = free_name(st, dir, name);
    if caller {
        let r = RFile::replace(&st.fs.0, &p, mode::WRITE | mode::SHARE_ANY)
            .and_then(|f| app.done_file(&f));
        match r {
            Ok(()) => dprintln!("files: saved {} for the caller", p),
            Err(e) => {
                toast(app, "Can't save there", e);
                let _ = app.done();
            }
        }
        return;
    }
    let Some(o) = st.open.as_mut() else { return };
    match o.save_as(&st.fs, &p) {
        Ok(_) => {
            ui.set_name(path::name_of(&p).into());
            note_with(app, &format!("Saved as {}", path::name_of(&p)), "");
        }
        Err(e) => toast(app, "Not saved", e),
    }
}

/// `name` in `dir`, or `stem (n).ext` when that is taken (Keep both).
fn free_name(st: &State, dir: &str, name: &str) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let mut p = path::join_file(dir, name);
    let mut n = 1;
    while st.fs.entry(&p).is_ok() {
        p = path::join_file(dir, &format!("{stem} ({n}){ext}"));
        n += 1;
    }
    p
}

/// Pixel test images (`--features test-files`): `C:\Data\fa8\` with a short note (Copy to clipboard copies its
/// text), contact cards (Open with: Contacts takes them by name), and a note whose path is 298 bytes of UTF-8,
/// more than an OPEN arg holds (Open with sends it by handle).
#[cfg(feature = "test-files")]
pub(crate) fn test_files(st: &State) {
    const DIR: &str = "C:\\Data\\fa8\\";
    const LONG: &str = "Շատ երկար անունով թղթապանակ, որ ճանապարհը 256 բայթից երկար լինի";
    if st.fs.entry(&format!("{DIR}trip-notes.txt")).is_ok() {
        return;
    }
    let long = format!("{DIR}{LONG}\\{LONG}\\նշումներ ճամփորդության մասին.txt");
    let files: [(String, &[u8]); 3] = [
        (
            format!("{DIR}trip-notes.txt"),
            b"Day 1: Yerevan, the Cascade at sunset\nDay 2: Garni and Geghard\n",
        ),
        (
            format!("{DIR}contacts.vcf"),
            include_bytes!("../../../crates/neo-format-pim/tests/data/contacts.vcf"),
        ),
        (
            long,
            "Opened by handle: this path is 298 bytes of UTF-8.\n".as_bytes(),
        ),
    ];
    for (p, b) in files {
        if let Err(e) = store::write(&*st.fs, &p, b) {
            dprintln!("files: test file {} failed {}", p, e.code());
        }
    }
}
