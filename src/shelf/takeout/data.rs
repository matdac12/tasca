//! The data object a Take out drags (spec §5): the shell's own, built from a
//! shell item array, like Explorer's; for files in different folders, an empty
//! shell data object with `CF_HDROP`. Nothing else goes on it: no text paths,
//! no `Preferred DropEffect`, no async opt-in (`research/drag-out.md`).

use std::mem::ManuallyDrop;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use windows::core::{Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, POINT};
use windows::Win32::System::Com::{
    CoTaskMemFree, IDataObject, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{ReleaseStgMedium, CF_HDROP};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    BHID_DataObject, SHCreateDataObject, SHCreateShellItemArrayFromIDLists, SHParseDisplayName, DROPFILES,
};

use super::rule;
use crate::log::log;
use crate::park::formats::hglobal_format;

/// Which way the data object was built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    ShellItems,
    HDrop,
}

/// The data object for these files, in order. Needs COM/OLE on the thread.
pub fn build(paths: &[PathBuf]) -> Result<(IDataObject, Via)> {
    if rule::one_folder(paths) {
        match shell_items(paths) {
            Ok(obj) => return Ok((obj, Via::ShellItems)),
            Err(e) => log(format!("take out: shell item array failed, using CF_HDROP: {e:?}")),
        }
    }
    hdrop(paths).map(|obj| (obj, Via::HDrop))
}

/// `SHParseDisplayName` per file → `SHCreateShellItemArrayFromIDLists` →
/// `BindToHandler(BHID_DataObject)`. Flat arrays only (one folder).
fn shell_items(paths: &[PathBuf]) -> Result<IDataObject> {
    let mut pidls: Vec<*const ITEMIDLIST> = Vec::with_capacity(paths.len());
    let built = (|| unsafe {
        for p in paths {
            let mut pidl = std::ptr::null_mut();
            SHParseDisplayName(&HSTRING::from(p.as_path()), None, &mut pidl, 0, None)?;
            pidls.push(pidl);
        }
        SHCreateShellItemArrayFromIDLists(&pidls)?.BindToHandler::<_, IDataObject>(None, &BHID_DataObject)
    })();
    for p in pidls {
        unsafe { CoTaskMemFree(Some(p as _)) };
    }
    built
}

/// `DROPFILES` (wide) followed by the double-NUL-terminated path list.
fn dropfiles(paths: &[PathBuf]) -> Vec<u8> {
    let header = DROPFILES { pFiles: size_of::<DROPFILES>() as u32, pt: POINT::default(), fNC: false.into(), fWide: true.into() };
    let mut bytes = unsafe { std::slice::from_raw_parts(&header as *const _ as *const u8, size_of::<DROPFILES>()) }.to_vec();
    let list = paths.iter().flat_map(|p| p.as_os_str().encode_wide().chain([0])).chain([0]);
    bytes.extend(list.flat_map(u16::to_le_bytes));
    bytes
}

/// `SHCreateDataObject` + `SetData(CF_HDROP)`: still a shell object, so the
/// drag image and targets' write-backs (`Logical Performed DropEffect`) work.
fn hdrop(paths: &[PathBuf]) -> Result<IDataObject> {
    let obj: IDataObject = unsafe { SHCreateDataObject(None, None, None::<&IDataObject>)? };
    set_hglobal(&obj, CF_HDROP.0, &dropfiles(paths))?;
    Ok(obj)
}

/// Put `bytes` on the object as `cf` in an `HGLOBAL` the object then owns.
fn set_hglobal(obj: &IDataObject, cf: u16, bytes: &[u8]) -> Result<()> {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            let e = windows::core::Error::from_thread();
            let _ = GlobalFree(Some(h));
            return Err(e);
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(h);
        let medium = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: h }, pUnkForRelease: ManuallyDrop::new(None) };
        obj.SetData(&hglobal_format(cf), &medium, true).inspect_err(|_| {
            // Not taken over: still ours to free.
            let _ = GlobalFree(Some(h));
        })
    }
}

