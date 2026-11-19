// SPDX-License-Identifier: Apache-2.0
//! PNG: the header by hand, the pixels with `zune-png` (16-bit stripped to 8, palettes expanded, grey widened to RGB).

use crate::{Error, Format, Info, Pixels, be32};
use alloc::vec::Vec;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_core::result::DecodingResult;
use zune_png::PngDecoder;

pub fn probe(b: &[u8]) -> Option<Info> {
    if b.get(12..16)? != b"IHDR" {
        return None;
    }
    let mut i = Info::new(Format::Png, be32(b, 16)?, be32(b, 20)?);
    let colour = *b.get(25)?;
    i.alpha = colour == 4 || colour == 6;
    // an acTL chunk before the first IDAT: an animated PNG (we show its default image)
    let mut at = 8;
    while let (Some(len), Some(kind)) = (be32(b, at), b.get(at + 4..at + 8)) {
        match kind {
            b"acTL" => i.animated = true,
            b"tRNS" => i.alpha = true,
            b"IDAT" => break,
            _ => {}
        }
        at += 12 + len as usize;
    }
    Some(i)
}

pub fn decode(b: &[u8]) -> Result<Pixels, Error> {
    let opts = DecoderOptions::default()
        .png_set_strip_to_8bit(true)
        .set_max_width(1 << 15)
        .set_max_height(1 << 15)
        .png_set_confirm_crc(false);
    let mut d = PngDecoder::new_with_options(ZCursor::new(b), opts);
    let px = match d.decode().map_err(|_| Error::Corrupt)? {
        DecodingResult::U8(v) => v,
        _ => return Err(Error::Corrupt),
    };
    let (w, h) = d.dimensions().ok_or(Error::Corrupt)?;
    let n = w * h;
    let (ch, px): (u8, Vec<u8>) = match d.colorspace().ok_or(Error::Corrupt)? {
        ColorSpace::RGB => (3, px),
        ColorSpace::RGBA => (4, px),
        ColorSpace::Luma => (3, px.iter().flat_map(|&g| [g, g, g]).collect()),
        ColorSpace::LumaA => (
            4,
            px.as_chunks::<2>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[0], p[0], p[1]])
                .collect(),
        ),
        _ => return Err(Error::Corrupt),
    };
    if px.len() != n * ch as usize {
        return Err(Error::Corrupt);
    }
    Ok(Pixels {
        w: w as u32,
        h: h as u32,
        ch,
        px,
    })
}
