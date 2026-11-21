// SPDX-License-Identifier: GPL-3.0-only
//! FA10 workbooks (files2-view-xlsx): the workbook's sheets, strings and formats read through the open file, a sheet
//! at a time (at most 200,000 cells), shown in files-ui's grid; taps select a cell, a column or a row.

use crate::audio::At;
use crate::fa7::Open7;
use crate::{FilesApp, State};
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::String;
use core::cell::RefCell;
use e32::dprintln;
use files_ui::doc::{SheetViewer, show_sheet};
use neo_format_archive::Member;
use neo_format_document::{Kind, Package, xlsx};
use neo_fs::{Fs as _, Open as FsOpen};
use slint::ComponentHandle;

pub(crate) struct Open {
    v: SheetViewer,
    path: String,
    members: alloc::vec::Vec<Member>,
}

pub(crate) enum Act {
    Scrolled(i32, i32),
    Tapped(i32, i32),
    Head(bool, i32),
    Picked(usize),
}

pub(crate) fn bind(ui: &FilesApp, q: &Rc<RefCell<VecDeque<crate::Action>>>) {
    let push = |q: &Rc<RefCell<VecDeque<crate::Action>>>| {
        let q = q.clone();
        move |a: Act| q.borrow_mut().push_back(crate::Action::Sheet(a))
    };
    let p = push(q);
    ui.on_sheet_scrolled(move |x, y| p(Act::Scrolled(x, y)));
    let p = push(q);
    ui.on_sheet_tapped(move |x, y| p(Act::Tapped(x, y)));
    let p = push(q);
    ui.on_sheet_head_tapped(move |col, at| p(Act::Head(col, at)));
    let p = push(q);
    ui.on_sheet_picked(move |i| p(Act::Picked(i.max(0) as usize)));
}

pub(crate) fn is_sheet(name: &str) -> bool {
    Kind::of_ext(neo_view::ext_of(name)) == Some(Kind::Xlsx)
}

fn package<'a>(
    st: &'a State,
    path: &str,
    members: Option<alloc::vec::Vec<Member>>,
) -> Result<Package<At<fs_e32::E32File<'a>>>, String> {
    let io = |e: neo_fs::Error| files_core::error_text(e.code());
    let f = st.fs.open(path, FsOpen::Read).map_err(io)?;
    let size = neo_fs::File::size(&f).map_err(io)?;
    match members {
        Some(m) => Ok(Package::with(At(f, size), m)),
        None => Package::open(At(f, size)).map_err(|e| String::from(e.reason())),
    }
}

pub(crate) fn open(ui: &FilesApp, st: &mut State, o: &Open7) -> Result<&'static str, String> {
    let p = package(st, &o.path, None)?;
    let reason = |e: neo_format_document::Error| String::from(e.reason());
    let wb = xlsx::open(&p).map_err(reason)?;
    // the first sheet with cells (a workbook may start with a chart)
    let first = wb.sheets.iter().position(|s| s.part.is_some()).unwrap_or(0);
    let sheet = match xlsx::sheet(&p, &wb, first) {
        Ok(s) => Some(s),
        Err(neo_format_document::Error::NotDocument) => None,
        Err(e) => return Err(reason(e)),
    };
    let members = p.members.clone();
    drop(p);
    let dp = ui.global::<files_ui::Theme>().get_dp();
    let mut v = SheetViewer::new(wb, dp);
    v.set_sheet(first, sheet, ui.get_sheet_view_w(), ui.get_sheet_view_h());
    dprintln!(
        "files: sheet {} of {}, {} cells",
        first,
        v.wb.sheets.len(),
        v.sheet.as_ref().map_or(0, |s| s.cells.len())
    );
    show_sheet(ui, &v);
    st.workbook = Some(Open {
        v,
        path: o.path.clone(),
        members,
    });
    Ok("sheet")
}

pub(crate) fn act(ui: &FilesApp, st: &mut State, a: Act) {
    let Some(s) = st.workbook.as_mut() else {
        return;
    };
    match a {
        Act::Scrolled(x, y) => {
            if s.v.scrolled(x, y) {
                show_sheet(ui, &s.v);
            }
        }
        Act::Tapped(x, y) => {
            s.v.tap(x, y);
            show_sheet(ui, &s.v);
        }
        Act::Head(col, at) => {
            s.v.head_tap(col, at);
            show_sheet(ui, &s.v);
        }
        Act::Picked(i) if i != s.v.index && i < s.v.wb.sheets.len() => {
            let path = s.path.clone();
            let members = s.members.clone();
            let r = package(st, &path, Some(members)).and_then(|p| {
                let Some(s) = st.workbook.as_ref() else {
                    return Err(String::new());
                };
                match xlsx::sheet(&p, &s.v.wb, i) {
                    Ok(sh) => Ok(Some(sh)),
                    Err(neo_format_document::Error::NotDocument) => Ok(None),
                    Err(e) => Err(String::from(e.reason())),
                }
            });
            let Some(s) = st.workbook.as_mut() else {
                return;
            };
            match r {
                Ok(sh) => {
                    s.v.set_sheet(i, sh, ui.get_sheet_view_w(), ui.get_sheet_view_h())
                }
                Err(why) => dprintln!("files: sheet {} {}", i, why),
            }
            show_sheet(ui, &s.v);
            ui.invoke_sheet_scroll_to(0, 0);
        }
        Act::Picked(_) => {}
    }
}

pub(crate) fn close(st: &mut State) {
    st.workbook = None;
}
