// PROTOTYPE, throwaway. TASCA-8 "Does Caps+D catch a live drag?".
// Branch prototype/catch-drag only, never merged. No tests, minimal error
// handling: the point is the log in spike.log next to the exe.
//
// `tasca-spike toggle` (from Hyperkey's Caps+D): warm = FindWindowW + PostMessageW;
// cold and elevated = relaunch through explorer.exe; cold and unelevated = become
// the resident. The resident owns one hidden layered Shelf, registered once as an
// OLE drop target, drawn with software Direct2D + DirectWrite.
#![windows_subsystem = "windows"]

use std::cell::{Cell, RefCell};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Security::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::*;
use windows::Win32::System::Performance::*;
use windows::Win32::System::SystemInformation::*;
use windows::Win32::System::SystemServices::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("Tasca.Shelf");
const MUTEX: PCWSTR = w!("Local\\TascaSpike.Resident");
const WM_TOGGLE: u32 = WM_APP + 1;
const WM_SHOWSHELF: u32 = WM_APP + 2;
const WM_MOUSELEAVE: u32 = 0x02A3; // not exported by the features we enable
const T_ANIM: usize = 1;
const T_HIDE_AFTER_PARK: usize = 2;

// Layout in DIPs, from mockup C.
const PANEL_W: f32 = 200.0;
const PAD: f32 = 8.0;
const HEAD_H: f32 = 22.0;
const HEAD_EMPTY_H: f32 = 48.0; // Mattia (TASCA-8): the empty strip at 32 DIP was too small; doubled to 64.
const ROW_H: f32 = 40.0;
const EDGE_GAP: f32 = 8.0;

// ---------------------------------------------------------------- log + clock

static CLOCK: OnceLock<(i64, f64)> = OnceLock::new();
static ROLE: OnceLock<&'static str> = OnceLock::new();

fn now_ms() -> f64 {
    let (t0, freq) = *CLOCK.get_or_init(|| unsafe {
        let (mut t, mut f) = (0i64, 0i64);
        let _ = QueryPerformanceCounter(&mut t);
        let _ = QueryPerformanceFrequency(&mut f);
        (t, f as f64 / 1000.0)
    });
    let mut t = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut t);
    }
    (t - t0) as f64 / freq
}

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default()
}

fn log(msg: impl AsRef<str>) {
    let t = unsafe { GetLocalTime() };
    let line = format!(
        "{:02}:{:02}:{:02}.{:03} {:<8} pid {:>6} +{:>9.1}ms  {}\n",
        t.wHour, t.wMinute, t.wSecond, t.wMilliseconds,
        ROLE.get().copied().unwrap_or("?"), std::process::id(), now_ms(), msg.as_ref()
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(exe_dir().join("spike.log")) {
        let _ = f.write_all(line.as_bytes());
    }
}

// ---------------------------------------------------------------- flags

#[derive(Clone, Copy, Default, Debug)]
struct Flags {
    under_cursor: bool,   // show the Shelf centred under the cursor (the "cursor still" test)
    no_anim: bool,        // no slide/fade
    fade_from_zero: bool, // fade starts at alpha 0 instead of ~40 %
    no_shadow: bool,      // no shadow margin
    focus_on_drag: bool,  // SetForegroundWindow(shelf) when a Take out starts
    allow_elevated: bool, // let an elevated process become the resident (negative control)
    hide_after_park: bool, // hide 450 ms after a Park (the TASCA-2 rule)
}

fn read_flags() -> Flags {
    let mut words: Vec<String> = std::fs::read_to_string(exe_dir().join("flags.txt"))
        .unwrap_or_default()
        .split_whitespace()
        .filter(|w| !w.starts_with('#'))
        .map(str::to_owned)
        .collect();
    words.extend(std::env::args().skip(1).filter_map(|a| a.strip_prefix("--").map(str::to_owned)));
    let has = |w: &str| words.iter().any(|x| x == w);
    Flags {
        under_cursor: has("under-cursor"),
        no_anim: has("no-anim"),
        fade_from_zero: has("fade-from-zero"),
        no_shadow: has("no-shadow"),
        focus_on_drag: has("focus-on-drag"),
        allow_elevated: has("allow-elevated"),
        hide_after_park: has("hide-after-park"),
    }
}

// ---------------------------------------------------------------- process helpers

fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elev = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elev.TokenIsElevated != 0
    }
}

fn process_name_of(hwnd: HWND) -> String {
    unsafe {
        if hwnd.is_invalid() {
            return "(none)".into();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return format!("pid {pid} (no access: elevated?)");
        };
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        if r.is_err() {
            return format!("pid {pid}");
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit('\\').next().unwrap_or(&full).to_owned()
    }
}

fn fg() -> String {
    unsafe { process_name_of(GetForegroundWindow()) }
}

fn relaunch_through_shell() {
    unsafe {
        if GetShellWindow().is_invalid() {
            log("cold+elevated: no shell window, giving up (would risk an elevated Shelf)");
            return;
        }
    }
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let exe = std::env::current_exe().unwrap_or_default();
    match std::process::Command::new(format!("{windir}\\explorer.exe")).arg(&exe).spawn() {
        Ok(_) => log(format!("cold+elevated: relaunched unelevated through explorer.exe {}", exe.display())),
        Err(e) => log(format!("cold+elevated: explorer.exe relaunch failed: {e}")),
    }
}

// ---------------------------------------------------------------- main

fn main() {
    now_ms();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let toggle = args.iter().any(|a| a == "toggle");
    let hidden = args.iter().any(|a| a == "--hidden");
    let flags = read_flags();
    let elevated = is_elevated();
    unsafe {
        if let Ok(hwnd) = FindWindowW(CLASS, PCWSTR::null()) {
            let _ = ROLE.set("helper");
            let msg = if toggle { WM_TOGGLE } else { WM_SHOWSHELF };
            let r = PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0));
            log(format!(
                "warm: posted {} to resident (helper elevated={elevated}, post={:?}, fg={})",
                if toggle { "TOGGLE" } else { "SHOW" },
                r.map_err(|e| e.code()),
                fg()
            ));
            return;
        }
        if OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, MUTEX).is_ok() {
            let _ = ROLE.set("helper");
            log("a resident is starting (mutex held, no window yet): dropping this press");
            return;
        }
        if elevated && !flags.allow_elevated {
            let _ = ROLE.set("helper");
            log(format!("cold: no resident; helper is elevated (args {args:?}, fg={})", fg()));
            relaunch_through_shell();
            return;
        }
        let _ = ROLE.set("resident");
        if let Err(e) = run_resident(flags, !hidden, elevated) {
            log(format!("resident failed: {e:?}"));
        }
    }
}

