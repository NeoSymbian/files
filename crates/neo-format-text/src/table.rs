// SPDX-License-Identifier: Apache-2.0
//! CSV and TSV as a table (files2-edit-csv), on `csv-core`. Each row keeps the bytes it was read from until it is
//! edited, and only edited rows are written again — so a file saves byte-identical, and an edit touches one line.

use crate::doc::NotText;
use alloc::string::String;
use alloc::vec::Vec;
use csv_core::{ReadRecordResult, Reader, ReaderBuilder, WriteResult, Writer, WriterBuilder};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    cells: Vec<String>,
    /// The source bytes, line end included; `None` once edited.
    raw: Option<Vec<u8>>,
    /// The line end it had (written after an edited row).
    end: Vec<u8>,
}

#[derive(Clone, Debug)]
enum Undo {
    Cell {
        r: usize,
        c: usize,
        old: String,
        raw: Option<Vec<u8>>,
    },
    RowAdded(usize),
    RowDeleted(usize, Row),
    ColAdded(Vec<(usize, Option<Vec<u8>>, usize)>),
}

pub struct Table {
    pub sep: u8,
    rows: Vec<Row>,
    /// The line end new rows get.
    end: Vec<u8>,
    undo: Vec<Undo>,
    saved: usize,
}

/// `A`, `B`, … `Z`, `AA` …
pub fn col_name(mut i: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (i % 26) as u8);
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// `;` when the first line has more of them than commas (outside quotes).
fn sniff(b: &[u8]) -> u8 {
    let (mut commas, mut semis, mut quoted) = (0, 0, false);
    for &c in b {
        match c {
            b'"' => quoted = !quoted,
            b'\n' | b'\r' if !quoted => break,
            b',' if !quoted => commas += 1,
            b';' if !quoted => semis += 1,
            _ => {}
        }
    }
    if semis > commas { b';' } else { b',' }
}

/// The line break(s) a row's bytes end with (blank lines after it included, so a rewritten row keeps them).
fn line_end(raw: &[u8]) -> Vec<u8> {
    let n = raw
        .iter()
        .rev()
        .take_while(|&&c| c == b'\r' || c == b'\n')
        .count();
    raw[raw.len() - n..].to_vec()
}

impl Table {
    /// `tsv` picks the tab; otherwise `,` or `;` by sniffing. The text must be UTF-8.
    pub fn parse(b: &[u8], tsv: bool) -> Result<Table, NotText> {
        core::str::from_utf8(b).map_err(|_| NotText)?;
        let sep = if tsv { b'\t' } else { sniff(b) };
        let mut rd: Reader = ReaderBuilder::new().delimiter(sep).build();
        let mut rows = Vec::new();
        let mut out = alloc::vec![0u8; b.len() + 1];
        let mut ends = alloc::vec![0usize; b.len() + 2];
        // where the record being read started in `b`, and how much of `out`/`ends` it has filled
        let (mut at, mut start, mut outpos, mut endpos) = (0, 0, 0, 0);
        loop {
            let (res, nin, nout, nend) =
                rd.read_record(&b[at..], &mut out[outpos..], &mut ends[endpos..]);
            // field ends are relative to the output slice this call was given
            for e in &mut ends[endpos..endpos + nend] {
                *e += outpos;
            }
            at += nin;
            outpos += nout;
            endpos += nend;
            match res {
                ReadRecordResult::InputEmpty => {}
                ReadRecordResult::Record => {
                    let raw = b[start..at].to_vec();
                    let mut cells = Vec::with_capacity(endpos);
                    let mut from = 0;
                    for &e in &ends[..endpos] {
                        cells.push(String::from(
                            core::str::from_utf8(&out[from..e]).map_err(|_| NotText)?,
                        ));
                        from = e;
                    }
                    rows.push(Row {
                        end: line_end(&raw),
                        cells,
                        raw: Some(raw),
                    });
                    start = at;
                    outpos = 0;
                    endpos = 0;
                }
                ReadRecordResult::End => break,
                ReadRecordResult::OutputFull | ReadRecordResult::OutputEndsFull => {
                    return Err(NotText);
                }
            }
        }
        // csv-core ends a record at the `\r` of a CRLF and skips blank lines: the `\n` and the blank lines arrive at
        // the start of the next record's bytes, where they belong to the row above
        for i in 1..rows.len() {
            let lead = rows[i].raw.as_ref().map_or(0, |r| {
                r.iter().take_while(|&&c| c == b'\r' || c == b'\n').count()
            });
            if lead > 0 {
                let moved: Vec<u8> = rows[i]
                    .raw
                    .as_mut()
                    .map(|r| r.drain(..lead).collect())
                    .unwrap_or_default();
                if let Some(prev) = rows[i - 1].raw.as_mut() {
                    prev.extend_from_slice(&moved);
                    rows[i - 1].end = line_end(prev);
                }
            }
        }
        // bytes the reader skipped (blank lines at the end) stay with the last row
        let used: usize = rows
            .iter()
            .map(|r| r.raw.as_ref().map_or(0, Vec::len))
            .sum();
        if used < b.len() {
            match rows.last_mut().and_then(|r| r.raw.as_mut()) {
                Some(raw) => {
                    raw.extend_from_slice(&b[used..]);
                    let end = line_end(raw);
                    if let Some(last) = rows.last_mut() {
                        last.end = end;
                    }
                }
                None => rows.push(Row {
                    cells: Vec::new(),
                    raw: Some(b[used..].to_vec()),
                    end: Vec::new(),
                }),
            }
        }
        let end = rows
            .iter()
            .find(|r| !r.end.is_empty())
            .map_or(b"\n".to_vec(), |r| r.end.clone());
        Ok(Table {
            sep,
            rows,
            end,
            undo: Vec::new(),
            saved: 0,
        })
    }

