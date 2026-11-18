// SPDX-License-Identifier: Apache-2.0
//! The Files app's model as data: Symbian paths (`X:\dir\`), folder ordering, the texts a row shows (size, date),
//! drive labels, text-file previews, name checks and error messages. Pure logic, tested on the host; the file server
//! calls live in `e32::fs` and the UI in the Files app (`files-ui`, `symbian/app`).
#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// One row of a folder listing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// `TTime`: microseconds since 0000-01-01 (UTC).
    pub modified: i64,
    /// `TEntry::iAtt` bits ([`HIDDEN`]…).
    pub att: u32,
    /// A folder's entries, when counted ("186 items").
    pub count: Option<u32>,
}

pub const READ_ONLY: u32 = 0x01;
pub const HIDDEN: u32 = 0x02;
pub const SYSTEM: u32 = 0x04;

/// A copy's file while it is written (`files-ops`): never listed.
pub const PART_SUFFIX: &str = ".~part";

/// What a folder shows: without hidden and system entries unless `show_hidden`, never a copy's unfinished file.
pub fn visible(items: Vec<Item>, show_hidden: bool) -> Vec<Item> {
    items
        .into_iter()
        .filter(|i| show_hidden || i.att & (HIDDEN | SYSTEM) == 0)
        .filter(|i| !i.name.ends_with(PART_SUFFIX))
        .collect()
}

/// The folder above `dir` (`X:\a\b\` → `X:\a\`); `None` at a drive root.
pub fn parent(dir: &str) -> Option<String> {
    let trimmed = dir.strip_suffix('\\')?;
    let cut = trimmed.rfind('\\')?;
    Some(String::from(&dir[..=cut]))
}

/// The folder `name` inside `dir`.
pub fn join_dir(dir: &str, name: &str) -> String {
    format!("{dir}{name}\\")
}

/// The file `name` inside `dir`.
pub fn join_file(dir: &str, name: &str) -> String {
    format!("{dir}{name}")
}

/// The last component of a folder path, for titles (`X:\a\b\` → `b`, `C:\` → `C:`).
pub fn folder_name(dir: &str) -> &str {
    let t = dir.strip_suffix('\\').unwrap_or(dir);
    t.rsplit('\\').next().unwrap_or(t)
}

/// What a folder is sorted by (the Sort sheet, files2-sort).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortKey {
    #[default]
    Name,
    Date,
    Size,
    Type,
}

impl SortKey {
    pub const ALL: [SortKey; 4] = [SortKey::Name, SortKey::Date, SortKey::Size, SortKey::Type];

    pub fn label(self) -> &'static str {
        match self {
            SortKey::Name => "Name",
            SortKey::Date => "Date modified",
            SortKey::Size => "Size",
            SortKey::Type => "Type",
        }
    }

    pub fn from_index(i: i32) -> SortKey {
        usize::try_from(i)
            .ok()
            .and_then(|i| Self::ALL.get(i).copied())
            .unwrap_or_default()
    }

    pub fn index(self) -> i32 {
        Self::ALL.iter().position(|k| *k == self).unwrap_or(0) as i32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    /// A → Z, oldest first, smallest first.
    pub ascending: bool,
    pub folders_first: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Sort {
            key: SortKey::Name,
            ascending: true,
            folders_first: true,
        }
    }
}

fn ext(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => "",
    }
}

fn cmp_ignore_case(a: &str, b: &str) -> core::cmp::Ordering {
    let (x, y) = (a.chars(), b.chars());
    x.flat_map(char::to_lowercase)
        .cmp(y.flat_map(char::to_lowercase))
}

/// Order a folder: folders first if asked, then by the key (ties by name), in either direction.
pub fn sort(items: &mut [Item], how: Sort) {
    items.sort_by(|a, b| {
        let dirs = if how.folders_first {
            b.is_dir.cmp(&a.is_dir)
        } else {
            core::cmp::Ordering::Equal
        };
        let by = match how.key {
            SortKey::Name => cmp_ignore_case(&a.name, &b.name),
            SortKey::Date => a.modified.cmp(&b.modified),
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Type => cmp_ignore_case(ext(&a.name), ext(&b.name)),
        }
        .then_with(|| cmp_ignore_case(&a.name, &b.name));
        dirs.then(if how.ascending { by } else { by.reverse() })
    });
}

