// SPDX-License-Identifier: Apache-2.0
//! Images for Files (files-app.md FA5): probe a file's header, decode it to pixels within a memory budget, shrink it for
//! the screen and the thumbnails, apply the editor's turn / crop / resize, and encode a copy as JPEG or PNG.
//!
//! Decoders: JPEG (`zune-jpeg`), PNG (`zune-png`), GIF (ours, over `weezl`), BMP (ours), WebP (`image-webp`, patched
//! for no_std) and a bounded SVG subset (ours, drawn with `tiny-skia`). A full decode keeps the whole image in memory
//! (a 12 MP JPEG is 36 MB as RGB), so [`decode`] checks [`cost`] against the caller's budget first and falls back to
//! a JPEG's EXIF preview when the image doesn't fit.
#![no_std]

extern crate alloc;

mod bmp;
pub mod encode;
pub mod exif;
pub mod gif;
mod jpeg;
pub mod ops;
mod png;
pub mod shrink;
pub mod svg;
mod webp;

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

/// 8-bit pixels, rows top to bottom, `ch` bytes a pixel: 3 (RGB) or 4 (RGBA, not premultiplied).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub w: u32,
    pub h: u32,
    pub ch: u8,
    pub px: Vec<u8>,
}

impl Pixels {
    pub fn new(w: u32, h: u32, ch: u8) -> Pixels {
        Pixels {
            w,
            h,
            ch,
            px: alloc::vec![0; w as usize * h as usize * ch as usize],
        }
    }

    /// The pixel at `(x, y)` as RGBA.
    #[inline]
    pub fn rgba(&self, x: u32, y: u32) -> [u8; 4] {
        let c = self.ch as usize;
        let i = (y as usize * self.w as usize + x as usize) * c;
        let p = &self.px[i..i + c];
        [p[0], p[1], p[2], if c == 4 { p[3] } else { 255 }]
    }

    /// Whether any pixel is less than opaque.
    pub fn has_alpha(&self) -> bool {
        self.ch == 4 && self.px.as_chunks::<4>().0.iter().any(|p| p[3] != 255)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    Gif,
    WebP,
    Bmp,
    Svg,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Jpeg => "JPEG",
            Format::Png => "PNG",
            Format::Gif => "GIF",
            Format::WebP => "WebP",
            Format::Bmp => "BMP",
            Format::Svg => "SVG",
        }
    }
}

/// What the header says, read without decoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub format: Format,
    /// As stored (before the EXIF orientation).
    pub w: u32,
    pub h: u32,
    /// EXIF orientation, 1–8 (1 = as stored).
    pub orientation: u8,
    /// EXIF DateTimeOriginal, "YYYY:MM:DD HH:MM:SS".
    pub taken: Option<String>,
    /// A progressive JPEG (its decode holds every coefficient too).
    pub progressive: bool,
    /// The EXIF preview's bytes in the file (a JPEG).
    pub thumb: Option<Range<usize>>,
    pub alpha: bool,
    /// More than one frame (a GIF or WebP animation).
    pub animated: bool,
}

impl Info {
    fn new(format: Format, w: u32, h: u32) -> Info {
        Info {
            format,
            w,
            h,
            orientation: 1,
            taken: None,
            progressive: false,
            thumb: None,
            alpha: false,
            animated: false,
        }
    }

    /// The size as shown (the orientation applied).
    pub fn shown(&self) -> (u32, u32) {
        if self.orientation >= 5 {
            (self.h, self.w)
        } else {
            (self.w, self.h)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No format we know.
    NotImage,
    /// A format we know, broken.
    Corrupt,
    /// It doesn't fit the memory budget (and has no preview).
    TooBig,
}

/// Reads the header: format, size, and for a JPEG its EXIF orientation, date and preview.
pub fn probe(b: &[u8]) -> Option<Info> {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        jpeg::probe(b)
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        png::probe(b)
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        gif::probe(b)
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        webp::probe(b)
    } else if b.starts_with(b"BM") {
        bmp::probe(b)
    } else {
        svg::probe(b)
    }
}

/// Bytes a full decode holds at its peak, besides the file itself.
pub fn cost(i: &Info) -> usize {
    let n = i.w as usize * i.h as usize;
    match i.format {
        // the output, plus every coefficient (2 bytes, 3 components at most) when progressive
        Format::Jpeg => n * 3 + if i.progressive { n * 6 } else { 0 },
        // the inflated rows (up to 8 bytes a pixel at 16 bits) and the output
        Format::Png => n * 8 + n * 4,
        // the canvas and one frame's indices
        Format::Gif => n * 5,
        Format::WebP | Format::Bmp | Format::Svg => n * 4,
    }
}

pub struct Decoded {
    pub info: Info,
    /// As stored: [`shrink::fit`] applies `info.orientation`.
    pub px: Pixels,
    /// The EXIF preview stands in for an image too big to decode.
    pub preview: bool,
}

/// Decodes the whole image (a GIF's first frame), if [`cost`] fits `budget`; a JPEG that doesn't fit shows its EXIF
/// preview instead.
pub fn decode(b: &[u8], budget: usize) -> Result<Decoded, Error> {
    let info = probe(b).ok_or(Error::NotImage)?;
    if info.w == 0 || info.h == 0 {
        return Err(Error::Corrupt);
    }
    if cost(&info) > budget {
        if let Some(r) = info.thumb.clone() {
            let px = jpeg::decode(&b[r])?;
            return Ok(Decoded {
                info,
                px,
                preview: true,
            });
        }
        return Err(Error::TooBig);
    }
    let px = match info.format {
        Format::Jpeg => jpeg::decode(b)?,
        Format::Png => png::decode(b)?,
        Format::Gif => gif::first(b)?,
        Format::WebP => webp::decode(b)?,
        Format::Bmp => bmp::decode(b)?,
        Format::Svg => svg::render(b, svg::SIDE)?,
    };
    Ok(Decoded {
        info,
        px,
        preview: false,
    })
}

#[inline]
pub(crate) fn be16(b: &[u8], i: usize) -> Option<u32> {
    Some(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32)
}

#[inline]
pub(crate) fn be32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

#[inline]
pub(crate) fn le16(b: &[u8], i: usize) -> Option<u32> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32)
}

#[inline]
pub(crate) fn le32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}
