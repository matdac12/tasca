//! The helper path (`tasca.exe [toggle | quit | --hidden]`): decide what this
//! process is for, then do it. The decision is a pure function; everything that
//! touches Windows lives below it. No COM, no OLE, no DPI work here.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, LPARAM, WPARAM};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenMutexW, OpenProcessToken, SYNCHRONIZATION_SYNCHRONIZE,
};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetShellWindow, PostMessageW};

use crate::log::{log, set_role};
use crate::proto::{CLASS, MUTEX, WM_QUIT_RESIDENT, WM_SHOW, WM_TOGGLE};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    Toggle,
    Quit,
    /// `--hidden`: become the resident but stay hidden (Startup shortcut).
    Hidden,
    /// No (known) argument: show the Shelf.
    Show,
}

pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Command {
    for a in args {
        match a.as_str() {
            "toggle" => return Command::Toggle,
            "quit" => return Command::Quit,
            "--hidden" => return Command::Hidden,
            _ => {}
        }
    }
    Command::Show
}

#[derive(Clone, Copy, Debug)]
pub struct Inputs {
    pub window_found: bool,
    pub mutex_held: bool,
    pub elevated: bool,
    pub shell_window: bool,
    pub command: Command,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Post this message to the resident's window and exit 0.
    Post(u32),
    /// Nothing to do; exit 0.
    ExitOk,
    /// Elevated and cold: start `explorer.exe <this exe>` (unelevated), exit 0.
    RelaunchThroughShell,
    /// Elevated and cold with no shell window to relaunch through: exit 1.
    ExitNoShell,
    BecomeResident { show: bool },
}

pub fn decide(i: Inputs) -> Action {
    if i.window_found {
        return match i.command {
            Command::Toggle => Action::Post(WM_TOGGLE),
            Command::Quit => Action::Post(WM_QUIT_RESIDENT),
            Command::Show => Action::Post(WM_SHOW),
            // A resident is already up; `--hidden` must not pop it open.
            Command::Hidden => Action::ExitOk,
        };
    }
    if i.command == Command::Quit || i.mutex_held {
        return Action::ExitOk;
    }
    if i.elevated {
        return if i.shell_window { Action::RelaunchThroughShell } else { Action::ExitNoShell };
    }
    Action::BecomeResident { show: i.command != Command::Hidden }
}

/// Elevation of this process. Debug builds only: `TASCA_TEST_ALLOW_ELEVATED=1`
/// makes an elevated process behave as unelevated, so the integration tests can
/// run on CI runners (which are elevated). Release builds never read it.
pub fn is_elevated() -> bool {
    #[cfg(debug_assertions)]
    if std::env::var_os("TASCA_TEST_ALLOW_ELEVATED").is_some_and(|v| v == "1") {
        return false;
    }
    token_is_elevated()
}

fn token_is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elev = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elev.TokenIsElevated != 0
    }
}

fn relaunch_through_shell() -> i32 {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let Ok(exe) = std::env::current_exe() else {
        log("elevated: current_exe() failed, not relaunching");
        return 1;
    };
    match std::process::Command::new(format!("{windir}\\explorer.exe")).arg(&exe).spawn() {
        Ok(_) => {
            log(format!("elevated: relaunched unelevated through explorer.exe {}", exe.display()));
            0
        }
        Err(e) => {
            log(format!("elevated: explorer.exe relaunch failed: {e}"));
            1
        }
    }
}