/// The breadcrumb: (label, folder) per level, the drive first (`E:\Music\` → `E:`, `Music`).
pub fn crumbs(dir: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut at = String::new();
    for part in dir.split('\\').filter(|p| !p.is_empty()) {
        at.push_str(part);
        at.push('\\');
        out.push((String::from(part), at.clone()));
    }
    out
}

/// A name as (stem, extension with its dot): Rename edits the stem and keeps the extension (files2-rename).
pub fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    }
}

/// `base` + `ext`, or the first free `base (n)` + `ext` when a name in `taken` has it (any case).
pub fn free_name(taken: &[&str], base: &str, ext: &str) -> String {
    let used = |n: &str| taken.iter().any(|t| cmp_ignore_case(t, n).is_eq());
    let first = format!("{base}{ext}");
    if !used(&first) {
        return first;
    }
    (1..)
        .map(|n| format!("{base} ({n}){ext}"))
        .find(|n| !used(n))
        .unwrap_or(first)
}

/// `512 B`, `1.5 KB`, `12 MB`: one decimal below 10; gigabytes keep one up to 100 (`41.8 GB`) unless it is 0
/// (`8 GB`), as the boards write drive sizes.
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    let decimals_below = if u >= 2 { 100.0 } else { 10.0 };
    if v < decimals_below {
        // one decimal, rounded half up, without float formatting's std dependencies
        let tenths = (v * 10.0 + 0.5) as u64;
        if u >= 2 && tenths.is_multiple_of(10) {
            return format!("{} {}", tenths / 10, UNITS[u]);
        }
        if (tenths as f64) < decimals_below * 10.0 {
            return format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[u]);
        }
    }
    format!("{} {}", (v + 0.5) as u64, UNITS[u])
}

/// Days from Symbian's `TTime` zero to 1970-01-01. Symbian counts leap years before 1600 the Julian way
/// (`euser/us_time.cpp:2486`; 2000-01-01 is day 730,497, `kernel/stimer.cpp:29`), 12 days more than proleptic
/// Gregorian; the kernel clock and ROM timestamps use it.
// shortcut: dates before 1600 come out Gregorian, not Symbian's Julian; nothing on a phone has them.
const DAYS_TO_UNIX: i64 = 719_540;

/// Year, month (1-12), day of the civil date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `2026-11-06 14:32` (UTC) for a `TTime`.
pub fn date_text(ttime: i64) -> String {
    let secs = ttime.div_euclid(1_000_000);
    let days = secs.div_euclid(86_400) - DAYS_TO_UNIX;
    let s = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", s / 3600, s / 60 % 60)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `3 Nov` this year, `2 Feb 2019` another (`now` is a `TTime` too).
pub fn day_text(ttime: i64, now: i64) -> String {
    let day = |t: i64| civil_from_days(t.div_euclid(86_400_000_000) - DAYS_TO_UNIX);
    let (y, m, d) = day(ttime);
    let month = MONTHS[(m - 1) as usize];
    if y == day(now).0 {
        format!("{d} {month}")
    } else {
        format!("{d} {month} {y}")
    }
}

/// A row's subtitle (files2-folder-list): `186 items` or `Folder`, else `4.1 MB · 3 Nov`.
pub fn detail_text(item: &Item, now: i64) -> String {
    match (item.is_dir, item.count) {
        (true, Some(1)) => String::from("1 item"),
        (true, Some(n)) => format!("{n} items"),
        (true, None) => String::from("Folder"),
        _ => format!(
            "{} · {}",
            size_text(item.size),
            day_text(item.modified, now)
        ),
    }
}

/// What to call a drive (files2-browse), from its letter, `TDriveInfo::iDriveAtt` and `TMediaType`.
pub fn drive_label(letter: char, drive_att: u32, media: u32) -> &'static str {
    const ROM: u32 = 0x02;
    const REMOVABLE: u32 = 0x20;
    const MEDIA_RAM: u32 = 5;
    const MEDIA_ROM: u32 = 7;
    if drive_att & ROM != 0 || media == MEDIA_ROM {
        "ROM"
    } else if drive_att & REMOVABLE != 0 {
        "Memory card"
    } else if letter == 'C' {
        "Phone memory"
    } else if media == MEDIA_RAM {
        "RAM disk"
    } else {
        "Mass memory"
    }
}

