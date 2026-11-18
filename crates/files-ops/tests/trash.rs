// SPDX-License-Identifier: Apache-2.0
use files_ops::trash::{self, Batch};
use neo_fs::fake::MemFs;
use neo_fs::{Error, File, Fs, Open, att};

fn filesystem() -> MemFs {
    let fs = MemFs::new();
    fs.add_drive('C', 4 * 1024 * 1024, false, false);
    fs.add_drive('E', 4 * 1024 * 1024, false, true);
    fs.add_drive('Z', 4 * 1024 * 1024, true, false);
    fs
}

fn bytes(fs: &impl Fs, path: &str) -> Vec<u8> {
    let file = fs.open(path, Open::Read).unwrap();
    let mut bytes = vec![0; file.size().unwrap() as usize];
    let mut offset = 0;
    while offset < bytes.len() {
        let n = file.read_at(offset as u64, &mut bytes[offset..]).unwrap();
        assert!(n > 0);
        offset += n;
    }
    bytes
}

#[test]
fn trash_lists_journal_and_undo_restores_files_and_nested_folders() {
    let fs = filesystem();
    fs.add_file("C:\\song.mp3", b"music").unwrap();
    fs.add_file("C:\\Photos\\nested\\image.jpg", b"picture")
        .unwrap();
    let batch = trash::trash(
        &fs,
        &["C:\\song.mp3".into(), "C:\\Photos\\".into()],
        123_456,
    )
    .unwrap();
    assert_eq!(batch.id, 1);
    assert_eq!(batch.items, ["C:\\.trash\\1", "C:\\.trash\\2\\"]);
    assert_eq!(fs.entry("C:\\song.mp3"), Err(Error::NotFound));
    let bin = fs.entry("C:\\.trash\\").unwrap();
    assert_eq!(
        bin.att & (att::HIDDEN | att::SYSTEM),
        att::HIDDEN | att::SYSTEM
    );
    let listed = trash::list(&fs, 'c').unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, 1);
    assert_eq!(listed[0].original, "C:\\song.mp3");
    assert_eq!(listed[0].name_in_bin, "1");
    assert_eq!(listed[0].deleted, 123_456);
    assert_eq!(listed[1].id, 2);
    let journal = String::from_utf8(bytes(&fs, "C:\\.trash\\index.dat")).unwrap();
    assert!(journal.contains("1\tpending\t123456\tC:\\song.mp3\t1\n"));
    assert!(journal.contains("1\tdone\t123456\tC:\\song.mp3\t1\n"));
    trash::undo(&fs, &batch).unwrap();
    assert_eq!(bytes(&fs, "C:\\song.mp3"), b"music");
    assert_eq!(bytes(&fs, "C:\\Photos\\nested\\image.jpg"), b"picture");
    assert!(trash::list(&fs, 'C').unwrap().is_empty());
    assert!(
        String::from_utf8(bytes(&fs, "C:\\.trash\\index.dat"))
            .unwrap()
            .contains("1\trestored\t")
    );
    trash::undo(&fs, &batch).unwrap();
    fs.add_file("C:\\next", b"next").unwrap();
    let next = trash::trash(&fs, &["C:\\next".into()], 0).unwrap();
    assert_eq!(next.id, 3);
}

#[test]
fn undo_uses_first_free_keep_both_name_for_files_and_folders() {
    let fs = filesystem();
    fs.add_file("C:\\song.mp3", b"original").unwrap();
    fs.add_file("C:\\Photos.v1\\pic", b"picture").unwrap();
    let batch = trash::trash(&fs, &["C:\\song.mp3".into(), "C:\\Photos.v1\\".into()], 0).unwrap();
    fs.add_file("C:\\song.mp3", b"new").unwrap();
    fs.add_file("C:\\song (1).mp3", b"taken").unwrap();
    fs.add_dir("C:\\Photos.v1\\").unwrap();
    trash::undo(&fs, &batch).unwrap();
    assert_eq!(bytes(&fs, "C:\\song.mp3"), b"new");
    assert_eq!(bytes(&fs, "C:\\song (1).mp3"), b"taken");
    assert_eq!(bytes(&fs, "C:\\song (2).mp3"), b"original");
    assert_eq!(bytes(&fs, "C:\\Photos.v1 (1)\\pic"), b"picture");
}

