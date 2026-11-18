// SPDX-License-Identifier: Apache-2.0
//! Highlighting (files-app.md F11): one lexer per syntax family — C-like and hash-comment languages on `logos`,
//! markup, Markdown and logs by hand — with keyword tables per language. A line's spans depend only on its text and
//! the state the line above left (inside a block comment or not), which [`Highlighter`] caches.

use crate::doc::Doc;
use alloc::vec::Vec;
use core::ops::Range;
use logos::Logos;

/// What a span is; the UI picks the colour (files2-edit-code's palette).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Keyword,
    /// A type, a constant, a section header.
    Type,
    /// A call, a macro, a Markdown code span's text.
    Func,
    Str,
    /// A number, an attribute, a key.
    Attr,
    Comment,
    Punct,
    /// A markup tag name.
    Tag,
    /// A log line's timestamp.
    Time,
    /// A log line's source (EFILE, NEOUI).
    Source,
    /// A Markdown heading.
    Heading,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Plain,
    Log,
    Markdown,
    Rust,
    C,
    Js,
    Css,
    Json,
    Python,
    Shell,
    Yaml,
    Toml,
    Ini,
    Markup,
}

impl Lang {
    /// From a file's extension (any case).
    pub fn of(ext: &str) -> Lang {
        match ext.to_ascii_lowercase().as_str() {
            "log" => Lang::Log,
            "md" => Lang::Markdown,
            "rs" => Lang::Rust,
            "c" | "cpp" | "h" => Lang::C,
            "js" => Lang::Js,
            "css" => Lang::Css,
            "json" => Lang::Json,
            "py" => Lang::Python,
            "sh" => Lang::Shell,
            "yaml" | "yml" => Lang::Yaml,
            "toml" => Lang::Toml,
            "ini" | "cfg" | "inf" => Lang::Ini,
            "xml" | "html" | "htm" | "svg" => Lang::Markup,
            _ => Lang::Plain,
        }
    }

    fn keywords(self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &[
                "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
                "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match",
                "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
                "super", "trait", "true", "type", "unsafe", "use", "where", "while",
            ],
            Lang::C => &[
                "auto",
                "bool",
                "break",
                "case",
                "char",
                "class",
                "const",
                "continue",
                "default",
                "delete",
                "do",
                "double",
                "else",
                "enum",
                "extern",
                "false",
                "float",
                "for",
                "goto",
                "if",
                "inline",
                "int",
                "long",
                "namespace",
                "new",
                "nullptr",
                "private",
                "protected",
                "public",
                "return",
                "short",
                "signed",
                "sizeof",
                "static",
                "struct",
                "switch",
                "template",
                "this",
                "true",
                "typedef",
                "union",
                "unsigned",
                "using",
                "virtual",
                "void",
                "volatile",
                "while",
            ],
            Lang::Js => &[
                "async",
                "await",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "default",
                "delete",
                "do",
                "else",
                "export",
                "extends",
                "false",
                "finally",
                "for",
                "function",
                "if",
                "import",
                "in",
                "instanceof",
                "let",
                "new",
                "null",
                "of",
                "return",
                "static",
                "super",
                "switch",
                "this",
                "throw",
                "true",
                "try",
                "typeof",
                "undefined",
                "var",
                "void",
                "while",
                "yield",
            ],
            Lang::Json => &["true", "false", "null"],
            Lang::Python => &[
                "and", "as", "assert", "async", "await", "break", "class", "continue", "def",
                "del", "elif", "else", "except", "False", "finally", "for", "from", "global", "if",
                "import", "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise",
                "return", "True", "try", "while", "with", "yield",
            ],
            Lang::Shell => &[
                "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function",
                "if", "in", "local", "return", "set", "then", "until", "while",
            ],
            Lang::Yaml | Lang::Toml => &["true", "false", "null", "yes", "no"],
            _ => &[],
        }
    }
}

/// Where a line starts: inside a block comment, or not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Normal,
    /// `/* … */`
    Block,
    /// `<!-- … -->`
    MarkupComment,
}

pub type Spans = Vec<(Range<usize>, Kind)>;

