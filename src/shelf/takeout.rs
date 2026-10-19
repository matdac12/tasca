//! Take out (spec §5). `window.rs` calls `start` on the UI thread once a press
//! on a row has moved past `SM_CXDRAG`/`SM_CYDRAG`, after releasing the mouse
//! capture. `ids` is the pressed row, or the selection if the row is part of
//! it, top row first.
//!
//! - `rule`: the pure rules (Missing filter, data-object choice, success).
//! - `data`: the shell data object and reading targets' write-backs.
//!
//! `SHDoDragDrop` runs a modal loop on the UI thread, so it is called outside
//! `shelf::with`. Tasca never deletes, moves or renames a file because of the
//! effect: Explorer moves the file itself, and Materialized files stay in
//! `%TEMP%\Tasca` because targets read them after the drop.

mod data;
mod rule;

use std::cell::Cell;

use windows::core::{implement, ComObject, BOOL, HRESULT};
use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HWND, POINT, S_OK};
use windows::Win32::System::Ole::{
    IDropSource, IDropSource_Impl, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
use windows::Win32::UI::Shell::{SHDoDragDrop, CFSTR_LOGICALPERFORMEDDROPEFFECT};
use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GetCursorPos, WindowFromPoint, GA_ROOT};

use super::model::{Item, ItemId};
use super::probe;
use crate::log::log;
use rule::{Ending, Outcome};

pub fn start(hwnd: HWND, ids: Vec<ItemId>) {
    // Re-check on the disk right before the drag (spec §3): a Missing Item
    // can't be Taken out.
    let Some((items, missing)) = super::with(|s| {
        let items: Vec<&Item> = ids.iter().filter_map(|&id| s.model.get(id)).collect();
        let (take, missing) = rule::split_missing(&items, |p| p.exists());
        (take.into_iter().cloned().collect::<Vec<_>>(), s.file_jobs(Some(&missing)))
    }) else {
        return;
    };
    if !missing.is_empty() {
        log(format!("take out: {:?} missing, not dragged", missing.iter().map(|(id, _)| id).collect::<Vec<_>>()));
        // Probe them again so the rows show "Missing".
        probe::request(hwnd, missing, false);
    }
    if items.is_empty() {
        return;
    }
    let ids: Vec<ItemId> = items.iter().map(|i| i.id).collect();
    let paths: Vec<_> = items.iter().flat_map(|i| i.files.iter().cloned()).collect();
    let (obj, via) = match data::build(&paths) {
        Ok(o) => o,
        Err(e) => {
            log(format!("take out of {ids:?}: no data object: {e:?}"));
            return;
        }
    };

    let source = ComObject::new(DropSource { hwnd, ending: Cell::new(Ending::Cancel), on_shelf: Cell::new(false) });
    let r = {
        let _own = OwnDrag::start();
        unsafe { SHDoDragDrop(Some(hwnd), &obj, &source.to_interface::<IDropSource>(), DROPEFFECT_COPY | DROPEFFECT_MOVE) }
    };
    if let Err(e) = &r {
        log(format!("take out: SHDoDragDrop failed: {e:?}"));
    }
    let o = Outcome {
        ending: source.ending.get(),
        on_shelf: source.on_shelf.get(),
        effect: r.unwrap_or(DROPEFFECT_NONE),
        logical: data::read_dword(&obj, CFSTR_LOGICALPERFORMEDDROPEFFECT).map(DROPEFFECT),
        reference_gone: rule::reference_gone(&items, |p| p.exists()),
    };
    let success = rule::succeeded(&o);
    log(format!(
        "take out of {ids:?} via {via:?}: ended {:?}, on the Shelf {}, effect {}, logical {}, Reference gone {} => {}",
        o.ending,
        o.on_shelf,
        effect_name(o.effect),
        o.logical.map_or("(absent)".into(), effect_name),
        o.reference_gone,
        if success { "Item(s) leave" } else { "kept" },
    ));

    // The cursor is elsewhere now: forget hover and press.
    super::with(|s| {
        s.reset_pointer();
        s.refresh();
    });
    if success {
        super::remove(&ids);
        // The Shelf hides only after the last Item is Taken out (TASCA-8).
        super::hide_if_empty();
    }
}

fn effect_name(e: DROPEFFECT) -> String {
    let names: Vec<_> = [(DROPEFFECT_COPY, "COPY"), (DROPEFFECT_MOVE, "MOVE"), (DROPEFFECT_LINK, "LINK")]
        .iter()
        .filter(|(bit, _)| e.0 & bit.0 != 0)
        .map(|(_, n)| *n)
        .collect();
    if names.is_empty() { "NONE".into() } else { names.join("|") }
}

/// `in_own_drag` for the drag's duration, cleared on every path (panics too):
/// the drop target answers `NONE` and toggles are ignored while it is set.
struct OwnDrag;

impl OwnDrag {
    fn start() -> Self {
        super::set_in_own_drag(true);
        OwnDrag
    }
}

impl Drop for OwnDrag {
    fn drop(&mut self) {
        super::set_in_own_drag(false);
    }
}

/// The cursor is over the Shelf window (where OLE would deliver the drop:
/// `WindowFromPoint` skips the layered window's fully transparent pixels).
fn cursor_on(hwnd: HWND) -> bool {
    unsafe {
        let mut pt = POINT::default();
        GetCursorPos(&mut pt).is_ok() && GetAncestor(WindowFromPoint(pt), GA_ROOT) == hwnd
    }
}

/// Records how the drag ended, and whether the drop landed on the Shelf.
#[implement(IDropSource)]
struct DropSource {
    hwnd: HWND,
    ending: Cell<Ending>,
    on_shelf: Cell<bool>,
}

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, esc: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        // The Shelf is never activated; like the spike, also look at the
        // physical Esc in case OLE does not see the keyboard.
        if esc.as_bool() || unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) } < 0 {
            self.ending.set(Ending::Cancel);
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            self.ending.set(Ending::Drop);
            self.on_shelf.set(cursor_on(self.hwnd));
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}
