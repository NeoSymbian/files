# Files

The file manager of [NeoSymbian](https://github.com/NeoSymbian): `NEOFILES.EXE`, a neoui app (its own process,
drawn with Slint and the neoui kit into the surface neoui's UI server gives it) on Symbian's file server (`e32::fs`).
It has the v2 browsing and file operations (FA1, FA2): the Browse tab with the drives, folders as a list or a grid
with a breadcrumb, sorting, hidden files, a read-only Z:, New folder / New file / Rename, copy and move through a paste
bar on a worker thread (a progress toast and a shade entry with Cancel, name clashes asked), and Delete into each
drive's Recycle bin (`X:\.trash\`) with Undo. It came from neoui (`symbian/apps/files`, `crates/neoui-files`, R8)
at neoui `e6aa05e`. The plan for the full app (every screen of the design canvas's Files page, viewers for 55 file
types, editors, archives, search) is NeoSymbian/docs `files-app.md`, phases FA0–FA13.

| Path | What | Licence |
|---|---|---|
| `crates/files-core` | The model: Symbian paths, sorting, row texts (sizes, dates), breadcrumbs, names, drive labels, text previews, error messages | Apache-2.0 |
| `crates/neo-view` | File types: the 55 extensions in their families (badge colour, viewer, editor) | Apache-2.0 |
| `crates/neo-fs` | The file-system contract (`Fs`, `File`, typed errors, paths) and `MemFs`, a fake with full disks, pulled drives and power cuts | Apache-2.0 |
| `crates/files-ops` | The job engine on `neo-fs`: copy and move in bounded steps (clashes, Cancel), the per-drive Recycle bin with its journal | Apache-2.0 |
| `crates/neo-format-text` | Text, code, Markdown and CSV (FA4): a byte-exact model, editing with undo, find, highlighting, layout | Apache-2.0 |
| `crates/neo-format-image` | Images (FA5): probe and EXIF, decoders (JPEG `zune-jpeg`, PNG `zune-png`, GIF ours over `weezl`, WebP NeoSymbian's no_std `image-webp`, BMP ours, an SVG subset ours on `tiny-skia`), shrink and turn, the editor's recipe, JPEG / PNG encoders | Apache-2.0 |
| `crates/files-ui` | The screens in Slint (no_std), with the neoui kit imported as `@neoui/...` (built by `neoui_kit::compile`); host snapshot tests write `snapshots/files2-*.png`, one per canvas board | GPL-3.0 (links Slint) |
| `symbian/fs-e32` | `neo-fs` over Symbian's file server (`e32::fs`) | GPL-3.0 |
| `symbian/app` | `NEOFILES.EXE` (UID 0xE0006001): the event loop, the worker, settings (`files.*`), the shell's dialogs, toasts and shade entry | GPL-3.0 (links Slint) |

The JPEG encoder (`jpeg-encoder`, in `neo-format-image`) is based in part on the work of the Independent JPEG Group.

## Build

```sh
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test   # host; snapshots/*.png
cd symbian && cargo fmt --all --check && cargo arm   # target/armv5te-none-eabi/release/libneofiles.a
```

For a ROM, copy `symbian/exes.txt`'s line and `libneofiles.a` into NeoSymbian/build's `rust/`. The pins (e32, Slint,
neoui) must match neoui's `symbian/Cargo.toml`: one e32 and one Slint per ROM. neoui's UI server starts the app by its
key `files` (`host.rs` `APPS`), so the shell needs no change when this repo moves on.
