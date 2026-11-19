// SPDX-License-Identifier: Apache-2.0
// The images are made here (no camera files from real people): our encoders, the `png` crate, hand-built GIF, BMP,
// EXIF and SVG bytes.
use neo_format_image::encode::{jpeg, png as png_out};
use neo_format_image::ops::{Edit, UNIT, View, apply};
use neo_format_image::shrink::{Rows, Turn, fit, resize, thumb, turned};
use neo_format_image::{Error, Format, Pixels, cost, decode, probe};

/// Four flat quadrants, `w`×`h`: red, green / blue, white.
fn quads(w: u32, h: u32) -> Pixels {
    let mut p = Pixels::new(w, h, 3);
    for y in 0..h {
        for x in 0..w {
            let c = match (x < w / 2, y < h / 2) {
                (true, true) => [255, 0, 0],
                (false, true) => [0, 255, 0],
                (true, false) => [0, 0, 255],
                (false, false) => [255, 255, 255],
            };
            let i = ((y * w + x) * 3) as usize;
            p.px[i..i + 3].copy_from_slice(&c);
        }
    }
    p
}

fn gradient(w: u32, h: u32, ch: u8) -> Pixels {
    let mut p = Pixels::new(w, h, ch);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * ch as u32) as usize;
            p.px[i] = (x * 255 / w.max(2)) as u8;
            p.px[i + 1] = (y * 255 / h.max(2)) as u8;
            p.px[i + 2] = ((x + y) * 3) as u8;
            if ch == 4 {
                p.px[i + 3] = (x * 7 + y) as u8;
            }
        }
    }
    p
}

fn near(a: [u8; 4], b: [u8; 3]) -> bool {
    (0..3).all(|i| (a[i] as i32 - b[i] as i32).abs() < 40)
}

fn le16(v: u16) -> [u8; 2] {
    v.to_le_bytes()
}
fn le32(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

/// An APP1 EXIF block: orientation, DateTimeOriginal and (optionally) a preview JPEG.
fn exif(orient: u16, thumb: Option<&[u8]>) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"II*\0");
    t.extend_from_slice(&le32(8));
    // IFD0 at 8: orientation, the EXIF IFD at 38; IFD1 at 76
    t.extend_from_slice(&le16(2));
    t.extend_from_slice(&[0x12, 0x01, 3, 0]);
    t.extend_from_slice(&le32(1));
    t.extend_from_slice(&le16(orient));
    t.extend_from_slice(&[0, 0]);
    t.extend_from_slice(&[0x69, 0x87, 4, 0]);
    t.extend_from_slice(&le32(1));
    t.extend_from_slice(&le32(38));
    t.extend_from_slice(&le32(if thumb.is_some() { 76 } else { 0 }));
    // the EXIF IFD at 38: DateTimeOriginal at 56
    t.extend_from_slice(&le16(1));
    t.extend_from_slice(&[0x03, 0x90, 2, 0]);
    t.extend_from_slice(&le32(20));
    t.extend_from_slice(&le32(56));
    t.extend_from_slice(&le32(0));
    t.extend_from_slice(b"2026:10:29 18:42:10\0");
    assert_eq!(t.len(), 76);
    if let Some(th) = thumb {
        t.extend_from_slice(&le16(2));
        t.extend_from_slice(&[0x01, 0x02, 4, 0]);
        t.extend_from_slice(&le32(1));
        t.extend_from_slice(&le32(106));
        t.extend_from_slice(&[0x02, 0x02, 4, 0]);
        t.extend_from_slice(&le32(1));
        t.extend_from_slice(&le32(th.len() as u32));
        t.extend_from_slice(&le32(0));
        t.extend_from_slice(th);
    }
    let mut a = b"Exif\0\0".to_vec();
    a.extend_from_slice(&t);
    a
}

