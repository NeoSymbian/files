// SPDX-License-Identifier: Apache-2.0
//! The hex view of any file, a row at a time (the UI asks for the rows it shows).

use alloc::string::String;
use core::fmt::Write;

/// Bytes a row, as on the design board.
pub const ROW: usize = 8;

/// `"00000000  7F 45 4C 46 01 01 01 00  .ELF...."`: the offset (8 hex digits, more past 4 GB), the first [`ROW`]
/// bytes of `bytes`, and their ASCII (`.` for anything unprintable). A short row is padded so the ASCII lines up.
pub fn row(offset: u64, bytes: &[u8]) -> String {
    let bytes = &bytes[..bytes.len().min(ROW)];
    let mut s = String::with_capacity(12 + ROW * 4);
    let _ = write!(s, "{offset:08X} ");
    for i in 0..ROW {
        match bytes.get(i) {
            Some(b) => {
                let _ = write!(s, " {b:02X}");
            }
            None => s.push_str("   "),
        }
    }
    s.push_str("  ");
    s.extend(bytes.iter().map(|&b| {
        if (0x20..0x7f).contains(&b) {
            b as char
        } else {
            '.'
        }
    }));
    s
}
