// SPDX-License-Identifier: GPL-3.0-only
//! FA9, the audio player (files2-view-audio): a track of the open folder plays through NeoAudio (AUDIOD) from a
//! worker thread; the UI polls where it is. The folder's other tracks follow (shuffle, repeat the folder or the
//! track). Playback pauses when Files goes to the background and stops when the player closes.
//!
//! NeoAudio sends no events: the worker reads its INFO after every WRITE. A call or the ringtone takes the speaker
//! (state PREEMPTED, the stream goes on unheard), so the worker pauses the stream and resumes it where it was when
//! the call ends. The end of a track is the queue running empty once the decoder has nothing more.

use crate::fa7::Open7;
use crate::{FilesApp, State, View};
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use e32::ipc::{Session, TIpcArgs, TVersion};
use e32::sync::{Mutex, Semaphore};
use e32::sys::{TPtr8, TPtrC8};
use e32::{dprintln, time};
use files_ui::audio as words;
use fs_e32::E32Fs;
use neo_format_media::{Error, ReadAt, Track};
use neo_fs::{File as _, Fs as _, Open};
use neoapp::App;
use neoaudio::{Info, op, state};

/// The decoders keep their tables on it (as the picture worker's).
const STACK: i32 = 256 * 1024;
/// The most PCM in one WRITE (bytes); a WRITE carries about [`WRITE_MS`] of the track, so a tap reaches the worker
/// within about that.
const CHUNK: usize = 8192;
const WRITE_MS: usize = 50;
/// How often a held stream asks whether the call is over.
const CALL_POLL_US: i32 = 100_000;
/// How often the end of a track is looked for while its queue plays out.
const END_POLL_US: i32 = 50_000;
/// The UI's poll while a track plays: the seek bar and the times.
pub const POLL_US: i32 = 250_000;
/// Before this, Previous goes to the previous track; after, to the start of this one.
const RESTART_MS: u64 = 3000;
pub const FOOTER: &str = "Stops when you leave Files.";

/// What the worker is doing with the track the UI last loaded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Play {
    #[default]
    Idle,
    Playing,
    Paused,
    /// A call or the ringtone has the speaker: paused until it ends.
    Held,
    /// The track played to its end.
    Ended,
    /// It couldn't play: NeoAudio's or the decoder's code.
    Failed(i32),
}

enum Cmd {
    /// Request number, path, where to start (ms), playing or paused.
    Load(u32, String, u64, bool),
    Play,
    Pause,
    /// Files went to the background: pause, unless a call has the speaker (then held, to resume after it).
    Hide,
    Seek(u64),
    Stop,
}

#[derive(Default)]
struct Shared {
    cmds: VecDeque<Cmd>,
    /// The request the state is about.
    n: u32,
    state: Play,
    pos_ms: u64,
    started: bool,
}

pub struct Worker {
    q: Mutex<Shared>,
    wake: Semaphore,
}

/// A file as the decoders read it.
struct At<F>(F, u64);

impl<F: neo_fs::File> ReadAt for At<F> {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> neo_format_archive::Result<usize> {
        self.0
            .read_at(pos, buf)
            .map_err(|e| neo_format_archive::Error::Io(e.code()))
    }
    fn size(&self) -> u64 {
        self.1
    }
}

fn open_track<'a>(fs: &'a E32Fs, path: &str) -> Result<(Track<'a>, u64), Error> {
    let f = fs.open(path, Open::Read).map_err(|e| Error::Io(e.code()))?;
    let size = f.size().map_err(|e| Error::Io(e.code()))?;
    Ok((neo_format_media::open(At(f, size))?, size))
}

/// A file system's code `c` is `FILE + c`; the decoders' are below `MEDIA`; NeoAudio's stay as they are.
const FILE: i32 = -1000;
const MEDIA: i32 = -2000;

fn media_code(e: Error) -> i32 {
    match e {
        Error::NotAudio => MEDIA - 1,
        Error::Unsupported => MEDIA - 2,
        Error::Corrupt => MEDIA - 3,
        Error::Io(c) => FILE + c,
    }
}

