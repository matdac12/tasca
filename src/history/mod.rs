//! The clipboard History: the resident's hooks and the shell around the pure
//! state machine.
//!
//! Files (one owner each when work runs in parallel):
//! - `state.rs`   the pure machine: events in, effects out
//! - `clip.rs`    the Clip type
//! - `record.rs`  clipboard listener and read (the private-copy filter point)
//! - `paste.rs`   paste-back: write, restore the target, release modifiers, Ctrl+V
//! - `window.rs`  the window class, placement, wndproc
//! - `draw.rs`    what the window shows (Direct2D, like the Shelf)
//! - `layout.rs`  the panel's geometry and hit-testing (pure)
//! - `text.rs`    row titles, type lines, ages (pure)
//!
//! All state lives on the UI thread in `SHELL`. The machine is stepped inside
//! a short borrow; its effects are carried out after the borrow is dropped, so
//! `SetForegroundWindow`, `ShowWindow`, `SendInput` and the like (which can
//! re-enter the wndproc) never run inside it.

mod clip;
mod draw;
mod layout;
mod paste;
mod record;
mod state;
mod text;
mod window;

use std::cell::RefCell;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::Result;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, WM_HOTKEY, WM_TIMER};

use crate::hotkey::Slot;
use crate::log::log;
use crate::proto::{WM_HISTORY_READ, WM_SHOW_HISTORY};
use layout::{Hit, Layout};
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
    /// Row under the pointer.
    hover: Option<usize>,
    /// List scroll in DIP; follows the selection, and the wheel moves it.
    scroll: f32,
    /// The selection the scroll last followed.
    followed: usize,
}

impl Shell {
    fn layout(&self) -> Layout {
        Layout::new(self.scale, self.machine.clips().len(), 0, false)
    }
}

/// Milliseconds since the Unix epoch: the machine's clock.
fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
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
            Some(Shell {
            resident,
            window,
            machine: Machine::default(),
            scale: 1.0,
            pointer: (0, 0),
            gap: 0,
            work: window::Work::default(),
            hover: None,
            scroll: 0.0,
            followed: 0,
        })
    });
    if let Err(e) = draw::init() {
        log(format!("history: renderer not ready yet: {e}"));
    }
    record::start(resident, window);
    Ok(())
}

pub fn destroy(resident: HWND) {
    record::stop(resident);
    draw::destroy();
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
        s.hover = None;
        s.scroll = 0.0;
        s.followed = 0;
    });
    log(format!("history: open at {pointer:?}, dpi {dpi}, {rows} clips, target {target:?}"));
    dispatch(Event::Opened(target));
}

/// Step the machine, then carry out its effects outside the borrow.
fn dispatch(event: Event) {
    let now = now_ms();
    let Some(effects) = with(|s| s.machine.step(event, now)) else { return };
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
        Effect::Redraw => redraw(),
        Effect::Close => window::hide(window),
        Effect::Paste { clip, target } => paste::paste(resident, window, &clip, target),
        Effect::Copy { clip } => paste::copy(resident, window, &clip),
    }
}

/// Render the panel for the machine's state and show it (or update it).
fn redraw() {
    let now = now_ms();
    let Some((window, view, origin)) = with(|s| {
        let layout = s.layout();
        let selected = s.machine.selected();
        if selected != s.followed {
            s.followed = selected;
            s.scroll = layout.scroll_to_show(s.scroll, selected);
        }
        s.scroll = layout.clamp_scroll(s.scroll);
        if s.hover.is_some_and(|i| i >= layout.rows) {
            s.hover = None;
        }
        let (origin, _) = window::frame(s.pointer, s.gap, &layout, s.work);
        (s.window, view(s, layout, now), origin)
    }) else {
        return;
    };
    if !window::show(window, &view, origin) {
        dispatch(Event::Deactivated);
    }
}

/// What the window shows now.
fn view(s: &Shell, layout: Layout, now: u64) -> draw::View {
    let clips = s.machine.clips();
    let rows = clips
        .iter()
        .map(|c| draw::RowView { title: text::one_line(&c.text), sub: text::type_line(&c.text, now, c.last_copied_ms) })
        .collect();
    draw::View {
        layout,
        rows,
        selected: (!clips.is_empty()).then(|| s.machine.selected()),
        hover: s.hover,
        scroll: s.scroll,
        query: String::new(),
        count: text::count_label(clips.len()),
    }
}

/// The pointer moved over the window: highlight the row under it.
pub(crate) fn on_mouse_move(x: i32, y: i32) {
    let changed = with(|s| {
        let hover = match s.layout().hit(x, y, s.scroll) {
            Hit::Row(i) => Some(i),
            _ => None,
        };
        let changed = s.machine.is_open() && hover != s.hover;
        if changed {
            s.hover = hover;
        }
        changed
    });
    if changed == Some(true) {
        redraw();
    }
}

/// The pointer left the window: no hover.
pub(crate) fn on_mouse_leave() {
    if with(|s| s.hover.take().is_some() && s.machine.is_open()) == Some(true) {
        redraw();
    }
}

/// The wheel turned over the window (`delta` in 1/120 notches, client x, y).
pub(crate) fn on_wheel(delta: i32, x: i32, y: i32) {
    let changed = with(|s| {
        let layout = s.layout();
        let scroll = layout.wheel(s.scroll, delta);
        let hover = match layout.hit(x, y, scroll) {
            Hit::Row(i) => Some(i),
            _ => None,
        };
        let changed = s.machine.is_open() && (scroll != s.scroll || hover != s.hover);
        if changed {
            s.scroll = scroll;
            s.hover = hover;
        }
        changed
    });
    if changed == Some(true) {
        redraw();
    }
}