#[test]
fn batch_handles_per_drive_ids_and_undo_across_drives() {
    let fs = filesystem();
    fs.add_file("C:\\first", b"first").unwrap();
    fs.add_file("E:\\second", b"second").unwrap();
    let batch = trash::trash(&fs, &["C:\\first".into(), "E:\\second".into()], 55).unwrap();
    assert_eq!(batch.items, ["C:\\.trash\\1", "E:\\.trash\\1"]);
    assert_eq!(trash::list(&fs, 'C').unwrap()[0].id, 1);
    assert_eq!(trash::list(&fs, 'E').unwrap()[0].id, 1);
    trash::undo(&fs, &batch).unwrap();
    assert_eq!(bytes(&fs, "C:\\first"), b"first");
    assert_eq!(bytes(&fs, "E:\\second"), b"second");
}

#[test]
fn recovery_rolls_back_pending_and_finishes_renamed_items() {
    let fs = filesystem();
    fs.add_file("C:\\original", b"still here").unwrap();
    fs.add_file("C:\\.trash\\2\\nested\\item", b"moved")
        .unwrap();
    fs.add_file(
        "C:\\.trash\\index.dat",
        b"1\tpending\t10\tC:\\original\t1\n2\tpending\t20\tC:\\folder\\\t2\n",
    )
    .unwrap();
    fs.add_file("C:\\.trash\\index.dat.~part", b"unfinished journal")
        .unwrap();
    fs.add_file("C:\\.trash\\orphan.~part", b"unfinished")
        .unwrap();
    fs.add_file("C:\\.trash\\2\\nested\\keep.~part", b"user data")
        .unwrap();
    trash::recover(&fs, 'C').unwrap();
    assert_eq!(bytes(&fs, "C:\\original"), b"still here");
    assert_eq!(bytes(&fs, "C:\\.trash\\2\\nested\\item"), b"moved");
    assert_eq!(
        bytes(&fs, "C:\\.trash\\2\\nested\\keep.~part"),
        b"user data"
    );
    assert_eq!(fs.entry("C:\\.trash\\orphan.~part"), Err(Error::NotFound));
    assert_eq!(
        fs.entry("C:\\.trash\\index.dat.~part"),
        Err(Error::NotFound)
    );
    let listed = trash::list(&fs, 'C').unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].original, "C:\\folder\\");
    assert_eq!(listed[0].id, 2);
    let before = fs.tree();
    trash::recover(&fs, 'C').unwrap();
    assert_eq!(fs.tree(), before);
    trash::recover(&fs, 'E').unwrap();
    trash::recover(&fs, 'Z').unwrap();
    assert!(trash::list(&fs, 'Z').unwrap().is_empty());
}

#[test]
fn read_only_and_invalid_batches_are_refused_before_any_moves() {
    let fs = filesystem();
    fs.add_file("C:\\good", b"good").unwrap();
    fs.add_file("Z:\\rom", b"rom").unwrap();
    assert_eq!(
        trash::trash(&fs, &["C:\\good".into(), "Z:\\rom".into()], 0),
        Err(Error::ReadOnly)
    );
    assert_eq!(bytes(&fs, "C:\\good"), b"good");
    assert_eq!(fs.entry("C:\\.trash\\"), Err(Error::NotFound));
    assert_eq!(
        trash::trash(&fs, &["C:\\unsafe\tname".into()], 0),
        Err(Error::BadName)
    );
    assert_eq!(trash::trash(&fs, &["C:\\".into()], 0), Err(Error::BadName));
    fs.add_file("C:\\.trash\\1", b"existing bin item").unwrap();
    assert_eq!(
        trash::trash(&fs, &["C:\\.trash\\1".into()], 0),
        Err(Error::IntoItself)
    );
    assert_eq!(
        trash::trash(&fs, &[], 0).unwrap(),
        Batch {
            id: 0,
            items: vec![]
        }
    );
}

fn cut_fixture() -> MemFs {
    let fs = filesystem();
    fs.add_file("C:\\one.bin", b"one complete payload").unwrap();
    fs.add_file("E:\\Folder\\nested\\two.bin", b"second complete payload")
        .unwrap();
    fs
}

