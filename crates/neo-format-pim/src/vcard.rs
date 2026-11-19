// SPDX-License-Identifier: Apache-2.0
//! vCard 2.1, 3.0 and 4.0 files: one or more cards, read for the viewer ([`Vcf::fields`]) and edited by the form
//! ([`Vcf::set`]). The form edits the names, the company, the numbers and the emails; every other line of the card,
//! and every other card, is kept as it was read.
//!
//! Lines a form writes follow the card's version: 2.1 as phone's `contacts::vcard::export` writes it (bare type
//! parameters, UTF-8 quoted-printable for anything not plain ASCII), 3.0 and 4.0 with `TYPE=` and backslash escapes.

use crate::{Error, Line, Param, escape, folded, joined, lines, made, params_text, qp_line};
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    V21,
    V30,
    V40,
}

impl Version {
    pub fn name(self) -> &'static str {
        match self {
            Version::V21 => "2.1",
            Version::V30 => "3.0",
            Version::V40 => "4.0",
        }
    }
}

/// A number's or an email's label (the form's dropdown).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Label {
    Mobile,
    Home,
    Work,
    Fax,
    Other,
}

impl Label {
    pub const PHONE: [Label; 5] = [
        Label::Mobile,
        Label::Home,
        Label::Work,
        Label::Fax,
        Label::Other,
    ];
    pub const EMAIL: [Label; 3] = [Label::Home, Label::Work, Label::Other];

    pub fn name(self) -> &'static str {
        match self {
            Label::Mobile => "Mobile",
            Label::Home => "Home",
            Label::Work => "Work",
            Label::Fax => "Fax",
            Label::Other => "Other",
        }
    }

    fn of(l: &Line) -> Label {
        if l.name == "TEL" && l.has("TYPE", "CELL") {
            Label::Mobile
        } else if l.name == "TEL" && l.has("TYPE", "FAX") {
            Label::Fax
        } else if l.has("TYPE", "HOME") {
            Label::Home
        } else if l.has("TYPE", "WORK") {
            Label::Work
        } else {
            Label::Other
        }
    }

    /// The TYPE values a new line gets.
    fn types(self, email: bool, v: Version) -> Vec<&'static str> {
        let mut t = match (email, self) {
            (false, Label::Mobile) => vec!["CELL"],
            (false, Label::Home) => vec!["HOME", "VOICE"],
            (false, Label::Work) => vec!["WORK", "VOICE"],
            (false, Label::Fax) => vec!["FAX"],
            (false, Label::Other) => vec!["VOICE"],
            (true, Label::Home) => vec!["INTERNET", "HOME"],
            (true, Label::Work) => vec!["INTERNET", "WORK"],
            (true, _) => vec!["INTERNET"],
        };
        if v == Version::V40 {
            // 4.0 has no INTERNET type and writes types in lower case
            t.retain(|x| *x != "INTERNET");
        }
        t
    }
}

/// What the form edits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fields {
    pub first: String,
    pub last: String,
    pub company: String,
    pub phones: Vec<(Label, String)>,
    pub emails: Vec<(Label, String)>,
}

#[derive(Clone, Debug)]
pub struct Vcf {
    lines: Vec<Line>,
    eol: &'static str,
    /// each complete card's BEGIN and END line
    cards: Vec<(usize, usize)>,
}

