# Changelog

All notable changes to Tasca are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Snippets: text kept on purpose, separate from the History. A bookmark at
  the top-right of the History panel opens the list. New, Save, Delete and
  Copy. **Save as snippet** on a History row keeps that Clip's text (copied
  files as paths; an image has nothing to save). The same text again moves
  that snippet to the top.
- Snippets are stored in `%LOCALAPPDATA%\Tasca\snippets.txt`. A damaged file
  loads empty.

## [0.2.0] - 2026-10-30

### Added

- Clipboard History: everything you copy is recorded as a Clip. **Alt+V**
  opens a dark panel at the pointer, inside the monitor's work area, with
  Pinned Clips on top and the rest under Recent, newest first. It takes
  keyboard focus while open and closes on Alt+V, on Esc with an empty search,
  or on a click elsewhere.
- Type to search: every word must appear in the Clip, in any order, ignoring
  case. File Clips match by file name; image Clips never match.
- Paste back into the app you came from: Enter or click, Shift for plain text,
  Alt+1..9 or Ctrl+1..9 for the numbered Clip (also while Alt is still held
  from Alt+V). The Clip stays on the clipboard and moves to the top.
- Ctrl+P pins or unpins a Clip, Ctrl+Delete deletes one, **Clear** in the
  footer deletes every unpinned Clip, and a right-click menu offers Paste,
  Paste as plain text, Pin/Unpin and Delete.
- Text is recorded with its HTML and RTF formatting, images as PNG, and files
  copied in Explorer as their path list. Copying a Clip again moves it to the
  top and keeps its pin. A copy over 10 MB is skipped.
- The History survives Quit and reboots: the newest 100 unpinned Clips and
  every pin are saved under `%LOCALAPPDATA%\Tasca\history\`. A damaged store
  loads as an empty History.
- Copies that password managers mark as private are skipped by default.
- Settings: **Clipboard history** on or off, **History shortcut** (Alt+V by
  default, refused when it equals the Shelf shortcut) and **Skip private
  copies (password managers)**. New `settings.ini` keys `history`,
  `history_shortcut` and `skip_private`.
- **Show clipboard history** in the tray menu, which copies the chosen Clip
  instead of pasting. The tray tooltip lists both shortcuts.
- A new app icon, the Shelf at the edge of the screen, with a small variant
  for the tray.

### Fixed

- Pasting a Clip no longer records it again as a new Clip.
- A failed Settings apply puts the Shelf shortcut back as it was.
- A right-click selects its row before the menu opens.

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

[Unreleased]: https://github.com/matdac12/tasca/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/matdac12/tasca/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/matdac12/tasca/releases/tag/v0.1.0
