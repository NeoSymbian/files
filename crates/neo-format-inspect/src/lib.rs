// SPDX-License-Identifier: Apache-2.0
//! Inspectors for Files (files-app.md FA7): what a file is when Files has no app for it. Fonts (parsed by
//! `ttf-parser`, drawn by our own rasteriser in [`raster`]: no rasteriser crate builds for the device without std),
//! X.509 certificates (`x509-cert`, view only: nothing here verifies a signature), app packages (Symbian 9 .sis/.sisx,
//! Java .jad/.jar) and the hex rows of anything else.
//!
//! Every parser takes untrusted bytes: corrupt input gives an [`Error`], never a panic (the tests feed each one cut
//! and flipped fixtures in debug mode, so an overflow counts).
#![no_std]

extern crate alloc;

pub mod cert;
pub mod font;
pub mod hex;
pub mod jad;
pub mod raster;
pub mod sis;

use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The file ends before what it declares.
    Truncated,
    /// Not this kind of file, or a broken one: a short sentence for the user.
    Bad(&'static str),
    /// A kind we know but don't read: what it is ("a Symbian 8 package").
    Unsupported(&'static str),
}

pub type Result<T> = core::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => f.write_str("The file is cut short"),
            Error::Bad(why) => f.write_str(why),
            Error::Unsupported(what) => write!(f, "Files can't read {what}"),
        }
    }
}

/// Unix seconds from a civil date (UTC), Howard Hinnant's days_from_civil.
pub(crate) fn unix_time(y: i64, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86_400 + (hh * 3600 + mm * 60 + ss) as i64
}

#[inline]
pub(crate) fn le32(b: &[u8], i: usize) -> Result<u32> {
    let s = b
        .get(i..i.checked_add(4).ok_or(Error::Truncated)?)
        .ok_or(Error::Truncated)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
