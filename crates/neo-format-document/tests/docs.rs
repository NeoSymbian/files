// SPDX-License-Identifier: Apache-2.0
//! The FA10 fixtures (tests/data/make.py): the board's Armenian plan as docx, odt and rtf reads into the same
//! blocks; broken copies never panic.
use neo_format_document::{Block, Doc, Kind, ParaKind, detect, read};

fn data(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name),
    )
    .unwrap()
}

fn shape(d: &Doc) -> Vec<String> {
    d.blocks
        .iter()
        .map(|b| match b {
            Block::Para(p) => match &p.kind {
                ParaKind::Heading(l) => format!("h{l} {}", p.text()),
                ParaKind::Item { marker, .. } => {
                    format!("{marker} {}", p.text().trim_start_matches(['•', '\t']))
                }
                _ => format!("p {}", p.text().trim_start_matches(['•', '\t'])),
            },
            Block::Table(t) => format!(
                "table {}x{} {}",
                t.rows.len(),
                t.rows[0].len(),
                t.rows[2][1].paras[0].text()
            ),
            Block::Image(_) => String::from("image"),
            Block::PageBreak => String::from("page"),
            Block::Rule => String::from("rule"),
        })
        .collect()
}

#[test]
fn the_plan_in_three_formats() {
    let docx = read(Kind::Docx, data("plan-hy.docx")).unwrap();
    let odt = read(Kind::Odt, data("plan-hy.odt")).unwrap();
    let rtf = read(Kind::Rtf, data("plan-hy.rtf")).unwrap();
    let want = [
        "h1 Ծրագիր 2026",
        "p Նախագիծ · 30 հոկտեմբերի 2026",
        "h2 1. Նպատակներ",
        "p NeoSymbian-ը պետք է աշխատի Pixel 3a XL հեռախոսի վրա որպես ամենօրյա սարք։ Առաջին քայլը զանգերն ու հաղորդագրություններն են։",
        "• Զանգեր և SMS",
        "• Wi-Fi և դիտարկիչ",
        "• Ֆայլեր և երաժշտություն",
        "h2 2. Ժամանակացույց",
        "table 3x2 Նոր դիտարկիչ",
    ];
    let d = shape(&docx);
    assert_eq!(&d[..want.len()], want, "docx");
    assert_eq!(
        &d[want.len()..],
        [
            "page",
            "p Երկրորդ էջ: the plan goes on after a page break, with a picture below.",
            "image"
        ]
    );
    assert_eq!(docx.page.margin, [1134, 850, 1134, 1701]);
    assert_eq!(docx.body, 22);
    let o = shape(&odt);
    assert_eq!(&o[..want.len()], want, "odt");
    assert_eq!(
        &o[want.len()..],
        [
            "page",
            "p Երկրորդ էջ: the plan goes on after a page break, with a picture below.",
            "image"
        ]
    );
    assert_eq!(odt.page.margin[3], 1700);
    // rtf: headings are big bold paragraphs, and the list's bullets are text
    let r = shape(&rtf);
    assert!(r.contains(&String::from("p Ծրագիր 2026")), "{r:?}");
    assert!(r.contains(&String::from("p Զանգեր և SMS")), "{r:?}");
    assert!(r.contains(&String::from("table 3x2 Նոր դիտարկիչ")), "{r:?}");
    assert!(r.contains(&String::from("p Пример в cp1251")), "{r:?}");
    assert!(
        r.ends_with(&[
            String::from("page"),
            String::from(
                "p Երկրորդ էջ: the plan goes on after a page break, with a picture below."
            ),
            String::from("image")
        ]),
        "{r:?}"
    );
    assert_eq!(rtf.page.margin, [1134, 850, 1134, 1701]);
    assert_eq!(rtf.images.len(), 1);

    // the run styles came through
    let Block::Para(p) = &docx.blocks[3] else {
        panic!()
    };
    assert!(
        p.runs
            .iter()
            .any(|r| r.text == "Pixel 3a XL" && r.style.bold)
    );
    assert!(
        p.runs
            .iter()
            .any(|r| r.text == "ամենօրյա" && r.style.italic)
    );
    let Block::Para(p) = &odt.blocks[3] else {
        panic!()
    };
    assert!(
        p.runs
            .iter()
            .any(|r| r.text == "Pixel 3a XL" && r.style.bold)
    );
    let Block::Para(h) = &docx.blocks[2] else {
        panic!()
    };
    assert_eq!(h.runs[0].style.colour, Some(0x1f4e8c));
}