    pub fn rows(&self) -> usize {
        self.rows.len()
    }

    pub fn cols(&self) -> usize {
        self.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0)
    }

    pub fn cell(&self, r: usize, c: usize) -> &str {
        self.rows
            .get(r)
            .and_then(|row| row.cells.get(c))
            .map_or("", String::as_str)
    }

    pub fn dirty(&self) -> bool {
        self.undo.len() != self.saved
    }

    pub fn saved(&mut self) {
        self.saved = self.undo.len();
    }

    pub fn set(&mut self, r: usize, c: usize, v: &str) {
        let Some(row) = self.rows.get_mut(r) else {
            return;
        };
        if row.cells.get(c).map(String::as_str) == Some(v) {
            return;
        }
        while row.cells.len() <= c {
            row.cells.push(String::new());
        }
        let old = core::mem::replace(&mut row.cells[c], String::from(v));
        let raw = row.raw.take();
        self.undo.push(Undo::Cell { r, c, old, raw });
    }

    /// A blank row before `r` (`r` = rows() adds at the end).
    pub fn add_row(&mut self, r: usize) {
        let r = r.min(self.rows.len());
        // the row above the new last one needs a line end
        if r == self.rows.len()
            && let Some(last) = self.rows.last_mut()
            && last.end.is_empty()
        {
            last.raw = None;
            last.end = self.end.clone();
        }
        let n = self.cols().max(1);
        self.rows.insert(
            r,
            Row {
                cells: alloc::vec![String::new(); n],
                raw: None,
                end: if r == self.rows.len() {
                    Vec::new()
                } else {
                    self.end.clone()
                },
            },
        );
        self.undo.push(Undo::RowAdded(r));
    }

    pub fn delete_row(&mut self, r: usize) {
        if r < self.rows.len() {
            let row = self.rows.remove(r);
            self.undo.push(Undo::RowDeleted(r, row));
        }
    }

    /// An empty column at the end of every row.
    pub fn add_col(&mut self) {
        let n = self.cols();
        let mut before = Vec::new();
        for (i, row) in self.rows.iter_mut().enumerate() {
            before.push((i, row.raw.take(), row.cells.len()));
            while row.cells.len() <= n {
                row.cells.push(String::new());
            }
        }
        self.undo.push(Undo::ColAdded(before));
    }

    pub fn undo(&mut self) -> bool {
        let Some(u) = self.undo.pop() else {
            return false;
        };
        match u {
            Undo::Cell { r, c, old, raw } => {
                if let Some(row) = self.rows.get_mut(r) {
                    row.cells[c] = old;
                    row.raw = raw;
                }
            }
            Undo::RowAdded(r) => {
                self.rows.remove(r);
            }
            Undo::RowDeleted(r, row) => self.rows.insert(r, row),
            Undo::ColAdded(before) => {
                for (i, raw, len) in before {
                    let row = &mut self.rows[i];
                    row.cells.truncate(len);
                    row.raw = raw;
                }
            }
        }
        true
    }

    fn write_row(&self, row: &Row, out: &mut Vec<u8>) {
        let mut w: Writer = WriterBuilder::new().delimiter(self.sep).build();
        let mut buf = alloc::vec![0u8; 64];
        for (i, cell) in row.cells.iter().enumerate() {
            if i > 0 {
                out.push(self.sep);
            }
            // an empty cell is nothing between separators (csv-core would quote a lone empty field)
            if cell.is_empty() {
                if row.cells.len() == 1 {
                    out.extend_from_slice(b"\"\"");
                }
                continue;
            }
            let mut input = cell.as_bytes();
            loop {
                if buf.len() < input.len() * 2 + 4 {
                    buf.resize(input.len() * 2 + 4, 0);
                }
                let (res, nin, nout) = w.field(input, &mut buf);
                out.extend_from_slice(&buf[..nout]);
                input = &input[nin..];
                if res == WriteResult::InputEmpty {
                    break;
                }
            }
            let (_, nout) = w.finish(&mut buf);
            out.extend_from_slice(&buf[..nout]);
            w = WriterBuilder::new().delimiter(self.sep).build();
        }
        out.extend_from_slice(&row.end);
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for row in &self.rows {
            match &row.raw {
                Some(raw) => out.extend_from_slice(raw),
                None => self.write_row(row, &mut out),
            }
        }
        out
    }
}
