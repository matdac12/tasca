//! The settings window: plain Win32 controls, a shortcut recorder that also
//! captures Win (via a temporary low-level keyboard hook), "Start with
//! Windows", and OK/Cancel. Single instance, per-monitor-v2 layout, and a
//! `TASCA_DEV_DPI` override for screenshots.
//!
//! One rule keeps this file safe: `UI` is never borrowed while a Win32 call
//! that can send messages runs (`SetWindowText`, `SetFocus`, `SetWindowPos`,
//! `DestroyWindow`...). Those re-enter our window procedures, whose `with_ui`
//! would silently find the cell busy. So: read plain values (or `Handles`) out
//! of `with_ui`, drop the borrow, then call Win32.

use std::cell::RefCell;
use std::sync::{Mutex, Once};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    FrameRect, GetMonitorInfoW, GetSysColor, GetSysColorBrush, InvalidateRect, MonitorFromPoint,
    SelectObject, SetBkMode, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_BTNFACE,
    COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, DEFAULT_CHARSET, DEFAULT_PITCH, DT_LEFT,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, HBRUSH, HDC, HFONT, HGDIOBJ, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, SetFocus, VK_BACK, VK_DELETE, VK_ESCAPE, VK_LWIN, VK_RETURN, VK_RWIN, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::hotkey::{self, Hotkey};
use crate::log::{log, now_ms};
use crate::proto::{WM_RECORD_STOP, WM_SETTINGS_SAVED};
use crate::{settings, startup, tray};

const CLASS: PCWSTR = w!("Tasca.Settings");
const FIELD_CLASS: PCWSTR = w!("Tasca.Settings.Hotkey");
const WINDOW_TITLE: PCWSTR = w!("Tasca Settings");

const ID_HOTKEY: i32 = 100;
const ID_CLEAR: i32 = 101;
const ID_START: i32 = 102;
const ID_STATUS: i32 = 103;
const ID_VERSION: i32 = 104;
// The dialog manager (`IsDialogMessageW`) sends WM_COMMAND with IDOK (1) for
// Enter and IDCANCEL (2) for Esc, so the buttons use exactly those ids.
const ID_OK: i32 = 1;
const ID_CANCEL: i32 = 2;

const HINT: &str = "Click the field or press Enter, then press the shortcut. Esc cancels.";
const KEY_UNUSABLE: &str = "That key cannot be used in a shortcut.";
const TIMED_OUT: &str = "Recording timed out. Click the field to try again.";

/// Recording gives up after this long without a key, so the keyboard hook can
/// never swallow the keyboard indefinitely.
const RECORD_TIMEOUT_MS: u32 = 10_000;
const TIMER_RECORD: usize = 1;

/// `DM_GETDEFID`, and the "has a default id" marker in its answer.
const DM_GETDEFID: u32 = 0x0400;
const DC_HASDEFID: isize = 0x534E;
/// `WM_GETDLGCODE` answer: the control wants this key itself.
const DLGC_WANTMESSAGE: isize = 0x0004;

const BN_CLICKED: u32 = 0;
const WA_INACTIVE: usize = 0;

const FOCUS_COLOR: COLORREF = COLORREF(0x00D8_4E1D); // #1D4ED8, BGRA order
const ERROR_COLOR: COLORREF = COLORREF(0x001E_1EC8); // #C81E1E

struct Ui {
    hwnd: HWND,
    /// The resident window: only data (RegisterHotKey owner, message target).
    /// The settings window is *not* owned by it: an owner that is topmost or
    /// hidden (the Shelf) would drag the window along.
    parent: HWND,
    dpi: u32,
    /// What OK will apply.
    pending: Option<Hotkey>,
    /// "Start with Windows" when the window opened.
    start_at_open: bool,
    recording: bool,
    finishing: bool,
    hook: Option<HHOOK>,
    /// The key that started recording (Enter/Space): its auto-repeat is ignored.
    skip_vk: Option<u32>,
    /// Ctrl, Alt, Shift, Win currently held while recording.
    mods: (bool, bool, bool, bool),
    status: String,
    error: bool,
    font: HFONT,
    font_bold: HFONT,
    focus_brush: HBRUSH,
    h: Handles,
}

/// The control handles. `Copy`, so a caller can take them out of `with_ui`
/// and call Win32 after the borrow is gone (see the module note).
#[derive(Clone, Copy)]
struct Handles {
    title: HWND,
    field: HWND,
    clear: HWND,
    status_ctl: HWND,
    start_ctl: HWND,
    version: HWND,
    ok: HWND,
    cancel: HWND,
}

/// Serialises the background apply workers (see `on_ok`).
static APPLY: Mutex<()> = Mutex::new(());

thread_local! {
    static UI: RefCell<Option<Box<Ui>>> = const { RefCell::new(None) };
}

fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|u| match u.try_borrow_mut() {
        Ok(mut g) => g.as_mut().map(|b| f(b)),
        Err(_) => None,
    })
}

