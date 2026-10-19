//! Rule 5 (spec §4): `CF_UNICODETEXT` (else `CF_TEXT`) → a UTF-8 `.txt`,
//! named from its first line or `Text yyyy-MM-dd HHmmss.txt`.

use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::{CF_TEXT, CF_UNICODETEXT};
use windows::Win32::System::SystemInformation::GetLocalTime;

use super::formats::{self, offers};
use super::{names, one_file, Park};

pub fn offered(obj: &IDataObject) -> bool {
    offers(obj, CF_UNICODETEXT.0) || offers(obj, CF_TEXT.0)
}

/// Blank text is no Park.
pub fn park(obj: &IDataObject) -> Option<Park> {
    let text = formats::hglobal_bytes(obj, CF_UNICODETEXT.0)
        .map(|b| formats::utf16_z(&b))
        .or_else(|| formats::hglobal_bytes(obj, CF_TEXT.0).map(|b| formats::ansi_z(&b)))?;
    if text.trim().is_empty() {
        return None;
    }
    let name = names::text_name(&text, &unsafe { GetLocalTime() });
    one_file(&name, text.into_bytes())
}