/// The sentence for a code the worker or the opener gave.
pub fn why(code: i32) -> String {
    match code {
        c if c == MEDIA - 1 => "It isn't audio Files knows.".into(),
        c if c == MEDIA - 2 => "Files can't play this kind of audio yet.".into(),
        c if c == MEDIA - 3 => "The file is broken.".into(),
        c if c == FILE - 1 => "The file isn't there any more.".into(),
        c if c == FILE - 18 => "Its drive isn't ready.".into(),
        c if c <= FILE => format!("It couldn't be read ({}).", c - FILE),
        neoaudio::err::ARGUMENT => "NeoAudio can't play its sample rate.".into(),
        -1 => "The audio service isn't running.".into(),
        neoaudio::err::NOT_READY => "The audio service isn't ready yet.".into(),
        c => format!("NeoAudio couldn't play it ({c})."),
    }
}

/// NeoAudio's stream for one track.
struct Out {
    s: Session,
    rate: u32,
    /// INFO's frame count when the position was last set, and that position (frames).
    f0: u64,
    base: u64,
}

impl Out {
    fn open(rate: u32, channels: u8) -> Result<Out, i32> {
        let (major, minor, build) = neoaudio::VERSION;
        let s = Session::connect(
            neoaudio::NAME,
            TVersion {
                major: major as u8,
                minor: minor as u8,
                build: build as u16,
            },
        )?;
        let mut a = TIpcArgs::new();
        a.int(0, rate as i32).int(1, channels as i32).int(2, 0);
        s.send_receive(op::OPEN, &a)?;
        // Pixel test images play muted (the stream's own gain; the media volume stays): the PCM still goes through
        // the ADSP and the underruns still count
        #[cfg(feature = "test-files")]
        {
            let mut v = TIpcArgs::new();
            v.int(0, 0);
            s.send_receive(op::VOLUME, &v)?;
        }
        Ok(Out {
            s,
            rate,
            f0: 0,
            base: 0,
        })
    }

    fn info(&self) -> Info {
        let mut b = [0u8; Info::SIZE];
        let mut des = TPtr8::new(&mut b);
        let mut a = TIpcArgs::new();
        a.des8(0, &mut des);
        match self.s.send_receive(op::INFO, &a) {
            Ok(_) => Info::from_bytes(&b).unwrap_or_default(),
            Err(_) => Info::default(),
        }
    }

    fn call(&self, o: i32, v: i32) {
        let mut a = TIpcArgs::new();
        a.int(0, v);
        let _ = self.s.send_receive(o, &a);
    }

    fn write(&self, pcm: &[u8]) -> Result<(), i32> {
        let des = TPtrC8::new(pcm);
        let mut a = TIpcArgs::new();
        a.desc8(0, &des);
        self.s.send_receive(op::WRITE, &a).map(|_| ())
    }

    /// Where playback is (ms), from what NeoAudio has consumed.
    fn pos_ms(&self, i: &Info) -> u64 {
        (self.base + i.frames.saturating_sub(self.f0)) * 1000 / self.rate as u64
    }

    /// Drop what is queued and go on from `frame`.
    fn restart_at(&mut self, frame: u64) {
        self.call(op::STOP, 0);
        self.f0 = self.info().frames;
        self.base = frame;
    }
}

/// The track the worker plays.
struct Now<'a> {
    n: u32,
    track: Track<'a>,
    out: Out,
    state: Play,
    /// The decoder has nothing more: the queue plays out.
    drained: bool,
    /// Samples a WRITE carries: about [`WRITE_MS`], whole frames.
    chunk: usize,
}

