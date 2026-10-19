//! The frozen interface between the helper (`tasca.exe toggle|quit`) and the
//! resident. Do not change these without changing the spec.

use windows::core::{w, PCWSTR};
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

pub const CLASS: PCWSTR = w!("Tasca.Shelf");
pub const MUTEX: PCWSTR = w!("Local\\Tasca.Resident");

pub const WM_TOGGLE: u32 = WM_APP + 1;
pub const WM_SHOW: u32 = WM_APP + 2;
pub const WM_QUIT_RESIDENT: u32 = WM_APP + 3;
