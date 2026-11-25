// SPDX-License-Identifier: GPL-3.0-only
//! FA12 V0 (`VBENCH.EXE`): how fast the pure-Rust decoders (H.264, VP8, VP9) run on the Pixel. Each clip in `C:\Data\fa12\` is decoded
//! whole, single-threaded, pinned to an A75 (CPU 7) and then an A55 (CPU 0); prints
//! `vbench: <clip> cpu N: F frames WxH in T ms = X fps, slowest S ms`. Exit code = clips that failed.
#![no_std]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use e32::{dprintln, time};
use fs_e32::E32Fs;
use neo_fs::{File as _, Fs as _, Open};

e32::main!(main);

const DIR: &str = "C:\\Data\\fa12\\";
const CLIPS: &[&str] = &[
    "phone720.h264",
    "baseline720.h264",
    "deblock1080.h264",
    "bench_720p_300f_bframes_complex.h264",
    "bench_720p_300f_ponly_complex.h264",
    "bench_1080p_100f_complex.h264",
];
/// The fast cluster's last CPU and the slow cluster's first (Pixel 3a XL: 0-5 A55, 6-7 A75).
const CPUS: &[u32] = &[7];

/// `EKernelHalLockThreadToCpu` (19): Files' e32 pin predates `thread::lock_to_cpu`; 0xffff_ffff unpins.
fn pin(cpu: u32) -> bool {
    // SAFETY: the kernel takes the CPU as a plain value in a1 and ignores a2.
    unsafe {
        e32::sys::ui::hal_function(
            e32::sys::ui::EHAL_GROUP_KERNEL,
            19,
            cpu as *mut u8,
            core::ptr::null_mut(),
        ) >= 0
    }
}

/// `ENeoHalCpuBoost` (0x4e29, kernel fa12-media-boost): top clocks for `ms`; Err without the kernel change.
fn boost(ms: u32) -> i32 {
    // SAFETY: the kernel takes the milliseconds as a plain value in a1.
    unsafe {
        e32::sys::ui::hal_function(
            e32::sys::ui::EHAL_GROUP_VARIANT,
            0x4e29,
            ms as usize as *mut u8,
            core::ptr::null_mut(),
        )
    }
}

fn read(fs: &E32Fs, name: &str) -> Option<Vec<u8>> {
    let path = alloc::format!("{DIR}{name}");
    let f = fs.open(&path, Open::Read).ok()?;
    let mut data = vec![0; f.size().ok()? as usize];
    let mut at = 0;
    while at < data.len() {
        let n = f.read_at(at as u64, &mut data[at..]).ok()?;
        if n == 0 {
            return None;
        }
        at += n;
    }
    Some(data)
}

/// FNV-1a over every picture's planes: the same on the Mac and the phone when the NEON kernels are right.
static SUM: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn fnv(h: u64, b: &[u8]) -> u64 {
    b.iter()
        .fold(h, |h, &x| (h ^ u64::from(x)).wrapping_mul(0x100_0000_01b3))
}

/// The player's colour conversion: a 720p picture to the 1080x607 place on screen, straight and turned.
fn yuv_bench(f: &rust_h264::decoder::Frame) {
    use neo_format_media::av::{video::Frame, yuv};
    let fr = Frame {
        width: f.width,
        height: f.height,
        y: f.y.clone(),
        u: f.u.clone(),
        v: f.v.clone(),
        y_stride: f.width as usize,
        uv_stride: f.chroma_width() as usize,
        pts_us: 0,
    };
    let mut px = vec![0u32; 1920 * 1080];
    for (turn, w, h) in [
        (yuv::Turn::None, 1080, 607),
        (yuv::Turn::Quarter, 1080, 1920),
    ] {
        let t0 = time::fast_counter();
        for _ in 0..10 {
            yuv::draw(&fr, turn, &mut px, w as usize, w, h);
        }
        let us = time::counter_us(t0, time::fast_counter(), time::fast_counter_hz()) / 10;
        dprintln!(
            "vbench: yuv {}x{} -> {}x{} {:?}: {} us",
            f.width,
            f.height,
            w,
            h,
            turn,
            us
        );
    }
}

