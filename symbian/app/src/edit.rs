// SPDX-License-Identifier: GPL-3.0-only
//! The open text file (files-app.md FA4): the viewer and the editors over neo-format-text. A file is read whole (up
//! to [`MAX`]), decoded byte-exact, laid out at the pane's width and drawn a window of rows at a time; keys come raw
//! from the event loop (as Terminal reads them), the code-key row and the keyboard's edit bar (Ctrl+A/C/V/X) add the
//! rest. Saving writes `<file>.~part` and puts it in place, keeping the file's attributes.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use files_core as model;
use files_ops::store;
use files_ui::text::{gutter, marks, md_rows, table_cols, table_rows, text_rows};
use files_ui::{FilesApp, Theme};
use fs_e32::E32Fs;
use neo_format_text::find::{find_all, next_from};
use neo_format_text::layout::Layout;
use neo_format_text::lex::{Highlighter, Lang};
use neo_format_text::md::blocks;
use neo_format_text::table::{Table, col_name};
use neo_format_text::{Doc, Editor, Pos};
use neo_fs::{Fs as _, att};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

/// The largest file opened (the heap is 64 MB; a doc takes about twice its bytes).
pub const MAX: u64 = 16 << 20;
/// Rows drawn above and below the screen, so a flick shows text before the next window arrives.
const MARGIN: usize = 30;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    View,
    Code,
    Md,
    Table,
}

pub struct Open {
    pub path: String,
    pub name: String,
    pub mode: Mode,
    pub ed: Editor,
    pub table: Option<Table>,
    hl: Highlighter,
    layout: Layout,
    pub wrap: bool,
    first: usize,
    size: u64,
    /// Not valid text (shown lossy), or on a read-only drive.
    pub read_only: bool,
    pub preview: bool,
    find: Vec<(Pos, Pos)>,
    find_at: usize,
    /// What the find bar looks for ("" = closed or empty): an edit searches again.
    needle: String,
    pub cell: Option<(usize, usize)>,
    /// 0x11 came: the next key is a Ctrl chord (the keyboard's edit bar).
    ctrl: bool,
    /// The selection handles show (after a long press).
    handles: bool,
}

/// What a key asks of the clipboard (the app holds it).
pub enum Clip {
    None,
    Copy(String),
    Paste,
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

/// Text types the editor takes (files2-spec-types: the 19 editable text types).
pub fn editable(name: &str) -> bool {
    use neo_view::Editor as E;
    neo_view::type_of(name).is_some_and(|t| matches!(t.editor, E::Text | E::Markdown | E::Table))
}

fn read(fs: &E32Fs, path: &str) -> Result<(Vec<u8>, u64), String> {
    let e = fs.entry(path).map_err(|e| model::error_text(e.code()))?;
    if e.size > MAX {
        return Err(format!("Too big to open ({})", model::size_text(e.size)));
    }
    let b = store::read(fs, path).map_err(|e| model::error_text(e.code()))?;
    Ok((b, e.size))
}

impl Open {
    /// `edit`: straight into the editor (New file, the pencil), else the viewer.
    pub fn new(fs: &E32Fs, path: &str, edit: bool, read_only_drive: bool) -> Result<Open, String> {
        let (bytes, size) = read(fs, path)?;
        let name = String::from(neo_fs::path::name_of(path));
        let ext = neo_view::ext_of(&name).to_ascii_lowercase();
        let (doc, lossy) = match Doc::decode(&bytes) {
            Ok(d) => (d, false),
            Err(_) => (Doc::lossy(&bytes), true),
        };
        let table_kind = ext == "csv" || ext == "tsv";
        let table = if edit && table_kind && !lossy {
            Table::parse(&bytes, ext == "tsv").ok()
        } else {
            None
        };
        let mode = if !edit {
            Mode::View
        } else if table.is_some() {
            Mode::Table
        } else if ext == "md" {
            Mode::Md
        } else {
            Mode::Code
        };
        let lang = Lang::of(&ext);
        let layout = Layout::new(&doc, 0);
        Ok(Open {
            path: String::from(path),
            name,
            mode,
            ed: Editor::new(doc),
            table,
            hl: Highlighter::new(lang),
            layout,
            wrap: true,
            first: 0,
            size,
            read_only: lossy || read_only_drive,
            preview: false,
            find: Vec::new(),
            find_at: 0,
            needle: String::new(),
            cell: None,
            ctrl: false,
            handles: false,
        })
    }

