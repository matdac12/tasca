//! The create/edit window: a normal Win32 dialog (title, text, Save, Cancel)
//! so a Snippet can be typed with a real edit control. It runs its own message
//! loop, like a menu, and the caller holds no `RefCell` borrow across it.
//!
//! Edit controls store newlines as CR LF. The rest of Tasca stores LF.

use std::cell::{Cell, RefCell};
use std::sync::Once;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, GetMonitorInfoW, MonitorFromPoint, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, COLOR_BTNFACE, DEFAULT_CHARSET, DEFAULT_PITCH, FF_DONTCARE, HBRUSH, HFONT,
    HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::log::log;
use crate::tray;

use super::store::MAX_BODY;
use super::text::body_error;

const CLASS: PCWSTR = w!("Tasca.SnippetEditor");
const CLIENT_W: i32 = 440;
const CLIENT_H: i32 = 376;
const ID_TITLE_LABEL: i32 = 98;
const ID_TEXT_LABEL: i32 = 99;
const ID_TITLE: i32 = 100;
const ID_BODY: i32 = 101;
const ID_STATUS: i32 = 102;
const ID_SAVE: i32 = 1;
const ID_CANCEL: i32 = 2;
/// `EM_SETLIMITTEXT`.
const EM_LIMITTEXT: u32 = 0x00C5;

struct Dialog {
    hwnd: HWND,
    title: HWND,
    body: HWND,
    status: HWND,
    font: HFONT,
    font_bold: HFONT,
    closing: bool,
}

thread_local! {
    static OPEN: Cell<bool> = const { Cell::new(false) };
    static DONE: Cell<bool> = const { Cell::new(false) };
    static OUT: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
    static DIALOG: RefCell<Option<Dialog>> = const { RefCell::new(None) };
}

/// The editor is up. The panel must not close on the deactivation that causes.
pub fn is_open() -> bool {
    OPEN.get()
}

/// Newlines as an edit control wants them, and back.
fn to_edit(s: &str) -> String {
    let mut out = String::new();
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            out.push_str("\r\n");
        }
        out.push_str(line.trim_end_matches('\r'));
    }
    out
}

fn from_edit(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Show the editor over `owner`. `None` when it was cancelled or could not open.
pub fn ask(owner: HWND, title: &str, body: &str) -> Option<(String, String)> {
    if OPEN.get() {
        return None;
    }
    OPEN.set(true);
    DONE.set(false);
    OUT.with(|o| *o.borrow_mut() = None);
    let result = unsafe { run(owner, title, body) };
    OPEN.set(false);
    result
}

unsafe fn run(owner: HWND, title: &str, body: &str) -> Option<(String, String)> {
    unsafe {
        let Ok(hwnd) = create(owner, title, body) else {
            log("snippets: editor failed to open");
            return None;
        };
        let _ = EnableWindow(owner, false);
        let mut msg = MSG::default();
        while !DONE.get() {
            let ret = GetMessageW(&mut msg, None, 0, 0).0;
            if ret == 0 {
                PostQuitMessage(msg.wParam.0 as i32);
                break;
            }
            if ret < 0 {
                break;
            }
            if msg.message == WM_KEYDOWN
                && msg.wParam.0 == VK_ESCAPE.0 as usize
                && msg.lParam.0 & (1 << 30) == 0
            {
                close_dialog(false);
                continue;
            }
            if IsDialogMessageW(hwnd, &msg).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = EnableWindow(owner, true);
        OUT.with(|o| o.borrow_mut().take())
    }
}

fn px(dpi: u32, dip: i32) -> i32 {
    (dip as f32 * dpi as f32 / 96.0).round() as i32
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

fn set_text(hwnd: HWND, text: &str) {
    let _ = unsafe { SetWindowTextW(hwnd, &HSTRING::from(text)) };
}

fn control_text(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) }.max(0) as usize;
    let mut buf = vec![0u16; len + 1];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}

unsafe fn create(owner: HWND, title: &str, body: &str) -> windows::core::Result<HWND> {
    unsafe {
        let hinst = GetModuleHandleW(None)?.into();
        register(hinst);
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
        let dpi = crate::shelf::dev_dpi().unwrap_or(dx);
        let _ = dy;
        let style = WS_CAPTION | WS_SYSMENU;
        let exstyle = WS_EX_TOPMOST | WS_EX_CONTROLPARENT;
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: px(dpi, CLIENT_W),
            bottom: px(dpi, CLIENT_H),
        };
        let _ = AdjustWindowRectExForDpi(&mut rc, style, false, exstyle, dpi);
        let w = rc.right - rc.left;
        let h = rc.bottom - rc.top;
        let mut or = RECT::default();
        let _ = GetWindowRect(owner, &mut or);
        let (ow, oh) = (or.right - or.left, or.bottom - or.top);
        let (mut x, mut y) = if ow > 50 && oh > 50 {
            (or.left + (ow - w) / 2, or.top + (oh - h) / 2)
        } else {
            let work = mi.rcWork;
            (
                work.left + (work.right - work.left - w) / 2,
                work.top + (work.bottom - work.top - h) / 2,
            )
        };
        let work = mi.rcWork;
        x = x.min(work.right - w).max(work.left);
        y = y.min(work.bottom - h).max(work.top);
        let hwnd = CreateWindowExW(
            exstyle,
            CLASS,
            w!("Snippet"),
            style,
            x,
            y,
            w,
            h,
            Some(owner),
            None,
            Some(hinst),
            None,
        )?;
        let font = make_font(dpi, false);
        let font_bold = make_font(dpi, true);
        let title_ctl = child(
            hwnd,
            w!("EDIT"),
            WS_EX_CLIENTEDGE,
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            ID_TITLE,
            font,
        )?;
        let body_ctl = child(
            hwnd,
            w!("EDIT"),
            WS_EX_CLIENTEDGE,
            WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | WS_VSCROLL
                | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32),
            ID_BODY,
            font,
        )?;
        let status = child(
            hwnd,
            w!("STATIC"),
            WINDOW_EX_STYLE(0),
            WS_CHILD | WS_VISIBLE,
            ID_STATUS,
            font,
        )?;
        let title_label = child(
            hwnd,
            w!("STATIC"),
            WINDOW_EX_STYLE(0),
            WS_CHILD | WS_VISIBLE,
            ID_TITLE_LABEL,
            font_bold,
        )?;
        let text_label = child(
            hwnd,
            w!("STATIC"),
            WINDOW_EX_STYLE(0),
            WS_CHILD | WS_VISIBLE,
            ID_TEXT_LABEL,
            font_bold,
        )?;
        let save = child(
            hwnd,
            w!("BUTTON"),
            WINDOW_EX_STYLE(0),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            ID_SAVE,
            font,
        )?;
        let cancel = child(
            hwnd,
            w!("BUTTON"),
            WINDOW_EX_STYLE(0),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            ID_CANCEL,
            font,
        )?;
        set_text(title_label, "Title");
        set_text(text_label, "Text");
        set_text(save, "Save");
        set_text(cancel, "Cancel");
        set_text(status, "Title is optional");
        set_text(title_ctl, title);
        set_text(body_ctl, &to_edit(body));
        let _ = SendMessageW(title_ctl, EM_LIMITTEXT, Some(WPARAM(80)), None);
        let _ = SendMessageW(body_ctl, EM_LIMITTEXT, Some(WPARAM(MAX_BODY)), None);
        place(
            dpi,
            &Placed {
                title_label,
                title: title_ctl,
                text_label,
                body: body_ctl,
                status,
                save,
                cancel,
            },
        );
        if let Some(icon) = tray::load_icon(false) {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(icon.0 as isize)),
            );
        }
        DIALOG.with(|d| {
            *d.borrow_mut() = Some(Dialog {
                hwnd,
                title: title_ctl,
                body: body_ctl,
                status,
                font,
                font_bold,
                closing: false,
            })
        });
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(title_ctl));
        Ok(hwnd)
    }
}

