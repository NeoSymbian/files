// SPDX-License-Identifier: GPL-3.0-only
//! A copy or move on a worker thread (files-app.md "Threads"): `files-ops` steps it with the worker's own file-server
//! session (a session belongs to its thread), and the UI polls [`Slot`] while it runs. A name clash parks the worker
//! on the slot's semaphore until the UI answers; Cancel is a flag it checks between steps. Deleting from the Recycle
//! bin for good (FA3) runs here too: `trash::purge` takes the items out, then a [`DeleteTree`] removes them.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use e32::sync::{Mutex, Semaphore};
use files_ops::delete::DeleteTree;
use files_ops::{Choice, Clash, Kind, Progress, Step, Summary, Transfer, trash};
use fs_e32::E32Fs;
use neo_fs::Error;

/// Bytes a step copies before the worker looks at the slot again.
const STEP: usize = 256 * 1024;
/// The worker's stack: the engine's buffers are on the heap, its paths and the IPC frames on the stack.
const STACK: i32 = 64 * 1024;

/// What a job does.
pub enum Work {
    /// Copy or move `sources` into a folder.
    Transfer(Kind, Vec<String>, String),
    /// Delete these Recycle bin items for good (their bin paths).
    Purge(Vec<String>),
}

/// What the worker and the UI share.
#[derive(Default)]
pub struct Shared {
    pub progress: Option<Progress>,
    /// Asked by the worker; the UI shows it once and answers through [`Slot::answer`].
    pub clash: Option<Clash>,
    answer: Option<(Choice, bool)>,
    cancel: bool,
    pub done: Option<Result<Summary, Error>>,
    /// Set with `done`, and kept when the UI takes `done`.
    ended: bool,
}

/// One job at a time; made once and kept (its mutex and semaphore are kernel objects, and ARMv5TE has no
/// compare-and-swap for an `Arc`).
pub struct Slot {
    pub shared: Mutex<Shared>,
    wake: Semaphore,
}

impl Slot {
    pub fn new() -> Result<&'static Slot, i32> {
        Ok(Box::leak(Box::new(Slot {
            shared: Mutex::new(Shared::default())?,
            wake: Semaphore::new(0)?,
        })))
    }

    /// Start `work` on a new worker thread.
    pub fn start(&'static self, work: Work) -> Result<(), i32> {
        *self.shared.lock() = Shared::default();
        e32::thread::spawn_with_stack(STACK, move || {
            let result = match work {
                Work::Transfer(kind, sources, dest) => run(self, kind, sources, &dest),
                Work::Purge(items) => purge(self, &items),
            };
            let mut s = self.shared.lock();
            s.done = Some(result);
            s.ended = true;
            0
        })
        .map(drop)
    }

    /// Answer the clash the worker waits on; nothing when none is waiting (a signal then would answer the next
    /// job's first clash).
    pub fn answer(&self, choice: Choice, for_all: bool) {
        let waiting = {
            let mut s = self.shared.lock();
            s.answer = Some((choice, for_all));
            s.clash.take().is_some()
        };
        if waiting {
            self.wake.signal();
        }
    }

    /// The worker has finished (its result taken or not).
    pub fn idle(&self) -> bool {
        let s = self.shared.lock();
        s.done.is_some() || s.ended
    }

    /// Stop between steps; a clash waiting for an answer ends too.
    pub fn cancel(&self) {
        let waiting = {
            let mut s = self.shared.lock();
            s.cancel = true;
            s.clash.take().is_some()
        };
        if waiting {
            self.wake.signal();
        }
    }
}

fn run(slot: &Slot, kind: Kind, sources: Vec<String>, dest: &str) -> Result<Summary, Error> {
    let fs = E32Fs::connect()?;
    let mut t = Transfer::new(&fs, kind, sources, dest)?;
    loop {
        if slot.shared.lock().cancel {
            t.cancel(&fs);
            return Err(Error::Cancelled);
        }
        match t.step(&fs, STEP) {
            Step::Progress(p) => slot.shared.lock().progress = Some(p),
            Step::Clash(c) => {
                {
                    // a Cancel that came while this step ran: don't park for an answer nobody will give
                    let mut s = slot.shared.lock();
                    if s.cancel {
                        drop(s);
                        t.cancel(&fs);
                        return Err(Error::Cancelled);
                    }
                    s.clash = Some(c);
                }
                slot.wake.wait();
                let answer = slot.shared.lock().answer.take();
                match answer {
                    Some((choice, all)) => t.answer(choice, all),
                    None => {
                        t.cancel(&fs);
                        return Err(Error::Cancelled);
                    }
                }
            }
            Step::Done(s) => return Ok(s),
            Step::Failed(e) => return Err(e),
        }
    }
}

/// Out of the bin for good; a Cancel stops between steps (what was taken out is gone, the rest stays in the bin).
fn purge(slot: &Slot, items: &[String]) -> Result<Summary, Error> {
    let fs = E32Fs::connect()?;
    let mut tree = DeleteTree::new(trash::purge(&fs, items)?);
    loop {
        if slot.shared.lock().cancel {
            return Err(Error::Cancelled);
        }
        let done = tree.step(&fs, 32)?;
        slot.shared.lock().progress = Some(Progress {
            files_done: tree.removed as usize,
            files_total: items.len(),
            bytes_done: tree.bytes,
            bytes_total: 0,
            current: String::new(),
            current_done: 0,
            current_size: 0,
        });
        if done {
            return Ok(Summary {
                copied: items.len(),
                skipped: 0,
                bytes: tree.bytes,
            });
        }
    }
}
