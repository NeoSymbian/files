// SPDX-License-Identifier: GPL-3.0-only
//! The audio player's words (files2-view-audio): the times either side of the seek bar, "Artist · Album" and the
//! format line under the buttons.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// `1:12`, `4:05`, `1:02:03`.
pub fn time(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// The title: the tag's, else the file's name without its extension.
pub fn title(tag: &str, file: &str) -> String {
    if !tag.is_empty() {
        return tag.into();
    }
    match file.rfind('.') {
        Some(i) if i > 0 => file[..i].into(),
        _ => file.into(),
    }
}

/// "Artist · Album", either alone, or "Unknown artist".
pub fn sub(artist: &str, album: &str) -> String {
    match (artist.is_empty(), album.is_empty()) {
        (false, false) => format!("{artist} · {album}"),
        (false, true) => artist.into(),
        (true, false) => album.into(),
        (true, true) => "Unknown artist".into(),
    }
}

/// "MP3 · 320 kbps · 44.1 kHz · 4.1 MB · E:\Music" (the folder without its last `\`).
pub fn meta(format: &str, kbps: u32, rate: u32, bytes: u64, dir: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format.into());
    if kbps > 0 {
        parts.push(format!("{kbps} kbps"));
    }
    if rate > 0 {
        let k = rate / 100;
        parts.push(if k.is_multiple_of(10) {
            format!("{} kHz", k / 10)
        } else {
            format!("{}.{} kHz", k / 10, k % 10)
        });
    }
    parts.push(files_core::size_text(bytes));
    let d = dir.strip_suffix('\\').unwrap_or(dir);
    if !d.is_empty() {
        parts.push(d.into());
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words() {
        assert_eq!(time(72_400), "1:12");
        assert_eq!(time(245_000), "4:05");
        assert_eq!(time(3_723_000), "1:02:03");
        assert_eq!(time(0), "0:00");
        assert_eq!(title("", "Yerevan Nights.mp3"), "Yerevan Nights");
        assert_eq!(title("Live", "a.mp3"), "Live");
        assert_eq!(title("", ".hidden"), ".hidden");
        assert_eq!(
            sub("Lusine Trio", "Live at the Café"),
            "Lusine Trio · Live at the Café"
        );
        assert_eq!(sub("", ""), "Unknown artist");
        assert_eq!(
            meta("MP3", 320, 44_100, 4_300_000, "E:\\Music\\"),
            "MP3 · 320 kbps · 44.1 kHz · 4.1 MB · E:\\Music"
        );
        assert_eq!(meta("WAV", 0, 48_000, 100, ""), "WAV · 48 kHz · 100 B");
    }
}
