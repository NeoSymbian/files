// SPDX-License-Identifier: Apache-2.0
use alloc::{format, string::String};

pub fn drive_of(path: &str) -> Option<char> {
    let bytes = path.as_bytes();
    (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && &bytes[1..3] == b":\\")
        .then(|| char::from(bytes[0].to_ascii_uppercase()))
}

pub fn parent(path: &str) -> Option<String> {
    if drive_of(path).is_none() || path.len() <= 3 {
        return None;
    }
    let trimmed = path.trim_end_matches('\\');
    let cut = trimmed.rfind('\\')?;
    Some(String::from(&path[..=cut]))
}

pub fn name_of(path: &str) -> &str {
    let trimmed = path.trim_end_matches('\\');
    trimmed.rsplit('\\').next().unwrap_or(trimmed)
}

pub fn join_dir(dir: &str, name: &str) -> String {
    format!(
        "{}\\{}\\",
        dir.trim_end_matches('\\'),
        name.trim_matches('\\')
    )
}

pub fn join_file(dir: &str, name: &str) -> String {
    format!("{}\\{}", dir.trim_end_matches('\\'), name)
}

pub fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
}

pub fn is_within(child: &str, folder: &str) -> bool {
    let child = child.trim_end_matches('\\');
    let folder = folder.trim_end_matches('\\');
    if eq_ignore_case(child, folder) {
        return true;
    }
    child
        .char_indices()
        .any(|(at, c)| c == '\\' && eq_ignore_case(&child[..at], folder))
}

pub fn split_ext(name: &str) -> (&str, &str) {
    if !name.starts_with('.')
        && let Some(at) = name.rfind('.')
    {
        return (&name[..at], &name[at..]);
    }
    (name, "")
}

pub fn keep_both_name(name: &str, n: u32) -> String {
    let (stem, ext) = split_ext(name);
    format!("{stem} ({n}){ext}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbian_paths_and_boundaries() {
        assert_eq!(drive_of("c:\\dir\\file"), Some('C'));
        assert_eq!(drive_of("bad"), None);
        assert_eq!(parent("C:\\dir\\file").as_deref(), Some("C:\\dir\\"));
        assert_eq!(parent("C:\\dir\\").as_deref(), Some("C:\\"));
        assert_eq!(parent("C:\\"), None);
        assert_eq!(name_of("C:\\dir\\"), "dir");
        assert_eq!(name_of("C:\\"), "C:");
        assert_eq!(join_dir("C:\\", "dir"), "C:\\dir\\");
        assert_eq!(join_file("C:\\dir\\", "file"), "C:\\dir\\file");
        assert!(is_within("c:\\DIR\\sub\\file", "C:\\dir\\"));
        assert!(is_within("C:\\dir\\", "c:\\DIR\\"));
        assert!(!is_within("C:\\directory\\file", "C:\\dir\\"));
        assert!(eq_ignore_case("Ä", "ä"));
    }

    #[test]
    fn extensions_and_duplicates() {
        assert_eq!(split_ext("song.mp3"), ("song", ".mp3"));
        assert_eq!(split_ext(".hidden.txt"), (".hidden.txt", ""));
        assert_eq!(split_ext("Photos"), ("Photos", ""));
        assert_eq!(keep_both_name("song.mp3", 2), "song (2).mp3");
        assert_eq!(keep_both_name("Photos", 1), "Photos (1)");
    }
}
