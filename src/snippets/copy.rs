//! Put a Snippet's text on the clipboard as `CF_UNICODETEXT`. A busy clipboard
//! is retried on a timer, a bounded number of times. Nothing here sleeps, and
//! nothing runs inside a `RefCell` borrow.

use std::cell::RefCell;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::log::log;

/// Timer id on the Snippets window. The fade-in uses 1.
pub const TIMER: usize = 2;
const RETRY_MS: u32 = 25;
const MAX_TRIES: u32 = 12;

struct Job {
    text: String,
    tries: u32,
}

thread_local! {
    static JOB: RefCell<Option<Job>> = const { RefCell::new(None) };
}

enum Written {
    Done,
    Busy,
    Failed,
}

unsafe fn global_of(bytes: &[u8]) -> Option<HGLOBAL> {
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)).ok()?;
        let p = GlobalLock(mem) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(Some(mem));
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(mem);
        Some(mem)
    }
}

fn write_once(owner: HWND, text: &str) -> Written {
    let bytes: Vec<u8> = text
        .encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            return Written::Busy;
        }
        let _ = EmptyClipboard();
        let ok = match global_of(&bytes) {
            Some(mem) => {
                if SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(mem.0))).is_ok() {
                    true
                } else {
                    let _ = GlobalFree(Some(mem));
                    false
                }
            }
            None => false,
        };
        let _ = CloseClipboard();
        if ok {
            Written::Done
        } else {
            Written::Failed
        }
    }
}

pub fn start(hwnd: HWND, text: String) {
    JOB.with(|j| *j.borrow_mut() = Some(Job { text, tries: 0 }));
    attempt(hwnd);
}

pub fn on_timer(hwnd: HWND) {
    attempt(hwnd);
}

fn attempt(hwnd: HWND) {
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER);
    }
    let Some(mut job) = JOB.with(|j| j.borrow_mut().take()) else {
        return;
    };
    match write_once(hwnd, &job.text) {
        Written::Done => log("snippets: copied"),
        Written::Busy if job.tries + 1 < MAX_TRIES => {
            job.tries += 1;
            JOB.with(|j| *j.borrow_mut() = Some(job));
            unsafe {
                let _ = SetTimer(Some(hwnd), TIMER, RETRY_MS, None);
            }
        }
        Written::Busy | Written::Failed => log("snippets: copy failed"),
    }
}
