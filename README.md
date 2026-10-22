# Tasca

A Yoink-style file shelf for Windows: drop files on the Shelf, keep working, drag them out into any app.

![Tasca demo: summon the Shelf mid-drag, park files, drag them out again](docs/images/demo.gif)

## What it does

- **Summon it mid-drag.** Press **Ctrl+Alt+D** (Tasca's global shortcut, or
  `tasca.exe toggle` from your own tool) and a compact list slides out of the
  left edge of the monitor under the pointer — even while you are already
  dragging something.
- **Park files by dropping them on the Shelf.** Files and folders are kept by
  path. Browser images, links and text snippets are written to a temporary file
  first, so they can be dragged out like any other file. Several files dropped
  together become one **Stack** and are taken out as a unit.
- **Keep working.** The Shelf stays out of the way and does not take focus from
  the app you are working in (its right-click menu is the only exception); it
  hides when you toggle it away or take out the last Item. Parking the same
  files again just moves that Item back to the top.
- **Take them out into any app, folder or chat.** Tasca itself never moves,
  renames or deletes your files — the app you drop them on decides what happens.
- **Nothing to manage.** One resident process per user with a tray icon, a few
  MB and no database. Settings are one small text file you can edit by hand.
  Thumbnails come from the shell, with tiles for images, folders and file types
  as a fallback.
- **Materialized files are temporary.** Anything that was not a file to begin
  with lives in `%TEMP%\Tasca\<drop-id>\`, and drop folders older than 24 hours
  are swept at startup. Your real files are never touched. Tasca also keeps
  `%LOCALAPPDATA%\Tasca\settings.ini` (your settings) and `tasca.log` (a debug
  log); that is all it writes.

Pure Rust on plain Win32/OLE: no webview, no Electron, no runtime to install.

![The Shelf with a Stack of two PowerShell files (setup.ps1 +1) and CLAUDE.md](docs/images/shelf.png)

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

Tasca is not code-signed yet, so Windows SmartScreen may warn if you start a
downloaded `tasca.exe` directly: choose **More info** → **Run anyway**.
`install.ps1` removes that download mark from the copy it installs (with
`Unblock-File`); for a standalone `tasca.exe`, run `Unblock-File .\tasca.exe`
once yourself.

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

## Shortcut, tray and settings

Tasca lives in the notification area. Left-click the tray icon, or press the
global shortcut (**Ctrl+Alt+D** by default), to show or hide the Shelf.
Right-click the icon for **Show shelf**, **Settings…** and **Quit**.

**Settings** lets you:

- record a different global shortcut (Ctrl, Alt, Shift and Win are supported),
  or clear it to **None**;
- turn **Start with Windows** on or off — this creates or removes
  `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\Tasca.lnk`, the same
  shortcut `install.ps1` creates;
- see the installed version.

Settings are stored in `%LOCALAPPDATA%\Tasca\settings.ini` as plain
`key=value` text, for example `shortcut=Ctrl+Alt+D`. Delete the file to return
to the default. If another app already owns the combination, Tasca keeps
running and says so in the tray tooltip and the Settings window; pick another
combination or clear it. `tasca.exe toggle` still works, so Hyperkey,
AutoHotkey or PowerToys users can bind it themselves.

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
