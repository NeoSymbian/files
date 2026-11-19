// SPDX-License-Identifier: Apache-2.0
//! GIF, ours (the `gif` crate needs std): the blocks walked by hand, each frame's LZW data through `weezl`, drawn onto
//! an RGBA canvas with the frame's disposal. The viewer steps through the frames with [`Gif::step`]; thumbnails take
//! the first.

use crate::{Error, Format, Info, Pixels, le16};
use alloc::vec::Vec;

pub struct Gif<B: AsRef<[u8]>> {
    b: B,
    /// The global palette's place in `b`.
    global: Option<(usize, usize)>,
    /// The first block after the header and the global palette.
    start: usize,
    pos: usize,
    canvas: Pixels,
    /// What the last frame asks done to its rectangle before the next is drawn.
    dispose: (u8, [u32; 4]),
    saved: Option<Vec<u8>>,
}

/// One block's place: an image descriptor at `.0`, or the end.
enum Block {
    Image(usize),
    End,
}

fn sub_blocks(b: &[u8], mut i: usize) -> Result<usize, Error> {
    loop {
        let n = *b.get(i).ok_or(Error::Corrupt)? as usize;
        i += 1 + n;
        if n == 0 {
            return Ok(i);
        }
    }
}

impl<B: AsRef<[u8]>> Gif<B> {
    pub fn new(bytes: B) -> Result<Gif<B>, Error> {
        let b = bytes.as_ref();
        let w = le16(b, 6).ok_or(Error::Corrupt)?;
        let h = le16(b, 8).ok_or(Error::Corrupt)?;
        let flags = *b.get(10).ok_or(Error::Corrupt)?;
        let mut start = 13;
        let global = if flags & 0x80 != 0 {
            let n = 3 << ((flags & 7) + 1);
            b.get(13..13 + n).ok_or(Error::Corrupt)?;
            start += n;
            Some((13, 13 + n))
        } else {
            None
        };
        if w == 0 || h == 0 {
            return Err(Error::Corrupt);
        }
        Ok(Gif {
            b: bytes,
            global,
            start,
            pos: start,
            canvas: Pixels::new(w, h, 4),
            dispose: (0, [0; 4]),
            saved: None,
        })
    }

    pub fn canvas(&self) -> &Pixels {
        &self.canvas
    }

    /// Back to the first frame (the canvas is cleared).
    pub fn rewind(&mut self) {
        self.pos = self.start;
        self.canvas.px.fill(0);
        self.dispose = (0, [0; 4]);
        self.saved = None;
    }

    /// Walks to the next image descriptor; `gce` gets the graphic control extension before it.
    fn seek(&mut self, gce: &mut (u8, u32, Option<u8>)) -> Result<Block, Error> {
        let b = self.b.as_ref();
        loop {
            match b.get(self.pos) {
                Some(0x21) => {
                    let label = *b.get(self.pos + 1).ok_or(Error::Corrupt)?;
                    if label == 0xF9 && b.get(self.pos + 2) == Some(&4) {
                        let p = *b.get(self.pos + 3).ok_or(Error::Corrupt)?;
                        let delay = le16(b, self.pos + 4).ok_or(Error::Corrupt)?;
                        let t = *b.get(self.pos + 6).ok_or(Error::Corrupt)?;
                        *gce = ((p >> 2) & 7, delay, (p & 1 != 0).then_some(t));
                    }
                    self.pos = sub_blocks(b, self.pos + 2)?;
                }
                Some(0x2C) => return Ok(Block::Image(self.pos)),
                // the trailer, or a stream cut short after a whole frame
                Some(0x3B) | None => return Ok(Block::End),
                Some(_) => return Err(Error::Corrupt),
            }
        }
    }

