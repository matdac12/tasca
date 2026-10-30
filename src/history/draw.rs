//! Drawing the History panel (the mockup `clipboard-history-a.html`, mode 1):
//! a Direct2D software DC render target over a premultiplied top-down 32 bpp
//! DIB, DirectWrite text, presented with `UpdateLayeredWindow` like the Shelf.
//! The whole bitmap is re-rendered on any change; the fade-in only re-presents
//! it.

use std::cell::RefCell;

use windows::core::{Result, HSTRING};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};
use windows_numerics::Vector2;

use super::layout::{Layout, LABEL_H, PAD, RADIUS, ROW_H, ROW_RADIUS, SIDE};
use crate::shelf::render::{font_family, rr, text_format, wide, Surface};

/// Text alphas: primary, secondary, tertiary white.
const T1: f32 = 0.88;
const T2: f32 = 0.56;
const T3: f32 = 0.34;
const GLASS: (f32, f32, f32, f32) = (18.0 / 255.0, 18.0 / 255.0, 20.0 / 255.0, 0.92);
const HAIRLINE_A: f32 = 0.14;
const RULE_A: f32 = 0.12;
const HOVER_A: f32 = 0.08;
const SELECTED_A: f32 = 0.14;
/// The grey number column on the left of a row.
const NUM_W: f32 = 12.0;
/// The lens glyph's box and its gap to the search text.
const LENS_W: f32 = 13.0;
const LENS_GAP: f32 = 8.0;
/// The pin glyph's box and its gap to the type text.
const PIN_W: f32 = 10.0;
const PIN_GAP: f32 = 4.0;
/// Rows that carry an Alt+number.
const NUMBERED: usize = 9;

/// One row as the panel shows it.
pub struct RowView {
    pub title: String,
    pub sub: String,
    /// Shows the pin glyph before `sub`.
    pub pinned: bool,
}

/// What one frame shows.
pub struct View {
    pub layout: Layout,
    /// The rows in the viewport only, starting at row `first`.
    pub rows: Vec<RowView>,
    pub first: usize,
    pub selected: Option<usize>,
    pub hover: Option<usize>,
    pub scroll: f32,
    /// The search text (empty shows the placeholder).
    pub query: String,
    /// Footer text on the right ("14 Clips"): every Clip, even when a search shows fewer rows.
    pub count: String,
}

struct Fonts {
    search: IDWriteTextFormat,
    num: IDWriteTextFormat,
    name: IDWriteTextFormat,
    sub: IDWriteTextFormat,
    label: IDWriteTextFormat,
    foot: IDWriteTextFormat,
    foot_r: IDWriteTextFormat,
    empty: IDWriteTextFormat,
    /// To measure the search text (the caret sits after it).
    dw: IDWriteFactory,
}

pub struct Renderer {
    d2d: ID2D1Factory,
    rt: Option<ID2D1DCRenderTarget>,
    fonts: Fonts,
    surf: Option<Surface>,
}

thread_local! {
    /// Own cell, apart from the shell's: rendering never holds the shell.
    static RENDERER: RefCell<Option<Renderer>> = const { RefCell::new(None) };
}

/// Build the renderer now (fonts are looked up once). Failure is retried by
/// the next `render`.
pub fn init() -> Result<()> {
    RENDERER.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            *c = Some(Renderer::new()?);
        }
        Ok(())
    })
}

/// Render `view` and hand the bitmap to `hwnd` at `pos` with `alpha`.
pub fn render(hwnd: HWND, view: &View, pos: POINT, alpha: u8) -> Result<()> {
    init()?;
    RENDERER.with(|c| match c.borrow_mut().as_mut() {
        Some(r) => {
            r.render(view)?;
            r.present(hwnd, pos, alpha)
        }
        None => Ok(()),
    })
}

/// Re-present the last bitmap (the fade-in).
pub fn present(hwnd: HWND, pos: POINT, alpha: u8) -> Result<()> {
    RENDERER.with(|c| match c.borrow().as_ref() {
        Some(r) => r.present(hwnd, pos, alpha),
        None => Ok(()),
    })
}

pub fn destroy() {
    RENDERER.with(|c| *c.borrow_mut() = None);
}

