// SPDX-License-Identifier: Apache-2.0
//! Archive jobs (files-app.md FA6), stepped like [`Transfer`](crate::Transfer) so a worker can report progress, ask
//! about clashes and stop between steps: [`Extract`] (all of an archive, or some of its members), [`Compress`] (files
//! and folders into a zip or tar.gz) and [`ScanGz`] (the list of a tar.gz, which has no index).
//!
//! Output goes to `.~part` files renamed when complete, as in `transfer`. Every target path is checked to stay inside
//! the destination, besides the member names being safe ([`neo_format_archive::name::safe`]).

use crate::transfer::{directory, optional_entry, target_path};
use crate::{Choice, Clash, Progress, Step, Summary};
use alloc::boxed::Box;
use alloc::collections::{BTreeSet, VecDeque};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;
use neo_format_archive::gzip::{self, GzReader, GzWriter};
use neo_format_archive::tar::{TarReader, TarWriter};
use neo_format_archive::zip::{self, ZipWriter};
use neo_format_archive::{self as ar, At, Format, Kind, Member, ReadAt, Stream};
use neo_fs::{Entry, Error, File, Fs, Open, Result, path};
use neo_view::{Family, family_of};

pub use neo_format_archive::{
    Format as ArchiveFormat, Kind as MemberKind, Member as ArchiveMember,
};

/// Room left on the destination drive besides the output.
const SPARE: u64 = 1 << 20;
/// Days from Symbian's epoch (`TTime` 0, the year 0) to the Unix one, as files-core counts them.
const DAYS_TO_UNIX: i64 = 719_540;

/// A `TTime` (microseconds since 0 AD) from Unix seconds.
pub fn ttime(unix: i64) -> i64 {
    (unix + DAYS_TO_UNIX * 86_400) * 1_000_000
}

/// Unix seconds from a `TTime`.
pub fn unix(ttime: i64) -> i64 {
    ttime.div_euclid(1_000_000) - DAYS_TO_UNIX * 86_400
}

fn err(e: ar::Error) -> Error {
    match e {
        ar::Error::Io(c) => Error::from_code(c),
        // KErrCorrupt
        ar::Error::NotArchive | ar::Error::Corrupt => Error::Other(-20),
        // KErrOverflow: more than the format (or a limit) can hold
        ar::Error::TooBig => Error::Other(-9),
    }
}

/// An open file as a [`ReadAt`], shared by the readers of one job, counting what they read (a tar.gz's progress).
struct Src<H> {
    h: Rc<H>,
    size: u64,
    read: Rc<Cell<u64>>,
}

impl<H> Clone for Src<H> {
    fn clone(&self) -> Self {
        Src {
            h: self.h.clone(),
            size: self.size,
            read: self.read.clone(),
        }
    }
}

impl<H: File> ReadAt for Src<H> {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> ar::Result<usize> {
        let n = self
            .h
            .read_at(pos, buf)
            .map_err(|e| ar::Error::Io(e.code()))?;
        self.read.set(self.read.get() + n as u64);
        Ok(n)
    }
    fn size(&self) -> u64 {
        self.size
    }
}

fn open_src<'a, F: Fs>(fs: &'a F, archive: &str) -> Result<Src<F::File<'a>>> {
    let h = fs.open(archive, Open::Read)?;
    let size = h.size()?;
    Ok(Src {
        h: Rc::new(h),
        size,
        read: Rc::default(),
    })
}

/// What the file is: by its first bytes, then its name (a gzip is a tar.gz when its name or its first block says so).
pub fn format_of<F: Fs>(fs: &F, archive: &str) -> Result<Format> {
    let src = open_src(fs, archive)?;
    let mut head = [0u8; 512];
    let n = src.read_at(0, &mut head).map_err(err)?;
    match ar::detect(&head[..n], archive) {
        Some(Format::Gz) if gzip::holds_tar(&src) => Ok(Format::TarGz),
        Some(f) => Ok(f),
        None => Err(Error::Other(-20)),
    }
}

