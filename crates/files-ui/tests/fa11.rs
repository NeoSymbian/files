// SPDX-License-Identifier: GPL-3.0-only
//! FA11 (PDF): the viewer's boards drawn with the real engine (the board's boarding pass, the 100-page book), and the
//! viewer's layout: pages at the view's width, the current page, the pictures it wants, zoom about a point.
mod common;

use files_ui::image::slint_image;
use files_ui::pdf::{Kind, PdfViewer, Pictures, Tile, info, show_pdf};
use neo_format_document::pdf::Pdf;
use slint::Image;
use std::collections::BTreeMap;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../neo-format-document/tests/data")
            .join(name),
    )
    .unwrap()
}

/// What the device's worker would have drawn: every wanted picture.
#[derive(Default)]
struct Drawn {
    pages: BTreeMap<usize, Image>,
    tile: Option<Tile>,
    thumbs: BTreeMap<usize, Image>,
}

impl Pictures for Drawn {
    fn page(&self, page: usize) -> Option<Image> {
        self.pages.get(&page).cloned()
    }
    fn tile(&self) -> Option<Tile> {
        self.tile.clone()
    }
    fn thumb(&self, page: usize) -> Option<Image> {
        self.thumbs.get(&page).cloned()
    }
}

fn draw_all(pdf: &Pdf, v: &PdfViewer, d: &mut Drawn) {
    let mut r = pdf.renderer();
    for w in v.wanted() {
        let px = r.render(w.page, w.scale, w.rect).unwrap().px;
        let image = slint_image(&px);
        match w.kind {
            Kind::Page => {
                d.pages.insert(w.page, image);
            }
            Kind::Tile => d.tile = Some((w.page, w.rect, image)),
            Kind::Thumb => {
                d.thumbs.insert(w.page, image);
            }
        }
    }
}

/// One app per test binary: the layout checks, then the boards.
#[test]
fn fa11() {
    let s = common::app();
    layout();
    boarding_pass(&s);
    book(&s);
}

fn layout() {
    // Two A5 pages at 1080 px: 16 dp sides, 14 dp over, 12 dp between.
    let mut v = PdfViewer::new(vec![(420.0, 595.0), (420.0, 595.0)], 3.0);
    v.layout(1080, 1500);
    assert_eq!(v.slot(0), (48, 42, 984, 1394));
    assert_eq!(v.slot(1), (48, 42 + 1394 + 36, 984, 1394));
    assert_eq!(v.current(), 0);
    assert_eq!(v.pill(), "Page 1 / 2");
    let w = v.wanted();
    assert_eq!(
        (w[0].kind, w[0].page, w[0].rect),
        (Kind::Page, 0, (0, 0, 984, 1394))
    );
    assert!(
        w.iter()
            .any(|w| w.kind == Kind::Thumb && w.page == 1 && w.rect.0 == 0 && w.rect.2 == 114)
    );
    assert!(!w.iter().any(|w| w.kind == Kind::Tile));
    // Scrolled half way into page 2 it is the current one.
    assert!(v.scroll(v.top_of(1) + 300, 0));
    assert_eq!(v.pill(), "Page 2 / 2");
    // Zoom 2x about the view's middle: the point under it stays; one tile of the view is wanted.
    let (cx, cy) = (540, 750);
    let before = (v.left + cx - v.slot(1).0, v.top + cy - v.slot(1).1);
    v.zoom_at(2.0, cx, cy);
    let after = (v.left + cx - v.slot(1).0, v.top + cy - v.slot(1).1);
    assert!(
        (after.0 - 2 * before.0).abs() <= 2 && (after.1 - 2 * before.1).abs() <= 2,
        "{before:?} {after:?}"
    );
    let tile = v
        .wanted()
        .into_iter()
        .find(|w| w.kind == Kind::Tile)
        .unwrap();
    assert_eq!(tile.page, 1);
    assert_eq!((tile.rect.2, tile.rect.3), (1080, 1500));
    assert!((tile.scale - 2.0 * 984.0 / 420.0).abs() < 0.01);
    // Pages are still drawn whole at the screen's width.
    let page = v
        .wanted()
        .into_iter()
        .find(|w| w.kind == Kind::Page)
        .unwrap();
    assert_eq!(page.rect, (0, 0, 984, 1394));
    // A new size keeps the place on the page.
    v.zoom_at(1.0, cx, cy);
    let p = v.current();
    v.layout(1080, 1200);
    assert_eq!(v.current(), p);
    // Absurd zooms are clamped.
    v.zoom_at(40.0, 0, 0);
    assert_eq!(v.zoom, files_ui::pdf::ZOOM_MAX);
}