/// The used fraction (0..1) of a drive, for its usage bar.
pub fn used_fraction(size: i64, free: i64) -> f32 {
    if size <= 0 {
        return 0.0;
    }
    (1.0 - free.clamp(0, size) as f32 / size as f32).clamp(0.0, 1.0)
}

/// Most of a file the viewer shows.
pub const PREVIEW_MAX: usize = 64 * 1024;

/// A file's bytes as text to show: UTF-16LE when it starts with a byte-order mark, else UTF-8 (bad sequences become
/// U+FFFD). Line ends become `\n`, tabs 4 spaces; past [`PREVIEW_MAX`] bytes a note says it was cut.
pub fn text_preview(bytes: &[u8]) -> String {
    let cut = bytes.len() > PREVIEW_MAX;
    let b = &bytes[..bytes.len().min(PREVIEW_MAX)];
    let raw: String = if let Some(rest) = b.strip_prefix(&[0xFF, 0xFE]) {
        char::decode_utf16(
            rest.as_chunks::<2>()
                .0
                .iter()
                .map(|p| u16::from_le_bytes(*p)),
        )
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
    } else {
        String::from_utf8_lossy(b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b)).into()
    };
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                chars.next_if_eq(&'\n');
                out.push('\n');
            }
            '\t' => out.push_str("    "),
            c => out.push(c),
        }
    }
    if cut {
        out.push_str("\n… (first 64 KB shown)");
    }
    out
}

/// A name the file server will take for a new file or folder: not empty, not `.`/`..`, at most 256 characters, none
/// of `\ / : * ? " < > |` or control characters, and no trailing dot or space.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.encode_utf16().count() <= 256
        && !name.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
}

/// A short message for a file-server error code.
pub fn error_text(code: i32) -> String {
    let s = match code {
        -1 => "Not found",
        -11 => "Already exists",
        -12 => "Path not found",
        -14 => "In use, or folder not empty",
        -21 => "Read-only drive",
        -28 => "Bad name",
        -46 => "Permission denied",
        _ => return format!("Error {code}"),
    };
    String::from(s)
}

