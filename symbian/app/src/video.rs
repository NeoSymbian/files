// SPDX-License-Identifier: GPL-3.0-only
//! FA12, the video player (files2-view-video): MP4, MOV, 3GP and Matroska/WebM with H.264, VP8 or VP9 pictures and
//! AAC, Vorbis or Opus sound (neo-format-media's `av`).
//!
//! One worker thread reads the file a packet at a time, decodes the pictures a few ahead and the sound a little ahead,
//! and feeds NeoAudio without blocking (a WRITE only while its queue has room). The sound is the clock: what NeoAudio
//! has taken, less what still sits in the ADSP (about 160 ms), interpolated between reads; a clip without sound runs on
//! the fast counter. The UI thread shows each picture when the clock reaches it and drops the late ones: with the
//! controls hidden it writes the picture straight into the app's surface (neoapp `App::raw`), else it hands it to Slint
//! under the controls.
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
use alloc::vec;
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
const PLACES: &str = "C:\\private\\e0006001\\videos.dat";
const PLACES_MAX: usize = 200;

/// The ENeoHalCpuBoost HAL call (variant group, 0x4e29): top clocks for `ms`. Files' e32 pin predates its wrapper.
fn boost(ms: usize) {
    // SAFETY: the kernel takes the milliseconds as a plain value in a1; on a kernel without it the call fails harmlessly.
    unsafe {
        e32::sys::ui::hal_function(1, 0x4e29, ms as *mut u8, core::ptr::null_mut());
    }
}

/// The Pixel's second A75 (CPUs 6-7 are the fast cluster).
const FAST_CPU: usize = 7;

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
}

