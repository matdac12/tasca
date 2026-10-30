//! Pure geometry of the Snippets panel and its hit-testing. No Win32.
//!
//! The numbers are the History panel's (the same glass card, row and footer)
//! with a title line where the History has its search box, and no section
//! labels. Everything is in DIP; `scale` = monitor DPI / 96.

pub const PANEL_W: f32 = 400.0;
pub const PAD: f32 = 6.0;
pub const RADIUS: f32 = 12.0;
pub const HEADER_H: f32 = 34.0;
pub const GAP: f32 = 4.0;
pub const ROW_H: f32 = 44.0;
pub const ROW_RADIUS: f32 = 7.0;
pub const SIDE: f32 = 10.0;
/// The list stops growing at nine rows.
pub const LIST_MAX_H: f32 = 9.0 * ROW_H;
pub const FOOT_H: f32 = 28.0;
pub const EMPTY_H: f32 = 52.0;
pub const MARGIN_L: f32 = 8.0;
pub const MARGIN_T: f32 = 10.0;
pub const MARGIN_R: f32 = 16.0;
pub const MARGIN_B: f32 = 24.0;
/// Where "New" can be hit, measured from the footer's left padding.
const NEW_HIT_W: f32 = 48.0;
const WHEEL_DELTA: f32 = 120.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Margin,
    Row(usize),
    New,
    Panel,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub scale: f32,
    pub rows: usize,
}

impl Layout {
    pub fn new(scale: f32, rows: usize) -> Self {
        Self { scale, rows }
    }

    pub fn hair(&self) -> f32 {
        1.0 / self.scale
    }

    pub fn row_top(&self, i: usize) -> f32 {
        i as f32 * ROW_H
    }

    pub fn content_h(&self) -> f32 {
        if self.rows == 0 {
            EMPTY_H
        } else {
            self.rows as f32 * ROW_H
        }
    }

    pub fn list_h(&self) -> f32 {
        self.content_h().min(LIST_MAX_H)
    }

    pub fn panel_h(&self) -> f32 {
        PAD + HEADER_H + self.hair() + GAP + self.list_h() + GAP + self.hair() + FOOT_H + PAD
    }

    pub fn panel(&self) -> RectF {
        RectF {
            left: MARGIN_L,
            top: MARGIN_T,
            right: MARGIN_L + PANEL_W,
            bottom: MARGIN_T + self.panel_h(),
        }
    }

    pub fn header(&self) -> RectF {
        let p = self.panel();
        RectF {
            left: p.left + PAD,
            top: p.top + PAD,
            right: p.right - PAD,
            bottom: p.top + PAD + HEADER_H,
        }
    }

    pub fn list_top(&self) -> f32 {
        MARGIN_T + PAD + HEADER_H + self.hair() + GAP
    }

    pub fn footer_top(&self) -> f32 {
        self.list_top() + self.list_h() + GAP
    }

    pub fn max_scroll(&self) -> f32 {
        (self.content_h() - self.list_h()).max(0.0)
    }

    pub fn clamp_scroll(&self, scroll: f32) -> f32 {
        scroll.clamp(0.0, self.max_scroll())
    }

    /// One row per wheel notch. Positive `delta` (wheel up) scrolls toward the top.
    pub fn wheel(&self, scroll: f32, delta: i32) -> f32 {
        self.clamp_scroll(scroll - delta as f32 / WHEEL_DELTA * ROW_H)
    }

    pub fn scroll_to_show(&self, scroll: f32, i: usize) -> f32 {
        if self.rows == 0 {
            return 0.0;
        }
        let i = i.min(self.rows - 1);
        let top = self.row_top(i);
        let bottom = top + ROW_H;
        let scroll = self.clamp_scroll(scroll);
        let shown = if top < scroll {
            top
        } else if bottom > scroll + self.list_h() {
            bottom - self.list_h()
        } else {
            scroll
        };
        self.clamp_scroll(shown)
    }

