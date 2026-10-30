//! Snippets: text kept on purpose, apart from the History.
//!
//! The list is a dark panel (the History's), opened from the tray. Creating
//! and editing uses a small Win32 window. The bytes live in
//! `%LOCALAPPDATA%\Tasca\snippets\`, written on a worker thread.
//!
//! - `state.rs`  the pure machine
//! - `model.rs`  the Snippet type
//! - `store.rs`  the file
//! - `text.rs`   titles, subtitles, the body check
//! - `layout.rs` geometry and hit-testing
//! - `draw.rs`   the panel
//! - `window.rs` the window
//! - `menu.rs`   the row menu
//! - `edit.rs`   the editor
//! - `copy.rs`   writing the clipboard

mod copy;
mod draw;
mod edit;
mod layout;
mod menu;
mod model;
mod state;
mod store;
mod text;
mod window;

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SetForegroundWindow};

use crate::log::log;
use crate::proto::{WM_SHOW_SNIPPETS, WM_SNIPPETS_LOADED};
use layout::Hit;
use model::Snippet;
use state::{Effect, Event, Machine};
use store::Store;

struct Shell {
    window: HWND,
    machine: Machine,
    scale: f32,
    pointer: (i32, i32),
    gap: i32,
    work: window::Work,
    hover: Option<usize>,
    scroll: f32,
    followed: usize,
    store: Option<Store>,
}

impl Shell {
    fn layout(&self) -> layout::Layout {
        layout::Layout::new(self.scale, self.machine.snippets().len())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

static LOADED: Mutex<Option<Vec<Snippet>>> = Mutex::new(None);

thread_local! {
    static SHELL: RefCell<Option<Shell>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Shell) -> R) -> Option<R> {
    SHELL.with(|c| match c.try_borrow_mut() {
        Ok(mut g) => g.as_mut().map(f),
        Err(_) => {
            log("snippets busy: re-entrant call skipped");
            None
        }
    })
}

/// Resident startup: the window and a load of the saved list.
pub fn create(resident: HWND) -> windows::core::Result<()> {
    let window = window::create()?;
    let store = start_store(resident);
    let memory_only = store.is_none();
    SHELL.with(|c| {
        *c.borrow_mut() = Some(Shell {
            window,
            machine: Machine::default(),
            scale: 1.0,
            pointer: (0, 0),
            gap: 0,
            work: window::Work::default(),
            hover: None,
            scroll: 0.0,
            followed: 0,
            store,
        })
    });
    if let Err(e) = draw::init() {
        log(format!("snippets: renderer not ready yet: {e}"));
    }
    if memory_only {
        // No `%LOCALAPPDATA%`: the list lives in this process only.
        dispatch(Event::Loaded(Vec::new()));
    }
    Ok(())
}

fn start_store(resident: HWND) -> Option<Store> {
    let store = Store::spawn(store::default_dir()?);
    let resident = resident.0 as isize;
    store.load(move |records| {
        let snippets: Vec<Snippet> = records
            .into_iter()
            .filter_map(Snippet::from_record)
            .collect();
        *LOADED.lock().unwrap_or_else(|e| e.into_inner()) = Some(snippets);
        let hwnd = HWND(resident as *mut _);
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_SNIPPETS_LOADED, WPARAM(0), LPARAM(0));
        }
    });
    Some(store)
}

pub fn destroy() {
    finish_load();
    draw::destroy();
    if let Some(shell) = SHELL.with(|c| c.borrow_mut().take()) {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(shell.window);
        }
    }
}

/// The resident's hook. `None` = not ours.
pub fn handle(msg: u32) -> Option<LRESULT> {
    match msg {
        WM_SNIPPETS_LOADED => on_loaded(),
        WM_SHOW_SNIPPETS => show_or_hide(),
        _ => return None,
    }
    Some(LRESULT(0))
}

/// A Clip's text, from the History's "Save as snippet".
pub fn save_text(text: String) {
    if SHELL.with(|c| c.borrow().is_none()) {
        log("snippets: not running, clip not saved");
        return;
    }
    dispatch(Event::Import(text));
}

fn finish_load() {
    if with(|s| !s.machine.is_loaded() && s.store.is_some()) != Some(true) {
        return;
    }
    with(|s| s.store.as_ref().map(Store::flush));
    on_loaded();
}

fn on_loaded() {
    let Some(snippets) = LOADED.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    log(format!("snippets: loaded {}", snippets.len()));
    dispatch(Event::Loaded(snippets));
}

