//! The words on a History row: the one-line title, the type line ("Text · 2
//! min") and the footer count. Pure, so the wording is unit-tested.

use super::clip::{Clip, Content};
use super::formats;

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

/// What a Clip is, as the type line says it: Text / Formatted text / Link /
/// Image · W × H / N files.
pub fn clip_kind(clip: &Clip) -> String {
    match &clip.content {
        Content::Text { html, rtf } => match kind_label(&clip.text) {
            "Link" => "Link".to_string(),
            _ if html.is_some() || rtf.is_some() => "Formatted text".to_string(),
            label => label.to_string(),
        },
        Content::Image { width, height, .. } => formats::image_label(*width, *height),
        Content::Files { paths } => formats::files_label(paths.len()),
    }
}

/// A row's one-line title. An image has no text: it is just "Image" (its size
/// is on the type line).
pub fn clip_title(clip: &Clip) -> String {
    match &clip.content {
        Content::Text { .. } => one_line(&clip.text),
        Content::Image { .. } => "Image".to_string(),
        Content::Files { paths } => formats::files_title(paths),
    }
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
pub fn type_line(clip: &Clip, now_ms: u64, copied_ms: u64) -> String {
    format!("{} · {}", clip_kind(clip), age(now_ms, copied_ms))
}

/// The grey line under a pinned Clip's title: its type and no age (a pin is
/// kept, not recent).
pub fn pinned_line(clip: &Clip) -> String {
    clip_kind(clip)
}

/// Plain text a Clip can be saved as a Snippet. An image has none. Trailing
/// space is dropped; leading space stays, so indented text is kept. The Clip
/// itself is left as it was.
pub fn snippet_text(clip: &Clip) -> Option<String> {
    match &clip.content {
        Content::Text { .. } => {
            if clip.text.trim().is_empty() { None } else { Some(clip.text.trim_end().to_string()) }
        }
        Content::Files { paths } if !paths.is_empty() => Some(paths.join("\n")),
        _ => None,
    }
}

/// The footer's right side.
pub fn count_label(clips: usize) -> String {
    if clips == 1 { "1 Clip".to_string() } else { format!("{clips} Clips") }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::clip::ClipId;
    use super::*;

    fn text_clip(text: &str) -> Clip {
        Clip::plain(ClipId(1), text.into(), 0, false)
    }

    fn with(content: Content, text: &str) -> Clip {
        Clip { content, ..text_clip(text) }
    }

    #[test]
    fn formatted_text_images_and_files_read_as_their_kind() {
        let html = Content::Text { html: Some("h".into()), rtf: None };
        assert_eq!(clip_kind(&with(html.clone(), "hello")), "Formatted text");
        // a lone URL stays a Link even when the page sent HTML with it
        assert_eq!(clip_kind(&with(html, "https://example.com")), "Link");
        let img = Content::Image { width: 1920, height: 1080, png: Arc::new(vec![]), hash: 0 };
        let img = with(img, "");
        assert_eq!(clip_kind(&img), "Image · 1920 × 1080");
        assert_eq!(clip_title(&img), "Image");
        assert_eq!(type_line(&img, 5 * DAY_MS, 5 * DAY_MS - 12 * MIN_MS), "Image · 1920 × 1080 · 12 min");
        assert_eq!(pinned_line(&img), "Image · 1920 × 1080");
        let paths = |p: &[&str]| Content::Files { paths: p.iter().map(|s| s.to_string()).collect() };
        let one = with(paths(&["C:\\a\\Report.pdf"]), "");
        assert_eq!((clip_title(&one).as_str(), clip_kind(&one).as_str()), ("Report.pdf", "1 file"));
        let three = with(paths(&["C:\\a\\Report.pdf", "C:\\b.txt", "C:\\c"]), "");
        assert_eq!((clip_title(&three).as_str(), clip_kind(&three).as_str()), ("Report.pdf +2", "3 files"));
        assert_eq!(type_line(&three, 5 * DAY_MS, 5 * DAY_MS - 25 * MIN_MS), "3 files · 25 min");
    }

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
    fn snippet_text_is_trimmed_text_or_paths_and_images_have_none() {
        assert_eq!(snippet_text(&text_clip("  hello \n")).as_deref(), Some("  hello"));
        assert_eq!(snippet_text(&text_clip("  \n")), None);
        let paths = Content::Files { paths: vec!["C:\\a.txt".into(), "D:\\b".into()] };
        assert_eq!(snippet_text(&with(paths, "")).as_deref(), Some("C:\\a.txt\nD:\\b"));
        let img = Content::Image { width: 2, height: 2, png: Arc::new(vec![]), hash: 1 };
        assert_eq!(snippet_text(&with(img, "")), None);
    }

    #[test]
    fn the_type_line_joins_kind_and_age() {
        let now = 5 * DAY_MS;
        assert_eq!(type_line(&text_clip("hello"), now, now - 2 * MIN_MS), "Text · 2 min");
        assert_eq!(type_line(&text_clip("https://example.com"), now, now - 6 * MIN_MS), "Link · 6 min");
        assert_eq!(type_line(&text_clip("x"), now, now - 3 * HOUR_MS), "Text · 3 h");
    }

    #[test]
    fn a_pinned_line_has_the_type_and_no_age() {
        assert_eq!(pinned_line(&text_clip("hello")), "Text");
        assert_eq!(pinned_line(&text_clip("https://example.com")), "Link");
    }

    #[test]
    fn the_footer_count_is_singular_for_one() {
        assert_eq!(count_label(0), "0 Clips");
        assert_eq!(count_label(1), "1 Clip");
        assert_eq!(count_label(14), "14 Clips");
    }
}
