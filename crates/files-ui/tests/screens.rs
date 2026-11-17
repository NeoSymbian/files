// SPDX-License-Identifier: GPL-3.0-only
// Today's screens (R8, on the kit), before the v2 redesign: the drive list, a folder, selection, the New folder form
// and the text viewer. The rows are made with files-core, as NEOFILES.EXE makes them, from a ROM-like Z:\Sys\Bin.
mod common;
use files_core::{self as model, Item};
use files_ui::{DriveRow, FileRow};
use slint::{ModelRc, VecModel};
use std::rc::Rc;

fn model_of<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

/// 2026-11-06 14:32 UTC as a Symbian `TTime` (microseconds since 0000-01-01, Julian before 1600).
const NOV6: i64 = 63_962_231_520_000_000; // (719,540 + 20,763 days) × 86,400 s + 14:32

fn drives() -> Vec<DriveRow> {
    const GB: i64 = 1 << 30;
    [('C', 0, 8 * GB, 3 * GB), ('Z', 0x02, 160 << 20, 0)]
        .iter()
        .map(|&(l, att, size, free)| DriveRow {
            label: format!("{l}: {}", model::drive_label(l, att, 0)).into(),
            detail: model::volume_text(size, free).into(),
            used: model::used_fraction(size, free),
            rom: att & 0x02 != 0,
        })
        .collect()
}

fn items() -> Vec<Item> {
    let mut v: Vec<Item> = [
        ("Data", true, 0),
        ("Resource", true, 0),
        ("ekern.exe", false, 212_480),
        ("efile.exe", false, 98_304),
        ("estart.txt", false, 1_412),
        ("euser.dll", false, 100_352),
        ("NEOFILES.EXE", false, 731_136),
        ("SYSSTART.EXE", false, 2_203_648),
        ("Дом", true, 0),
    ]
    .iter()
    .map(|&(n, d, s)| Item {
        name: n.into(),
        is_dir: d,
        size: s,
        modified: NOV6,
    })
    .collect();
    model::sort(&mut v);
    v
}

fn rows(checked: &[usize]) -> Vec<FileRow> {
    items()
        .iter()
        .enumerate()
        .map(|(i, it)| FileRow {
            name: it.name.as_str().into(),
            detail: model::detail_text(it).into(),
            kind: model::kind(&it.name, it.is_dir).into(),
            checked: checked.contains(&i),
        })
        .collect()
}

#[test]
fn screens() {
    assert_eq!(model::date_text(NOV6), "2026-11-06 14:32");
    let s = common::app();
    let ui = &s.ui;
    ui.set_drives(model_of(drives()));
    ui.set_view("drives".into());
    let px = s.shoot("files-drives");
    assert!(px.iter().any(|&p| p != 0), "the drive list draws something");

    ui.set_view("folder".into());
    ui.set_path("Z:\\Sys\\Bin\\".into());
    ui.set_name("Bin".into());
    ui.set_rows(model_of(rows(&[])));
    s.shoot("files-folder");

    ui.set_rows(model_of(rows(&[3, 5])));
    ui.set_selected(2);
    s.shoot("files-select");

    ui.set_rows(model_of(rows(&[])));
    ui.set_selected(0);
    ui.set_form("new".into());
    ui.set_name_text("neo".into());
    s.shoot("files-new-folder");

    ui.set_form("".into());
    ui.set_view("text".into());
    ui.set_path("Z:\\Sys\\Data\\estart.txt".into());
    ui.set_name("estart.txt".into());
    let txt = b"# estart.txt: the drives to mount\r\nC: 0 ELOCAL FAT 0 FS_FORMAT_CORRUPT,FS_SYNC_DRIVE\r\nE: 1 ELOCAL FAT 0\r\n";
    ui.set_text(model::text_preview(txt).into());
    s.shoot("files-text");
}
