# What do Mattia's apps hand Tasca when he drags from them? (TASCA-5)

Research note, 2026-10-19. No code, no timings. Anything marked **LIVE** can
only be settled by a real drag on Mattia's machine (a format dump in a
throwaway drop target, or ClipSpy-style tool).

## Answer in one paragraph

Every source falls into one of four shapes: **real paths** (`CF_HDROP`),
**delay-rendered real paths** (`CF_HDROP` that only exists after the drop and
only through `IDataObjectAsyncCapability`), **virtual files**
(`FileGroupDescriptorW`/`FileGroupDescriptor` + `FileContents` by `lindex`),
and **bare data** (URL, text, bitmap). Tasca needs one ordered reader for those
four, not per-app code. Chromium's own drop target (Brave, Edge, WebView2 and
Electron all share it) already does exactly this and is the best reference
implementation. Tasca always answers with `DROPEFFECT_COPY` (or `LINK` when copy
is not allowed), never `MOVE`.

## Formats, in Tasca's order of preference

| # | Format (registered name) | Tasca does |
|---|---|---|
| 1 | `CF_HDROP` (real paths) | **Reference** each path. Exception: paths inside the temp folder (`GetTempPath2W`, but not `%TEMP%\Tasca` itself) are copied into the drop folder during `Drop` (**Materialized**), because their source deletes them (7-Zip, Chromium downloads; see below). |
| 1a | `CF_HDROP`, delay-rendered (`GetData` fails during the drag; async mode on) | `StartOperation`, then `GetData(CF_HDROP)`, then treat as row 1 (they are temp paths, so copy), then `EndOperation`. |
| 2 | `FileGroupDescriptorW`, else `FileGroupDescriptor` (ANSI, decode with `CP_ACP`) + `FileContents` at `lindex` *i* | Materialize each entry. Ask for `TYMED_ISTREAM | TYMED_HGLOBAL | TYMED_ISTORAGE`; stream to disk in chunks; `ISTORAGE` → `StgCreateStorageEx` + `IStorage::CopyTo` (a `.msg`). |
| 3 | `UniformResourceLocatorW` (`CFSTR_INETURLW`), else `UniformResourceLocator` | Materialize a `.url` (`[InternetShortcut]\r\nURL=…\r\n`). Title from `text/x-moz-url` line 2 if present. |
| 4 | `CF_UNICODETEXT` (else `CF_TEXT`) | Materialize a UTF-8 `.txt`. |
| 5 | `PNG`, else `CF_DIBV5`/`CF_DIB` | Optional for v1: no listed source is known to offer only a bitmap. `PNG` bytes go straight to a `.png`; a DIB needs WIC encoding. |
| – | `HTML Format`, `text/html`, `Shell IDList Array`, `chromium/*`, drag-image formats | Ignore. `Shell IDList Array` alone (Control Panel items, etc.) → refuse the drop. |

Rules behind the order:

