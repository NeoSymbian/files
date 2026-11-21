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
