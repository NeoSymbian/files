// SPDX-License-Identifier: Apache-2.0
use files_ops::index::{Group, Index, Query, Scan};
use files_ops::tidy::Dupes;
use neo_fs::fake::MemFs;
use neo_fs::{Fs, att};

fn fixture() -> MemFs {
    let fs = MemFs::new();
    fs.add_drive('C', 64 << 20, false, false);
    fs.add_drive('E', 64 << 20, false, true);
    fs.add_drive('Z', 64 << 20, true, false);
    fs.add_file("E:\\Music\\song.mp3", &[1; 5000]).unwrap();
    fs.add_file("E:\\Backup\\song.mp3", &[1; 5000]).unwrap();
    fs.add_file("E:\\Backup\\other.mp3", &[2; 5000]).unwrap();
    fs.add_file("E:\\Docs\\Бюджет.xlsx", b"x").unwrap();
    fs.add_file("E:\\Docs\\Ծրագիր.docx", b"d").unwrap();
    fs.add_file("E:\\Downloads\\boarding-pass.pdf", &[3; 70_000])
        .unwrap();
    fs.add_file("E:\\Downloads\\copy.pdf", &[3; 70_000])
        .unwrap();
    let mut tail = vec![3; 70_000];
    tail[69_999] = 4;
    fs.add_file("E:\\Downloads\\tail.pdf", &tail).unwrap();
    fs.add_file("E:\\private\\e0006001\\secret.txt", b"s")
        .unwrap();
    fs.add_file("C:\\sys\\bin\\a.exe", b"e").unwrap();
    fs.add_file("C:\\.trash\\1", b"t").unwrap();
    fs.add_file("C:\\Data\\hidden.txt", b"h").unwrap();
    fs.set_att("C:\\Data\\hidden.txt", att::HIDDEN, 0).unwrap();
    fs.add_file("Z:\\Data\\estart.txt", b"z").unwrap();
    fs
}

fn scan(fs: &MemFs, hidden: bool) -> Index {
    let mut s = Scan::new(&[('C', false), ('E', false), ('Z', true)], hidden);
    let mut steps = 0;
    while !s.step(fs, 2) {
        steps += 1;
    }
    assert!(steps > 1);
    s.finish()
}

fn names(ix: &Index, v: &[usize]) -> Vec<String> {
    let mut n: Vec<String> = v.iter().map(|&i| ix.path(i)).collect();
    n.sort();
    n
}

#[test]
fn scan_skips_system_folders_bins_and_hidden_files() {
    let fs = fixture();
    let ix = scan(&fs, false);
    let all = names(&ix, &ix.search(&Query::default()));
    assert!(all.contains(&"E:\\Music\\song.mp3".to_string()));
    assert!(all.contains(&"Z:\\Data\\estart.txt".to_string()));
    for gone in [
        "secret",
        "a.exe",
        ".trash",
        "hidden",
        "\\private\\",
        "\\sys\\",
    ] {
        assert!(all.iter().all(|p| !p.contains(gone)), "{gone}: {all:?}");
    }
    let backup = ix.items.iter().find(|i| i.name == "Backup").unwrap();
    assert_eq!(backup.count, 2);
    assert!(scan(&fs, true).items.iter().any(|i| i.name == "hidden.txt"));
}

#[test]
fn save_and_load_round_trip_and_a_cut_file_loads_empty() {
    let fs = fixture();
    let ix = scan(&fs, false);
    let file = "C:\\private\\e0006001\\index.dat";
    ix.save(&fs, file).unwrap();
    assert_eq!(Index::load(&fs, file), ix);
    let bytes = ix.encode();
    assert_eq!(Index::decode(&bytes[..bytes.len() - 1]), None);
    assert_eq!(Index::decode(b"NFIX\x09\x00"), None);
    assert_eq!(Index::load(&fs, "C:\\nothing.dat"), Index::default());
}

#[test]
fn search_matches_without_case_in_any_script_and_filters() {
    let fs = fixture();
    let ix = scan(&fs, false);
    let q = |text: &str| Query {
        text: text.into(),
        ..Query::default()
    };
    assert_eq!(
        names(&ix, &ix.search(&q("бюдж"))),
        ["E:\\Docs\\Бюджет.xlsx"]
    );
    assert_eq!(
        names(&ix, &ix.search(&q("ԾՐԱԳ"))),
        ["E:\\Docs\\Ծրագիր.docx"]
    );
    assert_eq!(ix.search(&q("SONG")).len(), 2);
    let folder = Query {
        folder: Some("e:\\backup\\".into()),
        ..q("song")
    };
    assert_eq!(names(&ix, &ix.search(&folder)), ["E:\\Backup\\song.mp3"]);
    let audio = Query {
        group: Some(Group::Audio),
        ..Query::default()
    };
    assert_eq!(ix.search(&audio).len(), 3);
    let big = Query {
        size: Some((60_000, u64::MAX)),
        ..Query::default()
    };
    assert_eq!(ix.search(&big).len(), 3);
    let new = Query {
        since: Some(1),
        ..Query::default()
    };
    assert!(ix.search(&new).len() <= ix.search(&Query::default()).len());
}

#[test]
fn groups_count_writable_drives_only() {
    let fs = fixture();
    let ix = scan(&fs, false);
    let g = ix.groups();
    assert_eq!(g[0], (0, 0)); // Images
    assert_eq!(g[2], (3, 15_000)); // Audio
    assert_eq!(g[3].0, 5); // Documents: xlsx, docx, 3 pdf
    assert_eq!(g[6], (3, 210_000)); // Downloads
    assert_eq!(g[7].0, 0); // Code & text: Z:'s estart.txt is search-only
    assert_eq!(
        ix.in_group(Group::Documents, Some('E'), Some(neo_view::Family::Pdf))
            .len(),
        3
    );
    assert_eq!(names(&ix, &ix.large(60_000)).len(), 3);
    assert_eq!(ix.old_downloads(i64::MAX).len(), 3);
}

#[test]
fn duplicates_need_the_same_bytes_not_just_the_same_size() {
    let fs = fixture();
    let ix = scan(&fs, false);
    let mut d = Dupes::new(&ix);
    while !d.step(&fs, &ix, 1) {}
    let groups: Vec<Vec<String>> = d.groups(&ix).iter().map(|g| names(&ix, g)).collect();
    assert_eq!(
        groups,
        [
            vec![
                "E:\\Downloads\\boarding-pass.pdf".to_string(),
                "E:\\Downloads\\copy.pdf".into()
            ],
            vec![
                "E:\\Backup\\song.mp3".to_string(),
                "E:\\Music\\song.mp3".into()
            ],
        ]
    );
}
