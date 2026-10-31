//! The notification-area icon: add/modify/remove, the left-click toggle, the
//! right-click menu, and re-adding it after an Explorer restart
//! (`TaskbarCreated`). All on the resident's UI thread.

use std::cell::RefCell;
use std::sync::{Mutex, OnceLock};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, DestroyWindow, GetCursorPos, GetSystemMetrics,
    LoadImageW, PostMessageW, SetForegroundWindow, TrackPopupMenu, HICON, IMAGE_ICON, LR_SHARED,
    MF_GRAYED, MF_SEPARATOR, MF_STRING, SM_CXICON, SM_CXSMICON, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    WM_CONTEXTMENU, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
};

use crate::hotkey::{self, Hotkey, Slot};
use crate::log::log;
use crate::proto::{WM_SHOW_HISTORY, WM_TRAY};
use crate::{settings_win, shelf};

/// Resource id of the icon in `tasca.rc`.
const ICON_ID: u16 = 1;
const TRAY_ID: u32 = 1;

const ID_SHOW: usize = 1;
const ID_SETTINGS: usize = 2;
const ID_QUIT: usize = 3;
const ID_HISTORY: usize = 4;

/// `NIN_SELECT` (NOTIFYICON_VERSION_4); harmless in classic mode.
const NIN_SELECT: u32 = 1024;

struct State {
    hwnd: HWND,
    icon: Option<HICON>,
    tip: String,
}

/// Failure from the last background apply of the settings, shown in the
/// tooltip until the next apply succeeds. Written by the settings worker
/// thread, read on the UI thread.
static NOTICE: Mutex<Option<String>> = Mutex::new(None);

thread_local! {
    static TRAY: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// The message Explorer broadcasts when it (re)starts; register it once.
pub fn taskbar_created() -> u32 {
    static MSG: OnceLock<u32> = OnceLock::new();
    *MSG.get_or_init(|| unsafe {
        windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW(w!("TaskbarCreated"))
    })
}

/// Load the embedded 16/20/24/32/48/256 icon at the shell's small (tray) or
/// normal (window) size. `LR_SHARED`: Windows owns it, never destroy it.
pub fn load_icon(small: bool) -> Option<HICON> {
    let hinst = unsafe { GetModuleHandleW(None) }.ok()?.into();
    let px = unsafe { GetSystemMetrics(if small { SM_CXSMICON } else { SM_CXICON }) };
    unsafe {
        LoadImageW(
            Some(hinst),
            PCWSTR(ICON_ID as usize as *const u16),
            IMAGE_ICON,
            px,
            px,
            LR_SHARED,
        )
    }
    .ok()
    .map(|h| HICON(h.0))
}

fn data(hwnd: HWND, icon: Option<HICON>, tip: &str) -> NOTIFYICONDATAW {
    let mut d = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon.unwrap_or_default(),
        ..Default::default()
    };
    // szTip is [u16;128], always NUL-terminated (the struct starts zeroed).
    for (dst, src) in d.szTip.iter_mut().zip(tip.encode_utf16()).take(127) {
        *dst = src;
    }
    d
}

/// Add the icon (called once the resident window exists).
pub fn add(hwnd: HWND, tip: String) {
    let icon = load_icon(true);
    if icon.is_none() {
        log("tray: could not load the icon resource");
    }
    let d = data(hwnd, icon, &tip);
    let added = unsafe { Shell_NotifyIconW(NIM_ADD, &d) }.as_bool();
    if !added {
        log("tray: Shell_NotifyIcon(NIM_ADD) failed");
    }
    TRAY.with(|t| *t.borrow_mut() = Some(State { hwnd, icon, tip }));
}

/// Copy the icon data out. `Shell_NotifyIconW` pumps messages (a re-entrant
/// `TaskbarCreated` calls back in here), so no `TRAY` borrow may span it.
fn snapshot() -> Option<(HWND, Option<HICON>, String)> {
    TRAY.with(|t| {
        let t = t.try_borrow().ok()?;
        t.as_ref().map(|s| (s.hwnd, s.icon, s.tip.clone()))
    })
}

/// Re-add after Explorer restarted (the shell forgets every icon).
pub fn re_add() {
    let Some((hwnd, icon, tip)) = snapshot() else {
        return;
    };
    let d = data(hwnd, icon, &tip);
    if unsafe { Shell_NotifyIconW(NIM_ADD, &d) }.as_bool() {
        log("tray: re-added after TaskbarCreated");
    } else {
        log("tray: re-add after TaskbarCreated failed");
    }
}

/// Update the tooltip (shortcut changed or registration failed).
pub fn update_tip(tip: &str) {
    let changed = TRAY.with(|t| {
        let mut t = t.try_borrow_mut().ok()?;
        let state = t.as_mut()?;
        if state.tip == tip {
            return None;
        }
        state.tip = tip.to_string();
        Some((state.hwnd, state.icon))
    });
    let Some((hwnd, icon)) = changed else { return };
    let d = data(hwnd, icon, tip);
    if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) }.as_bool() {
        log("tray: Shell_NotifyIcon(NIM_MODIFY) failed");
    }
}

/// Failure from the last background apply of the settings (shown in the
/// tooltip until the next apply succeeds).
pub fn set_notice(notice: Option<String>) {
    *NOTICE.lock().unwrap_or_else(|p| p.into_inner()) = notice;
}

