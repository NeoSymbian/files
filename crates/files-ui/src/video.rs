// SPDX-License-Identifier: GPL-3.0-only
//! The video player's screen (files2-view-video), without the device: where the picture goes in the view, when the
//! controls hide, the speeds, and the words (times, the info line). The device's player (symbian/app video.rs) drives
//! it and hands the frames over.

use crate::FilesApp;
use alloc::format;
use alloc::string::String;
use neo_format_media::av::yuv::{self, Turn};

/// The controls hide this long after the last touch while a clip plays.
pub const HIDE_MS: u64 = 3000;

/// The speed chip's steps, as percent and as shown.
pub const SPEEDS: [(u32, &str); 6] = [
    (50, "0.5×"),
    (75, "0.75×"),
    (100, "1×"),
    (125, "1.25×"),
    (150, "1.5×"),
    (200, "2×"),
];
const NORMAL: usize = 2;

pub struct VideoPlayer {
    /// The picture's size.
    pub picture: (u32, u32),
    pub duration_us: u64,
    /// The view's size, px: the window's (already turned while landscape).
    pub area: (u32, u32),
    pub landscape: bool,
    pub full: bool,
    pub controls: bool,
    /// When the controls last showed or were touched (ms).
    touched_ms: u64,
    pub speed: usize,
    pub muted: bool,
}

impl VideoPlayer {
    pub fn new(picture: (u32, u32), duration_us: u64) -> Self {
        Self {
            picture,
            duration_us,
            area: (1, 1),
            landscape: false,
            full: false,
            controls: true,
            touched_ms: 0,
            speed: NORMAL,
            muted: false,
        }
    }

    pub fn layout(&mut self, w: u32, h: u32) {
        self.area = (w.max(1), h.max(1));
    }

    /// The picture's place in the view: the largest of its shape, centred.
    pub fn place(&self) -> (u32, u32, u32, u32) {
        yuv::fit(self.picture, Turn::None, self.area.0, self.area.1)
    }

    /// A tap on the picture: the controls show, or hide.
    pub fn tap(&mut self, now_ms: u64) {
        self.controls = !self.controls;
        self.touched_ms = now_ms;
    }

    /// A control was used: they stay a while longer.
    pub fn touched(&mut self, now_ms: u64) {
        self.controls = true;
        self.touched_ms = now_ms;
    }

    /// Hides the controls once a playing clip has gone untouched for [`HIDE_MS`]; true when they went.
    pub fn tick(&mut self, now_ms: u64, playing: bool) -> bool {
        if self.controls && playing && now_ms.saturating_sub(self.touched_ms) >= HIDE_MS {
            self.controls = false;
            return true;
        }
        false
    }

    /// The speed chip: the next step, round to the slowest after the fastest.
    pub fn next_speed(&mut self) {
        self.speed = (self.speed + 1) % SPEEDS.len();
    }

    pub fn percent(&self) -> u32 {
        SPEEDS[self.speed].0
    }

    pub fn speed_text(&self) -> &'static str {
        SPEEDS[self.speed].1
    }
}

/// `0:13`, `4:05`, `1:02:03`.
pub fn clock(us: u64) -> String {
    crate::audio::time(us / 1000)
}

/// The line over the seek bar: `17 Nov 2026, 09:58 · 1280 × 720 · 9.8 MB · E:\Videos`.
pub fn info(modified: i64, picture: (u32, u32), bytes: u64, folder: &str) -> String {
    // day_text leaves out this year's year; `now` at 0 (year 0) keeps it
    let day = files_core::day_text(modified, 0);
    let s = modified.div_euclid(1_000_000).rem_euclid(86_400);
    let folder = folder.trim_end_matches('\\');
    format!(
        "{day}, {:02}:{:02} · {} × {} · {} · {folder}",
        s / 3600,
        s / 60 % 60,
        picture.0,
        picture.1,
        files_core::size_text(bytes)
    )
}

/// The screen's properties from the player and the clip's state (`frame` is set apart: it changes per frame).
pub fn show_video(ui: &FilesApp, v: &VideoPlayer, now_us: u64, buffered_us: u64, playing: bool) {
    let (x, y, w, h) = v.place();
    ui.set_video_x(x as i32);
    ui.set_video_y(y as i32);
    ui.set_video_w(w as i32);
    ui.set_video_h(h as i32);
    ui.set_video_controls(v.controls);
    ui.set_video_playing(playing);
    ui.set_video_muted(v.muted);
    ui.set_video_now(clock(now_us).into());
    ui.set_video_total(clock(v.duration_us).into());
    ui.set_video_speed(v.speed_text().into());
    let of = |t: u64| (t as f64 / v.duration_us.max(1) as f64).clamp(0.0, 1.0) as f32;
    ui.set_video_played(of(now_us));
    ui.set_video_buffered(of(buffered_us));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_controls_hide_while_playing() {
        let mut v = VideoPlayer::new((1280, 720), 42_000_000);
        v.touched(1000);
        assert!(!v.tick(3999, true));
        assert!(v.tick(4000, true));
        assert!(!v.controls);
        // paused, they stay
        v.tap(5000);
        assert!(!v.tick(60_000, false));
        assert!(v.controls);
    }

    #[test]
    fn speeds_go_round() {
        let mut v = VideoPlayer::new((1280, 720), 0);
        assert_eq!((v.percent(), v.speed_text()), (100, "1×"));
        for _ in 0..3 {
            v.next_speed();
        }
        assert_eq!(v.speed_text(), "2×");
        v.next_speed();
        assert_eq!(v.percent(), 50);
    }

    #[test]
    fn the_picture_fits() {
        let mut v = VideoPlayer::new((1280, 720), 0);
        v.layout(1080, 2037);
        assert_eq!(v.place(), (0, 715, 1080, 607));
        // landscape: the window is turned, the picture fills its height
        v.layout(2160, 1080);
        assert_eq!(v.place(), (120, 0, 1920, 1080));
    }

    #[test]
    fn words() {
        assert_eq!(clock(13_400_000), "0:13");
        assert_eq!(clock(3_723_000_000), "1:02:03");
    }
}
