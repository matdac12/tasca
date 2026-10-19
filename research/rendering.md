# How a lean Rust Win32 window draws the Shelf (mockup C)

Research for TASCA-7, 2026-10-19. Target: mockup C in `docs/mockups/shelf-layouts.html`
(the compact list, no drop zone): a 200 DIP panel with `rgba(18,18,20,.92)` glass, a 0.5 px
`rgba(255,255,255,.14)` hairline, 12 px corners, shadow `0 6px 18px rgba(0,0,0,.35)`,
Segoe UI Variable Text at 12 / 10.5 / 11 px, 30 px thumbnails with 6 px corners, a 1.5 px
`#4cc2ff` outline while a drag hovers, a short slide-in, scrolling. Mattia's screens are a
1920x1200 laptop plus a 2560x1440 above it, at different scale factors. The window is
`WS_POPUP` with `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` and must also be an OLE drop target
(TASCA-3).

## Answer

Draw the whole Shelf yourself into a premultiplied BGRA bitmap and give it to Windows as a
per-pixel-alpha layered window (`WS_EX_LAYERED` + `UpdateLayeredWindow`). Render with
Direct2D in software mode plus DirectWrite. Load thumbnails with
`IShellItemImageFactory::GetImage` on a background thread. Run the slide-in as a timer that
moves the window and fades `SourceConstantAlpha` through `UpdateLayeredWindow`, with no
re-render per frame. Use `windows` 0.62.2. Don't use any DWM material or corner API on this
window.

This is the only path that matches all of mockup C (12 px corners, 92 % translucency, our
own shadow and blue outline) without creating a Direct3D device. The D3D/DirectComposition
path looks the same but costs tens of MB of idle memory. The DWM-native path is the leanest
code, but it can't do 12 px corners or translucency.

## Options compared

| | A. DWM-native popup (GDI or D2D on an opaque HWND) | B. Layered window: D2D software + DirectWrite + `UpdateLayeredWindow` (**recommended**) | C. DirectComposition + D3D11 + D2D 1.1 | D. `tiny-skia` + `UpdateLayeredWindow` |
|---|---|---|---|---|
| 12 px corners | No. DWM picks the radius: 8 px for windows and flyouts [geometry] | Yes, drawn | Yes, drawn | Yes, drawn |
| 92 % translucency | No (opaque). Any backdrop would be Acrylic, not a tint | Yes, per pixel | Yes, per pixel | Yes, per pixel |
| Hairline, blue drag outline | `DWMWA_BORDER_COLOR` (DWM border, 1 px) | Drawn | Drawn | Drawn |
| Soft shadow | DWM's own, if it draws one for a rounded popup (unverified) | Drawn into a bitmap margin | Drawn into a margin | Drawn into a margin |
| Text | ClearType (opaque target) | Grayscale AA (alpha target) [d2d-aa] | Grayscale AA [d2d-aa] | None built in. Needs a font rasteriser crate [tiny-skia] |
| Hit test / drop target | Window rect | Pixels with alpha 0 pass through; everything else, including the shadow, is the window [layered] | Whole window rect, uniformly [kerr14] | As B |
| GPU device | None (GDI) | None | D3D11 hardware device | None |
| Idle memory | Lowest | Low (estimate below) | Highest: one sample showed 34 MB private working set | Low |
| Exe size added | ~0 | ~0 (system DLLs) | ~0 | ~200 KiB [tiny-skia] + font crate |
| Code size | Smallest | Small/medium | Largest (device loss, swap chain or surfaces) | Medium (text by hand) |

Why the others lose:

- **A (DWM-native).** DWM rounds a popup only when it opts in with
  `DWMWA_WINDOW_CORNER_PREFERENCE` [corners]. The radius is the system's (8 px, or the
  small radius) [geometry], and the window stays opaque. `DWMWA_SYSTEMBACKDROP_TYPE`
  (Windows 11 22621+) gives Mica or Acrylic, not a 92 % dark tint [backdrop]. Background
  acrylic "will replace its translucency and texture with a solid color ... when an app
  window on desktop deactivates" [acrylic], and the Shelf is never activated. One real
  app that built Acrylic `WS_EX_NOACTIVATE` popups reports exactly that solid fallback
  [fizzy]. Keep A as the fallback if Mattia accepts 8 px corners and an opaque panel. It's
  the least code and gives ClearType text.
