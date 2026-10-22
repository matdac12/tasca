//! The settings window: plain Win32 controls, a shortcut recorder that also
//! captures Win (via a temporary low-level keyboard hook), "Start with
//! Windows", and OK/Cancel. Single instance, per-monitor-v2 layout, and a
//! `TASCA_DEV_DPI` override for screenshots.

use std::cell::RefCell;
use std::sync::Once;

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
use crate::proto::WM_RECORD_STOP;
use crate::{settings, startup, tray};

const CLASS: PCWSTR = w!("Tasca.Settings");
const FIELD_CLASS: PCWSTR = w!("Tasca.Settings.Hotkey");
const WINDOW_TITLE: PCWSTR = w!("Tasca Settings");

const ID_HOTKEY: i32 = 100;
const ID_CLEAR: i32 = 101;
const ID_START: i32 = 102;
const ID_STATUS: i32 = 103;
const ID_VERSION: i32 = 104;
const ID_OK: i32 = 105;
const ID_CANCEL: i32 = 106;

const HINT: &str = "Click the field, then press the combination. Esc cancels.";
const NEED_MODIFIER: &str = "Hold Ctrl, Alt, Shift or Win while pressing a letter or number.";

const BN_CLICKED: u32 = 0;
const WA_INACTIVE: usize = 0;

const FOCUS_COLOR: COLORREF = COLORREF(0x00D8_4E1D); // #1D4ED8, BGRA order
const ERROR_COLOR: COLORREF = COLORREF(0x001E_1EC8); // #C81E1E

