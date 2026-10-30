//! Paste-back. The state machine says `Paste { clip, target }` (or `Copy`);
//! this module does the Win32 part, in the order the spec fixes: write the Clip
//! (the window is the clipboard owner; the write's sequence number, taken while
//! the clipboard is still open, goes back as `OwnWrite`), restore the target,
//! then, after a short timer so the target has taken focus, release held
//! Alt/Ctrl/Shift/Win and send a plain Ctrl+V.
//!
//! Nothing here sleeps and nothing here runs inside a `RefCell` borrow: the
//! caller (`dispatch`) has already dropped it. A busy clipboard is retried on a
//! timer, a bounded number of times.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, IsIconic, IsWindow, KillTimer, PostMessageW, SetForegroundWindow, SetTimer, ShowWindow, SW_RESTORE,
};

use super::clip::{Clip, Content};
use super::{formats, image};
use super::state::{Event, Target};
use crate::log::log;
use crate::proto::WM_HISTORY_PASTE_READY;

/// Timer ids on the resident window.
pub const T_KEYS: usize = 0x4802;
pub const T_WRITE: usize = 0x4803;
/// Between `SetForegroundWindow` and the keystrokes.
const KEYS_DELAY_MS: u32 = 40;
const WRITE_RETRY_MS: u32 = 25;
const MAX_WRITE_TRIES: u32 = 12;

/// A clipboard format a Clip can be written as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slot {
    Text,
    Html,
    Rtf,
    Png,
    Dib,
    Hdrop,
}

impl Slot {
    fn id(self) -> u32 {
        let f = super::record::content_formats();
        match self {
            Slot::Text => CF_UNICODETEXT.0 as u32,
            Slot::Html => f.html,
            Slot::Rtf => f.rtf,
            Slot::Png => f.png,
            Slot::Dib => CF_DIB.0 as u32,
            Slot::Hdrop => CF_HDROP.0 as u32,
        }
    }
}

/// What goes on the clipboard for a Clip, in write order. Text: the text,
/// then its HTML and RTF unless `plain`. Image: PNG (alpha) and, once decoded,
/// DIB. Files: the `CF_HDROP` list. `plain` only concerns text. Pure.
fn plan(clip: &Clip, plain: bool, dib: Option<Vec<u8>>) -> Vec<(Slot, Vec<u8>)> {
    let nul = |s: &str| s.bytes().chain(Some(0)).collect::<Vec<u8>>();
    match &clip.content {
        Content::Text { html, rtf } => {
            let units = clip.text.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect();
            let mut items = vec![(Slot::Text, units)];
            if !plain {
                items.extend(html.as_deref().map(|h| (Slot::Html, nul(h))));
                items.extend(rtf.as_deref().map(|r| (Slot::Rtf, nul(r))));
            }
            items
        }
        Content::Image { png, .. } => {
            let mut items = vec![(Slot::Png, png.to_vec())];
            items.extend(dib.map(|d| (Slot::Dib, d)));
            items
        }
        Content::Files { paths } => vec![(Slot::Hdrop, formats::encode_hdrop(paths))],
    }
}

/// A write waiting for the clipboard to be free (or for the DIB to be made).
struct Job {
    items: Vec<(Slot, Vec<u8>)>,
    /// `None` = copy only.
    target: Option<Target>,
    tries: u32,
    owner: isize,
    /// An image's DIB is still being made on a worker.
    waiting: bool,
    /// Which paste this is, so a late worker cannot feed a newer one.
    serial: u64,
}

thread_local! {
    static JOB: RefCell<Option<Job>> = const { RefCell::new(None) };
    /// The window that should have focus when the keys go out.
    static PENDING: Cell<Option<isize>> = const { Cell::new(None) };
}

/// The newest paste's serial: a worker whose paste was replaced publishes nothing.
static CURRENT: AtomicU64 = AtomicU64::new(0);

/// The DIB a worker made: (serial of the paste, bytes; empty = it failed).
static PREPARED: Mutex<Option<(u64, Vec<u8>)>> = Mutex::new(None);

