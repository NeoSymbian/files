// SPDX-License-Identifier: GPL-3.0-only
//! FA7, cards and inspectors: a .vcf's contact cards (Add to Contacts through NEOCONT's IMPORT page) and its form, an
//! .ics's event cards and form, the font sample, the certificate, the app package, and the bytes of anything else.
//! A file a viewer can't read opens in the hex view with the reason, never a panic.

use crate::{Ask, State, View, cut, note_with, open_text, return_to, toast};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use e32::dprintln;
use files_ops::store;
use files_ui::{FilesApp, PimField, inspect, pim};
use neo_format_inspect::{cert, font::Font, jad, sis};
use neo_format_pim::date::Date;
use neo_format_pim::ical::{Event, Ics, Repeat, When};
use neo_format_pim::vcard::{Fields, Label, Vcf};
use neo_fs::{File as _, Fs as _, Open};
use neo_view::Family;
use neoapp::{App, Query};
use slint::{ComponentHandle as _, Model as _, ModelRc, VecModel};

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

fn strs<S: AsRef<str>>(v: &[S]) -> ModelRc<slint::SharedString> {
    model(v.iter().map(|s| s.as_ref().into()).collect())
}

/// What the FA7 callbacks ask.
pub(crate) enum Act {
    Menu(String),
    VcfToggle(usize),
    VcfOpen(usize),
    VcfAdd,
    Cancel,
    Save,
    LabelTap(bool, usize),
    LabelChosen(usize),
    FieldRemove(bool, usize),
    FieldAdd(bool),
    IcsOpen(usize),
    Pick(String),
    AllDay(bool),
    DateSet(i32, i32, i32),
    TimeSet(i32, i32),
    Chosen(usize),
    FontTab(usize),
    HexScrolled(usize),
    HexAsText,
}

/// NEOCONT and its IMPORT page (phone-core `page::number::contacts::IMPORT`): the arg is a .vcf path.
const CONTACTS: &str = "contacts";
const CONTACTS_IMPORT: u16 = 3;
/// NEOCONT's book (phone `store.rs`), read here only to mark the cards it has.
const BOOK: &str = "C:\\private\\e0005001\\contacts.dat";
/// The ticked cards handed to NEOCONT.
const ADD: &str = "C:\\private\\e0006001\\open\\add.vcf";
/// The largest file each viewer reads whole.
const CARDS_MAX: u64 = 16 << 20;
const FONT_MAX: u64 = 32 << 20;
const SMALL_MAX: u64 = 4 << 20;
/// shortcut: the hex view scrolls through the first 8 MB (a million rows keep the pane's coordinates exact in f32);
/// paging by offset when bigger files need it
const HEX_MAX: u64 = 8 << 20;

enum Kind {
    Vcf {
        v: Vcf,
        checked: Vec<bool>,
        known: Vec<bool>,
    },
    Ics(Ics),
    Font {
        data: Vec<u8>,
        glyphs: bool,
    },
    /// The certificate, the package: drawn once.
    Fixed,
    /// FA9's player (audio.rs keeps its state).
    Audio,
    /// FA10's document viewer (doc.rs keeps its state).
    Doc,
    /// FA10's reader (book.rs keeps its state).
    Book,
    /// FA10's grid (sheet.rs keeps its state).
    Sheet,
    Hex,
}

enum Picking {
    StartDate,
    StartTime,
    EndDate,
    EndTime,
    Reminder,
    Repeat,
}

enum Form {
    Card {
        i: usize,
        start: Fields,
        phones: Rc<VecModel<PimField>>,
        emails: Rc<VecModel<PimField>>,
        label: (bool, usize),
    },
    Event {
        i: usize,
        start: Event,
        ev: Event,
        picking: Picking,
    },
}

pub(crate) struct Open7 {
    pub path: String,
    pub(crate) name: String,
    pub(crate) size: u64,
    from: View,
    kind: Kind,
    form: Option<Form>,
    /// The cards' bytes weren't UTF-8 (a Latin-1 or UTF-16 vCard): shown, never written back (a save would lose
    /// what the decoding replaced).
    lossy: bool,
}

pub(crate) fn bind(
    ui: &FilesApp,
    q: &Rc<core::cell::RefCell<alloc::collections::VecDeque<crate::Action>>>,
) {
    let push = |q: &Rc<core::cell::RefCell<alloc::collections::VecDeque<crate::Action>>>| {
        let q = q.clone();
        move |a: Act| q.borrow_mut().push_back(crate::Action::Fa7(a))
    };
    let i = |n: i32| n.max(0) as usize;
    let p = push(q);
    ui.on_fa7_menu_picked(move |m| p(Act::Menu(m.to_string())));
    let p = push(q);
    ui.on_vcf_toggle(move |n| p(Act::VcfToggle(i(n))));
    let p = push(q);
    ui.on_vcf_open(move |n| p(Act::VcfOpen(i(n))));
    let p = push(q);
    ui.on_vcf_add(move || p(Act::VcfAdd));
    let p = push(q);
    ui.on_pim_cancel(move || p(Act::Cancel));
    let p = push(q);
    ui.on_pim_save(move || p(Act::Save));
    let p = push(q);
    ui.on_label_tap(move |m, n| p(Act::LabelTap(m, i(n))));
    let p = push(q);
    ui.on_label_chosen(move |n| p(Act::LabelChosen(i(n))));
    let p = push(q);
    ui.on_field_remove(move |m, n| p(Act::FieldRemove(m, i(n))));
    let p = push(q);
    ui.on_field_add(move |m| p(Act::FieldAdd(m)));
    let p = push(q);
    ui.on_ics_open(move |n| p(Act::IcsOpen(i(n))));
    let p = push(q);
    ui.on_e_pick(move |w| p(Act::Pick(w.to_string())));
    let p = push(q);
    ui.on_e_all_day_toggled(move |on| p(Act::AllDay(on)));
    let p = push(q);
    ui.on_date_set(move |y, m, d| p(Act::DateSet(y, m, d)));
    let p = push(q);
    ui.on_time_set(move |h, m| p(Act::TimeSet(h, m)));
    let p = push(q);
    ui.on_chosen(move |n| p(Act::Chosen(i(n))));
    let p = push(q);
    ui.on_font_tab_picked(move |n| p(Act::FontTab(i(n))));
    let p = push(q);
    ui.on_hex_scrolled(move |n| p(Act::HexScrolled(i(n))));
    let p = push(q);
    ui.on_hex_as_text(move || p(Act::HexAsText));
}

