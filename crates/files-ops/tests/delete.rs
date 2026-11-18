// SPDX-License-Identifier: Apache-2.0
use files_ops::delete::DeleteTree;
use neo_fs::fake::MemFs;
use neo_fs::{Fs, att};

#[test]
fn deletes_depth_first_in_small_steps_including_read_only_files() {
    let fs = MemFs::new();
    fs.add_drive('C', 1 << 20, false, false);
    fs.add_file("C:\\T\\a\\b\\c.txt", b"ccc").unwrap();
    fs.add_file("C:\\T\\ro.txt", b"r").unwrap();
    fs.add_file("C:\\keep.txt", b"k").unwrap();
    fs.set_att("C:\\T\\ro.txt", att::READ_ONLY, 0).unwrap();
    let mut job = DeleteTree::new(vec!["C:\\T\\".into(), "C:\\missing.txt".into()]);
    let mut steps = 0;
    while !job.step(&fs, 1).unwrap() {
        steps += 1;
    }
    assert!(steps > 3);
    assert_eq!((job.removed, job.bytes), (5, 4));
    assert!(fs.entry("C:\\T\\").is_err());
    assert!(fs.entry("C:\\keep.txt").is_ok());
}
