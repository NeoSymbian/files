// SPDX-License-Identifier: Apache-2.0
use crate::{Drive, Entry, Error, File, Fs, Open, Result, att, path};
use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
use core::cell::RefCell;

#[derive(Clone)]
struct Node {
    path: String,
    att: u32,
    data: Vec<u8>,
    modified: i64,
}

impl Node {
    fn entry(&self) -> Entry {
        Entry {
            name: String::from(path::name_of(&self.path)),
            att: self.att,
            size: self.data.len() as u64,
            modified: self.modified,
        }
    }
}

struct Volume {
    info: Drive,
    remove_after: Option<usize>,
    gone: bool,
}

#[derive(Default)]
struct State {
    drives: BTreeMap<char, Volume>,
    nodes: BTreeMap<String, Node>,
    clock: i64,
    ops: usize,
    cut_after: Option<usize>,
    power_cut: bool,
}

fn key(path: &str) -> String {
    path.trim_end_matches('\\')
        .chars()
        .flat_map(char::to_lowercase)
        .collect()
}

fn validate(path: &str) -> Result<char> {
    let drive = path::drive_of(path).ok_or(Error::BadName)?;
    let tail = path[3..].strip_suffix('\\').unwrap_or(&path[3..]);
    if !tail.is_empty()
        && tail.split('\\').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.ends_with(['.', ' '])
                || part.encode_utf16().count() > 256
                || part
                    .chars()
                    .any(|c| c.is_control() || "/:*?\"<>|".contains(c))
        })
    {
        return Err(Error::BadName);
    }
    Ok(drive)
}

impl State {
    fn begin(&mut self, drives: &[char], mutate: bool) -> Result<()> {
        if mutate {
            self.ops += 1;
        }
        if self.power_cut {
            return Err(Error::Gone);
        }
        if mutate && let Some(left) = &mut self.cut_after {
            *left = left.saturating_sub(1);
            if *left == 0 {
                self.power_cut = true;
                return Err(Error::Gone);
            }
        }
        for drive in drives {
            let volume = self.drives.get_mut(drive).ok_or(Error::Gone)?;
            if volume.gone {
                return Err(Error::Gone);
            }
            if let Some(left) = &mut volume.remove_after {
                *left = left.saturating_sub(1);
                if *left == 0 {
                    volume.gone = true;
                    return Err(Error::Gone);
                }
            }
            if mutate && volume.info.read_only {
                return Err(Error::ReadOnly);
            }
        }
        Ok(())
    }

    fn used(&self, drive: char) -> u64 {
        self.nodes
            .values()
            .filter(|node| path::drive_of(&node.path) == Some(drive))
            .map(|node| node.data.len() as u64)
            .sum()
    }

    fn tick(&mut self) -> i64 {
        self.clock += 1;
        self.clock
    }

    fn parent_exists(&self, name: &str) -> Result<()> {
        let parent = path::parent(name).ok_or(Error::BadName)?;
        match self.nodes.get(&key(&parent)) {
            Some(node) if node.att & att::DIR != 0 => Ok(()),
            _ => Err(Error::PathNotFound),
        }
    }

    fn writable(&self, name: &str) -> Result<()> {
        if self
            .nodes
            .get(&key(name))
            .is_some_and(|node| node.att & att::READ_ONLY != 0)
        {
            return Err(Error::ReadOnly);
        }
        Ok(())
    }

    fn missing(&self, name: &str) -> Error {
        if self.parent_exists(name).is_ok() {
            Error::NotFound
        } else {
            Error::PathNotFound
        }
    }
}

#[derive(Default)]
pub struct MemFs {
    state: RefCell<State>,
}