/// The names in `items`, for tests and logs.
pub fn names(items: &[Item]) -> Vec<&str> {
    items.iter().map(|i| i.name.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn item(name: &str, is_dir: bool) -> Item {
        Item {
            name: name.to_string(),
            is_dir,
            ..Item::default()
        }
    }

    #[test]
    fn paths() {
        assert_eq!(parent("C:\\a\\b\\").as_deref(), Some("C:\\a\\"));
        assert_eq!(parent("C:\\a\\").as_deref(), Some("C:\\"));
        assert_eq!(parent("C:\\"), None);
        assert_eq!(join_dir("Z:\\sys\\", "bin"), "Z:\\sys\\bin\\");
        assert_eq!(join_file("C:\\", "a.txt"), "C:\\a.txt");
        assert_eq!(folder_name("Z:\\sys\\bin\\"), "bin");
        assert_eq!(folder_name("C:\\"), "C:");
    }

    #[test]
    fn folders_first_then_names_without_case() {
        let mut v = vec![
            item("b.txt", false),
            item("System", true),
            item("A.txt", false),
            item("private", true),
            item("sys", true),
        ];
        sort(&mut v, Sort::default());
        assert_eq!(names(&v), ["private", "sys", "System", "A.txt", "b.txt"]);
    }

    #[test]
    fn sort_sheet() {
        let f = |name: &str, size: u64, modified: i64| Item {
            name: name.to_string(),
            size,
            modified,
            ..Item::default()
        };
        let mut v = vec![
            f("b.mp3", 3, 1),
            item("Music", true),
            f("a.txt", 1, 3),
            f("c.flac", 2, 2),
        ];
        let by = |key, ascending, folders_first| Sort {
            key,
            ascending,
            folders_first,
        };
        sort(&mut v, by(SortKey::Size, true, true));
        assert_eq!(names(&v), ["Music", "a.txt", "c.flac", "b.mp3"]);
        sort(&mut v, by(SortKey::Date, false, true));
        assert_eq!(names(&v), ["Music", "a.txt", "c.flac", "b.mp3"]);
        sort(&mut v, by(SortKey::Type, true, true));
        assert_eq!(names(&v), ["Music", "c.flac", "b.mp3", "a.txt"]);
        // folders mixed in: a folder's size is 0
        sort(&mut v, by(SortKey::Size, false, false));
        assert_eq!(names(&v), ["b.mp3", "c.flac", "a.txt", "Music"]);
        // names compare without case, Cyrillic too
        let mut w = vec![
            item("Яблоко", false),
            item("арбуз", false),
            item("Ёж", false),
        ];
        sort(&mut w, Sort::default());
        assert_eq!(names(&w), ["арбуз", "Яблоко", "Ёж"]);
        assert_eq!(SortKey::from_index(2), SortKey::Size);
        assert_eq!(SortKey::from_index(9), SortKey::Name);
        assert_eq!(SortKey::Type.index(), 3);
    }

    #[test]
    fn hidden_and_unfinished_entries() {
        let mut h = item(".trash", true);
        h.att = HIDDEN | SYSTEM;
        let v = vec![h, item("song.mp3", false), item("song.mp3.~part", false)];
        assert_eq!(names(&visible(v.clone(), false)), ["song.mp3"]);
        assert_eq!(names(&visible(v, true)), [".trash", "song.mp3"]);
    }

    #[test]
    fn crumbs_and_names() {
        let c = crumbs("E:\\Projects\\sketches\\");
        assert_eq!(
            c.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(),
            ["E:", "Projects", "sketches"]
        );
        assert_eq!(c[1].1, "E:\\Projects\\");
        assert_eq!(crumbs("C:\\").len(), 1);
        assert_eq!(split_name("live-take.flac"), ("live-take", ".flac"));
        assert_eq!(split_name("archive.tar.gz"), ("archive.tar", ".gz"));
        assert_eq!(split_name(".trash"), (".trash", ""));
        assert_eq!(split_name("README"), ("README", ""));
        assert_eq!(free_name(&[], "New file", ".txt"), "New file.txt");
        assert_eq!(
            free_name(&["new file.TXT", "New file (1).txt"], "New file", ".txt"),
            "New file (2).txt"
        );
        assert_eq!(
            free_name(&["New folder"], "New folder", ""),
            "New folder (1)"
        );
    }

    #[test]
    fn sizes() {
        assert_eq!(size_text(0), "0 B");
        assert_eq!(size_text(1023), "1023 B");
        assert_eq!(size_text(1024), "1.0 KB");
        assert_eq!(size_text(1536), "1.5 KB");
        assert_eq!(size_text(10 * 1024 - 1), "10 KB");
        assert_eq!(size_text(66_959_360), "64 MB");
        assert_eq!(size_text(3 << 30), "3 GB");
        assert_eq!(size_text((41 << 30) + (819 << 20)), "41.8 GB");
        assert_eq!(size_text(1229 << 20), "1.2 GB");
        assert_eq!(size_text(150 << 30), "150 GB");
    }

    #[test]
    fn dates() {
        // 2026-11-06 14:32:59 UTC: 20,763 days after 1970-01-01
        let t = ((DAYS_TO_UNIX + 20_763) * 86_400 + 14 * 3600 + 32 * 60 + 59) * 1_000_000;
        assert_eq!(date_text(t), "2026-11-06 14:32");
        assert_eq!(
            date_text(DAYS_TO_UNIX * 86_400 * 1_000_000),
            "1970-01-01 00:00"
        );
        // Symbian's anchor: 2000-01-01 is day 730,497 of TTime
        assert_eq!(date_text(730_497 * 86_400 * 1_000_000), "2000-01-01 00:00");
        let f = Item {
            name: "a".into(),
            size: 4_300_000,
            modified: t,
            ..Item::default()
        };
        assert_eq!(detail_text(&f, t), "4.1 MB · 6 Nov");
        let older = (DAYS_TO_UNIX + 17_929) * 86_400 * 1_000_000; // 2019-02-02
        assert_eq!(day_text(older, t), "2 Feb 2019");
        assert_eq!(detail_text(&item("d", true), t), "Folder");
        let mut albums = item("Albums", true);
        albums.count = Some(186);
        assert_eq!(detail_text(&albums, t), "186 items");
        albums.count = Some(1);
        assert_eq!(detail_text(&albums, t), "1 item");
    }

    #[test]
    fn drives() {
        assert_eq!(drive_label('Z', 0x12, 7), "ROM");
        assert_eq!(drive_label('E', 0x11, 9), "Mass memory");
        assert_eq!(drive_label('C', 0x11, 5), "Phone memory");
        assert_eq!(drive_label('E', 0x21, 6), "Memory card");
        assert_eq!(drive_label('D', 0x11, 5), "RAM disk");
        assert_eq!(used_fraction(100, 25), 0.75);
        assert_eq!(used_fraction(0, 0), 0.0);
        assert_eq!(used_fraction(100, 200), 0.0);
    }

    #[test]
    fn previews() {
        assert_eq!(text_preview(b"a\r\nb\rc\n\td"), "a\nb\nc\n    d");
        assert_eq!(text_preview(&[0xEF, 0xBB, 0xBF, b'h', b'i']), "hi");
        assert_eq!(text_preview(&[0xFF, 0xFE, b'O', 0, b'K', 0]), "OK");
        assert_eq!(text_preview(&[b'a', 0xFF, b'b']), "a\u{FFFD}b");
        let big = vec![b'x'; PREVIEW_MAX + 10];
        let p = text_preview(&big);
        assert!(p.starts_with("xxx") && p.ends_with("(first 64 KB shown)"));
        assert_eq!(p.matches('x').count(), PREVIEW_MAX);
    }

    #[test]
    fn names_the_server_takes() {
        for ok in ["neo", "My Notes", "a.b.txt", "ünï"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "", ".", "..", "a\\b", "a:b", "x*", "q?", "trail.", "trail ", "a\u{7}",
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert!(!valid_name(&"n".repeat(257)));
    }

    #[test]
    fn errors() {
        assert_eq!(error_text(-21), "Read-only drive");
        assert_eq!(error_text(-14), "In use, or folder not empty");
        assert_eq!(error_text(-99), "Error -99");
    }
}

// ── FA3: ages, Recent, Favourites, Details (files2-recent, -bin, -details, -search-filters) ──────────────────────

/// A day in `TTime` units.
pub const DAY: i64 = 86_400_000_000;
/// How long the Recycle bin keeps things.
pub const BIN_DAYS: i64 = 30;
/// Recent keeps this many.
pub const RECENT_MAX: usize = 50;

fn day_no(t: i64) -> i64 {
    t.div_euclid(DAY) - DAYS_TO_UNIX
}

/// Days after 1970-01-01 of a civil date (Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn ttime_of_day(days: i64) -> i64 {
    (days + DAYS_TO_UNIX) * DAY
}

/// Recent's sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Age {
    Today,
    Yesterday,
    ThisWeek,
    Earlier,
}

