//! Double-click open (spec §3). `ShellExecuteW` can block for a long time in
//! some handlers, so it runs on its own short-lived STA thread; the UI thread
//! only grants the opened app the right to come to the foreground.

use std::path::PathBuf;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY, SW_SHOWNORMAL};

use crate::log::log;

pub fn open(path: PathBuf) {
    // We just received the double-click, so we may pass foreground rights on.
    let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    let r = std::thread::Builder::new().name("tasca-open".into()).spawn(move || unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let file = HSTRING::from(path.as_path());
        let h = ShellExecuteW(None, w!("open"), &file, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        // ShellExecuteW returns a value > 32 on success.
        if h.0 as isize <= 32 {
            log(format!("open failed ({}): {}", h.0 as isize, path.display()));
        }
        if com.is_ok() {
            CoUninitialize();
        }
    });
    if let Err(e) = r {
        log(format!("open: could not start a thread: {e}"));
    }
}