/// Debug builds only, like the Shelf: render the layout at this DPI.
fn dev_dpi() -> Option<u32> {
    #[cfg(debug_assertions)]
    if let Some(d) = std::env::var("TASCA_DEV_DPI")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return Some(d);
    }
    None
}

fn px(dpi: u32, dip: i32) -> i32 {
    (dip as f32 * dpi as f32 / 96.0).round() as i32
}

/// Work area and effective DPI of the monitor under the cursor.
fn cursor_monitor() -> (RECT, u32) {
    unsafe {
        let mut c = POINT::default();
        let _ = GetCursorPos(&mut c);
        let mon = MonitorFromPoint(c, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        (mi.rcWork, dx)
    }
}

/// Open the settings window, or bring the existing one forward.
pub fn open(parent: HWND) {
    if let Some(hwnd) = with_ui(|ui| ui.hwnd) {
        if unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            unsafe {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
            focus_field();
            return;
        }
        cleanup(); // a stale cell: free its fonts, brush and hook
    }
    if let Err(e) = create(parent) {
        log(format!("settings window failed: {e:?}"));
    }
}

fn focus_field() {
    if let Some(field) = with_ui(|ui| ui.h.field) {
        let _ = unsafe { SetFocus(Some(field)) };
    }
}

/// Close it if it is open (resident shutdown).
pub fn close() {
    if let Some(hwnd) = with_ui(|ui| ui.hwnd) {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

/// The message-loop hook: `IsDialogMessageW` gives Tab/Esc/Enter and
/// default-button behaviour to our non-dialog window. Only when the window is
/// open.
pub fn pre_translate(msg: &MSG) -> bool {
    let Some(hwnd) = with_ui(|ui| ui.hwnd) else {
        return false;
    };
    unsafe { IsDialogMessageW(hwnd, msg).as_bool() }
}

/// The settings worker finished (`WM_SETTINGS_SAVED`): show a failure in the
/// tray tooltip, or clear the previous one.
pub fn on_saved() {
    if let Some(n) = tray::notice() {
        log(format!("tray notice: {n}"));
    }
    tray::refresh();
}

/// Create every control. On failure the caller destroys the window, which
/// takes the controls made so far with it.
unsafe fn build_children(
    hwnd: HWND,
    hinst: windows::Win32::Foundation::HINSTANCE,
    font: HFONT,
    font_bold: HFONT,
) -> windows::core::Result<Handles> {
    unsafe {
        let title = child(
            hwnd,
            w!("STATIC"),
            w!("Keyboard shortcut"),
            WS_CHILD | WS_VISIBLE,
            ID_HOTKEY - 1,
            font_bold,
        )?;
        let field = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            FIELD_CLASS,
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            0,
            0,
            0,
            0,
            Some(hwnd),
            Some(HMENU(ID_HOTKEY as isize as *mut _)),
            Some(hinst),
            None,
        )?;
        let clear = child(
            hwnd,
            w!("BUTTON"),
            w!("Clear"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            ID_CLEAR,
            font,
        )?;
        let status_ctl = child(
            hwnd,
            w!("STATIC"),
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE,
            ID_STATUS,
            font,
        )?;
        let start_ctl = child(
            hwnd,
            w!("BUTTON"),
            w!("Start with Windows"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
            ID_START,
            font,
        )?;
        let version = child(
            hwnd,
            w!("STATIC"),
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE,
            ID_VERSION,
            font,
        )?;
        let ok = child(
            hwnd,
            w!("BUTTON"),
            w!("OK"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            ID_OK,
            font,
        )?;
        let cancel = child(
            hwnd,
            w!("BUTTON"),
            w!("Cancel"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            ID_CANCEL,
            font,
        )?;
        Ok(Handles {
            title,
            field,
            clear,
            status_ctl,
            start_ctl,
            version,
            ok,
            cancel,
        })
    }
}

fn create(parent: HWND) -> windows::core::Result<()> {
    unsafe {
        let hinst = GetModuleHandleW(None)?.into();
        register_classes(hinst);
        let (work, monitor_dpi) = cursor_monitor();
        let dpi = dev_dpi().unwrap_or(monitor_dpi);
        let style = WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        // WS_EX_APPWINDOW: its own taskbar button (an unowned window gets one
        // anyway, this keeps it explicit).
        let exstyle = WS_EX_APPWINDOW | WS_EX_CONTROLPARENT;
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: px(dpi, 420),
            bottom: px(dpi, 200),
        };
        let _ = AdjustWindowRectExForDpi(&mut rc, style, false, exstyle, monitor_dpi);
        let w = rc.right - rc.left;
        let h = rc.bottom - rc.top;
        let x = work.left + ((work.right - work.left) - w) / 2;
        let y = work.top + ((work.bottom - work.top) - h) / 2;
        // No owner: the resident window is topmost and the Shelf hides with
        // SW_HIDE, and an owned window would follow both.
        let hwnd = CreateWindowExW(
            exstyle,
            CLASS,
            WINDOW_TITLE,
            style,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(hinst),
            None,
        )?;

        let (shortcut, error) = hotkey::display();
        let font = make_font(dpi, false);
        let font_bold = make_font(dpi, true);
        let focus_brush = CreateSolidBrush(FOCUS_COLOR);
        let kids = match build_children(hwnd, hinst, font, font_bold) {
            Ok(k) => k,
            Err(e) => {
                // Nothing is stored yet: free what we made and the window.
                let _ = DestroyWindow(hwnd);
                for obj in [font.0, font_bold.0, focus_brush.0] {
                    let _ = DeleteObject(HGDIOBJ(obj));
                }
                return Err(e);
            }
        };
        let status = error.clone().unwrap_or_else(|| HINT.to_string());
        // `startup::enabled` is one stat of the Startup folder on the UI
        // thread. Accepted exception to "no I/O on the UI thread": it runs
        // once per window open, on a local path (a redirected Startup folder
        // could make it slow).
        let start_at_open = startup::enabled();
        let ui = Box::new(Ui {
            hwnd,
            parent,
            dpi,
            pending: shortcut,
            start_at_open,
            recording: false,
            finishing: false,
            hook: None,
            skip_vk: None,
            mods: (false, false, false, false),
            status: status.clone(),
            error: error.is_some(),
            font,
            font_bold,
            focus_brush,
            h: kids,
        });
        UI.with(|u| *u.borrow_mut() = Some(ui));

        set_text(kids.version, &format!("Tasca {}", env!("CARGO_PKG_VERSION")));
        set_text(kids.status_ctl, &status);
        SendMessageW(
            kids.start_ctl,
            BM_SETCHECK,
            Some(WPARAM(usize::from(start_at_open))),
            None,
        );

        if let Some(icon) = tray::load_icon(false) {
            SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(icon.0 as isize)),
            );
        }
        if let Some(icon) = tray::load_icon(true) {
            SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_SMALL as usize)),
                Some(LPARAM(icon.0 as isize)),
            );
        }

        relayout(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
        focus_field();
        log(format!(
            "settings opened at {dpi} dpi in {:.1} ms",
            now_ms()
        ));
        Ok(())
    }
}

fn child(
    parent: HWND,
    class: PCWSTR,
    text: PCWSTR,
    style: WINDOW_STYLE,
    id: i32,
    font: HFONT,
) -> windows::core::Result<HWND> {
    unsafe {
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            text,
            style,
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(id as isize as *mut _)),
            None,
            None,
        )?;
        SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
        Ok(hwnd)
    }
}

