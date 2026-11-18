// SPDX-License-Identifier: Apache-2.0
use neo_format_text::find::{find_all, next_from};
use neo_format_text::{Doc, Editor, Encoding, LineEnd, NotText, Pos};

fn corpus() -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        b"one line, no newline".to_vec(),
        b"a\nb\n".to_vec(),
        b"a\r\nb\r\nc".to_vec(),
        b"mixed\nends\r\nhere\rand\n\n\r\n".to_vec(),
        b"\r".to_vec(),
        b"\n\n\n".to_vec(),
        "Ծրագիր\tБюджет\n  indented\ttabs\n".as_bytes().to_vec(),
        [&[0xEF, 0xBB, 0xBF][..], "bom\r\nтекст".as_bytes()].concat(),
        "x".repeat(10_000).into_bytes(),
    ];
    for le in [true, false] {
        let mut b = if le {
            vec![0xFF, 0xFE]
        } else {
            vec![0xFE, 0xFF]
        };
        for u in "UTF-16 😀 Ծ\r\nline 2\n".encode_utf16() {
            b.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() });
        }
        v.push(b);
    }
    v
}

#[test]
fn every_sample_round_trips_byte_exact() {
    for b in corpus() {
        let d = Doc::decode(&b).unwrap();
        assert_eq!(d.encode(), b, "{:?}", String::from_utf8_lossy(&b));
    }
}

#[test]
fn encodings_and_line_ends_are_found() {
    let d = Doc::decode(b"a\r\nb\r\nc\n").unwrap();
    assert_eq!(d.encoding, Encoding::Utf8);
    assert_eq!(d.lines.len(), 4);
    assert_eq!(d.lines[0].end, LineEnd::CrLf);
    assert_eq!(d.lines[2].end, LineEnd::Lf);
    assert_eq!(d.lines[3].end, LineEnd::None);
    assert_eq!(d.main_end(), LineEnd::CrLf);
    assert_eq!(
        Doc::decode(&[0xEF, 0xBB, 0xBF, b'x']).unwrap().encoding,
        Encoding::Utf8Bom
    );
    assert_eq!(Doc::decode(b"a\nb\n").unwrap().lines.len(), 3);
}

#[test]
fn invalid_text_is_refused_and_shown_lossy() {
    assert_eq!(Doc::decode(&[b'a', 0xFF, b'b']), Err(NotText));
    assert_eq!(Doc::decode(&[0xFF, 0xFE, 0x41]), Err(NotText)); // odd UTF-16
    assert_eq!(Doc::decode(&[0xFF, 0xFE, 0x00, 0xD8]), Err(NotText)); // lone surrogate
    assert_eq!(Doc::lossy(&[b'a', 0xFF, b'b']).lines[0].text, "a\u{FFFD}b");
}

#[test]
fn a_5_mb_log_decodes_and_round_trips() {
    let mut log = String::new();
    let mut i = 0;
    while log.len() < 5 << 20 {
        log.push_str(&format!(
            "10:21:{:02}.{:03} EFILE line {i} Mounted E: FAT32\r\n",
            i % 60,
            i % 1000
        ));
        i += 1;
    }
    let b = log.into_bytes();
    let t = std::time::Instant::now();
    let d = Doc::decode(&b).unwrap();
    assert!(d.lines.len() > 100_000);
    assert_eq!(d.encode(), b);
    // host budget; the phone is ~20x slower, still under a few seconds
    assert!(t.elapsed().as_millis() < 1500, "{:?}", t.elapsed());
}

#[test]
fn typing_newlines_backspace_and_undo_give_back_the_bytes() {
    let b = b"fn main() {\r\n    x\r\n}".to_vec();
    let mut e = Editor::new(Doc::decode(&b).unwrap());
    e.set_cursor(Pos::new(1, 5));
    for c in "yz".chars() {
        e.insert(&c.to_string());
    }
    e.newline();
    e.insert("ok");
    assert_eq!(e.doc.encode(), b"fn main() {\r\n    xyz\r\n    ok\r\n}");
    assert!(e.dirty());
    e.backspace();
    e.backspace();
    e.backspace();
    e.backspace();
    e.backspace();
    e.backspace();
    e.backspace();
    assert_eq!(e.doc.encode(), b"fn main() {\r\n    xyz\r\n}");
    while e.undo() {}
    assert_eq!(e.doc.encode(), b);
    assert!(!e.dirty());
    e.redo();
    assert!(e.dirty());
}

#[test]
fn deleting_across_lines_keeps_the_last_line_end() {
    let mut e = Editor::new(Doc::decode(b"ab\ncd\r\nef\rgh").unwrap());
    e.select(Pos::new(0, 1), Pos::new(2, 1));
    assert_eq!(e.selected_text().unwrap(), "b\ncd\ne");
    e.backspace();
    assert_eq!(e.doc.encode(), b"af\rgh");
    e.undo();
    assert_eq!(e.doc.encode(), b"ab\ncd\r\nef\rgh");
}

#[test]
fn random_edits_undo_back_to_identical_bytes() {
    let mut seed = 0x2545F491u32;
    let mut rnd = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as usize % n.max(1)
    };
    for b in corpus() {
        let mut e = Editor::new(Doc::decode(&b).unwrap());
        for _ in 0..60 {
            let l = rnd(e.doc.lines.len());
            let col = rnd(e.doc.lines[l].text.chars().count() + 1);
            e.set_cursor(Pos::new(l, col));
            match rnd(6) {
                0 => e.insert("ж"),
                1 => e.insert("a\r\nb\nc"),
                2 => e.newline(),
                3 => e.backspace(),
                4 => {
                    let l2 = rnd(e.doc.lines.len());
                    e.select(Pos::new(l, col), Pos::new(l2, 0));
                    e.backspace();
                }
                _ => e.tab(),
            }
        }
        while e.undo() {}
        assert_eq!(e.doc.encode(), b);
    }
}

#[test]
fn words_select_all_and_cut() {
    let mut e = Editor::new(Doc::decode("let дом_1 = 5;".as_bytes()).unwrap());
    assert_eq!(e.word_at(Pos::new(0, 6)), (Pos::new(0, 4), Pos::new(0, 9)));
    e.select_all();
    assert_eq!(e.cut().unwrap(), "let дом_1 = 5;");
    assert_eq!(e.doc.encode(), b"");
}

#[test]
fn find_and_replace_all_is_one_undo_step() {
    let mut e =
        Editor::new(Doc::decode("println!(a)\nPRINTLN!(b) println!\nДом дом".as_bytes()).unwrap());
    let m = find_all(&e.doc, "println!", false);
    assert_eq!(m.len(), 3);
    assert_eq!(find_all(&e.doc, "println!", true).len(), 2);
    assert_eq!(find_all(&e.doc, "дОм", false).len(), 2);
    assert_eq!(find_all(&e.doc, "aa", false).len(), 0);
    assert_eq!(next_from(&m, Pos::new(1, 1)), Some(2));
    assert_eq!(next_from(&m, Pos::new(9, 0)), Some(0));
    e.replace_ranges(&m, "log!");
    assert_eq!(e.doc.text(), "log!(a)\nlog!(b) log!\nДом дом");
    e.undo();
    assert_eq!(e.doc.text(), "println!(a)\nPRINTLN!(b) println!\nДом дом");
    let overlap = Doc::decode(b"aaaa").unwrap();
    assert_eq!(find_all(&overlap, "aa", true).len(), 2);
}