    pub fn dirty(&self) -> bool {
        self.ed.dirty() || self.table.as_ref().is_some_and(Table::dirty)
    }

    /// Lines changed since the last save (for the unsaved dialog).
    pub fn edited(&self) -> usize {
        if self.table.is_some() {
            1
        } else {
            self.ed.edited_lines().max(1)
        }
    }

    fn typing(&self) -> bool {
        matches!(self.mode, Mode::Code) || (self.mode == Mode::Md && !self.preview)
    }

    /// Lay out again at the pane's width (the gutter goes first: the columns depend on it).
    pub fn relayout(&mut self, ui: &FilesApp) {
        ui.set_text_gutter(gutter(&self.ed.doc));
        let cols = if self.wrap {
            ui.get_text_cols().max(8) as usize
        } else {
            0
        };
        self.layout = Layout::new(&self.ed.doc, cols);
    }

    /// Everything the screen shows for this file.
    pub fn show(&mut self, ui: &FilesApp) {
        ui.set_name(self.name.as_str().into());
        ui.set_wrap(self.wrap);
        ui.set_editable(!self.read_only && editable(&self.name));
        ui.set_edit_kind(
            match self.mode {
                Mode::Md => "md",
                Mode::Table => "table",
                _ => "code",
            }
            .into(),
        );
        ui.set_md_mode(i32::from(self.preview));
        ui.set_dirty(self.dirty());
        ui.set_can_undo(self.ed.can_undo() || self.table.as_ref().is_some_and(Table::dirty));
        if let Some(t) = &self.table {
            let (heads, rows) = table_rows(t);
            ui.set_table_heads(model(heads));
            ui.set_table_cols(model(table_cols(t, ui.global::<Theme>().get_dp())));
            ui.set_table_cells(model(rows));
            let (r, c) = self.cell.map_or((-1, -1), |(r, c)| (r as i32, c as i32));
            ui.set_sel_r(r);
            ui.set_sel_c(c);
            if let Some((r, c)) = self.cell {
                ui.set_cell_name(format!("{}{}", col_name(c), r + 1).as_str().into());
            }
            return;
        }
        if self.mode == Mode::Md && self.preview {
            ui.set_md_lines(model(md_rows(&blocks(&self.ed.doc), 360.0)));
        }
        self.window(ui);
        let d = &self.ed.doc;
        let c = self.ed.cursor;
        ui.set_status(
            format!(
                "Ln {}, Col {} · {} · {}",
                c.line + 1,
                c.col + 1,
                d.encoding.label(),
                d.main_end().label()
            )
            .as_str()
            .into(),
        );
        let lines = d.lines.len();
        let last = self.layout.line_of(
            (self.first + ui.get_text_visible() as usize).min(self.layout.rows().max(1) - 1),
        ) + 1;
        let first = self
            .layout
            .line_of(self.first.min(self.layout.rows().max(1) - 1))
            + 1;
        let ro = if self.read_only { " · read-only" } else { "" };
        ui.set_text_info(
            format!(
                "Lines {first}–{last} of {} · {} · {}{ro}",
                files_ui::thousands(lines as u64),
                model::size_text(self.size),
                d.encoding.label()
            )
            .as_str()
            .into(),
        );
    }

