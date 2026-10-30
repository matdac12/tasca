//! Pure geometry of the History panel (the mockup `clipboard-history-a.html`,
//! mode 1) and its hit-testing. No Win32.
//!
//! Everything is in DIP inside the window bitmap, whose top-left is (0, 0); the
//! panel sits inside a transparent margin that holds its shadow. `scale` =
//! monitor DPI / 96 turns DIP into physical pixels. Hairlines are one physical
//! pixel (`1 / scale` DIP), as in the mockup.

pub const PANEL_W: f32 = 400.0;
pub const PAD: f32 = 6.0;
pub const RADIUS: f32 = 12.0;
pub const SEARCH_H: f32 = 34.0;
/// Space between the search line's hairline (and the list) and what follows.
pub const GAP: f32 = 4.0;
pub const ROW_H: f32 = 44.0;
pub const ROW_RADIUS: f32 = 7.0;
/// Side padding inside a row, a label, the search line and the footer.
pub const SIDE: f32 = 10.0;
/// A section label ("Pinned" / "Recent"): 7 above, 3 below its 11 DIP line.
pub const LABEL_H: f32 = 21.0;
/// The list stops growing at nine rows plus the two labels.
pub const LIST_MAX_H: f32 = 438.0;
pub const FOOT_H: f32 = 28.0;
/// "No Clip matches": 18 DIP above and below one 16 DIP line.
pub const EMPTY_H: f32 = 52.0;
/// Where "Clear" can be hit, measured from the footer's left padding.
const CLEAR_HIT_W: f32 = 56.0;
/// Shadow margins around the panel (the Shelf's).
pub const MARGIN_L: f32 = 8.0;
pub const MARGIN_T: f32 = 10.0;
pub const MARGIN_R: f32 = 16.0;
pub const MARGIN_B: f32 = 24.0;
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
    /// The transparent shadow band around the panel.
    Margin,
    Search,
    /// Row index in the shown list, 0 = top.
    Row(usize),
    Clear,
    /// Inside the panel on nothing in particular.
    Panel,
}

/// The panel's geometry for one monitor scale and one list.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub scale: f32,
    /// Rows in the shown list.
    pub rows: usize,
    /// How many of them, from the top, are Pinned Clips.
    pub pinned: usize,
    /// Labels are hidden while searching.
    pub searching: bool,
}

impl Layout {
    pub fn new(scale: f32, rows: usize, pinned: usize, searching: bool) -> Self {
        Self { scale, rows, pinned: pinned.min(rows), searching }
    }

    pub fn hair(&self) -> f32 {
        1.0 / self.scale
    }

    fn pinned_label(&self) -> bool {
        !self.searching && self.pinned > 0
    }

    fn recent_label(&self) -> bool {
        !self.searching && self.rows > self.pinned
    }

    /// Section labels with their top in list-content DIP: `(top, is_pinned)`.
    pub fn labels(&self) -> Vec<(f32, bool)> {
        let mut out = Vec::new();
        if self.pinned_label() {
            out.push((0.0, true));
        }
        if self.recent_label() {
            let above = if self.pinned_label() { LABEL_H } else { 0.0 };
            out.push((above + self.pinned as f32 * ROW_H, false));
        }
        out
    }

    /// Top of row `i` in list-content DIP.
    pub fn row_top(&self, i: usize) -> f32 {
        let labels = self.pinned_label() as usize + (self.recent_label() && i >= self.pinned) as usize;
        labels as f32 * LABEL_H + i as f32 * ROW_H
    }

    /// All rows and labels, in list-content DIP.
    pub fn content_h(&self) -> f32 {
        if self.rows == 0 {
            return EMPTY_H;
        }
        (self.pinned_label() as usize + self.recent_label() as usize) as f32 * LABEL_H + self.rows as f32 * ROW_H
    }

    /// The visible part of the list: grows with its content up to nine rows.
    pub fn list_h(&self) -> f32 {
        self.content_h().min(LIST_MAX_H)
    }

    pub fn panel_h(&self) -> f32 {
        PAD + SEARCH_H + self.hair() + GAP + self.list_h() + GAP + self.hair() + FOOT_H + PAD
    }

