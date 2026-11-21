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

#[test]
fn layout_widths_are_slints() {
    let s = common::app();
    let m = probe(&s);
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