    /// The rows around the screen, the cursor, the marks and the handles.
    pub fn window(&mut self, ui: &FilesApp) {
        let vis = ui.get_text_visible().max(10) as usize;
        let from = self.first.saturating_sub(MARGIN);
        let n = vis + 2 * MARGIN;
        let doc = &self.ed.doc;
        ui.set_text_rows(model(text_rows(doc, &mut self.hl, &self.layout, from, n)));
        ui.set_text_total(self.layout.rows() as i32);
        let editing = self.typing();
        let (cr, cx) = self.layout.place(doc, self.ed.cursor);
        ui.set_text_cursor(editing && self.ed.selection().is_none());
        ui.set_cursor_row(cr as i32);
        ui.set_cursor_x(cx as i32);
        let mut ranges: Vec<(Pos, Pos, i32)> = self
            .find
            .iter()
            .enumerate()
            .map(|(i, m)| (m.0, m.1, if i == self.find_at { 2 } else { 1 }))
            .collect();
        if let Some((a, b)) = self.ed.selection() {
            ranges.push((a, b, 0));
            let (ar, ax) = self.layout.place(doc, a);
            let (br, bx) = self.layout.place(doc, b);
            ui.set_a_row(ar as i32);
            ui.set_a_x(ax as i32);
            ui.set_b_row(br as i32);
            ui.set_b_x(bx as i32);
        }
        ui.set_text_handles(self.handles && self.ed.selection().is_some());
        ui.set_text_marks(model(marks(doc, &self.layout, &ranges, from, n)));
    }

    pub fn scrolled(&mut self, ui: &FilesApp, first: usize) {
        // a new window only when the old one is nearly used up
        if first.abs_diff(self.first) >= MARGIN / 2 {
            self.first = first;
            self.window(ui);
        } else {
            self.first = first;
        }
    }

    /// After an edit at `line`: colours and rows from there down, then keep the cursor in view.
    fn edited_at(&mut self, ui: &FilesApp, line: usize) {
        self.hl.invalidate(line.saturating_sub(1));
        if gutter(&self.ed.doc) != ui.get_text_gutter() {
            // the line numbers grew a digit: fewer columns fit
            self.relayout(ui);
        } else {
            self.layout.update(&self.ed.doc, line.saturating_sub(1));
        }
        self.find = if self.needle.is_empty() {
            Vec::new()
        } else {
            find_all(&self.ed.doc, &self.needle, false)
        };
        self.find_at = self.find_at.min(self.find.len().saturating_sub(1));
        if !self.needle.is_empty() {
            ui.set_find_info(if self.find.is_empty() {
                SharedString::from("0 of 0")
            } else {
                format!("{} of {}", self.find_at + 1, self.find.len())
                    .as_str()
                    .into()
            });
        }
        self.show(ui);
        let (r, _) = self.layout.place(&self.ed.doc, self.ed.cursor);
        ui.invoke_text_show_row(r as i32);
    }

    /// A key from the system keyboard (one code point) or a chord after 0x11.
    pub fn key(&mut self, ui: &FilesApp, code: u32) -> Clip {
        if !self.typing() {
            return Clip::None;
        }
        let top = self
            .ed
            .cursor
            .line
            .min(self.ed.anchor.map_or(usize::MAX, |a| a.line));
        if self.ctrl {
            self.ctrl = false;
            return match char::from_u32(code) {
                Some('a') => {
                    self.ed.select_all();
                    self.handles = true;
                    self.show(ui);
                    Clip::None
                }
                Some('c') => self.ed.selected_text().map_or(Clip::None, Clip::Copy),
                Some('x') => match self.ed.cut() {
                    Some(t) => {
                        self.edited_at(ui, top);
                        Clip::Copy(t)
                    }
                    None => Clip::None,
                },
                Some('v') => Clip::Paste,
                _ => Clip::None,
            };
        }
        // Symbian's arrows: EKeyLeftArrow 0xF807, Right 0xF808, Up 0xF809, Down 0xF80A (neoui-proto KEYBOARD_TERMINAL)
        let moved = match code {
            0x11 => {
                self.ctrl = true;
                return Clip::None;
            }
            0xF807 => (-1, 0),
            0xF808 => (1, 0),
            0xF809 => (0, -1),
            0xF80A => (0, 1),
            _ => (0, 0),
        };
        if moved != (0, 0) {
            // a move changes no text: the find matches stay
            self.ed.move_by(moved.0, moved.1);
            self.handles = false;
            self.show(ui);
            let (r, _) = self.layout.place(&self.ed.doc, self.ed.cursor);
            ui.invoke_text_show_row(r as i32);
            return Clip::None;
        }
        match code {
            8 => self.ed.backspace(),
            13 | 10 => self.ed.newline(),
            9 => self.ed.tab(),
            c => match char::from_u32(c) {
                // other Symbian key codes (the private-use 0xF700–0xF8FF block) aren't text
                Some(ch) if !ch.is_control() && !('\u{F700}'..='\u{F8FF}').contains(&ch) => {
                    self.ed.insert(ch.encode_utf8(&mut [0; 4]))
                }
                _ => return Clip::None,
            },
        }
        self.handles = false;
        let line = self.ed.cursor.line.min(top);
        self.edited_at(ui, line);
        Clip::None
    }

