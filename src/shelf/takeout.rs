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
use std::path::Path;

use windows::core::{implement, ComObject, BOOL, HRESULT, HSTRING};
use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HWND, POINT, S_OK};
use windows::Win32::Storage::FileSystem::{GetFileAttributesW, INVALID_FILE_ATTRIBUTES};
use windows::Win32::System::Ole::{
    IDropSource, IDropSource_Impl, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
use windows::Win32::UI::Shell::{SHDoDragDrop, CFSTR_LOGICALPERFORMEDDROPEFFECT};
use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GetCursorPos, WindowFromPoint, GA_ROOT};

use super::model::{Facts, ItemId};
use crate::log::log;
use rule::{Ending, Outcome};

pub fn start(hwnd: HWND, ids: Vec<ItemId>) {
    let Some(candidates) = super::with(|s| ids.iter().filter_map(|&id| s.model.get(id).cloned()).collect::<Vec<_>>())
    else {
        return;
    };
    // Re-check on the disk right before the drag (spec §3): a Missing Item
    // can't be Taken out.
    let (items, missing) = rule::split_missing(&candidates, exists);
    if !missing.is_empty() {
        log(format!("take out: {:?} missing, not dragged", missing.iter().map(|i| i.id).collect::<Vec<_>>()));
        super::with(|s| {
            for i in &missing {
                s.model.set_facts(i.id, Facts { missing: true, ..i.facts.unwrap_or_default() });
            }
            s.refresh();
        });
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

    let source = ComObject::new(DropSource { hwnd, ending: Cell::new(Ending::Cancel) });
    let r = {
        let _own = OwnDrag::start();
        unsafe { SHDoDragDrop(Some(hwnd), &obj, &source.to_interface::<IDropSource>(), DROPEFFECT_COPY | DROPEFFECT_MOVE) }
    };
    if let Err(e) = &r {
        log(format!("take out: SHDoDragDrop failed: {e:?}"));
    }
    let o = Outcome {
        ending: source.ending.get(),
        effect: r.unwrap_or(DROPEFFECT_NONE),
        logical: data::read_dword(&obj, CFSTR_LOGICALPERFORMEDDROPEFFECT).map(DROPEFFECT),
    };
    let mut gone = None;
    let success = rule::succeeded(&o, || *gone.insert(rule::reference_gone(&items, exists)));
    log(format!(
        "take out of {ids:?} via {via:?}: ended {:?}, effect {}, logical {}, Reference gone {} => {}",
        o.ending,
        effect_name(o.effect),
        o.logical.map_or("(absent)".into(), effect_name),
        gone.map_or("(not checked)".into(), |g| g.to_string()),
        if success { "Item(s) leave" } else { "kept" },
    ));

    // One render: the cursor is elsewhere now (forget hover and press), and
    // the Items leave on success.
    super::with(|s| {
        s.reset_pointer();
        if success {
            s.model.remove(&ids);
        }
        s.refresh();
    });
    // The Shelf hides only after the last Item is Taken out (TASCA-8).
    if success {
        super::hide_if_empty();
    }
}

/// One attribute lookup, no handle opened (cheaper than `Path::exists`).
fn exists(p: &Path) -> bool {
    unsafe { GetFileAttributesW(&HSTRING::from(p)) != INVALID_FILE_ATTRIBUTES }
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

/// Records how the drag ended, and for a drop whether it landed on the Shelf.
#[implement(IDropSource)]
struct DropSource {
    hwnd: HWND,
    ending: Cell<Ending>,
}

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, esc: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        // The Shelf is never activated; like the spike, also look at the
        // physical Esc in case OLE does not see the keyboard.
        if esc.as_bool() || unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) } < 0 {
            self.ending.set(Ending::Cancel);
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            self.ending.set(Ending::Drop { on_shelf: cursor_on(self.hwnd) });
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}
