// SPDX-License-Identifier: Apache-2.0
//! Matroska and WebM: an EBML reader over [`ReadAt`] for what playback needs. The headers (Info, Tracks, Cues; the
//! SeekHead says where Cues are when they come after the clusters), then the clusters' blocks one by one, laced
//! blocks split (Xiph, EBML and fixed lacing). Clusters of unknown size (live WebM) are read too.
//!
//! Seeking uses the Cues; without them, a key-frame index built from the block headers alone (the payloads are
//! skipped).

use super::{Codec, Demux, Kind, Packet, Track};
use crate::{Error, ReadAt, Result};
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const EBML: u32 = 0x1a45_dfa3;
const SEGMENT: u32 = 0x1853_8067;
const SEEK_HEAD: u32 = 0x114d_9b74;
const SEEK: u32 = 0x4dbb;
const SEEK_ID: u32 = 0x53ab;
const SEEK_POSITION: u32 = 0x53ac;
const INFO: u32 = 0x1549_a966;
const TIMESTAMP_SCALE: u32 = 0x2a_d7b1;
const DURATION: u32 = 0x4489;
const TRACKS: u32 = 0x1654_ae6b;
const TRACK_ENTRY: u32 = 0xae;
const TRACK_NUMBER: u32 = 0xd7;
const TRACK_TYPE: u32 = 0x83;
const CODEC_ID: u32 = 0x86;
const CODEC_PRIVATE: u32 = 0x63a2;
const CONTENT_ENCODINGS: u32 = 0x6d80;
const VIDEO: u32 = 0xe0;
const PIXEL_WIDTH: u32 = 0xb0;
const PIXEL_HEIGHT: u32 = 0xba;
const AUDIO: u32 = 0xe1;
const SAMPLING_FREQUENCY: u32 = 0xb5;
const CHANNELS: u32 = 0x9f;
const CUES: u32 = 0x1c53_bb6b;
const CUE_POINT: u32 = 0xbb;
const CUE_TIME: u32 = 0xb3;
const CUE_TRACK_POSITIONS: u32 = 0xb7;
const CUE_CLUSTER_POSITION: u32 = 0xf1;
const CLUSTER: u32 = 0x1f43_b675;
const TIMESTAMP: u32 = 0xe7;
const SIMPLE_BLOCK: u32 = 0xa3;
const BLOCK_GROUP: u32 = 0xa0;
const BLOCK: u32 = 0xa1;
const REFERENCE_BLOCK: u32 = 0xfb;

/// The most read for one header element (Tracks with a big CodecPrivate, Cues of a long film).
const HEADER_MAX: u64 = 16 << 20;
/// The largest block.
const BLOCK_MAX: u64 = 16 << 20;
/// Elements nested deeper than this are skipped (a hostile file can't make the reader recurse).
const DEPTH_MAX: usize = 8;

/// An element: its id, where its data starts, its data's length (`None`: unknown, to the parent's end).
#[derive(Clone, Copy, Debug)]
struct El {
    id: u32,
    data: u64,
    size: Option<u64>,
}

impl El {
    fn end(&self, parent_end: u64) -> u64 {
        self.size
            .map_or(parent_end, |s| self.data.saturating_add(s).min(parent_end))
    }
}

pub struct Mkv<R> {
    r: R,
    tracks: Vec<Track>,
    /// Matroska's track number for each of ours.
    numbers: Vec<u64>,
    /// Nanoseconds per timestamp tick.
    scale_ns: u64,
    duration_us: u64,
    segment: u64,
    segment_end: u64,
    first_cluster: u64,
    /// (time, cluster position) of the video key frames: from Cues, or found while reading.
    keys: Vec<(i64, u64)>,
    cued: bool,
    /// Where the next element is read.
    pos: u64,
    /// The cluster being read: its start, end and timestamp.
    cluster: Option<(u64, u64, i64)>,
    laced: VecDeque<Packet>,
}

