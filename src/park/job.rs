//! The Park workers (spec §4: slow work runs on a worker thread). One thread
//! per drop that needs one: it writes the planned Materialized files of a
//! pending Item, or reads an async drop, and reports. Reports wait in `DONE`;
//! `WM_PARK_DONE` only says "drain it" and carries no pointer, so a stray
//! `WM_APP+17` from another process is harmless.
//!
//! COM interfaces reach a worker marshalled (`Carried`). Workers live in the
//! MTA, so they need no message loop. When the source runs an async
//! operation for us (`IDataObjectAsyncCapability`), the worker ends it after
//! the reads: until then the source keeps its data alive.

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::mem::ManuallyDrop;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use windows::core::{IUnknown, Interface, HSTRING};
use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, S_OK, WPARAM};
use windows::Win32::System::Com::Marshal::CoMarshalInterThreadInterfaceInStream;
use windows::Win32::System::Com::StructuredStorage::{
    CoGetInterfaceAndReleaseStream, IStorage, StgCreateStorageEx, STGFMT_STORAGE,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, IBindCtx, IDataObject, IStream, COINIT_MULTITHREADED, STGC_DEFAULT, STGM,
    STGMEDIUM, STGM_READWRITE, STGM_SHARE_EXCLUSIVE, STREAM_SEEK_SET, TYMED_HGLOBAL, TYMED_ISTORAGE, TYMED_ISTREAM,
};
use windows::Win32::System::Ole::{ReleaseStgMedium, DROPEFFECT_COPY, DROPEFFECT_NONE};
use windows::Win32::UI::Shell::{IDataObjectAsyncCapability, CFSTR_FILECONTENTS};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::{formats, hdrop, Parked};
use crate::log::log;
use crate::proto::WM_PARK_DONE;
use crate::shelf::model::ItemId;

/// Streams are copied to disk in chunks of this size (Chromium's).
pub const CHUNK: usize = 16 * 1024 * 1024;

/// What a worker reports.
#[derive(Debug, PartialEq, Eq)]
pub enum Done {
    /// The pending Item's files that were written; none = the Item goes.
    Filled(ItemId, Vec<PathBuf>),
    /// An async drop, read: put it on the Shelf.
    Parked(Parked),
}

/// A COM interface marshalled for another thread of this process.
pub struct Carried(IStream);

// SAFETY: the stream `CoMarshalInterThreadInterfaceInStream` returns exists
// to be handed to another thread, which unmarshals it once (`take`).
unsafe impl Send for Carried {}

impl Carried {
    fn new<T: Interface>(i: &T) -> windows::core::Result<Self> {
        let unk: IUnknown = i.cast()?;
        unsafe { CoMarshalInterThreadInterfaceInStream(&T::IID, &unk) }.map(Self)
    }

    /// The interface, in the calling thread's apartment.
    fn take<T: Interface>(self) -> windows::core::Result<T> {
        // CoGetInterfaceAndReleaseStream releases the stream, success or not.
        let s = ManuallyDrop::new(self.0);
        unsafe { CoGetInterfaceAndReleaseStream(&*s) }
    }
}

enum What {
    Dir,
    Bytes(Vec<u8>),
    Stream(Carried),
    /// Saved as a compound file (a `.msg`).
    Storage(Carried),
    /// `FileContents[lindex]`, read through the data object of an async
    /// operation, with the descriptor's size.
    Contents(i32, Option<u64>),
    /// Already written in `Drop` (its interface could not be marshalled).
    Written,
}

/// One file or folder a job writes.
pub struct Entry {
    pub path: PathBuf,
    what: What,
}

impl Entry {
    pub fn bytes(path: PathBuf, bytes: Vec<u8>) -> Self {
        Self { path, what: What::Bytes(bytes) }
    }

    pub fn dir(path: PathBuf) -> Self {
        Self { path, what: What::Dir }
    }

    pub fn contents(path: PathBuf, lindex: i32, size: Option<u64>) -> Self {
        Self { path, what: What::Contents(lindex, size) }
    }

    /// From a `FileContents` medium taken in `Drop`: `HGLOBAL` bytes are
    /// copied now; a stream or storage is carried to the worker, or written
    /// now if it can't be marshalled.
    pub fn medium(path: PathBuf, m: STGMEDIUM, size: Option<u64>) -> io::Result<Self> {
        let m = Medium(m);
        let carried = match m.0.tymed {
            t if t == TYMED_HGLOBAL.0 as u32 => {
                return Ok(Self::bytes(path, formats::global_bytes(unsafe { m.0.u.hGlobal }, size)))
            }
            t if t == TYMED_ISTREAM.0 as u32 => Carried::new(m.stream()?).map(What::Stream),
            t if t == TYMED_ISTORAGE.0 as u32 => Carried::new(m.storage()?).map(What::Storage),
            _ => return Err(m.unexpected()),
        };
        let what = match carried {
            Ok(w) => w,
            Err(e) => {
                log(format!("not marshalled ({e}), writing {} in Drop", path.display()));
                m.write(&path, size, &mut Vec::new())?;
                What::Written
            }
        };
        Ok(Self { path, what })
    }
}

