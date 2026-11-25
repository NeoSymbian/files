// SPDX-License-Identifier: Apache-2.0
//! The FA11 fixtures (tests/data/make_pdf.py): the board's boarding pass draws its QR, colours and text (embedded
//! fonts, Armenian); the 100-page book has its outline; the locked file needs its password; hostile and broken
//! copies give errors or partial pages, never a panic. `NEO_PDF_PNG=dir` writes the drawn pages there to look at.
#![cfg(feature = "pdf")]

use neo_format_document::Error;
use neo_format_document::pdf::{Pdf, is_pdf};
use neo_format_image::Pixels;

fn data(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name),
    )
    .unwrap()
}

/// Page `n` whole at `scale` pixels a point.
fn page(pdf: &Pdf, n: usize, scale: f32) -> Pixels {
    let (w, h) = pdf.size(n);
    let size = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
    let drawn = pdf
        .renderer()
        .render(n, scale, (0, 0, size.0, size.1))
        .unwrap();
    drawn.px
}

fn save(px: &Pixels, name: &str) {
    if let Some(dir) = std::env::var_os("NEO_PDF_PNG") {
        std::fs::write(
            std::path::Path::new(&dir).join(name),
            neo_format_image::encode::png(px),
        )
        .unwrap();
    }
}

fn at(px: &Pixels, x: f32, y: f32) -> [u8; 3] {
    let i = (y as usize * px.w as usize + x as usize) * 3;
    [px.px[i], px.px[i + 1], px.px[i + 2]]
}

