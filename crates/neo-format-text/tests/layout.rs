// SPDX-License-Identifier: Apache-2.0
use neo_format_text::layout::{Layout, Row, breaks, x_of};
use neo_format_text::{Doc, Editor, Pos};

#[test]
fn wraps_by_display_columns_with_tabs_as_four() {
    assert_eq!(breaks("abcdefghij", 4), vec![0, 4, 8]);
    assert_eq!(breaks("41.8 GB, label", 8), vec![0, 5, 9]);
    assert_eq!(breaks("ab   cd", 4), vec![0, 5]);
    assert_eq!(breaks("\tab\tc", 4), vec![0, 1, 3, 4]);
    assert_eq!(breaks("anything", 0), vec![0]);
    assert_eq!(x_of("\tab", 2), 5);
}

#[test]
fn rows_lines_positions_and_the_window() {
    let doc = Doc::decode("short\nabcdefghij\n\nДом и сад".as_bytes()).unwrap();
    let l = Layout::new(&doc, 4);
    // short=2, abcdefghij=3, empty=1, "Дом и сад"=3
    assert_eq!(l.rows(), 9);
    assert_eq!(l.line_of(0), 0);
    assert_eq!(l.line_of(4), 1);
    assert_eq!(l.line_of(5), 2);
    assert_eq!(l.line_of(8), 3);
    assert_eq!(l.place(&doc, Pos::new(1, 5)), (3, 1));
    assert_eq!(l.pos_at(&doc, 3, 1), Pos::new(1, 5));
    assert_eq!(l.pos_at(&doc, 5, 9), Pos::new(2, 0));
    assert_eq!(l.pos_at(&doc, 99, 99), Pos::new(3, 9));
    let w = l.window(&doc, 3, 3);
    assert_eq!(
        w,
        vec![
            Row {
                line: 1,
                part: 1,
                from: 4,
                to: 8
            },
            Row {
                line: 1,
                part: 2,
                from: 8,
                to: 10
            },
            Row {
                line: 2,
                part: 0,
                from: 0,
                to: 0
            },
        ]
    );
    let nowrap = Layout::new(&doc, 0);
    assert_eq!(nowrap.rows(), 4);
}

#[test]
fn an_edit_updates_the_rows_below() {
    let mut e = Editor::new(Doc::decode(b"a\nb\nc").unwrap());
    let mut l = Layout::new(&e.doc, 4);
    assert_eq!(l.rows(), 3);
    e.set_cursor(Pos::new(1, 1));
    e.insert("123456789");
    l.update(&e.doc, 1);
    assert_eq!(l.rows(), 5);
    assert_eq!(l.line_of(4), 2);
    e.newline();
    l.update(&e.doc, 1);
    assert_eq!(l.rows(), 6);
    assert_eq!(l.rows(), Layout::new(&e.doc, 4).rows());
}

#[test]
fn a_100k_line_log_lays_out_fast() {
    let text: String = (0..100_000)
        .map(|i| format!("10:21:03.412 EFILE line {i} mounted\n"))
        .collect();
    let doc = Doc::decode(text.as_bytes()).unwrap();
    let t = std::time::Instant::now();
    let l = Layout::new(&doc, 40);
    let w = l.window(&doc, l.rows() / 2, 40);
    assert_eq!(w.len(), 40);
    assert!(t.elapsed().as_millis() < 1000, "{:?}", t.elapsed());
}
