// SPDX-License-Identifier: GPL-3.0-only
//! The inspectors (FA7) from `neo-format-inspect`: the font sample and its glyphs (drawn by our rasteriser into
//! images), the certificate's and the package's rows, and the hex rows. The app and the snapshot tests share them.

use crate::{Detail, GlyphRow, InfoGroup, Sample, SampleGroup, TRow, TSpan, colour};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use neo_format_inspect::cert::{self, Cert, Validity};
use neo_format_inspect::font::{Bitmap, Font};
use neo_format_inspect::hex;
use neo_format_inspect::jad::Midlet;
use neo_format_inspect::sis::Package;
use neo_format_pim::date::Date;
use slint::{Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

fn detail(key: &str, value: impl Into<String>) -> Detail {
    Detail {
        key: key.into(),
        value: value.into().into(),
        mono: false,
    }
}

/// White ink of `b`'s coverage (premultiplied: white at alpha a is a, a, a, a).
pub fn ink(b: &Bitmap) -> Image {
    let mut px = SharedPixelBuffer::<Rgba8Pixel>::new(b.width.max(1), b.height.max(1));
    for (p, &a) in px.make_mut_slice().iter_mut().zip(&b.alpha) {
        *p = Rgba8Pixel {
            r: a,
            g: a,
            b: a,
            a,
        };
    }
    Image::from_rgba8_premultiplied(px)
}

/// The sample texts, by script (files2-view-font); a script shows when the font has every letter of its text.
const SAMPLES: [(&str, &str); 7] = [
    ("LATIN", "The quick brown fox jumps over the lazy dog"),
    ("CYRILLIC", "Съешь же ещё этих мягких французских булок"),
    ("ARMENIAN", "Բարև, Երևան։ Ճանապարհը տանում է դեպի ծով"),
    ("GREEK", "Τάχιστη αλώπηξ βαφής ψημένη γη, δρασκελίζει"),
    ("GEORGIAN", "ქართული ენა მსოფლიოს ერთ-ერთი უძველესი"),
    ("HEBREW", "דג סקרן שט בים מאוכזב ולפתע מצא חברה"),
    ("ARABIC", "نص حكيم له سر قاطع وذو شأن عظيم"),
];
const SIZES: [f32; 4] = [12.0, 16.0, 24.0, 32.0];
/// shortcut: the Glyphs tab draws the first 240 characters at once (each cell is RGBA: 240 are 14 MB at 3x); a lazily
/// drawn grid when fonts with thousands
/// (CJK) need all of them
pub const GLYPHS_MAX: usize = 240;

/// What the font view shows.
pub struct FontScreen {
    pub aa: Image,
    pub family_art: Image,
    pub family: String,
    pub line1: String,
    pub line2: String,
    pub groups: Vec<SampleGroup>,
}

/// The font's hero and samples; `scale` is physical pixels per dp, `width_dp` the sample lines' room.
pub fn font_screen(f: &mut Font, size: u64, scale: f32, width_dp: f32) -> FontScreen {
    let info = f.info();
    let w = (width_dp * scale) as u32;
    let aa = ink(&f.render_line("Aa", 40.0 * scale, (72.0 * scale) as u32));
    let family_art = if f.covers(&info.family) {
        ink(&f.render_line(&info.family, 22.0 * scale, w))
    } else {
        Image::default()
    };
    let mut groups = Vec::new();
    for (title, text) in SAMPLES {
        // letters only: a font may lack a script's punctuation (DejaVu's subset has no Armenian full stop)
        let letters: String = text.chars().filter(|c| c.is_alphabetic()).collect();
        if !f.covers(&letters) {
            continue;
        }
        let lines = SIZES
            .iter()
            .map(|&s| Sample {
                size: format!("{s}").into(),
                line: ink(&f.render_line(text, s * scale, w)),
                h: s,
            })
            .collect();
        groups.push(SampleGroup {
            title: title.into(),
            lines: model(lines),
        });
    }
    let mut kind = vec![info.subfamily.clone(), info.format.to_string()];
    if !info.version.is_empty() {
        kind.push(format!("version {}", info.version));
    }
    if info.faces > 1 {
        kind.push(format!("1 of {} faces", info.faces));
    }
    let scripts = match info.scripts.len() {
        0 => String::new(),
        n if n > 4 => format!("{}, more", info.scripts[..4].join(", ")),
        _ => info.scripts.join(", "),
    };
    let size = files_core::size_text(size);
    FontScreen {
        aa,
        family_art,
        family: info.family,
        line1: kind
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · "),
        line2: if scripts.is_empty() {
            size
        } else {
            format!("{size} · {scripts}")
        },
        groups,
    }
}

/// The Glyphs tab: six to a row, and the note when the font has more than are drawn.
pub fn glyph_rows(f: &mut Font, scale: f32) -> (Vec<GlyphRow>, String) {
    let chars = f.chars();
    let drawn: Vec<Image> = chars
        .iter()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .take(GLYPHS_MAX)
        .map(|&c| ink(&f.render_char(c, 28.0 * scale, (40.0 * scale) as u32)))
        .collect();
    let rows = drawn
        .chunks(6)
        .map(|c| GlyphRow {
            cells: model(c.to_vec()),
        })
        .collect();
    let note = if chars.len() > GLYPHS_MAX {
        format!(
            "The first {GLYPHS_MAX} of {} characters",
            crate::thousands(chars.len() as u64)
        )
    } else {
        String::new()
    };
    (rows, note)
}

fn when(t: &cert::Time) -> String {
    let d = Date {
        year: i32::from(t.year),
        month: t.month,
        day: t.day,
    };
    format!(
        "{} {} {}, {:02}:{:02}",
        t.day,
        d.month_name().get(..3).unwrap_or(""),
        t.year,
        t.hour,
        t.minute
    )
}

fn years(days: i64, word: &str) -> String {
    match days {
        d if d >= 730 => format!("{} years {word}", d / 365),
        d if d >= 365 => format!("1 year {word}"),
        1 => format!("1 day {word}"),
        d => format!("{d} days {word}"),
    }
}

fn name_rows(n: &cert::Name) -> Vec<Detail> {
    let mut r = Vec::new();
    for (k, v) in [
        ("Common name", &n.cn),
        ("Organisation", &n.o),
        ("Unit", &n.ou),
        ("Country", &n.c),
    ] {
        if let Some(v) = v {
            r.push(detail(k, v.as_str()));
        }
    }
    r
}

fn name_line(n: &cert::Name) -> String {
    n.cn.clone()
        .or_else(|| n.o.clone())
        .unwrap_or_else(|| String::from("(no name)"))
}

/// What the certificate view shows.
pub struct CertScreen {
    pub title: String,
    pub status: String,
    pub ok: bool,
    pub groups: Vec<InfoGroup>,
    pub fp: [String; 2],
}

/// Certificate `i` of `n` in the file, checked against `now` (seconds since 1970).
pub fn cert_screen(c: &Cert, i: usize, n: usize, now: i64) -> CertScreen {
    let v = cert::validity(c, now);
    let kind = if c.self_signed {
        if c.ca {
            "self-signed root"
        } else {
            "self-signed"
        }
    } else if c.ca {
        "certificate authority"
    } else {
        "issued by another"
    };
    let (status, ok) = match v {
        Validity::Valid { .. } => (format!("✓ Valid · {kind}"), true),
        Validity::Expired { .. } => (format!("Expired · {kind}"), false),
        Validity::NotYet => (format!("Not valid yet · {kind}"), false),
    };
    let mut groups = vec![InfoGroup {
        title: "SUBJECT".into(),
        rows: model(name_rows(&c.subject)),
    }];
    let issuer = if c.self_signed {
        format!("{} (itself)", name_line(&c.issuer))
    } else {
        name_line(&c.issuer)
    };
    let mut iss = vec![detail("Issued by", issuer)];
    if !c.self_signed {
        iss.extend(name_rows(&c.issuer).into_iter().skip(1));
    }
    groups.push(InfoGroup {
        title: "ISSUER".into(),
        rows: model(iss),
    });
    let to = match v {
        Validity::Valid { days_left } => {
            format!("{} · {}", when(&c.not_after), years(days_left, "left"))
        }
        Validity::Expired { days_ago } => {
            format!("{} · {}", when(&c.not_after), years(days_ago, "ago"))
        }
        Validity::NotYet => when(&c.not_after),
    };
    groups.push(InfoGroup {
        title: "VALIDITY".into(),
        rows: model(vec![
            detail("From", when(&c.not_before)),
            detail("To", to),
            detail("Key", format!("{} · {}", c.key, c.signature)),
        ]),
    });
    if n > 1
        && let Some(g) = groups.last_mut()
    {
        let mut rows: Vec<Detail> = slint::Model::iter(&g.rows).collect();
        rows.push(detail("In the file", format!("{} of {n}", i + 1)));
        g.rows = model(rows);
    }
    if !c.san.is_empty() {
        groups.push(InfoGroup {
            title: "NAMES".into(),
            rows: model(c.san.iter().map(|s| detail("Name", s.as_str())).collect()),
        });
    }
    CertScreen {
        title: name_line(&c.subject),
        status,
        ok,
        groups,
        fp: cert::fingerprint_lines(&c.sha256),
    }
}

/// What the package view shows.
pub struct AppScreen {
    pub title: String,
    pub vendor: String,
    pub meta: String,
    pub info_title: String,
    pub info_text: String,
    pub groups: Vec<InfoGroup>,
    pub caps_title: String,
    pub caps: Vec<Detail>,
    pub note: String,
}

/// A Symbian package (files2-view-app); `kind` is "SIS" or "SISX".
pub fn sis_screen(p: &Package, size: u64, kind: &str, now: i64) -> AppScreen {
    let (a, b, c) = p.version;
    let mut rows = vec![detail(
        "Platform",
        if p.platforms.is_empty() {
            String::from("Any Symbian 9")
        } else {
            p.platforms.join(", ")
        },
    )];
    rows.push(detail(
        "Signed by",
        match &p.signer {
            Some((who, until)) if until.unix() < now => format!("{who} (expired {})", until.year),
            Some((who, _)) => who.clone(),
            None => String::from("Not signed"),
        },
    ));
    rows.push(detail(
        "Install to",
        match p.drive {
            Some(d) => format!("{d}:"),
            None => String::from("The drive you pick"),
        },
    ));
    rows.push(Detail {
        key: "UID".into(),
        value: format!("0x{:08X}", p.uid).into(),
        mono: true,
    });
    rows.push(detail("Files", p.files.to_string()));
    let n = p.capabilities.len();
    AppScreen {
        title: p.name.clone(),
        vendor: p.vendor.clone(),
        meta: format!(
            "Version {a}.{b:02}{} · {} · {kind}",
            if c > 0 {
                format!(".{c}")
            } else {
                String::new()
            },
            files_core::size_text(size)
        ),
        info_title: "Installs to Old apps".into(),
        info_text: "Runs in the S60 compatibility layer".into(),
        groups: vec![InfoGroup {
            title: "DETAILS".into(),
            rows: model(rows),
        }],
        caps_title: match n {
            1 => String::from("ASKS FOR 1 CAPABILITY"),
            n => format!("ASKS FOR {n} CAPABILITIES"),
        },
        caps: p
            .capabilities
            .iter()
            .map(|c| Detail {
                key: c.name.into(),
                value: c.text.into(),
                mono: false,
            })
            .collect(),
        note: String::from("Installing packages comes with the on-phone installer (NEOINST)."),
    }
}

/// A Java ME app (.jad, .jar): its details only; NeoSymbian has no Java runtime.
pub fn midlet_screen(m: &Midlet, size: u64, kind: &str) -> AppScreen {
    let mut rows = vec![detail(
        "Platform",
        if m.profile.is_empty() {
            String::from("Java ME")
        } else {
            format!("Java ME ({})", m.profile)
        },
    )];
    if let Some(j) = m.jar_size {
        rows.push(detail("App size", files_core::size_text(j)));
    }
    AppScreen {
        title: m.name.clone(),
        vendor: m.vendor.clone(),
        meta: format!(
            "Version {} · {} · {kind}",
            m.version,
            files_core::size_text(size)
        ),
        info_title: "A Java app".into(),
        info_text: "NeoSymbian has no Java runtime, so it can't run".into(),
        groups: vec![InfoGroup {
            title: "DETAILS".into(),
            rows: model(rows),
        }],
        caps_title: format!("ASKS FOR {} PERMISSIONS", m.permissions.len()),
        caps: m
            .permissions
            .iter()
            .map(|p| Detail {
                key: p.as_str().into(),
                value: "".into(),
                mono: false,
            })
            .collect(),
        note: String::new(),
    }
}

/// Hex rows from `row` on, over `bytes` (which start at that row's offset): offset, bytes, text.
pub fn hex_rows(row: usize, bytes: &[u8]) -> Vec<TRow> {
    bytes
        .chunks(hex::ROW)
        .enumerate()
        .map(|(k, b)| {
            let r = row + k;
            let line = hex::row((r * hex::ROW) as u64, b);
            let cut = |a: usize, z: usize| line.get(a..z.min(line.len())).unwrap_or("").to_string();
            let spans = vec![
                TSpan {
                    text: cut(0, 8).into(),
                    colour: colour(0x4a4f57),
                    x: 0,
                },
                TSpan {
                    text: cut(10, 33).into(),
                    colour: colour(0xe6e8eb),
                    x: 9,
                },
                TSpan {
                    text: cut(35, line.len()).into(),
                    colour: colour(0x7d838c),
                    // one space between the columns (the board's two don't fit 360 dp in the text pane's size)
                    x: 33,
                },
            ];
            TRow {
                row: r as i32,
                no: "".into(),
                spans: model(spans),
            }
        })
        .collect()
}
