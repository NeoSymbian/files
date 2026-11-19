// SPDX-License-Identifier: GPL-3.0-only
// The FA6 boards (files2-zip-browse, -extract, -compress, and the folder picker, which has no board) as
// `snapshots/files2-<board>.png`.
mod common;
use common::*;
use files_core::Item;
use files_ui::{FileRow, PackItem, file_row, member_row, pack_item};

#[test]
fn fa6_boards() {
    let s = app();
    let ui = &s.ui;

    // files2-zip-browse: inside backup-2026.zip, in Documents
    ui.set_view("archive".into());
    ui.set_name("backup-2026.zip".into());
    ui.set_crumbs(strings(&["backup-2026.zip", "Documents"]));
    ui.set_read_only(true);
    let rows: Vec<FileRow> = vec![
        member_row("Scans", true, 0, 0, 6, false),
        member_row("boarding-pass.pdf", false, 212 << 10, 198 << 10, 0, false),
        member_row("ծրագիր.docx", false, 48 << 10, 41 << 10, 0, false),
        member_row("Бюджет 2026.xlsx", false, 96 << 10, 88 << 10, 0, false),
        member_row("budget.csv", false, 6 << 10, 2253, 0, false),
        member_row("trip-notes.txt", false, 4 << 10, 1638, 0, false),
        member_row("contacts.vcf", false, 18 << 10, 7 << 10, 0, false),
        member_row("concert.ics", false, 3 << 10, 1 << 10, 0, false),
    ];
    ui.set_rows(model_of(rows));
    ui.set_archive_count("412 files".into());
    ui.set_archive_sub("1.2 GB · zip".into());
    s.shoot("files2-zip-browse");

    // files2-extract: the sheet over E:\Backup, the job a third done
    ui.set_view("folder".into());
    ui.set_read_only(false);
    ui.set_name("Backup".into());
    ui.set_crumbs(strings(&["E:", "Backup"]));
    let t = 1_000_000 * 86_400 * (719_540 + 20_755);
    ui.set_rows(model_of(vec![
        file_row(
            &Item {
                name: "Old phone".into(),
                is_dir: true,
                count: Some(3),
                ..Item::default()
            },
            t,
            false,
        ),
        file_row(
            &Item {
                name: "backup-2026.zip".into(),
                size: 1_288_490_189,
                modified: t,
                ..Item::default()
            },
            t,
            false,
        ),
    ]));
    ui.set_extract_title("backup-2026.zip".into());
    ui.set_extract_info(
        "Zip archive · 1.2 GB · 412 files · needs 1.3 GB free (E: has 29.4 GB)".into(),
    );
    ui.set_extract_here("To E:\\Backup\\backup-2026\\".into());
    ui.set_extract_busy(true);
    ui.set_job_title("Extracting…".into());
    ui.set_job_progress(0.31);
    ui.set_job_detail("128 of 412 files · Documents/ծրագիր.docx · about 2 min left".into());
    ui.set_extract_open(true);
    s.shoot("files2-extract");
    ui.set_extract_open(false);
    ui.set_extract_busy(false);

    // files2-compress: three songs from E:\Music
    ui.set_form("compress".into());
    ui.set_compress_title("Compress 3 items".into());
    ui.set_name_text("Archive.zip".into());
    ui.set_compress_format(0);
    ui.set_compress_dir("E:\\Music".into());
    ui.set_compress_free("29.4 GB free".into());
    ui.set_compress_total("37.7 MB".into());
    let items: Vec<PackItem> = vec![
        pack_item("Yerevan Nights.mp3", false, "4.1 MB · 3 Nov"),
        pack_item("live-take.flac", false, "28 MB · 1 Nov"),
        pack_item("song.mp3", false, "5.6 MB · 30 Oct"),
    ];
    ui.set_compress_items(model_of(items));
    ui.set_compress_note("Audio is already compressed: expect about 37 MB".into());
    s.shoot("files2-compress");
    ui.set_form("".into());

    // the folder picker: Extract to…, on E:, in Backup
    ui.set_view("pick-folder".into());
    ui.set_picker_title("Choose a folder".into());
    ui.set_picker_drives(strings(&["C:", "E:"]));
    ui.set_picker_drive(1);
    ui.set_picker_crumbs(strings(&["E:", "Backup"]));
    ui.set_picker_rows(model_of(vec![
        pack_item("Old phone", true, "3 items"),
        pack_item("Photos 2025", true, "212 items"),
        pack_item("Phone", true, "Empty"),
    ]));
    ui.set_picker_action("Extract here".into());
    ui.set_picker_info("29.4 GB free".into());
    ui.set_picker_can(true);
    s.shoot("files2-pick-folder");
}
