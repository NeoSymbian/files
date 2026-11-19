// SPDX-License-Identifier: GPL-3.0-only
//! Archives (files-app.md FA6): the extract sheet (files2-extract), browsing inside an archive read-only
//! (files2-zip-browse), opening one member from it, the folder picker (Extract to…, Save in) and the Compress form
//! (files2-compress). The work itself runs on the job worker (`job::Work::{Extract, Compress, List}`).

use crate::job::Work;
use crate::{Running, State, View, note_with, now, open_dir, open_path, toast};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use e32::{dprintln, time};
use files_core::{self as model, Item};
use files_ops::Kind;
use files_ops::archive::{
    ArchiveFormat as Format, ArchiveMember as Member, MemberKind, Packing, Pick, estimate,
    format_of, list, needed,
};
use files_ui::{FilesApp, member_row, pack_item};
use neo_fs::{Fs, path};
use neoapp::App;
use slint::{ModelRc, SharedString, VecModel};

/// Where a member opened from an archive is unpacked (emptied at start and before each one).
pub const OPEN_CACHE: &str = "C:\\private\\e0006001\\open\\";
/// A member bigger than this isn't opened from the archive.
const OPEN_MAX: u64 = 64 << 20;

/// A path in the open cache (its viewer is read-only).
pub fn in_cache(p: &str) -> bool {
    p.len() >= OPEN_CACHE.len() && p[..OPEN_CACHE.len()].eq_ignore_ascii_case(OPEN_CACHE)
}

/// The archive the sheet is about.
pub struct Sheet {
    path: String,
    format: Format,
    /// A zip's or tar's members (a tar.gz's come from the worker).
    members: Option<Vec<Member>>,
}

/// Browsing inside an archive.
pub struct Browse {
    path: String,
    format: Format,
    members: Vec<Member>,
    /// The folder inside ("" the top, else `a/b`).
    dir: String,
    /// The rows: (name, folder, size, packed, items in a folder).
    rows: Vec<(String, bool, u64, u64, u32)>,
    pub checked: Vec<bool>,
    query: String,
    /// The folder of Files it was opened from.
    from_dir: String,
}

/// What the picker picks for.
pub enum Purpose {
    Extract(String, Option<Pick>),
    SaveIn,
}

pub struct Picker {
    purpose: Purpose,
    pub dir: String,
    drives: Vec<char>,
    rows: Vec<String>,
    from: View,
    /// Files' open folder then (the picker's New folder borrows `st.dir`).
    from_dir: String,
}

/// The Compress form.
pub struct CompressForm {
    sources: Vec<String>,
    dir: String,
    packing: Packing,
}

/// A running archive job, as poll_job reports it.
#[derive(Clone)]
pub enum ArcJob {
    Extract { archive: String, dest: String },
    Compress { out: String },
    List { archive: String },
    Unpack { path: String },
}

impl ArcJob {
    pub fn verb(&self) -> &'static str {
        match self {
            ArcJob::Extract { .. } => "Extracting",
            ArcJob::Compress { .. } => "Compressing",
            ArcJob::List { .. } => "Reading",
            ArcJob::Unpack { .. } => "Opening",
        }
    }
}

/// An archive Files can open (7z and rar are only recognised).
pub fn is_archive(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    [".zip", ".tar", ".tgz", ".gz"]
        .iter()
        .any(|e| l.ends_with(e))
}

/// The name without its archive extension (backup-2026.zip → backup-2026).
fn stem(name: &str) -> &str {
    let l = name.to_ascii_lowercase();
    for e in [".tar.gz", ".tgz", ".zip", ".tar", ".gz"] {
        if l.ends_with(e) && name.len() > e.len() {
            return &name[..name.len() - e.len()];
        }
    }
    name
}

fn kind_label(f: Format) -> &'static str {
    match f {
        Format::Zip => "Zip archive",
        Format::Tar => "Tar archive",
        Format::TarGz => "Tar.gz archive",
        Format::Gz => "Gzip file",
    }
}

fn free_of(st: &State, letter: char) -> Option<u64> {
    st.fs
        .drives()
        .ok()?
        .into_iter()
        .find(|d| d.letter.eq_ignore_ascii_case(&letter))
        .map(|d| d.free)
}

fn writable(st: &State, p: &str) -> bool {
    let letter = p.chars().next().unwrap_or('Z');
    st.drives
        .iter()
        .any(|&(d, ro)| d.eq_ignore_ascii_case(&letter) && !ro)
}

fn busy(app: &App, st: &State) -> bool {
    if st.running.is_some() {
        note_with(app, "Wait for the running job to finish", "");
        return true;
    }
    false
}

