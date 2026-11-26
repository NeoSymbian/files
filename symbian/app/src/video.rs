// SPDX-License-Identifier: GPL-3.0-only
//! FA12, the video player (files2-view-video): MP4, MOV, 3GP and Matroska/WebM with H.264, VP8 or VP9 pictures and
//! AAC, Vorbis or Opus sound (neo-format-media's `av`).
//!
//! One worker thread reads the file a packet at a time, decodes the pictures a few ahead and the sound a little ahead,
//! and feeds NeoAudio without blocking (a WRITE only while its queue has room). The sound is the clock: what NeoAudio
//! has taken, less what still sits in the ADSP (about 160 ms), interpolated between reads; a clip without sound runs on
//! the fast counter. A second thread, the converter (on the other fast core), turns each picture into screen pixels
//! ahead of its time: with the controls hidden straight into the app's surface (neoapp `App::raw_target`), else into a
//! Slint image for under the controls. The UI thread only times them: at a picture's time it sends the DAMAGE (or sets
//! the image), drops the late ones and hands the converter the next.
//!
//! While a clip plays the worker keeps the CPUs at their top clocks (HAL ENeoHalCpuBoost, kernel#44): nobody touches the
//! phone, and the fast cores otherwise sit at 300 MHz. A call (NeoAudio PREEMPTED) or leaving Files pauses it. The place
//! in each clip is kept in `C:\private\e0006001\videos.dat`.

use crate::audio::{At, Out};
use crate::fa7::Open7;
use crate::{FilesApp, State};
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use e32::sync::{Mutex, Semaphore};
use e32::{dprintln, time};
use files_ops::store;
use files_ui::video::{self as screen, VideoPlayer};
use fs_e32::E32Fs;
use neo_format_media::av::video::Frame;
use neo_format_media::av::yuv::{self, Turn};
use neo_format_media::av::{self, sound, video};
use neo_fs::{File as _, Fs as _, Open};
use neoapp::App;
use neoaudio::{op, state};
use neoui_proto::Rect;
use slint::{Image, Rgb8Pixel, SharedPixelBuffer};

/// The decoders' tables and rust_h264's slice state.
const STACK: i32 = 512 * 1024;
/// Pictures decoded ahead (720p: 1.4 MB each).
const AHEAD: usize = 4;
/// The most pictures queued even while the sound wants more packets: the queue is bounded (8 at 1080p is 25 MB of
/// the 64 MB heap). Without it the worker read on for sound and queued pictures without limit (VP8, Pixel).
const AHEAD_MAX: usize = 8;
/// Sound decoded ahead of NeoAudio, besides its own 250 ms queue.
const SOUND_AHEAD_US: u64 = 400_000;
/// NeoAudio's queue is topped up while it holds less than this.
const QUEUE_US: u64 = 150_000;
/// INFO's frames run ahead of the speaker by the ADSP's periods (docs hardware/audio.md: eight of 20 ms).
const LATENCY_US: i64 = 160_000;
/// Each media-boost ask, renewed every second while playing.
const BOOST_MS: usize = 2000;
const BOOST_EVERY_US: u32 = 1_000_000;
/// The worker's wait while it has nothing to do but play.
const IDLE_US: i32 = 10_000;
/// The UI's poll while a clip is open and nothing is due sooner (the seek bar, the controls' timer).
pub const POLL_US: i32 = 250_000;
/// The largest picture played (level 4.1).
const MAX_PIXELS: u32 = 1920 * 1088;
/// The largest picture the 64 MB heap holds with the queue and the decoder's references: 1080p ran the worker out of
/// memory (Pixel, 2026-11-26). shortcut: raise to MAX_PIXELS when e32-rs#17 (heap 256 MB) is in Files' e32 pin.
const HEAP_PIXELS: u32 = 1280 * 736;
const PLACES: &str = "C:\\private\\e0006001\\videos.dat";
const PLACES_MAX: usize = 200;

/// The ENeoHalCpuBoost HAL call (variant group, 0x4e29): top clocks for `ms`. Files' e32 pin predates its wrapper.
fn boost(ms: usize) {
    // SAFETY: the kernel takes the milliseconds as a plain value in a1; on a kernel without it the call fails harmlessly.
    unsafe {
        e32::sys::ui::hal_function(1, 0x4e29, ms as *mut u8, core::ptr::null_mut());
    }
}

/// The Pixel's A75s (CPUs 6-7 are the fast cluster): the decoder on one, the converter on the other.
const FAST_CPU: usize = 7;
const CONVERT_CPU: usize = 6;
/// The converter's stack: yuv::draw keeps its colour tables (5 KB) there.
const CONVERT_STACK: i32 = 64 * 1024;
/// The longest the UI waits for the converter before Slint may draw over the surface picture it is writing.
const CONVERT_WAIT_US: i32 = 200_000;
/// The clock is refreshed at most this often while playing.
const TICK_US: u32 = 4_000;
/// A small core for the converter's helper (0-5 are A55s).
const HELPER_CPU: usize = 5;
/// The helper's share of a big picture's rows, percent: at 30 the A55 finished 3-4 ms after the A75 (Pixel).
const HELPER_SHARE: u32 = 25;
/// Pictures at least this big (full-screen landscape) are split with the helper; portrait's 1080x607 is not.
const SPLIT_PIXELS: u32 = 1_200_000;
/// A helper this late has died.
const HELPER_WAIT_US: i32 = 1_000_000;
/// NeoAudio's queue standing still this long at the end of a clip: the sound is over.
const DRAIN_STUCK_US: u32 = 500_000;
/// A worker silent this long while playing has died.
const WORKER_DEAD_US: u32 = 2_000_000;
/// A converter silent this long has died (a panic ends the thread): the clip stops with the error card.
const CONVERT_DEAD_US: u32 = 2_000_000;

