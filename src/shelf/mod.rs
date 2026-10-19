//! The Shelf (spec §3): Item list, drawing, position, motion and mouse.
//!
//! All state lives on the UI thread in `SHELF`. Every function here must be
//! called on that thread. `with` is short and non-reentrant: a nested call is
//! skipped. Calls that can send the window a message the Shelf itself handles
//! (`SetWindowPos`, `ShowWindow`, `ReleaseCapture`, `TrackPopupMenu`, drag
//! loops, ...) are made outside it. `UpdateLayeredWindow` inside it only sends
//! `WM_WINDOWPOS*`, which go to `DefWindowProcW`.
//!
//! Hooks for later slices (no-ops or plain setters until they land):
//! `add_top`, `move_to_top`, `remove`, `hide_if_empty`, `set_hot`,
//! `set_in_own_drag` / `in_own_drag`, `with_model`, `is_shown`, and
//! `takeout::start`.

pub mod anim;
pub mod layout;
pub mod model;
mod open;
pub mod probe;
mod render;
mod takeout;
mod tiles;
mod window;

use std::cell::RefCell;
use std::path::PathBuf;

use windows::core::Result;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, KillTimer, SetTimer, SetWindowPos, ShowWindow, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE,
};

pub use window::handle;

use crate::log::{log, now_ms};
use anim::{Anim, AnimKind};
use layout::{Layout, WorkArea, SLIDE_IN_DIP, SLIDE_OUT_DIP};
use model::{ItemId, Kind, Model};
use crate::thumbs;
use render::{Renderer, View};

const T_ANIM: usize = 1;

/// A left-button press on a row, kept until release or until a drag starts.
struct Press {
    at: (i32, i32),
    id: ItemId,
    /// Pressed an already selected row: the plain click happens on release.
    deferred: bool,
}

struct Shelf {
    hwnd: HWND,
    model: Model,
    renderer: Renderer,
    layout: Layout,
    /// Shown, or fading out.
    visible: bool,
    anim: Option<Anim>,
    /// Last presented position and alpha.
    pos: POINT,
    alpha: u8,
    hover: Option<ItemId>,
    scroll: f32,
    hot: bool,
    in_own_drag: bool,
    press: Option<Press>,
    /// The left button went down on "Clear all".
    clear_armed: bool,
    tracking_leave: bool,
}

thread_local! {
    static SHELF: RefCell<Option<Shelf>> = const { RefCell::new(None) };
}

/// Short, non-reentrant access to the Shelf. `None` before `create`, after
/// `destroy`, or on a re-entrant call (logged and skipped).
fn with<R>(f: impl FnOnce(&mut Shelf) -> R) -> Option<R> {
    SHELF.with(|c| match c.try_borrow_mut() {
        Ok(mut g) => g.as_mut().map(f),
        Err(_) => {
            log("shelf busy: re-entrant call skipped");
            None
        }
    })
}

