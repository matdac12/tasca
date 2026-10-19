//! Rule 6 (spec §4): `PNG` bytes → `Image yyyy-MM-dd HHmmss.png`. `CF_DIB`
//! is optional for v1 and not read (it would need encoding).

use windows::core::w;
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::SystemInformation::GetLocalTime;

use super::formats::{self, offers, registered};
use super::{names, one_file, Park};

pub fn offered(obj: &IDataObject) -> bool {
    offers(obj, registered(w!("PNG")))
}

pub fn park(obj: &IDataObject) -> Option<Park> {
    let bytes = formats::hglobal_bytes(obj, registered(w!("PNG"))).filter(|b| !b.is_empty())?;
    one_file(&format!("Image {}.png", names::stamp(&unsafe { GetLocalTime() })), bytes)
}
