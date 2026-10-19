# Taking out an Item: what to offer and how to tell the drop worked

Research for **TASCA-6**, written 2026-10-19. Desk research only: no product code and
no live drags. Anything marked **LIVE** needs a real drag on Mattia's machine to
confirm.

## Answer in brief

- **What to offer:** give each Item a *shell data object*, the same kind Explorer
  hands out when you drag a file. It carries `CFSTR_SHELLIDLIST` (the item list)
  and `CF_HDROP` (the paths), and it accepts any format a target or the drag-image
  helper writes back. Every target on Mattia's list already accepts exactly this
  from Explorer drags, so Tasca needs **no** `FILEDESCRIPTOR`/`FILECONTENTS`, **no**
  text paths and **no** `CFSTR_PREFERREDDROPEFFECT`. Text paths would actively
  break Windows Terminal (see below).
- **Allowed effects:** `DROPEFFECT_COPY | DROPEFFECT_MOVE`. Explorer then applies
  its own rules (same drive moves, another drive copies, Ctrl copies, Shift moves).
  Every other target only ever reads the file.
- **Who deletes on a move:** never Tasca. Explorer does an *optimized move*, moving
  the file itself. Chromium-based targets can report `DROPEFFECT_MOVE` without
  having moved anything (details below). If Tasca followed the textbook "source
  deletes on MOVE" rule, it would destroy the user's file.
- **Success rule:** `SHDoDragDrop` returns `DRAGDROP_S_DROP` **and** the drop did not
  land on the Shelf itself **and** at least one of these holds: the returned effect
  is not `NONE`; `CFSTR_LOGICALPERFORMEDDROPEFFECT` reads back as anything but
  `NONE`; a Reference's file no longer exists at its path. If the rule holds,
  remove the Item. Otherwise keep it.
- **Materialized files** in `%TEMP%\Tasca` must **not** be deleted at Take out.
  Browsers, WhatsApp and Claude Code read the file *after* the drop has returned,
  in Claude Code's case possibly minutes later. Clean the folder up at startup
  instead.

## 1. The data object

### What the targets read

| Target | Engine that handles the drop | Format it reads | Effect it reports to Tasca | Who touches the file | Reads the file after the drop returns? |
|---|---|---|---|---|---|
| Explorer folder / desktop | Shell file-system drop target | `CFSTR_SHELLIDLIST` or `CF_HDROP` | Copy: `COPY`. Optimized move: `NONE` (or `COPY`), with `LOGICALPERFORMEDDROPEFFECT = MOVE` [1][2] | Explorer copies or moves the file itself | Only if the source opts into async (Tasca does not) [3] |
| Recycle Bin | Shell | same | as above, plus `CFSTR_TARGETCLSID = CLSID_RecycleBin` [1] | Shell recycles the file | n/a |
| Claude.ai, ChatGPT in Brave/Edge | Chromium (`DesktopDropTargetWin` → `WebContentsViewAura` → Blink) | `CF_HDROP` (real files). Virtual files only when there are no real ones [4] | The page's last `dropEffect`, or Blink's default: **`MOVE` whenever MOVE is offered**, unless the page sets `dropEffect` [5][6] | Never: the page uploads a copy | **Yes.** The page's `drop` handler runs after `DoDragDrop` returns, and the upload reads the path later [4] |
| Claude desktop, VS Code | Electron = Chromium, same code path | `CF_HDROP` | same as Chromium | VS Code opens the file, or copies it into the workspace when dropped on its Explorer tree | Yes |
| WhatsApp desktop | WebView2 wrapper around web.whatsapp.com since 2025 [7] | `CF_HDROP` | same as Chromium | never | Yes, until Mattia presses Send |
| New Outlook compose | WebView2 host (OWA inside a native shell) [8] | `CF_HDROP` | same as Chromium | never: it uploads an attachment | Yes, during the upload |
| Windows Terminal (Claude Code) | XAML drop in `TermControl` | **Text first**, then `StorageItems` (derived from `CF_HDROP`/"FileDrop") [9] | `COPY` (`AcceptedOperation = Copy`) [9] | never: it pastes the path as text | **Yes, much later.** Claude Code reads the path when the prompt runs |

