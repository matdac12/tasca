//! Take out (TASCA-15 owns this file). `window.rs` calls `start` on the UI
//! thread once a press on a row has moved past `SM_CXDRAG`/`SM_CYDRAG`, after
//! releasing the mouse capture. `ids` is the pressed row, or the selection if
//! the row is part of it, top row first.

use windows::Win32::Foundation::HWND;

use super::model::ItemId;
use crate::log::log;

pub fn start(_hwnd: HWND, ids: Vec<ItemId>) {
    log(format!("take out of {ids:?}: not implemented yet (TASCA-15)"));
}
