//! Park (spec §4): the Shelf as an OLE drop target.
//!
//! - `target`: the `IDropTarget`, the effect, and `READERS`, the per-format
//!   dispatch `Drop` runs through.
//! - `hdrop`: rule 1, `CF_HDROP` (temp paths copied into a drop folder).
//! - `dropdir`: `%TEMP%\Tasca\<drop-id>\` folders.
//! - `rules`: the pure rules. `formats`: clipboard-format helpers.
//!
//! The resident calls `register` once at startup (window created, still
//! hidden), `revoke` on `WM_DESTROY` and `uninit` after the message loop.

pub mod dropdir;
mod formats;
mod hdrop;
pub mod rules;
mod target;

use std::cell::Cell;
use std::path::PathBuf;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Ole::{IDropTarget, OleInitialize, OleUninitialize, RegisterDragDrop, RevokeDragDrop};
use windows::Win32::UI::Shell::{CLSID_DragDropHelper, IDropTargetHelper};

use crate::log::log;
use crate::shelf::{self, model::Kind};

/// What a reader made of a drop: the files of one Item, in drop order.
#[derive(Debug, PartialEq, Eq)]
pub struct Parked {
    pub files: Vec<PathBuf>,
    pub kind: Kind,
}

/// Put a Park on the Shelf: the same path set as an Item already there moves
/// that Item to the top; anything else is a new Item on top. The Shelf stays
/// shown either way.
pub fn place(p: Parked) {
    match shelf::with_model(|m| m.same_files(&p.files)).flatten() {
        Some(id) => {
            log(format!("duplicate Park: Item {id} moved to the top"));
            shelf::move_to_top(id);
        }
        None => {
            shelf::add_top(p.files, p.kind);
        }
    }
}

thread_local! {
    static OLE: Cell<bool> = const { Cell::new(false) };
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
}

/// `OleInitialize` and `RegisterDragDrop(shelf)`, once, on the UI thread.
/// A failure is logged and the resident runs on without Park.
pub fn register(hwnd: HWND) {
    unsafe {
        if let Err(e) = OleInitialize(None) {
            log(format!("OleInitialize failed, Park is off: {e:?}"));
            return;
        }
        OLE.set(true);
        let helper: Option<IDropTargetHelper> = CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER)
            .inspect_err(|e| log(format!("no IDropTargetHelper (no drag image over the Shelf): {e:?}")))
            .ok();
        let has_helper = helper.is_some();
        let target: IDropTarget = target::DropTarget::new(hwnd, helper).into();
        match RegisterDragDrop(hwnd, &target) {
            Ok(()) => {
                REGISTERED.set(true);
                log(format!("drop target registered (drag image helper: {has_helper})"));
            }
            Err(e) => log(format!("RegisterDragDrop failed, Park is off: {e:?}")),
        }
    }
}

/// `RevokeDragDrop` while the window still exists (`WM_DESTROY`).
pub fn revoke(hwnd: HWND) {
    if REGISTERED.replace(false) {
        let _ = unsafe { RevokeDragDrop(hwnd) };
    }
}

/// `OleUninitialize`, after the message loop.
pub fn uninit() {
    if OLE.replace(false) {
        unsafe { OleUninitialize() };
    }
}