fn start(app: &App, ui: &FilesApp, st: &mut State, work: Work, job: ArcJob, hidden: bool) -> bool {
    let verb = job.verb();
    if let Err(e) = st.slot.start(work) {
        toast(app, "", e);
        return false;
    }
    st.running = Some(Running {
        kind: Kind::Copy,
        purge: false,
        quiet: false,
        dest: String::new(),
        hidden,
        asked: false,
        noted_at: 0,
        arc: Some(job),
        started: time::millis(),
    });
    if !hidden {
        ui.set_job_text(format!("{verb} …").into());
    }
    ui.set_job_progress(0.0);
    true
}

// ── the sheet ────────────────────────────────────────────────────────────────────────────────────────────────────

/// A tap on an archive: the extract sheet (files2-extract).
pub fn sheet(app: &App, ui: &FilesApp, st: &mut State, archive: &str) {
    let format = match format_of(&*st.fs, archive) {
        Ok(f) => f,
        Err(e) => {
            dprintln!("files: archive {} {}", archive, e.code());
            return note_with(
                app,
                "This archive is damaged, or not one Files can open",
                "",
            );
        }
    };
    let members = match format {
        Format::Zip | Format::Tar => match list(&*st.fs, archive) {
            Ok((_, m)) => Some(m),
            Err(e) => {
                dprintln!("files: archive {} {}", archive, e.code());
                return note_with(
                    app,
                    "This archive is damaged, or not one Files can open",
                    "",
                );
            }
        },
        _ => None,
    };
    let size = st.fs.entry(archive).map_or(0, |e| e.size);
    let dir = path::parent(archive).unwrap_or_default();
    let name = path::name_of(archive);
    let letter = archive.chars().next().unwrap_or('C');
    let free = free_of(st, letter).unwrap_or(0);
    let mut info = format!("{} · {}", kind_label(format), model::size_text(size));
    if let Some(m) = &members {
        let (bytes, files) = needed(m, None);
        info.push_str(&format!(
            " · {} files · needs {} free ({letter}: has {})",
            files_ui::thousands(files as u64),
            model::size_text(bytes),
            model::size_text(free)
        ));
    }
    ui.set_extract_title(name.into());
    ui.set_extract_info(info.into());
    ui.set_extract_here(
        if format == Format::Gz {
            format!("To {dir}")
        } else {
            format!("To {dir}{}\\", stem(name))
        }
        .into(),
    );
    ui.set_extract_browse(format != Format::Gz);
    let mine = matches!(&st.running, Some(Running { arc: Some(ArcJob::Extract { archive: a, .. }), .. }) if a == archive);
    ui.set_extract_busy(mine);
    st.sheet = Some(Sheet {
        path: archive.into(),
        format,
        members,
    });
    dprintln!("files: archive {} {}", archive, format.label());
    ui.set_extract_open(true);
}

/// The sheet's rows: 0 Extract here, 1 Extract to…, 2 Browse inside.
pub fn way(app: &App, ui: &FilesApp, st: &mut State, i: usize) {
    let Some(s) = st.sheet.as_ref() else { return };
    let archive = s.path.clone();
    match i {
        0 => extract_here(app, ui, st, &archive, None),
        1 => {
            ui.set_extract_open(false);
            let from = path::parent(&archive).unwrap_or_default();
            open_picker(app, ui, st, Purpose::Extract(archive, None), from);
        }
        _ => {
            let (format, members) = (s.format, s.members.clone());
            match members {
                Some(m) => {
                    ui.set_extract_open(false);
                    browse(app, ui, st, &archive, format, m);
                }
                None if format == Format::TarGz => {
                    if busy(app, st) {
                        return;
                    }
                    ui.set_extract_open(false);
                    dprintln!("files: list {}", archive);
                    let job = ArcJob::List {
                        archive: archive.clone(),
                    };
                    start(app, ui, st, Work::List(archive), job, false);
                }
                None => {}
            }
        }
    }
}

/// Extract here: into `<folder>\<stem>\` (a new name when that is taken; a .gz's one file into the folder itself).
fn extract_here(app: &App, ui: &FilesApp, st: &mut State, archive: &str, pick: Option<Pick>) {
    let dir = path::parent(archive).unwrap_or_default();
    if !writable(st, &dir) {
        ui.set_extract_open(false);
        let from = dir.clone();
        return open_picker(app, ui, st, Purpose::Extract(archive.into(), pick), from);
    }
    let format = st
        .sheet
        .as_ref()
        .map(|s| s.format)
        .or_else(|| format_of(&*st.fs, archive).ok());
    let dest = if format == Some(Format::Gz) {
        dir
    } else {
        let base = stem(path::name_of(archive)).to_string();
        let mut name = base.clone();
        let mut n = 1;
        while st.fs.entry(&path::join_dir(&dir, &name)).is_ok() {
            name = format!("{base} ({n})");
            n += 1;
        }
        path::join_dir(&dir, &name)
    };
    extract_to(app, ui, st, archive, pick, &dest);
}