impl Worker {
    pub fn new() -> Result<&'static Worker, i32> {
        Ok(Box::leak(Box::new(Worker {
            q: Mutex::new(Shared::default())?,
            wake: Semaphore::new(0)?,
        })))
    }

    fn post(&'static self, c: Cmd) {
        self.q.lock().cmds.push_back(c);
        self.start();
        self.wake.signal();
    }

    fn set(&self, n: u32, state: Play, pos_ms: Option<u64>) {
        let mut q = self.q.lock();
        if q.n == n {
            q.state = state;
            if let Some(p) = pos_ms {
                q.pos_ms = p;
            }
        }
    }

    /// Request `n`, its state and position (ms).
    pub fn now(&self) -> (u32, Play, u64) {
        let q = self.q.lock();
        (q.n, q.state, q.pos_ms)
    }

    fn start(&'static self) {
        let mut q = self.q.lock();
        if q.started {
            return;
        }
        q.started = true;
        drop(q);
        let r = e32::thread::spawn_with_stack(STACK, move || {
            let Ok(fs) = E32Fs::connect() else { return -1 };
            self.run(&fs);
            0
        });
        if r.is_err() {
            self.q.lock().started = false;
        }
    }

    fn load<'a>(
        &self,
        fs: &'a E32Fs,
        n: u32,
        path: &str,
        at_ms: u64,
        play: bool,
    ) -> Option<Now<'a>> {
        let fail = |code: i32| {
            dprintln!("files: audio {} -> {}", path, code);
            self.set(n, Play::Failed(code), None);
            None
        };
        let mut track = match open_track(fs, path) {
            Ok((t, _)) => t,
            Err(e) => return fail(media_code(e)),
        };
        let mut out = match Out::open(track.info.rate, track.info.channels) {
            Ok(o) => o,
            Err(e) => return fail(e),
        };
        let frame = at_ms * track.info.rate as u64 / 1000;
        let frame = track.seek(frame).unwrap_or(0);
        out.base = frame;
        if !play {
            out.call(op::PAUSE, 1);
        }
        dprintln!(
            "files: audio {} {:?} {} Hz x{} {} frames",
            path,
            track.format,
            track.info.rate,
            track.info.channels,
            track.info.frames
        );
        let state = if play { Play::Playing } else { Play::Paused };
        self.set(n, state, Some(at_ms));
        let ch = track.info.channels.max(1) as usize;
        let frames = (track.info.rate as usize * WRITE_MS / 1000).max(64);
        let chunk = (frames * ch).min(CHUNK / 2) / ch * ch;
        Some(Now {
            chunk,
            n,
            track,
            out,
            state,
            drained: false,
        })
    }

    fn run(&self, fs: &E32Fs) {
        let mut now: Option<Now<'_>> = None;
        let mut pcm = alloc::vec![0i16; CHUNK / 2];
        let mut bytes: Vec<u8> = Vec::with_capacity(CHUNK);
        loop {
            let cmds = core::mem::take(&mut self.q.lock().cmds);
            for c in cmds {
                match c {
                    Cmd::Load(n, path, at, play) => {
                        // the old stream closes (and stops) before the new one opens
                        drop(now.take());
                        now = self.load(fs, n, &path, at, play);
                    }
                    Cmd::Stop => now = None,
                    Cmd::Play => {
                        if let Some(p) = now.as_mut().filter(|p| p.state == Play::Paused) {
                            p.out.call(op::PAUSE, 0);
                            p.state = Play::Playing;
                        }
                    }
                    Cmd::Pause => {
                        if let Some(p) = now
                            .as_mut()
                            .filter(|p| matches!(p.state, Play::Playing | Play::Held))
                        {
                            p.out.call(op::PAUSE, 1);
                            p.state = Play::Paused;
                        }
                    }
                    Cmd::Hide => {
                        if let Some(p) = now
                            .as_mut()
                            .filter(|p| matches!(p.state, Play::Playing | Play::Held))
                        {
                            p.out.call(op::PAUSE, 1);
                            p.state = if p.out.info().state == state::PREEMPTED {
                                Play::Held
                            } else {
                                Play::Paused
                            };
                        }
                    }
                    Cmd::Seek(ms) => {
                        if let Some(p) = now.as_mut() {
                            let f = p
                                .track
                                .seek(ms * p.track.info.rate as u64 / 1000)
                                .unwrap_or(0);
                            p.out.restart_at(f);
                            p.drained = false;
                            if p.state == Play::Ended {
                                p.state = Play::Paused;
                                p.out.call(op::PAUSE, 1);
                            }
                        }
                    }
                }
            }
            let Some(p) = now.as_mut() else {
                self.wake.wait();
                continue;
            };
            match p.state {
                Play::Playing if !p.drained => {
                    let n = match p.track.read(&mut pcm[..p.chunk]) {
                        Ok(n) => n,
                        Err(e) => {
                            // a broken frame mid-file: what came before plays out
                            dprintln!("files: audio decode stopped ({:?})", e);
                            0
                        }
                    };
                    if n == 0 {
                        p.drained = true;
                        continue;
                    }
                    bytes.clear();
                    for s in &pcm[..n] {
                        bytes.extend_from_slice(&s.to_le_bytes());
                    }
                    // waits while 250 ms are queued: the worker keeps pace with the speaker
                    // CANCEL: a seek or Stop dropped the queue under it
                    if let Err(e) = p.out.write(&bytes)
                        && e != neoaudio::err::CANCEL
                    {
                        dprintln!("files: audio write -> {}", e);
                        p.state = Play::Failed(e);
                    }
                }
                Play::Playing => {
                    let i = p.out.info();
                    if i.queued == 0 {
                        time::sleep(100_000); // the resampler's last frames
                        p.state = Play::Ended;
                        dprintln!("files: audio ended, {} underruns", i.underruns);
                    } else {
                        time::sleep(END_POLL_US);
                    }
                }
                Play::Held => {
                    time::sleep(CALL_POLL_US);
                    if p.out.info().state != state::PREEMPTED {
                        p.out.call(op::PAUSE, 0);
                        p.state = Play::Playing;
                        dprintln!("files: audio resumed after the call");
                    }
                }
                _ => {
                    let (n, st) = (p.n, p.state);
                    self.set(n, st, None);
                    self.wake.wait();
                    continue;
                }
            }
            let i = p.out.info();
            if p.state == Play::Playing && i.state == state::PREEMPTED {
                p.out.call(op::PAUSE, 1);
                p.state = Play::Held;
                dprintln!("files: audio held for a call");
            }
            let pos = p.out.pos_ms(&i);
            self.set(p.n, p.state, Some(pos));
        }
    }
}

