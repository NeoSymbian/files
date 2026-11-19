// SPDX-License-Identifier: Apache-2.0
//! Markdown to blocks for Preview (files2-edit-md): headings, paragraphs, lists with `[ ]`/`[x]` task items, block
//! quotes, fenced code, rules, and inline bold, italic, code and links. Our own subset (files-app.md: no crate).

use crate::doc::Doc;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Text,
    Bold,
    Italic,
    Code,
    Link,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading(u8, Vec<Span>),
    Para(Vec<Span>),
    /// A list item; `task` is `Some(done)` for `- [ ]` / `- [x]`.
    Item {
        depth: u8,
        task: Option<bool>,
        spans: Vec<Span>,
        /// Its line in the source (Preview's task boxes toggle it there).
        line: usize,
    },
    Quote(Vec<Span>),
    Code(Vec<String>),
    Rule,
}

fn push(out: &mut Vec<Span>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.style == style
    {
        last.text.push_str(text);
        return;
    }
    out.push(Span {
        text: String::from(text),
        style,
    });
}

/// Inline marks: `code`, **bold**, *italic* / _italic_, [text](url) (the text shows).
pub fn inline(s: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut i = 0;
    let b = s.as_bytes();
    let mut plain_from = 0;
    let flush = |out: &mut Vec<Span>, from: usize, to: usize| push(out, &s[from..to], Style::Text);
    while i < b.len() {
        let rest = &s[i..];
        let found = if let Some(r) = rest.strip_prefix('`') {
            r.find('`').map(|e| (1, e, 1, Style::Code))
        } else if let Some(r) = rest.strip_prefix("**") {
            r.find("**").map(|e| (2, e, 2, Style::Bold))
        } else if (rest.starts_with('*') || rest.starts_with('_')) && rest.len() > 1 {
            let m = &rest[..1];
            rest[1..]
                .find(m)
                .filter(|&e| e > 0)
                .map(|e| (1, e, 1, Style::Italic))
        } else if rest.starts_with('[') {
            rest.find("](")
                .and_then(|m| rest[m..].find(')').map(|e| (m, e)))
                .map(|(m, e)| (1, m - 1, 2 + (e - 2) + 1, Style::Link))
        } else {
            None
        };
        match found {
            Some((open, len, close, style)) => {
                flush(&mut out, plain_from, i);
                push(&mut out, &rest[open..open + len], style);
                i += open + len + close;
                plain_from = i;
            }
            None => i += rest.chars().next().map_or(1, char::len_utf8),
        }
    }
    flush(&mut out, plain_from, s.len());
    out
}

/// A list item's text after its marker (`- `, `* `, `+ `, `1. `).
fn list_item(trimmed: &str) -> Option<&str> {
    let bullet = ["- ", "* ", "+ "]
        .iter()
        .find_map(|m| trimmed.strip_prefix(m));
    let numbered = trimmed
        .find(". ")
        .filter(|&d| d > 0 && trimmed[..d].bytes().all(|c| c.is_ascii_digit()))
        .map(|d| &trimmed[d + 2..]);
    bullet.or(numbered)
}

fn task_of(item: &str) -> (Option<bool>, &str) {
    if let Some(r) = item.strip_prefix("[ ] ") {
        (Some(false), r)
    } else if let Some(r) = item
        .strip_prefix("[x] ")
        .or_else(|| item.strip_prefix("[X] "))
    {
        (Some(true), r)
    } else {
        (None, item)
    }
}

/// A task line's box: the char column of the ` `/`x` between its brackets, and whether it is done — parsed as
/// [`blocks`] parses it.
pub fn task_box(line: &str) -> Option<(usize, bool)> {
    let trimmed = line.trim_start();
    let item = list_item(trimmed)?;
    let done = task_of(item).0?;
    let before = line.len() - item.len();
    Some((line[..before].chars().count() + 1, done))
}

pub fn blocks(doc: &Doc) -> Vec<Block> {
    let mut out = Vec::new();
    let mut para = String::new();
    let mut code: Option<Vec<String>> = None;
    let end_para = |para: &mut String, out: &mut Vec<Block>| {
        if !para.is_empty() {
            out.push(Block::Para(inline(para)));
            para.clear();
        }
    };
    for (n, line) in doc.lines.iter().enumerate() {
        let t = line.text.as_str();
        let trimmed = t.trim_start();
        if trimmed.starts_with("```") {
            match code.take() {
                Some(lines) => out.push(Block::Code(lines)),
                None => {
                    end_para(&mut para, &mut out);
                    code = Some(Vec::new());
                }
            }
            continue;
        }
        if let Some(lines) = code.as_mut() {
            lines.push(String::from(t));
            continue;
        }
        if trimmed.is_empty() {
            end_para(&mut para, &mut out);
            continue;
        }
        let hashes = trimmed.bytes().take_while(|&c| c == b'#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            end_para(&mut para, &mut out);
            out.push(Block::Heading(
                hashes as u8,
                inline(trimmed[hashes..].trim()),
            ));
            continue;
        }
        if matches!(trimmed, "---" | "***" | "___") {
            end_para(&mut para, &mut out);
            out.push(Block::Rule);
            continue;
        }
        if let Some(q) = trimmed.strip_prefix('>') {
            end_para(&mut para, &mut out);
            out.push(Block::Quote(inline(q.trim_start())));
            continue;
        }
        if let Some(item) = list_item(trimmed) {
            end_para(&mut para, &mut out);
            let depth = ((t.len() - trimmed.len()) / 2).min(4) as u8;
            let (task, text) = task_of(item);
            out.push(Block::Item {
                depth,
                task,
                spans: inline(text),
                line: n,
            });
            continue;
        }
        if !para.is_empty() {
            para.push(' ');
        }
        para.push_str(trimmed);
    }
    end_para(&mut para, &mut out);
    if let Some(lines) = code {
        out.push(Block::Code(lines));
    }
    out
}