fn cut_paths() -> Vec<String> {
    vec!["C:\\one.bin".into(), "E:\\Folder\\".into()]
}

#[test]
fn every_trash_power_cut_preserves_whole_items_exactly_once_after_recovery() {
    let baseline = cut_fixture();
    let start = baseline.ops();
    trash::trash(&baseline, &cut_paths(), 1_234).unwrap();
    let mutations = baseline.ops() - start;
    assert!(mutations > 10);
    for cut in 1..=mutations + 1 {
        let fs = cut_fixture();
        fs.cut_after(cut);
        let _ = trash::trash(&fs, &cut_paths(), 1_234);
        fs.restore();
        for drive in ['C', 'E', 'Z'] {
            trash::recover(&fs, drive).unwrap();
        }
        for (original, destination, expected) in [
            (
                "C:\\one.bin",
                "C:\\.trash\\1",
                b"one complete payload".as_slice(),
            ),
            (
                "E:\\Folder\\nested\\two.bin",
                "E:\\.trash\\1\\nested\\two.bin",
                b"second complete payload".as_slice(),
            ),
        ] {
            let mut found = 0;
            for location in [original, destination] {
                if fs.entry(location).is_ok() {
                    assert_eq!(bytes(&fs, location), expected, "cut {cut}: {location}");
                    found += 1;
                }
            }
            assert_eq!(found, 1, "cut {cut}: {original}");
        }
        for (name, _) in fs.tree() {
            assert!(!name.ends_with(".~part"), "cut {cut}: {name}");
        }
        for drive in ['C', 'E'] {
            for item in trash::list(&fs, drive).unwrap() {
                assert!(fs.entry(&item.original).is_err());
                let in_bin = format!("{drive}:\\.trash\\{}", item.name_in_bin);
                assert!(fs.entry(&in_bin).is_ok() || fs.entry(&(in_bin + "\\")).is_ok());
            }
        }
    }
}

#[test]
fn every_same_drive_trash_power_cut_preserves_both_items() {
    fn fixture() -> MemFs {
        let fs = filesystem();
        fs.add_file("C:\\one", b"first payload").unwrap();
        fs.add_file("C:\\two", b"second payload").unwrap();
        fs
    }
    let paths = vec!["C:\\one".into(), "C:\\two".into()];
    let baseline = fixture();
    trash::trash(&baseline, &paths, 7).unwrap();
    let mutations = baseline.ops();
    assert!(mutations > 10);
    for cut in 1..=mutations + 1 {
        let fs = fixture();
        fs.cut_after(cut);
        let _ = trash::trash(&fs, &paths, 7);
        fs.restore();
        for drive in ['C', 'E', 'Z'] {
            trash::recover(&fs, drive).unwrap();
        }
        for (original, bin, expected) in [
            ("C:\\one", "C:\\.trash\\1", b"first payload".as_slice()),
            ("C:\\two", "C:\\.trash\\2", b"second payload".as_slice()),
        ] {
            let mut found = 0;
            for path in [original, bin] {
                if fs.entry(path).is_ok() {
                    assert_eq!(bytes(&fs, path), expected, "cut {cut}: {path}");
                    found += 1;
                }
            }
            assert_eq!(found, 1, "cut {cut}: {original}");
        }
        assert!(fs.tree().iter().all(|(path, _)| !path.ends_with(".~part")));
    }
}

#[test]
fn a_failed_trash_puts_back_what_it_moved() {
    let fs = filesystem();
    fs.add_file("C:\\a.txt", b"a").unwrap();
    fs.add_file("C:\\b.txt", b"b").unwrap();
    // a stray file where b.txt would go: the second move fails after a.txt went in
    fs.add_file("C:\\.trash\\2", b"stray").unwrap();
    let paths = ["C:\\a.txt".to_string(), "C:\\b.txt".to_string()];
    assert_eq!(trash::trash(&fs, &paths, 1).err(), Some(Error::Exists));
    assert_eq!(
        fs.entry("C:\\a.txt").map(|e| e.size),
        Ok(1),
        "a.txt went back"
    );
    assert_eq!(fs.entry("C:\\b.txt").map(|e| e.size), Ok(1));
    assert!(trash::list(&fs, 'C').unwrap().is_empty());
}
