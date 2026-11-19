// SPDX-License-Identifier: Apache-2.0
//! Gzip (RFC 1952): a stream of one or more members read with each member's CRC and length checked, and a writer of
//! one member. A gzip declares no reliable size, so the reader takes the caller's limit (the free space).

use crate::deflate::Deflater;
use crate::inflate::{Inflate, Raw};
use crate::{At, Error, ReadAt, Result, Sink, Stream, le32, read_exact, tar};
use alloc::string::String;
use alloc::vec::Vec;

pub struct GzReader<S> {
    inf: Inflate<S>,
    crc: crc32fast::Hasher,
    /// This member's bytes so far.
    n: u64,
    total: u64,
    limit: u64,
    ended: bool,
    /// The first member's stored file name (FNAME), if any.
    pub name: Option<String>,
    /// Unix seconds (0: none stored).
    pub mtime: i64,
}

/// A member header; `None` at the end (nothing more, or not a gzip after the first member: padding).
fn header<S: Stream>(inf: &mut Inflate<S>, first: bool) -> Result<Option<(Option<String>, i64)>> {
    let mut s = Raw(inf);
    let mut h = [0u8; 10];
    if s.read(&mut h[..1])? == 0 {
        return if first {
            Err(Error::NotArchive)
        } else {
            Ok(None)
        };
    }
    if let Err(e) = read_exact(&mut s, &mut h[1..]) {
        // after a member, a few bytes of padding or garbage end the stream (as gzip ignores them)
        return if first { Err(e) } else { Ok(None) };
    }
    if h[0] != 0x1f || h[1] != 0x8b || h[2] != 8 {
        return if first {
            Err(Error::NotArchive)
        } else {
            Ok(None)
        };
    }
    let flg = h[3];
    let mtime = le32(&h, 4) as i64;
    let mut b = [0u8; 2];
    if flg & 4 != 0 {
        read_exact(&mut s, &mut b)?;
        s.skip(u16::from_le_bytes(b) as u64)?;
    }
    let mut zstr = |keep: bool| -> Result<Option<String>> {
        let mut v = Vec::new();
        loop {
            read_exact(&mut s, &mut b[..1])?;
            if b[0] == 0 {
                break;
            }
            if v.len() < 1024 {
                v.push(b[0]);
            }
        }
        // ISO 8859-1 by the RFC; UTF-8 in practice
        Ok(keep.then(|| match String::from_utf8(v) {
            Ok(s) => s,
            Err(e) => e.into_bytes().iter().map(|&c| c as char).collect(),
        }))
    };
    let name = if flg & 8 != 0 { zstr(true)? } else { None };
    if flg & 16 != 0 {
        zstr(false)?;
    }
    if flg & 2 != 0 {
        read_exact(&mut s, &mut b)?;
    }
    Ok(Some((name, mtime)))
}

impl<S: Stream> GzReader<S> {
    /// `limit`: more output than this is [`Error::TooBig`].
    pub fn new(src: S, limit: u64) -> Result<Self> {
        let mut inf = Inflate::new(src);
        let (name, mtime) = header(&mut inf, true)?.ok_or(Error::NotArchive)?;
        Ok(GzReader {
            inf,
            crc: crc32fast::Hasher::new(),
            n: 0,
            total: 0,
            limit,
            ended: false,
            name,
            mtime,
        })
    }

    /// Output so far.
    pub fn total(&self) -> u64 {
        self.total
    }
}

impl<S: Stream> Stream for GzReader<S> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        loop {
            if self.ended || buf.is_empty() {
                return Ok(0);
            }
            let n = self.inf.read(buf)?;
            if n > 0 {
                self.total += n as u64;
                if self.total > self.limit {
                    return Err(Error::TooBig);
                }
                self.crc.update(&buf[..n]);
                self.n += n as u64;
                return Ok(n);
            }
            let mut t = [0u8; 8];
            read_exact(&mut Raw(&mut self.inf), &mut t)?;
            let crc = core::mem::replace(&mut self.crc, crc32fast::Hasher::new()).finalize();
            if le32(&t, 0) as u32 != crc || le32(&t, 4) != self.n & 0xFFFF_FFFF {
                return Err(Error::Corrupt);
            }
            self.n = 0;
            self.inf.restart();
            if header(&mut self.inf, false)?.is_none() {
                self.ended = true;
            }
        }
    }
}