    /// Draws the next frame; its delay in ms, or `None` after the last.
    pub fn step(&mut self) -> Result<Option<u32>, Error> {
        let mut gce = (0u8, 0u32, None);
        let at = match self.seek(&mut gce)? {
            Block::Image(at) => at,
            Block::End => return Ok(None),
        };
        let b = self.b.as_ref();
        let fx = le16(b, at + 1).ok_or(Error::Corrupt)?;
        let fy = le16(b, at + 3).ok_or(Error::Corrupt)?;
        let fw = le16(b, at + 5).ok_or(Error::Corrupt)?;
        let fh = le16(b, at + 7).ok_or(Error::Corrupt)?;
        let flags = *b.get(at + 9).ok_or(Error::Corrupt)?;
        let mut i = at + 10;
        // copied: drawing below changes `self`
        let palette: Vec<u8> = if flags & 0x80 != 0 {
            let n = 3 << ((flags & 7) + 1);
            let p = b.get(i..i + n).ok_or(Error::Corrupt)?;
            i += n;
            p.to_vec()
        } else {
            self.global.map_or(Vec::new(), |(a, e)| b[a..e].to_vec())
        };
        let min = *b.get(i).ok_or(Error::Corrupt)?;
        if !(1..=11).contains(&min) {
            return Err(Error::Corrupt);
        }
        // the data's sub-blocks, joined
        let mut data = Vec::new();
        let mut j = i + 1;
        loop {
            let n = *b.get(j).ok_or(Error::Corrupt)? as usize;
            if n == 0 {
                j += 1;
                break;
            }
            data.extend_from_slice(b.get(j + 1..j + 1 + n).ok_or(Error::Corrupt)?);
            j += 1 + n;
        }
        self.pos = j;
        let want = fw as usize * fh as usize;
        let mut idx = Vec::with_capacity(want);
        let mut lzw = weezl::decode::Decoder::new(weezl::BitOrder::Lsb, min);
        // a missing end code or junk after it is common: keep what decoded
        let _ = lzw.into_vec(&mut idx).decode(&data);
        idx.resize(want, 0);

        // the last frame's disposal
        let (d, r) = self.dispose;
        match d {
            2 => self.clear(r),
            3 => {
                if let Some(s) = self.saved.take() {
                    self.canvas.px = s;
                }
            }
            _ => {}
        }
        let rect = [fx, fy, fw, fh];
        self.dispose = (gce.0, rect);
        if gce.0 == 3 {
            self.saved = Some(self.canvas.px.clone());
        }

        let (cw, ch) = (self.canvas.w, self.canvas.h);
        let interlaced = flags & 0x40 != 0;
        for k in 0..fh {
            let row = if interlaced { interlace(k, fh) } else { k };
            let y = fy + row;
            if y >= ch {
                continue;
            }
            for x in 0..fw {
                let cx = fx + x;
                if cx >= cw {
                    break;
                }
                let c = idx[(k * fw + x) as usize];
                if gce.2 == Some(c) {
                    continue;
                }
                let Some(p) = palette.get(c as usize * 3..c as usize * 3 + 3) else {
                    continue;
                };
                let o = ((y * cw + cx) * 4) as usize;
                self.canvas.px[o..o + 4].copy_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
        // 0 or 1 cs means "as fast as you can": browsers use 100 ms
        let ms = if gce.1 <= 1 { 100 } else { gce.1 * 10 };
        Ok(Some(ms))
    }

    fn clear(&mut self, [x, y, w, h]: [u32; 4]) {
        let (cw, ch) = (self.canvas.w, self.canvas.h);
        for yy in y..(y + h).min(ch) {
            let a = ((yy * cw + x.min(cw)) * 4) as usize;
            let e = ((yy * cw + (x + w).min(cw)) * 4) as usize;
            self.canvas.px[a..e].fill(0);
        }
    }
}

/// The canvas row of the `k`th row stored in an interlaced frame `h` rows high.
fn interlace(k: u32, h: u32) -> u32 {
    let passes = [(0, 8), (4, 8), (2, 4), (1, 2)];
    let mut k = k;
    for (start, step) in passes {
        let n = if h > start {
            (h - start).div_ceil(step)
        } else {
            0
        };
        if k < n {
            return start + k * step;
        }
        k -= n;
    }
    0
}

pub fn probe(b: &[u8]) -> Option<Info> {
    let g = Gif::new(b).ok()?;
    let mut i = Info::new(Format::Gif, g.canvas.w, g.canvas.h);
    i.alpha = true;
    // a second image descriptor: animated
    let mut g = g;
    let mut gce = (0, 0, None);
    let mut frames = 0;
    while frames < 2 {
        match g.seek(&mut gce) {
            Ok(Block::Image(at)) => {
                frames += 1;
                let flags = *b.get(at + 9)?;
                let mut j = at + 10;
                if flags & 0x80 != 0 {
                    j += 3 << ((flags & 7) + 1);
                }
                g.pos = sub_blocks(b, j + 1).ok()?;
            }
            _ => break,
        }
    }
    i.animated = frames > 1;
    Some(i)
}

pub fn first(b: &[u8]) -> Result<Pixels, Error> {
    let mut g = Gif::new(b)?;
    g.step()?.ok_or(Error::Corrupt)?;
    Ok(g.canvas)
}