// ---------------------------------------------------------------- model

struct Item {
    paths: Vec<PathBuf>,
    name: String,
    sub: String,
    label: String,
    tile: (f32, f32, f32),
    label_color: (f32, f32, f32, f32),
    folder: bool,
}

fn human_size(b: u64) -> String {
    let b = b as f64;
    if b < 1024.0 * 1024.0 {
        format!("{:.0} KB", (b / 1024.0).max(1.0))
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    } else {
        format!("{:.1} GB", b / 1024.0 / 1024.0 / 1024.0)
    }
}

fn item_from_paths(paths: Vec<PathBuf>) -> Item {
    let first = paths[0].clone();
    let fname = first.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| first.display().to_string());
    let is_dir = first.is_dir();
    let ext = first.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
    let image = matches!(ext.as_str(), "PNG" | "JPG" | "JPEG" | "GIF" | "WEBP" | "HEIC" | "BMP");
    let total: u64 = paths.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
    let (name, sub) = if paths.len() == 1 {
        (fname, if is_dir { "Folder".into() } else { format!("{} · {}", if ext.is_empty() { "File".into() } else { ext.clone() }, human_size(total)) })
    } else {
        (format!("{fname} +{}", paths.len() - 1), format!("{} items · {}", paths.len(), human_size(total)))
    };
    let tile = if image { (0.76, 0.33, 0.42) } else { (0.165, 0.165, 0.19) };
    let label_color = if ext == "PDF" { (1.0, 0.42, 0.46, 1.0) } else { (1.0, 1.0, 1.0, 0.56) };
    Item { paths, name, sub, label: if is_dir || image { String::new() } else { ext.chars().take(4).collect() }, tile, label_color, folder: is_dir }
}

// ---------------------------------------------------------------- state

struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    w: i32,
    h: i32,
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum AnimKind {
    In,
    Out,
}

struct Anim {
    kind: AnimKind,
    start: f64,
    dur: f64,
    from_x: i32,
    to_x: i32,
    from_a: f32,
    to_a: f32,
}

struct Fonts {
    name: IDWriteTextFormat,
    sub: IDWriteTextFormat,
    head: IDWriteTextFormat,
    head_r: IDWriteTextFormat,
    label: IDWriteTextFormat,
    badge: IDWriteTextFormat,
}

struct State {
    hwnd: HWND,
    flags: Flags,
    d2d: ID2D1Factory,
    rt: Option<ID2D1DCRenderTarget>,
    fonts: Fonts,
    helper: Option<IDropTargetHelper>,
    items: Vec<Item>,
    visible: bool,
    hot: bool,
    hover: Option<usize>,
    scroll: f32,
    dpi: u32,
    work: RECT,
    cursor_at_show: POINT,
    pos: POINT,
    surf: Option<Surface>,
    anim: Option<Anim>,
    alpha: u8,
    toggle_at: f64,
    shown_at: f64,
    enter_at: f64,
    press: Option<(POINT, usize)>,
    in_own_drag: bool,
    own_drop_hit: bool,
    over_keys: u32,
    over_effect: u32,
    wheel_logged: bool,
    tracking_leave: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

// Short, non-reentrant access. Never call ShowWindow/SetWindowPos/DoDragDrop inside.
fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|c| match c.try_borrow_mut() {
        Ok(mut g) => g.as_mut().map(f),
        Err(_) => {
            log("(state busy: re-entrant call skipped)");
            None
        }
    })
}