impl<R: ReadAt> Mkv<R> {
    pub fn open(r: R) -> Result<Self> {
        let end = r.size();
        let head = el(&r, 0, end)?;
        if head.id != EBML {
            return Err(Error::NotAudio);
        }
        let seg = el(&r, head.end(end), end)?;
        if seg.id != SEGMENT {
            return Err(Error::Corrupt);
        }
        let segment_end = seg.end(end);
        let mut m = Self {
            r,
            tracks: Vec::new(),
            numbers: Vec::new(),
            scale_ns: 1_000_000,
            duration_us: 0,
            segment: seg.data,
            segment_end,
            first_cluster: segment_end,
            keys: Vec::new(),
            cued: false,
            pos: 0,
            cluster: None,
            laced: VecDeque::new(),
        };
        let mut cues_at = None;
        let mut duration = None;
        let mut at = seg.data;
        while at < segment_end {
            let e = el(&m.r, at, segment_end)?;
            match e.id {
                CLUSTER => {
                    m.first_cluster = at;
                    break;
                }
                INFO => {
                    let b = m.body(&e, segment_end)?;
                    for c in children(&b) {
                        match c.0 {
                            TIMESTAMP_SCALE => m.scale_ns = uint(c.1).max(1),
                            DURATION => duration = Some(float(c.1)),
                            _ => {}
                        }
                    }
                }
                TRACKS => {
                    let b = m.body(&e, segment_end)?;
                    for (id, entry) in children(&b) {
                        if id == TRACK_ENTRY
                            && let Some((n, t)) = track(entry)
                        {
                            m.numbers.push(n);
                            m.tracks.push(t);
                        }
                    }
                }
                SEEK_HEAD => {
                    let b = m.body(&e, segment_end)?;
                    for (id, seek) in children(&b) {
                        if id != SEEK {
                            continue;
                        }
                        let (mut what, mut where_) = (0, None);
                        for c in children(seek) {
                            match c.0 {
                                SEEK_ID => what = uint(c.1) as u32,
                                SEEK_POSITION => where_ = Some(uint(c.1)),
                                _ => {}
                            }
                        }
                        if what == CUES {
                            cues_at = where_.map(|p| seg.data.saturating_add(p));
                        }
                    }
                }
                CUES => cues_at = Some(at),
                _ => {}
            }
            let next = e.end(segment_end);
            if next <= at {
                break;
            }
            at = next;
        }
        if m.tracks.is_empty() {
            return Err(Error::NotAudio);
        }
        if let Some(d) = duration {
            m.duration_us = (d * m.scale_ns as f64 / 1000.0).max(0.0) as u64;
        }
        if let Some(c) = cues_at {
            let _ = m.read_cues(c); // without usable Cues, seeking scans
        }
        m.pos = m.first_cluster;
        Ok(m)
    }

    /// An element's data, if it is small enough to read whole.
    fn body(&self, e: &El, parent_end: u64) -> Result<Vec<u8>> {
        let len = e.end(parent_end) - e.data;
        if len > HEADER_MAX {
            return Err(Error::Unsupported);
        }
        let mut b = vec![0; len as usize];
        let n = self.r.read_at(e.data, &mut b)?;
        b.truncate(n);
        Ok(b)
    }

    fn read_cues(&mut self, at: u64) -> Result<()> {
        let e = el(&self.r, at, self.segment_end)?;
        if e.id != CUES {
            return Err(Error::Corrupt);
        }
        let video = self
            .tracks
            .iter()
            .position(|t| t.kind == Kind::Video)
            .map(|i| self.numbers[i]);
        let b = self.body(&e, self.segment_end)?;
        for (id, point) in children(&b) {
            if id != CUE_POINT {
                continue;
            }
            let mut time = None;
            let mut place = None;
            for (id, c) in children(point) {
                match id {
                    CUE_TIME => time = Some(uint(c) as i64),
                    CUE_TRACK_POSITIONS => {
                        let mut track = None;
                        let mut pos = None;
                        for (id, c) in children(c) {
                            match id {
                                0xf7 => track = Some(uint(c)),
                                CUE_CLUSTER_POSITION => pos = Some(uint(c)),
                                _ => {}
                            }
                        }
                        if video.is_none() || track == video {
                            place = place.or(pos);
                        }
                    }
                    _ => {}
                }
            }
            if let (Some(t), Some(p)) = (time, place) {
                self.keys.push((self.us(t), self.segment.saturating_add(p)));
            }
        }
        self.keys.sort_unstable();
        self.cued = !self.keys.is_empty();
        Ok(())
    }

    fn us(&self, ticks: i64) -> i64 {
        (i128::from(ticks) * i128::from(self.scale_ns) / 1000) as i64
    }

