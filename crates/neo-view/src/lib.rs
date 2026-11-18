// SPDX-License-Identifier: Apache-2.0
//! File types (docs `files-app.md`, decision F09; the canvas's `files2-spec-types` board): 55 extensions in families,
//! each family with its badge colour (the `ftile` icon) and its viewer, and the editor for the 24 editable types.
//! By extension only for now; probing the first bytes comes with the viewers (FA4 on).
#![no_std]

extern crate alloc;

use alloc::string::String;

/// A family: one badge colour and one viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Text,
    Code,
    Image,
    Audio,
    Video,
    Pdf,
    Word,
    Excel,
    Slides,
    Ebook,
    Archive,
    App,
    Contact,
    Font,
    Cert,
    Other,
}

impl Family {
    /// The badge colour, 0xRRGGBB (`files2_chrome.py` FAMILY).
    pub const fn colour(self) -> u32 {
        match self {
            Family::Text => 0x8a93a0,
            Family::Code => 0x14a3a3,
            Family::Image => 0x22b46a,
            Family::Audio => 0x8656e8,
            Family::Video => 0xf0642e,
            Family::Pdf => 0xe5484d,
            Family::Word => 0x2f80ed,
            Family::Excel => 0x1f9d55,
            Family::Slides => 0xf08c2e,
            Family::Ebook => 0xc0841a,
            Family::Archive => 0xa0703c,
            Family::App => 0x6b5cf0,
            Family::Contact => 0xe0457b,
            Family::Cert => 0xc9a227,
            Family::Font | Family::Other => 0x5b6472,
        }
    }

    /// What opens it (the type matrix's viewer column).
    /// What Details calls it (files2-details: "PDF document").
    pub const fn name(self) -> &'static str {
        match self {
            Family::Text => "Text file",
            Family::Code => "Code file",
            Family::Image => "Image",
            Family::Audio => "Audio",
            Family::Video => "Video",
            Family::Pdf => "PDF document",
            Family::Word => "Word document",
            Family::Excel => "Spreadsheet",
            Family::Slides => "Presentation",
            Family::Ebook => "E-book",
            Family::Archive => "Archive",
            Family::App => "App package",
            Family::Contact => "Contact or event",
            Family::Font => "Font",
            Family::Cert => "Certificate",
            Family::Other => "File",
        }
    }

    pub const fn viewer(self) -> &'static str {
        match self {
            Family::Text => "Text viewer",
            Family::Code => "Text viewer · syntax colours",
            Family::Image => "Image viewer",
            Family::Audio => "Audio player",
            Family::Video => "Video player",
            Family::Pdf => "PDF viewer",
            Family::Word => "Document viewer",
            Family::Excel => "Sheet viewer",
            Family::Slides => "Slides viewer",
            Family::Ebook => "Reader",
            Family::Archive => "Archive browser",
            Family::App => "Package info · Install",
            Family::Contact => "Contact / event card",
            Family::Font => "Font sample",
            Family::Cert => "Certificate viewer",
            Family::Other => "File info",
        }
    }
}

/// The editor a type opens in (F04, F11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Editor {
    None,
    Text,
    Markdown,
    Table,
    Image,
    Form,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileType {
    pub ext: &'static str,
    pub family: Family,
    pub editor: Editor,
    /// Shown as a type but only viewable once its decoder exists (FA13).
    pub needs_decoder: bool,
}

const fn t(ext: &'static str, family: Family, editor: Editor) -> FileType {
    FileType {
        ext,
        family,
        editor,
        needs_decoder: false,
    }
}

const fn d(ext: &'static str, family: Family) -> FileType {
    FileType {
        ext,
        family,
        editor: Editor::None,
        needs_decoder: true,
    }
}

use Editor as E;
use Family as F;