#[test]
fn detects_by_content() {
    assert_eq!(detect(&data("plan-hy.docx")), Some(Kind::Docx));
    assert_eq!(detect(&data("plan-hy.odt")), Some(Kind::Odt));
    assert_eq!(detect(&data("plan-hy.rtf")), Some(Kind::Rtf));
    assert_eq!(detect(&b"PK\x03\x04 not really".to_vec()), None);
    assert_eq!(Kind::of_ext("DOCX"), Some(Kind::Docx));
}

/// Cuts and flipped bytes (xorshift, as FA7's test) only ever give errors.
#[test]
fn broken_bytes_never_panic() {
    let mut seed: u64 = 0x2545_f491;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for (name, kind) in [
        ("plan-hy.docx", Kind::Docx),
        ("plan-hy.odt", Kind::Odt),
        ("plan-hy.rtf", Kind::Rtf),
    ] {
        let good = data(name);
        for k in 0..40 {
            let _ = read(kind, good[..good.len() * k / 40].to_vec());
        }
        for _ in 0..300 {
            let mut b = good.clone();
            for _ in 0..1 + rnd() % 4 {
                let at = (rnd() % b.len() as u64) as usize;
                b[at] ^= 1 << (rnd() % 8);
            }
            let _ = read(kind, b);
        }
    }
}