impl Vcf {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let mut v = Vcf {
            lines: lines(text),
            eol: crate::eol_of(text),
            cards: Vec::new(),
        };
        v.scan();
        if v.cards.is_empty() {
            return Err(Error::NoCards);
        }
        Ok(v)
    }

    fn scan(&mut self) {
        self.cards.clear();
        let mut start = None;
        for (i, l) in self.lines.iter().enumerate() {
            match l.name.as_str() {
                "BEGIN" if l.value.trim().eq_ignore_ascii_case("VCARD") => start = Some(i),
                "END" if l.value.trim().eq_ignore_ascii_case("VCARD") => {
                    if let Some(s) = start.take() {
                        self.cards.push((s, i));
                    }
                }
                _ => {}
            }
        }
    }

    pub fn len(&self) -> usize {
        self.cards.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    fn card(&self, i: usize) -> &[Line] {
        let (b, e) = self.cards[i];
        &self.lines[b..=e]
    }

    pub fn version(&self, i: usize) -> Version {
        let v = self
            .card(i)
            .iter()
            .find(|l| l.name == "VERSION")
            .map(|l| l.value.trim().to_string());
        match v.as_deref() {
            Some("3.0") => Version::V30,
            Some("4.0") => Version::V40,
            _ => Version::V21,
        }
    }

    pub fn fields(&self, i: usize) -> Fields {
        let mut f = Fields::default();
        let mut has_n = false;
        for l in self.card(i) {
            match l.name.as_str() {
                "N" => {
                    let p = l.parts();
                    f.last = p.first().cloned().unwrap_or_default();
                    f.first = p.get(1).cloned().unwrap_or_default();
                    has_n = true;
                }
                "ORG" => f.company = l.parts().into_iter().next().unwrap_or_default(),
                "TEL" => f.phones.push((Label::of(l), number(l))),
                "EMAIL" => f.emails.push((Label::of(l), l.text().trim().to_string())),
                _ => {}
            }
        }
        if !has_n && let Some(l) = self.card(i).iter().find(|l| l.name == "FN") {
            f.first = l.text();
        }
        f
    }

    /// The name the card shows: FN, else the names, else the company, else the first number or email.
    pub fn title(&self, i: usize) -> String {
        if let Some(l) = self.card(i).iter().find(|l| l.name == "FN") {
            let t = l.text();
            if !t.trim().is_empty() {
                return t.trim().to_string();
            }
        }
        let f = self.fields(i);
        let name = full_name(&f.first, &f.last);
        [
            name,
            f.company,
            f.phones.first().map(|p| p.1.clone()).unwrap_or_default(),
            f.emails.first().map(|p| p.1.clone()).unwrap_or_default(),
        ]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| String::from("Contact"))
    }

    /// Card `i`'s text as read (for Add to Contacts: only the ticked cards are handed over).
    pub fn card_text(&self, i: usize) -> String {
        joined(self.card(i))
    }

    /// The whole file.
    pub fn write(&self) -> String {
        joined(&self.lines)
    }

    /// Change card `i` to `f`. Lines whose field didn't change are kept as they were.
    pub fn set(&mut self, i: usize, f: &Fields) {
        let old = self.fields(i);
        if old == *f {
            return;
        }
        let v = self.version(i);
        let (b, e) = self.cards[i];
        let mut card: Vec<Line> = self.lines[b..=e].to_vec();
        let ed = Editor { v, eol: self.eol };
        if (old.first.as_str(), old.last.as_str()) != (f.first.as_str(), f.last.as_str()) {
            ed.names(&mut card, f);
        }
        if old.company != f.company {
            ed.company(&mut card, &f.company);
        }
        if old.phones != f.phones {
            ed.list(&mut card, "TEL", &old.phones, &f.phones);
        }
        if old.emails != f.emails {
            ed.list(&mut card, "EMAIL", &old.emails, &f.emails);
        }
        self.lines.splice(b..=e, card);
        self.scan();
    }
}

/// A number's text (4.0 may write it as a `tel:` URI).
fn number(l: &Line) -> String {
    let t = l.text();
    let t = t.trim();
    t.strip_prefix("tel:").unwrap_or(t).to_string()
}

pub fn full_name(first: &str, last: &str) -> String {
    match (first.trim(), last.trim()) {
        ("", l) => l.to_string(),
        (f, "") => f.to_string(),
        (f, l) => alloc::format!("{f} {l}"),
    }
}

struct Editor {
    v: Version,
    eol: &'static str,
}

impl Editor {
    /// A line's text: `group.NAME;params:value` in the card's dialect (`value` already escaped).
    fn text(&self, group: &str, name: &str, params: &[Param], value: &str) -> String {
        let mut head = String::new();
        if !group.is_empty() {
            head.push_str(group);
            head.push('.');
        }
        head.push_str(name);
        head.push_str(&params_text(params, self.v == Version::V21));
        if self.v == Version::V21 {
            qp_line(&head, value, self.eol)
        } else {
            folded(&head, value, self.eol)
        }
    }

    fn esc(&self, s: &str) -> String {
        escape(s, self.v != Version::V21)
    }

    /// Before END (or after the last line called `after`).
    fn insert_at(card: &[Line], after: &[&str]) -> usize {
        card.iter()
            .rposition(|l| after.contains(&l.name.as_str()))
            .map_or(card.len() - 1, |i| i + 1)
    }

