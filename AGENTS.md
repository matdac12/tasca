# AGENTS.md

Rules for anyone (human or agent) changing this repo.

- **The spec is the source.** Traccia project TASCA, document `tasca-v1-spec.md`.
  Decisions live in closed TASCA-2..11; if the spec and a ticket disagree, the
  later ticket wins. Don't re-decide what they settle.
- **Pure Rust, plain Win32/OLE, no webview.** `windows = "=0.62.2"` and
  `windows-core = "=0.62.2"` are exact pins: never bump them. Enable only the
  `windows` features the code uses. No new heavy dependency without a reason
  stated in the PR.
- **Never block the UI thread.** Cross-process drag-and-drop calls arrive on it.
  Slow work (file I/O, thumbnails, async data objects) goes to a worker thread.
- **Frozen interface** (`src/proto.rs`): window class `Tasca.Shelf`,
  `WM_APP+1` toggle, `+2` show, `+3` quit, mutex `Local\Tasca.Resident`.
  The helper path (`launch.rs`) does no COM, OLE or DPI work.
- **Layout:** `main.rs` (args → launch decision → helper or resident),
  `launch.rs` (pure `decide()` + the helper actions), `resident.rs` (mutex,
  window, wndproc, message loop), `log.rs`, `proto.rs`. New features get their
  own module; wire them in through the hooks in `resident.rs`.
- **Elevation:** an elevated launch must relaunch through `explorer.exe`, never
  become the resident (UIPI). `TASCA_TEST_ALLOW_ELEVATED=1` exists only in debug
  builds so integration tests can run on elevated CI runners. Release builds
  ignore it; keep it that way.
- **Tests:** keep decisions pure and unit-test them. Integration tests start a
  real resident, so they need `Tasca.Shelf` free (end `tasca-spike.exe` and any
  other `tasca.exe`) and they run one at a time.

## Checks (CI runs the same three on `windows-latest`)

```
cargo build --release
cargo clippy --all-targets -- -D warnings
cargo test
```