fn jpeg_with(p: &Pixels, app1: Option<Vec<u8>>, progressive: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut e = jpeg_encoder::Encoder::new(&mut out, 95);
    if let Some(a) = app1 {
        e.add_app_segment(1, a).unwrap();
    }
    e.set_progressive(progressive);
    e.encode(&p.px, p.w as u16, p.h as u16, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    out
}

#[test]
fn jpeg_round_trip_and_psnr() {
    let p = gradient(64, 48, 3);
    let j = jpeg(&p, 90).unwrap();
    let i = probe(&j).unwrap();
    assert_eq!(
        (i.format, i.w, i.h, i.progressive),
        (Format::Jpeg, 64, 48, false)
    );
    let d = decode(&j, 1 << 20).unwrap();
    assert!(!d.preview);
    let mse: f64 =
        p.px.iter()
            .zip(&d.px.px)
            .map(|(a, b)| (*a as f64 - *b as f64).powi(2))
            .sum::<f64>()
            / p.px.len() as f64;
    let psnr = 10.0 * (255.0f64 * 255.0 / mse).log10();
    assert!(psnr > 30.0, "psnr {psnr}");
    let pj = jpeg_with(&p, None, true);
    assert!(probe(&pj).unwrap().progressive);
    assert_eq!(decode(&pj, 1 << 20).unwrap().px.w, 64);
}

#[test]
fn exif_orientation_date_and_every_turn() {
    let p = quads(32, 16);
    for o in 1..=8u16 {
        let j = jpeg_with(&p, Some(exif(o, None)), false);
        let i = probe(&j).unwrap();
        assert_eq!(i.orientation, o as u8);
        assert_eq!(i.taken.as_deref(), Some("2026:10:29 18:42:10"));
        let d = decode(&j, 1 << 20).unwrap();
        let t = Turn::exif(i.orientation);
        let f = fit(&d.px, t, 1000);
        assert_eq!((f.w, f.h), i.shown());
        // where the red quadrant (stored top-left) ends up, by the EXIF definition
        let (sw, sh) = (f.w, f.h);
        let red_at = match o {
            1 => (0, 0),
            2 => (sw - 1, 0),
            3 => (sw - 1, sh - 1),
            4 => (0, sh - 1),
            5 => (0, 0),
            6 => (sw - 1, 0),
            7 => (sw - 1, sh - 1),
            _ => (0, sh - 1),
        };
        assert!(
            near(f.rgba(red_at.0, red_at.1), [255, 0, 0]),
            "orientation {o}: {:?}",
            f.rgba(red_at.0, red_at.1)
        );
    }
}

#[test]
fn turns_compose_and_invert() {
    let p = gradient(5, 3, 3);
    let all: Vec<Turn> = (0..4)
        .flat_map(|r| [false, true].map(|f| Turn { rot: r, flip: f }))
        .collect();
    for &a in &all {
        let (tw, th) = a.size(5, 3);
        for y in 0..th {
            for x in 0..tw {
                let (sx, sy) = a.src(x, y, 5, 3);
                assert_eq!(a.dst(sx, sy, 5, 3), (x, y));
            }
        }
        for &b in &all {
            assert_eq!(
                turned(&turned(&p, a), b),
                turned(&p, a.then(b)),
                "{a:?} then {b:?}"
            );
        }
    }
    for o in 1..=8 {
        let t = Turn::exif(o);
        assert_eq!(
            t.then(t).then(t).then(t).rot % 4,
            if t.flip { 0 } else { (t.rot * 4) % 4 }
        );
    }
}

#[test]
fn exif_preview_stands_in_when_too_big() {
    let small = quads(16, 8);
    let th = jpeg_with(&small, None, false);
    let big = quads(256, 128);
    let j = jpeg_with(&big, Some(exif(6, Some(&th))), true);
    let i = probe(&j).unwrap();
    assert!(i.progressive);
    let r = i.thumb.clone().unwrap();
    assert_eq!(&j[r], &th[..]);
    let budget = cost(&i) - 1;
    let d = decode(&j, budget).unwrap();
    assert!(d.preview);
    assert_eq!((d.px.w, d.px.h), (16, 8));
    // no preview: too big
    let j2 = jpeg_with(&big, None, true);
    assert_eq!(decode(&j2, budget).err(), Some(Error::TooBig));
}

#[test]
fn png_ours_round_trips_and_png_crate_files_decode() {
    for ch in [3, 4] {
        let p = gradient(37, 11, ch);
        let b = png_out(&p);
        let d = decode(&b, 1 << 20).unwrap();
        assert_eq!(d.px, p);
        assert_eq!(probe(&b).unwrap().alpha, ch == 4);
    }
    // grey, 16-bit grey + alpha, and a palette, written by the png crate
    let enc =
        |w: u32, h: u32, ct: png::ColorType, bd: png::BitDepth, data: &[u8], pal: Option<&[u8]>| {
            let mut out = Vec::new();
            let mut e = png::Encoder::new(&mut out, w, h);
            e.set_color(ct);
            e.set_depth(bd);
            if let Some(p) = pal {
                e.set_palette(p.to_vec());
            }
            e.write_header().unwrap().write_image_data(data).unwrap();
            out
        };
    let g = enc(
        2,
        1,
        png::ColorType::Grayscale,
        png::BitDepth::Eight,
        &[10, 200],
        None,
    );
    assert_eq!(
        decode(&g, 1 << 20).unwrap().px.px,
        [10, 10, 10, 200, 200, 200]
    );
    let ga = enc(
        1,
        1,
        png::ColorType::GrayscaleAlpha,
        png::BitDepth::Sixteen,
        &[0x80, 0, 0xFF, 0xFF],
        None,
    );
    assert_eq!(
        decode(&ga, 1 << 20).unwrap().px.px,
        [0x80, 0x80, 0x80, 0xFF]
    );
    let pl = enc(
        2,
        1,
        png::ColorType::Indexed,
        png::BitDepth::Eight,
        &[1, 0],
        Some(&[1, 2, 3, 4, 5, 6]),
    );
    let d = decode(&pl, 1 << 20).unwrap();
    assert_eq!(&d.px.px[..], &[4, 5, 6, 1, 2, 3][..]);
}

/// A GIF, 4×2: frame 1 fills it (red | green), frame 2 paints (0,0) blue with index 2 and leaves the rest
/// transparent (index 3); a palette of four.
fn gif2() -> Vec<u8> {
    let mut g = b"GIF89a".to_vec();
    g.extend_from_slice(&le16(4));
    g.extend_from_slice(&le16(2));
    g.extend_from_slice(&[0x81, 0, 0]); // global table of 4
    g.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 9, 9, 9]);
    g.extend_from_slice(b"\x21\xFF\x0BNETSCAPE2.0\x03\x01\x00\x00\x00");
    let frame = |g: &mut Vec<u8>, idx: &[u8], trans: Option<u8>, delay: u16| {
        g.extend_from_slice(&[0x21, 0xF9, 4, if trans.is_some() { 1 } else { 0 }]);
        g.extend_from_slice(&le16(delay));
        g.extend_from_slice(&[trans.unwrap_or(0), 0]);
        g.push(0x2C);
        g.extend_from_slice(&le16(0));
        g.extend_from_slice(&le16(0));
        g.extend_from_slice(&le16(4));
        g.extend_from_slice(&le16(2));
        g.push(0);
        g.push(2);
        let data = weezl::encode::Encoder::new(weezl::BitOrder::Lsb, 2)
            .encode(idx)
            .unwrap();
        for c in data.chunks(255) {
            g.push(c.len() as u8);
            g.extend_from_slice(c);
        }
        g.push(0);
    };
    frame(&mut g, &[0, 0, 1, 1, 0, 0, 1, 1], None, 5);
    frame(&mut g, &[2, 3, 3, 3, 3, 3, 3, 3], Some(3), 0);
    g.push(0x3B);
    g
}

