// SPDX-License-Identifier: Apache-2.0
//! The video decoders, all pure Rust: H.264 (`rust_h264`, Baseline to High), VP8 (`vp8-decoder`), VP9
//! (`rusty_vp9`). Each gives 8-bit 4:2:0 pictures in display order with the packet's time.
//!
//! A damaged packet is skipped (its picture missing), not an error: playback goes on from the next good one.

use super::{Codec, Packet, Track};
use crate::{Error, Result};
use alloc::boxed::Box;
use alloc::vec::Vec;

/// One picture, 8-bit 4:2:0: the chroma planes are half as wide and high (rounded up).
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub y_stride: usize,
    pub uv_stride: usize,
    pub pts_us: i64,
}

impl Frame {
    pub fn chroma_width(&self) -> u32 {
        self.width.div_ceil(2)
    }

    pub fn chroma_height(&self) -> u32 {
        self.height.div_ceil(2)
    }
}

pub trait VideoDecode {
    /// Decodes one packet; the pictures due out (none, or several after a reorder) go to `out`.
    fn decode(&mut self, p: &Packet, out: &mut Vec<Frame>) -> Result<()>;
    /// At the end: the pictures still held for reordering.
    fn flush(&mut self, out: &mut Vec<Frame>);
    /// After a seek: forget the references; the next packet is a key frame.
    fn reset(&mut self);
}

/// The decoder for a video track; `Unsupported` for the codecs FA12 names but doesn't play.
pub fn decoder(t: &Track) -> Result<Box<dyn VideoDecode>> {
    match t.codec {
        Codec::H264 => Ok(Box::new(H264::new(&t.config)?)),
        Codec::Vp8 => Ok(Box::new(Vp8(vp8_decoder::Decoder::new()))),
        Codec::Vp9 => Ok(Box::new(Vp9(rusty_vp9::Vp9Decoder::new()))),
        _ => Err(Error::Unsupported),
    }
}

/// H.264 in `avcC` form: length-prefixed NAL units, the parameter sets in the track's config.
struct H264 {
    d: rust_h264::decoder::OrderedDecoder<i64>,
    config: Vec<u8>,
    len: usize,
}

impl H264 {
    fn new(config: &[u8]) -> Result<Self> {
        let len = rust_h264::nal::parse_avcc_config(config)
            .map_err(|_| Error::Corrupt)?
            .length_size;
        let mut h = Self {
            d: rust_h264::decoder::OrderedDecoder::new(),
            config: config.into(),
            len,
        };
        h.reset();
        Ok(h)
    }

    fn take(f: rust_h264::decoder::Frame, pts_us: i64) -> Option<Frame> {
        if f.bit_depth != 8 || f.chroma_format != rust_h264::decoder::ChromaFormat::Yuv420 {
            return None; // shortcut: 10-bit and 4:2:2 pictures are dropped; Files says "isn't supported" from the SPS later
        }
        let (y_stride, uv_stride) = (f.width as usize, f.chroma_width() as usize);
        Some(Frame {
            width: f.width,
            height: f.height,
            y: f.y,
            u: f.u,
            v: f.v,
            y_stride,
            uv_stride,
            pts_us,
        })
    }
}

impl VideoDecode for H264 {
    fn decode(&mut self, p: &Packet, out: &mut Vec<Frame>) -> Result<()> {
        for nal in rust_h264::nal::parse_avcc(&p.data, self.len) {
            if let Ok(frames) = self.d.decode_nal_with_meta(&nal, p.pts_us) {
                out.extend(frames.into_iter().filter_map(|(f, pts)| Self::take(f, pts)));
            }
        }
        Ok(())
    }

    fn flush(&mut self, out: &mut Vec<Frame>) {
        out.extend(
            self.d
                .flush_with_meta()
                .into_iter()
                .filter_map(|(f, pts)| Self::take(f, pts)),
        );
    }

    fn reset(&mut self) {
        let c = rust_h264::nal::parse_avcc_config(&self.config).ok();
        // The pictures held for display order: as many as the stream says it reorders (VUI), else the spec's worst
        // case. 16 always held a key frame's pictures back ~800 ms, so they came out late (Pixel, 2026-11-26).
        let depth = c
            .as_ref()
            .and_then(|c| c.sps_nals.first())
            .and_then(|n| rust_h264::nal::parse_sps_info(n).ok())
            .and_then(|i| i.max_reorder_frames)
            .map_or(16, |r| r as usize);
        self.d = rust_h264::decoder::OrderedDecoder::with_max_depth(depth);
        if let Some(c) = c {
            for nal in c.sps_nals.iter().chain(&c.pps_nals) {
                let _ = self.d.decode_nal_with_meta(nal, 0);
            }
        }
    }
}

struct Vp8(vp8_decoder::Decoder);

impl VideoDecode for Vp8 {
    fn decode(&mut self, p: &Packet, out: &mut Vec<Frame>) -> Result<()> {
        if let Ok(Some(pic)) = self.0.decode(&p.data) {
            out.push(Frame {
                width: pic.width,
                height: pic.height,
                y: pic.y.into(),
                u: pic.u.into(),
                v: pic.v.into(),
                y_stride: pic.y_stride,
                uv_stride: pic.uv_stride,
                pts_us: p.pts_us,
            });
        }
        Ok(())
    }

    fn flush(&mut self, _: &mut Vec<Frame>) {}

    fn reset(&mut self) {
        self.0.reset();
    }
}

struct Vp9(rusty_vp9::Vp9Decoder);

impl VideoDecode for Vp9 {
    fn decode(&mut self, p: &Packet, out: &mut Vec<Frame>) -> Result<()> {
        if self.0.push(&p.data, Some(p.pts_us)).is_err() {
            return Ok(());
        }
        self.drain(out);
        Ok(())
    }

    fn flush(&mut self, out: &mut Vec<Frame>) {
        self.0.flush();
        self.drain(out);
    }

    fn reset(&mut self) {
        self.0 = rusty_vp9::Vp9Decoder::new();
    }
}

impl Vp9 {
    fn drain(&mut self, out: &mut Vec<Frame>) {
        while let Ok(f) = self.0.next_frame() {
            if f.bit_depth != 8
                || f.subsampling_x != 1
                || f.subsampling_y != 1
                || f.planes.len() < 3
            {
                continue; // shortcut: 10-bit and 4:4:4 VP9 (profiles 1-3) are skipped
            }
            let mut planes = f.planes.into_iter();
            let (y, u, v) = (
                planes.next().unwrap_or_default(),
                planes.next().unwrap_or_default(),
                planes.next().unwrap_or_default(),
            );
            out.push(Frame {
                width: f.width,
                height: f.height,
                y,
                u,
                v,
                y_stride: f.strides[0],
                uv_stride: f.strides[1],
                pts_us: f.pts.unwrap_or(0),
            });
        }
    }
}
