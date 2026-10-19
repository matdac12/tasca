# PROTOTYPE: catch-drag spike (TASCA-8)

Throwaway. This lives only on branch `prototype/catch-drag` and is never merged.
It answers "Does Caps+D catch a live drag?" by hand test, with a log.

What it is: one `tasca-spike.exe`, built the way the research recommends.

- `tasca-spike.exe toggle` (Caps+D): when the Shelf is running, it does `FindWindowW("Tasca.Shelf")` and posts `WM_APP+1`.
  When none is running and the caller is elevated (Hyperkey), it relaunches unelevated through `explorer.exe`.
  When none is running and the caller is unelevated, it becomes the resident.
- The resident is one hidden, layered `TOPMOST | TOOLWINDOW | NOACTIVATE` popup, registered once with
  `RegisterDragDrop`. It forwards to `IDropTargetHelper` and is drawn with software Direct2D + DirectWrite (mockup C).
- Park: it logs `DragEnter`/`DragOver`/`DragLeave`/`Drop`, every format offered and the async capability.
  It answers COPY and reads `CF_HDROP` only. A Park without `CF_HDROP` shows up as a placeholder row.
- Take out: drag a row out. It builds Explorer's data object (`SHCreateShellItemArrayFromIDLists` →
  `BHID_DataObject`) and calls `SHDoDragDrop(COPY|MOVE)` with its own `IDropSource`. That source logs
  what OLE reports for Esc/Ctrl/Shift next to the physical keys, then applies the success rule from TASCA-6.
- Right-click a row to remove it. Right-click the header to quit. "Clear all" clears.

## Install (once)

```powershell
cd prototype\catch-drag
cargo build --release
New-Item -ItemType Directory -Force "$env:LOCALAPPDATA\TascaSpike" | Out-Null
Copy-Item target\release\tasca-spike.exe "$env:LOCALAPPDATA\TascaSpike\"
```

Add this block to `%USERPROFILE%\Scripts\hyper.ahk`, after the Caps+R block (ASCII, LF), then press Caps+Esc to reload:

```ahk
; --- Tasca (TEMPORARY: TASCA-8 spike) --------------------------------------
CapsLock & d:: {
    Run '"' EnvGet("LOCALAPPDATA") '\TascaSpike\tasca-spike.exe" toggle'
    KeyWait "CapsLock", "T0.4"
}
```

Log: `%LOCALAPPDATA%\TascaSpike\spike.log`. Lines carry wall-clock time, so the helper's lines and the resident's lines interleave.

Options: put words in `%LOCALAPPDATA%\TascaSpike\flags.txt` (read at resident start, so quit and restart after editing):
`under-cursor`, `no-anim`, `fade-from-zero`, `no-shadow`, `focus-on-drag`, `allow-elevated`.

## Hand test

The checklist is in `research/catch-drag.md` (branch `research/catch-drag`), plus the extras on TASCA-8.

1. **Cold, mid-drag, from the Desktop.** Quit the spike (right-click the header). Start dragging a file on the
   Desktop, press Caps+D, and drop on the Shelf. Does it appear while you're still holding the button?
2. **Warm, mid-drag** from Explorer (one file, then several), Brave (image, link, selected text), new Outlook
   (an attachment, a mail), WhatsApp, Claude desktop, a file inside a .zip, and Edge's download flyout.
3. **Caps+D before the drag** vs **during the drag**. Is the drag cancelled? Does a stray `d` or a Caps toggle appear?
4. **Drag image** over the Shelf: is it still visible? Any ghost or trail afterwards?
5. **Edges:** drop on the padding, the gaps between rows, the corners and the shadow band just outside the panel.
6. **During the slide-in:** press Caps+D and drop at once.
7. **Cursor still** (`under-cursor` flag): hold a drag still, press Caps+D. The Shelf appears under the cursor.
   Does the copy cursor come without moving the mouse?
8. **Both monitors**, and **focus**: the source app should stay in front throughout.
9. **Esc** while hovering the Shelf mid-drag.
10. **Take out** a row into Explorer: plain, then Ctrl (copy), then Shift (move), then Esc mid-drag.
    Also into Claude/ChatGPT in the browser, WhatsApp and Terminal. Look for `MISMATCH` lines in the log.
    Repeat with `focus-on-drag`.
11. **Look:** is the grayscale text sharp on both monitors? Does wheel scroll work over the Shelf (it needs more rows than fit)?
12. **Elevation:** Task Manager → Details → "Elevated" column should say No for `tasca-spike.exe`.
