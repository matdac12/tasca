//! The 30x30 DIP row thumbnail. For now the spike's fallback tiles (spec §6):
//! images in a rose tile, folders as a yellow folder glyph, everything else a
//! dark tile with the extension (PDF in red). TASCA-17 owns this file and puts
//! real shell thumbnails in front of these, keeping them as the fallback.

use windows::core::{Result, HSTRING};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1RenderTarget, D2D1_DRAW_TEXT_OPTIONS_CLIP};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteFactory, IDWriteTextFormat, DWRITE_FONT_WEIGHT_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_TEXT_ALIGNMENT_CENTER,
};

use super::model::Item;
use super::render::{rr, text_format, wide};

pub const TILE: f32 = 30.0;
const TILE_RADIUS: f32 = 6.0;

const ROSE: (f32, f32, f32, f32) = (0.76, 0.33, 0.42, 1.0);
const DARK: (f32, f32, f32, f32) = (0.165, 0.165, 0.19, 1.0);
const FOLDER: (f32, f32, f32, f32) = (0xe0 as f32 / 255.0, 0xb0 as f32 / 255.0, 0x4a as f32 / 255.0, 1.0);
const PDF_RED: (f32, f32, f32, f32) = (1.0, 0.42, 0.46, 1.0);
const LABEL: (f32, f32, f32, f32) = (1.0, 1.0, 1.0, 0.56);

pub struct Tiles {
    label: IDWriteTextFormat,
}

impl Tiles {
    pub fn new(dw: &IDWriteFactory, family: &HSTRING) -> Result<Self> {
        Ok(Self { label: text_format(dw, family, 7.5, DWRITE_FONT_WEIGHT_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER, true)? })
    }

    /// The render target was recreated: drop anything made on the old one
    /// (D2D bitmaps). Nothing yet.
    pub fn target_reset(&mut self) {}

    /// Draw `item`'s tile into `r` (a TILE x TILE DIP square). `opacity`
    /// multiplies everything (missing rows are drawn at 45 %).
    pub fn draw(&mut self, rt: &ID2D1RenderTarget, item: &Item, r: D2D_RECT_F, opacity: f32) -> Result<()> {
        unsafe {
            let brush = |(cr, cg, cb, ca): (f32, f32, f32, f32)| {
                rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: cr, g: cg, b: cb, a: ca * opacity }, None)
            };
            let image = item.is_image();
            rt.FillRoundedRectangle(&rr(r.left, r.top, r.right, r.bottom, TILE_RADIUS), &brush(if image { ROSE } else { DARK })?);
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
