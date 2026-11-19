// SPDX-License-Identifier: Apache-2.0
//! Zip: the central directory listed (Zip64 too), a member read as a stream (stored or deflated, its size and CRC
//! checked), and a writer (UTF-8 names, deflated or stored, no Zip64).

use crate::deflate::Deflater;
use crate::inflate::Inflate;
use crate::{
    At, Error, Kind, MAX_MEMBERS, Member, Method, Patch, ReadAt, Result, Stream, civil, le16, le32,
    le64, name, unix_time,
};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const LOC: u64 = 0x0403_4b50;
const CEN: u64 = 0x0201_4b50;
const EOCD: u64 = 0x0605_4b50;
const EOCD64: u64 = 0x0606_4b50;
const LOC64: u64 = 0x0706_4b50;
/// A central directory bigger than this isn't read (200,000 members with long names fit).
const MAX_CD: u64 = 64 << 20;
/// The largest offset, size or count a zip without Zip64 can hold.
const LIMIT: u64 = 0xFFFF_FFFE;

fn read_full<R: ReadAt + ?Sized>(r: &R, pos: u64, buf: &mut [u8]) -> Result<()> {
    let mut at = 0;
    while at < buf.len() {
        let n = r.read_at(pos + at as u64, &mut buf[at..])?;
        if n == 0 {
            return Err(Error::Corrupt);
        }
        at += n;
    }
    Ok(())
}

/// The members, in central-directory order.
pub fn list<R: ReadAt + ?Sized>(r: &R) -> Result<Vec<Member>> {
    let size = r.size();
    let tail_len = size.min(22 + 0xFFFF);
    if tail_len < 22 {
        return Err(Error::NotArchive);
    }
    let mut tail = vec![0u8; tail_len as usize];
    read_full(r, size - tail_len, &mut tail)?;
    // the last end record whose comment fits the file
    let e = (0..=tail.len() - 22)
        .rev()
        .find(|&i| le32(&tail, i) == EOCD && i + 22 + le16(&tail, i + 20) as usize <= tail.len())
        .ok_or(Error::NotArchive)?;
    let eocd = size - tail_len + e as u64;
    let (mut count, mut cd_size, mut cd_at) = (
        le16(&tail, e + 10),
        le32(&tail, e + 12),
        le32(&tail, e + 16),
    );
    if eocd >= 20 {
        let mut loc = [0u8; 20];
        read_full(r, eocd - 20, &mut loc)?;
        if le32(&loc, 0) == LOC64 {
            let mut z = [0u8; 56];
            read_full(r, le64(&loc, 8), &mut z)?;
            if le32(&z, 0) != EOCD64 {
                return Err(Error::Corrupt);
            }
            (count, cd_size, cd_at) = (le64(&z, 32), le64(&z, 40), le64(&z, 48));
        }
    }
    if count > MAX_MEMBERS as u64 || cd_size > MAX_CD {
        return Err(Error::TooBig);
    }
    if cd_at.checked_add(cd_size).is_none_or(|end| end > eocd) {
        return Err(Error::Corrupt);
    }
    let mut cd = vec![0u8; cd_size as usize];
    read_full(r, cd_at, &mut cd)?;
    // a central entry is at least 46 bytes: a count bigger than the directory holds is a lie
    let mut out = Vec::with_capacity((count as usize).min(cd.len() / 46));
    let mut i = 0usize;
    for _ in 0..count {
        if i + 46 > cd.len() || le32(&cd, i) != CEN {
            return Err(Error::Corrupt);
        }
        let flags = le16(&cd, i + 8);
        let method = le16(&cd, i + 10) as u16;
        let (time, date) = (le16(&cd, i + 12) as u32, le16(&cd, i + 14) as u32);
        let crc = le32(&cd, i + 16) as u32;
        let (mut packed, mut usize_, mut offset) =
            (le32(&cd, i + 20), le32(&cd, i + 24), le32(&cd, i + 42));
        let (n, m, k) = (
            le16(&cd, i + 28) as usize,
            le16(&cd, i + 30) as usize,
            le16(&cd, i + 32) as usize,
        );
        let ext = le32(&cd, i + 38);
        let end = i + 46 + n + m + k;
        if end > cd.len() {
            return Err(Error::Corrupt);
        }
        let raw = &cd[i + 46..i + 46 + n];
        let mut extra = &cd[i + 46 + n..i + 46 + n + m];
        let mut unicode = None;
        let mut mtime = dos_time(date, time);
        while extra.len() >= 4 {
            let (id, len) = (le16(extra, 0), le16(extra, 2) as usize);
            let Some(f) = extra.get(4..4 + len) else {
                break;
            };
            match id {
                0x0001 => {
                    let mut at = 0;
                    for v in [&mut usize_, &mut packed, &mut offset] {
                        if *v == 0xFFFF_FFFF {
                            if at + 8 > f.len() {
                                return Err(Error::Corrupt);
                            }
                            *v = le64(f, at);
                            at += 8;
                        }
                    }
                }
                // Info-ZIP Unicode path: version 1, the CRC of the stored name, the UTF-8 name
                0x7075 if f.len() > 5 && f[0] == 1 && le32(f, 1) as u32 == crc32fast::hash(raw) => {
                    unicode = core::str::from_utf8(&f[5..]).ok();
                }
                // extended timestamp: flags, then the modification time
                0x5455 if f.len() >= 5 && f[0] & 1 != 0 => mtime = le32(f, 1) as u32 as i32 as i64,
                _ => {}
            }
            extra = &extra[4 + len..];
        }
        let text: String = match unicode {
            Some(u) => u.into(),
            None => match core::str::from_utf8(raw) {
                // flag bit 11, or plain UTF-8 written without it (macOS, Python)
                Ok(s) => s.into(),
                Err(_) if flags & 0x800 != 0 => String::from_utf8_lossy(raw).into_owned(),
                Err(_) => raw.iter().map(|&b| cp437(b)).collect(),
            },
        };
        let dir = raw.last().is_some_and(|&b| b == b'/' || b == b'\\') || ext & 0x10 != 0;
        let (name, kind) = match name::safe(&text) {
            Some(s) => (s, if dir { Kind::Dir } else { Kind::File }),
            None => (text, Kind::Unsafe),
        };
        out.push(Member {
            name,
            kind,
            size: if dir { 0 } else { usize_ },
            packed,
            method: match method {
                0 => Method::Stored,
                8 => Method::Deflated,
                m => Method::Other(m),
            },
            encrypted: flags & 1 != 0,
            crc,
            mtime,
            offset,
        });
        i = end;
    }
    Ok(out)
}

