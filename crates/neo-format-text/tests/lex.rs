// SPDX-License-Identifier: Apache-2.0
use neo_format_text::Doc;
use neo_format_text::lex::{Highlighter, Kind, Lang, State, line};

/// The kind of the span holding `word` in `text`.
fn kind_of(lang: Lang, text: &str, word: &str) -> Kind {
    let at = text.find(word).unwrap();
    let (spans, _) = line(lang, text, State::Normal);
    spans.iter().find(|(r, _)| r.contains(&at)).unwrap().1
}

#[test]
fn rust_tokens_get_the_boards_kinds() {
    let t = r#"pub fn run() -> Result<usize> { println!("Notes {}", NOTES); let n = 0; // done"#;
    assert_eq!(kind_of(Lang::Rust, t, "pub"), Kind::Keyword);
    assert_eq!(kind_of(Lang::Rust, t, "run"), Kind::Func);
    assert_eq!(kind_of(Lang::Rust, t, "Result"), Kind::Type);
    assert_eq!(kind_of(Lang::Rust, t, "println!"), Kind::Func);
    assert_eq!(kind_of(Lang::Rust, t, "\"Notes"), Kind::Str);
    assert_eq!(kind_of(Lang::Rust, t, "NOTES"), Kind::Type);
    assert_eq!(kind_of(Lang::Rust, t, "0;"), Kind::Attr);
    assert_eq!(kind_of(Lang::Rust, t, "// done"), Kind::Comment);
    assert_eq!(kind_of(Lang::Rust, "#![no_std]", "#!"), Kind::Attr);
    assert_eq!(
        kind_of(Lang::Rust, r#"const N: &str = r"E:\Notes\";"#, r#"r"E"#),
        Kind::Str
    );
}

#[test]
fn other_languages() {
    assert_eq!(
        kind_of(Lang::Python, "def f(x):  # note", "def"),
        Kind::Keyword
    );
    assert_eq!(
        kind_of(Lang::Python, "def f(x):  # note", "# note"),
        Kind::Comment
    );
    assert_eq!(kind_of(Lang::Toml, "[package]", "package"), Kind::Type);
    assert_eq!(kind_of(Lang::Toml, "name = \"files\"", "name"), Kind::Attr);
    assert_eq!(kind_of(Lang::Yaml, "  key: yes", "key"), Kind::Attr);
    assert_eq!(kind_of(Lang::Ini, "; comment", "comment"), Kind::Comment);
    assert_eq!(kind_of(Lang::Shell, "echo $HOME", "$HOME"), Kind::Type);
    assert_eq!(kind_of(Lang::Json, r#"{"a": true}"#, "true"), Kind::Keyword);
    assert_eq!(
        kind_of(Lang::Markup, r#"<a href="x">hi</a>"#, "a "),
        Kind::Tag
    );
    assert_eq!(
        kind_of(Lang::Markup, r#"<a href="x">hi</a>"#, "href"),
        Kind::Attr
    );
    assert_eq!(
        kind_of(Lang::Markup, r#"<a href="x">hi</a>"#, "\"x\""),
        Kind::Str
    );
    assert_eq!(kind_of(Lang::Markdown, "## Build", "Build"), Kind::Heading);
    assert_eq!(
        kind_of(Lang::Markdown, "Back up `E:\\Projects`", "`E"),
        Kind::Str
    );
    let log = "10:21:03.412 EFILE Mounted E: FAT32";
    assert_eq!(kind_of(Lang::Log, log, "10:21"), Kind::Time);
    assert_eq!(kind_of(Lang::Log, log, "EFILE"), Kind::Source);
    assert_eq!(kind_of(Lang::Log, log, "Mounted"), Kind::Plain);
    assert_eq!(Lang::of("RS"), Lang::Rust);
    assert_eq!(Lang::of("weird"), Lang::Plain);
}

#[test]
fn block_comments_carry_over_lines_and_edits_reset_them() {
    let doc =
        Doc::decode(b"let a = 1; /* start\nstill comment\nend */ let b = 2;\n<!-- x").unwrap();
    let mut h = Highlighter::new(Lang::Rust);
    assert_eq!(h.spans(&doc, 1), vec![(0..13, Kind::Comment)]);
    let third = h.spans(&doc, 2);
    assert_eq!(third[0], (0..6, Kind::Comment));
    assert!(third.iter().any(|(_, k)| *k == Kind::Keyword));
    let mut m = Highlighter::new(Lang::Markup);
    let d2 = Doc::decode(b"<!-- a\nb -->\n<p>").unwrap();
    assert_eq!(m.spans(&d2, 1)[0].1, Kind::Comment);
    assert_eq!(m.spans(&d2, 2)[1].1, Kind::Tag);
    m.invalidate(0);
    assert_eq!(m.spans(&d2, 1)[0].1, Kind::Comment);
}

#[test]
fn spans_cover_every_line_on_char_boundaries_for_any_text() {
    let mut seed = 7u32;
    let alphabet: Vec<char> = "ab Z_9\"'/*#<>!-:=[](){}`\\\t.$ ЖԾ😀\u{FFFD}"
        .chars()
        .collect();
    let langs = [
        Lang::Plain,
        Lang::Log,
        Lang::Markdown,
        Lang::Rust,
        Lang::C,
        Lang::Js,
        Lang::Css,
        Lang::Json,
        Lang::Python,
        Lang::Shell,
        Lang::Yaml,
        Lang::Toml,
        Lang::Ini,
        Lang::Markup,
    ];
    for _ in 0..3000 {
        let mut s = String::new();
        for _ in 0..(seed % 40) {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            s.push(alphabet[seed as usize % alphabet.len()]);
        }
        for &lang in &langs {
            for st in [State::Normal, State::Block, State::MarkupComment] {
                let (spans, _) = line(lang, &s, st);
                let mut at = 0;
                for (r, _) in &spans {
                    assert_eq!(r.start, at, "{lang:?} {s:?}");
                    assert!(
                        s.is_char_boundary(r.start) && s.is_char_boundary(r.end),
                        "{lang:?} {s:?}"
                    );
                    at = r.end;
                }
                assert_eq!(at, s.len(), "{lang:?} {s:?}");
            }
        }
    }
}
