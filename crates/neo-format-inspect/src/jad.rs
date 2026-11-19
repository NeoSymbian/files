// SPDX-License-Identifier: Apache-2.0
//! App package info for Java ME: a .jad descriptor, or the manifest (META-INF/MANIFEST.MF) of a .jar, read through
//! neo-format-archive's zip reader. Both are "Key: Value" lines; a line starting with a space continues the one
//! before (the manifest wraps at 72 bytes). Files only shows them: there is no Java runtime.

use crate::{Error, Result};
use alloc::string::String;
use alloc::vec::Vec;
use neo_format_archive::{Kind, Stream, zip};

/// A manifest larger than this is not one.
const MAX_MANIFEST: u64 = 256 << 10;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Midlet {
    pub name: String,
    pub vendor: String,
    pub version: String,
    /// MicroEdition-Profile ("MIDP-2.0").
    pub profile: String,
    /// MIDlet-Permissions, then MIDlet-Permissions-Opt.
    pub permissions: Vec<String>,
    /// MIDlet-Jar-Size (a .jad's).
    pub jar_size: Option<u64>,
}

pub fn parse(text: &str) -> Result<Midlet> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut pairs: Vec<(&str, String)> = Vec::new();
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(more) = line.strip_prefix(' ') {
            if let Some((_, v)) = pairs.last_mut() {
                v.push_str(more);
            }
        } else if let Some((k, v)) = line.split_once(':') {
            pairs.push((k.trim(), String::from(v.trim_start())));
        }
    }
    let get = |key: &str| {
        pairs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| String::from(v.trim()))
    };
    let mut permissions = Vec::new();
    for key in ["MIDlet-Permissions", "MIDlet-Permissions-Opt"] {
        if let Some(v) = get(key) {
            permissions.extend(
                v.split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(String::from),
            );
        }
    }
    Ok(Midlet {
        name: get("MIDlet-Name")
            .filter(|n| !n.is_empty())
            .ok_or(Error::Bad("Not a Java app: it has no name"))?,
        vendor: get("MIDlet-Vendor").unwrap_or_default(),
        version: get("MIDlet-Version").unwrap_or_default(),
        profile: get("MicroEdition-Profile").unwrap_or_default(),
        permissions,
        jar_size: get("MIDlet-Jar-Size").and_then(|s| s.parse().ok()),
    })
}

/// The manifest of a .jar.
pub fn from_jar(bytes: &[u8]) -> Result<Midlet> {
    let bad = |e| match e {
        neo_format_archive::Error::NotArchive => Error::Bad("Not a Java app"),
        _ => Error::Bad("The package is damaged"),
    };
    let members = zip::list(bytes).map_err(bad)?;
    let m = members
        .iter()
        .find(|m| m.kind == Kind::File && m.name.eq_ignore_ascii_case("META-INF/MANIFEST.MF"))
        .ok_or(Error::Bad("Not a Java app: it has no manifest"))?;
    if m.size > MAX_MANIFEST {
        return Err(Error::Bad("The package is damaged"));
    }
    let mut r = zip::open(bytes, m).map_err(bad)?;
    let mut text = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = r.read(&mut buf).map_err(bad)?;
        if n == 0 {
            break;
        }
        text.extend_from_slice(&buf[..n]);
    }
    parse(&String::from_utf8_lossy(&text))
}