impl Renderer {
    fn new() -> Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let fam: HSTRING = font_family(&dw);
            let n = DWRITE_FONT_WEIGHT_NORMAL;
            let lead = DWRITE_TEXT_ALIGNMENT_LEADING;
            let trail = DWRITE_TEXT_ALIGNMENT_TRAILING;
            let fonts = Fonts {
                search: text_format(&dw, &fam, 13.0, n, lead, true)?,
                num: text_format(&dw, &fam, 12.0, n, trail, true)?,
                name: text_format(&dw, &fam, 12.5, n, lead, true)?,
                sub: text_format(&dw, &fam, 10.5, n, lead, true)?,
                label: text_format(&dw, &fam, 10.5, n, lead, true)?,
                foot: text_format(&dw, &fam, 11.0, n, lead, true)?,
                foot_r: text_format(&dw, &fam, 11.0, n, trail, true)?,
                empty: text_format(&dw, &fam, 12.0, n, DWRITE_TEXT_ALIGNMENT_CENTER, true)?,
                dw: dw.clone(),
            };
            Ok(Self { d2d, rt: None, fonts, surf: None })
        }
    }

    /// Render `v` into the bitmap (resized to the layout's pixel size).
    fn render(&mut self, v: &View) -> Result<()> {
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
        }
        let (Some(surf), Some(rt)) = (self.surf.as_ref(), self.rt.as_ref()) else { unreachable!() };
        let dpi = v.layout.scale * 96.0;
        let r = unsafe {
            rt.SetDpi(dpi, dpi);
            rt.BindDC(surf.dc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
            rt.BeginDraw();
            let drawn = draw(rt, &self.fonts, v);
            let ended = rt.EndDraw(None, None);
            drawn.and(ended)
        };
        if r.is_err() {
            // D2DERR_RECREATE_TARGET and friends: start from a fresh target next time.
            self.rt = None;
        }
        r
    }

    fn present(&self, hwnd: HWND, pos: POINT, alpha: u8) -> Result<()> {
        let Some(surf) = self.surf.as_ref() else { return Ok(()) };
        let size = SIZE { cx: surf.w, cy: surf.h };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: alpha,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe { UpdateLayeredWindow(hwnd, None, Some(&pos), Some(&size), Some(surf.dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA) }
    }
}

unsafe fn put(rt: &ID2D1DCRenderTarget, s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush) {
    unsafe { rt.DrawText(&wide(s), f, &r, b, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL) }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

unsafe fn draw(rt: &ID2D1DCRenderTarget, fonts: &Fonts, v: &View) -> Result<()> {
    unsafe {
        let brush = |r: f32, g: f32, b: f32, a: f32| -> Result<ID2D1SolidColorBrush> {
            rt.CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a }, None)
        };
        let white = |a: f32| brush(1.0, 1.0, 1.0, a);
        let text = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| put(rt, s, f, r, b);
        let l = &v.layout;
        let p = l.panel();
        let (pl, pt, pr, pb) = (p.left, p.top, p.right, p.bottom);
        let hair = l.hair();
        let scroll = l.clamp_scroll(v.scroll);
        let (x0, x1) = (pl + PAD, pr - PAD);

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

        // Search line: lens, then the placeholder or the typed text; a rule under it.
        let s = l.search();
        let mid = (s.top + s.bottom) / 2.0;
        let lens = white(T3)?;
        let (cx, cy) = (s.left + SIDE + 5.5, mid - 1.0);
        rt.DrawEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: 4.5, radiusY: 4.5 }, &lens, 1.3, None);
        rt.DrawLine(Vector2 { X: cx + 3.7, Y: cy + 3.7 }, Vector2 { X: cx + 7.0, Y: cy + 7.0 }, &lens, 1.3, None);
        let tx = s.left + SIDE + LENS_W + LENS_GAP;
        let line = rect(tx, s.top, s.right - SIDE, s.bottom);
        let mut caret_x = tx;
        if v.query.is_empty() {
            text("Type to search", &fonts.search, line, &lens);
        } else {
            text(&v.query, &fonts.search, line, &white(T1)?);
            let q = wide(&v.query);
            let layout = fonts.dw.CreateTextLayout(&q, &fonts.search, 4096.0, 40.0)?;
            let mut m = Default::default();
            layout.GetMetrics(&mut m)?;
            caret_x = (tx + m.widthIncludingTrailingWhitespace).min(line.right - 1.0);
        }
        // The caret stays at the end of the text.
        rt.FillRectangle(&rect(caret_x, mid - 8.0, caret_x + 1.0, mid + 8.0), &white(T1)?);
        let rule = white(RULE_A)?;
        rt.FillRectangle(&rect(s.left, s.bottom, s.right, s.bottom + hair), &rule);

        // The list, clipped to its viewport.
        let top = l.list_top();
        let view = rect(x0, top, x1, top + l.list_h());
        rt.PushAxisAlignedClip(&view, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let drawn = draw_list(rt, fonts, v, scroll);
        rt.PopAxisAlignedClip();
        drawn?;

        // Footer: a rule, "Clear" on the left, the count on the right.
        let ft = l.footer_top();
        rt.FillRectangle(&rect(x0, ft, x1, ft + hair), &rule);
        let foot = rect(x0 + SIDE, ft + hair, x1 - SIDE, ft + hair + super::layout::FOOT_H);
        let t2 = white(T2)?;
        text("Clear", &fonts.foot, foot, &t2);
        text(&v.count, &fonts.foot_r, foot, &t2);
        Ok(())
    }
}