    /// The next block's header and where its frames are: (track index, pts, key, data start, data end, lacing).
    /// `None` at the end. With `skip`, nothing but block headers is read.
    fn next_block(&mut self) -> Result<Option<Block>> {
        loop {
            let (start, cend, ctime) = match self.cluster {
                Some(c) if self.pos < c.1 => c,
                _ => {
                    self.cluster = None;
                    if self.pos >= self.segment_end {
                        return Ok(None);
                    }
                    let e = el(&self.r, self.pos, self.segment_end)?;
                    let next = e.end(self.segment_end);
                    if e.id == CLUSTER {
                        self.cluster = Some((self.pos, next, 0));
                        self.pos = e.data;
                    } else if next > self.pos {
                        self.pos = next;
                    } else {
                        return Ok(None);
                    }
                    continue;
                }
            };
            let e = el(&self.r, self.pos, cend)?;
            let next = e.end(cend);
            if next <= self.pos {
                return Ok(None);
            }
            // an unknown-size cluster ends where the next top-level element starts
            if e.size.is_none() || matches!(e.id, CLUSTER | CUES | TRACKS | INFO) {
                self.cluster = None;
                continue;
            }
            self.pos = next;
            match e.id {
                TIMESTAMP => {
                    let mut b = [0u8; 8];
                    let len = (next - e.data).min(8) as usize;
                    self.r.read_at(e.data, &mut b[..len])?;
                    self.cluster = Some((start, cend, uint(&b[..len]) as i64));
                }
                SIMPLE_BLOCK => {
                    if let Some(b) = self.block(e.data, next, ctime, None, start)? {
                        return Ok(Some(b));
                    }
                }
                BLOCK_GROUP => {
                    let (mut block, mut referenced) = (None, false);
                    let mut at = e.data;
                    for _ in 0..DEPTH_MAX * 8 {
                        if at >= next {
                            break;
                        }
                        let c = el(&self.r, at, next)?;
                        match c.id {
                            BLOCK => block = Some((c.data, c.end(next))),
                            REFERENCE_BLOCK => referenced = true,
                            _ => {}
                        }
                        let n = c.end(next);
                        if n <= at {
                            break;
                        }
                        at = n;
                    }
                    if let Some((s, end)) = block
                        && let Some(b) = self.block(s, end, ctime, Some(!referenced), start)?
                    {
                        return Ok(Some(b));
                    }
                }
                _ => {}
            }
        }
    }

    /// A block's header at `at`: `None` for a track we don't play.
    fn block(
        &self,
        at: u64,
        end: u64,
        ctime: i64,
        key: Option<bool>,
        cluster: u64,
    ) -> Result<Option<Block>> {
        let mut h = [0u8; 12];
        let n = self.r.read_at(at, &mut h[..(end - at).min(12) as usize])?;
        let (number, len) = vint(&h[..n]).ok_or(Error::Corrupt)?;
        let Some(track) = self.numbers.iter().position(|&x| x == number) else {
            return Ok(None);
        };
        let h = &h[len..n];
        if h.len() < 3 {
            return Err(Error::Corrupt);
        }
        let rel = i16::from_be_bytes([h[0], h[1]]);
        let flags = h[2];
        let pts_us = self.us(ctime + i64::from(rel));
        Ok(Some(Block {
            track,
            pts_us,
            key: key.unwrap_or(flags & 0x80 != 0),
            data: at + len as u64 + 3,
            end,
            lacing: (flags >> 1) & 3,
            cluster,
        }))
    }

    fn read(&self, at: u64, end: u64) -> Result<Vec<u8>> {
        if end - at > BLOCK_MAX {
            return Err(Error::Unsupported);
        }
        let mut b = vec![0; (end - at) as usize];
        if self.r.read_at(at, &mut b)? < b.len() {
            return Err(Error::Corrupt);
        }
        Ok(b)
    }

    fn note_key(&mut self, b: &Block) {
        if !self.cued
            && b.key
            && self.tracks[b.track].kind == Kind::Video
            && self.keys.last().is_none_or(|&(t, _)| b.pts_us > t)
        {
            self.keys.push((b.pts_us, b.cluster));
        }
    }
}

struct Block {
    track: usize,
    pts_us: i64,
    key: bool,
    data: u64,
    end: u64,
    lacing: u8,
    cluster: u64,
}

impl<R: ReadAt> Demux for Mkv<R> {
    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn duration_us(&self) -> u64 {
        self.duration_us
    }

