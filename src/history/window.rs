//! The History window: its own class (popup, layered, topmost, tool window)
//! that, unlike the Shelf, takes keyboard focus while it is open. Placement
//! (pure `place` and `frame`), the fade-in, the wndproc and show/hide live
//! here; what it draws is `draw.rs`, what a key means is `state.rs`.

use std::cell::Cell;
use std::time::Instant;

use windows::core::{w, Result};
use windows::Win32::Foundation::{GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, ScreenToClient, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_ESCAPE, VK_MENU,
    VK_NEXT, VK_PRIOR, VK_RETURN, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::draw::{self, View};
use super::layout::Layout;
use super::state::{Event, Key, Mods};
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
    /// The first half of a typed surrogate pair (characters beyond the BMP).
    static HIGH_SURROGATE: Cell<u16> = const { Cell::new(0) };
}

/// Bit 30 of a key message's `lParam`: the key was already down.
const KEY_REPEAT: isize = 1 << 30;

/// `MK_SHIFT` in a mouse message's `wParam`.
const MK_SHIFT: usize = 0x0004;

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

/// The pointer position in screen pixels.
pub fn cursor() -> (i32, i32) {
    let mut p = POINT::default();
    let _ = unsafe { GetCursorPos(&mut p) };
    (p.x, p.y)
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
        ((p.x, p.y), Work { left: r.left, top: r.top, right: r.right, bottom: r.bottom }, crate::shelf::dev_dpi().unwrap_or(dx))
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
        // A second call finds the class already there, which is fine.
        if RegisterClassExW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
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
            if !was_visible {
                // Nothing drawn for this opening: never show or focus a stale frame.
                FADE.set(None);
                return false;
            }
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

/// The key a virtual-key code stands for. A digit counts only with Alt or
/// Ctrl (the number shortcuts); a bare digit is left to `WM_CHAR` and goes
/// into the search. Delete and P count only with Ctrl (a bare P is typed).
fn key_of(vk: u16, mods: Mods) -> Option<Key> {
    match vk {
        v if v == VK_UP.0 => Some(Key::Up),
        v if v == VK_DOWN.0 => Some(Key::Down),
        v if v == VK_PRIOR.0 => Some(Key::PageUp),
        v if v == VK_NEXT.0 => Some(Key::PageDown),
        v if v == VK_RETURN.0 => Some(Key::Enter),
        v if v == VK_ESCAPE.0 => Some(Key::Esc),
        v if v == VK_BACK.0 => Some(Key::Backspace),
        v if v == VK_DELETE.0 && mods.ctrl => Some(Key::Delete),
        v if v == b'P' as u16 && mods.ctrl => Some(Key::Pin),
        v @ 0x31..=0x39 if mods.alt || mods.ctrl => Some(Key::Digit((v - 0x30) as u8)),
        _ => None,
    }
}

/// Alt, Ctrl and Shift as they are right now (the state this message saw).
fn mods_now() -> Mods {
    let down = |vk: u16| unsafe { GetKeyState(vk as i32) } < 0;
    Mods { alt: down(VK_MENU.0), ctrl: down(VK_CONTROL.0), shift: down(VK_SHIFT.0) }
}

/// The character a `WM_CHAR` carries; a surrogate pair arrives as two messages.
fn char_of(unit: u16) -> Option<char> {
    if (0xD800..0xDC00).contains(&unit) {
        HIGH_SURROGATE.set(unit);
        return None;
    }
    let high = HIGH_SURROGATE.replace(0);
    if (0xDC00..0xE000).contains(&unit) {
        if high == 0 {
            return None;
        }
        return char::decode_utf16([high, unit]).next()?.ok();
    }
    char::from_u32(unit as u32)
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
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let mods = mods_now();
                match key_of(wp.0 as u16, mods) {
                    // A held Esc or Enter must not clear, then close (or paste twice);
                    // a held Ctrl+P or Ctrl+Delete must not flip or delete again and again.
                    Some(Key::Esc | Key::Enter | Key::Pin | Key::Delete) if lp.0 & KEY_REPEAT != 0 => LRESULT(0),
                    Some(key) => {
                        super::dispatch(Event::Key(key, mods));
                        LRESULT(0)
                    }
                    None => DefWindowProcW(hwnd, msg, wp, lp),
                }
            }
            // Typing goes into the search (Alt-held letters arrive as WM_SYSCHAR and are ignored).
            WM_CHAR => {
                if let Some(c) = char_of(wp.0 as u16) {
                    super::dispatch(Event::Char(c));
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let (x, y) = xy(lp);
                super::on_click(x, y, wp.0 & MK_SHIFT != 0);
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                let (x, y) = xy(lp);
                super::on_right_click(x, y);
                LRESULT(0)
            }
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

    const NONE: Mods = Mods { alt: false, ctrl: false, shift: false };
    const ALT: Mods = Mods { alt: true, ctrl: false, shift: false };

    #[test]
    fn the_navigation_and_edit_keys_are_mapped() {
        assert_eq!(key_of(VK_RETURN.0, NONE), Some(Key::Enter));
        assert_eq!(key_of(VK_ESCAPE.0, NONE), Some(Key::Esc));
        assert_eq!(key_of(VK_UP.0, NONE), Some(Key::Up));
        assert_eq!(key_of(VK_DOWN.0, NONE), Some(Key::Down));
        assert_eq!(key_of(VK_PRIOR.0, NONE), Some(Key::PageUp));
        assert_eq!(key_of(VK_NEXT.0, NONE), Some(Key::PageDown));
        assert_eq!(key_of(VK_BACK.0, NONE), Some(Key::Backspace));
        assert_eq!(key_of(b'A' as u16, ALT), None);
    }

    #[test]
    fn a_digit_is_a_number_shortcut_only_with_alt_or_ctrl() {
        assert_eq!(key_of(b'3' as u16, ALT), Some(Key::Digit(3)));
        assert_eq!(key_of(b'9' as u16, Mods { ctrl: true, ..NONE }), Some(Key::Digit(9)));
        assert_eq!(key_of(b'3' as u16, NONE), None);
        assert_eq!(key_of(b'3' as u16, Mods { shift: true, ..NONE }), None);
        assert_eq!(key_of(b'0' as u16, ALT), None);
    }

    #[test]
    fn ctrl_p_and_ctrl_delete_are_keys_but_bare_p_and_delete_are_not() {
        let ctrl = Mods { ctrl: true, ..NONE };
        assert_eq!(key_of(b'P' as u16, ctrl), Some(Key::Pin));
        assert_eq!(key_of(VK_DELETE.0, ctrl), Some(Key::Delete));
        assert_eq!(key_of(b'P' as u16, NONE), None);
        assert_eq!(key_of(VK_DELETE.0, NONE), None);
        assert_eq!(key_of(VK_DELETE.0, ALT), None);
    }

    #[test]
    fn typed_characters_include_pairs_beyond_the_bmp() {
        assert_eq!(char_of('a' as u16), Some('a'));
        assert_eq!(char_of('è' as u16), Some('è'));
        // U+1F600 is the pair D83D DE00.
        assert_eq!(char_of(0xD83D), None);
        assert_eq!(char_of(0xDE00), char::from_u32(0x1F600));
        assert_eq!(char_of(0xDE00), None);
    }
}
