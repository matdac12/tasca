//! The Shelf's window messages: animation timer, probe results, mouse (hover,
//! selection, Clear all, double-click open, wheel, right-click menu). The
//! resident's wndproc offers every message here first.

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, DestroyWindow, GetForegroundWindow, GetSystemMetrics,
    PostMessageW, SetForegroundWindow, TrackPopupMenu, MF_GRAYED, MF_SEPARATOR, MF_STRING, SM_CXDRAG, SM_CYDRAG,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_CAPTURECHANGED, WM_DPICHANGED, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NULL, WM_RBUTTONUP, WM_TIMER,
};

use super::layout::{past_drag_threshold, Hit};
use super::model::{ItemId, Mods};
use super::{open, takeout, with, Press, Shelf, T_ANIM};
use crate::log::log;
use crate::proto::WM_PROBED;

/// Not exported by the `windows` features we enable (it lives in UI_Controls).
const WM_MOUSELEAVE: u32 = 0x02A3;
const MK_LBUTTON: usize = 0x0001;
const MK_SHIFT: usize = 0x0004;
const MK_CONTROL: usize = 0x0008;

const ID_REMOVE: i32 = 1;
const ID_CLEAR: i32 = 2;
const ID_QUIT: i32 = 3;

fn xy(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32)
}

fn mods(wp: WPARAM) -> Mods {
    Mods { ctrl: wp.0 & MK_CONTROL != 0, shift: wp.0 & MK_SHIFT != 0 }
}

fn row_id(s: &Shelf, hit: Hit) -> Option<ItemId> {
    match hit {
        Hit::Row(i) => s.model.items().get(i).map(|it| it.id),
        _ => None,
    }
}

fn set_hover(s: &mut Shelf, hover: Option<ItemId>) {
    if s.hover != hover {
        s.hover = hover;
        s.refresh();
    }
}

/// Handle a Shelf message; `None` = not ours, let the resident handle it.
pub fn handle(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_TIMER if wp.0 == T_ANIM => super::tick(),
        // The Shelf positions itself on every show; ignore the suggested rect.
        WM_DPICHANGED => {}
        WM_PROBED => super::on_probed(),
        WM_MOUSEMOVE => mouse_move(hwnd, wp, lp),
        WM_MOUSELEAVE => {
            with(|s| {
                s.tracking_leave = false;
                set_hover(s, None);
            });
        }
        WM_LBUTTONDOWN => button_down(hwnd, wp, lp),
        WM_LBUTTONUP => {
            let (x, y) = xy(lp);
            with(|s| {
                let press = s.press.take();
                let clear_armed = std::mem::take(&mut s.clear_armed);
                match s.layout.hit(x, y, s.scroll) {
                    // Only a press that also started on "Clear all".
                    Hit::ClearAll if clear_armed => {
                        log("Clear all");
                        s.clear_all();
                    }
                    Hit::Row(i) if press.is_some_and(|p| p.deferred && s.model.items()[i].id == p.id) => {
                        s.model.click(i, Mods::default());
                        s.refresh();
                    }
                    _ => {}
                }
            });
            // After the press is handled: releasing sends WM_CAPTURECHANGED.
            if unsafe { GetCapture() } == hwnd {
                let _ = unsafe { ReleaseCapture() };
            }
        }
        WM_CAPTURECHANGED => {
            with(|s| {
                s.press = None;
                s.clear_armed = false;
            });
        }
        // CS_DBLCLKS turns the second press into this: it is still a press (for
        // selection and dragging), and a plain one also opens the row.
        WM_LBUTTONDBLCLK => {
            button_down(hwnd, wp, lp);
            let (x, y) = xy(lp);
            let plain = mods(wp) == Mods::default();
            let file = with(|s| match s.layout.hit(x, y, s.scroll) {
                Hit::Row(i) if plain => s.model.items().get(i).filter(|it| !it.missing()).map(|it| it.files[0].clone()),
                _ => None,
            })
            .flatten();
            if let Some(f) = file {
                log(format!("open {}", f.display()));
                open::open(f);
            }
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xffff) as u16 as i16 as i32;
            // lParam is in screen coordinates here.
            let (sx, sy) = xy(lp);
            let mut pt = POINT { x: sx, y: sy };
            let _ = unsafe { ScreenToClient(hwnd, &mut pt) };
            with(|s| {
                let scroll = s.layout.wheel(s.scroll, delta);
                let hover = row_id(s, s.layout.hit(pt.x, pt.y, scroll));
                if scroll != s.scroll || hover != s.hover {
                    s.scroll = scroll;
                    s.hover = hover;
                    s.refresh();
                }
            });
        }
        WM_RBUTTONUP => context_menu(hwnd, lp),
        _ => return None,
    }
    Some(LRESULT(0))
}