    pub fn paste(&mut self, ui: &FilesApp, text: &str) {
        let top = self
            .ed
            .cursor
            .line
            .min(self.ed.anchor.map_or(usize::MAX, |a| a.line));
        self.ed.insert(text);
        self.handles = false;
        self.edited_at(ui, top);
    }

    /// The code-key row (files2-edit-code).
    pub fn code_key(&mut self, ui: &FilesApp, k: &str) {
        let code = match k {
            "Tab" => 9,
            "←" => 0xF807,
            "→" => 0xF808,
            s => s.chars().next().map_or(0, |c| c as u32),
        };
        self.key(ui, code);
    }

    pub fn tapped(&mut self, ui: &FilesApp, row: usize, x: usize) {
        let p = self.layout.pos_at(&self.ed.doc, row, x);
        self.ed.set_cursor(p);
        self.handles = false;
        self.show(ui);
    }

    /// A long press: the word there, with handles.
    pub fn held(&mut self, ui: &FilesApp, row: usize, x: usize) {
        let p = self.layout.pos_at(&self.ed.doc, row, x);
        let (a, b) = self.ed.word_at(p);
        self.ed.select(a, b);
        self.handles = true;
        self.show(ui);
    }

    pub fn handle_moved(&mut self, ui: &FilesApp, which: i32, row: usize, x: usize) {
        let Some((a, b)) = self.ed.selection() else {
            return;
        };
        let p = self.layout.pos_at(&self.ed.doc, row, x);
        let (from, to) = if which == 0 { (p, b) } else { (a, p) };
        if from < to {
            self.ed.select(from, to);
            self.window(ui);
        }
    }

    pub fn toggle_wrap(&mut self, ui: &FilesApp) {
        self.wrap = !self.wrap;
        self.relayout(ui);
        self.first = 0;
        ui.invoke_text_top();
        self.show(ui);
    }

    // ── find and replace ─────────────────────────────────────────────────────────────────────────────────────────

    pub fn find(&mut self, ui: &FilesApp, needle: &str) {
        self.needle = String::from(needle);
        self.find = find_all(&self.ed.doc, needle, false);
        self.find_at = next_from(&self.find, self.ed.cursor).unwrap_or(0);
        self.show_find(ui);
    }

    fn show_find(&mut self, ui: &FilesApp) {
        ui.set_find_info(if self.find.is_empty() {
            SharedString::from("0 of 0")
        } else {
            format!("{} of {}", self.find_at + 1, self.find.len())
                .as_str()
                .into()
        });
        if let Some(m) = self.find.get(self.find_at) {
            let (r, _) = self.layout.place(&self.ed.doc, m.0);
            ui.invoke_text_show_row(r as i32);
        }
        self.window(ui);
    }

    pub fn find_step(&mut self, ui: &FilesApp, back: bool) {
        if self.find.is_empty() {
            return;
        }
        let n = self.find.len();
        self.find_at = if back {
            (self.find_at + n - 1) % n
        } else {
            (self.find_at + 1) % n
        };
        self.show_find(ui);
    }

