// SPDX-License-Identifier: Apache-2.0
//! Editing a [`Doc`]: a cursor and a selection in (line, char) positions, inserts and deletes as line fragments that
//! carry their own line ends (so undo puts back the very bytes), and an undo history where a run of typing is one
//! step.

use crate::doc::{Doc, Line, LineEnd, split_lines};
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    /// In chars, not bytes.
    pub col: usize,
}

impl Pos {
    pub const fn new(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }
}

/// A piece of text as lines: every piece but the last carries its line end.
pub type Fragment = Vec<Line>;

#[derive(Clone, Debug)]
enum Op {
    Insert { at: Pos, frag: Fragment },
    Delete { at: Pos, frag: Fragment },
}

#[derive(Clone, Debug)]
struct Step {
    id: u32,
    ops: Vec<Op>,
    /// Typing that the next character may join.
    typing: bool,
    cursor_before: Pos,
}

const UNDO_MAX: usize = 200;

pub(crate) fn byte_at(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(b, _)| b)
}

pub(crate) fn chars(s: &str) -> usize {
    s.chars().count()
}

/// The fragment's text with `\n` for every line end (what the clipboard gets).
pub fn fragment_text(frag: &Fragment) -> String {
    let mut s = String::new();
    for (i, l) in frag.iter().enumerate() {
        if i > 0 {
            s.push('\n');
        }
        s.push_str(&l.text);
    }
    s
}

impl Doc {
    pub fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        Pos::new(line, p.col.min(chars(&self.lines[line].text)))
    }

    /// Put `frag` at `at`; returns where it ends.
    pub fn insert_fragment(&mut self, at: Pos, frag: &Fragment) -> Pos {
        let at = self.clamp(at);
        let line = &mut self.lines[at.line];
        let b = byte_at(&line.text, at.col);
        if frag.len() == 1 {
            line.text.insert_str(b, &frag[0].text);
            return Pos::new(at.line, at.col + chars(&frag[0].text));
        }
        let tail = line.text.split_off(b);
        let tail_end = line.end;
        line.text.push_str(&frag[0].text);
        line.end = frag[0].end;
        let last = frag.len() - 1;
        let mut rest: Vec<Line> = frag[1..].to_vec();
        let end_col = chars(&rest[last - 1].text);
        rest[last - 1].text.push_str(&tail);
        rest[last - 1].end = tail_end;
        self.lines.splice(at.line + 1..at.line + 1, rest);
        Pos::new(at.line + last, end_col)
    }

    /// Take out `from..to`; returns what was there.
    pub fn delete_range(&mut self, from: Pos, to: Pos) -> Fragment {
        let (from, to) = (self.clamp(from.min(to)), self.clamp(from.max(to)));
        if from.line == to.line {
            let t = &mut self.lines[from.line].text;
            let (a, b) = (byte_at(t, from.col), byte_at(t, to.col));
            let gone: String = t.drain(a..b).collect();
            return alloc::vec![Line {
                text: gone,
                end: LineEnd::None,
            }];
        }
        let mut frag = Vec::with_capacity(to.line - from.line + 1);
        let first = &mut self.lines[from.line];
        let a = byte_at(&first.text, from.col);
        frag.push(Line {
            text: first.text.split_off(a),
            end: first.end,
        });
        let removed: Vec<Line> = self.lines.drain(from.line + 1..=to.line).collect();
        let n = removed.len();
        let mut tail = String::new();
        let mut tail_end = LineEnd::None;
        for (i, mut l) in removed.into_iter().enumerate() {
            if i + 1 == n {
                let b = byte_at(&l.text, to.col);
                tail = l.text.split_off(b);
                tail_end = l.end;
                frag.push(Line {
                    text: l.text,
                    end: LineEnd::None,
                });
            } else {
                frag.push(l);
            }
        }
        let first = &mut self.lines[from.line];
        first.text.push_str(&tail);
        first.end = tail_end;
        frag
    }

    pub fn range_fragment(&self, from: Pos, to: Pos) -> Fragment {
        let mut copy = self.clone();
        copy.delete_range(from, to)
    }
}

/// A document being edited.
pub struct Editor {
    pub doc: Doc,
    pub cursor: Pos,
    /// The other end of the selection, when there is one.
    pub anchor: Option<Pos>,
    /// The column Up/Down aims for.
    goal: Option<usize>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    next_id: u32,
    saved_id: u32,
}

impl Editor {
    pub fn new(doc: Doc) -> Editor {
        Editor {
            doc,
            cursor: Pos::default(),
            anchor: None,
            goal: None,
            undo: Vec::new(),
            redo: Vec::new(),
            next_id: 1,
            saved_id: 0,
        }
    }