    fn next(&mut self) -> Result<Option<Packet>> {
        if let Some(p) = self.laced.pop_front() {
            return Ok(Some(p));
        }
        let Some(b) = self.next_block()? else {
            return Ok(None);
        };
        self.note_key(&b);
        let data = self.read(b.data, b.end)?;
        if b.lacing == 0 {
            return Ok(Some(Packet {
                track: b.track,
                pts_us: b.pts_us,
                key: b.key,
                data,
            }));
        }
        for frame in unlace(&data, b.lacing).ok_or(Error::Corrupt)? {
            self.laced.push_back(Packet {
                track: b.track,
                pts_us: b.pts_us,
                key: b.key,
                data: frame.into(),
            });
        }
        Ok(self.laced.pop_front())
    }

    fn seek(&mut self, to_us: u64) -> Result<u64> {
        let to = to_us.min(i64::MAX as u64) as i64;
        self.laced.clear();
        self.cluster = None;
        if !self.cued {
            // index the key frames up to the time from the block headers
            self.pos = self.keys.last().map_or(self.first_cluster, |k| k.1);
            while self.keys.last().is_none_or(|k| k.0 <= to) {
                let Some(b) = self.next_block()? else { break };
                self.note_key(&b);
                if b.pts_us > to && self.tracks[b.track].kind == Kind::Video {
                    break;
                }
            }
            self.cluster = None;
        }
        let k = self.keys.iter().rev().find(|k| k.0 <= to).copied();
        let (t, at) = k.unwrap_or((0, self.first_cluster));
        self.pos = at;
        // the cluster may start before the key frame: skip the blocks before it
        let video = self.tracks.iter().position(|t| t.kind == Kind::Video);
        if k.is_some() && video.is_some() {
            loop {
                let save = (self.pos, self.cluster);
                let Some(b) = self.next_block()? else { break };
                if Some(b.track) == video && b.key && b.pts_us >= t {
                    (self.pos, self.cluster) = save;
                    break;
                }
            }
        }
        Ok(t.max(0) as u64)
    }
}

/// The element at `at` (id and size), its data inside `parent_end`.
fn el<R: ReadAt + ?Sized>(r: &R, at: u64, parent_end: u64) -> Result<El> {
    let mut h = [0u8; 12];
    let n = r.read_at(at, &mut h)?;
    let h = &h[..n];
    let (id, ilen) = id(h).ok_or(Error::Corrupt)?;
    let (size, slen) = vint(&h[ilen..]).ok_or(Error::Corrupt)?;
    let unknown = size == (1u64 << (7 * slen as u32)) - 1;
    let data = at + (ilen + slen) as u64;
    if data > parent_end {
        return Err(Error::Corrupt);
    }
    Ok(El {
        id,
        data,
        size: (!unknown).then_some(size),
    })
}

/// An element id: its leading 1 bit kept (0x1A45DFA3).
fn id(b: &[u8]) -> Option<(u32, usize)> {
    let len = b.first()?.leading_zeros() as usize + 1;
    if len > 4 || b.len() < len {
        return None;
    }
    Some((
        b[..len].iter().fold(0u32, |a, &x| a << 8 | u32::from(x)),
        len,
    ))
}

/// A variable-length integer, its length marker removed.
fn vint(b: &[u8]) -> Option<(u64, usize)> {
    let first = *b.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || b.len() < len {
        return None;
    }
    let v = b[1..len]
        .iter()
        .fold(u64::from(first) & (0xff >> len), |a, &x| {
            a << 8 | u64::from(x)
        });
    Some((v, len))
}

fn uint(b: &[u8]) -> u64 {
    b.iter().take(8).fold(0, |a, &x| a << 8 | u64::from(x))
}

fn float(b: &[u8]) -> f64 {
    match b.len() {
        4 => f64::from(f32::from_be_bytes([b[0], b[1], b[2], b[3]])),
        8 => f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        _ => 0.0,
    }
}

/// The elements in a body read whole, as (id, data); a broken one ends the list.
fn children(b: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
    let mut at = 0;
    core::iter::from_fn(move || {
        let (id, ilen) = id(&b[at..])?;
        let (size, slen) = vint(&b[at + ilen..])?;
        let start = at + ilen + slen;
        let end = start.checked_add(usize::try_from(size).ok()?)?.min(b.len());
        at = end;
        Some((id, &b[start..end]))
    })
}

