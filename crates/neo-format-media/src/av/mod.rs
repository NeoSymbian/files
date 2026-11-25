// SPDX-License-Identifier: Apache-2.0
//! Video files (files-app.md FA12): the containers (MP4, MOV, 3GP; Matroska, WebM) give each track's codec and its
//! packets in time order, read from the file in pieces; [`video`] and [`sound`] decode them.
//!
//! A broken file gives an [`Error`](crate::Error), never a panic.

pub mod mkv;
pub mod mp4;
pub mod sound;
pub mod video;
pub mod yuv;

use crate::{Error, ReadAt, Result};
use alloc::boxed::Box;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
}

/// The codecs FA12 plays, and the ones it names when it can't.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Vp8,
    Vp9,
    Aac,
    Vorbis,
    Opus,
    /// Known by name, not played: "Can't play this video: H.263 isn't supported".
    Other(&'static str),
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::Vp8 => "VP8",
            Codec::Vp9 => "VP9",
            Codec::Aac => "AAC",
            Codec::Vorbis => "Vorbis",
            Codec::Opus => "Opus",
            Codec::Other(n) => n,
        }
    }

    pub fn played(self) -> bool {
        !matches!(self, Codec::Other(_))
    }
}

/// One track of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub kind: Kind,
    pub codec: Codec,
    /// Video: the coded picture's size.
    pub width: u32,
    pub height: u32,
    /// Audio: samples per second and channels.
    pub rate: u32,
    pub channels: u8,
    /// The decoder's set-up bytes: H.264's avcC, AAC's AudioSpecificConfig, Vorbis's three headers (Matroska's
    /// Xiph-laced CodecPrivate), Opus's OpusHead; empty for VP8 and VP9.
    pub config: Vec<u8>,
}

/// One coded frame (video) or block of samples (audio).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    /// Index into [`Demux::tracks`].
    pub track: usize,
    /// When it shows or sounds, in microseconds from the start.
    pub pts_us: i64,
    /// A key frame: decoding may start here.
    pub key: bool,
    pub data: Vec<u8>,
}

/// A container read a piece at a time: tracks, then packets in time order.
pub trait Demux {
    fn tracks(&self) -> &[Track];
    /// The longest track's length.
    fn duration_us(&self) -> u64;
    /// The next packet of any track; `None` at the end.
    fn next(&mut self) -> Result<Option<Packet>>;
    /// Goes back to the video key frame at or before `us` (the start without video); returns its time. The packets
    /// from there on come next, every track's.
    fn seek(&mut self, us: u64) -> Result<u64>;
}

/// The first video track, and the first audio track.
pub fn pick(tracks: &[Track]) -> (Option<usize>, Option<usize>) {
    (
        tracks.iter().position(|t| t.kind == Kind::Video),
        tracks.iter().position(|t| t.kind == Kind::Audio),
    )
}

/// MP4 (`ftyp` at 4) or Matroska (the EBML magic at 0).
pub fn is_video(head: &[u8]) -> bool {
    head.get(4..8) == Some(b"ftyp") || head.starts_with(&[0x1a, 0x45, 0xdf, 0xa3])
}

/// The container by its first bytes.
pub fn open<'a, R: ReadAt + 'a>(r: R) -> Result<Box<dyn Demux + 'a>> {
    let mut head = [0u8; 12];
    let n = r.read_at(0, &mut head)?;
    let head = &head[..n];
    if head.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        Ok(Box::new(mkv::Mkv::open(r)?))
    } else if head.get(4..8) == Some(b"ftyp") {
        Ok(Box::new(mp4::Mp4::open(r)?))
    } else {
        Err(Error::NotAudio)
    }
}
