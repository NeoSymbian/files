// SPDX-License-Identifier: Apache-2.0
//! FA6 jobs against the fake file system: compress, extract (all, a pick, clashes, cancel), the zip-slip fixtures, the
//! free-space guards and the tar.gz scan.
use files_ops::archive::{Compress, Extract, Packing, Pick, ScanGz, estimate, list, needed};
use files_ops::{Choice, Step, Summary};
use neo_format_archive::Format;
use neo_format_archive::gzip::GzWriter;
use neo_fs::{Error, File, Fs, Open, fake::MemFs};

const SLIP_ZIP: &[u8] = include_bytes!("../../neo-format-archive/tests/data/slip.zip");
const SLIP_TAR: &[u8] = include_bytes!("../../neo-format-archive/tests/data/slip.tar");
const DATA_TGZ: &[u8] = include_bytes!("../../neo-format-archive/tests/data/data.tar.gz");

fn fs() -> MemFs {
    let fs = MemFs::new();
    fs.add_drive('C', 1 << 26, false, false);
    fs.add_drive('E', 1 << 26, false, true);
    fs
}

fn contents(fs: &MemFs, path: &str) -> Vec<u8> {
    let file = fs.open(path, Open::Read).unwrap();
    let mut bytes = vec![0; file.size().unwrap() as usize];
    assert_eq!(file.read_at(0, &mut bytes).unwrap(), bytes.len());
    bytes
}

fn photos(fs: &MemFs) {
    let text: Vec<u8> = (0..40_000u32)
        .map(|i| b"lorem ipsum "[i as usize % 12])
        .collect();
    fs.add_file("C:\\Photos\\notes.txt", &text).unwrap();
    fs.add_file("C:\\Photos\\Ծրագիր\\a.jpg", &[7u8; 3000])
        .unwrap();
    fs.add_file("C:\\Photos\\Ծրագիր\\deep\\b.txt", b"bee")
        .unwrap();
    fs.add_file("C:\\Photos\\empty.txt", b"").unwrap();
    fs.add_dir("C:\\Photos\\nothing\\").unwrap();
}

/// Everything under `dir`, relative, with each file's bytes.
fn under(fs: &MemFs, dir: &str) -> Vec<(String, Option<Vec<u8>>)> {
    fs.tree()
        .into_iter()
        .filter_map(|(p, d)| {
            p.strip_prefix(dir)
                .filter(|r| !r.is_empty())
                .map(|r| (r.to_string(), d))
        })
        .collect()
}

fn compress(fs: &MemFs, sources: &[&str], out: &str, packing: Packing) -> Step {
    let sources: Vec<String> = sources.iter().map(|s| s.to_string()).collect();
    let mut job = Compress::new(fs, &sources, out, packing).unwrap();
    loop {
        match job.step(fs, 5000) {
            Step::Progress(p) => assert!(p.bytes_done <= p.bytes_total),
            s => return s,
        }
    }
}

fn extract<'a>(job: &mut Extract<'a, MemFs>, fs: &'a MemFs, choice: Choice) -> Step {
    for _ in 0..100_000 {
        match job.step(fs, 1000) {
            Step::Clash(_) => job.answer(choice, false),
            Step::Progress(p) => assert!(p.bytes_done <= p.bytes_total || p.bytes_total == 0),
            s => return s,
        }
    }
    panic!("job did not finish");
}

#[test]
fn compress_then_extract_gives_the_same_tree() {
    for (packing, out) in [
        (Packing::Zip, "E:\\Photos.zip"),
        (Packing::TarGz, "E:\\Photos.tar.gz"),
    ] {
        let fs = fs();
        photos(&fs);
        assert!(
            matches!(compress(&fs, &["C:\\Photos\\"], out, packing), Step::Done(s) if s.copied == 4)
        );
        assert!(fs.entry(&format!("{out}.~part")).is_err());
        let (format, members) = if packing == Packing::Zip {
            list(&fs, out).unwrap()
        } else {
            let mut scan = ScanGz::new(&fs, out).unwrap();
            while !scan.step(2).unwrap() {}
            (Format::TarGz, scan.members)
        };
        assert_eq!(
            format,
            if packing == Packing::Zip {
                Format::Zip
            } else {
                Format::TarGz
            }
        );
        assert!(members.iter().any(|m| m.name == "Photos/Ծրագիր/deep/b.txt"));
        let mut job = Extract::new(&fs, out, None, "E:\\out\\").unwrap();
        let done = extract(&mut job, &fs, Choice::Skip);
        assert_eq!(
            done,
            Step::Done(Summary {
                copied: 4,
                skipped: 0,
                bytes: 43_003
            })
        );
        assert_eq!(under(&fs, "C:\\"), under(&fs, "E:\\out\\"));
    }
}

