// SPDX-License-Identifier: GPL-3.0-only
//! FA3, "find and tidy" (docs files-app.md): Favourites and the Recent strip on Browse, the Recent and Types tabs,
//! search over the file index with its filters, the Recycle bin (restore, delete for good, empty, 30 days), Free up
//! space (large files, old downloads, duplicates) and Details. The index comes from [`scan`]; deleting for good runs
//! on the job worker.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use e32::dprintln;
use files_core::{self as model, Item, Modified, Recent};
use files_ops::index::{Group, Index, Query};
use files_ops::{store, trash};
use neo_fs::{Fs as _, att, path};
use neoapp::App;
use slint::{ModelRc, SharedString, VecModel};

use crate::{Ask, State, View, ask, note_with, now, scan, toast};
use files_ui::{CleanCard, Detail, FileRow, FilesApp};

const RECENT: &str = "C:\\private\\e0006001\\recent.dat";
const FAVS: &str = "C:\\private\\e0006001\\favs.dat";
/// Free up space's thresholds.
const LARGE: u64 = 50 << 20;
const OLD_DAYS: i64 = 90;
/// Rows a list draws at most (they aren't virtualised); the rest is counted below it.
// shortcut: a capped list, not a virtual one; upgrade when NeoList recycles rows
const ROWS_MAX: usize = 300;

/// One row of Recent, a type page, the bin or a review: where it is and how it reads.
#[derive(Clone, Default)]
pub struct Listed {
    pub path: String,
    pub item: Item,
    pub detail: String,
    pub section: String,
    pub note: String,
}

/// What Free up space reviews.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Review {
    Large,
    Old,
    Dupes,
}

/// The search panel's choices.
#[derive(Default)]
pub struct Search {
    pub text: String,
    pub in_folder: bool,
    pub kind: usize,
    pub size: usize,
    pub modified: usize,
    pub hits: Vec<(String, Item)>,
}

fn model_of<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

fn strings(v: &[String]) -> ModelRc<SharedString> {
    model_of(v.iter().map(|s| SharedString::from(s.as_str())).collect())
}

/// `E:\Images\Camera` for a path in it.
fn place(path: &str) -> String {
    path::parent(path).map_or_else(String::new, |p| {
        if p.len() > 3 {
            p.trim_end_matches('\\').to_string()
        } else {
            p
        }
    })
}

fn item_at(e: &neo_fs::Entry) -> Item {
    Item {
        name: e.name.clone(),
        is_dir: e.is_dir(),
        size: e.size,
        modified: e.modified,
        att: e.att,
        count: None,
    }
}

fn indexed(ix: &Index, i: usize) -> (String, Item) {
    let it = &ix.items[i];
    (
        ix.path(i),
        Item {
            name: it.name.clone(),
            is_dir: it.is_dir(),
            size: it.size,
            modified: it.modified,
            att: it.att,
            count: it.is_dir().then_some(it.count),
        },
    )
}

/// `size · when · where` (files2-recent, -type-docs), or `186 items · where` for a folder.
fn placed(item: &Item, middle: &str, at: &str) -> String {
    let first = if item.is_dir {
        model::detail_text(item, 0)
    } else {
        model::size_text(item.size)
    };
    if middle.is_empty() {
        format!("{first} · {at}")
    } else {
        format!("{first} · {middle} · {at}")
    }
}

// ── the lists Files keeps (recent, favourites) ───────────────────────────────────────────────────────────────────

pub fn load(st: &mut State) {
    let text = |file| {
        store::read(&*st.fs, file)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default()
    };
    st.recent = Recent::decode(&text(RECENT));
    st.favs = model::favs_decode(&text(FAVS));
    st.index = Index::load(&*st.fs, scan::INDEX);
    dprintln!(
        "files: recent {} favs {} index {}",
        st.recent.items.len(),
        st.favs.len(),
        st.index.items.len()
    );
}

pub fn opened(st: &mut State, path: &str) {
    st.recent.add(path, now());
    let _ = store::write(&*st.fs, RECENT, st.recent.encode().as_bytes());
}

fn save_favs(st: &State) {
    let _ = store::write(&*st.fs, FAVS, model::favs_encode(&st.favs).as_bytes());
}

