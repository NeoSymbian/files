// SPDX-License-Identifier: GPL-3.0-only
// The FA5 boards (files2-view-image, -edit-image, -folder-grid with thumbnails, -type-images) as
// `snapshots/files2-<board>.png`. The photos are drawn here from SVG by neo-format-image (no camera files).
mod common;
use common::*;
use files_ui::image::slint_image;
use files_ui::{FileRow, ImgGroup, StripItem};
use neo_format_image::Pixels;
use neo_format_image::shrink::{Turn, thumb};
use neo_format_image::svg::render;

/// A landscape: sky (two bands), a sun, two hills, the ground.
fn photo(sky: u32, low: u32, sun: Option<(f32, f32)>, hill: u32, ground: u32) -> Pixels {
    let c = |v: u32| format!("#{v:06x}");
    let sun = sun.map_or(String::new(), |(x, y)| {
        format!(r##"<circle cx="{x}" cy="{y}" r="22" fill="#ffe1a0" opacity=".9"/>"##)
    });
    let s = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="4032" height="3024" viewBox="0 0 400 300">
<rect width="400" height="300" fill="{}"/><rect y="90" width="400" height="120" fill="{}" opacity=".75"/>{sun}
<path d="M-40 230 Q80 150 200 210 T440 200 V300 H-40 Z" fill="{}"/>
<path d="M140 240 Q260 170 440 230 V300 H140 Z" fill="{}" opacity=".85"/>
<rect y="250" width="400" height="50" fill="{}"/></svg>"##,
        c(sky),
        c(low),
        c(hill),
        c(hill),
        c(ground)
    );
    render(s.as_bytes(), 1024).unwrap()
}

fn sunset() -> Pixels {
    photo(0xf6c46a, 0xd2607a, Some((236.0, 130.0)), 0x4a2466, 0x1b0d2e)
}

fn others() -> Vec<Pixels> {
    vec![
        photo(0x3aa0d8, 0x8fd0f0, Some((80.0, 60.0)), 0x0b3c5d, 0x062436),
        photo(0x9fd88a, 0x2f9e44, None, 0x0b3d1e, 0x082a14),
        photo(0x6c7a89, 0xaab4c0, None, 0x1d2531, 0x11161d),
        photo(0xe88fb0, 0xf4c6d6, Some((300.0, 80.0)), 0x5a1d3a, 0x2e0e1e),
        photo(0xd8c46a, 0xf0e2a0, Some((60.0, 100.0)), 0x4a3b0b, 0x2a2106),
        photo(0x9ab0c8, 0xd0dce8, None, 0x22303f, 0x141c25),
        photo(0xf07a5a, 0xf6b49a, Some((200.0, 120.0)), 0x3a1206, 0x1e0903),
    ]
}

fn thumb_row(name: &str, p: &Pixels, short: &str) -> FileRow {
    FileRow {
        name: name.into(),
        short: short.into(),
        detail: short.into(),
        badge: "JPG".into(),
        colour: files_ui::colour(0x22b46a),
        thumb: slint_image(&thumb(p, Turn::NONE, 192)),
        has_thumb: true,
        ..FileRow::default()
    }
}

#[test]
fn fa5_boards() {
    let s = app();
    let ui = &s.ui;
    let sun = sunset();
    let rest = others();

    // files2-view-image: sunset.jpg, the third of 1,038, the strip around it
    ui.set_view("image".into());
    ui.set_name("sunset.jpg".into());
    ui.set_img(slint_image(&sun));
    ui.set_img_counter("3 / 1,038".into());
    ui.set_img_info("29 Oct 2026 · 4032 × 3024 · 3.1 MB".into());
    ui.set_img_hint(true);
    ui.set_img_editable(true);
    let strip: Vec<StripItem> = [
        &rest[0], &rest[1], &sun, &rest[6], &rest[2], &rest[0], &rest[1],
    ]
    .iter()
    .map(|p| StripItem {
        img: slint_image(&thumb(p, Turn::NONE, 96)),
        has: true,
    })
    .collect();
    ui.set_img_strip(model_of(strip));
    ui.set_img_current(2);
    s.shoot("files2-view-image");

    // files2-edit-image: Crop, Free, a box 2400 × 1690 of the 4032 × 3024
    ui.set_view("image-edit".into());
    ui.set_img_cropping(true);
    ui.set_img_can_undo(true);
    s.pixels();
    let (bw, bh) = (ui.get_img_box_w(), ui.get_img_box_h());
    assert!(bw > 0.0 && bh > 0.0);
    ui.set_crop_x(bw * 0.1);
    ui.set_crop_y(bh * 0.1);
    ui.set_crop_w(bw * 2400.0 / 4032.0);
    ui.set_crop_h(bh * 1690.0 / 3024.0);
    ui.set_crop_label("2400 × 1690".into());
    ui.set_crop_ratio(0);
    ui.set_img_save_note("Saves sunset-edit.jpg;\nthe original stays.".into());
    s.shoot("files2-edit-image");

    // files2-folder-grid with thumbnails: E:\Pictures
    ui.set_view("folder".into());
    ui.set_name("Pictures".into());
    ui.set_crumbs(strings(&["E:", "Pictures"]));
    ui.set_grid(true);
    let names = [
        "sunset.jpg",
        "sea.jpg",
        "forest.jpg",
        "rain.jpg",
        "blossom.jpg",
        "wheat.jpg",
        "mist.jpg",
        "dusk.jpg",
    ];
    let sizes = [
        "3.1 MB", "2.8 MB", "3.4 MB", "2.2 MB", "2.9 MB", "3.0 MB", "1.9 MB", "2.6 MB",
    ];
    let mut rows: Vec<FileRow> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let p = if i == 0 { &sun } else { &rest[i - 1] };
            thumb_row(n, p, sizes[i])
        })
        .collect();
    // one still waiting for the worker: the type tile
    rows.push(FileRow {
        has_thumb: false,
        ..thumb_row("new.png", &sun, "640 KB")
    });
    rows.last_mut().unwrap().badge = "PNG".into();
    ui.set_rows(model_of(rows.clone()));
    s.shoot("files2-folder-grid-thumbs");

    // the same as a list
    ui.set_grid(false);
    s.shoot("files2-folder-list-thumbs");

    // files2-type-images: by folder
    ui.set_view("type".into());
    ui.set_name("Images".into());
    ui.set_rows(model_of(vec![]));
    ui.set_type_drives(strings(&[
        "All drives",
        "E: Mass memory",
        "C: Phone memory",
    ]));
    ui.set_type_drive(0);
    let group = |title: &str, count: &str, more: &str, n: usize, from: usize| ImgGroup {
        title: title.into(),
        count: count.into(),
        more: more.into(),
        thumbs: model_of((0..n).map(|k| rows[(from + k) % 8].clone()).collect()),
    };
    ui.set_img_groups(model_of(vec![
        group("E:\\Images\\Camera", "812", "+804", 8, 0),
        group("E:\\Images\\Screenshots", "164", "+160", 4, 3),
        group("E:\\Downloads", "6", "+2", 4, 5),
        group("C:\\Data\\Messages", "23", "+19", 4, 1),
    ]));
    s.shoot("files2-type-images");
}
