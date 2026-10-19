//! Row thumbnails (spec §6). One long-lived STA worker asks the shell for each
//! Item's image (`IShellItemImageFactory::GetImage`), copies the pixels out
//! with `GetDIBits`, normalises the alpha and parks the result in `CACHE`.
//! `WM_THUMB_READY` only says "look in the cache" and carries no pointer, so a
//! stray `WM_APP+16` from another process is harmless. The UI thread never
//! calls the shell: it only reads the cache and draws.
//!
//! The cache is per Item per DPI. Failures leave no entry, so the tile
//! fallback keeps showing.

use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use windows::core::HSTRING;
use windows::Win32::Foundation::{HWND, LPARAM, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC,
    SelectObject, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON,
    SHGFI_USEFILEATTRIBUTES, SIIGBF, SIIGBF_CROPTOSQUARE, SIIGBF_RESIZETOFIT,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, DrawIconEx, PostMessageW, DI_NORMAL};

use crate::log::{log, now_ms};
use crate::proto::WM_THUMB_READY;
use crate::shelf::model::ItemId;

/// Tile edge in DIP (same as `shelf::tiles::TILE`).
const TILE_DIP: f32 = 30.0;

/// One cache entry: an Item at a DPI (`dpi` = the render target's DPI, rounded).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub id: ItemId,
    pub dpi: u32,
}

impl Key {
    pub fn new(id: ItemId, dpi_x: f32) -> Self {
        Self { id, dpi: dpi_x.round() as u32 }
    }
}

/// Premultiplied BGRA pixels, top-down, `w * h * 4` bytes.
pub struct Thumb {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

/// Physical pixels for a 30 DIP tile at `scale` (DPI / 96).
pub fn tile_px(scale: f32) -> u32 {
    (TILE_DIP * scale).round().max(1.0) as u32
}

/// The shell does not say whether the alpha is premultiplied. All-zero alpha
/// means the image carries none: make it opaque. Otherwise premultiply.
pub fn normalize_alpha(bgra: &mut [u8]) {
    if bgra.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
        for p in bgra.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
        return;
    }
    for p in bgra.as_chunks_mut::<4>().0 {
        let a = p[3] as u32;
        if a != 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
    }
}

/// Finished thumbnails and requests in flight.
#[derive(Default)]
pub struct Cache {
    ready: HashMap<Key, Arc<Thumb>>,
    pending: HashSet<Key>,
}

impl Cache {
    /// True when `key` still needs a request (and marks it in flight).
    pub fn want(&mut self, key: Key) -> bool {
        !self.ready.contains_key(&key) && self.pending.insert(key)
    }

    pub fn finish(&mut self, key: Key, thumb: Option<Thumb>) {
        self.pending.remove(&key);
        if let Some(t) = thumb {
            self.ready.insert(key, Arc::new(t));
        }
    }

    pub fn get(&self, key: Key) -> Option<Arc<Thumb>> {
        self.ready.get(&key).cloned()
    }

    pub fn has(&self, key: Key) -> bool {
        self.ready.contains_key(&key)
    }

    /// Keep entries of live Items at `dpi`: a removed Item or a DPI left behind frees its pixels.
    pub fn retain(&mut self, dpi: u32, live: impl Fn(ItemId) -> bool) {
        self.ready.retain(|k, _| k.dpi == dpi && live(k.id));
    }
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

fn cache<R>(f: impl FnOnce(&mut Cache) -> R) -> R {
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Cache::default))
}

pub fn get(key: Key) -> Option<Arc<Thumb>> {
    cache(|c| c.get(key))
}

pub fn has(key: Key) -> bool {
    cache(|c| c.has(key))
}

pub fn retain(dpi: u32, live: impl Fn(ItemId) -> bool) {
    cache(|c| c.retain(dpi, live));
}

struct Request {
    key: Key,
    path: PathBuf,
    px: u32,
    is_image: bool,
    /// The window to post to, as an integer (`HWND` is not `Send`).
    hwnd: isize,
}

static WORKER: OnceLock<Sender<Request>> = OnceLock::new();

fn worker() -> &'static Sender<Request> {
    WORKER.get_or_init(|| {
        let (tx, rx) = channel();
        if let Err(e) = std::thread::Builder::new().name("tasca-thumbs".into()).spawn(move || run(rx)) {
            // The receiver went down with the closure, so every send fails.
            log(format!("thumbs: could not start the worker: {e}"));
        }
        tx
    })
}

/// Queue the Items that have no thumbnail (or request) yet at `scale`.
/// `items` = (id, first file, is an image). Never blocks.
pub fn request(hwnd: HWND, scale: f32, items: Vec<(ItemId, PathBuf, bool)>) {
    let px = tile_px(scale);
    for (id, path, is_image) in items {
        let key = Key::new(id, scale * 96.0);
        if !cache(|c| c.want(key)) {
            continue;
        }
        let req = Request { key, path, px, is_image, hwnd: hwnd.0 as isize };
        if worker().send(req).is_err() {
            log("thumbs: worker unavailable");
            cache(|c| c.finish(key, None));
        }
    }
}

fn run(rx: Receiver<Request>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    for r in rx {
        let t = now_ms();
        let thumb = shell_image(&r.path, r.px, r.is_image).or_else(|| type_icon(&r.path, r.px));
        log(format!(
            "thumb {} {}: {} in {:.1} ms",
            r.key.id,
            r.path.display(),
            thumb.as_ref().map_or("none".to_string(), |t| format!("{}x{}", t.w, t.h)),
            now_ms() - t
        ));
        cache(|c| c.finish(r.key, thumb));
        let _ = unsafe { PostMessageW(Some(HWND(r.hwnd as *mut _)), WM_THUMB_READY, WPARAM(0), LPARAM(0)) };
    }
}

