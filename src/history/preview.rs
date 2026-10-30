//! The 44 x 28 DIP preview on an image row. The PNG is decoded and cut to size
//! on one worker thread, never on the UI thread; the result waits in a cache
//! per image (its pixel hash) and per DPI, like the Shelf's thumbnails.
//! `WM_HISTORY_PREVIEW` only says "look in the cache".

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::image::{self, Bitmap};
use crate::log::log;
use crate::proto::WM_HISTORY_PREVIEW;

/// The preview's size in DIP.
pub const W_DIP: f32 = 44.0;
pub const H_DIP: f32 = 28.0;

/// One cache entry: an image at a DPI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub hash: u64,
    pub dpi: u32,
}

impl Key {
    pub fn new(hash: u64, scale: f32) -> Self {
        Self { hash, dpi: (scale * 96.0).round() as u32 }
    }
}

/// Premultiplied BGRA pixels, top-down, `w * h * 4` bytes.
pub struct Preview {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

/// The preview's size in physical pixels at `scale` (DPI / 96).
pub fn size_px(scale: f32) -> (u32, u32) {
    ((W_DIP * scale).round().max(1.0) as u32, (H_DIP * scale).round().max(1.0) as u32)
}

/// `src` scaled to fill `w` x `h` and cropped to the middle (CSS `cover`),
/// averaging the source pixels each output pixel covers. Output is
/// premultiplied. Pure.
pub fn cover(src: &Bitmap, w: u32, h: u32) -> Vec<u8> {
    let (sw, sh) = (src.width as f32, src.height as f32);
    let scale = (w as f32 / sw).max(h as f32 / sh);
    // The part of the source that is shown, in source pixels.
    let (vw, vh) = (w as f32 / scale, h as f32 / scale);
    let (ox, oy) = ((sw - vw) / 2.0, (sh - vh) / 2.0);
    let (step_x, step_y) = (vw / w as f32, vh / h as f32);
    let mut out = vec![0u8; w as usize * h as usize * 4];
    for y in 0..h {
        let (y0, y1) = span(oy + y as f32 * step_y, step_y, src.height);
        for x in 0..w {
            let (x0, x1) = span(ox + x as f32 * step_x, step_x, src.width);
            let (mut b, mut g, mut r, mut a, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for sy in y0..y1 {
                let row = &src.bgra[sy as usize * src.width as usize * 4..];
                for sx in x0..x1 {
                    let p = &row[sx as usize * 4..sx as usize * 4 + 4];
                    let pa = p[3] as u32;
                    b += p[0] as u32 * pa;
                    g += p[1] as u32 * pa;
                    r += p[2] as u32 * pa;
                    a += pa;
                    n += 1;
                }
            }
            let o = (y as usize * w as usize + x as usize) * 4;
            // Colour sums are alpha-weighted; dividing by 255 * n premultiplies.
            let div = 255 * n.max(1);
            out[o] = ((b + div / 2) / div) as u8;
            out[o + 1] = ((g + div / 2) / div) as u8;
            out[o + 2] = ((r + div / 2) / div) as u8;
            out[o + 3] = ((a + n / 2) / n.max(1)) as u8;
        }
    }
    out
}

/// The source pixels `[from, from + len)` covers, at least one, inside `max`.
fn span(from: f32, len: f32, max: u32) -> (u32, u32) {
    let a = (from.floor().max(0.0) as u32).min(max - 1);
    let b = ((from + len).ceil() as u32).clamp(a + 1, max);
    (a, b)
}

#[derive(Default)]
struct Cache {
    ready: HashMap<Key, Arc<Preview>>,
    pending: HashSet<Key>,
    /// Could not be decoded: not asked for again (a redraw would loop).
    failed: HashSet<Key>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

fn cache<R>(f: impl FnOnce(&mut Cache) -> R) -> R {
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Cache::default))
}

pub fn get(key: Key) -> Option<Arc<Preview>> {
    cache(|c| c.ready.get(&key).cloned())
}

/// Keep only the previews of `live` images at `dpi`: a deleted Clip or a DPI
/// left behind frees its pixels.
pub fn retain(dpi: u32, live: &HashSet<u64>) {
    cache(|c| {
        c.ready.retain(|k, _| k.dpi == dpi && live.contains(&k.hash));
        c.failed.retain(|k| live.contains(&k.hash));
    });
}

struct Request {
    key: Key,
    png: Arc<Vec<u8>>,
    px: (u32, u32),
    hwnd: isize,
}

static WORKER: OnceLock<Sender<Request>> = OnceLock::new();

fn worker() -> &'static Sender<Request> {
    WORKER.get_or_init(|| {
        let (tx, rx) = channel();
        if let Err(e) = std::thread::Builder::new().name("history-preview".into()).spawn(move || run(rx)) {
            log(format!("history: could not start the preview worker: {e}"));
        }
        tx
    })
}

