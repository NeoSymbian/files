// SPDX-License-Identifier: Apache-2.0
//! Archives for Files (files-app.md FA6): list, read and write zip, gzip and tar, streamed so that a 1 GB archive is
//! never held in memory. Inflate and deflate are `miniz_oxide`'s; the containers are ours (the crates.io ones need
//! std).
//!
//! Every member name goes through [`name::safe`] once, when it is listed: a name that would land outside the
//! destination (zip-slip) becomes [`Kind::Unsafe`] and is never extracted. The bomb guards are a member never giving
//! more than it declares, [`MAX_MEMBERS`], and the caller's output limit for streams that declare nothing (gzip).
#![no_std]

extern crate alloc;

mod deflate;
pub mod gzip;
mod inflate;
pub mod name;
pub mod tar;
pub mod zip;

pub use inflate::Inflate;

use alloc::string::String;
use alloc::vec::Vec;

/// At most this many members are listed (a central directory or a tar can claim more).
pub const MAX_MEMBERS: usize = 200_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No archive we know.
    NotArchive,
    /// An archive we know, broken (or a member that gives more or less than it declares).
    Corrupt,
    /// Over a limit: [`MAX_MEMBERS`], the caller's output limit, or what the zip writer can describe.
    TooBig,
    /// The file system's error code.
    Io(i32),
}

pub type Result<T> = core::result::Result<T, Error>;

/// Random access to the archive file.
pub trait ReadAt {
    /// Bytes read at `pos`; fewer than `buf.len()` only at the end.
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> Result<usize>;
    fn size(&self) -> u64;
}

impl ReadAt for [u8] {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> Result<usize> {
        let start = (pos as usize).min(self.len());
        let n = buf.len().min(self.len() - start);
        buf[..n].copy_from_slice(&self[start..start + n]);
        Ok(n)
    }
    fn size(&self) -> u64 {
        self.len() as u64
    }
}

impl ReadAt for Vec<u8> {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> Result<usize> {
        self.as_slice().read_at(pos, buf)
    }
    fn size(&self) -> u64 {
        self.len() as u64
    }
}

/// Bytes in order: a member's data, a gzip stream, a tar.
pub trait Stream {
    /// Bytes read; 0 only at the end.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;

    /// Pass over `n` bytes (a tar member nobody wants).
    fn skip(&mut self, mut n: u64) -> Result<()> {
        let mut buf = [0u8; 4096];
        while n > 0 {
            let k = self.read(&mut buf[..n.min(4096) as usize])?;
            if k == 0 {
                return Err(Error::Corrupt);
            }
            n -= k as u64;
        }
        Ok(())
    }
}

/// Fill `buf` exactly, or `Corrupt` at an early end.
pub fn read_exact(s: &mut (impl Stream + ?Sized), buf: &mut [u8]) -> Result<()> {
    let mut at = 0;
    while at < buf.len() {
        let n = s.read(&mut buf[at..])?;
        if n == 0 {
            return Err(Error::Corrupt);
        }
        at += n;
    }
    Ok(())
}

/// A window `[pos, end)` of a [`ReadAt`], as a stream.
pub struct At<'r, R: ReadAt + ?Sized> {
    r: &'r R,
    pos: u64,
    end: u64,
}

impl<'r, R: ReadAt + ?Sized> At<'r, R> {
    pub fn new(r: &'r R, pos: u64, end: u64) -> Self {
        At { r, pos, end }
    }

    /// The whole file.
    pub fn all(r: &'r R) -> Self {
        At::new(r, 0, r.size())
    }

    pub fn pos(&self) -> u64 {
        self.pos
    }
}

impl<R: ReadAt + ?Sized> Stream for At<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = (buf.len() as u64).min(self.end.saturating_sub(self.pos)) as usize;
        if n == 0 {
            return Ok(0);
        }
        let got = self.r.read_at(self.pos, &mut buf[..n])?;
        self.pos += got as u64;
        Ok(got)
    }

    fn skip(&mut self, n: u64) -> Result<()> {
        if n > self.end - self.pos {
            return Err(Error::Corrupt);
        }
        self.pos += n;
        Ok(())
    }
}

