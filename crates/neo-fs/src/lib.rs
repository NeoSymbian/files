// SPDX-License-Identifier: Apache-2.0
#![no_std]

extern crate alloc;

use alloc::{string::String, vec::Vec};

pub mod fake;
pub mod path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    PathNotFound,
    Exists,
    InUse,
    Full,
    ReadOnly,
    Gone,
    BadName,
    IntoItself,
    SamePlace,
    Cancelled,
    Other(i32),
}

impl Error {
    pub const fn from_code(code: i32) -> Self {
        match code {
            -1 => Self::NotFound,
            -12 => Self::PathNotFound,
            -11 => Self::Exists,
            -14 => Self::InUse,
            -26 => Self::Full,
            -21 => Self::ReadOnly,
            -18 => Self::Gone,
            -28 => Self::BadName,
            -3 => Self::Cancelled,
            n => Self::Other(n),
        }
    }

    pub const fn code(self) -> i32 {
        match self {
            Self::NotFound => -1,
            Self::PathNotFound => -12,
            Self::Exists => -11,
            Self::InUse => -14,
            Self::Full => -26,
            Self::ReadOnly => -21,
            Self::Gone => -18,
            Self::BadName => -28,
            Self::Cancelled => -3,
            // These are job validation errors, represented as KErrArgument at the device boundary.
            Self::IntoItself | Self::SamePlace => -6,
            Self::Other(n) => n,
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

pub mod att {
    pub const READ_ONLY: u32 = 0x01;
    pub const HIDDEN: u32 = 0x02;
    pub const SYSTEM: u32 = 0x04;
    pub const DIR: u32 = 0x10;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub att: u32,
    pub size: u64,
    pub modified: i64,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.att & att::DIR != 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drive {
    pub letter: char,
    pub read_only: bool,
    pub removable: bool,
    pub size: u64,
    pub free: u64,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Open {
    Read,
    Create,
    Replace,
}

pub trait File {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> Result<usize>;
    fn write_at(&self, pos: u64, data: &[u8]) -> Result<()>;
    fn set_size(&self, size: u64) -> Result<()>;
    fn size(&self) -> Result<u64>;
    fn flush(&self) -> Result<()>;
}

pub trait Fs {
    type File<'a>: File
    where
        Self: 'a;

    fn drives(&self) -> Result<Vec<Drive>>;
    fn read_dir(&self, dir: &str) -> Result<Vec<Entry>>;
    fn entry(&self, path: &str) -> Result<Entry>;
    fn mkdir(&self, dir: &str) -> Result<()>;
    fn rmdir(&self, dir: &str) -> Result<()>;
    fn delete(&self, file: &str) -> Result<()>;
    fn rename(&self, from: &str, to: &str) -> Result<()>;
    fn replace(&self, from: &str, to: &str) -> Result<()>;
    fn set_att(&self, path: &str, set: u32, clear: u32) -> Result<()>;
    fn open(&self, path: &str, how: Open) -> Result<Self::File<'_>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbian_codes() {
        let codes = [-1, -12, -11, -14, -26, -21, -18, -28, -3, -999];
        for code in codes {
            assert_eq!(Error::from_code(code).code(), code);
        }
        assert_eq!(Error::from_code(-21), Error::ReadOnly);
        assert_eq!(Error::from_code(-18), Error::Gone);
    }
}