/// Frames, size, total and slowest decode time in microseconds.
fn h264(data: &[u8]) -> (u32, (u32, u32), u64, u32) {
    let hz = time::fast_counter_hz();
    let nals = rust_h264::nal::parse_annex_b(data);
    let mut dec = rust_h264::decoder::Decoder::new();
    let (mut frames, mut size, mut total, mut slowest) = (0, (0, 0), 0u64, 0);
    let mut sum = 0xcbf2_9ce4_8422_2325u64;
    let mut last = None;
    for (i, nal) in nals.iter().enumerate() {
        if i % 16 == 0 {
            boost(2000);
        }
        let t0 = time::fast_counter();
        let out = dec.decode_nal(nal);
        let us = time::counter_us(t0, time::fast_counter(), hz);
        total += u64::from(us);
        slowest = slowest.max(us);
        if let Ok(Some(f)) = out {
            frames += 1;
            size = (f.width, f.height);
            sum = fnv(fnv(fnv(sum, &f.y), &f.u), &f.v);
            last = Some(f);
        }
    }
    if let Some(f) = last.take() {
        yuv_bench(&f);
    }
    let t0 = time::fast_counter();
    while let Some(f) = dec.flush() {
        frames += 1;
        size = (f.width, f.height);
        sum = fnv(fnv(fnv(sum, &f.y), &f.u), &f.v);
    }
    SUM.store(sum, core::sync::atomic::Ordering::Relaxed);
    total += u64::from(time::counter_us(t0, time::fast_counter(), hz));
    (frames, size, total, slowest)
}

/// Times `step` over each IVF frame: frames, size, total and slowest microseconds.
fn ivf(
    data: &[u8],
    mut step: impl FnMut(&[u8]) -> Option<(u32, u32)>,
) -> (u32, (u32, u32), u64, u32) {
    let hz = time::fast_counter_hz();
    let (mut frames, mut size, mut total, mut slowest) = (0, (0, 0), 0u64, 0);
    let Ok(reader) = vp8_decoder::ivf::IvfReader::new(data) else {
        return (0, size, 0, 0);
    };
    for (i, f) in reader.flatten().enumerate() {
        if i % 16 == 0 {
            boost(2000);
        }
        let t0 = time::fast_counter();
        let out = step(f.data);
        let us = time::counter_us(t0, time::fast_counter(), hz);
        total += u64::from(us);
        slowest = slowest.max(us);
        if let Some(s) = out {
            frames += 1;
            size = s;
        }
    }
    (frames, size, total, slowest)
}

fn vp8(data: &[u8]) -> (u32, (u32, u32), u64, u32) {
    let mut dec = vp8_decoder::Decoder::new();
    ivf(data, |f| match dec.decode(f) {
        Ok(Some(p)) => Some((p.width, p.height)),
        _ => None,
    })
}

fn vp9(data: &[u8]) -> (u32, (u32, u32), u64, u32) {
    let mut dec = rusty_vp9::Vp9Decoder::new();
    ivf(data, |f| {
        dec.push(f, None).ok()?;
        dec.next_frame().ok().map(|p| (p.width, p.height))
    })
}

fn main() -> i32 {
    let Ok(fs) = E32Fs::connect() else {
        dprintln!("vbench: no file server");
        return 1;
    };
    let mut failed = 0;
    dprintln!("vbench: boost {}", boost(2000));
    for name in CLIPS {
        let Some(data) = read(&fs, name) else {
            dprintln!("vbench: {} missing", name);
            continue;
        };
        for &cpu in CPUS {
            if !pin(cpu) {
                dprintln!("vbench: can't pin to cpu {}", cpu);
                continue;
            }
            let (frames, (w, h), us, slowest) = if name.ends_with(".h264") {
                h264(&data)
            } else if name.starts_with("vp8") {
                vp8(&data)
            } else {
                vp9(&data)
            };
            pin(0xffff_ffff);
            if frames == 0 {
                failed += 1;
            }
            let ms = us / 1000;
            let fps10 = (u64::from(frames) * 10_000_000)
                .checked_div(us)
                .unwrap_or(0);
            dprintln!(
                "vbench: {} cpu {}: {} frames {}x{} in {} ms = {}.{} fps, slowest {} ms, sum {:x}",
                name,
                cpu,
                frames,
                w,
                h,
                ms,
                fps10 / 10,
                fps10 % 10,
                slowest / 1000,
                SUM.load(core::sync::atomic::Ordering::Relaxed)
            );
        }
    }
    dprintln!("vbench: done, {} failed", failed);
    failed
}
