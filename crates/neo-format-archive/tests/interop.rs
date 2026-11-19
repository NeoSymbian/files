// SPDX-License-Identifier: Apache-2.0
//! Archives made by other tools (`tests/data/make.py`), the zip-slip names, broken bytes, and our own archives read
//! by the host's `unzip` and `tar` (skipped when they aren't there).

use neo_format_archive::gzip::{GzReader, GzWriter};
use neo_format_archive::tar::{TarReader, TarWriter};
use neo_format_archive::zip::{self, ZipWriter};
use neo_format_archive::{At, Error, Format, Kind, Member, Method, Result, Stream, detect, gzip};
use std::path::PathBuf;
use std::process::Command;

fn data(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name),
    )
    .unwrap()
}

fn drain(s: &mut impl Stream) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 3000];
    loop {
        let n = s.read(&mut buf)?;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..n]);
    }
}

fn unzip_all(b: &[u8]) -> Result<Vec<(String, Kind, Vec<u8>)>> {
    zip::list(b)?
        .into_iter()
        .map(|m| {
            let d = if m.kind == Kind::File && m.extractable() {
                drain(&mut zip::open(b, &m)?)?
            } else {
                Vec::new()
            };
            Ok((m.name, m.kind, d))
        })
        .collect()
}

fn untar(s: impl Stream) -> Result<Vec<(Member, Vec<u8>)>> {
    let mut r = TarReader::new(s);
    let mut out = Vec::new();
    while let Some(m) = r.next_member()? {
        let d = drain(&mut r)?;
        out.push((m, d));
    }
    Ok(out)
}

const TEXT_LEN: usize = 3800;

#[test]
fn info_zip_deflated_stored_and_streamed() {
    for f in ["infozip.zip", "stored.zip", "stream.zip"] {
        let b = data(f);
        assert_eq!(detect(&b, f), Some(Format::Zip));
        let all = unzip_all(&b).unwrap();
        let get = |n: &str| {
            all.iter()
                .find(|m| m.0 == n)
                .unwrap_or_else(|| panic!("{f}: no {n}"))
        };
        assert_eq!(get("Docs").1, Kind::Dir);
        assert_eq!(get("Docs/Ծրագիր.txt").2.len(), TEXT_LEN);
        assert_eq!(get("Docs/Бюджет.csv").2, b"a,b\n1,2\n");
        assert_eq!(get("Docs/Scans/one.bin").2.len(), 2048);
        assert_eq!(get("notes.txt").2, b"hello\n");
    }
    let ms = zip::list(&data("stored.zip")[..]).unwrap();
    assert!(ms.iter().all(|m| m.method == Method::Stored));
    assert!(ms.iter().any(|m| m.mtime > 1_700_000_000));
}

#[test]
fn cp437_zip64_and_encrypted() {
    let ms = zip::list(&data("cp437.zip")[..]).unwrap();
    assert_eq!(ms[0].name, "Ç-cp437.txt");
    let b = data("zip64.zip");
    let ms = zip::list(&b[..]).unwrap();
    assert_eq!(
        (ms[0].name.as_str(), ms[0].size),
        ("big/zip64.txt", TEXT_LEN as u64)
    );
    assert_eq!(
        drain(&mut zip::open(&b[..], &ms[0]).unwrap())
            .unwrap()
            .len(),
        TEXT_LEN
    );
    let ms = zip::list(&data("secret.zip")[..]).unwrap();
    assert!(ms[0].encrypted && !ms[0].extractable());
}

#[test]
fn tars_with_long_names() {
    let long = format!("L{}/{}/g.txt", "o".repeat(60), "n".repeat(60));
    for f in ["pax.tar", "gnu.tar"] {
        let b = data(f);
        assert_eq!(detect(&b, f), Some(Format::Tar));
        let all = untar(At::all(&b[..])).unwrap();
        let get = |n: &str| {
            all.iter()
                .find(|m| m.0.name == n)
                .unwrap_or_else(|| panic!("{f}: no {n}"))
        };
        assert_eq!(get("Docs/Ծրագիր.txt").1.len(), TEXT_LEN);
        assert_eq!(get(&long).1, b"long\n");
        assert_eq!(get("Docs/Scans").0.kind, Kind::Dir);
    }
    let b = data("data.tar.gz");
    assert_eq!(detect(&b, "data.tar.gz"), Some(Format::TarGz));
    assert_eq!(detect(&b, "data.gz"), Some(Format::Gz));
    assert!(gzip::holds_tar(&b[..]));
    assert!(!gzip::holds_tar(&data("multi.gz")[..]));
    let all = untar(GzReader::new(At::all(&b[..]), u64::MAX).unwrap()).unwrap();
    assert!(
        all.iter()
            .any(|(m, d)| m.name == "notes.txt" && d == b"hello\n")
    );
    let multi = data("multi.gz");
    let mut g = GzReader::new(At::all(&multi[..]), u64::MAX).unwrap();
    assert_eq!(drain(&mut g).unwrap(), b"first second");
}

