//! The 30x30 DIP row thumbnail. For now the spike's fallback tiles (spec §6):
//! images in a rose tile, folders as a yellow folder glyph, everything else a
//! dark tile with the extension (PDF in red). TASCA-17 owns this file and puts
//! real shell thumbnails in front of these, keeping them as the fallback.

use std::collections::HashMap;

use windows_numerics::Matrix3x2;

use windows::core::{Result, HSTRING};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap, ID2D1RenderTarget, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES,
    D2D1_DRAW_TEXT_OPTIONS_CLIP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::DirectWrite::{
    IDWriteFactory, IDWriteTextFormat, DWRITE_FONT_WEIGHT_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_TEXT_ALIGNMENT_CENTER,
};

use super::model::Item;
use super::model::ItemId;
use super::render::{rr, text_format, wide};
use crate::thumbs::{self, Key};

pub const TILE: f32 = 30.0;
const TILE_RADIUS: f32 = 6.0;

const ROSE: (f32, f32, f32, f32) = (0.76, 0.33, 0.42, 1.0);
const DARK: (f32, f32, f32, f32) = (0.165, 0.165, 0.19, 1.0);
const FOLDER: (f32, f32, f32, f32) = (0xe0 as f32 / 255.0, 0xb0 as f32 / 255.0, 0x4a as f32 / 255.0, 1.0);
const PDF_RED: (f32, f32, f32, f32) = (1.0, 0.42, 0.46, 1.0);
const LABEL: (f32, f32, f32, f32) = (1.0, 1.0, 1.0, 0.56);

pub struct Tiles {
    label: IDWriteTextFormat,
    /// D2D copies of the cached thumbnails, made lazily on the current target.
    bitmaps: HashMap<Key, ID2D1Bitmap>,
}

impl Tiles {
    pub fn new(dw: &IDWriteFactory, family: &HSTRING) -> Result<Self> {
        Ok(Self {
            label: text_format(dw, family, 7.5, DWRITE_FONT_WEIGHT_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER, true)?,
            bitmaps: HashMap::new(),
        })
    }

    /// The render target was recreated: drop anything made on the old one
    /// (D2D bitmaps). The pixels stay cached in `thumbs`.
    pub fn target_reset(&mut self) {
        self.bitmaps.clear();
    }

    /// Forget bitmaps whose thumbnail left the cache (Item removed, DPI changed).
    pub fn sync(&mut self) {
        self.bitmaps.retain(|k, _| thumbs::has(*k));
    }

    /// The D2D bitmap for `id` at the target's DPI, if its thumbnail is cached.
    fn bitmap(&mut self, rt: &ID2D1RenderTarget, id: ItemId) -> Option<(ID2D1Bitmap, u32, u32)> {
        let (mut dx, mut dy) = (0.0, 0.0);
        unsafe { rt.GetDpi(&mut dx, &mut dy) };
        let key = Key::new(id, dx);
        let thumb = thumbs::get(key)?;
        if let Some(b) = self.bitmaps.get(&key) {
            return Some((b.clone(), thumb.w, thumb.h));
        }
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: dx,
            dpiY: dy,
        };
        let size = D2D_SIZE_U { width: thumb.w, height: thumb.h };
        let b = unsafe { rt.CreateBitmap(size, Some(thumb.bgra.as_ptr() as *const _), thumb.w * 4, &props) }.ok()?;
        self.bitmaps.insert(key, b.clone());
        Some((b, thumb.w, thumb.h))
    }

    /// Draw `item`'s tile into `r` (a TILE x TILE DIP square). `opacity`
    /// multiplies everything (missing rows are drawn at 45 %).
    pub fn draw(&mut self, rt: &ID2D1RenderTarget, item: &Item, r: D2D_RECT_F, opacity: f32) -> Result<()> {
        unsafe {
            let brush = |(cr, cg, cb, ca): (f32, f32, f32, f32)| {
                rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: cr, g: cg, b: cb, a: ca * opacity }, None)
            };
            let image = item.is_image();
            rt.FillRoundedRectangle(&rr(r.left, r.top, r.right, r.bottom, TILE_RADIUS), &brush(if image { ROSE } else { DARK })?);
            if let Some((bmp, w, h)) = self.bitmap(rt, item.id) {
                // The bitmap carries the target's DPI, so its DIP size is its pixels / scale.
                let (mut dx, mut dy) = (0.0, 0.0);
                rt.GetDpi(&mut dx, &mut dy);
                let (bw, bh) = (w as f32 * 96.0 / dx, h as f32 * 96.0 / dy);
                let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
                let dest = D2D_RECT_F { left: cx - bw / 2.0, top: cy - bh / 2.0, right: cx + bw / 2.0, bottom: cy + bh / 2.0 };
                if (bw - TILE).abs() < 0.75 && (bh - TILE).abs() < 0.75 {
                    // Fills the tile: clip to the rounded corners with a bitmap brush.
                    let bb = rt.CreateBitmapBrush(&bmp, None, None)?;
                    bb.SetOpacity(opacity);
                    bb.SetTransform(&Matrix3x2::translation(dest.left, dest.top));
                    rt.FillRoundedRectangle(&rr(r.left, r.top, r.right, r.bottom, TILE_RADIUS), &bb);
                } else {
                    rt.DrawBitmap(&bmp, Some(&dest), opacity, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                }
                return Ok(());
            }
            if item.is_folder() {
                let f = brush(FOLDER)?;
                let (x, y) = (r.left, r.top);
                rt.FillRoundedRectangle(&rr(x + 7.0, y + 10.5, x + 23.5, y + 23.0, 1.5), &f);
                rt.FillRoundedRectangle(&rr(x + 7.0, y + 8.5, x + 14.5, y + 11.5, 1.0), &f);
            } else if !image {
                let ext = item.ext();
                if !ext.is_empty() {
                    let label = wide(&ext.chars().take(4).collect::<String>());
                    let color = if ext == "PDF" { PDF_RED } else { LABEL };
                    rt.DrawText(&label, &self.label, &r, &brush(color)?, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                }
            }
        }
        Ok(())
    }
}
