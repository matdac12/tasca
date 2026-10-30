//! The resident: mutex, the hidden `Tasca.Shelf` window, wndproc dispatch and
//! the message loop. The Shelf (`crate::shelf`) sees every message first.

use std::cell::Cell;

use windows::core::{w, Result};
use windows::Win32::System::DataExchange::{AddClipboardFormatListener, RemoveClipboardFormatListener};
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
    CLASS, MUTEX, WM_PARK_DONE, WM_QUIT_RESIDENT, WM_SETTINGS_SAVED, WM_SHOW, WM_SHOW_HISTORY, WM_TOGGLE,
};
use crate::{history, hotkey, park, settings, settings_win, shelf, tray};

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

/// Where the History stands. The window is made once (its class is never
/// unregistered, so a second `create` always fails), and switching the History
/// off only removes the clipboard listener and the shortcut, so it can come
/// back without a restart.
#[derive(Clone, Copy, Default)]
struct HistoryState {
    created: bool,
    /// `create` failed once: it is never retried in this run.
    create_failed: bool,
    listening: bool,
}

thread_local! {
    static HISTORY: Cell<HistoryState> = const { Cell::new(HistoryState { created: false, create_failed: false, listening: false }) };
}

/// The History is recording and open-able right now. The tray, the tooltip,
/// the show message and the reader all ask this one question.
pub fn history_running() -> bool {
    HISTORY.get().listening
}

fn set_listener(hwnd: HWND, on: bool) -> bool {
    let result = unsafe {
        if on {
            AddClipboardFormatListener(hwnd)
        } else {
            RemoveClipboardFormatListener(hwnd)
        }
    };
    match result {
        Ok(()) => {
            log(format!("history: clipboard listener {}", if on { "on" } else { "off" }));
            true
        }
        Err(e) => {
            log(format!("history: clipboard listener {} failed: {e:?}", if on { "on" } else { "off" }));
            false
        }
    }
}

/// `history::create` adds the listener but does not say whether it worked.
/// Take it off (silently: it may not be there) and put it back to find out.
fn confirm_listener(hwnd: HWND) -> bool {
    unsafe {
        let _ = RemoveClipboardFormatListener(hwnd);
    }
    set_listener(hwnd, true)
}

/// Make the History window once. `false` when that failed (then never again).
fn create_history(hwnd: HWND) -> std::result::Result<(), String> {
    let mut state = HISTORY.get();
    match history::create(hwnd) {
        Ok(()) => {
            state.created = true;
            state.listening = confirm_listener(hwnd);
            HISTORY.set(state);
            Ok(())
        }
        Err(e) => {
            state.create_failed = true;
            HISTORY.set(state);
            log(format!("history failed to start, not retried: {e:?}"));
            Err(format!("Clipboard history could not start ({e:?}). Restart Tasca to try again."))
        }
    }
}

/// Resident startup with the History on.
fn start_history(hwnd: HWND, shortcut: Option<hotkey::Hotkey>) {
    if create_history(hwnd).is_ok() {
        let fallback = hotkey::Hotkey::HISTORY_DEFAULT;
        let _ = hotkey::start(hwnd, hotkey::Slot::History, shortcut, Some(fallback));
    }
}

/// The History as it was before a Settings apply, to put back if a later
/// step of that apply fails.
pub struct HistoryBackup {
    state: HistoryState,
    shortcut: hotkey::Snapshot,
}

pub fn history_backup() -> HistoryBackup {
    HistoryBackup { state: HISTORY.get(), shortcut: hotkey::snapshot(hotkey::Slot::History) }
}

/// Undo `set_history`: listener and shortcut as `backup` saw them. The window
/// stays (it cannot be made twice).
pub fn restore_history(hwnd: HWND, backup: HistoryBackup) {
    let now = HISTORY.get();
    let mut state = backup.state;
    state.created |= now.created;
    state.create_failed |= now.create_failed;
    if now.listening != state.listening && !set_listener(hwnd, state.listening) {
        state.listening = now.listening;
    }
    HISTORY.set(state);
    hotkey::restore(hwnd, hotkey::Slot::History, backup.shortcut);
}

/// Settings OK: switch the History on or off and give it `shortcut`. Off
/// removes the clipboard listener and frees the shortcut; the saved History
/// stays on disk. On, a refused or taken shortcut is the error, and the
/// History is left as it was.
pub fn set_history(hwnd: HWND, on: bool, shortcut: Option<hotkey::Hotkey>) -> std::result::Result<(), String> {
    let backup = history_backup();
    let mut state = HISTORY.get();
    if on {
        if state.create_failed {
            // Already reported at startup; failing every OK would block the
            // other settings for the rest of the run.
            log("history: not started this run, nothing to switch on");
            return Ok(());
        }
        if !state.created {
            create_history(hwnd)?;
        } else if !state.listening {
            if !set_listener(hwnd, true) {
                return Err("Clipboard history could not start listening".to_string());
            }
            state.listening = true;
            HISTORY.set(state);
        }
        if let Err(e) = hotkey::change(hwnd, hotkey::Slot::History, shortcut) {
            restore_history(hwnd, backup);
            return Err(e);
        }
    } else {
        if state.listening {
            set_listener(hwnd, false);
            state.listening = false;
            HISTORY.set(state);
        }
        let _ = hotkey::change(hwnd, hotkey::Slot::History, None);
    }
    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if let Some(r) = shelf::handle(hwnd, msg, wp, lp) {
            return r;
        }
        if msg == WM_SHOW_HISTORY && !history_running() {
            log("show history ignored: the History is off");
            return LRESULT(0);
        }
        if let Some(r) = history::handle(hwnd, msg, wp, lp) {
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
            WM_HOTKEY if hotkey::Slot::from_id(wp.0 as i32) == Some(hotkey::Slot::Shelf) => {
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
                history::destroy(hwnd);
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
        settings::set_current(configured);
        match configured.shortcut {
            Some(hk) => log(format!("settings: shortcut {}", hk.text())),
            None => log("settings: shortcut cleared"),
        }
        let _ = hotkey::start(
            hwnd,
            hotkey::Slot::Shelf,
            configured.shortcut,
            Some(hotkey::Hotkey::DEFAULT),
        );
        // The History (when switched on): its window, the clipboard listener
        // and its shortcut. A taken shortcut (another clipboard manager) is
        // logged by `hotkey::start` and shown in the tray tooltip.
        if configured.history {
            start_history(hwnd, configured.history_shortcut);
        } else {
            log("settings: clipboard history off");
        }
        tray::add(hwnd, tray::current_tip());
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