/// The members of a zip or tar (read at once: a zip's directory, a tar's headers), or a .gz's one file (its size
/// unknown: 0). A tar.gz needs [`ScanGz`].
pub fn list<F: Fs>(fs: &F, archive: &str) -> Result<(Format, Vec<Member>)> {
    let format = format_of(fs, archive)?;
    let src = open_src(fs, archive)?;
    let members = match format {
        Format::Zip => zip::list(&src).map_err(err)?,
        Format::Tar => {
            let mut t = TarReader::new(At::all(src));
            let mut v = Vec::new();
            while let Some(m) = t.next_member().map_err(err)? {
                v.push(m);
            }
            v
        }
        Format::Gz => {
            let g = GzReader::new(At::all(src), 0).map_err(err)?;
            vec![gz_member(archive, g.name.as_deref(), g.mtime)]
        }
        Format::TarGz => return Err(Error::InUse),
    };
    Ok((format, members))
}

/// A .gz's one member: its stored name, else the archive's without `.gz`.
fn gz_member(archive: &str, stored: Option<&str>, mtime: i64) -> Member {
    let own = path::name_of(archive);
    let stem = own
        .len()
        .checked_sub(3)
        .filter(|&n| own.is_char_boundary(n) && own[n..].eq_ignore_ascii_case(".gz"))
        .map_or(own, |n| &own[..n]);
    let raw = stored
        .map(|s| path::name_of(s.trim_end_matches(['/', '\\'])))
        .unwrap_or(stem);
    let (name, kind) = match ar::name::safe(raw) {
        Some(n) if !n.contains('/') => (n, Kind::File),
        // ".gz" alone: no stem to name the file by
        _ if stem.is_empty() => (String::from("file"), Kind::File),
        _ => (String::from(stem), Kind::File),
    };
    Member {
        name,
        kind,
        size: 0,
        packed: 0,
        method: ar::Method::Deflated,
        encrypted: false,
        crc: 0,
        mtime,
        offset: 0,
    }
}

/// A tar.gz listed step by step (it must be inflated to the end to see every header).
pub struct ScanGz<H: File> {
    tar: TarReader<GzReader<At<Src<H>>>>,
    read: Rc<Cell<u64>>,
    size: u64,
    pub members: Vec<Member>,
}

impl<'a, H: File + 'a> ScanGz<H> {
    pub fn new<F: Fs<File<'a> = H>>(fs: &'a F, archive: &str) -> Result<Self> {
        let src = open_src(fs, archive)?;
        let (read, size) = (src.read.clone(), src.size);
        let gz = GzReader::new(At::all(src), u64::MAX).map_err(err)?;
        Ok(ScanGz {
            tar: TarReader::new(gz),
            read,
            size,
            members: Vec::new(),
        })
    }

    /// Up to `n` headers; `Ok(true)` at the end. Progress: compressed bytes read of the file's size.
    pub fn step(&mut self, n: usize) -> Result<bool> {
        for _ in 0..n {
            match self.tar.next_member().map_err(err)? {
                Some(m) => self.members.push(m),
                None => return Ok(true),
            }
        }
        Ok(false)
    }

    pub fn done_of(&self) -> (u64, u64) {
        (self.read.get().min(self.size), self.size)
    }
}

/// Which members to extract: those named in `names` (and everything under a folder named there), placed relative to
/// `base` (the folder being browsed, "" for the top).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pick {
    pub base: String,
    pub names: Vec<String>,
}

impl Pick {
    /// Where `name` goes, relative to the destination; `None` if it isn't picked.
    fn place<'n>(&self, name: &'n str) -> Option<&'n str> {
        let rel = if self.base.is_empty() {
            name
        } else {
            name.strip_prefix(self.base.as_str())?.strip_prefix('/')?
        };
        self.names
            .iter()
            .any(|p| {
                name == p
                    || name
                        .strip_prefix(p.as_str())
                        .is_some_and(|r| r.starts_with('/'))
            })
            .then_some(rel)
    }
}