impl MemFs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_drive(&self, letter: char, capacity: u64, read_only: bool, removable: bool) {
        let letter = letter.to_ascii_uppercase();
        let mut state = self.state.borrow_mut();
        state.drives.insert(
            letter,
            Volume {
                info: Drive {
                    letter,
                    read_only,
                    removable,
                    size: capacity,
                    free: capacity,
                    label: String::new(),
                },
                remove_after: None,
                gone: false,
            },
        );
        state.nodes.insert(
            key(&format!("{letter}:\\")),
            Node {
                path: format!("{letter}:\\"),
                att: att::DIR,
                data: Vec::new(),
                modified: 0,
            },
        );
    }

    pub fn set_capacity(&self, drive: char, bytes: u64) {
        if let Some(volume) = self
            .state
            .borrow_mut()
            .drives
            .get_mut(&drive.to_ascii_uppercase())
        {
            volume.info.size = bytes;
        }
    }

    pub fn remove_drive_after(&self, drive: char, n_ops: usize) {
        if let Some(volume) = self
            .state
            .borrow_mut()
            .drives
            .get_mut(&drive.to_ascii_uppercase())
        {
            volume.remove_after = Some(n_ops);
        }
    }

    pub fn cut_after(&self, n_ops: usize) {
        self.state.borrow_mut().cut_after = Some(n_ops);
    }

    pub fn restore(&self) {
        let mut state = self.state.borrow_mut();
        state.power_cut = false;
        state.cut_after = None;
        for volume in state.drives.values_mut() {
            volume.remove_after = None;
            volume.gone = false;
        }
    }

    pub fn ops(&self) -> usize {
        self.state.borrow().ops
    }

    pub fn tree(&self) -> Vec<(String, Option<Vec<u8>>)> {
        let mut tree: Vec<_> = self
            .state
            .borrow()
            .nodes
            .values()
            .map(|node| {
                (
                    node.path.clone(),
                    (node.att & att::DIR == 0).then(|| node.data.clone()),
                )
            })
            .collect();
        tree.sort_by(|a, b| a.0.cmp(&b.0));
        tree
    }

    pub fn add_dir(&self, dir: &str) -> Result<()> {
        let drive = validate(dir)?;
        let mut state = self.state.borrow_mut();
        if !state.drives.contains_key(&drive) {
            return Err(Error::Gone);
        }
        let mut current = format!("{drive}:\\");
        for part in dir[3..]
            .trim_end_matches('\\')
            .split('\\')
            .filter(|p| !p.is_empty())
        {
            current = path::join_dir(&current, part);
            let name = key(&current);
            if let Some(node) = state.nodes.get(&name) {
                if node.att & att::DIR == 0 {
                    return Err(Error::Exists);
                }
            } else {
                let modified = state.tick();
                state.nodes.insert(
                    name,
                    Node {
                        path: current.clone(),
                        att: att::DIR,
                        data: Vec::new(),
                        modified,
                    },
                );
            }
        }
        Ok(())
    }

    pub fn add_file(&self, file: &str, bytes: &[u8]) -> Result<()> {
        validate(file)?;
        if file.ends_with('\\') {
            return Err(Error::BadName);
        }
        let parent = path::parent(file).ok_or(Error::BadName)?;
        self.add_dir(&parent)?;
        let mut state = self.state.borrow_mut();
        if state
            .nodes
            .get(&key(file))
            .is_some_and(|n| n.att & att::DIR != 0)
        {
            return Err(Error::Exists);
        }
        let modified = state.tick();
        state.nodes.insert(
            key(file),
            Node {
                path: String::from(file),
                att: 0,
                data: bytes.into(),
                modified,
            },
        );
        Ok(())
    }

    fn relocate(&self, from: &str, to: &str, replace: bool) -> Result<()> {
        let source_drive = validate(from)?;
        let dest_drive = validate(to)?;
        let mut state = self.state.borrow_mut();
        let drives = if source_drive == dest_drive {
            &[source_drive][..]
        } else {
            &[source_drive, dest_drive][..]
        };
        state.begin(drives, true)?;
        if source_drive != dest_drive {
            return Err(Error::BadName);
        }
        if from.len() <= 3 || to.len() <= 3 {
            return Err(Error::BadName);
        }
        let source_key = key(from);
        let dest_key = key(to);
        let source = state
            .nodes
            .get(&source_key)
            .ok_or_else(|| state.missing(from))?;
        let is_dir = source.att & att::DIR != 0;
        if is_dir && path::is_within(to, from) {
            return Err(Error::IntoItself);
        }
        state.parent_exists(to)?;
        state.writable(from)?;
        if let Some(target) = state.nodes.get(&dest_key) {
            if !replace {
                return Err(Error::Exists);
            }
            if is_dir || target.att & att::DIR != 0 {
                return Err(Error::InUse);
            }
            state.writable(to)?;
        } else if replace && is_dir {
            return Err(Error::InUse);
        }
        if replace && source_key == dest_key {
            return Ok(());
        }
        let affected: Vec<_> = state
            .nodes
            .iter()
            .filter(|(_, node)| {
                if is_dir {
                    path::is_within(&node.path, from)
                } else {
                    key(&node.path) == source_key
                }
            })
            .map(|(name, node)| (name.clone(), node.clone()))
            .collect();
        let depth = from.trim_end_matches('\\').matches('\\').count();
        if replace {
            state.nodes.remove(&dest_key);
        }
        for (old_key, mut node) in affected {
            state.nodes.remove(&old_key);
            // Case-equivalent components can have different UTF-8 lengths.
            let suffix_at = if is_dir {
                node.path
                    .match_indices('\\')
                    .nth(depth)
                    .map_or(node.path.len(), |(at, _)| at)
            } else {
                node.path.len()
            };
            let suffix = &node.path[suffix_at..];
            node.path = format!("{}{suffix}", to.trim_end_matches('\\'));
            node.modified = state.tick();
            state.nodes.insert(key(&node.path), node);
        }
        Ok(())
    }
}