/// `EKernelHalLockThreadToCpu` (kernel group 0, function 19): Files' e32 pin predates `thread::lock_to_cpu`.
fn pin(cpu: usize) {
    // SAFETY: the kernel takes the CPU as a plain value in a1; a CPU it doesn't run is refused (KErrArgument).
    unsafe {
        e32::sys::ui::hal_function(0, 19, cpu as *mut u8, core::ptr::null_mut());
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Play {
    #[default]
    Opening,
    Playing,
    Paused,
    Ended,
}

enum Cmd {
    /// Request number, path, where to start (µs), playing.
    Open(u32, String, u64, bool),
    Play,
    Pause,
    Seek(u64),
    /// Percent.
    Speed(u32),
    Mute(bool),
    Close,
}

/// What the opened clip is.
#[derive(Clone, Debug)]
pub struct Clip {
    pub size: (u32, u32),
    pub duration_us: u64,
    /// Why there is no sound ("AMR isn't supported"), if not.
    pub silent: Option<String>,
}

/// The clock as the worker last read it: the media time at a fast-counter reading, and whether it runs.
#[derive(Clone, Copy, Default)]
struct Clock {
    us: i64,
    at: u32,
    running: bool,
    /// Percent: how fast media time passes against the counter.
    speed: u32,
}

#[derive(Default)]
struct Shared {
    cmds: VecDeque<Cmd>,
    n: u32,
    clip: Option<Result<Clip, String>>,
    frames: VecDeque<Frame>,
    state: Play,
    clock: Clock,
    buffered_us: u64,
    started: bool,
    /// Pictures decoded and the time spent decoding them (µs), since the UI last read them.
    decoded: u32,
    decode_us: u64,
    /// Counts the seeks: a picture taken before one is never shown after it.
    seeks: u32,
    /// Counts the worker's turns: one that stands still has died (a panic ends the thread).
    beat: u32,
    /// The slowest picture's decode (this window).
    decode_max_us: u32,
    /// The worker's slowest read, sound decode and sound write (this window).
    slow: [u32; 3],
    /// The least a picture was ahead of the clock when queued while playing (this window; may be negative).
    ahead_min_us: Option<i64>,
}

pub struct Worker {
    q: Mutex<Shared>,
    wake: Semaphore,
    conv: Converter,
}

// ── the converter ────────────────────────────────────────────────────────────────────────────────────────────────

/// Where the converter puts a picture.
enum Target {
    /// Straight into the app's surface (the controls hidden, playing), turned: the UI's DAMAGE shows it later.
    Surface(neoapp::RawTarget, Turn),
    /// A Slint image of this size, for under the controls, into the buffer given when it is that size.
    Slint(u32, u32, Option<SharedPixelBuffer<Rgb8Pixel>>),
}

struct Job {
    /// The layout it was asked for, and the seek count when the picture was taken.
    layout: u32,
    epoch: u32,
    frame: Frame,
    target: Target,
    /// False: the last picture once more, for Slint (not counted as shown).
    count: bool,
}

/// A converted picture: the surface rectangle written (DAMAGE shows it) or the Slint image.
struct Done {
    layout: u32,
    epoch: u32,
    frame: Frame,
    surface: Option<Rect>,
    slint: Option<SharedPixelBuffer<Rgb8Pixel>>,
    count: bool,
    us: u32,
    /// No memory for the Slint image.
    failed: bool,
    /// With the helper: the converter's own share (0 without).
    own_us: u32,
}

#[derive(Default)]
struct Conv {
    job: Option<Job>,
    done: Option<Done>,
}

/// One picture at a time: the UI posts a job, the converter answers with `done`.
struct Converter {
    q: Mutex<Conv>,
    wake: Semaphore,
    done: Semaphore,
    helper: Helper,
}

/// Rows of a big picture for the helper: the screen rows `rows` of the rectangle, `px` starting at the first of them.
struct Part {
    frame: *const Frame,
    px: *mut u32,
    len: usize,
    stride: usize,
    w: u32,
    h: u32,
    turn: Turn,
    rows: core::ops::Range<u32>,
}

// SAFETY: the converter hands a Part over and waits for `done` before the frame or the surface rows can go away.
unsafe impl Send for Part {}

/// A second converter on a small core for the bottom rows of a full-screen picture: one A75 converts 1080x1920 in
/// 28 ms, over the 33 ms a picture has once the DAMAGE and the waits are in (Pixel, 2026-11-26).
struct Helper {
    job: Mutex<Option<Part>>,
    wake: Semaphore,
    done: Semaphore,
    on: core::sync::atomic::AtomicBool,
}

impl Helper {
    fn run(&self) -> ! {
        loop {
            self.wake.wait();
            let Some(p) = self.job.lock().take() else {
                continue;
            };
            // SAFETY: see `Part`; the rows are the converter's but for this one picture not written by it.
            let (f, px) = unsafe { (&*p.frame, core::slice::from_raw_parts_mut(p.px, p.len)) };
            yuv::draw_rows(f, p.turn, px, p.stride, p.w, p.h, p.rows);
            self.done.signal();
        }
    }
}

impl Converter {
    fn run(&self) -> ! {
        loop {
            self.wake.wait();
            let Some(j) = self.q.lock().job.take() else {
                continue;
            };
            let d = convert(j, &self.helper);
            self.q.lock().done = Some(d);
            self.done.signal();
        }
    }

    fn post(&self, j: Job) {
        self.q.lock().job = Some(j);
        self.wake.signal();
    }

    fn take(&self) -> Option<Done> {
        self.q.lock().done.take()
    }

    /// The picture in hand, waiting up to `us` for it (the semaphore may hold signals of pictures already taken).
    fn wait(&self, us: i32) -> Option<Done> {
        loop {
            if let Some(d) = self.take() {
                return Some(d);
            }
            if !self.done.wait_timeout(us) {
                return self.take();
            }
        }
    }
}

fn convert(j: Job, helper: &Helper) -> Done {
    use core::sync::atomic::Ordering;
    let t0 = time::fast_counter();
    let mut own_us = 0;
    let (surface, slint, failed) = match j.target {
        Target::Surface(mut t, turn) => {
            let (stride, r) = (t.stride, t.rect);
            let px = t.pixels();
            let k = r.h * (100 - HELPER_SHARE) / 100;
            if helper.on.load(Ordering::Relaxed) && r.w * r.h >= SPLIT_PIXELS && k > 0 {
                let (mine, theirs) = px.split_at_mut(k as usize * stride);
                *helper.job.lock() = Some(Part {
                    frame: &j.frame,
                    px: theirs.as_mut_ptr(),
                    len: theirs.len(),
                    stride,
                    w: r.w,
                    h: r.h,
                    turn,
                    rows: k..r.h,
                });
                helper.wake.signal();
                yuv::draw_rows(&j.frame, turn, mine, stride, r.w, r.h, 0..k);
                own_us = elapsed_us(t0);
                if !helper.done.wait_timeout(HELPER_WAIT_US) {
                    // shortcut: a helper this late is taken as dead; a slow one would still read this frame
                    helper.on.store(false, Ordering::Relaxed);
                    dprintln!("files: video helper stopped answering, the converter goes on alone");
                }
            } else {
                yuv::draw(&j.frame, turn, px, stride, r.w, r.h);
            }
            (Some(r), None, false)
        }
        Target::Slint(w, h, reuse) => {
            let b = slint_buffer(&j.frame, w, h, reuse);
            let failed = b.is_none();
            (None, b, failed)
        }
    };
    Done {
        layout: j.layout,
        epoch: j.epoch,
        frame: j.frame,
        surface,
        slint,
        count: j.count,
        us: elapsed_us(t0),
        own_us,
        failed,
    }
}

/// The picture for Slint at the size it shows (opaque RGB: Slint draws it without blending), straight into the image's
/// bytes: an XRGB copy as well ran a landscape picture out of heap on the Pixel (8.3 MB, 2026-11-26). The buffer is
/// reused: a new 2–6 MB one per picture fragmented the first-fit heap until one failed (Pixel, 2026-11-26). None: no
/// memory for it (a failed allocation would panic and end the converter thread).
fn slint_buffer(
    f: &Frame,
    w: u32,
    h: u32,
    reuse: Option<SharedPixelBuffer<Rgb8Pixel>>,
) -> Option<SharedPixelBuffer<Rgb8Pixel>> {
    let mut buf = match reuse {
        Some(b) if (b.width(), b.height()) == (w, h) => b,
        old => {
            drop(old); // freed first: the heap is nearly full in landscape
            // shortcut: the probe is freed before Slint allocates, so another thread could take the room in between;
            // a fallible SharedPixelBuffer is the upgrade if that ever shows.
            if Vec::<u8>::new()
                .try_reserve_exact(w as usize * h as usize * 3 + 64)
                .is_err()
            {
                return None;
            }
            SharedPixelBuffer::new(w, h)
        }
    };
    let (px, _) = buf.make_mut_bytes().as_chunks_mut::<3>();
    yuv::draw(f, Turn::None, px, w as usize, w, h);
    Some(buf)
}

/// A clip open in the worker.
struct Now<'a> {
    n: u32,
    demux: Box<dyn av::Demux + 'a>,
    vtrack: usize,
    vdec: Box<dyn video::VideoDecode>,
    strack: Option<usize>,
    sdec: Option<Box<dyn sound::SoundDecode>>,
    out: Option<Out>,
    /// Decoded sound not yet written, interleaved.
    pcm: VecDeque<i16>,
    rate: u32,
    channels: usize,
    /// Pictures and sound before this time (a seek's target) are decoded but not kept.
    skip_us: i64,
    /// The media time of NeoAudio's frame `f0` (the first after an open, seek or speed change).
    base_us: i64,
    f0: u64,
    /// Media time without sound: the clock started at `base_us` at this counter reading.
    started_at: u32,
    state: Play,
    speed: u32,
    muted: bool,
    input_done: bool,
    /// Paused after an open or a seek: one picture is still to be shown.
    still: bool,
    /// After an open or a seek the sound waits (and the clock stands) until a few pictures are decoded, so the
    /// first ones aren't late: on the Pixel the first 5 s ran 400 ms behind without it.
    preroll: bool,
    boosted_at: u32,
    /// NeoAudio's queue (bytes) and when it last changed: one that stops draining counts as played out.
    drain: (u32, u32),
    /// NeoAudio's played-frame count when the clock was last published.
    heard: u64,
    /// The last tick (NeoAudio asked, the clock published).
    ticked_at: u32,
    /// When pictures last came out of the decoder.
    kept_at: u32,
    /// The slowest read, sound decode and sound write since the UI last took them (µs).
    slow: [u32; 3],
    pictures: Vec<Frame>,
}

fn elapsed_us(from: u32) -> u32 {
    time::counter_us(from, time::fast_counter(), time::fast_counter_hz())
}

impl Worker {
    pub fn new() -> Result<&'static Worker, i32> {
        Ok(Box::leak(Box::new(Worker {
            q: Mutex::new(Shared::default())?,
            wake: Semaphore::new(0)?,
            conv: Converter {
                q: Mutex::new(Conv::default())?,
                wake: Semaphore::new(0)?,
                done: Semaphore::new(0)?,
                helper: Helper {
                    job: Mutex::new(None)?,
                    wake: Semaphore::new(0)?,
                    done: Semaphore::new(0)?,
                    on: core::sync::atomic::AtomicBool::new(false),
                },
            },
        })))
    }

    fn start(&'static self) -> bool {
        if core::mem::replace(&mut self.q.lock().started, true) {
            return true;
        }
        let helper = &self.conv.helper;
        let on = e32::thread::spawn_with_stack(CONVERT_STACK, move || {
            pin(HELPER_CPU);
            helper.run()
        })
        .is_ok();
        helper.on.store(on, core::sync::atomic::Ordering::Relaxed);
        if !self.spawn_converter() || !self.spawn_decoder() {
            // shortcut: a converter already started stays when the decoder fails to; a second one shares its jobs
            self.q.lock().started = false;
            return false;
        }
        true
    }

    fn spawn_decoder(&'static self) -> bool {
        e32::thread::spawn_with_stack(STACK, move || {
            // the decoder needs a fast core: left to the scheduler it ran on the A55s at 20 fps (Pixel, 2026-11-26)
            pin(FAST_CPU);
            let Ok(fs) = E32Fs::connect() else {
                self.q.lock().started = false;
                return -1;
            };
            self.run(&fs);
            0
        })
        .is_ok()
    }

    fn spawn_converter(&'static self) -> bool {
        let conv = &self.conv;
        e32::thread::spawn_with_stack(CONVERT_STACK, move || {
            pin(CONVERT_CPU);
            conv.run()
        })
        .is_ok()
    }

    fn send(&'static self, c: Cmd) {
        if matches!(c, Cmd::Open(..)) && !self.start() {
            let mut q = self.q.lock();
            q.clip = Some(Err("Files couldn't start its player.".into()));
            return;
        }
        self.q.lock().cmds.push_back(c);
        self.wake.signal();
    }

    fn run(&self, fs: &E32Fs) {
        let mut now: Option<Now<'_>> = None;
        loop {
            let cmds = core::mem::take(&mut self.q.lock().cmds);
            for c in cmds {
                match c {
                    Cmd::Open(n, path, at, play) => {
                        drop(now.take());
                        now = self.open(fs, n, &path, at, play);
                    }
                    Cmd::Close => {
                        now = None;
                        let mut q = self.q.lock();
                        q.frames.clear();
                        q.clock.running = false;
                    }
                    Cmd::Play => {
                        if let Some(p) = now
                            .as_mut()
                            .filter(|p| matches!(p.state, Play::Paused | Play::Ended))
                        {
                            if p.state == Play::Ended {
                                self.seek(p, 0);
                            }
                            p.state = Play::Playing;
                            p.still = false;
                            match p.out.as_ref() {
                                // the sound's clock goes on from the frames NeoAudio took
                                Some(o) => o.call(op::PAUSE, 0),
                                None => {
                                    p.started_at = time::fast_counter();
                                    p.base_us = self.q.lock().clock.us;
                                }
                            }
                        }
                    }
                    Cmd::Pause => {
                        if let Some(p) = now.as_mut().filter(|p| p.state == Play::Playing) {
                            self.pause(p);
                        }
                    }
                    Cmd::Seek(us) => {
                        if let Some(p) = now.as_mut() {
                            self.seek(p, us);
                        }
                    }
                    Cmd::Speed(pc) => {
                        if let Some(p) = now.as_mut() {
                            let at = self.q.lock().clock.us;
                            p.speed = pc;
                            self.restart_sound(p, at);
                        }
                    }
                    Cmd::Mute(m) => {
                        if let Some(p) = now.as_mut() {
                            p.muted = m;
                            if let Some(o) = p.out.as_ref() {
                                o.call(op::VOLUME, if m { 0 } else { 100 });
                            }
                        }
                    }
                }
            }
            let Some(p) = now.as_mut() else {
                self.wake.wait();
                continue;
            };
            let worked = self.step(p);
            if !worked {
                self.wake.wait_timeout(IDLE_US);
            }
        }
    }

    fn open<'a>(
        &self,
        fs: &'a E32Fs,
        n: u32,
        path: &str,
        at_us: u64,
        play: bool,
    ) -> Option<Now<'a>> {
        let answer = |r: Result<Clip, String>| {
            let mut q = self.q.lock();
            if q.n == n {
                q.clip = Some(r);
                q.frames.clear();
                q.state = Play::Opening;
            }
        };
        let f = match fs.open(path, Open::Read) {
            Ok(f) => f,
            Err(e) => {
                answer(Err(format!("It couldn't be read ({}).", e.code())));
                return None;
            }
        };
        let size = f.size().unwrap_or(0);
        let demux = match av::open(At(f, size)) {
            Ok(d) => d,
            Err(neo_format_media::Error::Unsupported) => {
                answer(Err("Its container isn't supported.".into()));
                return None;
            }
            Err(_) => {
                answer(Err("The file is damaged.".into()));
                return None;
            }
        };
        let tracks = demux.tracks().to_vec();
        let (v, s) = av::pick(&tracks);
        let Some(v) = v else {
            answer(Err("It has no picture.".into()));
            return None;
        };
        let t = &tracks[v];
        if !t.codec.played() {
            answer(Err(format!("{} isn't supported.", t.codec.name())));
            return None;
        }
        if t.width * t.height > HEAP_PIXELS && t.width * t.height <= MAX_PIXELS {
            answer(Err(format!(
                "Files can't play videos over 720p yet ({} × {}): they need more memory than it has for now.",
                t.width, t.height
            )));
            return None;
        }
        if t.width * t.height > MAX_PIXELS {
            answer(Err(format!(
                "{} × {} is too big to play.",
                t.width, t.height
            )));
            return None;
        }
        let vdec = match video::decoder(t) {
            Ok(d) => d,
            Err(_) => {
                answer(Err(format!("Its {} couldn't be read.", t.codec.name())));
                return None;
            }
        };
        let (mut silent, mut sdec, mut strack) = (None, None, None);
        if let Some(s) = s {
            match sound::decoder(&tracks[s]) {
                Ok(d) => {
                    sdec = Some(d);
                    strack = Some(s);
                }
                Err(_) => {
                    silent = Some(format!(
                        "No sound: {} isn't supported.",
                        tracks[s].codec.name()
                    ))
                }
            }
        }
        let (rate, channels) = sdec
            .as_ref()
            .map_or((0, 0), |d| (d.rate(), d.channels() as usize));
        let clip = Clip {
            size: (t.width, t.height),
            duration_us: demux.duration_us(),
            silent,
        };
        dprintln!(
            "files: video {} {} {}x{} {} ms, sound {:?} {} Hz x{}",
            path,
            t.codec.name(),
            t.width,
            t.height,
            clip.duration_us / 1000,
            strack.map(|s| tracks[s].codec.name()),
            rate,
            channels
        );
        answer(Ok(clip));
        let mut p = Now {
            n,
            demux,
            vtrack: v,
            vdec,
            strack,
            sdec,
            out: None,
            pcm: VecDeque::new(),
            rate,
            channels,
            skip_us: 0,
            base_us: 0,
            f0: 0,
            started_at: time::fast_counter(),
            state: if play { Play::Playing } else { Play::Paused },
            speed: 100,
            muted: false,
            input_done: false,
            still: true,
            preroll: true,
            boosted_at: 0,
            drain: (0, 0),
            heard: u64::MAX,
            ticked_at: 0,
            kept_at: 0,
            slow: [0; 3],
            pictures: Vec::new(),
        };
        self.seek(&mut p, at_us);
        Some(p)
    }

    fn pause(&self, p: &mut Now<'_>) {
        p.state = Play::Paused;
        if let Some(o) = p.out.as_ref() {
            o.call(op::PAUSE, 1);
        }
        let mut q = self.q.lock();
        q.clock.running = false;
        q.state = Play::Paused;
    }

    /// Back to the key frame before `us`; pictures and sound up to `us` are decoded and dropped.
    fn seek(&self, p: &mut Now<'_>, us: u64) {
        let at = p.demux.seek(us).unwrap_or(0);
        dprintln!("files: video seek {} ms -> key {} ms", us / 1000, at / 1000);
        p.vdec.reset();
        if let Some(d) = p.sdec.as_mut() {
            d.reset();
        }
        p.pcm.clear();
        p.skip_us = us as i64;
        p.input_done = false;
        p.still = true;
        p.preroll = true;
        if p.state == Play::Ended {
            p.state = Play::Paused;
        }
        {
            let mut q = self.q.lock();
            q.frames.clear();
            q.seeks = q.seeks.wrapping_add(1);
            q.clock = Clock {
                us: us as i64,
                at: time::fast_counter(),
                running: false,
                speed: p.speed,
            };
        }
        self.restart_sound(p, us as i64);
    }

    /// A new NeoAudio stream from media time `at_us` (after a seek or a speed change: the rate is the clip's × speed).
    fn restart_sound(&self, p: &mut Now<'_>, at_us: i64) {
        p.base_us = at_us;
        p.started_at = time::fast_counter();
        drop(p.out.take());
        if p.sdec.is_none() || p.rate == 0 {
            return;
        }
        let rate = p.rate * p.speed / 100;
        match Out::open(rate, p.channels as u8) {
            Ok(o) => {
                // paused until the pre-roll ends (or Play)
                if p.state != Play::Playing || p.preroll {
                    o.call(op::PAUSE, 1);
                }
                if p.muted {
                    o.call(op::VOLUME, 0);
                }
                p.f0 = o.info().frames;
                p.out = Some(o);
            }
            // outside NeoAudio's 8-96 kHz (8 kHz at half speed): the picture plays on the counter, silent
            Err(e) => dprintln!("files: video sound off at {}% ({})", p.speed, e),
        }
    }

    /// One turn of work: decode a packet, top NeoAudio up, keep the clock. False when there was nothing to do.
    fn step(&self, p: &mut Now<'_>) -> bool {
        let mut worked = false;
        // the picture shown after an open or a seek decodes while paused too
        let queued = self.q.lock().frames.len();
        let want_pictures = queued < AHEAD;
        let sound_ahead =
            (p.pcm.len() / p.channels.max(1)) as u64 * 1_000_000 / u64::from(p.rate.max(1));
        let want_sound = p.sdec.is_some() && sound_ahead < SOUND_AHEAD_US;
        let paused = p.state != Play::Playing && !p.still;
        if !p.input_done && (want_pictures || want_sound) && !paused && queued < AHEAD_MAX {
            worked = true;
            let t0 = time::fast_counter();
            let next = p.demux.next();
            p.slow[0] = p.slow[0].max(elapsed_us(t0));
            match next {
                Ok(Some(pk)) if pk.track == p.vtrack => {
                    let t0 = time::fast_counter();
                    p.vdec.decode(&pk, &mut p.pictures).ok();
                    let us = elapsed_us(t0);
                    {
                        let mut q = self.q.lock();
                        q.decoded += 1;
                        q.decode_us += u64::from(us);
                        q.decode_max_us = q.decode_max_us.max(us);
                    }
                    self.keep(p);
                }
                Ok(Some(pk)) if Some(pk.track) == p.strack => {
                    if let Some(d) = p.sdec.as_mut() {
                        let mut s = Vec::new();
                        let t0 = time::fast_counter();
                        d.decode(&pk, &mut s).ok();
                        p.slow[1] = p.slow[1].max(elapsed_us(t0));
                        // drop what comes before a seek's target
                        let ch = p.channels.max(1);
                        let mut from = 0;
                        if pk.pts_us < p.skip_us {
                            let skip = ((p.skip_us - pk.pts_us) as u64 * u64::from(p.rate)
                                / 1_000_000) as usize
                                * ch;
                            from = skip.min(s.len());
                        }
                        p.pcm.extend(&s[from..]);
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    p.input_done = true;
                    p.vdec.flush(&mut p.pictures);
                    self.keep(p);
                }
            }
        }
        if p.preroll && (p.input_done || self.q.lock().frames.len() >= AHEAD) {
            p.preroll = false;
            if p.state == Play::Playing {
                if let Some(o) = p.out.as_ref() {
                    o.call(op::PAUSE, 0);
                    p.f0 = o.info().frames;
                }
                p.started_at = time::fast_counter();
            }
        }
        if p.state == Play::Playing {
            let t0 = time::fast_counter();
            worked |= self.feed(p);
            p.slow[2] = p.slow[2].max(elapsed_us(t0));
            if elapsed_us(p.boosted_at) >= BOOST_EVERY_US {
                boost(BOOST_MS);
                p.boosted_at = time::fast_counter();
            }
        }
        self.tick(p);
        worked
    }

    /// The decoded pictures from a seek's target on go to the UI, in time order.
    fn keep(&self, p: &mut Now<'_>) {
        if p.pictures.is_empty() {
            return;
        }
        let mut q = self.q.lock();
        let (n, gap) = (p.pictures.len(), elapsed_us(p.kept_at));
        p.kept_at = time::fast_counter();
        for f in p.pictures.drain(..) {
            if f.pts_us >= p.skip_us - 20_000 {
                q.buffered_us = q.buffered_us.max(f.pts_us.max(0) as u64);
                if q.clock.running {
                    let ahead = f.pts_us - now_us(&q.clock);
                    q.ahead_min_us = Some(q.ahead_min_us.map_or(ahead, |a| a.min(ahead)));
                    if ahead < -50_000 {
                        dprintln!(
                            "files: video late picture {} ms by {} ms: {} out together, {} ms after the last, queue {}",
                            f.pts_us / 1000,
                            -ahead / 1000,
                            n,
                            gap / 1000,
                            q.frames.len()
                        );
                    }
                }
                q.frames.push_back(f);
                if p.state != Play::Playing {
                    p.still = false;
                }
            }
        }
    }

    /// Writes sound while NeoAudio's queue has room (never blocking on it); true if it wrote.
    fn feed(&self, p: &mut Now<'_>) -> bool {
        let Some(o) = p.out.as_ref() else {
            return false;
        };
        let i = o.info();
        if i.state == state::PREEMPTED {
            // a call took the speaker: the clip pauses and stays paused
            dprintln!("files: video paused for a call");
            self.pause(p);
            return false;
        }
        let ch = p.channels.max(1);
        let queued_us = u64::from(i.queued) / 2 / ch as u64 * 1_000_000 / u64::from(p.rate.max(1));
        if queued_us >= QUEUE_US || p.pcm.is_empty() {
            return false;
        }
        // about 50 ms, whole frames
        let n = ((p.rate as usize / 20).max(64) * ch).min(p.pcm.len()) / ch * ch;
        let mut bytes = Vec::with_capacity(n * 2);
        for s in p.pcm.drain(..n) {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        o.write(&bytes).is_ok()
    }

    /// Publishes the clock and notices the end.
    fn tick(&self, p: &mut Now<'_>) {
        let playing = p.state == Play::Playing && !p.preroll;
        // NeoAudio is a server on another core: three or four calls a turn, ~70 turns a second, kept the decoder from
        // its pictures (queued up to 120 ms late, Pixel 2026-11-26). One call, at most every TICK_US while playing.
        if playing && elapsed_us(p.ticked_at) < TICK_US {
            return;
        }
        p.ticked_at = time::fast_counter();
        let info = p.out.as_ref().map(|o| o.info());
        let mut q = self.q.lock();
        if q.n != p.n {
            return;
        }
        let clock = match &info {
            // NeoAudio's count moves in steps: republished between them, the clock stood still and then jumped, and
            // pictures came due in bursts (worst A/V ~100 ms, drops; Pixel 2026-11-26). The UI's interpolation runs on.
            Some(i) if playing && q.clock.running && i.frames == p.heard => None,
            Some(i) if playing => {
                p.heard = i.frames;
                let played = i.frames.saturating_sub(p.f0);
                let us =
                    p.base_us + (played * 1_000_000 / u64::from(p.rate.max(1))) as i64 - LATENCY_US;
                Some(Clock {
                    us: us.max(p.base_us),
                    at: time::fast_counter(),
                    running: true,
                    speed: 100,
                })
            }
            _ if playing => Some(Clock {
                us: p.base_us,
                at: p.started_at,
                running: true,
                speed: p.speed,
            }),
            _ => Some(Clock {
                running: false,
                speed: p.speed,
                ..q.clock
            }),
        };
        if let Some(c) = clock {
            q.clock = c;
        }
        q.beat = q.beat.wrapping_add(1);
        for (m, s) in q.slow.iter_mut().zip(core::mem::take(&mut p.slow)) {
            *m = (*m).max(s);
        }
        let queued = info.map_or(0, |i| i.queued);
        if queued != p.drain.0 {
            p.drain = (queued, time::fast_counter());
        }
        // a clip stayed "playing" at its end, black once the controls hid (Pixel, 2026-11-26): the sound may never
        // drain to 0, so a queue standing still this long counts as played out
        let stuck = queued > 0 && elapsed_us(p.drain.1) > DRAIN_STUCK_US;
        if stuck && playing && p.input_done && q.frames.is_empty() && p.state == Play::Playing {
            dprintln!("files: video end, {} sound bytes never played", queued);
        }
        let sound_left = (queued > 0 && !stuck) || !p.pcm.is_empty();
        if playing && p.input_done && q.frames.is_empty() && !sound_left {
            p.state = Play::Ended;
            q.clock.running = false;
        }
        q.state = p.state;
    }
}

/// The open clip on the UI side.
pub struct Video {
    n: u32,
    path: String,
    size: u64,
    modified: i64,
    v: Option<VideoPlayer>,
    clip: Option<Clip>,
    state: Play,
    /// The picture now shown and its time; whether it went to the surface directly.
    shown_us: i64,
    raw: bool,
    /// The worker's last clock.
    clock: Clock,
    buffered_us: u64,
    // per clip: pictures shown and dropped, worst A/V gap
    shown: u32,
    dropped: u32,
    since: u32,
    worst_us: i64,
    saved_us: u64,
    present_us: u64,
    /// The picture last shown: handed to Slint when the raw path stops (paused, ended, controls shown).
    last: Option<Frame>,
    /// The layout pictures are converted for (surface or Slint, the place, turned) and its count.
    key: Key,
    layout: u32,
    /// A picture at the converter: whether it goes into the surface.
    pending: Option<bool>,
    /// A converted picture waiting for its time.
    ready: Option<Done>,
    converted: u32,
    convert_us: u64,
    convert_own_us: u64,
    /// When the picture at the converter went there.
    posted_at: u32,
    /// The Slint image's buffer and a spare for the next (reused: see `slint_buffer`); the layout the image is for.
    showing: Option<SharedPixelBuffer<Rgb8Pixel>>,
    spare: Option<SharedPixelBuffer<Rgb8Pixel>>,
    slint_layout: u32,
    /// The converter failed: the error card shows.
    failed: bool,
    /// The last poll, and the longest gap between two while playing (this window).
    polled_at: u32,
    stall_us: u32,
    /// The biggest step of the clock against its own interpolation (this window).
    jump_us: u32,
    /// How late the latest picture handed to the converter already was, and the shortest queue then (this window).
    picked_late_us: i64,
    queue_min: usize,
    /// The worker's last turn count seen, and when it last changed.
    beat: u32,
    beat_at: u32,
}

/// Pictures straight into the surface; the picture's place in the view; landscape.
type Key = (bool, (u32, u32, u32, u32), bool);

pub(crate) fn is_video(name: &str) -> bool {
    matches!(
        neo_view::ext_of(name).to_ascii_lowercase().as_str(),
        "mp4" | "m4v" | "mov" | "3gp" | "3g2" | "mkv" | "webm"
    )
}

pub(crate) fn bind(ui: &FilesApp, q: &Rc<RefCell<VecDeque<crate::Action>>>) {
    let push = |a: fn(f32) -> Act| {
        let q = q.clone();
        move |f: f32| q.borrow_mut().push_back(crate::Action::Video(a(f)))
    };
    let tap = |a: Act| {
        let q = q.clone();
        move || q.borrow_mut().push_back(crate::Action::Video(a))
    };
    ui.on_video_tapped(tap(Act::Tap));
    ui.on_video_play_pause(tap(Act::PlayPause));
    ui.on_video_back10(tap(Act::Back10));
    ui.on_video_forward10(tap(Act::Forward10));
    ui.on_video_seek(push(Act::Seek));
    ui.on_video_mute(tap(Act::Mute));
    ui.on_video_speed_tapped(tap(Act::Speed));
    ui.on_video_rotate(tap(Act::Rotate));
    ui.on_video_full(tap(Act::Full));
    ui.on_video_open_with(tap(Act::OpenWith));
    let q2 = q.clone();
    ui.on_video_resized(move |w, h| {
        q2.borrow_mut()
            .push_back(crate::Action::Video(Act::Resized(w, h)))
    });
}

#[derive(Clone, Copy, Debug)]
pub enum Act {
    Tap,
    PlayPause,
    Back10,
    Forward10,
    Seek(f32),
    Mute,
    Speed,
    Rotate,
    Full,
    OpenWith,
    Resized(i32, i32),
}

pub(crate) fn open(st: &mut State, ui: &FilesApp, o: &Open7) -> Result<&'static str, String> {
    if st.videow.is_none() {
        st.videow =
            Some(Worker::new().map_err(|_| String::from("Files couldn't start its player."))?);
    }
    let w = st.videow.unwrap();
    let n = st.video.as_ref().map_or(1, |v| v.n + 1);
    let modified = st.fs.entry(&o.path).map(|e| e.modified).unwrap_or(0);
    let at = place(st, &o.path, o.size, modified).unwrap_or(0);
    {
        let mut q = w.q.lock();
        q.n = n;
        q.clip = None;
        q.frames.clear();
    }
    w.send(Cmd::Open(n, o.path.clone(), at, true));
    st.video = Some(Video {
        n,
        path: o.path.clone(),
        size: o.size,
        modified,
        v: None,
        clip: None,
        state: Play::Opening,
        shown_us: -1,
        raw: false,
        clock: Clock::default(),
        buffered_us: 0,
        shown: 0,
        dropped: 0,
        since: time::fast_counter(),
        worst_us: 0,
        saved_us: at,
        present_us: 0,
        last: None,
        key: Key::default(),
        layout: 0,
        pending: None,
        ready: None,
        converted: 0,
        convert_us: 0,
        convert_own_us: 0,
        posted_at: 0,
        showing: None,
        spare: None,
        slint_layout: u32::MAX,
        failed: false,
        polled_at: time::fast_counter(),
        stall_us: 0,
        jump_us: 0,
        picked_late_us: i64::MIN,
        queue_min: usize::MAX,
        beat: 0,
        beat_at: time::fast_counter(),
    });
    ui.set_video_error("".into());
    ui.set_video_error_detail("".into());
    ui.set_video_frame(Image::default());
    ui.set_video_controls(true);
    ui.set_video_info("Opening…".into());
    Ok("video")
}