/// Start extracting into `dest`, after the free-space check (a zip or tar says what it needs).
fn extract_to(
    app: &App,
    ui: &FilesApp,
    st: &mut State,
    archive: &str,
    pick: Option<Pick>,
    dest: &str,
) {
    if busy(app, st) {
        return;
    }
    let letter = dest.chars().next().unwrap_or('C');
    let members = match (&st.sheet, &st.archive) {
        (Some(s), _) if s.path == archive => s.members.clone(),
        (_, Some(b)) if b.path == archive => Some(b.members.clone()),
        _ => None,
    };
    if let (Some(m), Some(free)) = (members, free_of(st, letter)) {
        let (bytes, _) = needed(&m, pick.as_ref());
        if bytes + (1 << 20) > free {
            return note_with(
                app,
                &format!(
                    "Needs {} free; {letter}: has {}",
                    model::size_text(bytes),
                    model::size_text(free)
                ),
                "",
            );
        }
    }
    dprintln!("files: extract {} to {}", archive, dest);
    let job = ArcJob::Extract {
        archive: archive.into(),
        dest: dest.into(),
    };
    let work = Work::Extract {
        archive: archive.into(),
        pick,
        dest: dest.into(),
        replace: false,
    };
    if start(app, ui, st, work, job, false) {
        // the sheet shows the job's card while it is open
        ui.set_extract_busy(st.sheet.as_ref().is_some_and(|s| s.path == archive));
        ui.set_job_title("Extracting…".into());
        ui.set_job_detail(SharedString::new());
    }
}

/// The bar under an archive's list: Extract all (here) or Extract to….
pub fn bar(app: &App, ui: &FilesApp, st: &mut State, to: bool) {
    let Some(b) = st.archive.as_ref() else { return };
    let archive = b.path.clone();
    if to {
        let from = b.from_dir.clone();
        open_picker(app, ui, st, Purpose::Extract(archive, None), from);
    } else {
        extract_here(app, ui, st, &archive, None);
    }
}

// ── browsing inside ──────────────────────────────────────────────────────────────────────────────────────────────

pub fn browse(
    app: &App,
    ui: &FilesApp,
    st: &mut State,
    archive: &str,
    format: Format,
    members: Vec<Member>,
) {
    dprintln!("files: browse {} {}", archive, members.len());
    let from_dir = path::parent(archive).unwrap_or_default();
    crate::fa3::opened(st, archive);
    st.archive = Some(Browse {
        path: archive.into(),
        format,
        members,
        dir: String::new(),
        rows: Vec::new(),
        checked: Vec::new(),
        query: String::new(),
        from_dir,
    });
    st.view = View::Archive;
    let _ = app;
    show(ui, st);
}

/// Hidden in the list: macOS's resource forks and what can't be extracted safely.
fn listed(m: &Member) -> bool {
    matches!(m.kind, MemberKind::File | MemberKind::Dir)
        && !m.name.starts_with("__MACOSX")
        && !m.name.rsplit('/').next().is_some_and(|n| n == ".DS_Store")
}