fn make_font(dpi: u32, bold: bool) -> HFONT {
    unsafe {
        CreateFontW(
            -(dpi as i32 * 9 / 72),
            0,
            0,
            0,
            if bold { 600 } else { 400 },
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32 | FF_DONTCARE.0 as u32,
            w!("Segoe UI"),
        )
    }
}

fn register_classes(hinst: windows::Win32::Foundation::HINSTANCE) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let big = tray::load_icon(false).unwrap_or_default();
        let small = tray::load_icon(true).unwrap_or_default();
        let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let dialog = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hIcon: big,
            hCursor: cursor,
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 as usize + 1) as *mut _),
            lpszClassName: CLASS,
            hIconSm: small,
            ..Default::default()
        };
        if RegisterClassExW(&dialog) == 0 {
            log("settings: RegisterClassExW failed");
        }
        let field = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(field_proc),
            hInstance: hinst,
            hCursor: cursor,
            hbrBackground: HBRUSH((COLOR_WINDOW.0 as usize + 1) as *mut _),
            lpszClassName: FIELD_CLASS,
            ..Default::default()
        };
        if RegisterClassExW(&field) == 0 {
            log("settings: hotkey field class registration failed");
        }
    });
}


fn set_text(hwnd: HWND, text: &str) {
    if !hwnd.is_invalid() {
        let _ = unsafe { SetWindowTextW(hwnd, &windows::core::HSTRING::from(text)) };
    }
}

/// Show `text` in the status line (error = red). Never call from inside `with_ui`.
fn set_status(text: &str, error: bool) {
    let Some(ctl) = with_ui(|ui| {
        if ui.status == text && ui.error == error {
            return None; // already shown: no message, no repaint (hot in the hook)
        }
        ui.status = text.to_string();
        ui.error = error;
        Some(ui.h.status_ctl)
    })
    .flatten() else {
        return;
    };
    set_text(ctl, text);
    invalidate(ctl);
}