/// Files from another writer (macOS textutil, from our fixtures): the text and the table come through.
#[test]
fn textutil_files() {
    for (name, kind) in [
        ("tu-plan.docx", Kind::Docx),
        ("tu-plan.odt", Kind::Odt),
        ("tu-plan.rtf", Kind::Rtf),
    ] {
        let d = read(kind, data(name)).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let all: String = d
            .blocks
            .iter()
            .map(|b| match b {
                Block::Para(p) => p.text(),
                Block::Table(t) => t
                    .rows
                    .iter()
                    .flatten()
                    .flat_map(|c| c.paras.iter().map(|p| p.text()))
                    .collect::<Vec<_>>()
                    .join("|"),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        for want in [
            "Ծրագիր 2026",
            "Pixel 3a XL",
            "ամենօրյա",
            "Զանգեր և SMS",
            "Նոր դիտարկիչ",
        ] {
            assert!(all.contains(want), "{name}: no {want} in\n{all}");
        }
        // textutil's docx writer drops tables (its own HTML of the same rtf has one)
        if kind != Kind::Docx {
            assert!(
                d.blocks.iter().any(|b| matches!(b, Block::Table(_))),
                "{name}: no table"
            );
        }
    }
}

/// A picture's size from its member's first bytes (the viewer never reads the rest on the UI thread).
#[test]
fn picture_heads() {
    let file = data("plan-hy.docx");
    let p = neo_format_document::Package::open(&file[..]).unwrap();
    let head = p.head("word/media/image1.png", 64).unwrap();
    assert_eq!(head.len(), 64);
    let all = p.read("word/media/image1.png").unwrap();
    assert_eq!(&all[..64], &head[..]);
    assert_eq!(p.head("word/media/image1.png", 1 << 20).unwrap(), all);
    assert!(p.head("word/media/missing.png", 64).is_err());
}

#[test]
fn epub_books() {
    use neo_format_document::{Error, Package, epub};
    let file = data("book-300.epub");
    assert_eq!(detect(&file), Some(Kind::Epub));
    let p = Package::open(&file[..]).unwrap();
    let book = epub::open(&p).unwrap();
    assert_eq!(book.title, "Symbian^3 book");
    assert_eq!(book.spine.len(), 12);
    assert_eq!(book.spine[2], "OEBPS/c3.xhtml");
    assert_eq!(book.toc.len(), 12);
    assert_eq!(book.toc[2].title, "Chapter 3");
    assert_eq!(book.toc[2].chapter, 2);
    assert_eq!(book.title_of(5), Some("Chapter 6"));
    let c1 = epub::chapter(&p, &book, 0).unwrap();
    assert_eq!(
        c1.images,
        [neo_format_document::ImageSrc::Member(String::from(
            "OEBPS/images/cover.png"
        ))]
    );
    let s = shape(&c1);
    assert_eq!(s[0], "h2 Chapter 1");
    assert_eq!(s[1], "image");
    assert!(s.len() > 25, "{}", s.len());
    let text: usize = (0..12)
        .map(|i| {
            shape(&epub::chapter(&p, &book, i).unwrap())
                .iter()
                .map(|l| l.len())
                .sum::<usize>()
        })
        .sum();
    assert!(text > 70_000, "{text}");

    let drm = data("drm.epub");
    assert_eq!(
        epub::open(&Package::open(&drm[..]).unwrap()),
        Err(Error::Protected)
    );
    let none = data("no-spine.epub");
    assert_eq!(
        epub::open(&Package::open(&none[..]).unwrap()),
        Err(Error::Bad("The book has no chapters"))
    );
}

#[test]
fn broken_books_never_panic() {
    use neo_format_document::{Package, epub};
    let good = data("book-300.epub");
    let mut seed: u64 = 0x2545_f491;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let try_all = |b: &[u8]| {
        if let Ok(p) = Package::open(b)
            && let Ok(book) = epub::open(&p)
        {
            for i in 0..book.spine.len() {
                let _ = epub::chapter(&p, &book, i);
            }
        }
    };
    for k in 0..20 {
        try_all(&good[..good.len() * k / 20]);
    }
    for _ in 0..100 {
        let mut b = good.clone();
        for _ in 0..1 + rnd() % 4 {
            let at = (rnd() % b.len() as u64) as usize;
            b[at] ^= 1 << (rnd() % 8);
        }
        try_all(&b);
    }
}

#[test]
fn the_budget_workbook() {
    use neo_format_document::Package;
    use neo_format_document::xlsx::{self, Value, display, formula_bar};
    let file = data("budget-ru.xlsx");
    assert_eq!(detect(&file), Some(Kind::Xlsx));
    let p = Package::open(&file[..]).unwrap();
    let wb = xlsx::open(&p).unwrap();
    let names: Vec<&str> = wb.sheets.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["1 квартал", "2 квартал", "Итоги", "Графики"]);
    assert!(wb.sheets[3].part.is_none());
    let s = xlsx::sheet(&p, &wb, 0).unwrap();
    let cell = |r: &str| {
        let (row, col) = xlsx::parse_ref(r).unwrap();
        s.get(row, col).unwrap()
    };
    assert_eq!(display(&wb, cell("A2")), "Аренда");
    assert!(wb.bold[cell("E7").style as usize] && !wb.bold[cell("E6").style as usize]);
    assert_eq!(display(&wb, cell("B2")), "180\u{a0}000");
    assert_eq!(display(&wb, cell("E7")), "1\u{a0}038\u{a0}500");
    assert_eq!(formula_bar(&wb, cell("E7")), "=SUM(E2:E6)");
    assert_eq!(formula_bar(&wb, cell("E4")), "=SUM(B4:D4)");
    assert_eq!(formula_bar(&wb, cell("D10")), "=D9-D7");
    assert_eq!(formula_bar(&wb, cell("E9")), "=SUM(B9:D9)");
    assert_eq!(display(&wb, cell("A12")), "Бюджет утверждён");
    assert_eq!(s.merge_at(11, 0), Some(Some((11, 4))));
    assert_eq!(s.merge_at(11, 2), Some(None));
    assert_eq!(display(&wb, cell("A13")), "09.12.2025");
    assert_eq!(display(&wb, cell("B13")), "24.7%");
    assert_eq!(display(&wb, cell("C13")), "TRUE");
    assert_eq!(display(&wb, cell("D13")), "#DIV/0!");
    assert_eq!(s.width(0), 14.0);
    assert_eq!(s.width(9), 8.43);
    assert_eq!((s.rows, s.cols), (13, 5));
    assert!(matches!(cell("E10").value, Value::Num(n) if n == 341_500.0));
    assert_eq!(
        xlsx::sheet(&p, &wb, 3),
        Err(neo_format_document::Error::NotDocument)
    );

    // broken copies only give errors
    let mut seed: u64 = 0x2545_f491;
    for _ in 0..300 {
        let mut b = file.clone();
        for _ in 0..3 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let at = (seed % b.len() as u64) as usize;
            b[at] ^= 1 << (seed % 8);
        }
        if let Ok(p) = Package::open(&b[..])
            && let Ok(wb) = xlsx::open(&p)
        {
            for i in 0..wb.sheets.len() {
                if let Ok(s) = xlsx::sheet(&p, &wb, i) {
                    for c in &s.cells {
                        let _ = display(&wb, c);
                    }
                }
            }
        }
    }
}
