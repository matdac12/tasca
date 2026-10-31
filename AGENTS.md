# AGENTS.md

Rules for anyone (human or agent) changing this repo.

- **The spec is the source.** Traccia project TASCA, document `tasca-v1-spec.md`.
  Decisions live in closed TASCA-2..11; the tray, settings and shortcut rule in
  TASCA-24 and TASCA-26; the release pipeline in TASCA-23 and TASCA-27. If the
  spec and a ticket disagree, the later ticket wins. Don't re-decide what they
  settle.
- **Pure Rust, plain Win32/OLE, no webview.** `windows = "=0.62.2"` and
  `windows-core = "=0.62.2"` are exact pins: never bump them. Enable only the
  `windows` features the code uses. No new heavy dependency without a reason
  stated in the PR.
- **Toolchain:** `rust-toolchain.toml` pins Rust 1.99.1 so CI and local use the
  same compiler and lints; bump it only deliberately, fixing new lints in the same PR.
- **Never block the UI thread.** Cross-process drag-and-drop calls arrive on it.
  Slow work (file I/O, thumbnails, async data objects) goes to a worker thread.
- **Frozen interface** (`src/proto.rs`): window class `Tasca.Shelf`,
  `WM_APP+1` toggle, `+2` show, `+3` quit, mutex `Local\Tasca.Resident`.
  The helper path (`launch.rs`) does no COM, OLE or DPI work.
- **Layout:** `main.rs` (args → launch decision → helper or resident),
  `launch.rs` (pure `decide()` + the helper actions), `resident.rs` (mutex,
  window, wndproc, message loop), `shelf/` (window, layout, render, take out),
  `park/` (drop targets, materialized files, sweep), `thumbs.rs`, `hotkey.rs`
  (parsing, the safe-shortcut rule, `RegisterHotKey`), `settings.rs`
  (`settings.ini`, pure), `settings_win.rs` (the Settings window and key
  recorder), `tray.rs`, `startup.rs` (the `Tasca.lnk` shortcut), `log.rs`,
  `proto.rs`, `snippets.rs` (saved text). New features
  get their own module; wire them in through the hooks in `resident.rs`,
  and give every new `WM_APP+n` a line in `proto.rs`.
- **Re-entrancy:** release builds use `panic = "abort"`. Never hold a
  `RefCell` borrow across a call that can pump messages (menus, dialogs,
  `MessageBoxW`, COM); clone what you need and drop the borrow first.
- **Elevation:** an elevated launch must relaunch through `explorer.exe`, never
  become the resident (UIPI). `TASCA_TEST_ALLOW_ELEVATED=1` exists only in debug
  builds so integration tests can run on elevated CI runners. Release builds
  ignore it; keep it that way.
- **Tests:** keep decisions pure and unit-test them. Integration tests start a
  real resident, so they need `Tasca.Shelf` free (end `tasca-spike.exe` and any
  other `tasca.exe`) and they run one at a time.
- **Installed Tasca:** a hidden resident runs permanently. Before `cargo test`,
  quit it with `"%LOCALAPPDATA%\Tasca\tasca.exe" quit`; restart it afterwards
  with `scripts/install.ps1` or `tasca.exe --hidden`.

- **Releases:** push a `vX.Y.Z` tag equal to the `Cargo.toml` version, with
  `CHANGELOG.md` updated in the same commit; `release.yml` builds and
  publishes. Steps in the README's *Releasing* section. Never move or
  re-push a published tag; cut a new patch version instead.

## Checks (CI runs the same three on `windows-latest`)

```
cargo build --release
cargo clippy --all-targets -- -D warnings
cargo test
```