pub fn notice() -> Option<String> {
    NOTICE.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

pub fn remove() {
    let state = TRAY.with(|t| t.borrow_mut().take());
    if let Some(state) = state {
        let d = data(state.hwnd, state.icon, &state.tip);
        if !unsafe { Shell_NotifyIconW(NIM_DELETE, &d) }.as_bool() {
            log("tray: Shell_NotifyIcon(NIM_DELETE) failed");
        }
    }
}

/// Refresh the tooltip from the current shortcut state.
pub fn refresh() {
    update_tip(&current_tip());
}

/// The tooltip for the shortcuts and settings as they are now.
pub fn current_tip() -> String {
    let (shortcut, error) = hotkey::display(Slot::Shelf);
    let history = crate::resident::history_running().then(|| hotkey::display(Slot::History));
    tip(
        (shortcut, error.as_deref()),
        history.as_ref().map(|(hk, e)| (*hk, e.as_deref())),
        notice().as_deref(),
    )
}

/// Tooltip text: `Tasca (Ctrl+Alt+D) | History (Alt+V)`; a shortcut that
/// failed to register says `unavailable`; the History part is left out while
/// the History is off (`history` is `None`); a failed settings apply follows.
pub fn tip(
    shelf: (Option<Hotkey>, Option<&str>),
    history: Option<(Option<Hotkey>, Option<&str>)>,
    notice: Option<&str>,
) -> String {
    let part = |name: &str, (shortcut, error): (Option<Hotkey>, Option<&str>)| match (shortcut, error) {
        (Some(hk), None) => format!("{name} ({})", hk.text()),
        (Some(hk), Some(_)) => format!("{name} ({}, unavailable)", hk.text()),
        (None, _) => format!("{name} (no shortcut)"),
    };
    let mut text = part("Tasca", shelf);
    if let Some(history) = history {
        text.push_str(" | ");
        text.push_str(&part("History", history));
    }
    match notice {
        Some(n) => format!("{text} - {n}"),
        None => text,
    }
}

/// Handle the icon's callback message. `true` = it was ours.
pub fn handle(hwnd: HWND, msg: u32, lp: LPARAM) -> bool {
    if msg != WM_TRAY {
        return false;
    }
    // Classic mode: lParam is the mouse message. Version 4 puts it in the low
    // word; both fit this mask.
    match (lp.0 & 0xffff) as u32 {
        WM_LBUTTONUP | NIN_SELECT => {
            log("tray: left click");
            shelf::toggle();
        }
        WM_RBUTTONUP | WM_CONTEXTMENU => menu(hwnd),
        _ => {}
    }
    true
}

fn menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, ID_SHOW, w!("Show shelf"));
        let history_flags = if crate::resident::history_running() { MF_STRING } else { MF_STRING | MF_GRAYED };
        let _ = AppendMenuW(menu, history_flags, ID_HISTORY, w!("Show clipboard history"));
        let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, w!("Settings\u{2026}"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Quit"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // The documented tray-menu workaround: own the foreground while the
        // menu is up, then post WM_NULL so the next menu dismisses properly.
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        )
        .0 as usize;
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match cmd {
            ID_SHOW => {
                log("tray: show shelf");
                shelf::show();
            }
            ID_HISTORY => {
                log("tray: show clipboard history");
                // Posted, not called, so the menu is gone before the History opens.
                let _ = PostMessageW(Some(hwnd), WM_SHOW_HISTORY, WPARAM(0), LPARAM(0));
            }
            ID_SETTINGS => {
                log("tray: settings");
                settings_win::open(hwnd);
            }
            ID_QUIT => {
                log("tray: quit");
                let _ = DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_shows_the_shortcut_and_failures() {
        let hk = Hotkey::DEFAULT;
        assert_eq!(tip((Some(hk), None), None, None), "Tasca (Ctrl+Alt+D)");
        assert_eq!(
            tip((Some(hk), Some("taken")), None, None),
            "Tasca (Ctrl+Alt+D, unavailable)"
        );
        assert_eq!(tip((None, None), None, None), "Tasca (no shortcut)");
        assert_eq!(tip((None, Some("taken")), None, None), "Tasca (no shortcut)");
        assert_eq!(
            tip((Some(hk), None), None, Some("settings not saved")),
            "Tasca (Ctrl+Alt+D) - settings not saved"
        );
    }

    #[test]
    fn tooltip_lists_both_shortcuts() {
        let shelf = (Some(Hotkey::DEFAULT), None);
        let alt_v = Hotkey::HISTORY_DEFAULT;
        assert_eq!(
            tip(shelf, Some((Some(alt_v), None)), None),
            "Tasca (Ctrl+Alt+D) | History (Alt+V)"
        );
        assert_eq!(
            tip(shelf, Some((Some(alt_v), Some("taken"))), None),
            "Tasca (Ctrl+Alt+D) | History (Alt+V, unavailable)"
        );
        assert_eq!(
            tip(shelf, Some((None, None)), None),
            "Tasca (Ctrl+Alt+D) | History (no shortcut)"
        );
        assert_eq!(
            tip((Some(Hotkey::DEFAULT), Some("taken")), Some((Some(alt_v), None)), Some("settings not saved")),
            "Tasca (Ctrl+Alt+D, unavailable) | History (Alt+V) - settings not saved"
        );
    }

    #[test]
    fn long_tips_are_truncated_and_terminated() {
        let d = data(HWND(std::ptr::null_mut()), None, &"x".repeat(300));
        assert_eq!(d.szTip[126], b'x' as u16);
        assert_eq!(d.szTip[127], 0);
    }
}