/// The file's thumbnail or icon from the shell. Blocking: worker thread only.
fn shell_image(path: &Path, px: u32, is_image: bool) -> Option<Thumb> {
    unsafe {
        let item: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None).ok()?;
        let mut flags = SIIGBF_RESIZETOFIT;
        if is_image {
            flags = SIIGBF(flags.0 | SIIGBF_CROPTOSQUARE.0);
        }
        let hbm = item.GetImage(SIZE { cx: px as i32, cy: px as i32 }, flags).ok()?;
        let out = read_hbitmap(hbm);
        let _ = DeleteObject(HGDIOBJ(hbm.0));
        out
    }
}

/// Copy an HBITMAP's pixels out as normalised BGRA.
unsafe fn read_hbitmap(hbm: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<Thumb> {
    let mut bm = BITMAP::default();
    if GetObjectW(HGDIOBJ(hbm.0), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
    if w <= 0 || h <= 0 {
        return None;
    }
    let mut bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bgra = vec![0u8; (w * h * 4) as usize];
    let dc = GetDC(None);
    let lines = GetDIBits(dc, hbm, 0, h as u32, Some(bgra.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);
    ReleaseDC(None, dc);
    if lines == 0 {
        return None;
    }
    normalize_alpha(&mut bgra);
    Some(Thumb { w: w as u32, h: h as u32, bgra })
}

/// The type's icon from the extension alone (a vanished path). Worker thread only.
fn type_icon(path: &Path, px: u32) -> Option<Thumb> {
    unsafe {
        let name = match path.extension() {
            Some(e) => format!("x.{}", e.to_string_lossy()),
            None => "x".to_string(),
        };
        let mut info = SHFILEINFOW::default();
        let ok = SHGetFileInfoW(
            &HSTRING::from(name),
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES,
        );
        if ok == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let side = px as i32;
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: side,
                biHeight: -side,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0);
        let mut out = None;
        if let Ok(dib) = dib {
            let dc = CreateCompatibleDC(None);
            let old = SelectObject(dc, HGDIOBJ(dib.0));
            if DrawIconEx(dc, 0, 0, info.hIcon, side, side, 0, None, DI_NORMAL).is_ok() && !bits.is_null() {
                let mut bgra = std::slice::from_raw_parts(bits as *const u8, (side * side * 4) as usize).to_vec();
                // A pre-alpha icon leaves alpha 0 where it drew: opaque is right for it.
                normalize_alpha(&mut bgra);
                out = Some(Thumb { w: px, h: px, bgra });
            }
            SelectObject(dc, old);
            let _ = DeleteDC(dc);
            let _ = DeleteObject(HGDIOBJ(dib.0));
        }
        let _ = DestroyIcon(info.hIcon);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_zero_alpha_becomes_opaque() {
        let mut p = vec![10, 20, 30, 0, 40, 50, 60, 0];
        normalize_alpha(&mut p);
        assert_eq!(p, vec![10, 20, 30, 255, 40, 50, 60, 255]);
    }

    #[test]
    fn mixed_alpha_is_premultiplied() {
        let mut p = vec![200, 100, 50, 128, 10, 20, 30, 255, 90, 90, 90, 0];
        normalize_alpha(&mut p);
        assert_eq!(p[..4], [100, 50, 25, 128]);
        assert_eq!(p[4..8], [10, 20, 30, 255], "opaque pixels are untouched");
        assert_eq!(p[8..], [0, 0, 0, 0], "transparent pixels go to zero");
    }

    #[test]
    fn tile_size_follows_scale() {
        assert_eq!(tile_px(1.0), 30);
        assert_eq!(tile_px(1.5), 45);
        assert_eq!(tile_px(1.25), 38);
    }

    #[test]
    fn cache_is_per_item_per_dpi() {
        let mut c = Cache::default();
        let k96 = Key::new(7, 96.0);
        let k144 = Key::new(7, 144.0);
        assert_ne!(k96, k144);
        assert!(c.want(k96));
        assert!(!c.want(k96), "already in flight");
        c.finish(k96, Some(Thumb { w: 1, h: 1, bgra: vec![0, 0, 0, 255] }));
        assert!(c.has(k96) && !c.has(k144));
        assert!(!c.want(k96), "already cached");
        assert!(c.want(k144), "another DPI is a miss");
    }

    #[test]
    fn a_failure_leaves_no_entry_and_can_be_retried() {
        let mut c = Cache::default();
        let k = Key::new(1, 96.0);
        assert!(c.want(k));
        c.finish(k, None);
        assert!(!c.has(k));
        assert!(c.want(k));
    }

    #[test]
    fn retain_drops_other_dpis_and_removed_items() {
        let mut c = Cache::default();
        for key in [Key::new(1, 96.0), Key::new(1, 144.0), Key::new(2, 144.0)] {
            c.finish(key, Some(Thumb { w: 1, h: 1, bgra: vec![0; 4] }));
        }
        c.retain(144, |id| id == 1);
        assert!(c.has(Key::new(1, 144.0)));
        assert!(!c.has(Key::new(1, 96.0)));
        assert!(!c.has(Key::new(2, 144.0)));
    }
}
