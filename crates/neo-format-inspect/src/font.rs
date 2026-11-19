// SPDX-License-Identifier: Apache-2.0
//! The font viewer: what a .ttf/.otf/.ttc is (from `ttf-parser`) and sample text drawn with [`crate::raster`]. One
//! line at a time, no shaping or kerning: a preview, not a text engine.
//!
//! The cmap is only ever asked about single code points (`glyph_index`): ttf-parser's `codepoints` walks a group's
//! whole range, which a corrupt format 12 table can make four billion long.

use crate::raster::Raster;
use crate::{Error, Result};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use ttf_parser::{Face, GlyphId, OutlineBuilder, PlatformId, cmap};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub family: String,
    pub subfamily: String,
    /// "2.37" from the name table's "Version 2.37".
    pub version: String,
    /// "TrueType" (glyf outlines) or "OpenType" (CFF).
    pub format: &'static str,
    /// More than 1 for a collection (.ttc); [`Font`] shows the first.
    pub faces: u32,
    pub glyphs: u16,
    /// The scripts the cmap covers (at least [`SCRIPT_MIN`] code points of each, of 256 sampled in a big range).
    pub scripts: Vec<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// Row by row, 0 = no ink.
    pub alpha: Vec<u8>,
}

/// Code points a script needs in the cmap to be listed (a Latin font often carries a stray Ω or π).
pub const SCRIPT_MIN: usize = 10;

const SCRIPTS: &[(&str, &[(u32, u32)])] = &[
    ("Latin", &[(0x41, 0x5a), (0x61, 0x7a), (0xc0, 0x24f)]),
    ("Greek", &[(0x370, 0x3ff)]),
    ("Cyrillic", &[(0x400, 0x4ff)]),
    ("Armenian", &[(0x531, 0x58f)]),
    ("Hebrew", &[(0x5d0, 0x5ea)]),
    ("Arabic", &[(0x600, 0x6ff)]),
    ("Devanagari", &[(0x900, 0x97f)]),
    ("Thai", &[(0xe00, 0xe7f)]),
    ("Georgian", &[(0x10a0, 0x10ff)]),
    ("Kana", &[(0x3040, 0x30ff)]),
    ("CJK", &[(0x4e00, 0x9fff)]),
    ("Hangul", &[(0xac00, 0xd7a3), (0x1100, 0x11ff)]),
];

/// Pixel sizes drawn: a corrupt or silly size can't ask for a huge bitmap.
const PX_MAX: f32 = 512.0;
/// The largest bitmap side (a line's width is the caller's `max_width`, also capped).
const SIDE_MAX: u32 = 4096;
/// Glyphs kept, and their bytes; the cache starts over past either (a crafted bounding box makes every glyph
/// 4 em square).
const CACHE_MAX: usize = 1024;
const CACHE_BYTES: usize = 8 << 20;

struct Glyph {
    /// The bitmap's top-left from the pen on the baseline (y down).
    x: i32,
    y: i32,
    w: usize,
    h: usize,
    alpha: Vec<u8>,
}

pub struct Font<'a> {
    face: Face<'a>,
    /// One Unicode cmap subtable, chosen once: `Face::glyph_index` searches every subtable on each call, and a
    /// crafted cmap with 65,535 records makes that a hang.
    sub: Option<cmap::Subtable<'a>>,
    faces: u32,
    cache: BTreeMap<(u16, u32), Glyph>,
    cached: usize,
}

impl<'a> Font<'a> {
    /// The first face of `data`.
    pub fn new(data: &'a [u8]) -> Result<Self> {
        let faces = ttf_parser::fonts_in_collection(data).unwrap_or(1);
        let face = Face::parse(data, 0).map_err(|e| match e {
            ttf_parser::FaceParsingError::MalformedFont => Error::Truncated,
            ttf_parser::FaceParsingError::UnknownMagic => Error::Bad("Not a font"),
            _ => Error::Bad("Not a font: a table is missing or broken"),
        })?;
        let sub = face.tables().cmap.and_then(|t| {
            let uni = || t.subtables.into_iter().filter(|s| s.is_unicode());
            uni()
                .find(|s| matches!(s.format, cmap::Format::SegmentedCoverage(_)))
                .or_else(|| {
                    uni().find(|s| matches!(s.format, cmap::Format::SegmentMappingToDeltaValues(_)))
                })
                .or_else(|| uni().next())
        });
        Ok(Font {
            face,
            sub,
            faces,
            cache: BTreeMap::new(),
            cached: 0,
        })
    }

