//! Clipboard-format helpers shared by the readers: `FORMATETC` building, the
//! drag-safe `QueryGetData` probe, reading `HGLOBAL` data and text, and naming
//! offered formats for the log (one line per drag; TASCA-19 reads it to see
//! what sources offer).

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HGLOBAL, S_OK};
use windows::Win32::Globalization::{MultiByteToWideChar, CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};
use windows::Win32::System::Com::{
    CoTaskMemFree, IDataObject, DATADIR_GET, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::{GetClipboardFormatNameW, RegisterClipboardFormatW};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{
    CF_BITMAP, CF_DIB, CF_DIBV5, CF_ENHMETAFILE, CF_HDROP, CF_LOCALE, CF_METAFILEPICT, CF_OEMTEXT, CF_TEXT,
    CF_UNICODETEXT, CLIPBOARD_FORMAT, ReleaseStgMedium,
};

/// The id of a registered format (`FileContents`, `PNG`, ...); 0 if it
/// can't be registered, which no object offers.
pub fn registered(name: PCWSTR) -> u16 {
    unsafe { RegisterClipboardFormatW(name) as u16 }
}

/// The bytes of an `HGLOBAL`, at most `limit` of them (`GlobalSize` may be
/// rounded up, so a known size is trusted over it).
pub fn global_bytes(h: HGLOBAL, limit: Option<u64>) -> Vec<u8> {
    unsafe {
        let size = GlobalSize(h);
        let p = GlobalLock(h) as *const u8;
        if p.is_null() {
            return Vec::new();
        }
        let n = limit.map_or(size, |l| l.min(size as u64) as usize);
        let v = std::slice::from_raw_parts(p, n).to_vec();
        let _ = GlobalUnlock(h);
        v
    }
}

/// `cf` read as an `HGLOBAL` (in `Drop` or later, never during the drag).
pub fn hglobal_bytes(obj: &IDataObject, cf: u16) -> Option<Vec<u8>> {
    unsafe {
        let mut m = obj.GetData(&hglobal_format(cf)).ok()?;
        let v = (m.tymed == TYMED_HGLOBAL.0 as u32).then(|| global_bytes(m.u.hGlobal, None));
        ReleaseStgMedium(&mut m);
        v
    }
}

/// Little-endian UTF-16 up to the first NUL.
pub fn utf16_z(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    String::from_utf16_lossy(&units)
}

/// ANSI text up to the first NUL.
pub fn ansi_z(bytes: &[u8]) -> String {
    ansi(&bytes[..bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len())])
}

/// ANSI text (the system code page) as a `String`.
pub fn ansi(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    unsafe {
        let n = MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None);
        let mut w = vec![0u16; n.max(0) as usize];
        let n = MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, Some(&mut w));
        String::from_utf16_lossy(&w[..n.max(0) as usize])
    }
}

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