#[test]
fn gif_frames_transparency_and_delays() {
    let g = gif2();
    let i = probe(&g).unwrap();
    assert_eq!((i.format, i.w, i.h, i.animated), (Format::Gif, 4, 2, true));
    let d = decode(&g, 1 << 20).unwrap();
    assert_eq!(d.px.rgba(0, 0), [255, 0, 0, 255]);
    assert_eq!(d.px.rgba(3, 1), [0, 255, 0, 255]);
    let mut a = neo_format_image::gif::Gif::new(&g).unwrap();
    assert_eq!(a.step().unwrap(), Some(50));
    assert_eq!(a.step().unwrap(), Some(100));
    assert_eq!(a.canvas().rgba(0, 0), [0, 0, 255, 255]);
    assert_eq!(a.canvas().rgba(1, 0), [255, 0, 0, 255]);
    assert_eq!(a.step().unwrap(), None);
    a.rewind();
    assert_eq!(a.step().unwrap(), Some(50));
}

fn bmp(w: u32, h: i32, bpp: u16, palette: &[[u8; 4]], rows: &[Vec<u8>]) -> Vec<u8> {
    let off = 14 + 40 + palette.len() * 4;
    let mut b = b"BM".to_vec();
    let data: Vec<u8> = rows.concat();
    b.extend_from_slice(&le32((off + data.len()) as u32));
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&le32(off as u32));
    b.extend_from_slice(&le32(40));
    b.extend_from_slice(&le32(w));
    b.extend_from_slice(&h.to_le_bytes());
    b.extend_from_slice(&le16(1));
    b.extend_from_slice(&le16(bpp));
    b.extend_from_slice(&le32(0));
    b.extend_from_slice(&le32(data.len() as u32));
    b.extend_from_slice(&[0; 8]);
    b.extend_from_slice(&le32(palette.len() as u32));
    b.extend_from_slice(&le32(0));
    for p in palette {
        b.extend_from_slice(p);
    }
    b.extend_from_slice(&data);
    b
}

