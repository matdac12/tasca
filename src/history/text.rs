//! The words on a History row: the one-line title, the type line ("Text · 2
//! min") and the footer count. Pure, so the wording is unit-tested.

const MIN_MS: u64 = 60_000;
const HOUR_MS: u64 = 60 * MIN_MS;
const DAY_MS: u64 = 24 * HOUR_MS;

/// A Clip's text as one display line: whitespace runs become single spaces,
/// and it stops after `LINE_CHARS` characters (the row ellipsizes anyway).
/// Scans no further than the characters it keeps, so a huge Clip costs the
/// same as a short one.
pub fn one_line(text: &str) -> String {
    let mut line = String::new();
    let mut kept = 0;
    let mut gap = false;
    for c in text.chars() {
        if c.is_whitespace() {
            gap = !line.is_empty();
            continue;
        }
        if gap {
            line.push(' ');
            kept += 1;
            gap = false;
        }
        if kept >= LINE_CHARS {
            break;
        }
        line.push(c);
        kept += 1;
    }
    line.truncate(line.trim_end().len());
    line
}

const LINE_CHARS: usize = 300;
/// Longer than this (in bytes) is never "a lone URL"; keeps `is_link` from
/// scanning a huge Clip.
const LINK_MAX: usize = 2048;

/// Whether the whole text is one web address.
pub fn is_link(text: &str) -> bool {
    let t = text.trim();
    if t.len() > LINK_MAX {
        return false;
    }
    let rest = ["https://", "http://", "www."].iter().find_map(|p| t.strip_prefix(p));
    rest.is_some_and(|r| !r.is_empty() && !t.chars().any(char::is_whitespace))
}

/// "Link" for a lone URL, else "Text".
pub fn kind_label(text: &str) -> &'static str {
    if is_link(text) { "Link" } else { "Text" }
}

/// How long ago, as the row shows it: now / N min / N h / yesterday / N d.
/// A time in the future (clock moved back) reads as "now".
pub fn age(now_ms: u64, copied_ms: u64) -> String {
    let ms = now_ms.saturating_sub(copied_ms);
    match ms {
        m if m < MIN_MS => "now".to_string(),
        m if m < HOUR_MS => format!("{} min", m / MIN_MS),
        m if m < DAY_MS => format!("{} h", m / HOUR_MS),
        m if m < 2 * DAY_MS => "yesterday".to_string(),
        m => format!("{} d", m / DAY_MS),
    }
}

/// The grey line under a row's title: "Text · 2 min".
pub fn type_line(text: &str, now_ms: u64, copied_ms: u64) -> String {
    format!("{} · {}", kind_label(text), age(now_ms, copied_ms))
}

/// The footer's right side.
pub fn count_label(clips: usize) -> String {
    if clips == 1 { "1 Clip".to_string() } else { format!("{clips} Clips") }
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
        let text = format!("\n\n    {}\n{}", "word ".repeat(200), "x".repeat(10_000));
        let line = one_line(&text);
        assert!(line.starts_with("word word"));
        assert!(line.chars().count() <= LINE_CHARS);
    }

    #[test]
    fn a_huge_clip_without_spaces_is_cut_without_being_read_in_full() {
        let huge = "x".repeat(10 * 1024 * 1024);
        let line = one_line(&huge);
        assert_eq!(line.chars().count(), LINE_CHARS);
        assert_eq!(kind_label(&huge), "Text");
        let spaced = format!("   a   b{}", " c".repeat(LINE_CHARS));
        let line = one_line(&spaced);
        assert!(line.starts_with("a b c c"));
        assert!(line.chars().count() <= LINE_CHARS);
        assert!(!line.ends_with(' '));
        assert_eq!(one_line("  \n "), "");
    }

    #[test]
    fn a_lone_url_is_a_link_anything_else_is_text() {
        assert_eq!(kind_label("https://github.com/matdac12/tasca/releases/tag/v0.1.0"), "Link");
        assert_eq!(kind_label("  http://example.com \r\n"), "Link");
        assert_eq!(kind_label("www.example.com"), "Link");
        assert_eq!(kind_label("see https://example.com"), "Text");
        assert_eq!(kind_label("https://a.com https://b.com"), "Text");
        assert_eq!(kind_label("https://"), "Text");
        assert_eq!(kind_label("Ciao Marco"), "Text");
    }

    #[test]
    fn ages_read_now_minutes_hours_yesterday_days() {
        let at = |ms: u64| age(10 * DAY_MS, 10 * DAY_MS - ms);
        assert_eq!(at(0), "now");
        assert_eq!(at(59_999), "now");
        assert_eq!(at(MIN_MS), "1 min");
        assert_eq!(at(2 * MIN_MS + 5_000), "2 min");
        assert_eq!(at(HOUR_MS - 1), "59 min");
        assert_eq!(at(HOUR_MS), "1 h");
        assert_eq!(at(23 * HOUR_MS + 59 * MIN_MS), "23 h");
        assert_eq!(at(DAY_MS), "yesterday");
        assert_eq!(at(2 * DAY_MS - 1), "yesterday");
        assert_eq!(at(2 * DAY_MS), "2 d");
        assert_eq!(at(9 * DAY_MS + HOUR_MS), "9 d");
    }

    #[test]
    fn a_time_in_the_future_is_now() {
        assert_eq!(age(1_000, 500_000), "now");
    }

    #[test]
    fn the_type_line_joins_kind_and_age() {
        let now = 5 * DAY_MS;
        assert_eq!(type_line("hello", now, now - 2 * MIN_MS), "Text · 2 min");
        assert_eq!(type_line("https://example.com", now, now - 6 * MIN_MS), "Link · 6 min");
        assert_eq!(type_line("x", now, now - 3 * HOUR_MS), "Text · 3 h");
    }

    #[test]
    fn the_footer_count_is_singular_for_one() {
        assert_eq!(count_label(0), "0 Clips");
        assert_eq!(count_label(1), "1 Clip");
        assert_eq!(count_label(14), "14 Clips");
    }
}
