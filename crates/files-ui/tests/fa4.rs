// SPDX-License-Identifier: GPL-3.0-only
// The FA4 boards (files2-view-text, -edit-code, -edit-find, -edit-md, -edit-csv, -new-file, -edit-buttons) as
// `snapshots/files2-<board>.png`, drawn from neo-format-text's model with the boards' own texts. The system keyboard
// is the shell's, so it is missing here; `kb-h` leaves its room (870 px, the Pixel's text keyboard).
mod common;
use common::*;
use files_ui::text::{gutter, marks, md_rows, table_cols, table_rows, text_rows};
use neo_format_text::find::find_all;
use neo_format_text::layout::Layout;
use neo_format_text::lex::{Highlighter, Lang};
use neo_format_text::md::blocks;
use neo_format_text::table::Table;
use neo_format_text::{Doc, Editor, Pos};

const KB: f32 = 870.0;

const SYSLOG: &str = "10:21:03.412 EFILE Mounted E: FAT32, 41.8 GB, label MASS
10:21:03.418 EFILE Mounted C: FAT32, 8.0 GB
10:21:03.502 NEOUI Window server up, 1080x2160 @ 3.0
10:21:03.611 MODEMD qrtr: node 0 up, 14 services
10:21:04.020 MODEMD SIM ready, 3G, MCC 283 MNC 10
10:21:04.233 NEONET wlan0 associated, WPA2, ch 6
10:21:04.871 NEONET DHCP 192.168.3.41/24 via .1
10:21:05.004 SYSSTART Boot OK, slot a marked good
10:22:17.330 FILES Opened E:\\Downloads (9 items)
10:22:17.336 FILES Thumbnail cache miss: sunset.jpg
10:22:18.902 MUSIC Indexed E:\\Music, 212 tracks
10:23:40.115 NEONET DNS timeout 8.8.8.8, retry 1
10:23:41.007 NEONET DNS ok, 34 ms
10:24:02.551 FILES Opened E:\\Documents\\syslog.log
";

const MAIN_RS: &str = r#"// SPDX-License-Identifier: Apache-2.0
//! neonotes: lists the notes on E:
#![no_std]
#![no_main]

use e32::fs::{Dir, Fs};
use e32::{println, Result};

const NOTES: &str = r"E:\Notes\";

#[no_mangle]
pub extern "C" fn E32Main() -> i32 {
    match run() {
        Ok(n) => n as i32,
        Err(e) => e.code(),
    }
}

fn run() -> Result<usize> {
    println!("NeoNotes 0.2");
    let fs = Fs::connect()?;
    let dir = Dir::open(&fs, NOTES)?;
    println!("Notes in {}", NOTES);
    let mut n = 0;
    for entry in dir.entries() {
        let f = entry?;
        println!("{} ({} B)", f.name(), f.size());
        n += 1;
    }
    if n == 0 {
        println!("(empty)");
    }
    Ok(n)
}
"#;

/// Lays out `doc` at the pane's width and fills the surface from row `first`.
fn show(s: &Shot, doc: &Doc, lang: Lang, wrap: bool, first: usize) -> Layout {
    let ui = &s.ui;
    // the gutter first: the columns that fit depend on it
    ui.set_text_gutter(gutter(doc));
    s.shoot("_probe"); // the window has its size: the pane knows its columns
    let cols = if wrap { ui.get_text_cols() as usize } else { 0 };
    let layout = Layout::new(doc, cols);
    let mut hl = Highlighter::new(lang);
    let n = ui.get_text_visible().max(40) as usize;
    ui.set_text_rows(model_of(text_rows(doc, &mut hl, &layout, first, n)));
    ui.set_text_total(layout.rows() as i32);
    ui.set_wrap(wrap);
    layout
}