impl Age {
    pub fn label(self) -> &'static str {
        match self {
            Age::Today => "Today",
            Age::Yesterday => "Yesterday",
            Age::ThisWeek => "This week",
            Age::Earlier => "Earlier",
        }
    }
}

pub fn age(t: i64, now: i64) -> Age {
    match day_no(now) - day_no(t) {
        i64::MIN..=0 => Age::Today,
        1 => Age::Yesterday,
        2..=6 => Age::ThisWeek,
        _ => Age::Earlier,
    }
}

const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];

/// Recent's middle part: `10:12` today and yesterday, `Sun` this week, else `3 Nov`.
pub fn when_text(t: i64, now: i64) -> String {
    match age(t, now) {
        Age::Today | Age::Yesterday => {
            let s = t.div_euclid(1_000_000).rem_euclid(86_400);
            format!("{:02}:{:02}", s / 3600, s / 60 % 60)
        }
        Age::ThisWeek => String::from(WEEKDAYS[day_no(t).rem_euclid(7) as usize]),
        Age::Earlier => day_text(t, now),
    }
}

/// The bin's rows: `Deleted today`, `Deleted 1 day ago`, `Deleted 9 days ago`.
pub fn deleted_text(t: i64, now: i64) -> String {
    match day_no(now) - day_no(t) {
        i64::MIN..=0 => String::from("Deleted today"),
        1 => String::from("Deleted 1 day ago"),
        n => format!("Deleted {n} days ago"),
    }
}

