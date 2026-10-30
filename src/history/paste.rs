//! Paste-back. The state machine says `Paste { clip, target }`; this module
//! does the Win32 part, in the order the spec fixes: write the Clip (the
//! window is the clipboard owner, the write's sequence number goes back as
//! `OwnWrite`), restore the target, then, after a short timer so the target
//! has taken focus, release held Alt/Ctrl/Shift/Win and send a plain Ctrl+V.
//!
//! Nothing here sleeps and nothing here runs inside a `RefCell` borrow: the
//! caller (`dispatch`) has already dropped it.

use std::cell::Cell;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, IsIconic, IsWindow, KillTimer, SetForegroundWindow, SetTimer, ShowWindow, SW_RESTORE,
};

use super::clip::Clip;
use super::state::{Event, Target};
use crate::log::log;

/// Timer id on the resident window.
pub const T_KEYS: usize = 0x4802;
/// Between `SetForegroundWindow` and the keystrokes.
const KEYS_DELAY_MS: u32 = 40;

thread_local! {
    /// The window that should have focus when the keys go out.
    static PENDING: Cell<Option<isize>> = const { Cell::new(None) };
}

/// Put `clip` on the clipboard; the sequence number after the write.
fn write(owner: HWND, clip: &Clip) -> Option<u32> {
    let units: Vec<u16> = clip.text.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2).ok()?;
        let p = GlobalLock(mem) as *mut u16;
        if p.is_null() {
            let _ = GlobalFree(Some(mem));
            return None;
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
        let _ = GlobalUnlock(mem);
        if OpenClipboard(Some(owner)).is_err() {
            let _ = GlobalFree(Some(mem));
            return None;
        }
        let _ = EmptyClipboard();
        // On success the clipboard owns `mem`.
        let set = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(mem.0)));
        if set.is_err() {
            let _ = GlobalFree(Some(mem));
        }
        let _ = CloseClipboard();
        set.ok()?;
        Some(GetClipboardSequenceNumber())
    }
}

/// Copy-only: the Clip is on the clipboard and nothing is typed.
pub fn copy(owner: HWND, clip: &Clip) {
    match write(owner, clip) {
        Some(seq) => super::dispatch(Event::OwnWrite(seq)),
        None => log("history: could not write the clipboard"),
    }
}

/// Write the Clip, then bring `target` back and schedule the keystrokes on
/// `resident`'s timer. The History window is already hidden.
pub fn paste(resident: HWND, owner: HWND, clip: &Clip, target: Target) {
    let Some(seq) = write(owner, clip) else {
        log("history: could not write the clipboard, nothing pasted");
        return;
    };
    super::dispatch(Event::OwnWrite(seq));
    let hwnd = HWND(target.0 as *mut _);
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            log("history: the paste target is gone, the Clip stays on the clipboard");
            return;
        }
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if !SetForegroundWindow(hwnd).as_bool() {
            log("history: SetForegroundWindow refused, pasting anyway");
        }
        PENDING.set(Some(target.0));
        SetTimer(Some(resident), T_KEYS, KEYS_DELAY_MS, None);
    }
}

/// `WM_TIMER` with `T_KEYS`: release what the user still holds, send Ctrl+V.
pub fn on_keys(resident: HWND) {
    unsafe {
        let _ = KillTimer(Some(resident), T_KEYS);
    }
    let Some(target) = PENDING.take() else { return };
    // Focus did not come back (refused, or the user clicked elsewhere): typing
    // now would paste into the wrong window. The Clip is on the clipboard.
    if unsafe { GetForegroundWindow() }.0 as isize != target {
        log("history: the target is not in the foreground, Ctrl+V not sent");
        return;
    }
    let held: Vec<VIRTUAL_KEY> = MODIFIERS
        .iter()
        .copied()
        .filter(|vk| unsafe { GetAsyncKeyState(vk.0 as i32) } < 0)
        .collect();
    let inputs: Vec<INPUT> = paste_keys(&held).into_iter().map(|(vk, up)| key(vk, up)).collect();
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    log(format!("history: sent {sent}/{} key events (released {} held modifiers)", inputs.len(), held.len()));
}

/// Every key whose held state would turn Ctrl+V into something else.
const MODIFIERS: [VIRTUAL_KEY; 8] =
    [VK_LMENU, VK_RMENU, VK_LCONTROL, VK_RCONTROL, VK_LSHIFT, VK_RSHIFT, VK_LWIN, VK_RWIN];

/// The key events for a plain paste: key-up for each held modifier, then
/// Ctrl+V as `(key, is_up)` in order. Pure.
fn paste_keys(held: &[VIRTUAL_KEY]) -> Vec<(VIRTUAL_KEY, bool)> {
    let mut keys: Vec<(VIRTUAL_KEY, bool)> = held.iter().map(|&vk| (vk, true)).collect();
    keys.extend([(VK_CONTROL, false), (VK_V, false), (VK_V, true), (VK_CONTROL, true)]);
    keys
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_paste_is_ctrl_v() {
        assert_eq!(
            paste_keys(&[]),
            [(VK_CONTROL, false), (VK_V, false), (VK_V, true), (VK_CONTROL, true)]
        );
    }

    #[test]
    fn held_modifiers_are_released_before_ctrl_v() {
        // Alt+V was just pressed and Alt is still down: Ctrl+Alt+V must not happen.
        let keys = paste_keys(&[VK_LMENU, VK_LSHIFT]);
        assert_eq!(&keys[..2], [(VK_LMENU, true), (VK_LSHIFT, true)]);
        assert_eq!(keys[2], (VK_CONTROL, false));
        assert_eq!(keys.len(), 6);
    }
}
