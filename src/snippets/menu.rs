//! The right-click menu of a row: Copy, Edit, Delete. Showing it runs a nested
//! message loop, so the caller holds no `RefCell` borrow.

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, PostMessageW, SetForegroundWindow, TrackPopupMenu,
    MF_STRING, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_NULL,
};

use super::state::Action;

const ID_COPY: usize = 1;
const ID_EDIT: usize = 2;
const ID_DELETE: usize = 3;

fn action_of(cmd: i32) -> Option<Action> {
    match cmd as usize {
        ID_COPY => Some(Action::Copy),
        ID_EDIT => Some(Action::Edit),
        ID_DELETE => Some(Action::Delete),
        _ => None,
    }
}

pub fn pick(hwnd: HWND, x: i32, y: i32) -> Option<Action> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let _ = AppendMenuW(menu, MF_STRING, ID_COPY, w!("Copy"));
        let _ = AppendMenuW(menu, MF_STRING, ID_EDIT, w!("Edit"));
        let _ = AppendMenuW(menu, MF_STRING, ID_DELETE, w!("Delete"));
        let mut pt = POINT { x, y };
        let _ = ClientToScreen(hwnd, &mut pt);
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
        .0;
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
        assert_eq!(action_of(ID_COPY as i32), Some(Action::Copy));
        assert_eq!(action_of(ID_EDIT as i32), Some(Action::Edit));
        assert_eq!(action_of(ID_DELETE as i32), Some(Action::Delete));
        assert_eq!(action_of(0), None);
    }
}