fn relayout(hwnd: HWND) {
    let Some((dpi, k)) = with_ui(|ui| (ui.dpi, ui.h)) else {
        return;
    };
    let s = |dip: i32| px(dpi, dip);
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let exstyle = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: s(420),
            bottom: s(200),
        };
        let _ = AdjustWindowRectExForDpi(
            &mut rc,
            WINDOW_STYLE(style),
            false,
            WINDOW_EX_STYLE(exstyle),
            GetDpiForWindow(hwnd).max(96),
        );
        let mut wr = RECT::default();
        let _ = GetWindowRect(hwnd, &mut wr);
        let _ = SetWindowPos(
            hwnd,
            None,
            wr.left,
            wr.top,
            rc.right - rc.left,
            rc.bottom - rc.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let place = |ctl: HWND, x: i32, y: i32, w: i32, h: i32| {
            let _ = SetWindowPos(
                ctl,
                None,
                s(x),
                s(y),
                s(w),
                s(h),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        };
        place(k.title, 16, 16, 388, 20);
        place(k.field, 16, 40, 296, 30);
        place(k.clear, 324, 40, 80, 30);
        place(k.status_ctl, 16, 78, 388, 34);
        place(k.start_ctl, 16, 122, 388, 24);
        place(k.version, 16, 166, 200, 18);
        place(k.ok, 224, 162, 80, 28);
        place(k.cancel, 320, 162, 84, 28);
    }
}