    fn names(&self, card: &mut Vec<Line>, f: &Fields) {
        let n = card.iter().position(|l| l.name == "N");
        let mut parts = n.map(|i| card[i].parts()).unwrap_or_default();
        // a new N gets its five parts; an old one keeps its shape ("N:Carter;Sam")
        let shape = if n.is_some() { 2 } else { 5 };
        parts.resize(parts.len().max(shape), String::new());
        parts[0] = f.last.clone();
        parts[1] = f.first.clone();
        let value: Vec<String> = parts.iter().map(|p| self.esc(p)).collect();
        let value = value.join(";");
        match n {
            Some(i) => {
                let (g, p) = (card[i].group.clone(), card[i].params.clone());
                card[i] = made(self.text(&g, "N", &p, &value));
            }
            None => {
                let at = Self::insert_at(card, &["VERSION"]);
                card.insert(at, made(self.text("", "N", &[], &value)));
            }
        }
        let full = full_name(&f.first, &f.last);
        let full = if full.is_empty() {
            f.company.clone()
        } else {
            full
        };
        match card.iter().position(|l| l.name == "FN") {
            Some(i) => {
                let (g, p) = (card[i].group.clone(), card[i].params.clone());
                card[i] = made(self.text(&g, "FN", &p, &self.esc(&full)));
            }
            // FN is required from 3.0 on
            None if self.v != Version::V21 || !full.is_empty() => {
                let at = Self::insert_at(card, &["N"]);
                card.insert(at, made(self.text("", "FN", &[], &self.esc(&full))));
            }
            None => {}
        }
    }

    fn company(&self, card: &mut Vec<Line>, company: &str) {
        let at = card.iter().position(|l| l.name == "ORG");
        match (at, company.is_empty()) {
            (Some(i), true) => {
                card.remove(i);
            }
            (Some(i), false) => {
                let mut parts = card[i].parts();
                if parts.is_empty() {
                    parts.push(String::new());
                }
                parts[0] = company.to_string();
                let value: Vec<String> = parts.iter().map(|p| self.esc(p)).collect();
                let (g, p) = (card[i].group.clone(), card[i].params.clone());
                card[i] = made(self.text(&g, "ORG", &p, &value.join(";")));
            }
            (None, false) => {
                let at = Self::insert_at(card, &["FN", "N"]);
                card.insert(at, made(self.text("", "ORG", &[], &self.esc(company))));
            }
            (None, true) => {}
        }
    }

    fn typed(&self, label: Label, email: bool, keep: &[Param]) -> Vec<Param> {
        let mut p: Vec<Param> = keep.iter().filter(|p| p.name != "TYPE").cloned().collect();
        let t = label.types(email, self.v);
        if !t.is_empty() {
            let lower = self.v == Version::V40;
            p.insert(
                0,
                Param {
                    name: String::from("TYPE"),
                    values: t
                        .iter()
                        .map(|s| {
                            if lower {
                                s.to_ascii_lowercase()
                            } else {
                                s.to_string()
                            }
                        })
                        .collect(),
                },
            );
        }
        p
    }

    /// TEL or EMAIL lines from `old` to `new`, matched in order: an unchanged pair keeps its line, a changed value
    /// keeps the line's parameters (PREF, …), a changed label gets new TYPE values.
    fn list(
        &self,
        card: &mut Vec<Line>,
        name: &str,
        old: &[(Label, String)],
        new: &[(Label, String)],
    ) {
        let email = name == "EMAIL";
        let at: Vec<usize> = card
            .iter()
            .enumerate()
            .filter(|(_, l)| l.name == name)
            .map(|(i, _)| i)
            .collect();
        for (k, (label, value)) in new.iter().enumerate() {
            if let Some(&i) = at.get(k) {
                if old.get(k) == Some(&(*label, value.clone())) {
                    continue;
                }
                let line = &card[i];
                let params = if Label::of(line) == *label {
                    line.params.clone()
                } else {
                    self.typed(*label, email, &line.params)
                };
                let g = line.group.clone();
                card[i] = made(self.text(&g, name, &params, &self.esc(value)));
            }
        }
        // the lines past the new list's end go; the new list's extra entries follow the last kept line
        for &i in at.iter().skip(new.len()).rev() {
            card.remove(i);
        }
        let mut pos = Self::insert_at(card, &[name]);
        if !card.iter().any(|l| l.name == name) {
            pos = Self::insert_at(card, &["TEL", "ORG", "FN", "N"]);
        }
        for (label, value) in new.iter().skip(at.len()) {
            let params = self.typed(*label, email, &[]);
            card.insert(pos, made(self.text("", name, &params, &self.esc(value))));
            pos += 1;
        }
    }
}
