// SPDX-License-Identifier: Apache-2.0
//! MP4, MOV, 3GP and M4V through `shiguredo_mp4`'s file demuxer, which asks for the byte ranges it needs (the `moov`
//! box, wherever it is) instead of doing I/O; each sample is then read at its offset. Fragmented MP4 is not read.

use super::{Codec, Demux, Kind, Packet, Track};
use crate::{Error, ReadAt, Result};
use alloc::vec;
use alloc::vec::Vec;
use core::time::Duration;
use shiguredo_mp4::boxes::SampleEntry;
use shiguredo_mp4::demux::{DemuxError, Mp4FileDemuxer, Sample};
use shiguredo_mp4::{BoxType, TrackKind};

/// The most of the file read for the headers (a two-hour film's `moov` is a few MB).
const HEADERS_MAX: u64 = 32 << 20;
/// The largest frame read (a 4K key frame is a few MB).
const SAMPLE_MAX: usize = 16 << 20;
/// Samples looked at for each track's first one (its codec) before giving up on it.
const FIRST_SCAN: usize = 20_000;

#[derive(Clone, Copy)]
struct Id {
    track: u32,
    scale: u32,
    /// The edit list's start in the media (B-frame delay, AAC priming), in the track's timescale.
    media_start: i64,
    /// Its empty edits before that: the track starts this late, in microseconds.
    delay_us: i64,
}

pub struct Mp4<R> {
    r: R,
    d: Mp4FileDemuxer,
    tracks: Vec<Track>,
    /// Our track's MP4 track id, timescale and edit-list shift, by index.
    ids: Vec<Id>,
    duration_us: u64,
}

impl From<DemuxError> for Error {
    fn from(_: DemuxError) -> Self {
        Error::Corrupt
    }
}

impl<R: ReadAt> Mp4<R> {
    pub fn open(r: R) -> Result<Self> {
        let mut d = Mp4FileDemuxer::new();
        feed(&r, &mut d)?;
        let infos: Vec<_> = d
            .tracks()?
            .iter()
            .filter(|t| matches!(t.kind, TrackKind::Video | TrackKind::Audio))
            .map(|t| {
                let (mut media_start, mut delay_us) = (0, 0);
                for e in t.edit_list.iter().flat_map(|l| &l.entries) {
                    if e.media_time < 0 {
                        delay_us += us(e.edit_duration as i64, t.movie_timescale.get());
                    } else {
                        media_start = e.media_time;
                        break;
                    }
                }
                let id = Id {
                    track: t.track_id,
                    scale: t.timescale.get(),
                    media_start,
                    delay_us,
                };
                (id, t.kind, t.duration)
            })
            .collect();
        let mut tracks: Vec<Option<Track>> = vec![None; infos.len()];
        let mut duration_us = 0;
        for (id, _, duration) in &infos {
            duration_us = duration_us
                .max((us(*duration as i64 - id.media_start, id.scale) + id.delay_us).max(0) as u64);
        }
        // the codec is in a track's first sample's entry
        for _ in 0..FIRST_SCAN {
            if tracks.iter().all(Option::is_some) {
                break;
            }
            let Some(s) = d.next_sample()? else { break };
            let id = s.track.track_id;
            if let (Some(i), Some(entry)) =
                (infos.iter().position(|t| t.0.track == id), s.sample_entry)
                && tracks[i].is_none()
            {
                let kind = if infos[i].1 == TrackKind::Video {
                    Kind::Video
                } else {
                    Kind::Audio
                };
                tracks[i] = Some(track(kind, entry));
            }
        }
        d.seek(Duration::ZERO)?;
        let (mut out, mut ids) = (Vec::new(), Vec::new());
        for (t, info) in tracks.into_iter().zip(&infos) {
            if let Some(t) = t {
                out.push(t);
                ids.push(info.0);
            }
        }
        if out.is_empty() {
            return Err(Error::NotAudio);
        }
        Ok(Self {
            r,
            d,
            tracks: out,
            ids,
            duration_us,
        })
    }
}

/// Our index of a sample's track, and its time in microseconds after the edit list.
fn place(ids: &[Id], s: &Sample) -> Option<(usize, i64)> {
    let i = ids.iter().position(|id| id.track == s.track.track_id)?;
    let id = ids[i];
    let t = s.timestamp as i64 + s.composition_time_offset.unwrap_or(0) - id.media_start;
    Some((i, us(t, id.scale) + id.delay_us))
}

impl<R: ReadAt> Demux for Mp4<R> {
    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn duration_us(&self) -> u64 {
        self.duration_us
    }

    fn next(&mut self) -> Result<Option<Packet>> {
        loop {
            let Some(s) = self.d.next_sample()? else {
                return Ok(None);
            };
            let Some((track, pts_us)) = place(&self.ids, &s) else {
                continue; // subtitles, timed text
            };
            if s.data_size > SAMPLE_MAX {
                return Err(Error::Unsupported);
            }
            let (offset, size, key) = (s.data_offset, s.data_size, s.keyframe);
            let mut data = vec![0; size];
            if self.r.read_at(offset, &mut data)? < size {
                return Err(Error::Corrupt);
            }
            return Ok(Some(Packet {
                track,
                pts_us,
                key,
                data,
            }));
        }
    }