/// The clock now, interpolated from the worker's last reading.
fn now_us(c: &Clock) -> i64 {
    if !c.running {
        return c.us;
    }
    c.us + i64::from(elapsed_us(c.at)) * i64::from(c.speed) / 100
}

/// The next wake the UI loop needs: the converter's answer, the next picture's time, or the poll while a clip is open.
pub(crate) fn wake_us(st: &State) -> Option<i32> {
    let v = st.video.as_ref()?;
    let w = st.videow?;
    if v.pending.is_some() {
        return Some(2000);
    }
    if v.state != Play::Playing && v.ready.is_none() {
        return Some(POLL_US);
    }
    let q = w.q.lock();
    let next = match &v.ready {
        Some(d) => d.frame.pts_us - now_us(&q.clock),
        None if q.frames.is_empty() => IDLE_US as i64,
        None => 0,
    };
    Some(next.clamp(1000, POLL_US as i64) as i32)
}

/// The next picture to convert; those already passed by a newer one that is due are dropped (counted).
fn pick(frames: &mut VecDeque<Frame>, now: i64, shown: bool) -> (Option<Frame>, u32) {
    let mut late = 0;
    while let Some(f) = frames.pop_front() {
        if shown && frames.front().is_some_and(|n| n.pts_us <= now) {
            late += 1;
            continue;
        }
        return (Some(f), late);
    }
    (None, late)
}

