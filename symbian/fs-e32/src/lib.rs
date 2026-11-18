// SPDX-License-Identifier: GPL-3.0-only
//! [`neo_fs::Fs`] over Symbian's file server ([`e32::fs`]), so `files-ops` (copy, move, trash) runs on the phone as it
//! runs on the host fake. A session belongs to the thread that connected: a worker makes its own [`E32Fs`].
#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use e32::fs::{self, File as RFile, Fs as RFs, mode};
use neo_fs::{Drive, Entry, Error, File, Fs, Open, Result};

pub struct E32Fs(pub RFs);

impl E32Fs {
    pub fn connect() -> Result<Self> {
        RFs::connect().map(E32Fs).map_err(Error::from_code)
    }
}

fn err(code: i32) -> Error {
    Error::from_code(code)
}

fn entry(e: fs::Entry) -> Entry {
    Entry {
        name: e.name,
        att: e.att,
        size: e.size,
        modified: e.modified,
    }
}

pub struct E32File<'a>(RFile<'a>);

impl File for E32File<'_> {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> Result<usize> {
        self.0.read_at64(pos, buf).map_err(err)
    }
    fn write_at(&self, pos: u64, data: &[u8]) -> Result<()> {
        self.0.write_at64(pos, data).map_err(err)
    }
    fn set_size(&self, size: u64) -> Result<()> {
        self.0.set_size64(size).map_err(err)
    }
    fn size(&self) -> Result<u64> {
        self.0.size().map(|s| s.max(0) as u64).map_err(err)
    }
    fn flush(&self) -> Result<()> {
        self.0.flush().map_err(err)
    }
}

impl Fs for E32Fs {
    type File<'a> = E32File<'a>;

    /// The drives that answer: a drive in the list without media (an empty card slot) is left out.
    fn drives(&self) -> Result<Vec<Drive>> {
        let list = self.0.drives().map_err(err)?;
        let mut out = Vec::new();
        for (i, _) in list.iter().enumerate().filter(|(_, a)| **a != 0) {
            let (Ok(info), Ok(v)) = (self.0.drive(i as i32), self.0.volume(i as i32)) else {
                continue;
            };
            out.push(Drive {
                letter: (b'A' + i as u8) as char,
                read_only: info.read_only(),
                removable: info.drive_att & fs::drive_att::REMOVABLE != 0,
                size: v.size.max(0) as u64,
                free: v.free.max(0) as u64,
                label: v.name,
            });
        }
        Ok(out)
    }

    fn read_dir(&self, dir: &str) -> Result<Vec<Entry>> {
        Ok(self
            .0
            .read_dir(dir)
            .map_err(err)?
            .into_iter()
            .map(entry)
            .collect())
    }

    fn entry(&self, path: &str) -> Result<Entry> {
        self.0.entry(trim(path)).map(entry).map_err(err)
    }

    fn mkdir(&self, dir: &str) -> Result<()> {
        self.0.mkdir(dir).map_err(err)
    }

    fn rmdir(&self, dir: &str) -> Result<()> {
        self.0.rmdir(dir).map_err(err)
    }

    fn delete(&self, file: &str) -> Result<()> {
        self.0.delete(file).map_err(err)
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.0.rename(trim(from), trim(to)).map_err(err)
    }

    fn replace(&self, from: &str, to: &str) -> Result<()> {
        self.0.replace_file(from, to).map_err(err)
    }

    /// `RFs::SetEntry` sets the time too: it keeps the entry's own.
    fn set_att(&self, path: &str, set: u32, clear: u32) -> Result<()> {
        let path = trim(path);
        let modified = self.0.entry(path).map_err(err)?.modified;
        self.0.set_entry(path, modified, set, clear).map_err(err)
    }

    fn open(&self, path: &str, how: Open) -> Result<E32File<'_>> {
        let f = match how {
            Open::Read => RFile::open(&self.0, path, mode::SHARE_READERS_ONLY | mode::BIG_FILE),
            Open::Create => RFile::create(&self.0, path, mode::SHARE_EXCLUSIVE | mode::BIG_FILE),
            Open::Replace => RFile::replace(&self.0, path, mode::SHARE_EXCLUSIVE | mode::BIG_FILE),
        };
        f.map(E32File).map_err(err)
    }
}

/// The file server names a folder without its last `\` in Entry, Rename and SetEntry (`X:\a\b`).
fn trim(path: &str) -> &str {
    match path.strip_suffix('\\') {
        Some(p) if !p.ends_with(':') => p,
        _ => path,
    }
}

/// For logs: `path` as the server got it.
pub fn shown(path: &str) -> String {
    String::from(trim(path))
}
