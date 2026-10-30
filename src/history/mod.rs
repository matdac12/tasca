//! The clipboard History: the resident's hooks and the shell around the pure
//! state machine.
//!
//! Files (one owner each when work runs in parallel):
//! - `state.rs`   the pure machine: events in, effects out
//! - `clip.rs`    the Clip type
//! - `record.rs`  clipboard listener and read (the private-copy filter point)
//! - `paste.rs`   paste-back: write, restore the target, release modifiers, Ctrl+V
//! - `window.rs`  the window class, placement, wndproc
//! - `draw.rs`    what the window shows
//!
//! All state lives on the UI thread in `SHELL`. The machine is stepped inside
//! a short borrow; its effects are carried out after the borrow is dropped, so
//! `SetForegroundWindow`, `ShowWindow`, `SendInput` and the like (which can
//! re-enter the wndproc) never run inside it.

mod clip;
mod draw;
mod paste;
mod record;
mod state;
mod window;

use std::cell::RefCell;

use windows::core::Result;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, WM_HOTKEY, WM_TIMER};

use crate::hotkey::Slot;
use crate::log::log;
use crate::proto::{WM_HISTORY_READ, WM_SHOW_HISTORY};
use state::{Effect, Event, Machine, Target};

/// Not exported by the `windows` features we enable.
const WM_CLIPBOARDUPDATE: u32 = 0x031D;

struct Shell {
    /// The hidden resident window: clipboard listener and timers.
    resident: HWND,
    /// The History window.
    window: HWND,
    machine: Machine,
    scale: f32,
    /// Where the window was opened: the pointer, the gap below-right of it and
    /// the monitor's work area. The window is placed from these on every size
    /// change, so growing rows never fall off the screen.
    pointer: (i32, i32),
    gap: i32,
    work: window::Work,
}

thread_local! {
    static SHELL: RefCell<Option<Shell>> = const { RefCell::new(None) };
}

/// Short, non-reentrant access to the shell; `None` before `create`, after
/// `destroy`, or on a re-entrant call (logged and skipped).
fn with<R>(f: impl FnOnce(&mut Shell) -> R) -> Option<R> {
    SHELL.with(|c| match c.try_borrow_mut() {
        Ok(mut g) => g.as_mut().map(f),
        Err(_) => {
            log("history busy: re-entrant call skipped");
            None
        }
    })
}

/// Resident startup: the window and the clipboard listener.
pub fn create(resident: HWND) -> Result<()> {
    let window = window::create()?;
    SHELL.with(|c| {
        *c.borrow_mut() =
            Some(Shell { resident, window, machine: Machine::default(), scale: 1.0, pointer: (0, 0), gap: 0, work: window::Work::default() })
    });
    record::start(resident);
    Ok(())
}

pub fn destroy(resident: HWND) {
    record::stop(resident);
    if let Some(shell) = SHELL.with(|c| c.borrow_mut().take()) {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(shell.window);
        }
    }
}

/// The resident's hook: clipboard updates, timers, the History shortcut and
/// the show message. `None` = not ours.
pub fn handle(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let _ = lp;
    match msg {
        WM_CLIPBOARDUPDATE => record::on_update(hwnd),
        WM_HISTORY_READ => record::on_read(hwnd),
        WM_TIMER if wp.0 == paste::T_WRITE => paste::on_write_retry(hwnd),
        WM_TIMER if wp.0 == paste::T_KEYS => paste::on_keys(hwnd),
        WM_HOTKEY if Slot::from_id(wp.0 as i32) == Some(Slot::History) => toggle(),
        WM_SHOW_HISTORY => open(),
        _ => return None,
    }
    Some(LRESULT(0))
}

/// Alt+V: open, or close when already open.
fn toggle() {
    if with(|s| s.machine.is_open()) == Some(true) {
        dispatch(Event::Deactivated);
    } else {
        open();
    }
}

/// Open at the pointer, remembering the app to paste back into.
fn open() {
    let Some((window, rows, already)) = with(|s| (s.window, s.machine.clips().len(), s.machine.is_open())) else {
        return;
    };
    if already {
        return; // keep the paste target and the selection
    }
    let foreground = unsafe { GetForegroundWindow() };
    let target = (!foreground.is_invalid() && foreground != window).then_some(Target(foreground.0 as isize));
    let (pointer, work, dpi) = window::pointer_monitor();
    let scale = dpi as f32 / 96.0;
    let gap = window::pointer_gap(scale);
    with(|s| {
        s.scale = scale;
        s.pointer = pointer;
        s.gap = gap;
        s.work = work;
    });
    let (origin, _) = window::frame(pointer, gap, rows, scale, work);
    log(format!("history: open at {origin:?}, dpi {dpi}, {rows} clips, target {target:?}"));
    dispatch(Event::Opened(target));
}

/// Step the machine, then carry out its effects outside the borrow.
fn dispatch(event: Event) {
    let Some(effects) = with(|s| s.machine.step(event)) else { return };
    for effect in effects {
        apply(effect);
    }
}

fn apply(effect: Effect) {
    let Some((resident, window)) = with(|s| (s.resident, s.window)) else { return };
    match effect {
        Effect::Add(id) => log(format!("history: recorded clip {}", id.0)),
        Effect::MoveToTop(id) => log(format!("history: clip {} to the top", id.0)),
        Effect::Skip(why) => log(format!("history: skipped copy ({why:?})")),
        Effect::Redraw => {
            let frame = with(|s| window::frame(s.pointer, s.gap, s.machine.clips().len(), s.scale, s.work));
            if let Some((origin, size)) = frame {
                if !window::show(window, origin, size) {
                    dispatch(Event::Deactivated);
                }
            }
        }
        Effect::Close => window::hide(window),
        Effect::Paste { clip, target } => paste::paste(resident, window, &clip, target),
        Effect::Copy { clip } => paste::copy(resident, window, &clip),
    }
}

/// What the window shows now (for `WM_PAINT`).
fn view() -> Option<draw::View> {
    with(|s| {
        let clips = s.machine.clips();
        let first = draw::first_visible(s.machine.selected());
        let rows: Vec<String> = clips.iter().skip(first).take(draw::MAX_ROWS).map(|c| draw::one_line(&c.text)).collect();
        let selected = (!clips.is_empty()).then(|| s.machine.selected() - first);
        draw::View { rows, selected, scale: s.scale }
    })
}
