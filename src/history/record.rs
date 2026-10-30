//! Recording: the clipboard listener and the clipboard read. Windows posts
//! `WM_CLIPBOARDUPDATE` to the resident window on every change. The read
//! (OpenClipboard, GetClipboardData, decoding) runs on a worker thread, because
//! a source that renders lazily (Excel, Office) can make `GetClipboardData`
//! wait for seconds. The result comes back as `WM_HISTORY_READ` plus a queue,
//! and goes to the state machine as `Event::Copied`.
//!
//! At most one read is in flight. An update that arrives meanwhile only marks
//! the read stale; when the running read lands, one more starts, so the latest
//! copy wins. A busy clipboard is retried by the worker (it may sleep).
//!
//! The private-copy filter belongs in `read` (it sees the clipboard while it
//! is open); the state machine decides what to do with the answer.

use std::cell::Cell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use windows::Win32::Foundation::{HGLOBAL, HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, GetClipboardData, GetClipboardOwner, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RemoveClipboardFormatListener,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::state::{Capture, Event};
use crate::log::log;
use crate::proto::WM_HISTORY_READ;

const RETRY_MS: u64 = 25;
const MAX_TRIES: u32 = 12;
/// UTF-16 units; the cap is 10 MB of text, a few times that is read at most.
const MAX_UNITS: usize = 32 * 1024 * 1024;

/// The History window (clipboard owner of our own writes), for the worker.
static OWN_WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Finished reads, newest last; `None` = the clipboard stayed busy.
static DONE: Mutex<Vec<Option<Capture>>> = Mutex::new(Vec::new());

/// What the UI thread knows about the worker.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Flight {
    running: bool,
    stale: bool,
}

impl Flight {
    /// A clipboard update arrived; `true` = start a read now.
    fn update(&mut self) -> bool {
        if self.running {
            self.stale = true;
            false
        } else {
            self.running = true;
            true
        }
    }

    /// The running read landed; `true` = start another one (an update came in).
    fn landed(&mut self) -> bool {
        self.running = false;
        if std::mem::take(&mut self.stale) {
            self.running = true;
            return true;
        }
        false
    }
}

thread_local! {
    static FLIGHT: Cell<Flight> = const { Cell::new(Flight { running: false, stale: false }) };
}

pub fn start(hwnd: HWND, own_window: HWND) {
    OWN_WINDOW.store(own_window.0 as isize, Ordering::Relaxed);
    match unsafe { AddClipboardFormatListener(hwnd) } {
        Ok(()) => log("history: clipboard listener on"),
        Err(e) => log(format!("history: AddClipboardFormatListener failed: {e:?}")),
    }
}

pub fn stop(hwnd: HWND) {
    unsafe {
        let _ = RemoveClipboardFormatListener(hwnd);
    }
}

/// `WM_CLIPBOARDUPDATE`.
pub fn on_update(hwnd: HWND) {
    let mut flight = FLIGHT.get();
    let go = flight.update();
    FLIGHT.set(flight);
    if go {
        spawn(hwnd);
    }
}

/// `WM_HISTORY_READ`: take what the worker read.
pub fn on_read(hwnd: HWND) {
    let done: Vec<Option<Capture>> = std::mem::take(&mut *DONE.lock().unwrap_or_else(|e| e.into_inner()));
    let mut flight = FLIGHT.get();
    let again = flight.landed();
    FLIGHT.set(flight);
    if again {
        spawn(hwnd);
    }
    for capture in done {
        match capture {
            Some(c) => super::dispatch(Event::Copied(c)),
            None => log("history: the clipboard stayed busy, copy not recorded"),
        }
    }
}

fn spawn(hwnd: HWND) {
    let raw = hwnd.0 as isize;
    let spawned = std::thread::Builder::new().name("history-read".into()).spawn(move || {
        let capture = read_with_retry();
        DONE.lock().unwrap_or_else(|e| e.into_inner()).push(capture);
        unsafe {
            let _ = PostMessageW(Some(HWND(raw as *mut _)), WM_HISTORY_READ, WPARAM(0), LPARAM(0));
        }
    });
    if spawned.is_err() {
        // No thread: forget the flight so the next update can try again.
        FLIGHT.set(Flight::default());
        log("history: could not start the clipboard reader");
    }
}

fn read_with_retry() -> Option<Capture> {
    for attempt in 0..MAX_TRIES {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(RETRY_MS));
        }
        if let Some(c) = read() {
            return Some(c);
        }
    }
    None
}

/// Closes the clipboard when dropped.
struct Open;

impl Open {
    /// No owner window: fine for reading, and this runs off the UI thread.
    fn new() -> Option<Open> {
        unsafe { OpenClipboard(None) }.ok().map(|_| Open)
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
fn read() -> Option<Capture> {
    let _open = Open::new()?;
    let seq = unsafe { GetClipboardSequenceNumber() };
    let own = unsafe { GetClipboardOwner() }.is_ok_and(|o| o.0 as isize == OWN_WINDOW.load(Ordering::Relaxed));
    let text = if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32) }.is_ok() {
        unicode_text()
    } else {
        None
    };
    Some(Capture { seq, text, own })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_read_runs_at_a_time_and_the_latest_update_wins() {
        let mut f = Flight::default();
        assert!(f.update(), "first update starts a read");
        assert!(!f.update(), "a second one waits");
        assert!(!f.update(), "and a third changes nothing more");
        assert!(f.landed(), "one more read after the first lands");
        assert!(!f.landed(), "nothing stale: idle");
        assert!(f.update(), "idle again: the next update starts a read");
    }
}
