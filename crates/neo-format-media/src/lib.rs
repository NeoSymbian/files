// SPDX-License-Identifier: Apache-2.0
//! Audio for Files (files-app.md FA9): what a file is ([`probe`], by its first bytes, not its name), its tags and a
//! decoder to interleaved 16-bit PCM, read from the file in pieces (a long recording is never in memory). The player
//! sends the PCM to NeoAudio, which takes S16LE at 8–96 kHz, mono or stereo; decoders give at most two channels.
//!
//! A broken file gives an [`Error`], never a panic.
#![no_std]

extern crate alloc;

#[cfg(feature = "video")]
pub mod av;
pub mod wav;

pub use neo_format_archive::ReadAt;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not audio we know.
    NotAudio,
    /// Audio we know, in a variant we don't play (a codec inside WAV, more bits than we read, …).
    Unsupported,
    /// Audio we know, broken.
    Corrupt,
    /// The file system's error code.
    Io(i32),
}

impl From<neo_format_archive::Error> for Error {
    fn from(e: neo_format_archive::Error) -> Self {
        match e {
            neo_format_archive::Error::Io(c) => Error::Io(c),
            _ => Error::Corrupt,
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// The containers and codecs FA9 knows by their first bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Wav,
    Mp3,
    Flac,
    /// Ogg: Vorbis or Opus inside.
    Ogg,
    /// AAC in ADTS frames (.aac).
    Aac,
    /// MP4 audio (.m4a: AAC inside).
    M4a,
    Amr,
    Midi,
}

impl Format {
    /// The name on the viewer's line ("MP3 · 320 kbps · …").
    pub fn name(self) -> &'static str {
        match self {
            Format::Wav => "WAV",
            Format::Mp3 => "MP3",
            Format::Flac => "FLAC",
            Format::Ogg => "Ogg",
            Format::Aac => "AAC",
            Format::M4a => "M4A",
            Format::Amr => "AMR",
            Format::Midi => "MIDI",
        }
    }
}

/// What the first bytes say (at least 12 are needed for every format; 64 is plenty).
pub fn probe(head: &[u8]) -> Option<Format> {
    let at = |i: usize, m: &[u8]| head.get(i..i + m.len()) == Some(m);
    if at(0, b"RIFF") && at(8, b"WAVE") {
        return Some(Format::Wav);
    }
    if at(0, b"fLaC") {
        return Some(Format::Flac);
    }
    if at(0, b"OggS") {
        return Some(Format::Ogg);
    }
    if at(0, b"#!AMR\n") {
        return Some(Format::Amr);
    }
    if at(0, b"MThd") {
        return Some(Format::Midi);
    }
    if at(4, b"ftyp") {
        return Some(Format::M4a);
    }
    if at(0, b"ID3") {
        return Some(Format::Mp3);
    }
    // a frame sync: 12 bits set; ADTS has layer 00, MPEG audio never does
    match head {
        [0xff, b, ..] if b & 0xf6 == 0xf0 => Some(Format::Aac),
        [0xff, b, ..] if b & 0xe0 == 0xe0 && b & 0x06 != 0 => Some(Format::Mp3),
        _ => None,
    }
}

/// The track's names and picture, where the file has them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tags {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// The cover's file (JPEG or PNG), for the picture decoder.
    pub cover: Option<Vec<u8>>,
}

/// What the decoder gives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// Frames a second.
    pub rate: u32,
    /// Channels of the output: 1 or 2.
    pub channels: u8,
    /// Frames in the track, 0 when unknown.
    pub frames: u64,
    /// The stream's bitrate in kbit/s (the file's own for PCM).
    pub kbps: u32,
}

impl Info {
    /// The track's length in ms, 0 when unknown.
    pub fn ms(&self) -> u64 {
        if self.rate == 0 {
            0
        } else {
            self.frames * 1000 / self.rate as u64
        }
    }
}

/// A codec's decoder over its file.
pub trait Decode {
    /// Interleaved samples into `out`: whole frames, 0 at the end.
    fn read(&mut self, out: &mut [i16]) -> Result<usize>;
    /// Go to `frame` (clamped to the end); the frame it is at now.
    fn seek(&mut self, frame: u64) -> Result<u64>;
}

/// An open audio file.
pub struct Track<'a> {
    pub format: Format,
    pub info: Info,
    pub tags: Tags,
    dec: Box<dyn Decode + 'a>,
}

impl Track<'_> {
    /// Interleaved samples into `out` (whole frames of [`Info::channels`]), 0 at the end.
    pub fn read(&mut self, out: &mut [i16]) -> Result<usize> {
        self.dec.read(out)
    }

    /// Go to `frame`; the frame it is at now.
    pub fn seek(&mut self, frame: u64) -> Result<u64> {
        self.dec.seek(frame)
    }
}

/// Opens `r` by what its first bytes say.
pub fn open<'a, R: ReadAt + 'a>(r: R) -> Result<Track<'a>> {
    let mut head = [0u8; 64];
    let n = r.read_at(0, &mut head)?;
    match probe(&head[..n]) {
        Some(Format::Wav) => {
            let (info, tags, dec) = wav::open(r)?;
            Ok(Track {
                format: Format::Wav,
                info,
                tags,
                dec: Box::new(dec),
            })
        }
        Some(_) => Err(Error::Unsupported),
        None => Err(Error::NotAudio),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_by_magic() {
        let wav = *b"RIFF\x24\0\0\0WAVEfmt ";
        assert_eq!(probe(&wav), Some(Format::Wav));
        assert_eq!(probe(b"ID3\x04\0\0\0\0\0\0"), Some(Format::Mp3));
        assert_eq!(probe(&[0xff, 0xfb, 0x90, 0x64]), Some(Format::Mp3));
        assert_eq!(probe(&[0xff, 0xf1, 0x50, 0x80]), Some(Format::Aac));
        assert_eq!(probe(&[0xff, 0xf9, 0x50, 0x80]), Some(Format::Aac));
        assert_eq!(probe(b"fLaC\0\0\0\x22"), Some(Format::Flac));
        assert_eq!(probe(b"OggS\0\x02"), Some(Format::Ogg));
        assert_eq!(probe(b"\0\0\0\x20ftypM4A "), Some(Format::M4a));
        assert_eq!(probe(b"#!AMR\n\x3c"), Some(Format::Amr));
        assert_eq!(probe(b"MThd\0\0\0\x06"), Some(Format::Midi));
        assert_eq!(probe(b"RIFF\0\0\0\0AVI "), None);
        assert_eq!(probe(&[0xff, 0xd8, 0xff]), None);
        assert_eq!(probe(&[]), None);
    }

    #[test]
    fn length_in_ms() {
        let i = Info {
            rate: 44_100,
            channels: 2,
            frames: 44_100 * 3 + 22_050,
            kbps: 1411,
        };
        assert_eq!(i.ms(), 3500);
        assert_eq!(Info::default().ms(), 0);
    }

    #[test]
    fn unknown_and_later_formats() {
        assert_eq!(open(alloc::vec![0u8; 100]).err(), Some(Error::NotAudio));
        assert_eq!(
            open(b"fLaC\0\0\0\x22".to_vec()).err(),
            Some(Error::Unsupported)
        );
    }
}