fn dos_time(date: u32, time: u32) -> i64 {
    let (y, m, d) = (1980 + (date >> 9) as i64, (date >> 5) & 15, date & 31);
    if !(1..=12).contains(&m) || d == 0 {
        return 0;
    }
    unix_time(y, m, d, time >> 11, (time >> 5) & 63, (time & 31) * 2)
}

/// (date, time) as DOS stores them; before 1980 is 1980-01-01.
fn dos_of(t: i64) -> (u16, u16) {
    let (y, m, d, hh, mm, ss) = civil(t.max(315_532_800));
    let y = (y - 1980).min(127) as u32;
    (
        ((y << 9) | (m << 5) | d) as u16,
        ((hh << 11) | (mm << 5) | (ss / 2)) as u16,
    )
}

fn cp437(b: u8) -> char {
    const HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";
    if b < 0x80 {
        b as char
    } else {
        HIGH.chars().nth(b as usize - 0x80).unwrap_or('?')
    }
}

enum Body<R: ReadAt> {
    Stored(At<R>),
    Deflated(Inflate<At<R>>),
}

/// A member's data, checked: more than it declares, less, or a wrong CRC is [`Error::Corrupt`] (the bomb guard).
pub struct Reader<R: ReadAt> {
    body: Body<R>,
    crc: crc32fast::Hasher,
    out: u64,
    size: u64,
    want: u32,
}

/// Read `m` (a file from [`list`] that is [`Member::extractable`]).
pub fn open<R: ReadAt>(r: R, m: &Member) -> Result<Reader<R>> {
    if !m.extractable() || m.kind != Kind::File {
        return Err(Error::NotArchive);
    }
    let mut h = [0u8; 30];
    read_full(&r, m.offset, &mut h)?;
    if le32(&h, 0) != LOC {
        return Err(Error::Corrupt);
    }
    let data = m.offset + 30 + le16(&h, 26) + le16(&h, 28);
    let end = data.checked_add(m.packed).ok_or(Error::Corrupt)?;
    if end > r.size() {
        return Err(Error::Corrupt);
    }
    let at = At::new(r, data, end);
    let body = match m.method {
        Method::Stored if m.packed != m.size => return Err(Error::Corrupt),
        Method::Stored => Body::Stored(at),
        _ => Body::Deflated(Inflate::new(at)),
    };
    Ok(Reader {
        body,
        crc: crc32fast::Hasher::new(),
        out: 0,
        size: m.size,
        want: m.crc,
    })
}