#[test]
fn fa4_boards() {
    let s = app();
    let ui = &s.ui;

    // files2-view-text: syslog.log, wrapped, with line numbers
    let log = Doc::decode(SYSLOG.as_bytes()).unwrap();
    ui.set_view("text".into());
    ui.set_name("syslog.log".into());
    ui.set_editable(true);
    show(&s, &log, Lang::Log, true, 0);
    ui.set_text_info("Lines 1–14 of 1,204 · 64 KB · UTF-8".into());
    s.shoot("files2-view-text");

    // files2-edit-code: main.rs with the cursor on line 13, the keyboard up
    let mut ed = Editor::new(Doc::decode(MAIN_RS.as_bytes()).unwrap());
    ed.set_cursor(Pos::new(12, 17));
    ed.insert(" ");
    ed.backspace();
    ui.set_view("edit".into());
    ui.set_name("main.rs".into());
    ui.set_edit_kind("code".into());
    ui.set_dirty(true);
    ui.set_can_undo(true);
    ui.set_kb_h(KB);
    let layout = show(&s, &ed.doc, Lang::Rust, false, 0);
    let (r, x) = layout.place(&ed.doc, ed.cursor);
    ui.set_text_cursor(true);
    ui.set_cursor_row(r as i32);
    ui.set_cursor_x(x as i32);
    s.shoot("files2-edit-code");

    // files2-edit-buttons: the same in Buttons mode
    s.resize(1917);
    s.shoot("files2-edit-buttons");
    s.resize(H);

    // files2-edit-find: println! → log!, the second of five current
    let m = find_all(&ed.doc, "println!", true);
    assert_eq!(m.len(), 4);
    let layout = show(&s, &ed.doc, Lang::Rust, true, 17);
    let ranges: Vec<_> = m
        .iter()
        .enumerate()
        .map(|(i, r)| (r.0, r.1, if i == 1 { 2 } else { 1 }))
        .collect();
    ui.set_text_marks(model_of(marks(&ed.doc, &layout, &ranges, 17, 40)));
    ui.set_text_cursor(false);
    ui.set_find_open(true);
    ui.set_find_text("println!".into());
    ui.set_replace_text("log!".into());
    ui.set_find_info(format!("2 of {}", m.len()).into());
    s.shoot("files2-edit-find");
    ui.set_find_open(false);
    ui.set_text_marks(model_of(vec![]));

    // files2-edit-md: Preview
    let md = Doc::decode(
        "# Weekend notes\n\nIdeas for the **Files** app, and what to pack for *Dilijan*.\n\n## To do\n\n- [x] Back up `E:\\Projects`\n- [ ] Rename the Yerevan photos\n- [ ] Try the new keyboard pads\n\n## Build\n\nSources: [github.com/NeoSymbian](https://github.com/NeoSymbian)\n\n```\n# build and boot on QEMU\nneo rom && neo boot\n```\n"
            .as_bytes(),
    )
    .unwrap();
    ui.set_kb_h(0.0);
    ui.set_name("notes.md".into());
    ui.set_edit_kind("md".into());
    ui.set_md_mode(1);
    ui.set_dirty(false);
    ui.set_md_lines(model_of(md_rows(&blocks(&md), 360.0)));
    s.shoot("files2-edit-md");

    // files2-edit-csv: D7 picked, its value in the cell bar
    let csv = "Date,Item,Category,Amount\n2026-11-01,Rent,Home,180000\n2026-11-02,Groceries,Food,23500\n2026-11-03,Metro card,Transport,5000\n2026-11-05,Internet,Home,9900\n2026-11-08,Cinema,Fun,4500\n2026-11-12,Groceries,Food,18700\n2026-11-15,Taxi,Transport,2400\n2026-11-16,Bakery,Food,3200\n";
    let t = Table::parse(csv.as_bytes(), false).unwrap();
    let (heads, cells) = table_rows(&t);
    ui.set_name("budget.csv".into());
    ui.set_edit_kind("table".into());
    ui.set_dirty(true);
    ui.set_table_heads(model_of(heads));
    ui.set_table_cols(model_of(table_cols(&t, DP)));
    ui.set_table_cells(model_of(cells));
    ui.set_sel_r(6);
    ui.set_sel_c(3);
    ui.set_cell_name("D7".into());
    ui.set_cell_text(t.cell(6, 3).into());
    ui.set_kb_h(KB);
    s.shoot("files2-edit-csv");
    ui.set_kb_h(0.0);

    // files2-new-file: Markdown picked
    ui.set_view("folder".into());
    ui.set_form("file".into());
    ui.set_form_dir("E:\\Notes\\".into());
    ui.set_name_text("shopping-list".into());
    ui.set_name_suffix(".md".into());
    ui.set_new_type(1);
    s.shoot("files2-new-file");
    let _ = std::fs::remove_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../snapshots/_probe.png"
    ));
}
