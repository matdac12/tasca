//! Rule 5 (spec §4): `CF_UNICODETEXT` (else `CF_TEXT`) → a UTF-8 `.txt`,
//! named from its first line or `Text yyyy-MM-dd HHmmss.txt`.

use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::{CF_TEXT, CF_UNICODETEXT};
use windows::Win32::System::SystemInformation::GetLocalTime;

use super::formats;
use super::{names, one_file, Park};

pub fn offered(obj: &IDataObject) -> bool {
    formats::offers_either(obj, CF_UNICODETEXT.0, CF_TEXT.0)
}

/// Blank text is no Park.
pub fn park(obj: &IDataObject) -> Option<Park> {
    let text = formats::text(obj, CF_UNICODETEXT.0, CF_TEXT.0)?;
    if text.trim().is_empty() {
        return None;
    }
    let name = names::text_name(&text, &unsafe { GetLocalTime() });
    one_file(&name, text.into_bytes())
}
