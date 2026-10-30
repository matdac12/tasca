//! A Snippet: text kept on purpose, apart from a Clip in the History.

use super::store::Record;
use super::text;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct SnippetId(pub u64);

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Snippet {
    pub id: SnippetId,
    pub title: String,
    /// The text that is copied back to the clipboard. No trailing blank lines.
    pub body: String,
    /// When it was saved or last copied, in ms since the Unix epoch.
    pub updated_ms: u64,
}

impl Snippet {
    pub fn to_record(&self) -> Record {
        Record {
            id: self.id.0,
            updated_ms: self.updated_ms,
            title: self.title.clone(),
            body: self.body.clone(),
        }
    }

    /// `None` when the body is blank or over the store's limit.
    pub fn from_record(r: Record) -> Option<Snippet> {
        let body = r.body.trim_end().to_string();
        if text::body_error(&body).is_some() {
            return None;
        }
        let title = {
            let t = r.title.trim();
            if t.is_empty() {
                text::title_from_body(&body)
            } else {
                text::limit_chars(t, text::TITLE_CHARS)
            }
        };
        Some(Snippet {
            id: SnippetId(r.id),
            title,
            body,
            updated_ms: r.updated_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snippet_survives_the_trip_to_a_record() {
        let s = Snippet {
            id: SnippetId(7),
            title: "Hello".into(),
            body: "Hello\nthere".into(),
            updated_ms: 42,
        };
        assert_eq!(Snippet::from_record(s.to_record()), Some(s));
    }

    #[test]
    fn a_blank_title_is_taken_from_the_body_and_a_blank_body_is_dropped() {
        let r = Record {
            id: 1,
            updated_ms: 1,
            title: "  ".into(),
            body: "  first line \nrest".into(),
        };
        let s = Snippet::from_record(r).unwrap();
        assert_eq!(s.title, "first line");
        assert_eq!(s.body, "  first line \nrest");
        assert_eq!(
            Snippet::from_record(Record {
                id: 1,
                updated_ms: 1,
                title: "t".into(),
                body: " \n".into()
            }),
            None
        );
    }
}
