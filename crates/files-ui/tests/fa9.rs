// SPDX-License-Identifier: GPL-3.0-only
// The FA9 board (files2-view-audio) as `snapshots/files2-view-audio.png` with the board's sample track, and the
// player with a cover, paused, shuffle on and repeat one (`files2-view-audio-cover.png`). The cover is drawn from SVG.
mod common;
use common::*;
use files_ui::audio;
use files_ui::image::slint_image;
use neo_format_image::svg::render;

const FOOTER: &str = "Stops when you leave Files.";

fn track(ui: &files_ui::FilesApp, title: &str, sub: &str, at: u64, len: u64, meta: &str) {
    ui.set_view("audio".into());
    ui.set_fa7_menu(strings(&["Show as hex…", "Open with…", "Details"]));
    ui.set_fa7_footer(FOOTER.into());
    ui.set_audio_title(title.into());
    ui.set_audio_sub(sub.into());
    ui.set_audio_pos(at as f32 / len as f32);
    ui.set_audio_elapsed(audio::time(at).into());
    ui.set_audio_total(audio::time(len).into());
    ui.set_audio_meta(meta.into());
    ui.set_audio_has_prev(true);
    ui.set_audio_has_next(true);
}

#[test]
fn fa9_boards() {
    let s = app();
    let ui = &s.ui;

    // files2-view-audio: no cover art, playing at 1:12 of 4:05
    ui.set_name("Yerevan Nights.mp3".into());
    track(
        ui,
        &audio::title("", "Yerevan Nights.mp3"),
        &audio::sub("Lusine Trio", "Live at the Café"),
        72_000,
        245_000,
        &audio::meta("MP3", 320, 44_100, 4_300_000, "E:\\Music\\"),
    );
    ui.set_audio_playing(true);
    s.shoot("files2-view-audio");

    // a cover, paused, shuffle on, repeat this track
    let cover = render(
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="600" height="600" viewBox="0 0 100 100">
<rect width="100" height="100" fill="#1b2a4a"/><circle cx="50" cy="44" r="26" fill="#f2994a"/>
<rect y="70" width="100" height="30" fill="#0e1628"/></svg>"##,
        600,
    )
    .unwrap();
    ui.set_name("Dilijan.flac".into());
    track(
        ui,
        "Dilijan",
        &audio::sub("Arto Tunç", ""),
        200_000,
        3_723_000,
        &audio::meta("FLAC", 912, 48_000, 412_000_000, "C:\\Data\\Music\\"),
    );
    ui.set_audio_cover(slint_image(&cover));
    ui.set_audio_has_cover(true);
    ui.set_audio_playing(false);
    ui.set_audio_shuffle(true);
    ui.set_audio_repeat(2);
    s.shoot("files2-view-audio-cover");
}
