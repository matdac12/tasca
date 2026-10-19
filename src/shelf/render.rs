//! Drawing (spec §3): a Direct2D software DC render target over a premultiplied
//! top-down 32 bpp DIB, DirectWrite text, presented with
//! `UpdateLayeredWindow(ULW_ALPHA)`. The whole bitmap is re-rendered on any
//! change; motion only re-presents it.

use windows::core::{w, Result, HSTRING};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

use super::layout::{Layout, PAD, RADIUS, ROW_H, THUMB_W};
use super::model::{ItemId, Model};
use super::tiles::{Tiles, TILE};

const T1: f32 = 0.88;
const T2: f32 = 0.56;
const GLASS: (f32, f32, f32, f32) = (18.0 / 255.0, 18.0 / 255.0, 20.0 / 255.0, 0.92);
const HAIRLINE_A: f32 = 0.14;
const HOVER_A: f32 = 0.08;
const SELECTED_A: f32 = 0.14;
const ROW_RADIUS: f32 = 7.0;
const ACCENT: (f32, f32, f32, f32) = (0x4c as f32 / 255.0, 0xc2 as f32 / 255.0, 1.0, 1.0);
const MISSING_OPACITY: f32 = 0.45;
/// Line box of the 11 DIP header text.
const HEAD_TEXT_H: f32 = 15.0;
const FONT: &str = "Segoe UI Variable Text";
const FONT_FALLBACK: &str = "Segoe UI";

/// What one frame shows.
pub struct View<'a> {
    pub layout: &'a Layout,
    pub model: &'a Model,
    pub hover: Option<ItemId>,
    pub scroll: f32,
    /// A drag is over the Shelf: blue outline (TASCA-14 sets it).
    pub hot: bool,
}

/// Width of the white Stack count badge (DIP): 15 for up to two digits.
pub fn badge_w(count: usize) -> f32 {
    let digits = count.max(1).ilog10() as f32 + 1.0;
    15.0 + (digits - 2.0).max(0.0) * 5.0
}

struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    w: i32,
    h: i32,
}

impl Surface {
    fn new(w: i32, h: i32) -> Result<Self> {
        unsafe {
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                let _ = DeleteObject(bmp.into());
                return Err(windows::core::Error::from_thread());
            }
            let old = SelectObject(dc, bmp.into());
            Ok(Self { dc, bmp, old, w, h })
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

struct Fonts {
    head: IDWriteTextFormat,
    head_r: IDWriteTextFormat,
    name: IDWriteTextFormat,
    sub: IDWriteTextFormat,
    badge: IDWriteTextFormat,
}

pub struct Renderer {
    d2d: ID2D1Factory,
    rt: Option<ID2D1DCRenderTarget>,
    fonts: Fonts,
    tiles: Tiles,
    surf: Option<Surface>,
}

/// `FONT` if installed, else `FONT_FALLBACK`. DirectWrite does not fail on a
/// missing family (it substitutes silently), so ask the collection.
fn font_family(dw: &IDWriteFactory) -> HSTRING {
    let preferred = HSTRING::from(FONT);
    let installed = unsafe {
        let mut coll = None;
        dw.GetSystemFontCollection(&mut coll, false).is_ok()
            && coll.is_some_and(|c| {
                let (mut index, mut exists) = (0u32, windows::core::BOOL(0));
                c.FindFamilyName(&preferred, &mut index, &mut exists).is_ok() && exists.as_bool()
            })
    };
    if installed { preferred } else { HSTRING::from(FONT_FALLBACK) }
}

/// A single-line, ellipsis-trimmed text format.
pub(super) fn text_format(
    dw: &IDWriteFactory,
    family: &HSTRING,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
    align: DWRITE_TEXT_ALIGNMENT,
    center_v: bool,
) -> Result<IDWriteTextFormat> {
    unsafe {
        let f = dw.CreateTextFormat(
            family,
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("en-us"),
        )?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        f.SetTextAlignment(align)?;
        if center_v {
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        }
        let sign = dw.CreateEllipsisTrimmingSign(&f)?;
        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        f.SetTrimming(&trim, &sign)?;
        Ok(f)
    }
}

pub(super) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub(super) fn rr(l: f32, t: f32, r: f32, b: f32, rad: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: l, top: t, right: r, bottom: b }, radiusX: rad, radiusY: rad }
}

