// SPDX-License-Identifier: Apache-2.0
//! Duplicates for Free up space (files2-cleanup): files of the same size, then the same hash of their first 64 KB,
//! then the same hash of the whole file. Large files and old downloads are plain [`Index`] queries.

use crate::index::Index;
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use neo_fs::{File, Fs, Open};
use xxhash_rust::xxh3::Xxh3;

/// Smaller files aren't worth offering.
pub const DUPLICATE_MIN: u64 = 4 * 1024;
const HEAD: u64 = 64 * 1024;
const CHUNK: usize = 256 * 1024;

/// Finds the duplicate groups a chunk at a time.
pub struct Dupes {
    /// (item, size, head hash), filled as heads are read.
    heads: Vec<(usize, u64, Option<u64>)>,
    next_head: usize,
    /// Items whose whole file still needs hashing, and the ones done.
    full: Vec<(usize, u64)>,
    next_full: usize,
    cur: Option<(Xxh3, u64)>,
    done_full: Vec<(usize, u64, u64)>,
}

fn hash_range(fs: &impl Fs, path: &str, from: u64, len: usize, h: &mut Xxh3) -> Option<usize> {
    let f = fs.open(path, Open::Read).ok()?;
    let mut buf = vec![0u8; len];
    let n = f.read_at(from, &mut buf).ok()?;
    h.update(&buf[..n]);
    Some(n)
}

impl Dupes {
    pub fn new(index: &Index) -> Dupes {
        let mut by_size: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        for i in index.large(DUPLICATE_MIN) {
            by_size.entry(index.items[i].size).or_default().push(i);
        }
        let heads = by_size
            .into_iter()
            .filter(|(_, v)| v.len() > 1)
            .flat_map(|(size, v)| v.into_iter().map(move |i| (i, size, None)))
            .collect();
        Dupes {
            heads,
            next_head: 0,
            full: Vec::new(),
            next_full: 0,
            cur: None,
            done_full: Vec::new(),
        }
    }

    /// Reads up to `budget` chunks; `true` when finished. A file that can't be read drops out.
    pub fn step(&mut self, fs: &impl Fs, index: &Index, budget: usize) -> bool {
        let mut budget = budget.max(1);
        while budget > 0 && self.next_head < self.heads.len() {
            let (i, size, _) = self.heads[self.next_head];
            let mut h = Xxh3::new();
            if hash_range(fs, &index.path(i), 0, size.min(HEAD) as usize, &mut h).is_some() {
                self.heads[self.next_head].2 = Some(h.digest());
            }
            self.next_head += 1;
            budget -= 1;
            if self.next_head == self.heads.len() {
                self.plan_full();
            }
        }
        while budget > 0 && self.next_full < self.full.len() {
            let (i, size) = self.full[self.next_full];
            let (mut h, at) = self.cur.take().unwrap_or((Xxh3::new(), 0));
            let len = (size - at).min(CHUNK as u64) as usize;
            budget -= 1;
            match hash_range(fs, &index.path(i), at, len, &mut h) {
                Some(n) if n > 0 && at + (n as u64) < size => self.cur = Some((h, at + n as u64)),
                Some(n) if n > 0 || size == 0 => {
                    self.done_full.push((i, size, h.digest()));
                    self.next_full += 1;
                }
                _ => self.next_full += 1,
            }
        }
        self.next_head >= self.heads.len() && self.next_full >= self.full.len()
    }

    fn plan_full(&mut self) {
        let mut by_key: BTreeMap<(u64, u64), Vec<usize>> = BTreeMap::new();
        for &(i, size, h) in &self.heads {
            if let Some(h) = h {
                by_key.entry((size, h)).or_default().push(i);
            }
        }
        for ((size, h), v) in by_key {
            if v.len() < 2 {
                continue;
            }
            for i in v {
                if size <= HEAD {
                    self.done_full.push((i, size, h));
                } else {
                    self.full.push((i, size));
                }
            }
        }
    }

    /// Groups of the same bytes, the most wasted space first; the first item of each is the one to keep (the
    /// oldest).
    pub fn groups(&self, index: &Index) -> Vec<Vec<usize>> {
        let mut by_key: BTreeMap<(u64, u64), Vec<usize>> = BTreeMap::new();
        for &(i, size, h) in &self.done_full {
            by_key.entry((size, h)).or_default().push(i);
        }
        let mut out: Vec<Vec<usize>> = by_key.into_values().filter(|v| v.len() > 1).collect();
        for g in &mut out {
            g.sort_by_key(|&i| index.items[i].modified);
        }
        out.sort_by_key(|g| core::cmp::Reverse(index.items[g[0]].size * (g.len() as u64 - 1)));
        out
    }
}