- **C (DirectComposition).** This is the modern way to get per-pixel alpha on the GPU
  (`WS_EX_NOREDIRECTIONBITMAP` + a composition swap chain or surface + Direct2D) [kerr14],
  and its animations run on the compositor's thread [dcomp-anim]. Its cost is the D3D11
  device. One unscheduled sample of a probe (C + OLE + one shell thumbnail) showed a 65 MB
  working set, 34 MB of it private (see "Memory and size"). That breaks the README's "a few
  MB resident" for a window shown a few seconds at a time. Kerr also notes the window "will
  hit test uniformly" [kerr14], so a shadow margin catches drops and clicks anyway.
- **D (tiny-skia).** It is a good rasteriser (~200 KiB), but "the main missing feature is
  text rendering" [tiny-skia]. Segoe UI Variable is a variable font, and DirectWrite already
  ships its hinting and shaping in the OS for free.
- **GDI / GDI+ into a layered bitmap.** "Most of those [GDI functions] don't support the
  alpha channel", and GDI+ is CPU-only with weaker rendering [kerr09]. Direct2D's software
  path supersedes it.

## Recommended path in detail

### Window

- `CreateWindowExW(WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST, …, WS_POPUP, …)`.
  Show it with `SW_SHOWNOACTIVATE` and never call `SetLayeredWindowAttributes`: after that
  call, `UpdateLayeredWindow` fails until the style bit is reset [layered].
- Hit testing: "areas of the window that are color-keyed or whose alpha value is zero will
  let the mouse messages through" [layered]. The panel is 92 % opaque, so the whole panel
  is a drop target. Never add `WS_EX_TRANSPARENT`, which passes all mouse input through
  [layered]. `DoDragDrop` "determines the window under the current cursor location" and then
  checks whether it is a registered target [dodragdrop]. `RegisterDragDrop` on this HWND is
  the TASCA-3 question; nothing in this rendering path gets in its way.
- The shadow needs a transparent margin around the panel (about 18 px at the sides and 24 px
  below, at 100 %). Its pixels have alpha > 0, so that band also catches clicks and drops
  while the Shelf is shown. Choose between a softer, smaller shadow and accepting the band.
  A tighter shadow (for example 0 4px 12px) halves it.
- DWM attributes: none are needed. Per-pixel-alpha windows "cannot ever be rounded"
  [corners], so the corner preference is moot. Consider `DWMWA_TRANSITIONS_FORCEDISABLED =
  TRUE` [dwmattr] so DWM's own show/hide transition doesn't stack on ours. Dark-mode
  attributes only affect a frame, which this window doesn't have.

### Drawing

- Use `ID2D1Factory` with a render target over a 32 bpp premultiplied top-down DIB, then
  `UpdateLayeredWindow(…, ULW_ALPHA)` with `AC_SRC_ALPHA` [kerr09, layered]. Kerr's ranking:
  a WIC bitmap render target is "the best possible performance without hardware
  acceleration". A DC render target (`CreateDCRenderTarget` + `BindDC`, premultiplied
  BGRA) works too, with one extra internal copy [kerr09, d2d-gdi]. Request
  `D2D1_RENDER_TARGET_TYPE_SOFTWARE` so no GPU device is created.
- Text: create an `IDWriteFactory` and an `IDWriteTextFormat` for "Segoe UI Variable Text"
  (fallback "Segoe UI") at the mockup sizes, drawn with `DrawText`/`DrawTextLayout` and
  trimmed with an ellipsis (`DWRITE_TRIMMING`). On a target with an alpha channel the
  default anti-aliasing drops to grayscale [d2d-aa]. That is the price of translucency.
  The panel is 92 % opaque dark and Mattia's screens run above 100 %, so expect it to look
  fine, but check it by eye.
- Shapes: draw the panel and hairline with `FillRoundedRectangle`/`DrawRoundedRectangle`
  (radius 12 DIP). Draw the hover outline (1.5 DIP `#4cc2ff`) as a second
  `DrawRoundedRectangle`, and the selected row as a 7 DIP rounded rect at `rgba(255,255,255,.08)`.
  Clip thumbnails to 6 DIP rounded rects with a bitmap brush in `FillRoundedRectangle`.
  Snap hairlines to physical pixels (1 px = 1/scale DIP).
- Shadow: compute it once per size and DPI, either as a few stacked rounded rects with
  falling alpha or as a small box blur of the panel's alpha mask in Rust (about 30 lines).
  This avoids the D2D effects pipeline.
- Re-rendering: redraw the whole bitmap on any state change (hover outline on/off, Item
  added, scroll step, selection). It is about 300x600 px at 150 %, which is small enough for
  the CPU. Rendering cost was not timed (see "Not measured").

### Per-monitor DPI v2

