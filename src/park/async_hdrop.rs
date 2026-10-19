//! Rule 2 (spec §4): a delay-rendered `CF_HDROP`. Chromium's `DownloadURL`
//! drags (Gmail-style attachments) offer `CF_HDROP` but render it only in an
//! async operation: the file downloads once the target asks. So: start the
//! operation, read on a worker, apply rule 1 (temp paths are copied), end the
//! operation. Without async mode there is nothing to read: refused.

use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::CF_HDROP;

use super::job::{self, Async, Job};
use super::{formats, hdrop, Park};
use crate::log::log;

/// Only when rule 1 could not read `CF_HDROP` now.
pub fn park(obj: &IDataObject) -> Option<Park> {
    if formats::hglobal_bytes(obj, CF_HDROP.0).is_some() {
        return None; // readable: rule 1 had its chance (nothing survived)
    }
    match job::start_async(obj) {
        Async::No => {
            log("CF_HDROP is unreadable and the source has no async mode: refused");
            None
        }
        Async::Carried(c) => Some(Park::Later(Job::Hdrop(c))),
        Async::Here(cap) => {
            let p = hdrop::park(obj);
            job::end(&cap, p.is_some());
            p.map(Park::Ready)
        }
    }
}