pub fn add_fav(app: &App, st: &mut State, dir: String) {
    if !st.favs.iter().any(|f| f.eq_ignore_ascii_case(&dir)) {
        st.favs.push(dir);
        save_favs(st);
    }
    note_with(app, "Added to Favourites", "");
}

pub fn remove_fav(ui: &FilesApp, st: &mut State, i: usize) {
    if i < st.favs.len() {
        st.favs.remove(i);
        save_favs(st);
    }
    show_favs(ui, st);
}

/// Favourites with their item counts (`9 items`): on Browse only those that exist; with `paths` (the Edit page)
/// all of them, the path in the detail, so ✕ on row i removes `st.favs[i]` (a gone one too).
fn fav_rows(st: &State, paths: bool) -> Vec<FileRow> {
    st.favs
        .iter()
        .filter_map(|f| {
            let n = match st.fs.read_dir(f) {
                Ok(n) => n,
                Err(_) if paths => Vec::new(),
                Err(_) => return None,
            };
            let item = Item {
                name: model::folder_name(f).into(),
                is_dir: true,
                count: Some(
                    model::visible(n.iter().map(item_at).collect(), st.hidden).len() as u32,
                ),
                ..Item::default()
            };
            let mut r = files_ui::file_row(&item, 0, false);
            if paths {
                r.detail = f.as_str().into();
            }
            Some(r)
        })
        .collect()
}

/// The Recent list without what is gone (taken off for good).
fn recent_live(st: &mut State) -> Vec<(String, Item, i64)> {
    let mut live = Vec::new();
    let mut gone = Vec::new();
    for (p, t) in &st.recent.items {
        match st.fs.entry(p) {
            Ok(e) => live.push((p.clone(), item_at(&e), *t)),
            Err(_) => gone.push(p.clone()),
        }
    }
    if !gone.is_empty() {
        gone.iter().for_each(|p| st.recent.remove(p));
        let _ = store::write(&*st.fs, RECENT, st.recent.encode().as_bytes());
    }
    live
}

/// files2-browse's lower half.
pub fn browse_extras(ui: &FilesApp, st: &mut State) {
    let mut favs = fav_rows(st, false);
    favs.truncate(3);
    ui.set_favs(model_of(favs));
    let strip: Vec<FileRow> = recent_live(st)
        .iter()
        .take(4)
        .map(|(_, i, _)| files_ui::file_row(i, now(), false))
        .collect();
    ui.set_recent_strip(model_of(strip));
}

pub fn show_favs(ui: &FilesApp, st: &mut State) {
    ui.set_favs(model_of(fav_rows(st, true)));
    st.view = View::Favs;
    ui.set_view("favs".into());
}

/// A favourite on Browse (in the order shown, gone ones skipped).
pub fn fav_path(st: &State, i: usize) -> Option<String> {
    st.favs
        .iter()
        .filter(|f| st.fs.entry(f).is_ok())
        .nth(i)
        .cloned()
}

pub fn recent_path(st: &mut State, i: usize) -> Option<String> {
    recent_live(st).get(i).map(|(p, _, _)| p.clone())
}

// ── the listing views (Recent, a type, the bin, a review) ───────────────────────────────────────────────────────

pub fn show_listed(ui: &FilesApp, st: &State) {
    let now = now();
    let rows: Vec<FileRow> = st
        .listed
        .iter()
        .zip(&st.checked)
        .take(ROWS_MAX)
        .map(|(l, c)| {
            let mut r = files_ui::file_row(&l.item, now, *c);
            r.detail = l.detail.as_str().into();
            r.section = l.section.as_str().into();
            r.note = l.note.as_str().into();
            r
        })
        .collect();
    let more = st.listed.len().saturating_sub(ROWS_MAX);
    ui.set_updating(if more > 0 {
        format!("and {} more", files_ui::thousands(more as u64)).into()
    } else if st.scanner.running() && matches!(st.view, View::Type) {
        "Updating…".into()
    } else {
        SharedString::new()
    });
    ui.set_selected(st.checked.iter().filter(|c| **c).count() as i32);
    ui.set_rows(model_of(rows));
}

