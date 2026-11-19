// SPDX-License-Identifier: GPL-3.0-only
// The Pixel panic (FA4): a filled rectangle as tall as a long file passed the software renderer's 16-bit physical
// sizes. The text pane over a 75,058-line log must draw at the top and far down.
mod common;
use common::*;
use files_ui::text::{gutter, text_rows};
use neo_format_text::Doc;
use neo_format_text::layout::Layout;
use neo_format_text::lex::{Highlighter, Lang};

#[test]
fn a_huge_text_pane_draws() {
    let s = app();
    let ui = &s.ui;
    let text: String = (0..75_058)
        .map(|i| format!("10:21:03.412 EFILE line {i}: Mounted E: FAT32\r\n"))
        .collect();
    let doc = Doc::decode(text.as_bytes()).unwrap();
    ui.set_view("text".into());
    ui.set_text_gutter(gutter(&doc));
    s.shoot("_limits");
    let layout = Layout::new(&doc, ui.get_text_cols() as usize);
    let mut hl = Highlighter::new(Lang::Log);
    ui.set_text_rows(model_of(text_rows(&doc, &mut hl, &layout, 0, 60)));
    ui.set_text_total(layout.rows() as i32);
    eprintln!("rows {}", layout.rows());
    s.shoot("_limits");
    ui.invoke_text_show_row(50_000);
    s.shoot("_limits");
    let _ = std::fs::remove_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../snapshots/_limits.png"
    ));
}