fn refont(dpi: u32) {
    let font = make_font(dpi, false);
    let bold = make_font(dpi, true);
    let swapped = with_ui(|ui| {
        (
            std::mem::replace(&mut ui.font, font),
            std::mem::replace(&mut ui.font_bold, bold),
            ui.h,
        )
    });
    let Some((old, old_bold, k)) = swapped else {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(font.0));
            let _ = DeleteObject(HGDIOBJ(bold.0));
        }
        return;
    };
    unsafe {
        for ctl in [k.clear, k.status_ctl, k.start_ctl, k.version, k.ok, k.cancel] {
            SendMessageW(
                ctl,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
        SendMessageW(
            k.title,
            WM_SETFONT,
            Some(WPARAM(bold.0 as usize)),
            Some(LPARAM(1)),
        );
        let _ = DeleteObject(HGDIOBJ(old.0));
        let _ = DeleteObject(HGDIOBJ(old_bold.0));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if let Some(r) = handle(hwnd, msg, wp, lp) {
            return r;
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

/// `unsafe`: the Win32 calls below need the UI thread and live handles.
unsafe fn handle(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_COMMAND => {
            let id = (wp.0 & 0xffff) as i32;
            let code = ((wp.0 >> 16) & 0xffff) as u32;
            if code == BN_CLICKED {
                match id {
                    ID_OK => on_ok(hwnd),
                    ID_CANCEL => {
                        let _ = unsafe { DestroyWindow(hwnd) };
                    }
                    ID_CLEAR => {
                        let field = with_ui(|ui| {
                            ui.pending = None;
                            ui.h.field
                        });
                        set_status(HINT, false);
                        if let Some(field) = field {
                            invalidate(field);
                        }
                    }
                    _ => {}
                }
            }
            Some(LRESULT(0))
        }
        // The dialog manager asks for the default button with this.
        DM_GETDEFID => Some(LRESULT((DC_HASDEFID << 16) | ID_OK as isize)),
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            Some(LRESULT(0))
        }
        WM_ACTIVATE => {
            if (wp.0 & 0xffff) == WA_INACTIVE && with_ui(|ui| ui.recording).unwrap_or(false) {
                cancel_recording();
            }
            None
        }
        WM_TIMER if wp.0 == TIMER_RECORD => {
            if with_ui(|ui| ui.recording).unwrap_or(false) {
                log("settings: recording timed out");
                cancel_recording();
                set_status(TIMED_OUT, true);
            } else {
                let _ = unsafe { KillTimer(Some(hwnd), TIMER_RECORD) };
            }
            Some(LRESULT(0))
        }
        WM_DPICHANGED => {
            let dpi = dev_dpi().unwrap_or((wp.0 & 0xffff) as u32);
            with_ui(|ui| ui.dpi = dpi);
            refont(dpi);
            unsafe { codeguarded_relayout(hwnd, lp) };
            Some(LRESULT(0))
        }
        WM_CTLCOLORSTATIC => {
            let ctl = HWND(lp.0 as *mut _);
            let (status, version, error) = with_ui(|ui| (ui.h.status_ctl, ui.h.version, ui.error))
                .unwrap_or((HWND::default(), HWND::default(), false));
            unsafe {
                let hdc = HDC(wp.0 as *mut _);
                SetBkMode(hdc, TRANSPARENT);
                if ctl == status {
                    if error {
                        SetTextColor(hdc, ERROR_COLOR);
                    }
                } else if ctl == version {
                    SetTextColor(hdc, COLORREF(GetSysColor(COLOR_GRAYTEXT)));
                }
            }
            Some(LRESULT(GetSysColorBrush(COLOR_BTNFACE).0 as isize))
        }
        WM_DESTROY => {
            cleanup();
            Some(LRESULT(0))
        }
        _ => None,
    }
}

/// `WM_DPICHANGED` also carries the suggested rectangle; keep our position.
unsafe fn codeguarded_relayout(hwnd: HWND, lp: LPARAM) {
    if lp.0 != 0 {
        let suggested = unsafe { &*(lp.0 as *const RECT) };
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                None,
                suggested.left,
                suggested.top,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
    relayout(hwnd);
}

/// Take the state out of the cell, then free its hook, timer, fonts and brush.
/// Safe to call with nothing open (and from `WM_DESTROY` of a half-built window).
fn cleanup() {
    let ui = UI.with(|u| u.try_borrow_mut().ok().and_then(|mut g| g.take()));
    let Some(ui) = ui else { return };
    unsafe {
        let _ = KillTimer(Some(ui.hwnd), TIMER_RECORD);
        if let Some(hook) = ui.hook {
            let _ = UnhookWindowsHookEx(hook);
        }
        let _ = DeleteObject(HGDIOBJ(ui.font.0));
        let _ = DeleteObject(HGDIOBJ(ui.font_bold.0));
        let _ = DeleteObject(HGDIOBJ(ui.focus_brush.0));
    }
}

fn on_ok(hwnd: HWND) {
    let Some((pending, start_ctl, start_at_open, parent, field)) =
        with_ui(|ui| (ui.pending, ui.h.start_ctl, ui.start_at_open, ui.parent, ui.h.field))
    else {
        return;
    };
    let checked = unsafe { SendMessageW(start_ctl, BM_GETCHECK, None, None) }.0 == 1;
    if let Err(e) = hotkey::change(parent, pending) {
        set_status(&e, true);
        invalidate(field);
        let _ = unsafe { SetFocus(Some(field)) };
        return;
    }
    tray::refresh();

    // The Startup shortcut and the ini are written off the UI thread; the
    // outcome comes back as WM_SETTINGS_SAVED and a failure shows in the tray
    // tooltip (the window is already gone by then).
    let start_change = checked != start_at_open;
    let saved = settings::Settings { shortcut: pending };
    let resident = parent.0 as isize;
    std::thread::spawn(move || {
        // One apply at a time: two quick OKs must not share settings.ini.tmp
        // or finish out of order.
        let _one_at_a_time = APPLY.lock().unwrap_or_else(|p| p.into_inner());
        let start_err = if start_change {
            startup::set_enabled(checked).err() // logs itself
        } else {
            None
        };
        let save_err = settings::save(&saved).err();
        match &save_err {
            Some(e) => log(format!("settings save failed: {e}")),
            None => log("settings saved"),
        }
        tray::set_notice(settings::failure_notice(
            start_err.as_deref(),
            save_err.as_deref(),
        ));
        unsafe {
            let _ = PostMessageW(
                Some(HWND(resident as *mut _)),
                WM_SETTINGS_SAVED,
                WPARAM(0),
                LPARAM(0),
            );
        }
    });
    log("settings applied");
    let _ = unsafe { DestroyWindow(hwnd) };
}

fn invalidate(hwnd: HWND) {
    if !hwnd.is_invalid() {
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, true);
        }
    }
}

// ---------------------------------------------------------------- recorder

/// Start recording; `trigger` is the key that started it (its auto-repeat is
/// ignored until it is released), `None` for a mouse click.
fn start_recording(trigger: Option<u32>) {
    let Some((hwnd, field, already)) = with_ui(|ui| (ui.hwnd, ui.h.field, ui.recording)) else {
        return;
    };
    if already {
        return;
    }
    match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) } {
        Ok(hook) => {
            with_ui(|ui| {
                ui.recording = true;
                ui.finishing = false;
                ui.skip_vk = trigger;
                ui.mods = (false, false, false, false);
                ui.hook = Some(hook);
            });
            set_status(HINT, false);
            unsafe { SetTimer(Some(hwnd), TIMER_RECORD, RECORD_TIMEOUT_MS, None) };
            log("settings: recording a shortcut");
        }
        Err(e) => {
            set_status(&format!("Keyboard recorder could not start: {e}"), true);
            log(format!("settings: SetWindowsHookExW failed: {e}"));
        }
    }
    invalidate(field);
}

/// Leave recording: unhook, stop the timeout, repaint. Returns the shortcut
/// OK would apply. A no-op when not recording.
fn end_recording() -> Option<Option<Hotkey>> {
    let ended = with_ui(|ui| {
        if !ui.recording {
            return None; // a stale WM_RECORD_STOP after a cancel
        }
        ui.recording = false;
        ui.finishing = false;
        ui.skip_vk = None;
        ui.mods = (false, false, false, false);
        Some((ui.hook.take(), ui.hwnd, ui.h.field, ui.pending))
    })??;
    let (hook, hwnd, field, pending) = ended;
    unsafe {
        if let Some(hook) = hook {
            let _ = UnhookWindowsHookEx(hook);
        }
        let _ = KillTimer(Some(hwnd), TIMER_RECORD);
    }
    invalidate(field);
    Some(pending)
}