fn set_listed(ui: &FilesApp, st: &mut State, listed: Vec<Listed>, empty: &str) {
    st.checked = alloc::vec![false; listed.len()];
    st.listed = listed;
    ui.set_picking(false);
    ui.set_empty_text(empty.into());
    show_listed(ui, st);
}

pub fn show_recent(ui: &FilesApp, st: &mut State) {
    let now = now();
    let mut last = None;
    let listed = recent_live(st)
        .into_iter()
        .map(|(p, item, t)| {
            let age = model::age(t, now);
            let section = if last != Some(age) {
                last = Some(age);
                age.label().to_uppercase()
            } else {
                String::new()
            };
            Listed {
                detail: placed(&item, &model::when_text(t, now), &place(&p)),
                path: p,
                item,
                section,
                note: String::new(),
            }
        })
        .collect();
    st.view = View::Recent;
    let listed: Vec<Listed> = listed;
    dprintln!("files: recent {}", listed.len());
    set_listed(ui, st, listed, "Files you open show here.");
    ui.set_view("recent".into());
}

/// Writable drives as `(letter, size, free)`.
fn writable(st: &State) -> Vec<(char, u64, u64)> {
    st.fs
        .drives()
        .map(|v| {
            v.into_iter()
                .filter(|d| !d.read_only)
                .map(|d| (d.letter, d.size, d.free))
                .collect()
        })
        .unwrap_or_default()
}

/// What Browse calls drive `d` ("Mass memory", "Memory card"): the same attributes [`crate::show_drives`] passes.
fn label_of(st: &State, d: char) -> &'static str {
    let removable = st
        .fs
        .drives()
        .ok()
        .and_then(|v| v.into_iter().find(|x| x.letter == d))
        .is_some_and(|x| x.removable);
    model::drive_label(d, if removable { 0x20 } else { 0 }, 0)
}

pub fn show_types(ui: &FilesApp, st: &mut State) {
    let g = st.index.groups();
    ui.set_types(model_of(
        Group::ALL
            .iter()
            .enumerate()
            .map(|(k, grp)| files_ui::type_tile(k, grp.label(), g[k].0, g[k].1))
            .collect(),
    ));
    let drives = writable(st);
    let total: u64 = drives.iter().map(|d| d.1).sum();
    let used: u64 = drives.iter().map(|d| d.1.saturating_sub(d.2)).sum();
    let letters: Vec<String> = drives.iter().map(|d| format!("{}:", d.0)).collect();
    ui.set_types_used(model::size_text(used).into());
    ui.set_types_of(
        format!(
            "used of {} on {}",
            model::size_text(total),
            letters.join(" and ")
        )
        .into(),
    );
    let bytes: [u64; 8] = core::array::from_fn(|k| g[k].1);
    ui.set_types_bar(model_of(files_ui::used_bar(&bytes, total)));
    ui.set_updating(if st.scanner.running() {
        "Updating…".into()
    } else {
        SharedString::new()
    });
    st.view = View::Types;
    ui.set_view("types".into());
}

/// The Documents chips (files2-type-docs).
const DOC_SUBS: [(&str, Option<neo_view::Family>); 5] = [
    ("All", None),
    ("PDF", Some(neo_view::Family::Pdf)),
    ("Word", Some(neo_view::Family::Word)),
    ("Sheets", Some(neo_view::Family::Excel)),
    ("Slides", Some(neo_view::Family::Slides)),
];

