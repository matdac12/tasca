//! The History against the real binary, with `LOCALAPPDATA` pointed at a temp
//! folder (the real settings are never touched): a copy is recorded, a copy
//! that carries `ExcludeClipboardContentFromMonitorProcessing` is not, the
//! show-History message either opens the window in front or the resident
//! closes it because it could not take the foreground, and with `history=off`
//! in `settings.ini` nothing is recorded and the message opens nothing.
//!
//! Like `shelf.rs`, this needs the `Tasca.Shelf` class free (end
//! tasca-spike.exe and any other tasca.exe first), and one test function owns
//! the single resident per phase. It uses the real clipboard.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetForegroundWindow, IsWindowVisible, PostMessageW, WM_APP,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const EXE: &str = env!("CARGO_BIN_EXE_tasca");
/// `WM_SHOW_HISTORY` in `proto.rs`.
const WM_SHOW_HISTORY: u32 = WM_APP + 21;

fn find_resident() -> Option<HWND> {
    unsafe { FindWindowW(w!("Tasca.Shelf"), PCWSTR::null()) }.ok()
}

fn find_history() -> Option<HWND> {
    unsafe { FindWindowW(w!("Tasca.History"), PCWSTR::null()) }.ok()
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

struct Resident {
    child: Child,
    log: PathBuf,
}

impl Resident {
    fn start(local: &Path) -> Resident {
        assert!(
            find_resident().is_none(),
            "a Tasca.Shelf window already exists: end tasca-spike.exe / other tasca.exe and retry"
        );
        let child = Command::new(EXE)
            .arg("--hidden")
            .env("LOCALAPPDATA", local)
            .env("TASCA_TEST_ALLOW_ELEVATED", "1")
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        wait_until("the Tasca.Shelf window", || find_resident().is_some());
        Resident { child, log: local.join("Tasca").join("tasca.log") }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn count(&self, needle: &str) -> usize {
        self.log().matches(needle).count()
    }

    fn quit(mut self, local: &Path) {
        let status = Command::new(EXE)
            .arg("quit")
            .env("LOCALAPPDATA", local)
            .env("TASCA_TEST_ALLOW_ELEVATED", "1")
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .unwrap();
        assert!(status.success());
        let t = Instant::now();
        while self.child.try_wait().unwrap().is_none() {
            assert!(t.elapsed() < Duration::from_secs(10), "resident did not exit");
            std::thread::sleep(Duration::from_millis(25));
        }
        wait_until("the window to go away", || find_resident().is_none());
    }
}

impl Drop for Resident {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Replace the clipboard with `text`, plus each registered format in `extra`
/// (name, DWORD value) the way a password manager marks a private copy.
fn copy(text: &str, extra: &[(&str, u32)]) {
    unsafe {
        let mut opened = false;
        for _ in 0..50 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(opened, "could not open the clipboard");
        EmptyClipboard().unwrap();
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        put(CF_UNICODETEXT.0 as u32, bytes_of(&wide));
        for (name, value) in extra {
            let wide_name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            let format = RegisterClipboardFormatW(PCWSTR(wide_name.as_ptr()));
            put(format, value.to_le_bytes().to_vec());
        }
        CloseClipboard().unwrap();
    }
}

fn bytes_of(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|u| u.to_le_bytes()).collect()
}

unsafe fn put(format: u32, bytes: Vec<u8>) {
    unsafe {
        let h: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).unwrap();
        let p = GlobalLock(h) as *mut u8;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(h);
        SetClipboardData(format, Some(HANDLE(h.0))).unwrap();
    }
}

fn show_history(resident: HWND) {
    unsafe { PostMessageW(Some(resident), WM_SHOW_HISTORY, WPARAM(0), LPARAM(0)) }.unwrap();
}

/// Windows chooses whether the resident may take the foreground. Either result
/// is the product working: the window is in front, or it was closed because it
/// could not be.
enum ShowOutcome {
    Foreground,
    Refused,
}

fn await_show(resident: &Resident) -> ShowOutcome {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        if resident.log().contains("history: could not take the foreground, closing") {
            println!("show History: could not take the foreground, closed");
            return ShowOutcome::Refused;
        }
        if let Some(history) = find_history() {
            let visible = unsafe { IsWindowVisible(history) }.as_bool();
            let foreground = unsafe { GetForegroundWindow() } == history;
            if visible && foreground {
                println!("show History: visible and the foreground");
                return ShowOutcome::Foreground;
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for the History to be shown or refused:\n{}", resident.log());
}

#[test]
fn history_records_skips_private_copies_and_can_be_switched_off() {
    let local = std::env::temp_dir().join(format!("tasca-history-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local);
    std::fs::create_dir_all(local.join("Tasca")).unwrap();

    // Phase 1: the defaults (History on, private copies skipped).
    let resident = Resident::start(&local);
    wait_until("the History listener", || resident.log().contains("clipboard listener on"));

    copy("tasca history test one", &[]);
    wait_until("the first copy to be recorded", || resident.count("history: recorded clip") == 1);

    copy("tasca history test secret", &[("ExcludeClipboardContentFromMonitorProcessing", 1)]);
    wait_until("the private copy to be skipped", || resident.count("private copy skipped") == 1);
    copy("tasca history test two", &[]);
    wait_until("the next copy to be recorded", || resident.count("history: recorded clip") == 2);
    assert_eq!(resident.count("private copy skipped"), 1);

    // The other private marks.
    copy("secret 2", &[("CanIncludeInClipboardHistory", 0)]);
    wait_until("CanIncludeInClipboardHistory=0 skipped", || resident.count("private copy skipped") == 2);
    copy("not secret", &[("CanIncludeInClipboardHistory", 1)]);
    wait_until("CanIncludeInClipboardHistory=1 recorded", || resident.count("history: recorded clip") == 3);

    assert!(find_history().is_some_and(|h| !unsafe { IsWindowVisible(h) }.as_bool()), "closed History is hidden");
    show_history(find_resident().unwrap());
    // Either outcome is the product working. `await_show` prints which one.
    let _outcome = await_show(&resident);
    resident.quit(&local);

    // Phase 2: History off in settings.ini.
    std::fs::write(local.join("Tasca").join("settings.ini"), "history=off\n").unwrap();
    let resident = Resident::start(&local);
    wait_until("the resident to finish starting", || resident.log().contains("resident up"));
    assert!(resident.log().contains("clipboard history off"));
    assert!(!resident.log().contains("clipboard listener on"));
    copy("tasca history test while off", &[]);
    show_history(find_resident().unwrap());
    wait_until("the show message to be ignored", || resident.log().contains("show history ignored"));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(resident.count("history: recorded clip"), 0, "recorded while off:\n{}", resident.log());
    assert!(find_history().is_none(), "no History window while off");
    resident.quit(&local);

    let _ = std::fs::remove_dir_all(&local);
}
