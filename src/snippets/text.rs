//! The words on a Snippet row, and the checks on a body before it is kept.
//! Pure, so the wording is unit-tested.

use super::store::MAX_BODY;

/// Characters kept from a line when it becomes a title.
pub const TITLE_CHARS: usize = 80;
const LINE_CHARS: usize = 300;

/// Why a body cannot be stored. `None` means it can.
pub fn body_error(body: &str) -> Option<&'static str> {
    // Leading spaces stay: a snippet is often indented code. Only a body that
    // is blank after trimming, or longer than the cap once trailing space is
    // gone, is refused.
    if body.trim().is_empty() {
        Some("Text is empty")
    } else if body.trim_end().len() > MAX_BODY {
        Some("Text is too long")
    } else {
        None
    }
}

/// The first `max` characters, on a character boundary.
pub fn limit_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// A title for text that arrived without one: the first non-blank line,
/// collapsed to a single line, or "Untitled" when there is nothing to show.
pub fn title_from_body(body: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let title = limit_chars(line, TITLE_CHARS);
    if title.is_empty() {
        "Untitled".to_string()
    } else {
        title
    }
}

/// Whitespace runs become single spaces. Scanning stops once the line is full,
/// so a huge body costs the same as a short one.
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

/// The grey line under the title. Empty when it would only repeat the title.
pub fn subtitle(title: &str, body: &str) -> String {
    let line = one_line(body);
    if line == one_line(title) {
        String::new()
    } else {
        line
    }
}

/// The footer's right side.
pub fn count_label(n: usize) -> String {
    if n == 1 {
        "1 Snippet".to_string()
    } else {
        format!("{n} Snippets")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_come_from_the_first_line_and_stop_at_eighty_characters() {
        assert_eq!(title_from_body("  \n  Hello there  \nmore"), "Hello there");
        assert_eq!(title_from_body("   "), "Untitled");
        assert_eq!(
            title_from_body(&"é".repeat(100)).chars().count(),
            TITLE_CHARS
        );
    }

    #[test]
    fn a_subtitle_hides_when_it_repeats_the_title() {
        assert_eq!(subtitle("Hello", "Hello"), "");
        assert_eq!(subtitle("Hello", "Hello\nworld"), "Hello world");
        assert_eq!(one_line("a\r\n  b\t c"), "a b c");
    }

    #[test]
    fn empty_and_over_long_bodies_are_refused() {
        assert_eq!(body_error("  \n"), Some("Text is empty"));
        assert_eq!(body_error("ok"), None);
        assert_eq!(
            body_error(&"x".repeat(MAX_BODY + 1)),
            Some("Text is too long")
        );
        assert_eq!(body_error(&"x".repeat(MAX_BODY)), None);
    }

    #[test]
    fn the_footer_counts_snippets() {
        assert_eq!(count_label(0), "0 Snippets");
        assert_eq!(count_label(1), "1 Snippet");
        assert_eq!(count_label(2), "2 Snippets");
    }
}