enum Written {
    /// Sequence number of this write, taken while the clipboard was still ours.
    Done(u32),
    /// Another app has the clipboard open: try again later.
    Busy,
    Failed,
}

/// A moveable global block holding `bytes`; the clipboard takes it on success.
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

fn write(owner: HWND, items: &[(Slot, Vec<u8>)]) -> Written {
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            return Written::Busy;
        }
        let _ = EmptyClipboard();
        let mut first = false;
        for (i, (slot, bytes)) in items.iter().enumerate() {
            let Some(mem) = global_of(bytes) else { continue };
            // On success the clipboard owns `mem`.
            if SetClipboardData(slot.id(), Some(HANDLE(mem.0))).is_ok() {
                first |= i == 0;
            } else {
                let _ = GlobalFree(Some(mem));
            }
        }
        // Nobody else can change the clipboard while it is open: this is ours.
        let seq = GetClipboardSequenceNumber();
        let _ = CloseClipboard();
        // The first format is the one every target can take (text, PNG, the list).
        if first { Written::Done(seq) } else { Written::Failed }
    }
}

/// Copy-only: the Clip is on the clipboard and nothing is typed.
pub fn copy(resident: HWND, owner: HWND, clip: &Clip, plain: bool) {
    start(resident, owner, clip, None, plain);
}

/// Write the Clip, then bring `target` back and schedule the keystrokes. The
/// History window is already hidden.
pub fn paste(resident: HWND, owner: HWND, clip: &Clip, target: Target, plain: bool) {
    start(resident, owner, clip, Some(target), plain);
}

fn start(resident: HWND, owner: HWND, clip: &Clip, target: Option<Target>, plain: bool) {
    let serial = CURRENT.fetch_add(1, Ordering::SeqCst) + 1;
    let image = matches!(clip.content, Content::Image { .. });
    let job = Job { items: plan(clip, plain, None), target, tries: 0, owner: owner.0 as isize, waiting: image, serial };
    // A newer paste replaces one still waiting.
    JOB.with(|j| *j.borrow_mut() = Some(job));
    if let Content::Image { png, .. } = &clip.content {
        make_dib(resident, Arc::clone(png), serial);
    } else {
        attempt(resident);
    }
}

/// Decode the PNG into a DIB on a worker (the UI thread must not);
/// `WM_HISTORY_PASTE_READY` follows. If it fails the PNG alone is written.
fn make_dib(resident: HWND, png: Arc<Vec<u8>>, serial: u64) {
    let raw = resident.0 as isize;
    let spawned = std::thread::Builder::new().name("history-dib".into()).spawn(move || {
        let dib = image::decode_png(&png).ok().map(|b| image::bitmap_to_dib(&b)).unwrap_or_default();
        if CURRENT.load(Ordering::SeqCst) != serial {
            return;
        }
        *PREPARED.lock().unwrap_or_else(|e| e.into_inner()) = Some((serial, dib));
        unsafe {
            let _ = PostMessageW(Some(HWND(raw as *mut _)), WM_HISTORY_PASTE_READY, WPARAM(0), LPARAM(0));
        }
    });
    if spawned.is_err() {
        *PREPARED.lock().unwrap_or_else(|e| e.into_inner()) = Some((serial, Vec::new()));
        on_prepared(resident);
    }
}

/// `WM_HISTORY_PASTE_READY`: the DIB is made; write the Clip.
pub fn on_prepared(resident: HWND) {
    let Some((serial, dib)) = PREPARED.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    let ready = JOB.with(|j| match j.borrow_mut().as_mut() {
        Some(job) if job.serial == serial && job.waiting => {
            job.waiting = false;
            if !dib.is_empty() {
                job.items.push((Slot::Dib, dib));
            }
            true
        }
        _ => false,
    });
    if ready {
        attempt(resident);
    }
}

/// `WM_TIMER` with `T_WRITE`.
pub fn on_write_retry(resident: HWND) {
    attempt(resident);
}