#[derive(Logos, Clone, Copy, Debug, PartialEq)]
enum CTok {
    #[regex(r"//[^\n]*")]
    LineComment,
    #[token("/*")]
    BlockStart,
    #[regex(r#""([^"\\]|\\.)*"?"#)]
    Str,
    #[regex(r##"r#*"[^"]*"#*"##)]
    RawStr,
    #[regex(r"'([^'\\]|\\.)'")]
    Char,
    #[regex(r"[0-9][0-9A-Za-z_.]*")]
    Num,
    #[regex(r"[A-Za-z_$][A-Za-z0-9_$]*!?")]
    Ident,
    #[regex(r"#!?\[[^\]]*\]?")]
    Attr,
    #[regex(r"#[ \t]*[a-z]+")]
    Pre,
    #[regex(r"[ \t]+")]
    Space,
}

#[derive(Logos, Clone, Copy, Debug, PartialEq)]
enum HTok {
    #[regex(r"#[^\n]*")]
    Comment,
    #[regex(r#""([^"\\]|\\.)*"?"#)]
    DStr,
    #[regex(r"'[^']*'?")]
    SStr,
    #[regex(r"[0-9][0-9A-Za-z_.:-]*")]
    Num,
    #[regex(r"[A-Za-z_$][A-Za-z0-9_.\-$]*")]
    Ident,
    #[regex(r"[ \t]+")]
    Space,
}

fn push(out: &mut Spans, r: Range<usize>, k: Kind) {
    if r.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.1 == k
        && last.0.end == r.start
    {
        last.0.end = r.end;
        return;
    }
    out.push((r, k));
}

fn next_char_is(s: &str, at: usize, c: char) -> bool {
    s[at..].trim_start_matches([' ', '\t']).starts_with(c)
}

fn ident_kind(lang: Lang, text: &str, word: &str, end: usize) -> Kind {
    if lang.keywords().contains(&word) {
        Kind::Keyword
    } else if word.ends_with('!') || next_char_is(text, end, '(') {
        Kind::Func
    } else if word.starts_with(|c: char| c.is_ascii_uppercase()) {
        Kind::Type
    } else {
        Kind::Plain
    }
}

fn c_like(lang: Lang, text: &str, mut state: State, out: &mut Spans) -> State {
    let mut from = 0;
    if state == State::Block {
        match text.find("*/") {
            Some(i) => {
                push(out, 0..i + 2, Kind::Comment);
                from = i + 2;
                state = State::Normal;
            }
            None => {
                push(out, 0..text.len(), Kind::Comment);
                return State::Block;
            }
        }
    }
    let mut lex = CTok::lexer(&text[from..]);
    while let Some(t) = lex.next() {
        let r = lex.span().start + from..lex.span().end + from;
        let kind = match t {
            Ok(CTok::LineComment) => Kind::Comment,
            Ok(CTok::BlockStart) => {
                let rest = &text[r.end..];
                match rest.find("*/") {
                    Some(i) => {
                        let end = r.end + i + 2;
                        push(out, r.start..end, Kind::Comment);
                        lex.bump(i + 2);
                        continue;
                    }
                    None => {
                        push(out, r.start..text.len(), Kind::Comment);
                        return State::Block;
                    }
                }
            }
            Ok(CTok::Str | CTok::RawStr | CTok::Char) => Kind::Str,
            Ok(CTok::Num) => Kind::Attr,
            Ok(CTok::Ident) => ident_kind(lang, text, &text[r.clone()], r.end),
            Ok(CTok::Attr | CTok::Pre) => Kind::Attr,
            Ok(CTok::Space) => Kind::Plain,
            Err(()) => Kind::Punct,
        };
        push(out, r, kind);
    }
    state
}

