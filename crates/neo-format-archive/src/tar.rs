// SPDX-License-Identifier: Apache-2.0
//! Tar over any stream (a file, or a gzip): ustar and v7 headers, GNU long names, pax `path` and `size`; and a ustar
//! writer with a pax `path` for long names.

use crate::{Error, Kind, MAX_MEMBERS, Member, Method, Result, Sink, Stream, name, read_exact};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// A long name or pax record bigger than this is corrupt.
const MAX_META: u64 = 64 * 1024;

fn num(f: &[u8]) -> Option<u64> {
    if f[0] & 0x80 != 0 {
        // GNU base-256
        return f[1..].iter().try_fold((f[0] & 0x7f) as u64, |v, &b| {
            v.checked_mul(256).map(|v| v | b as u64)
        });
    }
    let t: &[u8] = f.split(|&b| b == 0).next().unwrap_or(&[]);
    let t = t.trim_ascii();
    t.iter().try_fold(0u64, |v, &b| {
        (b'0'..=b'7')
            .contains(&b)
            .then_some(())
            .and(v.checked_mul(8))
            .map(|v| v + (b - b'0') as u64)
    })
}

fn checksum_ok(h: &[u8]) -> bool {
    let Some(want) = num(&h[148..156]) else {
        return false;
    };
    let (mut u, mut s) = (0u64, 0i64);
    for (i, &b) in h[..512].iter().enumerate() {
        let b = if (148..156).contains(&i) { b' ' } else { b };
        u += b as u64;
        s += b as i8 as i64;
    }
    want == u || want as i64 == s
}

/// A tar header: its checksum adds up (and it has a name).
pub fn is_header(h: &[u8]) -> bool {
    h.len() >= 512 && h[0] != 0 && checksum_ok(h)
}

fn text(b: &[u8]) -> String {
    let b = b.split(|&c| c == 0).next().unwrap_or(&[]);
    match core::str::from_utf8(b) {
        Ok(s) => s.into(),
        Err(_) => b.iter().map(|&c| c as char).collect(),
    }
}

pub struct TarReader<S> {
    s: S,
    left: u64,
    pad: u64,
    /// Bytes of the tar consumed.
    pos: u64,
    count: usize,
    ended: bool,
}

impl<S: Stream> TarReader<S> {
    pub fn new(s: S) -> Self {
        TarReader {
            s,
            left: 0,
            pad: 0,
            pos: 0,
            count: 0,
            ended: false,
        }
    }

    /// Bytes of the tar read so far.
    pub fn pos(&self) -> u64 {
        self.pos
    }

    fn skip(&mut self, n: u64) -> Result<()> {
        self.s.skip(n)?;
        self.pos += n;
        Ok(())
    }

    fn data(&mut self, size: u64) -> Result<Vec<u8>> {
        if size > MAX_META {
            return Err(Error::Corrupt);
        }
        let mut v = vec![0u8; size as usize];
        read_exact(&mut self.s, &mut v)?;
        self.pos += size;
        self.skip((512 - size % 512) % 512)?;
        Ok(v)
    }