pub(crate) fn read(st: &State, path: &str, size: u64, max: u64) -> Result<Vec<u8>, String> {
    if size > max {
        return Err(format!(
            "It is too big to show ({})",
            files_core::size_text(size)
        ));
    }
    store::read(&*st.fs, path).map_err(|e| files_core::error_text(e.code()))
}

/// Text from bytes: UTF-8, or UTF-16 with a byte order mark (some phones export vCards so); and whether writing it
/// back as UTF-8 would change the file (not UTF-8, or replaced characters).
fn text_of(b: &[u8]) -> (String, bool) {
    let utf16 = |units: &mut dyn Iterator<Item = u16>| -> String {
        char::decode_utf16(units)
            .map(|r| r.unwrap_or('\u{fffd}'))
            .collect()
    };
    match b {
        [0xff, 0xfe, rest @ ..] => (
            utf16(
                &mut rest
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c)),
            ),
            true,
        ),
        [0xfe, 0xff, rest @ ..] => (
            utf16(
                &mut rest
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_be_bytes(*c)),
            ),
            true,
        ),
        _ => {
            let body = b.strip_prefix(b"\xef\xbb\xbf").unwrap_or(b);
            match core::str::from_utf8(body) {
                Ok(t) if body.len() == b.len() => (t.to_string(), false),
                // a UTF-8 BOM: the text is fine, but a save would drop the mark
                Ok(t) => (t.to_string(), true),
                Err(_) => (String::from_utf8_lossy(body).into_owned(), true),
            }
        }
    }
}

/// Open `path` in its FA7 viewer (or the hex view, with the reason when a viewer can't read it).
pub(crate) fn open(app: &App, ui: &FilesApp, st: &mut State, path: &str) {
    let name = neo_fs::path::name_of(path).to_string();
    let size = st.fs.entry(path).map(|e| e.size).unwrap_or(0);
    crate::fa3::opened(st, path);
    let from = match st.view {
        View::Fa7 => st.fa7.as_ref().map_or(View::Folder, |o| o.from),
        v => v,
    };
    let mut o = Open7 {
        path: path.to_string(),
        name,
        size,
        from,
        kind: Kind::Hex,
        form: None,
        lossy: false,
    };
    let ext = neo_view::ext_of(&o.name).to_ascii_lowercase();
    let r = match neo_view::family_of(&o.name) {
        _ if crate::doc::is_doc(&o.name) => {
            o.kind = Kind::Doc;
            crate::doc::open(ui, st, &o)
        }
        _ if crate::sheet::is_sheet(&o.name) => {
            o.kind = Kind::Sheet;
            crate::sheet::open(ui, st, &o)
        }
        _ if crate::book::is_book(&o.name) => {
            o.kind = Kind::Book;
            crate::book::open(app, ui, st, &o)
        }
        Family::Contact => cards(st, &mut o, &ext),
        Family::Font => font(ui, st, &mut o),
        Family::Cert => certificate(ui, st, &mut o),
        Family::App => package(ui, st, &mut o, &ext),
        Family::Audio => {
            o.kind = Kind::Audio;
            crate::audio::open(ui, st, &o)
        }
        _ => Err(String::new()),
    };
    dprintln!("files: fa7 {} {}", path, r.as_ref().map_or("hex", |_| "ok"));
    // after the viewer: the player compares it with the file's folder to reuse that folder's rows
    if let Some(dir) = neo_fs::path::parent(path) {
        st.dir = dir;
    }
    st.view = View::Fa7;
    st.fa7 = Some(o);
    match r {
        Ok(view) => {
            ui.set_view(view.into());
            show(app, ui, st);
        }
        Err(why) => hex(app, ui, st, &why),
    }
}

/// Back: the form (asking when it changed), else the page the viewer was opened from.
pub(crate) fn back(app: &App, ui: &FilesApp, st: &mut State) {
    if st.fa7.as_ref().is_some_and(|o| o.form.is_some()) {
        return cancel(app, ui, st);
    }
    if crate::book::back(ui, st) {
        return;
    }
    crate::audio::close(st);
    crate::doc::close(st);
    crate::book::close(st);
    crate::sheet::close(st);
    let from = st.fa7.take().map_or(View::Folder, |o| o.from);
    return_to(app, ui, st, from);
}

/// The shell's answer to "Save changes?": Save, Discard, Cancel.
pub(crate) fn answer(app: &App, ui: &FilesApp, st: &mut State, i: usize) {
    match i {
        0 => save(app, ui, st),
        1 => close_form(ui, st),
        _ => {}
    }
}

/// The app is in front again (NEOCONT handed the user back): the cards it has now are marked.
pub(crate) fn visible(ui: &FilesApp, st: &mut State) {
    if !matches!(st.fa7.as_ref().map(|o| &o.kind), Some(Kind::Vcf { .. })) {
        return;
    }
    let book = book(st);
    if let Some(Open7 {
        kind: Kind::Vcf { v, checked, known },
        ..
    }) = st.fa7.as_mut()
    {
        *known = known_cards(v, book.as_ref());
        for (c, k) in checked.iter_mut().zip(known.iter()) {
            *c &= !*k;
        }
    }
    show_vcf(ui, st);
}