fn hash(lang: Lang, text: &str, out: &mut Spans) {
    let trimmed = text.trim_start();
    let indent = text.len() - trimmed.len();
    // [section] in TOML and INI, `; comment` in INI
    if matches!(lang, Lang::Toml | Lang::Ini) && trimmed.starts_with('[') {
        push(out, 0..text.len(), Kind::Type);
        return;
    }
    if lang == Lang::Ini && trimmed.starts_with(';') {
        push(out, 0..text.len(), Kind::Comment);
        return;
    }
    let mut lex = HTok::lexer(text);
    let mut first = true;
    while let Some(t) = lex.next() {
        let r = lex.span();
        let kind = match t {
            Ok(HTok::Comment) => Kind::Comment,
            Ok(HTok::DStr | HTok::SStr) => Kind::Str,
            Ok(HTok::Num) => Kind::Attr,
            Ok(HTok::Ident) => {
                let w = &text[r.clone()];
                let key = first
                    && r.start == indent
                    && matches!(lang, Lang::Yaml | Lang::Toml | Lang::Ini)
                    && (next_char_is(text, r.end, ':') || next_char_is(text, r.end, '='));
                if key {
                    Kind::Attr
                } else if lang == Lang::Shell && w.starts_with('$') {
                    Kind::Type
                } else {
                    ident_kind(lang, text, w, r.end)
                }
            }
            Ok(HTok::Space) => Kind::Plain,
            Err(()) => Kind::Punct,
        };
        if t != Ok(HTok::Space) {
            first = false;
        }
        push(out, r, kind);
    }
}

fn markup(text: &str, mut state: State, out: &mut Spans) -> State {
    let b = text.as_bytes();
    let mut i = 0;
    if state == State::MarkupComment {
        match text.find("-->") {
            Some(e) => {
                push(out, 0..e + 3, Kind::Comment);
                i = e + 3;
                state = State::Normal;
            }
            None => {
                push(out, 0..text.len(), Kind::Comment);
                return state;
            }
        }
    }
    while i < b.len() {
        if text[i..].starts_with("<!--") {
            match text[i + 4..].find("-->") {
                Some(e) => {
                    push(out, i..i + 4 + e + 3, Kind::Comment);
                    i += 4 + e + 3;
                    continue;
                }
                None => {
                    push(out, i..text.len(), Kind::Comment);
                    return State::MarkupComment;
                }
            }
        }
        if b[i] == b'<' {
            push(out, i..i + 1, Kind::Punct);
            i += 1;
            let start = i;
            while i < b.len()
                && (b[i].is_ascii_alphanumeric()
                    || matches!(b[i], b'/' | b':' | b'-' | b'?' | b'!'))
            {
                i += 1;
            }
            push(out, start..i, Kind::Tag);
            // attributes up to '>'
            while i < b.len() && b[i] != b'>' && b[i] != b'<' {
                if b[i] == b'"' || b[i] == b'\'' {
                    let q = b[i];
                    let s = i;
                    i += 1;
                    while i < b.len() && b[i] != q {
                        i += 1;
                    }
                    i = (i + 1).min(b.len());
                    push(out, s..i, Kind::Str);
                } else if b[i].is_ascii_alphabetic() {
                    let s = i;
                    while i < b.len()
                        && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'-' | b':' | b'_'))
                    {
                        i += 1;
                    }
                    push(out, s..i, Kind::Attr);
                } else {
                    let s = i;
                    i += text[i..].chars().next().map_or(1, char::len_utf8);
                    push(out, s..i, Kind::Punct);
                }
            }
            if i < b.len() && b[i] == b'>' {
                push(out, i..i + 1, Kind::Punct);
                i += 1;
            }
            continue;
        }
        let s = i;
        while i < b.len() && b[i] != b'<' {
            i += 1;
        }
        push(out, s..i, Kind::Plain);
    }
    state
}

fn markdown(text: &str, out: &mut Spans) {
    let t = text.trim_start();
    if t.starts_with('#') {
        push(out, 0..text.len(), Kind::Heading);
        return;
    }
    let mut i = 0;
    let b = text.as_bytes();
    // list markers and task boxes
    let lead = text.len() - t.len();
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
        push(out, 0..lead + 2, Kind::Keyword);
        i = lead + 2;
    }
    while i < b.len() {
        if b[i] == b'`' {
            let s = i;
            i += 1;
            while i < b.len() && b[i] != b'`' {
                i += 1;
            }
            i = (i + 1).min(b.len());
            push(out, s..i, Kind::Str);
        } else if b[i] == b'[' {
            let s = i;
            match text[i..]
                .find("](")
                .and_then(|m| text[i + m..].find(')').map(|e| i + m + e + 1))
            {
                Some(e) => {
                    push(out, s..e, Kind::Func);
                    i = e;
                }
                None => {
                    push(out, s..s + 1, Kind::Plain);
                    i += 1;
                }
            }
        } else {
            let s = i;
            i += text[i..].chars().next().map_or(1, char::len_utf8);
            push(out, s..i, Kind::Plain);
        }
    }
}