    pub fn replace(&mut self, ui: &FilesApp, all: bool, with: &str, needle: &str) {
        if self.find.is_empty() || self.read_only {
            return;
        }
        let ranges: Vec<(Pos, Pos)> = if all {
            self.find.clone()
        } else {
            alloc::vec![self.find[self.find_at]]
        };
        let top = ranges[0].0.line;
        self.ed.replace_ranges(&ranges, with);
        self.edited_at(ui, top);
        self.find(ui, needle);
    }

    // ── the table ────────────────────────────────────────────────────────────────────────────────────────────────

    pub fn cell_picked(&mut self, ui: &FilesApp, r: usize, c: usize) {
        // what was typed in the last cell stays
        if self.cell.is_some_and(|p| p != (r, c)) {
            self.cell_done(ui);
        }
        self.cell = Some((r, c));
        if let Some(t) = &self.table {
            ui.set_cell_text(t.cell(r, c).into());
        }
        self.show(ui);
    }

    pub fn cell_done(&mut self, ui: &FilesApp) {
        if let (Some(t), Some((r, c))) = (self.table.as_mut(), self.cell) {
            t.set(r, c, ui.get_cell_text().as_str());
        }
        self.cell = None;
        self.show(ui);
    }

    pub fn table_menu(&mut self, ui: &FilesApp, m: &str) {
        let Some(t) = self.table.as_mut() else { return };
        let at = self.cell.map_or(t.rows(), |(r, _)| r + 1);
        match m {
            "Add row" => t.add_row(at),
            "Add column" => t.add_col(),
            "Delete row" => {
                if let Some((r, _)) = self.cell {
                    t.delete_row(r);
                    self.cell = None;
                }
            }
            _ => {}
        }
        self.show(ui);
    }

    pub fn undo(&mut self, ui: &FilesApp) {
        if let Some(t) = self.table.as_mut() {
            t.undo();
            self.show(ui);
            return;
        }
        let line = self.ed.cursor.line;
        if self.ed.undo() {
            self.edited_at(ui, line.min(self.ed.cursor.line));
            // a whole relayout: an undo can join or split many lines
            self.layout.update(&self.ed.doc, 0);
            self.hl.invalidate(0);
            self.show(ui);
        }
    }

    /// A Markdown task box tapped in Preview (`line`: its source line): `[ ]` ⇄ `[x]` there.
    pub fn toggle_task(&mut self, ui: &FilesApp, line: usize) {
        let Some(text) = self.ed.doc.lines.get(line).map(|l| l.text.clone()) else {
            return;
        };
        if let Some((col, done)) = neo_format_text::md::task_box(&text) {
            self.ed.replace_ranges(
                &[(Pos::new(line, col), Pos::new(line, col + 1))],
                if done { " " } else { "x" },
            );
            self.edited_at(ui, line);
        }
    }

    /// Write the file back: `<file>.~part`, then in place, with its attributes as they were.
    pub fn save(&mut self, fs: &E32Fs) -> Result<usize, i32> {
        if self.read_only {
            return Err(-21);
        }
        let bytes = match &self.table {
            Some(t) => t.encode(),
            None => self.ed.doc.encode(),
        };
        let att_before = fs.entry(&self.path).map(|e| e.att).unwrap_or(0);
        if att_before & att::READ_ONLY != 0 {
            return Err(-21);
        }
        store::write(fs, &self.path, &bytes).map_err(|e| e.code())?;
        let keep = att_before & (att::HIDDEN | att::SYSTEM);
        if keep != 0 {
            let _ = fs.set_att(&self.path, keep, 0);
        }
        self.ed.saved();
        if let Some(t) = self.table.as_mut() {
            t.saved();
        }
        self.size = bytes.len() as u64;
        Ok(bytes.len())
    }

    /// A copy of the unsaved text where the next start can find it (Quit while editing).
    pub fn keep_copy(&self, fs: &E32Fs) {
        let bytes = match &self.table {
            Some(t) => t.encode(),
            None => self.ed.doc.encode(),
        };
        let to = format!("{}unsaved\\{}", store::HOME, self.name);
        let _ = store::write(fs, &to, &bytes);
    }

    pub fn path_dir(&self) -> String {
        neo_fs::path::parent(&self.path).unwrap_or_else(|| self.path.to_string())
    }
}