/// Where the next picture goes: the surface rectangle and turn, or None for Slint at this size.
#[derive(Clone, Copy)]
struct Spec {
    surface: Option<(Rect, Turn)>,
    w: u32,
    h: u32,
}

fn post(app: &App, w: &Worker, v: &mut Video, spec: Spec, frame: Frame, count: bool, epoch: u32) {
    let target = match spec
        .surface
        .and_then(|(r, turn)| app.raw_target(r).map(|t| Target::Surface(t, turn)))
    {
        Some(t) => t,
        None => Target::Slint(spec.w, spec.h, v.spare.take()),
    };
    v.pending = Some(matches!(target, Target::Surface(..)));
    v.posted_at = time::fast_counter();
    w.conv.post(Job {
        layout: v.layout,
        epoch,
        frame,
        target,
        count,
    });
}

/// Takes the worker's news, shows the converted picture that is due and hands the converter the next.
pub(crate) fn poll(app: &App, ui: &FilesApp, st: &mut State) {
    let (Some(v), Some(w)) = (st.video.as_mut(), st.videow) else {
        return;
    };
    let (clip, epoch) = {
        let mut q = w.q.lock();
        if q.n != v.n {
            return;
        }
        v.state = q.state;
        if v.state == Play::Playing {
            v.stall_us = v.stall_us.max(elapsed_us(v.polled_at));
        }
        v.polled_at = time::fast_counter();
        if q.beat != v.beat {
            v.beat = q.beat;
            v.beat_at = v.polled_at;
        }
        if q.clock.running && v.clock.running && q.clock.at != v.clock.at {
            // where the old clock said the new reading's moment was: a big difference is a jump
            let said = v.clock.us
                + i64::from(time::counter_us(
                    v.clock.at,
                    q.clock.at,
                    time::fast_counter_hz(),
                )) * i64::from(v.clock.speed)
                    / 100;
            v.jump_us = v.jump_us.max((q.clock.us - said).unsigned_abs() as u32);
        }
        v.clock = q.clock;
        v.buffered_us = q.buffered_us;
        (q.clip.take(), q.seeks)
    };
    if let Some(c) = clip {
        match c {
            Ok(c) => {
                let mut p = VideoPlayer::new(c.size, c.duration_us);
                p.layout(
                    ui.get_video_view_w().max(1) as u32,
                    ui.get_video_view_h().max(1) as u32,
                );
                p.touched(ms());
                let folder = neo_fs::path::parent(&v.path).unwrap_or_default();
                let mut line = screen::info(v.modified, c.size, v.size, &folder);
                if let Some(s) = &c.silent {
                    line = format!("{line} · {s}");
                }
                ui.set_video_info(line.into());
                v.v = Some(p);
                v.clip = Some(c);
            }
            Err(why) => {
                ui.set_video_error("Can't play this video".into());
                ui.set_video_error_detail(why.into());
                return;
            }
        }
    }
    if v.failed {
        return;
    }
    if v.state == Play::Playing && elapsed_us(v.beat_at) > WORKER_DEAD_US {
        // shortcut: what the dead thread held stays allocated (no unwinding); the next clip gets a new worker
        dprintln!("files: video worker stopped answering, started again");
        w.spawn_decoder();
        return fail(ui, w, v, "The player stopped (out of memory?).");
    }
    let Some(p) = v.v.as_mut() else { return };
    let playing = v.state == Play::Playing;
    if p.tick(ms(), playing) {
        ui.set_video_frame(Image::default()); // the Slint frame goes: the surface shows the pictures from now
        v.spare = v.showing.take().or(v.spare.take()); // one buffer kept for when the controls come back
    }
    let key: Key = (!p.controls && playing, p.place(), p.landscape);
    let fresh = key != v.key;
    if fresh {
        v.key = key;
        v.layout = v.layout.wrapping_add(1);
    }
    let (x, y, pw, ph) = key.1;
    let spec = Spec {
        surface: key.0.then(|| {
            let r = Rect::new(
                ui.get_video_view_x() + x as i32,
                ui.get_video_view_y() + y as i32,
                pw,
                ph,
            );
            let turn = if key.2 { Turn::Quarter } else { Turn::None };
            (app.to_surface(r), turn)
        }),
        // turned, Slint draws a half-size picture scaled up: 1.5 MB, not 6.2 MB (out of heap on the Pixel, 2026-11-26)
        w: if key.2 { (pw / 2).max(1) } else { pw },
        h: if key.2 { (ph / 2).max(1) } else { ph },
    };
    // the converter's answer; one from before a seek goes
    if v.pending.is_some()
        && let Some(d) = w.conv.take()
    {
        v.pending = None;
        v.converted += 1;
        v.convert_us += u64::from(d.us);
        v.convert_own_us += u64::from(d.own_us);
        if d.failed {
            dprintln!("files: video no memory for a {}x{} picture", spec.w, spec.h);
            return fail(ui, w, v, "Files ran out of memory for this video.");
        }
        v.ready = Some(d);
    }
    if v.pending.is_some() && elapsed_us(v.posted_at) > CONVERT_DEAD_US {
        dprintln!("files: video converter stopped answering, started again");
        v.pending = None;
        w.spawn_converter();
        return fail(ui, w, v, "The player stopped.");
    }
    if let Some(mut d) = v.ready.take_if(|d| d.epoch != epoch) {
        v.spare = v.spare.take().or(d.slint.take());
        if !d.count {
            v.last = Some(d.frame);
        }
    }
    // the picture that is due, converted for this layout
    let now = now_us(&v.clock);
    // one a stall left behind goes when a newer one is due already, as the old present step did: shown late, it put
    // the UI's stalls into the A/V gap (100-130 ms every window, Pixel 2026-11-26). Its pixels in the surface stay
    // hidden until a DAMAGE.
    if v.shown_us >= 0
        && v.ready.as_ref().is_some_and(|d| {
            d.count && d.layout == v.layout && d.epoch == epoch && d.frame.pts_us <= now
        })
        && w.q.lock().frames.front().is_some_and(|n| n.pts_us <= now)
        && let Some(mut d) = v.ready.take()
    {
        v.spare = v.spare.take().or(d.slint.take());
        v.dropped += 1;
    }
    let due =
        |d: &Done| d.layout == v.layout && (!d.count || v.shown_us < 0 || d.frame.pts_us <= now);
    if let Some(d) = v.ready.take_if(|d| due(d)) {
        let t0 = time::fast_counter();
        if let Some(r) = d.surface {
            let _ = app.damage(r);
        }
        if let Some(b) = d.slint {
            ui.set_video_frame(Image::from_rgb8(b.clone()));
            v.spare = v.showing.replace(b); // the image it replaced is gone: its buffer takes the next picture
            v.slint_layout = d.layout;
        }
        v.present_us += u64::from(elapsed_us(t0));
        if d.count {
            v.worst_us = v.worst_us.max((now - d.frame.pts_us).abs());
            v.shown_us = d.frame.pts_us;
            v.shown += 1;
        }
        v.raw = d.surface.is_some();
        v.last = Some(d.frame);
    }
    // the next picture to the converter: one converted for another layout again, else the next one queued. Not on the
    // turn the surface takes over: Slint clears the area in this turn's redraw first.
    if v.pending.is_none() && !(fresh && key.0) {
        if let Some(mut d) = v.ready.take_if(|d| d.layout != v.layout) {
            v.spare = v.spare.take().or(d.slint.take());
            post(app, w, v, spec, d.frame, d.count, d.epoch);
        } else if v.ready.is_none() {
            let (f, late) = {
                let mut q = w.q.lock();
                if q.seeks == epoch {
                    if playing {
                        v.queue_min = v.queue_min.min(q.frames.len());
                    }
                    pick(&mut q.frames, now, v.shown_us >= 0)
                } else {
                    (None, 0)
                }
            };
            if playing && let Some(f) = &f {
                v.picked_late_us = v.picked_late_us.max(now - f.pts_us);
            }
            v.dropped += late;
            if let Some(f) = f {
                w.wake.signal(); // room to decode ahead again
                post(app, w, v, spec, f, true, epoch);
            } else if !key.0
                && v.slint_layout != v.layout
                && let Some(f) = v.last.take()
            {
                // no new picture due and Slint's is missing or for another layout (the surface stopped, or turned after
                // the end): Slint shows the last
                post(app, w, v, spec, f, false, epoch);
            }
        }
    }
    let Some(p) = v.v.as_ref() else { return };
    screen::show_video(ui, p, v.shown_us.max(0) as u64, v.buffered_us, playing);
    // a line every 5 s while playing: what the gate measures
    if elapsed_us(v.since) >= 5_000_000 {
        let secs = elapsed_us(v.since) as u64;
        let (decoded, decode_us, ahead, decode_max, slow) = {
            let mut q = w.q.lock();
            (
                core::mem::take(&mut q.decoded),
                core::mem::take(&mut q.decode_us),
                q.ahead_min_us.take(),
                core::mem::take(&mut q.decode_max_us),
                core::mem::take(&mut q.slow),
            )
        };
        if v.shown > 0 {
            dprintln!(
                "files: video {}.{} fps shown, {} dropped, worst A/V {} ms{}; decode {} at {} ms (max {}), convert {} ms (own {}), present {} ms, stall {} ms, clock jump {} ms",
                u64::from(v.shown) * 1_000_000 / secs,
                u64::from(v.shown) * 10_000_000 / secs % 10,
                v.dropped,
                v.worst_us / 1000,
                if v.raw { ", raw" } else { "" },
                decoded,
                decode_us / 1000 / u64::from(decoded.max(1)),
                decode_max / 1000,
                v.convert_us / 1000 / u64::from(v.converted.max(1)),
                v.convert_own_us / 1000 / u64::from(v.converted.max(1)),
                v.present_us / 1000 / u64::from(v.shown),
                v.stall_us / 1000,
                v.jump_us / 1000
            );
            // a second line: one over 256 bytes is cut off by the debug output
            dprintln!(
                "files: video queued ahead min {} ms, picked late max {} ms, queue min {}; slowest read {} ms, sound decode {} ms, sound write {} ms",
                ahead.unwrap_or(0) / 1000,
                v.picked_late_us.max(-999_000) / 1000,
                if v.queue_min == usize::MAX {
                    0
                } else {
                    v.queue_min
                },
                slow[0] / 1000,
                slow[1] / 1000,
                slow[2] / 1000
            );
        }
        v.present_us = 0;
        v.convert_us = 0;
        v.convert_own_us = 0;
        v.converted = 0;
        v.shown = 0;
        v.dropped = 0;
        v.worst_us = 0;
        v.stall_us = 0;
        v.jump_us = 0;
        v.picked_late_us = i64::MIN;
        v.queue_min = usize::MAX;
        v.since = time::fast_counter();
        if playing && v.shown_us > 0 && (v.shown_us as u64).abs_diff(v.saved_us) > 5_000_000 {
            save_place(st);
        }
    }
}

