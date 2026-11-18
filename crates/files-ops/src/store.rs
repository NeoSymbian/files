// SPDX-License-Identifier: Apache-2.0
//! Files' own small files in `C:\private\e0006001\` (recent, favourites, the index): read whole, written as
//! `<name>.~part` and then put in place, so a power cut leaves the old file whole.

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use neo_fs::{Error, File, Fs, Open, Result, path};

/// Files' private folder.
pub const HOME: &str = "C:\\private\\e0006001\\";

pub fn read(fs: &impl Fs, file: &str) -> Result<Vec<u8>> {
    let f = fs.open(file, Open::Read)?;
    let len = usize::try_from(f.size()?).map_err(|_| Error::Full)?;
    let mut b = vec![0; len];
    let mut at = 0;
    while at < len {
        let n = f.read_at(at as u64, &mut b[at..])?;
        if n == 0 {
            return Err(Error::Other(-20));
        }
        at += n;
    }
    Ok(b)
}

pub fn write(fs: &impl Fs, file: &str, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path::parent(file)
        && fs.entry(&dir).is_err()
    {
        mkdirs(fs, &dir)?;
    }
    let part = format!("{file}.~part");
    let f = fs.open(&part, Open::Replace)?;
    f.write_at(0, bytes)?;
    f.flush()?;
    drop(f);
    match fs.entry(file) {
        Ok(_) => fs.replace(&part, file),
        Err(_) => fs.rename(&part, file),
    }
}

fn mkdirs(fs: &impl Fs, dir: &str) -> Result<()> {
    if let Some(up) = path::parent(dir)
        && fs.entry(&up).is_err()
    {
        mkdirs(fs, &up)?;
    }
    match fs.mkdir(dir) {
        Err(Error::Exists) | Ok(()) => Ok(()),
        Err(e) => Err(e),
    }
}
