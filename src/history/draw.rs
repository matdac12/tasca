//! Drawing the History window. Deliberately minimal (plain GDI, dark
//! background, one line per Clip): the panel's real look replaces this file.
//! `window.rs` only needs `View`, `size_px` and `paint`.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, SelectObject, SetBkMode, SetTextColor,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_QUALITY, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, FF_DONTCARE, FW_NORMAL, HDC, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::core::w;

/// Panel width and row height, in DIP.
const WIDTH: f32 = 400.0;
const ROW_H: f32 = 44.0;
const PAD: f32 = 6.0;
const TEXT_PAD: f32 = 12.0;
/// Rows shown at once; more Clips scroll with the selection.
pub const MAX_ROWS: usize = 9;

const BG: COLORREF = rgb(24, 24, 27);
const SELECTED: COLORREF = rgb(58, 58, 66);
const TEXT: COLORREF = rgb(228, 228, 230);
const DIM: COLORREF = rgb(130, 130, 136);

const fn rgb(r: u32, g: u32, b: u32) -> COLORREF {
    COLORREF(r | (g << 8) | (b << 16))
}

/// What one frame shows: the visible rows' texts and which one is selected.
pub struct View {
    pub rows: Vec<String>,
    pub selected: Option<usize>,
    /// DPI / 96.
    pub scale: f32,
}

/// The first Clip shown when `selected` must stay on screen.
pub fn first_visible(selected: usize) -> usize {
    selected.saturating_sub(MAX_ROWS - 1)
}

/// Window size in pixels for `rows` visible rows (an empty History shows one
/// row of hint text).
pub fn size_px(rows: usize, scale: f32) -> (i32, i32) {
    let rows = rows.clamp(1, MAX_ROWS) as f32;
    ((WIDTH * scale).round() as i32, ((rows * ROW_H + 2.0 * PAD) * scale).round() as i32)
}

/// A Clip's text as one display line: whitespace runs become single spaces,
/// and it stops after `LINE_CHARS` characters (the row ellipsizes anyway).
pub fn one_line(text: &str) -> String {
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
        if line.chars().count() >= LINE_CHARS {
            break;
        }
    }
    line.chars().take(LINE_CHARS).collect()
}

const LINE_CHARS: usize = 300;

/// Draw `view` over the whole client area.
pub fn paint(hdc: HDC, view: &View) {
    let (w, h) = size_px(view.rows.len(), view.scale);
    let px = |dip: f32| (dip * view.scale).round() as i32;
    unsafe {
        let bg = CreateSolidBrush(BG);
        FillRect(hdc, &RECT { left: 0, top: 0, right: w, bottom: h }, bg);
        let _ = DeleteObject(bg.into());

        let font = CreateFontW(
            -px(13.0),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            DEFAULT_QUALITY,
            FF_DONTCARE.0 as u32,
            w!("Segoe UI"),
        );
        let old = SelectObject(hdc, font.into());
        SetBkMode(hdc, TRANSPARENT);

        let flags = DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX;
        if view.rows.is_empty() {
            SetTextColor(hdc, DIM);
            let mut line: Vec<u16> = "Nothing copied yet".encode_utf16().collect();
            let mut r = row_rect(0, w, view.scale);
            DrawTextW(hdc, &mut line, &mut r, flags);
        }
        for (i, text) in view.rows.iter().enumerate() {
            let mut r = row_rect(i, w, view.scale);
            if view.selected == Some(i) {
                let b = CreateSolidBrush(SELECTED);
                FillRect(hdc, &r, b);
                let _ = DeleteObject(b.into());
            }
            SetTextColor(hdc, TEXT);
            r.left += px(TEXT_PAD);
            r.right -= px(TEXT_PAD);
            let mut line: Vec<u16> = text.encode_utf16().collect();
            DrawTextW(hdc, &mut line, &mut r, flags);
        }

        SelectObject(hdc, old);
        let _ = DeleteObject(font.into());
    }
}

fn row_rect(i: usize, width: i32, scale: f32) -> RECT {
    let top = ((PAD + i as f32 * ROW_H) * scale).round() as i32;
    let pad = (PAD * scale).round() as i32;
    RECT { left: pad, top, right: width - pad, bottom: top + (ROW_H * scale).round() as i32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_and_multiline_text_is_one_line() {
        assert_eq!(one_line("a\r\n  b\t c"), "a b c");
    }

    #[test]
    fn a_long_clip_is_cut_after_collapsing_its_whitespace() {
        let text = format!("

    {}
{}", "word ".repeat(200), "x".repeat(10_000));
        let line = one_line(&text);
        assert!(line.starts_with("word word"));
        assert!(line.chars().count() <= LINE_CHARS);
    }

    #[test]
    fn the_window_grows_with_the_rows_up_to_nine() {
        assert_eq!(size_px(0, 1.0), size_px(1, 1.0));
        assert_eq!(size_px(1, 1.0), (400, 56));
        assert_eq!(size_px(9, 1.0), (400, 408));
        assert_eq!(size_px(40, 1.0), size_px(9, 1.0));
        assert_eq!(size_px(1, 1.5), (600, 84));
    }

    #[test]
    fn the_selection_stays_inside_the_visible_rows() {
        assert_eq!(first_visible(0), 0);
        assert_eq!(first_visible(8), 0);
        assert_eq!(first_visible(9), 1);
        assert_eq!(first_visible(20), 12);
    }
}