/// `time::delay` at the nanokernel's 1 ms instead of the tick queue's 1/64 s: `RTimer::After` rounds every wait up to
/// 15.6 ms on this kernel (stimer.cpp), and two such waits per picture put the player 100 ms behind the sound (Pixel,
/// 2026-11-26). shortcut: Files' own copy of e32's `Delay`; move it into e32 (`time::delay_high_res`) with a pin bump.
pub(crate) fn delay(micros: i32) -> Delay {
    Delay {
        micros,
        timer: 0,
        status: e32::request::TRequestStatus::pending(),
    }
}

pub(crate) struct Delay {
    micros: i32,
    /// The kernel timer's handle once started (0 before).
    timer: i32,
    status: e32::request::TRequestStatus,
}

impl core::future::Future for Delay {
    type Output = ();

    fn poll(
        self: core::pin::Pin<&mut Self>,
        _: &mut core::task::Context<'_>,
    ) -> core::task::Poll<()> {
        // SAFETY: nothing is moved out; `status` stays where it is while the request runs (the future is pinned).
        let this = unsafe { self.get_unchecked_mut() };
        if this.timer == 0 {
            let h = e32::sys::timer_create();
            if h < 0 {
                e32::process::panic("FILES-TIMER", h);
            }
            this.timer = h;
            // SAFETY: `status` is inside the pinned future; Drop cancels and waits before it goes away.
            unsafe { e32::sys::timer_high_res(h, &mut this.status, this.micros.max(0)) };
            return core::task::Poll::Pending;
        }
        if this.status.is_pending() {
            core::task::Poll::Pending
        } else {
            core::task::Poll::Ready(())
        }
    }
}

