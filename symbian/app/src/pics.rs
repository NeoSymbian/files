// SPDX-License-Identifier: GPL-3.0-only
//! Pictures on a worker (files-app.md FA5): the viewer's image, the thumbnails and the editor's Save as copy. One
//! thread decodes, so one full-size image at most is in memory at a time (a 12 MP JPEG is 36 MB of the 64 MB heap).
//! The UI posts what it wants ([`Pics::show`], [`Pics::thumbs`], [`Pics::save`]); the viewer's picture and a save go
//! before thumbnails, and thumbnails for rows that scrolled away are dropped when the UI posts the next set. The UI
//! polls [`Pics::take`] like the job slot.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use e32::sync::{Mutex, Semaphore};
use files_ops::store;
use fs_e32::E32Fs;
use neo_format_image::encode;
use neo_format_image::ops::{Edit, View};
use neo_format_image::shrink::{Turn, fit, thumb};
use neo_format_image::{Error, Format, Info, Pixels, decode, probe};
use neo_fs::Fs as _;

/// Thumbnails, kept between runs.
pub const CACHE: &str = "C:\\private\\e0006001\\thumbs\\";
/// A thumbnail's side in px (the grid's 60 dp at 3 px a dp is 180).
pub const SIDE: u32 = 160;
/// What a full decode may hold, besides the file: a 12 MP baseline JPEG is 36 MB as RGB.
pub const BUDGET: usize = 40 << 20;
/// Files larger than this aren't read (the decode needs the whole file in memory).
pub const FILE_MAX: u64 = 40 << 20;
/// The viewer's picture: the long side (zoom to about 2.5× before pixels show).
pub const SHOW_MAX: u32 = 2560;
/// Cached thumbnails kept: at most this many files, the oldest go at start.
const CACHE_FILES: usize = 4000;
const STACK: i32 = 64 * 1024;

pub struct Want {
    pub path: String,
    pub size: u64,
    pub modified: i64,
}

/// The viewer's picture, decoded and turned.
pub struct Shown {
    pub info: Info,
    pub px: Pixels,
    /// The EXIF preview stands in (the image is too big to decode).
    pub preview: bool,
    pub bytes: u64,
    /// An animated GIF's file, for the viewer to step through.
    pub gif: Option<Vec<u8>>,
}

pub enum Done {
    Thumb(String, Option<Pixels>),
    /// The viewer's request number, and the picture or an error code (-1 not an image, -2 too big, -3 broken).
    Show(u32, String, Result<Shown, i32>),
    /// Save as copy: the new file, its size, or an error code.
    Saved(String, Result<(u32, u32), i32>),
}

struct Save {
    src: String,
    dst: String,
    edit: Edit,
}

#[derive(Default)]
struct Queue {
    show: Option<(u32, String)>,
    save: Option<Save>,
    thumbs: VecDeque<Want>,
    done: Vec<Done>,
    busy: bool,
    started: bool,
}

pub struct Pics {
    q: Mutex<Queue>,
    wake: Semaphore,
}

fn code(e: Error) -> i32 {
    match e {
        Error::NotImage => -1,
        Error::TooBig => -2,
        Error::Corrupt => -3,
    }
}

/// The cache file of a thumbnail: FNV-1a of the path, size and time (a changed file gets a new one).
pub fn cache_name(w: &Want) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in w
        .path
        .bytes()
        .chain(w.size.to_le_bytes())
        .chain(w.modified.to_le_bytes())
    {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{CACHE}{h:016x}.jpg")
}

fn read(fs: &E32Fs, path: &str) -> Result<Vec<u8>, i32> {
    let e = fs.entry(path).map_err(|e| e.code())?;
    if e.size > FILE_MAX {
        return Err(-2);
    }
    store::read(fs, path).map_err(|e| e.code())
}

/// A thumbnail: from the cache, or made (a JPEG's EXIF preview when it has one) and cached.
fn make_thumb(fs: &E32Fs, w: &Want) -> Option<Pixels> {
    let name = cache_name(w);
    if let Ok(b) = store::read(fs, &name)
        && let Ok(d) = decode(&b, 1 << 20)
    {
        return Some(d.px);
    }
    let b = read(fs, &w.path).ok()?;
    let info = probe(&b)?;
    let d = match info.thumb.clone() {
        // the camera's preview is enough for a 160 px tile
        Some(r) if info.format == Format::Jpeg => {
            neo_format_image::decode(&b[r], BUDGET).or_else(|_| decode(&b, BUDGET))
        }
        _ => decode(&b, BUDGET),
    }
    .ok()?;
    drop(b);
    let mut t = thumb(&d.px, Turn::exif(info.orientation), SIDE);
    drop(d);
    if t.ch == 4 {
        t = flatten(&t);
    }
    if let Ok(j) = encode::jpeg_rgb(&t.px, t.w as u16, t.h as u16, 80) {
        let _ = store::write(fs, &name, &j);
    }
    Some(t)
}

/// RGBA over the tiles' grey, as RGB.
fn flatten(p: &Pixels) -> Pixels {
    let mut o = Pixels::new(p.w, p.h, 3);
    for (d, s) in
        o.px.as_chunks_mut::<3>()
            .0
            .iter_mut()
            .zip(p.px.as_chunks::<4>().0)
    {
        let a = s[3] as u32;
        for c in 0..3 {
            let bg = [0x2a, 0x2e, 0x35][c];
            d[c] = ((s[c] as u32 * a + bg * (255 - a) + 127) / 255) as u8;
        }
    }
    o
}

