// SPDX-License-Identifier: GPL-3.0-only
// The FA7 boards (files2-view-vcf, -edit-vcf, -view-ics, -edit-ics, -view-font, -view-cert, -view-app,
// -view-unknown) as `snapshots/files2-<board>.png`, with the boards' sample data.
mod common;
use common::*;
use files_ui::pim;
use neo_format_pim::ical::Ics;
use neo_format_pim::vcard::Vcf;

const CONTACTS: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Պետրոսյան;Անի;;;\r\nFN:Անի Պետրոսյան\r\n\
TEL;TYPE=CELL:+374 91 234 567\r\nEMAIL:ani@example.am\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:3.0\r\nN:Орлов;Дмитрий;;;\r\nFN:Дмитрий Орлов\r\nTEL;TYPE=WORK:+7 916 555-01-23\r\n\
EMAIL:d.orlov@example.ru\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:3.0\r\nN:Carter;Sam;;;\r\nFN:Sam Carter\r\nTEL;TYPE=CELL:+44 7700 900123\r\n\
EMAIL:sam@example.com\r\nEND:VCARD\r\n";

const ANAHIT: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Hakobyan;Anahit;;;\r\nFN:Anahit Hakobyan\r\n\
ORG:Ararat Software\r\nTEL;TYPE=CELL:+374 91 234567\r\nTEL;TYPE=WORK:+374 10 512048\r\n\
EMAIL;TYPE=HOME:anahit@example.am\r\nEND:VCARD\r\n";

const CONCERT: &str = include_str!("../../neo-format-pim/tests/data/concert.ics");

