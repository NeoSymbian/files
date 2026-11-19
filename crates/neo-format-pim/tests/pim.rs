// SPDX-License-Identifier: Apache-2.0
use neo_format_pim::date::{self, Date};
use neo_format_pim::ical::{Event, Ics, Kind, Repeat, When, duration};
use neo_format_pim::vcard::{Fields, Label, Vcf, Version};
use neo_format_pim::{Error, lines};

fn data(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

const FILES: [&str; 5] = [
    "contacts.vcf",
    "nokia21.vcf",
    "v4.vcf",
    "concert.ics",
    "meeting.ics",
];

#[test]
fn unedited_files_come_back_byte_for_byte() {
    for f in FILES {
        let t = data(f);
        if f.ends_with(".vcf") {
            assert_eq!(Vcf::parse(&t).unwrap().write(), t, "{f}");
        } else {
            assert_eq!(Ics::parse(&t).unwrap().write(), t, "{f}");
        }
    }
}

#[test]
fn cards_read() {
    let v = Vcf::parse(&data("contacts.vcf")).unwrap();
    assert_eq!(v.len(), 3);
    assert_eq!(v.version(0), Version::V30);
    assert_eq!(v.title(0), "Անի Պետրոսյան");
    let f = v.fields(1);
    assert_eq!((f.first.as_str(), f.last.as_str()), ("Дмитрий", "Орлов"));
    assert_eq!(f.company, "Яндекс");
    assert_eq!(f.phones, vec![(Label::Work, "+7 916 555-01-23".into())]);
    assert_eq!(f.emails, vec![(Label::Work, "d.orlov@example.ru".into())]);
    // a grouped TEL (Apple's item1.)
    assert_eq!(
        v.fields(2).phones,
        vec![(Label::Mobile, "+44 7700 900123".into())]
    );

    let n = Vcf::parse(&data("nokia21.vcf")).unwrap();
    assert_eq!(n.version(0), Version::V21);
    let f = n.fields(0);
    assert_eq!((f.first.as_str(), f.last.as_str()), ("Անի", "Պետրոսյան"));
    assert_eq!(
        f.phones,
        vec![
            (Label::Mobile, "+37491234567".into()),
            (Label::Home, "010123456".into()),
            (Label::Fax, "010999".into()),
        ]
    );
    assert_eq!(n.title(1), "Sam Carter");

    let v4 = Vcf::parse(&data("v4.vcf")).unwrap();
    assert_eq!(v4.version(0), Version::V40);
    let f = v4.fields(0);
    assert_eq!(f.phones[0], (Label::Mobile, "+374-91-234567".into()));
    assert_eq!(f.phones[1], (Label::Work, "+374-10-512048".into()));
    assert_eq!(f.emails, vec![(Label::Home, "anahit@example.am".into())]);
}

#[test]
fn an_edit_keeps_every_other_line() {
    let t = data("contacts.vcf");
    let mut v = Vcf::parse(&t).unwrap();
    let mut f = v.fields(1);
    f.first = "Dmitry".into();
    f.phones.push((Label::Mobile, "+7 999 000-00-00".into()));
    f.emails.clear();
    v.set(1, &f);
    let out = v.write();
    let again = Vcf::parse(&out).unwrap();
    assert_eq!(again.fields(1), f);
    assert_eq!(again.title(1), "Dmitry Орлов");
    // the cards around it and its unknown lines are untouched
    assert_eq!(again.card_text(0), Vcf::parse(&t).unwrap().card_text(0));
    assert_eq!(again.card_text(2), Vcf::parse(&t).unwrap().card_text(2));
    for keep in [
        "N:Орлов;Dmitry;Сергеевич;;\r\n",
        "TEL;TYPE=WORK,VOICE;TYPE=PREF:+7 916 555-01-23\r\n",
        "ADR;TYPE=WORK:;;ул. Льва Толстого\\, 16;Москва;;119021;Россия\r\n",
        " /8QAFAABAAAAAAAAAAAAAAAAAAAACf/EABQQAQAAAAAAAAAAAAAAAAAAAAD/2gAIAQEAAD8AKp//2Q==\r\n",
        "NOTE:Met at the\\, conference\\nSecond line\r\n",
        "TEL;TYPE=CELL:+7 999 000-00-00\r\n",
    ] {
        assert!(out.contains(keep), "{keep:?} in\n{out}");
    }
    assert!(!out.contains("d.orlov"));

    // a changed label rewrites the TYPE, a changed value keeps the parameters
    let mut f = again.fields(1);
    f.phones[0].1 = "+7 916 555-99-99".into();
    f.phones[1].0 = Label::Home;
    let mut v = again.clone();
    v.set(1, &f);
    let out = v.write();
    assert!(
        out.contains("TEL;TYPE=WORK,VOICE;TYPE=PREF:+7 916 555-99-99\r\n"),
        "{out}"
    );
    assert!(
        out.contains("TEL;TYPE=HOME,VOICE:+7 999 000-00-00\r\n"),
        "{out}"
    );
    assert_eq!(Vcf::parse(&out).unwrap().fields(1), f);
}

#[test]
fn edits_follow_the_cards_version() {
    // 2.1: quoted-printable for the Armenian name, bare types
    let mut n = Vcf::parse(&data("nokia21.vcf")).unwrap();
    let mut f = n.fields(1);
    f.first = "Սեմ".into();
    f.emails.push((Label::Work, "sam@work.example".into()));
    n.set(1, &f);
    let out = n.write();
    assert!(
        out.contains("N;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:Carter;=D5=8D=D5=A5=D5=B4\r\n"),
        "{out}"
    );
    assert!(
        out.contains("EMAIL;INTERNET;WORK:sam@work.example\r\n"),
        "{out}"
    );
    assert_eq!(Vcf::parse(&out).unwrap().fields(1), f);
    // the first card (folded quoted-printable, the PHOTO and its blank line) is untouched
    assert!(out.starts_with(&Vcf::parse(&data("nokia21.vcf")).unwrap().card_text(0)));

    // 4.0: lower-case types
    let mut v = Vcf::parse(&data("v4.vcf")).unwrap();
    let mut f = v.fields(0);
    f.phones.push((Label::Fax, "+374 10 000000".into()));
    f.company.clear();
    v.set(0, &f);
    let out = v.write();
    assert!(out.contains("TEL;TYPE=fax:+374 10 000000\n"), "{out}");
    assert!(!out.contains("ORG"));
    assert!(out.contains("KIND:individual\n"));
    assert_eq!(Vcf::parse(&out).unwrap().fields(0), f);
}

#[test]
fn long_values_fold_and_unfold() {
    let mut v = Vcf::parse(&data("v4.vcf")).unwrap();
    let mut f = v.fields(0);
    f.company = "Ա".repeat(60);
    v.set(0, &f);
    let out = v.write();
    assert!(out.lines().all(|l| l.len() <= 75), "{out}");
    assert_eq!(Vcf::parse(&out).unwrap().fields(0).company, f.company);
    // a 2.1 quoted-printable line breaks softly
    let mut n = Vcf::parse(&data("nokia21.vcf")).unwrap();
    let mut f = n.fields(1);
    f.company = "Ա".repeat(40);
    n.set(1, &f);
    let out = n.write();
    let card = Vcf::parse(&out).unwrap().card_text(1);
    assert!(card.lines().all(|l| l.len() <= 76), "{card}");
    assert_eq!(Vcf::parse(&out).unwrap().fields(1).company, f.company);
}

/// A number as Contacts keeps it (`Contact::tidy`).
fn dial(n: &str) -> String {
    n.chars()
        .filter(|c| c.is_ascii_digit() || "+*#pw".contains(*c))
        .collect()
}

/// The gate (files-app.md FA7): cards go both ways between Files and Contacts (phone's `contacts::vcard`).
#[test]
fn round_trip_with_phone() {
    use contacts::{Contact, Label as PL, vcard};
    // Contacts' export, read here
    let ani = Contact {
        first: "Անի".into(),
        last: "Պետրոսյան".into(),
        company: "Ararat; Software".into(),
        numbers: vec![
            (PL::Mobile, "+37491234567".into()),
            (PL::TelephoneWork, "010512048".into()),
            (PL::Fax, "010999".into()),
        ],
        email: "ani@example.am".into(),
        note: "Line one".into(),
        ..Default::default()
    };
    let text = vcard::export([&ani]);
    let v = Vcf::parse(&text).unwrap();
    assert_eq!(v.write(), text);
    let f = v.fields(0);
    assert_eq!((f.first.as_str(), f.last.as_str()), ("Անի", "Պետրոսյան"));
    assert_eq!(f.company, "Ararat; Software");
    assert_eq!(
        f.phones,
        vec![
            (Label::Mobile, "+37491234567".into()),
            (Label::Work, "010512048".into()),
            (Label::Fax, "010999".into()),
        ]
    );
    assert_eq!(f.emails, vec![(Label::Other, "ani@example.am".into())]);

    // edited here, read by Contacts
    let mut v = v;
    let mut f = f;
    f.first = "Անահիտ".into();
    f.company = "Արարատ".into();
    f.phones.remove(2);
    f.phones.push((Label::Home, "+374 10 111111".into()));
    v.set(0, &f);
    let back = vcard::import(&v.write());
    assert_eq!(back.skipped, 0);
    let c = &back.contacts[0];
    assert_eq!((c.first.as_str(), c.last.as_str()), ("Անահիտ", "Պետրոսյան"));
    assert_eq!(c.company, "Արարատ");
    assert_eq!(
        c.numbers,
        vec![
            (PL::Mobile, "+37491234567".into()),
            (PL::TelephoneWork, "010512048".into()),
            (PL::TelephoneHome, "+37410111111".into()), // Contacts keeps digits only
        ]
    );
    assert_eq!(c.note, "Line one");

    // every fixture card Contacts can read gives the same names and numbers here
    for name in ["contacts.vcf", "nokia21.vcf", "v4.vcf"] {
        let t = data(name);
        let ours = Vcf::parse(&t).unwrap();
        let theirs = vcard::import(&t);
        assert_eq!(theirs.contacts.len(), ours.len(), "{name}");
        for (i, c) in theirs.contacts.iter().enumerate() {
            let f = ours.fields(i);
            assert_eq!(
                (c.first.as_str(), c.last.as_str()),
                (f.first.as_str(), f.last.as_str()),
                "{name}"
            );
            assert_eq!(
                c.numbers.iter().map(|n| dial(&n.1)).collect::<Vec<_>>(),
                f.phones.iter().map(|n| dial(&n.1)).collect::<Vec<_>>(),
                "{name}"
            );
        }
    }
}

#[test]
fn events_read() {
    let c = Ics::parse(&data("concert.ics")).unwrap();
    assert_eq!(c.len(), 1);
    let e = c.event(0);
    assert_eq!(e.kind, Kind::Event);
    assert_eq!(e.summary, "Winter Jazz Night");
    assert_eq!(
        e.location,
        "Khachaturian Concert Hall, 46 Mashtots Ave, Yerevan"
    );
    let s = e.start.clone().unwrap();
    assert_eq!(s.date.long(), "Saturday, 21 November 2026");
    assert_eq!(s.time, Some((19, 0, 0)));
    assert_eq!(s.tzid.as_deref(), Some("Asia/Yerevan"));
    assert_eq!(e.end.unwrap().time, Some((21, 30, 0)));
    assert_eq!(e.reminder, Some(60));
    assert_eq!(date::before(e.reminder.unwrap()), "1 hour before");
    assert_eq!(c.tz_offset("Asia/Yerevan"), Some(240));
    assert_eq!(date::gmt(240), "GMT+4");
    assert_eq!(date::city("Asia/Yerevan"), "Yerevan");
    assert_eq!(e.repeat, Repeat::Never);

    let m = Ics::parse(&data("meeting.ics")).unwrap();
    assert_eq!(m.len(), 3);
    let s = m.event(0);
    assert!(s.start.as_ref().unwrap().utc);
    assert_eq!(s.end.unwrap().time, Some((6, 15, 0)), "DTSTART + DURATION");
    assert_eq!(
        s.repeat,
        Repeat::Custom("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".into())
    );
    let ny = m.event(1);
    assert_eq!(ny.start.as_ref().unwrap().time, None);
    assert_eq!(ny.repeat, Repeat::Yearly);
    let todo = m.event(2);
    assert_eq!(todo.kind, Kind::Todo);
    assert_eq!(
        todo.end.unwrap().date,
        Date {
            year: 2026,
            month: 11,
            day: 25
        }
    );
}

#[test]
fn an_event_edit_keeps_every_other_line() {
    let t = data("concert.ics");
    let mut c = Ics::parse(&t).unwrap();
    let mut e = c.event(0);
    e.summary = "Komitas Quartet · concert".into();
    e.location = "Komitas Chamber Music House, Yerevan".into();
    e.start = Some(When {
        date: Date {
            year: 2026,
            month: 11,
            day: 22,
        },
        ..e.start.unwrap()
    });
    e.reminder = Some(24 * 60);
    e.repeat = Repeat::Monthly;
    c.set(0, &e);
    let out = c.write();
    assert_eq!(Ics::parse(&out).unwrap().event(0), e);
    for keep in [
        "DTSTART;TZID=Asia/Yerevan:20261122T190000\r\n",
        "DTEND;TZID=Asia/Yerevan:20261121T213000\r\n",
        "SUMMARY:Komitas Quartet · concert\r\n",
        "LOCATION:Komitas Chamber Music House\\, Yerevan\r\n",
        "ATTENDEE;CN=\"Ani P\";ROLE=REQ-PARTICIPANT:mailto:ani@example.am\r\n",
        "GEO:40.1872;44.5152\r\n",
        "TRIGGER:-P1D\r\n",
        "RRULE:FREQ=MONTHLY\r\n",
        "TZOFFSETTO:+0400\r\n",
        "X-APPLE-TRAVEL-ADVISORY-BEHAVIOR:AUTOMATIC\r\n",
    ] {
        assert!(out.contains(keep), "{keep:?} in\n{out}");
    }

    // all day; no reminder: the VALARM goes
    let mut e = c.event(0);
    e.start = Some(When {
        date: Date {
            year: 2026,
            month: 11,
            day: 22,
        },
        time: None,
        utc: false,
        tzid: None,
    });
    e.end = Some(When {
        date: Date {
            year: 2026,
            month: 11,
            day: 23,
        },
        time: None,
        utc: false,
        tzid: None,
    });
    e.reminder = None;
    c.set(0, &e);
    let out = c.write();
    assert!(out.contains("DTSTART;VALUE=DATE:20261122\r\n"), "{out}");
    assert!(!out.contains("VALARM"));
    assert_eq!(Ics::parse(&out).unwrap().event(0), e);

    // a reminder on an event that had none: a new VALARM
    let mut m = Ics::parse(&data("meeting.ics")).unwrap();
    let mut e = m.event(1);
    e.reminder = Some(15);
    e.summary =
        "New Year's Eve party at the Republic Square with the whole family and friends".into();
    m.set(1, &e);
    let out = m.write();
    assert!(out.contains(
        "BEGIN:VALARM\nACTION:DISPLAY\nDESCRIPTION:Reminder\nTRIGGER:-PT15M\nEND:VALARM\nEND:VEVENT"
    ));
    assert!(
        out.lines()
            .all(|l| l.len() <= 75 || l.starts_with("SUMMARY:Stand-up")),
        "{out}"
    );
    let back = Ics::parse(&out).unwrap();
    assert_eq!(back.event(1), e);
    // the custom RRULE of the event before it is kept
    assert!(out.contains("RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR\n"));
    assert_eq!(
        back.event(0),
        Ics::parse(&data("meeting.ics")).unwrap().event(0)
    );
}

#[test]
fn dates_and_durations() {
    let d = Date {
        year: 2026,
        month: 11,
        day: 21,
    };
    assert_eq!(d.weekday_name(), "Saturday");
    assert_eq!(d.short(), "Sat, 21 Nov 2026");
    assert_eq!(Date::from_days(d.days()), d);
    assert_eq!(
        Date {
            year: 2024,
            month: 2,
            day: 29
        }
        .add_days(1),
        Date {
            year: 2024,
            month: 3,
            day: 1
        }
    );
    assert_eq!(
        Date {
            year: 1970,
            month: 1,
            day: 1
        }
        .days(),
        0
    );
    assert_eq!(Date::parse("20260230"), None);
    assert_eq!(Date::parse("2026-11-2"), None);
    assert_eq!(duration("-PT1H"), Some(-3600));
    assert_eq!(duration("P1W"), Some(7 * 86_400));
    assert_eq!(duration("P1DT2H30M"), Some(86_400 + 2 * 3600 + 30 * 60));
    assert_eq!(duration("PT"), None);
    assert_eq!(duration("P1H"), None);
    assert_eq!(duration("P99999999999999999999D"), None);
    assert_eq!(date::before(15), "15 minutes before");
    assert_eq!(date::before(2 * 24 * 60), "2 days before");
    assert_eq!(date::before(0), "At the start");
    assert_eq!(date::gmt(330), "GMT+5:30");
    assert_eq!(date::gmt(-180), "GMT−3");
}

#[test]
fn empty_or_foreign_files_are_errors() {
    assert_eq!(Vcf::parse("").unwrap_err(), Error::NoCards);
    assert_eq!(
        Vcf::parse("BEGIN:VCARD\r\nFN:x\r\n").unwrap_err(),
        Error::NoCards
    );
    assert_eq!(
        Ics::parse(&data("contacts.vcf")).unwrap_err(),
        Error::NoEvents
    );
    assert_eq!(
        Vcf::parse(&data("concert.ics")).unwrap_err(),
        Error::NoCards
    );
    assert_eq!(lines("no colon here\n")[0].name, "");
}

/// Cut short or with flipped bytes, every fixture parses, reads and edits without a panic.
#[test]
fn broken_files_never_panic() {
    let mut seed = 0x2545_f491u32;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for f in FILES {
        let good = data(f).into_bytes();
        let mut cases: Vec<Vec<u8>> = (1..good.len())
            .step_by(good.len() / 40 + 1)
            .map(|n| good[..n].to_vec())
            .collect();
        for _ in 0..300 {
            let mut b = good.clone();
            for _ in 0..1 + rnd() % 4 {
                let i = rnd() as usize % b.len();
                b[i] = rnd() as u8;
            }
            cases.push(b);
        }
        for b in cases {
            let t = String::from_utf8_lossy(&b);
            if let Ok(mut v) = Vcf::parse(&t) {
                for i in 0..v.len() {
                    let _ = v.title(i);
                    let mut f: Fields = v.fields(i);
                    f.first.push('x');
                    f.phones.push((Label::Work, "1".into()));
                    f.emails.clear();
                    v.set(i, &f);
                }
                let _ = Vcf::parse(&v.write());
            }
            if let Ok(mut c) = Ics::parse(&t) {
                for i in 0..c.len() {
                    let mut e: Event = c.event(i);
                    let _ = c.tz_offset("Asia/Yerevan");
                    e.summary.push('x');
                    e.reminder = e.reminder.map_or(Some(5), |_| None);
                    e.repeat = Repeat::Daily;
                    if let Some(s) = &e.start {
                        e.end = Some(s.plus(3600));
                    }
                    c.set(i, &e);
                }
                let _ = Ics::parse(&c.write());
            }
        }
    }
}
