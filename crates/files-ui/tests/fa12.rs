// SPDX-License-Identifier: GPL-3.0-only
//! FA12 (video): the player's boards with a real frame (the H.264 fixture of neo-format-media, decoded and converted
//! as the device does), portrait, full screen and landscape, and a clip that can't play.
mod common;

use files_ui::video::{VideoPlayer, info, show_video};
use neo_format_media::av::{self, video, yuv};
use slint::{Image, Rgb8Pixel, SharedPixelBuffer};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../neo-format-media/tests/data")
            .join(name),
    )
    .unwrap()
}

/// The picture shown at `us`, decoded, converted to the place's size as the player's Slint path does.
fn frame_at(file: &[u8], us: i64, w: u32, h: u32) -> Image {
    let mut d = av::open(file).unwrap();
    let (v, _) = av::pick(d.tracks());
    let v = v.unwrap();
    let mut dec = video::decoder(&d.tracks()[v]).unwrap();
    let mut out = Vec::new();
    while let Some(p) = d.next().unwrap() {
        if p.track == v {
            dec.decode(&p, &mut out).unwrap();
        }
        if out.iter().any(|f| f.pts_us >= us) {
            break;
        }
    }
    let f = out.into_iter().find(|f| f.pts_us >= us).unwrap();
    let mut px = vec![0u32; (w * h) as usize];
    yuv::draw(&f, yuv::Turn::None, &mut px, w as usize, w, h);
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

#[test]
fn fa12() {
    let s = common::app();
    let ui = &s.ui;
    let file = fixture("h264-aac.mp4");
    ui.set_view("video".into());
    ui.set_name("clip-0412.mp4".into());
    ui.set_fa7_menu(common::strings(&["Details", "Open with…", "Show as hex…"]));
    s.shoot("_video_warmup");

    // portrait, the controls showing, 13 s into a 42 s clip (the board; the fixture is 4:3)
    let mut v = VideoPlayer::new((320, 240), 42_000_000);
    v.layout(ui.get_video_view_w() as u32, ui.get_video_view_h() as u32);
    let (_, _, w, h) = v.place();
    ui.set_video_frame(frame_at(&file, 1_000_000, w, h));
    ui.set_video_info(
        info(
            common::day(common::TODAY + 1) + (9 * 3600 + 58 * 60) * 1_000_000,
            (320, 240),
            9_800_000,
            "E:\\Videos\\",
        )
        .into(),
    );
    show_video(ui, &v, 13_000_000, 23_000_000, true);
    s.shoot("files2-view-video");

    // full screen: the shell's bars gone, the picture alone after the controls hid
    s.resize(2160);
    v.layout(ui.get_video_view_w() as u32, ui.get_video_view_h() as u32);
    v.touched(0);
    assert!(v.tick(3000, true));
    show_video(ui, &v, 14_000_000, 23_000_000, true);
    s.shoot("files2-view-video-fullscreen");

    // landscape: the window turned, the picture fills its height; the controls back
    s.size(2160, 1080);
    v.layout(ui.get_video_view_w() as u32, ui.get_video_view_h() as u32);
    assert_eq!((ui.get_video_view_w(), ui.get_video_view_h()), (2160, 1080));
    let (_, _, w, h) = v.place();
    ui.set_video_frame(frame_at(&file, 1_000_000, w, h));
    v.touched(5000);
    show_video(ui, &v, 14_000_000, 23_000_000, false);
    s.shoot("files2-view-video-landscape");

    // a clip in a codec Files names but doesn't play
    s.size(common::W, common::H);
    ui.set_name("old-phone.3gp".into());
    ui.set_video_frame(Image::default());
    ui.set_video_error("Can't play this video".into());
    ui.set_video_error_detail(
        "H.263 isn't supported. Open it with another app, or see its bytes.".into(),
    );
    s.shoot("files2-view-video-error");
    ui.set_video_error("".into());
    let _ = std::fs::remove_file(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../snapshots/_video_warmup.png"),
    );
}
