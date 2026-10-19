# How does elevated Hyperkey reach Tasca fastest, warm and cold? (TASCA-4)

Research only. No product code, and no timed measurements (shared-machine lock rule). All latencies
below are **estimates** with the reasoning shown. Measure them in the implementation turn.

## Answer in one paragraph

**Warm:** `Run "tasca.exe toggle"`. The elevated helper calls `FindWindowW` on Tasca's window class,
`PostMessageW`s a private toggle message, and exits. High to medium integrity is allowed by UIPI, and
`PostMessage` doesn't wait for Tasca. Estimate: **~15-45 ms** from Caps+D to the Shelf on screen.
AHK posting the message itself would be **~3-20 ms**, but `hyper.ahk` would then depend on Tasca's
window class and message number (Mattia decides).

**Cold:** copy Traccia's pattern: with no Tasca window and no resident mutex, the elevated helper
runs `%WINDIR%\explorer.exe <tasca.exe>` and exits. The shell starts Tasca unelevated, and a launch
with no arguments shows the Shelf. Estimate: **~200-500 ms**, but cold only happens after a Quit.
The documented, faster upgrade is `CreateProcessW` with `PROC_THREAD_ATTRIBUTE_PARENT_PROCESS` set to
the shell's process (**~70-170 ms**).

## 1. What UIPI allows (warm path)

- `PostMessageW`: "Starting with Windows Vista, message posting is subject to UIPI. The thread of a
  process can post messages only to message queues of threads in processes of **lesser or equal
  integrity level**." A blocked post fails with error 5.
  ([PostMessageW, Microsoft Learn](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-postmessagew))
  `SendMessage` and `WM_COPYDATA` follow the same rule.
- So an elevated (high IL) helper or AHK **can** post to an unelevated (medium IL) Tasca. Tasca needs
  no `ChangeWindowMessageFilterEx`: that call only lets *lower* integrity senders in.
- If the resident Tasca is ever accidentally elevated (for example started from an admin terminal),
  the helper still reaches it, because the IL is equal. But drops from normal apps would then fail.
  So **any** elevated launch that would become the resident should relaunch through the shell, not
  only `toggle`. That is stricter than Traccia's `first && toggle && elevated` rule, which is fine
  for Traccia, which takes no drops.

## 2. Warm path design

### Message choice

| Mechanism | Fit for "toggle" | Notes |
|---|---|---|
| **`PostMessageW(hwnd, WM_APP+n, 0, 0)`** | **Best** | Asynchronous: the helper exits at once. No pointers below `WM_USER` in async calls ([PostMessageW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-postmessagew)), and a toggle carries none. |
| `WM_COPYDATA` | Only when there is data | It must be *sent*, so the helper blocks until Tasca's window proc returns (use `SendMessageTimeoutW`). Keep it for a later `tasca.exe add <paths>`. This is what `tauri-plugin-single-instance` uses (ANNOTA-6). |
| Named pipe | No gain | Needs a listener thread or overlapped I/O folded into the message loop. Same speed, more code. |
| Named event + `MsgWaitForMultipleObjectsEx` | No gain | Works across IL (the resident creates it), but a window message is free, since Tasca already pumps messages. |
| Named mutex | Presence only | It can't carry a signal. Use it only as the "a resident is starting or running" guard (Traccia's `Local\` claim). |

`RegisterWindowMessageW("Tasca.Toggle")` is the textbook choice for a cross-app private message. A
fixed `WM_APP+1` sent to a window with a unique class is just as safe, because only Tasca's own class
gets it, and it's simpler for AHK if Mattia ever posts directly.

### Which window

- **Use a hidden top-level window with a unique class** (the Shelf window itself is fine: it is a
  top-level popup that stays hidden between shows). Don't use a message-only window: message-only
  windows "cannot be enumerated", and to find them you must pass `HWND_MESSAGE` to `FindWindowEx`
  ([Window Features: Message-Only Windows](https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features#message-only-windows)).
  The Rust helper could do that, but AHK's `WinExist`/`PostMessage` can't, so a message-only window
  closes the door on the direct-AHK option.
- The resident should create this window **first**, before Direct2D/DComp/OLE setup, so the window
  appears as early as possible in a cold start.

### Keeping the helper tiny

The helper is the same `tasca.exe`. The forwarding path, in order:
`args` → `FindWindowW(class, null)` → `PostMessageW` → `ExitProcess(0)`. No COM, no `OleInitialize`,
no DPI setup, no window creation. `#![windows_subsystem = "windows"]` (no console flash).

