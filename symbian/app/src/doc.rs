// SPDX-License-Identifier: GPL-3.0-only
//! FA10 documents (files2-view-docx): a docx, odt or rtf read whole (it is small next to the heap), laid out by
//! files-ui's DocViewer at the advances Slint draws, in Page or Reflow mode, a window of it at a time.

use crate::fa7::Open7;
use crate::{FilesApp, State};
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use e32::dprintln;
use files_ui::doc::{DocViewer, Fonts, Mode};
use neo_format_document::layout::Face;
use neo_format_document::{ImageSrc, Kind, Package};
use neo_format_image::shrink::{Turn, fit};
use slint::ComponentHandle;

/// The largest document read.
const DOC_MAX: u64 = 32 << 20;
/// An image in a document decodes only within this many bytes of pixels.
/// shortcut: decoded on the UI thread when the document opens; the picture worker if documents with many big
/// photos open slowly
const IMAGE_BUDGET: usize = 24 << 20;

pub(crate) struct Open {
    v: DocViewer,
    pics: Vec<Option<slint::Image>>,
    info: String,
}

pub(crate) enum Act {
    Mode(i32),
    Scrolled(i32),
}

pub(crate) fn bind(ui: &FilesApp, q: &Rc<RefCell<VecDeque<crate::Action>>>) {
    let push = |q: &Rc<RefCell<VecDeque<crate::Action>>>| {
        let q = q.clone();
        move |a: Act| q.borrow_mut().push_back(crate::Action::Doc(a))
    };
    let p = push(q);
    ui.on_doc_mode_picked(move |i| p(Act::Mode(i)));
    let p = push(q);
    ui.on_doc_scrolled(move |px| p(Act::Scrolled(px)));
}

/// The documents this viewer reads, by name (the bytes are checked when it opens).
pub(crate) fn is_doc(name: &str) -> bool {
    matches!(
        Kind::of_ext(neo_view::ext_of(name)),
        Some(Kind::Docx | Kind::Odt | Kind::Rtf)
    )
}

/// The glyph advances, calibrated against Slint once per run.
fn fonts(ui: &FilesApp, st: &mut State) -> Fonts {
    *st.fonts.get_or_insert_with(|| {
        let f = Fonts::calibrate(|text, face: Face, px| {
            ui.set_probe_text(text.into());
            ui.set_probe_px(px);
            ui.set_probe_face(face as i32);
            ui.get_probe_width() as i32
        });
        if f.is_none() {
            dprintln!("files: doc fonts not calibrated");
        }
        f.unwrap_or(Fonts::guess())
    })
}

pub(crate) fn open(ui: &FilesApp, st: &mut State, o: &Open7) -> Result<&'static str, String> {
    let bytes = crate::fa7::read(st, &o.path, o.size, DOC_MAX)?;
    let kind = neo_format_document::detect(&bytes)
        .filter(|k| matches!(k, Kind::Docx | Kind::Odt | Kind::Rtf))
        .ok_or_else(|| String::from("It isn't a document we read"))?;
    let doc = neo_format_document::read(kind, &bytes[..]).map_err(|e| String::from(e.reason()))?;
    let pkg = Package::open(&bytes[..]).ok();
    let view_w = ui.get_doc_view_w().max(1) as u32;
    let mut pics = Vec::with_capacity(doc.images.len());
    let mut sizes = Vec::with_capacity(doc.images.len());
    for src in &doc.images {
        let data = match src {
            ImageSrc::Member(m) => pkg.as_ref().and_then(|p| p.read(m).ok()),
            ImageSrc::Bytes(b) => Some(b.clone()),
        };
        let px = data
            .and_then(|d| neo_format_image::decode(&d, IMAGE_BUDGET).ok())
            .map(|d| d.px);
        // the picture's own size for the layout, the pixels at most the view's width
        sizes.push(px.as_ref().map(|p| (p.w as i32, p.h as i32)));
        pics.push(px.map(|p| {
            files_ui::image::slint_image(&if p.w > view_w {
                fit(&p, Turn::NONE, view_w)
            } else {
                p
            })
        }));
    }
    let fonts = fonts(ui, st);
    let dp = ui.global::<files_ui::Theme>().get_dp();
    let mut v = DocViewer::new(doc, fonts, dp, Mode::Reflow);
    for (i, s) in sizes.into_iter().enumerate() {
        v.set_size(i, s);
    }
    v.layout(ui.get_doc_view_w(), ui.get_doc_view_h());
    v.scroll(0);
    dprintln!(
        "files: doc {:?} {} blocks, {} px",
        kind,
        v.doc.blocks.len(),
        v.total()
    );
    let info = files_ui::doc::info(neo_view::family_of(&o.name), o.size);
    let d = Open { v, pics, info };
    show(ui, &d);
    ui.invoke_doc_scroll_to(0);
    st.doc = Some(d);
    Ok("doc")
}