/// The player's page: the folder's tracks and what the user set.
pub struct Player {
    list: Vec<String>,
    at: usize,
    n: u32,
    shuffle: bool,
    /// 0 off, 1 the folder, 2 this track.
    repeat: u8,
    seed: u32,
    ms: u64,
    shown: (Play, u64),
}

/// FA9's state in [`State`].
pub struct Audio {
    w: Option<&'static Worker>,
    p: Option<Player>,
    n: u32,
    /// Files is in the background: a track that ends doesn't start the next.
    hidden: bool,
}

impl Audio {
    pub fn new() -> Audio {
        Audio {
            w: Worker::new().ok(),
            p: None,
            n: 0,
            hidden: false,
        }
    }

    /// The player is open: the UI polls (a state change right after a tap shows within a poll).
    pub fn busy(&self) -> bool {
        self.p.is_some()
    }
}

pub(crate) enum Act {
    Toggle,
    Prev,
    Next,
    Shuffle,
    Repeat,
    Seek(f32),
}

pub(crate) fn bind(ui: &FilesApp, q: &Rc<RefCell<VecDeque<crate::Action>>>) {
    let push = |q: &Rc<RefCell<VecDeque<crate::Action>>>| {
        let q = q.clone();
        move |a: Act| q.borrow_mut().push_back(crate::Action::Audio(a))
    };
    let p = push(q);
    ui.on_audio_toggle(move || p(Act::Toggle));
    let p = push(q);
    ui.on_audio_prev(move || p(Act::Prev));
    let p = push(q);
    ui.on_audio_next(move || p(Act::Next));
    let p = push(q);
    ui.on_audio_shuffle_tapped(move || p(Act::Shuffle));
    let p = push(q);
    ui.on_audio_repeat_tapped(move || p(Act::Repeat));
    let p = push(q);
    ui.on_audio_seek(move |v| p(Act::Seek(v)));
}

pub fn is_audio(name: &str) -> bool {
    neo_view::family_of(name) == neo_view::Family::Audio
}

/// Opens `o` in the player with the audio files of its folder: the tags and length read here (so a file the
/// decoders can't read goes to the hex view with the reason), the sound on the worker.
pub(crate) fn open(ui: &FilesApp, st: &mut State, o: &Open7) -> Result<&'static str, String> {
    let w = st
        .audio
        .w
        .ok_or_else(|| String::from("No thread for the player."))?;
    let dir = neo_fs::path::parent(&o.path).unwrap_or_default();
    let list: Vec<String> = crate::folder_files(st, &dir, is_audio)
        .into_iter()
        .map(|(p, _, _)| p)
        .collect();
    let at = list.iter().position(|p| *p == o.path).unwrap_or(0);
    let list = if list.is_empty() {
        alloc::vec![o.path.clone()]
    } else {
        list
    };
    let (shuffle, repeat) = st
        .audio
        .p
        .as_ref()
        .map_or((false, 0), |p| (p.shuffle, p.repeat));
    st.audio.p = Some(Player {
        list,
        at,
        n: 0,
        shuffle,
        repeat,
        seed: time::millis() as u32 | 1,
        ms: 0,
        shown: (Play::Idle, u64::MAX),
    });
    describe(ui, st).map_err(why)?;
    load(st, w, 0, true);
    Ok("audio")
}