impl Drop for Delay {
    fn drop(&mut self) {
        if self.timer != 0 {
            if self.status.is_pending() {
                e32::sys::timer_cancel(self.timer);
                e32::request::wait_for_request(&self.status);
            }
            e32::sys::handle_close(self.timer);
        }
    }
}

/// The converter can't go on: the clip pauses and the error card shows.
fn fail(ui: &FilesApp, w: &'static Worker, v: &mut Video, why: &str) {
    v.failed = true;
    v.ready = None;
    v.showing = None;
    v.spare = None;
    w.send(Cmd::Pause);
    ui.set_video_frame(Image::default());
    ui.set_video_error("Can't play this video".into());
    ui.set_video_error_detail(why.into());
}

fn ms() -> u64 {
    time::millis()
}

/// Before Slint draws: a picture the converter is writing into the surface is finished first when Slint may draw over
/// it now (the controls came up, the layout changed, the clip paused); the next poll converts it again for the new one.
pub(crate) fn settle(st: &mut State) {
    let (Some(v), Some(w)) = (st.video.as_mut(), st.videow) else {
        return;
    };
    if v.pending != Some(true) {
        return;
    }
    let playing = v.state == Play::Playing;
    if v.v
        .as_ref()
        .is_some_and(|p| (!p.controls && playing, p.place(), p.landscape) == v.key)
    {
        return;
    }
    v.layout = v.layout.wrapping_add(1);
    match w.conv.wait(CONVERT_WAIT_US) {
        Some(d) => {
            v.pending = None;
            v.ready = Some(d);
        }
        None => dprintln!("files: video converter late, Slint draws over it"),
    }
}

