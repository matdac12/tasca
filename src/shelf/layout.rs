//! Pure layout and hit-test maths for the Shelf (spec §3, mockup C). No Win32.
//!
//! Everything is laid out in DIP inside the window bitmap; the window's top-left
//! is at (0, 0). `scale` = monitor DPI / 96 converts to physical pixels.

pub const PANEL_W: f32 = 200.0;
pub const PAD: f32 = 8.0;
pub const RADIUS: f32 = 12.0;
pub const HEAD_H: f32 = 22.0;
/// The empty Shelf is a 64 DIP panel (TASCA-8/11): padding + header strip.
pub const EMPTY_PANEL_H: f32 = 64.0;
pub const ROW_H: f32 = 40.0;
/// Shadow margins around the panel. Left is the 8 DIP gap to the screen edge.
pub const MARGIN_L: f32 = 8.0;
pub const MARGIN_T: f32 = 10.0;
pub const MARGIN_R: f32 = 16.0;
pub const MARGIN_B: f32 = 24.0;
/// The panel is at most this share of the work area's height; then it scrolls.
pub const MAX_HEIGHT_FRAC: f32 = 0.6;
/// Width of the "Clear all" hit zone, measured from the panel's right edge.
pub const CLEAR_HIT_W: f32 = 60.0;
pub const THUMB_W: f32 = 3.0;
pub const THUMB_MIN_H: f32 = 16.0;
/// Slide-in starts this far left of the final position; fade-out ends this far.
pub const SLIDE_IN_DIP: f32 = 16.0;
pub const SLIDE_OUT_DIP: f32 = 8.0;
const WHEEL_DELTA: f32 = 120.0;

/// A monitor work area (`rcWork`) in physical screen pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkArea {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

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
    Header,
    ClearAll,
    /// Inside the panel but on no row (padding, below the last row).
    Panel,
    /// Row index, 0 = top (newest).
    Row(usize),
}

/// The Shelf's geometry for one monitor and one Item count.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub scale: f32,
    pub work: WorkArea,
    pub rows: usize,
}

impl Layout {
    pub fn new(dpi: u32, work: WorkArea, rows: usize) -> Self {
        Self { scale: dpi as f32 / 96.0, work, rows }
    }

    pub fn head_h(&self) -> f32 {
        if self.rows == 0 { EMPTY_PANEL_H - 2.0 * PAD } else { HEAD_H }
    }

    pub fn rows_view_h(&self) -> f32 {
        let work_h = (self.work.bottom - self.work.top) as f32 / self.scale;
        let room = work_h * MAX_HEIGHT_FRAC - 2.0 * PAD - HEAD_H;
        let max_rows = (room / ROW_H).floor().max(1.0);
        (self.rows as f32 * ROW_H).min(max_rows * ROW_H)
    }

    pub fn panel_h(&self) -> f32 {
        PAD + self.head_h() + self.rows_view_h() + PAD
    }

    pub fn panel(&self) -> RectF {
        RectF { left: MARGIN_L, top: MARGIN_T, right: MARGIN_L + PANEL_W, bottom: MARGIN_T + self.panel_h() }
    }

    pub fn rows_top(&self) -> f32 {
        MARGIN_T + PAD + self.head_h()
    }

    pub fn max_scroll(&self) -> f32 {
        (self.rows as f32 * ROW_H - self.rows_view_h()).max(0.0)
    }

    pub fn clamp_scroll(&self, scroll: f32) -> f32 {
        scroll.clamp(0.0, self.max_scroll())
    }

    /// Scroll after a `WM_MOUSEWHEEL` delta (positive = away from the user =
    /// up). Fractional deltas from precision touchpads accumulate in `scroll`.
    pub fn wheel(&self, scroll: f32, delta: i32) -> f32 {
        self.clamp_scroll(scroll - delta as f32 / WHEEL_DELTA * ROW_H)
    }

    /// Window size in physical pixels, shadow margins included.
    pub fn size_px(&self) -> (i32, i32) {
        let w = (MARGIN_L + PANEL_W + MARGIN_R) * self.scale;
        let h = (MARGIN_T + self.panel_h() + MARGIN_B) * self.scale;
        (w.ceil() as i32, h.ceil() as i32)
    }