/// The bytes `members` would take when extracted (what `pick` takes, or all), and how many files.
pub fn needed(members: &[Member], pick: Option<&Pick>) -> (u64, usize) {
    members
        .iter()
        .filter(|m| m.kind == Kind::File && m.extractable())
        .filter(|m| pick.is_none_or(|p| p.place(&m.name).is_some()))
        // a Zip64 can declare sizes that would wrap a sum
        .fold((0u64, 0), |(b, n), m| (b.saturating_add(m.size), n + 1))
}

/// Where the members come from.
enum Source<'s, H: File> {
    Zip {
        src: Src<H>,
        members: VecDeque<Member>,
        reader: Option<Box<zip::Reader<Src<H>>>>,
    },
    /// A tar (plain or inside a gzip).
    Tar(TarReader<Box<dyn Stream + 's>>),
    /// A .gz: one member, then nothing.
    Gz {
        first: Option<Member>,
        reader: Box<GzReader<At<Src<H>>>>,
    },
}

struct Writing<H> {
    part: String,
    target: String,
    out: H,
    offset: u64,
    replace: bool,
}

struct Waiting {
    member: Member,
    target: String,
    clash: Clash,
}

pub struct Extract<'a, F: Fs + 'a> {
    source: Source<'a, F::File<'a>>,
    dest: String,
    pick: Option<Pick>,
    /// Folders known to exist (lowercase).
    made: BTreeSet<String>,
    /// The member being read (its data is the source's current stream).
    current: Option<(Member, String)>,
    writing: Option<Writing<F::File<'a>>>,
    waiting: Option<Waiting>,
    answer: Option<Choice>,
    for_all: Option<Choice>,
    progress: Progress,
    summary: Summary,
    terminal: Option<Step>,
    /// A tar.gz: progress counts compressed bytes read.
    read: Option<Rc<Cell<u64>>>,
    buf: Vec<u8>,
    /// The room on the destination drive, and what was written there so far.
    room: u64,
    written: u64,
    /// Picked files not met yet: at 0 the rest of a tar.gz needn't be inflated.
    wanted: Option<usize>,
}