#[test]
fn a_pick_lands_relative_to_the_browsed_folder() {
    let fs = fs();
    photos(&fs);
    compress(&fs, &["C:\\Photos\\"], "C:\\p.zip", Packing::Zip);
    let pick = Pick {
        base: "Photos".into(),
        names: vec!["Photos/Ծրագիր".into()],
    };
    let (_, members) = list(&fs, "C:\\p.zip").unwrap();
    assert_eq!(needed(&members, Some(&pick)), (3003, 2));
    let mut job = Extract::new(&fs, "C:\\p.zip", Some(pick), "E:\\x\\").unwrap();
    assert!(matches!(extract(&mut job, &fs, Choice::Skip), Step::Done(s) if s.copied == 2));
    let got: Vec<String> = under(&fs, "E:\\x\\").into_iter().map(|(p, _)| p).collect();
    assert_eq!(
        got,
        [
            "Ծրագիր\\",
            "Ծրագիր\\a.jpg",
            "Ծրագիր\\deep\\",
            "Ծրագիր\\deep\\b.txt"
        ]
    );
}

#[test]
fn clashes_replace_keep_both_and_skip() {
    for (choice, file, extra) in [
        (Choice::Replace, b"bee".to_vec(), None),
        (Choice::Skip, b"old".to_vec(), None),
        (
            Choice::KeepBoth,
            b"old".to_vec(),
            Some("E:\\x\\Photos\\Ծրագիր\\deep\\b (1).txt"),
        ),
    ] {
        let fs = fs();
        photos(&fs);
        compress(&fs, &["C:\\Photos\\"], "C:\\p.zip", Packing::Zip);
        fs.add_file("E:\\x\\Photos\\Ծրագիր\\deep\\b.txt", b"old")
            .unwrap();
        let mut job = Extract::new(&fs, "C:\\p.zip", None, "E:\\x\\").unwrap();
        let mut asked = 0;
        let done = loop {
            match job.step(&fs, 1000) {
                Step::Clash(c) => {
                    assert_eq!(c.name, "b.txt");
                    asked += 1;
                    job.answer(choice, false);
                }
                Step::Progress(_) => {}
                s => break s,
            }
        };
        assert_eq!(asked, 1);
        assert!(matches!(done, Step::Done(_)));
        assert_eq!(contents(&fs, "E:\\x\\Photos\\Ծրագիր\\deep\\b.txt"), file);
        if let Some(e) = extra {
            assert_eq!(contents(&fs, e), b"bee");
        }
    }
}

#[test]
fn cancel_leaves_no_part() {
    let fs = fs();
    fs.add_file("C:\\big.bin", &vec![3u8; 1 << 20]).unwrap();
    compress(&fs, &["C:\\big.bin"], "C:\\big.zip", Packing::Zip);
    let mut job = Extract::new(&fs, "C:\\big.zip", None, "E:\\x\\").unwrap();
    for _ in 0..10 {
        job.step(&fs, 4096);
    }
    assert!(fs.entry("E:\\x\\big.bin.~part").is_ok());
    job.cancel(&fs);
    assert_eq!(job.step(&fs, 1), Step::Failed(Error::Cancelled));
    assert!(under(&fs, "E:\\x\\").is_empty());
    // a cancelled compress too
    let mut c = Compress::new(&fs, &["C:\\big.bin".into()], "E:\\b.zip", Packing::Zip).unwrap();
    c.step(&fs, 4096);
    c.step(&fs, 4096);
    c.cancel(&fs);
    assert!(fs.entry("E:\\b.zip.~part").is_err() && fs.entry("E:\\b.zip").is_err());
}

#[test]
fn slips_never_leave_the_destination() {
    for (name, bytes) in [("C:\\slip.zip", SLIP_ZIP), ("C:\\slip.tar", SLIP_TAR)] {
        let fs = fs();
        fs.add_file(name, bytes).unwrap();
        let before = fs.tree();
        let mut job = Extract::new(&fs, name, None, "C:\\in\\").unwrap();
        let Step::Done(s) = extract(&mut job, &fs, Choice::Skip) else {
            panic!("{name}");
        };
        assert_eq!(s.copied, 1);
        assert!(s.skipped >= 2);
        let after = fs.tree();
        let new: Vec<_> = after
            .iter()
            .filter(|e| !before.contains(e))
            .map(|e| e.0.as_str())
            .collect();
        assert_eq!(new, ["C:\\in\\", "C:\\in\\ok.txt"], "{name}");
    }
}

