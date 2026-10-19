//! Slide-in / fade-out (spec §3 Motion). Pure sampling: the window code drives
//! it from a ~10 ms timer with QPC time and moves the already-rendered bitmap
//! through `UpdateLayeredWindow`, without re-rendering.

pub const TIMER_MS: u32 = 10;
pub const IN_MS: f64 = 180.0;
pub const OUT_MS: f64 = 120.0;
pub const IN_FROM_ALPHA: f32 = 0.4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimKind {
    In,
    Out,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anim {
    pub kind: AnimKind,
    start_ms: f64,
    dur_ms: f64,
    from_x: i32,
    to_x: i32,
    from_a: f32,
    to_a: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub x: i32,
    /// `SourceConstantAlpha`.
    pub alpha: u8,
    pub done: bool,
}

pub fn ease_out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

impl Anim {
    /// From `offset_px` left of `to_x` at 40 % alpha to `to_x` at 100 %.
    pub fn slide_in(now_ms: f64, to_x: i32, offset_px: i32) -> Self {
        Self { kind: AnimKind::In, start_ms: now_ms, dur_ms: IN_MS, from_x: to_x - offset_px, to_x, from_a: IN_FROM_ALPHA, to_a: 1.0 }
    }

    /// From `x` at the current alpha to `offset_px` further left at 0.
    pub fn fade_out(now_ms: f64, x: i32, offset_px: i32, alpha: u8) -> Self {
        Self { kind: AnimKind::Out, start_ms: now_ms, dur_ms: OUT_MS, from_x: x, to_x: x - offset_px, from_a: alpha as f32 / 255.0, to_a: 0.0 }
    }

    pub fn sample(&self, now_ms: f64) -> Frame {
        let t = ((now_ms - self.start_ms) / self.dur_ms).clamp(0.0, 1.0);
        let e = ease_out_cubic(t);
        let x = self.from_x + ((self.to_x - self.from_x) as f64 * e).round() as i32;
        let a = self.from_a + (self.to_a - self.from_a) * e as f32;
        Frame { x, alpha: (a * 255.0).round().clamp(0.0, 255.0) as u8, done: t >= 1.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_ends() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert!(ease_out_cubic(0.5) > 0.5, "ease-out is ahead of linear");
    }

    #[test]
    fn slide_in_frames() {
        let a = Anim::slide_in(1000.0, 0, 24);
        assert_eq!(a.sample(1000.0), Frame { x: -24, alpha: 102, done: false });
        let mid = a.sample(1090.0);
        assert!(mid.x > -24 && mid.x < 0 && mid.alpha > 102 && mid.alpha < 255 && !mid.done);
        assert_eq!(a.sample(1180.0), Frame { x: 0, alpha: 255, done: true });
        assert_eq!(a.sample(5000.0), Frame { x: 0, alpha: 255, done: true });
        // A timer tick that arrives "early" (clock skew) is clamped.
        assert_eq!(a.sample(900.0).x, -24);
    }

    #[test]
    fn fade_out_frames() {
        let a = Anim::fade_out(0.0, 100, 12, 255);
        assert_eq!(a.kind, AnimKind::Out);
        assert_eq!(a.sample(0.0), Frame { x: 100, alpha: 255, done: false });
        assert_eq!(a.sample(OUT_MS), Frame { x: 88, alpha: 0, done: true });
        // Interrupting a slide-in fades from where it was.
        let b = Anim::fade_out(0.0, 90, 12, 180);
        assert_eq!(b.sample(0.0).alpha, 180);
    }
}
