# Can a window that appears mid-drag catch the drop? (TASCA-3)

Research note for Tasca v1. Throwaway branch `research/catch-drag`, never merged.
Question: if the Shelf is hidden when a drag starts and Caps+D shows it while the
drag is still in progress, does the Shelf get `IDropTarget::DragEnter` and `Drop`?

## Answer

**Yes.** OLE drag and drop has no list of targets fixed at the start of the drag.
The source's `DoDragDrop` loop looks up the window under the cursor again on every
step, so a window that shows up later is found as soon as the cursor is over it.
It must be visible, enabled, registered with `RegisterDragDrop`, owned by an
unelevated process, and opaque to hit-testing where the cursor is. Nothing about
"appeared mid-drag" is special. The user always has to move the cursor to the
left edge anyway, so the timing of the first `DragEnter` does not matter.

The real risks lie elsewhere. The first is integrity level: an elevated Shelf
silently refuses drops. The second is hit-testing on a layered window. The third
is the cold-start delay (TASCA-4).

## Findings

### 1. DoDragDrop picks the target from the current cursor position on every step

- Microsoft's documentation describes the loop this way: "The DoDragDrop function
  determines the window under the current cursor location. It then checks to see if
  this window is a valid drop target ... DoDragDrop tracks mouse cursor movements
  and changes in the keyboard or mouse button state ... If the mouse enters another
  window, DoDragDrop determines if that window is a valid drop target and then calls
  IDropTarget::DragEnter for that window."
  [DoDragDrop, Microsoft Learn](https://learn.microsoft.com/en-us/windows/win32/api/ole2/nf-ole2-dodragdrop)
- Wine's reimplementation of `DoDragDrop` shows what this means in practice.
  Each step calls `WindowFromPoint(pt)`, and when the HWND changes it walks up to a
  window with a registered drop target and calls `DragEnter`. The loop runs on
  `WM_MOUSEMOVE`, on button changes and on a **50 ms `WM_TIMER`**, so it also runs
  when the mouse is still.
  [wine `dlls/ole32/ole2.c`: `OLEDD_DragTrackerWindowProc`, `OLEDD_TrackStateChange`, `drag_enter`](https://github.com/wine-mirror/wine/blob/master/dlls/ole32/ole2.c)
- Windows behaves the same way. On Windows, drag-over events keep arriving every
  ~60 ms while the mouse is idle
  ([WebKit bug 101487](https://bugs.webkit.org/show_bug.cgi?id=101487)), and
  "the win32 ... widget libraries send NS_DRAGDROP_OVER repeatedly even when the
  mouse isn't moving"
  ([Mozilla bug 342229](https://bugzilla.mozilla.org/show_bug.cgi?id=342229)).
  The exact interval in Windows' own ole32 is undocumented. What matters is that it
  polls.
- `WindowFromPoint` "does not retrieve a handle to a hidden or disabled window"
  ([WindowFromPoint](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-windowfrompoint)).
  A Shelf that is created hidden at startup and registered is therefore never hit
  until it is shown, and is hit right after. Registering while hidden is safe.

**Sources that use shell or Chromium helpers.** Explorer, Chromium, Electron
(Claude desktop) and WebView2 apps (new Outlook `olk.exe`, current WhatsApp desktop)
all end up in OLE `DoDragDrop`. Chromium calls `::DoDragDrop` directly
([`desktop_drag_drop_client_win.cc`](https://github.com/chromium/chromium/blob/main/ui/views/widget/desktop_aura/desktop_drag_drop_client_win.cc)).
It sets its drag image through the shell's `IDragSourceHelper::InitializeFromBitmap`
([`os_exchange_data_provider_win.cc`](https://github.com/chromium/chromium/blob/main/ui/base/dragdrop/os_exchange_data_provider_win.cc)).
That means the same loop and the same drag-image manager. I found no source that
treats targets differently depending on when they appeared. *Not verified:* the
WhatsApp and Claude desktop internals beyond "Chromium-based". The hand test covers
them.

### 2. Window styles

| Style | Effect on being a drop target | Source |
|---|---|---|
| `WS_EX_TOPMOST` | Fine and recommended: puts the Shelf above the window it overlaps, so `WindowFromPoint` returns it. | [Extended Window Styles](https://learn.microsoft.com/en-us/windows/win32/winmsg/extended-window-styles) |
| `WS_EX_NOACTIVATE` | Fine. It only controls activation and foreground, not hit-testing. A drop never activates the target anyway. | same |
| `WS_EX_TOOLWINDOW` | Fine. It only hides the window from the taskbar and Alt+Tab. | same |
| `WS_EX_LAYERED`, per-pixel alpha | **Careful.** "Hit testing of a layered window is based on the shape and transparency of the window ... areas ... whose alpha value is zero will let the mouse messages through." Any pixel with alpha 0 cannot be a drop target; the drag falls through to the window behind. | [Window Features: Layered Windows](https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features) |
| `WS_EX_LAYERED` + `WS_EX_TRANSPARENT` | **Never.** "The shape of the layered window will be ignored and the mouse events will be passed to other windows", so the whole Shelf becomes see-through to drags. Don't use it even briefly during the slide-in animation. | same |
| DWM rounded corners (`DWMWA_WINDOW_CORNER_PREFERENCE`) | They don't affect hit-testing. But windows that use "per-pixel alpha layering" or "window regions" "cannot ever be rounded", so a layered Shelf has to draw its own 12 px corners. Alpha-0 corner pixels then fall through, which is harmless (a few pixels). | [Apply rounded corners](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/ui/apply-rounded-corners) |
| Disabled window (`EnableWindow(false)`) | Never hit: `WindowFromPoint` skips disabled windows. | [WindowFromPoint](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-windowfrompoint) |

For TASCA-7: mockup C's glass (`rgba(18,18,20,.92)`) is ~92 % alpha, which is
fine. Every pixel inside the rounded rectangle, including padding and gaps between
rows, must keep alpha > 0. A DirectComposition or `WS_EX_NOREDIRECTIONBITMAP`
window avoids layered hit-testing entirely: it hit-tests as a rectangle.

### 3. Threading and lifecycle

- Call `OleInitialize`, not `CoInitializeEx`: "If you use CoInitialize or
  CoInitializeEx instead of OleInitialize ... RegisterDragDrop will always return an
  E_OUTOFMEMORY error."
  [RegisterDragDrop](https://learn.microsoft.com/en-us/windows/win32/api/ole2/nf-ole2-registerdragdrop)
- The registering thread "must be pumping messages ... If this requirement isn't
  met, then any application that drags an object over the window ... will hang until
  the target application closes." (same page.) Cross-process `IDropTarget` calls
  arrive as COM calls into Tasca's STA. Never block the UI thread: a stuck Tasca
  freezes the source app's drag.
- The docs say to register "when the window appears unobscured", but registration
  is only a property on the HWND, and hidden windows are never returned by
  `WindowFromPoint`. Chromium and Wine both register once at creation. Registering
  once at startup on the hidden Shelf is fine, and keeps `RegisterDragDrop` off the
  Caps+D path.
- Keep `IDropTarget::Drop` short. The source is blocked inside `DoDragDrop` until
  `Drop` returns. Slow extraction (e.g. Outlook attachments, see TASCA-5) belongs on
  a worker thread, using `IDataObjectAsyncCapability` when the source offers it
  ([IDataObjectAsyncCapability](https://learn.microsoft.com/en-us/windows/win32/api/shldisp/nn-shldisp-idataobjectasynccapability)).
  Hide the Shelf with a posted message after `Drop` returns, not inside it.

### 4. Integrity level: the elevated Hyperkey trap

- "UIPI blocks Windows messages being sent from process with a lower MIC level to
  one running at a higher MIC level. Drag-and-drop is implemented via Windows
  messages ... drag-and-drop from Windows Explorer (medium MIC) to Notepad running
  elevated (high MIC) ... doesn't work." `ChangeWindowMessageFilterEx` "isn't
  recommended as a safe solution for drag and drop".
  [Microsoft archive blog, Patrick Arnold](https://learn.microsoft.com/en-us/archive/blogs/patricka/q-why-doesnt-drag-and-drop-work-when-my-application-is-running-elevated-a-mandatory-integrity-control-and-uipi)
- The symptom is the nasty part: "the pointer icon changes but the file doesn't open"
  (same post). An elevated Shelf would look alive and swallow nothing.
- `hyper.ahk` runs elevated, so a `tasca.exe` it starts is elevated. **The window
  that owns the drop target must belong to an unelevated (medium IL) process.** The
  elevated `tasca.exe toggle` may only signal the resident unelevated Tasca (cold:
  relaunch unelevated, as in TASCA-2). It must never create the Shelf itself. A
  signal from high IL to medium IL is allowed (UIPI only blocks low to high).
  Choosing the transport is TASCA-4's job.

### 5. Caps+D while the left button is held in a drag

- AutoHotkey: "Custom combinations involving keyboard keys always use the keyboard
  hook", and CapsLock as a prefix keeps its toggle function only on its own.
  [AutoHotkey v2 Hotkeys](https://www.autohotkey.com/docs/v2/Hotkeys.htm)
- The `WH_KEYBOARD_LL` hook "is called in the context of the thread that installed
  it" for "every ... keyboard input event ... about to be posted into a thread input
  queue". So it runs in AHK's own thread, whatever the foreground app is doing,
  including sitting in a `DoDragDrop` modal loop with mouse capture.
  [LowLevelKeyboardProc](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc)
- On the source side, the hook swallows Caps and D, so the source never sees them.
  If a key did leak through, the drag loop only reacts to Esc (cancel) and to
  Ctrl/Shift/Alt (drop effect): Wine forwards only key messages to
  `QueryContinueDrag`, and `grfKeyState` only carries `MK_*` modifiers
  ([wine `ole2.c`](https://github.com/wine-mirror/wine/blob/master/dlls/ole32/ole2.c),
  [IDropTarget::DragOver](https://learn.microsoft.com/en-us/windows/win32/api/oleidl/nf-oleidl-idroptarget-dragover)).
- Risk: if the hook is slow it hits `LowLevelHooksTimeout` (≤ 1 s on Windows 10
  1709+), and on Windows 7+ "the hook is silently removed". `Run` happens after the
  hook returns, so this only matters if AHK is already wedged.
- I found no documented incompatibility between AHK hotkeys and an active OLE drag.
  The hand test confirms it.

### 6. Keeping the drag image over the Shelf (IDropTargetHelper)

- Create `CLSID_DragDropHelper` once (`CoCreateInstance`, `CLSCTX_INPROC_SERVER`,
  `IID_IDropTargetHelper`). In each `IDropTarget` method, call the matching helper
  method: "When you implement IDropTarget, each of its methods should call the
  corresponding IDropTargetHelper method."
  [IDropTargetHelper](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-idroptargethelper)
- The pattern, as Chromium's `DropTargetWin` does it:
  `DragEnter(hwnd_shelf, data_object, &pt, *effect)`, `DragOver(&pt, *effect)`,
  `DragLeave()`, `Drop(data_object, &pt, *effect)`, with `pt` in screen coordinates
  straight from the `POINTL`, called **before** computing Tasca's own effect.
  [`drop_target_win.cc`](https://github.com/chromium/chromium/blob/main/ui/base/dragdrop/drop_target_win.cc)
- `Show(FALSE)` / `Show(TRUE)` hides the image while the target repaints. Use it
  around any repaint that is not done through the compositor, if artifacts appear.
- Optional polish: the "Park on Tasca" label next to the image comes from the
  shell's drop-description mechanism (`SetDropDescription`), not from tracking the
  cursor yourself.
  [Old New Thing, 2021-05-14](https://devblogs.microsoft.com/oldnewthing/20210514-00/?p=105214)
- Mixed DPI (1920×1200 + 2560×1440): make the process Per-Monitor-V2 DPI-aware, so
  the `POINTL` values, the Shelf's position and the helper all share physical
  coordinates.

### 7. Pitfalls that sank similar apps

- **Windows 11 Drag Tray / Drop Tray.** "When you start dragging a local file ...
  from File Explorer or your desktop", a tray "appears at the very top of your
  screen" ([Windows Insider build 22635.4950](https://blogs.windows.com/windows-insider/2025/02/21/announcing-windows-11-insider-preview-build-22635-4950/)).
  Users complained it popped up during ordinary drags, and Microsoft is removing it
  starting with build 26340.9212. There is a toggle in Settings > System >
  Multitasking > Drop Tray (older builds: Nearby sharing)
  ([Pureinfotech](https://pureinfotech.com/disable-drag-tray-windows-11/), secondary).
  Lesson for Tasca: it hijacked drags because it appeared *uninvited*. Tasca appears
  only on Caps+D and sits mid-left, far from the top-edge tray. Mattia runs 25H2
  build 26200.9711; whether the tray is on there is untested.
- **Dropshelf.** Closed source, and I found no public issue tracker. Mattia's report
  (unreliable drags, Outlook fails) is the only evidence. Outlook payloads are a
  data-format problem (TASCA-5), not a "can the window catch it" problem.
- **Generic causes of "drops sometimes don't land"**, each covered above: a target
  that blocks its STA, a layered window with transparent gaps, elevation mismatch, a
  target that hides or moves itself during `DragOver`.

## Recommended window setup

1. One resident, **unelevated**, Per-Monitor-V2 DPI-aware `tasca.exe`. Its UI thread
   calls `OleInitialize`, then runs a normal `GetMessage` loop and never blocks.
2. At startup, create the Shelf hidden: `WS_POPUP`,
   `WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE` (plus `WS_EX_LAYERED` or
   `WS_EX_NOREDIRECTIONBITMAP` per TASCA-7), **never** `WS_EX_TRANSPARENT`, never
   disabled. Call `RegisterDragDrop(hwnd, target)` once, and create the
   `IDropTargetHelper` once.
3. On the toggle signal: position on the cursor's monitor (left edge, centred), then
   `SetWindowPos(HWND_TOPMOST, …, SWP_SHOWWINDOW | SWP_NOACTIVATE)` (or
   `ShowWindow(SW_SHOWNOACTIVATE)`). Show the final hit-testable rectangle at once;
   if there is a slide-in, animate inside an already-full-size window so
   `WindowFromPoint` hits it from the first frame.
4. Every pixel the user might drop on has alpha > 0.
5. `IDropTarget`: forward to the helper first. `DragEnter` and `DragOver` return
   `DROPEFFECT_COPY` or `LINK` (a Reference never moves the source); `Drop` reads
   the formats it needs quickly, defers slow work, and posts "hide" for after it
   returns.
6. The elevated `tasca.exe toggle` from Hyperkey only signals (warm) or relaunches
   unelevated (cold). It never owns a window.

## Checklist for TASCA-8 "Does Caps+D catch a live drag?"

Prototype on the setup above. Mattia drags by hand; no timed measurements.

- [ ] **Explorer** file drag, Caps+D mid-drag, move to the Shelf: the Shelf gets
      `DragEnter` and `Drop` (log them), and the source file is untouched (copy or
      link, not move).
- [ ] The same with a multi-file selection, a **Chromium/Edge** image or link, **new
      Outlook** (`olk.exe`) attachment, **WhatsApp** desktop media, **Claude desktop**
      file/image. Log `DragEnter` and the format list only; the payload is TASCA-5.
- [ ] Caps+D with the left button held: AHK fires, the drag is not cancelled, the
      cursor and drop-effect feedback continue, and no stray `d` or Caps toggle
      reaches the source.
- [ ] The Shelf is shown and hit-testable with the cursor **still**, already over
      where it appears (move the Shelf under the cursor for this test): `DragEnter`
      without moving the mouse (expected within ~50–100 ms).
- [ ] The source's **drag image stays visible** over the Shelf, with no ghost or
      trail after the drop or after leaving the Shelf.
- [ ] Dropping on the Shelf's **padding, row gaps and rounded corners**: nothing
      falls through to the window behind, except at most the alpha-0 corner pixels.
- [ ] The **slide-in animation** (if any) does not drop `DragEnter` or let drops
      fall through during the animation.
- [ ] **Elevation:** the Shelf process is medium IL (Task Manager "Elevated: No"),
      both when warm and when cold-started from elevated Hyperkey. Negative control
      (optional): an elevated Shelf shows the copy cursor but `Drop` never comes,
      which confirms the failure mode.
- [ ] **Cold start:** with Tasca not running, Caps+D mid-drag; the Shelf appears
      while the button is still held. Note roughly how long it takes (by feel; the
      timing itself is TASCA-4).
- [ ] **Mixed DPI:** a drag on the 2560×1440 monitor and on the laptop panel; the
      Shelf appears on the cursor's monitor and the drag image lines up with the
      cursor.
- [ ] Focus is not stolen: the source app stays the foreground window during and
      after the drop.
- [ ] Esc during the drag over the Shelf: `DragLeave` and no Park; the Shelf stays
      or hides, as the spec decides.
- [ ] The Windows 11 Drop Tray (if enabled on 26200.9711) does not interfere when
      dragging toward the left edge.
- [ ] Hyperkey regression: Caps+D still works normally outside a
      drag, and AltGr and Esc are unaffected.

## Open risks

- The exact polling interval of Windows' ole32 is undocumented. Evidence:
  WebKit's ~60 ms observation, and Wine's 50 ms imitation. Not a problem for Tasca,
  because the user must move the cursor to the edge anyway.
- Cold start latency: if relaunching unelevated through explorer.exe takes longer
  than Mattia holds the button, the drag is already over. That belongs to TASCA-4,
  but TASCA-8 should note the feel.
- Chromium/Electron/WebView2 sources were verified only from Chromium source, not
  from WhatsApp or Claude desktop builds specifically.
- A layered per-pixel-alpha Shelf trades away DWM rounding and needs hand-drawn
  corners. TASCA-7 should weigh DirectComposition (rectangle hit-test, can opt in to DWM
  rounding via `DWMWA_WINDOW_CORNER_PREFERENCE`) against that.