fn show(ui: &FilesApp, d: &Open) {
    files_ui::doc::show(ui, &d.v, &d.info, |id| d.pics.get(id).cloned().flatten());
}

pub(crate) fn act(ui: &FilesApp, st: &mut State, a: Act) {
    let Some(d) = st.doc.as_mut() else { return };
    match a {
        Act::Mode(m) => {
            let mode = if m == 0 { Mode::Page } else { Mode::Reflow };
            if mode == d.v.mode {
                return;
            }
            // keep the same share of the document in view
            let at = d.v.top as f32 / d.v.total().max(1) as f32;
            d.v.mode = mode;
            d.v.layout(ui.get_doc_view_w(), ui.get_doc_view_h());
            let top = (at * d.v.total() as f32) as i32;
            d.v.scroll(top);
            show(ui, d);
            ui.invoke_doc_scroll_to(top);
        }
        Act::Scrolled(px) => {
            if d.v.scroll(px) {
                show(ui, d);
            } else {
                ui.set_doc_percent(d.v.percent().into());
            }
        }
    }
}

pub(crate) fn close(st: &mut State) {
    st.doc = None;
}

/// `C:\Data\fa10\`: the plan as docx, odt and rtf (and textutil's copies), each with a broken copy as FA7's.
#[cfg(feature = "test-files")]
pub(crate) fn test_files(st: &State) {
    use alloc::format;
    use files_ops::store;
    use neo_fs::Fs as _;
    const DIR: &str = "C:\\Data\\fa10\\";
    if st.fs.entry(&format!("{DIR}plan-hy.docx")).is_ok() {
        return;
    }
    let files: [(&str, &[u8]); 6] = [
        (
            "plan-hy.docx",
            include_bytes!("../../../crates/neo-format-document/tests/data/plan-hy.docx"),
        ),
        (
            "plan-hy.odt",
            include_bytes!("../../../crates/neo-format-document/tests/data/plan-hy.odt"),
        ),
        (
            "plan-hy.rtf",
            include_bytes!("../../../crates/neo-format-document/tests/data/plan-hy.rtf"),
        ),
        (
            "tu-plan.docx",
            include_bytes!("../../../crates/neo-format-document/tests/data/tu-plan.docx"),
        ),
        (
            "tu-plan.odt",
            include_bytes!("../../../crates/neo-format-document/tests/data/tu-plan.odt"),
        ),
        (
            "tu-plan.rtf",
            include_bytes!("../../../crates/neo-format-document/tests/data/tu-plan.rtf"),
        ),
    ];
    for (name, bytes) in files {
        let _ = store::write(&*st.fs, &format!("{DIR}{name}"), bytes);
        let mut bad = bytes[..bytes.len() * 2 / 3].to_vec();
        for i in (7..bad.len()).step_by(97) {
            bad[i] ^= 0x5a;
        }
        let _ = store::write(&*st.fs, &format!("{DIR}broken-{name}"), &bad);
    }
}