/// Run the launch logic; returns the process exit code. If this process
/// becomes the resident, it returns only after the resident has quit.
pub fn run(command: Command) -> i32 {
    let elevated = is_elevated();
    let hwnd = unsafe { FindWindowW(CLASS, PCWSTR::null()) }.ok();
    let mutex_held = hwnd.is_none()
        && unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, MUTEX) }
            .map(|h| unsafe {
                let _ = CloseHandle(h);
            })
            .is_ok();
    let shell_window = !unsafe { GetShellWindow() }.is_invalid();
    let action = decide(Inputs { window_found: hwnd.is_some(), mutex_held, elevated, shell_window, command });

    if !matches!(action, Action::BecomeResident { .. }) {
        set_role("helper");
        log(format!("{command:?}: {action:?} (elevated={elevated}, window={}, mutex={mutex_held})", hwnd.is_some()));
    }
    match action {
        Action::Post(msg) => {
            let hwnd = hwnd.expect("Post implies a window");
            match unsafe { PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0)) } {
                Ok(()) => 0,
                Err(e) => {
                    log(format!("PostMessageW failed: {e}"));
                    1
                }
            }
        }
        Action::ExitOk => 0,
        Action::ExitNoShell => 1,
        Action::RelaunchThroughShell => relaunch_through_shell(),
        Action::BecomeResident { show } => {
            set_role("resident");
            crate::resident::run(show, elevated)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Command::*;

    fn inp(window_found: bool, mutex_held: bool, elevated: bool, shell_window: bool, command: Command) -> Inputs {
        Inputs { window_found, mutex_held, elevated, shell_window, command }
    }

    #[test]
    fn args() {
        let p = |a: &[&str]| parse_args(a.iter().map(|s| s.to_string()));
        assert_eq!(p(&[]), Show);
        assert_eq!(p(&["toggle"]), Toggle);
        assert_eq!(p(&["quit"]), Quit);
        assert_eq!(p(&["--hidden"]), Hidden);
        assert_eq!(p(&["--bogus"]), Show);
        assert_eq!(p(&["--bogus", "toggle"]), Toggle);
    }

    #[test]
    fn warm_posts_the_matching_message_whatever_else_holds() {
        for mutex in [false, true] {
            for elevated in [false, true] {
                for shell in [false, true] {
                    assert_eq!(decide(inp(true, mutex, elevated, shell, Toggle)), Action::Post(WM_TOGGLE));
                    assert_eq!(decide(inp(true, mutex, elevated, shell, Quit)), Action::Post(WM_QUIT_RESIDENT));
                    assert_eq!(decide(inp(true, mutex, elevated, shell, Show)), Action::Post(WM_SHOW));
                    assert_eq!(decide(inp(true, mutex, elevated, shell, Hidden)), Action::ExitOk);
                }
            }
        }
    }

    #[test]
    fn frozen_message_numbers() {
        assert_eq!(WM_TOGGLE, 0x8001);
        assert_eq!(WM_SHOW, 0x8002);
        assert_eq!(WM_QUIT_RESIDENT, 0x8003);
    }

    #[test]
    fn cold_quit_does_nothing_even_elevated() {
        for elevated in [false, true] {
            for shell in [false, true] {
                assert_eq!(decide(inp(false, false, elevated, shell, Quit)), Action::ExitOk);
            }
        }
    }

    #[test]
    fn resident_is_starting_drops_the_press() {
        for elevated in [false, true] {
            for shell in [false, true] {
                for c in [Toggle, Hidden, Show] {
                    assert_eq!(decide(inp(false, true, elevated, shell, c)), Action::ExitOk);
                }
            }
        }
    }

    #[test]
    fn cold_elevated_relaunches_through_shell_or_exits_1() {
        for c in [Toggle, Hidden, Show] {
            assert_eq!(decide(inp(false, false, true, true, c)), Action::RelaunchThroughShell);
            assert_eq!(decide(inp(false, false, true, false, c)), Action::ExitNoShell);
        }
    }

    #[test]
    fn cold_unelevated_becomes_resident() {
        for shell in [false, true] {
            assert_eq!(decide(inp(false, false, false, shell, Toggle)), Action::BecomeResident { show: true });
            assert_eq!(decide(inp(false, false, false, shell, Show)), Action::BecomeResident { show: true });
            assert_eq!(decide(inp(false, false, false, shell, Hidden)), Action::BecomeResident { show: false });
        }
    }

    #[test]
    fn exhaustive_invariants() {
        let b = [false, true];
        for w in b {
            for m in b {
                for e in b {
                    for s in b {
                        for c in [Toggle, Quit, Hidden, Show] {
                            let a = decide(inp(w, m, e, s, c));
                            if let Action::BecomeResident { .. } = a {
                                assert!(!w && !m && !e && c != Quit, "{w} {m} {e} {s} {c:?}");
                            }
                            if let Action::Post(_) = a {
                                assert!(w);
                            }
                            if e && !w && !m && c != Quit {
                                assert!(matches!(a, Action::RelaunchThroughShell | Action::ExitNoShell));
                            }
                        }
                    }
                }
            }
        }
    }
}