    /// Final window top-left in screen pixels: on `rcWork.left`, with the panel
    /// vertically centred in the work area.
    pub fn position(&self) -> (i32, i32) {
        let mid = (self.work.top + self.work.bottom) as f32 / 2.0;
        let y = mid - (MARGIN_T + self.panel_h() / 2.0) * self.scale;
        (self.work.left, y.round() as i32)
    }

    pub fn dip_to_px(&self, dip: f32) -> i32 {
        (dip * self.scale).round() as i32
    }

    /// What is under a client-area point (physical pixels).
    pub fn hit(&self, x_px: i32, y_px: i32, scroll: f32) -> Hit {
        let (x, y) = (x_px as f32 / self.scale, y_px as f32 / self.scale);
        let p = self.panel();
        if x < p.left || x >= p.right || y < p.top || y >= p.bottom {
            return Hit::Margin;
        }
        if self.rows == 0 {
            return Hit::Header;
        }
        let rows_top = self.rows_top();
        if y < rows_top {
            return if x >= p.right - CLEAR_HIT_W { Hit::ClearAll } else { Hit::Header };
        }
        if y >= rows_top + self.rows_view_h() {
            return Hit::Panel;
        }
        let i = ((y - rows_top + self.clamp_scroll(scroll)) / ROW_H).floor() as usize;
        if i < self.rows { Hit::Row(i) } else { Hit::Panel }
    }

    /// Scroll thumb (top, height) in DIP, when the rows overflow.
    pub fn thumb(&self, scroll: f32) -> Option<(f32, f32)> {
        let max = self.max_scroll();
        if max <= 0.0 {
            return None;
        }
        let view = self.rows_view_h();
        let h = (view * view / (self.rows as f32 * ROW_H)).max(THUMB_MIN_H);
        Some((self.rows_top() + (view - h) * (self.clamp_scroll(scroll) / max), h))
    }
}