pub fn show_type(ui: &FilesApp, st: &mut State) {
    let group = Group::ALL[st.group.min(7)];
    let docs = group == Group::Documents;
    let drives: Vec<char> = writable(st).iter().map(|d| d.0).collect();
    let mut chips = alloc::vec![String::from("All drives")];
    for d in &drives {
        chips.push(format!("{d}: {}", label_of(st, *d)));
    }
    let drive = st
        .type_drive
        .checked_sub(1)
        .and_then(|k| drives.get(k).copied());
    let family = if docs {
        DOC_SUBS[st.type_sub.min(4)].1
    } else {
        None
    };
    let mut found: Vec<(String, Item)> = st
        .index
        .in_group(group, drive, family)
        .into_iter()
        .map(|i| indexed(&st.index, i))
        .collect();
    found.sort_by_key(|f| core::cmp::Reverse(f.1.modified));
    let now = now();
    let listed = found
        .into_iter()
        .map(|(p, item)| Listed {
            detail: placed(&item, &model::day_text(item.modified, now), &place(&p)),
            path: p,
            item,
            ..Listed::default()
        })
        .collect();
    ui.set_name(group.label().into());
    ui.set_type_drives(strings(if docs { &[] } else { &chips }));
    ui.set_type_drive(st.type_drive as i32);
    ui.set_type_subs(strings(&if docs {
        DOC_SUBS.iter().map(|s| String::from(s.0)).collect()
    } else {
        Vec::new()
    }));
    ui.set_type_sub(st.type_sub as i32);
    st.view = View::Type;
    set_listed(
        ui,
        st,
        listed,
        if st.scanner.running() {
            "Looking…"
        } else {
            "None here"
        },
    );
    ui.set_view("type".into());
}

// ── search ───────────────────────────────────────────────────────────────────────────────────────────────────────

const KINDS: [Option<Group>; 8] = [
    None,
    Some(Group::Images),
    Some(Group::Audio),
    Some(Group::Videos),
    Some(Group::Documents),
    Some(Group::Archives),
    Some(Group::Apps),
    Some(Group::CodeText),
];
const KIND_NAMES: [&str; 8] = [
    "",
    "Images",
    "Audio",
    "Video",
    "Documents",
    "Archives",
    "Apps",
    "Code & text",
];
const SIZE_NAMES: [&str; 4] = ["", "under 1 MB", "1–100 MB", "over 100 MB"];
const HITS_MAX: usize = 200;

pub fn search(ui: &FilesApp, st: &mut State) {
    let s = &mut st.search;
    let folder = st.view == View::Folder;
    ui.set_can_search_folder(folder);
    ui.set_search_folder(folder && s.in_folder);
    let modified = Modified::ALL[s.modified.min(4)];
    let q = Query {
        text: s.text.clone(),
        folder: (folder && s.in_folder).then(|| st.dir.clone()),
        group: KINDS[s.kind.min(7)],
        size: model::SIZE_BANDS[s.size.min(3)].1,
        since: modified.since(now()),
    };
    let filters = [s.kind > 0, s.size > 0, s.modified > 0]
        .iter()
        .filter(|f| **f)
        .count();
    ui.set_filter_count(filters as i32);
    ui.set_f_type(s.kind as i32);
    ui.set_f_size(s.size as i32);
    ui.set_f_modified(s.modified as i32);
    if s.text.is_empty() && filters == 0 {
        s.hits.clear();
        ui.set_hits(model_of(Vec::new()));
        ui.set_search_info(SharedString::new());
        ui.set_filter_results(0);
        return;
    }
    let mut found: Vec<(String, Item)> = st
        .index
        .search(&q)
        .into_iter()
        .map(|i| indexed(&st.index, i))
        .collect();
    let how = st.sort;
    found.sort_by(|a, b| model::compare(&a.1, &b.1, how));
    let mut ordered = found;
    let n = ordered.len();
    dprintln!("files: search \"{}\" {}", s.text, n);
    let mut drives: Vec<char> = ordered.iter().filter_map(|h| h.0.chars().next()).collect();
    drives.sort_unstable();
    drives.dedup();
    let results = if n == 1 {
        "1 result".to_string()
    } else {
        format!("{n} results")
    };
    // files2-search-filters: "1 result · Audio, 1–100 MB, this month"
    let modified_name = modified.label().to_lowercase();
    let what: Vec<&str> = [
        KIND_NAMES[s.kind.min(7)],
        SIZE_NAMES[s.size.min(3)],
        if s.modified > 0 {
            modified_name.as_str()
        } else {
            ""
        },
    ]
    .into_iter()
    .filter(|w| !w.is_empty())
    .collect();
    let info = if !what.is_empty() {
        format!("{results} · {}", what.join(", "))
    } else if q.folder.is_some() {
        format!("{results} in {}", st.dir.trim_end_matches('\\'))
    } else {
        let names: Vec<String> = drives.iter().map(|d| format!("{d}:")).collect();
        match names.len() {
            0 => results,
            1 => format!("{results} · {}", names[0]),
            k => format!(
                "{results} · {} and {}",
                names[..k - 1].join(", "),
                names[k - 1]
            ),
        }
    };
    let updating = if st.scanner.running() {
        " · updating…"
    } else {
        ""
    };
    ui.set_search_info(format!("{info}{updating}").into());
    ui.set_filter_results(n as i32);
    ordered.truncate(HITS_MAX);
    let rows = ordered
        .iter()
        .map(|(p, it)| {
            let mut r = files_ui::file_row(it, now(), false);
            r.detail = placed(it, "", &place(p)).into();
            files_ui::hit_row(r, &s.text)
        })
        .collect();
    ui.set_hits(model_of(rows));
    s.hits = ordered;
}