/// `Drop`'s reading path, end to end, with hand-built data objects (no OLE
/// drag): `target::read` → readers → `hdrop` → drop folders, then the Shelf's
/// duplicate rule on a `Model`, as `place` applies it.
#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::ManuallyDrop;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::core::HSTRING;
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Storage::FileSystem::GetShortPathNameW;
    use windows::Win32::System::Com::{IDataObject, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
    use windows::Win32::UI::Shell::{SHCreateDataObject, DROPFILES};

    use crate::shelf::model::Model;

    /// An empty shell data object on an OLE thread (each test has its own).
    fn data_object() -> IDataObject {
        unsafe {
            let _ = OleInitialize(None);
            SHCreateDataObject(None, None, None::<&IDataObject>).unwrap()
        }
    }

    fn set_hglobal(obj: &IDataObject, cf: u16, bytes: &[u8]) {
        unsafe {
            let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).unwrap();
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), GlobalLock(h) as *mut u8, bytes.len());
            let _ = GlobalUnlock(h);
            let medium = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: h }, pUnkForRelease: ManuallyDrop::new(None) };
            obj.SetData(&formats::hglobal_format(cf), &medium, true).unwrap();
        }
    }

    /// A data object offering these paths as `CF_HDROP`, like Explorer's.
    fn hdrop_object(paths: &[PathBuf]) -> IDataObject {
        let mut list: Vec<u16> = paths.iter().flat_map(|p| p.as_os_str().encode_wide().chain([0])).collect();
        list.push(0);
        let header = DROPFILES { pFiles: size_of::<DROPFILES>() as u32, pt: POINT::default(), fNC: false.into(), fWide: true.into() };
        let mut bytes = unsafe { std::slice::from_raw_parts(&header as *const _ as *const u8, size_of::<DROPFILES>()) }.to_vec();
        bytes.extend(list.iter().flat_map(|c| c.to_le_bytes()));
        let obj = data_object();
        set_hglobal(&obj, CF_HDROP.0, &bytes);
        obj
    }

    /// A fresh folder with these files (relative paths; `/` makes subfolders).
    fn files_in(base: &Path, tag: &str, names: &[&str]) -> (PathBuf, Vec<PathBuf>) {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = base.join(format!("tasca-test-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = names
            .iter()
            .map(|name| {
                let p = dir.join(name);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, name.as_bytes()).unwrap();
                p
            })
            .collect();
        (dir, paths)
    }

    /// Not a temp path: the crate's target folder.
    fn not_temp() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target")
    }

    fn names(files: &[PathBuf]) -> Vec<&str> {
        files.iter().map(|f| f.file_name().unwrap().to_str().unwrap()).collect()
    }

    #[test]
    fn hdrop_is_supported_text_alone_is_not() {
        assert!(target::supported(&hdrop_object(&[r"C:\a.txt".into()])));
        assert!(!target::supported(&data_object()));
        let text = data_object();
        set_hglobal(&text, CF_UNICODETEXT.0, &[b'h', 0, b'i', 0, 0, 0]);
        assert!(!target::supported(&text));
        assert_eq!(target::read(&text), None);
    }

    #[test]
    fn explorer_files_park_as_references_one_stack_then_a_duplicate() {
        let (dir, paths) = files_in(&not_temp(), "ref", &["a.txt", "b.jpg", "c.pdf"]);
        let mut m = Model::new();

        let one = target::read(&hdrop_object(&paths[..1])).unwrap();
        assert_eq!(one, Parked { files: paths[..1].to_vec(), kind: Kind::Reference });
        assert_eq!(m.same_files(&one.files), None);
        let single = m.add_top(one.files, one.kind);
        assert!(!m.get(single).unwrap().is_stack());

        let three = target::read(&hdrop_object(&paths)).unwrap();
        assert_eq!(three, Parked { files: paths.clone(), kind: Kind::Reference });
        assert_eq!(m.same_files(&three.files), None);
        let stack = m.add_top(three.files, three.kind);
        assert!(m.get(stack).unwrap().is_stack());

        // The same selection again, in another order: no new Item, it goes on top.
        let again = target::read(&hdrop_object(&[paths[2].clone(), paths[0].clone(), paths[1].clone()])).unwrap();
        assert_eq!(m.same_files(&again.files), Some(stack));
        assert_eq!(m.same_files(&paths[..1]), Some(single));

        // References are never copied or touched.
        for p in &paths {
            assert_eq!(std::fs::read(p).unwrap(), p.file_name().unwrap().to_str().unwrap().as_bytes());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn temp_files_and_folders_are_copied_and_survive_their_source() {
        let (src, paths) = files_in(dropdir::temp_dir(), "7zE", &["photo.jpg", "folder/inner/deep.txt", "folder/top.txt"]);
        let dropped = vec![paths[0].clone(), src.join("folder")];

        let p = target::read(&hdrop_object(&dropped)).unwrap();
        assert_eq!(p.kind, Kind::Materialized);
        assert_eq!(names(&p.files), ["photo.jpg", "folder"]);
        let drop_dir = p.files[0].parent().unwrap().to_path_buf();
        assert_eq!(drop_dir.parent(), Some(dropdir::root().as_path()));
        assert_eq!(p.files[1].parent(), Some(drop_dir.as_path()));

        // 7-Zip deletes its temp folder after DoDragDrop returns.
        std::fs::remove_dir_all(&src).unwrap();
        assert_eq!(std::fs::read(&p.files[0]).unwrap(), b"photo.jpg");
        assert_eq!(std::fs::read(p.files[1].join("inner").join("deep.txt")).unwrap(), b"folder/inner/deep.txt");
        assert_eq!(std::fs::read(p.files[1].join("top.txt")).unwrap(), b"folder/top.txt");

        // Copies in our drop folder are kept by path when dropped again.
        let back = target::read(&hdrop_object(&p.files)).unwrap();
        assert_eq!(back, Parked { files: p.files.clone(), kind: Kind::Reference });
        std::fs::remove_dir_all(drop_dir).unwrap();
    }

    #[test]
    fn same_named_temp_files_get_distinct_copies() {
        let (src, paths) = files_in(dropdir::temp_dir(), "dup", &["x/a.txt", "y/a.txt"]);
        let p = target::read(&hdrop_object(&paths)).unwrap();
        assert_eq!(names(&p.files), ["a.txt", "a (2).txt"]);
        assert_eq!(std::fs::read(&p.files[1]).unwrap(), b"y/a.txt");
        std::fs::remove_dir_all(src).unwrap();
        std::fs::remove_dir_all(p.files[0].parent().unwrap()).unwrap();
    }

    #[test]
    fn a_mixed_drop_is_a_reference_stack_and_failed_copies_are_dropped() {
        let (ref_dir, refs) = files_in(&not_temp(), "mix", &["kept.txt"]);
        let (tmp_dir, tmps) = files_in(dropdir::temp_dir(), "mix", &["copied.txt"]);
        let gone = tmp_dir.join("vanished.txt");

        let p = target::read(&hdrop_object(&[refs[0].clone(), gone.clone(), tmps[0].clone()])).unwrap();
        assert_eq!(p.kind, Kind::Reference);
        assert_eq!(p.files[0], refs[0]);
        assert_eq!(names(&p.files), ["kept.txt", "copied.txt"]);
        assert_ne!(p.files[1], tmps[0]);

        // Nothing survives: no Park at all.
        assert_eq!(target::read(&hdrop_object(&[gone])), None);

        std::fs::remove_dir_all(p.files[1].parent().unwrap()).unwrap();
        std::fs::remove_dir_all(ref_dir).unwrap();
        std::fs::remove_dir_all(tmp_dir).unwrap();
    }

    #[test]
    fn short_names_of_temp_paths_count_as_temp() {
        let (dir, paths) = files_in(dropdir::temp_dir(), "short-name-check", &["file.txt"]);
        let wide = HSTRING::from(paths[0].as_path());
        let mut buf = vec![0u16; 1024];
        let n = unsafe { GetShortPathNameW(&wide, Some(&mut buf)) } as usize;
        assert!(n > 0 && n < buf.len());
        let short = PathBuf::from(String::from_utf16_lossy(&buf[..n]));
        // Without 8.3 names on the volume, short == long. Either way the drop
        // reads the long form, sees a temp path and copies it.
        let p = target::read(&hdrop_object(std::slice::from_ref(&short))).unwrap();
        assert_eq!(p.kind, Kind::Materialized, "{}", short.display());
        assert_eq!(names(&p.files), ["file.txt"]);
        assert!(!p.files[0].to_string_lossy().contains('~'), "{}", p.files[0].display());
        std::fs::remove_dir_all(p.files[0].parent().unwrap()).unwrap();
        assert!(hdrop::is_temp(&paths[0]));
        assert!(!hdrop::is_temp(&dropdir::root().join("1").join("file.txt")));
        // A path that is gone can't be expanded: it still counts, in either form.
        for t in dropdir::temp_dirs() {
            assert!(hdrop::is_temp(&t.join("gone").join("x.txt")), "{}", t.display());
            assert!(!hdrop::is_temp(&t.join("Tasca").join("1").join("x.txt")));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