fn button_down(hwnd: HWND, wp: WPARAM, lp: LPARAM) {
    let (x, y) = xy(lp);
    let m = mods(wp);
    with(|s| match s.layout.hit(x, y, s.scroll) {
        Hit::Row(i) => {
            let deferred = s.model.press(i, m);
            s.press = Some(Press { at: (x, y), id: s.model.items()[i].id, deferred });
            if !deferred {
                s.refresh();
            }
        }
        Hit::Header | Hit::Panel => {
            if s.model.select_none() {
                s.refresh();
            }
        }
        Hit::ClearAll => s.clear_armed = true,
        Hit::Margin => {}
    });
    unsafe { SetCapture(hwnd) };
}

fn mouse_move(hwnd: HWND, wp: WPARAM, lp: LPARAM) {
    let (x, y) = xy(lp);
    let take_out = with(|s| {
        if let Some(p) = &s.press {
            let (cx, cy) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
            if wp.0 & MK_LBUTTON != 0 && past_drag_threshold(p.at, (x, y), cx, cy) {
                let ids = s.model.index_of(p.id).map(|i| s.model.targets(i)).unwrap_or_default();
                s.press = None;
                return Some(ids);
            }
            return None;
        }
        if !s.tracking_leave {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: s.hwnd,
                dwHoverTime: 0,
            };
            s.tracking_leave = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
        }
        let hover = row_id(s, s.layout.hit(x, y, s.scroll));
        set_hover(s, hover);
        None
    })
    .flatten();
    if let Some(ids) = take_out {
        let _ = unsafe { ReleaseCapture() };
        if !ids.is_empty() {
            takeout::start(hwnd, ids);
        }
    }
}

/// Native menu: Remove (the row or the selection), Clear all, Quit. The Shelf
/// takes the foreground for it (or the menu won't dismiss), then hands it back.
fn context_menu(hwnd: HWND, lp: LPARAM) {
    let (x, y) = xy(lp);
    let Some((targets, any)) = with(|s| {
        let targets = match s.layout.hit(x, y, s.scroll) {
            Hit::Row(i) => s.model.targets(i),
            _ => s.model.selected_ids(),
        };
        (targets, !s.model.is_empty())
    }) else {
        return;
    };
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let grayed = |on: bool| if on { MF_STRING } else { MF_STRING | MF_GRAYED };
        let _ = AppendMenuW(menu, grayed(!targets.is_empty()), ID_REMOVE as usize, w!("Remove"));
        let _ = AppendMenuW(menu, grayed(any), ID_CLEAR as usize, w!("Clear all"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT as usize, w!("Quit"));
        let mut pt = POINT { x, y };
        let _ = ClientToScreen(hwnd, &mut pt);
        let previous = GetForegroundWindow();
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, None, hwnd, None).0;
        // Documented workaround so the next menu dismisses properly.
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        // Nobody else took the foreground while the menu was up: give it back.
        if GetForegroundWindow() == hwnd && !previous.is_invalid() && previous != hwnd {
            let _ = SetForegroundWindow(previous);
        }
        match cmd {
            ID_REMOVE => {
                log(format!("menu: remove {targets:?}"));
                super::remove(&targets);
            }
            ID_CLEAR => {
                log("menu: clear all");
                with(Shelf::clear_all);
            }
            ID_QUIT => {
                log("menu: quit");
                let _ = DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}
