// SPDX-License-Identifier: Apache-2.0
use files_ops::{Choice, Kind, Step, Summary, Transfer, trash};
use neo_fs::{Error, File, Fs, Open, fake::MemFs};

fn fs() -> MemFs {
    let fs = MemFs::new();
    fs.add_drive('C', 1 << 24, false, false);
    fs.add_drive('E', 1 << 24, false, true);
    fs.add_drive('Z', 1 << 24, true, false);
    fs
}

fn contents(fs: &MemFs, path: &str) -> Vec<u8> {
    let file = fs.open(path, Open::Read).unwrap();
    let mut bytes = vec![0; file.size().unwrap() as usize];
    let n = file.read_at(0, &mut bytes).unwrap();
    assert_eq!(n, bytes.len());
    bytes
}

fn run<'a>(job: &mut Transfer<'a, MemFs>, fs: &'a MemFs, choice: Choice) -> Step {
    for _ in 0..10_000 {
        match job.step(fs, 3) {
            Step::Clash(_) => job.answer(choice, false),
            Step::Progress(_) => {}
            result => return result,
        }
    }
    panic!("job did not finish");
}

#[test]
fn copy_nested_folder_is_incremental_and_progress_is_monotonic() {
    let fs = fs();
    fs.add_file("C:\\Photos\\one", b"1234567890").unwrap();
    fs.add_file("C:\\Photos\\nested\\two", b"abcdefghijk")
        .unwrap();
    let mut job = Transfer::new(&fs, Kind::Copy, vec!["C:\\Photos\\".into()], "E:\\").unwrap();
    let mut bytes = 0;
    let mut files = 0;
    let mut steps = 0;
    loop {
        match job.step(&fs, 3) {
            Step::Progress(p) => {
                assert_eq!((p.files_total, p.bytes_total), (2, 21));
                assert!(p.bytes_done >= bytes && p.bytes_done - bytes <= 3);
                assert!(p.files_done >= files);
                assert!(p.current_done <= p.current_size);
                bytes = p.bytes_done;
                files = p.files_done;
                steps += 1;
            }
            Step::Done(s) => {
                assert_eq!(
                    s,
                    Summary {
                        copied: 2,
                        skipped: 0,
                        bytes: 21
                    }
                );
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(steps >= 7);
    assert_eq!(contents(&fs, "E:\\Photos\\one"), b"1234567890");
    assert_eq!(contents(&fs, "E:\\Photos\\nested\\two"), b"abcdefghijk");
    assert_eq!(contents(&fs, "C:\\Photos\\one"), b"1234567890");
}

#[test]
fn copying_a_file_and_an_empty_file() {
    let fs = fs();
    fs.add_file("C:\\one", b"payload").unwrap();
    fs.add_file("C:\\empty", b"").unwrap();
    let mut job = Transfer::new(
        &fs,
        Kind::Copy,
        vec!["C:\\one".into(), "C:\\empty".into()],
        "E:\\",
    )
    .unwrap();
    assert_eq!(
        run(&mut job, &fs, Choice::Skip),
        Step::Done(Summary {
            copied: 2,
            skipped: 0,
            bytes: 7
        })
    );
    assert_eq!(contents(&fs, "E:\\one"), b"payload");
    assert!(contents(&fs, "E:\\empty").is_empty());
}

#[test]
fn disk_full_removes_partial_and_preserves_source() {
    let fs = fs();
    fs.add_file("C:\\one", b"123456789").unwrap();
    fs.set_capacity('E', 4);
    let mut job = Transfer::new(&fs, Kind::Move, vec!["C:\\one".into()], "E:\\").unwrap();
    assert_eq!(run(&mut job, &fs, Choice::Skip), Step::Failed(Error::Full));
    assert!(fs.entry("E:\\one").is_err());
    assert!(fs.entry("E:\\one.~part").is_err());
    assert_eq!(contents(&fs, "C:\\one"), b"123456789");
}

#[test]
fn removal_during_copy_fails_gone() {
    let fs = fs();
    fs.add_file("C:\\one", b"123456789").unwrap();
    let mut job = Transfer::new(&fs, Kind::Copy, vec!["C:\\one".into()], "E:\\").unwrap();
    assert!(matches!(job.step(&fs, 3), Step::Progress(_)));
    fs.remove_drive_after('E', 2);
    assert_eq!(run(&mut job, &fs, Choice::Skip), Step::Failed(Error::Gone));
    fs.restore();
    assert_eq!(contents(&fs, "C:\\one"), b"123456789");
}

#[test]
fn cancel_removes_current_partial_and_is_terminal() {
    let fs = fs();
    fs.add_file("C:\\one", b"123456789").unwrap();
    fs.add_file("E:\\one.~part", b"stale and longer").unwrap();
    let mut job = Transfer::new(&fs, Kind::Move, vec!["C:\\one".into()], "E:\\").unwrap();
    job.step(&fs, 3);
    job.step(&fs, 3);
    assert_eq!(contents(&fs, "E:\\one.~part"), b"123");
    job.cancel(&fs);
    assert_eq!(job.step(&fs, 3), Step::Failed(Error::Cancelled));
    assert!(fs.entry("E:\\one.~part").is_err());
    assert_eq!(contents(&fs, "C:\\one"), b"123456789");
}

#[test]
fn zero_budget_does_not_copy_and_large_budget_is_capped_at_64k() {
    let fs = fs();
    fs.add_file("C:\\one", &vec![42; 200_000]).unwrap();
    let mut job = Transfer::new(&fs, Kind::Copy, vec!["C:\\one".into()], "E:\\").unwrap();
    job.step(&fs, 0);
    match job.step(&fs, 0) {
        Step::Progress(p) => assert_eq!(p.bytes_done, 0),
        other => panic!("{other:?}"),
    }
    match job.step(&fs, usize::MAX) {
        Step::Progress(p) => assert_eq!(p.bytes_done, 64 * 1024),
        other => panic!("{other:?}"),
    }
    job.cancel(&fs);
}

#[test]
fn clashes_replace_keep_both_skip_and_apply_to_all() {
    for choice in [Choice::Replace, Choice::KeepBoth, Choice::Skip] {
        let fs = fs();
        fs.add_file("C:\\song.mp3", b"new").unwrap();
        fs.add_file("C:\\two", b"second").unwrap();
        fs.add_file("E:\\song.mp3", b"old").unwrap();
        fs.add_file("E:\\two", b"before").unwrap();
        fs.add_file("E:\\song (1).mp3", b"first").unwrap();
        let mut job = Transfer::new(
            &fs,
            Kind::Copy,
            vec!["C:\\song.mp3".into(), "C:\\two".into()],
            "E:\\",
        )
        .unwrap();
        match job.step(&fs, 3) {
            Step::Clash(clash) => {
                assert_eq!(clash.name, "song.mp3");
                assert_eq!(clash.here.size, 3);
                assert_eq!(clash.new.size, 3);
                assert!(clash.left >= 1);
            }
            other => panic!("{other:?}"),
        }
        job.answer(choice, true);
        let mut finished = false;
        for _ in 0..50 {
            match job.step(&fs, 3) {
                Step::Progress(_) => {}
                Step::Done(s) => {
                    assert_eq!(s.skipped, if choice == Choice::Skip { 2 } else { 0 });
                    finished = true;
                    break;
                }
                other => panic!("for_all failed: {other:?}"),
            }
        }
        assert!(finished);
        match choice {
            Choice::Replace => {
                assert_eq!(contents(&fs, "E:\\song.mp3"), b"new");
                assert_eq!(contents(&fs, "E:\\two"), b"second");
            }
            Choice::KeepBoth => {
                assert_eq!(contents(&fs, "E:\\song (2).mp3"), b"new");
                assert_eq!(contents(&fs, "E:\\two (1)"), b"second");
            }
            Choice::Skip => assert_eq!(contents(&fs, "E:\\song.mp3"), b"old"),
        }
    }
}

#[test]
fn repeated_keep_both_uses_first_free_suffix() {
    let fs = fs();
    fs.add_file("C:\\song.mp3", b"new").unwrap();
    fs.add_file("E:\\song.mp3", b"old").unwrap();
    for expected in ["E:\\song (1).mp3", "E:\\song (2).mp3"] {
        let mut job = Transfer::new(&fs, Kind::Copy, vec!["C:\\song.mp3".into()], "E:\\").unwrap();
        assert!(matches!(
            run(&mut job, &fs, Choice::KeepBoth),
            Step::Done(_)
        ));
        assert_eq!(contents(&fs, expected), b"new");
    }
}

#[test]
fn copy_same_folder_automatically_duplicates_without_clash() {
    let fs = fs();
    fs.add_file("C:\\song.mp3", b"new").unwrap();
    let mut job = Transfer::new(&fs, Kind::Copy, vec!["c:\\SONG.MP3".into()], "C:\\").unwrap();
    let mut finished = false;
    for _ in 0..20 {
        match job.step(&fs, 3) {
            Step::Progress(_) => {}
            Step::Done(_) => {
                finished = true;
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(finished);
    assert_eq!(contents(&fs, "C:\\song (1).mp3"), b"new");
}

#[test]
fn refuses_self_moves_and_read_only_drives_up_front() {
    let fs = fs();
    fs.add_file("C:\\folder\\sub\\one", b"new").unwrap();
    fs.add_file("Z:\\rom", b"ROM").unwrap();
    for (kind, source, destination, error) in [
        (
            Kind::Copy,
            "C:\\folder\\",
            "c:\\FOLDER\\sub\\",
            Error::IntoItself,
        ),
        (Kind::Copy, "C:\\folder", "C:\\folder\\", Error::IntoItself),
        (Kind::Move, "C:\\folder\\", "C:\\", Error::SamePlace),
        (
            Kind::Move,
            "C:\\folder\\sub\\one",
            "C:\\folder\\sub\\",
            Error::SamePlace,
        ),
        (Kind::Copy, "C:\\folder\\", "Z:\\", Error::ReadOnly),
        (Kind::Move, "Z:\\rom", "C:\\", Error::ReadOnly),
    ] {
        assert!(
            matches!(Transfer::new(&fs, kind, vec![source.into()], destination), Err(e) if e == error)
        );
    }
}

#[test]
fn same_drive_folder_move_uses_one_rename() {
    let fs = fs();
    fs.add_file("C:\\folder\\sub\\one", b"payload").unwrap();
    fs.add_dir("C:\\dest\\").unwrap();
    let before = fs.ops();
    let mut job =
        Transfer::new(&fs, Kind::Move, vec!["C:\\folder\\".into()], "C:\\dest\\").unwrap();
    assert_eq!(
        job.step(&fs, 0),
        Step::Done(Summary {
            copied: 1,
            skipped: 0,
            bytes: 7
        })
    );
    assert_eq!(fs.ops() - before, 1);
    assert_eq!(contents(&fs, "C:\\dest\\folder\\sub\\one"), b"payload");
    assert!(fs.entry("C:\\folder\\").is_err());
}

#[test]
fn same_drive_merge_moves_files_and_keeps_skipped_sources() {
    let fs = fs();
    fs.add_file("C:\\folder\\one", b"new").unwrap();
    fs.add_file("C:\\folder\\two", b"move").unwrap();
    fs.add_file("C:\\dest\\folder\\one", b"old").unwrap();
    let mut job =
        Transfer::new(&fs, Kind::Move, vec!["C:\\folder\\".into()], "C:\\dest\\").unwrap();
    assert_eq!(
        run(&mut job, &fs, Choice::Skip),
        Step::Done(Summary {
            copied: 1,
            skipped: 1,
            bytes: 4
        })
    );
    assert_eq!(contents(&fs, "C:\\folder\\one"), b"new");
    assert_eq!(contents(&fs, "C:\\dest\\folder\\one"), b"old");
    assert_eq!(contents(&fs, "C:\\dest\\folder\\two"), b"move");
    assert!(fs.entry("C:\\folder\\two").is_err());
}

#[test]
fn same_drive_replace_deletes_target_then_renames() {
    let fs = fs();
    fs.add_file("C:\\song", b"new").unwrap();
    fs.add_file("C:\\dest\\song", b"old").unwrap();
    let before = fs.ops();
    let mut job = Transfer::new(&fs, Kind::Move, vec!["C:\\song".into()], "C:\\dest\\").unwrap();
    assert!(matches!(run(&mut job, &fs, Choice::Replace), Step::Done(_)));
    assert_eq!(fs.ops() - before, 2);
    assert_eq!(contents(&fs, "C:\\dest\\song"), b"new");
    assert!(fs.entry("C:\\song").is_err());
}

#[test]
fn mismatched_file_folder_clash_preserves_existing_folder() {
    let fs = fs();
    fs.add_file("C:\\one", b"new").unwrap();
    fs.add_file("E:\\one\\inside", b"old").unwrap();
    let mut job = Transfer::new(&fs, Kind::Copy, vec!["C:\\one".into()], "E:\\").unwrap();
    assert_eq!(
        run(&mut job, &fs, Choice::Replace),
        Step::Failed(Error::Exists)
    );
    assert_eq!(contents(&fs, "E:\\one\\inside"), b"old");
}

#[test]
fn replacement_keeps_old_file_until_new_file_commits() {
    let fs = fs();
    fs.add_file("C:\\one", b"new bytes").unwrap();
    fs.add_file("E:\\one", b"old bytes").unwrap();
    let mut job = Transfer::new(&fs, Kind::Copy, vec!["C:\\one".into()], "E:\\").unwrap();
    assert!(matches!(job.step(&fs, 3), Step::Clash(_)));
    job.answer(Choice::Replace, false);
    job.step(&fs, 3);
    job.step(&fs, 3);
    assert_eq!(contents(&fs, "E:\\one"), b"old bytes");
    assert_eq!(contents(&fs, "E:\\one.~part"), b"new");
    assert!(matches!(run(&mut job, &fs, Choice::Skip), Step::Done(_)));
    assert_eq!(contents(&fs, "E:\\one"), b"new bytes");
    assert!(fs.entry("E:\\one.~part").is_err());
}

#[test]
fn same_folder_directory_duplicate_keeps_a_dotted_folder_name_whole() {
    let fs = fs();
    fs.add_file("C:\\folder.ext\\one", b"data").unwrap();
    let mut job = Transfer::new(&fs, Kind::Copy, vec!["C:\\folder.ext\\".into()], "C:\\").unwrap();
    assert!(matches!(run(&mut job, &fs, Choice::Skip), Step::Done(_)));
    assert_eq!(contents(&fs, "C:\\folder.ext (1)\\one"), b"data");
}

#[test]
fn power_cut_at_every_move_mutation_never_loses_a_real_file() {
    fn fixture() -> MemFs {
        let fs = fs();
        fs.add_file("C:\\folder\\one", b"alpha").unwrap();
        fs.add_file("C:\\folder\\sub\\two", b"beta beta").unwrap();
        fs.add_file("C:\\folder\\sub\\three", b"gamma gamma gamma")
            .unwrap();
        fs
    }
    let successful = fixture();
    let before = successful.ops();
    let mut job =
        Transfer::new(&successful, Kind::Move, vec!["C:\\folder\\".into()], "E:\\").unwrap();
    assert!(matches!(
        run(&mut job, &successful, Choice::Skip),
        Step::Done(_)
    ));
    let operations = successful.ops() - before;
    assert!(operations > 10);
    for cut in 1..=operations + 1 {
        let fs = fixture();
        fs.cut_after(cut);
        let mut job = Transfer::new(&fs, Kind::Move, vec!["C:\\folder\\".into()], "E:\\").unwrap();
        let _ = run(&mut job, &fs, Choice::Skip);
        drop(job);
        fs.restore();
        for drive in ['C', 'E', 'Z'] {
            trash::recover(&fs, drive).unwrap();
        }
        for (name, bytes) in [
            ("one", b"alpha".as_slice()),
            ("sub\\two", b"beta beta".as_slice()),
            ("sub\\three", b"gamma gamma gamma".as_slice()),
        ] {
            let source = format!("C:\\folder\\{name}");
            let target = format!("E:\\folder\\{name}");
            let present: Vec<_> = [&source, &target]
                .into_iter()
                .filter(|path| fs.entry(path).is_ok())
                .collect();
            assert!(!present.is_empty(), "cut {cut}: lost {name}");
            for path in present {
                assert_eq!(contents(&fs, path), bytes, "cut {cut}: partial {path}");
            }
        }
    }
}

#[test]
fn move_without_destination_trailing_slash_still_refuses_same_place() {
    let fs = fs();
    fs.add_file("C:\\folder\\one", b"whole").unwrap();
    let before = fs.tree();
    assert!(matches!(
        Transfer::new(
            &fs,
            Kind::Move,
            vec!["C:\\folder\\one".into()],
            "c:\\FOLDER"
        ),
        Err(Error::SamePlace)
    ));
    assert_eq!(fs.tree(), before);
}

#[test]
fn temporary_name_cannot_overwrite_a_selected_source() {
    let fs = fs();
    fs.add_file("C:\\src\\a", b"first").unwrap();
    fs.add_file("C:\\dest\\a.~part", b"selected source")
        .unwrap();
    let before = fs.tree();
    let mut job = Transfer::new(
        &fs,
        Kind::Copy,
        vec!["C:\\src\\a".into(), "C:\\dest\\a.~part".into()],
        "C:\\dest\\",
    )
    .unwrap();
    assert_eq!(
        run(&mut job, &fs, Choice::Replace),
        Step::Failed(Error::InUse)
    );
    assert_eq!(fs.tree(), before);
}

#[test]
fn temporary_name_cannot_overwrite_a_committed_output() {
    let fs = fs();
    fs.add_file("C:\\src\\a.~part", b"first").unwrap();
    fs.add_file("C:\\src\\a", b"second").unwrap();
    let mut job = Transfer::new(
        &fs,
        Kind::Move,
        vec!["C:\\src\\a.~part".into(), "C:\\src\\a".into()],
        "E:\\",
    )
    .unwrap();
    assert_eq!(
        run(&mut job, &fs, Choice::Replace),
        Step::Failed(Error::InUse)
    );
    assert_eq!(contents(&fs, "E:\\a.~part"), b"first");
    assert_eq!(contents(&fs, "C:\\src\\a"), b"second");
    assert!(fs.entry("E:\\a").is_err());
}

#[test]
fn invalid_trailing_slash_file_is_rejected_before_any_copy() {
    let fs = fs();
    fs.add_file("C:\\src\\a", b"first").unwrap();
    fs.add_file("C:\\dest\\a.~part", b"selected source")
        .unwrap();
    let before = fs.tree();
    assert!(matches!(
        Transfer::new(
            &fs,
            Kind::Copy,
            vec!["C:\\src\\a".into(), "C:\\dest\\a.~part\\".into()],
            "C:\\dest\\"
        ),
        Err(Error::BadName)
    ));
    assert_eq!(fs.tree(), before);
}