pub(crate) fn act(app: &App, ui: &FilesApp, st: &mut State, a: Act) {
    let Some(w) = st.videow else { return };
    let Some(v) = st.video.as_mut() else { return };
    let now = ms();
    let pos = now_us(&v.clock).max(0) as u64;
    let duration = v.clip.as_ref().map_or(0, |c| c.duration_us);
    if let Some(p) = v.v.as_mut()
        && !matches!(a, Act::Tap | Act::Resized(..))
    {
        p.touched(now);
    }
    match a {
        Act::Tap => {
            if let Some(p) = v.v.as_mut() {
                p.tap(now);
            }
        }
        Act::PlayPause => match v.state {
            Play::Playing => w.send(Cmd::Pause),
            _ => w.send(Cmd::Play),
        },
        Act::Back10 => w.send(Cmd::Seek(pos.saturating_sub(10_000_000))),
        Act::Forward10 => w.send(Cmd::Seek((pos + 10_000_000).min(duration))),
        Act::Seek(f) => w.send(Cmd::Seek((f64::from(f) * duration as f64) as u64)),
        Act::Mute => {
            if let Some(p) = v.v.as_mut() {
                p.muted = !p.muted;
                w.send(Cmd::Mute(p.muted));
            }
        }
        Act::Speed => {
            if let Some(p) = v.v.as_mut() {
                p.next_speed();
                w.send(Cmd::Speed(p.percent()));
            }
        }
        Act::Rotate => {
            if let Some(p) = v.v.as_mut() {
                p.landscape = !p.landscape;
                app.set_landscape(p.landscape);
                dprintln!("files: video landscape {}", p.landscape);
            }
        }
        Act::Full => {
            if let Some(p) = v.v.as_mut() {
                p.full = !p.full;
                dprintln!("files: video full screen {}", p.full);
            }
        }
        Act::OpenWith => {
            let path = v.path.clone();
            crate::fa8::open_with(ui, st, path);
        }
        Act::Resized(wd, h) => {
            dprintln!("files: video area {}x{}", wd, h);
            if let Some(p) = v.v.as_mut() {
                p.layout(wd.max(1) as u32, h.max(1) as u32);
            }
            // a new size: the Slint frame is the wrong size until the next picture
            v.shown_us = v.shown_us.min(0);
        }
    }
    if let Some(p) = st.video.as_ref().and_then(|v| v.v.as_ref()) {
        let (pos, buffered, playing) = (
            pos,
            st.video.as_ref().map_or(0, |v| v.buffered_us),
            playing(st),
        );
        screen::show_video(ui, p, pos, buffered, playing);
    }
}