pub fn reset_filters(st: &mut State) {
    st.search.kind = 0;
    st.search.size = 0;
    st.search.modified = 0;
}

// ── the Recycle bin ──────────────────────────────────────────────────────────────────────────────────────────────

pub fn show_bin(ui: &FilesApp, st: &mut State) {
    let now = now();
    let mut all = Vec::new();
    for (d, _, _) in writable(st) {
        all.extend(trash::list(&*st.fs, d).unwrap_or_default());
    }
    all.sort_by_key(|t| core::cmp::Reverse(t.deleted));
    let mut bytes = 0;
    let mut bins: Vec<String> = Vec::new();
    let listed: Vec<Listed> = all
        .into_iter()
        .map(|t| {
            let bin_path = t.bin_path();
            let is_dir = bin_path.ends_with('\\');
            let entry = st.fs.entry(&bin_path).ok();
            let size = entry.as_ref().map_or(0, |e| e.size);
            bytes += size;
            let bin = format!("{}:\\.trash\\", t.original.chars().next().unwrap_or('C'));
            if !bins.contains(&bin) {
                bins.push(bin);
            }
            let count = is_dir.then(|| st.fs.read_dir(&bin_path).map_or(0, |v| v.len() as u32));
            let item = Item {
                name: path::name_of(&t.original).trim_end_matches('\\').into(),
                is_dir,
                size,
                modified: t.deleted,
                count,
                ..Item::default()
            };
            let deleted = model::deleted_text(t.deleted, now);
            let (detail, note) = match model::left_text(t.deleted, now) {
                Some(left) => (deleted, left),
                None => (
                    format!("{deleted} · from {}", place(&t.original)),
                    String::new(),
                ),
            };
            Listed {
                path: bin_path,
                item,
                detail,
                section: String::new(),
                note,
            }
        })
        .collect();
    let n = listed.len();
    dprintln!("files: bin {}", n);
    ui.set_name("Recycle bin".into());
    ui.set_info_title(if n > 0 {
        "Kept for 30 days".into()
    } else {
        SharedString::new()
    });
    let items = if n == 1 {
        "1 item".to_string()
    } else {
        format!("{n} items")
    };
    ui.set_info_text(
        format!(
            "Then deleted for good · {items} · {} in {}",
            model::size_text(bytes),
            bins.join(" and ")
        )
        .into(),
    );
    st.view = View::Bin;
    set_listed(ui, st, listed, "The Recycle bin is empty.");
    ui.set_view("bin".into());
}

pub fn picked_paths(st: &State) -> Vec<String> {
    st.listed
        .iter()
        .zip(&st.checked)
        .filter(|(_, c)| **c)
        .map(|(l, _)| l.path.clone())
        .collect()
}

pub fn restore(app: &App, ui: &FilesApp, st: &mut State) {
    let items = picked_paths(st);
    if items.is_empty() {
        return;
    }
    let r = trash::restore(&*st.fs, &items);
    dprintln!(
        "files: restore {} {}",
        items.len(),
        r.as_ref().err().map_or(0, |e| e.code())
    );
    match r {
        Ok(()) => note_with(
            app,
            &if items.len() == 1 {
                "1 item restored".to_string()
            } else {
                format!("{} items restored", items.len())
            },
            "",
        ),
        Err(e) => toast(app, "", e.code()),
    }
    show_bin(ui, st);
    crate::rescan(st, false);
}