So `CF_HDROP` is enough for every target. Chromium's own download-shelf drag offers
only `CF_HDROP` and a drag image, with `COPY | LINK` [10]. That is a real
implementation which works with all of these targets.

### Things not to add

- **No `CF_UNICODETEXT`/`CF_TEXT` paths.** Terminal's drop handler checks `Text`
  *before* `StorageItems`, so text would replace the proper path paste [9].
  Chromium would also hand the page a `text/plain` item next to the file, and the
  chat apps could paste the path into the composer as well as attaching the file.
- **No `CFSTR_FILEDESCRIPTOR`/`CFSTR_FILECONTENTS`.** Microsoft's guidance is that
  "existing files should be offered with the CF_HDROP format" [2]. Every Materialized
  file is already a real file in `%TEMP%\Tasca`.
- **No `CFSTR_PREFERREDDROPEFFECT`.** It tells the Shell which action to take and
  overrides its default by-volume rule [11]. Mattia wants Windows' normal rules.
- **No async opt-in** (`IDataObjectAsyncCapability::SetAsyncMode(TRUE)`). The default
  is synchronous. With async on, Explorer returns from `Drop` straight away and
  reports the outcome later through `SetData`/`EndOperation` [3]. Tasca would
  then have to wait for a private notification before it could decide anything.

### How to build it in Rust (`windows` crate)

Everything below is in `windows::Win32::UI::Shell` / `System::Ole`. I checked that each
symbol exists in windows-docs-rs: `SHParseDisplayName`,
`SHCreateShellItemArrayFromIDLists`, `BHID_DataObject`, `SHCreateDataObject`,
`SHDoDragDrop`, `DoDragDrop`, `IDragSourceHelper2`, `CLSID_DragDropHelper`,
`IShellItemImageFactory`, `CFSTR_LOGICALPERFORMEDDROPEFFECT`, `IDropSource_Impl` [26].