    /// The panel inside the window.
    pub fn panel(&self) -> RectF {
        RectF { left: MARGIN_L, top: MARGIN_T, right: MARGIN_L + PANEL_W, bottom: MARGIN_T + self.panel_h() }
    }

    /// The search line (without its hairline).
    pub fn search(&self) -> RectF {
        let p = self.panel();
        RectF { left: p.left + PAD, top: p.top + PAD, right: p.right - PAD, bottom: p.top + PAD + SEARCH_H }
    }

    pub fn list_top(&self) -> f32 {
        MARGIN_T + PAD + SEARCH_H + self.hair() + GAP
    }

    /// The footer's top edge, where its hairline is.
    pub fn footer_top(&self) -> f32 {
        self.list_top() + self.list_h() + GAP
    }

    pub fn max_scroll(&self) -> f32 {
        (self.content_h() - self.list_h()).max(0.0)
    }

    pub fn clamp_scroll(&self, scroll: f32) -> f32 {
        scroll.clamp(0.0, self.max_scroll())
    }

    /// Scroll after a `WM_MOUSEWHEEL` delta (positive = up): one row per notch.
    pub fn wheel(&self, scroll: f32, delta: i32) -> f32 {
        self.clamp_scroll(scroll - delta as f32 / WHEEL_DELTA * ROW_H)
    }