    pub fn info(&self) -> Info {
        let version = self.name(ttf_parser::name_id::VERSION);
        let version = version.trim();
        let version = version
            .strip_prefix("Version ")
            .or_else(|| version.strip_prefix("version "))
            .unwrap_or(version);
        let version = version.split([';', ' ']).next().unwrap_or("");
        let scripts = SCRIPTS
            .iter()
            .filter(|(_, ranges)| {
                ranges
                    .iter()
                    // a big range (CJK, Hangul) is sampled: 256 probes, not 20 000
                    .flat_map(|&(a, b)| (a..=b).step_by(((b - a) / 256).max(1) as usize))
                    .filter(|&c| self.has(c))
                    .take(SCRIPT_MIN)
                    .count()
                    >= SCRIPT_MIN
            })
            .map(|(name, _)| *name)
            .collect();
        Info {
            family: self.name(ttf_parser::name_id::FAMILY),
            subfamily: self.name(ttf_parser::name_id::SUBFAMILY),
            version: String::from(version),
            format: if self.face.tables().cff.is_some() {
                "OpenType"
            } else {
                "TrueType"
            },
            faces: self.faces,
            glyphs: self.face.number_of_glyphs(),
            scripts,
        }
    }

    /// Every non-space character of `text` has a glyph.
    pub fn covers(&self, text: &str) -> bool {
        text.chars()
            .filter(|c| !c.is_whitespace())
            .all(|c| self.has(c as u32))
    }

    /// Every mapped code point, in order.
    pub fn chars(&self) -> Vec<char> {
        // shortcut: planes 0-3 only (2 and 3 hold the CJK extensions); probe more if a font for planes 15/16 matters
        let top = if self.has_full_unicode_cmap() {
            0x3_ffff
        } else {
            0xffff
        };
        (0..=top)
            .filter_map(char::from_u32)
            .filter(|&c| self.has(c as u32))
            .collect()
    }

    /// `text` on one line at `px`: the baseline at the ascender, the height the ascender to the descender, the width
    /// the advances (hmtx, no kerning) clipped at `max_width`.
    pub fn render_line(&mut self, text: &str, px: f32, max_width: u32) -> Bitmap {
        let px = clamp_px(px);
        let scale = px / self.face.units_per_em() as f32;
        let asc = self.face.ascender() as f32 * scale;
        let desc = self.face.descender() as f32 * scale;
        let height = (libm::ceilf(asc - desc).max(1.0) as u32).min(side_max(px));
        let baseline = libm::roundf(asc) as i32;
        let mut pen = 0.0f32;
        let mut glyphs = Vec::new();
        for c in text.chars() {
            if pen >= max_width as f32 {
                break;
            }
            let gid = self.gid(c).unwrap_or(GlyphId(0));
            glyphs.push((gid, libm::roundf(pen) as i32));
            pen += self.face.glyph_hor_advance(gid).unwrap_or(0) as f32 * scale;
        }
        let width = (libm::ceilf(pen).max(0.0) as u32)
            .min(max_width)
            .min(SIDE_MAX);
        let mut out = Bitmap {
            width,
            height,
            alpha: vec![0; width as usize * height as usize],
        };
        for (gid, x) in glyphs {
            if let Some(g) = self.glyph(gid, px) {
                blit(&mut out, g, x, baseline);
            }
        }
        out
    }

    /// `c` in a `cell`×`cell` square: centred by its advance across, by the ascender and descender down.
    pub fn render_char(&mut self, c: char, px: f32, cell: u32) -> Bitmap {
        let px = clamp_px(px);
        let cell = cell.min(SIDE_MAX);
        let scale = px / self.face.units_per_em() as f32;
        let asc = self.face.ascender() as f32 * scale;
        let desc = self.face.descender() as f32 * scale;
        let gid = self.gid(c).unwrap_or(GlyphId(0));
        let adv = self.face.glyph_hor_advance(gid).unwrap_or(0) as f32 * scale;
        let x = libm::roundf((cell as f32 - adv) / 2.0) as i32;
        let baseline = libm::roundf((cell as f32 - (asc - desc)) / 2.0 + asc) as i32;
        let mut out = Bitmap {
            width: cell,
            height: cell,
            alpha: vec![0; cell as usize * cell as usize],
        };
        // drawn once each (the Glyphs tab): not kept, so a grid of big glyphs isn't held twice
        if let Some(g) = self.draw(gid, px) {
            blit(&mut out, &g, x, baseline);
        }
        out
    }

    fn gid(&self, c: char) -> Option<GlyphId> {
        self.sub.and_then(|s| s.glyph_index(u32::from(c)))
    }

    fn has(&self, c: u32) -> bool {
        char::from_u32(c)
            .and_then(|c| self.gid(c))
            .is_some_and(|g| g.0 != 0)
    }

    fn has_full_unicode_cmap(&self) -> bool {
        self.face.tables().cmap.is_some_and(|t| {
            t.subtables
                .into_iter()
                .any(|s| s.is_unicode() && matches!(s.format, cmap::Format::SegmentedCoverage(_)))
        })
    }