pub(crate) fn act(app: &App, ui: &FilesApp, st: &mut State, a: Act) {
    if st.fa7.is_none() {
        return;
    }
    match a {
        Act::Menu(m) => menu(app, ui, st, &m),
        Act::VcfToggle(i) => {
            if let Some(Open7 {
                kind: Kind::Vcf { checked, known, .. },
                ..
            }) = st.fa7.as_mut()
                && let Some(c) = checked.get_mut(i)
                && !known[i]
            {
                *c = !*c;
            }
            show_vcf(ui, st);
        }
        Act::VcfOpen(i) => edit_card(ui, st, i),
        Act::VcfAdd => add(app, ui, st),
        Act::Cancel => cancel(app, ui, st),
        Act::Save => save(app, ui, st),
        Act::LabelTap(email, i) => label_tap(ui, st, email, i),
        Act::LabelChosen(k) => label_chosen(ui, st, k),
        Act::FieldRemove(email, i) => {
            if let Some(Form::Card { phones, emails, .. }) = form(st) {
                let m = if email { emails } else { phones };
                if i < m.row_count() {
                    m.remove(i);
                }
            }
        }
        Act::FieldAdd(email) => {
            if let Some(Form::Card { phones, emails, .. }) = form(st) {
                let (m, l) = if email {
                    (emails, Label::Home)
                } else {
                    (phones, Label::Mobile)
                };
                m.push(PimField {
                    label: l.name().into(),
                    value: "".into(),
                });
            }
        }
        Act::IcsOpen(i) => edit_event(ui, st, i),
        Act::Pick(w) => pick(ui, st, &w),
        Act::AllDay(on) => all_day(ui, st, on),
        Act::DateSet(y, m, d) => date_set(ui, st, y, m, d),
        Act::TimeSet(h, m) => time_set(ui, st, h, m),
        Act::Chosen(k) => chosen(ui, st, k),
        Act::FontTab(t) => font_tab(ui, st, t),
        Act::HexScrolled(r) => hex_rows(ui, st, r),
        Act::HexAsText => {
            if let Some(o) = st.fa7.take() {
                st.view = o.from;
                open_text(app, ui, st, &o.path, false);
            }
        }
    }
}

/// The header's Edit: the first card or event.
pub(crate) fn edit(ui: &FilesApp, st: &mut State) {
    match st.fa7.as_ref().map(|o| &o.kind) {
        Some(Kind::Vcf { .. }) => edit_card(ui, st, 0),
        Some(Kind::Ics(_)) => edit_event(ui, st, 0),
        _ => {}
    }
}

fn form(st: &mut State) -> Option<&mut Form> {
    st.fa7.as_mut().and_then(|o| o.form.as_mut())
}

fn menu(app: &App, ui: &FilesApp, st: &mut State, m: &str) {
    let Some(path) = st.fa7.as_ref().map(|o| o.path.clone()) else {
        return;
    };
    if crate::book::menu(ui, st, m) {
        return;
    }
    match m {
        "Show as hex…" => hex(app, ui, st, ""),
        "Open with…" => crate::fa8::open_with(ui, st, path),
        "Details" => crate::fa3::details(app, ui, st, &path),
        _ => {}
    }
}

fn show(app: &App, ui: &FilesApp, st: &mut State) {
    let Some(o) = st.fa7.as_ref() else { return };
    ui.set_name(o.name.as_str().into());
    let hexing = matches!(o.kind, Kind::Hex);
    ui.set_fa7_menu(strs(if hexing {
        &["Open with…", "Details"]
    } else if matches!(o.kind, Kind::Book) {
        &[
            "Contents",
            "Text size",
            "Theme",
            "Show as hex…",
            "Open with…",
            "Details",
        ]
    } else {
        &["Show as hex…", "Open with…", "Details"]
    }));
    let ro = st.drives.iter().any(|&(d, ro)| ro && o.path.starts_with(d))
        || crate::archive::in_cache(&o.path)
        || o.lossy;
    ui.set_read_only(ro);
    match &o.kind {
        Kind::Vcf { .. } => show_vcf(ui, st),
        Kind::Ics(ics) => {
            let n = ics.len();
            ui.set_ics_events(model(
                (0..n).map(|i| pim::event_card(ics, i, &o.name)).collect(),
            ));
            ui.set_ics_footer(
                format!(
                    "{} · {}",
                    pim::count(n, "event", "events"),
                    files_core::size_text(o.size)
                )
                .into(),
            );
        }
        _ => {}
    }
    let _ = app;
}

// ── cards ────────────────────────────────────────────────────────────────────────────────────────────────────────

fn book(st: &State) -> Option<contacts::Book> {
    let size = st.fs.entry(BOOK).ok()?.size;
    let b = read(st, BOOK, size, SMALL_MAX).ok()?;
    contacts::Book::decode(&b)
}

/// Which cards Contacts has (read the way NEOCONT imports them, judged by the rule it skips them by).
fn known_cards(v: &Vcf, book: Option<&contacts::Book>) -> Vec<bool> {
    (0..v.len())
        .map(|i| {
            book.is_some_and(|b| {
                contacts::vcard::import(&v.card_text(i))
                    .contacts
                    .first()
                    .is_some_and(|c| b.has_same(c))
            })
        })
        .collect()
}

