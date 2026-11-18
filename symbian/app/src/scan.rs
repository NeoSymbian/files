// SPDX-License-Identifier: GPL-3.0-only
//! The file index (files-app.md FA3) on its own worker: walk the drives with `files_ops::index::Scan`, save
//! `index.dat`, hand the new index to the UI, and on request look for duplicates in it. The UI polls [`Scanner`]
//! like the job slot; one scan at a time, beside a copy.

use alloc::boxed::Box;
use alloc::vec::Vec;
use e32::sync::Mutex;
use e32::time;
use files_ops::index::{Index, Scan};
use files_ops::tidy::Dupes;
use fs_e32::E32Fs;

/// Where the index is kept.
pub const INDEX: &str = "C:\\private\\e0006001\\index.dat";
const STACK: i32 = 64 * 1024;

#[derive(Default)]
pub struct Found {
    pub index: Option<Index>,
    /// Duplicate groups, as indexes into the index just handed over (or the one sent with the request).
    pub dupes: Option<Vec<Vec<usize>>>,
    pub ms: u64,
    pub running: bool,
}

pub struct Scanner {
    pub found: Mutex<Found>,
}

impl Scanner {
    pub fn new() -> Result<&'static Scanner, i32> {
        Ok(Box::leak(Box::new(Scanner {
            found: Mutex::new(Found::default())?,
        })))
    }

    pub fn running(&self) -> bool {
        self.found.lock().running
    }

    /// Walk `drives` (`(letter, search_only)`) into a new index; then, with `dupes`, find the duplicates in it.
    pub fn start(
        &'static self,
        drives: Vec<(char, bool)>,
        hidden: bool,
        dupes: bool,
    ) -> Result<(), i32> {
        self.found.lock().running = true;
        e32::thread::spawn_with_stack(STACK, move || {
            let start = time::millis();
            let result = E32Fs::connect().map(|fs| {
                let mut scan = Scan::new(&drives, hidden);
                while !scan.step(&fs, 16) {}
                let index = scan.finish();
                let _ = index.save(&fs, INDEX);
                let groups = dupes.then(|| {
                    let mut d = Dupes::new(&index);
                    while !d.step(&fs, &index, 8) {}
                    d.groups(&index)
                });
                (index, groups)
            });
            let mut f = self.found.lock();
            if let Ok((index, groups)) = result {
                f.index = Some(index);
                f.dupes = groups;
            }
            f.ms = time::millis() - start;
            f.running = false;
            0
        })
        .map(drop)
        .inspect_err(|_| self.found.lock().running = false)
    }
}