pub struct Worker {
    q: Mutex<Shared>,
    wake: Semaphore,
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
        })))
    }

    fn start(&'static self) -> bool {
        if core::mem::replace(&mut self.q.lock().started, true) {
            return true;
        }
        let r = e32::thread::spawn_with_stack(STACK, move || {
            // the decoder needs a fast core: left to the scheduler it ran on the A55s at 20 fps (Pixel, 2026-11-26)
            pin(FAST_CPU);
            let Ok(fs) = E32Fs::connect() else {
                self.q.lock().started = false;
                return -1;
            };
            self.run(&fs);
            0
        });
        if r.is_err() {
            self.q.lock().started = false;
            return false;
        }
        true
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
        let want_pictures = self.q.lock().frames.len() < AHEAD;
        let sound_ahead =
            (p.pcm.len() / p.channels.max(1)) as u64 * 1_000_000 / u64::from(p.rate.max(1));
        let want_sound = p.sdec.is_some() && sound_ahead < SOUND_AHEAD_US;
        let paused = p.state != Play::Playing && !p.still;
        if !p.input_done && (want_pictures || want_sound) && !paused {
            worked = true;
            match p.demux.next() {
                Ok(Some(pk)) if pk.track == p.vtrack => {
                    let t0 = time::fast_counter();
                    p.vdec.decode(&pk, &mut p.pictures).ok();
                    let us = elapsed_us(t0);
                    {
                        let mut q = self.q.lock();
                        q.decoded += 1;
                        q.decode_us += u64::from(us);
                    }
                    self.keep(p);
                }
                Ok(Some(pk)) if Some(pk.track) == p.strack => {
                    if let Some(d) = p.sdec.as_mut() {
                        let mut s = Vec::new();
                        d.decode(&pk, &mut s).ok();
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
            worked |= self.feed(p);
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
        for f in p.pictures.drain(..) {
            if f.pts_us >= p.skip_us - 20_000 {
                q.buffered_us = q.buffered_us.max(f.pts_us.max(0) as u64);
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
        let clock = match p.out.as_ref() {
            Some(o) if playing => {
                let i = o.info();
                let played = i.frames.saturating_sub(p.f0);
                let us =
                    p.base_us + (played * 1_000_000 / u64::from(p.rate.max(1))) as i64 - LATENCY_US;
                Clock {
                    us: us.max(p.base_us),
                    at: time::fast_counter(),
                    running: true,
                    speed: 100,
                }
            }
            _ if playing => Clock {
                us: p.base_us,
                at: p.started_at,
                running: true,
                speed: p.speed,
            },
            _ => Clock {
                running: false,
                speed: p.speed,
                ..self.q.lock().clock
            },
        };
        let mut q = self.q.lock();
        if q.n != p.n {
            return;
        }
        q.clock = clock;
        let sound_left = p.out.as_ref().is_some_and(|o| o.info().queued > 0) || !p.pcm.is_empty();
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
}

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

/// The next wake the UI loop needs: the next picture's time (or the poll while a clip is open).
pub(crate) fn wake_us(st: &State) -> Option<i32> {
    let v = st.video.as_ref()?;
    let w = st.videow?;
    if v.state != Play::Playing {
        return Some(POLL_US);
    }
    let q = w.q.lock();
    let next = q
        .frames
        .front()
        .map_or(IDLE_US as i64, |f| f.pts_us - now_us(&q.clock));
    Some(next.clamp(1000, POLL_US as i64) as i32)
}

/// Takes the worker's news and shows the picture that is due.
pub(crate) fn poll(app: &App, ui: &FilesApp, st: &mut State) {
    let (Some(v), Some(w)) = (st.video.as_mut(), st.videow) else {
        return;
    };
    let (due, late, clip) = {
        let mut q = w.q.lock();
        if q.n != v.n {
            return;
        }
        v.state = q.state;
        v.clock = q.clock;
        v.buffered_us = q.buffered_us;
        let now = now_us(&q.clock);
        let mut late = 0;
        // the newest picture whose time has come; the ones before it are dropped (shown nowhere)
        let mut due = None;
        while q
            .frames
            .front()
            .is_some_and(|f| f.pts_us <= now || v.shown_us < 0)
        {
            let f = q.frames.pop_front().unwrap();
            if due.is_some() {
                late += 1;
            }
            let first = v.shown_us < 0;
            due = Some(f);
            if first || q.frames.front().is_none_or(|n| n.pts_us > now) {
                break;
            }
        }
        if due.is_some() {
            w.wake.signal(); // room to decode ahead again
        }
        (due, late, q.clip.take())
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
    let Some(p) = v.v.as_mut() else { return };
    let playing = v.state == Play::Playing;
    if p.tick(ms(), playing) {
        ui.set_video_frame(Image::default()); // the Slint frame goes: the surface shows the pictures from now
    }
    let raw = !p.controls && playing;
    v.dropped += late;
    if let Some(f) = due {
        let gap = now_us(&v.clock) - f.pts_us;
        v.worst_us = v.worst_us.max(gap.abs());
        v.shown_us = f.pts_us;
        v.shown += 1;
        let t0 = time::fast_counter();
        let (x, y, w, h) = p.place();
        if raw {
            let r = Rect::new(
                ui.get_video_view_x() + x as i32,
                ui.get_video_view_y() + y as i32,
                w,
                h,
            );
            let turn = if p.landscape {
                Turn::Quarter
            } else {
                Turn::None
            };
            let s = app.to_surface(r);
            let _ = app.raw(s, |px, stride| yuv::draw(&f, turn, px, stride, s.w, s.h));
        } else {
            ui.set_video_frame(slint_frame(&f, w, h));
        }
        v.present_us += u64::from(elapsed_us(t0));
        v.raw = raw;
        v.last = Some(f);
    } else if v.raw && !raw {
        // the raw path stopped with no new picture due (paused, ended, the controls shown): Slint shows the last one
        if let Some(f) = v.last.as_ref() {
            let (_, _, w, h) = p.place();
            ui.set_video_frame(slint_frame(f, w, h));
        }
        v.raw = false;
    }
    screen::show_video(ui, p, v.shown_us.max(0) as u64, v.buffered_us, playing);
    // a line every 5 s while playing: what the gate measures
    if elapsed_us(v.since) >= 5_000_000 {
        let secs = elapsed_us(v.since) as u64;
        let (decoded, decode_us) = {
            let mut q = w.q.lock();
            (
                core::mem::take(&mut q.decoded),
                core::mem::take(&mut q.decode_us),
            )
        };
        if v.shown > 0 {
            dprintln!(
                "files: video {}.{} fps shown, {} dropped, worst A/V {} ms{}; decode {} at {} ms, present {} ms",
                u64::from(v.shown) * 1_000_000 / secs,
                u64::from(v.shown) * 10_000_000 / secs % 10,
                v.dropped,
                v.worst_us / 1000,
                if v.raw { ", raw" } else { "" },
                decoded,
                decode_us / 1000 / u64::from(decoded.max(1)),
                v.present_us / 1000 / u64::from(v.shown)
            );
        }
        v.present_us = 0;
        v.shown = 0;
        v.dropped = 0;
        v.worst_us = 0;
        v.since = time::fast_counter();
        if playing && v.shown_us > 0 && (v.shown_us as u64).abs_diff(v.saved_us) > 5_000_000 {
            save_place(st);
        }
    }
}

fn ms() -> u64 {
    time::millis()
}

/// The picture converted for Slint at the size it shows (opaque RGB: Slint draws it without blending).
fn slint_frame(f: &Frame, w: u32, h: u32) -> Image {
    let mut px = vec![0u32; (w * h) as usize];
    yuv::draw(f, Turn::None, &mut px, w as usize, w, h);
    let mut buf = SharedPixelBuffer::<Rgb8Pixel>::new(w, h);
    for (d, s) in buf.make_mut_slice().iter_mut().zip(&px) {
        *d = Rgb8Pixel {
            r: (s >> 16) as u8,
            g: (s >> 8) as u8,
            b: *s as u8,
        };
    }
    Image::from_rgb8(buf)
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
            }
        }
        Act::Full => {
            if let Some(p) = v.v.as_mut() {
                p.full = !p.full;
            }
        }
        Act::OpenWith => {
            let path = v.path.clone();
            crate::fa8::open_with(ui, st, path);
        }
        Act::Resized(wd, h) => {
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
