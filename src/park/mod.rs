//! Park (spec §4): the Shelf as an OLE drop target.
//!
//! - `target`: the `IDropTarget`, the effect, and `READERS`, the per-format
//!   dispatch `Drop` runs through.
//! - The readers, in the spec's order: `hdrop` (rule 1, `CF_HDROP`, temp paths
//!   copied), `async_hdrop` (2), `virtual_files` (3), `url` (4), `text` (5),
//!   `png` (6).
//! - `job`: the worker threads that write Materialized files and read async
//!   drops; they report back through `WM_PARK_DONE` (`on_done`).
//! - `dropdir`: `%TEMP%\Tasca\<drop-id>\` folders. `names`: the names of the
//!   files in them. `sweep`: the startup sweep of old ones.
//! - `rules`: the pure rules. `formats`: clipboard-format helpers.
//!
//! The resident calls `register` once at startup (window created, still
//! hidden), `sweep_old_drops` once, `revoke` on `WM_DESTROY`, `uninit` after
//! the message loop, and `on_done` on `WM_PARK_DONE`.

mod async_hdrop;
pub mod dropdir;
mod formats;
mod hdrop;
mod job;
mod names;
mod png;
pub mod rules;
mod sweep;
mod target;
#[cfg(test)]
mod testobj;
mod text;
mod url;
mod virtual_files;

use std::cell::Cell;
use std::path::PathBuf;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Ole::{IDropTarget, OleInitialize, OleUninitialize, RegisterDragDrop, RevokeDragDrop};
use windows::Win32::UI::Shell::{CLSID_DragDropHelper, IDropTargetHelper};

use crate::log::log;
use crate::shelf::{self, model::Kind};

/// The files of one Item, in drop order.
#[derive(Debug, PartialEq, Eq)]
pub struct Parked {
    pub files: Vec<PathBuf>,
    pub kind: Kind,
}

/// What a reader made of a drop.
pub enum Park {
    /// The files exist now (rule 1).
    Ready(Parked),
    /// The files are planned in a drop folder; the job writes them on a
    /// worker. The Item shows at once, pending, and fills in when they land.
    Planned(Parked, job::Job),
    /// The job reads the drop on a worker and Parks what it finds (rule 2:
    /// nothing is known until the source renders).
    Later(job::Job),
}

impl Park {
    /// For the log.
    fn describe(&self) -> String {
        match self {
            Park::Ready(p) => format!("{} file(s), {:?}", p.files.len(), p.kind),
            Park::Planned(p, _) => format!("{} file(s) planned, {:?}", p.files.len(), p.kind),
            Park::Later(_) => "read on a worker".into(),
        }
    }
}