/// `3 days left` once three or fewer remain of [`BIN_DAYS`], else nothing.
pub fn left_text(deleted: i64, now: i64) -> Option<String> {
    let left = BIN_DAYS - (now - deleted).div_euclid(DAY);
    match left {
        i64::MIN..=1 => Some(String::from("1 day left")),
        2 | 3 => Some(format!("{left} days left")),
        _ => None,
    }
}

/// Details: `212 KB (217,088 bytes)`.
pub fn bytes_text(bytes: u64) -> String {
    let digits = format!("{bytes}");
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let unit = if bytes == 1 { "byte" } else { "bytes" };
    if bytes < 1024 {
        return format!("{grouped} {unit}");
    }
    format!("{} ({grouped} {unit})", size_text(bytes))
}

/// Details: `1 Nov 2026, 08:40`.
pub fn full_date_text(t: i64) -> String {
    let (y, m, d) = civil_from_days(day_no(t));
    let s = t.div_euclid(1_000_000).rem_euclid(86_400);
    format!(
        "{d} {} {y}, {:02}:{:02}",
        MONTHS[(m - 1) as usize],
        s / 3600,
        s / 60 % 60
    )
}

/// The search filter's Modified chips.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Modified {
    #[default]
    Any,
    Today,
    Week,
    Month,
    Year,
}

impl Modified {
    pub const ALL: [Modified; 5] = [
        Modified::Any,
        Modified::Today,
        Modified::Week,
        Modified::Month,
        Modified::Year,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Modified::Any => "Any time",
            Modified::Today => "Today",
            Modified::Week => "This week",
            Modified::Month => "This month",
            Modified::Year => "This year",
        }
    }

    /// The earliest `TTime` that counts: the start of today, of this week (Monday), of the month or the year.
    pub fn since(self, now: i64) -> Option<i64> {
        let today = day_no(now);
        let (y, _, d) = civil_from_days(today);
        let day = match self {
            Modified::Any => return None,
            Modified::Today => today,
            // 1970-01-01 was a Thursday: Monday is 3 days later in the cycle
            Modified::Week => today - (today - 4).rem_euclid(7),
            Modified::Month => today - (d as i64 - 1),
            Modified::Year => days_from_civil(y, 1, 1),
        };
        Some(ttime_of_day(day))
    }
}

/// The search filter's Size chips: `(low, high)` in bytes, both inclusive.
pub const SIZE_BANDS: [(&str, Option<(u64, u64)>); 4] = [
    ("Any", None),
    ("Under 1 MB", Some((0, (1 << 20) - 1))),
    ("1–100 MB", Some((1 << 20, 100 << 20))),
    ("Over 100 MB", Some(((100 << 20) + 1, u64::MAX))),
];

/// Files opened from Files, the newest first, without repeats (files-app.md: Files' own list).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recent {
    pub items: Vec<(String, i64)>,
}

impl Recent {
    pub fn add(&mut self, path: &str, t: i64) {
        self.items.retain(|(p, _)| !p.eq_ignore_ascii_case(path));
        self.items.insert(0, (String::from(path), t));
        self.items.truncate(RECENT_MAX);
    }

    pub fn remove(&mut self, path: &str) {
        self.items.retain(|(p, _)| !p.eq_ignore_ascii_case(path));
    }

    /// One `TTime<TAB>path` line each.
    pub fn encode(&self) -> String {
        let mut s = String::new();
        for (p, t) in &self.items {
            s.push_str(&format!("{t}\t{p}\n"));
        }
        s
    }

    /// Lines that don't read are skipped.
    pub fn decode(text: &str) -> Recent {
        let mut r = Recent::default();
        for line in text.lines() {
            if let Some((t, p)) = line.split_once('\t')
                && let Ok(t) = t.parse()
                && !p.is_empty()
                && r.items.len() < RECENT_MAX
            {
                r.items.push((String::from(p), t));
            }
        }
        r
    }
}

/// Favourite folders in the user's order, one path per line.
pub fn favs_encode(favs: &[String]) -> String {
    let mut s = String::new();
    for f in favs {
        s.push_str(f);
        s.push('\n');
    }
    s
}

pub fn favs_decode(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.ends_with('\\'))
        .map(String::from)
        .collect()
}
