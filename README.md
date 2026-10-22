# Tasca

A Yoink-style file shelf for Windows: drop files on the Shelf, keep working, drag them out into any app.

![The Tasca shelf with a few parked files](docs/images/preview.png)

## What it does

- **Summon it mid-drag.** Run `tasca.exe toggle` from a shortcut and a compact
  list slides out of the left edge of the monitor under the pointer — even
  while you are already dragging something.
- **Park files by dropping them on the Shelf.** Files and folders are kept by
  path. Browser images and links, text snippets and Outlook attachments are
  written to a temporary file first, so they can be dragged out like any other
  file. Several files dropped together become one **Stack** and are taken out
  as a unit.
- **Keep working.** The Shelf stays out of the way and does not take focus from
  the app you are working in (its right-click menu is the only exception); it
  hides when you toggle it away or take out the last Item. Parking the same
  files again just moves that Item back to the top.
- **Take them out into any app, folder or chat.** Tasca itself never moves,
  renames or deletes your files — the app you drop them on decides what happens.
- **Nothing to manage.** One resident process per user, a few MB, no tray icon
  and no settings. Thumbnails come from the shell, with tiles for images,
  folders and file types as a fallback.
- **Materialized files are temporary.** Anything that was not a file to begin
  with lives in `%TEMP%\Tasca\<drop-id>\`, and drop folders older than 24 hours
  are swept at startup. Your real files are never touched. The only other thing
  Tasca writes is its debug log, `%LOCALAPPDATA%\Tasca\tasca.log`.

Pure Rust on plain Win32/OLE: no webview, no Electron, no runtime to install.

## Download & install

Prebuilt builds are on the [Releases page](https://github.com/matdac12/tasca/releases).

1. Download `tasca-<version>-windows-x64.zip` and unzip it anywhere.
2. In the unzipped folder, run:

   ```
   powershell -ExecutionPolicy Bypass -File install.ps1
   ```

The script copies `tasca.exe` into `%LOCALAPPDATA%\Tasca\`, adds the Startup
shortcut `Tasca.lnk` (which runs `tasca.exe --hidden`) and starts Tasca hidden.
Rerun it to update; it quits a running Tasca first.

Each release also attaches `tasca.exe` and `SHA256SUMS.txt` on its own, if you
would rather place the exe yourself. Check a download with
`Get-FileHash tasca.exe -Algorithm SHA256` and compare it to `SHA256SUMS.txt`.

Needs Windows 10 or 11, x64. No admin rights: the binary and the shortcut live
in your own profile.

## Usage

```
tasca.exe [toggle | quit | --hidden]
```

| Argument   | What it does                                                     |
| ---------- | ---------------------------------------------------------------- |
| none       | Shows the Shelf, starting Tasca first if it is not running       |
| `toggle`   | Shows or hides the Shelf — bind this to your shortcut            |
| `quit`     | Ends the resident                                                |
| `--hidden` | Starts the resident without showing anything (Startup shortcut)  |

Only one resident runs per user; every invocation talks to it, so the extra
process exits immediately. Launch Tasca unelevated: an elevated launch relaunches
itself through `explorer.exe`, because Windows blocks drag-and-drop between
different integrity levels.

On the Shelf: double-click opens an Item, drag it out to hand it to another app,
click / Ctrl-click / Shift-click selects, the wheel scrolls a long list, and
right-click offers **Remove**, **Clear all** and **Quit**.

## Shortcut

A built-in shortcut (default `Ctrl+Alt+D`), tray icon and settings window are
coming in v0.1.0; meanwhile bind `tasca.exe toggle` in Hyperkey, AutoHotkey or
PowerToys Keyboard Manager.

## Build from source

Needs Windows 10 or 11, [Rust](https://rustup.rs) (the version is pinned in
`rust-toolchain.toml`) and the MSVC build tools (Visual Studio's "Desktop
development with C++" workload). Nothing else.

```
cargo build --release        # target\release\tasca.exe
cargo clippy --all-targets -- -D warnings
cargo test
```

The integration tests start a real resident, so end any running Tasca first
(`"%LOCALAPPDATA%\Tasca\tasca.exe" quit`) and run them one at a time.

To build and install in one go:

```
powershell -ExecutionPolicy Bypass -File scripts\install.ps1
```

When a `tasca.exe` sits next to the script — as in an unzipped release — the
script installs that file instead of building.

## Releasing

Releases are cut from `main` by pushing a tag; CI does the rest.

1. Bump `version` in `Cargo.toml`.
2. Move the `Unreleased` entries in `CHANGELOG.md` under the new version, with
   the date.
3. Commit, then tag and push:

   ```
   git tag v0.1.0
   git push origin v0.1.0
   ```

The tag must be `v` plus the exact `Cargo.toml` version.
`.github/workflows/release.yml` runs the same checks as CI, fails if the two
disagree, and publishes a GitHub Release with generated notes, `tasca.exe`,
`tasca-<version>-windows-x64.zip` and `SHA256SUMS.txt`.

## Development

`AGENTS.md` has the rules: module layout, the frozen window-class and message
interface in `src/proto.rs`, the "never block the UI thread" rule, elevation,
and the three checks CI runs.

## License

MIT — see [LICENSE](LICENSE). Copyright (c) 2026 Mattia Da Campo.