/// A medium, released when dropped.
struct Medium(STGMEDIUM);

impl Medium {
    fn stream(&self) -> io::Result<&IStream> {
        unsafe { (*self.0.u.pstm).as_ref() }.ok_or_else(|| io::Error::other("no stream"))
    }

    fn storage(&self) -> io::Result<&IStorage> {
        unsafe { (*self.0.u.pstg).as_ref() }.ok_or_else(|| io::Error::other("no storage"))
    }

    fn unexpected(&self) -> io::Error {
        io::Error::other(format!("unexpected medium (tymed {})", self.0.tymed))
    }

    /// Write the contents to a new file at `path` (`buf`: the stream buffer).
    fn write(&self, path: &Path, size: Option<u64>, buf: &mut Vec<u8>) -> io::Result<()> {
        match self.0.tymed {
            t if t == TYMED_HGLOBAL.0 as u32 => {
                new_file(path)?.write_all(&formats::global_bytes(unsafe { self.0.u.hGlobal }, size))
            }
            t if t == TYMED_ISTREAM.0 as u32 => copy_stream(self.stream()?, path, buf),
            t if t == TYMED_ISTORAGE.0 as u32 => save_storage(self.storage()?, path),
            _ => Err(self.unexpected()),
        }
    }
}

impl Drop for Medium {
    fn drop(&mut self) {
        unsafe { ReleaseStgMedium(&mut self.0) };
    }
}

/// The planned files of one pending Item.
pub struct Writes {
    /// The drop folder.
    pub dir: PathBuf,
    /// The Item's (top-level) paths.
    pub files: Vec<PathBuf>,
    pub entries: Vec<Entry>,
    /// The data object of an async operation (the worker ends it).
    pub source: Option<Carried>,
}

pub enum Job {
    /// Write the planned files of the pending Item.
    Write(ItemId, Writes),
    /// Rule 2: read `CF_HDROP` from the data object of an async operation.
    Hdrop(Carried),
}

/// Run `job` on a new worker thread; `report` gets its result there.
pub fn spawn(job: Job, report: impl FnOnce(Option<Done>) + Send + 'static) -> io::Result<()> {
    std::thread::Builder::new().name("tasca-park".into()).spawn(move || report(run(job))).map(|_| ())
}

static DONE: Mutex<Vec<Done>> = Mutex::new(Vec::new());

/// A worker's report for the UI thread: queue it and post `WM_PARK_DONE`.
pub fn deliver(hwnd: isize, done: Option<Done>) {
    if let Some(d) = done {
        DONE.lock().unwrap_or_else(|e| e.into_inner()).push(d);
        let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_PARK_DONE, WPARAM(0), LPARAM(0)) };
    }
}

/// Everything reported so far (UI thread, on `WM_PARK_DONE`).
pub fn take() -> Vec<Done> {
    std::mem::take(&mut *DONE.lock().unwrap_or_else(|e| e.into_inner()))
}

/// This thread in the MTA for as long as it lives.
struct Mta(bool);

