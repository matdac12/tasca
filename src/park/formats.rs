//! Clipboard-format helpers shared by the readers: `FORMATETC` building, the
//! drag-safe `QueryGetData` probe, and naming offered formats for the log
//! (one line per drag; TASCA-19 reads it to see what sources offer).

use windows::Win32::Foundation::S_OK;
use windows::Win32::System::Com::{
    CoTaskMemFree, IDataObject, DATADIR_GET, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::GetClipboardFormatNameW;
use windows::Win32::System::Ole::{
    CF_BITMAP, CF_DIB, CF_DIBV5, CF_ENHMETAFILE, CF_HDROP, CF_LOCALE, CF_METAFILEPICT, CF_OEMTEXT, CF_TEXT,
    CF_UNICODETEXT, CLIPBOARD_FORMAT,
};

/// A format asked for as an `HGLOBAL`.
pub fn hglobal_format(cf: u16) -> FORMATETC {
    FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
}

/// The object offers `cf` as an `HGLOBAL`. `QueryGetData` only: it renders
/// nothing, so it is safe in `DragEnter`.
pub fn offers(obj: &IDataObject, cf: u16) -> bool {
    unsafe { obj.QueryGetData(&hglobal_format(cf)) == S_OK }
}

fn cf_name(cf: u16) -> String {
    let name = match CLIPBOARD_FORMAT(cf) {
        CF_TEXT => "CF_TEXT",
        CF_BITMAP => "CF_BITMAP",
        CF_METAFILEPICT => "CF_METAFILEPICT",
        CF_OEMTEXT => "CF_OEMTEXT",
        CF_DIB => "CF_DIB",
        CF_UNICODETEXT => "CF_UNICODETEXT",
        CF_ENHMETAFILE => "CF_ENHMETAFILE",
        CF_HDROP => "CF_HDROP",
        CF_LOCALE => "CF_LOCALE",
        CF_DIBV5 => "CF_DIBV5",
        _ => {
            let mut buf = [0u16; 256];
            let n = unsafe { GetClipboardFormatNameW(cf as u32, &mut buf) };
            return if n > 0 { String::from_utf16_lossy(&buf[..n as usize]) } else { format!("#{cf}") };
        }
    };
    name.into()
}

/// The names of the bits set in `bits`, joined with `|` ("" if none).
pub fn flag_names(bits: u32, names: &[(u32, &str)]) -> String {
    names.iter().filter(|(bit, _)| bits & bit != 0).map(|(_, n)| *n).collect::<Vec<_>>().join("|")
}

fn tymed_name(t: u32) -> String {
    flag_names(t, &[(1, "HGLOBAL"), (2, "FILE"), (4, "ISTREAM"), (8, "ISTORAGE"), (16, "GDI"), (32, "MFPICT"), (64, "ENHMF")])
}

/// The formats the object offers, comma-separated, for the log. Enumerating
/// renders nothing, so it is safe during the drag.
pub fn list(obj: &IDataObject) -> String {
    const MAX: usize = 40;
    let mut out = Vec::new();
    unsafe {
        let Ok(e) = obj.EnumFormatEtc(DATADIR_GET.0 as u32) else { return "(EnumFormatEtc failed)".into() };
        loop {
            let mut f = [FORMATETC::default()];
            let mut n = 0u32;
            if e.Next(&mut f, Some(&mut n)) != S_OK || n == 0 {
                break;
            }
            if !f[0].ptd.is_null() {
                CoTaskMemFree(Some(f[0].ptd as _));
            }
            if out.len() == MAX {
                out.push("…".to_string());
                break;
            }
            out.push(format!("{}[{}]", cf_name(f[0].cfFormat), tymed_name(f[0].tymed)));
        }
    }
    out.join(", ")
}
