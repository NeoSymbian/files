// SPDX-License-Identifier: Apache-2.0
//! Text, code, Markdown and CSV for Files (docs files-app.md FA4): a byte-exact model ([`doc`]), editing with undo
//! ([`edit`]), find and replace ([`find`]), highlighting ([`lex`]), soft-wrap layout ([`layout`]), Markdown blocks
//! ([`md`]) and CSV/TSV tables ([`table`]).
#![no_std]

extern crate alloc;

pub mod doc;
pub mod edit;
pub mod find;
pub mod layout;
pub mod lex;
pub mod md;
pub mod table;

pub use doc::{Doc, Encoding, Line, LineEnd, NotText};
pub use edit::{Editor, Pos};