impl<'a, F: Fs + 'a> Extract<'a, F> {
    /// Extract `archive` into `dest_dir` (made if missing; its parent must exist). `members`: the list, when the
    /// caller has it (a tar.gz's from [`ScanGz`]); a zip or tar is listed here anyway.
    pub fn new(fs: &'a F, archive: &str, pick: Option<Pick>, dest_dir: &str) -> Result<Self> {
        let dest = directory(dest_dir);
        let letter = path::drive_of(&dest).ok_or(Error::BadName)?;
        let drive = fs
            .drives()?
            .into_iter()
            .find(|d| d.letter.eq_ignore_ascii_case(&letter))
            .ok_or(Error::Gone)?;
        if drive.read_only {
            return Err(Error::ReadOnly);
        }
        let room = drive.free.saturating_sub(SPARE);
        let format = format_of(fs, archive)?;
        let src = open_src(fs, archive)?;
        let mut progress = Progress {
            files_done: 0,
            files_total: 0,
            bytes_done: 0,
            bytes_total: 0,
            current: String::new(),
            current_done: 0,
            current_size: 0,
        };
        let mut read = None;
        let source = match format {
            Format::Zip | Format::Tar => {
                let members = if format == Format::Zip {
                    zip::list(&src).map_err(err)?
                } else {
                    let mut t = TarReader::new(At::all(src.clone()));
                    let mut v = Vec::new();
                    while let Some(m) = t.next_member().map_err(err)? {
                        v.push(m);
                    }
                    v
                };
                let (bytes, files) = needed(&members, pick.as_ref());
                if bytes > room {
                    return Err(Error::Full);
                }
                (progress.bytes_total, progress.files_total) = (bytes, files);
                if format == Format::Zip {
                    Source::Zip {
                        src,
                        members: members.into(),
                        reader: None,
                    }
                } else {
                    Source::Tar(TarReader::new(Box::new(At::all(src))))
                }
            }
            Format::TarGz => {
                read = Some(src.read.clone());
                progress.bytes_total = src.size;
                // the room is checked against what is written (unpicked members and headers take none)
                let gz = GzReader::new(At::all(src), u64::MAX).map_err(err)?;
                Source::Tar(TarReader::new(Box::new(gz)))
            }
            Format::Gz => {
                read = Some(src.read.clone());
                progress.bytes_total = src.size;
                progress.files_total = 1;
                let gz = GzReader::new(At::all(src), room).map_err(err)?;
                let m = gz_member(archive, gz.name.as_deref(), gz.mtime);
                Source::Gz {
                    first: Some(m),
                    reader: Box::new(gz),
                }
            }
        };
        // the destination folder, if it isn't there yet
        let mut made = BTreeSet::new();
        // a drive's root has no entry on the file server; the drive was found above
        if let Some(parent) = path::parent(&dest) {
            match optional_entry(fs, &dest)? {
                Some(e) if !e.is_dir() => return Err(Error::Exists),
                Some(_) => {}
                None => {
                    if path::parent(&parent).is_some() && !fs.entry(&parent)?.is_dir() {
                        return Err(Error::PathNotFound);
                    }
                    fs.mkdir(&dest)?;
                }
            }
        }
        made.insert(dest.to_lowercase());
        let wanted = pick.as_ref().map(|p| p.names.len());
        Ok(Extract {
            source,
            dest,
            pick,
            made,
            current: None,
            writing: None,
            waiting: None,
            answer: None,
            for_all: None,
            progress,
            summary: Summary::default(),
            terminal: None,
            read,
            buf: vec![0; 64 * 1024],
            room,
            written: 0,
            wanted,
        })
    }

    /// Every clash is answered Replace (the open-from-archive cache).
    pub fn replace_all(&mut self) {
        self.for_all = Some(Choice::Replace);
    }

    pub fn step(&mut self, fs: &'a F, budget: usize) -> Step {
        if let Some(t) = &self.terminal {
            return t.clone();
        }
        let r = self.advance(fs, budget);
        if let Some(read) = &self.read {
            self.progress.bytes_done = read.get().min(self.progress.bytes_total);
        }
        match r {
            Err(e) => {
                self.cleanup(fs);
                let t = Step::Failed(e);
                self.terminal = Some(t.clone());
                t
            }
            Ok(true) => {
                let t = Step::Done(self.summary);
                self.terminal = Some(t.clone());
                t
            }
            Ok(false) => match &self.waiting {
                Some(w) => Step::Clash(w.clash.clone()),
                None => Step::Progress(self.progress.clone()),
            },
        }
    }

    pub fn answer(&mut self, choice: Choice, for_all: bool) {
        if self.waiting.is_some() {
            self.answer = Some(choice);
            if for_all {
                self.for_all = Some(choice);
            }
        }
    }

    pub fn cancel(&mut self, fs: &'a F) {
        if self.terminal.is_none() {
            self.cleanup(fs);
            self.waiting = None;
            self.terminal = Some(Step::Failed(Error::Cancelled));
        }
    }

    fn cleanup(&mut self, fs: &F) {
        if let Some(w) = self.writing.take() {
            let part = w.part.clone();
            drop(w);
            let _ = fs.delete(&part);
        }
    }

    /// `Ok(true)` when every member is done.
    fn advance(&mut self, fs: &'a F, budget: usize) -> Result<bool> {
        if self.writing.is_some() {
            self.copy(fs, budget)?;
            return Ok(false);
        }
        if self.waiting.is_some() {
            let Some(choice) = self.answer.take() else {
                return Ok(false);
            };
            let Waiting { member, target, .. } = self.waiting.take().ok_or(Error::Other(-2))?;
            return self.choose(fs, member, target, choice).map(|_| false);
        }
        // every picked file met: the rest of a tar.gz isn't inflated for nothing
        if self.wanted == Some(0) {
            return Ok(true);
        }
        let Some(m) = self.next_member()? else {
            return Ok(true);
        };
        let Some(rel) = self
            .pick
            .as_ref()
            .map_or(Some(m.name.as_str()), |p| p.place(&m.name))
        else {
            return Ok(false);
        };
        if !m.extractable() {
            if m.kind != Kind::Dir {
                self.met(&m.name);
                self.summary.skipped += 1;
            }
            return Ok(false);
        }
        let target = format!("{}{}", self.dest, rel.replace('/', "\\"));
        // the names are safe already; this is the second lock on the door
        if !path::is_within(&target, &self.dest) || target.chars().count() > 256 {
            self.summary.skipped += 1;
            return Ok(false);
        }
        self.progress.current = String::from(rel);
        self.progress.current_size = m.size;
        self.progress.current_done = 0;
        if m.kind == Kind::Dir {
            self.make_dirs(fs, &directory(&target))?;
            return Ok(false);
        }
        self.make_dirs(fs, &path::parent(&target).ok_or(Error::BadName)?)?;
        match optional_entry(fs, &target)? {
            None => self.start(fs, m, target, false),
            Some(here) => match self.for_all {
                Some(c) => self.choose(fs, m, target, c),
                None => {
                    let new = Entry {
                        name: String::from(path::name_of(&target)),
                        att: 0,
                        size: m.size,
                        modified: ttime(m.mtime),
                    };
                    self.waiting = Some(Waiting {
                        clash: Clash {
                            name: new.name.clone(),
                            here,
                            new,
                            left: 1,
                        },
                        member: m,
                        target,
                    });
                    Ok(())
                }
            },
        }
        .map(|_| false)
    }

    /// The next member, its data ready to read.
    fn next_member(&mut self) -> Result<Option<Member>> {
        match &mut self.source {
            Source::Zip {
                src,
                members,
                reader,
            } => {
                *reader = None;
                loop {
                    let Some(m) = members.pop_front() else {
                        return Ok(None);
                    };
                    if m.kind == Kind::File && m.extractable() {
                        let picked = self
                            .pick
                            .as_ref()
                            .is_none_or(|p| p.place(&m.name).is_some());
                        if !picked {
                            continue;
                        }
                        *reader = Some(Box::new(zip::open(src.clone(), &m).map_err(err)?));
                    }
                    return Ok(Some(m));
                }
            }
            Source::Tar(t) => t.next_member().map_err(err),
            Source::Gz { first, .. } => Ok(first.take()),
        }
    }

    fn make_dirs(&mut self, fs: &F, dir: &str) -> Result<()> {
        if self.made.contains(&dir.to_lowercase()) {
            return Ok(());
        }
        if !path::is_within(dir, &self.dest) {
            return Err(Error::BadName);
        }
        let parent = path::parent(dir).ok_or(Error::BadName)?;
        self.make_dirs(fs, &parent)?;
        match optional_entry(fs, dir)? {
            Some(e) if e.is_dir() => {}
            Some(_) => return Err(Error::Exists),
            None => fs.mkdir(dir)?,
        }
        self.made.insert(dir.to_lowercase());
        Ok(())
    }

    fn choose(&mut self, fs: &'a F, m: Member, target: String, choice: Choice) -> Result<()> {
        match choice {
            Choice::Skip => {
                self.met(&m.name);
                self.summary.skipped += 1;
                self.progress.files_done += 1;
                self.progress.bytes_done += m.size;
                Ok(())
            }
            Choice::Replace => {
                // a folder isn't replaced by a file: this one is skipped
                if fs.entry(&target)?.is_dir() {
                    self.met(&m.name);
                    self.summary.skipped += 1;
                    return Ok(());
                }
                self.start(fs, m, target, true)
            }
            Choice::KeepBoth => {
                let parent = path::parent(&target).ok_or(Error::BadName)?;
                let name = path::name_of(&target);
                for n in 1..10_000 {
                    let t = path::join_file(&parent, &path::keep_both_name(name, n));
                    if optional_entry(fs, &t)?.is_none() {
                        return self.start(fs, m, t, false);
                    }
                }
                Err(Error::Full)
            }
        }
    }

    fn start(&mut self, fs: &'a F, m: Member, target: String, replace: bool) -> Result<()> {
        let part = format!("{target}.~part");
        let out = fs.open(&part, Open::Replace)?;
        self.current = Some((m, target.clone()));
        self.writing = Some(Writing {
            part,
            target,
            out,
            offset: 0,
            replace,
        });
        Ok(())
    }

    fn copy(&mut self, fs: &'a F, budget: usize) -> Result<()> {
        let mut left = budget.max(1);
        loop {
            let n = {
                let want = self.buf.len().min(left);
                let buf = &mut self.buf[..want];
                match &mut self.source {
                    Source::Zip {
                        reader: Some(r), ..
                    } => r.read(buf),
                    Source::Zip { reader: None, .. } => Ok(0),
                    Source::Tar(t) => t.read(buf),
                    Source::Gz { reader, .. } => reader.read(buf),
                }
                .map_err(|e| match e {
                    // a .gz bigger than the room on the drive
                    ar::Error::TooBig => Error::Full,
                    e => err(e),
                })?
            };
            let w = self.writing.as_mut().ok_or(Error::Other(-2))?;
            if n == 0 {
                break;
            }
            self.written += n as u64;
            if self.written > self.room {
                return Err(Error::Full);
            }
            w.out.write_at(w.offset, &self.buf[..n])?;
            w.offset += n as u64;
            self.progress.current_done = w.offset;
            if self.read.is_none() {
                self.progress.bytes_done += n as u64;
            }
            left = left.saturating_sub(n);
            if left == 0 {
                return Ok(());
            }
        }
        let Writing {
            part,
            target,
            out,
            offset,
            replace,
        } = self.writing.take().ok_or(Error::Other(-2))?;
        let flushed = out.flush();
        // e32 refuses a rename while the file is open
        drop(out);
        if let Err(e) = flushed {
            let _ = fs.delete(&part);
            return Err(e);
        }
        let done = if replace {
            fs.replace(&part, &target)
        } else {
            fs.rename(&part, &target)
        };
        if let Err(e) = done {
            let _ = fs.delete(&part);
            return Err(e);
        }
        if let Some((m, _)) = self.current.take() {
            self.met(&m.name);
        }
        self.summary.copied += 1;
        self.summary.bytes += offset;
        self.progress.files_done += 1;
        Ok(())
    }

    /// A picked file is done (extracted or skipped).
    fn met(&mut self, name: &str) {
        if let (Some(w), Some(p)) = (self.wanted.as_mut(), self.pick.as_ref())
            && p.names.iter().any(|n| n == name)
        {
            *w = w.saturating_sub(1);
        }
    }
}