#[test]
fn bmp_depths_and_row_order() {
    // 24-bit, bottom-up, 3 wide (9 bytes + 3 padding): bottom row first
    let b = bmp(
        3,
        2,
        24,
        &[],
        &[
            vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0],
            vec![0, 0, 0, 255, 255, 255, 1, 2, 3, 0, 0, 0],
        ],
    );
    let d = decode(&b, 1 << 20).unwrap().px;
    assert_eq!(d.rgba(0, 1), [0, 0, 255, 255]);
    assert_eq!(d.rgba(2, 1), [255, 0, 0, 255]);
    assert_eq!(d.rgba(1, 0), [255, 255, 255, 255]);
    assert_eq!(d.rgba(2, 0), [3, 2, 1, 255]);
    // 8-bit palette, top-down
    let b = bmp(
        2,
        -1,
        8,
        &[[0, 0, 255, 0], [255, 0, 0, 0]],
        &[vec![1, 0, 0, 0]],
    );
    let d = decode(&b, 1 << 20).unwrap().px;
    assert_eq!(
        (d.rgba(0, 0), d.rgba(1, 0)),
        ([0, 0, 255, 255], [255, 0, 0, 255])
    );
    // 32-bit with alpha all zero: opaque
    let b = bmp(1, 1, 32, &[], &[vec![10, 20, 30, 0]]);
    assert_eq!(
        decode(&b, 1 << 20).unwrap().px.rgba(0, 0),
        [30, 20, 10, 255]
    );
    // 1-bit
    let b = bmp(
        3,
        1,
        1,
        &[[0, 0, 0, 0], [255, 255, 255, 0]],
        &[vec![0b1010_0000, 0, 0, 0]],
    );
    let d = decode(&b, 1 << 20).unwrap().px;
    assert_eq!(
        (d.rgba(0, 0)[0], d.rgba(1, 0)[0], d.rgba(2, 0)[0]),
        (255, 0, 255)
    );
}

