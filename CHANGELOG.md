# Changelog

All notable changes to Tasca are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-10-22

### Added

- The Shelf: a compact list that slides out of the left edge of the monitor
  under the pointer and does not take focus, per-monitor DPI aware, with slide
  and fade motion.
- Park: drop files and folders on the Shelf; several files dropped together
  become one Stack, taken out as a unit. Parking an Item's files again moves it
  back to the top instead of duplicating it.
- Content without a file of its own becomes a real file: browser images, links
  and plain text are written to `%TEMP%\Tasca\<drop-id>\`; drop folders older
  than 24 hours are swept at startup.
- Take out: drag Items into any app, folder or chat, with success detected from
  the drop effect, the logical performed effect or a moved source file. Tasca
  never moves, renames or deletes a file itself.
- Shell thumbnails, with tiles for images, folders and file types until they
  arrive or when they fail.
- Selection (click, Ctrl-click, Shift-click), double-click to open, wheel
  scrolling for long lists, and a right-click menu with Remove, Clear all and
  Quit. Items whose files are gone show as Missing and cannot be taken out.
- One resident per user, started hidden from the Startup-folder shortcut
  `Tasca.lnk`. Elevated launches relaunch through `explorer.exe` so the Shelf
  can still accept drops.
- Tray icon in the notification area: left-click to show or hide the Shelf,
  right-click for **Show shelf**, **Settings…** and **Quit**. It is re-added
  when Explorer restarts.
- A global shortcut to show or hide the Shelf, **Ctrl+Alt+D** by default, with
  a recorder in Settings for any combination that includes Ctrl, Alt or Win
  (Shift alone is not enough) or an F13–F24 key on its own, or **None**.
- Settings window: record or clear the global shortcut, turn **Start with
  Windows** on or off, and see the installed version.
- Settings persist in `%LOCALAPPDATA%\Tasca\settings.ini` as plain `key=value`
  text (`shortcut=Ctrl+Alt+D`); deleting the file returns to the default. If
  another app already owns the combination, Tasca keeps running and the tray
  tooltip and the Settings window say so.
- **Start with Windows** creates or removes the same Startup-folder shortcut
  `Tasca.lnk` that `scripts/install.ps1` writes.
- An app icon for the exe, the tray and the Settings window.
- `tasca.exe [toggle | quit | --hidden]` command line for shortcuts and scripts.
- Prebuilt releases: a GitHub Release workflow that runs the CI checks, checks
  the tag against the `Cargo.toml` version and uploads `tasca.exe`, a
  `tasca-<version>-windows-x64.zip` bundle and `SHA256SUMS.txt`.
- `scripts/install.ps1` installs an unzipped release without building when a
  `tasca.exe` sits next to it, and still builds from source otherwise.

[Unreleased]: https://github.com/matdac12/tasca/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/matdac12/tasca/releases/tag/v0.1.0
