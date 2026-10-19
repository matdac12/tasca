//! Double-click open (spec §3). `ShellExecuteExW` can block for a long time in
//! some handlers, so it runs on its own short-lived STA thread; the UI thread
//! only grants the opened app the right to come to the foreground.
//! `SEE_MASK_NOASYNC` makes it finish asynchronous verbs (DelegateExecute,
//! DDE) before the thread tears COM down.

use std::path::PathBuf;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY, SW_SHOWNORMAL};

use crate::log::log;

pub fn open(path: PathBuf) {
    // We just received the double-click, so we may pass foreground rights on.
    let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    let r = std::thread::Builder::new().name("tasca-open".into()).spawn(move || unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let file = HSTRING::from(path.as_path());
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpVerb: w!("open"),
            lpFile: PCWSTR(file.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        if let Err(e) = ShellExecuteExW(&mut info) {
            log(format!("open failed ({e}): {}", path.display()));
        }
        if com.is_ok() {
            CoUninitialize();
        }
    });
    if let Err(e) = r {
        log(format!("open: could not start a thread: {e}"));
    }
}