#[test]
fn svg_subset_draws_and_skips() {
    let s = br##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 20 10">
  <defs><rect id="hidden" width="20" height="10" fill="#00ff00"/></defs>
  <rect x="0" y="0" width="10" height="10" fill="#f00"/>
  <g transform="translate(10 0)" style="fill:blue">
    <circle cx="5" cy="5" r="4"/>
    <text x="0" y="5">ignored</text>
  </g>
  <path d="M0 0 L1 0 1 1 Z m2 0 h1 v1 h-1z" fill="none" stroke="black" stroke-width=".1"/>
</svg>"##;
    let i = probe(s).unwrap();
    assert_eq!((i.format, i.w, i.h), (Format::Svg, 200, 100));
    let d = decode(s, 64 << 20).unwrap().px;
    assert_eq!((d.w, d.h), (1024, 512));
    assert_eq!(d.rgba(256, 256), [255, 0, 0, 255]);
    assert_eq!(d.rgba(768, 256), [0, 0, 255, 255]);
    // the circle's corner is empty: transparent, not the defs' green
    assert_eq!(d.rgba(1020, 4)[3], 0);
    assert!(probe(b"<html><svg/></html>").is_none());
    assert!(probe(b"plain text").is_none());
}

/// A deterministic shuffle of bytes: truncations and flips must give errors, never panics.
#[test]
fn corrupt_files_never_panic() {
    let p = gradient(24, 16, 3);
    let files: Vec<Vec<u8>> = vec![
        jpeg(&p, 80).unwrap(),
        jpeg_with(&quads(32, 16), Some(exif(6, Some(&jpeg(&p, 50).unwrap()))), true),
        png_out(&gradient(24, 16, 4)),
        gif2(),
        bmp(3, 2, 24, &[], &[vec![1; 12], vec![2; 12]]),
        br#"<svg viewBox="0 0 4 4"><path d="M0 0 A2 2 0 1 1 4 4 Q1 1 2 2 T3 3 S1 1 2 2 C1 1 1 1 1 1 z"/></svg>"#.to_vec(),
    ];
    let mut seed = 0x1234_5678u32;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for f in &files {
        for cut in (0..f.len()).step_by(7) {
            let _ = decode(&f[..cut], 1 << 20);
        }
        for _ in 0..300 {
            let mut g = f.clone();
            for _ in 0..1 + rnd() % 4 {
                let i = rnd() as usize % g.len();
                g[i] = rnd() as u8;
            }
            let _ = decode(&g, 1 << 20);
            if let Ok(mut a) = neo_format_image::gif::Gif::new(&g) {
                for _ in 0..4 {
                    if !matches!(a.step(), Ok(Some(_))) {
                        break;
                    }
                }
            }
        }
    }
}

#[test]
fn edits_view_rows_match_apply_and_turn_with_the_crop() {
    let p = gradient(40, 30, 3);
    let base = Turn::exif(6);
    let e = Edit {
        crop: [1000, 2000, 5000, 4000],
        ..Edit::NONE
    };
    let a = apply(&p, base, &e);
    let v = View::new(&p, base, &e);
    assert_eq!((v.w(), v.h()), (a.w, a.h));
    assert_eq!(e.out_size(base, 40, 30), (a.w, a.h));
    // rotating the edit turns the same cropped pixels
    let r = apply(&p, base, &e.rotate());
    assert_eq!(
        r,
        turned(
            &a,
            Turn {
                rot: 1,
                flip: false
            }
        )
    );
    let f = apply(&p, base, &e.flip());
    assert_eq!(f, turned(&a, Turn { rot: 0, flip: true }));
    // four turns and two flips: back where it started
    let back = e.rotate().rotate().rotate().rotate().flip().flip();
    assert_eq!(back.crop, e.crop);
    assert_eq!(apply(&p, base, &back), a);
    // a resize
    let s = apply(
        &p,
        base,
        &Edit {
            size: Some((7, 5)),
            ..e
        },
    );
    assert_eq!((s.w, s.h), (7, 5));
    assert_eq!(Edit::NONE.crop, [0, 0, UNIT, UNIT]);
    // the encoders take a view
    let j = jpeg(&v, 90).unwrap();
    assert_eq!(probe(&j).unwrap().w, a.w);
    let pn = png_out(&v);
    assert_eq!(decode(&pn, 1 << 20).unwrap().px, a);
}

