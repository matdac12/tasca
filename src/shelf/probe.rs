//! File facts (exists, folder, size) for Items, read on one long-lived worker
//! thread and posted back as `WM_PROBED`: `metadata` on a network path can hang
//! for seconds, and the UI thread must never block (drops arrive on it).
//!
//! Jobs run in order, so a hung share delays later answers but never piles up
//! threads. The re-check on every show is queued at most once at a time.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::OnceLock;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::model::{Facts, ItemId};
use crate::log::log;
use crate::proto::WM_PROBED;

/// A re-check is queued or running.
static RECHECK_PENDING: AtomicBool = AtomicBool::new(false);
static WORKER: OnceLock<Sender<Job>> = OnceLock::new();

struct Job {
    /// The window to post to, as an integer (`HWND` is not `Send`).
    hwnd: isize,
    items: Vec<(ItemId, Vec<PathBuf>)>,
    recheck: bool,
}

pub struct Probed {
    pub facts: Vec<(ItemId, Facts)>,
}

/// File-system facts for one Item's files. Blocking: worker thread only.
pub fn facts_of(files: &[PathBuf]) -> Facts {
    let mut f = Facts::default();
    for (i, p) in files.iter().enumerate() {
        match std::fs::metadata(p) {
            Ok(m) => {
                if i == 0 {
                    f.first_is_dir = m.is_dir();
                }
                if !m.is_dir() {
                    f.total_bytes += m.len();
                }
            }
            Err(_) => f.missing = true,
        }
    }
    f
}

fn run(rx: Receiver<Job>) {
    for job in rx {
        let facts = job.items.into_iter().map(|(id, files)| (id, facts_of(&files))).collect();
        if job.recheck {
            RECHECK_PENDING.store(false, Ordering::Release);
        }
        let msg = Box::into_raw(Box::new(Probed { facts }));
        let hwnd = HWND(job.hwnd as *mut _);
        if unsafe { PostMessageW(Some(hwnd), WM_PROBED, WPARAM(0), LPARAM(msg as isize)) }.is_err() {
            // The window is gone: take the box back.
            drop(unsafe { Box::from_raw(msg) });
        }
    }
}

fn worker() -> &'static Sender<Job> {
    WORKER.get_or_init(|| {
        let (tx, rx) = channel();
        if let Err(e) = std::thread::Builder::new().name("tasca-probe".into()).spawn(move || run(rx)) {
            // The receiver went down with the closure, so every send fails.
            log(format!("probe: could not start the worker: {e}"));
        }
        tx
    })
}

/// Queue `items` for probing; the result is posted to `hwnd`.
pub fn request(hwnd: HWND, items: Vec<(ItemId, Vec<PathBuf>)>, recheck: bool) {
    if items.is_empty() {
        return;
    }
    if recheck && RECHECK_PENDING.swap(true, Ordering::AcqRel) {
        log("re-check skipped: the previous one has not finished");
        return;
    }
    if worker().send(Job { hwnd: hwnd.0 as isize, items, recheck }).is_err() {
        log("probe: worker unavailable");
        if recheck {
            RECHECK_PENDING.store(false, Ordering::Release);
        }
    }
}

/// Take ownership of a `WM_PROBED` payload.
///
/// # Safety
/// `lp` must be the `LPARAM` of a `WM_PROBED` posted by the worker, taken once.
pub unsafe fn take(lp: LPARAM) -> Probed {
    *unsafe { Box::from_raw(lp.0 as *mut Probed) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facts_of_real_and_missing_files() {
        let dir = std::env::temp_dir().join(format!("tasca-probe-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.bin"), [0u8; 1000]).unwrap();
        std::fs::write(dir.join("b.bin"), [0u8; 24]).unwrap();

        let f = facts_of(&[dir.join("a.bin"), dir.join("b.bin")]);
        assert_eq!(f, Facts { first_is_dir: false, total_bytes: 1024, missing: false });
        let f = facts_of(&[dir.join("sub")]);
        assert!(f.first_is_dir && !f.missing);
        let f = facts_of(&[dir.join("a.bin"), dir.join("gone.bin")]);
        assert!(f.missing, "one missing file makes the Item missing");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
