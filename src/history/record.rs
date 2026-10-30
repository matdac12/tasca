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
//! The private-copy filter runs in `read`, on the worker, while the clipboard
//! is open: a copy that a password manager marks as private is dropped there
//! (logged, never posted), so its text is not even read.

use std::cell::Cell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows::core::w;
use windows::Win32::Foundation::{HGLOBAL, HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, GetClipboardData, GetClipboardOwner, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, RemoveClipboardFormatListener,
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

/// What one read found.
enum Outcome {
    /// The clipboard stayed busy.
    Busy,
    /// A private copy, skipped while "Skip private copies" is on.
    Private,
    Copy(Capture),
}

/// Finished reads, newest last.
static DONE: Mutex<Vec<Outcome>> = Mutex::new(Vec::new());

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
    let done: Vec<Outcome> = std::mem::take(&mut *DONE.lock().unwrap_or_else(|e| e.into_inner()));
    let mut flight = FLIGHT.get();
    let again = flight.landed();
    FLIGHT.set(flight);
    if again {
        spawn(hwnd);
    }
    // A read that lands after the History was switched off is dropped.
    let on = crate::settings::current().history;
    for outcome in done {
        match outcome {
            Outcome::Copy(c) if on => super::dispatch(Event::Copied(c)),
            Outcome::Copy(_) => {}
            Outcome::Private => log("history: private copy skipped"),
            Outcome::Busy => log("history: the clipboard stayed busy, copy not recorded"),
        }
    }
}

fn spawn(hwnd: HWND) {
    let raw = hwnd.0 as isize;
    let spawned = std::thread::Builder::new().name("history-read".into()).spawn(move || {
        let outcome = read_with_retry();
        DONE.lock().unwrap_or_else(|e| e.into_inner()).push(outcome);
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

fn read_with_retry() -> Outcome {
    for attempt in 0..MAX_TRIES {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(RETRY_MS));
        }
        if let Some(outcome) = read() {
            return outcome;
        }
    }
    Outcome::Busy
}

/// The private marks a copy carries: registered formats that password
/// managers and Windows' own clipboard history use to say "do not keep this".
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Marks {
    /// `ExcludeClipboardContentFromMonitorProcessing` is present.
    exclude_from_monitoring: bool,
    /// `Clipboard Viewer Ignore` is present.
    viewer_ignore: bool,
    /// `CanIncludeInClipboardHistory`: its DWORD, when present.
    can_include_in_history: Option<u32>,
    /// `CanUploadToCloudClipboard`: its DWORD, when present.
    can_upload_to_cloud: Option<u32>,
}

impl Marks {
    /// Any one mark makes the copy private; the DWORD marks only count as 0.
    fn is_private(&self) -> bool {
        self.exclude_from_monitoring
            || self.viewer_ignore
            || self.can_include_in_history == Some(0)
            || self.can_upload_to_cloud == Some(0)
    }
}

/// Skip this copy? Only while "Skip private copies" is on; with it off a
/// private copy is an ordinary one.
fn skip_as_private(marks: &Marks, skip_private: bool) -> bool {
    skip_private && marks.is_private()
}

/// The registered format ids of the marks (ids never change once registered).
struct MarkFormats {
    exclude_from_monitoring: u32,
    viewer_ignore: u32,
    can_include_in_history: u32,
    can_upload_to_cloud: u32,
}

fn mark_formats() -> &'static MarkFormats {
    static FORMATS: OnceLock<MarkFormats> = OnceLock::new();
    FORMATS.get_or_init(|| unsafe {
        MarkFormats {
            exclude_from_monitoring: RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")),
            viewer_ignore: RegisterClipboardFormatW(w!("Clipboard Viewer Ignore")),
            can_include_in_history: RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")),
            can_upload_to_cloud: RegisterClipboardFormatW(w!("CanUploadToCloudClipboard")),
        }
    })
}

/// The marks on the open clipboard.
fn read_marks() -> Marks {
    let f = mark_formats();
    let present = |format: u32| format != 0 && unsafe { IsClipboardFormatAvailable(format) }.is_ok();
    Marks {
        exclude_from_monitoring: present(f.exclude_from_monitoring),
        viewer_ignore: present(f.viewer_ignore),
        can_include_in_history: present(f.can_include_in_history).then(|| dword(f.can_include_in_history)).flatten(),
        can_upload_to_cloud: present(f.can_upload_to_cloud).then(|| dword(f.can_upload_to_cloud)).flatten(),
    }
}

/// A DWORD-valued format; a mark whose value cannot be read does not count.
fn dword(format: u32) -> Option<u32> {
    unsafe {
        let handle = GetClipboardData(format).ok()?;
        let h = HGLOBAL(handle.0);
        if GlobalSize(h) < 4 {
            return None;
        }
        let p = GlobalLock(h) as *const u32;
        if p.is_null() {
            return None;
        }
        let value = p.read_unaligned();
        let _ = GlobalUnlock(h);
        Some(value)
    }
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
fn read() -> Option<Outcome> {
    let _open = Open::new()?;
    if skip_as_private(&read_marks(), crate::settings::current().skip_private) {
        return Some(Outcome::Private);
    }
    let seq = unsafe { GetClipboardSequenceNumber() };
    let own = unsafe { GetClipboardOwner() }.is_ok_and(|o| o.0 as isize == OWN_WINDOW.load(Ordering::Relaxed));
    let text = if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32) }.is_ok() {
        unicode_text()
    } else {
        None
    };
    Some(Outcome::Copy(Capture { seq, text, own }))
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

    const CLEAN: Marks = Marks {
        exclude_from_monitoring: false,
        viewer_ignore: false,
        can_include_in_history: None,
        can_upload_to_cloud: None,
    };

    /// Each way a copy can say "private".
    fn private_marks() -> [Marks; 4] {
        [
            Marks { exclude_from_monitoring: true, ..CLEAN },
            Marks { viewer_ignore: true, ..CLEAN },
            Marks { can_include_in_history: Some(0), ..CLEAN },
            Marks { can_upload_to_cloud: Some(0), ..CLEAN },
        ]
    }

    #[test]
    fn each_private_mark_is_skipped_with_the_setting_on() {
        for marks in private_marks() {
            assert!(skip_as_private(&marks, true), "{marks:?}");
        }
    }

    #[test]
    fn each_private_mark_is_recorded_with_the_setting_off() {
        for marks in private_marks() {
            assert!(!skip_as_private(&marks, false), "{marks:?}");
        }
    }

    #[test]
    fn an_unmarked_copy_is_recorded_either_way() {
        assert!(!skip_as_private(&CLEAN, true));
        assert!(!skip_as_private(&CLEAN, false));
        assert!(!skip_as_private(&Marks::default(), true));
    }

    #[test]
    fn the_dword_marks_only_count_when_zero() {
        for value in [1, 2, u32::MAX] {
            let marks = Marks { can_include_in_history: Some(value), can_upload_to_cloud: Some(value), ..CLEAN };
            assert!(!skip_as_private(&marks, true), "{value}");
        }
        // One DWORD mark at 1 does not cancel another mark.
        let marks = Marks { can_include_in_history: Some(1), viewer_ignore: true, ..CLEAN };
        assert!(skip_as_private(&marks, true));
    }

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
