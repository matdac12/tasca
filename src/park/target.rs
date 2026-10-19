//! The Shelf's `IDropTarget` (spec §4). Every method forwards to the shell's
//! `IDropTargetHelper` first (drag image), with screen points. `DragEnter`
//! only asks `QueryGetData`; all reading happens in `Drop`, through `READERS`,
//! and anything slow goes on to a worker (`job`).
//!
//! These calls arrive on the UI thread from the OLE drag loop. They use only
//! the Shelf's non-blocking hooks, never inside `shelf::with`.

use std::cell::Cell;

use windows::core::{implement, Ref, Result};
use windows::Win32::Foundation::{HWND, POINT, POINTL};
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::{IDropTarget, IDropTarget_Impl, DROPEFFECT, DROPEFFECT_NONE};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::IDropTargetHelper;

use super::{async_hdrop, formats, hdrop, land, png, rules, text, url, virtual_files, Park};
use crate::log::log;
use crate::shelf;

/// One way to turn a data object into a Park.
pub struct Reader {
    pub name: &'static str,
    /// Offered? `QueryGetData` only (called during the drag).
    pub offered: fn(&IDataObject) -> bool,
    /// Read it, in `Drop`. `None` = nothing usable; the next reader is tried.
    pub park: fn(&IDataObject) -> Option<Park>,
}

fn hdrop_now(obj: &IDataObject) -> Option<Park> {
    hdrop::park(obj).map(Park::Ready)
}

/// The readers, in the spec's order (rules 1–6). A format with no reader is
/// answered `NONE` (`Shell IDList Array` alone, for one), so nothing
/// half-parks.
pub const READERS: &[Reader] = &[
    Reader { name: "CF_HDROP", offered: hdrop::offered, park: hdrop_now },
    Reader { name: "async CF_HDROP", offered: hdrop::offered, park: async_hdrop::park },
    Reader { name: "FileGroupDescriptor", offered: virtual_files::offered, park: virtual_files::park },
    Reader { name: "UniformResourceLocator", offered: url::offered, park: url::park },
    Reader { name: "text", offered: text::offered, park: text::park },
    Reader { name: "PNG", offered: png::offered, park: png::park },
];

/// Some reader handles what the object offers.
pub fn supported(obj: &IDataObject) -> bool {
    READERS.iter().any(|r| (r.offered)(obj))
}

/// The first reader that is offered and reads successfully makes the Park.
pub fn read(obj: &IDataObject) -> Option<Park> {
    READERS.iter().filter(|r| (r.offered)(obj)).find_map(|r| {
        let p = (r.park)(obj)?;
        log(format!("drop: {}, {}", r.name, p.describe()));
        Some(p)
    })
}

fn effect_name(e: DROPEFFECT) -> String {
    let s = formats::flag_names(e.0, &[(1, "COPY"), (2, "MOVE"), (4, "LINK")]);
    if s.is_empty() { "NONE".into() } else { s }
}

#[implement(IDropTarget)]
pub struct DropTarget {
    hwnd: HWND,
    helper: Option<IDropTargetHelper>,
    /// What `DragEnter` found (`DragOver` gets no data object).
    supported: Cell<bool>,
}

impl DropTarget {
    pub fn new(hwnd: HWND, helper: Option<IDropTargetHelper>) -> Self {
        Self { hwnd, helper, supported: Cell::new(false) }
    }

    /// Our answer, and the drag-over outline to match it.
    fn answer(&self, allowed: DROPEFFECT) -> DROPEFFECT {
        let e = rules::choose_effect(allowed, self.supported.get(), shelf::in_own_drag());
        shelf::set_hot(e != DROPEFFECT_NONE);
        e
    }

    /// The drag left or ended: forget it and drop the outline.
    fn reset(&self) {
        self.supported.set(false);
        shelf::set_hot(false);
    }
}

fn screen(pt: &POINTL) -> POINT {
    POINT { x: pt.x, y: pt.y }
}

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(&self, obj: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, eff: *mut DROPEFFECT) -> Result<()> {
        let allowed = unsafe { *eff };
        if let Some(h) = &self.helper {
            let _ = unsafe { h.DragEnter(self.hwnd, obj.as_ref(), &screen(pt), allowed) };
        }
        self.supported.set(obj.as_ref().is_some_and(supported));
        let e = self.answer(allowed);
        unsafe { *eff = e };
        let offered = obj.as_ref().map_or_else(|| "(no data object)".into(), formats::list);
        log(format!("drag enter: allowed {}, answer {}, offers {offered}", effect_name(allowed), effect_name(e)));
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, eff: *mut DROPEFFECT) -> Result<()> {
        let allowed = unsafe { *eff };
        if let Some(h) = &self.helper {
            let _ = unsafe { h.DragOver(&screen(pt), allowed) };
        }
        unsafe { *eff = self.answer(allowed) };
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        if let Some(h) = &self.helper {
            let _ = unsafe { h.DragLeave() };
        }
        self.reset();
        Ok(())
    }

    fn Drop(&self, obj: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, eff: *mut DROPEFFECT) -> Result<()> {
        let allowed = unsafe { *eff };
        if let Some(h) = &self.helper {
            let _ = unsafe { h.Drop(obj.as_ref(), &screen(pt), allowed) };
        }
        self.reset();
        let own = shelf::in_own_drag();
        // Read (and copy) only if the source accepts the effect we will answer.
        let parked = match obj.as_ref() {
            Some(o) if rules::choose_effect(allowed, true, own) != DROPEFFECT_NONE => read(o),
            _ => None,
        };
        let e = rules::choose_effect(allowed, parked.is_some(), own);
        unsafe { *eff = e };
        match parked {
            Some(p) => land(self.hwnd, p),
            None if own => log("drop of our own Take out: ignored"),
            None => log(format!("drop: nothing parked, answer {}", effect_name(e))),
        }
        Ok(())
    }
}