    /// The next member, its data ready for [`read`](Stream::read); `None` at the end.
    pub fn next_member(&mut self) -> Result<Option<Member>> {
        if self.ended {
            return Ok(None);
        }
        self.skip(self.left + self.pad)?;
        (self.left, self.pad) = (0, 0);
        let (mut long, mut pax_path, mut pax_size) = (None, None, None);
        let mut h = [0u8; 512];
        loop {
            // a tar that stops without its zero blocks ends there
            if self.s.read(&mut h[..1])? == 0 {
                self.ended = true;
                return Ok(None);
            }
            read_exact(&mut self.s, &mut h[1..])?;
            self.pos += 512;
            if h.iter().all(|&b| b == 0) {
                self.ended = true;
                return Ok(None);
            }
            if !checksum_ok(&h) {
                return Err(Error::Corrupt);
            }
            let size = match pax_size.take() {
                Some(s) => s,
                None => num(&h[124..136]).ok_or(Error::Corrupt)?,
            };
            match h[156] {
                b'L' => {
                    long = Some(text(&self.data(size)?));
                    continue;
                }
                b'x' => {
                    let d = self.data(size)?;
                    pax(&d, &mut pax_path, &mut pax_size);
                    continue;
                }
                b'K' | b'g' => {
                    self.data(size)?;
                    continue;
                }
                _ => {}
            }
            let raw = match pax_path.take().or(long.take()) {
                Some(p) => p,
                None if &h[257..262] == b"ustar" && h[345] != 0 => {
                    format!("{}/{}", text(&h[345..500]), text(&h[..100]))
                }
                None => text(&h[..100]),
            };
            let typ = h[156];
            let dir = typ == b'5' || (matches!(typ, 0 | b'0') && raw.ends_with('/'));
            let kind = if dir {
                Kind::Dir
            } else if matches!(typ, 0 | b'0' | b'7') {
                Kind::File
            } else {
                Kind::Other
            };
            self.count += 1;
            if self.count > MAX_MEMBERS {
                return Err(Error::TooBig);
            }
            (self.left, self.pad) = (size, (512 - size % 512) % 512);
            let (name, kind) = match name::safe(&raw) {
                Some(n) => (n, kind),
                None => (raw, Kind::Unsafe),
            };
            return Ok(Some(Member {
                name,
                kind,
                size: if dir { 0 } else { size },
                packed: size,
                method: Method::Stored,
                encrypted: false,
                crc: 0,
                mtime: num(&h[136..148]).unwrap_or(0) as i64,
                offset: self.pos,
            }));
        }
    }

    pub fn into_inner(self) -> S {
        self.s
    }
}

/// `len key=value\n` records: `path` and `size`.
fn pax(d: &[u8], path: &mut Option<String>, size: &mut Option<u64>) {
    let mut rest = d;
    while let Some(sp) = rest.iter().position(|&b| b == b' ') {
        let Some(len) = core::str::from_utf8(&rest[..sp])
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
        else {
            return;
        };
        if len <= sp + 1 || len > rest.len() {
            return;
        }
        let rec = &rest[sp + 1..len];
        let rec = rec.strip_suffix(b"\n").unwrap_or(rec);
        if let Some(eq) = rec.iter().position(|&b| b == b'=') {
            let (k, v) = (&rec[..eq], &rec[eq + 1..]);
            match k {
                b"path" => *path = Some(String::from_utf8_lossy(v).into_owned()),
                b"size" => *size = core::str::from_utf8(v).ok().and_then(|s| s.parse().ok()),
                _ => {}
            }
        }
        rest = &rest[len..];
    }
}

impl<S: Stream> Stream for TarReader<S> {
    /// The current member's data.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = (buf.len() as u64).min(self.left) as usize;
        if n == 0 {
            return Ok(0);
        }
        let k = self.s.read(&mut buf[..n])?;
        if k == 0 {
            return Err(Error::Corrupt);
        }
        self.left -= k as u64;
        self.pos += k as u64;
        Ok(k)
    }
}

/// Writes a ustar into `s`; every file's size is given before its data.
pub struct TarWriter<S: Sink> {
    s: S,
    left: u64,
    pad: u64,
}

fn octal(f: &mut [u8], v: u64) {
    let w = f.len() - 1;
    if v >= 1 << (3 * w) {
        // GNU base-256 (over 8 GB)
        f.fill(0);
        let n = f.len();
        for (i, b) in v.to_be_bytes().iter().rev().enumerate().take(n) {
            f[n - 1 - i] = *b;
        }
        f[0] |= 0x80;
        return;
    }
    let s = format!("{v:0w$o}");
    f[..w].copy_from_slice(s.as_bytes());
    f[w] = 0;
}

impl<S: Sink> TarWriter<S> {
    pub fn new(s: S) -> Self {
        TarWriter { s, left: 0, pad: 0 }
    }