/// What [`Compress`] writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packing {
    Zip,
    TarGz,
}

/// Whether a file is stored rather than deflated: its own format compresses already.
pub fn compressed_already(name: &str) -> bool {
    matches!(
        family_of(name),
        Family::Audio | Family::Video | Family::Image | Family::Archive
    ) && !name.to_ascii_lowercase().ends_with(".bmp")
}

/// The sources' bytes, and the archive's size to expect (stored as they are when already compressed, about half
/// otherwise), and the bytes already compressed.
pub fn estimate<F: Fs>(fs: &F, sources: &[String]) -> Result<(u64, u64, u64)> {
    let mut files = Vec::new();
    for s in sources {
        let e = fs.entry(s)?;
        let s = if e.is_dir() { directory(s) } else { s.clone() };
        walk(fs, &s, &e, String::from(&*e.name), &mut files)?;
    }
    let (mut total, mut packed) = (0u64, 0u64);
    for (_, name, e) in &files {
        if !e.is_dir() {
            total += e.size;
            if compressed_already(name) {
                packed += e.size;
            }
        }
    }
    Ok((total, packed + (total - packed) / 2, packed))
}

fn walk<F: Fs>(
    fs: &F,
    source: &str,
    e: &Entry,
    name: String,
    out: &mut Vec<(String, String, Entry)>,
) -> Result<()> {
    out.push((String::from(source), name.clone(), e.clone()));
    if e.is_dir() {
        let mut children = fs.read_dir(source)?;
        children.sort_by(|a, b| a.name.cmp(&b.name));
        for c in children {
            let p = target_path(source, &c.name, c.is_dir());
            walk(fs, &p, &c, format!("{name}/{}", c.name), out)?;
        }
    }
    Ok(())
}