impl Fs for MemFs {
    type File<'a> = MemFile<'a>;

    fn drives(&self) -> Result<Vec<Drive>> {
        let mut state = self.state.borrow_mut();
        let letters: Vec<_> = state.drives.keys().copied().collect();
        state.begin(&letters, false)?;
        Ok(state
            .drives
            .values()
            .map(|volume| {
                let mut info = volume.info.clone();
                info.free = info.size.saturating_sub(state.used(info.letter));
                info
            })
            .collect())
    }

    fn read_dir(&self, dir: &str) -> Result<Vec<Entry>> {
        let drive = validate(dir)?;
        let mut state = self.state.borrow_mut();
        state.begin(&[drive], false)?;
        let node = state.nodes.get(&key(dir)).ok_or(Error::PathNotFound)?;
        if node.att & att::DIR == 0 {
            return Err(Error::PathNotFound);
        }
        Ok(state
            .nodes
            .values()
            .filter(|node| {
                path::parent(&node.path).is_some_and(|p| {
                    path::eq_ignore_case(&p, &format!("{}\\", dir.trim_end_matches('\\')))
                })
            })
            .map(Node::entry)
            .collect())
    }

    fn entry(&self, path: &str) -> Result<Entry> {
        let drive = validate(path)?;
        // as RFs::Entry: a drive's root has no entry (t_nmbs.cpp: KErrBadName)
        if path::parent(path).is_none() {
            return Err(Error::BadName);
        }
        let mut state = self.state.borrow_mut();
        state.begin(&[drive], false)?;
        state
            .nodes
            .get(&key(path))
            .map(Node::entry)
            .ok_or_else(|| state.missing(path))
    }

    fn mkdir(&self, dir: &str) -> Result<()> {
        let drive = validate(dir)?;
        let mut state = self.state.borrow_mut();
        state.begin(&[drive], true)?;
        let name = key(dir);
        if state.nodes.contains_key(&name) {
            return Err(Error::Exists);
        }
        state.parent_exists(dir)?;
        let modified = state.tick();
        state.nodes.insert(
            name,
            Node {
                path: format!("{}\\", dir.trim_end_matches('\\')),
                att: att::DIR,
                data: Vec::new(),
                modified,
            },
        );
        Ok(())
    }

