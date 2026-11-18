// SPDX-License-Identifier: GPL-3.0-only
// The FA1 and FA2 boards (the canvas's files2-* boards, docs files-app.md), each as `snapshots/files2-<board>.png`
// with the board's own sample data, for side-by-side review. Rows are made with `files_ui::file_row`, as NEOFILES.EXE
// makes them. `files2-delete` has no snapshot: its dialog and toast are the shell's (QUERY, NOTE).
mod common;
use files_core::{self as model, Item};
use files_ui::{DriveRow, FileRow, colour, drive_row, file_row};
use slint::{ModelRc, SharedString, VecModel};
use std::rc::Rc;

fn model_of<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

fn strings(v: &[&str]) -> ModelRc<SharedString> {
    model_of(v.iter().map(|s| SharedString::from(*s)).collect())
}

/// A day of 2026 (or another year) as a Symbian `TTime`: days since 1970-01-01, plus Symbian's 719,540 before that.
fn day(days_since_1970: i64) -> i64 {
    (719_540 + days_since_1970) * 86_400 * 1_000_000
}

/// 16 Nov 2026, "today" for the boards.
const TODAY: i64 = 20_773;

fn dated(d: &str) -> i64 {
    let (n, month) = d.split_once(' ').unwrap();
    let n: i64 = n.parse().unwrap();
    // days since 1970 of the 1st of Oct/Nov 2026
    let first = match month {
        "Oct" => 20_727,
        "Nov" => 20_758,
        other => panic!("{other}"),
    };
    day(first + n - 1)
}

fn file(name: &str, size: u64, date: &str) -> Item {
    Item {
        name: name.into(),
        size,
        modified: dated(date),
        ..Item::default()
    }
}

fn folder(name: &str, count: u32) -> Item {
    Item {
        name: name.into(),
        is_dir: true,
        count: Some(count),
        ..Item::default()
    }
}

const KB: u64 = 1024;
const MB: u64 = 1024 * 1024;

fn rows(items: &[Item], checked: &[usize]) -> ModelRc<FileRow> {
    model_of(
        items
            .iter()
            .enumerate()
            .map(|(i, it)| file_row(it, day(TODAY), checked.contains(&i)))
            .collect(),
    )
}

fn drives() -> Vec<DriveRow> {
    const GB: u64 = 1 << 30;
    vec![
        drive_row(
            'C',
            model::drive_label('C', 0x11, 9),
            "phone",
            8 * GB,
            2 * GB + 922 * MB,
        ),
        drive_row(
            'E',
            model::drive_label('E', 0x11, 9),
            "mass",
            41 * GB + 819 * MB,
            29 * GB + 410 * MB,
        ),
        drive_row(
            'Z',
            model::drive_label('Z', 0x12, 7),
            "rom",
            2 * GB + 512 * MB,
            0,
        ),
    ]
}

fn music() -> Vec<Item> {
    vec![
        folder("Albums", 186),
        folder("Live 2024", 12),
        folder("Ringtones", 8),
        file("Yerevan Nights.mp3", 4300 * KB, "3 Nov"),
        file("live-take.flac", 28 * MB, "1 Nov"),
        file("song.mp3", 5734 * KB, "30 Oct"),
        file("voice-memo.m4a", 1229 * KB, "27 Oct"),
        file("Կռունկ.ogg", 3482 * KB, "12 Oct"),
        file("cover.jpg", 412 * KB, "12 Oct"),
        file("Ноктюрн.opus", 2253 * KB, "4 Oct"),
        file("lyrics.txt", 2 * KB, "4 Oct"),
    ]
}

fn folder_page(s: &common::Shot, name: &str, crumbs: &[&str], items: &[Item], checked: &[usize]) {
    let ui = &s.ui;
    ui.set_view("folder".into());
    ui.set_name(name.into());
    ui.set_crumbs(strings(crumbs));
    ui.set_rows(rows(items, checked));
    ui.set_selected(checked.len() as i32);
}