/// The open track's names, cover and format line (read on the UI thread: the header only).
fn describe(ui: &FilesApp, st: &mut State) -> Result<(), i32> {
    let Some(p) = st.audio.p.as_mut() else {
        return Ok(());
    };
    let path = p.list[p.at].clone();
    let name = neo_fs::path::name_of(&path);
    ui.set_name(name.into());
    ui.set_audio_title(words::title("", name).into());
    ui.set_audio_sub("".into());
    ui.set_audio_meta("".into());
    ui.set_audio_total(words::time(0).into());
    p.ms = 0;
    let (t, size) = open_track(&st.fs, &path).map_err(media_code)?;
    p.ms = t.info.ms();
    let dir = neo_fs::path::parent(&path).unwrap_or_default();
    ui.set_audio_title(words::title(&t.tags.title, name).into());
    ui.set_audio_sub(words::sub(&t.tags.artist, &t.tags.album).into());
    ui.set_audio_meta(words::meta(t.format.name(), t.info.kbps, t.info.rate, size, &dir).into());
    ui.set_audio_total(words::time(p.ms).into());
    ui.set_audio_has_cover(false);
    ui.set_audio_shuffle(p.shuffle);
    ui.set_audio_repeat(p.repeat as i32);
    let many = p.list.len() > 1;
    ui.set_audio_has_prev(many || p.repeat != 0);
    ui.set_audio_has_next(many || p.repeat != 0);
    ui.set_fa7_footer(FOOTER.into());
    p.shown = (Play::Idle, u64::MAX);
    drop(t);
    crate::fa3::opened(st, &path);
    Ok(())
}

fn load(st: &mut State, w: &'static Worker, at_ms: u64, play: bool) {
    let Some(p) = st.audio.p.as_mut() else { return };
    st.audio.n = st.audio.n.wrapping_add(1);
    p.n = st.audio.n;
    {
        let mut q = w.q.lock();
        q.n = p.n;
        q.state = Play::Idle;
        q.pos_ms = at_ms;
    }
    w.post(Cmd::Load(p.n, p.list[p.at].clone(), at_ms, play));
}

/// Go to track `to` of the list and play it (or show it paused).
fn go(ui: &FilesApp, st: &mut State, to: usize, play: bool) {
    let Some(w) = st.audio.w else { return };
    if let Some(p) = st.audio.p.as_mut() {
        p.at = to;
    }
    match describe(ui, st) {
        Ok(()) => load(st, w, 0, play),
        Err(e) => fail(st, w, e),
    }
}

/// The track can't play: the old one stops and the page shows why (once, from [`poll`]).
fn fail(st: &mut State, w: &'static Worker, code: i32) {
    let Some(p) = st.audio.p.as_mut() else { return };
    st.audio.n = st.audio.n.wrapping_add(1);
    p.n = st.audio.n;
    {
        let mut q = w.q.lock();
        q.n = p.n;
        q.state = Play::Failed(code);
        q.pos_ms = 0;
    }
    w.post(Cmd::Stop);
}

/// The next track by shuffle and repeat; None at the end of the folder without repeat.
fn next_of(p: &mut Player, ended: bool) -> Option<usize> {
    let n = p.list.len();
    if ended && p.repeat == 2 {
        return Some(p.at);
    }
    if p.shuffle && n > 1 {
        // xorshift: any other track
        p.seed ^= p.seed << 13;
        p.seed ^= p.seed >> 17;
        p.seed ^= p.seed << 5;
        let k = 1 + (p.seed as usize) % (n - 1);
        return Some((p.at + k) % n);
    }
    if p.at + 1 < n {
        Some(p.at + 1)
    } else if p.repeat != 0 || !ended {
        Some(0)
    } else {
        None
    }
}