fn track(entry: &[u8]) -> Option<(u64, Track)> {
    let (mut number, mut kind, mut codec_id, mut private) = (0, 0, String::new(), Vec::new());
    let (mut width, mut height, mut rate, mut channels, mut encoded) = (0, 0, 0.0, 1, false);
    for (id, c) in children(entry) {
        match id {
            TRACK_NUMBER => number = uint(c),
            TRACK_TYPE => kind = uint(c),
            CODEC_ID => codec_id = String::from_utf8_lossy(c).into(),
            CODEC_PRIVATE => private = c.to_vec(),
            CONTENT_ENCODINGS => encoded = true,
            VIDEO => {
                for (id, c) in children(c) {
                    match id {
                        PIXEL_WIDTH => width = uint(c) as u32,
                        PIXEL_HEIGHT => height = uint(c) as u32,
                        _ => {}
                    }
                }
            }
            AUDIO => {
                for (id, c) in children(c) {
                    match id {
                        SAMPLING_FREQUENCY => rate = float(c),
                        CHANNELS => channels = uint(c) as u8,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let kind = match kind {
        1 => Kind::Video,
        2 => Kind::Audio,
        _ => return None, // subtitles and the rest
    };
    let codec = if encoded {
        // shortcut: header-stripping compression and encryption aren't undone
        Codec::Other("a compressed or encrypted track")
    } else {
        match codec_id.trim_end_matches('\0') {
            "V_MPEG4/ISO/AVC" => Codec::H264,
            "V_VP8" => Codec::Vp8,
            "V_VP9" => Codec::Vp9,
            "V_AV1" => Codec::Other("AV1"),
            "V_MPEGH/ISO/HEVC" => Codec::Other("HEVC"),
            "V_MPEG4/ISO/ASP" | "V_MPEG4/ISO/SP" | "V_MS/VFW/FOURCC" => {
                Codec::Other("MPEG-4 Part 2")
            }
            "V_MPEG2" => Codec::Other("MPEG-2"),
            c if c.starts_with("A_AAC") => Codec::Aac,
            "A_VORBIS" => Codec::Vorbis,
            "A_OPUS" => Codec::Opus,
            "A_MPEG/L3" => Codec::Other("MP3"),
            "A_AC3" | "A_EAC3" => Codec::Other("Dolby Digital"),
            "A_DTS" => Codec::Other("DTS"),
            "A_FLAC" => Codec::Other("FLAC"),
            _ => Codec::Other(if kind == Kind::Video {
                "an unknown codec"
            } else {
                "an unknown sound"
            }),
        }
    };
    let t = Track {
        kind,
        codec,
        width,
        height,
        rate: rate as u32,
        channels,
        config: private,
    };
    Some((number, t))
}

/// A laced block's frames: Xiph (1), fixed-size (2) or EBML (3) lacing.
fn unlace(d: &[u8], lacing: u8) -> Option<Vec<&[u8]>> {
    let count = usize::from(*d.first()?) + 1;
    let mut at = 1;
    let mut sizes = Vec::with_capacity(count);
    match lacing {
        1 => {
            for _ in 0..count - 1 {
                let mut s = 0usize;
                loop {
                    let b = *d.get(at)?;
                    at += 1;
                    s += usize::from(b);
                    if b != 255 {
                        break;
                    }
                }
                sizes.push(s);
            }
        }
        3 => {
            let (first, len) = vint(d.get(at..)?)?;
            at += len;
            let mut s = first as i64;
            sizes.push(usize::try_from(s).ok()?);
            for _ in 1..count - 1 {
                let (v, len) = vint(d.get(at..)?)?;
                at += len;
                // signed: the raw value minus half its range
                s += v as i64 - ((1i64 << (7 * len - 1)) - 1);
                sizes.push(usize::try_from(s).ok()?);
            }
        }
        2 => {
            let each = (d.len() - at) / count;
            sizes.resize(count - 1, each);
        }
        _ => return None,
    }
    let used: usize = sizes.iter().sum();
    let last = d.len().checked_sub(at)?.checked_sub(used)?;
    sizes.push(last);
    let mut out = Vec::with_capacity(count);
    for s in sizes {
        out.push(d.get(at..at + s)?);
        at += s;
    }
    Some(out)
}
