//! Drawing the Snippets panel: the same Direct2D glass card as the History,
//! with a title line, one row per Snippet and a footer. The whole bitmap is
//! re-rendered on any change; the fade-in only re-presents it.

use std::cell::RefCell;

use windows::core::{Result, HSTRING};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

use super::layout::{Layout, PAD, RADIUS, ROW_H, ROW_RADIUS, SIDE};
use crate::shelf::render::{font_family, rr, text_format, wide, Surface};

const T1: f32 = 0.88;
const T2: f32 = 0.56;
const T3: f32 = 0.34;
const GLASS: (f32, f32, f32, f32) = (18.0 / 255.0, 18.0 / 255.0, 20.0 / 255.0, 0.92);
const HAIRLINE_A: f32 = 0.14;
const RULE_A: f32 = 0.12;
const HOVER_A: f32 = 0.08;
const SELECTED_A: f32 = 0.14;
const NUM_W: f32 = 12.0;
const NUMBERED: usize = 9;

pub struct RowView {
    pub title: String,
    pub sub: String,
}

pub struct View {
    pub layout: Layout,
    pub rows: Vec<RowView>,
    pub first: usize,
    pub selected: Option<usize>,
    pub hover: Option<usize>,
    pub scroll: f32,
    pub count: String,
}

struct Fonts {
    title: IDWriteTextFormat,
    num: IDWriteTextFormat,
    name: IDWriteTextFormat,
    sub: IDWriteTextFormat,
    foot: IDWriteTextFormat,
    foot_r: IDWriteTextFormat,
    empty: IDWriteTextFormat,
    /// Holds the factory alive for the formats above.
    _dw: IDWriteFactory,
}

struct Renderer {
    d2d: ID2D1Factory,
    rt: Option<ID2D1DCRenderTarget>,
    fonts: Fonts,
    surf: Option<Surface>,
}

thread_local! {
    static RENDERER: RefCell<Option<Renderer>> = const { RefCell::new(None) };
}

