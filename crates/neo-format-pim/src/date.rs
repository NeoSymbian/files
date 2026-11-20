// SPDX-License-Identifier: Apache-2.0
//! Civil dates for the event card: weekdays, names, the texts of the design boards ("Saturday, 21 November 2026",
//! "Sat, 21 Nov 2026", "1 hour before", "GMT+4").

use alloc::format;
use alloc::string::{String, ToString};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

impl Date {
    /// `YYYYMMDD`; `None` when it isn't a real date.
    pub fn parse(s: &str) -> Option<Date> {
        let s = s.get(..8)?;
        if !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let d = Date {
            year: s[..4].parse().ok()?,
            month: s[4..6].parse().ok()?,
            day: s[6..8].parse().ok()?,
        };
        if (1..=12).contains(&d.month) && d.day >= 1 && d.day <= days_in(d.year, d.month) {
            Some(d)
        } else {
            None
        }
    }

    pub fn ymd(&self) -> String {
        format!("{:04}{:02}{:02}", self.year, self.month, self.day)
    }

    /// Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    pub fn days(&self) -> i64 {
        let y = i64::from(self.year) - i64::from(self.month <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let m = i64::from(self.month);
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    pub fn from_days(z: i64) -> Date {
        let z = z + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
        let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
        Date { year, month, day }
    }

    pub fn add_days(&self, n: i64) -> Date {
        Date::from_days(self.days() + n)
    }

    /// 0 = Monday.
    pub fn weekday(&self) -> usize {
        // 1970-01-01 was a Thursday
        (self.days() + 3).rem_euclid(7) as usize
    }

    pub fn weekday_name(&self) -> &'static str {
        DAYS[self.weekday()]
    }

    pub fn month_name(&self) -> &'static str {
        MONTHS[usize::from(self.month.clamp(1, 12)) - 1]
    }

    /// "Saturday, 21 November 2026"
    pub fn long(&self) -> String {
        format!(
            "{}, {} {} {}",
            self.weekday_name(),
            self.day,
            self.month_name(),
            self.year
        )
    }

    /// "Sat, 21 Nov 2026"
    pub fn short(&self) -> String {
        format!(
            "{}, {} {} {}",
            &self.weekday_name()[..3],
            self.day,
            &self.month_name()[..3],
            self.year
        )
    }
}

pub fn leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn days_in(y: i32, m: u8) -> u8 {
    match m {
        2 if leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// "19:00"
pub fn hm(h: u8, m: u8) -> String {
    format!("{h:02}:{m:02}")
}

/// A reminder `minutes` before the start: "At the start", "15 minutes before", "1 hour before", "2 days before".
pub fn before(minutes: i64) -> String {
    let unit = |n: i64, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one} before")
        } else {
            format!("{n} {many} before")
        }
    };
    match minutes {
        0 => String::from("At the start"),
        m if m < 0 => unit(-m, "minute", "minutes").replace("before", "after"),
        m if m % (24 * 60) == 0 => unit(m / (24 * 60), "day", "days"),
        m if m % 60 == 0 => unit(m / 60, "hour", "hours"),
        m => unit(m, "minute", "minutes"),
    }
}

/// "GMT+4", "GMT+5:30", "GMT−3", "GMT"
pub fn gmt(minutes: i32) -> String {
    if minutes == 0 {
        return String::from("GMT");
    }
    let sign = if minutes < 0 { '−' } else { '+' };
    let (h, m) = (minutes.abs() / 60, minutes.abs() % 60);
    if m == 0 {
        format!("GMT{sign}{h}")
    } else {
        format!("GMT{sign}{h}:{m:02}")
    }
}

/// A time zone's city: "Asia/Yerevan" → "Yerevan", "America/New_York" → "New York"; a TZID that isn't an Olson name
/// shows as it is.
pub fn city(tzid: &str) -> String {
    let t = tzid.trim_start_matches('/');
    match t.rsplit_once('/') {
        Some((_, c)) => c.replace('_', " "),
        None => t.to_string(),
    }
}
