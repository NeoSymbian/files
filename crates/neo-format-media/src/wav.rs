// SPDX-License-Identifier: Apache-2.0
//! WAV (RIFF/WAVE): PCM of 8, 16, 24 and 32 bits, 32/64-bit float, A-law and µ-law (voice recorders), plain or
//! WAVE_FORMAT_EXTENSIBLE; the names from a LIST/INFO chunk. More than two channels play as their first two (front
//! left and right).

use crate::{Decode, Error, Info, ReadAt, Result, Tags};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const PCM: u16 = 1;
const FLOAT: u16 = 3;
const ALAW: u16 = 6;
const MULAW: u16 = 7;
const EXTENSIBLE: u16 = 0xfffe;
/// Chunks walked before giving up on finding `data`.
const MAX_CHUNKS: usize = 1000;
/// A LIST chunk larger than this isn't read for names.
const MAX_LIST: u32 = 64 << 10;

pub struct Wav<R> {
    r: R,
    codec: u16,
    /// Bytes a sample of one channel.
    width: usize,
    /// Channels in the file.
    channels: usize,
    data: u64,
    frames: u64,
    at: u64,
    buf: Vec<u8>,
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn read_exact<R: ReadAt>(r: &R, pos: u64, buf: &mut [u8]) -> Result<()> {
    if r.read_at(pos, buf)? == buf.len() {
        Ok(())
    } else {
        Err(Error::Corrupt)
    }
}

/// INFO's text: Latin-1 or UTF-8, up to the first NUL.
fn text(b: &[u8]) -> String {
    let b = &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())];
    match core::str::from_utf8(b) {
        Ok(s) => String::from(s.trim()),
        Err(_) => b
            .iter()
            .map(|&c| c as char)
            .collect::<String>()
            .trim()
            .into(),
    }
}

fn info_tags(list: &[u8], tags: &mut Tags) {
    if list.get(..4) != Some(b"INFO") {
        return;
    }
    let mut i = 4;
    while i + 8 <= list.len() {
        let id = &list[i..i + 4];
        let len = u32_at(list, i + 4) as usize;
        let body = &list[i + 8..(i + 8).saturating_add(len).min(list.len())];
        match id {
            b"INAM" => tags.title = text(body),
            b"IART" => tags.artist = text(body),
            b"IPRD" => tags.album = text(body),
            _ => {}
        }
        i = i + 8 + len + (len & 1);
    }
}

/// The header, the names and the decoder.
pub fn open<R: ReadAt>(r: R) -> Result<(Info, Tags, Wav<R>)> {
    let size = r.size();
    let mut head = [0u8; 12];
    read_exact(&r, 0, &mut head)?;
    if &head[..4] != b"RIFF" || &head[8..] != b"WAVE" {
        return Err(Error::NotAudio);
    }
    let mut fmt: Option<[u8; 40]> = None;
    let mut tags = Tags::default();
    let mut pos = 12u64;
    let mut data = None;
    for _ in 0..MAX_CHUNKS {
        let mut ch = [0u8; 8];
        if pos + 8 > size {
            break;
        }
        read_exact(&r, pos, &mut ch)?;
        let len = u32_at(&ch, 4);
        let body = pos + 8;
        match &ch[..4] {
            b"fmt " => {
                if len < 16 {
                    return Err(Error::Corrupt);
                }
                let mut f = [0u8; 40];
                let n = (len as usize).min(40);
                read_exact(&r, body, &mut f[..n])?;
                fmt = Some(f);
            }
            b"LIST" if len <= MAX_LIST => {
                let mut l = vec![0u8; len as usize];
                let n = r.read_at(body, &mut l)?;
                info_tags(&l[..n], &mut tags);
            }
            b"data" => {
                // a streaming writer leaves the size 0 or all ones: the data runs to the end
                let rest = size.saturating_sub(body);
                let len = if len == 0 || len == u32::MAX {
                    rest
                } else {
                    (len as u64).min(rest)
                };
                data = Some((body, len));
                if fmt.is_some() {
                    break;
                }
            }
            _ => {}
        }
        pos = body + len as u64 + (len & 1) as u64;
    }
    let (f, (data, len)) = match (fmt, data) {
        (Some(f), Some(d)) => (f, d),
        _ => return Err(Error::Corrupt),
    };
    let mut codec = u16_at(&f, 0);
    let channels = u16_at(&f, 2) as usize;
    let rate = u32_at(&f, 4);
    let align = u16_at(&f, 12) as usize;
    let bits = u16_at(&f, 14);
    if codec == EXTENSIBLE {
        // the sub-format GUID starts with the plain codec
        codec = u16_at(&f, 24);
    }
    let width = (bits as usize).div_ceil(8);
    let ok = match codec {
        PCM => matches!(bits, 8 | 16 | 24 | 32),
        FLOAT => matches!(bits, 32 | 64),
        ALAW | MULAW => bits == 8,
        _ => return Err(Error::Unsupported),
    };
    if !ok {
        return Err(Error::Unsupported);
    }
    if channels == 0 || channels > 32 || rate == 0 || align != width * channels {
        return Err(Error::Corrupt);
    }
    let frames = len / align as u64;
    let out = channels.min(2) as u8;
    let info = Info {
        rate,
        channels: out,
        frames,
        kbps: ((rate as u64 * align as u64 * 8) / 1000) as u32,
    };
    Ok((
        info,
        tags,
        Wav {
            r,
            codec,
            width,
            channels,
            data,
            frames,
            at: 0,
            buf: Vec::new(),
        },
    ))
}

