// SPDX-License-Identifier: Apache-2.0
//! The few EXIF fields Files uses: orientation, the date taken and the embedded preview. A small IFD walker over the
//! TIFF block of a JPEG's APP1 segment; anything it doesn't understand is skipped.

use alloc::string::String;
use core::ops::Range;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exif {
    /// 1–8; 1 when absent or out of range.
    pub orientation: u8,
    pub taken: Option<String>,
    /// The preview JPEG, relative to the TIFF block.
    pub thumb: Option<Range<usize>>,
}

struct Tiff<'a> {
    b: &'a [u8],
    le: bool,
}

impl Tiff<'_> {
    fn u16(&self, i: usize) -> Option<u32> {
        if self.le {
            crate::le16(self.b, i)
        } else {
            crate::be16(self.b, i)
        }
    }

    fn u32(&self, i: usize) -> Option<u32> {
        if self.le {
            crate::le32(self.b, i)
        } else {
            crate::be32(self.b, i)
        }
    }

    /// The entries of the IFD at `at` as `(tag, type, count, value-or-offset place)`, and the next IFD's offset.
    fn ifd(&self, at: usize) -> Option<(impl Iterator<Item = (u32, u32, u32, usize)> + '_, u32)> {
        let n = self.u16(at)? as usize;
        let next = self.u32(at + 2 + n * 12).unwrap_or(0);
        Some((
            (0..n.min(512)).filter_map(move |k| {
                let e = at + 2 + k * 12;
                Some((self.u16(e)?, self.u16(e + 2)?, self.u32(e + 4)?, e + 8))
            }),
            next,
        ))
    }
}

/// Parses the TIFF block (after `Exif\0\0`).
pub fn parse(b: &[u8]) -> Exif {
    let mut x = Exif {
        orientation: 1,
        ..Exif::default()
    };
    let le = match b.get(..4) {
        Some(b"II*\0") => true,
        Some(b"MM\0*") => false,
        _ => return x,
    };
    let t = Tiff { b, le };
    let Some(ifd0) = t.u32(4) else { return x };
    let mut exif_ifd = None;
    let mut next = 0;
    if let Some((entries, n)) = t.ifd(ifd0 as usize) {
        next = n;
        for (tag, _, _, v) in entries {
            match tag {
                0x0112 => {
                    let o = t.u16(v).unwrap_or(1);
                    if (1..=8).contains(&o) {
                        x.orientation = o as u8;
                    }
                }
                0x8769 => exif_ifd = t.u32(v),
                _ => {}
            }
        }
    }
    if let Some(at) = exif_ifd
        && let Some((entries, _)) = t.ifd(at as usize)
    {
        for (tag, ty, count, v) in entries {
            // DateTimeOriginal: ASCII, 20 bytes with the NUL, so always at an offset
            if tag == 0x9003 && ty == 2 && count >= 19 {
                let off = t.u32(v).unwrap_or(0) as usize;
                if let Some(s) = b
                    .get(off..off + 19)
                    .and_then(|s| core::str::from_utf8(s).ok())
                {
                    x.taken = Some(String::from(s));
                }
            }
        }
    }
    // IFD1 holds the preview
    if next != 0
        && let Some((entries, _)) = t.ifd(next as usize)
    {
        let (mut off, mut len) = (None, None);
        for (tag, _, _, v) in entries {
            match tag {
                0x0201 => off = t.u32(v),
                0x0202 => len = t.u32(v),
                _ => {}
            }
        }
        if let (Some(o), Some(l)) = (off, len) {
            let (o, l) = (o as usize, l as usize);
            if l > 0 && o.checked_add(l).is_some_and(|e| e <= b.len()) {
                x.thumb = Some(o..o + l);
            }
        }
    }
    x
}
