//! The History window: its own class (popup, layered, topmost, tool window)
//! that, unlike the Shelf, takes keyboard focus while it is open. Placement
//! (pure `place`), the wndproc and show/hide live here; what it draws is
//! `draw.rs`, what a key means is `state.rs`.

use windows::core::{w, Result};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_ESCAPE, VK_RETURN, VK_UP};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::state::{Event, Key};
use crate::log::log;

const CLASS: windows::core::PCWSTR = w!("Tasca.History");
/// 0..255; the window is a little see-through.
const ALPHA: u8 = 245;
/// The window's corner sits this far below-right of the pointer, in DIP.
const POINTER_GAP: f32 = 8.0;

/// Work area of a monitor, in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Top-left for a window of `size` opened at `pointer`: just below-right of
/// it, then shifted left and up so it lies entirely inside `work`. A window
/// larger than the work area sticks to its top-left.
pub fn place(pointer: (i32, i32), gap: i32, size: (i32, i32), work: Work) -> (i32, i32) {
    let x = (pointer.0 + gap).min(work.right - size.0).max(work.left);
    let y = (pointer.1 + gap).min(work.bottom - size.1).max(work.top);
    (x, y)
}

/// Position and size of the window for `rows` Clips: sized by `draw`, then
/// placed with `place`. Pure; called again whenever the Clip count changes.
pub fn frame(pointer: (i32, i32), gap: i32, rows: usize, scale: f32, work: Work) -> ((i32, i32), (i32, i32)) {
    let size = super::draw::size_px(rows, scale);
    (place(pointer, gap, size, work), size)
}

/// The monitor under the pointer: pointer, work area and DPI.
pub fn pointer_monitor() -> ((i32, i32), Work, u32) {
    unsafe {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let r = mi.rcWork;
        ((p.x, p.y), Work { left: r.left, top: r.top, right: r.right, bottom: r.bottom }, dx)
    }
}

/// Pixel gap between the pointer and the window's corner at `scale`.
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
        if RegisterClassExW(&wc) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS,
            w!("Tasca History"),
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
        SetLayeredWindowAttributes(hwnd, COLORREF(0), ALPHA, LWA_ALPHA)?;
        Ok(hwnd)
    }
}

/// Show (or resize) the window at `at`, and take the keyboard when it was
/// hidden. `false` when Windows refused the foreground: the window would sit
/// there without keys, so the caller closes it again.
pub fn show(hwnd: HWND, at: (i32, i32), size: (i32, i32)) -> bool {
    unsafe {
        let was_visible = IsWindowVisible(hwnd).as_bool();
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), at.0, at.1, size.0, size.1, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        let _ = InvalidateRect(Some(hwnd), None, false);
        if !was_visible && !SetForegroundWindow(hwnd).as_bool() {
            log("history: could not take the foreground, closing");
            return false;
        }
        true
    }
}

pub fn hide(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

fn key_of(vk: u16) -> Option<Key> {
    match vk {
        v if v == VK_UP.0 => Some(Key::Up),
        v if v == VK_DOWN.0 => Some(Key::Down),
        v if v == VK_RETURN.0 => Some(Key::Enter),
        v if v == VK_ESCAPE.0 => Some(Key::Esc),
        _ => None,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                if let Some(view) = super::view() {
                    super::draw::paint(hdc, &view);
                }
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_ACTIVATE => {
                if wp.0 & 0xffff == WA_INACTIVE as usize {
                    super::dispatch(Event::Deactivated);
                    return LRESULT(0);
                }
                // Activated: the default handler gives this window the keyboard focus.
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            // Alt is usually still held from Alt+V, so Enter arrives as a SYSKEYDOWN.
            WM_KEYDOWN | WM_SYSKEYDOWN => match key_of(wp.0 as u16) {
                Some(key) => {
                    super::dispatch(Event::Key(key));
                    LRESULT(0)
                }
                None => DefWindowProcW(hwnd, msg, wp, lp),
            },
            // No menu bar to activate, and no beep for Alt+letter.
            WM_SYSCHAR => LRESULT(0),
            WM_SYSCOMMAND if wp.0 & 0xfff0 == SC_KEYMENU as usize => LRESULT(0),
            WM_DPICHANGED => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Work = Work { left: 0, top: 0, right: 1920, bottom: 1040 };
    const SIZE: (i32, i32) = (400, 408);

    #[test]
    fn opens_just_below_right_of_the_pointer() {
        assert_eq!(place((500, 300), 8, SIZE, WORK), (508, 308));
    }

    #[test]
    fn shifts_left_and_up_to_stay_in_the_work_area() {
        assert_eq!(place((1900, 1000), 8, SIZE, WORK), (1520, 632));
    }

    #[test]
    fn respects_a_work_area_that_does_not_start_at_zero() {
        let work = Work { left: -1920, top: 0, right: 0, bottom: 1040 };
        assert_eq!(place((-5, 10), 8, SIZE, work), (-400, 18));
        assert_eq!(place((-1925, 10), 8, SIZE, work), (-1917, 18));
    }

    #[test]
    fn a_window_larger_than_the_work_area_sticks_to_its_top_left() {
        assert_eq!(place((10, 10), 8, (400, 2000), WORK), (18, 0));
    }

    #[test]
    fn growing_rows_near_the_bottom_edge_move_the_window_up() {
        let work = Work { left: 0, top: 0, right: 1920, bottom: 1040 };
        let (at1, size1) = frame((100, 900), 8, 1, 1.0, work);
        let (at9, size9) = frame((100, 900), 8, 9, 1.0, work);
        assert_eq!((at1, size1), ((108, 908), (400, 56)));
        assert!(at9.1 + size9.1 <= work.bottom, "{at9:?} {size9:?}");
        assert!(at9.1 < at1.1);
        // Shrinking again returns to the pointer.
        assert_eq!(frame((100, 900), 8, 1, 1.0, work).0, at1);
    }

    #[test]
    fn only_the_four_keys_are_mapped() {
        assert_eq!(key_of(VK_RETURN.0), Some(Key::Enter));
        assert_eq!(key_of(VK_ESCAPE.0), Some(Key::Esc));
        assert_eq!(key_of(VK_UP.0), Some(Key::Up));
        assert_eq!(key_of(VK_DOWN.0), Some(Key::Down));
        assert_eq!(key_of(b'A' as u16), None);
    }
}