fn alaw(a: u8) -> i16 {
    let a = a ^ 0x55;
    let seg = (a >> 4) & 7;
    let mut t = ((a & 0x0f) as i16) << 4;
    t = match seg {
        0 => t + 8,
        1 => t + 0x108,
        _ => (t + 0x108) << (seg - 1),
    };
    if a & 0x80 != 0 { t } else { -t }
}

fn mulaw(u: u8) -> i16 {
    let u = !u;
    let t = ((((u & 0x0f) as i16) << 3) + 0x84) << ((u >> 4) & 7);
    if u & 0x80 != 0 { 0x84 - t } else { t - 0x84 }
}

impl<R: ReadAt> Wav<R> {
    fn sample(&self, s: &[u8]) -> i16 {
        match (self.codec, self.width) {
            (PCM, 1) => ((s[0] as i16) - 128) << 8,
            (PCM, 2) => i16::from_le_bytes([s[0], s[1]]),
            // the top 16 bits
            (PCM, 3) => i16::from_le_bytes([s[1], s[2]]),
            (PCM, _) => i16::from_le_bytes([s[2], s[3]]),
            (FLOAT, 4) => {
                let v = f32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                (v.clamp(-1.0, 1.0) * 32767.0) as i16
            }
            (FLOAT, _) => {
                let v = f64::from_le_bytes(s[..8].try_into().unwrap_or([0; 8]));
                (v.clamp(-1.0, 1.0) * 32767.0) as i16
            }
            (ALAW, _) => alaw(s[0]),
            _ => mulaw(s[0]),
        }
    }
}

impl<R: ReadAt> Decode for Wav<R> {
    fn read(&mut self, out: &mut [i16]) -> Result<usize> {
        let och = self.channels.min(2);
        let want = ((out.len() / och) as u64).min(self.frames - self.at) as usize;
        if want == 0 {
            return Ok(0);
        }
        let align = self.width * self.channels;
        self.buf.resize(want * align, 0);
        let got = self
            .r
            .read_at(self.data + self.at * align as u64, &mut self.buf)?;
        let n = got / align;
        for f in 0..n {
            let frame = &self.buf[f * align..(f + 1) * align];
            for c in 0..och {
                out[f * och + c] = self.sample(&frame[c * self.width..]);
            }
        }
        if n < want {
            // the file is shorter than its header said: stop there
            self.frames = self.at + n as u64;
        }
        self.at += n as u64;
        Ok(n * och)
    }

