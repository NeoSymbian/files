// SPDX-License-Identifier: Apache-2.0
//! Find and replace (files2-edit-find): matches within a line, in order, with or without case (any script).

use crate::doc::Doc;
use crate::edit::Pos;
use alloc::vec::Vec;

fn fold(c: char, case: bool) -> char {
    if case {
        c
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// Every match of `needle` (no line breaks), as `(start, end)` in order; matches don't overlap.
pub fn find_all(doc: &Doc, needle: &str, case: bool) -> Vec<(Pos, Pos)> {
    let want: Vec<char> = needle.chars().map(|c| fold(c, case)).collect();
    let mut out = Vec::new();
    if want.is_empty() || want.contains(&'\n') {
        return out;
    }
    for (l, line) in doc.lines.iter().enumerate() {
        let have: Vec<char> = line.text.chars().map(|c| fold(c, case)).collect();
        let mut i = 0;
        while i + want.len() <= have.len() {
            if have[i..i + want.len()] == want[..] {
                out.push((Pos::new(l, i), Pos::new(l, i + want.len())));
                i += want.len();
            } else {
                i += 1;
            }
        }
    }
    out
}

/// The match at or after `from` (wrapping to the first), as an index into `matches`.
pub fn next_from(matches: &[(Pos, Pos)], from: Pos) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    Some(matches.iter().position(|m| m.0 >= from).unwrap_or(0))
}