/// Where written archives go, in order.
pub trait Sink {
    fn write(&mut self, data: &[u8]) -> Result<()>;
}

/// A [`Sink`] that can go back (the zip writer fills in a header once its member's size is known).
pub trait Patch: Sink {
    /// Bytes written so far.
    fn pos(&self) -> u64;
    /// Overwrite bytes already written.
    fn patch(&mut self, at: u64, data: &[u8]) -> Result<()>;
}

impl Sink for Vec<u8> {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        self.extend_from_slice(data);
        Ok(())
    }
}

impl Patch for Vec<u8> {
    fn pos(&self) -> u64 {
        self.len() as u64
    }
    fn patch(&mut self, at: u64, data: &[u8]) -> Result<()> {
        let at = at as usize;
        self.get_mut(at..at + data.len())
            .ok_or(Error::Corrupt)?
            .copy_from_slice(data);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    /// A link, device or FIFO (tar): listed, never extracted.
    Other,
    /// A name that would leave the destination (zip-slip), or one FAT can't hold: listed, never extracted.
    Unsafe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Stored,
    Deflated,
    /// A zip method we don't read (bzip2, LZMA, zstd…).
    Other(u16),
}

/// One thing in an archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// Safe (see [`name::safe`]): parts joined with `/`, no trailing `/` for a folder. As stored for [`Kind::Unsafe`].
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    /// Bytes in the archive (= `size` in a tar).
    pub packed: u64,
    pub method: Method,
    pub encrypted: bool,
    pub crc: u32,
    /// Unix seconds (0 when unknown).
    pub mtime: i64,
    /// A zip's local header, a tar's data.
    pub offset: u64,
}

impl Member {
    /// Whether extracting it can work: a safe file or folder, a method we read, not encrypted.
    pub fn extractable(&self) -> bool {
        matches!(self.kind, Kind::File | Kind::Dir)
            && !self.encrypted
            && !matches!(self.method, Method::Other(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Zip,
    Tar,
    TarGz,
    /// One gzipped file.
    Gz,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Zip => "zip",
            Format::Tar => "tar",
            Format::TarGz => "tar.gz",
            Format::Gz => "gz",
        }
    }
}

/// What the file is, from its first bytes (at least 512 for a tar) and then its name. A gzip is a tar.gz when its
/// name says so; [`gzip::holds_tar`] can look inside.
pub fn detect(head: &[u8], name: &str) -> Option<Format> {
    let lower =
        |s: &str| name.len() >= s.len() && name[name.len() - s.len()..].eq_ignore_ascii_case(s);
    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        Some(Format::Zip)
    } else if head.starts_with(&[0x1f, 0x8b]) {
        Some(if lower(".tar.gz") || lower(".tgz") {
            Format::TarGz
        } else {
            Format::Gz
        })
    } else if tar::is_header(head) {
        Some(Format::Tar)
    } else {
        None
    }
}

/// Unix seconds from a civil date (UTC).
pub(crate) fn unix_time(y: i64, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> i64 {
    // Howard Hinnant's days_from_civil
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86_400 + (hh * 3600 + mm * 60 + ss) as i64
}

/// The civil date of unix seconds: (y, m, d, hh, mm, ss).
pub(crate) fn civil(t: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d, secs / 3600, secs / 60 % 60, secs % 60)
}

#[inline]
pub(crate) fn le16(b: &[u8], i: usize) -> u64 {
    u16::from_le_bytes([b[i], b[i + 1]]) as u64
}

#[inline]
pub(crate) fn le32(b: &[u8], i: usize) -> u64 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as u64
}

#[inline]
pub(crate) fn le64(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap_or([0; 8]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        for t in [0i64, 951_782_400, 1_763_548_800, 4_102_444_799] {
            let (y, m, d, hh, mm, ss) = civil(t);
            assert_eq!(unix_time(y, m, d, hh, mm, ss), t);
        }
        assert_eq!(civil(951_782_400), (2000, 2, 29, 0, 0, 0));
    }
}
