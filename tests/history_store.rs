//! The History store against the real binary, with `LOCALAPPDATA` pointed at a
//! temp folder (the real History is never touched): a copy reaches
//! `history\index.bin`, a restarted resident loads it and keeps it next to a
//! new copy, and a garbled store starts an empty History without a crash.
//!
//! Like `history.rs`, this needs the `Tasca.Shelf` class free (end
//! tasca-spike.exe and any other tasca.exe first), runs alone, and uses the
//! real clipboard.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const EXE: &str = env!("CARGO_BIN_EXE_tasca");

fn find_resident() -> Option<HWND> {
    unsafe { FindWindowW(w!("Tasca.Shelf"), PCWSTR::null()) }.ok()
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

fn store_has(local: &Path, needles: &[&str]) -> bool {
    let bytes = std::fs::read(local.join("Tasca").join("history").join("index.bin")).unwrap_or_default();
    needles.iter().all(|n| bytes.windows(n.len()).any(|w| w == n.as_bytes()))
}

#[test]
fn the_history_survives_a_restart_and_a_garbled_store_starts_empty() {
    let local = std::env::temp_dir().join(format!("tasca-history-store-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local);
    std::fs::create_dir_all(&local).unwrap();
    let one = format!("store-one-{}", std::process::id());
    let two = format!("store-two-{}", std::process::id());

    // First run: a copy reaches the disk.
    let r = Resident::start(&local);
    wait_until("the saved History to be read", || r.count("history: loaded 0 saved clips") == 1);
    copy(&one, &[]);
    wait_until("the copy in the store", || store_has(&local, &[&one]));
    r.quit(&local);

    // Second run: the first copy is loaded and stays when another is added.
    let r = Resident::start(&local);
    wait_until("the saved History to be read", || r.count("history: loaded 1 saved clips") == 1);
    copy(&two, &[]);
    wait_until("both copies in the store", || store_has(&local, &[&one, &two]));
    r.quit(&local);

    // Third run: a garbled store is an empty History, not a crash.
    let index = local.join("Tasca").join("history").join("index.bin");
    std::fs::write(&index, b"garbage that is not a store").unwrap();
    let r = Resident::start(&local);
    wait_until("the garbled store to load empty", || r.count("history: loaded 0 saved clips") == 1);
    r.quit(&local);

    let _ = std::fs::remove_dir_all(&local);
}