- **Imports cost even when unused:** the resident needs `ole32` (OLE drag and drop), `d2d1`,
  `dwrite` and `dcomp` (TASCA-7). Static imports are loaded and initialised at process start, on the
  warm path too. Mitigation: delay-load the heavy DLLs (`/DELAYLOAD:ole32.dll` and so on, plus
  `delayimp.lib`, which `lld-link` supports). Estimated saving: ~2-8 ms. Measure before bothering,
  because most of these are already mapped system-wide and load cheaply.
- No `AllowSetForegroundWindow` is needed: the Shelf shows without taking focus (TASCA-2,
  `SW_SHOWNOACTIVATE` / `WS_EX_NOACTIVATE`). Traccia and Annota call it only because their windows
  take focus.
- If Tasca is the **drag source** (an Item is being Taken out), its thread sits in `DoDragDrop`'s
  modal loop, which still dispatches posted messages. Ignore a toggle while a Take out is in progress,
  or a Caps+D press would hide the window the drag is coming from.

### Estimated latency, Caps+D to Shelf visible (warm)

| Step | Via `Run tasca.exe toggle` | AHK posts directly |
|---|---|---|
| AHK hook, hotkey thread | < 1 ms | < 1 ms |
| AHK `Run`: `CreateProcess` with an exact path, no shell ([AHK Run](https://www.autohotkey.com/docs/v2/lib/Run.htm): "If no verb is specified, Target is passed as-is to the lpCommandLine parameter of CreateProcess") | ~2-5 ms | n/a |
| Helper: loader, `user32` / win32k thread init, Rust runtime, `FindWindowW` + `PostMessageW` | ~8-20 ms (+2-8 ms if the heavy DLLs load eagerly) | n/a |
| `WinExist("ahk_class ...")` + `PostMessage` | n/a | ~0.2-1 ms |
| Tasca wakes, positions and shows a pre-rendered Shelf | ~1-3 ms | ~1-3 ms |
| DWM composes on the next vblank (60 Hz) | 0-17 ms | 0-17 ms |
| **Total** | **~15-45 ms** | **~3-20 ms** |

The first launch after a rebuild can take 100 ms or more, because Defender rescans the new image
(same caveat as ANNOTA-6). Both paths stay well under the ~100 ms where a response stops feeling
instant. The gap is about one or two frames.

## 3. Should AHK post directly? (trade-off)

```ahk
CapsLock & d:: {
    DetectHiddenWindows true            ; the Shelf is hidden between shows
    if hwnd := WinExist("ahk_class Tasca.Shelf")
        PostMessage 0x8001, 0, 0, , hwnd   ; WM_APP+1 = toggle
    else
        Run '"' EnvGet("LOCALAPPDATA") '\Tasca\tasca.exe" toggle'
    KeyWait "CapsLock", "T0.4"
}
```

- **For:** no process spawn on the hot path, so it's about 10-25 ms faster and immune to Defender
  rescans. `PostMessage` doesn't wait, and AHK throws if the window is missing. With an HWND,
  "hidden windows are always detected"
  ([AHK PostMessage](https://www.autohotkey.com/docs/v2/lib/PostMessage.htm)).
- **Against:** `hyper.ahk` would hard-code Tasca's class name and message number, so a Tasca change
  could silently break Caps+D. It is also six lines in Hyperkey rather than "the Caps+D line" the map
  allows. The same `tasca.exe toggle` still has to exist for the cold path.
- **Recommendation:** ship v1 with the one-line `Run` (Caps+R pattern). Treat the direct post as an
  optional upgrade **for Mattia to decide** once he has felt the `Run` path. Freeze the class name
  `Tasca.Shelf` and `WM_APP+1` from day one so the upgrade stays possible.

## 4. Cold path options (elevated helper, no Tasca running)

| Option | How | Est. cost to Shelf visible | Simplicity | Reliability and risks |
|---|---|---|---|---|
| **A. `explorer.exe <exe>`** (Traccia) | `Command::new(%WINDIR%\explorer.exe).arg(current_exe)`. The new explorer.exe hands the path to the running shell and exits, and the shell starts the exe with its own (medium) token. | ~200-500 ms (a heavyweight elevated `explorer.exe` starts, hands off and exits; then Tasca initialises) | **Simplest**: about 5 lines, proven on this machine by Caps+R (Traccia `apps/desktop/src-tauri/src/resident.rs`, `relaunch_through_shell`) | **Undocumented** behaviour. It can't pass arguments (fine: no arguments means "show"). Traccia's comment notes it fails when Explorer isn't running or "opened folder windows in a separate process and dropped the request". If no shell is running, an elevated `explorer.exe` may become an **elevated** shell. |
| **B. `IShellDispatch2::ShellExecute`** via the desktop folder view | `ShellWindows.FindWindowSW(CSIDL_DESKTOP)` → `IShellBrowser` → `IShellView` → `IShellFolderViewDual::get_Application` → `IShellDispatch2::ShellExecute` | ~100-250 ms (COM init plus cross-process calls into Explorer, ~10-40 ms, then Tasca initialises) | Medium: about 60 lines of COM, but only on the cold path | **Documented recipe** from Raymond Chen ([Old New Thing, 2013-11-18](https://devblogs.microsoft.com/oldnewthing/20131118-00/?p=2643)): "Since Explorer is running as the original unelevated user" the child runs unelevated. Firefox's NSIS `ExecInExplorer` and Chromium's `RunDeElevatedNoWait` use it ([Chromium elevation_util.cc](https://chromium.googlesource.com/chromium/src/+/a3dafb8/base/win/elevation_util.cc)). It passes arguments. It returns no PID. |
| **C. `CreateProcessW` + `PROC_THREAD_ATTRIBUTE_PARENT_PROCESS`** = shell process | `GetShellWindow` → `GetWindowThreadProcessId` → `OpenProcess(PROCESS_CREATE_PROCESS)` → attribute list → `CreateProcessW(EXTENDED_STARTUPINFO_PRESENT)` | **~70-170 ms** (one `CreateProcess`, ~5-15 ms, then Tasca initialises) | Medium: about 40 lines, `kernel32` + `user32` only, no COM | **Documented**: "Attributes inherited from the specified process include handles, the device map, processor affinity, priority, quotas, **the process token**, and job object" ([UpdateProcThreadAttribute](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute)). Raymond's "redux" recommends it for anything that can be a direct `CreateProcess` ([Old New Thing, 2019-04-25](https://devblogs.microsoft.com/oldnewthing/20190425-00/?p=102443)). It passes arguments and returns a PID. Caveats: the child gets the *elevated caller's* environment unless you pass one (reader comments in the redux post), and **EDR products flag parent-PID spoofing as suspicious**, which is a real concern on a corporate laptop. |
| D. `CreateProcessWithTokenW` with Explorer's token | `OpenProcessToken(explorer, TOKEN_DUPLICATE)` → `DuplicateTokenEx` → enable `SE_IMPERSONATE_NAME` → `CreateProcessWithTokenW` | ~80-200 ms | Medium-high: privilege juggling, environment and profile handling | Documented ([CreateProcessWithTokenW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-createprocesswithtokenw): caller "must have the SE_IMPERSONATE_NAME privilege"; with a NULL environment the child gets one "created from the profile"). Chromium's `RunDeElevated` uses it and first checks that Explorer is medium IL. Token manipulation is also an EDR signal. No advantage over C. |
| E. Task Scheduler | Register a task at install (`/RL LIMITED`, the user's interactive token), then run it on demand | ~150-400 ms (the service starts the task asynchronously) | Lowest: an install step, plus COM or `schtasks.exe` to trigger it | Reliable, but it adds install surface, and Mattia keeps choosing fewer moving parts. **Reject.** |

`GetShellWindow` "returns NULL if no Shell process is present"
([GetShellWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getshellwindow)).
Use it as the guard for every option: if there's no shell window, or the shell's process is not
medium IL (Chromium's check), don't relaunch. Fail quietly instead of risking an elevated Tasca or
an elevated shell. Raymond also notes that with UAC disabled, Explorer itself is elevated, so no
technique can produce an unelevated child
([Old New Thing, 2015-06-18](https://devblogs.microsoft.com/oldnewthing/20150618-00/?p=45351)).

### Cold-path recommendation

**Option A (Traccia's `explorer.exe <exe>`)**, because cold is rare (Tasca starts with Windows,
unelevated, from the Startup entry, so cold means "after Quit"), A is proven by Caps+R on this exact
machine and AHK, and it adds no COM and no EDR-flagged pattern. Add the `GetShellWindow` guard
above. **Upgrade path:** if a cold Caps+D ever feels slow, switch to C (fastest, documented, no COM).
If A ever breaks, switch to B (documented, the most "benign" for EDR). Keep the relaunch in one
function so the switch touches nothing else.

## 5. Proposed launch logic (for the spec, not code)

```
tasca.exe [toggle]
  hwnd = FindWindowW("Tasca.Shelf")
  if hwnd:                         # warm
      if arg == toggle: PostMessageW(hwnd, WM_APP+1); exit 0
      else (plain launch):        PostMessageW(hwnd, WM_APP+2 /*show*/); exit 0
  if OpenMutexW("Local\Tasca.Resident") succeeds:   # a resident is starting
      exit 0                       # it shows itself; drop the press (Traccia does the same)
  if elevated:                     # cold, from hyper.ahk
      guard: GetShellWindow() != NULL and the shell is medium IL, else exit 1
      relaunch through the shell (option A), no arguments; exit 0
  # cold, unelevated (Startup entry or a double-click): become the resident
  CreateMutexW("Local\Tasca.Resident"); create the Shelf window first; then D2D/OLE
  show the Shelf unless launched with --hidden (the Startup entry)
```

Unlike Traccia, the helper doesn't need to wait or poll for the relaunched instance: the new
instance shows the Shelf itself, so waiting adds nothing for the user.

## 6. Open risks

- **Cold Caps+D mid-drag from the Desktop or an Explorer window:** options A and B call into
  Explorer while the drag source (possibly an Explorer thread) is in `DoDragDrop`'s modal loop.
  Explorer runs folder windows on separate threads, and the modal loop pumps messages, so it
  probably works, but this is **unverified**. Option C doesn't touch Explorer's threads. Have Mattia
  check by hand in TASCA-8: Quit Tasca, start dragging from the Desktop, press Caps+D.
- **EDR on the corporate laptop:** if Defender for Endpoint (or similar) is present, option C's
  parent-PID spoofing, and to a lesser degree option A, may raise alerts. A is already in daily use
  through Traccia with no reported issue.
- **Defender rescans after a rebuild** add 100 ms or more to the first warm press. That's an
  annoyance in development, not a design issue.
- **Explorer restart window:** while Explorer restarts, `GetShellWindow` is NULL and a cold Caps+D
  is dropped by design.
- All numbers are estimates. **Measure in the implementation turn (under the machine lock):**
  QPC at the AHK hotkey entry vs QPC when Tasca handles `WM_APP+1`, p50/p95 over 30 presses, for
  `Run` and for the direct post. Also the helper's lifetime with eager vs delay-loaded DLLs, and the
  cold Caps+D to first frame with option A.

## Sources

- Microsoft Learn: [PostMessageW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-postmessagew),
  [Window Features: Message-Only Windows](https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features#message-only-windows),
  [UpdateProcThreadAttribute](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute),
  [CreateProcessWithTokenW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-createprocesswithtokenw),
  [GetShellWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getshellwindow).
- The Old New Thing (Raymond Chen):
  [How can I launch an unelevated process from my elevated process and vice versa? (2013)](https://devblogs.microsoft.com/oldnewthing/20131118-00/?p=2643),
  [... redux (2019, parent-process attribute)](https://devblogs.microsoft.com/oldnewthing/20190425-00/?p=102443),
  [UAC disabled (2015)](https://devblogs.microsoft.com/oldnewthing/20150618-00/?p=45351).
- Chromium: [base/win/elevation_util.cc](https://chromium.googlesource.com/chromium/src/+/a3dafb8/base/win/elevation_util.cc)
  (`RunDeElevated` = token + `CreateProcessWithTokenW`; `RunDeElevatedNoWait` = `IShellDispatch2`).
- AutoHotkey v2 docs: [Run](https://www.autohotkey.com/docs/v2/lib/Run.htm),
  [PostMessage](https://www.autohotkey.com/docs/v2/lib/PostMessage.htm).
- Local: `%USERPROFILE%\Scripts\hyper.ahk` (Caps+R block: `Run ... --toggle`, then
  `KeyWait "CapsLock", "T0.4"`); Traccia
  `apps/desktop/src-tauri/src/resident.rs` and `main.rs` (the `Local\` mutex claim, the
  `first && toggle && elevated` relaunch through `%WINDIR%\explorer.exe`, a 3 s / 50 ms poll for the
  new instance) and `docs/desktop-app.md` §8; Traccia ANNOTA-6 resolution (the Tauri single-instance
  plugin uses a hidden window plus `WM_COPYDATA`, a pre-Tauri fast path estimated at 15-35 ms).
