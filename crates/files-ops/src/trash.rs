// SPDX-License-Identifier: Apache-2.0
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use neo_fs::{Entry, Error, File, Fs, Open, Result, att, path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    /// First item ID, or zero for an empty batch; IDs are local to each drive.
    pub id: u32,
    /// Bin paths identify items even when a batch spans drives.
    pub items: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trashed {
    pub id: u32,
    pub original: String,
    pub deleted: i64,
    pub name_in_bin: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Pending,
    Done,
    Restored,
}

#[derive(Clone)]
struct Record {
    item: Trashed,
    state: State,
}

fn bin(drive: char) -> String {
    format!("{}:\\.trash\\", drive.to_ascii_uppercase())
}

fn maybe_entry(fs: &impl Fs, name: &str) -> Result<Option<Entry>> {
    match fs.entry(name) {
        Ok(entry) => Ok(Some(entry)),
        Err(Error::NotFound | Error::PathNotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

fn item_path(root: &str, item: &Trashed) -> String {
    if item.original.ends_with('\\') {
        path::join_dir(root, &item.name_in_bin)
    } else {
        path::join_file(root, &item.name_in_bin)
    }
}

fn journal_text(fs: &impl Fs, root: &str) -> Result<String> {
    let journal = path::join_file(root, "index.dat");
    if maybe_entry(fs, &journal)?.is_none() {
        return Ok(String::new());
    }
    let file = fs.open(&journal, Open::Read)?;
    let length = usize::try_from(file.size()?).map_err(|_| Error::Full)?;
    let mut bytes = vec![0; length];
    let mut offset = 0;
    while offset < length {
        let read = file.read_at(offset as u64, &mut bytes[offset..])?;
        if read == 0 {
            return Err(Error::Other(-20));
        }
        offset += read;
    }
    String::from_utf8(bytes).map_err(|_| Error::Other(-20))
}

fn records(fs: &impl Fs, root: &str) -> Result<Vec<Record>> {
    let text = journal_text(fs, root)?;
    let mut result: Vec<Record> = Vec::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let mut next = || fields.next().ok_or(Error::Other(-20));
        let id: u32 = next()?.parse().map_err(|_| Error::Other(-20))?;
        let state = match next()? {
            "pending" => State::Pending,
            "done" => State::Done,
            "restored" => State::Restored,
            _ => return Err(Error::Other(-20)),
        };
        let deleted = next()?.parse().map_err(|_| Error::Other(-20))?;
        let original = next()?.to_string();
        let name_in_bin = next()?.to_string();
        if fields.next().is_some()
            || id == 0
            || path::drive_of(&original).is_none()
            || name_in_bin != id.to_string()
        {
            return Err(Error::Other(-20));
        }
        let record = Record {
            item: Trashed {
                id,
                original,
                deleted,
                name_in_bin,
            },
            state,
        };
        if let Some(old) = result.iter_mut().find(|old| old.item.id == id) {
            *old = record;
        } else {
            result.push(record);
        }
    }
    Ok(result)
}

fn write_records(fs: &impl Fs, root: &str, records: &[Record], append: bool) -> Result<()> {
    let mut text = if append {
        journal_text(fs, root)?
    } else {
        String::new()
    };
    for record in records {
        let state = match record.state {
            State::Pending => "pending",
            State::Done => "done",
            State::Restored => "restored",
        };
        text.push_str(&format!(
            "{}\t{state}\t{}\t{}\t{}\n",
            record.item.id, record.item.deleted, record.item.original, record.item.name_in_bin
        ));
    }
    let journal = path::join_file(root, "index.dat");
    let part = path::join_file(root, "index.dat.~part");
    let file = fs.open(&part, Open::Replace)?;
    file.write_at(0, text.as_bytes())?;
    file.flush()?;
    drop(file);
    if maybe_entry(fs, &journal)?.is_some() {
        fs.replace(&part, &journal)
    } else {
        fs.rename(&part, &journal)
    }
}

fn writable(fs: &impl Fs, drive: char) -> Result<()> {
    let info = fs
        .drives()?
        .into_iter()
        .find(|info| info.letter.eq_ignore_ascii_case(&drive))
        .ok_or(Error::Gone)?;
    if info.read_only {
        Err(Error::ReadOnly)
    } else {
        Ok(())
    }
}

fn ensure_bin(fs: &impl Fs, root: &str) -> Result<()> {
    match maybe_entry(fs, root)? {
        Some(entry) if !entry.is_dir() => return Err(Error::Exists),
        Some(_) => {}
        None => fs.mkdir(root)?,
    }
    fs.set_att(root, att::HIDDEN | att::SYSTEM, 0)
}

pub fn trash(fs: &impl Fs, paths: &[String], now: i64) -> Result<Batch> {
    for original in paths {
        if original.contains(['\t', '\n', '\r']) {
            return Err(Error::BadName);
        }
        let drive = path::drive_of(original).ok_or(Error::BadName)?;
        writable(fs, drive)?;
        let root = bin(drive);
        if path::is_within(original, &root) || path::eq_ignore_case(original, &root) {
            return Err(Error::IntoItself);
        }
        if path::parent(original).is_none() {
            return Err(Error::BadName);
        }
        fs.entry(original)?;
    }
    let mut batch = Batch {
        id: 0,
        items: Vec::new(),
    };
    for original in paths {
        let drive = path::drive_of(original).ok_or(Error::BadName)?;
        let root = bin(drive);
        ensure_bin(fs, &root)?;
        let mut journal = records(fs, &root)?;
        let id = journal
            .iter()
            .map(|record| record.item.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(Error::Full)?;
        let item = Trashed {
            id,
            original: original.clone(),
            deleted: now,
            name_in_bin: id.to_string(),
        };
        let target = item_path(&root, &item);
        if maybe_entry(fs, &target)?.is_some() {
            return Err(Error::Exists);
        }
        journal.push(Record {
            item,
            state: State::Pending,
        });
        write_records(fs, &root, &journal[journal.len() - 1..], true)?;
        fs.rename(original, &target)?;
        if let Some(last) = journal.last_mut() {
            last.state = State::Done;
        }
        write_records(fs, &root, &journal[journal.len() - 1..], true)?;
        if batch.items.is_empty() {
            batch.id = id;
        }
        batch.items.push(target);
    }
    Ok(batch)
}

fn restore_name(fs: &impl Fs, original: &str) -> Result<String> {
    if maybe_entry(fs, original)?.is_none() {
        return Ok(original.to_string());
    }
    let parent = path::parent(original).ok_or(Error::BadName)?;
    let name = path::name_of(original);
    for n in 1..=u32::MAX {
        let candidate = if original.ends_with('\\') {
            path::join_dir(&parent, &format!("{name} ({n})"))
        } else {
            path::join_file(&parent, &path::keep_both_name(name, n))
        };
        if maybe_entry(fs, &candidate)?.is_none() {
            return Ok(candidate);
        }
    }
    Err(Error::Full)
}

pub fn undo(fs: &impl Fs, batch: &Batch) -> Result<()> {
    for source in &batch.items {
        let drive = path::drive_of(source).ok_or(Error::BadName)?;
        writable(fs, drive)?;
        let root = bin(drive);
        let mut journal = records(fs, &root)?;
        let index = journal
            .iter()
            .position(|record| path::eq_ignore_case(&item_path(&root, &record.item), source))
            .ok_or(Error::NotFound)?;
        if journal[index].state == State::Restored {
            continue;
        }
        if journal[index].state != State::Done {
            return Err(Error::InUse);
        }
        let target = restore_name(fs, &journal[index].item.original)?;
        fs.rename(source, &target)?;
        journal[index].state = State::Restored;
        write_records(fs, &root, &journal[index..=index], true)?;
    }
    Ok(())
}

pub fn recover(fs: &impl Fs, drive: char) -> Result<()> {
    let root = bin(drive);
    if maybe_entry(fs, &root)?.is_none() {
        return Ok(());
    }
    let mut journal = records(fs, &root)?;
    let mut changed = false;
    let mut reconciled = Vec::new();
    for mut record in journal.drain(..) {
        if record.state == State::Pending {
            changed = true;
            if maybe_entry(fs, &item_path(&root, &record.item))?.is_none() {
                continue;
            }
            record.state = State::Done;
        }
        reconciled.push(record);
    }
    if changed {
        write_records(fs, &root, &reconciled, false)?;
    }
    for entry in fs.read_dir(&root)? {
        if !entry.is_dir() && entry.name.to_ascii_lowercase().ends_with(".~part") {
            fs.delete(&path::join_file(&root, &entry.name))?;
        }
    }
    Ok(())
}

pub fn list(fs: &impl Fs, drive: char) -> Result<Vec<Trashed>> {
    Ok(records(fs, &bin(drive))?
        .into_iter()
        .filter(|record| record.state == State::Done)
        .map(|record| record.item)
        .collect())
}
