// SPDX-License-Identifier: GPL-3.0-only
//! The Files app's screens (Slint, no_std): the generated component [`FilesApp`], its rows ([`DriveRow`],
//! [`FileRow`]) and the kit's globals it re-exports (`Theme`, `Kit`, `Scroll`). NEOFILES.EXE (symbian/app) and the
//! host snapshot tests drive the same UI.
#![no_std]

extern crate alloc;

slint::include_modules!();