#[test]
fn fa7_pim_boards() {
    let s = app();
    let ui = &s.ui;

    // files2-view-vcf: Ani and Dmitry ticked, Sam already in Contacts
    let v = Vcf::parse(CONTACTS).unwrap();
    let known = [false, false, true];
    ui.set_view("vcf".into());
    ui.set_name("contacts.vcf".into());
    ui.set_fa7_menu(strings(&["Show as hex…", "Open with…", "Details"]));
    ui.set_vcf_title(pim::count(v.len(), "contact", "contacts").into());
    ui.set_vcf_sub("vCard 3.0 · 18 KB · E:\\Downloads".into());
    ui.set_vcf_cards(model_of(
        (0..v.len())
            .map(|i| pim::card(&v, i, !known[i], known[i]))
            .collect(),
    ));
    ui.set_vcf_note(pim::vcf_note(&v, &known).into());
    ui.set_vcf_footer(pim::vcf_footer(2, 2, 3).into());
    ui.set_vcf_can_add(true);
    s.shoot("files2-view-vcf");

    // files2-edit-vcf: contact 1 of 3, the email focused on the board
    let a = Vcf::parse(ANAHIT).unwrap();
    let f = a.fields(0);
    let (phones, emails) = pim::phones_emails(&f);
    ui.set_pim_form("vcf".into());
    ui.set_form_line("contacts.vcf · contact 1 of 3".into());
    ui.set_form_initials(pim::initials(&a.title(0)).into());
    ui.set_form_tint(files_ui::colour(0xe0457b));
    ui.set_c_first(f.first.as_str().into());
    ui.set_c_last(f.last.as_str().into());
    ui.set_c_company(f.company.as_str().into());
    ui.set_c_phones(phones);
    ui.set_c_emails(emails);
    s.shoot("files2-edit-vcf");
    ui.set_pim_form("".into());

    // files2-view-ics: Winter Jazz Night
    let c = Ics::parse(CONCERT).unwrap();
    ui.set_view("ics".into());
    ui.set_name("concert.ics".into());
    ui.set_ics_events(model_of(vec![pim::event_card(&c, 0, "concert.ics")]));
    ui.set_ics_footer("1 event · 1 KB".into());
    s.shoot("files2-view-ics");

    // files2-edit-ics: the board's Komitas Quartet concert
    let e = c.event(0);
    let start = e.start.clone().unwrap();
    ui.set_pim_form("ics".into());
    ui.set_form_line("concert.ics · 1 event".into());
    ui.set_e_title("Komitas Quartet · concert".into());
    ui.set_e_place("Komitas Chamber Music House, Yerevan".into());
    ui.set_e_start_date(start.date.short().into());
    ui.set_e_start_time("19:00".into());
    ui.set_e_end_date(start.date.short().into());
    ui.set_e_end_time("21:30".into());
    ui.set_e_reminder(pim::reminder_text(e.reminder).into());
    ui.set_e_repeat(e.repeat.text().into());
    ui.set_e_zone(pim::zone_text(&c, &start).into());
    s.shoot("files2-edit-ics");
    ui.set_pim_form("".into());
    inspector_boards(&s);
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/../neo-format-inspect/tests/data/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// One Slint window per test binary: the inspectors follow the cards in the same test.
fn inspector_boards(s: &Shot) {
    use files_ui::inspect;
    use neo_format_inspect::{cert, font::Font, sis};
    let ui = &s.ui;
    let scale = 3.0;
    ui.set_fa7_menu(strings(&["Show as hex…", "Open with…", "Details"]));

    // files2-view-font: DejaVu Sans (the subset fixture)
    let data = fixture("DejaVuSans-subset.ttf");
    let mut f = Font::new(&data).unwrap();
    let fs = inspect::font_screen(&mut f, 740 << 10, scale, 316.0);
    ui.set_view("font".into());
    ui.set_name("DejaVuSans.ttf".into());
    ui.set_font_aa(fs.aa);
    ui.set_font_family_art(fs.family_art);
    ui.set_font_family(fs.family.as_str().into());
    ui.set_font_line1(fs.line1.as_str().into());
    ui.set_font_line2(fs.line2.as_str().into());
    ui.set_font_groups(model_of(fs.groups));
    ui.set_fa7_footer("Not installed. Installing fonts comes later.".into());
    s.shoot("files2-view-font");
    let (rows, note) = inspect::glyph_rows(&mut f, scale);
    assert!(!rows.is_empty());
    ui.set_font_glyphs(model_of(rows));
    ui.set_font_glyphs_note(note.into());
    ui.set_font_tab(1);
    s.shoot("files2-view-font-glyphs");
    ui.set_font_tab(0);

    // files2-view-cert: the Home Lab root, seen on 2026-11-20
    let certs = cert::parse(&fixture("root-ca.pem")).unwrap();
    let now = 1_795_132_800; // 2026-11-20
    let c = inspect::cert_screen(&certs[0], 0, certs.len(), now);
    ui.set_view("cert".into());
    ui.set_name("root-ca.pem".into());
    ui.set_cert_title(c.title.as_str().into());
    ui.set_cert_status(c.status.as_str().into());
    ui.set_cert_ok(c.ok);
    ui.set_info_groups(model_of(c.groups));
    ui.set_cert_fp1(c.fp[0].as_str().into());
    ui.set_cert_fp2(c.fp[1].as_str().into());
    ui.set_fa7_footer("Trusted certificates come with the browser".into());
    s.shoot("files2-view-cert");

    // files2-view-app: the synthetic X-plore package
    let pkg = fixture("sample.sisx");
    let p = sis::read(&pkg[..sis::needed(&pkg).unwrap()]).unwrap();
    let a = inspect::sis_screen(&p, 890 << 10, "SISX", now);
    ui.set_view("app".into());
    ui.set_name("XPlore.sisx".into());
    ui.set_app_title(a.title.as_str().into());
    ui.set_app_vendor(a.vendor.as_str().into());
    ui.set_app_meta(a.meta.as_str().into());
    ui.set_app_info_title(a.info_title.as_str().into());
    ui.set_app_info_text(a.info_text.as_str().into());
    ui.set_info_groups(model_of(a.groups));
    ui.set_app_caps_title(a.caps_title.as_str().into());
    ui.set_app_caps(model_of(a.caps));
    ui.set_app_note(a.note.as_str().into());
    s.shoot("files2-view-app");

    // files2-view-unknown: an ARM ELF header
    let elf: Vec<u8> = [
        0x7f, b'E', b'L', b'F', 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0x28, 0, 1, 0, 0, 0, 0,
        0x80, 0, 0, 0x34, 0, 0, 0, 0xa4, 0xf1, 0x1e, 0, 0, 2, 0, 5, 0x34, 0, 0x20, 0, 3, 0, 0x28,
        0,
    ]
    .to_vec();
    ui.set_view("unknown".into());
    ui.set_name("firmware.bin".into());
    ui.set_hex_badge("BIN".into());
    ui.set_hex_colour(files_ui::colour(0x5b6472));
    ui.set_hex_meta("2.0 MB · 12 Nov 2026 · E:\\Downloads".into());
    ui.set_hex_heading("No viewer for .bin".into());
    ui.set_hex_text(
        "Files can't show this type. Open it in another app, or look at its bytes.".into(),
    );
    ui.set_hex_info("2,097,152 bytes".into());
    ui.set_hex_rows(model_of(inspect::hex_rows(0, &elf)));
    ui.set_hex_total(6);
    s.shoot("files2-view-unknown");
}