fn cards(st: &State, o: &mut Open7, ext: &str) -> Result<&'static str, String> {
    let (text, lossy) = text_of(&read(st, &o.path, o.size, CARDS_MAX)?);
    o.lossy = lossy;
    if ext == "ics" {
        let ics = Ics::parse(&text).map_err(|e| e.to_string())?;
        o.kind = Kind::Ics(ics);
        return Ok("ics");
    }
    let v = Vcf::parse(&text).map_err(|e| e.to_string())?;
    let known = known_cards(&v, book(st).as_ref());
    o.kind = Kind::Vcf {
        checked: known.iter().map(|k| !k).collect(),
        known,
        v,
    };
    Ok("vcf")
}

fn show_vcf(ui: &FilesApp, st: &State) {
    let Some(Open7 {
        kind: Kind::Vcf { v, checked, known },
        size,
        path,
        ..
    }) = st.fa7.as_ref()
    else {
        return;
    };
    let n = v.len();
    ui.set_vcf_title(pim::count(n, "contact", "contacts").into());
    let dir = neo_fs::path::parent(path).unwrap_or_default();
    ui.set_vcf_sub(
        format!(
            "vCard {} · {} · {}",
            v.version(0).name(),
            files_core::size_text(*size),
            dir.trim_end_matches('\\')
        )
        .into(),
    );
    ui.set_vcf_cards(model(
        (0..n)
            .map(|i| pim::card(v, i, checked[i], known[i]))
            .collect(),
    ));
    ui.set_vcf_note(pim::vcf_note(v, known).into());
    let picked = checked.iter().filter(|c| **c).count();
    let addable = known.iter().filter(|k| !**k).count();
    ui.set_vcf_footer(pim::vcf_footer(picked, addable, n).into());
    ui.set_vcf_can_add(picked > 0);
}

/// Add to Contacts: the ticked cards, as they are in the file, go to NEOCONT's IMPORT page.
fn add(app: &App, ui: &FilesApp, st: &mut State) {
    let Some(Open7 {
        kind: Kind::Vcf { v, checked, .. },
        ..
    }) = st.fa7.as_ref()
    else {
        return;
    };
    let text: String = (0..v.len())
        .filter(|&i| checked[i])
        .map(|i| v.card_text(i))
        .collect();
    if text.is_empty() {
        return;
    }
    if let Err(e) = store::write(&*st.fs, ADD, text.as_bytes()) {
        return toast(app, "", e.code());
    }
    match app.open(CONTACTS, CONTACTS_IMPORT, ADD) {
        Ok(()) => dprintln!("files: add to contacts {}", ADD),
        Err(e) => note_with(app, &format!("Contacts didn't open ({e})"), ""),
    }
    let _ = ui;
}

fn edit_card(ui: &FilesApp, st: &mut State, i: usize) {
    let Some(Open7 {
        kind: Kind::Vcf { v, .. },
        name,
        ..
    }) = st.fa7.as_ref()
    else {
        return;
    };
    if ui.get_read_only() || i >= v.len() {
        return;
    }
    let f = v.fields(i);
    let title = v.title(i);
    let list = |l: &[(Label, String)]| {
        Rc::new(VecModel::from(
            l.iter()
                .map(|(l, s)| PimField {
                    label: l.name().into(),
                    value: s.as_str().into(),
                })
                .collect::<Vec<_>>(),
        ))
    };
    let (phones, emails) = (list(&f.phones), list(&f.emails));
    ui.set_form_line(format!("{name} · contact {} of {}", i + 1, v.len()).into());
    ui.set_form_initials(pim::initials(&title).into());
    ui.set_form_tint(pim::tint(&title));
    ui.set_c_first(f.first.as_str().into());
    ui.set_c_last(f.last.as_str().into());
    ui.set_c_company(f.company.as_str().into());
    ui.set_c_phones(ModelRc::from(phones.clone()));
    ui.set_c_emails(ModelRc::from(emails.clone()));
    ui.set_pim_form("vcf".into());
    // what the form shows, read back the way Save reads it (trimmed, empty rows dropped): Back asks only after edits
    let start = card_fields(ui, &phones, &emails);
    if let Some(o) = st.fa7.as_mut() {
        o.form = Some(Form::Card {
            i,
            start,
            phones,
            emails,
            label: (false, 0),
        });
    }
}

fn card_fields(
    ui: &FilesApp,
    phones: &Rc<VecModel<PimField>>,
    emails: &Rc<VecModel<PimField>>,
) -> Fields {
    Fields {
        first: ui.get_c_first().trim().to_string(),
        last: ui.get_c_last().trim().to_string(),
        company: ui.get_c_company().trim().to_string(),
        phones: pim::rows_to_list(&ModelRc::from(phones.clone())),
        emails: pim::rows_to_list(&ModelRc::from(emails.clone())),
    }
}

fn label_tap(ui: &FilesApp, st: &mut State, email: bool, i: usize) {
    let Some(Form::Card {
        phones,
        emails,
        label,
        ..
    }) = form(st)
    else {
        return;
    };
    let m = if email { &*emails } else { &*phones };
    let Some(row) = m.row_data(i) else { return };
    *label = (email, i);
    let choices: &[Label] = if email { &Label::EMAIL } else { &Label::PHONE };
    ui.set_sheet_choices(strs(&choices.iter().map(|l| l.name()).collect::<Vec<_>>()));
    ui.set_sheet_current(
        choices
            .iter()
            .position(|l| l.name() == row.label.as_str())
            .unwrap_or(0) as i32,
    );
    ui.set_sheet_open(true);
}

fn label_chosen(ui: &FilesApp, st: &mut State, k: usize) {
    ui.set_sheet_open(false);
    let Some(Form::Card {
        phones,
        emails,
        label: (email, i),
        ..
    }) = form(st)
    else {
        return;
    };
    let choices: &[Label] = if *email { &Label::EMAIL } else { &Label::PHONE };
    let m = if *email { &*emails } else { &*phones };
    if let (Some(mut row), Some(l)) = (m.row_data(*i), choices.get(k)) {
        row.label = l.name().into();
        m.set_row_data(*i, row);
    }
}

