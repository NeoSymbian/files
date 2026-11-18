// SPDX-License-Identifier: GPL-3.0-only
//! The Files app's screens (Slint, no_std): the generated component [`FilesApp`], its rows ([`DriveRow`],
//! [`FileRow`]) and the kit's globals it re-exports (`Theme`, `Kit`, `Scroll`). NEOFILES.EXE (symbian/app) and the
//! host snapshot tests drive the same UI and make its rows with the same functions ([`file_row`], [`drive_row`]).
#![no_std]

extern crate alloc;

use alloc::format;
use files_core::{self as model, Item};

slint::include_modules!();

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
        state: 0,
        progress: 0.0,
    }
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
        detail: if read_only {
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