    fn rmdir(&self, dir: &str) -> Result<()> {
        let drive = validate(dir)?;
        let mut state = self.state.borrow_mut();
        state.begin(&[drive], true)?;
        if dir.len() <= 3 {
            return Err(Error::InUse);
        }
        let name = key(dir);
        let node = state.nodes.get(&name).ok_or_else(|| state.missing(dir))?;
        if node.att & att::DIR == 0 {
            return Err(Error::PathNotFound);
        }
        state.writable(dir)?;
        if state
            .nodes
            .iter()
            .any(|(key, node)| key != &name && path::is_within(&node.path, dir))
        {
            return Err(Error::InUse);
        }
        state.nodes.remove(&name);
        Ok(())
    }

    fn delete(&self, file: &str) -> Result<()> {
        let drive = validate(file)?;
        let mut state = self.state.borrow_mut();
        state.begin(&[drive], true)?;
        let node = state
            .nodes
            .get(&key(file))
            .ok_or_else(|| state.missing(file))?;
        if node.att & att::DIR != 0 {
            return Err(Error::InUse);
        }
        state.writable(file)?;
        state.nodes.remove(&key(file));
        Ok(())
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.relocate(from, to, false)
    }
    fn replace(&self, from: &str, to: &str) -> Result<()> {
        self.relocate(from, to, true)
    }

    fn set_att(&self, path: &str, set: u32, clear: u32) -> Result<()> {
        let drive = validate(path)?;
        let mut state = self.state.borrow_mut();
        state.begin(&[drive], true)?;
        let modified = state.tick();
        let missing = state.missing(path);
        let node = state.nodes.get_mut(&key(path)).ok_or(missing)?;
        // DIR describes the node's type and cannot be toggled by an attribute update.
        node.att = ((node.att | set) & !clear & !att::DIR) | (node.att & att::DIR);
        node.modified = modified;
        Ok(())
    }

    fn open(&self, path: &str, how: Open) -> Result<Self::File<'_>> {
        let drive = validate(path)?;
        if path.ends_with('\\') || path.len() <= 3 {
            return Err(Error::BadName);
        }
        let mut state = self.state.borrow_mut();
        state.begin(&[drive], how != Open::Read)?;
        let name = key(path);
        if let Some(node) = state.nodes.get(&name) {
            if node.att & att::DIR != 0 {
                return Err(Error::InUse);
            }
            if how == Open::Create {
                return Err(Error::Exists);
            }
            if how == Open::Replace {
                state.writable(path)?;
            }
        } else {
            state.parent_exists(path)?;
            if how == Open::Read {
                return Err(Error::NotFound);
            }
        }
        if how != Open::Read {
            let modified = state.tick();
            let att = state.nodes.get(&name).map_or(0, |node| node.att);
            state.nodes.insert(
                name.clone(),
                Node {
                    path: String::from(path),
                    att,
                    data: Vec::new(),
                    modified,
                },
            );
        }
        Ok(MemFile {
            fs: self,
            name,
            drive,
            writable: how != Open::Read,
        })
    }
}

pub struct MemFile<'a> {
    fs: &'a MemFs,
    name: String,
    drive: char,
    writable: bool,
}

impl MemFile<'_> {
    fn resize_write(&self, size: u64, write: Option<(u64, &[u8])>) -> Result<()> {
        let mut state = self.fs.state.borrow_mut();
        state.begin(&[self.drive], true)?;
        if !self.writable {
            return Err(Error::ReadOnly);
        }
        let node = state.nodes.get(&self.name).ok_or(Error::NotFound)?;
        if node.att & att::READ_ONLY != 0 {
            return Err(Error::ReadOnly);
        }
        let size = if write.is_some() {
            size.max(node.data.len() as u64)
        } else {
            size
        };
        let old = node.data.len() as u64;
        let capacity = state.drives[&self.drive].info.size;
        if size > old && size - old > capacity.saturating_sub(state.used(self.drive)) {
            return Err(Error::Full);
        }
        let len = usize::try_from(size).map_err(|_| Error::Full)?;
        let modified = state.tick();
        let node = state.nodes.get_mut(&self.name).ok_or(Error::NotFound)?;
        node.data.resize(len, 0);
        if let Some((pos, bytes)) = write {
            let start = usize::try_from(pos).map_err(|_| Error::Full)?;
            node.data[start..start + bytes.len()].copy_from_slice(bytes);
        }
        node.modified = modified;
        Ok(())
    }
}

