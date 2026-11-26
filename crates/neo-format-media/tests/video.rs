// SPDX-License-Identifier: Apache-2.0
//! FA12: the containers give every packet, and the decoders give ffmpeg's pictures bit for bit (tests/data/*.frames,
//! from make_video.sh).
use neo_format_media::av::{self, Codec, Kind, video};
use sha2::{Digest, Sha256};

fn data(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name),
    )
    .unwrap()
}

fn hash(f: &video::Frame) -> String {
    let mut h = Sha256::new();
    let rows = |h: &mut Sha256, p: &[u8], stride: usize, w: u32, n: u32| {
        for r in 0..n as usize {
            h.update(&p[r * stride..r * stride + w as usize]);
        }
    };
    rows(&mut h, &f.y, f.y_stride, f.width, f.height);
    rows(
        &mut h,
        &f.u,
        f.uv_stride,
        f.chroma_width(),
        f.chroma_height(),
    );
    rows(
        &mut h,
        &f.v,
        f.uv_stride,
        f.chroma_width(),
        f.chroma_height(),
    );
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Every picture of the file's video track, decoded.
fn pictures(file: &[u8]) -> Vec<video::Frame> {
    let mut d = av::open(file).unwrap();
    let (v, _) = av::pick(d.tracks());
    let v = v.unwrap();
    let mut dec = video::decoder(&d.tracks()[v]).unwrap();
    let mut out = Vec::new();
    while let Some(p) = d.next().unwrap() {
        if p.track == v {
            dec.decode(&p, &mut out).unwrap();
        }
    }
    dec.flush(&mut out);
    out
}

/// A picture comes out within a few packets of going in (the stream's reorder depth), not when the next key frame
/// ends its group: held to the group's end, the Pixel showed them late.
#[test]
fn h264_pictures_come_out_soon() {
    let file = data("h264-aac.mp4");
    let mut d = av::open(&file[..]).unwrap();
    let (v, _) = av::pick(d.tracks());
    let v = v.unwrap();
    let mut dec = video::decoder(&d.tracks()[v]).unwrap();
    let (mut out, mut held, mut most) = (Vec::new(), 0, 0);
    while let Some(p) = d.next().unwrap() {
        if p.track == v {
            let n = out.len();
            dec.decode(&p, &mut out).unwrap();
            held = if out.len() > n { 0 } else { held + 1 };
            most = most.max(held);
        }
    }
    assert!(!out.is_empty());
    assert!(most <= 4, "{most} packets in before a picture came out");
    assert!(
        out.windows(2).all(|w| w[0].pts_us < w[1].pts_us),
        "display order"
    );
}

fn same_as_ffmpeg(name: &str, codec: Codec) {
    let file = data(name);
    let d = av::open(&file[..]).unwrap();
    let (v, a) = av::pick(d.tracks());
    let t = &d.tracks()[v.unwrap()];
    assert_eq!((t.codec, t.width, t.height), (codec, 320, 240), "{name}");
    assert!(a.is_some(), "{name}: no sound track");
    assert!(
        (1_900_000..=2_100_000).contains(&d.duration_us()),
        "{name}: {}",
        d.duration_us()
    );
    let want: Vec<String> = String::from_utf8(data(&format!("{name}.frames")))
        .unwrap()
        .lines()
        .map(Into::into)
        .collect();
    let got = pictures(&file);
    assert_eq!(got.len(), want.len(), "{name}: pictures");
    for (i, (f, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(&hash(f), w, "{name}: picture {i}");
    }
    // display order, 25 fps
    for w in got.windows(2) {
        assert!(
            w[1].pts_us > w[0].pts_us,
            "{name}: {} then {}",
            w[0].pts_us,
            w[1].pts_us
        );
    }
}

#[test]
fn h264_in_mp4() {
    same_as_ffmpeg("h264-aac.mp4", Codec::H264);
}

#[test]
fn h264_baseline_in_3gp() {
    same_as_ffmpeg("h264-baseline.3gp", Codec::H264);
}

#[test]
fn seeking_lands_on_a_key_frame() {
    let file = data("h264-aac.mp4");
    let mut d = av::open(&file[..]).unwrap();
    let (v, _) = av::pick(d.tracks());
    for to in [0, 700_000, 1_300_000, 5_000_000] {
        let at = d.seek(to).unwrap();
        assert!(at <= to, "{at} after {to}");
        // the first video packet is a key frame at that time
        let p = std::iter::from_fn(|| d.next().unwrap()).find(|p| Some(p.track) == v);
        if to < 2_000_000 {
            let p = p.unwrap();
            assert!(
                p.key && p.pts_us as u64 == at,
                "{to}: {} {}",
                p.key,
                p.pts_us
            );
            assert!(
                to - at < 500_000,
                "a key frame every 12 pictures: {to} -> {at}"
            );
        }
    }
}

#[test]
fn h263_is_named_not_played() {
    let file = data("h263.3gp");
    let d = av::open(&file[..]).unwrap();
    let t = &d.tracks()[0];
    assert_eq!(
        (t.kind, t.codec.name(), t.codec.played()),
        (Kind::Video, "H.263", false)
    );
    assert!(video::decoder(t).is_err());
}

/// The file's sound track decoded: rate, channels, interleaved samples.
fn sound(file: &[u8]) -> (u32, u8, Vec<i16>) {
    let mut d = av::open(file).unwrap();
    let (_, a) = av::pick(d.tracks());
    let a = a.unwrap();
    let mut dec = av::sound::decoder(&d.tracks()[a]).unwrap();
    let mut out = Vec::new();
    while let Some(p) = d.next().unwrap() {
        if p.track == a {
            dec.decode(&p, &mut out).unwrap();
        }
    }
    (dec.rate(), dec.channels(), out)
}

/// The fixtures' tone: 440 Hz for 2 s, so 880 zero crossings a second in each channel, and loud.
fn is_the_tone(name: &str, rate: u32, channels: u8, s: &[i16]) {
    let ch = channels as usize;
    let left: Vec<i16> = s.iter().step_by(ch).copied().collect();
    let secs = left.len() as f64 / rate as f64;
    assert!((1.9..2.2).contains(&secs), "{name}: {secs} s");
    // skip the codec's ramp-in
    let body = &left[rate as usize / 10..left.len() - rate as usize / 10];
    let crossings = body.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count() as f64;
    let hz = crossings / 2.0 / (body.len() as f64 / rate as f64);
    assert!((430.0..450.0).contains(&hz), "{name}: {hz} Hz");
    let rms = (body.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>() / body.len() as f64).sqrt();
    assert!(rms > 1000.0, "{name}: rms {rms}");
}

#[test]
fn aac_sound() {
    for name in ["h264-aac.mp4", "h264-baseline.3gp"] {
        let (rate, channels, s) = sound(&data(name));
        is_the_tone(name, rate, channels, &s);
    }
}

#[test]
fn h264_in_matroska() {
    same_as_ffmpeg("h264-aac.mkv", Codec::H264);
    let (rate, channels, s) = sound(&data("h264-aac.mkv"));
    is_the_tone("h264-aac.mkv", rate, channels, &s);
}

#[test]
fn vp8_and_vorbis_in_webm() {
    same_as_ffmpeg("vp8-vorbis.webm", Codec::Vp8);
    let (rate, channels, s) = sound(&data("vp8-vorbis.webm"));
    assert_eq!((rate, channels), (48_000, 2));
    is_the_tone("vp8-vorbis.webm", rate, channels, &s);
}

#[test]
fn vp9_and_opus_in_webm() {
    same_as_ffmpeg("vp9-opus.webm", Codec::Vp9);
    let (rate, channels, s) = sound(&data("vp9-opus.webm"));
    assert_eq!(rate, 48_000);
    is_the_tone("vp9-opus.webm", rate, channels, &s);
}

#[test]
fn seeking_in_webm() {
    for name in ["vp8-vorbis.webm", "h264-aac.mkv"] {
        let file = data(name);
        let mut d = av::open(&file[..]).unwrap();
        let (v, _) = av::pick(d.tracks());
        for to in [1_300_000, 0, 700_000] {
            let at = d.seek(to).unwrap();
            assert!(at <= to && to - at < 600_000, "{name}: {to} -> {at}");
            let p = std::iter::from_fn(|| d.next().unwrap())
                .find(|p| Some(p.track) == v)
                .unwrap();
            assert!(
                p.key && p.pts_us as u64 == at,
                "{name} {to}: key {} at {}",
                p.key,
                p.pts_us
            );
        }
    }
}

/// Reads and decodes everything a (damaged) file gives; the result is ignored, only panics count.
fn play_all(file: &[u8]) {
    let Ok(mut d) = av::open(file) else { return };
    let tracks = d.tracks().to_vec();
    let mut vd: Vec<_> = tracks.iter().map(|t| video::decoder(t).ok()).collect();
    let mut sd: Vec<_> = tracks.iter().map(|t| av::sound::decoder(t).ok()).collect();
    let (mut frames, mut samples) = (Vec::new(), Vec::new());
    for _ in 0..2000 {
        match d.next() {
            Ok(Some(p)) => {
                if let Some(v) = vd[p.track].as_mut() {
                    let _ = v.decode(&p, &mut frames);
                    frames.clear();
                }
                if let Some(s) = sd[p.track].as_mut() {
                    let _ = s.decode(&p, &mut samples);
                    samples.clear();
                }
            }
            _ => break,
        }
    }
    let _ = d.seek(1_000_000);
    let _ = d.next();
}

/// xorshift: the same damage every run.
fn rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

#[test]
fn broken_files_never_panic() {
    for name in [
        "h264-aac.mp4",
        "h264-aac.mkv",
        "h264-baseline.3gp",
        "vp8-vorbis.webm",
        "vp9-opus.webm",
        "h263.3gp",
    ] {
        let good = data(name);
        let mut seed = 0x9e37_79b9_7f4a_7c15;
        for round in 0..24 {
            let mut f = good.clone();
            if round % 3 == 0 {
                f.truncate((rng(&mut seed) % good.len() as u64) as usize);
            } else {
                for _ in 0..(1 + round) {
                    let i = (rng(&mut seed) % f.len() as u64) as usize;
                    f[i] ^= 1 << (rng(&mut seed) % 8);
                }
            }
            let r = std::panic::catch_unwind(|| play_all(&f));
            assert!(r.is_ok(), "{name}: round {round} panicked");
        }
    }
}
