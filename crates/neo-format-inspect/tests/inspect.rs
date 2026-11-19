// SPDX-License-Identifier: Apache-2.0
//! The inspectors against files other tools made (`tests/data/make.py`), real packages when this machine has them,
//! and broken bytes.

use neo_format_inspect::cert::{self, Validity};
use neo_format_inspect::font::Font;
use neo_format_inspect::{Error, hex, jad, sis};
use std::path::PathBuf;

fn data(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name),
    )
    .unwrap()
}

fn time(year: u16, month: u8, day: u8) -> cert::Time {
    cert::Time {
        year,
        month,
        day,
        hour: 0,
        minute: 0,
        second: 0,
    }
}

#[test]
fn hex_rows() {
    assert_eq!(
        hex::row(0, b"\x7fELF\x01\x01\x01\x00"),
        "00000000  7F 45 4C 46 01 01 01 00  .ELF...."
    );
    assert_eq!(
        hex::row(0x10, b"Hi\n"),
        "00000010  48 69 0A                 Hi."
    );
    assert_eq!(
        hex::row(0x10, b"Hi\n").len(),
        hex::row(0, b"\x7fELF\x01\x01\x01\x00").len() - 5
    );
    assert_eq!(
        hex::row(0x1_2345_6789, b"0123456789"),
        "123456789  30 31 32 33 34 35 36 37  01234567"
    );
    assert_eq!(hex::row(8, b""), "00000008                           ");
}

#[test]
fn font_info_and_coverage() {
    let bytes = data("DejaVuSans-subset.ttf");
    let f = Font::new(&bytes).unwrap();
    let i = f.info();
    assert_eq!(i.family, "DejaVu Sans");
    assert_eq!(i.subfamily, "Book");
    assert_eq!(i.version, "2.37");
    assert_eq!((i.format, i.faces), ("TrueType", 1));
    assert!(i.glyphs > 300, "{}", i.glyphs);
    for s in ["Latin", "Cyrillic", "Armenian"] {
        assert!(i.scripts.contains(&s), "{:?}", i.scripts);
    }
    assert!(!i.scripts.contains(&"Greek") && !i.scripts.contains(&"CJK"));
    assert!(f.covers("Hello, Привет, Բարեւ!"));
    assert!(!f.covers("Γεια"));
    let chars = f.chars();
    assert!(chars.windows(2).all(|w| w[0] < w[1]));
    assert!(chars.contains(&'Ա') && chars.contains(&'я') && !chars.contains(&'Ω'));
}

#[test]
fn font_render() {
    let bytes = data("DejaVuSans-subset.ttf");
    let mut f = Font::new(&bytes).unwrap();
    let b = f.render_line("Aa", 40.0, 1000);
    // DejaVu: ascender 1901, descender -483, 2048 units an em
    assert_eq!(b.height, 47);
    assert!((45..=60).contains(&b.width), "{}", b.width);
    assert_eq!(b.alpha.len(), (b.width * b.height) as usize);
    let ink: f32 = b.alpha.iter().map(|&a| a as f32 / 255.0).sum::<f32>() / b.alpha.len() as f32;
    assert!((0.08..0.5).contains(&ink), "{ink}");
    assert!(b.alpha.contains(&255));
    // the cache gives the same pixels
    assert_eq!(f.render_line("Aa", 40.0, 1000), b);
    let clipped = f.render_line("A long line of sample text", 40.0, 120);
    assert_eq!(clipped.width, 120);
    assert_eq!(&clipped.alpha[..10], &b.alpha[..10]);
    let c = f.render_char('Ж', 30.0, 48);
    assert_eq!((c.width, c.height, c.alpha.len()), (48, 48, 48 * 48));
    let col = |x: usize| (0..48).map(|y| c.alpha[y * 48 + x] as u32).sum::<u32>();
    assert!(col(0) == 0 && col(47) == 0 && col(24) > 0, "centred");
    // a character the font lacks draws .notdef, a silly size is clamped
    assert!(f.render_char('Ω', 30.0, 48).alpha.iter().any(|&a| a > 0));
    assert!(f.render_line("x", f32::NAN, 100).height > 0);
}