#[test]
fn free_space_is_checked_first_and_a_gz_stops_at_it() {
    let fs = fs();
    fs.add_file("C:\\big.bin", &vec![0u8; 4 << 20]).unwrap();
    compress(&fs, &["C:\\big.bin"], "C:\\big.zip", Packing::Zip);
    // a gzip of zeros: 4 MB inflate from a few KB
    let mut g = GzWriter::new(Vec::new(), Some("zeros.bin"), 0).unwrap();
    neo_format_archive::Sink::write(&mut g, &vec![0u8; 4 << 20]).unwrap();
    fs.add_file("C:\\zeros.bin.gz", &g.finish().unwrap())
        .unwrap();
    let small = MemFs::new();
    small.add_drive('C', 1 << 26, false, false);
    small.add_drive('E', 3 << 20, false, true);
    small
        .add_file("C:\\big.zip", &contents(&fs, "C:\\big.zip"))
        .unwrap();
    small
        .add_file("C:\\zeros.bin.gz", &contents(&fs, "C:\\zeros.bin.gz"))
        .unwrap();
    assert_eq!(
        Extract::new(&small, "C:\\big.zip", None, "E:\\x\\").err(),
        Some(Error::Full)
    );
    let mut job = Extract::new(&small, "C:\\zeros.bin.gz", None, "E:\\").unwrap();
    assert_eq!(
        extract(&mut job, &small, Choice::Skip),
        Step::Failed(Error::Full)
    );
    assert!(small.entry("E:\\zeros.bin.~part").is_err());
    // with room, the .gz becomes its one file
    let mut job = Extract::new(&fs, "C:\\zeros.bin.gz", None, "E:\\").unwrap();
    assert!(matches!(extract(&mut job, &fs, Choice::Skip), Step::Done(s) if s.bytes == 4 << 20));
    assert_eq!(fs.entry("E:\\zeros.bin").unwrap().size, 4 << 20);
}

#[test]
fn compressing_the_folder_that_holds_the_output() {
    let fs = fs();
    photos(&fs);
    assert!(matches!(
        compress(&fs, &["C:\\Photos\\"], "C:\\Photos\\all.zip", Packing::Zip),
        Step::Done(_)
    ));
    let (_, members) = list(&fs, "C:\\Photos\\all.zip").unwrap();
    assert!(
        members
            .iter()
            .all(|m| !m.name.ends_with("all.zip") && !m.name.ends_with(".~part"))
    );
    assert_eq!(
        Compress::new(
            &fs,
            &["C:\\Photos\\".into()],
            "C:\\Photos\\all.zip",
            Packing::Zip
        )
        .err(),
        Some(Error::Exists)
    );
}

#[test]
fn a_tar_gz_from_another_tool() {
    let fs = fs();
    fs.add_file("C:\\data.tar.gz", DATA_TGZ).unwrap();
    let mut scan = ScanGz::new(&fs, "C:\\data.tar.gz").unwrap();
    while !scan.step(1).unwrap() {}
    assert_eq!(scan.done_of().0, DATA_TGZ.len() as u64);
    assert!(scan.members.iter().any(|m| m.name == "notes.txt"));
    let pick = Pick {
        base: String::new(),
        names: vec!["notes.txt".into()],
    };
    let mut job = Extract::new(&fs, "C:\\data.tar.gz", Some(pick), "C:\\open\\").unwrap();
    job.replace_all();
    assert!(matches!(extract(&mut job, &fs, Choice::Skip), Step::Done(s) if s.copied == 1));
    assert_eq!(contents(&fs, "C:\\open\\notes.txt"), b"hello\n");
}

/// The review's cases: a pick from a tar.gz bigger than the room left (only what is written counts, and it stops
/// once the pick is met), Replace onto a folder skips that one, and sizes that would wrap a sum.
#[test]
fn review_cases() {
    let fs = fs();
    fs.add_file("C:\\big.bin", &vec![9u8; 4 << 20]).unwrap();
    fs.add_file("C:\\a.txt", b"small").unwrap();
    compress(
        &fs,
        &["C:\\a.txt", "C:\\big.bin"],
        "C:\\x.tar.gz",
        Packing::TarGz,
    );
    let small = MemFs::new();
    small.add_drive('C', 1 << 26, false, false);
    small.add_drive('E', 2 << 20, false, true);
    small
        .add_file("C:\\x.tar.gz", &contents(&fs, "C:\\x.tar.gz"))
        .unwrap();
    let pick = Pick {
        base: String::new(),
        names: vec!["a.txt".into()],
    };
    let mut job = Extract::new(&small, "C:\\x.tar.gz", Some(pick), "E:\\o\\").unwrap();
    assert!(matches!(extract(&mut job, &small, Choice::Skip), Step::Done(s) if s.copied == 1));
    assert_eq!(contents(&small, "E:\\o\\a.txt"), b"small");
    // Replace where a folder has the file's name: skipped, the rest goes on
    let fs2 = self::fs();
    photos(&fs2);
    compress(&fs2, &["C:\\Photos\\"], "C:\\p.zip", Packing::Zip);
    fs2.add_dir("E:\\x\\Photos\\notes.txt\\").unwrap();
    let mut job = Extract::new(&fs2, "C:\\p.zip", None, "E:\\x\\").unwrap();
    assert!(
        matches!(extract(&mut job, &fs2, Choice::Replace), Step::Done(s) if s.skipped == 1 && s.copied == 3)
    );
    let (_, mut members) = list(&fs2, "C:\\p.zip").unwrap();
    for m in members.iter_mut() {
        m.size = u64::MAX / 2 + 1;
    }
    assert_eq!(needed(&members, None).0, u64::MAX);
}

#[test]
fn the_estimate() {
    let fs = fs();
    photos(&fs);
    let (total, expect, packed) = estimate(&fs, &["C:\\Photos\\".into()]).unwrap();
    assert_eq!((total, packed), (43_003, 3000));
    assert_eq!(expect, 3000 + 40_003 / 2);
}
