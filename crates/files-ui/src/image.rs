// SPDX-License-Identifier: GPL-3.0-only
//! Pixels from neo-format-image as Slint images (FA5): the viewer's picture, the editor's preview, the thumbnails.

use neo_format_image::Pixels;
use slint::{Image, Rgb8Pixel, Rgba8Pixel, SharedPixelBuffer};

/// A copy of `p` as an image (RGB or RGBA).
pub fn slint_image(p: &Pixels) -> Image {
    if p.ch == 4 {
        Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            &p.px, p.w, p.h,
        ))
    } else {
        Image::from_rgb8(SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(
            &p.px, p.w, p.h,
        ))
    }
}

/// "4032 × 3024".
pub fn size_text(w: u32, h: u32) -> alloc::string::String {
    alloc::format!("{w} × {h}")
}
