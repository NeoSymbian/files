// SPDX-License-Identifier: Apache-2.0
//! Saving a copy: JPEG through `jpeg-encoder` (rows pulled one at a time), PNG through our chunk writer over a
//! streamed `miniz_oxide` deflate.

use crate::Error;
use crate::shrink::Rows;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use jpeg_encoder::{ColorType, Encoder, ImageBuffer, JpegColorType, rgb_to_ycbcr};
use miniz_oxide::deflate::core::{
    CompressorOxide, TDEFLFlush, TDEFLStatus, compress, create_comp_flags_from_zip_params,
};

struct Pull<'a, R: Rows> {
    src: &'a R,
    row: RefCell<Vec<u8>>,
}

impl<R: Rows> ImageBuffer for Pull<'_, R> {
    fn get_jpeg_color_type(&self) -> JpegColorType {
        JpegColorType::Ycbcr
    }
    fn width(&self) -> u16 {
        self.src.w() as u16
    }
    fn height(&self) -> u16 {
        self.src.h() as u16
    }
    fn fill_buffers(&self, y: u16, buffers: &mut [Vec<u8>; 4]) {
        let mut row = self.row.borrow_mut();
        self.src.row(y as u32, &mut row);
        let c = self.src.ch() as usize;
        for p in row.chunks_exact(c) {
            // over white where there is alpha (JPEG has none)
            let (r, g, b) = if c == 4 && p[3] != 255 {
                let a = p[3] as u32;
                let o = |v: u8| ((v as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
                (o(p[0]), o(p[1]), o(p[2]))
            } else {
                (p[0], p[1], p[2])
            };
            let (y, cb, cr) = rgb_to_ycbcr(r, g, b);
            buffers[0].push(y);
            buffers[1].push(cb);
            buffers[2].push(cr);
        }
    }
}

/// JPEG at `quality` (1–100), 4:2:0.
pub fn jpeg(src: &impl Rows, quality: u8) -> Result<Vec<u8>, Error> {
    if src.w() == 0 || src.h() == 0 || src.w() > 65535 || src.h() > 65535 {
        return Err(Error::TooBig);
    }
    let mut out = Vec::new();
    let pull = Pull {
        src,
        row: RefCell::new(vec![0; src.w() as usize * src.ch() as usize]),
    };
    Encoder::new(&mut out, quality)
        .encode_image(pull)
        .map_err(|_| Error::Corrupt)?;
    Ok(out)
}

/// A JPEG of whole pixels in memory (the thumbnails).
pub fn jpeg_rgb(px: &[u8], w: u16, h: u16, quality: u8) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    Encoder::new(&mut out, quality)
        .encode(px, w, h, ColorType::Rgb)
        .map_err(|_| Error::Corrupt)?;
    Ok(out)
}

const fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[n] = c;
        n += 1;
    }
    t
}

static CRC: [u32; 256] = crc_table();

fn crc(parts: &[&[u8]]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for p in parts {
        for &b in *p {
            c = CRC[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
    }
    c ^ 0xFFFF_FFFF
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc(&[kind, data]).to_be_bytes());
}

/// Deflate fed in pieces: zlib-wrapped (`zlib`) for PNG, raw for zip (FA6).
pub struct Deflate {
    c: alloc::boxed::Box<CompressorOxide>,
    buf: Vec<u8>,
    pub out: Vec<u8>,
}

impl Deflate {
    pub fn new(level: u8, zlib: bool) -> Deflate {
        let flags = create_comp_flags_from_zip_params(level as i32, if zlib { 15 } else { -15 }, 0);
        Deflate {
            c: alloc::boxed::Box::new(CompressorOxide::new(flags)),
            buf: vec![0; 32 * 1024],
            out: Vec::new(),
        }
    }

    fn run(&mut self, mut data: &[u8], flush: TDEFLFlush) {
        loop {
            let (status, used, made) = compress(&mut self.c, data, &mut self.buf, flush);
            self.out.extend_from_slice(&self.buf[..made]);
            data = &data[used..];
            match status {
                TDEFLStatus::Okay if !data.is_empty() || made == self.buf.len() => {}
                TDEFLStatus::Okay if flush == TDEFLFlush::Finish => {}
                _ => return,
            }
        }
    }

    pub fn write(&mut self, data: &[u8]) {
        self.run(data, TDEFLFlush::None);
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.run(&[], TDEFLFlush::Finish);
        self.out
    }
}

/// PNG, 8-bit RGB or RGBA, every row Paeth-filtered.
pub fn png(src: &impl Rows) -> Vec<u8> {
    let (w, h, c) = (src.w(), src.h(), src.ch() as usize);
    let mut out = Vec::from(&b"\x89PNG\r\n\x1a\n"[..]);
    let mut ihdr = [0u8; 13];
    ihdr[..4].copy_from_slice(&w.to_be_bytes());
    ihdr[4..8].copy_from_slice(&h.to_be_bytes());
    ihdr[8] = 8;
    ihdr[9] = if c == 4 { 6 } else { 2 };
    chunk(&mut out, b"IHDR", &ihdr);
    let n = w as usize * c;
    let (mut prev, mut cur) = (vec![0u8; n], vec![0u8; n]);
    let mut line = vec![0u8; n + 1];
    let mut z = Deflate::new(6, true);
    line[0] = 4;
    for y in 0..h {
        src.row(y, &mut cur);
        for i in 0..n {
            let a = if i >= c { cur[i - c] as i16 } else { 0 };
            let b = prev[i] as i16;
            let cc = if i >= c { prev[i - c] as i16 } else { 0 };
            let p = a + b - cc;
            let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - cc).abs());
            let pred = if pa <= pb && pa <= pc {
                a
            } else if pb <= pc {
                b
            } else {
                cc
            };
            line[i + 1] = cur[i].wrapping_sub(pred as u8);
        }
        z.write(&line);
        core::mem::swap(&mut prev, &mut cur);
    }
    chunk(&mut out, b"IDAT", &z.finish());
    chunk(&mut out, b"IEND", &[]);
    out
}