/// A Park of one Materialized file called `name` holding `bytes` (rules 4–6).
fn one_file(name: &str, bytes: Vec<u8>) -> Option<Park> {
    let dir = dropdir::create().inspect_err(|e| log(format!("no drop folder: {e}"))).ok()?;
    let path = dir.join(name);
    let job = job::Job::write(dir, vec![path.clone()], vec![job::Entry::bytes(path.clone(), bytes)], None);
    Some(Park::Planned(Parked { files: vec![path], kind: Kind::Materialized }, job))
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

/// Land a reader's Park (in `Drop`): on the Shelf now, pending, or later.
/// Workers report to `hwnd`.
fn land(hwnd: HWND, park: Park) {
    match park {
        Park::Ready(p) => place(p),
        Park::Planned(p, job) => {
            let id = shelf::add_pending(p.files, p.kind);
            start(hwnd, job, id);
        }
        Park::Later(job) => start(hwnd, job, None),
    }
}

fn start(hwnd: HWND, job: job::Job, id: Option<shelf::model::ItemId>) {
    let to = hwnd.0 as isize;
    if let Err(e) = job::spawn(job, id, move |done| job::deliver(to, done)) {
        log(format!("no worker for the drop: {e}"));
        if let Some(id) = id {
            shelf::remove(&[id]);
        }
    }
}

/// `WM_PARK_DONE`: workers finished. Pending Items fill in (or leave, if
/// nothing was written); async drops are placed.
pub fn on_done() {
    for done in job::take() {
        match done {
            job::Done::Filled(Some(id), files) => {
                log(format!("Item {id}: {} file(s) written", files.len()));
                shelf::filled(id, files);
            }
            job::Done::Filled(None, _) => {}
            job::Done::Parked(p) => {
                log(format!("async drop: {} file(s), {:?}", p.files.len(), p.kind));
                place(p);
            }
        }
    }
}

/// Delete old drop folders, off the UI thread (once, at resident start).
pub fn sweep_old_drops() {
    sweep::start();
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
    use windows::Win32::System::Ole::CF_HDROP;
    use windows::Win32::UI::Shell::{SHCreateDataObject, DROPFILES};

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{E_FAIL, S_OK};
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
    use windows::Win32::System::Ole::{CF_TEXT, CF_UNICODETEXT};
    use windows::Win32::UI::Shell::{
        CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW, CFSTR_INETURLA, CFSTR_INETURLW, FD_ATTRIBUTES, FD_FILESIZE,
    };

    use super::testobj::{pump_until, As, Source};
    use super::virtual_files::tests::group_w;
    use crate::shelf::model::{ItemId, Model};

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

    /// `DROPFILES` with these paths, wide.
    fn dropfiles(paths: &[PathBuf]) -> Vec<u8> {
        let mut list: Vec<u16> = paths.iter().flat_map(|p| p.as_os_str().encode_wide().chain([0])).collect();
        list.push(0);
        let header = DROPFILES { pFiles: size_of::<DROPFILES>() as u32, pt: POINT::default(), fNC: false.into(), fWide: true.into() };
        let mut bytes = unsafe { std::slice::from_raw_parts(&header as *const _ as *const u8, size_of::<DROPFILES>()) }.to_vec();
        bytes.extend(list.iter().flat_map(|c| c.to_le_bytes()));
        bytes
    }

    /// A data object offering these paths as `CF_HDROP`, like Explorer's.
    fn hdrop_object(paths: &[PathBuf]) -> IDataObject {
        let obj = data_object();
        set_hglobal(&obj, CF_HDROP.0, &dropfiles(paths));
        obj
    }

    fn ready(p: Option<Park>) -> Parked {
        match p {
            Some(Park::Ready(p)) => p,
            other => panic!("expected a ready Park, got {:?}", other.map(|p| p.describe())),
        }
    }

    fn planned(p: Option<Park>) -> (Parked, job::Job) {
        match p {
            Some(Park::Planned(p, j)) => (p, j),
            other => panic!("expected a planned Park, got {:?}", other.map(|p| p.describe())),
        }
    }

    /// Run a job on its worker thread, pumping this thread's messages meanwhile.
    fn run(job: job::Job, item: Option<ItemId>) -> Option<job::Done> {
        let (tx, rx) = std::sync::mpsc::channel();
        job::spawn(job, item, move |d| {
            let _ = tx.send(d);
        })
        .unwrap();
        pump_until(|| rx.try_recv().ok())
    }

    fn utf16z(s: &str) -> Vec<u8> {
        s.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect()
    }

    fn cf(name: PCWSTR) -> u16 {
        formats::registered(name)
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
    fn hdrop_is_supported_an_empty_object_is_not() {
        assert!(target::supported(&hdrop_object(&[r"C:.txt".into()])));
        assert!(!target::supported(&data_object()));
        assert!(target::read(&data_object()).is_none());
    }

    #[test]
    fn explorer_files_park_as_references_one_stack_then_a_duplicate() {
        let (dir, paths) = files_in(&not_temp(), "ref", &["a.txt", "b.jpg", "c.pdf"]);
        let mut m = Model::new();

        let one = ready(target::read(&hdrop_object(&paths[..1])));
        assert_eq!(one, Parked { files: paths[..1].to_vec(), kind: Kind::Reference });
        assert_eq!(m.same_files(&one.files), None);
        let single = m.add_top(one.files, one.kind);
        assert!(!m.get(single).unwrap().is_stack());

        let three = ready(target::read(&hdrop_object(&paths)));
        assert_eq!(three, Parked { files: paths.clone(), kind: Kind::Reference });
        assert_eq!(m.same_files(&three.files), None);
        let stack = m.add_top(three.files, three.kind);
        assert!(m.get(stack).unwrap().is_stack());

        // The same selection again, in another order: no new Item, it goes on top.
        let again = ready(target::read(&hdrop_object(&[paths[2].clone(), paths[0].clone(), paths[1].clone()])));
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

        let p = ready(target::read(&hdrop_object(&dropped)));
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
        let back = ready(target::read(&hdrop_object(&p.files)));
        assert_eq!(back, Parked { files: p.files.clone(), kind: Kind::Reference });
        std::fs::remove_dir_all(drop_dir).unwrap();
    }

    #[test]
    fn same_named_temp_files_get_distinct_copies() {
        let (src, paths) = files_in(dropdir::temp_dir(), "dup", &["x/a.txt", "y/a.txt"]);
        let p = ready(target::read(&hdrop_object(&paths)));
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

        let p = ready(target::read(&hdrop_object(&[refs[0].clone(), gone.clone(), tmps[0].clone()])));
        assert_eq!(p.kind, Kind::Reference);
        assert_eq!(p.files[0], refs[0]);
        assert_eq!(names(&p.files), ["kept.txt", "copied.txt"]);
        assert_ne!(p.files[1], tmps[0]);

        // Nothing survives: no Park at all.
        assert!(target::read(&hdrop_object(&[gone])).is_none());

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
        let p = ready(target::read(&hdrop_object(std::slice::from_ref(&short))));
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

    // ------------------------------------------------------------ Materialized files (TASCA-16)

    const COPY: u32 = 1;
    const DIR: u32 = FILE_ATTRIBUTE_DIRECTORY.0;

    fn drop_dir_of(files: &[PathBuf]) -> PathBuf {
        let mut d = files[0].parent().unwrap().to_path_buf();
        while d.parent() != Some(dropdir::root().as_path()) {
            d = d.parent().unwrap().to_path_buf();
        }
        d
    }

    #[test]
    fn a_virtual_file_stream_is_materialized_under_its_name() {
        // Two chunks and a bit, from a stream left at its end.
        let bytes: Vec<u8> = (0..job::CHUNK + 3).map(|i| (i % 251) as u8).collect();
        let source = Source::new(false)
            .with(cf(CFSTR_FILEDESCRIPTORW), -1, As::HGlobal, &group_w(&[("report: final.pdf", FD_FILESIZE.0, 0, bytes.len() as u64)], 1))
            .with(cf(CFSTR_FILECONTENTS), 0, As::Stream, &bytes);
        let obj: IDataObject = source.into();
        assert!(target::supported(&obj));

        let (p, job) = planned(target::read(&obj));
        assert_eq!(p.kind, Kind::Materialized);
        assert_eq!(names(&p.files), ["report_ final.pdf"]);
        assert_eq!(p.files[0].parent().unwrap().parent(), Some(dropdir::root().as_path()));
        assert!(!p.files[0].exists(), "written on the worker, not in Drop");

        assert_eq!(run(job, Some(7)), Some(job::Done::Filled(Some(7), p.files.clone())));
        assert_eq!(std::fs::read(&p.files[0]).unwrap(), bytes);
        std::fs::remove_dir_all(drop_dir_of(&p.files)).unwrap();
    }

    #[test]
    fn an_async_zip_tree_is_read_on_the_worker_and_the_operation_ended() {
        let descriptor = group_w(
            &[
                ("Folder", FD_ATTRIBUTES.0, DIR, 0),
                (r"Folder\a.txt", 0, 0, 0),
                (r"Folder\sub\b.txt", 0, 0, 0),
                (r"..\evil.txt", 0, 0, 0),
                ("top.txt", 0, 0, 0),
            ],
            5,
        );
        let source = Source::new(true)
            .with(cf(CFSTR_FILEDESCRIPTORW), -1, As::HGlobal, &descriptor)
            .with(cf(CFSTR_FILECONTENTS), 1, As::HGlobal, b"a")
            .with(cf(CFSTR_FILECONTENTS), 2, As::Stream, b"b")
            .with(cf(CFSTR_FILECONTENTS), 3, As::HGlobal, b"evil")
            .with(cf(CFSTR_FILECONTENTS), 4, As::HGlobal, b"top");
        let ended = source.ended();
        let obj: IDataObject = source.into();

        let (p, job) = planned(target::read(&obj));
        assert_eq!(names(&p.files), ["Folder", "top.txt"]);
        assert_eq!(*ended.borrow(), None);
        assert_eq!(run(job, Some(3)), Some(job::Done::Filled(Some(3), p.files.clone())));
        assert_eq!(*ended.borrow(), Some((S_OK, COPY)));

        let dir = drop_dir_of(&p.files);
        assert_eq!(std::fs::read(dir.join("Folder").join("a.txt")).unwrap(), b"a");
        assert_eq!(std::fs::read(dir.join("Folder").join("sub").join("b.txt")).unwrap(), b"b");
        assert_eq!(std::fs::read(dir.join("top.txt")).unwrap(), b"top");
        assert!(!dropdir::root().join("evil.txt").exists() && !dir.join("evil.txt").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn entries_that_fail_in_drop_are_left_out() {
        let source = Source::new(false)
            .with(cf(CFSTR_FILEDESCRIPTORW), -1, As::HGlobal, &group_w(&[("ok.txt", 0, 0, 0), ("gone.txt", 0, 0, 0)], 2))
            .with(cf(CFSTR_FILECONTENTS), 0, As::HGlobal, b"ok");
        let (p, job) = planned(target::read(&source.into()));
        assert_eq!(names(&p.files), ["ok.txt"]);
        assert_eq!(run(job, Some(1)), Some(job::Done::Filled(Some(1), p.files.clone())));
        std::fs::remove_dir_all(drop_dir_of(&p.files)).unwrap();

        // Nothing readable: no Park.
        let source = Source::new(false).with(cf(CFSTR_FILEDESCRIPTORW), -1, As::HGlobal, &group_w(&[("gone.txt", 0, 0, 0)], 1));
        assert!(target::read(&source.into()).is_none());
    }

    #[test]
    fn entries_that_fail_on_the_worker_are_dropped_and_nothing_left_removes_the_item() {
        let source = Source::new(true)
            .with(cf(CFSTR_FILEDESCRIPTORW), -1, As::HGlobal, &group_w(&[("ok.txt", 0, 0, 0), ("gone.txt", 0, 0, 0)], 2))
            .with(cf(CFSTR_FILECONTENTS), 0, As::HGlobal, b"ok");
        let (p, job) = planned(target::read(&source.into()));
        assert_eq!(names(&p.files), ["ok.txt", "gone.txt"]);
        assert_eq!(run(job, Some(1)), Some(job::Done::Filled(Some(1), p.files[..1].to_vec())));
        assert!(!p.files[1].exists());
        std::fs::remove_dir_all(drop_dir_of(&p.files)).unwrap();

        let source = Source::new(true).with(cf(CFSTR_FILEDESCRIPTORW), -1, As::HGlobal, &group_w(&[("gone.txt", 0, 0, 0)], 1));
        let ended = source.ended();
        let (p, job) = planned(target::read(&source.into()));
        assert_eq!(run(job, Some(2)), Some(job::Done::Filled(Some(2), vec![])));
        assert!(!p.files[0].parent().unwrap().exists(), "the empty drop folder is removed");
        assert_eq!(*ended.borrow(), Some((E_FAIL, 0)));
    }

    #[test]
    fn a_delay_rendered_hdrop_is_read_after_start_operation() {
        let (src, paths) = files_in(dropdir::temp_dir(), "chrome_drag", &["report.pdf"]);
        let source = Source::new(true).delayed(CF_HDROP.0, &dropfiles(&paths));
        let ended = source.ended();
        let obj: IDataObject = source.into();
        assert!(target::supported(&obj));

        let Some(Park::Later(job)) = target::read(&obj) else { panic!("expected a later Park") };
        let Some(job::Done::Parked(p)) = run(job, None) else { panic!("expected a Park") };
        assert_eq!(p.kind, Kind::Materialized);
        assert_eq!(names(&p.files), ["report.pdf"]);
        assert_eq!(*ended.borrow(), Some((S_OK, COPY)));
        // Chromium deletes its chrome_drag folder later: ours is a copy.
        std::fs::remove_dir_all(src).unwrap();
        assert_eq!(std::fs::read(&p.files[0]).unwrap(), b"report.pdf");
        std::fs::remove_dir_all(p.files[0].parent().unwrap()).unwrap();

        // Without async mode a delay-rendered CF_HDROP can't be read: refused.
        let source = Source::new(false).delayed(CF_HDROP.0, &dropfiles(&[r"C:\x.pdf".into()]));
        assert!(target::read(&source.into()).is_none());
    }

    /// Plan, write, and return the one file with its bytes.
    fn one(obj: IDataObject) -> (String, Vec<u8>) {
        let (p, job) = planned(target::read(&obj));
        assert_eq!(p.files.len(), 1);
        assert_eq!(run(job, Some(1)), Some(job::Done::Filled(Some(1), p.files.clone())));
        let bytes = std::fs::read(&p.files[0]).unwrap();
        std::fs::remove_dir_all(p.files[0].parent().unwrap()).unwrap();
        (names(&p.files)[0].to_owned(), bytes)
    }

    #[test]
    fn a_url_becomes_an_internet_shortcut_named_by_its_title() {
        let url = "https://example.com/a?b=1";
        let source = Source::new(false)
            .with(cf(CFSTR_INETURLW), -1, As::HGlobal, &utf16z(url))
            .with(cf(w!("text/x-moz-url")), -1, As::HGlobal, &utf16z(&format!("{url}\nExample: A")));
        let (name, bytes) = one(source.into());
        assert_eq!(name, "Example_ A.url");
        assert_eq!(bytes, b"[InternetShortcut]\r\nURL=https://example.com/a?b=1\r\n");

        // ANSI, no title: named by the host.
        let source = Source::new(false).with(cf(CFSTR_INETURLA), -1, As::HGlobal, b"http://www.example.org/x\0");
        assert_eq!(one(source.into()).0, "www.example.org.url");
    }

    #[test]
    fn text_becomes_a_utf8_txt_named_from_its_first_line() {
        let source = Source::new(false).with(CF_UNICODETEXT.0, -1, As::HGlobal, &utf16z("Caffè list\r\nmilk\r\n"));
        let (name, bytes) = one(source.into());
        assert_eq!(name, "Caffè list.txt");
        assert_eq!(bytes, "Caffè list\r\nmilk\r\n".as_bytes());

        let source = Source::new(false).with(CF_TEXT.0, -1, As::HGlobal, b"hello\0garbage");
        assert_eq!(one(source.into()), ("hello.txt".to_owned(), b"hello".to_vec()));

        // Blank text is no Park.
        let source = Source::new(false).with(CF_UNICODETEXT.0, -1, As::HGlobal, &utf16z(" \r\n"));
        assert!(target::read(&source.into()).is_none());
    }

    #[test]
    fn png_bytes_become_a_png() {
        let png = b"\x89PNG\r\n\x1a\nrest";
        let (name, bytes) = one(Source::new(false).with(cf(w!("PNG")), -1, As::HGlobal, png).into());
        assert!(name.starts_with("Image ") && name.ends_with(".png"), "{name}");
        assert_eq!(bytes, png);
    }

    #[test]
    fn a_shell_idlist_alone_is_refused() {
        let obj: IDataObject = Source::new(false).with(cf(w!("Shell IDList Array")), -1, As::HGlobal, &[1, 0, 0, 0]).into();
        assert!(!target::supported(&obj));
        assert!(target::read(&obj).is_none());
    }

    #[test]
    fn readers_follow_the_spec_order() {
        let order: Vec<&str> = target::READERS.iter().map(|r| r.name).collect();
        assert_eq!(order, ["CF_HDROP", "async CF_HDROP", "FileGroupDescriptor", "UniformResourceLocator", "text", "PNG"]);
        // A browser link offers a virtual .url file, a URL and text: the file wins.
        let source = Source::new(false)
            .with(CF_UNICODETEXT.0, -1, As::HGlobal, &utf16z("https://example.com/"))
            .with(cf(CFSTR_INETURLW), -1, As::HGlobal, &utf16z("https://example.com/"))
            .with(cf(CFSTR_FILEDESCRIPTORW), -1, As::HGlobal, &group_w(&[("Example Domain.url", 0, 0, 0)], 1))
            .with(cf(CFSTR_FILECONTENTS), 0, As::HGlobal, b"[InternetShortcut]\r\nURL=https://example.com/\r\n");
        assert_eq!(one(source.into()).0, "Example Domain.url");
    }
}