impl Mta {
    fn enter() -> Self {
        Self(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for Mta {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

/// The drop's data object, on the worker.
fn arrive(c: Carried) -> Option<IDataObject> {
    c.take().inspect_err(|e| log(format!("the drop did not reach the worker: {e}"))).ok()
}

/// The first component of `p` under `dir`.
fn top<'a>(dir: &Path, p: &'a Path) -> Option<Component<'a>> {
    p.strip_prefix(dir).ok()?.components().next()
}

fn run(job: Job) -> Option<Done> {
    let _mta = Mta::enter();
    match job {
        Job::Write(item, Writes { dir, files, entries, source }) => {
            let obj = source.and_then(arrive);
            let mut buf = Vec::new();
            let mut written = HashSet::new();
            for e in entries {
                match write(&e.path, e.what, obj.as_ref(), &mut buf) {
                    Ok(()) => {
                        written.extend(top(&dir, &e.path).map(|c| c.as_os_str().to_owned()));
                    }
                    Err(err) => {
                        log(format!("writing {} failed: {err}", e.path.display()));
                        if e.path.is_file() {
                            let _ = std::fs::remove_file(&e.path);
                        }
                    }
                }
            }
            let files: Vec<PathBuf> =
                files.into_iter().filter(|f| top(&dir, f).is_some_and(|c| written.contains(c.as_os_str()))).collect();
            if files.is_empty() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            if let Some(o) = &obj {
                end(o, !files.is_empty());
            }
            Some(Done::Filled(item, files))
        }
        Job::Hdrop(c) => {
            let obj = arrive(c)?;
            let p = hdrop::read_and_keep(&obj);
            end(&obj, p.is_some());
            p.map(Done::Parked)
        }
    }
}

fn write(path: &Path, what: What, obj: Option<&IDataObject>, buf: &mut Vec<u8>) -> io::Result<()> {
    match what {
        What::Dir => std::fs::create_dir_all(path),
        What::Bytes(b) => new_file(path)?.write_all(&b),
        What::Stream(c) => copy_stream(&c.take().map_err(io::Error::other)?, path, buf),
        What::Storage(c) => save_storage(&c.take().map_err(io::Error::other)?, path),
        What::Contents(lindex, size) => {
            let obj = obj.ok_or_else(|| io::Error::other("no data object"))?;
            Medium(file_contents(obj, lindex)?).write(path, size, buf)
        }
        What::Written => Ok(()),
    }
}

/// `FileContents[lindex]`, as a stream, an `HGLOBAL` or a storage.
pub fn file_contents(obj: &IDataObject, lindex: i32) -> io::Result<STGMEDIUM> {
    let tymed = (TYMED_ISTREAM.0 | TYMED_HGLOBAL.0 | TYMED_ISTORAGE.0) as u32;
    let f = formats::format(formats::registered(CFSTR_FILECONTENTS), lindex, tymed);
    unsafe { obj.GetData(&f) }.map_err(io::Error::other)
}

/// The folder for a new file.
fn parent_dirs(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(p) => std::fs::create_dir_all(p),
        None => Ok(()),
    }
}

/// A new file (never an existing one), its folders created as needed.
fn new_file(path: &Path) -> io::Result<std::fs::File> {
    parent_dirs(path)?;
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Copy a stream from its start to a new file, in `CHUNK`s through `buf`
/// (one buffer per job). The size is not trusted: read until the stream ends.
fn copy_stream(s: &IStream, path: &Path, buf: &mut Vec<u8>) -> io::Result<()> {
    // A source may hand the stream over at its end; not every stream seeks.
    let _ = unsafe { s.Seek(0, STREAM_SEEK_SET, None) };
    let mut f = new_file(path)?;
    buf.resize(CHUNK, 0);
    loop {
        let mut n = 0u32;
        unsafe { s.Read(buf.as_mut_ptr() as _, CHUNK as u32, Some(&mut n)) }.ok().map_err(io::Error::other)?;
        if n == 0 {
            return Ok(());
        }
        f.write_all(&buf[..n as usize])?;
    }
}

/// Save a storage as a new compound file (Outlook's `.msg`).
fn save_storage(src: &IStorage, path: &Path) -> io::Result<()> {
    parent_dirs(path)?;
    unsafe {
        let mut raw = std::ptr::null_mut();
        let mode = STGM(STGM_READWRITE.0 | STGM_SHARE_EXCLUSIVE.0); // fails if the file exists
        StgCreateStorageEx(&HSTRING::from(path), mode, STGFMT_STORAGE, 0, None, None, &IStorage::IID, &mut raw)
            .map_err(io::Error::other)?;
        let dst = IStorage::from_raw(raw);
        src.CopyTo(None, None, &dst).map_err(io::Error::other)?;
        dst.Commit(STGC_DEFAULT.0 as u32).map_err(io::Error::other)
    }
}

/// If the source is in async mode: marshal the data object for a worker,
/// then `StartOperation` (the worker ends it). `None`: read in `Drop`, which
/// is also the fallback when the object can't be marshalled.
pub fn start_async(obj: &IDataObject) -> Option<Carried> {
    let cap = obj.cast::<IDataObjectAsyncCapability>().ok()?;
    if !unsafe { cap.GetAsyncMode() }.is_ok_and(|m| m.as_bool()) {
        return None;
    }
    let c = Carried::new(obj).inspect_err(|e| log(format!("the drop can't be marshalled, reading in Drop: {e}"))).ok()?;
    match unsafe { cap.StartOperation(None::<&IBindCtx>) } {
        Ok(()) => Some(c),
        Err(e) => {
            log(format!("StartOperation failed, reading in Drop: {e}"));
            None
        }
    }
}

/// End the source's async operation: `COPY` if anything was Parked.
fn end(obj: &IDataObject, ok: bool) {
    let (hr, effect) = if ok { (S_OK, DROPEFFECT_COPY) } else { (E_FAIL, DROPEFFECT_NONE) };
    let r = obj.cast::<IDataObjectAsyncCapability>().and_then(|cap| unsafe { cap.EndOperation(hr, None::<&IBindCtx>, effect.0) });
    if let Err(e) = r {
        log(format!("EndOperation failed: {e}"));
    }
}