pub fn ask_purge(app: &App, st: &mut State) {
    let items = picked_paths(st);
    let text = match items.len() {
        0 => return,
        1 => "Delete 1 item for good? It can't be brought back.".to_string(),
        n => format!("Delete {n} items for good? They can't be brought back."),
    };
    ask(app, st, text, "Delete", Ask::Purge(items));
}

pub fn ask_empty(app: &App, st: &mut State) {
    let items: Vec<String> = st.listed.iter().map(|l| l.path.clone()).collect();
    if items.is_empty() {
        return;
    }
    let text = format!(
        "Empty the Recycle bin? {} items are deleted for good.",
        items.len()
    );
    ask(app, st, text, "Empty bin", Ask::Purge(items));
}

/// Bin paths of everything kept longer than 30 days, on every writable drive.
pub fn expired(st: &State) -> Vec<String> {
    let mut all = Vec::new();
    for (d, _, _) in writable(st) {
        all.extend(
            trash::expired(&*st.fs, d, now(), model::BIN_DAYS * model::DAY).unwrap_or_default(),
        );
    }
    all
}

// ── Free up space ────────────────────────────────────────────────────────────────────────────────────────────────

fn review_items(st: &State, kind: Review) -> Vec<usize> {
    match kind {
        Review::Large => st.index.large(LARGE),
        Review::Old => st.index.old_downloads(now() - OLD_DAYS * model::DAY),
        // the copies: each group's first (the oldest) stays
        Review::Dupes => st
            .dupes
            .as_ref()
            .map(|g| g.iter().flat_map(|g| g[1..].iter().copied()).collect())
            .unwrap_or_default(),
    }
}

fn bin_total(st: &State) -> (usize, u64) {
    let mut n = 0;
    let mut bytes = 0;
    for (d, _, _) in writable(st) {
        for t in trash::list(&*st.fs, d).unwrap_or_default() {
            n += 1;
            bytes += st.fs.entry(&t.bin_path()).map_or(0, |e| e.size);
        }
    }
    (n, bytes)
}

pub fn show_cleanup(ui: &FilesApp, st: &mut State) {
    let drives: Vec<files_ui::DriveRow> = writable(st)
        .iter()
        .rev()
        .map(|&(d, size, free)| {
            let kind = if d == 'C' { "phone" } else { "mass" };
            let label = label_of(st, d);
            files_ui::space_row(d, label, kind, size, free)
        })
        .collect();
    ui.set_drives(model_of(drives));
    let sum = |v: &[usize]| v.iter().map(|&i| st.index.items[i].size).sum::<u64>();
    let large = review_items(st, Review::Large);
    let old = review_items(st, Review::Old);
    let copies = review_items(st, Review::Dupes);
    let (bin_n, bin_bytes) = bin_total(st);
    let looking = st.dupes.is_none();
    let card = |title: &str, bytes: u64, sub: String, kind: i32, enabled: bool| CleanCard {
        title: title.into(),
        size: model::size_text(bytes).into(),
        sub: sub.into(),
        kind,
        enabled,
    };
    let files = |n: usize, what: &str| {
        if n == 1 {
            format!("1 {what}")
        } else {
            format!("{n} {what}s")
        }
    };
    ui.set_clean(model_of(alloc::vec![
        card(
            "Large files",
            sum(&large),
            files(large.len(), "file"),
            0,
            !large.is_empty()
        ),
        card(
            "Old downloads",
            sum(&old),
            files(old.len(), "old file"),
            1,
            !old.is_empty()
        ),
        card(
            "Duplicates",
            sum(&copies),
            if looking {
                "looking…".into()
            } else {
                files(copies.len(), "copy").replace("copys", "copies")
            },
            2,
            !copies.is_empty(),
        ),
        card("Recycle bin", bin_bytes, files(bin_n, "item"), 3, bin_n > 0),
    ]));
    ui.set_can_free(model::size_text(sum(&large) + sum(&old) + sum(&copies) + bin_bytes).into());
    ui.set_updating(if st.scanner.running() {
        "looking…".into()
    } else {
        SharedString::new()
    });
    st.view = View::Cleanup;
    ui.set_view("cleanup".into());
}

