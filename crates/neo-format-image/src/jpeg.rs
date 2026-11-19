// SPDX-License-Identifier: Apache-2.0
//! JPEG: our marker walk for the header and EXIF, `zune-jpeg` for the pixels.

use crate::{Error, Format, Info, Pixels, be16, exif};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// The largest side we decode (zune-jpeg's own limit is 16384 too).
const MAX_SIDE: usize = 16384;

pub fn probe(b: &[u8]) -> Option<Info> {
    let mut i = 2;
    let mut info: Option<Info> = None;
    let mut ex = None;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let m = b[i + 1];
        if m == 0xFF {
            i += 1; // fill byte
            continue;
        }
        if m == 0xD8 || (0xD0..=0xD7).contains(&m) || m == 0x01 {
            i += 2;
            continue;
        }
        if m == 0xD9 || m == 0xDA {
            break;
        }
        let len = be16(b, i + 2)? as usize;
        if len < 2 {
            return None;
        }
        let seg = b.get(i + 4..(i + 2 + len).min(b.len()))?;
        match m {
            0xE1 if ex.is_none() && seg.starts_with(b"Exif\0\0") => {
                ex = Some((i + 4 + 6, exif::parse(&seg[6..])));
            }
            0xC0..=0xCF if !matches!(m, 0xC4 | 0xC8 | 0xCC) => {
                let h = be16(seg, 1)?;
                let w = be16(seg, 3)?;
                let mut x = Info::new(Format::Jpeg, w, h);
                x.progressive = matches!(m, 0xC2 | 0xC6 | 0xCA | 0xCE);
                info = Some(x);
            }
            _ => {}
        }
        i += 2 + len;
    }
    let mut info = info?;
    if let Some((base, x)) = ex {
        info.orientation = x.orientation;
        info.taken = x.taken;
        info.thumb = x.thumb.map(|r| base + r.start..base + r.end);
    }
    Some(info)
}

pub fn decode(b: &[u8]) -> Result<Pixels, Error> {
    let opts = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_max_width(MAX_SIDE)
        .set_max_height(MAX_SIDE)
        .set_strict_mode(false);
    let mut d = JpegDecoder::new_with_options(ZCursor::new(b), opts);
    let px = d.decode().map_err(|_| Error::Corrupt)?;
    let (w, h) = d.dimensions().ok_or(Error::Corrupt)?;
    let ch = match d.output_colorspace() {
        Some(ColorSpace::RGB) => 3,
        Some(ColorSpace::RGBA) => 4,
        _ => return Err(Error::Corrupt),
    };
    if px.len() != w * h * ch {
        return Err(Error::Corrupt);
    }
    Ok(Pixels {
        w: w as u32,
        h: h as u32,
        ch: ch as u8,
        px,
    })
}