1. **Recommended: let the Shell build the data object.**
   `SHParseDisplayName(path)` for each file, then `SHCreateShellItemArrayFromIDLists`,
   then `IShellItemArray::BindToHandler(None, &BHID_DataObject)` to get an `IDataObject`.
   This is the Explorer-equivalent object (the parent folder's `GetUIObjectOf`).
   Raymond Chen: "you are much better off letting the shell create the data
   object, since that data object will contain much richer information" [12]. It
   also already "accept[s] arbitrary blobs", which the drag-image helper requires
   [13]. **Constraint:** `BHID_DataObject` is documented only for *flat* arrays, where
   all items share one folder [14]. That always holds for a single file and for
   `%TEMP%\Tasca`, and almost always for a Stack (one Explorer selection).
2. **Fallback for a Stack whose files sit in different folders** (for example
   Explorer search results): `SHCreateDataObject(None, 0, None, None)` gives an empty
   shell data object. Fill it with `SetData(CF_HDROP, DROPFILES-in-HGLOBAL, fRelease=TRUE)`.
   The doc says this object "has generic support for arbitrary clipboard formats
   through IDataObject::SetData" [15]. It ends up with the same format set as
   Chromium's file drag [10]. It is still a Shell object, so SetData read-back and
   drag images work. Tasca writes no COM code for it.
3. **Not recommended:** a hand-written `IDataObject`. It is possible with
   `#[implement]`, but Tasca would also have to store arbitrary `SetData` formats
   (for drag images and drop-outcome read-back) and enumerate them. That is
   ~200 lines replicating what `SHCreateDataObject` already does. `SHCreateFileDataObject`
   is undocumented on Learn and missing from windows-rs, so skip it.

**Start the drag** with `SHDoDragDrop(hwnd_or_null, &dataobj, None, DROPEFFECT_COPY | DROPEFFECT_MOVE, &mut effect)`.
Since Vista, passing a null `IDropSource` makes the Shell supply one. If no drag image
is stored, the Shell also supplies a generic one [16]. So v1 needs no COM
implementation at all.

## 2. Detecting success

What the sources establish:

- `DoDragDrop`'s return code says how the *drag loop* ended, not whether a target
  took the data. MS: when `QueryContinueDrag` returns `DRAGDROP_S_DROP`, "DoDragDrop
  calls IDropTarget::Drop … returns the last effect code" [17]. Wine's
  implementation calls `DragLeave` instead of `Drop` when the cursor is over nothing
  or over a target that said `NONE`. It still returns `DRAGDROP_S_DROP`, with
  `*pdwEffect = DROPEFFECT_NONE` [18]. **So `DRAGDROP_S_DROP` alone is not success.**
  Tasca must also look at the effect.
- **Optimized move (Explorer, same drive):** the target sets `pdwEffect` "to some value
  other than DROPEFFECT_MOVE … typically DROPEFFECT_NONE or DROPEFFECT_COPY". It also
  sets `CFSTR_PERFORMEDDROPEFFECT` to `NONE` [2]. **So a successful move into Explorer
  can look exactly like a failed drop.** The documented way out is
  `CFSTR_LOGICALPERFORMEDDROPEFFECT`. The source reads it with `GetData`, and it holds
  "what happens from the user's point of view": `MOVE`, `COPY` or `LINK` [11].
  Raymond Chen's 2004 sample reads `CFSTR_PERFORMEDDROPEFFECT` instead and says it
  holds the pre-optimization effect [19]. That contradicts the current docs, which
  is one more reason to read the *logical* value.
- **Chromium (both browsers, Electron, WebView2):** `OnDrop` returns
  `current_drag_data_->operation`, the operation from the *last dragover* [5].
  The drop itself is delivered to the page asynchronously, after `DoDragDrop`
  has returned [4]. If the page called `preventDefault()` without setting
  `dropEffect`, Blink's `DefaultOperationForDrag` returns **`kMove` whenever the
  source allows MOVE**. It returns `kCopy` only when the mask is the special
  `kDragOperationEvery` (`UINT_MAX`), which an OLE source never sends [6]. A page that
  ignores the drop entirely reports `kCopy` if the data "contains a URL" (Chromium
  would navigate to the file), else `kNone` [6]. Consequences:
  - Chromium never reports `NONE` for a drop it accepted. No false negatives there.
  - It can report `MOVE` for a plain upload. The textbook "MOVE means the source
    deletes the original" rule [19][20] must not be applied.
  - It reports success *before* the page decides. If ChatGPT then rejects the file
    type, Tasca has already removed the Item. Nothing on the OLE side can detect this.
- **Terminal** reports `COPY` [9]. Explorer's right-drag menu with **Cancel**
  reports `NONE` (ReactOS's clone of the Shell target does this [21]).

### Recommended rule

```text
let hr = SHDoDragDrop(...);
let success = hr == DRAGDROP_S_DROP
    && !dropped_on_own_shelf                     // Shelf's IDropTarget returns NONE for its own drag
    && ( effect != DROPEFFECT_NONE
         || logical_performed_effect(dataobj) is Some(e) where e != NONE   // GetData(CFSTR_LOGICALPERFORMEDDROPEFFECT), DWORD in HGLOBAL
         || any Reference path of the Item no longer exists );            // belt-and-braces for moves that report nothing
if success { remove the Item (the whole Stack) } else { keep it }
// Tasca never deletes, moves or renames a file because of `effect`.
```

The path-exists check costs one `GetFileAttributesW` per file, a few microseconds.
It catches an optimized move if Explorer sets the logical effect late or not at all
(**LIVE**).

## 3. Move semantics

- **References:** offering `COPY | MOVE` lets Explorer apply the normal rules. Explorer
  moves the file itself (optimized move), so the source has nothing to delete
  [2][19]. Other targets only read the file. Tasca must therefore **never** act on
  a returned `MOVE`. Inference worth one **LIVE** glance: Explorer is itself a file
  drag source that offers MOVE. Chromium reports MOVE for any page drop zone that
  doesn't set `dropEffect`, yet dropping files from Explorer into web apps doesn't
  delete them. So Explorer as a source evidently ignores a returned MOVE for file
  system items too.
- **Materialized files** in `%TEMP%\Tasca` (on C:): dropped into a C: folder, Explorer
  **moves** the temp file out, leaving nothing behind. Dropped on another drive or
  with Ctrl, it is a copy and the temp file stays. Dropped into a browser, chat app
  or Terminal, the target reads it later. Rule: **never delete a Materialized file at
  Take out**, not even after success. Sweep `%TEMP%\Tasca` when Tasca starts. Nothing
  is persisted, so no Item can point at those files. Suggestion for the spec: only
  delete entries older than about 24 h, so that a Tasca restart doesn't pull a file
  out from under a Claude Code prompt pasted minutes earlier.
- **Implication for TASCA-5:** give each Materialized Item its own subfolder
  (`%TEMP%\Tasca\<id>\<original name>`). Parking two "image.png" files must never
  overwrite a file a target is still reading.
- **Do not offer `LINK`.** Alt-drop into Explorer would otherwise create shortcuts to
  temp files. Leaving it out also doesn't change Chromium's default, since the mask
  is still ≠ `UINT_MAX` [6].

## 4. Drag image

- `SHDoDragDrop` with no stored image and no `DI_GETDRAGIMAGE` window shows a generic
  Shell drag image [16]. That is enough for v1. How it looks for a Shell data object
  (thumbnail? a count badge for a Stack?) is **LIVE**.
- Polish path: `CoCreateInstance(CLSID_DragDropHelper)` → `IDragSourceHelper2::SetFlags(DSH_ALLOWDROPDESCRIPTIONTEXT)`
  → `InitializeFromBitmap(SHDRAGIMAGE, dataobj)` *before* the drag. Take the bitmap
  from `IShellItemImageFactory::GetImage`. **Pass straight (non-premultiplied) alpha.**
  The method premultiplies again otherwise [22]. Chromium un-premultiplies for the
  same reason [10]. The helper stores private formats via `SetData`, which the
  Shell data object accepts [13][23].
- For a Stack, draw the first file's thumbnail plus a count, or let the generic image
  stand.

## 5. Starting the drag from a `WS_EX_NOACTIVATE` Shelf

- The click still arrives (answer `WM_MOUSEACTIVATE` with `MA_NOACTIVATE`). Start the
  drag on `WM_MOUSEMOVE` once the button is down and the cursor has moved past
  `SM_CXDRAG`/`SM_CYDRAG`.
- **Capture:** `SetCapture` "captures mouse input … when the mouse button was pressed
  while the mouse was over the capturing window and the button is still down". The
  same page also says "only the foreground window can capture the mouse" [24]. A
  drag starting from a button-down on the Shelf fits the first sentence, but the two
  statements conflict. **LIVE**.
- **Keyboard during the drag:** the OLE loop reads the *thread's* keyboard state and
  sees Esc only as a keyboard message to its own thread. Wine's implementation
  (`GetKeyboardState`, `WM_KEYDOWN VK_ESCAPE`) shows this [18]. A thread that isn't
  in the foreground may therefore miss **Esc** (cancel) and **Ctrl/Shift** (copy/move
  override), and the target would get stale `grfKeyState`. **LIVE: the top risk in
  this note.**
  - Fix for Esc: own `IDropSource` (`#[implement]`, ~30 lines) whose
    `QueryContinueDrag` also checks `GetAsyncKeyState(VK_ESCAPE)`.
  - Fix for Ctrl/Shift: the source can't rewrite the key state OLE passes to the
    target. The fallback is `SetForegroundWindow(shelf)` at drag start. That is
    allowed because Tasca's process "received the last input event" [25]. It means
    a brief focus change on Take out, which is a product call for Mattia.
- The Shelf is also a drop target. During its own drag it must answer `DROPEFFECT_NONE`
  (keep an `in_own_drag` flag), so dropping an Item back on the Shelf is a no-op and
  the Item stays.
- Hide the Shelf only after `SHDoDragDrop` returns, then apply the success rule.

## Open risks

| # | Risk | Effect | Confirm by |
|---|---|---|---|
| R1 | Esc / Ctrl / Shift not seen during a drag from the non-activated Shelf | can't cancel; normal move/copy override broken | **LIVE**: drag to Explorer with Ctrl, with Shift, and press Esc |
| R2 | Capture from a non-foreground window | drag fails to start or stutters | **LIVE** |
| R3 | Chromium apps report success before the page accepts (unsupported type, page ignores the drop, Electron blocks navigation) | Item removed although nothing was attached; a Reference's file is still on disk | **LIVE** per app; accept as known v1 behaviour |
| R4 | What Explorer actually returns for an optimized move (effect `NONE` vs `COPY`, logical value present) | covered by the logical-effect and path-gone checks | **LIVE**: drag to a C: folder |
| R5 | Generic drag image for a Stack looks poor | cosmetic | **LIVE** |
| R6 | Sync drop into Explorer of a big file blocks Tasca's UI thread until the copy ends (ReactOS does the copy inside `Drop` when not async [21]) | Shelf frozen during a large copy; it hides anyway | **LIVE** with a large file across drives |

## Sources

1. MS Learn, *Handling Shell Data Transfer Scenarios*, "Dropping Files on the Recycle Bin": https://learn.microsoft.com/en-us/windows/win32/shell/datascenarios
2. Same page, "General Guidelines" and "Handling Optimized Move Operations" (pdwEffect not MOVE, `PERFORMEDDROPEFFECT = NONE`).
3. Same page, "Using IASyncOperation/IDataObjectAsyncCapability"; and IDataObjectAsyncCapability::SetAsyncMode (default VARIANT_FALSE): https://learn.microsoft.com/en-us/windows/win32/api/shldisp/nf-shldisp-idataobjectasynccapability-setasyncmode
4. Chromium `content/browser/web_contents/web_contents_view_aura.cc`: comment block above `PerformDropCallback` ("Performing the drop is an asynchronous operation"; virtual files to temp files on Windows), `CompleteDrop`. https://github.com/chromium/chromium/blob/main/content/browser/web_contents/web_contents_view_aura.cc
5. Same file, `PerformDropOrExitDrag`: `output_drag_op = current_drag_data_ ? current_drag_data_->operation : kNone`; and `ui/views/widget/desktop_aura/desktop_drop_target_win.cc` `OnDrop` → `DragOperationToDropEffect`. `ui/aura/client/drag_drop_delegate.h` passes `DragOperation&` by reference.
6. Blink `third_party/blink/renderer/core/page/drag_controller.cc`: `DefaultOperationForDrag`, `TryDHTMLDrag`, `OperationForLoad`, `GetDragOperation`; `third_party/blink/public/common/page/drag_operation.h` (`kDragOperationEvery = UINT_MAX`). https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/core/page/drag_controller.cc
7. WhatsApp Beta for Windows 2.2569 moves to WebView2: https://www.notebookcheck.net/WhatsApp-ditches-native-Windows-11-app-in-favor-of-Microsoft-WebView2.1063682.0.html (secondary; stable-channel status not confirmed)
8. MS Learn, *Overview of the new Outlook for Windows* (WebView2): https://learn.microsoft.com/deployoffice/outlook/overview-new-outlook; WebView2 runtime in Outlook: https://learn.microsoft.com/deployoffice/webview2-install
9. Windows Terminal `src/cascadia/TerminalControl/TermControl.cpp`: `_DragDropHandler` (ApplicationLink → WebLink → Text → StorageItems; "StorageItem must be last"), `_DragOverHandler` (`AcceptedOperation(Copy)`). https://github.com/microsoft/terminal/blob/main/src/cascadia/TerminalControl/TermControl.cpp
10. Chromium `ui/base/dragdrop/os_exchange_data_provider_win.cc` (`SetFilenames` = CF_HDROP only; un-premultiplies the drag bitmap); `chrome/browser/download/drag_download_item_aura.cc` (`DRAG_COPY | DRAG_LINK`).
11. MS Learn, *Shell Clipboard Formats*: `CFSTR_LOGICALPERFORMEDDROPEFFECT`, `CFSTR_PERFORMEDDROPEFFECT`, `CFSTR_PREFERREDDROPEFFECT`, `CFSTR_TARGETCLSID`. https://learn.microsoft.com/en-us/windows/win32/shell/clipboard
12. Raymond Chen, *Improving the performance of CF_HDROP by providing file attribute information* (2014): https://devblogs.microsoft.com/oldnewthing/20140609-00/?p=783
13. Raymond Chen, *Dragging a shell object, part 4: Adding a prettier drag icon* (2004): https://devblogs.microsoft.com/oldnewthing/20041209-00/?p=37083 ("the standard shell data object" accepts arbitrary blobs)
14. MS Learn, `IShellItemArray::BindToHandler` (`BHID_DataObject`: "only for flat data objects"): https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-ishellitemarray-bindtohandler; Raymond Chen on paths → `SHCreateShellItemArrayFromIDLists`: https://devblogs.microsoft.com/oldnewthing/20140314-00/?p=1503
15. MS Learn, `SHCreateDataObject`: https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shcreatedataobject
16. MS Learn, `SHDoDragDrop` (null `pdsrc` since Vista; generic drag image): https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shdodragdrop
17. MS Learn, `DoDragDrop`: https://learn.microsoft.com/en-us/windows/win32/api/ole2/nf-ole2-dodragdrop
18. Wine `dlls/ole32/ole2.c`: `drag_end` (Drop vs DragLeave, `pdwEffect = NONE`), `DoDragDrop` loop (Esc via `WM_KEYDOWN`), `OLEDD_GetButtonState` (`GetKeyboardState`). https://gitlab.winehq.org/wine/wine/-/blob/master/dlls/ole32/ole2.c (a reimplementation, not Microsoft's code)
19. Raymond Chen, *Dragging a shell object, part 2: Enabling the Move operation* and *part 3: Detecting an optimized move*: https://devblogs.microsoft.com/oldnewthing/20041207-00/?p=37123, https://devblogs.microsoft.com/oldnewthing/20041208-00/?p=37093
20. MS Learn, *DROPEFFECT Constants*: https://learn.microsoft.com/en-us/windows/win32/com/dropeffect-constants
21. ReactOS `dll/win32/shell32/droptargets/CFSDropTarget.cpp` (`Drop`: right-drag menu, async path via `IAsyncOperation`, else synchronous `_DoDrop`). https://github.com/reactos/reactos/blob/master/dll/win32/shell32/droptargets/CFSDropTarget.cpp (clean-room clone, indicative only)
22. MS Learn, `IDragSourceHelper::InitializeFromBitmap` (premultiplies again): https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-idragsourcehelper-initializefrombitmap
23. MS Learn, `IDragSourceHelper` (data object must accept arbitrary private formats): https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-idragsourcehelper
24. MS Learn, `SetCapture`: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setcapture
25. MS Learn, `SetForegroundWindow` ("The calling process received the last input event"): https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setforegroundwindow
26. windows-rs API docs (symbol availability): https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/UI/Shell/