pub fn show_review(ui: &FilesApp, st: &mut State, kind: Review) {
    st.review = kind;
    let now = now();
    let listed = review_items(st, kind)
        .into_iter()
        .map(|i| {
            let (p, item) = indexed(&st.index, i);
            Listed {
                detail: placed(&item, &model::day_text(item.modified, now), &place(&p)),
                path: p,
                item,
                ..Listed::default()
            }
        })
        .collect();
    ui.set_name(
        match kind {
            Review::Large => "Large files",
            Review::Old => "Old downloads",
            Review::Dupes => "Duplicates",
        }
        .into(),
    );
    ui.set_info_title(SharedString::new());
    st.view = View::Review;
    set_listed(ui, st, listed, "Nothing to free here.");
    ui.set_view("review".into());
}

// ── Details ──────────────────────────────────────────────────────────────────────────────────────────────────────

pub fn details(app: &App, ui: &FilesApp, st: &mut State, target: &str) {
    let e = match st.fs.entry(target.trim_end_matches('\\')) {
        Ok(e) => e,
        Err(_) if target.len() <= 3 => neo_fs::Entry {
            name: target.into(),
            att: att::DIR,
            size: 0,
            modified: 0,
        },
        Err(e) => return toast(app, "", e.code()),
    };
    let is_dir = target.ends_with('\\');
    let name = path::name_of(target).trim_end_matches('\\');
    let mut item = Item {
        name: if name.is_empty() {
            target.into()
        } else {
            name.into()
        },
        is_dir,
        size: e.size,
        modified: e.modified,
        att: e.att,
        count: None,
    };
    let mut rows = Vec::new();
    let d = |k: &str, v: String, mono: bool| Detail {
        key: k.into(),
        value: v.into(),
        mono,
    };
    let kind = if is_dir {
        let n = st.fs.read_dir(target).map_or(0, |v| v.len() as u32);
        item.count = Some(n);
        rows.push(d("Type", "Folder".into(), false));
        rows.push(d("Contains", model::detail_text(&item, 0), false));
        "Folder".to_string()
    } else {
        let fam = neo_view::family_of(&item.name);
        let ext = neo_view::ext_of(&item.name);
        let t = if ext.is_empty() {
            fam.name().to_string()
        } else {
            format!("{} (.{})", fam.name(), ext.to_lowercase())
        };
        rows.push(d("Type", t, false));
        rows.push(d("Size", model::bytes_text(item.size), false));
        fam.name().to_string()
    };
    rows.push(d(
        "Location",
        path::parent(target).unwrap_or_default(),
        true,
    ));
    if item.modified != 0 {
        rows.push(d("Modified", model::full_date_text(item.modified), false));
    }
    if !is_dir {
        let fam = neo_view::family_of(&item.name);
        let opens = if crate::is_text(&item.name) {
            "Text viewer (built in)".to_string()
        } else {
            format!("{} (coming)", fam.viewer())
        };
        rows.push(d("Opens with", opens, false));
    }
    let letter = target.chars().next().unwrap_or('C');
    let read_only = st.drives.iter().any(|&(dr, ro)| dr == letter && ro);
    ui.set_details_item(files_ui::file_row(&item, now(), false));
    ui.set_details_kind(kind.into());
    ui.set_details(model_of(rows));
    ui.set_details_attributes(!read_only && target.len() > 3);
    ui.set_d_read_only(item.att & att::READ_ONLY != 0);
    ui.set_d_hidden(item.att & att::HIDDEN != 0);
    ui.set_d_system(item.att & att::SYSTEM != 0);
    st.details = target.into();
    ui.set_details_open(true);
}

pub fn set_attribute(app: &App, ui: &FilesApp, st: &mut State, bit: u32, on: bool) {
    let target = st.details.trim_end_matches('\\').to_string();
    let (set, clear) = if on { (bit, 0) } else { (0, bit) };
    if let Err(e) = st.fs.set_att(&target, set, clear) {
        toast(app, "", e.code());
    }
    let t = st.details.clone();
    details(app, ui, st, &t);
}
