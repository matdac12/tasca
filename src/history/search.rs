//! The search line's rules. Pure: the query text in, which Clips match out.

/// The words of a query, lowercased. Empty when the query is blank.
pub fn words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// Whether `text` holds every word, in any order, ignoring case. No words
/// matches everything.
pub fn matches(words: &[String], text: &str) -> bool {
    if words.is_empty() {
        return true;
    }
    let text = text.to_lowercase();
    words.iter().all(|w| text.contains(w.as_str()))
}

/// Ctrl+Backspace: the query without its last word (and the spaces before it).
pub fn delete_word(query: &mut String) {
    query.truncate(query.trim_end().len());
    let keep = query.char_indices().rev().find(|(_, c)| c.is_whitespace()).map_or(0, |(i, c)| i + c.len_utf8());
    query.truncate(keep);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(query: &str, text: &str) -> bool {
        matches(&words(query), text)
    }

    #[test]
    fn every_word_must_appear_in_any_order() {
        assert!(hit("inv march", "March invoice for Rossi"));
        assert!(hit("rossi march", "March invoice for Rossi"));
        assert!(!hit("inv april", "March invoice for Rossi"));
    }

    #[test]
    fn case_and_extra_spaces_do_not_matter() {
        assert!(hit("  INV   mArCh ", "march INVOICE"));
        assert!(hit("ÀB", "xàbx"));
    }

    #[test]
    fn a_blank_query_matches_everything() {
        assert!(hit("", "anything"));
        assert!(hit("   ", ""));
    }

    #[test]
    fn ctrl_backspace_removes_the_last_word_and_the_spaces_before_it() {
        let cut = |s: &str| {
            let mut q = s.to_string();
            delete_word(&mut q);
            q
        };
        assert_eq!(cut("inv march"), "inv ");
        assert_eq!(cut("inv march  "), "inv ");
        assert_eq!(cut("inv"), "");
        assert_eq!(cut(""), "");
        assert_eq!(cut("è à"), "è ");
    }
}