/// The output file, buffered; a patch inside the buffer stays there.
struct Out<H: File> {
    h: H,
    /// Where `buf` starts in the file.
    at: u64,
    buf: Vec<u8>,
}

impl<H: File> Out<H> {
    fn flush(&mut self) -> ar::Result<()> {
        if !self.buf.is_empty() {
            self.h
                .write_at(self.at, &self.buf)
                .map_err(|e| ar::Error::Io(e.code()))?;
            self.at += self.buf.len() as u64;
            self.buf.clear();
        }
        Ok(())
    }
}

impl<H: File> ar::Sink for Out<H> {
    fn write(&mut self, data: &[u8]) -> ar::Result<()> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= 64 * 1024 {
            self.flush()?;
        }
        Ok(())
    }
}

impl<H: File> ar::Patch for Out<H> {
    fn pos(&self) -> u64 {
        self.at + self.buf.len() as u64
    }
    fn patch(&mut self, at: u64, data: &[u8]) -> ar::Result<()> {
        if at >= self.at {
            let i = (at - self.at) as usize;
            if let Some(b) = self.buf.get_mut(i..i + data.len()) {
                b.copy_from_slice(data);
                return Ok(());
            }
        }
        self.flush()?;
        self.h
            .write_at(at, data)
            .map_err(|e| ar::Error::Io(e.code()))
    }
}