pub(crate) fn playing(st: &State) -> bool {
    st.video.as_ref().is_some_and(|v| v.state == Play::Playing)
}

/// The page while a clip shows: full screen when asked or turned (the shell's bars would cover the picture).
pub(crate) fn full(st: &State) -> bool {
    st.video
        .as_ref()
        .and_then(|v| v.v.as_ref())
        .is_some_and(|p| p.full || p.landscape)
}

/// Files went to the background or came back: a playing clip pauses (and stays paused).
pub(crate) fn visible(st: &mut State, shown: bool) {
    if !shown
        && let (Some(v), Some(w)) = (st.video.as_ref(), st.videow)
        && v.state == Play::Playing
    {
        w.send(Cmd::Pause);
    }
}

/// Back or another file: the clip stops, its place kept; the picture and landscape go.
pub(crate) fn close(app: &App, ui: &FilesApp, st: &mut State) {
    if st.video.is_none() {
        return;
    }
    save_place(st);
    if let Some(w) = st.videow {
        w.send(Cmd::Close);
        if st.video.as_ref().is_some_and(|v| v.pending.is_some()) {
            w.conv.wait(CONVERT_WAIT_US);
        }
    }
    if st
        .video
        .as_ref()
        .and_then(|v| v.v.as_ref())
        .is_some_and(|p| p.landscape)
    {
        app.set_landscape(false);
    }
    st.video = None;
    ui.set_video_frame(Image::default());
}

// ── the place in each clip ───────────────────────────────────────────────────────────────────────────────────────

fn places(st: &State) -> Vec<String> {
    store::read(&*st.fs, PLACES)
        .ok()
        .map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Where the clip was left (µs); none near its end.
fn place(st: &State, path: &str, size: u64, modified: i64) -> Option<u64> {
    let key = format!("{path}\t{size}\t{modified}\t");
    let line = places(st).into_iter().find(|l| l.starts_with(&key))?;
    line[key.len()..].split('\t').next()?.parse().ok()
}

fn save_place(st: &mut State) {
    let Some(v) = st.video.as_mut() else { return };
    let duration = v.clip.as_ref().map_or(0, |c| c.duration_us);
    let at = v.shown_us.max(0) as u64;
    // within 5 s of the end: next time from the start
    let at = if duration > 0 && at + 5_000_000 >= duration {
        0
    } else {
        at
    };
    v.saved_us = at;
    let key = format!("{}\t{}\t{}\t", v.path, v.size, v.modified);
    let line = format!("{key}{at}");
    let mut lines = places(st);
    lines.retain(|l| !l.starts_with(&key));
    lines.insert(0, line);
    lines.truncate(PLACES_MAX);
    let mut out = lines.join("\n");
    out.push('\n');
    if let Err(e) = store::write(&*st.fs, PLACES, out.as_bytes()) {
        dprintln!("files: videos.dat {}", e.code());
    }
}