// ── events ───────────────────────────────────────────────────────────────────────────────────────────────────────

fn today() -> Date {
    Date::from_days(e32::time::utc_now().0.div_euclid(86_400))
}

fn edit_event(ui: &FilesApp, st: &mut State, i: usize) {
    let Some(Open7 {
        kind: Kind::Ics(ics),
        name,
        ..
    }) = st.fa7.as_ref()
    else {
        return;
    };
    if ui.get_read_only() || i >= ics.len() {
        return;
    }
    let mut start = ics.event(i);
    // as changed() reads the fields back
    start.summary = start.summary.trim().to_string();
    start.location = start.location.trim().to_string();
    ui.set_form_line(format!("{name} · {}", pim::count(ics.len(), "event", "events")).into());
    ui.set_e_title(start.summary.as_str().into());
    ui.set_e_place(start.location.as_str().into());
    let ev = start.clone();
    if let Some(o) = st.fa7.as_mut() {
        o.form = Some(Form::Event {
            i,
            start,
            ev,
            picking: Picking::StartDate,
        });
    }
    show_event(ui, st);
    ui.set_pim_form("ics".into());
}

fn show_event(ui: &FilesApp, st: &State) {
    let Some(Open7 {
        kind: Kind::Ics(ics),
        form: Some(Form::Event { ev, .. }),
        ..
    }) = st.fa7.as_ref()
    else {
        return;
    };
    let day = |w: &Option<When>| {
        w.as_ref()
            .map_or(String::from("No date"), |w| w.date.short())
    };
    let time = |w: &Option<When>| {
        w.as_ref()
            .and_then(|w| w.time)
            .map_or(String::from("—"), |(h, m, _)| {
                neo_format_pim::date::hm(h, m)
            })
    };
    // an all-day end is exclusive: the form shows the last day
    let end_day = ev.end.as_ref().map(|e| match e.time {
        None => When {
            date: e.date.add_days(-1),
            ..e.clone()
        },
        Some(_) => e.clone(),
    });
    ui.set_e_start_date(day(&ev.start).into());
    ui.set_e_start_time(time(&ev.start).into());
    ui.set_e_end_date(day(&end_day).into());
    ui.set_e_end_time(time(&ev.end).into());
    ui.set_e_all_day(ev.start.as_ref().is_some_and(|s| s.time.is_none()));
    ui.set_e_reminder(pim::reminder_text(ev.reminder).into());
    ui.set_e_repeat(ev.repeat.text().into());
    ui.set_e_zone(
        ev.start
            .as_ref()
            .map(|s| pim::zone_text(ics, s))
            .filter(|z| !z.is_empty())
            .unwrap_or_else(|| String::from("Phone's time"))
            .into(),
    );
}

fn ev_mut(st: &mut State) -> Option<(&mut Event, &mut Picking)> {
    match form(st)? {
        Form::Event { ev, picking, .. } => Some((ev, picking)),
        Form::Card { .. } => None,
    }
}

fn pick(ui: &FilesApp, st: &mut State, w: &str) {
    let today = today();
    let Some((ev, picking)) = ev_mut(st) else {
        return;
    };
    let start = ev.start.clone().unwrap_or(When {
        date: today,
        time: Some((9, 0, 0)),
        utc: false,
        tzid: None,
    });
    let end = ev.end.clone().unwrap_or_else(|| start.plus(3600));
    match w {
        "start-date" | "end-date" => {
            let d = if w == "start-date" {
                start.date
            } else if end.time.is_none() {
                end.date.add_days(-1)
            } else {
                end.date
            };
            *picking = if w == "start-date" {
                Picking::StartDate
            } else {
                Picking::EndDate
            };
            ui.set_date_y(d.year);
            ui.set_date_m(i32::from(d.month));
            ui.set_date_d(i32::from(d.day));
            ui.set_date_open(true);
        }
        "start-time" | "end-time" => {
            let (h, m, _) = if w == "start-time" {
                start.time
            } else {
                end.time
            }
            .unwrap_or((9, 0, 0));
            *picking = if w == "start-time" {
                Picking::StartTime
            } else {
                Picking::EndTime
            };
            ui.set_time_h(i32::from(h));
            ui.set_time_m(i32::from(m));
            ui.set_time_open(true);
        }
        "reminder" => {
            *picking = Picking::Reminder;
            let cur = pim::REMINDERS
                .iter()
                .position(|r| *r == ev.reminder)
                .unwrap_or(0);
            ui.set_choice_title("Reminder".into());
            ui.set_choices(strs(
                &pim::REMINDERS
                    .iter()
                    .map(|r| pim::reminder_text(*r))
                    .collect::<Vec<_>>()
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            ));
            ui.set_choice(cur as i32);
            ui.set_choice_open(true);
        }
        "repeat" => {
            *picking = Picking::Repeat;
            let cur = Repeat::CHOICES.iter().position(|r| *r == ev.repeat);
            let mut names: Vec<&str> = Repeat::CHOICES.iter().map(Repeat::text).collect();
            if cur.is_none() {
                names.push("Custom (keep)");
            }
            ui.set_choice_title("Repeat".into());
            ui.set_choices(strs(&names));
            ui.set_choice(cur.unwrap_or(names.len() - 1) as i32);
            ui.set_choice_open(true);
        }
        _ => {}
    }
}

