// SPDX-License-Identifier: Apache-2.0
use neo_format_text::Doc;
use neo_format_text::md::{Block, Span, Style, blocks, inline};
use neo_format_text::table::{Table, col_name};

fn s(text: &str, style: Style) -> Span {
    Span {
        text: text.into(),
        style,
    }
}

#[test]
fn markdown_blocks_from_the_board() {
    let md = "# Weekend notes\n\nIdeas for the **Files** app, and what to pack for *Dilijan*.\n\n## To do\n\n- [x] Back up `E:\\Projects`\n- [ ] Rename the Yerevan photos\n  - nested\n1. first\n\n> quoted\n---\nSources: [github.com/NeoSymbian](https://github.com/NeoSymbian)\n```\n# build\nneo rom\n```\n";
    let b = blocks(&Doc::decode(md.as_bytes()).unwrap());
    assert_eq!(
        b[0],
        Block::Heading(1, vec![s("Weekend notes", Style::Text)])
    );
    assert_eq!(
        b[1],
        Block::Para(vec![
            s("Ideas for the ", Style::Text),
            s("Files", Style::Bold),
            s(" app, and what to pack for ", Style::Text),
            s("Dilijan", Style::Italic),
            s(".", Style::Text),
        ])
    );
    assert_eq!(b[2], Block::Heading(2, vec![s("To do", Style::Text)]));
    assert_eq!(
        b[3],
        Block::Item {
            depth: 0,
            task: Some(true),
            spans: vec![s("Back up ", Style::Text), s("E:\\Projects", Style::Code)],
            line: 6
        }
    );
    assert!(matches!(
        b[4],
        Block::Item {
            task: Some(false),
            ..
        }
    ));
    assert!(matches!(
        b[5],
        Block::Item {
            depth: 1,
            task: None,
            ..
        }
    ));
    assert!(matches!(b[6], Block::Item { depth: 0, .. }));
    assert_eq!(b[7], Block::Quote(vec![s("quoted", Style::Text)]));
    assert_eq!(b[8], Block::Rule);
    assert_eq!(
        b[9],
        Block::Para(vec![
            s("Sources: ", Style::Text),
            s("github.com/NeoSymbian", Style::Link)
        ])
    );
    assert_eq!(b[10], Block::Code(vec!["# build".into(), "neo rom".into()]));
    assert_eq!(inline("a * b"), vec![s("a * b", Style::Text)]);
    assert_eq!(
        inline("unclosed `code"),
        vec![s("unclosed `code", Style::Text)]
    );
}

const CSV: &[u8] = b"Date,Item,Amount\r\n2026-11-01,Rent,180000\r\n2026-11-02,\"Milk, bread\",2350\r\n2026-11-03,\"two\r\nlines\",5\r\nragged\r\n";

#[test]
fn csv_round_trips_byte_exact_and_reads_quotes() {
    let t = Table::parse(CSV, false).unwrap();
    assert_eq!(t.encode(), CSV);
    assert_eq!(t.rows(), 5);
    assert_eq!(t.cols(), 3);
    assert_eq!(t.cell(2, 1), "Milk, bread");
    assert_eq!(t.cell(3, 1), "two\r\nlines");
    assert_eq!(t.cell(4, 0), "ragged");
    assert_eq!(t.cell(4, 2), "");
    for b in [&b""[..], b"a,b", b"a;b;c\n1;2;3\n", b"\n\na,b\n\n", b"x\n"] {
        assert_eq!(
            Table::parse(b, false).unwrap().encode(),
            b,
            "{:?}",
            String::from_utf8_lossy(b)
        );
    }
    assert_eq!(Table::parse(b"a;b;c\n", false).unwrap().sep, b';');
    assert_eq!(Table::parse(b"a\tb\n", true).unwrap().cols(), 2);
    assert!(Table::parse(&[0xFF], false).is_err());
}

#[test]
fn an_edit_rewrites_only_its_row_and_undo_restores() {
    let mut t = Table::parse(CSV, false).unwrap();
    t.set(1, 2, "190,000");
    assert!(t.dirty());
    assert_eq!(
        t.encode(),
        b"Date,Item,Amount\r\n2026-11-01,Rent,\"190,000\"\r\n2026-11-02,\"Milk, bread\",2350\r\n2026-11-03,\"two\r\nlines\",5\r\nragged\r\n"
    );
    t.add_row(5);
    t.set(5, 0, "new");
    t.add_col();
    t.delete_row(0);
    assert!(
        t.encode().ends_with(b"ragged,,,\r\nnew,,,"),
        "{:?}",
        String::from_utf8_lossy(&t.encode())
    );
    while t.undo() {}
    assert_eq!(t.encode(), CSV);
    assert!(!t.dirty());
    assert_eq!(col_name(0), "A");
    assert_eq!(col_name(25), "Z");
    assert_eq!(col_name(26), "AA");
}

#[test]
fn review_csv_last_row_without_newline_and_dirty_and_add_row_undo() {
    let mut t = Table::parse(b"a,b\nc,d", false).unwrap();
    t.set(1, 0, "c2");
    assert_eq!(t.encode(), b"a,b\nc2,d");
    let mut t = Table::parse(b"ab,cd\nef,gh", false).unwrap();
    t.set(1, 1, "z");
    assert_eq!(t.encode(), b"ab,cd\nef,z");
    let mut t = Table::parse(b"a,b", false).unwrap();
    t.add_row(1);
    assert_eq!(t.encode(), b"a,b\n,");
    t.undo();
    assert_eq!(t.encode(), b"a,b");
    assert!(!t.dirty());
    // edit, save, undo, another edit: still dirty
    let mut t = Table::parse(b"a,b\n", false).unwrap();
    t.set(0, 0, "x");
    t.saved();
    t.undo();
    t.set(0, 1, "y");
    assert!(t.dirty());
}

#[test]
fn task_boxes_are_found_as_blocks_finds_them() {
    use neo_format_text::md::task_box;
    assert_eq!(task_box("- [ ] a"), Some((3, false)));
    assert_eq!(task_box("  * [X] b"), Some((5, true)));
    assert_eq!(task_box("12. [x] c"), Some((5, true)));
    assert_eq!(task_box("+ [ ] d"), Some((3, false)));
    assert_eq!(task_box("- plain"), None);
    assert_eq!(task_box("text"), None);
}
