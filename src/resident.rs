//! The resident: mutex, the hidden `Tasca.Shelf` window, wndproc dispatch and
//! the message loop. Later slices hook into `on_toggle` / `on_show`.

use windows::core::{w, Result};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_TRANSITIONS_FORCEDISABLED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::log::{log, now_ms, truncate};
use crate::proto::{CLASS, MUTEX, WM_QUIT_RESIDENT, WM_SHOW, WM_TOGGLE};

/// Caps+D while resident. No-op until the Shelf slice.
fn on_toggle(_hwnd: HWND) {
    log("toggle");
}

/// Show request. No-op until the Shelf slice.
fn on_show(_hwnd: HWND) {
    log("show");
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TOGGLE => {
                on_toggle(hwnd);
                LRESULT(0)
            }
            WM_SHOW => {
                on_show(hwnd);
                LRESULT(0)
            }
            WM_QUIT_RESIDENT => {
                log("quit requested");
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Become the resident. Returns the exit code once the loop ends.
pub fn run(show: bool, elevated: bool) -> i32 {
    match unsafe { run_inner(show, elevated) } {
        Ok(code) => code,
        Err(e) => {
            log(format!("resident failed: {e:?}"));
            1
        }
    }
}

unsafe fn run_inner(show: bool, elevated: bool) -> Result<i32> {
    unsafe {
        let mutex = CreateMutexW(None, true, MUTEX)?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // Lost a race with another starting resident.
            let _ = CloseHandle(mutex);
            return Ok(0);
        }
        truncate();

        let hinst = GetModuleHandleW(None)?.into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: CLASS,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        // The window comes first: it is what the helper path looks for.
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS,
            w!("Tasca"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinst),
            None,
        )?;
        let off = 1i32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &off as *const _ as _, 4);

        let pmv2 = AreDpiAwarenessContextsEqual(
            GetThreadDpiAwarenessContext(),
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
        .as_bool();
        log(format!("resident up {:.1} ms after start: elevated={elevated}, show={show}, per-monitor-v2={pmv2}", now_ms()));
        if show {
            on_show(hwnd);
        }

        let mut msg = MSG::default();
        // 0 = WM_QUIT, -1 = error: both end the loop.
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        log("resident exit");
        let _ = CloseHandle(mutex);
        Ok(0)
    }
}