- **Real files beat virtual files.** Microsoft: "Existing files should be
  offered with the CF_HDROP format" and "files from providers such as namespace
  extensions generally use CFSTR_FILECONTENTS/CFSTR_FILEDESCRIPTOR"
  ([Handling Shell Data Transfer Scenarios](https://learn.microsoft.com/en-us/windows/win32/shell/datascenarios)).
  Chromium's target does the same: "Favor real files on the file system over
  virtual files" (`HasVirtualFilenames`,
  [clipboard_util_win.cc](https://chromium.googlesource.com/chromium/src/+/main/ui/base/clipboard/clipboard_util_win.cc)).
- **Virtual files beat URL and text.** Chromium puts an image's bytes as
  `FileContents` *before* the URL "to prefer file content data over a shortcut",
  and its link drags already carry a synthetic `<title>.url` virtual file
  ([web_contents_view_aura.cc `PrepareDragData`](https://chromium.googlesource.com/chromium/src/+/main/content/browser/web_contents/web_contents_view_aura.cc),
  [os_exchange_data_provider_win.cc `SetURLs`](https://chromium.googlesource.com/chromium/src/+/main/ui/base/dragdrop/os_exchange_data_provider_win.cc)).
  So a browser link Parks as the same `.url` Explorer would create, with no
  URL-specific code on Tasca's side. Rows 3–4 only catch non-Chromium sources.
- **Do not trust `EnumFormatEtc` order blindly.** Microsoft says sources list
  formats best-first ([datascenarios](https://learn.microsoft.com/en-us/windows/win32/shell/datascenarios)),
  but Chromium's `SetData` inserts at the front and its own comment admits the
  setters disagree on order. Probe with `QueryGetData` in Tasca's fixed order instead.

## Per-source table

| Source | Formats offered (what arrives) | Tasca's handling | Materialized name | Explorer does for the same drop |
|---|---|---|---|---|
| **Explorer / desktop** (files, folders, search results, OneDrive placeholders) | `CF_HDROP`, `Shell IDList Array`, `Preferred DropEffect`, drag-image formats ([Shell Clipboard Formats](https://learn.microsoft.com/en-us/windows/win32/shell/clipboard)) | Row 1: Reference. Multiple paths = one Stack. Folders are References too. | n/a | Copy/move by volume and modifiers. |
| **Explorer, inside a .zip** (Compressed Folders) | Virtual files: `FileGroupDescriptorW` + `FileContents`; descriptor has names, no archive path ([Qt forum](https://forum.qt.io/post/272589)). Whether a temp `CF_HDROP` is also offered: **LIVE**. | Row 2 (or row 1 with the temp rule, if `CF_HDROP` shows up). Recreate directory entries (`FD_ATTRIBUTES` + `FILE_ATTRIBUTE_DIRECTORY`) and relative `a\b.txt` names under the drop folder. | `cFileName` as given | Extracts using `cFileName`; conflict dialog on clash. |
| **7-Zip File Manager**, from an archive | `CF_HDROP` listing paths in a fresh `%TEMP%\7zE…` folder; 7-Zip extracts there when the button is released, and **deletes the temp folder after `DoDragDrop` returns**. Before the release, `GetData(CF_HDROP)` returns only the temp folder path ("Pre" list). Allowed effects `MOVE|COPY`. ([PanelDrag.cpp](https://github.com/ip7z/7zip/blob/main/CPP/7zip/UI/FileManager/PanelDrag.cpp), comments on `m_hGlobal_HDROP_Pre/Final` and "we delete temp folder after DoDragDrop()") | Row 1 with the temp rule: copy during `Drop`. Read `CF_HDROP` **in `Drop` only**, never cache it from `DragEnter` (7-Zip notes Edge got the stale "Pre" list that way). | Original names (they are real files by then) | Moves/copies the extracted files (7-Zip prefers `MOVE` from its temp folder). |
| **Brave / Edge: selected text** | `CF_UNICODETEXT`, `CF_TEXT`, `HTML Format`, `text/html` (`SetString`, `SetHtml`) | Row 4. | First line of text, ≤ 40 chars, sanitized; empty → `Text 2026-10-19 101530.txt` | Probably refuses (no file format offered): **LIVE**. |
| **Brave / Edge: link** | `text/x-moz-url` (URL + title), `FileGroupDescriptorW` + `FileContents` for `<title>.url`, `chromium/x-ignore-file-contents`, `UniformResourceLocatorW`/`UniformResourceLocator`, `CF_UNICODETEXT` (`SetURLs`) | Row 2 (Chromium's synthetic `.url`). | Chromium's: page title with illegal chars → `-`, truncated to ~125 chars, `+ .url`; no title → name from the URL (`CreateValidFileNameFromTitle`) | Creates the same `<title>.url`. |
| **Brave / Edge: image** | `FileGroupDescriptorW` + `FileContents` (image bytes, `TYMED_ISTREAM|HGLOBAL`), then the link formats and HTML. **No** `CF_DIB`/`PNG`. The image file is only offered if its extension maps to an `image/*` type; otherwise only the `.url` remains. ([`GetSafeFilenameForImageFileContents`](https://chromium.googlesource.com/chromium/src/+/main/content/public/common/drop_data.cc)) | Row 2: take descriptor index 0 (the image); the `.url` is only added when there are no image contents. | From URL / `Content-Disposition` with the right image extension | Saves the image file under that name. |
| **Brave / Edge: page that sets `DownloadURL`** (Gmail-style attachment drags; [web.dev case study](https://web.dev/case-studies/box-dnd-download)) | Delay-rendered `CF_HDROP`; the data object forces async mode on; `GetData` fails while the drag loop runs and, in async mode, until the target calls `StartOperation`. The file downloads into `%TEMP%\chrome_dragNNN\` and is deleted on reboot. (`SetDownloadFileInfo`, `DataObjectImpl::GetData`, [os_exchange_data_provider_win.cc](https://chromium.googlesource.com/chromium/src/+/main/ui/base/dragdrop/os_exchange_data_provider_win.cc); `PrepareDragForDownload`) | Row 1a. **Without `StartOperation` Tasca gets nothing.** | Chromium's (`net::GenerateFileName` from the suggested name/MIME) | Explorer runs the async dance and gets the file. |
| **Brave / Edge: download bubble / downloads page** | `CF_HDROP` of the finished file in Downloads; allowed effects `COPY|LINK` only ([drag_download_item_aura.cc](https://chromium.googlesource.com/chromium/src/+/main/chrome/browser/download/drag_download_item_aura.cc)). Edge's own flyout is Edge UI on Chromium: **LIVE**. | Row 1: Reference (not in temp). | n/a | Copies the file. |
| **New Outlook** (`olk.exe`, WebView2) | Mails: a `CF_HDROP` of an `.eml` (a user found `CF_HDROP` missing from the enum when the drag broke; Microsoft says mails drag out as `.eml`, never `.msg`) ([Q&A thread](https://learn.microsoft.com/en-us/answers/a/12410472), [Q&A .msg](https://learn.microsoft.com/en-us/answers/questions/5915469/how-can-i-drag-and-drop-a-msg-from-the-new-outlook)). Mail drag has been flaky since Nov 2025 (must be selected; sometimes needs a restart). Attachments: reports conflict ("not supported, a known limitation" vs "still works"). Whether the `CF_HDROP` is a Chromium `DownloadURL` (row 1a) or a host-written temp file: **LIVE**. | Rows 1/1a with the temp rule: copy during `Drop`. | Outlook's (subject-based `.eml`; attachment's own name) | `.eml` file; attachments unreliable. |
| **Classic Outlook** (`outlook.exe`, not Mattia's main client, for reference) | Virtual files. Messages: `FileContents` as `TYMED_ISTORAGE` (a `.msg`); attachments: `TYMED_ISTREAM` (Chromium comments in `clipboard_util_win.cc`). Several mails with the same subject give duplicate `cFileName`s. The "ANSI descriptor only" claim is not settled: a Qt user saw both `FileGroupDescriptor` and `FileGroupDescriptorW` ([Qt forum](https://forum.qt.io/post/341793)). | Row 2; handle both A and W; uniquify names. | `cFileName` (subject + `.msg`, or the attachment name) | `.msg` / attachment file. |
| **WhatsApp desktop** (WebView2 wrapper of web.whatsapp.com since the 2025 rewrite; [Notebookcheck](https://www.notebookcheck.net/WhatsApp-ditches-native-Windows-11-app-in-favor-of-Microsoft-WebView2.1063682.0.html)) | Chromium web drags, so the Brave rows apply (text, link, image as virtual file). Images are often `blob:` URLs, so the synthesized name may be a GUID. Whether documents can be dragged out at all, and via `DownloadURL`: **LIVE**. | As Brave rows. | Chromium's; could be `3f2a…jpg` | Same. |
| **Claude desktop** (Electron = Chromium content layer) | Chromium web drags for text, links and images. Any file drag uses Electron `webContents.startDrag`, which gives real-path `CF_HDROP`. Whether attachments or artifacts can be dragged: **LIVE**. | As Brave rows / row 1. | Chromium's | Same. |
| **Windows Terminal** | Not a drag source: a left-drag selects text, and the selection is copied with right-click or Ctrl+C ([Terminal selection docs](https://learn.microsoft.com/en-us/windows/terminal/selection)). Terminal only accepts drops. | Nothing to handle. To Park terminal text, Mattia copies it and drags from elsewhere (out of scope for v1). **LIVE** to confirm no drag starts. | – | – |

## Known traps and how Tasca avoids them

1. **Peeking during the drag.** Chromium's delay-rendered `CF_HDROP` returns
   `DV_E_FORMATETC` while `in_drag_loop_` is set. 7-Zip's `CF_HDROP` holds a
   placeholder list until the button is released. So `DragEnter`/`DragOver`
   use only `QueryGetData`, never `GetData`, and every byte is read in
   `Drop` or after it. (Microsoft also warns that rendering during the drag
   can stall the cursor: `CFSTR_INDRAGLOOP`,
   [Shell Clipboard Formats](https://learn.microsoft.com/en-us/windows/win32/shell/clipboard).)
2. **Async data objects.** In `Drop`: QI `IDataObjectAsyncCapability`; if
   `GetAsyncMode` is TRUE, call `StartOperation`, marshal the `IDataObject`
   (`CoMarshalInterThreadInterfaceInStream`), return from `Drop` at once, do the
   reads on a worker thread, then call `EndOperation(hr, null, DROPEFFECT_COPY)`.
   This is Microsoft's documented target procedure
   ([datascenarios, "Using IASyncOperation/IDataObjectAsyncCapability"](https://learn.microsoft.com/en-us/windows/win32/shell/datascenarios))
   and what Chromium's target does (`GetVirtualFilesAsTempFiles`). It also
   frees the source's UI during big copies. If marshalling fails, call
   `EndOperation(E_FAIL…)` and fall back to reading synchronously inside `Drop`.
3. **Virtual files must be read before the source lets go.** Without async
   mode, Microsoft's synchronous model is: extract in `Drop`, then return. Only
   `StartOperation` lets the target keep the data object usable past `Drop`.
   Sources such as 7-Zip delete their temp files the moment `DoDragDrop` returns.
   So: no async → copy everything inside `Drop`.
4. **Large virtual files.** Never ask for `HGLOBAL` only. Prefer `ISTREAM`, seek
   to 0 first (a `GetData` may leave the pointer at the end), and copy in chunks
   straight to the file (Chromium uses 16 MB chunks). `FD_FILESIZE` may be
   missing, so do not pre-size. With an `HGLOBAL`, trust `nFileSize*` over
   `GlobalSize` ("the size of a global memory block is not byte-precise",
   datascenarios). On an error part-way, delete the partial file and drop that entry.
5. **ANSI and Unicode descriptors.** Check `FileGroupDescriptorW` first, then
   `FileGroupDescriptor`. Chromium checks both because "nothing prevents the drag
   source app from using the CFSTR_FILEDESCRIPTORA ANSI format".
6. **Hostile or odd names.** Drag sources can put path separators in
   `cFileName` (Chromium strips them with `BaseName`). Tasca keeps relative
   paths only for genuine directory trees (zip folders): reject `..`, drive
   letters and absolute paths. Replace `<>:"/\|?*` and control characters,
   trim trailing dots and spaces, avoid reserved device names (`CON`, `NUL`…),
   cap the stem at ~128 chars (Chromium's `kTruncatedDisplayNameLength`).
7. **Duplicate names in one drop** (several Outlook mails with one subject):
   `name (2).ext`, case-insensitive (Chromium's `GetUniqueVirtualFilename`).
8. **Elevation.** A drop from a normal app onto an elevated window is blocked
   by UIPI, so the Shelf must run unelevated (already decided in TASCA-2; how,
   in TASCA-4).

## Naming and layout of Materialized files

- One folder per Park: `%TEMP%\Tasca\<drop-id>\` (e.g. a monotonic counter or
  timestamp). Original names then survive unchanged: no cross-drop clashes, a
  Stack is one folder, and removing an Item deletes one folder. When an Item
  is Taken out, the file carries the name Explorer would have given it.
- Name source, in order: `cFileName` from the descriptor → Chromium's `.url`
  title (comes for free via the descriptor) → for a bare URL, the
  `text/x-moz-url` title or the URL host → for text, the first line →
  timestamp fallback `Text yyyy-MM-dd HHmmss.txt`.
- Temp-path `CF_HDROP` copies keep their file names.
- `CopyFileW` keeps alternate data streams, so the `Zone.Identifier`
  (Mark of the Web) that Chromium puts on download files survives. Materialized
  browser bytes (row 2) get no Mark of the Web; acceptable for v1, noted as a risk.

## Which drop effect to return

- In `DragEnter`/`DragOver`/`Drop`: if any accepted format is present, answer
  `DROPEFFECT_COPY` if it is in the allowed set, else `DROPEFFECT_LINK` if
  allowed, else `DROPEFFECT_NONE`. **Ignore `grfKeyState`** (Shift would
  normally mean move). **Never return `DROPEFFECT_MOVE`.**
- Why: the source decides whether to delete from what the target reports. For
  an unoptimized move "if both are set to DROPEFFECT_MOVE, [the source]
  completes the unoptimized move by deleting the original data"
  ([datascenarios, "Handling Optimized Move Operations"](https://learn.microsoft.com/en-us/windows/win32/shell/datascenarios)).
  Chromium web pages would also see `dropEffect = "move"` in `dragend` and may
  remove the dragged element. `COPY` means "the original is still there" in
  every source's eyes (`CFSTR_LOGICALPERFORMEDDROPEFFECT`, [Shell Clipboard Formats](https://learn.microsoft.com/en-us/windows/win32/shell/clipboard)).
- Do not call `SetData(CFSTR_PERFORMEDDROPEFFECT)`: it only matters for moves.
  Never send `CFSTR_TARGETCLSID` (that is how the Recycle Bin asks a source to
  delete).
- `COPY` over `LINK` for real files: it is the cursor users expect for "put it
  on the shelf", and Chromium download items allow only `COPY|LINK` while web
  drags may not allow `LINK`. A `LINK`-only source (rare) still works.
- If the copy or extraction fails, return `DROPEFFECT_NONE` from `Drop` (sync
  case) or `EndOperation(E_FAIL, …, DROPEFFECT_NONE)` (async case).

## Open risks

- **LIVE**: what new Outlook really offers for mails and attachments, whether
  it is delay-rendered (`DownloadURL`) or a host-written temp file, and whether
  attachment drag works at all on Mattia's build.
- **LIVE**: Explorer's zip folders: virtual only, or also a temp `CF_HDROP`;
  folder trees (relative names, directory entries).
- **LIVE**: WhatsApp (image names from `blob:` URLs; documents draggable?),
  Claude desktop (anything beyond text/links/images?), and Edge's download
  flyout.
- **LIVE**: Windows Terminal starts no drag (expected).
- New Outlook mail drag has been unreliable since Nov 2025 on Microsoft's side;
  Tasca cannot fix a drag the source never offers.
- Sync fallback for big virtual files blocks both apps until the copy ends.
  Acceptable for v1; async sources (Chromium, Explorer) avoid it.

## Sources

- Microsoft Learn: [Shell Clipboard Formats](https://learn.microsoft.com/en-us/windows/win32/shell/clipboard);
  [Handling Shell Data Transfer Scenarios](https://learn.microsoft.com/en-us/windows/win32/shell/datascenarios);
  [FILEDESCRIPTORW](https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/ns-shlobj_core-filedescriptorw);
  [Windows Terminal selection](https://learn.microsoft.com/en-us/windows/terminal/selection).
- The Old New Thing, "What a drag" series (virtual files as
  [HGLOBAL](https://devblogs.microsoft.com/oldnewthing/20080318-00/?p=23083),
  [IStream](https://devblogs.microsoft.com/oldnewthing/20080319-00/?p=23073),
  [IStorage](https://devblogs.microsoft.com/oldnewthing/20080320-00/?p=23063);
  [multiple virtual objects](https://devblogs.microsoft.com/oldnewthing/20080331-00/?p=22933)).
- Chromium source (main, read 2026-10-19): `ui/base/dragdrop/os_exchange_data_provider_win.cc`
  (source side), `ui/base/clipboard/clipboard_util_win.cc` (target side, the
  reference implementation), `content/browser/web_contents/web_contents_view_aura.cc`,
  `content/public/common/drop_data.cc`, `ui/base/dragdrop/drag_source_win.cc`,
  `chrome/browser/download/drag_download_item_aura.cc`.
- 7-Zip source: [CPP/7zip/UI/FileManager/PanelDrag.cpp](https://github.com/ip7z/7zip/blob/main/CPP/7zip/UI/FileManager/PanelDrag.cpp).
- New Outlook behaviour: Microsoft Q&A threads [12410472](https://learn.microsoft.com/en-us/answers/a/12410472) and
  [5915469](https://learn.microsoft.com/en-us/answers/questions/5915469/how-can-i-drag-and-drop-a-msg-from-the-new-outlook)
  (community and moderator answers, not documentation).
- Classic Outlook descriptors: [Qt forum](https://forum.qt.io/post/341793) (community).
- `DownloadURL`: [web.dev Box case study](https://web.dev/case-studies/box-dnd-download).
- WhatsApp on WebView2: [Notebookcheck](https://www.notebookcheck.net/WhatsApp-ditches-native-Windows-11-app-in-favor-of-Microsoft-WebView2.1063682.0.html).
