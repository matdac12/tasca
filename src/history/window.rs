//! The History window: its own class (popup, layered, topmost, tool window)
//! that, unlike the Shelf, takes keyboard focus while it is open. Placement
//! (pure `place` and `frame`), the fade-in, the wndproc and show/hide live
//! here; what it draws is `draw.rs`, what a key means is `state.rs`.

use std::cell::Cell;
use std::time::Instant;

use windows::core::{w, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, ScreenToClient, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::draw::{self, View};
use super::layout::Layout;
use super::state::{Event, Key};
use crate::log::log;

const CLASS: windows::core::PCWSTR = w!("Tasca.History");
/// The window's corner sits this far below-right of the pointer, in DIP.
const POINTER_GAP: f32 = 8.0;
/// Not exported by the `windows` features we enable (it lives in UI_Controls).
const WM_MOUSELEAVE: u32 = 0x02A3;
/// The fade-in runs on this window timer.
const T_FADE: usize = 1;
const FADE_TICK_MS: u32 = 10;
const FADE_MS: u128 = 100;

thread_local! {
    /// When the fade-in started; `None` = fully opaque.
    static FADE: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Where the window is on screen (for re-presenting during the fade).
    static ORIGIN: Cell<(i32, i32)> = const { Cell::new((0, 0)) };
    /// A `WM_MOUSELEAVE` is already requested.
    static TRACKING: Cell<bool> = const { Cell::new(false) };
}

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

/// Position and size of the window (panel plus shadow margin) for `layout`:
/// the panel is placed with `place`, the window is the panel less its margin.
/// Pure; called again whenever the Clip count changes.
pub fn frame(pointer: (i32, i32), gap: i32, layout: &Layout, work: Work) -> ((i32, i32), (i32, i32)) {
    let panel = place(pointer, gap, layout.panel_size_px(), work);
    let (ox, oy) = layout.panel_offset_px();
    ((panel.0 - ox, panel.1 - oy), layout.size_px())
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

/// Pixel gap between the pointer and the panel's corner at `scale`.
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
        CreateWindowExW(
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
        )
    }
}

/// Opacity of the fade-in after `elapsed_ms`: 0 up to 255 over `FADE_MS`.
fn fade_alpha(elapsed_ms: u128) -> u8 {
    (elapsed_ms.min(FADE_MS) * 255 / FADE_MS) as u8
}

fn alpha_now() -> u8 {
    match FADE.get() {
        Some(start) => fade_alpha(start.elapsed().as_millis()),
        None => 255,
    }
}

/// Render `view` and show (or move, or resize) the window at `at`; when it was
/// hidden, fade it in and take the keyboard. `false` when Windows refused the
/// foreground: the window would sit there without keys, so the caller closes
/// it again.
pub fn show(hwnd: HWND, view: &View, at: (i32, i32)) -> bool {
    unsafe {
        let was_visible = IsWindowVisible(hwnd).as_bool();
        if !was_visible {
            FADE.set(Some(Instant::now()));
        }
        ORIGIN.set(at);
        if let Err(e) = draw::render(hwnd, view, POINT { x: at.0, y: at.1 }, alpha_now()) {
            log(format!("history: render failed: {e}"));
        }
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        if !was_visible {
            if !SetForegroundWindow(hwnd).as_bool() {
                log("history: could not take the foreground, closing");
                return false;
            }
            SetTimer(Some(hwnd), T_FADE, FADE_TICK_MS, None);
        }
        true
    }
}

/// Closing is instant: no fade-out, so a paste is never delayed.
pub fn hide(hwnd: HWND) {
    unsafe {
        let _ = KillTimer(Some(hwnd), T_FADE);
        FADE.set(None);
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

/// One step of the fade-in: present the same bitmap a little more opaque.
fn fade_tick(hwnd: HWND) {
    let (x, y) = ORIGIN.get();
    if let Err(e) = draw::present(hwnd, POINT { x, y }, alpha_now()) {
        log(format!("history: present failed: {e}"));
    }
    if alpha_now() == 255 {
        FADE.set(None);
        unsafe {
            let _ = KillTimer(Some(hwnd), T_FADE);
        }
    }
}

fn xy(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32)
}

/// Ask for one `WM_MOUSELEAVE` when the pointer leaves the window.
fn track_leave(hwnd: HWND) {
    if !TRACKING.get() {
        let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
        TRACKING.set(unsafe { TrackMouseEvent(&mut tme) }.is_ok());
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
            WM_TIMER if wp.0 == T_FADE => {
                fade_tick(hwnd);
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
                // lParam is in screen coordinates here.
                let (sx, sy) = xy(lp);
                let mut pt = POINT { x: sx, y: sy };
                let _ = ScreenToClient(hwnd, &mut pt);
                super::on_wheel(delta, pt.x, pt.y);
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

    /// Whether the panel (not the shadow around it) lies inside `work`.
    fn panel_inside(origin: (i32, i32), layout: &Layout, work: Work) -> bool {
        let (ox, oy) = layout.panel_offset_px();
        let (pw, ph) = layout.panel_size_px();
        let (px, py) = (origin.0 + ox, origin.1 + oy);
        px >= work.left && py >= work.top && px + pw <= work.right && py + ph <= work.bottom
    }

    #[test]
    fn the_panel_opens_below_right_of_the_pointer_less_the_shadow_margin() {
        let layout = Layout::new(1.0, 3, 0, false);
        let (origin, size) = frame((500, 300), 8, &layout, WORK);
        assert_eq!(origin, (500 + 8 - 8, 300 + 8 - 10));
        assert_eq!(size, layout.size_px());
    }

    #[test]
    fn growing_rows_near_the_bottom_edge_move_the_window_up() {
        let one = Layout::new(1.0, 1, 0, false);
        let nine = Layout::new(1.0, 9, 0, false);
        let (at1, _) = frame((100, 900), 8, &one, WORK);
        let (at9, size9) = frame((100, 900), 8, &nine, WORK);
        assert!(panel_inside(at1, &one, WORK));
        assert!(panel_inside(at9, &nine, WORK), "{at9:?} {size9:?}");
        assert!(at9.1 < at1.1);
        // Shrinking again returns to the pointer.
        assert_eq!(frame((100, 900), 8, &one, WORK).0, at1);
    }

    #[test]
    fn the_panel_stays_on_the_work_area_near_any_edge_at_every_scale() {
        for scale in [1.0, 1.5, 2.0] {
            // The same 1920 x 1040 DIP work area on a monitor at this scale.
            let work = Work { left: 0, top: 0, right: (1920.0 * scale) as i32, bottom: (1040.0 * scale) as i32 };
            for rows in [0, 1, 3, 9, 30] {
                let layout = Layout::new(scale, rows, 0, false);
                let gap = pointer_gap(scale);
                let (r, b) = (work.right - 5, work.bottom - 5);
                for pointer in [(5, 5), (r, 5), (5, b), (r, b), (work.right / 2, work.bottom / 2)] {
                    let (origin, size) = frame(pointer, gap, &layout, work);
                    assert_eq!(size, layout.size_px());
                    assert!(panel_inside(origin, &layout, work), "scale {scale} rows {rows} pointer {pointer:?} -> {origin:?}");
                }
            }
        }
    }

    #[test]
    fn the_fade_in_takes_a_hundred_milliseconds() {
        assert_eq!(fade_alpha(0), 0);
        assert!(fade_alpha(50) > 100 && fade_alpha(50) < 150);
        assert_eq!(fade_alpha(100), 255);
        assert_eq!(fade_alpha(5_000), 255);
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