- Declare PMv2 in the app manifest (`<dpiAwareness>PerMonitorV2</dpiAwareness>`), or call
  `SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)` first thing,
  as `windows-window` does [dpi, windows-window]. Under PMv2 the app "see[s] the raw pixels
  of each display" and is "never ... bitmap scaled" [dpi].
- On each show, call `GetCursorPos` → `MonitorFromPoint(MONITOR_DEFAULTTONEAREST)` →
  `GetMonitorInfoW` (`rcWork`) + `GetDpiForMonitor(MDT_EFFECTIVE_DPI)` [getdpiformonitor].
  Lay out in DIPs, scale = dpi/96, render at that scale (`SetDpi` on the render target).
  Then call `UpdateLayeredWindow` with `pptDst`/`psize` in physical pixels, vertically
  centred on the left edge of `rcWork`.
- `WM_DPICHANGED` then arrives for the window's new monitor [dpichanged]. If its DPI differs
  from the one rendered, re-render. Its suggested rect is for windows the user drags between
  monitors; the Shelf positions itself, so apply your own rect. Docs call ignoring the
  suggestion a pitfall only for that drag case [dpi].
- Reload thumbnails at the new pixel size when the DPI changes [dpi, step 4].

### Thumbnails and icons

- On a single background thread with `CoInitializeEx(COINIT_APARTMENTTHREADED)`, call
  `SHCreateItemFromParsingName(path)` → `IShellItemImageFactory::GetImage(30 DIP × scale,
  SIIGBF_RESIZETOFIT)` → `HBITMAP`. The default returns a thumbnail, or the icon if there is
  none, "extracted if it is not currently cached" [getimage]. Add `SIIGBF_CROPTOSQUARE`
  (Windows 8+) for image Items to fill the square tile like the mockup. Use `SIIGBF_ICONONLY`
  for folders and Stacks if the icon reads better.
- Never call it on the UI thread: "An extraction should never be done on a UI thread"
  [getimage]. Copy the bits out with `GetDIBits`, `DeleteObject` the `HBITMAP` (the caller
  frees it) [getimage], post the pixels to the UI thread, and build a D2D bitmap there.
- The docs don't say whether the `HBITMAP` alpha is premultiplied. Normalise it: if every
  alpha is 0, treat the image as opaque; otherwise premultiply.
- Cache one bitmap per Item per DPI in memory: 45x45x4 B ≈ 8 KB at 150 %. The shell keeps
  its own disk cache (`SIIGBF_INCACHEONLY` reads it) [getimage]. Draw a neutral tile at once
  so the Shelf never waits for the shell.
- Fallback when `GetImage` fails (for example on a path that has vanished): call
  `SHGetFileInfoW(ext, FILE_ATTRIBUTE_NORMAL, SHGFI_ICON | SHGFI_USEFILEATTRIBUTES)` for the
  type's icon, on a background thread, after COM init, and `DestroyIcon` it [shgetfileinfo].
- Cost per Item: one shell call off-thread plus about 8 KB. The first call loads the shell
  stack into the process, which is the largest share of idle memory after D2D/DWrite (see
  below).

### Slide-in animation

- Set the window up at its final size, then for about 160–180 ms (Parla's `appear` is 180 ms
  with `cubic-bezier(0.2,0,0,1)`) call `UpdateLayeredWindow` each frame with a new `pptDst`
  (x from −16 DIP to the final x) and a rising `BLENDFUNCTION.SourceConstantAlpha`. Pass the
  same already-rendered DIB as `hdcSrc` every frame, with no re-render. The docs say "If
  *hdcSrc* is NULL, *dwFlags* should be zero" [ulw], so a null source can't be trusted to
  apply a new blend. ULW "permits seamless animation of the layered window" [ulw], and
  Microsoft adds: "For maximum
  efficiency in animating windows, call UpdateLayeredWindow to change the position and the
  size" [layered]. Kerr: `SourceConstantAlpha` fades "without the cost of redrawing"
  [kerr09].
- Drive the frames with `SetTimer` at about 10 ms and easing based on elapsed time (QPC),
  so a late tick never stretches the animation. Never block the UI thread: drag-enter calls
  come in on it.
- Skip the animation when `SPI_GETCLIENTAREAANIMATION` is off [spi].
- Not `AnimateWindow`: it paints through `WM_PRINT`/`WM_PRINTCLIENT`, and Microsoft says
  "Avoid animating a window that has a drop shadow" [animatewindow]. Not DirectComposition:
  it needs path C's device.
- Open point for TASCA-3/8: whether a layered window at low `SourceConstantAlpha` already
  catches a drag that enters during the fade. The docs only describe per-pixel alpha 0.
  Start the fade at about 40 % rather than 0 if this matters.

### Scrolling

