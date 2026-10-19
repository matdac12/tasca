//! The Shelf against the real binary: `toggle` shows it without taking focus,
//! the next `toggle` hides it.
//!
//! Like `launch.rs`, this needs the `Tasca.Shelf` class free (end
//! tasca-spike.exe and any other tasca.exe first). Cargo runs test binaries one
//! after another, so the two files never hold a resident at the same time.

use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, IsWindowVisible, GWL_EXSTYLE,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const EXE: &str = env!("CARGO_BIN_EXE_tasca");

fn find() -> Option<HWND> {
    unsafe { FindWindowW(w!("Tasca.Shelf"), PCWSTR::null()) }.ok()
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(10) {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

fn tasca(args: &[&str]) -> Command {
    let mut c = Command::new(EXE);
    c.args(args).env("TASCA_TEST_ALLOW_ELEVATED", "1").creation_flags(CREATE_NO_WINDOW);
    c
}

struct Resident(Child);
impl Drop for Resident {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn visible(hwnd: HWND) -> bool {
    unsafe { IsWindowVisible(hwnd) }.as_bool()
}

#[test]
fn toggle_shows_without_taking_focus_then_hides() {
    assert!(find().is_none(), "a Tasca.Shelf window already exists: end tasca-spike.exe / other tasca.exe and retry");
    let mut resident = Resident(tasca(&["--hidden"]).spawn().unwrap());
    wait_until("the Tasca.Shelf window", || find().is_some());
    let hwnd = find().unwrap();
    assert!(!visible(hwnd));

    let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    for (bit, name) in [
        (WS_EX_LAYERED, "LAYERED"),
        (WS_EX_TOPMOST, "TOPMOST"),
        (WS_EX_TOOLWINDOW, "TOOLWINDOW"),
        (WS_EX_NOACTIVATE, "NOACTIVATE"),
    ] {
        assert!(ex & bit.0 != 0, "missing WS_EX_{name}");
    }
    assert!(ex & WS_EX_TRANSPARENT.0 == 0, "the Shelf must never be WS_EX_TRANSPARENT");

    let fg = unsafe { GetForegroundWindow() };
    assert!(tasca(&["toggle"]).status().unwrap().success());
    wait_until("the Shelf to show", || visible(hwnd));
    std::thread::sleep(Duration::from_millis(300)); // past the 180 ms slide-in
    assert_eq!(unsafe { GetForegroundWindow() }, fg, "showing the Shelf took the foreground");
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r) }.unwrap();
    assert!(r.right - r.left >= 224 && r.bottom - r.top >= 98, "unexpected Shelf size {r:?}");

    assert!(tasca(&["toggle"]).status().unwrap().success());
    wait_until("the Shelf to hide", || !visible(hwnd));
    assert_eq!(unsafe { GetForegroundWindow() }, fg);

    assert!(tasca(&["quit"]).status().unwrap().success());
    let t = Instant::now();
    while resident.0.try_wait().unwrap().is_none() {
        assert!(t.elapsed() < Duration::from_secs(10), "resident did not exit");
        std::thread::sleep(Duration::from_millis(25));
    }
    wait_until("the window to go away", || find().is_none());
}