    /// The name table's `id`, from a Unicode or Windows record (else Mac Roman, as ASCII).
    fn name(&self, id: u16) -> String {
        let mut mac = None;
        for n in self.face.names() {
            if n.name_id != id {
                continue;
            }
            let utf16 = match n.platform_id {
                PlatformId::Unicode => true,
                PlatformId::Windows => matches!(n.encoding_id, 0 | 1 | 10),
                PlatformId::Macintosh if n.encoding_id == 0 => {
                    mac.get_or_insert(n.name);
                    false
                }
                _ => false,
            };
            if utf16 {
                let units = n
                    .name
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&b| u16::from_be_bytes(b));
                return char::decode_utf16(units)
                    .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                    .collect();
            }
        }
        mac.map(|b| {
            b.iter()
                .map(|&b| if b.is_ascii() { b as char } else { '?' })
                .collect()
        })
        .unwrap_or_default()
    }

    fn glyph(&mut self, gid: GlyphId, px: f32) -> Option<&Glyph> {
        let key = (gid.0, px.to_bits());
        if !self.cache.contains_key(&key) {
            let g = self.draw(gid, px)?;
            if self.cache.len() >= CACHE_MAX || self.cached + g.alpha.len() > CACHE_BYTES {
                self.cache.clear();
                self.cached = 0;
            }
            self.cached += g.alpha.len();
            self.cache.insert(key, g);
        }
        self.cache.get(&key)
    }

    fn draw(&self, gid: GlyphId, px: f32) -> Option<Glyph> {
        let scale = px / self.face.units_per_em() as f32;
        let bb = self.face.glyph_bounding_box(gid)?;
        let x0 = libm::floorf(bb.x_min as f32 * scale);
        let y0 = libm::floorf(-(bb.y_max as f32) * scale);
        let side =
            |a: f32, b: f32| (libm::ceilf(b - a).max(0.0) as usize + 1).min(side_max(px) as usize);
        let w = side(x0, bb.x_max as f32 * scale);
        let h = side(y0, -(bb.y_min as f32) * scale);
        let mut pen = Pen {
            r: Raster::new(w, h),
            scale,
            dx: -x0,
            dy: -y0,
            start: (0.0, 0.0),
            at: (0.0, 0.0),
        };
        self.face.outline_glyph(gid, &mut pen)?;
        pen.close();
        Some(Glyph {
            x: x0 as i32,
            y: y0 as i32,
            w,
            h,
            alpha: pen.r.alpha(),
        })
    }
}

/// A glyph's or a line's height at most 4 em: a corrupt bounding box or ascender can't ask for more.
fn side_max(px: f32) -> u32 {
    (4.0 * px) as u32 + 8
}

fn clamp_px(px: f32) -> f32 {
    if px.is_finite() {
        px.clamp(1.0, PX_MAX)
    } else {
        16.0
    }
}

/// `g` with its pen at (`x`, `baseline`), clipped to `out`; overlaps keep the darker.
fn blit(out: &mut Bitmap, g: &Glyph, x: i32, baseline: i32) {
    let (ow, oh) = (out.width as i64, out.height as i64);
    for gy in 0..g.h {
        let oy = baseline as i64 + g.y as i64 + gy as i64;
        if !(0..oh).contains(&oy) {
            continue;
        }
        for gx in 0..g.w {
            let ox = x as i64 + g.x as i64 + gx as i64;
            if !(0..ow).contains(&ox) {
                continue;
            }
            let (Some(&a), Some(o)) = (
                g.alpha.get(gy * g.w + gx),
                out.alpha.get_mut((oy * ow + ox) as usize),
            ) else {
                continue;
            };
            *o = (*o).max(a);
        }
    }
}

/// ttf-parser's outline (font units, y up) into a [`Raster`] (pixels, y down).
struct Pen {
    r: Raster,
    scale: f32,
    dx: f32,
    dy: f32,
    start: (f32, f32),
    at: (f32, f32),
}

impl Pen {
    fn p(&self, x: f32, y: f32) -> (f32, f32) {
        (x * self.scale + self.dx, -y * self.scale + self.dy)
    }
}

impl OutlineBuilder for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.at = self.p(x, y);
        self.start = self.at;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.r.line(self.at, p);
        self.at = p;
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (c, p) = (self.p(x1, y1), self.p(x, y));
        self.r.quad(self.at, c, p);
        self.at = p;
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (c1, c2, p) = (self.p(x1, y1), self.p(x2, y2), self.p(x, y));
        self.r.cubic(self.at, c1, c2, p);
        self.at = p;
    }
    fn close(&mut self) {
        if self.at != self.start {
            self.r.line(self.at, self.start);
            self.at = self.start;
        }
    }
}