    fn seek(&mut self, frame: u64) -> Result<u64> {
        self.at = frame.min(self.frames);
        Ok(self.at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Format, Track};

    /// A WAV file: `fmt ` from the arguments, an optional LIST, then `data`.
    fn wav(
        codec: u16,
        channels: u16,
        rate: u32,
        bits: u16,
        data: &[u8],
        list: Option<&[u8]>,
    ) -> Vec<u8> {
        let align = channels * bits.div_ceil(8);
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&codec.to_le_bytes());
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&rate.to_le_bytes());
        fmt.extend_from_slice(&(rate * align as u32).to_le_bytes());
        fmt.extend_from_slice(&align.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        let mut body = Vec::new();
        let mut chunk = |id: &[u8], b: &[u8]| {
            body.extend_from_slice(id);
            body.extend_from_slice(&(b.len() as u32).to_le_bytes());
            body.extend_from_slice(b);
            if b.len() & 1 == 1 {
                body.push(0);
            }
        };
        chunk(b"fmt ", &fmt);
        if let Some(l) = list {
            chunk(b"LIST", l);
        }
        chunk(b"data", data);
        let mut f = Vec::new();
        f.extend_from_slice(b"RIFF");
        f.extend_from_slice(&(4 + body.len() as u32).to_le_bytes());
        f.extend_from_slice(b"WAVE");
        f.extend_from_slice(&body);
        f
    }

    fn all(t: &mut Track<'_>) -> Vec<i16> {
        let mut v = Vec::new();
        let mut b = [0i16; 6];
        loop {
            let n = t.read(&mut b).unwrap();
            if n == 0 {
                return v;
            }
            v.extend_from_slice(&b[..n]);
        }
    }

    #[test]
    fn pcm16_stereo() {
        let s: [i16; 6] = [1, -1, 1000, -1000, i16::MAX, i16::MIN];
        let data: Vec<u8> = s.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut t = crate::open(wav(PCM, 2, 44_100, 16, &data, None)).unwrap();
        assert_eq!(t.format, Format::Wav);
        assert_eq!(t.info.rate, 44_100);
        assert_eq!(t.info.channels, 2);
        assert_eq!(t.info.frames, 3);
        assert_eq!(t.info.kbps, 1411);
        assert_eq!(all(&mut t), s);
    }