fn save() {
    with(|s| {
        if let Some(store) = &s.store {
            store.save(
                s.machine
                    .snippets()
                    .iter()
                    .map(Snippet::to_record)
                    .collect(),
            );
        }
    });
}

fn show_or_hide() {
    if with(|s| s.machine.is_open()) == Some(true) {
        dispatch(Event::Deactivated);
    } else {
        open();
    }
}

fn open() {
    let Some((rows, already)) = with(|s| (s.machine.snippets().len(), s.machine.is_open())) else {
        return;
    };
    if already {
        return;
    }
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
    log(format!(
        "snippets: open at {pointer:?}, dpi {dpi}, {rows} snippets"
    ));
    dispatch(Event::Opened);
}

pub(crate) fn dispatch(event: Event) {
    let now = now_ms();
    let Some(effects) = with(|s| s.machine.step(event, now)) else {
        return;
    };
    for effect in effects {
        apply(effect);
    }
}

fn apply(effect: Effect) {
    let Some(window) = with(|s| s.window) else {
        return;
    };
    match effect {
        Effect::Redraw => redraw(),
        Effect::Save => save(),
        Effect::Close => window::hide(window),
        Effect::Copy(text) => copy::start(window, text),
        Effect::Refused(why) => log(format!("snippets: ignored ({why:?})")),
        Effect::Editor(draft) => {
            let id = draft.id;
            let saved = edit::ask(window, &draft.title, &draft.body);
            if with(|s| s.machine.is_open()) == Some(true) {
                let _ = unsafe { SetForegroundWindow(window) };
            }
            if let Some((title, body)) = saved {
                dispatch(Event::Commit { id, title, body });
            } else if with(|s| s.machine.is_open()) == Some(true) {
                redraw();
            }
        }
    }
}

fn redraw() {
    let Some((window, view, origin)) = with(|s| {
        let layout = s.layout();
        let selected = s.machine.selected();
        if selected != s.followed {
            s.followed = selected;
            s.scroll = layout.scroll_to_show(s.scroll, selected);
        }
        s.scroll = layout.clamp_scroll(s.scroll);
        let (origin, _) = window::frame(s.pointer, s.gap, &layout, s.work);
        s.hover = hover_at(&layout, window::cursor(), origin, s.scroll);
        (s.window, view_of(s), origin)
    }) else {
        return;
    };
    if !window::show(window, &view, origin) {
        dispatch(Event::Deactivated);
    }
}

fn hover_at(
    layout: &layout::Layout,
    pointer: (i32, i32),
    origin: (i32, i32),
    scroll: f32,
) -> Option<usize> {
    match layout.hit(pointer.0 - origin.0, pointer.1 - origin.1, scroll) {
        Hit::Row(i) => Some(i),
        _ => None,
    }
}

fn view_of(s: &Shell) -> draw::View {
    let snippets = s.machine.snippets();
    let visible = s.layout().visible_rows(s.scroll);
    let rows = snippets[visible.clone()]
        .iter()
        .map(|sn| draw::RowView {
            title: sn.title.clone(),
            sub: text::subtitle(&sn.title, &sn.body),
        })
        .collect();
    draw::View {
        layout: s.layout(),
        rows,
        first: visible.start,
        selected: (!snippets.is_empty()).then(|| s.machine.selected()),
        hover: s.hover,
        scroll: s.scroll,
        count: text::count_label(snippets.len()),
    }
}

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

pub(crate) fn on_mouse_leave() {
    if with(|s| s.hover.take().is_some() && s.machine.is_open()) == Some(true) {
        redraw();
    }
}

pub(crate) fn on_click(x: i32, y: i32) {
    let hit = with(|s| {
        if s.machine.is_open() {
            Some(s.layout().hit(x, y, s.scroll))
        } else {
            None
        }
    });
    match hit {
        Some(Some(Hit::Row(row))) => dispatch(Event::Click { row }),
        Some(Some(Hit::New)) => dispatch(Event::New),
        _ => {}
    }
}

pub(crate) fn on_right_click(x: i32, y: i32) {
    let target = with(|s| match s.layout().hit(x, y, s.scroll) {
        Hit::Row(i) if s.machine.is_open() => {
            s.machine.snippets().get(i).map(|sn| (s.window, sn.id))
        }
        _ => None,
    });
    let Some(Some((window, id))) = target else {
        return;
    };
    dispatch(Event::Select(id));
    if let Some(action) = menu::pick(window, x, y) {
        dispatch(Event::Action { id, action });
    }
}

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
