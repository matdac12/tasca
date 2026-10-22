//! The resident: mutex, the hidden `Tasca.Shelf` window, wndproc dispatch and
//! the message loop. The Shelf (`crate::shelf`) sees every message first.

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
use crate::proto::{
    CLASS, MUTEX, WM_PARK_DONE, WM_QUIT_RESIDENT, WM_SETTINGS_SAVED, WM_SHOW, WM_TOGGLE,
};
use crate::{hotkey, park, settings, settings_win, shelf, tray};

/// Caps+D while resident.
fn on_toggle(_hwnd: HWND) {
    log("toggle");
    shelf::toggle();
}

/// Show request (plain `tasca.exe`, or a cold start without `--hidden`).
fn on_show(_hwnd: HWND) {
    log("show");
    shelf::show();
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if let Some(r) = shelf::handle(hwnd, msg, wp, lp) {
            return r;
        }
        if msg == tray::taskbar_created() {
            tray::re_add();
            return LRESULT(0);
        }
        if tray::handle(hwnd, msg, lp) {
            return LRESULT(0);
        }
        match msg {
            WM_HOTKEY if wp.0 as i32 == hotkey::REGISTER_ID => {
                on_toggle(hwnd);
                LRESULT(0)
            }
            WM_TOGGLE => {
                on_toggle(hwnd);
                LRESULT(0)
            }
            WM_SHOW => {
                on_show(hwnd);
                LRESULT(0)
            }
            WM_PARK_DONE => {
                park::on_done(hwnd);
                LRESULT(0)
            }
            WM_SETTINGS_SAVED => {
                settings_win::on_saved();
                LRESULT(0)
            }
            WM_QUIT_RESIDENT => {
                log("quit requested");
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_DESTROY => {
                tray::remove();
                hotkey::unregister(hwnd);
                settings_win::close();
                park::revoke(hwnd);
                shelf::destroy();
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
            style: CS_DBLCLKS,
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
        shelf::create(hwnd)?;
        // Then OLE: the Shelf is a drop target from now on, shown or not.
        park::register(hwnd);
        park::sweep::start();

        // TASCA-24: settings, the global shortcut and the tray icon.
        let configured = settings::load();
        match configured.shortcut {
            Some(hk) => log(format!("settings: shortcut {}", hk.text())),
            None => log("settings: shortcut cleared"),
        }
        let _ = hotkey::start(hwnd, configured.shortcut);
        let (shortcut, shortcut_error) = hotkey::display();
        tray::add(hwnd, tray::tip(shortcut, shortcut_error.as_deref(), None));
        #[cfg(debug_assertions)]
        if std::env::var_os("TASCA_DEV_SETTINGS").is_some() {
            settings_win::open(hwnd);
        }

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
            // Tab/Esc for the settings window while it is open.
            if settings_win::pre_translate(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        park::revoke(hwnd); // no-op after WM_DESTROY; covers a failed GetMessageW
        park::uninit();
        log("resident exit");
        let _ = CloseHandle(mutex);
        Ok(0)
    }
}