/// Has a press at `p0` moved far enough to start a drag (`SM_CXDRAG`/`SM_CYDRAG`)?
pub fn past_drag_threshold(p0: (i32, i32), p: (i32, i32), cx: i32, cy: i32) -> bool {
    (p.0 - p0.0).abs() > cx || (p.1 - p0.1).abs() > cy
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 3840x2160 at 150 %, taskbar 48 px at the bottom.
    const UHD: WorkArea = WorkArea { left: 0, top: 0, right: 3840, bottom: 2112 };
    /// 1920x1200 at 125 %, below a 1440 px monitor, taskbar 48 px.
    const WUXGA: WorkArea = WorkArea { left: 0, top: 1440, right: 1920, bottom: 2592 };

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn empty_panel_is_exactly_64_dip() {
        for (dpi, work) in [(96, UHD), (144, UHD), (120, WUXGA), (192, UHD)] {
            assert_eq!(Layout::new(dpi, work, 0).panel_h(), 64.0);
        }
    }

    #[test]
    fn panel_height_grows_one_row_per_item() {
        let l = |n| Layout::new(144, UHD, n).panel_h();
        assert_eq!(l(1), PAD + HEAD_H + ROW_H + PAD); // 78
        assert_eq!(l(1), 78.0);
        assert_eq!(l(3), 158.0);
        assert_eq!(l(10), 438.0);
    }

    #[test]
    fn panel_is_clamped_to_60_percent_of_the_work_area_in_whole_rows() {
        // 1152 px / 1.25 = 921.6 DIP; 60 % = 552.96 DIP; 12 rows fit.
        let max = Layout::new(120, WUXGA, 12).panel_h();
        assert_eq!(max, PAD + HEAD_H + 12.0 * ROW_H + PAD);
        for n in [13, 50, 500] {
            let l = Layout::new(120, WUXGA, n);
            assert_eq!(l.panel_h(), max);
            assert!(l.panel_h() <= 0.6 * 1152.0 / 1.25);
            assert_eq!(l.rows_view_h(), 12.0 * ROW_H);
        }
        // 2112 px / 1.5 = 1408 DIP; 60 % = 844.8 DIP; 20 rows fit.
        assert_eq!(Layout::new(144, UHD, 100).rows_view_h(), 20.0 * ROW_H);
    }

    #[test]
    fn a_tiny_work_area_still_shows_one_row() {
        let work = WorkArea { left: 0, top: 0, right: 800, bottom: 100 };
        assert_eq!(Layout::new(96, work, 5).rows_view_h(), ROW_H);
    }

    #[test]
    fn size_includes_the_shadow_margins() {
        // (8 + 200 + 16) x (10 + 158 + 24) DIP at 150 %.
        assert_eq!(Layout::new(144, UHD, 3).size_px(), (336, 288));
        // (224) x (10 + 78 + 24) DIP at 125 %.
        assert_eq!(Layout::new(120, WUXGA, 1).size_px(), (280, 140));
        assert_eq!(Layout::new(96, UHD, 0).size_px(), (224, 98));
    }

    #[test]
    fn position_on_a_3840x2160_work_area_at_150_percent() {
        let l = Layout::new(144, UHD, 3);
        // Left edge on rcWork.left; panel centred on y = 1056.
        // y = 1056 - (10 + 158 / 2) * 1.5 = 922.5, rounded away from zero.
        assert_eq!(l.position(), (0, 923));
        let (_, y) = l.position();
        let panel_mid = y as f32 + (MARGIN_T + l.panel_h() / 2.0) * l.scale;
        assert!((panel_mid - 1056.0).abs() <= 1.0);
    }

    #[test]
    fn position_on_a_1920x1200_work_area_at_125_percent() {
        let l = Layout::new(120, WUXGA, 1);
        // mid = 2016; y = 2016 - (10 + 39) * 1.25 = 1954.75.
        assert_eq!(l.position(), (0, 1955));
        let left = WorkArea { left: -1920, ..WUXGA };
        assert_eq!(Layout::new(120, left, 1).position().0, -1920);
    }

    #[test]
    fn position_recentres_when_the_height_changes() {
        let (_, y1) = Layout::new(144, UHD, 1).position();
        let (_, y5) = Layout::new(144, UHD, 5).position();
        // Four more rows: 160 DIP taller, so the top moves up 80 DIP = 120 px.
        assert_eq!(y1 - y5, 120);
    }

    #[test]
    fn panel_sits_inside_the_margins() {
        let l = Layout::new(144, UHD, 2);
        let p = l.panel();
        assert_eq!(p, RectF { left: 8.0, top: 10.0, right: 208.0, bottom: 10.0 + 118.0 });
        assert_eq!(l.rows_top(), 10.0 + PAD + HEAD_H);
        assert_eq!(Layout::new(144, UHD, 0).rows_top(), 10.0 + 64.0 - PAD);
    }

    // Hit testing at 150 %: DIP * 1.5 = px.
    fn px(dip: f32) -> i32 {
        (dip * 1.5) as i32
    }

    #[test]
    fn hit_shadow_margin() {
        let l = Layout::new(144, UHD, 3);
        assert_eq!(l.hit(px(4.0), px(50.0), 0.0), Hit::Margin); // left gap
        assert_eq!(l.hit(px(100.0), px(5.0), 0.0), Hit::Margin); // above
        assert_eq!(l.hit(px(215.0), px(50.0), 0.0), Hit::Margin); // right shadow
        assert_eq!(l.hit(px(100.0), px(10.0 + 158.0 + 5.0), 0.0), Hit::Margin); // below
        assert_eq!(l.hit(-3, -3, 0.0), Hit::Margin);
    }

    #[test]
    fn hit_header_and_clear_all() {
        let l = Layout::new(144, UHD, 3);
        assert_eq!(l.hit(px(40.0), px(25.0), 0.0), Hit::Header);
        assert_eq!(l.hit(px(190.0), px(25.0), 0.0), Hit::ClearAll);
        assert_eq!(l.hit(px(150.0), px(12.0), 0.0), Hit::ClearAll);
        assert_eq!(l.hit(px(146.0), px(25.0), 0.0), Hit::Header);
        // The empty Shelf has no "Clear all": all of it is header.
        let e = Layout::new(144, UHD, 0);
        assert_eq!(e.hit(px(190.0), px(25.0), 0.0), Hit::Header);
        assert_eq!(e.hit(px(100.0), px(60.0), 0.0), Hit::Header);
    }

    #[test]
    fn hit_rows() {
        let l = Layout::new(144, UHD, 3);
        // rows_top = 40 DIP.
        assert_eq!(l.hit(px(100.0), px(41.0), 0.0), Hit::Row(0));
        assert_eq!(l.hit(px(100.0), px(79.0), 0.0), Hit::Row(0));
        assert_eq!(l.hit(px(100.0), px(81.0), 0.0), Hit::Row(1));
        assert_eq!(l.hit(px(10.0), px(150.0), 0.0), Hit::Row(2));
        // Bottom padding.
        assert_eq!(l.hit(px(100.0), px(163.0), 0.0), Hit::Panel);
    }

    #[test]
    fn hit_rows_with_scroll() {
        let l = Layout::new(144, UHD, 30); // 20 rows visible
        assert_eq!(l.hit(px(100.0), px(45.0), 100.0), Hit::Row(2));
        let max = l.max_scroll();
        assert_eq!(max, 10.0 * ROW_H);
        // The last visible slot shows the last Item when fully scrolled.
        let last_y = l.rows_top() + l.rows_view_h() - 1.0;
        assert_eq!(l.hit(px(100.0), px(last_y), max), Hit::Row(29));
        assert_eq!(l.hit(px(100.0), px(last_y), 0.0), Hit::Row(19));
    }

    #[test]
    fn hit_works_at_125_percent() {
        let l = Layout::new(120, WUXGA, 2);
        let px = |d: f32| (d * 1.25) as i32;
        assert_eq!(l.hit(px(100.0), px(85.0), 0.0), Hit::Row(1));
        assert_eq!(l.hit(px(4.0), px(85.0), 0.0), Hit::Margin);
    }

    #[test]
    fn scroll_is_clamped() {
        let l = Layout::new(144, UHD, 30);
        assert_eq!(l.clamp_scroll(-5.0), 0.0);
        assert_eq!(l.clamp_scroll(1e6), 400.0);
        assert_eq!(Layout::new(144, UHD, 3).max_scroll(), 0.0);
        assert_eq!(Layout::new(144, UHD, 3).clamp_scroll(50.0), 0.0);
    }

    #[test]
    fn wheel_scrolls_a_row_per_notch_and_accumulates_partial_deltas() {
        let l = Layout::new(144, UHD, 30);
        assert_eq!(l.wheel(0.0, -120), ROW_H); // towards the user = down
        assert_eq!(l.wheel(ROW_H, 120), 0.0);
        assert_eq!(l.wheel(0.0, 120), 0.0); // clamped at the top
        let half = l.wheel(0.0, -60);
        assert!(close(l.wheel(half, -60), ROW_H));
        assert_eq!(l.wheel(390.0, -240), 400.0); // clamped at the bottom
    }

    #[test]
    fn thumb_only_when_overflowing() {
        assert_eq!(Layout::new(144, UHD, 3).thumb(0.0), None);
        let l = Layout::new(144, UHD, 40); // 20 of 40 rows visible
        let (top, h) = l.thumb(0.0).unwrap();
        assert_eq!(top, l.rows_top());
        assert!(close(h, l.rows_view_h() / 2.0));
        let (top, h2) = l.thumb(l.max_scroll()).unwrap();
        assert!(close(top + h2, l.rows_top() + l.rows_view_h()));
        // Never thinner than the minimum.
        let (_, h) = Layout::new(144, UHD, 5000).thumb(0.0).unwrap();
        assert_eq!(h, THUMB_MIN_H);
    }

    #[test]
    fn dip_to_px_rounds() {
        assert_eq!(Layout::new(144, UHD, 0).dip_to_px(SLIDE_IN_DIP), 24);
        assert_eq!(Layout::new(120, UHD, 0).dip_to_px(SLIDE_OUT_DIP), 10);
    }

    #[test]
    fn drag_threshold() {
        assert!(!past_drag_threshold((10, 10), (14, 14), 4, 4));
        assert!(past_drag_threshold((10, 10), (15, 10), 4, 4));
        assert!(past_drag_threshold((10, 10), (10, 5), 4, 4));
    }
}