fn date_set(ui: &FilesApp, st: &mut State, y: i32, m: i32, d: i32) {
    ui.set_date_open(false);
    let today = today();
    let Some((ev, picking)) = ev_mut(st) else {
        return;
    };
    let date = Date {
        year: y,
        month: m.clamp(1, 12) as u8,
        day: d.clamp(1, 31) as u8,
    };
    let mut start = ev.start.clone().unwrap_or(When {
        date: today,
        time: Some((9, 0, 0)),
        utc: false,
        tzid: None,
    });
    match picking {
        Picking::StartDate => {
            // the event keeps its length
            let len = ev.end.as_ref().map(|e| e.seconds() - start.seconds());
            start.date = date;
            ev.end = len.map(|l| start.plus(l));
            ev.start = Some(start);
        }
        Picking::EndDate => {
            let mut end = ev.end.clone().unwrap_or_else(|| start.plus(3600));
            end.date = if end.time.is_none() {
                date.add_days(1)
            } else {
                date
            };
            if end.seconds() < start.seconds() {
                end = start.clone();
            }
            ev.end = Some(end);
        }
        _ => {}
    }
    show_event(ui, st);
}

fn time_set(ui: &FilesApp, st: &mut State, h: i32, m: i32) {
    ui.set_time_open(false);
    let today = today();
    let Some((ev, picking)) = ev_mut(st) else {
        return;
    };
    let t = Some((h.clamp(0, 23) as u8, m.clamp(0, 59) as u8, 0));
    match picking {
        Picking::StartTime => {
            let mut s = ev.start.clone().unwrap_or(When {
                date: today,
                time: t,
                utc: false,
                tzid: None,
            });
            let len = ev.end.as_ref().map(|e| e.seconds() - s.seconds());
            s.time = t;
            ev.end = len.map(|l| s.plus(l));
            ev.start = Some(s);
        }
        Picking::EndTime => {
            let s = ev.start.clone().unwrap_or(When {
                date: today,
                time: Some((9, 0, 0)),
                utc: false,
                tzid: None,
            });
            let mut e = ev.end.clone().unwrap_or_else(|| s.clone());
            e.time = t;
            if e.seconds() < s.seconds() {
                e = s.clone();
            }
            ev.start = Some(s);
            ev.end = Some(e);
        }
        _ => {}
    }
    show_event(ui, st);
}

fn all_day(ui: &FilesApp, st: &mut State, on: bool) {
    let today = today();
    let Some((ev, _)) = ev_mut(st) else { return };
    let mut s = ev.start.clone().unwrap_or(When {
        date: today,
        time: None,
        utc: false,
        tzid: None,
    });
    if on {
        let last = ev.end.as_ref().map_or(s.date, |e| e.date);
        s.time = None;
        ev.end = Some(When {
            date: last.max(s.date).add_days(1),
            time: None,
            utc: false,
            tzid: None,
        });
    } else {
        let last = ev
            .end
            .as_ref()
            .map_or(s.date, |e| e.date.add_days(-1))
            .max(s.date);
        s.time = Some((9, 0, 0));
        ev.end = Some(When {
            date: last,
            time: Some((10, 0, 0)),
            ..s.clone()
        });
    }
    ev.start = Some(s);
    show_event(ui, st);
}

fn chosen(ui: &FilesApp, st: &mut State, k: usize) {
    ui.set_choice_open(false);
    let Some((ev, picking)) = ev_mut(st) else {
        return;
    };
    match picking {
        Picking::Reminder => {
            if let Some(r) = pim::REMINDERS.get(k) {
                ev.reminder = *r;
            }
        }
        Picking::Repeat => {
            // past the list: "Custom (keep)"
            if let Some(r) = Repeat::CHOICES.get(k) {
                ev.repeat = r.clone();
            }
        }
        _ => {}
    }
    show_event(ui, st);
}

// ── forms: save and cancel ───────────────────────────────────────────────────────────────────────────────────────

/// The form's state now, and whether it changed.
fn changed(ui: &FilesApp, st: &mut State) -> bool {
    match form(st) {
        Some(Form::Card {
            start,
            phones,
            emails,
            ..
        }) => card_fields(ui, phones, emails) != *start,
        Some(Form::Event { start, ev, .. }) => {
            ev.summary = ui.get_e_title().trim().to_string();
            ev.location = ui.get_e_place().trim().to_string();
            ev != start
        }
        None => false,
    }
}

fn cancel(app: &App, ui: &FilesApp, st: &mut State) {
    if !changed(ui, st) {
        return close_form(ui, st);
    }
    let q = Query {
        text: cut(
            "Save your changes? Discard drops them; the file stays as it was.",
            neoui_proto::page::TEXT_MAX,
        ),
        buttons: alloc::vec!["Save".into(), "Discard".into(), "Cancel".into()],
        destructive: false,
    };
    if app.query(&q).is_ok() {
        st.asking = Some(Ask::Fa7Unsaved);
    }
}

fn close_form(ui: &FilesApp, st: &mut State) {
    ui.global::<files_ui::KeyboardFocus>().invoke_close();
    ui.set_pim_form("".into());
    if let Some(o) = st.fa7.as_mut() {
        o.form = None;
    }
}

fn save(app: &App, ui: &FilesApp, st: &mut State) {
    changed(ui, st);
    let Some(o) = st.fa7.as_mut() else { return };
    // edited on a copy: a failed write leaves the model as the file is (Add to Contacts reads it)
    let (text, kind) = match (&o.kind, &o.form) {
        (
            Kind::Vcf { v, checked, known },
            Some(Form::Card {
                i, phones, emails, ..
            }),
        ) => {
            let mut v = v.clone();
            v.set(*i, &card_fields(ui, phones, emails));
            let text = v.write();
            let kind = Kind::Vcf {
                checked: checked.clone(),
                known: known.clone(),
                v,
            };
            (text, kind)
        }
        (Kind::Ics(ics), Some(Form::Event { i, ev, .. })) => {
            let mut ics = ics.clone();
            ics.set(*i, ev);
            (ics.write(), Kind::Ics(ics))
        }
        _ => return,
    };
    let path = o.path.clone();
    let name = o.name.clone();
    match store::write(&*st.fs, &path, text.as_bytes()) {
        Ok(()) => {
            dprintln!("files: saved {} {}", path, text.len());
            if let Some(o) = st.fa7.as_mut() {
                o.size = text.len() as u64;
                o.kind = kind;
                if let Kind::Vcf { v, checked, known } = &mut o.kind {
                    checked.resize(v.len(), true);
                    known.resize(v.len(), false);
                }
            }
            close_form(ui, st);
            show(app, ui, st);
            note_with(app, &format!("Saved {name}"), "");
        }
        Err(e) => toast(app, "", e.code()),
    }
}