- Set a maximum panel height, for example about 60 % of `rcWork` (TASCA-1 lists max height
  as a visual detail still to decide). Keep a scroll offset and handle `WM_MOUSEWHEEL`,
  accumulating `WHEEL_DELTA` (120) for precision touchpads [wheel]. Draw a 6 DIP thin
  scrollbar like Parla's `.card-body`.
- Risk: `WM_MOUSEWHEEL` is "sent to the focus window" [wheel], and the Shelf never has focus.
  On Windows 10/11 the user setting "Scroll inactive windows when I hover over them" (on by
  default) routes the wheel to the window under the cursor. Verify by hand. If it fails, a
  low-level mouse hook while the Shelf is shown is the fallback.

### Crates

- **`windows` = "=0.62.2"**, the latest on crates.io on 2026-10-19 [crates-io]. COM is
  needed (D2D, DirectWrite, Shell, OLE drag and drop), and `windows-sys` "lacks support for
  COM and WinRT APIs" [win-or-sys]. Pin it exactly. windows-rs has announced `0.100.0`,
  which moves native APIs to new header-derived metadata with "changes to Cargo features,
  imports, function calls, types". The `windows`/`windows-sys` umbrellas "may initially" be
  held back [newsletter]. Migrate in one deliberate step later, or generate only the
  needed APIs with `windows-bindgen`, as the newsletter suggests [newsletter].
- **Skip `windows-window` 0.100** (HWND + message loop, depends only on `windows-core`)
  [windows-window]. It has `ex_style` and `no_redirection_bitmap`, but it registers a shared
  class and owns the WndProc dispatch. A hand-rolled `WNDCLASSW` + WndProc for one popup is
  about 60 lines and keeps OLE drop-target plumbing under our control.
- **Skip `windows-canvas` 0.100** (new in Sept 2026, about 650 downloads). It wraps
  D2D/D3D11/DXGI with GPU and WARP devices and swap chains [canvas], which is path C's
  memory profile.
- **Skip `tiny-skia`** (no text) [tiny-skia] and any windowing framework.
- Release profile: `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`.

## Memory and size

Measured on 2026-10-19 with throwaway probes outside the repo (`%TEMP%\tasca-render-probe`,
`windows` 0.62.2, the release profile above), one binary per path, each drawing the panel,
hairline and one line of text:

| Probe | Release exe |
|---|---|
| A: GDI + DWM round corners | 110,080 B |
| B: layered + D2D software + DWrite | 112,128 B |
| C: DComp + D3D11 + D2D + DWrite (+ shell thumbnail code) | 123,392 B |

- **Exe size estimate for Tasca v1:** 0.3–0.7 MB. The rendering path adds almost nothing
  (all system DLLs). The bulk will come from OLE `IDropTarget`/`IDropSource`/`IDataObject`
  implementations, file handling and std. Not measured beyond the probes.
- **Idle memory, measured once:** probe C plus `OleInitialize` and one
  `IShellItemImageFactory::GetImage` (a large wallpaper JPEG) was at 65.1 MB working set,
  59.4 MB private bytes and 34.4 MB private working set 4 s after start. That was a single
  unscheduled reading. Further readings for A and B were not taken because launching
  windows on the shared machine was refused. Take them in an assigned turn (TASCA-8 or the
  first build slice).
- **Idle memory estimate for B:** about 8–15 MB working set, of which about 4–8 MB private.
  That covers the Rust process plus D2D/DWrite factories and the DIB (about 0.5 MB), then OLE
  and the shell stack once the first thumbnail loads. This is an estimate, not a
  measurement. The D3D11 device and GPU driver are what path B leaves out.

## Open risks

1. **Mockup fidelity vs leanness.** B gives grayscale text, not ClearType [d2d-aa]. Mattia
   should judge it on screen. If he dislikes it, path A gives ClearType but has 8 px
   corners and no translucency.
2. **Shadow band.** A drawn shadow makes a margin of alpha > 0 pixels that catches clicks
   and drops [layered]. Keep it small, or drop the shadow.
3. **Wheel without focus.** Depends on the "Scroll inactive windows" setting [wheel].
4. **Drop during the fade.** Hit-testing at low `SourceConstantAlpha` is undocumented (TASCA-3/8).
5. **`windows` 0.100 churn.** Pin 0.62.2; the migration is a separate, deliberate job [newsletter].
6. **Memory numbers.** One sample for C, estimates for B; measure in an assigned turn.

## Not measured

No timings were taken (shared-machine rule): no render time per frame, no `GetImage`
latency, no show-to-visible latency. Only static exe sizes and one memory sample.

## Sources

