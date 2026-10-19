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

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use windows::core::{IUnknown, Interface, HSTRING};
use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, S_OK, WPARAM};
use windows::Win32::System::Com::Marshal::CoMarshalInterThreadInterfaceInStream;
use windows::Win32::System::Com::StructuredStorage::{
    CoGetInterfaceAndReleaseStream, IStorage, StgCreateStorageEx, STGFMT_STORAGE,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, IBindCtx, IDataObject, IStream, COINIT_MULTITHREADED, DVASPECT_CONTENT, FORMATETC,
    STGC_DEFAULT, STGM, STGMEDIUM, STGM_READWRITE, STGM_SHARE_EXCLUSIVE, STREAM_SEEK_SET, TYMED_HGLOBAL, TYMED_ISTORAGE,
    TYMED_ISTREAM,
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
    Filled(Option<ItemId>, Vec<PathBuf>),
    /// An async drop, read: put it on the Shelf.
    Parked(Parked),
}

/// A COM interface marshalled for another thread of this process.
pub struct Carried(IStream);

// SAFETY: the stream `CoMarshalInterThreadInterfaceInStream` returns exists
// to be handed to another thread, which unmarshals it once (`take`).
unsafe impl Send for Carried {}

impl Carried {
    pub fn new<T: Interface>(i: &T) -> windows::core::Result<Self> {
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
        let what = match m.0.tymed {
            t if t == TYMED_HGLOBAL.0 as u32 => What::Bytes(formats::global_bytes(unsafe { m.0.u.hGlobal }, size)),
            t if t == TYMED_ISTREAM.0 as u32 => {
                let s = m.stream()?;
                match Carried::new(s) {
                    Ok(c) => What::Stream(c),
                    Err(e) => {
                        log(format!("stream not marshalled ({e}), copying {} in Drop", path.display()));
                        copy_stream(s, &path)?;
                        What::Written
                    }
                }
            }
            t if t == TYMED_ISTORAGE.0 as u32 => {
                let s = m.storage()?;
                match Carried::new(s) {
                    Ok(c) => What::Storage(c),
                    Err(e) => {
                        log(format!("storage not marshalled ({e}), saving {} in Drop", path.display()));
                        save_storage(s, &path)?;
                        What::Written
                    }
                }
            }
            t => return Err(io::Error::other(format!("unexpected medium (tymed {t})"))),
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
}

impl Drop for Medium {
    fn drop(&mut self) {
        unsafe { ReleaseStgMedium(&mut self.0) };
    }
}

pub enum Job {
    /// Write `entries` into the drop folder `dir`; `files` are the Item's
    /// (top-level) paths. `source`: the data object of an async operation.
    Write { dir: PathBuf, files: Vec<PathBuf>, entries: Vec<Entry>, source: Option<Carried> },
    /// Rule 2: read `CF_HDROP` from the data object of an async operation.
    Hdrop(Carried),
}

impl Job {
    pub fn write(dir: PathBuf, files: Vec<PathBuf>, entries: Vec<Entry>, source: Option<Carried>) -> Self {
        Job::Write { dir, files, entries, source }
    }
}

/// Run `job` on a new worker thread; `report` gets its result there.
pub fn spawn(job: Job, item: Option<ItemId>, report: impl FnOnce(Option<Done>) + Send + 'static) -> io::Result<()> {
    std::thread::Builder::new().name("tasca-park".into()).spawn(move || report(run(job, item))).map(|_| ())
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

fn run(job: Job, item: Option<ItemId>) -> Option<Done> {
    let _mta = Mta::enter();
    match job {
        Job::Write { dir, files, entries, source } => {
            let obj: Option<IDataObject> =
                source.and_then(|c| c.take().inspect_err(|e| log(format!("the drop did not reach the worker: {e}"))).ok());
            let written: Vec<PathBuf> = entries
                .into_iter()
                .filter_map(|e| match write(&e.path, e.what, obj.as_ref()) {
                    Ok(()) => Some(e.path),
                    Err(err) => {
                        log(format!("writing {} failed: {err}", e.path.display()));
                        if e.path.is_file() {
                            let _ = std::fs::remove_file(&e.path);
                        }
                        None
                    }
                })
                .collect();
            let files: Vec<PathBuf> = files.into_iter().filter(|f| written.iter().any(|w| w.starts_with(f))).collect();
            if files.is_empty() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            if let Some(o) = &obj {
                end_object(o, !files.is_empty());
            }
            Some(Done::Filled(item, files))
        }
        Job::Hdrop(c) => {
            let obj: IDataObject = c.take().inspect_err(|e| log(format!("the drop did not reach the worker: {e}"))).ok()?;
            let p = hdrop::park(&obj);
            end_object(&obj, p.is_some());
            p.map(Done::Parked)
        }
    }
}

fn write(path: &Path, what: What, obj: Option<&IDataObject>) -> io::Result<()> {
    match what {
        What::Dir => std::fs::create_dir_all(path),
        What::Bytes(b) => new_file(path)?.write_all(&b),
        What::Stream(c) => copy_stream(&c.take().map_err(io::Error::other)?, path),
        What::Storage(c) => save_storage(&c.take().map_err(io::Error::other)?, path),
        What::Contents(lindex, size) => {
            let obj = obj.ok_or_else(|| io::Error::other("no data object"))?;
            write_medium(file_contents(obj, lindex)?, path, size)
        }
        What::Written => Ok(()),
    }
}

/// `FileContents[lindex]`, as a stream, an `HGLOBAL` or a storage.
pub fn file_contents(obj: &IDataObject, lindex: i32) -> io::Result<STGMEDIUM> {
    let f = FORMATETC {
        cfFormat: formats::registered(CFSTR_FILECONTENTS),
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex,
        tymed: (TYMED_ISTREAM.0 | TYMED_HGLOBAL.0 | TYMED_ISTORAGE.0) as u32,
    };
    unsafe { obj.GetData(&f) }.map_err(io::Error::other)
}

fn write_medium(m: STGMEDIUM, path: &Path, size: Option<u64>) -> io::Result<()> {
    let m = Medium(m);
    match m.0.tymed {
        t if t == TYMED_HGLOBAL.0 as u32 => new_file(path)?.write_all(&formats::global_bytes(unsafe { m.0.u.hGlobal }, size)),
        t if t == TYMED_ISTREAM.0 as u32 => copy_stream(m.stream()?, path),
        t if t == TYMED_ISTORAGE.0 as u32 => save_storage(m.storage()?, path),
        t => Err(io::Error::other(format!("unexpected medium (tymed {t})"))),
    }
}

/// A new file (never an existing one), its folders created as needed.
fn new_file(path: &Path) -> io::Result<std::fs::File> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Copy a stream from its start to a new file, in `CHUNK`s. The size is not
/// trusted: read until the stream ends.
fn copy_stream(s: &IStream, path: &Path) -> io::Result<()> {
    // A source may hand the stream over at its end; not every stream seeks.
    let _ = unsafe { s.Seek(0, STREAM_SEEK_SET, None) };
    let mut f = new_file(path)?;
    let mut buf = vec![0u8; CHUNK];
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
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
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

/// How a drop's async operation (`IDataObjectAsyncCapability`) went.
pub enum Async {
    /// The source has none: read in `Drop`.
    No,
    /// Started; the data object is carried to a worker, which ends it.
    Carried(Carried),
    /// Started, but the data object could not be marshalled: read in `Drop`,
    /// then `end`.
    Here(IDataObjectAsyncCapability),
}

/// Start an async operation if the source is in async mode.
pub fn start_async(obj: &IDataObject) -> Async {
    let Ok(cap) = obj.cast::<IDataObjectAsyncCapability>() else { return Async::No };
    if !unsafe { cap.GetAsyncMode() }.is_ok_and(|m| m.as_bool()) {
        return Async::No;
    }
    if let Err(e) = unsafe { cap.StartOperation(None::<&IBindCtx>) } {
        log(format!("StartOperation failed, reading in Drop: {e}"));
        return Async::No;
    }
    match Carried::new(obj) {
        Ok(c) => Async::Carried(c),
        Err(e) => {
            log(format!("the drop could not be marshalled, reading in Drop: {e}"));
            Async::Here(cap)
        }
    }
}

/// End the source's async operation: `COPY` if anything was Parked.
pub fn end(cap: &IDataObjectAsyncCapability, ok: bool) {
    let (hr, effect) = if ok { (S_OK, DROPEFFECT_COPY) } else { (E_FAIL, DROPEFFECT_NONE) };
    if let Err(e) = unsafe { cap.EndOperation(hr, None::<&IBindCtx>, effect.0) } {
        log(format!("EndOperation failed: {e}"));
    }
}

fn end_object(obj: &IDataObject, ok: bool) {
    match obj.cast::<IDataObjectAsyncCapability>() {
        Ok(cap) => end(&cap, ok),
        Err(e) => log(format!("no async capability to end: {e}")),
    }
}
