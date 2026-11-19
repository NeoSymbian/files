// SPDX-License-Identifier: Apache-2.0
//! WebP: the header by hand; the pixels with `image-webp` (NeoSymbian's no_std patch).

use crate::{Error, Format, Info, Pixels, le16, le32};
use image_webp::WebPDecoder;
use image_webp::io::Cursor;

fn le24(b: &[u8], i: usize) -> Option<u32> {
    le32(&[*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, 0], 0)
}

pub fn probe(b: &[u8]) -> Option<Info> {
    let (w, h, alpha, animated) = match b.get(12..16)? {
        b"VP8 " => {
            if b.get(23..26)? != [0x9D, 0x01, 0x2A] {
                return None;
            }
            (le16(b, 26)? & 0x3FFF, le16(b, 28)? & 0x3FFF, false, false)
        }
        b"VP8L" => {
            if *b.get(20)? != 0x2F {
                return None;
            }
            let v = le32(b, 21)?;
            (
                (v & 0x3FFF) + 1,
                ((v >> 14) & 0x3FFF) + 1,
                v >> 28 & 1 != 0,
                false,
            )
        }
        b"VP8X" => {
            let f = *b.get(20)?;
            (
                le24(b, 24)? + 1,
                le24(b, 27)? + 1,
                f & 0x10 != 0,
                f & 0x02 != 0,
            )
        }
        _ => return None,
    };
    let mut i = Info::new(Format::WebP, w, h);
    i.alpha = alpha;
    i.animated = animated;
    Some(i)
}

/// The image (an animation's first frame), RGB or RGBA.
pub fn decode(b: &[u8]) -> Result<Pixels, Error> {
    let mut d = WebPDecoder::new(Cursor::new(b)).map_err(|_| Error::Corrupt)?;
    let (w, h) = d.dimensions();
    let ch = if d.has_alpha() { 4 } else { 3 };
    let n = d.output_buffer_size().ok_or(Error::TooBig)?;
    let mut px = alloc::vec![0; n];
    d.read_image(&mut px).map_err(|_| Error::Corrupt)?;
    if n != w as usize * h as usize * ch {
        return Err(Error::Corrupt);
    }
    Ok(Pixels {
        w,
        h,
        ch: ch as u8,
        px,
    })
}
