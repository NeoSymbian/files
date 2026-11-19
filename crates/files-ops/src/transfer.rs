// SPDX-License-Identifier: Apache-2.0
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use neo_fs::{Entry, Error, File, Fs, Open, Result, path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Copy,
    Move,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Replace,
    KeepBoth,
    Skip,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub files_done: usize,
    pub files_total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub current: String,
    pub current_done: u64,
    pub current_size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clash {
    pub name: String,
    pub here: Entry,
    pub new: Entry,
    pub left: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub copied: usize,
    pub skipped: usize,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Progress(Progress),
    Clash(Clash),
    Done(Summary),
    Failed(Error),
}

struct Node {
    source: String,
    entry: Entry,
    children: Vec<Node>,
    files: usize,
    bytes: u64,
}

enum Task {
    Visit(Node, String),
    KeepBoth(Node, String, u32),
    RemoveDir(String),
}

struct Pending {
    node: Node,
    destination: String,
    clash: Clash,
}

struct Current<H> {
    source: String,
    destination: String,
    part: String,
    read: H,
    write: H,
    offset: u64,
    size: u64,
    replace: bool,
}

pub struct Transfer<'a, F: Fs + 'a> {
    kind: Kind,
    sources: Vec<String>,
    committed: Vec<String>,
    tasks: VecDeque<Task>,
    current: Option<Current<F::File<'a>>>,
    pending: Option<Pending>,
    answer: Option<Choice>,
    for_all: Option<Choice>,
    progress: Progress,
    summary: Summary,
    terminal: Option<Step>,
}

impl<'a, F: Fs + 'a> Transfer<'a, F> {
    pub fn new(fs: &'a F, kind: Kind, sources: Vec<String>, dest_dir: &str) -> Result<Self> {
        let destination_drive = path::drive_of(dest_dir).ok_or(Error::BadName)?;
        let drives = fs.drives()?;
        let destination = drives
            .iter()
            .find(|d| d.letter.eq_ignore_ascii_case(&destination_drive))
            .ok_or(Error::Gone)?;
        if destination.read_only {
            return Err(Error::ReadOnly);
        }
        // a drive's root has no entry on the file server; the drive was found above
        if path::parent(dest_dir).is_some() && !fs.entry(dest_dir)?.is_dir() {
            return Err(Error::PathNotFound);
        }
        let mut tasks = VecDeque::new();
        let mut source_paths = Vec::new();
        let mut files_total = 0;
        let mut bytes_total = 0;
        for source in sources {
            let entry = fs.entry(&source)?;
            if !entry.is_dir() && source.ends_with('\\') {
                return Err(Error::BadName);
            }
            if kind == Kind::Move {
                let source_drive = path::drive_of(&source).ok_or(Error::BadName)?;
                let drive = drives
                    .iter()
                    .find(|d| d.letter.eq_ignore_ascii_case(&source_drive))
                    .ok_or(Error::Gone)?;
                if drive.read_only {
                    return Err(Error::ReadOnly);
                }
            }
            let source = if entry.is_dir() {
                directory(&source)
            } else {
                source
            };
            if entry.is_dir() && path::is_within(dest_dir, &source) {
                return Err(Error::IntoItself);
            }
            if kind == Kind::Move
                && path::parent(&source)
                    .is_some_and(|p| path::eq_ignore_case(&p, &directory(dest_dir)))
            {
                return Err(Error::SamePlace);
            }
            let node = measure(fs, source, entry)?;
            collect_paths(&node, &node.source, &mut source_paths);
            files_total += node.files;
            bytes_total += node.bytes;
            let target = target_path(dest_dir, &node.entry.name, node.entry.is_dir());
            tasks.push_back(Task::Visit(node, target));
        }
        Ok(Self {
            kind,
            sources: source_paths,
            committed: Vec::new(),
            tasks,
            current: None,
            pending: None,
            answer: None,
            for_all: None,
            progress: Progress {
                files_done: 0,
                files_total,
                bytes_done: 0,
                bytes_total,
                current: String::new(),
                current_done: 0,
                current_size: 0,
            },
            summary: Summary::default(),
            terminal: None,
        })
    }

    pub fn step(&mut self, fs: &'a F, budget: usize) -> Step {
        if let Some(result) = &self.terminal {
            return result.clone();
        }
        let result = self.advance(fs, budget);
        if let Err(error) = result {
            self.cleanup(fs);
            let result = Step::Failed(error);
            self.terminal = Some(result.clone());
            return result;
        }
        if let Some(pending) = &self.pending {
            return Step::Clash(pending.clash.clone());
        }
        if self.current.is_none() && self.tasks.is_empty() {
            let result = Step::Done(self.summary);
            self.terminal = Some(result.clone());
            result
        } else {
            Step::Progress(self.progress.clone())
        }
    }

    pub fn answer(&mut self, choice: Choice, for_all: bool) {
        if self.pending.is_some() {
            self.answer = Some(choice);
            if for_all {
                self.for_all = Some(choice);
            }
        }
    }

    pub fn cancel(&mut self, fs: &'a F) {
        if self.terminal.is_none() {
            self.cleanup(fs);
            self.tasks.clear();
            self.pending = None;
            self.terminal = Some(Step::Failed(Error::Cancelled));
        }
    }

    fn advance(&mut self, fs: &'a F, budget: usize) -> Result<()> {
        if self.current.is_some() {
            return self.copy_chunk(fs, budget);
        }
        if self.pending.is_some() {
            if let Some(choice) = self.answer.take() {
                let pending = self.pending.take().ok_or(Error::Other(-2))?;
                return self.apply_choice(fs, pending.node, pending.destination, choice);
            }
            return Ok(());
        }
        match self.tasks.pop_front() {
            Some(Task::Visit(node, destination)) => self.visit(fs, node, destination),
            Some(Task::KeepBoth(node, destination, number)) => {
                let parent = path::parent(&destination).ok_or(Error::BadName)?;
                let name = path::name_of(&destination);
                let name = if node.entry.is_dir() {
                    format!("{name} ({number})")
                } else {
                    path::keep_both_name(name, number)
                };
                let candidate = target_path(&parent, &name, node.entry.is_dir());
                if optional_entry(fs, &candidate)?.is_none() {
                    self.start(fs, node, candidate, false)
                } else {
                    self.tasks.push_front(Task::KeepBoth(
                        node,
                        destination,
                        number.checked_add(1).ok_or(Error::Full)?,
                    ));
                    Ok(())
                }
            }
            Some(Task::RemoveDir(source)) => {
                // Skipped files deliberately keep their source directory alive.
                if fs.read_dir(&source)?.is_empty() {
                    fs.rmdir(&source)?;
                }
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn visit(&mut self, fs: &'a F, node: Node, destination: String) -> Result<()> {
        self.progress.current.clone_from(&node.source);
        self.progress.current_done = 0;
        self.progress.current_size = node.bytes;
        if self.kind == Kind::Copy && path::eq_ignore_case(&node.source, &destination) {
            return self.apply_choice(fs, node, destination, Choice::KeepBoth);
        }
        match optional_entry(fs, &destination)? {
            Some(here) if here.is_dir() && node.entry.is_dir() => {
                self.expand(node, destination);
                Ok(())
            }
            Some(here) => {
                if let Some(choice) = self.for_all {
                    return self.apply_choice(fs, node, destination, choice);
                }
                self.pending = Some(Pending {
                    clash: Clash {
                        name: node.entry.name.clone(),
                        here,
                        new: node.entry.clone(),
                        left: 1,
                    },
                    node,
                    destination,
                });
                Ok(())
            }
            None => self.start(fs, node, destination, false),
        }
    }

    fn apply_choice(
        &mut self,
        fs: &'a F,
        node: Node,
        destination: String,
        choice: Choice,
    ) -> Result<()> {
        match choice {
            Choice::Skip => {
                self.summary.skipped += node.files;
                self.progress.files_done += node.files;
                Ok(())
            }
            Choice::KeepBoth => {
                self.tasks.push_front(Task::KeepBoth(node, destination, 1));
                Ok(())
            }
            Choice::Replace => {
                let here = fs.entry(&destination)?;
                if here.is_dir() || node.entry.is_dir() {
                    return Err(Error::Exists);
                }
                self.start(fs, node, destination, true)
            }
        }
    }

    fn start(&mut self, fs: &'a F, node: Node, destination: String, replace: bool) -> Result<()> {
        if self.kind == Kind::Move && path::drive_of(&node.source) == path::drive_of(&destination) {
            if path::eq_ignore_case(
                node.source.trim_end_matches('\\'),
                destination.trim_end_matches('\\'),
            ) {
                return Err(Error::SamePlace);
            }
            // the old file stays until the new one takes its place (only files get Replace)
            if replace {
                fs.replace(&node.source, &destination)?;
            } else {
                fs.rename(&node.source, &destination)?;
            }
            collect_paths(&node, &destination, &mut self.committed);
            self.summary.copied += node.files;
            self.summary.bytes += node.bytes;
            self.progress.files_done += node.files;
            self.progress.bytes_done += node.bytes;
            self.progress.current_done = node.bytes;
            return Ok(());
        }
        if node.entry.is_dir() {
            fs.mkdir(&destination)?;
            self.expand(node, destination);
            return Ok(());
        }
        let part = format!("{destination}.~part");
        // A selected file or a committed output is never a stale temporary.
        if self
            .sources
            .iter()
            .chain(&self.committed)
            .any(|name| path::eq_ignore_case(name.trim_end_matches('\\'), &part))
        {
            return Err(Error::InUse);
        }
        let read = fs.open(&node.source, Open::Read)?;
        let write = fs.open(&part, Open::Replace)?;
        self.current = Some(Current {
            source: node.source,
            destination,
            part,
            read,
            write,
            offset: 0,
            size: node.bytes,
            replace,
        });
        Ok(())
    }

    fn expand(&mut self, node: Node, destination: String) {
        if self.kind == Kind::Move {
            self.tasks.push_front(Task::RemoveDir(node.source));
        }
        for child in node.children.into_iter().rev() {
            let target = target_path(&destination, &child.entry.name, child.entry.is_dir());
            self.tasks.push_front(Task::Visit(child, target));
        }
    }

    fn copy_chunk(&mut self, fs: &'a F, budget: usize) -> Result<()> {
        let current = self.current.as_mut().ok_or(Error::Other(-2))?;
        if current.offset < current.size {
            let remaining = current.size - current.offset;
            let count = remaining.min(budget.min(64 * 1024) as u64) as usize;
            if count == 0 {
                return Ok(());
            }
            let mut buffer = vec![0; count];
            let read = current.read.read_at(current.offset, &mut buffer)?;
            if read == 0 {
                return Err(Error::Other(-2));
            }
            current.write.write_at(current.offset, &buffer[..read])?;
            current.offset += read as u64;
            self.progress.bytes_done += read as u64;
            self.progress.current_done = current.offset;
            if current.offset < current.size {
                return Ok(());
            }
        }
        current.write.flush()?;
        let Current {
            source,
            destination,
            part,
            read,
            write,
            size,
            replace,
            ..
        } = self.current.take().ok_or(Error::Other(-2))?;
        // e32 refuses rename while a file handle is open.
        drop(write);
        drop(read);
        let commit = if replace {
            fs.replace(&part, &destination)
        } else {
            fs.rename(&part, &destination)
        };
        if let Err(error) = commit {
            let _ = fs.delete(&part);
            return Err(error);
        }
        self.committed.push(destination);
        self.summary.copied += 1;
        self.summary.bytes += size;
        self.progress.files_done += 1;
        if self.kind == Kind::Move {
            fs.delete(&source)?;
        }
        Ok(())
    }

    fn cleanup(&mut self, fs: &F) {
        if let Some(current) = self.current.take() {
            let part = current.part.clone();
            drop(current);
            let _ = fs.delete(&part);
        }
    }
}

pub(crate) fn optional_entry(fs: &impl Fs, path: &str) -> Result<Option<Entry>> {
    match fs.entry(path) {
        Ok(entry) => Ok(Some(entry)),
        Err(Error::NotFound | Error::PathNotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn directory(path: &str) -> String {
    if path.ends_with('\\') {
        path.into()
    } else {
        format!("{path}\\")
    }
}

pub(crate) fn target_path(parent: &str, name: &str, is_dir: bool) -> String {
    if is_dir {
        path::join_dir(parent, name)
    } else {
        path::join_file(parent, name)
    }
}

fn measure(fs: &impl Fs, source: String, entry: Entry) -> Result<Node> {
    let mut node = Node {
        files: usize::from(!entry.is_dir()),
        bytes: if entry.is_dir() { 0 } else { entry.size },
        source,
        entry,
        children: Vec::new(),
    };
    if node.entry.is_dir() {
        for entry in fs.read_dir(&node.source)? {
            let source = target_path(&node.source, &entry.name, entry.is_dir());
            let child = measure(fs, source, entry)?;
            node.files += child.files;
            node.bytes += child.bytes;
            node.children.push(child);
        }
    }
    Ok(node)
}

fn collect_paths(node: &Node, destination: &str, paths: &mut Vec<String>) {
    paths.push(destination.into());
    for child in &node.children {
        let target = target_path(destination, &child.entry.name, child.entry.is_dir());
        collect_paths(child, &target, paths);
    }
}
