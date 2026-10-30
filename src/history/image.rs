//! Images in the History: clipboard DIB/DIBV5 → pixels → PNG (stored), PNG →
//! pixels → DIB (pasted back), and the pixel hash that finds duplicates.
//!
//! The DIB parsing is pure. The PNG codec is WIC (no new crate, real
//! compression, decodes whatever the store holds); it initialises COM on the
//! calling thread, so call it from a worker, never the UI thread.
//!
//! The DIB reading follows what arboard (MIT/Apache-2.0) learned about the
//! clipboard's bitmaps: a 32-bit BI_RGB bitmap often has an unused alpha byte
//! of zero, so a bitmap whose alpha is all zero counts as opaque.

// Not wired into the recorder and paste-back yet.
#![allow(dead_code)]

use windows::core::{Result, GUID};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapEncoderNoCache, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IStream, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    STREAM_SEEK_END, STREAM_SEEK_SET,
};
use windows::Win32::UI::Shell::SHCreateMemStream;

/// Larger than this on a side is refused (a corrupt header, not a screenshot).
const MAX_SIDE: u32 = 16_384;
/// Decoded size limit in bytes; keeps a bogus header from asking for gigabytes.
const MAX_PIXEL_BYTES: usize = 512 * 1024 * 1024;

/// Pixels as top-down 32-bit BGRA, straight (not premultiplied) alpha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}
fn i32_at(d: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// One colour channel of a pixel: where it sits and how wide it is.
#[derive(Clone, Copy)]
struct Channel {
    mask: u32,
    shift: u32,
    max: u32,
}

impl Channel {
    fn new(mask: u32) -> Self {
        if mask == 0 {
            return Channel { mask: 0, shift: 0, max: 0 };
        }
        let shift = mask.trailing_zeros();
        Channel { mask, shift, max: mask >> shift }
    }
    /// The channel scaled to 0..=255 (255 when the channel is absent).
    fn get(&self, px: u32) -> u8 {
        if self.max == 0 {
            return 255;
        }
        let v = (px & self.mask) >> self.shift;
        if self.max == 255 { v as u8 } else { ((v * 255 + self.max / 2) / self.max) as u8 }
    }
}

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;

/// Reads a `CF_DIB` / `CF_DIBV5` block (a header, optional masks and palette,
/// then the pixels) into top-down BGRA. Handles bottom-up and top-down
/// heights, 1/4/8-bit palettes, 16/24/32-bit, `BI_RGB` and `BI_BITFIELDS`,
/// and a 32-bit bitmap without alpha. `None` for anything else (RLE, JPEG,
/// a truncated block).
pub fn dib_to_bitmap(dib: &[u8]) -> Option<Bitmap> {
    let header = u32_at(dib, 0)? as usize;
    if header < 40 {
        return None; // BITMAPCOREHEADER: not worth supporting
    }
    let width = i32_at(dib, 4)?;
    let raw_h = i32_at(dib, 8)?;
    let bits = u16_at(dib, 14)? as u32;
    let compression = u32_at(dib, 16)?;
    let colors_used = u32_at(dib, 32)? as usize;
    if width <= 0 || raw_h == 0 || raw_h == i32::MIN {
        return None;
    }
    let (width, height, top_down) = (width as u32, raw_h.unsigned_abs(), raw_h < 0);
    if width > MAX_SIDE || height > MAX_SIDE || !matches!(bits, 1 | 4 | 8 | 16 | 24 | 32) {
        return None;
    }
    if compression != BI_RGB && compression != BI_BITFIELDS {
        return None;
    }
    if compression == BI_BITFIELDS && !matches!(bits, 16 | 32) {
        return None;
    }
    let out_len = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
    if out_len > MAX_PIXEL_BYTES {
        return None;
    }

    // Masks: inside V4/V5 headers; after a 40-byte header only with BI_BITFIELDS.
    let mut at = header;
    let masks = if header >= 56 {
        Some([u32_at(dib, 40)?, u32_at(dib, 44)?, u32_at(dib, 48)?, u32_at(dib, 52)?])
    } else if compression == BI_BITFIELDS {
        let m = [u32_at(dib, at)?, u32_at(dib, at + 4)?, u32_at(dib, at + 8)?, 0];
        at += 12;
        Some(m)
    } else {
        None
    };
    // V4/V5 masks only apply with BI_BITFIELDS; BI_RGB uses the default layout.
    let masks = if compression == BI_BITFIELDS { masks } else { None };

    let palette_len = if bits <= 8 {
        if colors_used == 0 { 1usize << bits } else { colors_used.min(1usize << bits) }
    } else {
        0
    };
    let palette: Vec<[u8; 4]> = (0..palette_len)
        .map(|i| {
            let o = at + i * 4;
            dib.get(o..o + 4).map(|p| [p[0], p[1], p[2], 255]).ok_or(())
        })
        .collect::<std::result::Result<_, _>>()
        .ok()?;
    // With a palette, colours are indexed; without one, the pixels follow the
    // header (and masks) directly, plus the colour table of a deep bitmap.
    at += if bits <= 8 { palette_len * 4 } else { colors_used * 4 };

    let stride = ((width as usize * bits as usize).div_ceil(32)) * 4;
    let need = stride.checked_mul(height as usize)?;
    let pixels = dib.get(at..at.checked_add(need)?)?;

    let (r, g, b, a) = match masks {
        Some([r, g, b, a]) => (Channel::new(r), Channel::new(g), Channel::new(b), Channel::new(a)),
        None if bits == 16 => (Channel::new(0x7C00), Channel::new(0x03E0), Channel::new(0x001F), Channel::new(0)),
        None => (Channel::new(0x00FF_0000), Channel::new(0x0000_FF00), Channel::new(0x0000_00FF), Channel::new(0xFF00_0000)),
    };

    let mut bgra = vec![0u8; out_len];
    for y in 0..height as usize {
        let src_y = if top_down { y } else { height as usize - 1 - y };
        let row = &pixels[src_y * stride..src_y * stride + stride];
        let dst = &mut bgra[y * width as usize * 4..(y + 1) * width as usize * 4];
        for x in 0..width as usize {
            let out = match bits {
                32 => {
                    let px = u32::from_le_bytes(row[x * 4..x * 4 + 4].try_into().ok()?);
                    [b.get(px), g.get(px), r.get(px), a.get(px)]
                }
                24 => [row[x * 3], row[x * 3 + 1], row[x * 3 + 2], 255],
                16 => {
                    let px = u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]) as u32;
                    [b.get(px), g.get(px), r.get(px), a.get(px)]
                }
                _ => {
                    let per = 8 / bits as usize;
                    let byte = row[x / per];
                    let shift = 8 - bits as usize * (x % per + 1);
                    let idx = (byte >> shift) as usize & ((1 << bits) - 1);
                    *palette.get(idx)?
                }
            };
            dst[x * 4..x * 4 + 4].copy_from_slice(&out);
        }
    }
    // A 32-bit bitmap that never set its alpha byte is opaque.
    if bits == 32 && bgra.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
        bgra.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p[3] = 255);
    }
    Some(Bitmap { width, height, bgra })
}