impl File for MemFile<'_> {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> Result<usize> {
        let mut state = self.fs.state.borrow_mut();
        state.begin(&[self.drive], false)?;
        let node = state.nodes.get(&self.name).ok_or(Error::NotFound)?;
        if pos >= node.data.len() as u64 {
            return Ok(0);
        }
        let start = pos as usize;
        let count = buf.len().min(node.data.len() - start);
        buf[..count].copy_from_slice(&node.data[start..start + count]);
        Ok(count)
    }

    fn write_at(&self, pos: u64, data: &[u8]) -> Result<()> {
        let size = pos.checked_add(data.len() as u64).ok_or(Error::Full)?;
        self.resize_write(size, Some((pos, data)))
    }

    fn set_size(&self, size: u64) -> Result<()> {
        self.resize_write(size, None)
    }

    fn size(&self) -> Result<u64> {
        let mut state = self.fs.state.borrow_mut();
        state.begin(&[self.drive], false)?;
        state
            .nodes
            .get(&self.name)
            .map(|node| node.data.len() as u64)
            .ok_or(Error::NotFound)
    }

    fn flush(&self) -> Result<()> {
        let mut state = self.fs.state.borrow_mut();
        state.begin(&[self.drive], true)?;
        if !state.nodes.contains_key(&self.name) {
            return Err(Error::NotFound);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs() -> MemFs {
        let fs = MemFs::new();
        fs.add_drive('C', 1000, false, false);
        fs.add_drive('E', 1000, false, true);
        fs.add_drive('Z', 1000, true, false);
        fs
    }

    fn read(fs: &MemFs, path: &str) -> Vec<u8> {
        let file = fs.open(path, Open::Read).unwrap();
        let mut bytes = alloc::vec![0; file.size().unwrap() as usize];
        file.read_at(0, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn case_insensitive_tree_and_folder_rename() {
        let fs = fs();
        fs.add_file("C:\\Photos\\Nested\\A.JPG", b"image").unwrap();
        assert_eq!(read(&fs, "c:\\photos\\nested\\a.jpg"), b"image");
        assert_eq!(fs.rmdir("C:\\Photos\\"), Err(Error::InUse));
        fs.rename("c:\\photos\\", "C:\\Moved\\").unwrap();
        assert_eq!(read(&fs, "C:\\Moved\\Nested\\A.JPG"), b"image");
        assert_eq!(fs.entry("C:\\Photos\\"), Err(Error::NotFound));
        assert_eq!(fs.read_dir("C:\\Moved\\").unwrap()[0].name, "Nested");
        assert!(fs.tree().contains(&(
            String::from("C:\\Moved\\Nested\\A.JPG"),
            Some(b"image".to_vec())
        )));
    }

    #[test]
    fn folder_rename_handles_case_variants_with_different_utf8_lengths() {
        let fs = fs();
        fs.add_dir("C:\\Ⱥ\\").unwrap();
        fs.add_file("C:\\ⱥ\\file", b"whole").unwrap();
        fs.rename("C:\\Ⱥ\\", "C:\\moved\\").unwrap();
        assert_eq!(read(&fs, "C:\\moved\\file"), b"whole");
    }

    #[test]
    fn atomic_full_sparse_writes_and_sizes() {
        let fs = fs();
        fs.set_capacity('C', 6);
        let file = fs.open("C:\\a", Open::Create).unwrap();
        file.write_at(2, b"abcd").unwrap();
        assert_eq!(read(&fs, "C:\\a"), b"\0\0abcd");
        assert_eq!(file.write_at(1, b"123456"), Err(Error::Full));
        assert_eq!(read(&fs, "C:\\a"), b"\0\0abcd");
        assert_eq!(file.set_size(7), Err(Error::Full));
        file.set_size(3).unwrap();
        assert_eq!(read(&fs, "C:\\a"), b"\0\0a");
        assert_eq!(fs.drives().unwrap()[0].free, 3);
        assert_eq!(fs.open("C:\\a", Open::Create).err(), Some(Error::Exists));
    }

    #[test]
    fn replace_is_atomic_and_rejects_folders() {
        let fs = fs();
        fs.add_file("C:\\a", b"new").unwrap();
        fs.add_file("C:\\b", b"old").unwrap();
        fs.cut_after(1);
        assert_eq!(fs.replace("C:\\a", "C:\\b"), Err(Error::Gone));
        fs.restore();
        assert_eq!(read(&fs, "C:\\a"), b"new");
        assert_eq!(read(&fs, "C:\\b"), b"old");
        fs.replace("C:\\a", "C:\\b").unwrap();
        assert_eq!(read(&fs, "C:\\b"), b"new");
        assert_eq!(fs.entry("C:\\a"), Err(Error::NotFound));
        fs.add_dir("C:\\dir\\").unwrap();
        assert_eq!(fs.replace("C:\\b", "C:\\dir\\"), Err(Error::InUse));
    }

    #[test]
    fn drive_removal_and_power_cut_restore() {
        let fs = fs();
        fs.add_file("E:\\a", b"data").unwrap();
        fs.remove_drive_after('E', 2);
        assert!(fs.entry("E:\\a").is_ok());
        assert_eq!(fs.entry("E:\\a"), Err(Error::Gone));
        assert_eq!(fs.delete("E:\\a"), Err(Error::Gone));
        fs.restore();
        let before = fs.ops();
        fs.cut_after(2);
        fs.mkdir("C:\\one\\").unwrap();
        assert!(fs.entry("C:\\one\\").is_ok());
        assert_eq!(fs.mkdir("C:\\two\\"), Err(Error::Gone));
        assert_eq!(fs.entry("C:\\one\\"), Err(Error::Gone));
        fs.restore();
        assert_eq!(fs.ops(), before + 2);
        assert!(fs.entry("C:\\one\\").is_ok());
        assert_eq!(fs.entry("C:\\two\\"), Err(Error::NotFound));
        assert_eq!(read(&fs, "E:\\a"), b"data");
    }

    #[test]
    fn read_only_names_and_metadata() {
        let fs = fs();
        fs.add_file("Z:\\rom", b"rom").unwrap();
        assert_eq!(fs.mkdir("Z:\\dir\\"), Err(Error::ReadOnly));
        assert_eq!(
            fs.open("Z:\\rom", Open::Replace).err(),
            Some(Error::ReadOnly)
        );
        assert_eq!(fs.delete("Z:\\rom"), Err(Error::ReadOnly));
        assert_eq!(fs.mkdir("C:\\missing\\dir\\"), Err(Error::PathNotFound));
        assert_eq!(fs.mkdir("C:\\..\\"), Err(Error::BadName));
        fs.add_file("C:\\file", b"a").unwrap();
        let old = fs.entry("C:\\file").unwrap().modified;
        fs.set_att("C:\\file", att::HIDDEN | att::READ_ONLY, 0)
            .unwrap();
        let entry = fs.entry("C:\\file").unwrap();
        assert!(entry.modified > old);
        assert_eq!(entry.att, att::HIDDEN | att::READ_ONLY);
        assert_eq!(fs.delete("C:\\file"), Err(Error::ReadOnly));
        fs.set_att("C:\\file", 0, att::READ_ONLY).unwrap();
        fs.delete("C:\\file").unwrap();
    }
}
