// SPDX-License-Identifier: GPL-3.0-only
//! The snapshot harness: one software-rendered `FilesApp` per test binary (Slint's platform is per process), drawn
//! at the Pixel's app area (1080 x 2037: 2160 minus the UI server's status bar and the Gestures band, dp = 3) into
//! `snapshots/<name>.png` at the repo root, for side-by-side review with the approved mockups.
#![allow(dead_code)]

use files_ui::{FilesApp, Theme};
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, TargetPixel,
};
use slint::platform::{Platform, WindowAdapter};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use std::rc::Rc;

pub const W: u32 = 1080;
pub const H: u32 = 2037;
pub const DP: f32 = 3.0;

struct Host(Rc<MinimalSoftwareWindow>);

thread_local! {
    /// The test clock: each shot moves it a second on, so animations (a sheet opening or closing) have finished.
    static NOW: std::cell::Cell<core::time::Duration> = const { std::cell::Cell::new(core::time::Duration::ZERO) };
}

impl Platform for Host {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
    fn duration_since_start(&self) -> core::time::Duration {
        NOW.get()
    }
}

#[derive(Clone, Copy, Default)]
struct Px(u32);

impl TargetPixel for Px {
    fn blend(&mut self, c: PremultipliedRgbaColor) {
        let a = 255 - u32::from(c.alpha);
        let ch = |s: u32, v: u8| (u32::from(v) + ((self.0 >> s) & 0xff) * a / 255).min(255);
        self.0 = ch(16, c.red) << 16 | ch(8, c.green) << 8 | ch(0, c.blue);
    }
    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Px(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b))
    }
}

pub struct Shot {
    window: Rc<MinimalSoftwareWindow>,
    pub ui: FilesApp,
    /// the app's height now: 2037 in Gestures (679 dp), 1917 in Buttons mode (639 dp)
    h: std::cell::Cell<u32>,
}

/// The app at the Pixel's size; call once per test binary.
pub fn app() -> Shot {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Host(window.clone())))
        .expect("one platform per process");
    let ui = FilesApp::new().expect("FilesApp");
    ui.global::<Theme>().set_dp(3.0);
    window.set_size(slint::PhysicalSize::new(W, H));
    ui.show().expect("show");
    Shot {
        window,
        ui,
        h: std::cell::Cell::new(H),
    }
}

impl Shot {
    /// The app's height in px (the navigation mode changes it).
    pub fn resize(&self, h: u32) {
        self.h.set(h);
        self.window.set_size(slint::PhysicalSize::new(W, h));
    }

    /// A finger down at (x, y) dp, and up again when `release`.
    pub fn press(&self, x: f32, y: f32, release: bool) {
        let w = self.ui.window();
        let position = LogicalPosition::new(x * DP, y * DP);
        w.dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        if release {
            w.dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
        }
    }

    /// The finger up at (x, y) dp.
    pub fn release(&self, x: f32, y: f32) {
        self.ui
            .window()
            .dispatch_event(WindowEvent::PointerReleased {
                position: LogicalPosition::new(x * DP, y * DP),
                button: PointerEventButton::Left,
            });
    }

    /// Let `ms` pass (Slint's timers: a long press).
    pub fn wait(&self, ms: u64) {
        NOW.set(NOW.get() + core::time::Duration::from_millis(ms));
        slint::platform::update_timers_and_animations();
    }

    /// Draw and save `snapshots/<name>.png`; returns the pixels for checks.
    pub fn shoot(&self, name: &str) -> Vec<u32> {
        let px = self.pixels();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../snapshots");
        std::fs::create_dir_all(&root).unwrap();
        let file = std::fs::File::create(root.join(format!("{name}.png"))).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), W, self.h.get());
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        let rgb: Vec<u8> = px
            .iter()
            .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8])
            .collect();
        w.write_image_data(&rgb).unwrap();
        px
    }

    /// Draw (timers and animations first) and return the pixels, 0xRRGGBB by rows of [`W`].
    pub fn pixels(&self) -> Vec<u32> {
        // change handlers run first (they may start an animation), then a second passes
        slint::platform::update_timers_and_animations();
        NOW.set(NOW.get() + core::time::Duration::from_secs(1));
        slint::platform::update_timers_and_animations();
        self.window.request_redraw();
        let h = self.h.get();
        let mut px = vec![Px(0); (W * h) as usize];
        self.window.draw_if_needed(|r| {
            r.render(&mut px, W as usize);
        });
        px.into_iter().map(|p| p.0).collect()
    }
}

// ── sample data shared by the board tests ──

use files_core::{self as model, Item};
use files_ui::{DriveRow, FileRow, drive_row, file_row};
use slint::{ModelRc, SharedString, VecModel};

pub fn model_of<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

pub fn strings(v: &[&str]) -> ModelRc<SharedString> {
    model_of(v.iter().map(|s| SharedString::from(*s)).collect())
}

/// A day of 2026 (or another year) as a Symbian `TTime`: days since 1970-01-01, plus Symbian's 719,540 before that.
pub fn day(days_since_1970: i64) -> i64 {
    (719_540 + days_since_1970) * 86_400 * 1_000_000
}

/// 16 Nov 2026, "today" for the boards.
pub const TODAY: i64 = 20_773;

pub fn dated(d: &str) -> i64 {
    let (n, month) = d.split_once(' ').unwrap();
    let n: i64 = n.parse().unwrap();
    // days since 1970 of the 1st of Oct/Nov 2026
    let first = match month {
        "Oct" => 20_727,
        "Nov" => 20_758,
        other => panic!("{other}"),
    };
    day(first + n - 1)
}

pub fn file(name: &str, size: u64, date: &str) -> Item {
    Item {
        name: name.into(),
        size,
        modified: dated(date),
        ..Item::default()
    }
}

pub fn folder(name: &str, count: u32) -> Item {
    Item {
        name: name.into(),
        is_dir: true,
        count: Some(count),
        ..Item::default()
    }
}

pub const KB: u64 = 1024;
pub const MB: u64 = 1024 * 1024;

pub fn rows(items: &[Item], checked: &[usize]) -> ModelRc<FileRow> {
    model_of(
        items
            .iter()
            .enumerate()
            .map(|(i, it)| file_row(it, day(TODAY), checked.contains(&i)))
            .collect(),
    )
}

pub fn drives() -> Vec<DriveRow> {
    const GB: u64 = 1 << 30;
    vec![
        drive_row(
            'C',
            model::drive_label('C', 0x11, 9),
            "phone",
            8 * GB,
            2 * GB + 922 * MB,
        ),
        drive_row(
            'E',
            model::drive_label('E', 0x11, 9),
            "mass",
            41 * GB + 819 * MB,
            29 * GB + 410 * MB,
        ),
        drive_row(
            'Z',
            model::drive_label('Z', 0x12, 7),
            "rom",
            2 * GB + 512 * MB,
            0,
        ),
    ]
}
