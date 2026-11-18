// SPDX-License-Identifier: GPL-3.0-only
//! A copy or move on a worker thread (files-app.md "Threads"): `files-ops` steps it with the worker's own file-server
//! session (a session belongs to its thread), and the UI polls [`Slot`] while it runs. A name clash parks the worker
//! on the slot's semaphore until the UI answers; Cancel is a flag it checks between steps.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use e32::sync::{Mutex, Semaphore};
use files_ops::{Choice, Clash, Kind, Progress, Step, Summary, Transfer};
use fs_e32::E32Fs;
use neo_fs::Error;

/// Bytes a step copies before the worker looks at the slot again.
const STEP: usize = 256 * 1024;
/// The worker's stack: the engine's buffers are on the heap, its paths and the IPC frames on the stack.
const STACK: i32 = 64 * 1024;

/// What the worker and the UI share.
#[derive(Default)]
pub struct Shared {
    pub progress: Option<Progress>,
    /// Asked by the worker; the UI shows it once and answers through [`Slot::answer`].
    pub clash: Option<Clash>,
    answer: Option<(Choice, bool)>,
    cancel: bool,
    pub done: Option<Result<Summary, Error>>,
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

    /// Start `kind` of `sources` into `dest` on a new worker thread.
    pub fn start(&'static self, kind: Kind, sources: Vec<String>, dest: String) -> Result<(), i32> {
        *self.shared.lock() = Shared::default();
        e32::thread::spawn_with_stack(STACK, move || {
            let result = run(self, kind, sources, &dest);
            self.shared.lock().done = Some(result);
            0
        })
        .map(drop)
    }

    pub fn answer(&self, choice: Choice, for_all: bool) {
        {
            let mut s = self.shared.lock();
            s.clash = None;
            s.answer = Some((choice, for_all));
        }
        self.wake.signal();
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
                slot.shared.lock().clash = Some(c);
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