/// Queue the preview of an image unless it is cached or already queued. Never blocks.
pub fn request(hwnd: HWND, scale: f32, hash: u64, png: &Arc<Vec<u8>>) {
    let key = Key::new(hash, scale);
    if !cache(|c| !c.ready.contains_key(&key) && !c.failed.contains(&key) && c.pending.insert(key)) {
        return;
    }
    let req = Request { key, png: Arc::clone(png), px: size_px(scale), hwnd: hwnd.0 as isize };
    if worker().send(req).is_err() {
        cache(|c| c.pending.remove(&key));
    }
}

fn run(rx: Receiver<Request>) {
    for r in rx {
        let preview = image::decode_png(&r.png).ok().map(|bmp| Preview {
            w: r.px.0,
            h: r.px.1,
            bgra: cover(&bmp, r.px.0, r.px.1),
        });
        let Some(preview) = preview else {
            log("history: an image preview could not be decoded");
            cache(|c| {
                c.pending.remove(&r.key);
                c.failed.insert(r.key);
            });
            continue;
        };
        cache(|c| {
            c.pending.remove(&r.key);
            c.ready.insert(r.key, Arc::new(preview));
        });
        let _ = unsafe { PostMessageW(Some(HWND(r.hwnd as *mut _)), WM_HISTORY_PREVIEW, WPARAM(0), LPARAM(0)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Bitmap {
        Bitmap { width: w, height: h, bgra: px.iter().copied().cycle().take((w * h * 4) as usize).collect() }
    }

    #[test]
    fn the_size_follows_the_scale() {
        assert_eq!(size_px(1.0), (44, 28));
        assert_eq!(size_px(1.5), (66, 42));
        assert_eq!(size_px(2.0), (88, 56));
    }

    #[test]
    fn a_flat_picture_stays_flat_at_any_shape() {
        for (w, h) in [(1920, 1080), (50, 400), (3, 2), (44, 28)] {
            let out = cover(&solid(w, h, [10, 20, 30, 255]), 44, 28);
            assert_eq!(out.len(), 44 * 28 * 4);
            assert!(out.chunks(4).all(|p| p == [10, 20, 30, 255]), "{w}x{h}");
        }
    }

    #[test]
    fn a_wide_picture_is_cropped_to_its_middle() {
        // 200 x 28: red on the left half, blue on the right. At scale 1 the
        // middle 44 columns show: 22 red, 22 blue.
        let mut bgra = Vec::new();
        for _ in 0..28 {
            for x in 0..200 {
                bgra.extend_from_slice(if x < 100 { &[0, 0, 255, 255] } else { &[255, 0, 0, 255] });
            }
        }
        let out = cover(&Bitmap { width: 200, height: 28, bgra }, 44, 28);
        assert_eq!(&out[..4], [0, 0, 255, 255]);
        assert_eq!(&out[21 * 4..21 * 4 + 4], [0, 0, 255, 255]);
        assert_eq!(&out[22 * 4..22 * 4 + 4], [255, 0, 0, 255]);
        assert_eq!(&out[43 * 4..43 * 4 + 4], [255, 0, 0, 255]);
    }

    #[test]
    fn a_big_picture_is_averaged_not_sampled() {
        // 88 x 56 checker of black and white pixels shrinks to mid grey.
        let mut bgra = Vec::new();
        for y in 0..56 {
            for x in 0..88 {
                let v = if (x + y) % 2 == 0 { 0 } else { 255 };
                bgra.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let out = cover(&Bitmap { width: 88, height: 56, bgra }, 44, 28);
        assert!(out.chunks(4).all(|p| (126..=129).contains(&p[0]) && p[3] == 255));
    }

    #[test]
    fn transparency_comes_out_premultiplied() {
        let out = cover(&solid(4, 4, [200, 100, 50, 128]), 4, 4);
        assert_eq!(&out[..4], [100, 50, 25, 128]);
        let clear = cover(&solid(4, 4, [200, 100, 50, 0]), 4, 4);
        assert_eq!(&clear[..4], [0, 0, 0, 0]);
    }

    #[test]
    fn a_tiny_picture_is_enlarged_without_reading_outside_it() {
        let out = cover(&solid(1, 1, [1, 2, 3, 255]), 44, 28);
        assert!(out.chunks(4).all(|p| p == [1, 2, 3, 255]));
    }

    #[test]
    fn previews_are_kept_per_image_and_dpi() {
        let live: HashSet<u64> = [1].into_iter().collect();
        let k = |hash, dpi| Key { hash, dpi };
        cache(|c| {
            for key in [k(1, 96), k(1, 144), k(2, 96)] {
                c.ready.insert(key, Arc::new(Preview { w: 1, h: 1, bgra: vec![0; 4] }));
            }
        });
        retain(96, &live);
        assert!(get(k(1, 96)).is_some());
        assert!(get(k(1, 144)).is_none());
        assert!(get(k(2, 96)).is_none());
        retain(96, &HashSet::new());
    }
}