    #[test]
    fn widths_and_laws() {
        let mut t = crate::open(wav(PCM, 1, 8000, 8, &[0, 128, 255], None)).unwrap();
        assert_eq!(all(&mut t), [-32768, 0, 32512]);
        // 24 bits: the top two bytes
        let mut t = crate::open(wav(
            PCM,
            1,
            48_000,
            24,
            &[0x11, 0x34, 0x12, 0xff, 0xff, 0x80],
            None,
        ))
        .unwrap();
        assert_eq!(all(&mut t), [0x1234, i16::from_le_bytes([0xff, 0x80])]);
        let f: Vec<u8> = [0.5f32, -1.5, 1.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let mut t = crate::open(wav(FLOAT, 1, 48_000, 32, &f, None)).unwrap();
        assert_eq!(all(&mut t), [16383, -32767, 32767]);
        let mut t = crate::open(wav(MULAW, 1, 8000, 8, &[0xff, 0x00, 0x7f, 0x80], None)).unwrap();
        assert_eq!(all(&mut t), [0, -32124, 0, 32124]);
        let mut t = crate::open(wav(ALAW, 1, 8000, 8, &[0xd5, 0x55, 0x2a, 0xaa], None)).unwrap();
        assert_eq!(all(&mut t), [8, -8, -32256, 32256]);
    }

    #[test]
    fn extensible_and_many_channels() {
        // WAVE_FORMAT_EXTENSIBLE, 4 channels: plays the first two
        let mut fmt = Vec::new();
        for v in [EXTENSIBLE, 4] {
            fmt.extend_from_slice(&v.to_le_bytes());
        }
        fmt.extend_from_slice(&48_000u32.to_le_bytes());
        fmt.extend_from_slice(&(48_000u32 * 8).to_le_bytes());
        for v in [8u16, 16, 22, 16] {
            fmt.extend_from_slice(&v.to_le_bytes());
        }
        fmt.extend_from_slice(&0x33u32.to_le_bytes());
        fmt.extend_from_slice(&PCM.to_le_bytes());
        fmt.extend_from_slice(&[0; 14]);
        let data: Vec<u8> = [1i16, 2, 3, 4, 5, 6, 7, 8]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let mut f = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        f.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        f.extend_from_slice(&fmt);
        f.extend_from_slice(b"data");
        f.extend_from_slice(&(data.len() as u32).to_le_bytes());
        f.extend_from_slice(&data);
        let mut t = crate::open(f).unwrap();
        assert_eq!(t.info.channels, 2);
        assert_eq!(all(&mut t), [1, 2, 5, 6]);
    }

    #[test]
    fn names_from_info() {
        let mut l = b"INFO".to_vec();
        for (id, v) in [
            (b"INAM", &b"Yerevan Nights\0"[..]),
            (b"IART", b"Lusine Trio"),
            (b"IPRD", b"Live\0"),
        ] {
            l.extend_from_slice(id);
            l.extend_from_slice(&(v.len() as u32).to_le_bytes());
            l.extend_from_slice(v);
            if v.len() & 1 == 1 {
                l.push(0);
            }
        }
        let t = crate::open(wav(PCM, 1, 8000, 16, &[0, 0], Some(&l))).unwrap();
        assert_eq!(t.tags.title, "Yerevan Nights");
        assert_eq!(t.tags.artist, "Lusine Trio");
        assert_eq!(t.tags.album, "Live");
    }

    #[test]
    fn seeks_by_frame() {
        let data: Vec<u8> = (0i16..10).flat_map(|v| v.to_le_bytes()).collect();
        let mut t = crate::open(wav(PCM, 1, 8000, 16, &data, None)).unwrap();
        assert_eq!(t.seek(7).unwrap(), 7);
        assert_eq!(all(&mut t), [7, 8, 9]);
        assert_eq!(t.seek(99).unwrap(), 10);
        assert_eq!(all(&mut t), []);
    }

    #[test]
    fn short_and_streamed_data() {
        // data's size says more than the file has: it plays what is there
        let mut f = wav(PCM, 1, 8000, 16, &[1, 0, 2, 0], None);
        let n = f.len();
        f[n - 8..n - 4].copy_from_slice(&1000u32.to_le_bytes());
        let mut t = crate::open(f.clone()).unwrap();
        assert_eq!(t.info.frames, 2);
        assert_eq!(all(&mut t), [1, 2]);
        // a streaming writer's size 0: to the end
        f[n - 8..n - 4].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(crate::open(f).unwrap().info.frames, 2);
    }

    #[test]
    fn broken_files_are_errors() {
        let good = wav(PCM, 2, 44_100, 16, &[0; 64], None);
        // every cut is an error or a shorter track, never a panic
        for n in 0..good.len() {
            if let Ok(mut t) = crate::open(good[..n].to_vec()) {
                all(&mut t);
            }
        }
        // and every flipped byte of the header
        for i in 0..44 {
            let mut f = good.clone();
            f[i] ^= 0xa5;
            if let Ok(mut t) = crate::open(f) {
                all(&mut t);
            }
        }
        assert_eq!(
            crate::open(wav(PCM, 0, 8000, 16, &[], None)).err(),
            Some(Error::Corrupt)
        );
        assert_eq!(
            crate::open(wav(PCM, 1, 8000, 12, &[], None)).err(),
            Some(Error::Unsupported)
        );
        assert_eq!(
            crate::open(wav(0x55, 1, 8000, 16, &[], None)).err(),
            Some(Error::Unsupported)
        );
        assert_eq!(
            crate::open(b"RIFF\0\0\0\0WAVE".to_vec()).err(),
            Some(Error::Corrupt)
        );
    }
}
