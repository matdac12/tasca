//! The Snippets window: a popup, layered, topmost tool window that takes
//! keyboard focus while it is open, placed like the History.

use std::cell::Cell;
use std::time::Instant;

use windows::core::{w, Result};
use windows::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, ScreenToClient, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_CONTROL, VK_DELETE, VK_DOWN,
    VK_ESCAPE, VK_MENU, VK_NEXT, VK_PRIOR, VK_RETURN, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::draw::{self, View};
use super::layout::Layout;
use super::state::{Key, Mods};
use crate::log::log;

const CLASS: windows::core::PCWSTR = w!("Tasca.Snippets");
const POINTER_GAP: f32 = 8.0;
const WM_MOUSELEAVE: u32 = 0x02A3;
const T_FADE: usize = 1;
const FADE_TICK_MS: u32 = 10;
const FADE_MS: u128 = 100;
const KEY_REPEAT: isize = 1 << 30;

thread_local! {
    static FADE: Cell<Option<Instant>> = const { Cell::new(None) };
    static ORIGIN: Cell<(i32, i32)> = const { Cell::new((0, 0)) };
    static TRACKING: Cell<bool> = const { Cell::new(false) };
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

pub fn place(pointer: (i32, i32), gap: i32, size: (i32, i32), work: Work) -> (i32, i32) {
    let x = (pointer.0 + gap).min(work.right - size.0).max(work.left);
    let y = (pointer.1 + gap).min(work.bottom - size.1).max(work.top);
    (x, y)
}

pub fn frame(
    pointer: (i32, i32),
    gap: i32,
    layout: &Layout,
    work: Work,
) -> ((i32, i32), (i32, i32)) {
    let panel = place(pointer, gap, layout.panel_size_px(), work);
    let (ox, oy) = layout.panel_offset_px();
    ((panel.0 - ox, panel.1 - oy), layout.size_px())
}

pub fn cursor() -> (i32, i32) {
    let mut p = POINT::default();
    let _ = unsafe { GetCursorPos(&mut p) };
    (p.x, p.y)
}

pub fn pointer_monitor() -> ((i32, i32), Work, u32) {
    unsafe {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let r = mi.rcWork;
        (
            (p.x, p.y),
            Work {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            },
            crate::shelf::dev_dpi().unwrap_or(dx),
        )
    }
}

pub fn pointer_gap(scale: f32) -> i32 {
    (POINTER_GAP * scale).round() as i32
}

pub fn create() -> Result<HWND> {
    unsafe {
        let hinst = GetModuleHandleW(None)?.into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: CLASS,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return Err(windows::core::Error::from_thread());
        }
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS,
            w!("Tasca Snippets"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinst),
            None,
        )
    }
}

fn fade_alpha(elapsed_ms: u128) -> u8 {
    (elapsed_ms.min(FADE_MS) * 255 / FADE_MS) as u8
}

fn alpha_now() -> u8 {
    match FADE.get() {
        Some(start) => fade_alpha(start.elapsed().as_millis()),
        None => 255,
    }
}

pub fn show(hwnd: HWND, view: &View, at: (i32, i32)) -> bool {
    unsafe {
        let was_visible = IsWindowVisible(hwnd).as_bool();
        if !was_visible {
            FADE.set(Some(Instant::now()));
        }
        ORIGIN.set(at);
        if let Err(e) = draw::render(hwnd, view, POINT { x: at.0, y: at.1 }, alpha_now()) {
            log(format!("snippets: render failed: {e}"));
            if !was_visible {
                FADE.set(None);
                return false;
            }
        }
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        if !was_visible {
            if !SetForegroundWindow(hwnd).as_bool() {
                log("snippets: could not take the foreground, closing");
                return false;
            }
            SetTimer(Some(hwnd), T_FADE, FADE_TICK_MS, None);
        }
        true
    }
}

