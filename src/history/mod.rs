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
//! - `menu.rs`    the right-click menu of a row
//! - `text.rs`    row titles, type lines, ages (pure)
//!
//! All state lives on the UI thread in `SHELL`. The machine is stepped inside
//! a short borrow; its effects are carried out after the borrow is dropped, so
//! `SetForegroundWindow`, `ShowWindow`, `SendInput` and the like (which can
//! re-enter the wndproc) never run inside it.

mod clip;
mod draw;
mod formats;
mod image;
mod layout;
mod menu;
mod paste;
mod preview;
mod record;
mod search;
mod state;
mod store;
mod text;
mod window;

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::Result;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, PostMessageW, WM_HOTKEY, WM_TIMER};

use crate::hotkey::Slot;
use crate::log::log;
use crate::proto::{WM_HISTORY_LOADED, WM_HISTORY_PASTE_READY, WM_HISTORY_PREVIEW, WM_HISTORY_READ, WM_SHOW_HISTORY};
use clip::Clip;
use clip::Content;
use layout::{Hit, Layout};
use state::{Effect, Event, Machine, Target};
use store::Store;

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
    /// Recording copies (the clipboard listener is registered).
    listening: bool,
    /// The worker that reads and writes the saved History; `None` when
    /// `%LOCALAPPDATA%` is not set (the History then lives in memory only).
    store: Option<Store>,
}

impl Shell {
    fn layout(&self) -> Layout {
        Layout::new(self.scale, self.machine.shown().len(), self.machine.shown_pinned(), !self.machine.query().trim().is_empty())
    }
}

/// Milliseconds since the Unix epoch: the machine's clock.
fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The saved History, handed from the store's worker to the UI thread (the
/// message that follows carries no data).
static LOADED: Mutex<Option<Vec<Clip>>> = Mutex::new(None);

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
            listening: true,
            store: start_store(resident),
        })
    });
    if let Err(e) = draw::init() {
        log(format!("history: renderer not ready yet: {e}"));
    }
    record::start(resident, window);
    Ok(())
}

/// Start the store's worker and ask it to read the saved History.
fn start_store(resident: HWND) -> Option<Store> {
    let store = Store::spawn(store::default_dir()?);
    let resident = resident.0 as isize;
    store.load(move |records| {
        // Still on the store's worker: images are decoded here to hash their pixels.
        let clips: Vec<Clip> = records.into_iter().filter_map(Clip::from_record).collect();
        *LOADED.lock().unwrap_or_else(|e| e.into_inner()) = Some(clips);
        let hwnd = HWND(resident as *mut _);
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_HISTORY_LOADED, WPARAM(0), LPARAM(0));
        }
    });
    Some(store)
}

pub fn destroy(resident: HWND) {
    record::stop(resident);
    finish_load();
    with(|s| s.listening = false);
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
        WM_HISTORY_LOADED => on_loaded(),
        WM_HISTORY_PASTE_READY => paste::on_prepared(hwnd),
        WM_HISTORY_PREVIEW => redraw_if_open(),
        WM_TIMER if wp.0 == paste::T_WRITE => paste::on_write_retry(hwnd),
        WM_TIMER if wp.0 == paste::T_KEYS => paste::on_keys(hwnd),
        WM_HOTKEY if Slot::from_id(wp.0 as i32) == Some(Slot::History) => toggle(),
        WM_SHOW_HISTORY => open(),
        _ => return None,
    }
    Some(LRESULT(0))
}

/// Quit before the saved History was merged in: wait for the worker to finish
/// reading, merge it and queue the save, so what was copied since start is not
/// lost (a save is held back until the merge). A short block at quit.
fn finish_load() {
    if with(|s| !s.machine.is_loaded() && s.store.is_some()) != Some(true) {
        return;
    }
    // The wait is on the worker only; nothing in it pumps messages.
    with(|s| s.store.as_ref().map(Store::flush));
    on_loaded();
}

