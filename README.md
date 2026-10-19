# Tasca

A Yoink-style file shelf for Windows. Press Caps+D (via Hyperkey), even in the
middle of a drag, and a compact list slides out of the left edge. Drop files to
park them, keep working, then drag them out into any app, folder or chat.

Lean Rust on plain Win32: no webview, a few MB resident. Separate from Parla and
Annota.

Planning lives in Traccia, project TASCA (map: "Map: Tasca v1").

## Usage

```
tasca.exe [toggle | quit | --hidden]
```

One resident per user. `toggle` (Caps+D) and no argument post to the running
resident, or start it if none is running; `quit` ends it; `--hidden` starts it
without showing anything (the Startup shortcut).
Debug builds only: `--seed <path>…` parks those paths at startup, last on top (`a|b|c` = one Stack).

## Build

Windows, Rust stable (MSVC toolchain).

```
cargo build --release     # target\release\tasca.exe
cargo clippy --all-targets -- -D warnings
cargo test
```

The integration tests start a real resident. End `tasca-spike.exe` and any
other running `tasca.exe` first, or they fail on purpose.

## Install

See TASCA-18 / `scripts/install.ps1` (coming).
