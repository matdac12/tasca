//! A Clip: one entry in the History (see `CONTEXT.md`). Plain data, shared by
//! the state machine, the drawing and the store.

use super::store::{Body, Record};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClipId(pub u64);

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Clip {
    pub id: ClipId,
    /// The copied text. Two Clips with the same text are the same Clip.
    pub text: String,
    /// When it was last copied (or pasted back), in ms since the Unix epoch.
    /// A plain number so the store can keep it as it is.
    pub last_copied_ms: u64,
    /// Kept at the top and never pushed out by the 100-Clip cap.
    pub pinned: bool,
}

impl Clip {
    pub fn to_record(&self) -> Record {
        Record {
            id: self.id.0,
            last_copied_ms: self.last_copied_ms,
            pinned: self.pinned,
            body: Body::Text { text: self.text.clone(), html: None, rtf: None },
        }
    }

    /// `None` for a stored kind the History cannot show yet.
    pub fn from_record(r: Record) -> Option<Clip> {
        match r.body {
            Body::Text { text, .. } => {
                Some(Clip { id: ClipId(r.id), text, last_copied_ms: r.last_copied_ms, pinned: r.pinned })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clip_survives_the_trip_to_a_record() {
        let c = Clip { id: ClipId(7), text: "hi".into(), last_copied_ms: 42, pinned: true };
        assert_eq!(Clip::from_record(c.to_record()), Some(c));
    }

    #[test]
    fn kinds_not_shown_yet_are_left_out() {
        let r = Record { id: 1, last_copied_ms: 1, pinned: false, body: Body::Files { paths: vec![] } };
        assert_eq!(Clip::from_record(r), None);
    }
}
