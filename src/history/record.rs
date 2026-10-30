//! Recording: the clipboard listener and the clipboard read. Windows posts
//! `WM_CLIPBOARDUPDATE` to the resident window on every change; the read
//! happens here on the UI thread (open, copy the bytes out, close) and the
//! result goes to the state machine as `Event::Copied`. When another app holds
//! the clipboard the read is retried on a short timer, never by sleeping.
//!
//! The private-copy filter belongs in `read` (it sees the clipboard while it
//! is open); the state machine decides what to do with the answer.

use std::cell::Cell;

use windows::Win32::Foundation::{HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RemoveClipboardFormatListener,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::state::{Capture, Event};
use crate::log::log;

/// Timer id on the resident window.
pub const T_RETRY: usize = 0x4801;
const RETRY_MS: u32 = 25;
const MAX_RETRIES: u32 = 12;
/// UTF-16 units; the cap is 10 MB of text, a few times that is read at most.
const MAX_UNITS: usize = 32 * 1024 * 1024;

thread_local! {
    static RETRIES: Cell<u32> = const { Cell::new(0) };
}

pub fn start(hwnd: HWND) {
    match unsafe { AddClipboardFormatListener(hwnd) } {
        Ok(()) => log("history: clipboard listener on"),
        Err(e) => log(format!("history: AddClipboardFormatListener failed: {e:?}")),
    }
}

pub fn stop(hwnd: HWND) {
    unsafe {
        let _ = KillTimer(Some(hwnd), T_RETRY);
        let _ = RemoveClipboardFormatListener(hwnd);
    }
}

/// `WM_CLIPBOARDUPDATE`.
pub fn on_update(hwnd: HWND) {
    RETRIES.set(0);
    attempt(hwnd);
}

/// `WM_TIMER` with `T_RETRY`.
pub fn on_retry(hwnd: HWND) {
    attempt(hwnd);
}

fn attempt(hwnd: HWND) {
    unsafe {
        let _ = KillTimer(Some(hwnd), T_RETRY);
    }
    match read(hwnd) {
        Some(capture) => super::dispatch(Event::Copied(capture)),
        None if RETRIES.get() < MAX_RETRIES => {
            RETRIES.set(RETRIES.get() + 1);
            unsafe {
                SetTimer(Some(hwnd), T_RETRY, RETRY_MS, None);
            }
        }
        None => log("history: the clipboard stayed busy, copy not recorded"),
    }
}

/// Closes the clipboard when dropped.
struct Open;

impl Open {
    fn new(hwnd: HWND) -> Option<Open> {
        unsafe { OpenClipboard(Some(hwnd)) }.ok().map(|_| Open)
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// What the clipboard holds now; `None` when it could not be opened (busy).
fn read(hwnd: HWND) -> Option<Capture> {
    let _open = Open::new(hwnd)?;
    let seq = unsafe { GetClipboardSequenceNumber() };
    let text = if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32) }.is_ok() {
        unicode_text()
    } else {
        None
    };
    Some(Capture { seq, text })
}

/// `CF_UNICODETEXT` as a string. The clipboard owns the handle: copy, never free.
fn unicode_text() -> Option<String> {
    unsafe {
        let handle = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
        let h = HGLOBAL(handle.0);
        let bytes = GlobalSize(h);
        let p = GlobalLock(h) as *const u16;
        if p.is_null() {
            return None;
        }
        // Far past the size cap: skip it without decoding (the machine would anyway).
        if bytes / 2 > MAX_UNITS {
            let _ = GlobalUnlock(h);
            log("history: text too big, not read");
            return None;
        }
        let units = std::slice::from_raw_parts(p, bytes / 2);
        let len = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        let text = String::from_utf16_lossy(&units[..len]);
        let _ = GlobalUnlock(h);
        Some(text)
    }
}