    fn current_id(&self) -> u32 {
        self.undo.last().map_or(0, |s| s.id)
    }

    /// Changed since it was opened or saved.
    pub fn dirty(&self) -> bool {
        self.current_id() != self.saved_id
    }

    pub fn saved(&mut self) {
        self.saved_id = self.current_id();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Lines touched since the last save, for "You edited 3 lines".
    pub fn edited_lines(&self) -> usize {
        let mut lines = alloc::collections::BTreeSet::new();
        let start = self
            .undo
            .iter()
            .position(|s| s.id > self.saved_id)
            .unwrap_or(self.undo.len());
        for s in &self.undo[start..] {
            for op in &s.ops {
                let (Op::Insert { at, frag } | Op::Delete { at, frag }) = op;
                lines.extend(at.line..at.line + frag.len());
            }
        }
        lines.len()
    }

    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        (a != self.cursor).then(|| (a.min(self.cursor), a.max(self.cursor)))
    }

    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection()?;
        Some(fragment_text(&self.doc.range_fragment(a, b)))
    }

    fn record(&mut self, op: Op, typing: bool, before: Pos) {
        self.redo.clear();
        if typing
            && let Some(last) = self.undo.last_mut()
            && last.typing
            && last.id != self.saved_id
        {
            last.ops.push(op);
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.undo.push(Step {
            id,
            ops: alloc::vec![op],
            typing,
            cursor_before: before,
        });
        if self.undo.len() > UNDO_MAX {
            let gone = self.undo.remove(0);
            // the saved state can't be reached by undo any more: it stays dirty
            if self.saved_id == 0 || self.saved_id <= gone.id {
                self.saved_id = u32::MAX;
            }
        }
    }

    fn end_typing(&mut self) {
        if let Some(last) = self.undo.last_mut() {
            last.typing = false;
        }
    }

    fn delete_selection(&mut self, before: Pos) -> bool {
        let Some((a, b)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let frag = self.doc.delete_range(a, b);
        self.record(Op::Delete { at: a, frag }, false, before);
        self.cursor = a;
        self.anchor = None;
        true
    }

    /// Typed or pasted text (`\n`, `\r\n` and `\r` all break lines, with the file's own line end).
    pub fn insert(&mut self, s: &str) {
        let before = self.cursor;
        let replaced = self.delete_selection(before);
        let end = self.doc.main_end();
        let mut frag = split_lines(s);
        for l in frag.iter_mut().rev().skip(1) {
            l.end = end;
        }
        let typing = !replaced && frag.len() == 1 && chars(s) == 1 && !s.contains(' ');
        let at = self.cursor;
        self.cursor = self.doc.insert_fragment(at, &frag);
        if !typing {
            self.end_typing();
        }
        self.record(Op::Insert { at, frag }, typing, before);
        if !typing {
            self.end_typing();
        }
        self.goal = None;
    }

    /// Enter: a new line with the current line's indentation.
    pub fn newline(&mut self) {
        let indent: String = self.doc.lines[self.cursor.line]
            .text
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .take(self.cursor.col)
            .collect();
        let mut s = String::from("\n");
        s.push_str(&indent);
        self.insert(&s);
    }

    /// Tab: four spaces, or a tab in a file indented with tabs.
    pub fn tab(&mut self) {
        let t = if self.doc.uses_tabs() { "\t" } else { "    " };
        self.insert(t);
    }

    pub fn backspace(&mut self) {
        let before = self.cursor;
        if self.delete_selection(before) {
            return;
        }
        let from = if self.cursor.col > 0 {
            Pos::new(self.cursor.line, self.cursor.col - 1)
        } else if self.cursor.line > 0 {
            let l = self.cursor.line - 1;
            Pos::new(l, chars(&self.doc.lines[l].text))
        } else {
            return;
        };
        let frag = self.doc.delete_range(from, self.cursor);
        self.cursor = from;
        self.record(Op::Delete { at: from, frag }, true, before);
        self.goal = None;
    }

    pub fn cut(&mut self) -> Option<String> {
        let t = self.selected_text()?;
        let before = self.cursor;
        self.delete_selection(before);
        self.end_typing();
        Some(t)
    }

    fn line_len(&self, l: usize) -> usize {
        chars(&self.doc.lines[l].text)
    }

    /// ← → ↑ ↓ (no Shift on this keyboard: a move drops the selection).
    pub fn move_by(&mut self, dx: i32, dy: i32) {
        self.anchor = None;
        self.end_typing();
        let c = self.cursor;
        if dy != 0 {
            let goal = *self.goal.get_or_insert(c.col);
            let line = if dy < 0 {
                c.line.saturating_sub((-dy) as usize)
            } else {
                (c.line + dy as usize).min(self.doc.lines.len() - 1)
            };
            self.cursor = Pos::new(line, goal.min(self.line_len(line)));
            return;
        }
        self.goal = None;
        self.cursor = if dx < 0 {
            if c.col > 0 {
                Pos::new(c.line, c.col - 1)
            } else if c.line > 0 {
                Pos::new(c.line - 1, self.line_len(c.line - 1))
            } else {
                c
            }
        } else if c.col < self.line_len(c.line) {
            Pos::new(c.line, c.col + 1)
        } else if c.line + 1 < self.doc.lines.len() {
            Pos::new(c.line + 1, 0)
        } else {
            c
        };
    }

    pub fn set_cursor(&mut self, p: Pos) {
        self.end_typing();
        self.cursor = self.doc.clamp(p);
        self.anchor = None;
        self.goal = None;
    }

    pub fn select(&mut self, from: Pos, to: Pos) {
        self.end_typing();
        self.anchor = Some(self.doc.clamp(from));
        self.cursor = self.doc.clamp(to);
    }

    pub fn select_all(&mut self) {
        let last = self.doc.lines.len() - 1;
        self.select(Pos::new(0, 0), Pos::new(last, self.line_len(last)));
    }

    /// The word around `p` (letters, digits and `_`), or the one character there.
    pub fn word_at(&self, p: Pos) -> (Pos, Pos) {
        let p = self.doc.clamp(p);
        let cs: Vec<char> = self.doc.lines[p.line].text.chars().collect();
        let word = |c: char| c.is_alphanumeric() || c == '_';
        if cs.is_empty() {
            return (p, p);
        }
        let at = p.col.min(cs.len() - 1);
        if !word(cs[at]) {
            return (Pos::new(p.line, at), Pos::new(p.line, at + 1));
        }
        let mut a = at;
        while a > 0 && word(cs[a - 1]) {
            a -= 1;
        }
        let mut b = at;
        while b < cs.len() && word(cs[b]) {
            b += 1;
        }
        (Pos::new(p.line, a), Pos::new(p.line, b))
    }

    fn apply(&mut self, op: &Op, forward: bool) {
        match (op, forward) {
            (Op::Insert { at, frag }, true) | (Op::Delete { at, frag }, false) => {
                self.cursor = self.doc.insert_fragment(*at, frag);
            }
            (Op::Insert { at, frag }, false) | (Op::Delete { at, frag }, true) => {
                let end = end_of(*at, frag);
                self.doc.delete_range(*at, end);
                self.cursor = *at;
            }
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(step) = self.undo.pop() else {
            return false;
        };
        for op in step.ops.iter().rev() {
            self.apply(op, false);
        }
        self.cursor = self.doc.clamp(step.cursor_before);
        self.anchor = None;
        self.redo.push(step);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(mut step) = self.redo.pop() else {
            return false;
        };
        for op in step.ops.iter() {
            self.apply(op, true);
        }
        step.typing = false;
        self.anchor = None;
        self.undo.push(step);
        true
    }

    /// Replace each `(from, to)` with `with`, as one undo step; the ranges are on single lines, in order.
    pub fn replace_ranges(&mut self, ranges: &[(Pos, Pos)], with: &str) {
        if ranges.is_empty() {
            return;
        }
        let before = self.cursor;
        let id = self.next_id;
        self.next_id += 1;
        let mut ops = Vec::new();
        let new = alloc::vec![Line {
            text: String::from(with),
            end: LineEnd::None,
        }];
        // from the end, so earlier positions stay valid
        for &(a, b) in ranges.iter().rev() {
            let frag = self.doc.delete_range(a, b);
            ops.push(Op::Delete { at: a, frag });
            self.doc.insert_fragment(a, &new);
            ops.push(Op::Insert {
                at: a,
                frag: new.clone(),
            });
        }
        self.redo.clear();
        self.undo.push(Step {
            id,
            ops,
            typing: false,
            cursor_before: before,
        });
        self.cursor = self.doc.clamp(ranges[0].0);
        self.anchor = None;
    }
}

/// Where `frag` ends when it starts at `at`.
fn end_of(at: Pos, frag: &Fragment) -> Pos {
    if frag.len() == 1 {
        Pos::new(at.line, at.col + chars(&frag[0].text))
    } else {
        Pos::new(at.line + frag.len() - 1, chars(&frag[frag.len() - 1].text))
    }
}
