// SPDX-License-Identifier: GPL-3.0-only
//! FA10 (documents): the layout measures what Slint draws, and the viewers' boards.
mod common;

use files_ui::doc::Fonts;
use neo_format_document::layout::{Face, Measure};

fn probe(s: &common::Shot) -> impl Fn(&str, Face, i32) -> i32 + '_ {
    |text, face, px| {
        s.ui.set_probe_text(text.into());
        s.ui.set_probe_px(px);
        s.ui.set_probe_face(face as i32);
        s.ui.get_probe_width() as i32
    }
}

/// One app per test binary: the widths, then the boards.
#[test]
fn fa10() {
    let s = common::app();
    layout_widths_are_slints(&s);
    docx_boards(&s);
}

fn layout_widths_are_slints(s: &common::Shot) {
    let m = probe(s);
    let fonts = Fonts::calibrate(&m).expect("one T that gives every sample");
    assert_eq!(fonts, Fonts::guess());
    let runs = [
        "Ծրագիր 2026 · Նախագիծ · 30 հոկտեմբերի 2026",
        "NeoSymbian-ը պետք է աշխատի Pixel 3a XL հեռախոսի վրա",
        "Бюджет 2026: Аренда, Еда, Транспорт — Итого «1 038 500 ₽»",
        "Ελληνικά: αβγδε ΑΒΓΔΕ; Ґанок, їжак, Єва; café, Łódź, Ştefan…",
        "A Symbian application spends most of its life waiting: RunL()",
        "  leading spaces and \"quotes\" — ‘single’ ‰ № ™ → ✓",
    ];
    let mut checked = 0;
    for text in runs {
        for face in [Face::Regular, Face::Bold, Face::Italic, Face::Mono] {
            for px in [20, 33, 40, 45, 51, 66, 78] {
                if face == Face::Mono && text.chars().any(|c| fonts.advance(c, face, px).is_none())
                {
                    continue; // the mono face has no Armenian
                }
                assert_eq!(
                    fonts.width(text, face, px),
                    m(text, face, px),
                    "{face:?} {px}px {text}"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 100, "{checked}");
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../neo-format-document/tests/data")
            .join(name),
    )
    .unwrap()
}

/// The doc's images decoded (a zip member, or bytes).
fn pictures(file: &[u8], doc: &neo_format_document::Doc) -> Vec<Option<neo_format_image::Pixels>> {
    let p = neo_format_document::Package::open(file);
    doc.images
        .iter()
        .map(|src| {
            let bytes = match src {
                neo_format_document::ImageSrc::Member(m) => p.as_ref().ok()?.read(m).ok()?,
                neo_format_document::ImageSrc::Bytes(b) => b.clone(),
            };
            neo_format_image::decode(&bytes, 8 << 20).ok().map(|d| d.px)
        })
        .collect()
}

fn show_doc(
    s: &common::Shot,
    file: &[u8],
    kind: neo_format_document::Kind,
    mode: files_ui::doc::Mode,
    name: &str,
) {
    use files_ui::doc::{DocViewer, info, show};
    let ui = &s.ui;
    ui.set_view("doc".into());
    ui.set_name(name.into());
    ui.set_fa7_menu(common::strings(&["Show as hex…", "Open with…", "Details"]));
    let fonts = Fonts::calibrate(probe(s)).unwrap();
    let doc = neo_format_document::read(kind, file.to_vec()).unwrap();
    let pics = pictures(file, &doc);
    let mut v = DocViewer::new(doc, fonts, common::DP, mode);
    for (i, p) in pics.iter().enumerate() {
        v.set_size(i, p.as_ref().map(|p| (p.w as i32, p.h as i32)));
    }
    s.shoot("_doc_warmup");
    v.layout(ui.get_doc_view_w(), ui.get_doc_view_h());
    v.scroll(0);
    let images: Vec<Option<slint::Image>> = pics
        .iter()
        .map(|p| p.as_ref().map(files_ui::image::slint_image))
        .collect();
    show(ui, &v, &info(neo_view::family_of(name), 48 * 1024), |id| {
        images.get(id).cloned().flatten()
    });
}

fn docx_boards(s: &common::Shot) {
    let file = fixture("plan-hy.docx");
    show_doc(
        s,
        &file,
        neo_format_document::Kind::Docx,
        files_ui::doc::Mode::Reflow,
        "Ծրագիր.docx",
    );
    s.shoot("files2-view-docx");
    show_doc(
        s,
        &file,
        neo_format_document::Kind::Docx,
        files_ui::doc::Mode::Page,
        "Ծրագիր.docx",
    );
    s.shoot("files2-view-docx-page");
    let _ = std::fs::remove_file(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../snapshots/_doc_warmup.png"),
    );
}