/// How many pixels in the box (points × scale) are dark.
fn ink(px: &Pixels, scale: f32, (x0, y0, x1, y1): (f32, f32, f32, f32)) -> usize {
    let mut n = 0;
    for y in (y0 * scale) as usize..(y1 * scale) as usize {
        for x in (x0 * scale) as usize..(x1 * scale) as usize {
            let i = (y * px.w as usize + x) * 3;
            if px.px[i..i + 3].iter().all(|&c| c < 120) {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn boarding_pass() {
    let b = data("boarding-pass.pdf");
    assert!(is_pdf(&b));
    let pdf = Pdf::open(b, "").unwrap();
    assert_eq!(pdf.pages(), 2);
    assert_eq!(pdf.size(0), (420.0, 595.0));
    assert_eq!(pdf.title().as_deref(), Some("Boarding pass RA 214"));
    let s = 2.0;
    let px = page(&pdf, 0, s);
    save(&px, "boarding-pass-1.png");
    assert_eq!((px.w, px.h), (840, 1190));
    // The header bar, #1f4e8c.
    let blue = at(&px, 5.0 * s, 5.0 * s);
    assert!(
        blue.iter()
            .zip([0x1f, 0x4e, 0x8c])
            .all(|(a, b)| a.abs_diff(b) <= 2),
        "header {blue:?}"
    );
    // Every QR module where the .qr file says, dark or light at its centre.
    let qr = String::from_utf8(data("boarding-pass.qr")).unwrap();
    let mut lines = qr.lines();
    let head = lines.next().unwrap();
    // "... modules of 3.84 pt, top left at (20.0, 313.0) pt ..."
    let nums: Vec<f32> = head
        .split("modules of")
        .nth(1)
        .unwrap()
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter_map(|t| t.parse().ok())
        .collect();
    let (m, x0, y0) = (nums[0], nums[1], nums[2]);
    for (r, row) in lines.enumerate() {
        for (c, bit) in row.chars().enumerate() {
            let v = at(
                &px,
                (x0 + (c as f32 + 0.5) * m) * s,
                (y0 + (r as f32 + 0.5) * m) * s,
            );
            if bit == '1' {
                assert!(
                    v.iter().all(|&c| c < 60),
                    "module {r},{c} should be dark: {v:?}"
                );
            } else {
                assert!(
                    v.iter().all(|&c| c > 200),
                    "module {r},{c} should be light: {v:?}"
                );
            }
        }
    }
    // Text drawn with the embedded subset fonts: the passenger name, the Armenian line.
    assert!(
        ink(&px, s, (20.0, 96.0, 250.0, 112.0)) > 300,
        "passenger name"
    );
    assert!(
        ink(&px, s, (20.0, 477.0, 120.0, 495.0)) > 150,
        "the Armenian line"
    );
    // Page 2: the JPEG (warm sky at its top left) and the half-transparent box over it.
    let p2 = page(&pdf, 1, 1.0);
    save(&p2, "boarding-pass-2.png");
    let sky = at(&p2, 30.0, 595.0 - 385.0);
    assert!(sky[0] > 200 && sky[2] < 160, "the JPEG's sky {sky:?}");
    let boxed = at(&p2, 300.0, 595.0 - 245.0);
    assert!(
        boxed[2] > boxed[0] && boxed[0] > 40,
        "half-transparent blue over the picture {boxed:?}"
    );
}

#[test]
fn a_window_of_a_page_matches_the_whole() {
    let pdf = Pdf::open(data("boarding-pass.pdf"), "").unwrap();
    let whole = page(&pdf, 0, 2.0);
    let win = pdf
        .renderer()
        .render(0, 2.0, (100, 300, 200, 150))
        .unwrap()
        .px;
    let mut differ = 0;
    for y in 0..150 {
        for x in 0..200 {
            let a = at(&whole, (100 + x) as f32, (300 + y) as f32);
            let b = at(&win, x as f32, y as f32);
            differ += usize::from(a.iter().zip(b).any(|(a, b)| a.abs_diff(b) > 8));
        }
    }
    assert!(differ < 30, "{differ} pixels differ");
}

#[test]
fn book_100() {
    let pdf = Pdf::open(data("book-100.pdf"), "").unwrap();
    assert_eq!(pdf.pages(), 100);
    assert_eq!(pdf.size(99), (612.0, 792.0));
    let outline = pdf.outline();
    let shape: Vec<(String, Option<usize>, usize)> = outline
        .iter()
        .map(|e| (e.title.clone(), e.page, e.depth))
        .collect();
    assert_eq!(shape.len(), 12);
    assert_eq!(shape[0], ("Chapter 1".into(), Some(0), 0));
    assert_eq!(shape[1], ("Section 1.1".into(), Some(3), 1));
    assert_eq!(shape[2], ("Section 1.2".into(), Some(6), 1));
    assert_eq!(shape[4], ("Գլուխ 3 · Chapter 3".into(), Some(20), 0));
    assert_eq!(shape[6], ("Chapter 5".into(), Some(40), 0), "a GoTo action");
    assert_eq!(shape[11], ("Chapter 10".into(), Some(90), 0));
    // Helvetica isn't embedded: the standard-font fallback draws it.
    let mut r = pdf.renderer();
    for n in [0, 1, 57, 99] {
        let d = r.render(n, 1.0, (0, 0, 612, 792)).unwrap();
        assert!(!d.partial, "page {n}");
        assert!(
            ink(&d.px, 1.0, (72.0, 40.0, 540.0, 120.0)) > 500,
            "page {n} has text"
        );
        if n == 0 {
            save(&d.px, "book-1.png");
        }
    }
}

#[test]
fn passwords() {
    let b = data("locked.pdf");
    assert_eq!(Pdf::open(b.clone(), "").err(), Some(Error::Password));
    assert_eq!(Pdf::open(b.clone(), "4321").err(), Some(Error::Password));
    let pdf = Pdf::open(b, "1234").unwrap();
    let px = page(&pdf, 0, 1.0);
    save(&px, "locked.png");
    assert_eq!(
        at(&px, 150.0, 792.0 - 620.0),
        [0, 128, 0],
        "the decrypted page's green box"
    );
    assert!(ink(&px, 1.0, (72.0, 70.0, 300.0, 95.0)) > 200, "its text");
}

#[test]
fn macos_quartz_pdf() {
    let pdf = Pdf::open(data("quartz.pdf"), "").unwrap();
    let px = page(&pdf, 0, 1.5);
    save(&px, "quartz.png");
    // Three lines of text near the top (Armenian, Latin, Cyrillic) in Quartz's subset fonts.
    let (w, h) = pdf.size(0);
    assert!(ink(&px, 1.5, (0.0, 0.0, w, h / 3.0)) > 500);
}

#[test]
fn hostile_files() {
    // Forms 200 deep: hayro stops at its nesting limit; the page's own red box still draws.
    let pdf = Pdf::open(data("deep.pdf"), "").unwrap();
    let px = page(&pdf, 0, 1.0);
    assert_eq!(at(&px, 25.0, 175.0), [255, 0, 0]);
    // A page tree that loops beside a page with a huge MediaBox: an error or a page, no panic, no hang.
    if let Ok(pdf) = Pdf::open(data("loop.pdf"), "") {
        for n in 0..pdf.pages() {
            let _ = pdf.renderer().render(n, 1e-6, (0, 0, 64, 64));
        }
    }
    // A tiling cell 3e9 wide with a step of 1: refused (no 2^31 copies), the rest of the page draws, quickly.
    let t0 = std::time::Instant::now();
    let pdf = Pdf::open(data("tiling.pdf"), "").unwrap();
    let d = pdf.renderer().render(0, 2.0, (0, 0, 400, 400)).unwrap();
    assert!(t0.elapsed().as_secs() < 10);
    assert_eq!(at(&d.px, 20.0, 380.0), [255, 0, 0]);
    // 40 nested groups, each under its own soft mask, at a big size: within the memory budget (partly left out),
    // the last red box still drawn.
    let pdf = Pdf::open(data("masks.pdf"), "").unwrap();
    let d = pdf.renderer().render(0, 10.0, (0, 0, 2000, 2000)).unwrap();
    assert_eq!(at(&d.px, 1900.0, 100.0), [255, 0, 0]);
    // The cross-reference offsets are all wrong: hayro finds the objects again.
    let pdf = Pdf::open(data("broken-xref.pdf"), "").unwrap();
    let px = page(&pdf, 0, 1.0);
    assert_eq!(at(&px, 200.0, 200.0), [0, 0, 255]);
    // Not PDFs.
    assert!(Pdf::open(b"%PDF-1.7\nnothing", "").is_err());
    assert!(Pdf::open(Vec::new(), "").is_err());
    assert!(!is_pdf(b"PK\x03\x04"));
    // Limits.
    let pdf = Pdf::open(data("book-100.pdf"), "").unwrap();
    assert_eq!(
        pdf.renderer().render(0, 1.0, (0, 0, 5000, 5000)).err(),
        Some(Error::TooBig)
    );
    assert!(pdf.renderer().render(100, 1.0, (0, 0, 10, 10)).is_err());
    assert!(pdf.renderer().render(0, f32::NAN, (0, 0, 10, 10)).is_err());
}

/// Cuts and flipped bytes (xorshift, as FA7's and FA10's tests) only ever give errors or (partial) pages.
#[test]
fn broken_bytes_never_panic() {
    let mut seed: u64 = 0x2545_f491;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let draw = |b: Vec<u8>| {
        if let Ok(pdf) = Pdf::open(b, "") {
            let _ = pdf.outline();
            let mut r = pdf.renderer();
            for n in 0..pdf.pages().min(2) {
                let (w, h) = pdf.size(n);
                let s = 120.0 / w.max(h).max(1.0);
                let _ = r.render(
                    n,
                    s,
                    (
                        0,
                        0,
                        ((w * s) as u32).clamp(1, 200),
                        ((h * s) as u32).clamp(1, 200),
                    ),
                );
            }
        }
    };
    for name in [
        "boarding-pass.pdf",
        "book-100.pdf",
        "locked.pdf",
        "quartz.pdf",
        "broken-xref.pdf",
    ] {
        let good = data(name);
        for k in 0..24 {
            draw(good[..good.len() * k / 24].to_vec());
        }
        for _ in 0..60 {
            let mut b = good.clone();
            for _ in 0..1 + rnd() % 4 {
                let at = (rnd() % b.len() as u64) as usize;
                b[at] ^= 1 << (rnd() % 8);
            }
            draw(b);
        }
    }
}
