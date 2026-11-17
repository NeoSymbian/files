# Files

The file manager of [NeoSymbian](https://github.com/NeoSymbian): `NEOFILES.EXE`, a neoui app (its own process,
drawn with Slint and the neoui kit into the surface neoui's UI server gives it) on Symbian's file server (`e32::fs`).
Today it shows the drives with their free space, folders, and text files, and makes, renames and deletes; it came
from neoui (`symbian/apps/files` and `crates/neoui-files`, R8) at neoui `e6aa05e`. The plan for the full app — every
screen of the design canvas's Files page, viewers for 55 file types, editors, archives, search, a Recycle bin — is
NeoSymbian/docs `files-app.md`, phases FA0–FA13.

| Path | What | Licence |
|---|---|---|
| `crates/files-core` | The model: Symbian paths, folder ordering, row texts (sizes, dates), drive labels, text previews, name checks, error messages | Apache-2.0 |
| `crates/files-ui` | The screens in Slint (no_std), with the neoui kit imported as `@neoui/...` (built by `neoui_kit::compile`); host snapshot tests write `snapshots/*.png` | GPL-3.0 (links Slint) |
| `symbian/app` | `NEOFILES.EXE` (UID 0xE0006001): the event loop, the file server calls, the shell's dialogs and toasts | GPL-3.0 (links Slint) |

## Build

```sh
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test   # host; snapshots/*.png
cd symbian && cargo fmt --all --check && cargo arm   # target/armv5te-none-eabi/release/libneofiles.a
```

For a ROM, copy `symbian/exes.txt`'s line and `libneofiles.a` into NeoSymbian/build's `rust/`. The pins (e32, Slint,
neoui) must match neoui's `symbian/Cargo.toml`: one e32 and one Slint per ROM. neoui's UI server starts the app by its
key `files` (`host.rs` `APPS`), so the shell needs no change when this repo moves on.
