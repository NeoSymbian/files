// SPDX-License-Identifier: GPL-3.0-only
// The FA1 and FA2 boards (the canvas's files2-* boards, docs files-app.md), each as `snapshots/files2-<board>.png`
// with the board's own sample data, for side-by-side review. Rows are made with `files_ui::file_row`, as NEOFILES.EXE
// makes them. `files2-delete` has no snapshot: its dialog and toast are the shell's (QUERY, NOTE).
mod common;
use common::*;
use files_core::{self as model, Item};
use files_ui::{FileRow, colour, file_row};

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
}

/// The audio family's badge colour (neo-view), as a FLAC row shows it.
fn neo_view_audio() -> u32 {
    0x8656e8
}