/// A `CF_DIB` block for pasting back: a 40-byte header, 32 bits, bottom-up,
/// `BI_RGB`. DIB readers ignore alpha, so transparent pixels are flattened
/// onto white here; the PNG next to it keeps the alpha.
pub fn bitmap_to_dib(bmp: &Bitmap) -> Vec<u8> {
    let (w, h) = (bmp.width as usize, bmp.height as usize);
    let mut out = Vec::with_capacity(40 + w * h * 4);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(bmp.width as i32).to_le_bytes());
    out.extend_from_slice(&(bmp.height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&BI_RGB.to_le_bytes());
    out.extend_from_slice(&((w * h * 4) as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // resolution, colours used / important
    for y in (0..h).rev() {
        for p in bmp.bgra[y * w * 4..(y + 1) * w * 4].as_chunks::<4>().0 {
            let a = p[3] as u32;
            let over_white = |c: u8| ((c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
            out.extend_from_slice(&[over_white(p[0]), over_white(p[1]), over_white(p[2]), 255]);
        }
    }
    out
}

/// A 64-bit FNV-1a hash of the size and pixels: the duplicate key for images.
pub fn pixel_hash(bmp: &Bitmap) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01B3;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h = (h ^ b as u64).wrapping_mul(PRIME);
        }
    };
    eat(&bmp.width.to_le_bytes());
    eat(&bmp.height.to_le_bytes());
    eat(&bmp.bgra);
    h
}

/// Width and height from a PNG's header, without decoding it.
pub fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.get(..8)? != b"\x89PNG\r\n\x1a\n" || png.get(12..16)? != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(png.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(png.get(20..24)?.try_into().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

// ---- PNG through WIC ----

/// COM on this thread for the length of one call.
struct Com(bool);
impl Com {
    fn init() -> Self {
        // S_OK and S_FALSE both need a matching CoUninitialize; a different
        // apartment already set (RPC_E_CHANGED_MODE) still lets WIC run.
        Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn factory() -> Result<IWICImagingFactory> {
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
}

fn read_stream(stream: &IStream) -> Result<Vec<u8>> {
    unsafe {
        let mut len = 0u64;
        stream.Seek(0, STREAM_SEEK_END, Some(&mut len))?;
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut buf = vec![0u8; len as usize];
        let mut got = 0u32;
        stream.Read(buf.as_mut_ptr().cast(), buf.len() as u32, Some(&mut got)).ok()?;
        buf.truncate(got as usize);
        Ok(buf)
    }
}

/// Encodes the pixels as a PNG (alpha kept).
pub fn encode_png(bmp: &Bitmap) -> Result<Vec<u8>> {
    let (w, h) = (bmp.width as usize, bmp.height as usize);
    if bmp.bgra.len() != w * h * 4 || w == 0 || h == 0 {
        return Err(E_FAIL.into());
    }
    let _com = Com::init();
    let factory = factory()?;
    unsafe {
        let stream = SHCreateMemStream(None).ok_or_else(|| windows::core::Error::from(E_FAIL))?;
        let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(|| windows::core::Error::from(E_FAIL))?;
        frame.Initialize(None)?;
        frame.SetSize(bmp.width, bmp.height)?;
        let mut format: GUID = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut format)?;
        frame.WritePixels(bmp.height, (w * 4) as u32, &bmp.bgra)?;
        frame.Commit()?;
        encoder.Commit()?;
        read_stream(&stream)
    }
}

/// Decodes a PNG (or anything WIC reads) into BGRA pixels.
pub fn decode_png(png: &[u8]) -> Result<Bitmap> {
    let (w, h) = png_size(png).ok_or_else(|| windows::core::Error::from(E_FAIL))?;
    if w > MAX_SIDE || h > MAX_SIDE || w as usize * h as usize * 4 > MAX_PIXEL_BYTES {
        return Err(E_FAIL.into());
    }
    let _com = Com::init();
    let factory = factory()?;
    unsafe {
        let stream = SHCreateMemStream(Some(png)).ok_or_else(|| windows::core::Error::from(E_FAIL))?;
        let decoder = factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
        let frame = decoder.GetFrame(0)?;
        let conv = factory.CreateFormatConverter()?;
        conv.Initialize(&frame, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let mut bgra = vec![0u8; w as usize * h as usize * 4];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut bgra)?;
        Ok(Bitmap { width: w, height: h, bgra })
    }
}

/// CF_DIB/CF_DIBV5 bytes → PNG, the whole recording conversion in one call.
/// Returns the PNG, its size and the duplicate hash.
pub fn dib_to_png(dib: &[u8]) -> Option<(Vec<u8>, u32, u32, u64)> {
    let bmp = dib_to_bitmap(dib)?;
    let png = encode_png(&bmp).ok()?;
    Some((png, bmp.width, bmp.height, pixel_hash(&bmp)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 40-byte-header DIB with the given fields, then `extra` (masks,
    /// palette) and the pixel rows.
    fn dib40(w: i32, h: i32, bits: u16, compression: u32, colors: u32, extra: &[u8], pixels: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&40u32.to_le_bytes());
        d.extend_from_slice(&w.to_le_bytes());
        d.extend_from_slice(&h.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&bits.to_le_bytes());
        d.extend_from_slice(&compression.to_le_bytes());
        d.extend_from_slice(&[0u8; 12]);
        d.extend_from_slice(&colors.to_le_bytes());
        d.extend_from_slice(&[0u8; 4]);
        d.extend_from_slice(extra);
        d.extend_from_slice(pixels);
        d
    }

    /// A 124-byte V5 header with BI_BITFIELDS masks (BGRA) for 32 bits.
    fn dib_v5(w: i32, h: i32, masks: [u32; 4], pixels: &[u8]) -> Vec<u8> {
        let mut d = dib40(w, h, 32, BI_BITFIELDS, 0, &[], &[]);
        d[0..4].copy_from_slice(&124u32.to_le_bytes());
        for m in masks {
            d.extend_from_slice(&m.to_le_bytes());
        }
        d.resize(124, 0);
        d.extend_from_slice(pixels);
        d
    }

    const BGRA_MASKS: [u32; 4] = [0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000];

    #[test]
    fn a_bottom_up_24_bit_dib_reads_top_down_with_padded_rows() {
        // 2 x 2, rows padded to 8 bytes. Bottom row first: red, green / blue, white.
        let px = [
            0, 0, 255, 0, 255, 0, 0, 0, // red, green (BGR) + pad
            255, 0, 0, 255, 255, 255, 0, 0, // blue, white + pad
        ];
        let b = dib_to_bitmap(&dib40(2, 2, 24, BI_RGB, 0, &[], &px)).unwrap();
        assert_eq!((b.width, b.height), (2, 2));
        // top row is the file's last row: blue, white
        assert_eq!(&b.bgra[..8], &[255, 0, 0, 255, 255, 255, 255, 255]);
        assert_eq!(&b.bgra[8..], &[0, 0, 255, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn a_top_down_header_is_not_flipped() {
        let px = [1, 2, 3, 255, 4, 5, 6, 255]; // two rows of one pixel
        let b = dib_to_bitmap(&dib40(1, -2, 32, BI_BITFIELDS, 0, &masks_bytes(BGRA_MASKS), &px)).unwrap();
        assert_eq!(b.bgra, px);
        let up = dib_to_bitmap(&dib40(1, 2, 32, BI_BITFIELDS, 0, &masks_bytes(BGRA_MASKS), &px)).unwrap();
        assert_eq!(&up.bgra[..4], &[4, 5, 6, 255]);
    }

    fn masks_bytes(m: [u32; 4]) -> Vec<u8> {
        m[..3].iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn a_v5_dib_keeps_its_alpha() {
        let px = [10, 20, 30, 128, 40, 50, 60, 0];
        let b = dib_to_bitmap(&dib_v5(2, -1, BGRA_MASKS, &px)).unwrap();
        assert_eq!(b.bgra, px);
    }

    #[test]
    fn a_32_bit_dib_with_empty_alpha_is_opaque() {
        let px = [10, 20, 30, 0, 40, 50, 60, 0];
        let plain = dib_to_bitmap(&dib40(2, -1, 32, BI_RGB, 0, &[], &px)).unwrap();
        assert_eq!(plain.bgra, [10, 20, 30, 255, 40, 50, 60, 255]);
        let v5 = dib_to_bitmap(&dib_v5(2, -1, BGRA_MASKS, &px)).unwrap();
        assert_eq!(v5.bgra, plain.bgra);
        // 32 bits with the masks but no alpha mask is opaque too
        let no_alpha = dib_to_bitmap(&dib40(2, -1, 32, BI_BITFIELDS, 0, &masks_bytes([BGRA_MASKS[0], BGRA_MASKS[1], BGRA_MASKS[2], 0]), &[10, 20, 30, 77, 40, 50, 60, 77])).unwrap();
        assert_eq!(no_alpha.bgra, [10, 20, 30, 255, 40, 50, 60, 255]);
    }

    #[test]
    fn bitfields_with_other_channel_orders_and_16_bit_565() {
        // RGBA byte order: R in the low byte
        let masks = [0x0000_00FF, 0x0000_FF00, 0x00FF_0000, 0xFF00_0000];
        let b = dib_to_bitmap(&dib40(1, -1, 32, BI_BITFIELDS, 0, &masks_bytes(masks), &[1, 2, 3, 4])).unwrap();
        assert_eq!(b.bgra, [3, 2, 1, 255]); // a 40-byte header has no alpha mask
        // 565: red 11111, green 111111, blue 00000
        let m565 = [0xF800u32, 0x07E0, 0x001F, 0];
        let px = (0xF800u16 | 0x07E0).to_le_bytes();
        let b = dib_to_bitmap(&dib40(1, -1, 16, BI_BITFIELDS, 0, &masks_bytes(m565), &[px[0], px[1], 0, 0])).unwrap();
        assert_eq!(b.bgra, [0, 255, 255, 255]);
        // plain 16-bit is 5-5-5
        let b = dib_to_bitmap(&dib40(1, -1, 16, BI_RGB, 0, &[], &[0xFF, 0x7F, 0, 0])).unwrap();
        assert_eq!(b.bgra, [255, 255, 255, 255]);
    }

    #[test]
    fn a_palette_dib_maps_indexes_to_colours() {
        // 4 x 1, 1 bit: pattern 1010; palette black, red (BGRX)
        let pal = [0, 0, 0, 0, 0, 0, 255, 0];
        let b = dib_to_bitmap(&dib40(4, -1, 1, BI_RGB, 2, &pal, &[0b1010_0000, 0, 0, 0])).unwrap();
        assert_eq!(&b.bgra[..4], &[0, 0, 255, 255]);
        assert_eq!(&b.bgra[4..8], &[0, 0, 0, 255]);
        assert_eq!(&b.bgra[8..12], &[0, 0, 255, 255]);
    }

    #[test]
    fn bad_or_truncated_dibs_are_refused() {
        assert!(dib_to_bitmap(&[]).is_none());
        assert!(dib_to_bitmap(&[0u8; 39]).is_none());
        assert!(dib_to_bitmap(&dib40(2, 2, 24, BI_RGB, 0, &[], &[0; 8])).is_none()); // short pixels
        assert!(dib_to_bitmap(&dib40(2, 2, 24, 1, 0, &[], &[0; 16])).is_none()); // RLE
        assert!(dib_to_bitmap(&dib40(0, 2, 24, BI_RGB, 0, &[], &[])).is_none());
        assert!(dib_to_bitmap(&dib40(100_000, 1, 24, BI_RGB, 0, &[], &[0; 16])).is_none());
        assert!(dib_to_bitmap(&dib40(1, 1, 7, BI_RGB, 0, &[], &[0; 4])).is_none());
    }

    fn sample() -> Bitmap {
        // 3 x 2, varied alpha
        let bgra = vec![
            255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, //
            10, 20, 30, 255, 40, 50, 60, 200, 70, 80, 90, 255,
        ];
        Bitmap { width: 3, height: 2, bgra }
    }

    #[test]
    fn png_round_trip_keeps_pixels_and_alpha() {
        let bmp = sample();
        let png = encode_png(&bmp).unwrap();
        assert_eq!(png_size(&png), Some((3, 2)));
        assert_eq!(decode_png(&png).unwrap(), bmp);
    }

    #[test]
    fn dib_to_png_gives_png_size_and_hash() {
        let px = [1, 2, 3, 255, 4, 5, 6, 255];
        let dib = dib_v5(2, -1, BGRA_MASKS, &px);
        let (png, w, h, hash) = dib_to_png(&dib).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(png_size(&png), Some((2, 1)));
        assert_eq!(hash, pixel_hash(&decode_png(&png).unwrap()));
    }

    #[test]
    fn a_big_flat_image_compresses() {
        let bmp = Bitmap { width: 1000, height: 1000, bgra: vec![200; 4_000_000] };
        assert!(encode_png(&bmp).unwrap().len() < 100_000);
    }

    #[test]
    fn png_back_to_dib_is_bottom_up_32_bit_over_white() {
        let dib = bitmap_to_dib(&sample());
        assert_eq!(u32_at(&dib, 0), Some(40));
        assert_eq!(i32_at(&dib, 8), Some(2)); // bottom-up
        assert_eq!(u16_at(&dib, 14), Some(32));
        assert_eq!(dib.len(), 40 + 3 * 2 * 4);
        // first stored row is the bitmap's last row
        assert_eq!(&dib[40..44], &[10, 20, 30, 255]);
        // fully transparent red pixel (row 0, x 2) becomes white
        assert_eq!(&dib[40 + 12 + 8..40 + 12 + 12], &[255, 255, 255, 255]);
        // and reading it back gives the flattened picture
        let back = dib_to_bitmap(&dib).unwrap();
        assert_eq!((back.width, back.height), (3, 2));
        assert_eq!(&back.bgra[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn the_pixel_hash_tells_pixels_and_shapes_apart() {
        let a = sample();
        assert_eq!(pixel_hash(&a), pixel_hash(&a.clone()));
        let mut b = a.clone();
        b.bgra[0] ^= 1;
        assert_ne!(pixel_hash(&a), pixel_hash(&b));
        let flat = Bitmap { width: 1, height: 4, bgra: vec![7; 16] };
        let wide = Bitmap { width: 4, height: 1, bgra: vec![7; 16] };
        assert_ne!(pixel_hash(&flat), pixel_hash(&wide));
    }

    #[test]
    fn png_size_and_decode_refuse_garbage() {
        assert_eq!(png_size(b"not a png at all, really not"), None);
        assert!(decode_png(&[1, 2, 3]).is_err());
        assert!(encode_png(&Bitmap { width: 2, height: 2, bgra: vec![0; 3] }).is_err());
    }
}
