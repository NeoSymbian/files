// SPDX-License-Identifier: Apache-2.0
//! The index of every file (files-app.md FA3): built by a worker with [`Scan`], kept in `C:\private\e0006001\index.dat`,
//! and read by the Types tab, search and Free up space. It skips the system folders (`\private\`, `\sys\`,
//! `\resource\`) and the bins; hidden and system entries only when asked.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use neo_fs::{Fs, Result, att, path};
use neo_view::Family;

/// A file or folder. Folders end their name with nothing special; [`Indexed::is_dir`] tells them apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Indexed {
    /// Into [`Index::dirs`].
    pub dir: u32,
    pub name: String,
    pub size: u64,
    pub modified: i64,
    pub att: u32,
    /// A folder's direct children; 0 for a file.
    pub count: u32,
}

impl Indexed {
    pub fn is_dir(&self) -> bool {
        self.att & att::DIR != 0
    }
}

/// The Types tab's tiles (files2-types), in board order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Images,
    Videos,
    Audio,
    Documents,
    Archives,
    Apps,
    Downloads,
    CodeText,
}

impl Group {
    pub const ALL: [Group; 8] = [
        Group::Images,
        Group::Videos,
        Group::Audio,
        Group::Documents,
        Group::Archives,
        Group::Apps,
        Group::Downloads,
        Group::CodeText,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Group::Images => "Images",
            Group::Videos => "Videos",
            Group::Audio => "Audio",
            Group::Documents => "Documents",
            Group::Archives => "Archives",
            Group::Apps => "Apps",
            Group::Downloads => "Downloads",
            Group::CodeText => "Code & text",
        }
    }

    fn holds(self, family: Family, in_downloads: bool) -> bool {
        match self {
            Group::Images => family == Family::Image,
            Group::Videos => family == Family::Video,
            Group::Audio => family == Family::Audio,
            Group::Documents => matches!(
                family,
                Family::Pdf | Family::Word | Family::Excel | Family::Slides | Family::Ebook
            ),
            Group::Archives => family == Family::Archive,
            Group::Apps => family == Family::App,
            Group::Downloads => in_downloads,
            Group::CodeText => matches!(family, Family::Text | Family::Code),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Index {
    /// Folders, each ending with `\`.
    pub dirs: Vec<String>,
    pub items: Vec<Indexed>,
    /// Drives listed for search only (Z:): left out of the Types tab and Free up space.
    pub search_only: Vec<char>,
}

/// What search looks for (files2-search, files2-search-filters).
#[derive(Clone, Debug, Default)]
pub struct Query {
    pub text: String,
    /// `None` = all drives; else this folder and everything under it.
    pub folder: Option<String>,
    pub group: Option<Group>,
    /// Bytes, both ends inclusive.
    pub size: Option<(u64, u64)>,
    /// Modified at or after (the caller works out "today", "this week"… in its time units).
    pub since: Option<i64>,
}

fn in_downloads(dir: &str) -> bool {
    dir.to_lowercase().contains("\\downloads\\")
}

impl Index {
    /// The full path (folders end with `\`).
    pub fn path(&self, i: usize) -> String {
        let item = &self.items[i];
        let dir = &self.dirs[item.dir as usize];
        if item.is_dir() {
            path::join_dir(dir, &item.name)
        } else {
            path::join_file(dir, &item.name)
        }
    }

    pub fn dir_of(&self, i: usize) -> &str {
        &self.dirs[self.items[i].dir as usize]
    }

    fn counted(&self, i: usize) -> bool {
        let drive = self.dir_of(i).chars().next().unwrap_or('?');
        !self.search_only.contains(&drive)
    }

    /// The items that match, in index order (the caller sorts).
    pub fn search(&self, q: &Query) -> Vec<usize> {
        let needle = q.text.to_lowercase();
        let folder = q.folder.as_ref().map(|f| f.to_lowercase());
        (0..self.items.len())
            .filter(|&i| {
                let item = &self.items[i];
                if !needle.is_empty() && !item.name.to_lowercase().contains(&needle) {
                    return false;
                }
                if let Some(f) = &folder
                    && !self.dir_of(i).to_lowercase().starts_with(f.as_str())
                {
                    return false;
                }
                if (q.group.is_some() || q.size.is_some()) && item.is_dir() {
                    return false;
                }
                if let Some(g) = q.group
                    && !g.holds(
                        neo_view::family_of(&item.name),
                        in_downloads(self.dir_of(i)),
                    )
                {
                    return false;
                }
                if let Some((lo, hi)) = q.size
                    && !(lo..=hi).contains(&item.size)
                {
                    return false;
                }
                if let Some(since) = q.since
                    && item.modified < since
                {
                    return false;
                }
                true
            })
            .collect()
    }

    /// Files in `group`, on `drive` or all counted drives, of `family` or the whole group.
    pub fn in_group(
        &self,
        group: Group,
        drive: Option<char>,
        family: Option<Family>,
    ) -> Vec<usize> {
        (0..self.items.len())
            .filter(|&i| {
                let item = &self.items[i];
                let dir = self.dir_of(i);
                let fam = neo_view::family_of(&item.name);
                !item.is_dir()
                    && self.counted(i)
                    && drive.is_none_or(|d| dir.starts_with(d))
                    && family.is_none_or(|f| f == fam)
                    && group.holds(fam, in_downloads(dir))
            })
            .collect()
    }

    /// (files, bytes) per group, in [`Group::ALL`] order.
    pub fn groups(&self) -> [(u32, u64); 8] {
        let mut out = [(0u32, 0u64); 8];
        for (i, item) in self.items.iter().enumerate() {
            if item.is_dir() || !self.counted(i) {
                continue;
            }
            let fam = neo_view::family_of(&item.name);
            let dl = in_downloads(self.dir_of(i));
            for (k, g) in Group::ALL.iter().enumerate() {
                if g.holds(fam, dl) {
                    out[k].0 += 1;
                    out[k].1 += item.size;
                }
            }
        }
        out
    }

    /// Files at or over `min` bytes, the biggest first.
    pub fn large(&self, min: u64) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.items.len())
            .filter(|&i| !self.items[i].is_dir() && self.counted(i) && self.items[i].size >= min)
            .collect();
        v.sort_by(|&a, &b| self.items[b].size.cmp(&self.items[a].size));
        v
    }

    /// Files under a Downloads folder modified before `before`, the oldest first.
    pub fn old_downloads(&self, before: i64) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.items.len())
            .filter(|&i| {
                let it = &self.items[i];
                !it.is_dir()
                    && self.counted(i)
                    && it.modified < before
                    && in_downloads(self.dir_of(i))
            })
            .collect();
        v.sort_by_key(|&i| self.items[i].modified);
        v
    }

    // ── index.dat: "NFIX", version, then the dirs and the items, little-endian ────────────────────────────────────

    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(64 + self.items.len() * 48);
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&VERSION.to_le_bytes());
        put_str(&mut b, &self.search_only.iter().collect::<String>());
        b.extend_from_slice(&(self.dirs.len() as u32).to_le_bytes());
        for d in &self.dirs {
            put_str(&mut b, d);
        }
        b.extend_from_slice(&(self.items.len() as u32).to_le_bytes());
        for it in &self.items {
            b.extend_from_slice(&it.dir.to_le_bytes());
            put_str(&mut b, &it.name);
            b.extend_from_slice(&it.size.to_le_bytes());
            b.extend_from_slice(&it.modified.to_le_bytes());
            b.extend_from_slice(&it.att.to_le_bytes());
            b.extend_from_slice(&it.count.to_le_bytes());
        }
        b
    }

    /// `None` for anything that isn't a whole index of this version (a cut-short file, an old format).
    pub fn decode(b: &[u8]) -> Option<Index> {
        let mut r = Reader { b, at: 0 };
        if r.take(4)? != MAGIC || r.u16()? != VERSION {
            return None;
        }
        let search_only = r.str()?.chars().collect();
        let n = r.u32()? as usize;
        // counts come from the file: reserve no more than its bytes can hold (2 per dir, 30 per item at least)
        let mut dirs = Vec::with_capacity(n.min(b.len() / 2));
        for _ in 0..n {
            dirs.push(r.str()?);
        }
        let n = r.u32()? as usize;
        let mut items = Vec::with_capacity(n.min(b.len() / 30));
        for _ in 0..n {
            let dir = r.u32()?;
            if dir as usize >= dirs.len() {
                return None;
            }
            items.push(Indexed {
                dir,
                name: r.str()?,
                size: r.u64()?,
                modified: r.u64()? as i64,
                att: r.u32()?,
                count: r.u32()?,
            });
        }
        (r.at == b.len()).then_some(Index {
            dirs,
            items,
            search_only,
        })
    }

    /// Saved through [`crate::store::write`], so a cut leaves the old index whole.
    pub fn save(&self, fs: &impl Fs, file: &str) -> Result<()> {
        crate::store::write(fs, file, &self.encode())
    }

    /// The saved index, or an empty one when there is none or it doesn't read.
    pub fn load(fs: &impl Fs, file: &str) -> Index {
        crate::store::read(fs, file)
            .ok()
            .and_then(|b| Index::decode(&b))
            .unwrap_or_default()
    }
}

