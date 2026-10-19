//! Rule 4 (spec §4): `UniformResourceLocatorW` (else ANSI) → an Internet
//! Shortcut, `<title>.url`, the title from `text/x-moz-url`, else the host.
//! Chromium links arrive as a virtual `.url` file first (rule 3); this
//! catches other sources.

use windows::core::w;
use windows::Win32::System::Com::IDataObject;
use windows::Win32::UI::Shell::{CFSTR_INETURLA, CFSTR_INETURLW};

use super::formats::{self, offers, registered};
use super::{names, one_file, Park};

pub fn offered(obj: &IDataObject) -> bool {
    offers(obj, registered(CFSTR_INETURLW)) || offers(obj, registered(CFSTR_INETURLA))
}

pub fn park(obj: &IDataObject) -> Option<Park> {
    let url = formats::hglobal_bytes(obj, registered(CFSTR_INETURLW))
        .map(|b| formats::utf16_z(&b))
        .or_else(|| formats::hglobal_bytes(obj, registered(CFSTR_INETURLA)).map(|b| formats::ansi_z(&b)))?;
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let moz = formats::hglobal_bytes(obj, registered(w!("text/x-moz-url"))).map(|b| formats::utf16_z(&b));
    let name = names::url_name(moz.as_deref().and_then(names::moz_title), url);
    one_file(&name, names::url_file(url).into_bytes())
}
