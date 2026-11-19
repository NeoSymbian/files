// SPDX-License-Identifier: GPL-3.0-only
//! The vCard and iCal views and forms (FA7) from `neo-format-pim`'s files: the contact cards, the event cards, the
//! forms' rows and back. The app and the snapshot tests make them with the same functions.

use crate::{EvCard, EvRow, PimCard, PimField, PimLine, colour};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use neo_format_pim::date;
use neo_format_pim::ical::{Event, Ics, Kind, When};
use neo_format_pim::vcard::{Fields, Label, Vcf};
use slint::{ModelRc, SharedString, VecModel};

/// The avatar colours (the contact family's pink first, as on the boards).
const TINTS: [u32; 6] = [0xe0457b, 0x2f80ed, 0x14a37f, 0xf59e0b, 0x6b5cf0, 0xc9a227];

/// "Անի Պետրոսյան" → "ԱՊ"; one word gives one letter.
pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .take(2)
        .filter_map(|w| w.chars().next())
        .flat_map(char::to_uppercase)
        .collect()
}

/// A name's avatar colour (stable: the same name, the same colour).
pub fn tint(name: &str) -> slint::Color {
    let h = name
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    colour(TINTS[h as usize % TINTS.len()])
}

/// Card `i` of `v` as the viewer draws it.
pub fn card(v: &Vcf, i: usize, checked: bool, known: bool) -> PimCard {
    let f = v.fields(i);
    let name = v.title(i);
    let mut lines: Vec<PimLine> = f
        .phones
        .iter()
        .map(|(l, n)| PimLine {
            mail: false,
            text: n.as_str().into(),
            label: l.name().into(),
        })
        .collect();
    lines.extend(f.emails.iter().map(|(_, e)| PimLine {
        mail: true,
        text: e.as_str().into(),
        label: SharedString::new(),
    }));
    PimCard {
        initials: initials(&name).into(),
        tint: tint(&name),
        name: name.into(),
        lines: ModelRc::new(VecModel::from(lines)),
        checked,
        known,
    }
}

/// "3 contacts", "1 contact"
pub fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The viewer's footer: "2 of 3 selected", or why nothing can be added.
pub fn vcf_footer(selected: usize, addable: usize, total: usize) -> String {
    match (addable, selected) {
        (0, _) => String::from("All of them are in Contacts"),
        (_, 0) => String::from("Tick the contacts to add"),
        _ => format!("{selected} of {total} selected"),
    }
}

/// The note under the cards for those Contacts has: "Sam Carter is already in Contacts with the same number, so he is
/// left out." (neutral wording: "they").
pub fn vcf_note(v: &Vcf, known: &[bool]) -> String {
    let names: Vec<String> = known
        .iter()
        .enumerate()
        .filter(|(_, k)| **k)
        .map(|(i, _)| v.title(i))
        .collect();
    match names.len() {
        0 => String::new(),
        1 => format!(
            "{} is already in Contacts with the same number, so it is left out.",
            names[0]
        ),
        n => format!(
            "{n} contacts are already in Contacts with the same numbers, so they are left out."
        ),
    }
}

fn fields_model(list: &[(Label, String)]) -> ModelRc<PimField> {
    ModelRc::new(VecModel::from(
        list.iter()
            .map(|(l, v)| PimField {
                label: l.name().into(),
                value: v.as_str().into(),
            })
            .collect::<Vec<_>>(),
    ))
}

/// The contact form's lists.
pub fn phones_emails(f: &Fields) -> (ModelRc<PimField>, ModelRc<PimField>) {
    (fields_model(&f.phones), fields_model(&f.emails))
}

/// A label's name back to the label.
pub fn label_of(name: &str) -> Label {
    Label::PHONE
        .into_iter()
        .find(|l| l.name() == name)
        .unwrap_or(Label::Other)
}

/// The form's rows back to fields (empty values dropped).
pub fn rows_to_list(rows: &ModelRc<PimField>) -> Vec<(Label, String)> {
    use slint::Model as _;
    rows.iter()
        .map(|r| (label_of(&r.label), r.value.trim().to_string()))
        .filter(|(_, v)| !v.is_empty())
        .collect()
}