impl Renderer {
    pub fn new() -> Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let fam = font_family(&dw);
            let n = DWRITE_FONT_WEIGHT_NORMAL;
            let fonts = Fonts {
                head: text_format(&dw, &fam, 11.0, n, DWRITE_TEXT_ALIGNMENT_LEADING, false)?,
                head_r: text_format(&dw, &fam, 11.0, n, DWRITE_TEXT_ALIGNMENT_TRAILING, false)?,
                name: text_format(&dw, &fam, 12.0, n, DWRITE_TEXT_ALIGNMENT_LEADING, false)?,
                sub: text_format(&dw, &fam, 10.5, n, DWRITE_TEXT_ALIGNMENT_LEADING, false)?,
                badge: text_format(&dw, &fam, 9.0, DWRITE_FONT_WEIGHT_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER, true)?,
            };
            let tiles = Tiles::new(&dw, &fam)?;
            Ok(Self { d2d, rt: None, fonts, tiles, surf: None })
        }
    }

    /// The row-tile drawer (TASCA-17 hands it thumbnails).
    #[allow(dead_code)] // TASCA-17
    pub fn tiles_mut(&mut self) -> &mut Tiles {
        &mut self.tiles
    }

    /// Render `v` into the bitmap (resized to the layout's pixel size).
    pub fn render(&mut self, v: &View) -> Result<()> {
        let (w, h) = v.layout.size_px();
        if self.surf.as_ref().map(|s| (s.w, s.h)) != Some((w, h)) {
            self.surf = None;
            self.surf = Some(Surface::new(w, h)?);
        }
        if self.rt.is_none() {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            self.rt = Some(unsafe { self.d2d.CreateDCRenderTarget(&props)? });
            // Device-dependent resources made on the old target are invalid now.
            self.tiles.target_reset();
        }
        let (Some(surf), Some(rt)) = (self.surf.as_ref(), self.rt.as_ref()) else { unreachable!() };
        let dpi = v.layout.scale * 96.0;
        let r = unsafe {
            rt.SetDpi(dpi, dpi);
            rt.BindDC(surf.dc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
            rt.BeginDraw();
            let drawn = draw(rt, &self.fonts, &mut self.tiles, v);
            let ended = rt.EndDraw(None, None);
            drawn.and(ended)
        };
        if r.is_err() {
            // D2DERR_RECREATE_TARGET and friends: start from a fresh target next time.
            self.rt = None;
        }
        r
    }

    /// Hand the last rendered bitmap to the window at `pos` (screen px) with
    /// `alpha` as `SourceConstantAlpha`.
    pub fn present(&self, hwnd: HWND, pos: POINT, alpha: u8) -> Result<()> {
        let Some(surf) = self.surf.as_ref() else { return Ok(()) };
        let size = SIZE { cx: surf.w, cy: surf.h };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: alpha,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&pos),
                Some(&size),
                Some(surf.dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
    }
}