/// A small pushpin (the mockup's 10 DIP glyph) with its left edge at `x` and
/// centred on `cy`: a round head and a slanted needle.
unsafe fn draw_pin(rt: &ID2D1DCRenderTarget, x: f32, cy: f32, b: &ID2D1SolidColorBrush) {
    unsafe {
        let head = Vector2 { X: x + 6.4, Y: cy - 2.6 };
        rt.FillEllipse(&D2D1_ELLIPSE { point: head, radiusX: 2.9, radiusY: 2.9 }, b);
        rt.DrawLine(Vector2 { X: x + 5.0, Y: cy - 1.2 }, Vector2 { X: x + 0.6, Y: cy + 4.0 }, b, 1.2, None);
    }
}

/// Labels and rows (or the empty state), in list-viewport coordinates.
unsafe fn draw_list(
    rt: &ID2D1DCRenderTarget,
    fonts: &Fonts,
    v: &View,
    scroll: f32,
) -> Result<()> {
    unsafe {
        let white = |a: f32| rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a }, None);
        let l = &v.layout;
        let p = l.panel();
        let (x0, x1) = (p.left + PAD, p.right - PAD);
        let (top, bottom) = (l.list_top(), l.list_top() + l.list_h());
        let (t1, t2, t3) = (white(T1)?, white(T2)?, white(T3)?);
        let text = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| put(rt, s, f, r, b);

        if v.layout.rows == 0 {
            let msg = if v.query.is_empty() { "Nothing copied yet" } else { "No Clip matches" };
            text(msg, &fonts.empty, rect(x0, top, x1, bottom), &t3);
            return Ok(());
        }
        for (at, pinned) in l.labels() {
            let y = top + at - scroll;
            if y + LABEL_H < top || y > bottom {
                continue;
            }
            let name = if pinned { "Pinned" } else { "Recent" };
            text(name, &fonts.label, rect(x0 + SIDE, y + 7.0, x1 - SIDE, y + 21.0), &t3);
        }
        for (k, row) in v.rows.iter().enumerate() {
            let i = v.first + k;
            let y = top + l.row_top(i) - scroll;
            if y + ROW_H < top || y > bottom {
                continue;
            }
            let selected = v.selected == Some(i);
            let bg = if selected {
                SELECTED_A
            } else if v.hover == Some(i) {
                HOVER_A
            } else {
                0.0
            };
            if bg > 0.0 {
                rt.FillRoundedRectangle(&rr(x0, y, x1, y + ROW_H, ROW_RADIUS), &white(bg)?);
            }
            if i < NUMBERED {
                let num = rect(x0 + SIDE, y, x0 + SIDE + NUM_W, y + ROW_H);
                text(&(i + 1).to_string(), &fonts.num, num, if selected { &t2 } else { &t3 });
            }
            let mx = x0 + SIDE + NUM_W + SIDE;
            text(&row.title, &fonts.name, rect(mx, y + 7.0, x1 - SIDE, y + 23.0), &t1);
            let sub_x = if row.pinned {
                draw_pin(rt, mx, y + 30.0, &t2);
                mx + PIN_W + PIN_GAP
            } else {
                mx
            };
            text(&row.sub, &fonts.sub, rect(sub_x, y + 23.0, x1 - SIDE, y + 37.0), &t2);
        }
        Ok(())
    }
}