/// "19:00 – 21:30 · Yerevan time (GMT+4)", "All day", "06:00 UTC"
pub fn time_line(ics: &Ics, e: &Event) -> String {
    let Some(s) = &e.start else {
        return String::new();
    };
    let Some((h, m, _)) = s.time else {
        return String::from("All day");
    };
    let mut t = date::hm(h, m);
    if let Some(end) = &e.end
        && let Some((eh, em, _)) = end.time
    {
        t = format!("{t} – {}", date::hm(eh, em));
    }
    let zone = zone_text(ics, s);
    if zone.is_empty() {
        t
    } else {
        format!("{t} · {zone}")
    }
}

/// "Yerevan time (GMT+4)", "UTC", "" (floating)
pub fn zone_text(ics: &Ics, w: &When) -> String {
    if w.utc {
        return String::from("UTC");
    }
    let Some(z) = &w.tzid else {
        return String::new();
    };
    match ics.tz_offset(z) {
        Some(o) => format!("{} time ({})", date::city(z), date::gmt(o)),
        None => format!("{} time", date::city(z)),
    }
}

/// The first line of the time row: the day, or the days ("Sat 21 – Sun 22 Nov 2026").
pub fn day_line(e: &Event) -> String {
    let Some(s) = &e.start else {
        return String::from("No date");
    };
    // an all-day DTEND is the day after (RFC 5545 is exclusive)
    let last = e.end.as_ref().map(|end| {
        if end.time.is_none() {
            end.date.add_days(-1)
        } else {
            end.date
        }
    });
    match last {
        Some(l) if l > s.date => format!("{} – {}", s.date.short(), l.short()),
        _ => s.date.long(),
    }
}

/// Item `i` as the event card draws it (files2-view-ics).
pub fn event_card(ics: &Ics, i: usize, file: &str) -> EvCard {
    let e = ics.event(i);
    let mut rows = Vec::new();
    let mut when = time_line(ics, &e);
    if e.repeat != neo_format_pim::ical::Repeat::Never {
        when = format!("{when} · {}", e.repeat.text());
    }
    rows.push(EvRow {
        icon: 0,
        main: day_line(&e).into(),
        sub: when.into(),
    });
    if !e.location.is_empty() {
        let (main, sub) = e.location.split_once(", ").unwrap_or((&e.location, ""));
        rows.push(EvRow {
            icon: 1,
            main: main.into(),
            sub: sub.into(),
        });
    }
    if let Some(m) = e.reminder {
        rows.push(EvRow {
            icon: 2,
            main: format!("Reminder {}", lower_first(&date::before(m))).into(),
            sub: SharedString::new(),
        });
    }
    if !e.description.is_empty() {
        rows.push(EvRow {
            icon: 3,
            main: e.description.as_str().into(),
            sub: SharedString::new(),
        });
    }
    let d = e.start.as_ref().map(|s| s.date);
    let up = |s: &str| s.get(..3).unwrap_or("").to_uppercase();
    let title = if e.summary.is_empty() {
        String::from("(No title)")
    } else {
        e.summary.clone()
    };
    EvCard {
        month: d.map(|d| up(d.month_name())).unwrap_or_default().into(),
        day: d.map(|d| d.day.to_string()).unwrap_or_default().into(),
        weekday: d.map(|d| up(d.weekday_name())).unwrap_or_default().into(),
        title: title.into(),
        sub: format!(
            "{} · from {file}",
            if e.kind == Kind::Todo {
                "To-do"
            } else {
                "Event"
            }
        )
        .into(),
        tint: colour(0xe0457b),
        rows: ModelRc::new(VecModel::from(rows)),
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

/// The reminder choices of the event form, in minutes before (None: no reminder).
pub const REMINDERS: [Option<i64>; 8] = [
    None,
    Some(0),
    Some(5),
    Some(15),
    Some(30),
    Some(60),
    Some(24 * 60),
    Some(7 * 24 * 60),
];

pub fn reminder_text(m: Option<i64>) -> String {
    m.map_or(String::from("None"), date::before)
}