/// The store's worker finished reading: merge the saved Clips in.
fn on_loaded() {
    let Some(clips) = LOADED.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    log(format!("history: loaded {} saved clips", clips.len()));
    dispatch(Event::Loaded(clips));
}

/// Hand the Clips to the store's worker (the write happens there).
fn save() {
    with(|s| {
        if let Some(store) = &s.store {
            store.save(s.machine.clips().iter().map(Clip::to_record).collect());
        }
    });
}

/// Alt+V: open, or close when already open.
fn toggle() {
    if with(|s| s.machine.is_open()) == Some(true) {
        dispatch(Event::Deactivated);
    } else {
        open();
    }
}

/// The app to paste back into: the foreground window, unless there is none or
/// it is one of ours (the History window, or the resident when the tray menu
/// opened the History), which cannot take a paste.
fn paste_target(foreground: isize, history: isize, resident: isize) -> Option<Target> {
    (foreground != 0 && foreground != history && foreground != resident).then_some(Target(foreground))
}

/// Start or stop recording copies (the Settings switch for the History).
#[allow(dead_code)] // resident.rs switches to it after the merge
pub fn set_listening(resident: HWND, on: bool) {
    let Some((window, change)) = with(|s| (s.window, switch(&mut s.listening, on))) else { return };
    if !change {
        return;
    }
    if on {
        record::start(resident, window);
    } else {
        record::stop(resident);
    }
}

/// Move a switch to `on`; `true` when that changed it (so the listener is
/// added or removed once, never twice).
fn switch(current: &mut bool, on: bool) -> bool {
    std::mem::replace(current, on) != on
}

/// Open at the pointer, remembering the app to paste back into.
fn open() {
    let Some((window, resident, rows, already)) =
        with(|s| (s.window, s.resident, s.machine.clips().len(), s.machine.is_open()))
    else {
        return;
    };
    if already {
        return; // keep the paste target and the selection
    }
    let foreground = unsafe { GetForegroundWindow() };
    let target = paste_target(foreground.0 as isize, window.0 as isize, resident.0 as isize);
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
    let Some(effects) = with(|s| {
        // A read that was already in flight when recording was switched off.
        if !s.listening && matches!(event, Event::Copied(_)) {
            return Vec::new();
        }
        let before = s.machine.query().len();
        let effects = s.machine.step(event, now);
        if s.machine.query().len() != before {
            // A new list: show it from the top.
            s.scroll = 0.0;
            s.followed = 0;
        }
        effects
    }) else {
        return;
    };
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
        Effect::Refused(why) => log(format!("history: action ignored ({why:?})")),
        Effect::Redraw => redraw(),
        Effect::Save => save(),
        Effect::Close => window::hide(window),
        Effect::Paste { clip, target, plain } => paste::paste(resident, window, &clip, target, plain),
        Effect::Copy { clip, plain } => paste::copy(resident, window, &clip, plain),
    }
}

/// A preview landed: show it if the panel is up.
fn redraw_if_open() {
    if with(|s| s.machine.is_open()) == Some(true) {
        redraw();
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
        let (origin, _) = window::frame(s.pointer, s.gap, &layout, s.work);
        // Rows may have moved under a pointer that did not: look again.
        s.hover = hover_at(&layout, window::cursor(), origin, s.scroll);
        (s.window, view(s, layout, now), origin)
    }) else {
        return;
    };
    if !window::show(window, &view, origin) {
        dispatch(Event::Deactivated);
    }
}

/// The row under a screen point, for a window at `origin`.
fn hover_at(layout: &Layout, pointer: (i32, i32), origin: (i32, i32), scroll: f32) -> Option<usize> {
    match layout.hit(pointer.0 - origin.0, pointer.1 - origin.1, scroll) {
        Hit::Row(i) => Some(i),
        _ => None,
    }
}