/// The rows of the folder inside, folders first by name (or the matches of a search, in any folder below).
pub fn show(ui: &FilesApp, st: &mut State) {
    let Some(b) = st.archive.as_mut() else { return };
    let prefix = if b.dir.is_empty() {
        String::new()
    } else {
        format!("{}/", b.dir)
    };
    let q = b.query.to_lowercase();
    let mut rows: Vec<(String, bool, u64, u64, u32)> = Vec::new();
    for m in b.members.iter().filter(|m| listed(m)) {
        let Some(rel) = m.name.strip_prefix(prefix.as_str()) else {
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        if !q.is_empty() {
            if m.kind == MemberKind::File && rel.to_lowercase().contains(&q) {
                rows.push((rel.into(), false, m.size, m.packed, 0));
            }
            continue;
        }
        let (first, deeper) = match rel.find('/') {
            Some(i) => (&rel[..i], true),
            None => (rel, false),
        };
        let folder = deeper || m.kind == MemberKind::Dir;
        match rows.iter_mut().find(|r| r.0 == first && r.1 == folder) {
            Some(r) if folder => {
                // a direct child counts once (deeper ones count for their own folder)
                if deeper && !rel[first.len() + 1..].contains('/') {
                    r.4 += 1;
                }
            }
            Some(_) => {}
            None => rows.push((
                first.into(),
                folder,
                m.size,
                m.packed,
                u32::from(deeper && !rel[first.len() + 1..].contains('/')),
            )),
        }
    }
    rows.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    b.checked = vec![false; rows.len()];
    b.rows = rows;
    paint(ui, st);
}

/// The rows and the header (after a pick changed).
pub fn paint(ui: &FilesApp, st: &State) {
    let Some(b) = st.archive.as_ref() else { return };
    let rows: Vec<files_ui::FileRow> = b
        .rows
        .iter()
        .zip(&b.checked)
        .map(|((n, f, s, p, c), k)| member_row(n, *f, *s, *p, *c, *k))
        .collect();
    let picked = b.checked.iter().filter(|c| **c).count();
    ui.set_selected(picked as i32);
    ui.set_one_item(picked == 1);
    ui.set_one_folder(false);
    ui.set_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
    let name = path::name_of(&b.path);
    let mut crumbs: Vec<SharedString> = vec![name.into()];
    crumbs.extend(
        b.dir
            .split('/')
            .filter(|s| !s.is_empty())
            .map(SharedString::from),
    );
    ui.set_crumbs(ModelRc::from(Rc::new(VecModel::from(crumbs))));
    ui.set_name(name.into());
    ui.set_read_only(true);
    let (bytes, files) = needed(&b.members, None);
    ui.set_archive_count(
        if files == 1 {
            "1 file".to_string()
        } else {
            format!("{} files", files_ui::thousands(files as u64))
        }
        .into(),
    );
    ui.set_archive_sub(format!("{} · {}", model::size_text(bytes), b.format.label()).into());
    ui.set_view("archive".into());
}

fn full(b: &Browse, rel: &str) -> String {
    if b.dir.is_empty() {
        rel.into()
    } else {
        format!("{}/{rel}", b.dir)
    }
}

/// A row: into a folder, or a file opened (unpacked into the cache first).
pub fn row(app: &App, ui: &FilesApp, st: &mut State, i: usize) {
    let Some(b) = st.archive.as_mut() else { return };
    let Some((name, folder, size, ..)) = b.rows.get(i).cloned() else {
        return;
    };
    if folder {
        b.dir = full(b, &name);
        b.query.clear();
        ui.set_searching(false);
        return show(ui, st);
    }
    let member = full(b, &name);
    let archive = b.path.clone();
    if size > OPEN_MAX {
        return note_with(app, "Too big to open from the archive: extract it", "");
    }
    if busy(app, st) {
        return;
    }
    let base = match member.rfind('/') {
        Some(k) => member[..k].to_string(),
        None => String::new(),
    };
    let leaf = member.rsplit('/').next().unwrap_or(&member).to_string();
    // only this one in the cache (the image viewer lists the cache's images)
    if let Ok(old) = st.fs.read_dir(OPEN_CACHE) {
        for e in old.iter().filter(|e| !e.is_dir()) {
            let _ = st.fs.delete(&path::join_file(OPEN_CACHE, &e.name));
        }
    } else {
        let _ = st.fs.mkdir(OPEN_CACHE);
    }
    dprintln!("files: unpack {} {}", archive, member);
    let work = Work::Extract {
        archive,
        pick: Some(Pick {
            base,
            names: vec![member],
        }),
        dest: OPEN_CACHE.into(),
        replace: true,
    };
    let job = ArcJob::Unpack {
        path: path::join_file(OPEN_CACHE, &leaf),
    };
    start(app, ui, st, work, job, false);
}

/// Up a folder inside; at the top, back to the folder the archive is in.
pub fn up(app: &App, ui: &FilesApp, st: &mut State) {
    let Some(b) = st.archive.as_mut() else { return };
    if !b.query.is_empty() {
        b.query.clear();
        ui.set_searching(false);
        ui.set_query(SharedString::new());
        return show(ui, st);
    }
    if b.dir.is_empty() {
        let dir = b.from_dir.clone();
        st.archive = None;
        return open_dir(app, ui, st, dir);
    }
    b.dir = match b.dir.rfind('/') {
        Some(k) => b.dir[..k].into(),
        None => String::new(),
    };
    show(ui, st);
}

pub fn crumb(ui: &FilesApp, st: &mut State, i: usize) {
    let Some(b) = st.archive.as_mut() else { return };
    b.dir = b.dir.split('/').take(i).collect::<Vec<_>>().join("/");
    b.query.clear();
    show(ui, st);
}

pub fn searched(ui: &FilesApp, st: &mut State, q: &str) {
    if let Some(b) = st.archive.as_mut() {
        b.query = q.trim().into();
        show(ui, st);
    }
}

pub fn toggle(ui: &FilesApp, st: &mut State, i: usize) {
    if let Some(c) = st.archive.as_mut().and_then(|b| b.checked.get_mut(i)) {
        *c = !*c;
        paint(ui, st);
    }
}

pub fn select_all(ui: &FilesApp, st: &mut State, on: bool) {
    if let Some(b) = st.archive.as_mut() {
        b.checked.iter_mut().for_each(|c| *c = on);
        paint(ui, st);
    }
}

/// The selection's Extract: the picked rows, to a folder of your choice.
pub fn extract_picked(app: &App, ui: &FilesApp, st: &mut State) {
    let Some(b) = st.archive.as_ref() else { return };
    let names: Vec<String> = b
        .rows
        .iter()
        .zip(&b.checked)
        .filter(|(_, c)| **c)
        .map(|(r, _)| full(b, &r.0))
        .collect();
    if names.is_empty() {
        return;
    }
    let pick = Pick {
        base: b.dir.clone(),
        names,
    };
    let (archive, from) = (b.path.clone(), b.from_dir.clone());
    select_all(ui, st, false);
    open_picker(app, ui, st, Purpose::Extract(archive, Some(pick)), from);
}

/// The archive's Details.
pub fn details(app: &App, ui: &FilesApp, st: &mut State) {
    if let Some(p) = st.archive.as_ref().map(|b| b.path.clone()) {
        crate::fa3::details(app, ui, st, &p);
    }
}

// ── the folder picker ────────────────────────────────────────────────────────────────────────────────────────────

pub fn open_picker(app: &App, ui: &FilesApp, st: &mut State, purpose: Purpose, start_dir: String) {
    let drives: Vec<char> = st.drives.iter().filter(|d| !d.1).map(|d| d.0).collect();
    let Some(&first) = drives.first() else {
        return note_with(app, "No drive to write to", "");
    };
    let ok = writable(st, &start_dir)
        && (path::parent(&start_dir).is_none() || st.fs.entry(&start_dir).is_ok());
    let dir = if ok { start_dir } else { format!("{first}:\\") };
    let from = st.view;
    st.picker = Some(Picker {
        purpose,
        dir,
        drives,
        rows: Vec::new(),
        from,
        from_dir: st.dir.clone(),
    });
    // the Compress form waits under the picker
    ui.set_form(SharedString::new());
    st.view = View::PickFolder;
    picker_list(app, ui, st);
}

pub fn picker_list(app: &App, ui: &FilesApp, st: &mut State) {
    let hidden = st.hidden;
    let Some(p) = st.picker.as_mut() else { return };
    let mut names: Vec<Item> = match st.fs.read_dir(&p.dir) {
        Ok(v) => v
            .into_iter()
            .filter(|e| e.is_dir())
            .map(|e| Item {
                name: e.name,
                is_dir: true,
                att: e.att,
                ..Item::default()
            })
            .collect(),
        Err(e) => {
            toast(app, "", e.code());
            Vec::new()
        }
    };
    names = model::visible(names, hidden);
    names.sort_by_key(|a| a.name.to_lowercase());
    p.rows = names.into_iter().map(|i| i.name).collect();
    let letter = p.dir.chars().next().unwrap_or('C');
    let rows: Vec<files_ui::PackItem> = p.rows.iter().map(|n| pack_item(n, true, "")).collect();
    ui.set_picker_title("Choose a folder".into());
    ui.set_picker_drives(ModelRc::from(Rc::new(VecModel::from(
        p.drives
            .iter()
            .map(|d| SharedString::from(format!("{d}:")))
            .collect::<Vec<_>>(),
    ))));
    ui.set_picker_drive(p.drives.iter().position(|&d| d == letter).unwrap_or(0) as i32);
    let crumbs: Vec<SharedString> = model::crumbs(&p.dir)
        .into_iter()
        .map(|(l, _)| l.as_str().into())
        .collect();
    ui.set_picker_crumbs(ModelRc::from(Rc::new(VecModel::from(crumbs))));
    ui.set_picker_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
    ui.set_picker_action(
        match p.purpose {
            Purpose::Extract(..) => "Extract here",
            Purpose::SaveIn => "Save here",
        }
        .into(),
    );
    let free =
        free_of(st, letter).map_or(String::new(), |f| format!("{} free", model::size_text(f)));
    ui.set_picker_info(free.into());
    ui.set_picker_can(true);
    ui.set_view("pick-folder".into());
}

pub fn picker_open(app: &App, ui: &FilesApp, st: &mut State, i: usize) {
    let Some(p) = st.picker.as_mut() else { return };
    if let Some(n) = p.rows.get(i) {
        p.dir = path::join_dir(&p.dir, n);
        picker_list(app, ui, st);
    }
}

pub fn picker_crumb(app: &App, ui: &FilesApp, st: &mut State, i: usize) {
    let Some(p) = st.picker.as_mut() else { return };
    if let Some((_, d)) = model::crumbs(&p.dir).get(i) {
        p.dir = d.clone();
        picker_list(app, ui, st);
    }
}

pub fn picker_drive(app: &App, ui: &FilesApp, st: &mut State, i: usize) {
    let Some(p) = st.picker.as_mut() else { return };
    if let Some(d) = p.drives.get(i) {
        p.dir = format!("{d}:\\");
        picker_list(app, ui, st);
    }
}

/// ✕ / Back: where it was opened from.
pub fn picker_cancel(app: &App, ui: &FilesApp, st: &mut State) {
    let Some(p) = st.picker.take() else { return };
    leave_picker(app, ui, st, p.from, p.from_dir);
}

fn leave_picker(app: &App, ui: &FilesApp, st: &mut State, from: View, from_dir: String) {
    st.view = from;
    st.dir = from_dir;
    if st.compress.is_some() {
        ui.set_form("compress".into());
    }
    match from {
        View::Archive => show(ui, st),
        _ => {
            let dir = st.dir.clone();
            if dir.is_empty() {
                crate::show_tab(app, ui, st, st.tab);
            } else {
                open_dir(app, ui, st, dir);
            }
        }
    }
}

/// ✓ / the action: extract into the folder, or set the Compress form's Save in.
pub fn picker_done(app: &App, ui: &FilesApp, st: &mut State) {
    let Some(p) = st.picker.take() else { return };
    let dir = p.dir.clone();
    leave_picker(app, ui, st, p.from, p.from_dir);
    match p.purpose {
        Purpose::Extract(archive, pick) => {
            // everything goes into a folder of its own, unless it is one file or a pick
            let dest = match (&pick, st.sheet.as_ref().map(|s| s.format)) {
                (None, f) if f != Some(Format::Gz) => {
                    let base = stem(path::name_of(&archive)).to_string();
                    let mut name = base.clone();
                    let mut n = 1;
                    while st.fs.entry(&path::join_dir(&dir, &name)).is_ok() {
                        name = format!("{base} ({n})");
                        n += 1;
                    }
                    path::join_dir(&dir, &name)
                }
                _ => dir,
            };
            extract_to(app, ui, st, &archive, pick, &dest);
        }
        Purpose::SaveIn => {
            if let Some(c) = st.compress.as_mut() {
                c.dir = dir;
            }
            compress_show(ui, st);
        }
    }
}

/// New folder in the picker: the folder form, over it.
pub fn picker_new_folder(app: &App, ui: &FilesApp, st: &mut State) {
    let Some(p) = st.picker.as_ref() else { return };
    st.dir = p.dir.clone();
    st.read_only = false;
    st.items = p
        .rows
        .iter()
        .map(|n| Item {
            name: n.clone(),
            is_dir: true,
            ..Item::default()
        })
        .collect();
    crate::open_form(app, ui, st, crate::Form::NewFolder);
}

// ── Compress ─────────────────────────────────────────────────────────────────────────────────────────────────────

/// ⋮ Compress on a selection (files2-compress): the name from the one item, else Archive.zip; saved beside them.
pub fn compress_open(app: &App, ui: &FilesApp, st: &mut State, sources: Vec<String>) {
    if sources.is_empty() || busy(app, st) {
        return;
    }
    let dir = if writable(st, &st.dir) {
        st.dir.clone()
    } else {
        String::from("C:\\")
    };
    let base = match sources.as_slice() {
        [one] => {
            let n = path::name_of(one);
            if one.ends_with('\\') {
                n.to_string()
            } else {
                model::split_name(n).0.to_string()
            }
        }
        _ => "Archive".to_string(),
    };
    let taken: Vec<String> = st
        .fs
        .read_dir(&dir)
        .map(|v| v.into_iter().map(|e| e.name).collect())
        .unwrap_or_default();
    let taken: Vec<&str> = taken.iter().map(String::as_str).collect();
    ui.set_name_text(model::free_name(&taken, &base, ".zip").into());
    ui.set_compress_title(
        if sources.len() == 1 {
            "Compress 1 item".to_string()
        } else {
            format!("Compress {} items", sources.len())
        }
        .into(),
    );
    let items: Vec<files_ui::PackItem> = sources
        .iter()
        .map(|s| {
            let folder = s.ends_with('\\');
            let e = st.fs.entry(s).ok();
            let detail = e.map_or(String::new(), |e| {
                files_core::detail_text(
                    &Item {
                        name: e.name.clone(),
                        is_dir: folder,
                        size: e.size,
                        modified: e.modified,
                        ..Item::default()
                    },
                    now(),
                )
            });
            pack_item(path::name_of(s), folder, &detail)
        })
        .collect();
    ui.set_compress_items(ModelRc::from(Rc::new(VecModel::from(items))));
    let (total, expect, packed) = estimate(&*st.fs, &sources).unwrap_or((0, 0, 0));
    ui.set_compress_total(model::size_text(total).into());
    let family = |s: &String| neo_view::family_of(path::name_of(s));
    let what = match sources.first().map(family) {
        Some(f)
            if !sources.iter().any(|s| s.ends_with('\\'))
                && sources.iter().all(|s| family(s) == f) =>
        {
            match f {
                neo_view::Family::Audio => Some("Audio is"),
                neo_view::Family::Video => Some("Videos are"),
                neo_view::Family::Image => Some("Photos are"),
                neo_view::Family::Archive => Some("Archives are"),
                _ => None,
            }
        }
        _ => None,
    };
    let note = match what {
        Some(w) if packed * 2 > total => format!(
            "{w} already compressed: expect about {}",
            model::size_text(expect)
        ),
        _ if packed * 2 > total => format!(
            "Most of this is already compressed: expect about {}",
            model::size_text(expect)
        ),
        _ => format!("Expect about {}", model::size_text(expect)),
    };
    ui.set_compress_note(note.into());
    st.compress = Some(CompressForm {
        sources,
        dir,
        packing: Packing::Zip,
    });
    ui.set_compress_format(0);
    compress_show(ui, st);
    ui.set_form("compress".into());
}

fn compress_show(ui: &FilesApp, st: &State) {
    let Some(c) = st.compress.as_ref() else {
        return;
    };
    ui.set_compress_dir(c.dir.trim_end_matches('\\').into());
    let letter = c.dir.chars().next().unwrap_or('C');
    ui.set_compress_free(
        free_of(st, letter)
            .map_or(String::new(), |f| format!("{} free", model::size_text(f)))
            .into(),
    );
}

/// The format chips: the name's extension follows.
pub fn compress_format(ui: &FilesApp, st: &mut State, i: usize) {
    let Some(c) = st.compress.as_mut() else {
        return;
    };
    c.packing = if i == 1 { Packing::TarGz } else { Packing::Zip };
    ui.set_compress_format(i.min(1) as i32);
    let name = ui.get_name_text().to_string();
    let l = name.to_ascii_lowercase();
    let base = if l.ends_with(".tar.gz") {
        &name[..name.len() - 7]
    } else if l.ends_with(".zip") || l.ends_with(".tgz") {
        &name[..name.len() - 4]
    } else {
        &name
    };
    let ext = if c.packing == Packing::Zip {
        ".zip"
    } else {
        ".tar.gz"
    };
    ui.set_name_text(format!("{base}{ext}").into());
}

pub fn save_in(app: &App, ui: &FilesApp, st: &mut State) {
    let Some(c) = st.compress.as_ref() else {
        return;
    };
    let dir = c.dir.clone();
    open_picker(app, ui, st, Purpose::SaveIn, dir);
}

/// ✕ / Back on the form.
pub fn compress_cancel(ui: &FilesApp, st: &mut State) {
    st.compress = None;
    ui.set_form(SharedString::new());
}

/// ✓: the archive is made on the worker.
pub fn compress_save(app: &App, ui: &FilesApp, st: &mut State) {
    let Some(c) = st.compress.as_ref() else {
        return;
    };
    let mut name = ui.get_name_text().trim().to_string();
    let ext = if c.packing == Packing::Zip {
        ".zip"
    } else {
        ".tar.gz"
    };
    if !name.to_ascii_lowercase().ends_with(ext) {
        name.push_str(ext);
    }
    if !model::valid_name(&name) {
        return note_with(app, "That name can't be used", "");
    }
    let out = path::join_file(&c.dir, &name);
    if st.fs.entry(&out).is_ok() {
        return note_with(app, &format!("There is a {name} there already"), "");
    }
    if busy(app, st) {
        return;
    }
    let Some(c) = st.compress.take() else { return };
    crate::close_form(app, ui, st);
    dprintln!("files: compress {} to {}", c.sources.len(), out);
    let job = ArcJob::Compress { out: out.clone() };
    start(
        app,
        ui,
        st,
        Work::Compress(c.sources, out, c.packing),
        job,
        false,
    );
}

// ── the end of a job ─────────────────────────────────────────────────────────────────────────────────────────────

/// An archive job ended.
pub fn done(
    app: &App,
    ui: &FilesApp,
    st: &mut State,
    job: ArcJob,
    r: Result<files_ops::Summary, neo_fs::Error>,
) {
    ui.set_extract_busy(false);
    let cancelled = matches!(r, Err(neo_fs::Error::Cancelled));
    match (job, r) {
        (ArcJob::Extract { archive, dest }, Ok(s)) => {
            dprintln!("files: extract {} {} to {} done", archive, s.copied, dest);
            ui.set_extract_open(false);
            let skipped = if s.skipped > 0 {
                format!(", {} skipped", s.skipped)
            } else {
                String::new()
            };
            let what = if s.copied == 1 {
                "1 file extracted".to_string()
            } else {
                format!("{} files extracted", files_ui::thousands(s.copied as u64))
            };
            note_with(app, &format!("{what}{skipped}"), "");
            if st.view == View::Folder {
                open_dir(app, ui, st, dest);
            }
        }
        (ArcJob::Compress { out }, Ok(s)) => {
            let size = st.fs.entry(&out).map_or(0, |e| e.size);
            dprintln!(
                "files: compress {} files to {} {} done",
                s.copied,
                out,
                size
            );
            note_with(
                app,
                &format!("{} made · {}", path::name_of(&out), model::size_text(size)),
                "",
            );
            if st.view == View::Folder {
                crate::relist(app, ui, st);
            }
        }
        (ArcJob::List { archive }, Ok(_)) => {
            let members = st.slot.shared.lock().members.take().unwrap_or_default();
            dprintln!("files: list {} {}", archive, members.len());
            if let Some(s) = st.sheet.as_mut().filter(|s| s.path == archive) {
                s.members = Some(members.clone());
            }
            browse(app, ui, st, &archive, Format::TarGz, members);
        }
        (ArcJob::Unpack { path }, Ok(_)) => {
            dprintln!("files: unpack {} done", path);
            open_path(app, ui, st, &path);
        }
        (job, Err(e)) => {
            let what = match &job {
                ArcJob::Extract { archive, .. } | ArcJob::List { archive } => archive.clone(),
                ArcJob::Compress { out } => out.clone(),
                ArcJob::Unpack { path } => path.clone(),
            };
            dprintln!("files: {} {} failed {}", job.verb(), what, e.code());
            if !cancelled {
                match e {
                    // KErrCorrupt: our archive readers' "damaged"
                    neo_fs::Error::Other(-20) => note_with(app, "The archive is damaged", ""),
                    // KErrOverflow: a zip can't hold that much (4 GB, 65,535 files)
                    neo_fs::Error::Other(-9) => {
                        note_with(app, "Too much for one zip: try tar.gz", "")
                    }
                    e => toast(app, "", e.code()),
                }
            }
            if let ArcJob::Compress { .. } = job
                && st.view == View::Folder
            {
                crate::relist(app, ui, st);
            }
        }
    }
}

/// The sheet's card and the toast while an archive job runs: "128 of 412 files · Documents/x · about 2 min left".
pub fn progress_text(job: &ArcJob, p: &files_ops::Progress, started: u64) -> String {
    let mut parts = Vec::new();
    match job {
        ArcJob::List { .. } => parts.push(format!(
            "{} found",
            files_ui::thousands(p.files_done as u64)
        )),
        _ if p.files_total > 0 => parts.push(format!(
            "{} of {} files",
            files_ui::thousands(p.files_done as u64),
            files_ui::thousands(p.files_total as u64)
        )),
        _ => parts.push(format!(
            "{} files",
            files_ui::thousands(p.files_done as u64)
        )),
    }
    if !p.current.is_empty() {
        parts.push(p.current.clone());
    }
    let ms = time::millis().saturating_sub(started);
    if p.bytes_done > 0 && p.bytes_total > p.bytes_done && ms > 3000 {
        let left =
            (p.bytes_total - p.bytes_done) as u128 * ms as u128 / p.bytes_done as u128 / 1000;
        parts.push(if left < 60 {
            "under a minute left".into()
        } else {
            format!("about {} min left", (left + 30) / 60)
        });
    }
    parts.join(" · ")
}

/// At start: what was opened from archives last time goes.
pub fn clear_cache(st: &State) {
    if let Ok(v) = st.fs.read_dir(OPEN_CACHE) {
        for e in v.iter().filter(|e| !e.is_dir()) {
            let _ = st.fs.delete(&path::join_file(OPEN_CACHE, &e.name));
        }
    }
}