    fn seek(&mut self, to_us: u64) -> Result<u64> {
        self.d.seek(Duration::from_micros(to_us))?;
        let Some(video) = self.tracks.iter().position(|t| t.kind == Kind::Video) else {
            return Ok(to_us);
        };
        // forward to the first video sample at or after the time
        loop {
            let Some(s) = self.d.next_sample()? else {
                return Ok(to_us);
            };
            if place(&self.ids, &s).is_some_and(|(i, _)| i == video) {
                break;
            }
        }
        // back over everything read, then on to the video key frame before it (the first back is that sample)
        loop {
            let Some(s) = self.d.prev_sample()? else {
                return Ok(0);
            };
            if let Some((i, pts)) = place(&self.ids, &s)
                && i == video
                && s.keyframe
            {
                return Ok(pts.max(0) as u64);
            }
        }
    }
}

/// Gives the demuxer the byte ranges it asks for until it has its headers.
fn feed<R: ReadAt>(r: &R, d: &mut Mp4FileDemuxer) -> Result<()> {
    while let Some(want) = d.required_input() {
        let size = want
            .size
            .map_or(r.size().saturating_sub(want.position), |n| n as u64);
        if size > HEADERS_MAX {
            return Err(Error::Unsupported);
        }
        let mut buf = vec![0; size as usize];
        let n = r.read_at(want.position, &mut buf)?;
        buf.truncate(n);
        d.handle_input(want.to_input(&buf));
        if d.last_error().is_some() {
            return Err(Error::Corrupt);
        }
    }
    Ok(())
}

fn us(t: i64, scale: u32) -> i64 {
    (i128::from(t) * 1_000_000 / i128::from(scale.max(1))) as i64
}

fn track(kind: Kind, e: &SampleEntry) -> Track {
    let mut t = Track {
        kind,
        codec: Codec::Other("an unknown codec"),
        width: 0,
        height: 0,
        rate: 0,
        channels: 0,
        config: Vec::new(),
    };
    match e {
        SampleEntry::Avc1(b) => {
            t.codec = Codec::H264;
            (t.width, t.height) = (b.visual.width.into(), b.visual.height.into());
            let c = &b.avcc_box;
            t.config = avcc(
                c.avc_profile_indication,
                c.profile_compatibility,
                c.avc_level_indication,
                c.length_size_minus_one.get(),
                &c.sps_list,
                &c.pps_list,
            );
        }
        SampleEntry::Vp08(b) => {
            t.codec = Codec::Vp8;
            (t.width, t.height) = (b.visual.width.into(), b.visual.height.into());
        }
        SampleEntry::Vp09(b) => {
            t.codec = Codec::Vp9;
            (t.width, t.height) = (b.visual.width.into(), b.visual.height.into());
        }
        SampleEntry::Hev1(_) | SampleEntry::Hvc1(_) => t.codec = Codec::Other("HEVC"),
        SampleEntry::Av01(_) => t.codec = Codec::Other("AV1"),
        SampleEntry::Mp4a(b) => {
            let dc = &b.esds_box.es.dec_config_descr;
            // 0x40 MPEG-4 audio, 0x66-0x68 MPEG-2 AAC; 0x69 and 0x6b are MP3
            t.codec = match dc.object_type_indication {
                0x40 | 0x66..=0x68 => Codec::Aac,
                0x69 | 0x6b => Codec::Other("MP3"),
                _ => Codec::Other("an unknown sound"),
            };
            t.config = dc
                .dec_specific_info
                .as_ref()
                .map(|i| i.payload.clone())
                .unwrap_or_default();
            (t.rate, t.channels) = (
                b.audio.samplerate.integer.into(),
                b.audio.channelcount as u8,
            );
        }
        SampleEntry::Opus(b) => {
            t.codec = Codec::Opus;
            let o = &b.dops_box;
            (t.rate, t.channels) = (48_000, o.output_channel_count);
            t.config = opus_head(o.output_channel_count, o.pre_skip, o.input_sample_rate);
        }
        SampleEntry::Flac(_) => t.codec = Codec::Other("FLAC"),
        SampleEntry::Unknown(b) => {
            t.codec = Codec::Other(match b.box_type {
                BoxType::Normal(n) => match &n {
                    b"mp4v" => "MPEG-4 Part 2",
                    b"s263" | b"h263" | b"H263" => "H.263",
                    b"samr" => "AMR",
                    b"sawb" => "AMR-WB",
                    b".mp3" => "MP3",
                    b"hvc1" | b"hev1" => "HEVC",
                    b"av01" => "AV1",
                    _ => "an unknown codec",
                },
                _ => "an unknown codec",
            })
        }
        _ => {}
    }
    t
}

/// An `avcC` record as the container stores it (Matroska's CodecPrivate is the same bytes).
fn avcc(
    profile: u8,
    compat: u8,
    level: u8,
    len_minus_one: u8,
    sps: &[Vec<u8>],
    pps: &[Vec<u8>],
) -> Vec<u8> {
    let mut v = vec![
        1,
        profile,
        compat,
        level,
        0xfc | (len_minus_one & 3),
        0xe0 | (sps.len().min(31) as u8),
    ];
    for s in sps.iter().take(31) {
        v.extend_from_slice(&(s.len() as u16).to_be_bytes());
        v.extend_from_slice(s);
    }
    v.push(pps.len().min(255) as u8);
    for p in pps.iter().take(255) {
        v.extend_from_slice(&(p.len() as u16).to_be_bytes());
        v.extend_from_slice(p);
    }
    v
}

/// The `OpusHead` an Ogg or Matroska file carries, from MP4's `dOps` (mapping family 0).
pub(crate) fn opus_head(channels: u8, pre_skip: u16, rate: u32) -> Vec<u8> {
    let mut v = b"OpusHead".to_vec();
    v.push(1);
    v.push(channels);
    v.extend_from_slice(&pre_skip.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes());
    v.push(0);
    v
}