- [corners] Microsoft Learn, *Apply rounded corners in desktop apps*: https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/ui/apply-rounded-corners
- [geometry] Microsoft Learn, *Geometry in Windows 11* (8 px / 4 px radii): https://learn.microsoft.com/en-us/windows/apps/design/signature-experiences/geometry
- [dwmattr] Microsoft Learn, `DWMWINDOWATTRIBUTE`: https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute
- [backdrop] Microsoft Learn, `DWM_SYSTEMBACKDROP_TYPE` (22621+): https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwm_systembackdrop_type
- [acrylic] Microsoft Learn, *Acrylic material* (solid fallback when deactivated): https://learn.microsoft.com/en-us/windows/apps/design/style/acrylic
- [fizzy] fizzyedit/fizzy PR #238 (Zig app, Acrylic `WS_EX_NOACTIVATE` popups; secondary, real-app evidence): https://github.com/fizzyedit/fizzy/pull/238
- [layered] Microsoft Learn, *Window Features*, Layered Windows: https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features#layered-windows
- [ulw] Microsoft Learn, `UpdateLayeredWindow`: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-updatelayeredwindow
- [kerr09] Kenny Kerr, *Layered Windows with Direct2D*, MSDN Magazine Dec 2009: https://learn.microsoft.com/en-us/archive/msdn-magazine/2009/december/windows-with-c-layered-windows-with-direct2d
- [kerr14] Kenny Kerr, *High-Performance Window Layering Using the Windows Composition Engine*, MSDN Magazine Jun 2014: https://learn.microsoft.com/en-us/archive/msdn-magazine/2014/june/windows-with-c-high-performance-window-layering-using-the-windows-composition-engine
- [d2d-aa] Microsoft Learn, `D2D1_TEXT_ANTIALIAS_MODE` remarks: https://learn.microsoft.com/en-us/windows/win32/api/d2d1/ne-d2d1-d2d1_text_antialias_mode
- [d2d-gdi] Microsoft Learn, *Direct2D and GDI Interoperability Overview*: https://learn.microsoft.com/en-us/windows/win32/direct2d/direct2d-and-gdi-interoperation-overview
- [dcomp-anim] Microsoft Learn, *Animation (DirectComposition)*: https://learn.microsoft.com/en-us/windows/win32/directcomp/animation
- [animatewindow] Microsoft Learn, `AnimateWindow`: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-animatewindow
- [spi] Microsoft Learn, `SystemParametersInfoW` (`SPI_GETCLIENTAREAANIMATION`): https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-systemparametersinfow
- [dpi] Microsoft Learn, *High DPI Desktop Application Development on Windows*: https://learn.microsoft.com/en-us/windows/win32/hidpi/high-dpi-desktop-application-development-on-windows
- [dpichanged] Microsoft Learn, `WM_DPICHANGED`: https://learn.microsoft.com/en-us/windows/win32/hidpi/wm-dpichanged
- [getdpiformonitor] Microsoft Learn, `GetDpiForMonitor`: https://learn.microsoft.com/en-us/windows/win32/api/shellscalingapi/nf-shellscalingapi-getdpiformonitor
- [getimage] Microsoft Learn, `IShellItemImageFactory::GetImage`: https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-ishellitemimagefactory-getimage
- [shgetfileinfo] Microsoft Learn, `SHGetFileInfoW`: https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shgetfileinfow
- [dodragdrop] Microsoft Learn, `DoDragDrop`: https://learn.microsoft.com/en-us/windows/win32/api/ole2/nf-ole2-dodragdrop
- [wheel] Microsoft Learn, `WM_MOUSEWHEEL`: https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-mousewheel
- [win-or-sys] Kenny Kerr, *windows or windows-sys*: https://kennykerr.ca/rust-getting-started/windows-or-windows-sys.html
- [newsletter] windows-rs, *Rust for Windows – August 2026* (issue #4867): https://github.com/microsoft/windows-rs/issues/4867
- [windows-window] windows-rs docs, `windows-window`: https://github.com/microsoft/windows-rs/blob/master/docs/crates/windows-window.md
- [canvas] windows-rs docs, `windows-canvas`: https://github.com/microsoft/windows-rs/blob/master/docs/crates/windows-canvas.md
- [dcomp-sample] windows-rs `dcomp` sample (D3D11 + D2D + DComp + DWrite in Rust): https://github.com/microsoft/windows-rs/tree/master/crates/samples/windows/dcomp
- [tiny-skia] linebender/tiny-skia README: https://github.com/linebender/tiny-skia
- [crates-io] crates.io API, `windows` max stable 0.62.2 (checked 2026-10-19): https://crates.io/crates/windows