enum Writer<H: File> {
    Zip(ZipWriter<Out<H>>),
    TarGz(TarWriter<GzWriter<Out<H>>>),
}

struct Reading<H> {
    h: H,
    offset: u64,
    size: u64,
}

pub struct Compress<'a, F: Fs + 'a> {
    out: String,
    part: String,
    writer: Option<Writer<F::File<'a>>>,
    items: VecDeque<(String, String, Entry)>,
    reading: Option<Reading<F::File<'a>>>,
    buf: Vec<u8>,
    progress: Progress,
    summary: Summary,
    terminal: Option<Step>,
}

impl<'a, F: Fs + 'a> Compress<'a, F> {
    /// `sources` (files and folders, each named in the archive by its own name) into the new file `out`.
    pub fn new(fs: &'a F, sources: &[String], out: &str, packing: Packing) -> Result<Self> {
        if optional_entry(fs, out)?.is_some() {
            return Err(Error::Exists);
        }
        let mut items = Vec::new();
        for s in sources {
            let e = fs.entry(s)?;
            let s = if e.is_dir() { directory(s) } else { s.clone() };
            walk(fs, &s, &e, String::from(&*e.name), &mut items)?;
        }
        let part = format!("{out}.~part");
        // the output and its part may land inside a folder being compressed: never in the archive
        items.retain(|(p, ..)| !path::eq_ignore_case(p, out) && !path::eq_ignore_case(p, &part));
        let (bytes, files) = items
            .iter()
            .filter(|i| !i.2.is_dir())
            .fold((0, 0), |(b, n), i| (b + i.2.size, n + 1));
        let h = fs.open(&part, Open::Replace)?;
        let o = Out {
            h,
            at: 0,
            buf: Vec::with_capacity(64 * 1024),
        };
        let writer = match packing {
            Packing::Zip => Writer::Zip(ZipWriter::new(o)),
            Packing::TarGz => Writer::TarGz(TarWriter::new(
                GzWriter::new(o, None, unix(items.first().map_or(0, |i| i.2.modified)))
                    .map_err(err)?,
            )),
        };
        Ok(Compress {
            out: String::from(out),
            part,
            writer: Some(writer),
            items: items.into(),
            reading: None,
            buf: vec![0; 64 * 1024],
            progress: Progress {
                files_done: 0,
                files_total: files,
                bytes_done: 0,
                bytes_total: bytes,
                current: String::new(),
                current_done: 0,
                current_size: 0,
            },
            summary: Summary::default(),
            terminal: None,
        })
    }

    pub fn step(&mut self, fs: &'a F, budget: usize) -> Step {
        if let Some(t) = &self.terminal {
            return t.clone();
        }
        match self.advance(fs, budget) {
            Err(e) => {
                self.cleanup(fs);
                let t = Step::Failed(e);
                self.terminal = Some(t.clone());
                t
            }
            Ok(true) => {
                let t = Step::Done(self.summary);
                self.terminal = Some(t.clone());
                t
            }
            Ok(false) => Step::Progress(self.progress.clone()),
        }
    }

    pub fn cancel(&mut self, fs: &'a F) {
        if self.terminal.is_none() {
            self.cleanup(fs);
            self.terminal = Some(Step::Failed(Error::Cancelled));
        }
    }

    fn cleanup(&mut self, fs: &F) {
        self.reading = None;
        self.writer = None;
        let _ = fs.delete(&self.part);
    }

    fn advance(&mut self, fs: &'a F, budget: usize) -> Result<bool> {
        let w = self.writer.as_mut().ok_or(Error::Other(-2))?;
        if let Some(r) = self.reading.as_mut() {
            let mut left = budget.max(1) as u64;
            while r.offset < r.size && left > 0 {
                let n = (r.size - r.offset).min(left).min(self.buf.len() as u64) as usize;
                let got = r.h.read_at(r.offset, &mut self.buf[..n])?;
                if got == 0 {
                    // the file shrank while it was being read: KErrCorrupt
                    return Err(Error::Other(-20));
                }
                match w {
                    Writer::Zip(z) => z.write(&self.buf[..got]),
                    Writer::TarGz(t) => t.write(&self.buf[..got]),
                }
                .map_err(err)?;
                r.offset += got as u64;
                left -= got as u64;
                self.progress.bytes_done += got as u64;
                self.progress.current_done = r.offset;
            }
            if r.offset == r.size {
                self.summary.copied += 1;
                self.summary.bytes += r.size;
                self.progress.files_done += 1;
                self.reading = None;
            }
            return Ok(false);
        }
        let Some((source, name, e)) = self.items.pop_front() else {
            let w = self.writer.take().ok_or(Error::Other(-2))?;
            let mut o = match w {
                Writer::Zip(z) => z.finish(),
                Writer::TarGz(t) => t.finish().and_then(|g| g.finish()),
            }
            .map_err(err)?;
            o.flush().map_err(err)?;
            o.h.flush()?;
            drop(o);
            fs.rename(&self.part, &self.out)?;
            return Ok(true);
        };
        let mtime = unix(e.modified);
        self.progress.current = name.clone();
        self.progress.current_done = 0;
        self.progress.current_size = e.size;
        if e.is_dir() {
            match w {
                Writer::Zip(z) => z.dir(&name, mtime),
                Writer::TarGz(t) => t.dir(&name, mtime),
            }
            .map_err(err)?;
            return Ok(false);
        }
        let h = fs.open(&source, Open::Read)?;
        match w {
            Writer::Zip(z) => z.start(&name, mtime, !compressed_already(&name)),
            Writer::TarGz(t) => t.start(&name, e.size, mtime),
        }
        .map_err(err)?;
        self.reading = Some(Reading {
            h,
            offset: 0,
            size: e.size,
        });
        Ok(false)
    }
}
