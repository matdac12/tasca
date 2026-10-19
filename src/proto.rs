//! The frozen interface between the helper (`tasca.exe toggle|quit`) and the
//! resident. Do not change these without changing the spec.

use windows::core::{w, PCWSTR};
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

pub const CLASS: PCWSTR = w!("Tasca.Shelf");
pub const MUTEX: PCWSTR = w!("Local\\Tasca.Resident");

pub const WM_TOGGLE: u32 = WM_APP + 1;
pub const WM_SHOW: u32 = WM_APP + 2;
pub const WM_QUIT_RESIDENT: u32 = WM_APP + 3;

// Internal resident messages: not part of the frozen interface, but every
// WM_APP+n lives in this file so the numbers never collide.

/// TASCA-13: file facts from a probe thread. `lParam` = `Box<shelf::probe::Probed>`.
pub const WM_PROBED: u32 = WM_APP + 4;
/// TASCA-17: a row thumbnail landed in `thumbs`' cache. No pointer in the message.
pub const WM_THUMB_READY: u32 = WM_APP + 16;
/// TASCA-16: a Park worker finished (files written, async drop read). No pointer in the message.
pub const WM_PARK_DONE: u32 = WM_APP + 17;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_numbers_are_distinct() {
        let all = [WM_TOGGLE, WM_SHOW, WM_QUIT_RESIDENT, WM_PROBED, WM_THUMB_READY, WM_PARK_DONE];
        for (i, a) in all.iter().enumerate() {
            assert!(all[i + 1..].iter().all(|b| a != b), "{a:#x} used twice");
        }
    }
}
