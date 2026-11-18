// SPDX-License-Identifier: Apache-2.0
//! Delete for good, depth-first and a few entries per step, so a worker can show progress and stop between steps.

use alloc::string::String;
use alloc::vec::Vec;
use neo_fs::{Error, Fs, Result, att, path};

/// Folders end with `\`, as everywhere in `neo-fs`.
pub struct DeleteTree {
    /// `(path, listed)`: a folder is listed once, its children pushed above it, and removed when it comes back up.
    todo: Vec<(String, bool)>,
    /// Files and folders removed so far.
    pub removed: u64,
    /// Bytes of the files removed so far.
    pub bytes: u64,
}

impl DeleteTree {
    pub fn new(paths: Vec<String>) -> Self {
        DeleteTree {
            todo: paths.into_iter().rev().map(|p| (p, false)).collect(),
            removed: 0,
            bytes: 0,
        }
    }

    pub fn done(&self) -> bool {
        self.todo.is_empty()
    }

    /// Up to `budget` file-server calls; `Ok(true)` once everything is gone. Something already gone counts as done.
    pub fn step(&mut self, fs: &impl Fs, budget: usize) -> Result<bool> {
        for _ in 0..budget.max(1) {
            let Some((item, listed)) = self.todo.pop() else {
                return Ok(true);
            };
            let gone = |r: Result<()>| match r {
                Err(Error::NotFound | Error::PathNotFound) => Ok(()),
                r => r,
            };
            if !item.ends_with('\\') {
                let entry = match fs.entry(&item) {
                    Ok(e) => e,
                    Err(Error::NotFound | Error::PathNotFound) => continue,
                    Err(e) => return Err(e),
                };
                if entry.att & att::READ_ONLY != 0 {
                    fs.set_att(&item, 0, att::READ_ONLY)?;
                }
                gone(fs.delete(&item))?;
                self.removed += 1;
                self.bytes += entry.size;
            } else if listed {
                gone(fs.rmdir(&item))?;
                self.removed += 1;
            } else {
                let children = match fs.read_dir(&item) {
                    Ok(c) => c,
                    Err(Error::NotFound | Error::PathNotFound) => continue,
                    Err(e) => return Err(e),
                };
                self.todo.push((item.clone(), true));
                for child in children {
                    let p = if child.is_dir() {
                        path::join_dir(&item, &child.name)
                    } else {
                        path::join_file(&item, &child.name)
                    };
                    self.todo.push((p, false));
                }
            }
        }
        Ok(self.todo.is_empty())
    }

    /// All of it in one go (recovery at start, tests).
    pub fn run(mut self, fs: &impl Fs) -> Result<(u64, u64)> {
        while !self.step(fs, 64)? {}
        Ok((self.removed, self.bytes))
    }
}