#[test]
fn slips_are_listed_unsafe() {
    let ms = zip::list(&data("slip.zip")[..]).unwrap();
    let ok: Vec<_> = ms
        .iter()
        .filter(|m| m.kind != Kind::Unsafe)
        .map(|m| m.name.as_str())
        .collect();
    assert_eq!(ok, ["ok.txt"]);
    assert_eq!(ms.len(), 6);
    assert!(ms.iter().all(|m| m.kind == Kind::File || !m.extractable()));
    let all = untar(At::all(&data("slip.tar")[..])).unwrap();
    let ok: Vec<_> = all
        .iter()
        .filter(|m| m.0.kind != Kind::Unsafe)
        .map(|m| m.0.name.as_str())
        .collect();
    assert_eq!(ok, ["ok.txt"]);
}

#[test]
fn a_nested_zip_is_only_a_file() {
    let all = unzip_all(&data("nested.zip")).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].0, "inner.zip");
    assert_eq!(detect(&all[0].2, "inner.zip"), Some(Format::Zip));
}

/// Cut short or with flipped bytes, every fixture gives `Err` or a result, never a panic.
#[test]
fn broken_bytes_never_panic() {
    let files = [
        "infozip.zip",
        "stored.zip",
        "stream.zip",
        "zip64.zip",
        "cp437.zip",
        "pax.tar",
        "gnu.tar",
        "data.tar.gz",
        "multi.gz",
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
        for b in cases {
            let _ = unzip_all(&b);
            let _ = untar(At::all(&b[..]));
            if let Ok(g) = GzReader::new(At::all(&b[..]), 64 << 20) {
                let _ = untar(g);
            }
            if let Ok(mut g) = GzReader::new(At::all(&b[..]), 64 << 20) {
                let _ = drain(&mut g);
            }
        }
    }
}

/// Our writers, read by other tools.
#[test]
fn host_tools_read_ours() {
    let dir = std::env::temp_dir().join(format!("neo-archive-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let text: Vec<u8> = (0..50_000u32).map(|i| (i % 97) as u8 + 32).collect();

    let mut z = ZipWriter::new(Vec::new());
    z.dir("Docs", 1_763_548_800).unwrap();
    z.start("Docs/Ծրագիր.txt", 1_763_548_800, true).unwrap();
    z.write(&text).unwrap();
    z.start("song.mp3", 1_763_548_800, false).unwrap();
    z.write(b"ID3").unwrap();
    std::fs::write(dir.join("ours.zip"), z.finish().unwrap()).unwrap();

    let mut t = TarWriter::new(GzWriter::new(Vec::new(), None, 1_763_548_800).unwrap());
    t.dir("Docs", 1_763_548_800).unwrap();
    let long = format!("Docs/{}.txt", "ն".repeat(80));
    t.start(&long, text.len() as u64, 1_763_548_800).unwrap();
    t.write(&text).unwrap();
    std::fs::write(
        dir.join("ours.tar.gz"),
        t.finish().unwrap().finish().unwrap(),
    )
    .unwrap();

    match Command::new("unzip")
        .arg("-tq")
        .arg(dir.join("ours.zip"))
        .output()
    {
        Ok(o) => assert!(
            o.status.success(),
            "unzip -t: {}",
            String::from_utf8_lossy(&o.stdout)
        ),
        Err(_) => eprintln!("no unzip: skipped"),
    }
    match Command::new("tar")
        .arg("-tzf")
        .arg(dir.join("ours.tar.gz"))
        .output()
    {
        Ok(o) => {
            assert!(
                o.status.success(),
                "tar -t: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            assert!(
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|l| l == long)
            );
        }
        Err(_) => eprintln!("no tar: skipped"),
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn errors_are_typed() {
    assert_eq!(
        zip::list(&b"not a zip at all, just text"[..]).err(),
        Some(Error::NotArchive)
    );
    assert_eq!(detect(b"hello", "x.zip"), None);
}