impl State {
    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }
    fn margins(&self) -> (f32, f32, f32, f32) {
        // left, top, right, bottom
        if self.flags.no_shadow { (EDGE_GAP, 2.0, 2.0, 2.0) } else { (EDGE_GAP, 10.0, 16.0, 24.0) }
    }
    fn head_h(&self) -> f32 {
        if self.items.is_empty() { HEAD_EMPTY_H } else { HEAD_H }
    }
    fn rows_view_h(&self) -> f32 {
        let work_h = (self.work.bottom - self.work.top) as f32 / self.scale();
        let max = (work_h * 0.6 - 2.0 * PAD - HEAD_H).max(ROW_H);
        (self.items.len() as f32 * ROW_H).min((max / ROW_H).floor() * ROW_H)
    }
    fn panel_h(&self) -> f32 {
        PAD + self.head_h() + self.rows_view_h() + PAD
    }
    fn size_px(&self) -> (i32, i32) {
        let (ml, mt, mr, mb) = self.margins();
        let s = self.scale();
        (((ml + PANEL_W + mr) * s).ceil() as i32, ((mt + self.panel_h() + mb) * s).ceil() as i32)
    }
    fn final_pos(&self) -> POINT {
        let (_, h) = self.size_px();
        let (_, mt, _, _) = self.margins();
        let s = self.scale();
        if self.flags.under_cursor {
            let c = self.cursor_at_show;
            let x = c.x - ((EDGE_GAP + PANEL_W / 2.0) * s) as i32;
            let y = c.y - ((mt + self.panel_h() / 2.0) * s) as i32;
            return POINT { x, y };
        }
        let mid = (self.work.top + self.work.bottom) / 2;
        POINT { x: self.work.left, y: mid - h / 2 }
    }
    // Client px -> what is under it.
    fn hit(&self, x: i32, y: i32) -> Hit {
        let s = self.scale();
        let (x, y) = (x as f32 / s, y as f32 / s);
        let (ml, mt, _, _) = self.margins();
        if x < ml || x > ml + PANEL_W || y < mt || y > mt + self.panel_h() {
            return Hit::Margin;
        }
        let rows_top = mt + PAD + self.head_h();
        if y < rows_top {
            return if x > ml + PANEL_W - PAD - 64.0 && !self.items.is_empty() { Hit::ClearAll } else { Hit::Header };
        }
        let i = ((y - rows_top + self.scroll) / ROW_H).floor() as usize;
        if y < rows_top + self.rows_view_h() && i < self.items.len() { Hit::Row(i) } else { Hit::Panel }
    }

    fn render(&mut self) -> Result<()> {
        let (w, h) = self.size_px();
        if self.surf.as_ref().map(|s| (s.w, s.h)) != Some((w, h)) {
            self.surf = None;
            self.surf = Some(make_surface(w, h)?);
        }
        let surf = self.surf.as_ref().unwrap();
        if self.rt.is_none() {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            self.rt = Some(unsafe { self.d2d.CreateDCRenderTarget(&props)? });
        }
        let rt = self.rt.as_ref().unwrap();
        let s = self.scale();
        let (ml, mt, _, _) = self.margins();
        let ph = self.panel_h();
        let max_scroll = (self.items.len() as f32 * ROW_H - self.rows_view_h()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        unsafe {
            rt.SetDpi(self.dpi as f32, self.dpi as f32);
            rt.BindDC(surf.dc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
            rt.BeginDraw();
            rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            let rr = |l: f32, t: f32, r: f32, b: f32, rad: f32| D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: l, top: t, right: r, bottom: b },
                radiusX: rad,
                radiusY: rad,
            };
            let brush = |r: f32, g: f32, b: f32, a: f32| rt.CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a }, None).unwrap();
            let (pl, pt, pr, pb) = (ml, mt, ml + PANEL_W, mt + ph);

            // Shadow: stacked rounded rects, ~ 0 6px 18px rgba(0,0,0,.35).
            if !self.flags.no_shadow {
                for i in (0..10).rev() {
                    let e = i as f32 * 1.6;
                    let a = 0.045 * (1.0 - i as f32 / 10.0);
                    rt.FillRoundedRectangle(&rr(pl - e, pt + 4.0 - e * 0.6, pr + e, pb + 6.0 + e, 12.0 + e), &brush(0.0, 0.0, 0.0, a));
                }
            }
            // Glass, hairline, drag-hover outline.
            rt.FillRoundedRectangle(&rr(pl, pt, pr, pb, 12.0), &brush(18.0 / 255.0, 18.0 / 255.0, 20.0 / 255.0, 0.92));
            let hair = 1.0 / s;
            rt.DrawRoundedRectangle(&rr(pl + hair / 2.0, pt + hair / 2.0, pr - hair / 2.0, pb - hair / 2.0, 12.0), &brush(1.0, 1.0, 1.0, 0.14), hair, None);
            if self.hot {
                rt.DrawRoundedRectangle(&rr(pl - 0.75, pt - 0.75, pr + 0.75, pb + 0.75, 12.75), &brush(0x4c as f32 / 255.0, 0xc2 as f32 / 255.0, 1.0, 1.0), 1.5, None);
            }
            // Header.
            let t2 = brush(1.0, 1.0, 1.0, 0.56);
            let t1 = brush(1.0, 1.0, 1.0, 0.88);
            let head = if self.items.is_empty() { "Tasca".to_string() } else { format!("Tasca · {}", self.items.len()) };
            let hb = D2D_RECT_F { left: pl + PAD + 4.0, top: pt + PAD, right: pr - PAD - 4.0, bottom: pt + PAD + 15.0 };
            rt.DrawText(&wide(&head), &self.fonts.head, &hb, &t2, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
            if !self.items.is_empty() {
                rt.DrawText(&wide("Clear all"), &self.fonts.head_r, &hb, &t2, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
            }
            // Rows.
            let rows_top = pt + PAD + self.head_h();
            let view = D2D_RECT_F { left: pl, top: rows_top, right: pr, bottom: rows_top + self.rows_view_h() };
            rt.PushAxisAlignedClip(&view, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            for (i, it) in self.items.iter().enumerate() {
                let y = rows_top + i as f32 * ROW_H - self.scroll;
                if y + ROW_H < view.top || y > view.bottom {
                    continue;
                }
                let (x0, x1) = (pl + PAD, pr - PAD);
                if self.hover == Some(i) {
                    rt.FillRoundedRectangle(&rr(x0, y, x1, y + ROW_H, 7.0), &brush(1.0, 1.0, 1.0, 0.08));
                }
                let (tx, ty) = (x0 + 4.0, y + 5.0);
                let (r, g, b) = it.tile;
                rt.FillRoundedRectangle(&rr(tx, ty, tx + 30.0, ty + 30.0, 6.0), &brush(r, g, b, 1.0));
                if it.folder {
                    let fb = brush(0xe0 as f32 / 255.0, 0xb0 as f32 / 255.0, 0x4a as f32 / 255.0, 1.0);
                    rt.FillRoundedRectangle(&rr(tx + 7.0, ty + 10.5, tx + 23.5, ty + 23.0, 1.5), &fb);
                    rt.FillRoundedRectangle(&rr(tx + 7.0, ty + 8.5, tx + 14.5, ty + 11.5, 1.0), &fb);
                } else if !it.label.is_empty() {
                    let (lr, lg, lb, la) = it.label_color;
                    let tr = D2D_RECT_F { left: tx, top: ty, right: tx + 30.0, bottom: ty + 30.0 };
                    rt.DrawText(&wide(&it.label), &self.fonts.label, &tr, &brush(lr, lg, lb, la), D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                }
                if it.paths.len() > 1 {
                    let br = &rr(tx + 30.0 - 11.0, ty - 4.0, tx + 30.0 + 4.0, ty + 10.0, 7.0);
                    rt.FillRoundedRectangle(br, &brush(1.0, 1.0, 1.0, 1.0));
                    rt.DrawText(&wide(&it.paths.len().to_string()), &self.fonts.badge, &br.rect, &brush(0.07, 0.07, 0.08, 1.0), D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                }
                let mx = tx + 30.0 + 9.0;
                let nr = D2D_RECT_F { left: mx, top: ty + 0.5, right: x1 - 4.0, bottom: ty + 16.0 };
                rt.DrawText(&wide(&it.name), &self.fonts.name, &nr, &t1, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                let sr = D2D_RECT_F { left: mx, top: ty + 16.0, right: x1 - 4.0, bottom: ty + 30.0 };
                rt.DrawText(&wide(&it.sub), &self.fonts.sub, &sr, &t2, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
            }
            rt.PopAxisAlignedClip();
            if max_scroll > 0.0 {
                let vh = view.bottom - view.top;
                let total = self.items.len() as f32 * ROW_H;
                let th = (vh * vh / total).max(16.0);
                let ty = view.top + (vh - th) * (self.scroll / max_scroll);
                rt.FillRoundedRectangle(&rr(pr - 5.0, ty, pr - 2.0, ty + th, 1.5), &brush(1.0, 1.0, 1.0, 0.22));
            }
            rt.EndDraw(None, None)?;
        }
        Ok(())
    }

    // Push the current surface to the screen at (pos, alpha).
    fn present(&mut self, pos: POINT, alpha: u8) {
        let Some(surf) = self.surf.as_ref() else { return };
        self.pos = pos;
        self.alpha = alpha;
        let size = SIZE { cx: surf.w, cy: surf.h };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: AC_SRC_ALPHA as u8 };
        let r = unsafe { UpdateLayeredWindow(self.hwnd, None, Some(&pos), Some(&size), Some(surf.dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA) };
        if let Err(e) = r {
            log(format!("UpdateLayeredWindow failed: {e:?}"));
        }
    }

    // Re-render after a state change and keep the Shelf centred on its edge.
    fn refresh(&mut self) {
        if let Err(e) = self.render() {
            log(format!("render failed: {e:?}"));
            return;
        }
        if !self.visible {
            return;
        }
        let mut p = self.final_pos();
        if let Some(a) = &self.anim {
            p.x = self.pos.x; // the animation owns x
            let _ = a;
        }
        let alpha = self.alpha;
        self.present(p, alpha);
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Hit {
    Margin,
    Header,
    ClearAll,
    Panel,
    Row(usize),
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn make_surface(w: i32, h: i32) -> Result<Surface> {
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bmp = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, bmp.into());
        Ok(Surface { dc, bmp, old, w, h })
    }
}

fn text_format(dw: &IDWriteFactory, size: f32, weight: DWRITE_FONT_WEIGHT, align: DWRITE_TEXT_ALIGNMENT, center_v: bool) -> Result<IDWriteTextFormat> {
    unsafe {
        let f = dw
            .CreateTextFormat(w!("Segoe UI Variable Text"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))
            .or_else(|_| dw.CreateTextFormat(w!("Segoe UI"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us")))?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        f.SetTextAlignment(align)?;
        if center_v {
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        }
        let sign = dw.CreateEllipsisTrimmingSign(&f)?;
        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        f.SetTrimming(&trim, &sign)?;
        Ok(f)
    }
}

// ---------------------------------------------------------------- show / hide

fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

fn show_shelf(why: &str) {
    let hwnd = with(|st| {
        unsafe {
            let mut c = POINT::default();
            let _ = GetCursorPos(&mut c);
            st.cursor_at_show = c;
            let mon = MonitorFromPoint(c, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut mi);
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            st.work = mi.rcWork;
            st.dpi = dx;
            log(format!(
                "show ({why}): cursor ({},{}), monitor work {:?}x{:?} at ({},{}), dpi {dx} ({}%), fg={}",
                c.x, c.y, mi.rcWork.right - mi.rcWork.left, mi.rcWork.bottom - mi.rcWork.top, mi.rcWork.left, mi.rcWork.top,
                dx * 100 / 96, fg()
            ));
        }
        st.visible = true;
        st.hot = false;
        st.hover = None;
        st.wheel_logged = false;
        if let Err(e) = st.render() {
            log(format!("render failed: {e:?}"));
        }
        let to = st.final_pos();
        if st.flags.no_anim {
            st.anim = None;
            st.present(to, 255);
        } else {
            let from_x = to.x - (16.0 * st.scale()) as i32;
            let from_a = if st.flags.fade_from_zero { 0.0 } else { 0.4 };
            st.anim = Some(Anim { kind: AnimKind::In, start: now_ms(), dur: 180.0, from_x, to_x: to.x, from_a, to_a: 1.0 });
            st.present(POINT { x: from_x, y: to.y }, (from_a * 255.0) as u8);
        }
        st.hwnd
    });
    let Some(hwnd) = hwnd else { return };
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
    let animate = with(|st| {
        st.shown_at = now_ms();
        log(format!(
            "shown: on screen {:.1} ms after the toggle message (first frame; DWM adds up to one vblank), visible={}",
            st.shown_at - st.toggle_at,
            unsafe { IsWindowVisible(st.hwnd).as_bool() }
        ));
        st.anim.is_some()
    });
    if animate == Some(true) {
        unsafe { SetTimer(Some(hwnd), T_ANIM, 10, None) };
    }
}

fn hide_shelf(why: &str) {
    let r = with(|st| {
        if !st.visible {
            return None;
        }
        log(format!("hide ({why})"));
        if st.flags.no_anim {
            st.visible = false;
            return Some((st.hwnd, false));
        }
        let x = st.pos.x;
        st.anim = Some(Anim { kind: AnimKind::Out, start: now_ms(), dur: 120.0, from_x: x, to_x: x - (8.0 * st.scale()) as i32, from_a: st.alpha as f32 / 255.0, to_a: 0.0 });
        Some((st.hwnd, true))
    });
    match r.flatten() {
        Some((hwnd, true)) => unsafe {
            SetTimer(Some(hwnd), T_ANIM, 10, None);
        },
        Some((hwnd, false)) => unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        },
        None => {}
    }
}

fn anim_tick() {
    let done = with(|st| {
        let Some(a) = &st.anim else { return Some((st.hwnd, None)) };
        let t = ((now_ms() - a.start) / a.dur).clamp(0.0, 1.0);
        let e = ease_out(t);
        let x = a.from_x + ((a.to_x - a.from_x) as f64 * e).round() as i32;
        let alpha = ((a.from_a + (a.to_a - a.from_a) * e as f32) * 255.0).round().clamp(0.0, 255.0) as u8;
        let kind = a.kind;
        let y = st.final_pos().y;
        st.present(POINT { x, y }, alpha);
        if t >= 1.0 {
            st.anim = None;
            if kind == AnimKind::Out {
                st.visible = false;
            } else {
                log(format!("slide-in done ({:.0} ms after shown)", now_ms() - st.shown_at));
            }
            return Some((st.hwnd, Some(kind)));
        }
        None
    });
    if let Some(Some((hwnd, kind))) = done {
        unsafe {
            let _ = KillTimer(Some(hwnd), T_ANIM);
            if kind == Some(AnimKind::Out) {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

fn toggle() {
    let r = with(|st| {
        st.toggle_at = now_ms();
        if st.in_own_drag {
            log("toggle ignored: a Take out is in progress");
            return None;
        }
        let showing = st.visible && !matches!(st.anim, Some(Anim { kind: AnimKind::Out, .. }));
        Some(showing)
    });
    match r.flatten() {
        Some(true) => hide_shelf("toggle"),
        Some(false) => show_shelf("toggle"),
        None => {}
    }
}

// ---------------------------------------------------------------- data objects

fn cf_name(cf: u16) -> String {
    let std = match cf {
        1 => "CF_TEXT",
        2 => "CF_BITMAP",
        3 => "CF_METAFILEPICT",
        7 => "CF_OEMTEXT",
        8 => "CF_DIB",
        13 => "CF_UNICODETEXT",
        14 => "CF_ENHMETAFILE",
        15 => "CF_HDROP",
        16 => "CF_LOCALE",
        17 => "CF_DIBV5",
        _ => "",
    };
    if !std.is_empty() {
        return std.into();
    }
    let mut buf = [0u16; 256];
    let n = unsafe { GetClipboardFormatNameW(cf as u32, &mut buf) };
    if n > 0 { String::from_utf16_lossy(&buf[..n as usize]) } else { format!("#{cf}") }
}

fn tymed_str(t: u32) -> String {
    let mut v = vec![];
    for (bit, n) in [(1, "HGLOBAL"), (2, "FILE"), (4, "ISTREAM"), (8, "ISTORAGE"), (16, "GDI"), (32, "MFPICT"), (64, "ENHMF")] {
        if t & bit != 0 {
            v.push(n);
        }
    }
    v.join("|")
}

fn list_formats(obj: &IDataObject) -> Vec<String> {
    let mut out = vec![];
    unsafe {
        let Ok(e) = obj.EnumFormatEtc(DATADIR_GET.0 as u32) else { return vec!["(EnumFormatEtc failed)".into()] };
        loop {
            let mut f = [FORMATETC::default()];
            let mut n = 0u32;
            if e.Next(&mut f, Some(&mut n)) != S_OK || n == 0 {
                break;
            }
            let f = &f[0];
            out.push(format!("{}[{}{}]", cf_name(f.cfFormat), tymed_str(f.tymed), if f.lindex != -1 { format!(",lindex {}", f.lindex) } else { String::new() }));
            if !f.ptd.is_null() {
                CoTaskMemFree(Some(f.ptd as _));
            }
            if out.len() > 60 {
                out.push("…".into());
                break;
            }
        }
    }
    out
}

fn async_mode(obj: &IDataObject) -> String {
    unsafe {
        match obj.cast::<IDataObjectAsyncCapability>() {
            Ok(a) => format!(
                "async-capable (GetAsyncMode={:?}, InOperation={:?})",
                a.GetAsyncMode().map(|b| b.as_bool()).map_err(|e| e.code()),
                a.InOperation().map(|b| b.as_bool()).map_err(|e| e.code())
            ),
            Err(_) => "no IDataObjectAsyncCapability".into(),
        }
    }
}

fn read_hdrop(obj: &IDataObject) -> std::result::Result<Vec<PathBuf>, HRESULT> {
    unsafe {
        let fe = FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
        let mut m = obj.GetData(&fe).map_err(|e| e.code())?;
        let hdrop = HDROP(m.u.hGlobal.0);
        let n = DragQueryFileW(hdrop, u32::MAX, None);
        let mut v = vec![];
        for i in 0..n {
            let len = DragQueryFileW(hdrop, i, None);
            let mut buf = vec![0u16; len as usize + 1];
            DragQueryFileW(hdrop, i, Some(&mut buf));
            v.push(PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])));
        }
        ReleaseStgMedium(&mut m);
        Ok(v)
    }
}

fn read_dword(obj: &IDataObject, name: &str) -> Option<u32> {
    unsafe {
        let cf = RegisterClipboardFormatW(&HSTRING::from(name));
        let fe = FORMATETC { cfFormat: cf as u16, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
        let mut m = obj.GetData(&fe).ok()?;
        let h = m.u.hGlobal;
        let p = GlobalLock(h) as *const u32;
        let v = if p.is_null() { None } else { Some(*p) };
        let _ = GlobalUnlock(h);
        ReleaseStgMedium(&mut m);
        v
    }
}

fn effect_str(e: u32) -> String {
    if e == 0 {
        return "NONE".into();
    }
    let mut v = vec![];
    for (bit, n) in [(1, "COPY"), (2, "MOVE"), (4, "LINK")] {
        if e & bit != 0 {
            v.push(n);
        }
    }
    if e & 0x8000_0000 != 0 {
        v.push("SCROLL");
    }
    v.join("|")
}

fn keys_str(k: u32) -> String {
    let mut v = vec![];
    for (bit, n) in [(MK_LBUTTON.0, "L"), (MK_RBUTTON.0, "R"), (MK_SHIFT.0, "SHIFT"), (MK_CONTROL.0, "CTRL"), (MK_MBUTTON.0, "M"), (MK_ALT, "ALT")] {
        if k & bit != 0 {
            v.push(n);
        }
    }
    if v.is_empty() { "-".into() } else { v.join("+") }
}

fn choose_effect(allowed: DROPEFFECT, any_format: bool) -> DROPEFFECT {
    if !any_format {
        DROPEFFECT_NONE
    } else if allowed.0 & DROPEFFECT_COPY.0 != 0 {
        DROPEFFECT_COPY
    } else if allowed.0 & DROPEFFECT_LINK.0 != 0 {
        DROPEFFECT_LINK
    } else {
        DROPEFFECT_NONE
    }
}

// ---------------------------------------------------------------- IDropTarget (Park)

#[implement(IDropTarget)]
struct DropTarget;

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(&self, obj: Ref<IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, eff: *mut DROPEFFECT) -> Result<()> {
        let allowed = unsafe { *eff };
        let p = POINT { x: pt.x, y: pt.y };
        let Some((hwnd, helper, own, shown_at, toggle_at, visible)) =
            with(|st| (st.hwnd, st.helper.clone(), st.in_own_drag, st.shown_at, st.toggle_at, st.visible))
        else {
            unsafe { *eff = DROPEFFECT_NONE };
            return Ok(());
        };
        if let Some(h) = &helper {
            let _ = unsafe { h.DragEnter(hwnd, obj.as_ref(), &p, allowed) };
        }
        let (formats, asy) = match obj.as_ref() {
            Some(o) => (list_formats(o), async_mode(o)),
            None => (vec![], "no data object".into()),
        };
        let result = if own { DROPEFFECT_NONE } else { choose_effect(allowed, !formats.is_empty()) };
        unsafe { *eff = result };
        let now = now_ms();
        log(format!(
            "DragEnter at ({},{}): {:.0} ms after shown, {:.0} ms after toggle (visible={visible}); own drag={own}; allowed {}; keys {}; answer {}; fg={}",
            pt.x, pt.y, now - shown_at, now - toggle_at, effect_str(allowed.0), keys_str(keys.0), effect_str(result.0), fg()
        ));
        log(format!("  formats ({}): {}", formats.len(), formats.join(", ")));
        log(format!("  {asy}"));
        with(|st| {
            st.enter_at = now;
            st.over_keys = keys.0;
            st.over_effect = result.0;
            st.hot = !own;
            st.refresh();
        });
        Ok(())
    }

    fn DragOver(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, eff: *mut DROPEFFECT) -> Result<()> {
        let allowed = unsafe { *eff };
        let p = POINT { x: pt.x, y: pt.y };
        let r = with(|st| {
            let result = if st.in_own_drag { DROPEFFECT_NONE } else { choose_effect(allowed, true) };
            if keys.0 != st.over_keys || result.0 != st.over_effect {
                log(format!("DragOver: keys {} (was {}), allowed {}, answer {}", keys_str(keys.0), keys_str(st.over_keys), effect_str(allowed.0), effect_str(result.0)));
                st.over_keys = keys.0;
                st.over_effect = result.0;
            }
            (st.helper.clone(), result)
        });
        let (helper, result) = r.unwrap_or((None, DROPEFFECT_NONE));
        if let Some(h) = helper {
            let _ = unsafe { h.DragOver(&p, allowed) };
        }
        unsafe { *eff = result };
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        let helper = with(|st| {
            log(format!("DragLeave ({:.0} ms after DragEnter; Esc or left the Shelf)", now_ms() - st.enter_at));
            st.hot = false;
            st.refresh();
            st.helper.clone()
        })
        .flatten();
        if let Some(h) = helper {
            let _ = unsafe { h.DragLeave() };
        }
        Ok(())
    }

    fn Drop(&self, obj: Ref<IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, eff: *mut DROPEFFECT) -> Result<()> {
        let allowed = unsafe { *eff };
        let p = POINT { x: pt.x, y: pt.y };
        let Some((hwnd, helper, own, enter_at)) = with(|st| (st.hwnd, st.helper.clone(), st.in_own_drag, st.enter_at)) else {
            unsafe { *eff = DROPEFFECT_NONE };
            return Ok(());
        };
        if let Some(h) = &helper {
            let _ = unsafe { h.Drop(obj.as_ref(), &p, allowed) };
        }
        if own {
            log("Drop of our own Take out onto the Shelf: answering NONE, Item stays");
            unsafe { *eff = DROPEFFECT_NONE };
            with(|st| {
                st.own_drop_hit = true;
                st.hot = false;
                st.refresh();
            });
            return Ok(());
        }
        let Some(o) = obj.as_ref() else {
            unsafe { *eff = DROPEFFECT_NONE };
            return Ok(());
        };
        let t = now_ms();
        let paths = read_hdrop(o);
        let formats = list_formats(o);
        let result = choose_effect(allowed, !formats.is_empty());
        unsafe { *eff = result };
        let item = match &paths {
            Ok(v) if !v.is_empty() => Some(item_from_paths(v.clone())),
            _ => None,
        };
        log(format!(
            "Drop at ({},{}): {:.0} ms after DragEnter; keys {}; allowed {}; answer {}; CF_HDROP {}; read in {:.1} ms; fg={}",
            pt.x, pt.y, t - enter_at, keys_str(keys.0), effect_str(allowed.0), effect_str(result.0),
            match &paths { Ok(v) => format!("{} path(s): {:?}", v.len(), v), Err(e) => format!("unavailable ({e:?})") },
            now_ms() - t, fg()
        ));
        log(format!("  formats at Drop ({}): {}", formats.len(), formats.join(", ")));
        log(format!("  {}", async_mode(o)));
        with(|st| {
            st.hot = false;
            st.items.push(item.unwrap_or_else(|| Item {
                paths: vec![],
                name: "(no CF_HDROP: not read in spike)".into(),
                sub: formats.iter().take(3).cloned().collect::<Vec<_>>().join(", "),
                label: "?".into(),
                tile: (0.165, 0.165, 0.19),
                label_color: (1.0, 1.0, 1.0, 0.56),
                folder: false,
            }));
            st.refresh();
        });
        // Mattia (TASCA-8): the Shelf stays after a Park. `hide-after-park` restores the old rule.
        // When on, hide after Drop has returned, never inside it.
        if with(|st| st.flags.hide_after_park) == Some(true) {
            unsafe { SetTimer(Some(hwnd), T_HIDE_AFTER_PARK, 450, None) };
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- IDropSource (Take out)

thread_local! {
    static QCD_LAST: Cell<HRESULT> = const { Cell::new(S_OK) };
    static QCD_SIG: Cell<(bool, u32, bool, bool, bool)> = const { Cell::new((false, u32::MAX, false, false, false)) };
    static FB_LAST: Cell<u32> = const { Cell::new(u32::MAX) };
}

#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, fesc: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        let down = |vk: VIRTUAL_KEY| unsafe { GetAsyncKeyState(vk.0 as i32) } < 0;
        let (aesc, actrl, ashift) = (down(VK_ESCAPE), down(VK_CONTROL), down(VK_SHIFT));
        let sig = (fesc.as_bool(), keys.0, aesc, actrl, ashift);
        if QCD_SIG.get() != sig {
            QCD_SIG.set(sig);
            let mismatch = (keys.0 & MK_CONTROL.0 != 0) != actrl || (keys.0 & MK_SHIFT.0 != 0) != ashift || fesc.as_bool() != aesc;
            log(format!(
                "Take out QueryContinueDrag: OLE says Esc={} keys {} | physical Esc={aesc} Ctrl={actrl} Shift={ashift}{}",
                fesc.as_bool(), keys_str(keys.0), if mismatch { "   <-- MISMATCH (OLE does not see the keyboard)" } else { "" }
            ));
        }
        let r = if fesc.as_bool() {
            DRAGDROP_S_CANCEL
        } else if aesc {
            log("Take out cancelled by physical Esc that OLE did not report (GetAsyncKeyState fallback)");
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        };
        QCD_LAST.set(r);
        r
    }

    fn GiveFeedback(&self, eff: DROPEFFECT) -> HRESULT {
        if FB_LAST.get() != eff.0 {
            FB_LAST.set(eff.0);
            log(format!("Take out GiveFeedback: target answers {}", effect_str(eff.0)));
        }
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

fn shell_data_object(paths: &[PathBuf]) -> Result<IDataObject> {
    unsafe {
        let mut pidls: Vec<*const windows::Win32::UI::Shell::Common::ITEMIDLIST> = vec![];
        let mut err = None;
        for p in paths {
            let mut pidl = std::ptr::null_mut();
            match SHParseDisplayName(&HSTRING::from(p.as_os_str()), None, &mut pidl, 0, None) {
                Ok(()) => pidls.push(pidl),
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        let r = match err {
            Some(e) => Err(e),
            None => SHCreateShellItemArrayFromIDLists(&pidls).and_then(|a| a.BindToHandler::<_, IDataObject>(None, &BHID_DataObject)),
        };
        for p in pidls {
            CoTaskMemFree(Some(p as _));
        }
        r
    }
}

fn take_out(idx: usize) {
    let Some((hwnd, paths, flags)) = with(|st| {
        st.press = None;
        st.in_own_drag = true;
        st.own_drop_hit = false;
        (st.hwnd, st.items[idx].paths.clone(), st.flags)
    }) else {
        return;
    };
    unsafe {
        let had_capture = GetCapture() == hwnd;
        let _ = ReleaseCapture();
        if flags.focus_on_drag {
            let ok = SetForegroundWindow(hwnd).as_bool();
            log(format!("focus-on-drag: SetForegroundWindow={ok}, fg now {}", fg()));
        }
        log(format!("Take out start: item {idx} {:?}; had mouse capture={had_capture}; fg={}", paths, fg()));
        if paths.is_empty() {
            log("  (placeholder Item, nothing to drag)");
            with(|st| st.in_own_drag = false);
            return;
        }
        let obj = match shell_data_object(&paths) {
            Ok(o) => o,
            Err(e) => {
                log(format!("  shell data object failed: {e:?}"));
                with(|st| st.in_own_drag = false);
                return;
            }
        };
        QCD_LAST.set(S_OK);
        QCD_SIG.set((false, u32::MAX, false, false, false));
        FB_LAST.set(u32::MAX);
        let src: IDropSource = DropSource.into();
        let t = now_ms();
        let r = SHDoDragDrop(Some(hwnd), &obj, &src, DROPEFFECT_COPY | DROPEFFECT_MOVE);
        let last = QCD_LAST.get();
        let effect = r.as_ref().map(|e| e.0).unwrap_or(0);
        let logical = read_dword(&obj, "Logical Performed DropEffect");
        let performed = read_dword(&obj, "Performed DropEffect");
        let gone: Vec<_> = paths.iter().filter(|p| !p.exists()).collect();
        let own = with(|st| st.own_drop_hit).unwrap_or(false);
        let dropped = last == DRAGDROP_S_DROP;
        let success = dropped && !own && (effect != 0 || logical.is_some_and(|e| e != 0) || !gone.is_empty());
        log(format!(
            "Take out end after {:.0} ms: SHDoDragDrop={:?}, ended by {}, effect {}, logical performed {}, performed {}, gone {:?}, onto own Shelf={own} => {}; fg={}",
            now_ms() - t,
            r.as_ref().map(|_| ()).map_err(|e| e.code()),
            if dropped { "DROP" } else if last == DRAGDROP_S_CANCEL { "CANCEL" } else { "?" },
            effect_str(effect),
            logical.map(effect_str).unwrap_or("(absent)".into()),
            performed.map(effect_str).unwrap_or("(absent)".into()),
            gone,
            if success { "SUCCESS, Item leaves" } else { "kept" },
            fg()
        ));
        let empty = with(|st| {
            st.in_own_drag = false;
            if success && idx < st.items.len() {
                st.items.remove(idx);
            }
            st.hover = None;
            st.refresh();
            st.items.is_empty()
        });
        // Mattia (TASCA-8): the Shelf hides only when the last Item is Taken out (or on Caps+D).
        if success && empty == Some(true) {
            hide_shelf("last Item Taken out");
        }
    }
}

// ---------------------------------------------------------------- window

fn lparam_xy(l: LPARAM) -> (i32, i32) {
    ((l.0 & 0xffff) as i16 as i32, ((l.0 >> 16) & 0xffff) as i16 as i32)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TOGGLE => {
                toggle();
                LRESULT(0)
            }
            WM_SHOWSHELF => {
                with(|st| st.toggle_at = now_ms());
                show_shelf("plain launch while warm");
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_DPICHANGED => LRESULT(0), // the Shelf positions itself on every show
            WM_TIMER => {
                match wp.0 {
                    T_ANIM => anim_tick(),
                    T_HIDE_AFTER_PARK => {
                        let _ = KillTimer(Some(hwnd), T_HIDE_AFTER_PARK);
                        hide_shelf("after Park");
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let (x, y) = lparam_xy(lp);
                with(|st| {
                    let h = st.hit(x, y);
                    log(format!("click down on {h:?}; fg={}", fg()));
                    if let Hit::Row(i) = h {
                        st.press = Some((POINT { x, y }, i));
                    }
                });
                SetCapture(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = lparam_xy(lp);
                let _ = ReleaseCapture();
                with(|st| {
                    st.press = None;
                    if st.hit(x, y) == Hit::ClearAll {
                        log("Clear all");
                        st.items.clear();
                        st.refresh();
                    }
                });
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                let (x, y) = lparam_xy(lp);
                let quit = with(|st| match st.hit(x, y) {
                    Hit::Row(i) => {
                        log(format!("right-click: remove item {i}"));
                        st.items.remove(i);
                        st.hover = None;
                        st.refresh();
                        false
                    }
                    Hit::Header | Hit::ClearAll => true,
                    _ => false,
                });
                if quit == Some(true) {
                    log("right-click on the header: quit");
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = lparam_xy(lp);
                let drag = with(|st| {
                    if let Some((p0, i)) = st.press {
                        let (dx, dy) = (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG));
                        if wp.0 & MK_LBUTTON.0 as usize != 0 && ((x - p0.x).abs() > dx || (y - p0.y).abs() > dy) {
                            return Some(i);
                        }
                        return None;
                    }
                    if !st.tracking_leave {
                        let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: st.hwnd, dwHoverTime: 0 };
                        st.tracking_leave = TrackMouseEvent(&mut tme).is_ok();
                    }
                    let hov = match st.hit(x, y) {
                        Hit::Row(i) => Some(i),
                        _ => None,
                    };
                    if hov != st.hover {
                        st.hover = hov;
                        st.refresh();
                    }
                    None
                })
                .flatten();
                if let Some(i) = drag {
                    take_out(i);
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                with(|st| {
                    st.tracking_leave = false;
                    if st.hover.take().is_some() {
                        st.refresh();
                    }
                });
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xffff) as i16 as f32;
                with(|st| {
                    if !st.wheel_logged {
                        st.wheel_logged = true;
                        log(format!("wheel reached the non-activated Shelf (delta {delta}); fg={}", fg()));
                    }
                    st.scroll -= delta / 120.0 * ROW_H;
                    st.refresh();
                });
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

unsafe fn run_resident(flags: Flags, show: bool, elevated: bool) -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _mutex = CreateMutexW(None, true, MUTEX)?;
        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // Create the window first: it is what the warm path looks for.
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS,
            w!("Tasca spike"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinst),
            None,
        )?;
        let off = BOOL(1);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &off as *const _ as _, 4);
        OleInitialize(None)?;
        let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let fonts = Fonts {
            name: text_format(&dw, 12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING, false)?,
            sub: text_format(&dw, 10.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING, false)?,
            head: text_format(&dw, 11.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING, false)?,
            head_r: text_format(&dw, 11.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_TRAILING, false)?,
            label: text_format(&dw, 7.5, DWRITE_FONT_WEIGHT_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER, true)?,
            badge: text_format(&dw, 9.0, DWRITE_FONT_WEIGHT_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER, true)?,
        };
        let helper: Option<IDropTargetHelper> = CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER).ok();
        STATE.with(|c| {
            *c.borrow_mut() = Some(State {
                hwnd,
                flags,
                d2d,
                rt: None,
                fonts,
                helper: helper.clone(),
                items: vec![],
                visible: false,
                hot: false,
                hover: None,
                scroll: 0.0,
                dpi: 96,
                work: RECT::default(),
                cursor_at_show: POINT::default(),
                pos: POINT::default(),
                surf: None,
                anim: None,
                alpha: 255,
                toggle_at: 0.0,
                shown_at: 0.0,
                enter_at: 0.0,
                press: None,
                in_own_drag: false,
                own_drop_hit: false,
                over_keys: 0,
                over_effect: 0,
                wheel_logged: false,
                tracking_leave: false,
            })
        });
        let target: IDropTarget = DropTarget.into();
        RegisterDragDrop(hwnd, &target)?;

        // Dev aid: `--seed <path>...` parks these at startup, to eyeball the rows.
        let seed: Vec<PathBuf> = std::env::args().skip_while(|a| a != "--seed").skip(1).map(PathBuf::from).collect();
        // Render once while hidden so the first Caps+D doesn't pay for D2D/DWrite warm-up.
        let t = now_ms();
        with(|st| {
            for p in seed {
                st.items.push(item_from_paths(vec![p]));
            }
            st.dpi = GetDpiForWindow(hwnd);
            let _ = st.render();
        });
        log(format!("pre-render while hidden: {:.1} ms", now_ms() - t));

        let mut routing = 0u32;
        let _ = SystemParametersInfoW(SPI_GETMOUSEWHEELROUTING, 0, Some(&mut routing as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        let mut anim_on = BOOL(0);
        let _ = SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut anim_on as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        log(format!(
            "resident up {:.1} ms after process start: elevated={elevated}, show={show}, flags {flags:?}, drag helper={}, wheel routing={routing} (0 focus, 1 hybrid, 2 under mouse), client-area animation={}, fg={}",
            now_ms(), helper.is_some(), anim_on.as_bool(), fg()
        ));
        if show {
            with(|st| st.toggle_at = 0.0);
            show_shelf("cold start");
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = RevokeDragDrop(hwnd);
        STATE.with(|c| *c.borrow_mut() = None);
        OleUninitialize();
        log("resident exit");
        Ok(())
    }
}