fn log(text: &str, out: &mut Spans) {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len()
        && (b[i].is_ascii_digit()
            || matches!(b[i], b':' | b'.' | b'-' | b'T' | b'Z' | b'/' | b'[' | b']'))
    {
        i += 1;
    }
    // a timestamp needs a digit and a separator
    if i > 0 && text[..i].contains(|c: char| c.is_ascii_digit()) && text[..i].contains([':', '-']) {
        push(out, 0..i, Kind::Time);
    } else {
        i = 0;
    }
    while i < b.len() && b[i] == b' ' {
        i += 1;
    }
    let s = i;
    while i < b.len() && (b[i].is_ascii_uppercase() || b[i].is_ascii_digit() || b[i] == b'_') {
        i += 1;
    }
    if i - s >= 2 && (i == b.len() || !b[i].is_ascii_alphabetic()) {
        push(out, s..i, Kind::Source);
    } else {
        i = s;
    }
    push(out, i..text.len(), Kind::Plain);
}

/// One line's spans (byte ranges into `text`, covering all of it) and the state the next line starts in.
pub fn line(lang: Lang, text: &str, state: State) -> (Spans, State) {
    let mut out = Spans::new();
    let next = match lang {
        Lang::Rust | Lang::C | Lang::Js | Lang::Css | Lang::Json => {
            c_like(lang, text, state, &mut out)
        }
        Lang::Python | Lang::Shell | Lang::Yaml | Lang::Toml | Lang::Ini => {
            hash(lang, text, &mut out);
            State::Normal
        }
        Lang::Markup => markup(text, state, &mut out),
        Lang::Markdown => {
            markdown(text, &mut out);
            State::Normal
        }
        Lang::Log => {
            log(text, &mut out);
            State::Normal
        }
        Lang::Plain => {
            push(&mut out, 0..text.len(), Kind::Plain);
            State::Normal
        }
    };
    // anything a lexer skipped stays plain, so the spans cover the line
    let mut full = Spans::new();
    let mut at = 0;
    for (r, k) in out {
        if r.start > at {
            push(&mut full, at..r.start, Kind::Plain);
        }
        let start = r.start.max(at);
        if r.end > start {
            push(&mut full, start..r.end, k);
            at = r.end;
        }
    }
    if at < text.len() {
        push(&mut full, at..text.len(), Kind::Plain);
    }
    (full, next)
}

/// The state each line starts in, worked out lazily from the top and kept until an edit above.
pub struct Highlighter {
    pub lang: Lang,
    states: Vec<State>,
}

impl Highlighter {
    pub fn new(lang: Lang) -> Highlighter {
        Highlighter {
            lang,
            states: alloc::vec![State::Normal],
        }
    }

    /// An edit at line `l`: what was worked out from there down is stale.
    pub fn invalidate(&mut self, l: usize) {
        self.states.truncate(l + 1);
    }

    pub fn spans(&mut self, doc: &Doc, l: usize) -> Spans {
        // single-line languages need no history
        let multi = matches!(
            self.lang,
            Lang::Rust | Lang::C | Lang::Js | Lang::Css | Lang::Json | Lang::Markup
        );
        if !multi {
            return line(self.lang, &doc.lines[l].text, State::Normal).0;
        }
        while self.states.len() <= l {
            let k = self.states.len() - 1;
            let (_, next) = line(self.lang, &doc.lines[k].text, self.states[k]);
            self.states.push(next);
        }
        line(self.lang, &doc.lines[l].text, self.states[l]).0
    }
}