/// Whether a gzip holds a tar (its first 512 bytes are a tar header).
pub fn holds_tar<R: ReadAt + ?Sized>(r: &R) -> bool {
    let mut h = [0u8; 512];
    GzReader::new(At::all(r), 1 << 20)
        .is_ok_and(|mut g| read_exact(&mut g, &mut h).is_ok() && tar::is_header(&h))
}

/// One gzip member into `s`; [`finish`](GzWriter::finish) writes the trailer.
pub struct GzWriter<S: Sink> {
    s: S,
    d: Deflater,
    crc: crc32fast::Hasher,
    n: u64,
}

impl<S: Sink> GzWriter<S> {
    pub fn new(mut s: S, name: Option<&str>, mtime: i64) -> Result<Self> {
        let mut h = Vec::from(&[0x1f, 0x8b, 8, if name.is_some() { 8 } else { 0 }][..]);
        h.extend_from_slice(&(mtime.clamp(0, u32::MAX as i64) as u32).to_le_bytes());
        // no extra flags; OS 3 (Unix), as GNU tar writes
        h.extend_from_slice(&[0, 3]);
        if let Some(n) = name {
            h.extend_from_slice(n.as_bytes());
            h.push(0);
        }
        s.write(&h)?;
        Ok(GzWriter {
            s,
            d: Deflater::new(6),
            crc: crc32fast::Hasher::new(),
            n: 0,
        })
    }

    pub fn finish(mut self) -> Result<S> {
        self.d.finish(&mut self.s)?;
        let mut t = Vec::from(self.crc.finalize().to_le_bytes());
        t.extend_from_slice(&(self.n as u32).to_le_bytes());
        self.s.write(&t)?;
        Ok(self.s)
    }
}

impl<S: Sink> Sink for GzWriter<S> {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        self.crc.update(data);
        self.n += data.len() as u64;
        self.d.write(data, &mut self.s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn gz(data: &[u8], name: Option<&str>) -> Vec<u8> {
        let mut w = GzWriter::new(Vec::new(), name, 1_700_000_000).unwrap();
        w.write(data).unwrap();
        w.finish().unwrap()
    }

    fn gunzip(b: &[u8], limit: u64) -> Result<Vec<u8>> {
        let mut r = GzReader::new(At::all(b), limit)?;
        let mut out = Vec::new();
        let mut buf = vec![0u8; 7000];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                return Ok(out);
            }
            out.extend_from_slice(&buf[..n]);
        }
    }

    #[test]
    fn round_trip_and_members() {
        let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 253) as u8).collect();
        let one = gz(&data, Some("data.bin"));
        let r = GzReader::new(At::all(&one[..]), u64::MAX).unwrap();
        assert_eq!(
            (r.name.as_deref(), r.mtime),
            (Some("data.bin"), 1_700_000_000)
        );
        assert_eq!(gunzip(&one, u64::MAX).unwrap(), data);
        // two members, then zero padding
        let mut two = gz(b"hello ", None);
        two.extend(gz(b"world", None));
        two.extend([0u8; 16]);
        assert_eq!(gunzip(&two, u64::MAX).unwrap(), b"hello world");
    }

    #[test]
    fn limit_crc_and_truncation() {
        let data = vec![0u8; 5 << 20];
        let b = gz(&data, None);
        assert_eq!(gunzip(&b, 1 << 20).err(), Some(Error::TooBig));
        let mut bad = b.clone();
        let n = bad.len();
        bad[n - 8] ^= 1;
        assert_eq!(gunzip(&bad, u64::MAX).err(), Some(Error::Corrupt));
        assert_eq!(
            gunzip(&b[..b.len() / 2], u64::MAX).err(),
            Some(Error::Corrupt)
        );
        assert_eq!(
            gunzip(b"not a gzip", u64::MAX).err(),
            Some(Error::NotArchive)
        );
    }
}