pub(crate) fn act(app: &App, ui: &FilesApp, st: &mut State, a: Act) {
    let (Some(w), Some(p)) = (st.audio.w, st.audio.p.as_mut()) else {
        return;
    };
    let (n, state, pos) = w.now();
    let mine = n == p.n;
    match a {
        Act::Toggle => match state {
            Play::Playing | Play::Held if mine => w.post(Cmd::Pause),
            Play::Paused if mine => w.post(Cmd::Play),
            // ended or failed: from the start
            _ => load(st, w, 0, true),
        },
        Act::Prev => {
            if pos > RESTART_MS || p.list.len() == 1 {
                w.post(Cmd::Seek(0));
                if state == Play::Ended || state == Play::Paused {
                    w.post(Cmd::Play);
                }
            } else {
                let to = (p.at + p.list.len() - 1) % p.list.len();
                go(ui, st, to, true);
            }
        }
        Act::Next => {
            if let Some(to) = next_of(p, false) {
                go(ui, st, to, true);
            }
        }
        Act::Shuffle => {
            p.shuffle = !p.shuffle;
            ui.set_audio_shuffle(p.shuffle);
        }
        Act::Repeat => {
            p.repeat = (p.repeat + 1) % 3;
            ui.set_audio_repeat(p.repeat as i32);
            let many = p.list.len() > 1;
            ui.set_audio_has_prev(many || p.repeat != 0);
            ui.set_audio_has_next(many || p.repeat != 0);
        }
        Act::Seek(v) => {
            let ms = (p.ms as f32 * v.clamp(0.0, 1.0)) as u64;
            w.q.lock().pos_ms = ms;
            w.post(Cmd::Seek(ms));
            if state == Play::Ended {
                w.post(Cmd::Play);
            }
        }
    }
    poll(app, ui, st);
}

/// The worker's state on the page; the next track when one ends.
pub(crate) fn poll(app: &App, ui: &FilesApp, st: &mut State) {
    if st.view != View::Fa7 {
        return;
    }
    let (Some(w), Some(p)) = (st.audio.w, st.audio.p.as_mut()) else {
        return;
    };
    let (n, state, pos) = w.now();
    if n != p.n {
        return;
    }
    if state == Play::Ended {
        match next_of(p, true) {
            Some(to) => {
                let play = !st.audio.hidden;
                return go(ui, st, to, play);
            }
            None => {
                // the folder played through: back to its first track, paused
                p.shown = (Play::Idle, u64::MAX);
                return go(ui, st, 0, false);
            }
        }
    }
    if p.shown == (state, pos / 1000) {
        return;
    }
    p.shown = (state, pos / 1000);
    if let Play::Failed(e) = state {
        crate::note_with(app, &why(e), "");
    }
    ui.set_audio_playing(matches!(state, Play::Playing | Play::Held));
    ui.set_audio_elapsed(words::time(pos).into());
    ui.set_audio_pos(if p.ms == 0 {
        0.0
    } else {
        (pos as f32 / p.ms as f32).min(1.0)
    });
}

/// The player closes (Back, Show as hex): the stream stops.
pub(crate) fn close(st: &mut State) {
    if st.audio.p.take().is_some()
        && let Some(w) = st.audio.w
    {
        w.post(Cmd::Stop);
    }
}

/// Files went to the background ("Stops when you leave Files") or came back. A call that brings Phone to the front
/// has the speaker already: the worker holds the track and resumes it after the call.
pub(crate) fn visible(st: &mut State, shown: bool) {
    st.audio.hidden = !shown;
    if let (false, Some(w), Some(_)) = (shown, st.audio.w, st.audio.p.as_ref()) {
        w.post(Cmd::Hide);
    }
}