impl<R: ReadAt> Stream for Reader<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = match &mut self.body {
            Body::Stored(s) => s.read(buf)?,
            Body::Deflated(s) => s.read(buf)?,
        };
        if n == 0 {
            if self.out != self.size || self.crc.clone().finalize() != self.want {
                return Err(Error::Corrupt);
            }
            return Ok(0);
        }
        self.out += n as u64;
        if self.out > self.size {
            return Err(Error::Corrupt);
        }
        self.crc.update(&buf[..n]);
        Ok(n)
    }
}

struct Cur {
    at: u64,
    data: u64,
    name: Vec<u8>,
    date: u16,
    time: u16,
    dir: bool,
    crc: crc32fast::Hasher,
    size: u64,
    deflate: Option<Deflater>,
}

/// Writes a zip into `w`: [`ZipWriter::start`] a member, [`write`](ZipWriter::write) its data, [`finish`]
/// (ZipWriter::finish) the archive.
pub struct ZipWriter<W: Patch> {
    w: W,
    cd: Vec<u8>,
    count: u64,
    cur: Option<Cur>,
}

impl<W: Patch> ZipWriter<W> {
    pub fn new(w: W) -> Self {
        ZipWriter {
            w,
            cd: Vec::new(),
            count: 0,
            cur: None,
        }
    }

    /// A folder (`name` without the trailing `/`).
    pub fn dir(&mut self, name: &str, mtime: i64) -> Result<()> {
        self.begin(name, mtime, false, true)?;
        self.end()
    }

    /// A file; `deflate` false stores it (already compressed: audio, video, photos, archives).
    pub fn start(&mut self, name: &str, mtime: i64, deflate: bool) -> Result<()> {
        self.begin(name, mtime, deflate, false)
    }

    fn begin(&mut self, name: &str, mtime: i64, deflate: bool, dir: bool) -> Result<()> {
        self.end()?;
        if self.count >= 0xFFFF || self.w.pos() > LIMIT {
            return Err(Error::TooBig);
        }
        let mut raw = Vec::from(name.as_bytes());
        if dir {
            raw.push(b'/');
        }
        if raw.len() > 0xFFFF {
            return Err(Error::TooBig);
        }
        let (date, time) = dos_of(mtime);
        let at = self.w.pos();
        let mut h = Vec::with_capacity(30 + raw.len());
        put32(&mut h, LOC as u32);
        put16(&mut h, 20);
        put16(&mut h, 0x800);
        put16(&mut h, if deflate { 8 } else { 0 });
        put16(&mut h, time);
        put16(&mut h, date);
        h.extend_from_slice(&[0; 12]);
        put16(&mut h, raw.len() as u16);
        put16(&mut h, 0);
        h.extend_from_slice(&raw);
        self.w.write(&h)?;
        self.cur = Some(Cur {
            at,
            data: self.w.pos(),
            name: raw,
            date,
            time,
            dir,
            crc: crc32fast::Hasher::new(),
            size: 0,
            deflate: deflate.then(|| Deflater::new(6)),
        });
        Ok(())
    }

    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        let Some(c) = self.cur.as_mut() else {
            return Err(Error::Corrupt);
        };
        c.crc.update(data);
        c.size += data.len() as u64;
        match c.deflate.as_mut() {
            Some(d) => d.write(data, &mut self.w),
            None => self.w.write(data),
        }
    }

    /// The member written so far is complete (also done by the next `start` and by `finish`).
    pub fn end(&mut self) -> Result<()> {
        let Some(mut c) = self.cur.take() else {
            return Ok(());
        };
        if let Some(d) = c.deflate.as_mut() {
            d.finish(&mut self.w)?;
        }
        let packed = self.w.pos() - c.data;
        if c.size > LIMIT || packed > LIMIT {
            return Err(Error::TooBig);
        }
        let crc = c.crc.finalize();
        let mut sizes = Vec::with_capacity(12);
        put32(&mut sizes, crc);
        put32(&mut sizes, packed as u32);
        put32(&mut sizes, c.size as u32);
        self.w.patch(c.at + 14, &sizes)?;
        let cd = &mut self.cd;
        put32(cd, CEN as u32);
        put16(cd, 20);
        put16(cd, 20);
        put16(cd, 0x800);
        put16(cd, if c.deflate.is_some() { 8 } else { 0 });
        put16(cd, c.time);
        put16(cd, c.date);
        cd.extend_from_slice(&sizes);
        put16(cd, c.name.len() as u16);
        cd.extend_from_slice(&[0; 8]);
        put32(cd, if c.dir { 0x10 } else { 0 });
        put32(cd, c.at as u32);
        cd.extend_from_slice(&c.name);
        self.count += 1;
        Ok(())
    }

    /// The central directory and the end record; the sink back.
    pub fn finish(mut self) -> Result<W> {
        self.end()?;
        let at = self.w.pos();
        if at > LIMIT || self.cd.len() as u64 > LIMIT {
            return Err(Error::TooBig);
        }
        self.w.write(&self.cd)?;
        let mut e = Vec::with_capacity(22);
        put32(&mut e, EOCD as u32);
        put32(&mut e, 0);
        put16(&mut e, self.count as u16);
        put16(&mut e, self.count as u16);
        put32(&mut e, self.cd.len() as u32);
        put32(&mut e, at as u32);
        put16(&mut e, 0);
        self.w.write(&e)?;
        Ok(self.w)
    }
}