fn attempt(resident: HWND) {
    unsafe {
        let _ = KillTimer(Some(resident), T_WRITE);
    }
    let Some(job) = JOB.with(|j| j.borrow_mut().take()) else { return };
    if job.waiting {
        JOB.with(|j| *j.borrow_mut() = Some(job));
        return;
    }
    match write(HWND(job.owner as *mut _), &job.items) {
        Written::Done(seq) => {
            let kinds: Vec<Slot> = job.items.iter().map(|(s, _)| *s).collect();
            log(format!("history: wrote a Clip ({kinds:?})"));
            super::dispatch(Event::OwnWrite(seq));
            if let Some(target) = job.target {
                restore_and_schedule_keys(resident, target);
            }
        }
        Written::Busy if more_tries(job.tries) => {
            JOB.with(|j| *j.borrow_mut() = Some(Job { tries: job.tries + 1, ..job }));
            unsafe {
                SetTimer(Some(resident), T_WRITE, WRITE_RETRY_MS, None);
            }
        }
        Written::Busy => log("history: the clipboard stayed busy, nothing pasted"),
        Written::Failed => log("history: could not write the clipboard, nothing pasted"),
    }
}

/// Whether a write that failed after `tries` retries may be retried again.
fn more_tries(tries: u32) -> bool {
    tries < MAX_WRITE_TRIES
}

fn restore_and_schedule_keys(resident: HWND, target: Target) {
    let hwnd = HWND(target.0 as *mut _);
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            log("history: the paste target is gone, the Clip stays on the clipboard");
            return;
        }
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if !SetForegroundWindow(hwnd).as_bool() {
            log("history: SetForegroundWindow refused");
        }
        PENDING.set(Some(target.0));
        SetTimer(Some(resident), T_KEYS, KEYS_DELAY_MS, None);
    }
}

/// `WM_TIMER` with `T_KEYS`: release what the user still holds, send Ctrl+V.
pub fn on_keys(resident: HWND) {
    unsafe {
        let _ = KillTimer(Some(resident), T_KEYS);
    }
    let Some(target) = PENDING.take() else { return };
    // Focus did not come back (refused, or the user clicked elsewhere): typing
    // now would paste into the wrong window. The Clip is on the clipboard.
    if unsafe { GetForegroundWindow() }.0 as isize != target {
        log("history: the target is not in the foreground, Ctrl+V not sent");
        return;
    }
    let held: Vec<VIRTUAL_KEY> = MODIFIERS
        .iter()
        .copied()
        .filter(|vk| unsafe { GetAsyncKeyState(vk.0 as i32) } < 0)
        .collect();
    let inputs: Vec<INPUT> = paste_keys(&held).into_iter().map(|(vk, up)| key(vk, up)).collect();
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    log(format!("history: sent {sent}/{} key events (released {} held modifiers)", inputs.len(), held.len()));
}

/// Every key whose held state would turn Ctrl+V into something else.
const MODIFIERS: [VIRTUAL_KEY; 8] =
    [VK_LMENU, VK_RMENU, VK_LCONTROL, VK_RCONTROL, VK_LSHIFT, VK_RSHIFT, VK_LWIN, VK_RWIN];

/// An unassigned key. Tapped between the start of an Alt hold and its release
/// it keeps the target from taking the Alt for a click on its menu bar.
const VK_UNASSIGNED: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);