fn show(fs: &E32Fs, path: &str) -> Result<Shown, i32> {
    let b = read(fs, path)?;
    let bytes = b.len() as u64;
    let d = decode(&b, BUDGET).map_err(code)?;
    // the file goes before the shrink (a 12 MP JPEG: 36 MB decoded + 4 MB file + 9 MB shrunk otherwise)
    let gif = if d.info.format == Format::Gif && d.info.animated {
        Some(b)
    } else {
        drop(b);
        None
    };
    let px = fit(&d.px, Turn::exif(d.info.orientation), SHOW_MAX);
    Ok(Shown {
        info: d.info,
        px,
        preview: d.preview,
        bytes,
        gif,
    })
}

fn save(fs: &E32Fs, s: &Save) -> Result<(u32, u32), i32> {
    let b = read(fs, &s.src)?;
    let d = decode(&b, BUDGET).map_err(code)?;
    if d.preview {
        // the copy would be the preview's size
        return Err(-2);
    }
    let jpeg = d.info.format == Format::Jpeg;
    drop(b);
    let base = Turn::exif(d.info.orientation);
    let out = match s.edit.size {
        Some(_) => {
            let p = neo_format_image::ops::apply(&d.px, base, &s.edit);
            drop(d);
            let size = (p.w, p.h);
            let bytes = if jpeg {
                encode::jpeg(&p, 90).map_err(code)?
            } else {
                encode::png(&p)
            };
            (bytes, size)
        }
        None => {
            let v = View::new(&d.px, base, &s.edit);
            use neo_format_image::shrink::Rows;
            let size = (v.w(), v.h());
            let bytes = if jpeg {
                encode::jpeg(&v, 90).map_err(code)?
            } else {
                encode::png(&v)
            };
            (bytes, size)
        }
    };
    store::write(fs, &s.dst, &out.0).map_err(|e| e.code())?;
    Ok(out.1)
}

/// Keeps the cache to [`CACHE_FILES`]: the oldest go.
fn prune(fs: &E32Fs) {
    let Ok(mut list) = fs.read_dir(CACHE) else {
        return;
    };
    if list.len() <= CACHE_FILES {
        return;
    }
    list.sort_by_key(|e| e.modified);
    let extra = list.len() - CACHE_FILES;
    for e in list.into_iter().take(extra) {
        let _ = fs.delete(&format!("{CACHE}{}", e.name));
    }
}

impl Pics {
    pub fn new() -> Result<&'static Pics, i32> {
        Ok(Box::leak(Box::new(Pics {
            q: Mutex::new(Queue::default())?,
            wake: Semaphore::new(0)?,
        })))
    }

    /// The worker: started once, it waits on the semaphore between batches.
    fn start(&'static self) {
        let mut q = self.q.lock();
        if q.started {
            return;
        }
        q.started = true;
        drop(q);
        let r = e32::thread::spawn_with_stack(STACK, move || {
            let Ok(fs) = E32Fs::connect() else { return -1 };
            prune(&fs);
            loop {
                self.wake.wait();
                loop {
                    let job = {
                        let mut q = self.q.lock();
                        let j = if let Some(s) = q.save.take() {
                            Some((None, Some(s), None))
                        } else if let Some(s) = q.show.take() {
                            Some((Some(s), None, None))
                        } else {
                            q.thumbs.pop_front().map(|w| (None, None, Some(w)))
                        };
                        q.busy = j.is_some();
                        j
                    };
                    let Some(job) = job else { break };
                    let done = match job {
                        (Some((n, path)), _, _) => {
                            let r = show(&fs, &path);
                            Done::Show(n, path, r)
                        }
                        (_, Some(s), _) => Done::Saved(s.dst.clone(), save(&fs, &s)),
                        (_, _, Some(w)) => {
                            let t = make_thumb(&fs, &w);
                            Done::Thumb(w.path, t)
                        }
                        _ => continue,
                    };
                    self.q.lock().done.push(done);
                }
            }
        });
        if r.is_err() {
            self.q.lock().started = false;
        }
    }

    /// Decode `path` for the viewer (request `n`; an older request still waiting is dropped).
    pub fn show(&'static self, n: u32, path: &str) {
        self.q.lock().show = Some((n, String::from(path)));
        self.start();
        self.wake.signal();
    }

    /// The thumbnails wanted now, in order (what was asked before and not made yet is dropped).
    pub fn thumbs(&'static self, wants: Vec<Want>) {
        {
            let mut q = self.q.lock();
            q.thumbs = wants.into();
        }
        self.start();
        self.wake.signal();
    }

    /// Save `src` with `edit` applied as `dst`.
    pub fn save(&'static self, src: &str, dst: &str, edit: Edit) {
        self.q.lock().save = Some(Save {
            src: String::from(src),
            dst: String::from(dst),
            edit,
        });
        self.start();
        self.wake.signal();
    }

    /// What was made since the last call.
    pub fn take(&self) -> Vec<Done> {
        core::mem::take(&mut self.q.lock().done)
    }

    /// Work waiting or under way (the UI polls more often then).
    pub fn busy(&self) -> bool {
        let q = self.q.lock();
        q.busy || q.show.is_some() || q.save.is_some() || !q.thumbs.is_empty() || !q.done.is_empty()
    }
}