    fn header(&mut self, name: &str, typ: u8, size: u64, mtime: i64, mode: u32) -> Result<()> {
        if name.len() > 100 {
            // a pax record: its length counts its own digits
            let body = format!(" path={name}\n");
            let mut len = body.len() + 1;
            while format!("{len}").len() + body.len() != len {
                len += 1;
            }
            let rec = format!("{len}{body}");
            self.header("PaxHeader", b'x', rec.len() as u64, mtime, 0o644)?;
            self.s.write(rec.as_bytes())?;
            self.s.write(&[0u8; 512][..(512 - rec.len() % 512) % 512])?;
        }
        let mut h = [0u8; 512];
        // the first 100 bytes, cut on a character boundary (the pax record has the whole name)
        let mut cut = name.len().min(100);
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        h[..cut].copy_from_slice(&name.as_bytes()[..cut]);
        octal(&mut h[100..108], mode as u64);
        octal(&mut h[108..116], 0);
        octal(&mut h[116..124], 0);
        octal(&mut h[124..136], size);
        octal(&mut h[136..148], mtime.max(0) as u64);
        h[148..156].fill(b' ');
        h[156] = typ;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u64 = h.iter().map(|&b| b as u64).sum();
        let s = format!("{sum:06o}");
        h[148..154].copy_from_slice(s.as_bytes());
        h[154] = 0;
        self.s.write(&h)
    }

    fn end(&mut self) -> Result<()> {
        if self.left != 0 {
            return Err(Error::Corrupt);
        }
        let pad = core::mem::take(&mut self.pad) as usize;
        self.s.write(&[0u8; 512][..pad])
    }

    pub fn dir(&mut self, name: &str, mtime: i64) -> Result<()> {
        self.end()?;
        self.header(&format!("{name}/"), b'5', 0, mtime, 0o755)
    }

    /// A file of exactly `size` bytes.
    pub fn start(&mut self, name: &str, size: u64, mtime: i64) -> Result<()> {
        self.end()?;
        self.header(name, b'0', size, mtime, 0o644)?;
        (self.left, self.pad) = (size, (512 - size % 512) % 512);
        Ok(())
    }

    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        if data.len() as u64 > self.left {
            return Err(Error::Corrupt);
        }
        self.left -= data.len() as u64;
        self.s.write(data)
    }

    /// The two zero blocks; the sink back.
    pub fn finish(mut self) -> Result<S> {
        self.end()?;
        self.s.write(&[0u8; 1024])?;
        Ok(self.s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::At;

    #[test]
    fn writes_and_reads_back() {
        let long = format!("{}/Ծրագիր.txt", "deep/".repeat(30).trim_end_matches('/'));
        let mut t = TarWriter::new(Vec::new());
        t.dir("Docs", 1_700_000_000).unwrap();
        t.start("Docs/a.txt", 5, 1_700_000_001).unwrap();
        t.write(b"hello").unwrap();
        t.start(&long, 3, 0).unwrap();
        t.write(b"abc").unwrap();
        t.start("empty", 0, 0).unwrap();
        let b = t.finish().unwrap();
        assert!(is_header(&b));
        let mut r = TarReader::new(At::all(&b[..]));
        let mut got = Vec::new();
        while let Some(m) = r.next_member().unwrap() {
            let mut d = vec![0u8; m.size as usize];
            read_exact(&mut r, &mut d).unwrap();
            got.push((m.name, m.kind, d, m.mtime));
        }
        assert_eq!(got.len(), 4);
        assert_eq!(got[0].0, "Docs");
        assert_eq!(got[0].1, Kind::Dir);
        assert_eq!(
            (got[1].0.as_str(), got[1].2.as_slice(), got[1].3),
            ("Docs/a.txt", &b"hello"[..], 1_700_000_001)
        );
        assert_eq!(
            (got[2].0.as_str(), got[2].2.as_slice()),
            (long.as_str(), &b"abc"[..])
        );
        assert_eq!(got[3].2, b"");
    }

    #[test]
    fn numbers() {
        assert_eq!(num(b"0000644\0"), Some(0o644));
        assert_eq!(num(b"   12 \0"), Some(0o12));
        assert_eq!(num(b"\0\0\0"), Some(0));
        assert_eq!(num(b"9\0"), None);
        let mut f = [0u8; 12];
        octal(&mut f, 9 << 30);
        assert_eq!(num(&f), Some(9 << 30));
        octal(&mut f, 1 << 40);
        assert_eq!(num(&f), Some(1 << 40));
    }
}