/// The 55 types, in the board's order.
pub const TYPES: [FileType; 55] = [
    t("txt", F::Text, E::Text),
    t("md", F::Text, E::Markdown),
    t("log", F::Text, E::Text),
    t("csv", F::Excel, E::Table),
    t("tsv", F::Excel, E::Table),
    t("json", F::Code, E::Text),
    t("xml", F::Code, E::Text),
    t("html", F::Code, E::Text),
    t("css", F::Code, E::Text),
    t("js", F::Code, E::Text),
    t("py", F::Code, E::Text),
    t("rs", F::Code, E::Text),
    t("c", F::Code, E::Text),
    t("cpp", F::Code, E::Text),
    t("h", F::Code, E::Text),
    t("ini", F::Code, E::Text),
    t("yaml", F::Code, E::Text),
    t("toml", F::Code, E::Text),
    t("sh", F::Code, E::Text),
    t("jpg", F::Image, E::Image),
    t("png", F::Image, E::Image),
    t("gif", F::Image, E::None),
    t("webp", F::Image, E::None),
    t("bmp", F::Image, E::None),
    t("svg", F::Image, E::Text),
    d("heic", F::Image),
    t("mp3", F::Audio, E::None),
    t("m4a", F::Audio, E::None),
    t("ogg", F::Audio, E::None),
    t("opus", F::Audio, E::None),
    t("wav", F::Audio, E::None),
    t("flac", F::Audio, E::None),
    t("amr", F::Audio, E::None),
    t("mid", F::Audio, E::None),
    t("mp4", F::Video, E::None),
    t("3gp", F::Video, E::None),
    d("mkv", F::Video),
    t("webm", F::Video, E::None),
    t("pdf", F::Pdf, E::None),
    t("epub", F::Ebook, E::None),
    t("docx", F::Word, E::None),
    t("xlsx", F::Excel, E::None),
    d("pptx", F::Slides),
    d("odt", F::Word),
    t("rtf", F::Word, E::None),
    t("zip", F::Archive, E::None),
    t("gz", F::Archive, E::None),
    d("7z", F::Archive),
    d("rar", F::Archive),
    t("vcf", F::Contact, E::Form),
    t("ics", F::Contact, E::Form),
    t("sis", F::App, E::None),
    t("jar", F::App, E::None),
    t("ttf", F::Font, E::None),
    t("pem", F::Cert, E::None),
];

/// Other spellings of a type (the board lists "sis / sisx", "ttf / otf"…).
const ALIASES: [(&str, &str); 12] = [
    ("jpeg", "jpg"),
    ("htm", "html"),
    ("yml", "yaml"),
    ("hpp", "h"),
    ("tar", "gz"),
    ("tgz", "gz"),
    ("sisx", "sis"),
    ("jad", "jar"),
    ("otf", "ttf"),
    ("cer", "pem"),
    ("crt", "pem"),
    ("midi", "mid"),
];

/// The extension of `name` without the dot ("" for none, or a name that starts with its only dot).
pub fn ext_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => "",
    }
}

/// The type of a file by its name's extension, any case.
pub fn type_of(name: &str) -> Option<&'static FileType> {
    let ext = ext_of(name);
    let ext = ALIASES
        .iter()
        .find(|(a, _)| a.eq_ignore_ascii_case(ext))
        .map_or(ext, |(_, to)| to);
    TYPES.iter().find(|t| t.ext.eq_ignore_ascii_case(ext))
}

pub fn family_of(name: &str) -> Family {
    type_of(name).map_or(Family::Other, |t| t.family)
}

/// The badge's letters: the file's own extension in capitals, at most 4 (an unknown one too: BIN, EXE, DAT);
/// "FILE" without one.
pub fn badge(name: &str) -> String {
    match ext_of(name) {
        "" => "FILE".into(),
        ext => ext.chars().take(4).flat_map(char::to_uppercase).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_board_counts() {
        assert_eq!(TYPES.len(), 55);
        assert_eq!(
            TYPES.iter().filter(|t| t.editor != Editor::None).count(),
            24
        );
        assert_eq!(TYPES.iter().filter(|t| t.needs_decoder).count(), 6);
        for (i, a) in TYPES.iter().enumerate() {
            assert!(
                TYPES[i + 1..].iter().all(|b| b.ext != a.ext),
                "{} twice",
                a.ext
            );
        }
        for (alias, to) in ALIASES {
            assert!(TYPES.iter().any(|t| t.ext == to), "{alias} -> {to}");
        }
    }

    #[test]
    fn names_to_types() {
        assert_eq!(family_of("Yerevan Nights.mp3"), Family::Audio);
        assert_eq!(family_of("IMG_2041.JPEG"), Family::Image);
        assert_eq!(type_of("notes.md").unwrap().editor, Editor::Markdown);
        assert_eq!(type_of("data.CSV").unwrap().family, Family::Excel);
        assert_eq!(family_of("backup.tar"), Family::Archive);
        assert_eq!(family_of("ekern.exe"), Family::Other);
        assert_eq!(family_of(".trash"), Family::Other);
        assert_eq!(family_of("README"), Family::Other);
        assert!(type_of("clip.mkv").unwrap().needs_decoder);
        assert_eq!(Family::Pdf.colour(), 0xe5484d);
    }

    #[test]
    fn badges() {
        assert_eq!(badge("song.mp3"), "MP3");
        assert_eq!(badge("a.jpeg"), "JPEG");
        assert_eq!(badge("x.backup"), "BACK");
        assert_eq!(badge("ekern.exe"), "EXE");
        assert_eq!(badge("README"), "FILE");
        assert_eq!(badge("файл.тхт"), "ТХТ");
    }
}