#[test]
fn boards() {
    assert_eq!(model::date_text(day(20_763)), "2026-11-06 00:00");
    let s = common::app();
    let ui = &s.ui;

    // FA1: browsing
    ui.set_drives(model_of(drives()));
    ui.set_view("browse".into());
    let px = s.shoot("files2-browse");
    assert!(px.iter().any(|&p| p != 0), "the Browse tab draws something");
    s.resize(1917);
    s.shoot("files2-browse-buttons");
    s.resize(common::H);

    folder_page(&s, "Music", &["E:", "Music"], &music(), &[]);
    s.shoot("files2-folder-list");
    s.resize(1917);
    s.shoot("files2-folder-buttons");
    s.resize(common::H);

    // the folder ⋮ (the header's right-most button)
    s.press(336.0, 24.0, true);
    s.shoot("files2-folder-menu");
    s.press(20.0, 600.0, true); // outside: closes it

    ui.set_sort_open(true);
    s.shoot("files2-sort");
    ui.set_sort_open(false);

    let images = vec![
        folder("Camera", 812),
        folder("Screenshots", 164),
        folder("Wallpapers", 24),
        file("sunset.jpg", 3174 * KB, "3 Nov"),
        file("IMG_2041.jpg", 2867 * KB, "3 Nov"),
        file("IMG_2040.jpg", 2662 * KB, "3 Nov"),
        file("Yerevan-city.png", 1946 * KB, "1 Nov"),
        file("Dilijan.webp", 640 * KB, "1 Nov"),
        file("loading.gif", 220 * KB, "30 Oct"),
        file("logo.svg", 6 * KB, "30 Oct"),
        file("IMG_1987.heic", 1434 * KB, "12 Oct"),
        file("passport.pdf", 380 * KB, "4 Oct"),
    ];
    ui.set_grid(true);
    folder_page(&s, "Images", &["E:", "Images"], &images, &[]);
    s.shoot("files2-folder-grid");
    ui.set_grid(false);

    folder_page(&s, "sketches", &["E:", "Projects", "sketches"], &[], &[]);
    s.shoot("files2-folder-empty");

    ui.set_form("folder".into());
    ui.set_form_dir("E:\\Projects\\".into());
    ui.set_name_text("sketches".into());
    s.shoot("files2-new-folder");
    ui.set_form("".into());

    let bin: Vec<Item> = [
        ("ekern.exe", 212),
        ("efile.exe", 96),
        ("estart.exe", 18),
        ("euser.dll", 98),
        ("efsrv.dll", 64),
        ("SYSSTART.EXE", 2150),
        ("MODEMD.EXE", 640),
        ("NEOPHONE.EXE", 1434),
    ]
    .iter()
    .map(|&(n, k)| file(n, k * KB, "16 Nov"))
    .collect();
    ui.set_read_only(true);
    folder_page(&s, "bin", &["Z:", "sys", "bin"], &bin, &[]);
    s.shoot("files2-rom");
    ui.set_read_only(false);

    // FA2: selection and file operations
    folder_page(&s, "Music", &["E:", "Music"], &music(), &[3, 4, 5]);
    s.shoot("files2-select");
    s.press(336.0, 24.0, true);
    s.shoot("files2-select-menu");
    s.press(20.0, 600.0, true);

    let backup = vec![
        folder("Old phone", 3),
        file("backup-2026.zip", 1229 * MB, "27 Oct"),
    ];
    folder_page(&s, "Backup", &["E:", "Backup"], &backup, &[]);
    ui.set_paste_title("2 files to move".into());
    ui.set_paste_from("from E:\\Music".into());
    ui.set_paste_action("Move here".into());
    s.shoot("files2-paste-bar");
    ui.set_paste_action("".into());

    let mut copying = backup.clone();
    copying.push(file("song.mp3", 5734 * KB, "16 Nov"));
    copying.push(file("live-take.flac", 28 * MB, "16 Nov"));
    let mut r: Vec<FileRow> = copying
        .iter()
        .map(|i| file_row(i, day(TODAY), false))
        .collect();
    r[2].detail = "5.6 MB · just now".into();
    r[2].state = 2;
    r[3].detail = "Copying · 12.6 of 28 MB".into();
    r[3].state = 1;
    r[3].progress = 0.45;
    ui.set_rows(model_of(r));
    ui.set_job_text("Copying 2 files · 45 %".into());
    ui.set_job_progress(0.45);
    s.shoot("files2-progress");
    ui.set_job_text("".into());

    let old_phone = vec![
        folder("Images", 312),
        Item {
            modified: day(17_969), // 14 Mar 2019
            ..file("N8-backup.nbu", 640 * MB, "1 Oct")
        },
        Item {
            modified: day(17_929), // 2 Feb 2019
            ..file("song.mp3", 5325 * KB, "1 Oct")
        },
    ];
    folder_page(
        &s,
        "Old phone",
        &["E:", "Backup", "Old phone"],
        &old_phone,
        &[],
    );
    ui.set_paste_title("3 items".into());
    ui.set_paste_from("from E:\\Music".into());
    ui.set_paste_action("Paste here".into());
    ui.set_clash_title("song.mp3 is already here".into());
    ui.set_clash_text(
        "The one here: 5.2 MB, 2 Feb 2019.\nThe new one: 5.6 MB, 30 Oct 2026.".into(),
    );
    ui.set_clash_count(2);
    ui.set_clash_open(true);
    s.shoot("files2-clash");
    ui.set_clash_open(false);
    ui.set_paste_action("".into());

    folder_page(&s, "Music", &["E:", "Music"], &music(), &[]);
    ui.set_form("rename".into());
    ui.set_name_text("live-take".into());
    ui.set_name_suffix(".flac".into());
    ui.set_form_dir("E:\\Music".into());
    ui.set_form_detail("28 MB · 1 Nov".into());
    ui.set_form_badge("FLAC".into());
    ui.set_form_colour(colour(neo_view_audio()));
    s.shoot("files2-rename");
    ui.set_form("".into());

    // the text viewer (R8, until FA4's)
    ui.set_view("text".into());
    ui.set_name("estart.txt".into());
    ui.set_crumbs(strings(&["Z:", "sys", "data", "estart.txt"]));
    let txt = b"# estart.txt: the drives to mount\r\nC: 0 ELOCAL FAT 0 FS_FORMAT_CORRUPT,FS_SYNC_DRIVE\r\nE: 1 ELOCAL FAT 0\r\n";
    ui.set_text(model::text_preview(txt).into());
    s.shoot("files2-text");
}

/// The audio family's badge colour (neo-view), as a FLAC row shows it.
fn neo_view_audio() -> u32 {
    0x8656e8
}
