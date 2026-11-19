// SPDX-License-Identifier: Apache-2.0
//! The one place that decides whether a member name may be written (zip-slip): every list goes through [`safe`].

use alloc::string::String;

/// Longest safe name, in characters (a Symbian path is at most 256 with the drive and folder in front).
pub const MAX: usize = 255;

/// `raw` as a relative path that stays inside the folder it is extracted to: its parts joined with `/`, or `None`.
///
/// - `/` and `\` both separate parts; empty and `.` parts go;
/// - **rejected:** a `..` part, an absolute path (`/x`, `\x`), a drive (`C:`), a control character, a part that is
///   only dots or spaces, a name over [`MAX`] characters, and nothing left;
/// - FAT's forbidden characters `: * ? " < > |` become `_`, and trailing dots and spaces go (FAT drops them, and two
///   names would become one).
pub fn safe(raw: &str) -> Option<String> {
    if raw.starts_with(['/', '\\']) || raw.chars().any(|c| c.is_control()) {
        return None;
    }
    let mut out = String::new();
    for (i, part) in raw.split(['/', '\\']).enumerate() {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None;
        }
        // a drive: "C:" first
        if i == 0
            && part.len() == 2
            && part.as_bytes()[0].is_ascii_alphabetic()
            && part.ends_with(':')
        {
            return None;
        }
        let part = part.trim_end_matches(['.', ' ']);
        if part.is_empty() {
            return None;
        }
        if !out.is_empty() {
            out.push('/');
        }
        out.extend(part.chars().map(|c| match c {
            ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        }));
    }
    (!out.is_empty() && out.chars().count() <= MAX).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slips_are_rejected() {
        for bad in [
            "../x",
            "a/../../x",
            "a/..",
            "/etc/passwd",
            "\\x",
            "C:\\x",
            "c:/x",
            "a\\..\\x",
            "a\0b",
            "a\nb",
            "...",
            "a/ . /b",
            "",
            "./",
        ] {
            assert_eq!(safe(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(safe("a/b/c.txt").as_deref(), Some("a/b/c.txt"));
        assert_eq!(safe("a\\b\\").as_deref(), Some("a/b"));
        assert_eq!(safe("./a//b/./c").as_deref(), Some("a/b/c"));
        assert_eq!(safe("what?.txt").as_deref(), Some("what_.txt"));
        assert_eq!(safe("a:b").as_deref(), Some("a_b"));
        assert_eq!(safe("notes. ").as_deref(), Some("notes"));
        assert_eq!(
            safe("Ծրագիր/Бюджет.xlsx").as_deref(),
            Some("Ծրագիր/Бюджет.xlsx")
        );
        assert_eq!(safe(&"x".repeat(MAX)).map(|s| s.len()), Some(MAX));
        assert_eq!(safe(&"x".repeat(MAX + 1)), None);
    }
}
