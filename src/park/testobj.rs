//! A hand-built drag source for tests: an `IDataObject` that hands out what
//! it was given, by format and `lindex`, as an `HGLOBAL` or an `IStream`, and
//! optionally the async capability (`IDataObjectAsyncCapability`) with a
//! delay-rendered format that only renders once `StartOperation` was called.
//!
//! Not agile, like a real source's proxy: a worker thread reaches it through
//! COM marshalling, so the test thread must pump messages (`pump_until`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use windows::core::{implement, Ref, Result, BOOL, HRESULT};
use windows::Win32::Foundation::{DV_E_FORMATETC, E_NOTIMPL, OLE_E_ADVISENOTSUPPORTED, S_OK};
use windows::Win32::System::Com::{
    IAdviseSink, IBindCtx, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, FORMATETC, STGMEDIUM,
    STGMEDIUM_0, STREAM_SEEK_END, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::UI::Shell::{IDataObjectAsyncCapability, IDataObjectAsyncCapability_Impl, SHCreateMemStream};
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE};

/// What the source was told at `EndOperation`: result and effect.
pub type Ended = Rc<RefCell<Option<(HRESULT, u32)>>>;

/// How one format is handed out.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum As {
    HGlobal,
    /// A stream left at its end, as some sources do: readers must seek to 0.
    Stream,
}

struct Offer {
    cf: u16,
    lindex: i32,
    how: As,
    bytes: Vec<u8>,
    /// Renders only after `StartOperation` (Chromium's `DownloadURL`).
    delayed: bool,
}

#[implement(IDataObject, IDataObjectAsyncCapability, Agile = false)]
pub struct Source {
    offers: RefCell<Vec<Offer>>,
    async_mode: bool,
    started: Cell<bool>,
    /// `EndOperation`'s result and effect, shared with the test.
    ended: Ended,
}

impl Source {
    pub fn new(async_mode: bool) -> Self {
        unsafe {
            let _ = OleInitialize(None);
        }
        Self { offers: RefCell::new(Vec::new()), async_mode, started: Cell::new(false), ended: Ended::default() }
    }

    pub fn with(self, cf: u16, lindex: i32, how: As, bytes: &[u8]) -> Self {
        self.offers.borrow_mut().push(Offer { cf, lindex, how, bytes: bytes.to_vec(), delayed: false });
        self
    }

    /// Offered during the drag, but renders only once an async operation started.
    pub fn delayed(self, cf: u16, bytes: &[u8]) -> Self {
        self.offers.borrow_mut().push(Offer { cf, lindex: -1, how: As::HGlobal, bytes: bytes.to_vec(), delayed: true });
        self
    }

    /// Where `EndOperation` will be recorded.
    pub fn ended(&self) -> Ended {
        self.ended.clone()
    }

    fn find(&self, f: &FORMATETC) -> Option<usize> {
        self.offers.borrow().iter().position(|o| {
            let tymed = match o.how {
                As::HGlobal => TYMED_HGLOBAL.0 as u32,
                As::Stream => TYMED_ISTREAM.0 as u32,
            };
            o.cf == f.cfFormat && (o.lindex == -1 || o.lindex == f.lindex) && f.tymed & tymed != 0
        })
    }
}

pub fn hglobal(bytes: &[u8]) -> STGMEDIUM {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)).unwrap();
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), GlobalLock(h) as *mut u8, bytes.len());
        let _ = GlobalUnlock(h);
        STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: h }, pUnkForRelease: ManuallyDrop::new(None) }
    }
}

impl IDataObject_Impl for Source_Impl {
    fn GetData(&self, f: *const FORMATETC) -> Result<STGMEDIUM> {
        let f = unsafe { &*f };
        let i = self.find(f).ok_or(DV_E_FORMATETC)?;
        let offers = self.offers.borrow();
        let o = &offers[i];
        if o.delayed && !self.started.get() {
            return Err(DV_E_FORMATETC.into());
        }
        Ok(match o.how {
            As::HGlobal => hglobal(&o.bytes),
            As::Stream => unsafe {
                let s = SHCreateMemStream(Some(&o.bytes)).unwrap();
                s.Seek(0, STREAM_SEEK_END, None)?;
                STGMEDIUM { tymed: TYMED_ISTREAM.0 as u32, u: STGMEDIUM_0 { pstm: ManuallyDrop::new(Some(s)) }, pUnkForRelease: ManuallyDrop::new(None) }
            },
        })
    }

    fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, f: *const FORMATETC) -> HRESULT {
        if self.find(unsafe { &*f }).is_some() { S_OK } else { DV_E_FORMATETC }
    }

    fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, _: *mut FORMATETC) -> HRESULT {
        E_NOTIMPL
    }

    fn SetData(&self, _: *const FORMATETC, _: *const STGMEDIUM, _: BOOL) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, _: u32) -> Result<IEnumFORMATETC> {
        Err(E_NOTIMPL.into())
    }

    fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Ref<IAdviseSink>) -> Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _: u32) -> Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

impl IDataObjectAsyncCapability_Impl for Source_Impl {
    fn SetAsyncMode(&self, _: BOOL) -> Result<()> {
        Ok(())
    }

    fn GetAsyncMode(&self) -> Result<BOOL> {
        Ok(self.async_mode.into())
    }

    fn StartOperation(&self, _: Ref<IBindCtx>) -> Result<()> {
        self.started.set(true);
        Ok(())
    }

    fn InOperation(&self) -> Result<BOOL> {
        Ok(self.started.get().into())
    }

    fn EndOperation(&self, hr: HRESULT, _: Ref<IBindCtx>, effect: u32) -> Result<()> {
        self.started.set(false);
        *self.ended.borrow_mut() = Some((hr, effect));
        Ok(())
    }
}

/// Pump this thread's messages (COM calls from the worker arrive that way)
/// until `done` gives something, for at most 20 s.
pub fn pump_until<T>(mut done: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if let Some(v) = done() {
            return v;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "the worker never finished");
        std::thread::sleep(Duration::from_millis(2));
    }
}