/// A WAV file: `fmt ` (PCM or float), the names as LIST/INFO, the samples.
#[cfg(feature = "test-files")]
fn wav_file(rate: u32, ch: u16, bits: u16, float: bool, tags: [&str; 3], data: &[u8]) -> Vec<u8> {
    let align = ch * bits / 8;
    let mut info = b"INFO".to_vec();
    for (id, v) in [b"INAM", b"IART", b"IPRD"].into_iter().zip(tags) {
        let mut v = v.as_bytes().to_vec();
        v.push(0);
        if v.len() & 1 == 1 {
            v.push(0);
        }
        info.extend_from_slice(id);
        info.extend_from_slice(&(v.len() as u32).to_le_bytes());
        info.extend_from_slice(&v);
    }
    let mut f = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0".to_vec();
    f.extend_from_slice(&(if float { 3u16 } else { 1 }).to_le_bytes());
    f.extend_from_slice(&ch.to_le_bytes());
    f.extend_from_slice(&rate.to_le_bytes());
    f.extend_from_slice(&(rate * align as u32).to_le_bytes());
    f.extend_from_slice(&align.to_le_bytes());
    f.extend_from_slice(&bits.to_le_bytes());
    f.extend_from_slice(b"LIST");
    f.extend_from_slice(&(info.len() as u32).to_le_bytes());
    f.extend_from_slice(&info);
    f.extend_from_slice(b"data");
    f.extend_from_slice(&(data.len() as u32).to_le_bytes());
    f.extend_from_slice(data);
    let riff = (f.len() - 8) as u32;
    f[4..8].copy_from_slice(&riff.to_le_bytes());
    f
}

/// A note's samples at `rate`: `hz` for `ms`, fading out.
#[cfg(feature = "test-files")]
fn note(rate: u32, hz: f32, ms: u32) -> impl Iterator<Item = f32> {
    let n = rate * ms / 1000;
    (0..n).map(move |i| {
        let t = i as f32 / rate as f32;
        let fade = 1.0 - i as f32 / n as f32;
        libm::sinf(2.0 * core::f32::consts::PI * hz * t) * 0.5 * fade
    })
}

/// Pixel test images only (FA9): `C:\Data\fa9\` with WAVs made here (no recordings in the repo): an arpeggio with
/// names (44.1 kHz stereo, 20 s), a 3-minute tone for calls (22.05 kHz mono), 8 kHz 8-bit, 48 kHz float, and a
/// broken one.
#[cfg(feature = "test-files")]
pub(crate) fn test_files(st: &State) {
    use files_ops::store;
    const DIR: &str = "C:\\Data\\fa9\\";
    if st.fs.entry(&format!("{DIR}Arpeggio.wav")).is_ok() {
        return;
    }
    let s16 = |v: f32| ((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes();
    // C E G C, ten times; the right channel a fifth up
    let notes = [261.6f32, 329.6, 392.0, 523.3];
    let mut arp = Vec::new();
    for k in 0..40 {
        let hz = notes[k % 4];
        for (l, r) in note(44_100, hz, 500).zip(note(44_100, hz * 1.5, 500)) {
            arp.extend_from_slice(&s16(l));
            arp.extend_from_slice(&s16(r));
        }
    }
    let arp = wav_file(
        44_100,
        2,
        16,
        false,
        ["Arpeggio", "NeoSymbian", "FA9 tests"],
        &arp,
    );
    // three minutes: a tone that steps each second, so a resumed call can be heard to pick up where it was
    let mut long = Vec::new();
    for s in 0..180u32 {
        for v in note(22_050, 220.0 + (s % 12) as f32 * 20.0, 1000) {
            long.extend_from_slice(&s16(v));
        }
    }
    let long = wav_file(
        22_050,
        1,
        16,
        false,
        ["Three minutes", "NeoSymbian", "FA9 tests"],
        &long,
    );
    let low: Vec<u8> = note(8000, 440.0, 6000)
        .map(|v| (v * 127.0 + 128.0) as u8)
        .collect();
    let low = wav_file(8000, 1, 8, false, ["8 kHz 8-bit", "", ""], &low);
    let mut float = Vec::new();
    for (l, r) in note(48_000, 440.0, 10_000).zip(note(48_000, 660.0, 10_000)) {
        float.extend_from_slice(&l.to_le_bytes());
        float.extend_from_slice(&r.to_le_bytes());
    }
    let float = wav_file(48_000, 2, 32, true, ["", "", ""], &float);
    let broken = arp[..30].to_vec();
    for (name, b) in [
        ("Arpeggio.wav", &arp),
        ("Three minutes.wav", &long),
        ("8k 8-bit.wav", &low),
        ("Float 48k.wav", &float),
        ("broken.wav", &broken),
    ] {
        let _ = store::write(&*st.fs, &format!("{DIR}{name}"), b);
    }
}