fn put16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all<R: ReadAt>(r: R, m: &Member) -> Result<Vec<u8>> {
        let mut s = open(r, m)?;
        let mut out = Vec::new();
        let mut buf = [0u8; 1000];
        loop {
            let n = s.read(&mut buf)?;
            if n == 0 {
                return Ok(out);
            }
            out.extend_from_slice(&buf[..n]);
        }
    }

    #[test]
    fn writes_and_reads_back() {
        let text: Vec<u8> = (0..100_000u32)
            .flat_map(|i| (i % 251).to_le_bytes())
            .collect();
        let mut z = ZipWriter::new(Vec::new());
        z.dir("Docs", 1_763_548_800).unwrap();
        z.start("Docs/Ծրագիր.txt", 1_763_548_800, true).unwrap();
        z.write(&text[..5000]).unwrap();
        z.write(&text[5000..]).unwrap();
        z.start("song.mp3", 0, false).unwrap();
        z.write(b"ID3 not really").unwrap();
        z.start("empty", 0, true).unwrap();
        let bytes = z.finish().unwrap();
        let ms = list(&bytes).unwrap();
        assert_eq!(
            ms.iter()
                .map(|m| (m.name.as_str(), m.kind))
                .collect::<Vec<_>>(),
            [
                ("Docs", Kind::Dir),
                ("Docs/Ծրագիր.txt", Kind::File),
                ("song.mp3", Kind::File),
                ("empty", Kind::File)
            ]
        );
        assert_eq!(ms[1].mtime, 1_763_548_800);
        assert!(ms[1].packed < ms[1].size);
        assert_eq!(read_all(&bytes, &ms[1]).unwrap(), text);
        assert_eq!(ms[2].method, Method::Stored);
        assert_eq!(read_all(&bytes, &ms[2]).unwrap(), b"ID3 not really");
        assert_eq!(read_all(&bytes, &ms[3]).unwrap(), b"");
    }

    #[test]
    fn a_member_never_gives_more_than_it_declares() {
        let big = vec![0u8; 10 << 20];
        let mut z = ZipWriter::new(Vec::new());
        z.start("bomb", 0, true).unwrap();
        z.write(&big).unwrap();
        let mut bytes = z.finish().unwrap();
        let ms = list(&bytes).unwrap();
        // the central directory now says 1 KB
        let cd = bytes.len() - 22 - (46 + 4);
        bytes[cd + 24..cd + 28].copy_from_slice(&1024u32.to_le_bytes());
        let ms2 = list(&bytes).unwrap();
        assert_eq!(ms2[0].size, 1024);
        let mut s = open(&bytes, &ms2[0]).unwrap();
        let mut buf = vec![0u8; 64 * 1024];
        let mut got = 0u64;
        let err = loop {
            match s.read(&mut buf) {
                Ok(0) => break None,
                Ok(n) => got += n as u64,
                Err(e) => break Some(e),
            }
        };
        assert_eq!(err, Some(Error::Corrupt));
        assert!(got <= 1024);
        // and a wrong CRC fails at the end
        let mut bad = ms[0].clone();
        bad.crc ^= 1;
        assert_eq!(read_all(&bytes, &bad).err(), Some(Error::Corrupt));
    }

    #[test]
    fn cp437_names() {
        assert_eq!(cp437(0x80), 'Ç');
        assert_eq!(cp437(0xFF), '\u{a0}');
        assert_eq!(cp437(b'a'), 'a');
    }
}