/// The hook decided on a key and asked the field to finish.
fn stop_recording() {
    match end_recording() {
        Some(Some(hk)) => log(format!("settings: recorded {}", hk.text())),
        Some(None) => log("settings: recorded none"),
        None => {}
    }
}

/// Abandon a half-recorded change (focus lost, Esc, timeout). Keeps `pending`.
fn cancel_recording() {
    let _ = end_recording();
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum HookAction {
    /// Not ours: its key-down was never swallowed, so let the key-up through.
    Pass,
    Swallow,
    SwallowAndStop,
}

/// What a key event means while recording: the hook action and an optional
/// status line (text, is-error). Pure state change, no Win32.
fn decide(ui: &mut Ui, vk: u32, down: bool) -> (HookAction, Option<(String, bool)>) {
    if ui.finishing {
        return (HookAction::Swallow, None);
    }
    if let Some(slot) = modifier_slot(vk) {
        let was_down = mod_state(&ui.mods, slot);
        set_mod(&mut ui.mods, slot, down);
        // A modifier already held when recording began: its release must reach
        // the system or it stays "stuck down".
        let action = if !down && !was_down {
            HookAction::Pass
        } else {
            HookAction::Swallow
        };
        return (action, None);
    }
    if ui.skip_vk == Some(vk) {
        if !down {
            // The key that started recording went down before the hook: pass
            // its release for the same reason.
            ui.skip_vk = None;
            return (HookAction::Pass, None);
        }
        return (HookAction::Swallow, None);
    }
    if !down {
        return (HookAction::Swallow, None);
    }
    if vk == VK_ESCAPE.0 as u32 {
        ui.finishing = true;
        return (HookAction::SwallowAndStop, None);
    }
    let (ctrl, alt, shift, win) = ui.mods;
    if (vk == VK_BACK.0 as u32 || vk == VK_DELETE.0 as u32) && !(ctrl || alt || shift || win) {
        ui.pending = None;
        ui.finishing = true;
        return (
            HookAction::SwallowAndStop,
            Some((HINT.to_string(), false)),
        );
    }
    if hotkey::key_name(vk).is_none() {
        return (
            HookAction::Swallow,
            Some((KEY_UNUSABLE.to_string(), true)),
        );
    }
    let hk = Hotkey {
        ctrl,
        alt,
        shift,
        win,
        vk,
    };
    if let Some(reason) = hk.unsafe_reason() {
        return (HookAction::Swallow, Some((reason, true)));
    }
    ui.pending = Some(hk);
    ui.finishing = true;
    (HookAction::SwallowAndStop, Some((HINT.to_string(), false)))
}

unsafe extern "system" fn hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wp, lp) };
    }
    let msg = wp.0 as u32;
    let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    if !down && msg != WM_KEYUP && msg != WM_SYSKEYUP {
        return unsafe { CallNextHookEx(None, code, wp, lp) };
    }
    let kb = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) };

    // Decide under the borrow; every Win32 call below runs after it is gone.
    let decided = UI.with(|u| {
        let mut guard = u.try_borrow_mut().ok()?;
        let ui = guard.as_mut()?;
        if !ui.recording {
            return None;
        }
        let (action, status) = decide(ui, kb.vkCode, down);
        Some((action, status, ui.h.field, ui.hwnd))
    });
    let Some((action, status, field, hwnd)) = decided else {
        return unsafe { CallNextHookEx(None, code, wp, lp) };
    };
    if let Some((text, error)) = status {
        set_status(&text, error);
    }
    invalidate(field);
    unsafe {
        match action {
            HookAction::Pass => CallNextHookEx(None, code, wp, lp),
            HookAction::Swallow => {
                if down {
                    SetTimer(Some(hwnd), TIMER_RECORD, RECORD_TIMEOUT_MS, None);
                }
                LRESULT(1)
            }
            HookAction::SwallowAndStop => {
                let _ = PostMessageW(Some(field), WM_RECORD_STOP, WPARAM(0), LPARAM(0));
                LRESULT(1)
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ModSlot {
    Ctrl,
    Alt,
    Shift,
    Win,
}

fn modifier_slot(vk: u32) -> Option<ModSlot> {
    match vk {
        0x11 | 0xA2 | 0xA3 => Some(ModSlot::Ctrl),
        0x12 | 0xA4 | 0xA5 => Some(ModSlot::Alt),
        0x10 | 0xA0 | 0xA1 => Some(ModSlot::Shift),
        x if x == VK_LWIN.0 as u32 || x == VK_RWIN.0 as u32 => Some(ModSlot::Win),
        _ => None,
    }
}

fn mod_state(mods: &(bool, bool, bool, bool), slot: ModSlot) -> bool {
    match slot {
        ModSlot::Ctrl => mods.0,
        ModSlot::Alt => mods.1,
        ModSlot::Shift => mods.2,
        ModSlot::Win => mods.3,
    }
}

fn set_mod(mods: &mut (bool, bool, bool, bool), slot: ModSlot, down: bool) {
    match slot {
        ModSlot::Ctrl => mods.0 = down,
        ModSlot::Alt => mods.1 = down,
        ModSlot::Shift => mods.2 = down,
        ModSlot::Win => mods.3 = down,
    }
}

fn recording_text(ui: &Ui) -> String {
    let (ctrl, alt, shift, win) = ui.mods;
    if !(ctrl || alt || shift || win) {
        return "Press keys\u{2026}".to_string();
    }
    let mut s = String::new();
    if ctrl {
        s.push_str("Ctrl+");
    }
    if alt {
        s.push_str("Alt+");
    }
    if shift {
        s.push_str("Shift+");
    }
    if win {
        s.push_str("Win+");
    }
    s.push('\u{2026}');
    s
}

fn field_text(ui: &Ui) -> (String, bool) {
    if ui.recording {
        return (recording_text(ui), true);
    }
    match ui.pending {
        Some(hk) => (hk.text(), false),
        None => ("None".to_string(), false),
    }
}

/// Keys the field wants for itself instead of the dialog manager (which would
/// turn Enter into "click OK" and Esc into "cancel"): Enter and Space start
/// recording, Esc leaves it.
fn wants_key(vk: u32) -> bool {
    vk == VK_RETURN.0 as u32 || vk == VK_SPACE.0 as u32 || vk == VK_ESCAPE.0 as u32
}

unsafe extern "system" fn field_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                paint_field(hwnd);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_GETDLGCODE => LRESULT(if wants_key(wp.0 as u32) {
                DLGC_WANTMESSAGE
            } else {
                0
            }),
            WM_LBUTTONDOWN => {
                let _ = SetFocus(Some(hwnd));
                start_recording(None);
                LRESULT(0)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => match wp.0 as u32 {
                v if v == VK_ESCAPE.0 as u32 => {
                    // Recording is normally ended by the hook before we see
                    // Esc; this is the plain "close the window" case.
                    if with_ui(|ui| ui.recording).unwrap_or(false) {
                        cancel_recording();
                    } else if let Ok(parent) = GetParent(hwnd) {
                        let _ = PostMessageW(
                            Some(parent),
                            WM_COMMAND,
                            WPARAM(ID_CANCEL as usize),
                            LPARAM(0),
                        );
                    }
                    LRESULT(0)
                }
                v if v == VK_RETURN.0 as u32 || v == VK_SPACE.0 as u32 => {
                    start_recording(Some(v));
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wp, lp),
            },
            WM_CHAR => LRESULT(0),
            WM_SETFOCUS | WM_KILLFOCUS => {
                if msg == WM_KILLFOCUS {
                    cancel_recording();
                }
                let _ = InvalidateRect(Some(hwnd), None, true);
                LRESULT(0)
            }
            WM_RECORD_STOP => {
                stop_recording();
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn paint_field(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let focused = GetFocus() == hwnd;
        let (text, gray, dpi, focus_brush, font) = with_ui(|ui| {
            let (text, placeholder) = field_text(ui);
            (text, placeholder, ui.dpi, ui.focus_brush, ui.font)
        })
        .unwrap_or((
            "None".to_string(),
            false,
            96,
            HBRUSH::default(),
            HFONT::default(),
        ));
        FillRect(hdc, &rc, GetSysColorBrush(COLOR_WINDOW));
        let border = if focused {
            focus_brush
        } else {
            GetSysColorBrush(COLOR_GRAYTEXT)
        };
        FrameRect(hdc, &rc, border);
        if focused {
            let inner = RECT {
                left: rc.left + 2,
                top: rc.top + 2,
                right: rc.right - 2,
                bottom: rc.bottom - 2,
            };
            FrameRect(hdc, &inner, border);
        }
        if gray {
            SetTextColor(hdc, COLORREF(GetSysColor(COLOR_GRAYTEXT)));
        } else {
            SetTextColor(hdc, COLORREF(GetSysColor(COLOR_WINDOWTEXT)));
        }
        SetBkMode(hdc, TRANSPARENT);
        let old_font = SelectObject(hdc, HGDIOBJ(font.0));
        let pad = px(dpi, 10);
        let mut trc = RECT {
            left: rc.left + pad,
            top: rc.top,
            right: rc.right - pad,
            bottom: rc.bottom,
        };
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(
            hdc,
            &mut wide,
            &mut trc,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        if !old_font.is_invalid() {
            SelectObject(hdc, old_font);
        }
        let _ = EndPaint(hwnd, &ps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank() -> Ui {
        let h = HWND::default();
        Ui {
            hwnd: h,
            parent: h,
            dpi: 96,
            pending: Some(Hotkey::DEFAULT),
            start_at_open: false,
            recording: true,
            finishing: false,
            hook: None,
            skip_vk: None,
            mods: (false, false, false, false),
            status: String::new(),
            error: false,
            font: HFONT::default(),
            font_bold: HFONT::default(),
            focus_brush: HBRUSH::default(),
            h: Handles {
                title: h,
                field: h,
                clear: h,
                status_ctl: h,
                start_ctl: h,
                version: h,
                ok: h,
                cancel: h,
            },
        }
    }

    const CTRL: u32 = 0x11;
    const ALT: u32 = 0x12;
    const SHIFT: u32 = 0x10;

    #[test]
    fn dialog_ids_are_the_dialog_managers() {
        assert_eq!(ID_OK, 1, "IDOK");
        assert_eq!(ID_CANCEL, 2, "IDCANCEL");
    }

    #[test]
    fn the_field_wants_enter_space_and_esc_but_not_tab() {
        assert!(wants_key(0x0D) && wants_key(0x20) && wants_key(0x1B));
        assert!(!wants_key(0x09), "Tab must keep moving focus");
        assert!(!wants_key(0), "no key (focus queries)");
    }

    #[test]
    fn a_safe_combination_is_accepted_and_finishes() {
        let mut ui = blank();
        assert_eq!(decide(&mut ui, CTRL, true).0, HookAction::Swallow);
        assert_eq!(decide(&mut ui, ALT, true).0, HookAction::Swallow);
        let (action, status) = decide(&mut ui, b'F' as u32, true);
        assert_eq!(action, HookAction::SwallowAndStop);
        assert_eq!(status, Some((HINT.to_string(), false)));
        assert_eq!(ui.pending, Hotkey::parse("Ctrl+Alt+F"));
        // Everything after the decision is swallowed until the field stops.
        assert_eq!(decide(&mut ui, b'G' as u32, true).0, HookAction::Swallow);
        assert_eq!(ui.pending, Hotkey::parse("Ctrl+Alt+F"));
    }

    #[test]
    fn unsafe_combinations_show_the_reason_and_keep_recording() {
        let mut ui = blank();
        decide(&mut ui, SHIFT, true);
        let (action, status) = decide(&mut ui, b'D' as u32, true);
        assert_eq!(action, HookAction::Swallow);
        let (text, error) = status.unwrap();
        assert!(error);
        assert_eq!(text, hotkey::NEED_MODIFIER);
        assert_eq!(ui.pending, Some(Hotkey::DEFAULT), "pending untouched");
        assert!(!ui.finishing);

        let mut ui = blank();
        decide(&mut ui, ALT, true);
        let (action, status) = decide(&mut ui, 0x73, true); // F4
        assert_eq!(action, HookAction::Swallow);
        assert!(status.unwrap().0.starts_with("Alt+F4 is reserved"));

        let mut ui = blank();
        decide(&mut ui, CTRL, true);
        assert!(decide(&mut ui, b'C' as u32, true).1.unwrap().1);
    }

    #[test]
    fn esc_cancels_without_changing_pending() {
        let mut ui = blank();
        let (action, status) = decide(&mut ui, VK_ESCAPE.0 as u32, true);
        assert_eq!(action, HookAction::SwallowAndStop);
        assert_eq!(status, None);
        assert_eq!(ui.pending, Some(Hotkey::DEFAULT));
    }

    #[test]
    fn bare_backspace_or_delete_clears() {
        for vk in [VK_BACK.0 as u32, VK_DELETE.0 as u32] {
            let mut ui = blank();
            assert_eq!(decide(&mut ui, vk, true).0, HookAction::SwallowAndStop);
            assert_eq!(ui.pending, None);
        }
    }

    #[test]
    fn the_starting_keys_auto_repeat_is_ignored() {
        let mut ui = blank();
        ui.skip_vk = Some(VK_RETURN.0 as u32);
        // Held Enter repeats key-down: swallowed, not recorded as "Enter".
        assert_eq!(decide(&mut ui, 0x0D, true), (HookAction::Swallow, None));
        // Its release passes: the system saw the press, not the hook.
        assert_eq!(decide(&mut ui, 0x0D, false), (HookAction::Pass, None));
        assert_eq!(ui.skip_vk, None);
        // A later Enter is a normal key (unsafe on its own, so refused inline).
        assert!(decide(&mut ui, 0x0D, true).1.unwrap().1);
    }

    #[test]
    fn releasing_a_modifier_held_before_recording_passes_through() {
        let mut ui = blank();
        // Ctrl-up with no Ctrl-down seen by the hook.
        assert_eq!(decide(&mut ui, CTRL, false).0, HookAction::Pass);
        // A press the hook swallowed is released with the matching swallow.
        assert_eq!(decide(&mut ui, ALT, true).0, HookAction::Swallow);
        assert_eq!(decide(&mut ui, ALT, false).0, HookAction::Swallow);
    }

    #[test]
    fn unnameable_keys_are_refused() {
        let mut ui = blank();
        decide(&mut ui, CTRL, true);
        let (action, status) = decide(&mut ui, 0xE7, true);
        assert_eq!(action, HookAction::Swallow);
        assert_eq!(status, Some((KEY_UNUSABLE.to_string(), true)));
    }
}