    pub fn visible_rows(&self, scroll: f32) -> std::ops::Range<usize> {
        let scroll = self.clamp_scroll(scroll);
        let bottom = scroll + self.list_h();
        let first = (0..self.rows)
            .find(|&i| self.row_top(i) + ROW_H > scroll)
            .unwrap_or(self.rows);
        let end = (first..self.rows)
            .find(|&i| self.row_top(i) >= bottom)
            .unwrap_or(self.rows);
        first..end
    }

    pub fn size_px(&self) -> (i32, i32) {
        let w = (MARGIN_L + PANEL_W + MARGIN_R) * self.scale;
        let h = (MARGIN_T + self.panel_h() + MARGIN_B) * self.scale;
        (w.ceil() as i32, h.ceil() as i32)
    }

    pub fn panel_size_px(&self) -> (i32, i32) {
        (
            (PANEL_W * self.scale).round() as i32,
            (self.panel_h() * self.scale).round() as i32,
        )
    }

    pub fn panel_offset_px(&self) -> (i32, i32) {
        (
            (MARGIN_L * self.scale).round() as i32,
            (MARGIN_T * self.scale).round() as i32,
        )
    }

    pub fn hit(&self, x_px: i32, y_px: i32, scroll: f32) -> Hit {
        let (x, y) = (x_px as f32 / self.scale, y_px as f32 / self.scale);
        let p = self.panel();
        if x < p.left || x >= p.right || y < p.top || y >= p.bottom {
            return Hit::Margin;
        }
        let list_top = self.list_top();
        if y < list_top {
            return Hit::Panel;
        }
        if y >= self.footer_top() {
            let new_right = p.left + PAD + SIDE + NEW_HIT_W;
            return if x < new_right && y < self.footer_top() + self.hair() + FOOT_H {
                Hit::New
            } else {
                Hit::Panel
            };
        }
        if y >= list_top + self.list_h() || x < p.left + PAD || x >= p.right - PAD {
            return Hit::Panel;
        }
        let at = y - list_top + self.clamp_scroll(scroll);
        (0..self.rows)
            .find(|&i| at >= self.row_top(i) && at < self.row_top(i) + ROW_H)
            .map_or(Hit::Panel, Hit::Row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    /// Padding, header, footer and the two hairlines, at `scale`.
    fn chrome(scale: f32) -> f32 {
        82.0 + 2.0 / scale
    }

    #[test]
    fn the_panel_grows_with_its_rows_up_to_nine() {
        assert!(close(Layout::new(1.0, 0).panel_h(), chrome(1.0) + 52.0));
        assert!(close(Layout::new(1.0, 3).panel_h(), chrome(1.0) + 132.0));
        assert!(close(Layout::new(1.0, 9).panel_h(), chrome(1.0) + 396.0));
        assert!(close(Layout::new(1.0, 20).panel_h(), chrome(1.0) + 396.0));
        assert_eq!(Layout::new(1.0, 3).size_px(), (424, 250));
    }

    #[test]
    fn hits_are_the_row_the_new_button_or_the_margin() {
        let l = Layout::new(1.0, 3);
        assert_eq!(l.hit(0, 0, 0.0), Hit::Margin);
        assert_eq!(
            l.hit(200, (l.list_top() + ROW_H / 2.0) as i32, 0.0),
            Hit::Row(0)
        );
        let long = Layout::new(1.0, 20);
        assert_eq!(
            long.hit(200, (long.list_top() + ROW_H / 2.0) as i32, ROW_H),
            Hit::Row(1)
        );
        assert_eq!(l.hit(40, (l.footer_top() + 8.0) as i32, 0.0), Hit::New);
        assert_eq!(l.hit(200, (l.footer_top() + 8.0) as i32, 0.0), Hit::Panel);
    }

    #[test]
    fn the_wheel_moves_one_row_and_the_selection_stays_in_view() {
        let l = Layout::new(1.0, 20);
        assert_eq!(l.wheel(0.0, -120), ROW_H);
        assert_eq!(l.wheel(ROW_H, 120), 0.0);
        assert_eq!(l.visible_rows(0.0), 0..9);
        assert!(close(
            l.scroll_to_show(0.0, 10),
            10.0 * ROW_H + ROW_H - l.list_h()
        ));
    }
}