unsafe fn draw(rt: &ID2D1DCRenderTarget, fonts: &Fonts, tiles: &mut Tiles, v: &View) -> Result<()> {
    unsafe {
        let brush = |r: f32, g: f32, b: f32, a: f32| -> Result<ID2D1SolidColorBrush> {
            rt.CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a }, None)
        };
        let white = |a: f32| brush(1.0, 1.0, 1.0, a);
        let text = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
            rt.DrawText(&wide(s), f, &r, b, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL)
        };
        let l = v.layout;
        let p = l.panel();
        let (pl, pt, pr, pb) = (p.left, p.top, p.right, p.bottom);
        let hair = 1.0 / l.scale;
        let scroll = l.clamp_scroll(v.scroll);
        let items = v.model.items();

        rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));

        // Shadow: stacked rounded rects, about 0 6px 18px rgba(0,0,0,.35).
        for i in (0..10).rev() {
            let e = i as f32 * 1.6;
            let a = 0.045 * (1.0 - i as f32 / 10.0);
            rt.FillRoundedRectangle(&rr(pl - e, pt + 4.0 - e * 0.6, pr + e, pb + 6.0 + e, RADIUS + e), &brush(0.0, 0.0, 0.0, a)?);
        }
        // Glass and hairline (1 physical pixel).
        let (gr, gg, gb, ga) = GLASS;
        rt.FillRoundedRectangle(&rr(pl, pt, pr, pb, RADIUS), &brush(gr, gg, gb, ga)?);
        let hh = hair / 2.0;
        rt.DrawRoundedRectangle(&rr(pl + hh, pt + hh, pr - hh, pb - hh, RADIUS), &white(HAIRLINE_A)?, hair, None);
        // Drag over the Shelf: 1.5 DIP accent outline outside the panel.
        if v.hot {
            let (ar, ag, ab, aa) = ACCENT;
            rt.DrawRoundedRectangle(&rr(pl - 0.75, pt - 0.75, pr + 0.75, pb + 0.75, RADIUS + 0.75), &brush(ar, ag, ab, aa)?, 1.5, None);
        }

        // Header. The empty Shelf centres "Tasca" in its 64 DIP strip.
        let t2 = white(T2)?;
        let head_top = if items.is_empty() { pt + (pb - pt - HEAD_TEXT_H) / 2.0 } else { pt + PAD };
        let hb = D2D_RECT_F { left: pl + PAD + 4.0, top: head_top, right: pr - PAD - 4.0, bottom: head_top + HEAD_TEXT_H };
        if items.is_empty() {
            text("Tasca", &fonts.head, hb, &t2);
        } else {
            text(&format!("Tasca · {}", items.len()), &fonts.head, hb, &t2);
            text("Clear all", &fonts.head_r, hb, &t2);
        }

        // Rows, clipped to the scroll viewport.
        let rows_top = l.rows_top();
        let view = D2D_RECT_F { left: pl, top: rows_top, right: pr, bottom: rows_top + l.rows_view_h() };
        rt.PushAxisAlignedClip(&view, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let mut row_result = Ok(());
        for (i, it) in items.iter().enumerate() {
            let y = rows_top + i as f32 * ROW_H - scroll;
            if y + ROW_H < view.top || y > view.bottom {
                continue;
            }
            let (x0, x1) = (pl + PAD, pr - PAD);
            let bg = if v.model.is_selected(it.id) {
                SELECTED_A
            } else if v.hover == Some(it.id) {
                HOVER_A
            } else {
                0.0
            };
            if bg > 0.0 {
                rt.FillRoundedRectangle(&rr(x0, y, x1, y + ROW_H, ROW_RADIUS), &white(bg)?);
            }
            let o = if it.missing() { MISSING_OPACITY } else { 1.0 };
            let (tx, ty) = (x0 + 4.0, y + 5.0);
            row_result = tiles.draw(rt, it, D2D_RECT_F { left: tx, top: ty, right: tx + TILE, bottom: ty + TILE }, o);
            if row_result.is_err() {
                break;
            }
            if it.is_stack() {
                let n = it.files.len();
                let bw = badge_w(n);
                let badge = rr(tx + TILE + 4.0 - bw, ty - 4.0, tx + TILE + 4.0, ty + 10.0, 7.0);
                rt.FillRoundedRectangle(&badge, &white(o)?);
                text(&n.to_string(), &fonts.badge, badge.rect, &brush(0.07, 0.07, 0.08, o)?);
            }
            let mx = tx + TILE + 9.0;
            text(&it.title(), &fonts.name, D2D_RECT_F { left: mx, top: ty + 0.5, right: x1 - 4.0, bottom: ty + 16.0 }, &white(T1 * o)?);
            text(&it.subtitle(), &fonts.sub, D2D_RECT_F { left: mx, top: ty + 16.0, right: x1 - 4.0, bottom: ty + 30.0 }, &white(T2 * o)?);
        }
        rt.PopAxisAlignedClip();
        row_result?;

        // Overflow: a 3 DIP scroll thumb on the right.
        if let Some((top, th)) = l.thumb(scroll) {
            rt.FillRoundedRectangle(&rr(pr - 2.0 - THUMB_W, top, pr - 2.0, top + th, THUMB_W / 2.0), &white(0.22)?);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::badge_w;

    #[test]
    fn badge_grows_past_two_digits() {
        assert_eq!(badge_w(2), 15.0);
        assert_eq!(badge_w(99), 15.0);
        assert_eq!(badge_w(100), 20.0);
        assert_eq!(badge_w(1500), 25.0);
    }
}