pub fn init() -> Result<()> {
    RENDERER.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            *c = Some(Renderer::new()?);
        }
        Ok(())
    })
}

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
                title: text_format(&dw, &fam, 13.0, n, lead, true)?,
                num: text_format(&dw, &fam, 12.0, n, trail, true)?,
                name: text_format(&dw, &fam, 12.5, n, lead, true)?,
                sub: text_format(&dw, &fam, 10.5, n, lead, true)?,
                foot: text_format(&dw, &fam, 11.0, n, lead, true)?,
                foot_r: text_format(&dw, &fam, 11.0, n, trail, true)?,
                empty: text_format(&dw, &fam, 12.0, n, DWRITE_TEXT_ALIGNMENT_CENTER, true)?,
                _dw: dw,
            };
            Ok(Self {
                d2d,
                rt: None,
                fonts,
                surf: None,
            })
        }
    }

    fn render(&mut self, v: &View) -> Result<()> {
        let (w, h) = v.layout.size_px();
        if self.surf.as_ref().map(|s| (s.w, s.h)) != Some((w, h)) {
            self.surf = None;
            self.surf = Some(Surface::new(w, h)?);
        }
        if self.rt.is_none() {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            self.rt = Some(unsafe { self.d2d.CreateDCRenderTarget(&props)? });
        }
        let (Some(surf), Some(rt)) = (self.surf.as_ref(), self.rt.as_ref()) else {
            unreachable!()
        };
        let dpi = v.layout.scale * 96.0;
        let r = unsafe {
            rt.SetDpi(dpi, dpi);
            rt.BindDC(
                surf.dc,
                &RECT {
                    left: 0,
                    top: 0,
                    right: w,
                    bottom: h,
                },
            )?;
            rt.BeginDraw();
            let drawn = draw(rt, &self.fonts, v);
            let ended = rt.EndDraw(None, None);
            drawn.and(ended)
        };
        if r.is_err() {
            self.rt = None;
        }
        r
    }

    fn present(&self, hwnd: HWND, pos: POINT, alpha: u8) -> Result<()> {
        let Some(surf) = self.surf.as_ref() else {
            return Ok(());
        };
        let size = SIZE {
            cx: surf.w,
            cy: surf.h,
        };
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

unsafe fn put(
    rt: &ID2D1DCRenderTarget,
    s: &str,
    f: &IDWriteTextFormat,
    r: D2D_RECT_F,
    b: &ID2D1SolidColorBrush,
) {
    unsafe {
        rt.DrawText(
            &wide(s),
            f,
            &r,
            b,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        )
    }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}

unsafe fn draw(rt: &ID2D1DCRenderTarget, fonts: &Fonts, v: &View) -> Result<()> {
    unsafe {
        let brush = |r: f32, g: f32, b: f32, a: f32| -> Result<ID2D1SolidColorBrush> {
            rt.CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a }, None)
        };
        let white = |a: f32| brush(1.0, 1.0, 1.0, a);
        let text = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
            put(rt, s, f, r, b)
        };
        let l = &v.layout;
        let p = l.panel();
        let (pl, pt, pr, pb) = (p.left, p.top, p.right, p.bottom);
        let hair = l.hair();
        let scroll = l.clamp_scroll(v.scroll);
        let (x0, x1) = (pl + PAD, pr - PAD);

        rt.Clear(Some(&D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        }));
        for i in (0..10).rev() {
            let e = i as f32 * 1.6;
            let a = 0.045 * (1.0 - i as f32 / 10.0);
            rt.FillRoundedRectangle(
                &rr(pl - e, pt + 4.0 - e * 0.6, pr + e, pb + 6.0 + e, RADIUS + e),
                &brush(0.0, 0.0, 0.0, a)?,
            );
        }
        let (gr, gg, gb, ga) = GLASS;
        rt.FillRoundedRectangle(&rr(pl, pt, pr, pb, RADIUS), &brush(gr, gg, gb, ga)?);
        let hh = hair / 2.0;
        rt.DrawRoundedRectangle(
            &rr(pl + hh, pt + hh, pr - hh, pb - hh, RADIUS),
            &white(HAIRLINE_A)?,
            hair,
            None,
        );

        let h = l.header();
        text(
            "Snippets",
            &fonts.title,
            rect(h.left + SIDE, h.top, h.right - SIDE, h.bottom),
            &white(T1)?,
        );
        let rule = white(RULE_A)?;
        rt.FillRectangle(&rect(h.left, h.bottom, h.right, h.bottom + hair), &rule);

        let top = l.list_top();
        let view = rect(x0, top, x1, top + l.list_h());
        rt.PushAxisAlignedClip(&view, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let drawn = draw_list(rt, fonts, v, scroll);
        rt.PopAxisAlignedClip();
        drawn?;

        let ft = l.footer_top();
        rt.FillRectangle(&rect(x0, ft, x1, ft + hair), &rule);
        let foot = rect(
            x0 + SIDE,
            ft + hair,
            x1 - SIDE,
            ft + hair + super::layout::FOOT_H,
        );
        let t2 = white(T2)?;
        text("New", &fonts.foot, foot, &t2);
        text(&v.count, &fonts.foot_r, foot, &t2);
        Ok(())
    }
}

unsafe fn draw_list(rt: &ID2D1DCRenderTarget, fonts: &Fonts, v: &View, scroll: f32) -> Result<()> {
    unsafe {
        let white = |a: f32| {
            rt.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a,
                },
                None,
            )
        };
        let l = &v.layout;
        let p = l.panel();
        let (x0, x1) = (p.left + PAD, p.right - PAD);
        let (top, bottom) = (l.list_top(), l.list_top() + l.list_h());
        let (t1, t2, t3) = (white(T1)?, white(T2)?, white(T3)?);
        let text = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
            put(rt, s, f, r, b)
        };

        if v.layout.rows == 0 {
            text(
                "No snippets yet",
                &fonts.empty,
                rect(x0, top, x1, bottom),
                &t3,
            );
            return Ok(());
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
            let mx = if i < NUMBERED {
                text(
                    &(i + 1).to_string(),
                    &fonts.num,
                    rect(x0 + SIDE, y, x0 + SIDE + NUM_W, y + ROW_H),
                    if selected { &t2 } else { &t3 },
                );
                x0 + SIDE + NUM_W + SIDE
            } else {
                x0 + SIDE
            };
            let right = x1 - SIDE;
            if row.sub.is_empty() {
                text(&row.title, &fonts.name, rect(mx, y, right, y + ROW_H), &t1);
            } else {
                text(
                    &row.title,
                    &fonts.name,
                    rect(mx, y + 7.0, right, y + 23.0),
                    &t1,
                );
                text(
                    &row.sub,
                    &fonts.sub,
                    rect(mx, y + 23.0, right, y + 37.0),
                    &t2,
                );
            }
        }
        Ok(())
    }
}
