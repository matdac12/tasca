//! The right-click menu of a row: a native popup menu (Paste, Paste as plain
//! text, Pin or Unpin, Delete). Showing it runs a nested message loop, so the
//! caller holds no `RefCell` borrow; it passes in what the menu needs and
//! gets the choice back.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, PostMessageW, SetForegroundWindow, TrackPopupMenu, MF_SEPARATOR,
    MF_STRING, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_NULL,
};

use super::state::Action;

const ID_PASTE: usize = 1;
const ID_PLAIN: usize = 2;
const ID_PIN: usize = 3;
const ID_DELETE: usize = 4;

/// The action a menu command id stands for.
fn action_of(cmd: i32) -> Option<Action> {
    match cmd as usize {
        ID_PASTE => Some(Action::Paste),
        ID_PLAIN => Some(Action::PastePlain),
        ID_PIN => Some(Action::TogglePin),
        ID_DELETE => Some(Action::Delete),
        _ => None,
    }
}

/// The text of the pin entry.
fn pin_label(pinned: bool) -> PCWSTR {
    if pinned { w!("Unpin") } else { w!("Pin") }
}

/// Show the menu for a Clip at client point (`x`, `y`) of `hwnd` and wait for a
/// choice. `None` when it was dismissed.
pub fn pick(hwnd: HWND, pinned: bool, x: i32, y: i32) -> Option<Action> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let _ = AppendMenuW(menu, MF_STRING, ID_PASTE, w!("Paste"));
        let _ = AppendMenuW(menu, MF_STRING, ID_PLAIN, w!("Paste as plain text"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_PIN, pin_label(pinned));
        let _ = AppendMenuW(menu, MF_STRING, ID_DELETE, w!("Delete"));
        let mut pt = POINT { x, y };
        let _ = ClientToScreen(hwnd, &mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, None, hwnd, None).0;
        // Documented workaround so the next menu dismisses properly.
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        action_of(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_command_maps_to_its_action_and_zero_to_none() {
        assert_eq!(action_of(ID_PASTE as i32), Some(Action::Paste));
        assert_eq!(action_of(ID_PLAIN as i32), Some(Action::PastePlain));
        assert_eq!(action_of(ID_PIN as i32), Some(Action::TogglePin));
        assert_eq!(action_of(ID_DELETE as i32), Some(Action::Delete));
        assert_eq!(action_of(0), None);
    }
}