/// DPI and work area of the monitor under the cursor.
fn cursor_monitor() -> (u32, WorkArea) {
    unsafe {
        let mut c = POINT::default();
        let _ = GetCursorPos(&mut c);
        let mon = MonitorFromPoint(c, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let r = mi.rcWork;
        (dev_dpi().unwrap_or(dx), WorkArea { left: r.left, top: r.top, right: r.right, bottom: r.bottom })
    }
}

/// Debug builds only: `TASCA_DEV_DPI=144` renders at 150 % whatever the
/// monitor says (screenshots on a 100 % screen). Release builds ignore it.
fn dev_dpi() -> Option<u32> {
    #[cfg(debug_assertions)]
    if let Some(d) = std::env::var("TASCA_DEV_DPI").ok().and_then(|v| v.parse().ok()) {
        return Some(d);
    }
    None
}

/// Debug builds only: `tasca.exe [--hidden] --seed <path>...` parks these as
/// References at startup, in order (the last ends on top). `a|b|c` is one
/// Stack of three files.
#[cfg(debug_assertions)]
fn seed_args(args: impl IntoIterator<Item = String>) -> Vec<Vec<PathBuf>> {
    args.into_iter()
        .skip_while(|a| a != "--seed")
        .skip(1)
        .map(|a| a.split('|').filter(|p| !p.is_empty()).map(PathBuf::from).collect::<Vec<_>>())
        .filter(|files| !files.is_empty())
        .collect()
}

impl Shelf {
    fn render(&mut self) {
        self.layout.rows = self.model.len();
        self.scroll = self.layout.clamp_scroll(self.scroll);
        // Whatever changed the list, forget pointer state about Items that left.
        if self.hover.is_some_and(|id| self.model.get(id).is_none()) {
            self.hover = None;
        }
        if self.press.as_ref().is_some_and(|p| self.model.get(p.id).is_none()) {
            self.press = None;
        }
        // Free thumbnails of Items that left and of other DPIs.
        let model = &self.model;
        thumbs::retain((self.layout.scale * 96.0).round() as u32, |id| model.get(id).is_some());
        self.renderer.tiles_mut().sync();
        let view = View { layout: &self.layout, model: &self.model, hover: self.hover, scroll: self.scroll, hot: self.hot };
        if let Err(e) = self.renderer.render(&view) {
            log(format!("render failed: {e:?}"));
        }
    }

    fn present(&mut self, pos: POINT, alpha: u8) {
        self.pos = pos;
        self.alpha = alpha;
        if let Err(e) = self.renderer.present(self.hwnd, pos, alpha) {
            log(format!("UpdateLayeredWindow failed: {e:?}"));
        }
    }

    /// Re-render after a change and keep the panel centred (the animation, if
    /// any, owns x).
    fn refresh(&mut self) {
        self.render();
        if !self.visible {
            return;
        }
        let (x, y) = self.layout.position();
        let x = if self.anim.is_some() { self.pos.x } else { x };
        self.present(POINT { x, y }, self.alpha);
    }

    /// "Clear all": the Shelf stays, empty (it hides only on Caps+D or after
    /// the last Take out).
    fn clear_all(&mut self) {
        self.model.clear();
        self.refresh();
    }

    /// Forget hover, press and the drag-over outline (on show and hide).
    fn reset_pointer(&mut self) {
        self.hot = false;
        self.hover = None;
        self.press = None;
        self.clear_armed = false;
        // A hidden window may never get its WM_MOUSELEAVE: re-arm on the next move.
        self.tracking_leave = false;
    }

    fn file_jobs(&self, only: Option<&[ItemId]>) -> Vec<(ItemId, Vec<PathBuf>)> {
        self.model
            .items()
            .iter()
            .filter(|i| only.is_none_or(|ids| ids.contains(&i.id)))
            .map(|i| (i.id, i.files.clone()))
            .collect()
    }

    fn showing(&self) -> bool {
        self.visible && !matches!(self.anim, Some(Anim { kind: AnimKind::Out, .. }))
    }
}

/// Build the Shelf for the (hidden) window and pre-render once, so the first
/// Caps+D doesn't pay for the D2D/DirectWrite warm-up.
pub fn create(hwnd: HWND) -> Result<()> {
    let t = now_ms();
    let (dpi, work) = cursor_monitor();
    let mut s = Shelf {
        hwnd,
        model: Model::new(),
        renderer: Renderer::new()?,
        layout: Layout::new(dpi, work, 0),
        visible: false,
        anim: None,
        pos: POINT::default(),
        alpha: 255,
        hover: None,
        scroll: 0.0,
        hot: false,
        in_own_drag: false,
        press: None,
        clear_armed: false,
        tracking_leave: false,
    };
    #[cfg(debug_assertions)]
    for files in seed_args(std::env::args().skip(1)) {
        s.model.add_top(files, Kind::Reference);
    }
    s.render();
    let jobs = s.file_jobs(None);
    SHELF.with(|c| *c.borrow_mut() = Some(s));
    files_need_work(hwnd, jobs, false);
    log(format!("shelf pre-rendered while hidden in {:.1} ms (dpi {dpi})", now_ms() - t));
    Ok(())
}

/// The one place where background work on Items' files starts: when Items
/// appear (`recheck` false) and on every show (`recheck` true): the probe
/// (exists, folder, size) and the thumbnails (cached ones are skipped).
fn files_need_work(hwnd: HWND, jobs: Vec<(ItemId, Vec<PathBuf>)>, recheck: bool) {
    let picks = with(|s| {
        let items = jobs.iter().filter_map(|(id, _)| s.model.get(*id)).map(|i| (i.id, i.files[0].clone(), i.is_image()));
        (s.layout.scale, items.collect::<Vec<_>>())
    });
    if let Some((scale, items)) = picks {
        thumbs::request(hwnd, scale, items);
    }
    probe::request(hwnd, jobs, recheck);
}

/// `WM_PROBED`: store the facts, re-render if any changed. Drained inside
/// `with`, so a skipped (re-entrant) call leaves them queued for the next one.
fn on_probed() {
    with(|s| {
        let mut changed = false;
        for (id, facts) in probe::take() {
            changed |= s.model.set_facts(id, facts);
        }
        if changed {
            s.refresh();
        }
    });
}

/// `WM_THUMB_READY`: a thumbnail landed in the cache. Hidden, the next show
/// renders it anyway.
fn on_thumb_ready() {
    with(|s| {
        if s.visible {
            log(format!("thumbnail arrived at {:.1} ms", now_ms()));
            s.refresh();
        }
    });
}

pub fn destroy() {
    SHELF.with(|c| c.borrow_mut().take());
}

/// Caps+D: show or hide. Ignored while a Take out is in progress (spec §2).
pub fn toggle() {
    match with(|s| (s.in_own_drag, s.showing())) {
        Some((true, _)) => log("toggle ignored: a Take out is in progress"),
        Some((false, true)) => hide(),
        Some((false, false)) => show(),
        None => {}
    }
}

/// Slide the Shelf in on the cursor's monitor. Already showing: only re-check
/// the files.
pub fn show() {
    let started = with(|s| {
        let jobs = s.file_jobs(None);
        if s.showing() {
            return (s.hwnd, jobs, false);
        }
        let (dpi, work) = cursor_monitor();
        s.layout = Layout::new(dpi, work, s.model.len());
        s.visible = true;
        s.reset_pointer();
        s.render();
        let (x, y) = s.layout.position();
        let a = Anim::slide_in(now_ms(), x, s.layout.dip_to_px(SLIDE_IN_DIP));
        let first = a.sample(now_ms());
        s.anim = Some(a);
        s.present(POINT { x: first.x, y }, first.alpha);
        log(format!("show at ({x},{y}) dpi {dpi}, work {work:?}, {} items", s.model.len()));
        (s.hwnd, jobs, true)
    });
    let Some((hwnd, jobs, started)) = started else { return };
    if started {
        unsafe {
            // Hit-testable from the first frame; never activated.
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            SetTimer(Some(hwnd), T_ANIM, anim::TIMER_MS, None);
        }
    }
    files_need_work(hwnd, jobs, true);
}

/// Fade the Shelf out, then `SW_HIDE`.
pub fn hide() {
    let hwnd = with(|s| {
        if !s.showing() {
            return None;
        }
        s.anim = Some(Anim::fade_out(now_ms(), s.pos.x, s.layout.dip_to_px(SLIDE_OUT_DIP), s.alpha));
        s.reset_pointer();
        log("hide");
        Some(s.hwnd)
    })
    .flatten();
    if let Some(hwnd) = hwnd {
        unsafe { SetTimer(Some(hwnd), T_ANIM, anim::TIMER_MS, None) };
    }
}

/// One animation frame (`WM_TIMER`): move and fade the rendered bitmap.
fn tick() {
    let done = with(|s| {
        let Some(a) = s.anim else { return Some((s.hwnd, None)) };
        let f = a.sample(now_ms());
        let (_, y) = s.layout.position();
        s.present(POINT { x: f.x, y }, f.alpha);
        if !f.done {
            return None;
        }
        s.anim = None;
        if a.kind == AnimKind::Out {
            s.visible = false;
        }
        Some((s.hwnd, Some(a.kind)))
    })
    .flatten();
    if let Some((hwnd, kind)) = done {
        unsafe {
            let _ = KillTimer(Some(hwnd), T_ANIM);
            if kind == Some(AnimKind::Out) {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

// ---------------------------------------------------------------- hooks for TASCA-14..17

/// Add an Item on top (a Park). Scrolls to the top so it is seen, re-renders,
/// and probes its files off the UI thread. `files` must not be empty.
pub fn add_top(files: Vec<PathBuf>, kind: Kind) -> Option<ItemId> {
    let (hwnd, id, jobs) = with(|s| {
        let id = s.model.add_top(files, kind);
        s.scroll = 0.0;
        s.refresh();
        (s.hwnd, id, s.file_jobs(Some(&[id])))
    })?;
    files_need_work(hwnd, jobs, false);
    Some(id)
}

/// Move an existing Item to the top (duplicate Park, spec §4).
pub fn move_to_top(id: ItemId) -> bool {
    with(|s| {
        let moved = s.model.move_to_top(id);
        if moved {
            s.scroll = 0.0;
            s.refresh();
        }
        moved
    })
    .unwrap_or(false)
}

/// Remove Items by id (Take out success, failed Materialize, menu Remove).
pub fn remove(ids: &[ItemId]) -> usize {
    with(|s| {
        let n = s.model.remove(ids);
        if n > 0 {
            s.refresh();
        }
        n
    })
    .unwrap_or(0)
}

/// Hide if the Shelf is empty (after the last Item is Taken out).
#[allow(dead_code)] // TASCA-15
pub fn hide_if_empty() {
    if with(|s| s.model.is_empty()) == Some(true) {
        hide();
    }
}

/// A drag is over the Shelf: draw the blue outline (TASCA-14's drop target).
pub fn set_hot(hot: bool) {
    with(|s| {
        if s.hot != hot {
            s.hot = hot;
            s.refresh();
        }
    });
}

/// The Shelf's own Take out is running (TASCA-15 sets it; TASCA-14 answers
/// `DROPEFFECT_NONE` while it is set; toggles are ignored).
#[allow(dead_code)] // TASCA-15
pub fn set_in_own_drag(on: bool) {
    with(|s| s.in_own_drag = on);
}

pub fn in_own_drag() -> bool {
    with(|s| s.in_own_drag).unwrap_or(false)
}

/// Read access to the Item list (duplicate check, building a data object).
pub fn with_model<R>(f: impl FnOnce(&Model) -> R) -> Option<R> {
    with(|s| f(&s.model))
}

/// Shown and not fading out.
#[allow(dead_code)] // TASCA-15
pub fn is_shown() -> bool {
    with(|s| s.showing()).unwrap_or(false)
}

#[cfg(all(test, debug_assertions))]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn seed_paths_follow_the_flag() {
        assert!(seed_args(args(&["--hidden"])).is_empty());
        assert!(seed_args(args(&["--seed"])).is_empty());
        assert_eq!(
            seed_args(args(&["--hidden", "--seed", r"C:\a.jpg", r"C:\b.pdf"])),
            vec![vec![PathBuf::from(r"C:\a.jpg")], vec![PathBuf::from(r"C:\b.pdf")]]
        );
    }

    #[test]
    fn a_bar_joined_seed_is_one_stack() {
        assert_eq!(
            seed_args(args(&["--seed", r"C:\x\1.jpg|C:\x\2.jpg||C:\x\3.txt"])),
            vec![vec![PathBuf::from(r"C:\x\1.jpg"), PathBuf::from(r"C:\x\2.jpg"), PathBuf::from(r"C:\x\3.txt")]]
        );
    }
}