#[test]
fn not_a_font() {
    assert!(matches!(
        Font::new(b"hello world, not a font"),
        Err(Error::Bad(_))
    ));
    assert!(Font::new(&data("DejaVuSans-subset.ttf")[..100]).is_err());
}

#[test]
fn certificates() {
    let root = &cert::parse(&data("root-ca.pem")).unwrap()[0];
    assert_eq!(root.subject.cn.as_deref(), Some("Home Lab Root CA"));
    assert_eq!(root.subject.o.as_deref(), Some("notallow.cc"));
    assert_eq!(root.subject.c.as_deref(), Some("AM"));
    assert_eq!(root.subject, root.issuer);
    assert!(root.self_signed && root.ca);
    assert_eq!(root.key, "EC P-256");
    assert_eq!(root.signature, "SHA-256 with ECDSA");
    assert_eq!(root.serial, "4E:45:4F:01");
    assert_eq!(root.not_before, time(2026, 1, 1));
    assert_eq!(
        root.not_after,
        cert::Time {
            hour: 23,
            minute: 59,
            second: 59,
            ..time(2035, 12, 31)
        }
    );
    assert!(root.san.is_empty());
    assert_eq!(
        cert::parse(&data("root-ca.der")).unwrap(),
        std::slice::from_ref(root)
    );

    let leaf = &cert::parse(&data("leaf.pem")).unwrap()[0];
    assert_eq!(leaf.subject.cn.as_deref(), Some("files.notallow.cc"));
    assert_eq!(leaf.subject.ou.as_deref(), Some("Files"));
    assert_eq!(leaf.issuer, root.subject);
    assert!(!leaf.self_signed && !leaf.ca);
    assert_eq!(leaf.key, "RSA 2048");
    assert_eq!(leaf.signature, "SHA-256 with ECDSA");
    assert_eq!(
        leaf.san,
        ["files.notallow.cc", "*.notallow.cc", "192.168.3.20"]
    );
    assert_eq!(leaf.serial, "10:01");

    let bundle = cert::parse(&data("bundle.pem")).unwrap();
    assert_eq!(bundle, [root.clone(), leaf.clone()]);

    let fps = String::from_utf8(data("fingerprints.txt")).unwrap();
    for (c, line) in [root, leaf].into_iter().zip(fps.lines()) {
        let lines = cert::fingerprint_lines(&c.sha256);
        assert!(lines[0].ends_with(':') && lines[0].len() == 48);
        assert_eq!(lines.concat(), line.split_once(' ').unwrap().1);
    }

    assert_eq!(time(2026, 1, 1).unix(), 1_767_225_600);
    let at = |y, m, d| time(y, m, d).unix();
    assert_eq!(cert::validity(root, at(2025, 6, 1)), Validity::NotYet);
    assert_eq!(
        cert::validity(root, at(2030, 1, 1)),
        Validity::Valid { days_left: 2190 }
    );
    assert_eq!(
        cert::validity(root, at(2036, 1, 31)),
        Validity::Expired { days_ago: 30 }
    );

    assert_eq!(cert::parse(b""), Err(Error::Bad("Not a certificate")));
    assert_eq!(
        cert::parse(b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n").unwrap_err(),
        Error::Bad("The certificate is damaged")
    );
    assert!(cert::parse(&data("root-ca.der")[..200]).is_err());
}

#[test]
fn sis_package() {
    let file = data("sample.sisx");
    let need = sis::needed(&file[..64]).unwrap();
    assert!(need < file.len(), "{need} of {}", file.len());
    let p = sis::read(&file[..need]).unwrap();
    assert_eq!(p, sis::read(&file).unwrap());
    assert_eq!(p.name, "X-plore");
    assert_eq!(p.vendor, "Lonely Cat Games");
    assert_eq!(p.version, (1, 58, 0));
    assert_eq!(p.uid, 0xA000_0BCD);
    assert_eq!(p.platforms, ["S60 3rd Edition"]);
    assert_eq!(p.drive, None);
    let caps: Vec<_> = p.capabilities.iter().map(|c| c.name).collect();
    assert_eq!(
        caps,
        [
            "NetworkServices",
            "LocalServices",
            "ReadUserData",
            "WriteUserData"
        ]
    );
    assert_eq!(
        p.capabilities[2].text,
        "Read your contacts, messages and files"
    );
    assert_eq!(p.signer, Some(("Symbian CA I".into(), time(2012, 1, 1))));
    assert_eq!(p.files, 2);

    assert_eq!(sis::read(&file[..need - 1]), Err(Error::Truncated));
    assert_eq!(sis::needed(&file[..30]), Err(Error::Truncated));
    assert_eq!(
        sis::read(b"PK\x03\x04 not a package at all"),
        Err(Error::Bad("Not a Symbian package"))
    );
}

/// Real packages from NeoSymbian/legacy's lock file, when `build/.sis-cache` has them (never copied here).
#[test]
fn real_sis_when_present() {
    let cache = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build/.sis-cache");
    let Ok(dir) = std::fs::read_dir(&cache) else {
        eprintln!("no {}: skipped", cache.display());
        return;
    };
    for e in dir.flatten() {
        let bytes = std::fs::read(e.path()).unwrap();
        let need = sis::needed(&bytes[..64]).unwrap();
        let p = sis::read(&bytes[..need]).unwrap();
        eprintln!(
            "{}: {need} of {} bytes: {p:?}",
            e.path().display(),
            bytes.len()
        );
        assert!(!p.name.is_empty() && p.files > 0 && !p.platforms.is_empty());
    }
}

#[test]
fn java_descriptors() {
    let m = jad::parse(std::str::from_utf8(&data("midlet.jad")).unwrap()).unwrap();
    assert_eq!(m.name, "Pocket Chess");
    assert_eq!(m.vendor, "NeoSymbian Tests");
    assert_eq!(m.version, "2.1.0");
    assert_eq!(m.profile, "MIDP-2.0");
    assert_eq!(
        m.permissions,
        [
            "javax.microedition.io.Connector.http",
            "javax.microedition.io.Connector.https",
            "javax.microedition.io.Connector.sms"
        ]
    );
    assert_eq!(m.jar_size, Some(48213));

    let j = jad::from_jar(&data("midlet.jar")).unwrap();
    assert_eq!(
        (j.name.as_str(), j.profile.as_str()),
        ("Pocket Chess", "MIDP-2.1")
    );
    assert_eq!(j.permissions, ["javax.microedition.io.Connector.http"]);
    assert_eq!(j.jar_size, None);

    assert!(jad::parse("MIDlet-Vendor: x\n").is_err());
    assert!(jad::from_jar(b"not a zip").is_err());
}

#[test]
fn errors_read_as_sentences() {
    assert_eq!(Error::Truncated.to_string(), "The file is cut short");
    assert_eq!(Error::Bad("Not a font").to_string(), "Not a font");
    assert_eq!(
        Error::Unsupported("a Symbian 8 package").to_string(),
        "Files can't read a Symbian 8 package"
    );
}

/// Cut short or with flipped bytes, every fixture through every parser gives `Err` or a result, never a panic.
#[test]
fn broken_bytes_never_panic() {
    let files = [
        "DejaVuSans-subset.ttf",
        "root-ca.pem",
        "root-ca.der",
        "leaf.pem",
        "bundle.pem",
        "sample.sisx",
        "midlet.jad",
        "midlet.jar",
    ];
    let mut seed = 0x2545_f491u32;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for f in files {
        let good = data(f);
        let mut cases: Vec<Vec<u8>> = (1..good.len())
            .step_by(good.len() / 40 + 1)
            .map(|n| good[..n].to_vec())
            .collect();
        for _ in 0..300 {
            let mut b = good.clone();
            for _ in 0..1 + rnd() % 4 {
                let i = rnd() as usize % b.len();
                b[i] = rnd() as u8;
            }
            cases.push(b);
        }
        for (k, b) in cases.iter().enumerate() {
            if let Ok(mut font) = Font::new(b) {
                let _ = font.info();
                let _ = font.covers("Aa Ձ");
                let _ = font.render_line("Aa Ձя!", 20.0, 200);
                let _ = font.render_char('W', 64.0, 72);
                if k % 40 == 0 {
                    let _ = font.chars();
                }
            }
            let _ = cert::parse(b);
            let _ = sis::needed(b);
            let _ = sis::read(b);
            let _ = jad::parse(&String::from_utf8_lossy(b));
            let _ = jad::from_jar(b);
            let _ = hex::row(u64::MAX, b);
        }
    }
}
