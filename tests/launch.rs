//! End-to-end launch tests against the real binary.
//!
//! They need the `Tasca.Shelf` class to be free: end tasca-spike.exe and any other
//! tasca.exe first. One test function, because every step shares the single
//! resident (window class + mutex are per-user, not per-test).
//!
//! The binary is the debug build, where `TASCA_TEST_ALLOW_ELEVATED=1` lets an
//! elevated process (GitHub's runners) become the resident instead of relaunching
//! through explorer.exe. Release builds ignore that variable.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, FindWindowW, IsWindowVisible};

const CLASS: PCWSTR = w!("Tasca.Shelf");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const EXE: &str = env!("CARGO_BIN_EXE_tasca");

fn find() -> Option<HWND> {
    unsafe { FindWindowW(CLASS, PCWSTR::null()) }.ok()
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(10) {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

fn tasca(args: &[&str]) -> Command {
    let mut c = Command::new(EXE);
    c.args(args).env("TASCA_TEST_ALLOW_ELEVATED", "1").creation_flags(CREATE_NO_WINDOW);
    c
}

fn wait_exit(child: &mut Child, what: &str) -> ExitStatus {
    let t = Instant::now();
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "timed out waiting for {what} to exit");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Kills the resident if the test fails half-way, so no window is left behind.
struct Resident(Child);
impl Drop for Resident {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn launch_lifecycle() {
    assert!(Path::new(EXE).exists());
    assert!(
        find().is_none(),
        "a Tasca.Shelf window already exists: end tasca-spike.exe / other tasca.exe and retry"
    );

    // Resident mutex held but no window yet ("a resident is starting"): the press is dropped.
    {
        let mutex = unsafe { CreateMutexW(None, true, w!("Local\\Tasca.Resident")) }.unwrap();
        let status = tasca(&["--hidden"]).status().unwrap();
        assert!(status.success());
        assert!(find().is_none(), "helper must not create a window while the mutex is held");
        unsafe {
            let _ = CloseHandle(mutex);
        }
    }

    // `quit` with no resident: exit 0, nothing started.
    assert!(tasca(&["quit"]).status().unwrap().success());
    assert!(find().is_none());

    // `--hidden` becomes the resident, with a hidden window.
    let mut resident = Resident(tasca(&["--hidden"]).spawn().unwrap());
    wait_until("the Tasca.Shelf window", || find().is_some());
    let hwnd = find().unwrap();
    assert!(!unsafe { IsWindowVisible(hwnd) }.as_bool(), "--hidden must stay hidden");

    // A second `--hidden` exits 0 and creates no second window.
    assert!(tasca(&["--hidden"]).status().unwrap().success());
    assert_eq!(find(), Some(hwnd));
    let second = unsafe { FindWindowExW(None, Some(hwnd), CLASS, PCWSTR::null()) };
    assert!(second.is_err(), "a second Tasca.Shelf window exists");
    assert!(resident.0.try_wait().unwrap().is_none(), "resident died");

    // toggle and show are posted and are harmless no-op hooks.
    assert!(tasca(&["toggle"]).status().unwrap().success());
    assert!(tasca(&[]).status().unwrap().success());
    std::thread::sleep(Duration::from_millis(100));
    assert!(resident.0.try_wait().unwrap().is_none());

    // quit makes the resident exit.
    assert!(tasca(&["quit"]).status().unwrap().success());
    let status = wait_exit(&mut resident.0, "the resident");
    assert!(status.success());
    wait_until("the window to go away", || find().is_none());
}
