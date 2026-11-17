// SPDX-License-Identifier: GPL-3.0-only
//! Compiles the screens with the neoui kit (`@neoui`): glyphs and images baked in, SDF text.

fn main() {
    neoui_kit::compile("ui/files.slint");
}