/// The key events for a plain paste: key-up for each held modifier (behind a
/// harmless key tap when Alt is among them, so the target does not open its
/// menu on the lone Alt release), then Ctrl+V as `(key, is_up)` in order. Pure.
fn paste_keys(held: &[VIRTUAL_KEY]) -> Vec<(VIRTUAL_KEY, bool)> {
    let mut keys: Vec<(VIRTUAL_KEY, bool)> = Vec::new();
    if held.iter().any(|&vk| vk == VK_LMENU || vk == VK_RMENU) {
        keys.extend([(VK_UNASSIGNED, false), (VK_UNASSIGNED, true)]);
    }
    keys.extend(held.iter().map(|&vk| (vk, true)));
    keys.extend([(VK_CONTROL, false), (VK_V, false), (VK_V, true), (VK_CONTROL, true)]);
    keys
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::clip::ClipId;

    fn text_clip(html: Option<&str>, rtf: Option<&str>) -> Clip {
        Clip {
            content: Content::Text { html: html.map(Into::into), rtf: rtf.map(Into::into) },
            ..Clip::plain(ClipId(1), "hi".into(), 0, false)
        }
    }

    fn slots(items: &[(Slot, Vec<u8>)]) -> Vec<Slot> {
        items.iter().map(|(s, _)| *s).collect()
    }

    #[test]
    fn formatted_text_is_written_with_html_and_rtf_and_plain_only_text() {
        let clip = text_clip(Some("<b>"), Some("{\rtf1}"));
        let full = plan(&clip, false, None);
        assert_eq!(slots(&full), [Slot::Text, Slot::Html, Slot::Rtf]);
        assert_eq!(full[0].1, [b'h', 0, b'i', 0, 0, 0]);
        assert_eq!(full[1].1, b"<b>\0");
        assert_eq!(slots(&plan(&clip, true, None)), [Slot::Text]);
        assert_eq!(slots(&plan(&text_clip(None, None), false, None)), [Slot::Text]);
        assert_eq!(slots(&plan(&text_clip(None, Some("r")), false, None)), [Slot::Text, Slot::Rtf]);
    }

    #[test]
    fn an_image_is_written_as_png_then_dib_and_files_as_a_drop_list() {
        let img = Clip {
            content: Content::Image { width: 1, height: 1, png: Arc::new(vec![9, 9]), hash: 1 },
            text: String::new(),
            ..Clip::plain(ClipId(1), String::new(), 0, false)
        };
        assert_eq!(plan(&img, false, None), [(Slot::Png, vec![9, 9])]);
        assert_eq!(slots(&plan(&img, true, Some(vec![1]))), [Slot::Png, Slot::Dib]);
        let files = Clip {
            content: Content::Files { paths: vec!["C:\\a.txt".into()] },
            text: String::new(),
            ..img
        };
        let items = plan(&files, false, None);
        assert_eq!(slots(&items), [Slot::Hdrop]);
        assert_eq!(formats::decode_hdrop(&items[0].1), ["C:\\a.txt"]);
    }


    #[test]
    fn a_plain_paste_is_ctrl_v() {
        assert_eq!(
            paste_keys(&[]),
            [(VK_CONTROL, false), (VK_V, false), (VK_V, true), (VK_CONTROL, true)]
        );
    }

    #[test]
    fn held_modifiers_are_released_before_ctrl_v() {
        // Alt+V was just pressed and Alt is still down: Ctrl+Alt+V must not happen.
        let keys = paste_keys(&[VK_LCONTROL, VK_LSHIFT]);
        assert_eq!(&keys[..2], [(VK_LCONTROL, true), (VK_LSHIFT, true)]);
        assert_eq!(keys[2], (VK_CONTROL, false));
        assert_eq!(keys.len(), 6);
    }

    #[test]
    fn a_held_alt_is_released_behind_a_key_tap_before_ctrl_v() {
        // Alt+V, then Alt+3 with Alt still down: the target sees Alt, a tap, Alt up,
        // then a plain Ctrl+V; never Ctrl+Alt+V, never a lone Alt press and release.
        let keys = paste_keys(&[VK_LMENU]);
        assert_eq!(
            keys,
            [
                (VK_UNASSIGNED, false),
                (VK_UNASSIGNED, true),
                (VK_LMENU, true),
                (VK_CONTROL, false),
                (VK_V, false),
                (VK_V, true),
                (VK_CONTROL, true),
            ]
        );
        assert!(!keys.iter().any(|&(vk, up)| (vk == VK_LMENU || vk == VK_RMENU) && !up));
    }

    #[test]
    fn right_alt_counts_as_alt_and_no_alt_means_no_tap() {
        assert_eq!(paste_keys(&[VK_RMENU])[0], (VK_UNASSIGNED, false));
        assert!(!paste_keys(&[VK_LWIN]).iter().any(|&(vk, _)| vk == VK_UNASSIGNED));
    }

    #[test]
    fn a_busy_clipboard_is_retried_a_bounded_number_of_times() {
        assert!(more_tries(0));
        assert!(more_tries(MAX_WRITE_TRIES - 1));
        assert!(!more_tries(MAX_WRITE_TRIES));
    }
}