pub fn hide(hwnd: HWND) {
    unsafe {
        let _ = KillTimer(Some(hwnd), T_FADE);
        FADE.set(None);
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

fn fade_tick(hwnd: HWND) {
    let (x, y) = ORIGIN.get();
    if let Err(e) = draw::present(hwnd, POINT { x, y }, alpha_now()) {
        log(format!("snippets: present failed: {e}"));
    }
    if alpha_now() == 255 {
        FADE.set(None);
        unsafe {
            let _ = KillTimer(Some(hwnd), T_FADE);
        }
    }
}

fn xy(lp: LPARAM) -> (i32, i32) {
    (
        (lp.0 & 0xffff) as i16 as i32,
        ((lp.0 >> 16) & 0xffff) as i16 as i32,
    )
}

fn track_leave(hwnd: HWND) {
    if !TRACKING.get() {
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        TRACKING.set(unsafe { TrackMouseEvent(&mut tme) }.is_ok());
    }
}

fn key_of(vk: u16, mods: Mods) -> Option<Key> {
    match vk {
        v if v == VK_UP.0 => Some(Key::Up),
        v if v == VK_DOWN.0 => Some(Key::Down),
        v if v == VK_PRIOR.0 => Some(Key::PageUp),
        v if v == VK_NEXT.0 => Some(Key::PageDown),
        v if v == VK_RETURN.0 => Some(Key::Enter),
        v if v == VK_ESCAPE.0 => Some(Key::Esc),
        v if v == VK_DELETE.0 => Some(Key::Delete),
        v if v == b'N' as u16 && mods.ctrl => Some(Key::New),
        v if v == b'E' as u16 && mods.ctrl => Some(Key::Edit),
        v @ 0x31..=0x39 if mods.alt || mods.ctrl => Some(Key::Digit((v - 0x30) as u8)),
        _ => None,
    }
}

fn mods_now() -> Mods {
    let down = |vk: u16| unsafe { GetKeyState(vk as i32) } < 0;
    Mods {
        alt: down(VK_MENU.0),
        ctrl: down(VK_CONTROL.0),
        shift: down(VK_SHIFT.0),
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER if wp.0 == T_FADE => {
                fade_tick(hwnd);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == super::copy::TIMER => {
                super::copy::on_timer(hwnd);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                track_leave(hwnd);
                let (x, y) = xy(lp);
                super::on_mouse_move(x, y);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                TRACKING.set(false);
                super::on_mouse_leave();
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xffff) as u16 as i16 as i32;
                let (sx, sy) = xy(lp);
                let mut pt = POINT { x: sx, y: sy };
                let _ = ScreenToClient(hwnd, &mut pt);
                super::on_wheel(delta, pt.x, pt.y);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_ACTIVATE => {
                // The editor disables this window and takes focus. Closing here
                // would hide the editor, which is owned by it.
                if wp.0 & 0xffff == WA_INACTIVE as usize && !super::edit::is_open() {
                    super::dispatch(super::state::Event::Deactivated);
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let mods = mods_now();
                match key_of(wp.0 as u16, mods) {
                    Some(Key::Esc | Key::Enter | Key::Delete | Key::New | Key::Edit)
                        if lp.0 & KEY_REPEAT != 0 =>
                    {
                        LRESULT(0)
                    }
                    Some(key) => {
                        super::dispatch(super::state::Event::Key(key, mods));
                        LRESULT(0)
                    }
                    None => DefWindowProcW(hwnd, msg, wp, lp),
                }
            }
            WM_LBUTTONDOWN => {
                let (x, y) = xy(lp);
                super::on_click(x, y);
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                let (x, y) = xy(lp);
                super::on_right_click(x, y);
                LRESULT(0)
            }
            WM_CHAR | WM_SYSCHAR => LRESULT(0),
            WM_SYSCOMMAND if wp.0 & 0xfff0 == SC_KEYMENU as usize => LRESULT(0),
            WM_DPICHANGED => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Work = Work {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    };
    const NONE: Mods = Mods {
        alt: false,
        ctrl: false,
        shift: false,
    };

    #[test]
    fn opens_just_below_right_of_the_pointer_and_stays_inside_the_work_area() {
        let layout = Layout::new(1.0, 3);
        let (origin, size) = frame((500, 300), 8, &layout, WORK);
        assert_eq!(origin, (500, 298));
        assert_eq!(size, layout.size_px());
        assert_eq!(
            place((1900, 1000), 8, layout.panel_size_px(), WORK).0 + layout.panel_size_px().0,
            WORK.right
        );
    }

    #[test]
    fn the_fade_in_takes_a_hundred_milliseconds() {
        assert_eq!(fade_alpha(0), 0);
        assert_eq!(fade_alpha(100), 255);
        assert_eq!(fade_alpha(5_000), 255);
    }

    #[test]
    fn keys_map_to_navigation_copy_edit_and_new() {
        let ctrl = Mods { ctrl: true, ..NONE };
        assert_eq!(key_of(VK_RETURN.0, NONE), Some(Key::Enter));
        assert_eq!(key_of(VK_ESCAPE.0, NONE), Some(Key::Esc));
        assert_eq!(key_of(VK_DELETE.0, NONE), Some(Key::Delete));
        assert_eq!(key_of(b'N' as u16, ctrl), Some(Key::New));
        assert_eq!(key_of(b'N' as u16, NONE), None);
        assert_eq!(key_of(b'E' as u16, ctrl), Some(Key::Edit));
        assert_eq!(
            key_of(b'2' as u16, Mods { alt: true, ..NONE }),
            Some(Key::Digit(2))
        );
        assert_eq!(key_of(b'2' as u16, NONE), None);
    }
}