struct Ui {
    hwnd: HWND,
    parent: HWND,
    dpi: u32,
    /// What OK will apply.
    pending: Option<Hotkey>,
    /// Checkbox state at open and the control's current state.
    start_at_open: bool,
    recording: bool,
    finishing: bool,
    hook: Option<HHOOK>,
    /// Ctrl, Alt, Shift, Win currently held while recording.
    mods: (bool, bool, bool, bool),
    status: String,
    error: bool,
    font: HFONT,
    font_bold: HFONT,
    focus_brush: HBRUSH,
    title: HWND,
    field: HWND,
    clear: HWND,
    status_ctl: HWND,
    start_ctl: HWND,
    version: HWND,
    ok: HWND,
    cancel: HWND,
}

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
                with_ui(|ui| {
                    let _ = SetFocus(Some(ui.field));
                });
            }
            return;
        }
        UI.with(|u| *u.borrow_mut() = None);
    }
    if let Err(e) = create(parent) {
        log(format!("settings window failed: {e:?}"));
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

/// The message-loop hook: `IsDialogMessageW` gives Tab/Esc/default-button
/// behaviour to our non-dialog window. Only when the window is open.
pub fn pre_translate(msg: &MSG) -> bool {
    let Some(hwnd) = with_ui(|ui| ui.hwnd) else {
        return false;
    };
    unsafe { IsDialogMessageW(hwnd, msg).as_bool() }
}

fn create(parent: HWND) -> windows::core::Result<()> {
    unsafe {
        let hinst = GetModuleHandleW(None)?.into();
        register_classes(hinst);
        let (work, monitor_dpi) = cursor_monitor();
        let dpi = dev_dpi().unwrap_or(monitor_dpi);
        let style = WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
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
        let hwnd = CreateWindowExW(
            exstyle,
            CLASS,
            WINDOW_TITLE,
            style,
            x,
            y,
            w,
            h,
            Some(parent),
            None,
            Some(hinst),
            None,
        )?;

        let (shortcut, error) = hotkey::display();
        let font = make_font(dpi, false);
        let font_bold = make_font(dpi, true);
        let focus_brush = CreateSolidBrush(FOCUS_COLOR);
        let mut ui = Box::new(Ui {
            hwnd,
            parent,
            dpi,
            pending: shortcut,
            start_at_open: startup::enabled(),
            recording: false,
            finishing: false,
            hook: None,
            mods: (false, false, false, false),
            status: error.clone().unwrap_or_else(|| HINT.to_string()),
            error: error.is_some(),
            font,
            font_bold,
            focus_brush,
            title: HWND::default(),
            field: HWND::default(),
            clear: HWND::default(),
            status_ctl: HWND::default(),
            start_ctl: HWND::default(),
            version: HWND::default(),
            ok: HWND::default(),
            cancel: HWND::default(),
        });

        ui.title = child(
            hwnd,
            w!("STATIC"),
            w!("Keyboard shortcut"),
            WS_CHILD | WS_VISIBLE,
            ID_HOTKEY - 1,
            font_bold,
        );
        ui.field = CreateWindowExW(
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
        ui.clear = child(
            hwnd,
            w!("BUTTON"),
            w!("Clear"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            ID_CLEAR,
            font,
        );
        ui.status_ctl = child(
            hwnd,
            w!("STATIC"),
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE,
            ID_STATUS,
            font,
        );
        ui.start_ctl = child(
            hwnd,
            w!("BUTTON"),
            w!("Start with Windows"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
            ID_START,
            font,
        );
        ui.version = child(
            hwnd,
            w!("STATIC"),
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE,
            ID_VERSION,
            font,
        );
        set_text(ui.version, &format!("Tasca {}", env!("CARGO_PKG_VERSION")));
        ui.ok = child(
            hwnd,
            w!("BUTTON"),
            w!("OK"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            ID_OK,
            font,
        );
        ui.cancel = child(
            hwnd,
            w!("BUTTON"),
            w!("Cancel"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            ID_CANCEL,
            font,
        );

        set_text(ui.status_ctl, &ui.status);
        let checked = if ui.start_at_open { 1 } else { 0 };
        SendMessageW(
            ui.start_ctl,
            BM_SETCHECK,
            Some(WPARAM(checked as usize)),
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

        UI.with(|u| *u.borrow_mut() = Some(ui));
        relayout(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
        with_ui(|ui| {
            let _ = SetFocus(Some(ui.field));
        });
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
) -> HWND {
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
        )
        .unwrap_or_default();
        SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
        hwnd
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

fn relayout(hwnd: HWND) {
    let Some((dpi, style, exstyle)) = with_ui(|ui| {
        (
            ui.dpi,
            unsafe { GetWindowLongPtrW(ui.hwnd, GWL_STYLE) } as u32,
            unsafe { GetWindowLongPtrW(ui.hwnd, GWL_EXSTYLE) } as u32,
        )
    }) else {
        return;
    };
    let s = |dip: i32| px(dpi, dip);
    unsafe {
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
        with_ui(|ui| {
            place(ui.title, 16, 16, 388, 20);
            place(ui.field, 16, 40, 296, 30);
            place(ui.clear, 324, 40, 80, 30);
            place(ui.status_ctl, 16, 78, 388, 34);
            place(ui.start_ctl, 16, 122, 388, 24);
            place(ui.version, 16, 166, 200, 18);
            place(ui.ok, 224, 162, 80, 28);
            place(ui.cancel, 320, 162, 84, 28);
        });
    }
}

fn refont(dpi: u32) {
    let font = make_font(dpi, false);
    let bold = make_font(dpi, true);
    with_ui(|ui| {
        let old = std::mem::replace(&mut ui.font, font);
        let old_bold = std::mem::replace(&mut ui.font_bold, bold);
        unsafe {
            let _ = DeleteObject(HGDIOBJ(old.0));
            let _ = DeleteObject(HGDIOBJ(old_bold.0));
            for ctl in [
                ui.title,
                ui.clear,
                ui.status_ctl,
                ui.start_ctl,
                ui.version,
                ui.ok,
                ui.cancel,
            ] {
                SendMessageW(
                    ctl,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            SendMessageW(
                ui.title,
                WM_SETFONT,
                Some(WPARAM(bold.0 as usize)),
                Some(LPARAM(1)),
            );
        }
    });
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
                        with_ui(|ui| {
                            ui.pending = None;
                            ui.status = HINT.to_string();
                            ui.error = false;
                            set_text(ui.status_ctl, HINT);
                            invalidate(ui.field);
                        });
                    }
                    _ => {}
                }
            }
            Some(LRESULT(0))
        }
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            Some(LRESULT(0))
        }
        WM_ACTIVATE => {
            if (wp.0 & 0xffff) == WA_INACTIVE {
                with_ui(|ui| {
                    if ui.recording {
                        cancel_recording();
                    }
                });
            }
            None
        }
        WM_KEYDOWN if (wp.0 as u16) == VK_ESCAPE.0 => {
            let _ = unsafe { DestroyWindow(hwnd) };
            Some(LRESULT(0))
        }
        WM_DPICHANGED => {
            let dpi = dev_dpi().unwrap_or((wp.0 & 0xffff) as u32);
            with_ui(|ui| {
                ui.dpi = dpi;
            });
            refont(dpi);
            codeguarded_relayout(hwnd, lp);
            Some(LRESULT(0))
        }
        WM_CTLCOLORSTATIC => {
            let ctl = HWND(lp.0 as *mut _);
            let (status, version, error) = with_ui(|ui| (ui.status_ctl, ui.version, ui.error))
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

fn cleanup() {
    with_ui(|ui| {
        if let Some(hook) = ui.hook.take() {
            unsafe {
                let _ = UnhookWindowsHookEx(hook);
            }
        }
        unsafe {
            let _ = DeleteObject(HGDIOBJ(ui.font.0));
            let _ = DeleteObject(HGDIOBJ(ui.font_bold.0));
            let _ = DeleteObject(HGDIOBJ(ui.focus_brush.0));
        }
    });
    UI.with(|u| *u.borrow_mut() = None);
}

fn on_ok(hwnd: HWND) {
    let Some((pending, checked, parent)) = with_ui(|ui| {
        let checked = unsafe { SendMessageW(ui.start_ctl, BM_GETCHECK, None, None) }.0 == 1;
        (ui.pending, checked, ui.parent)
    }) else {
        return;
    };
    if let Err(e) = hotkey::change(parent, pending) {
        with_ui(|ui| {
            ui.status = e.clone();
            ui.error = true;
            set_text(ui.status_ctl, &e);
            invalidate(ui.status_ctl);
            invalidate(ui.field);
            let _ = unsafe { SetFocus(Some(ui.field)) };
        });
        return;
    }
    tray::refresh();

    let start_change = checked != startup::enabled();
    let saved = settings::Settings { shortcut: pending };
    std::thread::spawn(move || {
        if start_change {
            if let Err(e) = startup::set_enabled(checked) {
                log(format!("start with windows failed: {e}"));
            }
        }
        if let Err(e) = settings::save(&saved) {
            log(format!("settings save failed: {e}"));
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

fn start_recording() {
    with_ui(|ui| {
        if ui.recording {
            return;
        }
        ui.recording = true;
        ui.finishing = false;
        ui.mods = (false, false, false, false);
        ui.status = HINT.to_string();
        ui.error = false;
        set_text(ui.status_ctl, HINT);
        match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) } {
            Ok(hook) => {
                ui.hook = Some(hook);
                log("settings: recording a shortcut");
            }
            Err(e) => {
                ui.recording = false;
                ui.status = format!("Keyboard recorder could not start: {e}");
                ui.error = true;
                set_text(ui.status_ctl, &ui.status);
                log(format!("settings: SetWindowsHookExW failed: {e}"));
            }
        }
        invalidate(ui.field);
    });
}

/// Stop and remove the hook. `keep` false = discard a half-recorded change.
fn stop_recording() {
    with_ui(|ui| {
        ui.recording = false;
        ui.finishing = false;
        if let Some(hook) = ui.hook.take() {
            unsafe {
                let _ = UnhookWindowsHookEx(hook);
            }
        }
        match ui.pending {
            Some(hk) => log(format!("settings: recorded {}", hk.text())),
            None => log("settings: recorded none"),
        }
        invalidate(ui.field);
    });
}

fn cancel_recording() {
    with_ui(|ui| {
        ui.recording = false;
        ui.finishing = false;
        ui.mods = (false, false, false, false);
        if let Some(hook) = ui.hook.take() {
            unsafe {
                let _ = UnhookWindowsHookEx(hook);
            }
        }
        invalidate(ui.field);
    });
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum HookAction {
    Pass,
    Swallow,
    SwallowAndStop,
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
    let vk = kb.vkCode;

    let action = UI.with(|u| {
        let Ok(mut guard) = u.try_borrow_mut() else {
            return HookAction::Pass;
        };
        let Some(ui) = guard.as_mut() else {
            return HookAction::Pass;
        };
        if !ui.recording {
            return HookAction::Pass;
        }
        if ui.finishing {
            return HookAction::Swallow;
        }
        if let Some(slot) = modifier_slot(vk) {
            set_mod(&mut ui.mods, slot, down);
            invalidate(ui.field);
            return HookAction::Swallow;
        }
        if !down {
            return HookAction::Swallow;
        }
        if vk == VK_ESCAPE.0 as u32 {
            ui.finishing = true;
            return HookAction::SwallowAndStop;
        }
        let (ctrl, alt, shift, win) = ui.mods;
        if (vk == VK_BACK.0 as u32 || vk == VK_DELETE.0 as u32) && !(ctrl || alt || shift || win) {
            ui.pending = None;
            ui.status = HINT.to_string();
            ui.error = false;
            ui.finishing = true;
            set_text(ui.status_ctl, HINT);
            return HookAction::SwallowAndStop;
        }
        if hotkey::key_name(vk).is_none() {
            ui.status = "That key cannot be used in a shortcut.".to_string();
            ui.error = true;
            set_text(ui.status_ctl, &ui.status);
            return HookAction::Swallow;
        }
        let hk = Hotkey {
            ctrl,
            alt,
            shift,
            win,
            vk,
        };
        if !hk.is_safe() {
            ui.status = NEED_MODIFIER.to_string();
            ui.error = true;
            set_text(ui.status_ctl, &ui.status);
            return HookAction::Swallow;
        }
        ui.pending = Some(hk);
        ui.status = HINT.to_string();
        ui.error = false;
        ui.finishing = true;
        set_text(ui.status_ctl, HINT);
        HookAction::SwallowAndStop
    });

    match action {
        HookAction::Pass => unsafe { CallNextHookEx(None, code, wp, lp) },
        HookAction::Swallow => LRESULT(1),
        HookAction::SwallowAndStop => {
            if let Some(field) = with_ui(|ui| ui.field) {
                unsafe {
                    let _ = PostMessageW(Some(field), WM_RECORD_STOP, WPARAM(0), LPARAM(0));
                }
            }
            LRESULT(1)
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

unsafe extern "system" fn field_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                paint_field(hwnd);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_LBUTTONDOWN => {
                let _ = SetFocus(Some(hwnd));
                start_recording();
                LRESULT(0)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => match wp.0 as u16 {
                v if v == VK_ESCAPE.0 => {
                    if let Ok(parent) = GetParent(hwnd) {
                        let _ = PostMessageW(
                            Some(parent),
                            WM_COMMAND,
                            WPARAM(ID_CANCEL as usize),
                            LPARAM(0),
                        );
                    }
                    LRESULT(0)
                }
                v if v == VK_RETURN.0 || v == VK_SPACE.0 => {
                    start_recording();
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