/// A DWORD format a target wrote back (`Logical Performed DropEffect`, ...).
pub fn read_dword(obj: &IDataObject, name: PCWSTR) -> Option<u32> {
    unsafe {
        let cf = u16::try_from(RegisterClipboardFormatW(name)).ok().filter(|&cf| cf != 0)?;
        let mut m = obj.GetData(&hglobal_format(cf)).ok()?;
        let h = m.u.hGlobal;
        let p = GlobalLock(h) as *const u32;
        // A target may write fewer bytes than a DWORD: no reading past them.
        let v = (!p.is_null() && GlobalSize(h) >= size_of::<u32>()).then(|| p.read_unaligned());
        let _ = GlobalUnlock(h);
        ReleaseStgMedium(&mut m);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::park::formats::{offers, registered};
    use windows::Win32::System::Ole::OleInitialize;
    use windows::Win32::UI::Shell::{DragQueryFileW, CFSTR_LOGICALPERFORMEDDROPEFFECT, CFSTR_SHELLIDLIST, HDROP};

    fn ole() {
        unsafe {
            let _ = OleInitialize(None);
        }
    }

    /// Under the crate's `target` folder: `%TEMP%` may be an 8.3 path, which
    /// the shell hands back in long form (real Items are long form already).
    fn files(tag: &str, names: &[&str]) -> (PathBuf, Vec<PathBuf>) {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("tasca-takeout-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = names
            .iter()
            .map(|n| {
                let p = dir.join(n);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, n.as_bytes()).unwrap();
                p
            })
            .collect();
        (dir, paths)
    }

    fn cf(name: PCWSTR) -> u16 {
        registered(name)
    }

    fn hdrop_paths(obj: &IDataObject) -> Vec<PathBuf> {
        unsafe {
            let mut m = obj.GetData(&hglobal_format(CF_HDROP.0)).unwrap();
            let h = HDROP(m.u.hGlobal.0);
            let mut v = vec![];
            for i in 0..DragQueryFileW(h, u32::MAX, None) {
                let mut buf = vec![0u16; DragQueryFileW(h, i, None) as usize + 1];
                let n = DragQueryFileW(h, i, Some(&mut buf)) as usize;
                v.push(PathBuf::from(String::from_utf16_lossy(&buf[..n])));
            }
            ReleaseStgMedium(&mut m);
            v
        }
    }

    fn same(a: &[PathBuf], b: &[PathBuf]) -> bool {
        let key = |v: &[PathBuf]| v.iter().map(|p| crate::shelf::model::path_key(p)).collect::<Vec<_>>();
        key(a) == key(b)
    }

    #[test]
    fn one_folder_uses_the_shell_item_array() {
        ole();
        let (dir, paths) = files("flat", &["a.txt", "b.jpg", "c.pdf"]);
        for set in [&paths[..1], &paths[..]] {
            let (obj, via) = build(set).unwrap();
            assert_eq!(via, Via::ShellItems);
            assert!(offers(&obj, cf(CFSTR_SHELLIDLIST)), "Explorer's item list");
            assert!(same(&hdrop_paths(&obj), set), "{:?} vs {set:?}", hdrop_paths(&obj));
            // No text paths on it (Terminal would paste text instead of the path).
            assert!(!offers(&obj, windows::Win32::System::Ole::CF_UNICODETEXT.0));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn different_folders_fall_back_to_hdrop() {
        ole();
        let (dir, paths) = files("spread", &["x/a.txt", "y/b.txt", "y/z/c.txt"]);
        let (obj, via) = build(&paths).unwrap();
        assert_eq!(via, Via::HDrop);
        assert!(same(&hdrop_paths(&obj), &paths));
        assert!(!offers(&obj, windows::Win32::System::Ole::CF_UNICODETEXT.0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_target_write_back_is_read_as_a_dword() {
        ole();
        let (dir, paths) = files("dword", &["a.txt"]);
        let (obj, _) = build(&paths).unwrap();
        assert_eq!(read_dword(&obj, CFSTR_LOGICALPERFORMEDDROPEFFECT), None);
        // What Explorer does after an optimized move: logical effect MOVE.
        set_hglobal(&obj, cf(CFSTR_LOGICALPERFORMEDDROPEFFECT), &2u32.to_le_bytes()).unwrap();
        assert_eq!(read_dword(&obj, CFSTR_LOGICALPERFORMEDDROPEFFECT), Some(2));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
