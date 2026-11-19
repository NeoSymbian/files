// SPDX-License-Identifier: Apache-2.0
//! BMP, ours: 1, 4, 8, 16, 24 and 32 bits, uncompressed, RLE8 and bit fields; bottom-up or top-down.

use crate::{Error, Format, Info, Pixels, le16, le32};

struct Head {
    w: u32,
    h: u32,
    top_down: bool,
    bpp: u32,
    comp: u32,
    data: usize,
    palette: usize,
    /// Bytes a palette entry takes (3 for the old OS/2 header).
    entry: usize,
    colours: usize,
    masks: [u32; 4],
}

fn head(b: &[u8]) -> Option<Head> {
    let data = le32(b, 10)? as usize;
    let size = le32(b, 14)? as usize;
    let (w, h, bpp, comp, colours, entry) = if size == 12 {
        (le16(b, 18)?, le16(b, 20)? as i32, le16(b, 24)?, 0, 0, 3)
    } else if size >= 40 {
        (
            le32(b, 18)?,
            le32(b, 22)? as i32,
            le16(b, 28)?,
            le32(b, 30)?,
            le32(b, 46)? as usize,
            4,
        )
    } else {
        return None;
    };
    let mut masks = match bpp {
        16 => [0x7C00, 0x03E0, 0x001F, 0],
        _ => [0xFF_0000, 0xFF00, 0xFF, 0xFF00_0000],
    };
    let mut palette = 14 + size;
    if comp == 3 || comp == 6 {
        // bit fields: inside a v4/v5 header, or three (four) masks after a 40-byte one
        let at = if size >= 52 { 54 } else { 14 + size };
        for (k, m) in masks
            .iter_mut()
            .enumerate()
            .take(if comp == 6 || size >= 56 { 4 } else { 3 })
        {
            *m = le32(b, at + k * 4)?;
        }
        if size < 52 {
            palette += if comp == 6 { 16 } else { 12 };
        }
    }
    Some(Head {
        w,
        h: h.unsigned_abs(),
        top_down: h < 0,
        bpp,
        comp,
        data,
        palette,
        entry,
        colours,
        masks,
    })
}

pub fn probe(b: &[u8]) -> Option<Info> {
    let h = head(b)?;
    let mut i = Info::new(Format::Bmp, h.w, h.h);
    i.alpha = h.bpp == 32;
    Some(i)
}

/// A value under `mask`, scaled to 0–255.
fn channel(v: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 255;
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).count_ones();
    let x = (v & mask) >> shift;
    ((x * 255 + ((1 << bits) - 1) / 2) / ((1 << bits) - 1)) as u8
}

pub fn decode(b: &[u8]) -> Result<Pixels, Error> {
    let h = head(b).ok_or(Error::Corrupt)?;
    if h.w == 0 || h.h == 0 || h.w > 1 << 15 || h.h > 1 << 15 {
        return Err(Error::Corrupt);
    }
    let alpha = h.bpp == 32;
    let mut out = Pixels::new(h.w, h.h, if alpha { 4 } else { 3 });
    let n = if h.colours == 0 && h.bpp <= 8 {
        1 << h.bpp
    } else {
        h.colours
    };
    let pal = |i: usize| -> [u8; 3] {
        if i >= n {
            return [0; 3];
        }
        let at = h.palette + i * h.entry;
        match b.get(at..at + 3) {
            Some(p) => [p[2], p[1], p[0]],
            None => [0; 3],
        }
    };
    let row_of = |y: u32| if h.top_down { y } else { h.h - 1 - y };
    let c = out.ch as usize;
    let put = |out: &mut Pixels, x: u32, y: u32, p: [u8; 4]| {
        let o = (y as usize * h.w as usize + x as usize) * c;
        out.px[o..o + c].copy_from_slice(&p[..c]);
    };
    if h.comp == 1 && h.bpp == 8 {
        // RLE8: rows bottom-up, absent pixels stay black
        let (mut x, mut y, mut i) = (0u32, 0u32, h.data);
        while i + 1 < b.len() && y < h.h {
            let (a, v) = (b[i], b[i + 1]);
            i += 2;
            if a > 0 {
                let [r, g, bl] = pal(v as usize);
                for _ in 0..a {
                    if x < h.w {
                        put(&mut out, x, row_of(y), [r, g, bl, 255]);
                    }
                    x += 1;
                }
            } else {
                match v {
                    0 => {
                        x = 0;
                        y += 1;
                    }
                    1 => break,
                    2 => {
                        x += *b.get(i).ok_or(Error::Corrupt)? as u32;
                        y += *b.get(i + 1).ok_or(Error::Corrupt)? as u32;
                        i += 2;
                    }
                    n => {
                        for k in 0..n as usize {
                            let [r, g, bl] = pal(*b.get(i + k).ok_or(Error::Corrupt)? as usize);
                            if x < h.w {
                                put(&mut out, x, row_of(y), [r, g, bl, 255]);
                            }
                            x += 1;
                        }
                        i += (n as usize + 1) & !1;
                    }
                }
            }
        }
        return Ok(out);
    }
    if !(h.comp == 0 || h.comp == 3 || h.comp == 6) || !matches!(h.bpp, 1 | 4 | 8 | 16 | 24 | 32) {
        return Err(Error::Corrupt);
    }
    let stride = (h.w as usize * h.bpp as usize).div_ceil(32) * 4;
    let mut any_alpha = false;
    for y in 0..h.h {
        let at = h.data + y as usize * stride;
        let row = b.get(at..at + stride).ok_or(Error::Corrupt)?;
        for x in 0..h.w {
            let xi = x as usize;
            let p = match h.bpp {
                1 | 4 | 8 => {
                    let bit = xi * h.bpp as usize;
                    let byte = row[bit / 8];
                    let v = (byte >> (8 - h.bpp as usize - bit % 8)) & ((1u16 << h.bpp) - 1) as u8;
                    let [r, g, bl] = pal(v as usize);
                    [r, g, bl, 255]
                }
                24 => [row[xi * 3 + 2], row[xi * 3 + 1], row[xi * 3], 255],
                16 => {
                    let v = le16(row, xi * 2).unwrap_or(0);
                    let m = h.masks;
                    [channel(v, m[0]), channel(v, m[1]), channel(v, m[2]), 255]
                }
                _ => {
                    let v = le32(row, xi * 4).unwrap_or(0);
                    let m = h.masks;
                    let a = channel(v, m[3]);
                    any_alpha |= m[3] != 0 && a != 0;
                    [channel(v, m[0]), channel(v, m[1]), channel(v, m[2]), a]
                }
            };
            put(&mut out, x, row_of(y), p);
        }
    }
    // a 32-bit BMP whose alpha is all zero means opaque
    if alpha && !any_alpha {
        for p in out.px.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
    }
    Ok(out)
}