    /// The least scrolling that shows row `i` whole, with its section label
    /// when it is the first of its section.
    pub fn scroll_to_show(&self, scroll: f32, i: usize) -> f32 {
        if self.rows == 0 {
            return 0.0;
        }
        let i = i.min(self.rows - 1);
        let first_of_section = (i == 0 && self.pinned_label()) || (i == self.pinned && self.recent_label());
        let top = self.row_top(i) - if first_of_section { LABEL_H } else { 0.0 };
        let bottom = self.row_top(i) + ROW_H;
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

    /// The rows that show (even partly) at `scroll`: the only ones worth building.
    pub fn visible_rows(&self, scroll: f32) -> std::ops::Range<usize> {
        let scroll = self.clamp_scroll(scroll);
        let bottom = scroll + self.list_h();
        let first = (0..self.rows).find(|&i| self.row_top(i) + ROW_H > scroll).unwrap_or(self.rows);
        let end = (first..self.rows).find(|&i| self.row_top(i) >= bottom).unwrap_or(self.rows);
        first..end
    }

    /// Window size in physical pixels, shadow margins included.
    pub fn size_px(&self) -> (i32, i32) {
        let w = (MARGIN_L + PANEL_W + MARGIN_R) * self.scale;
        let h = (MARGIN_T + self.panel_h() + MARGIN_B) * self.scale;
        (w.ceil() as i32, h.ceil() as i32)
    }

    /// The panel's size in physical pixels (what is placed on the work area).
    pub fn panel_size_px(&self) -> (i32, i32) {
        ((PANEL_W * self.scale).round() as i32, (self.panel_h() * self.scale).round() as i32)
    }

    /// Where the panel starts inside the window, in physical pixels.
    pub fn panel_offset_px(&self) -> (i32, i32) {
        ((MARGIN_L * self.scale).round() as i32, (MARGIN_T * self.scale).round() as i32)
    }

    /// What is under a client-area point (physical pixels).
    pub fn hit(&self, x_px: i32, y_px: i32, scroll: f32) -> Hit {
        let (x, y) = (x_px as f32 / self.scale, y_px as f32 / self.scale);
        let p = self.panel();
        if x < p.left || x >= p.right || y < p.top || y >= p.bottom {
            return Hit::Margin;
        }
        let list_top = self.list_top();
        if y < list_top {
            return if y < p.top + PAD + SEARCH_H { Hit::Search } else { Hit::Panel };
        }
        if y >= self.footer_top() {
            let clear_right = p.left + PAD + SIDE + CLEAR_HIT_W;
            return if x < clear_right && y < self.footer_top() + self.hair() + FOOT_H { Hit::Clear } else { Hit::Panel };
        }
        if y >= list_top + self.list_h() || x < p.left + PAD || x >= p.right - PAD {
            return Hit::Panel;
        }
        let at = y - list_top + self.clamp_scroll(scroll);
        (0..self.rows).find(|&i| at >= self.row_top(i) && at < self.row_top(i) + ROW_H).map_or(Hit::Panel, Hit::Row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    fn layout(rows: usize) -> Layout {
        Layout::new(1.0, rows, 0, false)
    }

    /// Everything but the list: padding, search line and gap, footer and gap.
    fn chrome(scale: f32) -> f32 {
        82.0 + 2.0 / scale
    }

    #[test]
    fn the_panel_grows_with_its_clips_up_to_nine_rows() {
        // No Clips: the empty-state block. Three: the "Recent" label and 3 rows.
        assert!(close(layout(0).panel_h(), chrome(1.0) + 52.0));
        assert!(close(layout(3).panel_h(), chrome(1.0) + 21.0 + 3.0 * 44.0));
        assert!(close(layout(9).panel_h(), chrome(1.0) + 21.0 + 9.0 * 44.0));
        // Twenty: the list stops at 438 DIP and scrolls.
        assert!(close(layout(20).panel_h(), chrome(1.0) + 438.0));
        assert_eq!(layout(20).list_h(), 438.0);
        assert_eq!(layout(10).list_h(), 438.0);
        assert_eq!(layout(21).panel_h(), layout(20).panel_h());
    }

    #[test]
    fn the_panel_height_holds_at_every_scale() {
        for scale in [1.0, 1.5, 2.0] {
            for (rows, list) in [(0, 52.0), (3, 21.0 + 132.0), (9, 21.0 + 396.0), (20, 438.0)] {
                let l = Layout::new(scale, rows, 0, false);
                assert!(close(l.panel_h(), chrome(scale) + list), "scale {scale} rows {rows}");
            }
        }
    }

    #[test]
    fn pins_add_their_own_label() {
        let l = Layout::new(1.0, 5, 2, false);
        assert_eq!(l.content_h(), 2.0 * 21.0 + 5.0 * 44.0);
        assert_eq!(l.labels(), vec![(0.0, true), (21.0 + 2.0 * 44.0, false)]);
        assert_eq!(l.row_top(0), 21.0);
        assert_eq!(l.row_top(2), 21.0 + 2.0 * 44.0 + 21.0);
        // All pinned: no "Recent" label. Searching: no labels at all.
        assert_eq!(Layout::new(1.0, 2, 2, false).labels(), vec![(0.0, true)]);
        let s = Layout::new(1.0, 5, 2, true);
        assert!(s.labels().is_empty());
        assert_eq!(s.content_h(), 5.0 * 44.0);
    }

    #[test]
    fn the_size_follows_the_scale() {
        for scale in [1.0, 1.5, 2.0] {
            let l = Layout::new(scale, 3, 0, false);
            let (w, h) = l.size_px();
            assert_eq!(w, ((8.0 + 400.0 + 16.0) * scale).ceil() as i32);
            assert_eq!(h, ((10.0 + l.panel_h() + 24.0) * scale).ceil() as i32);
            assert_eq!(l.panel_size_px().0, (400.0 * scale) as i32);
            // Hairlines stay one physical pixel.
            assert!(close(l.hair() * scale, 1.0));
        }
        assert_eq!(layout(3).size_px(), (424, 271));
        assert_eq!(Layout::new(1.5, 3, 0, false).panel_offset_px(), (12, 15));
    }

    /// Centre of row `i` (physical pixels) for a layout, unscrolled.
    fn row_point(l: &Layout, i: usize) -> (i32, i32) {
        let y = l.list_top() + l.row_top(i) + ROW_H / 2.0;
        ((200.0 * l.scale) as i32, (y * l.scale) as i32)
    }

    #[test]
    fn a_point_finds_its_row_at_every_scale() {
        for scale in [1.0, 1.5, 2.0] {
            let l = Layout::new(scale, 5, 0, false);
            for i in 0..5 {
                let (x, y) = row_point(&l, i);
                assert_eq!(l.hit(x, y, 0.0), Hit::Row(i), "scale {scale} row {i}");
            }
            // Between the last row and the footer: the panel.
            let below = ((l.list_top() + l.list_h() + 1.0) * scale) as i32;
            assert_eq!(l.hit((200.0 * scale) as i32, below, 0.0), Hit::Panel);
        }
    }

    #[test]
    fn labels_the_search_line_the_footer_and_the_margin_are_not_rows() {
        let l = layout(5);
        let at = |x: f32, y: f32| l.hit(x as i32, y as i32, 0.0);
        assert_eq!(at(2.0, 2.0), Hit::Margin);
        assert_eq!(at(200.0, 30.0), Hit::Search);
        // The "Recent" label is the first 21 DIP of the list.
        assert_eq!(at(200.0, l.list_top() + 5.0), Hit::Panel);
        assert_eq!(at(200.0, l.list_top() + 22.0), Hit::Row(0));
        // Row padding at the sides.
        assert_eq!(at(MARGIN_L + 2.0, l.list_top() + 30.0), Hit::Panel);
        assert_eq!(at(MARGIN_L + 30.0, l.footer_top() + 14.0), Hit::Clear);
        assert_eq!(at(MARGIN_L + 300.0, l.footer_top() + 14.0), Hit::Panel);
        assert_eq!(at(200.0, l.panel().bottom + 1.0), Hit::Margin);
    }

    #[test]
    fn hit_testing_follows_the_scroll() {
        let l = layout(20);
        let y = (l.list_top() + 10.0) as i32;
        assert_eq!(l.hit(200, y, 0.0), Hit::Panel); // the label
        // Scrolled past the label and two rows, the top of the list is row 2.
        let scroll = 21.0 + 2.0 * 44.0;
        assert_eq!(l.hit(200, y, scroll), Hit::Row(2));
        // The last row at the bottom of a fully scrolled list.
        let bottom = (l.list_top() + l.list_h() - 5.0) as i32;
        assert_eq!(l.hit(200, bottom, l.max_scroll()), Hit::Row(19));
    }

    #[test]
    fn only_the_rows_in_the_viewport_are_visible() {
        let l = layout(100);
        // Top: label + rows 0..=8 fill 417, row 9 starts at 417 < 438.
        assert_eq!(l.visible_rows(0.0), 0..10);
        // Scrolled to row 50's top: it is first; the viewport reaches row 59.
        assert_eq!(l.visible_rows(l.row_top(50)), 50..60);
        // A partly hidden first row still counts.
        assert_eq!(l.visible_rows(l.row_top(50) + 10.0).start, 50);
        // Past the end clamps; short and empty lists.
        assert_eq!(l.visible_rows(1e9).end, 100);
        assert_eq!(layout(3).visible_rows(0.0), 0..3);
        assert_eq!(layout(0).visible_rows(0.0), 0..0);
    }

    #[test]
    fn the_wheel_moves_a_row_per_notch_and_stops_at_the_ends() {
        let l = layout(20);
        assert_eq!(l.max_scroll(), 21.0 + 20.0 * 44.0 - 438.0);
        assert_eq!(l.wheel(0.0, 120), 0.0);
        assert_eq!(l.wheel(0.0, -120), 44.0);
        assert_eq!(l.wheel(44.0, -60), 66.0);
        assert_eq!(l.wheel(44.0, 120), 0.0);
        assert_eq!(l.wheel(l.max_scroll() - 1.0, -360), l.max_scroll());
        // Nothing to scroll with a short list.
        assert_eq!(layout(3).wheel(0.0, -120), 0.0);
    }

    #[test]
    fn the_selection_is_kept_in_view() {
        let l = layout(20);
        assert_eq!(l.scroll_to_show(0.0, 0), 0.0);
        // Row 8 ends at 21 + 9 * 44 = 417, inside 438: nothing moves.
        assert_eq!(l.scroll_to_show(0.0, 8), 0.0);
        // Row 9 ends at 461: scroll so its bottom meets the list's.
        assert_eq!(l.scroll_to_show(0.0, 9), 461.0 - 438.0);
        // Going back up: row 1 starts at 21 + 44 = 65.
        assert_eq!(l.scroll_to_show(200.0, 1), 65.0);
        // Back to the first row shows the label too.
        assert_eq!(l.scroll_to_show(200.0, 0), 0.0);
        // The last row.
        assert_eq!(l.scroll_to_show(0.0, 19), l.max_scroll());
        // A stale scroll past the end is clamped.
        assert_eq!(l.scroll_to_show(9999.0, 19), l.max_scroll());
        assert_eq!(layout(0).scroll_to_show(10.0, 0), 0.0);
    }
}
