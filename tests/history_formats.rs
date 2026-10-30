//! Formatted text, images and copied files against the real binary, with
//! `LOCALAPPDATA` pointed at a temp folder: each copy reaches the store with
//! its formats (HTML and RTF next to the text, a PNG for a DIB, the path list
//! for a file drop), a copy that offers text and an image keeps only the text,
//! and a restarted resident loads all of them (decoding the image to hash it).
//!
//! Like `history.rs`, this needs the `Tasca.Shelf` class free, runs alone, and
//! uses the real clipboard. Pasting back needs a real window and keyboard, so
//! it stays a hand check.

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
use windows::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const EXE: &str = env!("CARGO_BIN_EXE_tasca");

fn find_resident() -> Option<HWND> {
    unsafe { FindWindowW(w!("Tasca.Shelf"), PCWSTR::null()) }.ok()
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(15) {
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

/// Replace the clipboard with these formats (id, bytes).
fn copy(items: &[(u32, Vec<u8>)]) {
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
        for (format, bytes) in items {
            let h: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).unwrap();
            let p = GlobalLock(h) as *mut u8;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            let _ = GlobalUnlock(h);
            SetClipboardData(*format, Some(HANDLE(h.0))).unwrap();
        }
        CloseClipboard().unwrap();
    }
}

fn registered(name: &str) -> u32 {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) }
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect()
}

/// A 4 x 2, 24-bit, bottom-up `CF_DIB`.
fn dib() -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(&40u32.to_le_bytes());
    d.extend_from_slice(&4i32.to_le_bytes());
    d.extend_from_slice(&2i32.to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&24u16.to_le_bytes());
    d.extend_from_slice(&[0u8; 24]);
    for row in 0..2u8 {
        for x in 0..4u8 {
            d.extend_from_slice(&[x * 60, row * 100, 200]);
        }
        // 12 bytes per row is already a multiple of 4
    }
    d
}

/// An `HTML Format` block around `fragment`.
fn cf_html(fragment: &str) -> Vec<u8> {
    let pre = "<html><body><!--StartFragment-->";
    let post = "<!--EndFragment--></body></html>";
    let head = |s: usize, e: usize, fs: usize, fe: usize| {
        format!("Version:0.9\r\nStartHTML:{s:010}\r\nEndHTML:{e:010}\r\nStartFragment:{fs:010}\r\nEndFragment:{fe:010}\r\n")
    };
    let n = head(0, 0, 0, 0).len();
    let (fs, fe) = (n + pre.len(), n + pre.len() + fragment.len());
    format!("{}{pre}{fragment}{post}", head(n, fe + post.len(), fs, fe)).into_bytes()
}

/// A wide `CF_HDROP`.
fn hdrop(paths: &[&str]) -> Vec<u8> {
    let mut d = vec![0u8; 20];
    d[0] = 20;
    d[16] = 1;
    for p in paths {
        d.extend(p.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes));
    }
    d.extend_from_slice(&[0, 0]);
    d
}

fn history_dir(local: &Path) -> PathBuf {
    local.join("Tasca").join("history")
}

/// Everything in the store: the index and every blob file, one after the other.
fn store_bytes(local: &Path) -> Vec<u8> {
    let mut all = Vec::new();
    for e in std::fs::read_dir(history_dir(local)).into_iter().flatten().flatten() {
        all.extend(std::fs::read(e.path()).unwrap_or_default());
    }
    all
}

fn store_has(local: &Path, needles: &[&[u8]]) -> bool {
    let bytes = store_bytes(local);
    needles.iter().all(|n| bytes.windows(n.len()).any(|w| w == *n))
}

#[test]
fn formatted_text_images_and_files_reach_the_store_and_come_back() {
    let local = std::env::temp_dir().join(format!("tasca-history-formats-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local);
    std::fs::create_dir_all(&local).unwrap();
    let id = std::process::id();
    let html = format!("<b>bold-{id}</b>");
    let rtf = format!("{{\\rtf1 rtf-{id}}}");
    let path = format!("C:\\tasca-test-{id}\\Report Q3.pdf");

    let r = Resident::start(&local);
    wait_until("the saved History to be read", || r.count("history: loaded 0 saved clips") == 1);

    // Excel-like: text, HTML, RTF and an image together are one text Clip; the image is dropped.
    copy(&[
        (CF_UNICODETEXT.0 as u32, utf16(&format!("cells-{id}"))),
        (registered("HTML Format"), cf_html(&html)),
        (registered("Rich Text Format"), rtf.clone().into_bytes()),
        (CF_DIB.0 as u32, dib()),
    ]);
    wait_until("the formatted text", || r.count("history: recorded clip") == 1);
    wait_until("it in the store", || store_has(&local, &[html.as_bytes(), rtf.as_bytes()]));
    assert_eq!(r.count("history: image"), 0, "the image next to text must not be converted:\n{}", r.log());

    // A browser-like image: HTML and a DIB, no text.
    copy(&[(registered("HTML Format"), cf_html("<img>")), (CF_DIB.0 as u32, dib())]);
    wait_until("the image", || r.count("history: recorded clip") == 2);
    assert_eq!(r.count("history: image 4x2"), 1, "{}", r.log());
    wait_until("a PNG in the store", || store_has(&local, &[b"\x89PNG\r\n\x1a\n"]));

    // The same picture again is the same Clip.
    copy(&[(CF_DIB.0 as u32, dib())]);
    wait_until("the second copy of the image", || r.count("history: clip") >= 1 && r.count("to the top") >= 1);
    assert_eq!(r.count("history: recorded clip"), 2);

    // Explorer: files win over the text they also offer.
    copy(&[(CF_HDROP.0 as u32, hdrop(&[&path, "C:\\other.txt"])), (CF_UNICODETEXT.0 as u32, utf16("Report Q3.pdf"))]);
    wait_until("the files", || r.count("history: recorded clip") == 3);
    wait_until("the paths in the store", || store_has(&local, &[path.as_bytes()]));
    r.quit(&local);

    // Restart: all three load (the image is decoded and hashed on the worker).
    let r = Resident::start(&local);
    wait_until("the saved History to be read", || r.count("history: loaded 3 saved clips") == 1);
    // And the same picture copied again meets its saved Clip instead of adding one.
    copy(&[(CF_DIB.0 as u32, dib())]);
    wait_until("the copy to be seen", || r.count("to the top") == 1 || r.count("history: recorded clip") == 1);
    assert_eq!(r.count("history: recorded clip"), 0, "a saved image must be found as a duplicate:\n{}", r.log());
    r.quit(&local);

    let _ = std::fs::remove_dir_all(&local);
}
