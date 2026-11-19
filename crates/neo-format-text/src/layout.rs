// SPDX-License-Identifier: Apache-2.0
//! Soft wrap for a monospace view: each line takes one or more visual rows of at most `cols` columns (a tab is 4),
//! and prefix sums turn a row into a line and back in O(log n), so a 100k-line log scrolls by rows.

use crate::doc::Doc;
use crate::edit::Pos;
use alloc::vec::Vec;

/// Columns a tab takes on screen.
pub const TAB: usize = 4;

fn width(c: char) -> usize {
    if c == '\t' { TAB } else { 1 }
}

/// One visual row: a part of a line, chars `from..to`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub line: usize,
    /// 0 for a line's first row (it shows the line number).
    pub part: usize,
    pub from: usize,
    pub to: usize,
}

pub struct Layout {
    /// 0 = no wrap.
    pub cols: usize,
    /// `starts[l]` = the first row of line `l`; one more entry at the end = all rows.
    starts: Vec<usize>,
}

/// Where each row of a line starts (char indexes), wrapping at `cols` display columns: after the last space that
/// fits (files2-view-text wraps by word), or mid-word when a word is longer than a row.
pub fn breaks(text: &str, cols: usize) -> Vec<usize> {
    let mut out = alloc::vec![0];
    if cols == 0 {
        return out;
    }
    let cs: Vec<char> = text.chars().collect();
    let mut start = 0;
    while start < cs.len() {
        let mut w = 0;
        let mut end = start;
        while end < cs.len() && (w + width(cs[end]) <= cols || end == start) {
            w += width(cs[end]);
            end += 1;
        }
        if end >= cs.len() {
            break;
        }
        // back to just after the last space in this row, unless that would leave it empty
        if let Some(sp) = (start + 1..end)
            .rev()
            .find(|&i| cs[i - 1] == ' ' && cs[i] != ' ')
        {
            end = sp;
        } else if cs[end] == ' ' {
            // the row ends at a word's end: the spaces after it stay on this row
            while end < cs.len() && cs[end] == ' ' {
                end += 1;
            }
            if end >= cs.len() {
                break;
            }
        }
        out.push(end);
        start = end;
    }
    out
}

fn rows_of(text: &str, cols: usize) -> usize {
    if cols == 0 || (text.len() <= cols && !text.contains('\t')) {
        return 1;
    }
    breaks(text, cols).len()
}

/// Display columns of `text[..col chars]`.
pub fn x_of(text: &str, col: usize) -> usize {
    text.chars().take(col).map(width).sum()
}

/// The char index nearest display column `x` within `from..to`.
pub fn col_at(text: &str, from: usize, to: usize, x: usize) -> usize {
    let mut w = 0;
    for (i, c) in text.chars().enumerate().skip(from).take(to - from) {
        let cw = width(c);
        if w + cw / 2 >= x && w + cw > x {
            return i;
        }
        w += cw;
    }
    to
}

impl Layout {
    pub fn new(doc: &Doc, cols: usize) -> Layout {
        let mut l = Layout {
            cols,
            starts: Vec::with_capacity(doc.lines.len() + 1),
        };
        l.update(doc, 0);
        l
    }

    /// Lines from `from` down changed (an edit): count their rows again.
    pub fn update(&mut self, doc: &Doc, from: usize) {
        let from = from.min(self.starts.len().saturating_sub(1));
        self.starts.truncate(from);
        let row0 = if from == 0 {
            0
        } else {
            self.starts[from - 1] + rows_of(&doc.lines[from - 1].text, self.cols)
        };
        let mut row = row0;
        for line in &doc.lines[from..] {
            self.starts.push(row);
            row += rows_of(&line.text, self.cols);
        }
        self.starts.push(row);
    }

    pub fn rows(&self) -> usize {
        *self.starts.last().unwrap_or(&0)
    }

    pub fn first_row(&self, line: usize) -> usize {
        self.starts[line.min(self.starts.len() - 1)]
    }

    /// The line a row belongs to.
    pub fn line_of(&self, row: usize) -> usize {
        let lines = self.starts.len() - 1;
        match self.starts[..lines].binary_search(&row) {
            Ok(l) => l,
            Err(l) => l - 1,
        }
    }

    /// The row and display column of a position.
    pub fn place(&self, doc: &Doc, p: Pos) -> (usize, usize) {
        let text = &doc.lines[p.line].text;
        let b = breaks(text, self.cols);
        let part = b.iter().rposition(|&s| s <= p.col).unwrap_or(0);
        let x = x_of(text, p.col) - x_of(text, b[part]);
        (self.first_row(p.line) + part, x)
    }

    /// The position at a row and display column.
    pub fn pos_at(&self, doc: &Doc, row: usize, x: usize) -> Pos {
        let row = row.min(self.rows().saturating_sub(1));
        let line = self.line_of(row);
        let text = &doc.lines[line].text;
        let b = breaks(text, self.cols);
        let part = (row - self.first_row(line)).min(b.len() - 1);
        let to = b
            .get(part + 1)
            .copied()
            .unwrap_or_else(|| text.chars().count());
        Pos::new(line, col_at(text, b[part], to, x))
    }

    /// The rows `first..first + n`.
    pub fn window(&self, doc: &Doc, first: usize, n: usize) -> Vec<Row> {
        let mut out = Vec::with_capacity(n);
        if self.rows() == 0 {
            return out;
        }
        let mut line = self.line_of(first.min(self.rows() - 1));
        let mut part = first.min(self.rows() - 1) - self.first_row(line);
        while out.len() < n && line < doc.lines.len() {
            let text = &doc.lines[line].text;
            let b = breaks(text, self.cols);
            let len = text.chars().count();
            while part < b.len() && out.len() < n {
                out.push(Row {
                    line,
                    part,
                    from: b[part],
                    to: b.get(part + 1).copied().unwrap_or(len),
                });
                part += 1;
            }
            line += 1;
            part = 0;
        }
        out
    }
}