const MAGIC: &[u8] = b"NFIX";
const VERSION: u16 = 1;

fn put_str(b: &mut Vec<u8>, s: &str) {
    let s = &s.as_bytes()[..s.len().min(u16::MAX as usize)];
    b.extend_from_slice(&(s.len() as u16).to_le_bytes());
    b.extend_from_slice(s);
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u16()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

/// Top-level folders left out on every drive (data caging and the system), and the bins.
const SKIP: [&str; 4] = ["private", "sys", "resource", ".trash"];

/// Builds an [`Index`] a few folders per step.
pub struct Scan {
    queue: VecDeque<String>,
    hidden: bool,
    pub index: Index,
}

impl Scan {
    /// `drives` as `(letter, search_only)`; `hidden` takes hidden and system entries too.
    pub fn new(drives: &[(char, bool)], hidden: bool) -> Scan {
        let mut index = Index::default();
        let mut queue = VecDeque::new();
        for &(d, search_only) in drives {
            let d = d.to_ascii_uppercase();
            if search_only {
                index.search_only.push(d);
            }
            queue.push_back(alloc::format!("{d}:\\"));
        }
        Scan {
            queue,
            hidden,
            index,
        }
    }

    /// Lists up to `budget` folders; `true` once all are done. A folder that can't be read is left out.
    pub fn step(&mut self, fs: &impl Fs, budget: usize) -> bool {
        for _ in 0..budget.max(1) {
            let Some(dir) = self.queue.pop_front() else {
                return true;
            };
            let Ok(entries) = fs.read_dir(&dir) else {
                continue;
            };
            let top = dir.len() == 3;
            let id = self.index.dirs.len() as u32;
            self.index.dirs.push(dir.clone());
            for e in entries {
                if !self.hidden && e.att & (att::HIDDEN | att::SYSTEM) != 0 {
                    continue;
                }
                if top && e.is_dir() && SKIP.iter().any(|s| e.name.eq_ignore_ascii_case(s)) {
                    continue;
                }
                let mut count = 0;
                if e.is_dir() {
                    let sub = path::join_dir(&dir, &e.name);
                    // children are counted when the folder is listed; patched in `finish`
                    count = u32::MAX;
                    self.queue.push_back(sub);
                }
                self.index.items.push(Indexed {
                    dir: id,
                    name: e.name,
                    size: if count == u32::MAX { 0 } else { e.size },
                    modified: e.modified,
                    att: e.att,
                    count,
                });
            }
        }
        self.queue.is_empty()
    }

    /// The finished index, with each folder's item count filled in.
    pub fn finish(mut self) -> Index {
        let mut counts = alloc::vec![0u32; self.index.dirs.len()];
        for it in &self.index.items {
            counts[it.dir as usize] += 1;
        }
        let ix = &mut self.index;
        let mut by_path: Vec<(String, u32)> = ix
            .dirs
            .iter()
            .enumerate()
            .map(|(i, d)| (d.to_lowercase(), counts[i]))
            .collect();
        by_path.sort();
        for i in 0..ix.items.len() {
            if ix.items[i].count == u32::MAX {
                let p = path::join_dir(&ix.dirs[ix.items[i].dir as usize], &ix.items[i].name)
                    .to_lowercase();
                ix.items[i].count = by_path
                    .binary_search_by(|(d, _)| d.as_str().cmp(&p))
                    .map(|k| by_path[k].1)
                    .unwrap_or(0);
            }
        }
        self.index
    }
}
