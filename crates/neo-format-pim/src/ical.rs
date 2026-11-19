// SPDX-License-Identifier: Apache-2.0
//! iCalendar (RFC 5545) files: the events and to-dos in them, read for the event card ([`Ics::event`]) and edited by
//! the form ([`Ics::set`]). The form edits the title, the times, the place, the notes, the first reminder and a plain
//! repeat; everything else (VTIMEZONE, ATTENDEE, X-… and a custom RRULE the user didn't change) stays as it was read.

use crate::date::Date;
use crate::{Error, Line, Param, escape, folded, joined, lines, made, params_text};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Event,
    Todo,
}

/// A start or an end: a date (all day), or a date and a time (floating, UTC, or in a TZID's zone).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct When {
    pub date: Date,
    /// hours, minutes, seconds; `None` all day
    pub time: Option<(u8, u8, u8)>,
    pub utc: bool,
    pub tzid: Option<String>,
}

impl When {
    fn parse(l: &Line) -> Option<When> {
        let v = l.value.trim();
        let date = Date::parse(v)?;
        let time = match v.get(8..) {
            Some(t) if t.starts_with('T') && t.len() >= 7 => {
                let n = |r: core::ops::Range<usize>| t.get(r).and_then(|s| s.parse::<u8>().ok());
                let (h, m, s) = (n(1..3)?, n(3..5)?, n(5..7)?);
                (h < 24 && m < 60 && s < 61).then_some((h, m, s))
            }
            _ => None,
        };
        Some(When {
            date,
            time,
            utc: time.is_some() && v.ends_with('Z'),
            tzid: l.param("TZID").map(str::to_string),
        })
    }

    fn value(&self) -> String {
        match self.time {
            None => self.date.ymd(),
            Some((h, m, s)) => format!(
                "{}T{h:02}{m:02}{s:02}{}",
                self.date.ymd(),
                if self.utc { "Z" } else { "" }
            ),
        }
    }

    /// The parameters it writes, after `keep` (the old line's others).
    fn params(&self, keep: &[Param]) -> Vec<Param> {
        let mut p: Vec<Param> = keep
            .iter()
            .filter(|p| p.name != "VALUE" && p.name != "TZID")
            .cloned()
            .collect();
        if self.time.is_none() {
            p.push(param("VALUE", "DATE"));
        } else if let Some(z) = &self.tzid
            && !self.utc
        {
            p.push(param("TZID", z));
        }
        p
    }

    /// Seconds since 1970 as if UTC (for ordering and durations).
    pub fn seconds(&self) -> i64 {
        let (h, m, s) = self.time.unwrap_or_default();
        self.date.days() * 86_400 + i64::from(h) * 3600 + i64::from(m) * 60 + i64::from(s)
    }

    /// `secs` later (a DURATION's end).
    pub fn plus(&self, secs: i64) -> When {
        let t = self.seconds() + secs;
        let day = t.div_euclid(86_400);
        let rem = t.rem_euclid(86_400);
        When {
            date: Date::from_days(day),
            time: self
                .time
                .map(|_| ((rem / 3600) as u8, (rem / 60 % 60) as u8, (rem % 60) as u8)),
            utc: self.utc,
            tzid: self.tzid.clone(),
        }
    }
}

