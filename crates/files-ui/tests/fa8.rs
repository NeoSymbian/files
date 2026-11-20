// SPDX-License-Identifier: GPL-3.0-only
// The FA8 boards (files2-open-with, -share, and the picker as Files' file picker and Save as, which have no board)
// as `snapshots/files2-<board>.png`.
mod common;
use common::*;
use files_ui::{WithRow, colour, pack_item};

#[test]
fn fa8_boards() {
    let s = app();
    let ui = &s.ui;

    // files2-open-with: contacts.vcf from its card viewer; Files, Contacts (by name), the kit (by handle)
    ui.set_view("unknown".into());
    ui.set_name("contacts.vcf".into());
    ui.set_with_title("Open with".into());
    ui.set_with_rows(model_of(vec![
        WithRow {
            title: "Files".into(),
            sub: "Contact cards".into(),
            colour: colour(0xe2a92a),
        },
        WithRow {
            title: "Contacts".into(),
            sub: "Gets the file's name".into(),
            colour: colour(0x8a63d2),
        },
        WithRow {
            title: "Kit file viewer".into(),
            sub: "Gets the file".into(),
            colour: colour(0x8a63d2),
        },
    ]));
    ui.set_with_always(true);
    ui.set_with_open(true);
    s.shoot("files2-open-with");
    ui.set_with_open(false);

    // files2-share: a short note, so Copy to clipboard copies its text
    ui.set_share_title("Share".into());
    ui.set_share_clip("The text, to paste anywhere".into());
    ui.set_share_open(true);
    s.shoot("files2-share");
    ui.set_share_open(false);

    // the file picker another app opened (Pick a file in the kit), in C:\Data
    ui.set_view("pick-folder".into());
    ui.set_picker_title("Choose a file".into());
    ui.set_picker_drives(strings(&["C:", "E:", "Z:"]));
    ui.set_picker_drive(0);
    ui.set_picker_crumbs(strings(&["C:", "Data"]));
    ui.set_picker_rows(model_of(vec![
        pack_item("fa8", true, ""),
        pack_item("photos", true, ""),
        pack_item("big.log", false, "5.0 MB"),
        pack_item("trip-notes.txt", false, "1.6 KB"),
    ]));
    ui.set_picker_action("Tap a file".into());
    ui.set_picker_info("2 GB free".into());
    ui.set_picker_can(false);
    ui.set_picker_can_new_folder(false);
    ui.set_picker_empty("Nothing to pick here".into());
    s.shoot("files2-pick-file");

    // Save as: the kit's note.txt, into C:\Data
    ui.set_picker_title("Save as".into());
    ui.set_picker_rows(model_of(vec![
        pack_item("fa8", true, ""),
        pack_item("photos", true, ""),
    ]));
    ui.set_picker_action("Save".into());
    ui.set_picker_can(true);
    ui.set_picker_can_new_folder(true);
    ui.set_picker_naming(true);
    ui.set_picker_name("note.txt".into());
    s.shoot("files2-save-as");
}