// ── inspectors ───────────────────────────────────────────────────────────────────────────────────────────────────

fn scale(ui: &FilesApp) -> f32 {
    let dp = ui.global::<files_ui::Theme>().get_dp();
    if dp > 0.0 { dp } else { 1.0 }
}

fn font(ui: &FilesApp, st: &State, o: &mut Open7) -> Result<&'static str, String> {
    let data = read(st, &o.path, o.size, FONT_MAX)?;
    let mut f = Font::new(&data).map_err(|e| e.to_string())?;
    let fs = inspect::font_screen(&mut f, o.size, scale(ui), 316.0);
    ui.set_font_aa(fs.aa);
    ui.set_font_family_art(fs.family_art);
    ui.set_font_family(fs.family.as_str().into());
    ui.set_font_line1(fs.line1.as_str().into());
    ui.set_font_line2(fs.line2.as_str().into());
    ui.set_font_groups(model(fs.groups));
    ui.set_font_glyphs(model(Vec::new()));
    ui.set_font_glyphs_note("".into());
    ui.set_font_tab(0);
    ui.set_fa7_footer("Not installed. Installing fonts comes later.".into());
    drop(f);
    o.kind = Kind::Font {
        data,
        glyphs: false,
    };
    Ok("font")
}

fn font_tab(ui: &FilesApp, st: &mut State, t: usize) {
    ui.set_font_tab(t as i32);
    let sc = scale(ui);
    if let Some(Open7 {
        kind: Kind::Font { data, glyphs },
        ..
    }) = st.fa7.as_mut()
        && t == 1
        && !*glyphs
        && let Ok(mut f) = Font::new(data)
    {
        let start = e32::time::millis();
        let (rows, note) = inspect::glyph_rows(&mut f, sc);
        dprintln!("files: glyphs drawn in {} ms", e32::time::millis() - start);
        ui.set_font_glyphs(model(rows));
        ui.set_font_glyphs_note(note.into());
        *glyphs = true;
    }
}

fn certificate(ui: &FilesApp, st: &State, o: &mut Open7) -> Result<&'static str, String> {
    let data = read(st, &o.path, o.size, SMALL_MAX)?;
    let certs = cert::parse(&data).map_err(|e| e.to_string())?;
    let c = inspect::cert_screen(&certs[0], 0, certs.len(), e32::time::utc_now().0);
    ui.set_cert_title(c.title.as_str().into());
    ui.set_cert_status(c.status.as_str().into());
    ui.set_cert_ok(c.ok);
    ui.set_info_groups(model(c.groups));
    ui.set_cert_fp1(c.fp[0].as_str().into());
    ui.set_cert_fp2(c.fp[1].as_str().into());
    ui.set_fa7_footer("Trusted certificates come with the browser".into());
    o.kind = Kind::Fixed;
    Ok("cert")
}

fn package(ui: &FilesApp, st: &State, o: &mut Open7, ext: &str) -> Result<&'static str, String> {
    let a = match ext {
        "sis" | "sisx" => {
            // only the header and the controller, never the payload
            let f = st
                .fs
                .open(&o.path, Open::Read)
                .map_err(|e| files_core::error_text(e.code()))?;
            let mut head = [0u8; 64];
            let n = f
                .read_at(0, &mut head)
                .map_err(|e| files_core::error_text(e.code()))?;
            let need = sis::needed(&head[..n]).map_err(|e| e.to_string())?;
            if need as u64 > SMALL_MAX {
                return Err(String::from("The package's header is too big"));
            }
            let mut b = alloc::vec![0u8; need];
            let mut at = 0;
            while at < need {
                let k = f
                    .read_at(at as u64, &mut b[at..])
                    .map_err(|e| files_core::error_text(e.code()))?;
                if k == 0 {
                    return Err(String::from("The file is cut short"));
                }
                at += k;
            }
            let p = sis::read(&b).map_err(|e| e.to_string())?;
            inspect::sis_screen(
                &p,
                o.size,
                &ext.to_ascii_uppercase(),
                e32::time::utc_now().0,
            )
        }
        "jad" => {
            let m = jad::parse(&text_of(&read(st, &o.path, o.size, SMALL_MAX)?).0)
                .map_err(|e| e.to_string())?;
            inspect::midlet_screen(&m, o.size, "JAD")
        }
        _ => {
            let m =
                jad::from_jar(&read(st, &o.path, o.size, CARDS_MAX)?).map_err(|e| e.to_string())?;
            inspect::midlet_screen(&m, o.size, "JAR")
        }
    };
    ui.set_app_title(a.title.as_str().into());
    ui.set_app_vendor(a.vendor.as_str().into());
    ui.set_app_meta(a.meta.as_str().into());
    ui.set_app_info_title(a.info_title.as_str().into());
    ui.set_app_info_text(a.info_text.as_str().into());
    ui.set_info_groups(model(a.groups));
    ui.set_app_caps_title(a.caps_title.as_str().into());
    ui.set_app_caps(model(a.caps));
    ui.set_app_note(a.note.as_str().into());
    o.kind = Kind::Fixed;
    Ok("app")
}