#[test]
fn shrink_fit_thumb_and_resize() {
    let p = quads(400, 300);
    let f = fit(&p, Turn::NONE, 100);
    assert_eq!((f.w, f.h), (100, 75));
    assert!(near(f.rgba(10, 10), [255, 0, 0]));
    let t = thumb(&p, Turn::exif(6), 64);
    assert_eq!((t.w, t.h), (64, 64));
    let r = resize(&p, 33, 17);
    assert_eq!((r.w, r.h), (33, 17));
    assert!(near(r.rgba(32, 16), [255, 255, 255]));
    let up = resize(&quads(2, 2), 8, 8);
    assert!(near(up.rgba(0, 0), [255, 0, 0]));
}

/// The gate's size on the host, for comparing with the Pixel (`cargo test --release -- --ignored --nocapture`).
#[test]
#[ignore]
fn time_a_12_mp_jpeg() {
    let p = gradient(4032, 3024, 3);
    let j = jpeg(&p, 90).unwrap();
    let t = std::time::Instant::now();
    let d = decode(&j, 44 << 20).unwrap();
    let dec = t.elapsed();
    let f = fit(&d.px, Turn::exif(6), 2560);
    println!(
        "12 MP: {} KB, decode {:?}, fit {:?} → {}×{}",
        j.len() / 1024,
        dec,
        t.elapsed() - dec,
        f.w,
        f.h
    );
}

#[test]
fn webp_lossless_decodes_exactly() {
    for ch in [3u8, 4] {
        let p = gradient(19, 7, ch);
        let mut out = Vec::new();
        let ct = if ch == 4 {
            image_webp::ColorType::Rgba8
        } else {
            image_webp::ColorType::Rgb8
        };
        image_webp::WebPEncoder::new(&mut out)
            .encode(&p.px, 19, 7, ct)
            .unwrap();
        let i = probe(&out).unwrap();
        assert_eq!((i.format, i.w, i.h), (Format::WebP, 19, 7));
        let d = decode(&out, 1 << 20).unwrap();
        assert_eq!(d.px, p, "{ch} channels");
        for cut in (0..out.len()).step_by(5) {
            let _ = decode(&out[..cut], 1 << 20);
        }
    }
}

#[test]
fn decode_into_the_callers_memory_matches_decode() {
    let p = gradient(40, 24, 3);
    for b in [jpeg(&p, 90).unwrap(), jpeg_with(&p, None, true)] {
        let i = probe(&b).unwrap();
        let (w, h, ch) = neo_format_image::into_shape(&b, &i).unwrap();
        let mut out = vec![0u8; (w * h * ch as u32) as usize];
        let d = neo_format_image::decode_into(&b, &i, &mut out).unwrap();
        assert_eq!((d.w, d.h, d.ch), (40, 24, 3));
        let f = fit(&d, Turn::NONE, 20);
        assert_eq!((f.w, f.h), (20, 12));
        assert_eq!(out, decode(&b, 1 << 20).unwrap().px.px);
    }
    // a PNG decodes on the heap
    let pn = png_out(&p);
    assert!(neo_format_image::into_shape(&pn, &probe(&pn).unwrap()).is_none());
    assert!(
        neo_format_image::heap_cost(&probe(&jpeg_with(&p, None, true)).unwrap()) >= 40 * 24 * 6
    );
}