fn child(
    parent: HWND,
    class: PCWSTR,
    ex: WINDOW_EX_STYLE,
    style: WINDOW_STYLE,
    id: i32,
    font: HFONT,
) -> windows::core::Result<HWND> {
    unsafe {
        let hwnd = CreateWindowExW(
            ex,
            class,
            PCWSTR::null(),
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
        let _ = SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
        Ok(hwnd)
    }
}

struct Placed {
    title_label: HWND,
    title: HWND,
    text_label: HWND,
    body: HWND,
    status: HWND,
    save: HWND,
    cancel: HWND,
}

fn place(dpi: u32, c: &Placed) {
    let s = |dip: i32| px(dpi, dip);
    unsafe {
        let _ = MoveWindow(c.title_label, s(16), s(16), s(200), s(16), true);
        let _ = MoveWindow(c.title, s(16), s(36), s(408), s(26), true);
        let _ = MoveWindow(c.text_label, s(16), s(74), s(200), s(16), true);
        let _ = MoveWindow(c.body, s(16), s(96), s(408), s(200), true);
        let _ = MoveWindow(c.status, s(16), s(304), s(220), s(18), true);
        let _ = MoveWindow(c.save, s(240), s(332), s(88), s(28), true);
        let _ = MoveWindow(c.cancel, s(336), s(332), s(88), s(28), true);
    }
}

fn register(hinst: windows::Win32::Foundation::HINSTANCE) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 as usize + 1) as *mut _),
            lpszClassName: CLASS,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            log("snippets: editor class registration failed");
        }
    });
}

fn close_dialog(save: bool) {
    let snap = DIALOG.with(|d| {
        d.borrow()
            .as_ref()
            .filter(|dialog| !dialog.closing)
            .map(|dialog| (dialog.hwnd, dialog.title, dialog.body, dialog.status))
    });
    let Some((hwnd, title, body, status)) = snap else {
        return;
    };
    if save {
        let title = control_text(title).trim().to_string();
        let body = from_edit(&control_text(body)).trim_end().to_string();
        if let Some(err) = body_error(&body) {
            set_text(status, err);
            return;
        }
        OUT.with(|o| *o.borrow_mut() = Some((title, body)));
    }
    DIALOG.with(|d| {
        if let Some(dialog) = d.borrow_mut().as_mut() {
            dialog.closing = true;
        }
    });
    DONE.set(true);
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = (wp.0 & 0xffff) as i32;
                if id == ID_SAVE || id == ID_CANCEL {
                    close_dialog(id == ID_SAVE);
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_CLOSE => {
                close_dialog(false);
                LRESULT(0)
            }
            WM_DESTROY => {
                let fonts = DIALOG.with(|d| {
                    d.borrow_mut()
                        .take()
                        .map(|dialog| (dialog.font, dialog.font_bold))
                });
                if let Some((font, bold)) = fonts {
                    let _ = DeleteObject(HGDIOBJ(font.0));
                    let _ = DeleteObject(HGDIOBJ(bold.0));
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newlines_round_trip_through_the_edit_control() {
        assert_eq!(to_edit("a\nb"), "a\r\nb");
        assert_eq!(from_edit("a\r\nb\rc"), "a\nb\nc");
        assert_eq!(from_edit(&to_edit("a\nb\n")), "a\nb\n");
        assert_eq!(to_edit(""), "");
    }
}