/// The bytes (files2-view-unknown); `why` is the reason a viewer couldn't read the file ("" none: no viewer, or
/// Show as hex…).
fn hex(app: &App, ui: &FilesApp, st: &mut State, why: &str) {
    crate::audio::close(st);
    let Some(o) = st.fa7.as_mut() else { return };
    let had_viewer = !matches!(o.kind, Kind::Hex);
    o.kind = Kind::Hex;
    let ext = neo_view::ext_of(&o.name).to_ascii_lowercase();
    let family = neo_view::family_of(&o.name);
    let modified = st.fs.entry(&o.path).map(|e| e.modified).unwrap_or(0);
    let dir = neo_fs::path::parent(&o.path).unwrap_or_default();
    ui.set_hex_badge(neo_view::badge(&o.name).as_str().into());
    ui.set_hex_colour(files_ui::colour(family.colour()));
    ui.set_hex_meta(
        format!(
            "{} · {} · {}",
            files_core::size_text(o.size),
            files_core::date_text(modified),
            dir.trim_end_matches('\\')
        )
        .into(),
    );
    let (heading, text) = if !why.is_empty() {
        (
            format!(
                "Can't read this {}",
                if ext.is_empty() { "file" } else { &ext }
            ),
            why.to_string(),
        )
    } else if had_viewer {
        (
            String::from("Its bytes"),
            String::from("What the file holds, byte by byte."),
        )
    } else {
        (
            if ext.is_empty() {
                String::from("No viewer for this file")
            } else {
                format!("No viewer for .{ext}")
            },
            String::from(
                "Files can't show this type. Open it in another app, or look at its bytes.",
            ),
        )
    };
    ui.set_hex_heading(heading.into());
    ui.set_hex_text(text.into());
    let shown = o.size.min(HEX_MAX);
    ui.set_hex_info(
        if shown < o.size {
            format!(
                "First {} of {} bytes",
                files_core::size_text(shown),
                files_ui::thousands(o.size)
            )
        } else {
            format!("{} bytes", files_ui::thousands(o.size))
        }
        .into(),
    );
    ui.set_hex_total(shown.div_ceil(neo_format_inspect::hex::ROW as u64) as i32);
    ui.set_view("unknown".into());
    show(app, ui, st);
    hex_rows(ui, st, 0);
}

/// The rows on screen from `first` (read when the pane scrolls).
fn hex_rows(ui: &FilesApp, st: &mut State, first: usize) {
    let Some(o) = st.fa7.as_ref().filter(|o| matches!(o.kind, Kind::Hex)) else {
        return;
    };
    let row = neo_format_inspect::hex::ROW;
    let at = (first * row) as u64;
    let shown = o.size.min(HEX_MAX);
    if at >= shown && shown > 0 {
        return;
    }
    let want = ((ui.get_text_visible().max(40) as usize + 2) * row).min((shown - at) as usize);
    let mut b = alloc::vec![0u8; want];
    let n = match st
        .fs
        .open(&o.path, Open::Read)
        .and_then(|f| f.read_at(at, &mut b))
    {
        Ok(n) => n,
        Err(e) => {
            dprintln!("files: hex read {} ({})", o.path, e.code());
            0
        }
    };
    b.truncate(n);
    ui.set_hex_rows(model(inspect::hex_rows(first, &b)));
}

/// Test images only (`--features test-files`): FA7's files in `C:\Data\fa7\`, each with a damaged copy.
#[cfg(feature = "test-files")]
pub(crate) fn test_files(st: &State) {
    const DIR: &str = "C:\\Data\\fa7\\";
    if st.fs.entry(&format!("{DIR}contacts.vcf")).is_ok() {
        return;
    }
    let files: [(&str, &[u8]); 9] = [
        (
            "contacts.vcf",
            include_bytes!("../../../crates/neo-format-pim/tests/data/contacts.vcf"),
        ),
        (
            "nokia21.vcf",
            include_bytes!("../../../crates/neo-format-pim/tests/data/nokia21.vcf"),
        ),
        (
            "concert.ics",
            include_bytes!("../../../crates/neo-format-pim/tests/data/concert.ics"),
        ),
        (
            "meeting.ics",
            include_bytes!("../../../crates/neo-format-pim/tests/data/meeting.ics"),
        ),
        (
            "DejaVuSans.ttf",
            include_bytes!("../../../crates/neo-format-inspect/tests/data/DejaVuSans-subset.ttf"),
        ),
        (
            "root-ca.pem",
            include_bytes!("../../../crates/neo-format-inspect/tests/data/root-ca.pem"),
        ),
        (
            "bundle.pem",
            include_bytes!("../../../crates/neo-format-inspect/tests/data/bundle.pem"),
        ),
        (
            "XPlore.sisx",
            include_bytes!("../../../crates/neo-format-inspect/tests/data/sample.sisx"),
        ),
        (
            "midlet.jar",
            include_bytes!("../../../crates/neo-format-inspect/tests/data/midlet.jar"),
        ),
    ];
    for (name, bytes) in files {
        let _ = store::write(&*st.fs, &format!("{DIR}{name}"), bytes);
        // the damaged copy: cut at two thirds, a few bytes flipped
        let mut bad = bytes[..bytes.len() * 2 / 3].to_vec();
        for i in (7..bad.len()).step_by(97) {
            bad[i] ^= 0x5a;
        }
        let _ = store::write(&*st.fs, &format!("{DIR}broken-{name}"), &bad);
    }
    let elf: alloc::vec::Vec<u8> = (0..2u32 << 20)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    let _ = store::write(&*st.fs, &format!("{DIR}firmware.bin"), &elf);
}
