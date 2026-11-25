// SPDX-License-Identifier: Apache-2.0
//! The sound decoders: AAC-LC and Vorbis (symphonia's no_std branch), Opus (`opus-rs`). Each gives interleaved 16-bit
//! samples, at most two channels (more are mixed down), for NeoAudio.
//!
//! A damaged packet gives no samples, not an error: the sound skips and goes on.

use super::{Codec, Packet, Track};
use crate::{Error, Result};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia_core::packet::PacketRef;
use symphonia_core::units::{Duration, Timestamp};

pub trait SoundDecode {
    /// Samples per second of what [`SoundDecode::decode`] gives.
    fn rate(&self) -> u32;
    /// 1 or 2.
    fn channels(&self) -> u8;
    /// Decodes one packet, appending its samples to `out`.
    fn decode(&mut self, p: &Packet, out: &mut Vec<i16>) -> Result<()>;
    /// After a seek.
    fn reset(&mut self);
}

/// The decoder for a sound track; `Unsupported` for the codecs FA12 names but doesn't play (AMR, MP3 in MP4).
pub fn decoder(t: &Track) -> Result<Box<dyn SoundDecode>> {
    match t.codec {
        Codec::Aac | Codec::Vorbis => Ok(Box::new(Sym::new(t)?)),
        Codec::Opus => Ok(Box::new(Opus::new(t)?)),
        _ => Err(Error::Unsupported),
    }
}

struct Sym {
    d: Box<dyn AudioDecoder>,
    rate: u32,
    channels: usize,
    buf: Vec<i16>,
}

impl Sym {
    fn new(t: &Track) -> Result<Self> {
        use symphonia_core::codecs::audio::well_known::{CODEC_ID_AAC, CODEC_ID_VORBIS};
        let mut params = AudioCodecParameters::new();
        params.for_codec(if t.codec == Codec::Aac {
            CODEC_ID_AAC
        } else {
            CODEC_ID_VORBIS
        });
        if t.rate > 0 {
            params.with_sample_rate(t.rate);
        }
        if !t.config.is_empty() {
            params.with_extra_data(t.config.clone().into_boxed_slice());
        }
        let opts = AudioDecoderOptions::default();
        let d: Box<dyn AudioDecoder> = if t.codec == Codec::Aac {
            Box::new(
                symphonia_codec_aac::AacDecoder::try_new(&params, &opts)
                    .map_err(|_| Error::Unsupported)?,
            )
        } else {
            Box::new(
                symphonia_codec_vorbis::VorbisDecoder::try_new(&params, &opts)
                    .map_err(|_| Error::Corrupt)?,
            )
        };
        let p = d.codec_params();
        let rate = p.sample_rate.unwrap_or(t.rate);
        let channels = p
            .channels
            .as_ref()
            .map_or(t.channels as usize, |c| c.count())
            .max(1);
        if rate == 0 {
            return Err(Error::Corrupt);
        }
        Ok(Self {
            d,
            rate,
            channels,
            buf: Vec::new(),
        })
    }
}

impl SoundDecode for Sym {
    fn rate(&self) -> u32 {
        self.rate
    }

    fn channels(&self) -> u8 {
        self.channels.min(2) as u8
    }

    fn decode(&mut self, p: &Packet, out: &mut Vec<i16>) -> Result<()> {
        let packet = PacketRef::new(0, Timestamp::new(0), Duration::new(0), &p.data);
        let Ok(decoded) = self.d.decode_ref(&packet) else {
            return Ok(());
        };
        // the channel count can change mid-stream (AAC's implicit signalling); follow it
        let channels = decoded.spec().channels().count().max(1);
        self.channels = channels;
        self.buf.clear();
        decoded.copy_to_vec_interleaved(&mut self.buf);
        mix_down(&self.buf, channels, out);
        Ok(())
    }

    fn reset(&mut self) {
        self.d.reset();
    }
}

/// Opus: 48 kHz, its pre-skip dropped from the start (the encoder's look-ahead).
struct Opus {
    /// 175 KB without opus-rs's std-only `heap` feature: made in place on the heap, never on a thread's stack.
    d: Box<opus_rs::OpusDecoder>,
    channels: usize,
    pre_skip: usize,
    skip: usize,
    buf: Vec<f32>,
}

/// The longest Opus packet: 120 ms at 48 kHz.
const OPUS_MAX: usize = 5760;

impl Opus {
    fn new(t: &Track) -> Result<Self> {
        // OpusHead: "OpusHead", version, channels, pre-skip (LE16), rate, gain, mapping
        let head = &t.config;
        let (channels, pre_skip) = if head.len() >= 12 && head.starts_with(b"OpusHead") {
            (
                head[9] as usize,
                u16::from_le_bytes([head[10], head[11]]) as usize,
            )
        } else {
            (t.channels.max(1) as usize, 0)
        };
        if !(1..=2).contains(&channels) {
            return Err(Error::Unsupported); // shortcut: surround Opus (mapping family 1) isn't decoded
        }
        let d = opus_decoder(channels)?;
        Ok(Self {
            d,
            channels,
            pre_skip,
            skip: pre_skip,
            buf: vec![0.0; OPUS_MAX * 2],
        })
    }
}

impl SoundDecode for Opus {
    fn rate(&self) -> u32 {
        48_000
    }

    fn channels(&self) -> u8 {
        self.channels as u8
    }

    fn decode(&mut self, p: &Packet, out: &mut Vec<i16>) -> Result<()> {
        let Ok(n) = self.d.decode(&p.data, OPUS_MAX, &mut self.buf) else {
            return Ok(());
        };
        let samples = &self.buf[..(n * self.channels).min(self.buf.len())];
        let drop = (self.skip * self.channels).min(samples.len());
        self.skip -= drop / self.channels;
        out.extend(
            samples[drop..]
                .iter()
                .map(|&s| (s * 32767.0).clamp(-32768.0, 32767.0) as i16),
        );
        Ok(())
    }

    fn reset(&mut self) {
        if let Ok(d) = opus_decoder(self.channels) {
            self.d = d;
        }
        self.skip = self.pre_skip;
    }
}

fn opus_decoder(channels: usize) -> Result<Box<opus_rs::OpusDecoder>> {
    let mut d = Box::<opus_rs::OpusDecoder>::new_uninit();
    opus_rs::OpusDecoder::init(&mut d, 48_000, channels).map_err(|_| Error::Corrupt)?;
    // SAFETY: `init` returned Ok, so it wrote the whole decoder.
    Ok(unsafe { d.assume_init() })
}

/// Interleaved `channels`-channel samples to at most stereo: front left and right kept, the rest averaged in.
fn mix_down(s: &[i16], channels: usize, out: &mut Vec<i16>) {
    if channels <= 2 {
        out.extend_from_slice(s);
        return;
    }
    for f in s.chunks_exact(channels) {
        let rest: i32 =
            f[2..].iter().map(|&x| i32::from(x)).sum::<i32>() / (channels as i32 - 2) / 2;
        out.push((i32::from(f[0]) / 2 + rest).clamp(-32768, 32767) as i16);
        out.push((i32::from(f[1]) / 2 + rest).clamp(-32768, 32767) as i16);
    }
}
