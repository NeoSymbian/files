// SPDX-License-Identifier: GPL-3.0-only
//! The Files app's screens (Slint, no_std): the generated component [`FilesApp`], its rows ([`DriveRow`],
//! [`FileRow`]) and the kit's globals it re-exports (`Theme`, `Kit`, `Scroll`). NEOFILES.EXE (symbian/app) and the
//! host snapshot tests drive the same UI and make its rows with the same functions ([`file_row`], [`drive_row`]).
#![no_std]

extern crate alloc;

use alloc::format;
use files_core::{self as model, Item};

slint::include_modules!();

pub mod image;
pub mod text;

/// `0xRRGGBB` as an opaque colour.
pub fn colour(rgb: u32) -> slint::Color {
    slint::Color::from_argb_encoded(0xff00_0000 | rgb)
}

/// A folder's row: its tile (neo-view's badge and family colour for a file), name and detail (`now` dates it).
pub fn file_row(item: &Item, now: i64, checked: bool) -> FileRow {
    let family = neo_view::family_of(&item.name);
    FileRow {
        name: item.name.as_str().into(),
        detail: model::detail_text(item, now).into(),
        short: if item.is_dir {
            model::detail_text(item, now).into()
        } else {
            model::size_text(item.size).into()
        },
        folder: item.is_dir,
        badge: neo_view::badge(&item.name).as_str().into(),
        colour: colour(family.colour()),
        checked,
        ..FileRow::default()
    }
}

/// A search hit (files2-search): the name split around the first match of `needle` (any case, any script), drawn
/// as name · hit (accent) · post.
pub fn hit_row(mut row: FileRow, needle: &str) -> FileRow {
    let name = alloc::string::String::from(row.name.as_str());
    let want = needle.to_lowercase();
    if want.is_empty() {
        return row;
    }
    for (i, _) in name.char_indices() {
        for (j, c) in name[i..].char_indices() {
            let end = i + j + c.len_utf8();
            let got = name[i..end].to_lowercase();
            if got == want {
                row.name = name[..i].into();
                row.hit = name[i..end].into();
                row.post = name[end..].into();
                return row;
            }
            if !want.starts_with(got.as_str()) {
                break;
            }
        }
    }
    row
}

/// The Types tiles' colours, in files-ops `Group` order (files2-types).
pub const GROUP_COLOURS: [u32; 8] = [
    0x22b46a, 0xf0662a, 0x8b5cf6, 0x3d8bfd, 0xb07a3a, 0x6366f1, 0x2f9bf0, 0x14b8a6,
];

/// A Types tile: `5.2 GB` and `1,038 files`.
pub fn type_tile(kind: usize, label: &str, files: u32, bytes: u64) -> TypeTile {
    TypeTile {
        label: label.into(),
        size: model::size_text(bytes).into(),
        files: if files == 1 {
            "1 file".into()
        } else {
            format!("{} files", thousands(files as u64)).into()
        },
        colour: colour(GROUP_COLOURS[kind % 8]),
        kind: kind as i32,
    }
}

/// `1,038`.
pub fn thousands(n: u64) -> alloc::string::String {
    let d = format!("{n}");
    let mut out = alloc::string::String::new();
    for (i, c) in d.chars().enumerate() {
        if i > 0 && (d.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The used bar of the Types card: one run per group with bytes (Downloads left out: its files are counted in their
/// own groups already), as shares of `total`.
pub fn used_bar(bytes: &[u64; 8], total: u64) -> alloc::vec::Vec<Seg> {
    let mut out = alloc::vec::Vec::new();
    let mut start = 0.0f32;
    for (k, &b) in bytes.iter().enumerate() {
        if k == 6 || b == 0 || total == 0 {
            continue;
        }
        let part = (b as f64 / total as f64) as f32;
        out.push(Seg {
            start,
            part,
            colour: colour(GROUP_COLOURS[k]),
        });
        start += part;
    }
    out
}

/// What a drive is drawn as (files2-spec-icons): `phone` C:, `card` a removable one, `rom` Z:, `ram`, else `mass`.
pub fn drive_kind(letter: char, read_only: bool, removable: bool, media: u32) -> &'static str {
    const MEDIA_RAM: u32 = 5;
    if read_only {
        "rom"
    } else if removable {
        "card"
    } else if letter == 'C' {
        "phone"
    } else if media == MEDIA_RAM {
        "ram"
    } else {
        "mass"
    }
}

/// A drive's row on the Browse tab (files2-browse): `C: Phone memory` · `8 GB`, `5.1 GB used · 2.9 GB free` and its bar;
/// a read-only one says so instead.
pub fn drive_row(letter: char, label: &str, kind: &str, size: u64, free: u64) -> DriveRow {
    let read_only = kind == "rom";
    let used = size.saturating_sub(free);
    DriveRow {
        title: format!("{letter}: {label}").into(),
        size: if read_only {
            "".into()
        } else {
            model::size_text(size).into()
        },
        detail: if read_only && size == 0 {
            // a ROFS volume reports no size
            "read-only · system files".into()
        } else if read_only {
            format!("{} · read-only · system files", model::size_text(size)).into()
        } else {
            format!(
                "{} used · {} free",
                model::size_text(used),
                model::size_text(free)
            )
            .into()
        },
        used: model::used_fraction(size as i64, free as i64),
        kind: kind.into(),
        read_only,
    }
}

/// A drive on Free up space (files2-cleanup): `E: Mass memory` · `12.4 GB of 41.8 GB`, its bar, `29.4 GB free` (C:
/// adds that apps and their data live there).
pub fn space_row(letter: char, label: &str, kind: &str, size: u64, free: u64) -> DriveRow {
    let used = size.saturating_sub(free);
    let apps = if kind == "phone" {
        " · apps and their data"
    } else {
        ""
    };
    DriveRow {
        title: format!("{letter}: {label}").into(),
        size: format!("{} of {}", model::size_text(used), model::size_text(size)).into(),
        detail: format!("{} free{apps}", model::size_text(free)).into(),
        used: model::used_fraction(size as i64, free as i64),
        kind: kind.into(),
        read_only: false,
    }
}