/// What the window shows now: only the rows in the viewport are built.
fn view(s: &Shell, layout: Layout, now: u64) -> draw::View {
    let clips = s.machine.shown();
    let visible = layout.visible_rows(s.scroll);
    let first = visible.start;
    let dpi_key = preview::Key::new(0, s.scale).dpi;
    // Previews of images that left the History, or of another DPI, are freed.
    let live: std::collections::HashSet<u64> = s
        .machine
        .clips()
        .iter()
        .filter_map(|c| match &c.content {
            Content::Image { hash, .. } => Some(*hash),
            _ => None,
        })
        .collect();
    preview::retain(dpi_key, &live);
    let rows = clips[visible]
        .iter()
        .map(|c| draw::RowView {
            title: text::clip_title(c),
            sub: if c.pinned { text::pinned_line(c) } else { text::type_line(c, now, c.last_copied_ms) },
            pinned: c.pinned,
            preview: match &c.content {
                Content::Image { hash, png, .. } => {
                    let key = preview::Key::new(*hash, s.scale);
                    let ready = preview::get(key);
                    if ready.is_none() {
                        preview::request(s.resident, s.scale, *hash, png);
                    }
                    Some(ready)
                }
                _ => None,
            },
        })
        .collect();
    draw::View {
        layout,
        rows,
        first,
        selected: (!clips.is_empty()).then(|| s.machine.selected()),
        hover: s.hover,
        scroll: s.scroll,
        query: s.machine.query().to_string(),
        count: text::count_label(s.machine.clips().len()),
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

/// The left button went down at client pixel (x, y).
pub(crate) fn on_click(x: i32, y: i32, shift: bool) {
    let hit = with(|s| if s.machine.is_open() { Some(s.layout().hit(x, y, s.scroll)) } else { None });
    match hit {
        Some(Some(Hit::Row(row))) => dispatch(Event::Click { row, shift }),
        Some(Some(Hit::Clear)) => dispatch(Event::Clear),
        _ => {}
    }
}

/// The right button went up at client pixel (x, y): the row's menu. The shell
/// is borrowed only to read what the menu needs; the menu runs its own message
/// loop, so nothing is held while it is up. The Clip is named by id because
/// the list can change meanwhile (a copy arriving).
pub(crate) fn on_right_click(x: i32, y: i32) {
    let target = with(|s| match s.layout().hit(x, y, s.scroll) {
        Hit::Row(i) if s.machine.is_open() => s.machine.shown().get(i).map(|c| (s.window, c.id, c.pinned)),
        _ => None,
    });
    let Some(Some((window, id, pinned))) = target else { return };
    dispatch(Event::Select(id));
    if let Some(action) = menu::pick(window, pinned, x, y) {
        dispatch(Event::ClipAction { id, action });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_paste_target_is_the_foreground_app_never_one_of_ours() {
        assert_eq!(paste_target(0x500, 0x100, 0x200), Some(Target(0x500)));
        assert_eq!(paste_target(0, 0x100, 0x200), None);
        assert_eq!(paste_target(0x100, 0x100, 0x200), None);
        // The tray menu leaves the resident in the foreground.
        assert_eq!(paste_target(0x200, 0x100, 0x200), None);
    }

    #[test]
    fn a_switch_reports_only_a_real_change() {
        let mut on = true;
        assert!(!switch(&mut on, true));
        assert!(switch(&mut on, false));
        assert!(!switch(&mut on, false));
        assert!(switch(&mut on, true));
        assert!(on);
    }

    #[test]
    fn hover_follows_the_row_under_a_still_pointer() {
        let l = Layout::new(1.0, 20, 0, false);
        let origin = (1000, 200);
        // Pointer over the list's first row (after the 26 DIP label), then the same
        // pointer after the list scrolled by two rows.
        let pointer = (origin.0 + 200, origin.1 + l.list_top() as i32 + 30);
        assert_eq!(hover_at(&l, pointer, origin, 0.0), Some(0));
        assert_eq!(hover_at(&l, pointer, origin, 2.0 * 44.0), Some(2));
        // Outside the window: none.
        assert_eq!(hover_at(&l, (5, 5), origin, 0.0), None);
    }
}