fn boarding_pass(s: &common::Shot) {
    let ui = &s.ui;
    let file = fixture("boarding-pass.pdf");
    let size = file.len() as u64;
    let pdf = Pdf::open(file, "").unwrap();
    ui.set_view("pdf".into());
    ui.set_name("boarding-pass.pdf".into());
    ui.set_fa7_menu(common::strings(&[
        "Go to page…",
        "Show as hex…",
        "Open with…",
        "Details",
    ]));
    s.shoot("_pdf_warmup");
    let mut v = PdfViewer::new((0..pdf.pages()).map(|p| pdf.size(p)).collect(), common::DP);
    v.layout(ui.get_pdf_view_w(), ui.get_pdf_view_h());
    let mut d = Drawn::default();
    draw_all(&pdf, &v, &mut d);
    let info = info(size, pdf.pages());
    assert_eq!(info, "43 KB · 2 pages");
    show_pdf(ui, &v, &d, &info);
    s.shoot("files2-view-pdf");
    // Pinched to 2.5x over the QR: the sharp tile of the view.
    v.zoom_at(2.5, 200, 1100);
    draw_all(&pdf, &v, &mut d);
    show_pdf(ui, &v, &d, &info);
    ui.invoke_pdf_scroll_to(v.top);
    ui.invoke_pdf_scroll_x_to(v.left);
    s.shoot("files2-view-pdf-zoom");
    // Go to page…
    ui.set_pdf_ask("page".into());
    ui.set_pdf_ask_text("2".into());
    s.shoot("files2-view-pdf-page");
    ui.set_pdf_ask("".into());
    let _ = std::fs::remove_file(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../snapshots/_pdf_warmup.png"),
    );
}

fn book(s: &common::Shot) {
    let ui = &s.ui;
    // The locked file asks for its password first.
    ui.set_name("locked.pdf".into());
    ui.set_pdf_pages(Default::default());
    ui.set_pdf_thumbs(Default::default());
    ui.set_pdf_info("1 KB".into());
    ui.set_pdf_pill("".into());
    ui.set_pdf_ask("password".into());
    ui.set_pdf_ask_text("12".into());
    ui.set_pdf_ask_error("Wrong password".into());
    s.shoot("files2-view-pdf-password");
    ui.set_pdf_ask("".into());
    ui.set_pdf_ask_error("".into());

    let file = fixture("book-100.pdf");
    let size = file.len() as u64;
    let pdf = Pdf::open(file, "").unwrap();
    ui.set_name("book-100.pdf".into());
    let mut v = PdfViewer::new((0..pdf.pages()).map(|p| pdf.size(p)).collect(), common::DP);
    v.layout(ui.get_pdf_view_w(), ui.get_pdf_view_h());
    // Page 41 (Chapter 5, from the outline), as Contents would jump.
    let outline = pdf.outline();
    let five = outline.iter().position(|e| e.title == "Chapter 5").unwrap();
    v.scroll(v.top_of(outline[five].page.unwrap()), 0);
    assert_eq!(v.current(), 40);
    let mut d = Drawn::default();
    draw_all(&pdf, &v, &mut d);
    show_pdf(ui, &v, &d, &info(size, pdf.pages()));
    ui.invoke_pdf_scroll_to(v.top);
    s.shoot("files2-view-pdf-book");
    let titles: Vec<&str> = outline.iter().map(|e| e.title.as_str()).collect();
    ui.set_pdf_contents(common::strings(&titles));
    ui.set_pdf_contents_depth(
        std::rc::Rc::new(slint::VecModel::from(
            outline.iter().map(|e| e.depth as i32).collect::<Vec<_>>(),
        ))
        .into(),
    );
    ui.set_pdf_contents_current(five as i32);
    ui.set_pdf_contents_open(true);
    s.shoot("files2-view-pdf-contents");
    ui.set_pdf_contents_open(false);
}