fn param(name: &str, value: &str) -> Param {
    Param {
        name: name.to_string(),
        values: alloc::vec![value.to_string()],
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Repeat {
    Never,
    Daily,
    Weekly,
    Monthly,
    Yearly,
    /// An RRULE with more than a FREQ (BYDAY, COUNT, UNTIL…), kept as it is unless the user picks another.
    Custom(String),
}

impl Repeat {
    pub const CHOICES: [Repeat; 5] = [
        Repeat::Never,
        Repeat::Daily,
        Repeat::Weekly,
        Repeat::Monthly,
        Repeat::Yearly,
    ];

    pub fn text(&self) -> &str {
        match self {
            Repeat::Never => "Never",
            Repeat::Daily => "Every day",
            Repeat::Weekly => "Every week",
            Repeat::Monthly => "Every month",
            Repeat::Yearly => "Every year",
            Repeat::Custom(_) => "Custom",
        }
    }

    fn of(rule: &str) -> Repeat {
        let mut freq = None;
        let mut other = false;
        for part in rule.trim().split(';').filter(|p| !p.is_empty()) {
            match part.split_once('=') {
                Some(("FREQ", f)) => freq = Some(f),
                Some(("INTERVAL", "1")) => {}
                _ => other = true,
            }
        }
        match (freq, other) {
            (Some("DAILY"), false) => Repeat::Daily,
            (Some("WEEKLY"), false) => Repeat::Weekly,
            (Some("MONTHLY"), false) => Repeat::Monthly,
            (Some("YEARLY"), false) => Repeat::Yearly,
            _ => Repeat::Custom(rule.trim().to_string()),
        }
    }

    fn rule(&self) -> Option<String> {
        Some(match self {
            Repeat::Never => return None,
            Repeat::Daily => String::from("FREQ=DAILY"),
            Repeat::Weekly => String::from("FREQ=WEEKLY"),
            Repeat::Monthly => String::from("FREQ=MONTHLY"),
            Repeat::Yearly => String::from("FREQ=YEARLY"),
            Repeat::Custom(r) => r.clone(),
        })
    }
}

/// What the card shows and the form edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: Kind,
    pub summary: String,
    pub location: String,
    pub description: String,
    pub start: Option<When>,
    pub end: Option<When>,
    /// minutes before the start of the first reminder (VALARM)
    pub reminder: Option<i64>,
    pub repeat: Repeat,
}

#[derive(Clone, Debug)]
pub struct Ics {
    lines: Vec<Line>,
    eol: &'static str,
    items: Vec<(Kind, usize, usize)>,
}

/// `[+-]P[nW][nD][T[nH][nM][nS]]` in seconds.
pub fn duration(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, s) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let s = s.strip_prefix('P')?;
    let (mut total, mut n, mut time, mut any) = (0i64, 0i64, false, false);
    for c in s.chars() {
        match c {
            '0'..='9' => n = n.checked_mul(10)?.checked_add(i64::from(c as u8 - b'0'))?,
            'T' => time = true,
            'W' | 'D' | 'H' | 'M' | 'S' => {
                let unit = match (c, time) {
                    ('W', _) => 7 * 86_400,
                    ('D', _) => 86_400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
                total = total.checked_add(n.checked_mul(unit)?)?;
                n = 0;
                any = true;
            }
            _ => return None,
        }
    }
    any.then_some(if neg { -total } else { total })
}

/// A reminder `minutes` before the start as a TRIGGER value.
fn trigger(minutes: i64) -> String {
    if minutes == 0 {
        return String::from("PT0S");
    }
    let (sign, m) = if minutes > 0 {
        ("-", minutes)
    } else {
        ("", -minutes)
    };
    if m % (24 * 60) == 0 {
        format!("{sign}P{}D", m / (24 * 60))
    } else if m % 60 == 0 {
        format!("{sign}PT{}H", m / 60)
    } else {
        format!("{sign}PT{m}M")
    }
}

/// "+0400" → 240
fn offset(s: &str) -> Option<i32> {
    let s = s.trim();
    let sign = match s.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let d = s.get(1..5)?;
    if !d.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(sign * (d[..2].parse::<i32>().ok()? * 60 + d[2..].parse::<i32>().ok()?))
}

fn begins(l: &Line) -> bool {
    l.name == "BEGIN"
}

fn ends(l: &Line) -> bool {
    l.name == "END"
}

impl Ics {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let mut v = Ics {
            lines: lines(text),
            eol: crate::eol_of(text),
            items: Vec::new(),
        };
        v.scan();
        if v.items.is_empty() {
            return Err(Error::NoEvents);
        }
        Ok(v)
    }

    fn scan(&mut self) {
        self.items.clear();
        let mut open: Option<(Kind, usize, usize)> = None;
        let mut depth = 0usize;
        for (i, l) in self.lines.iter().enumerate() {
            let v = l.value.trim().to_ascii_uppercase();
            if begins(l) {
                depth += 1;
                if open.is_none() {
                    let kind = match v.as_str() {
                        "VEVENT" => Some(Kind::Event),
                        "VTODO" => Some(Kind::Todo),
                        _ => None,
                    };
                    if let Some(k) = kind {
                        open = Some((k, i, depth));
                    }
                }
            } else if ends(l) {
                if let Some((k, b, d)) = open
                    && d == depth
                {
                    self.items.push((k, b, i));
                    open = None;
                }
                depth = depth.saturating_sub(1);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn write(&self) -> String {
        joined(&self.lines)
    }

    /// Item `i`'s own lines (not those of its VALARMs): their indices.
    fn own(&self, i: usize) -> Vec<usize> {
        let (_, b, e) = self.items[i];
        let mut out = Vec::new();
        let mut depth = 0usize;
        for k in b + 1..e {
            let l = &self.lines[k];
            if begins(l) {
                depth += 1;
            } else if ends(l) {
                depth = depth.saturating_sub(1);
            } else if depth == 0 {
                out.push(k);
            }
        }
        out
    }

    fn find(&self, i: usize, name: &str) -> Option<usize> {
        self.own(i)
            .into_iter()
            .find(|&k| self.lines[k].name == name)
    }

    /// The first VALARM's TRIGGER line in item `i`.
    fn trigger_line(&self, i: usize) -> Option<usize> {
        let (_, b, e) = self.items[i];
        let mut in_alarm = false;
        for k in b + 1..e {
            let l = &self.lines[k];
            if begins(l) && l.value.trim().eq_ignore_ascii_case("VALARM") {
                in_alarm = true;
            } else if ends(l) && l.value.trim().eq_ignore_ascii_case("VALARM") {
                in_alarm = false;
            } else if in_alarm && l.name == "TRIGGER" {
                return Some(k);
            }
        }
        None
    }

    pub fn event(&self, i: usize) -> Event {
        let text = |n: &str| {
            self.find(i, n)
                .map(|k| self.lines[k].text())
                .unwrap_or_default()
        };
        let when = |n: &str| self.find(i, n).and_then(|k| When::parse(&self.lines[k]));
        let start = when("DTSTART");
        let end = when("DTEND").or_else(|| when("DUE")).or_else(|| {
            let d = duration(&self.lines[self.find(i, "DURATION")?].value)?;
            Some(start.as_ref()?.plus(d))
        });
        // shortcut: a TRIGGER relative to the end, or at a fixed time, isn't shown; the form keeps it unless changed
        let reminder = self.trigger_line(i).and_then(|k| {
            let l = &self.lines[k];
            if l.has("VALUE", "DATE-TIME") || l.has("RELATED", "END") {
                return None;
            }
            duration(&l.value).map(|s| -s / 60)
        });
        Event {
            kind: self.items[i].0,
            summary: text("SUMMARY"),
            location: text("LOCATION"),
            description: text("DESCRIPTION"),
            start,
            end,
            reminder,
            repeat: self
                .find(i, "RRULE")
                .map_or(Repeat::Never, |k| Repeat::of(&self.lines[k].value)),
        }
    }

    /// The UTC offset (minutes) of a VTIMEZONE in the file; its STANDARD part (shortcut: daylight saving is not
    /// chosen by date; Yerevan, Moscow and most of the boards' zones have none).
    pub fn tz_offset(&self, tzid: &str) -> Option<i32> {
        let mut in_zone = false;
        let mut this = false;
        let (mut standard, mut daylight) = (None, None);
        let mut part = "";
        for l in &self.lines {
            let v = l.value.trim();
            match l.name.as_str() {
                "BEGIN" if v.eq_ignore_ascii_case("VTIMEZONE") => {
                    in_zone = true;
                    this = false;
                }
                "END" if v.eq_ignore_ascii_case("VTIMEZONE") => {
                    if this {
                        return standard.or(daylight);
                    }
                    in_zone = false;
                }
                "BEGIN" if in_zone => {
                    part = if v.eq_ignore_ascii_case("DAYLIGHT") {
                        "D"
                    } else {
                        "S"
                    }
                }
                "TZID" if in_zone => this = l.text().trim() == tzid,
                "TZOFFSETTO" if in_zone => {
                    if part == "D" {
                        daylight = daylight.or(offset(v));
                    } else {
                        standard = standard.or(offset(v));
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Change item `i` to `ev`; lines whose field didn't change are kept.
    pub fn set(&mut self, i: usize, ev: &Event) {
        let old = self.event(i);
        if old == *ev {
            return;
        }
        if old.summary != ev.summary {
            self.text_line(i, "SUMMARY", &ev.summary);
        }
        if old.location != ev.location {
            self.text_line(i, "LOCATION", &ev.location);
        }
        if old.description != ev.description {
            self.text_line(i, "DESCRIPTION", &ev.description);
        }
        if old.start != ev.start {
            self.when_line(i, "DTSTART", ev.start.as_ref());
        }
        if old.end != ev.end {
            let name = if self.items[i].0 == Kind::Todo {
                "DUE"
            } else {
                "DTEND"
            };
            if let Some(k) = self.find(i, "DURATION") {
                self.remove(k);
            }
            self.when_line(i, name, ev.end.as_ref());
        }
        if old.repeat != ev.repeat {
            match ev.repeat.rule() {
                Some(r) => self.put(i, "RRULE", Vec::new(), r),
                None => self.drop_prop(i, "RRULE"),
            }
        }
        if old.reminder != ev.reminder {
            self.reminder(i, ev.reminder);
        }
    }

    fn remove(&mut self, k: usize) {
        self.lines.remove(k);
        self.scan();
    }

    fn drop_prop(&mut self, i: usize, name: &str) {
        if let Some(k) = self.find(i, name) {
            self.remove(k);
        }
    }

    /// Replace `name`'s line in item `i` (keeping its other parameters) or add it before the item's first nested
    /// component (or its END).
    fn put(&mut self, i: usize, name: &str, params: Vec<Param>, value: String) {
        let at = self.find(i, name);
        let mut params = params;
        if let Some(k) = at {
            for p in &self.lines[k].params {
                if !params.iter().any(|q| q.name == p.name) && p.name != "VALUE" && p.name != "TZID"
                {
                    params.push(p.clone());
                }
            }
        }
        let head = format!("{name}{}", params_text(&params, false));
        let line = made(folded(&head, &value, self.eol));
        match at {
            Some(k) => self.lines[k] = line,
            None => {
                let (_, b, e) = self.items[i];
                let pos = (b + 1..e).find(|&k| begins(&self.lines[k])).unwrap_or(e);
                self.lines.insert(pos, line);
            }
        }
        self.scan();
    }

    fn text_line(&mut self, i: usize, name: &str, text: &str) {
        if text.is_empty() {
            self.drop_prop(i, name);
        } else {
            self.put(i, name, Vec::new(), escape(text, true));
        }
    }

    fn when_line(&mut self, i: usize, name: &str, w: Option<&When>) {
        let Some(w) = w else {
            return self.drop_prop(i, name);
        };
        let keep = self
            .find(i, name)
            .map(|k| self.lines[k].params.clone())
            .unwrap_or_default();
        // put() keeps the old line's parameters, minus VALUE and TZID: the new value sets its own
        let params = w.params(&keep);
        let at = self.find(i, name);
        if let Some(k) = at {
            self.lines[k]
                .params
                .retain(|p| p.name != "VALUE" && p.name != "TZID");
        }
        self.put(i, name, params, w.value());
    }

    fn reminder(&mut self, i: usize, minutes: Option<i64>) {
        match (minutes, self.trigger_line(i)) {
            (Some(m), Some(k)) => {
                let mut params = self.lines[k].params.clone();
                params.retain(|p| p.name != "VALUE" && p.name != "RELATED");
                let head = format!("TRIGGER{}", params_text(&params, false));
                self.lines[k] = made(folded(&head, &trigger(m), self.eol));
            }
            (Some(m), None) => {
                let (_, _, e) = self.items[i];
                let eol = self.eol;
                let block = format!(
                    "BEGIN:VALARM{eol}ACTION:DISPLAY{eol}DESCRIPTION:Reminder{eol}TRIGGER:{}{eol}END:VALARM{eol}",
                    trigger(m)
                );
                let new = lines(&block);
                self.lines.splice(e..e, new);
                self.scan();
            }
            (None, _) => {
                // every VALARM of the item goes
                loop {
                    let (_, b, e) = self.items[i];
                    let Some(s) = (b + 1..e).find(|&k| {
                        begins(&self.lines[k])
                            && self.lines[k].value.trim().eq_ignore_ascii_case("VALARM")
                    }) else {
                        break;
                    };
                    let end = (s..e)
                        .find(|&k| {
                            ends(&self.lines[k])
                                && self.lines[k].value.trim().eq_ignore_ascii_case("VALARM")
                        })
                        .unwrap_or(s);
                    self.lines.drain(s..=end);
                    self.scan();
                }
            }
        }
    }
}
