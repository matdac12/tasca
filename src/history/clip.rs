//! A Clip: one entry in the History (see `CONTEXT.md`). Plain data, shared by
//! the state machine, the drawing and the store.

use std::borrow::Cow;
use std::sync::Arc;

use super::formats::{self, DupKey};
use super::image;
use super::store::{Body, Record};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct ClipId(pub u64);

/// What a Clip holds besides its text.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Content {
    /// Plain text (in `Clip::text`) with the formats it came with, if any:
    /// the `HTML Format` block and the RTF text.
    Text { html: Option<String>, rtf: Option<String> },
    /// A picture, stored as PNG. `hash` is the pixel hash (the duplicate key).
    Image { width: u32, height: u32, png: Arc<Vec<u8>>, hash: u64 },
    /// Copied files, as full paths in copy order.
    Files { paths: Vec<String> },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Clip {
    pub id: ClipId,
    /// The copied text; empty for images and files.
    pub text: String,
    pub content: Content,
    /// When it was last copied (or pasted back), in ms since the Unix epoch.
    /// A plain number so the store can keep it as it is.
    pub last_copied_ms: u64,
    /// Kept at the top and never pushed out by the 100-Clip cap.
    pub pinned: bool,
}

impl Clip {
    /// A plain text Clip.
    #[cfg(test)]
    pub fn plain(id: ClipId, text: String, last_copied_ms: u64, pinned: bool) -> Clip {
        Clip { id, text, content: Content::Text { html: None, rtf: None }, last_copied_ms, pinned }
    }

    /// What two copies must share to be the same Clip. Formatting never counts.
    pub fn dup_key(&self) -> DupKey {
        match &self.content {
            Content::Text { .. } => formats::text_key(&self.text),
            Content::Image { width, height, hash, .. } => formats::pixels_key(*width, *height, *hash),
            Content::Files { paths } => formats::files_key(paths),
        }
    }

    /// What the search looks in: the text, or a files Clip's file names. An
    /// image has none, so it only shows while nothing is searched.
    pub fn search_text(&self) -> Cow<'_, str> {
        match &self.content {
            Content::Files { paths } => Cow::Owned(formats::files_search_text(paths)),
            _ => Cow::Borrowed(&self.text),
        }
    }

    pub fn to_record(&self) -> Record {
        let body = match &self.content {
            Content::Text { html, rtf } => Body::Text { text: self.text.clone(), html: html.clone(), rtf: rtf.clone() },
            Content::Image { width, height, png, .. } => Body::Image { width: *width, height: *height, png: png.to_vec() },
            Content::Files { paths } => Body::Files { paths: paths.clone() },
        };
        Record { id: self.id.0, last_copied_ms: self.last_copied_ms, pinned: self.pinned, body }
    }

    /// `None` for an image whose PNG cannot be decoded. Decodes an image to
    /// hash its pixels, so call it on the store's worker, not the UI thread.
    pub fn from_record(r: Record) -> Option<Clip> {
        let (id, last_copied_ms, pinned) = (ClipId(r.id), r.last_copied_ms, r.pinned);
        let (text, content) = match r.body {
            Body::Text { text, html, rtf } => (text, Content::Text { html, rtf }),
            Body::Image { width, height, png } => {
                let bmp = image::decode_png(&png).ok()?;
                let hash = image::pixel_hash(&bmp);
                (String::new(), Content::Image { width, height, png: Arc::new(png), hash })
            }
            Body::Files { paths } => (String::new(), Content::Files { paths }),
        };
        Some(Clip { id, text, content, last_copied_ms, pinned })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        image::encode_png(&image::Bitmap { width: 2, height: 1, bgra: vec![1, 2, 3, 255, 4, 5, 6, 128] }).unwrap()
    }

    fn trip(c: &Clip) -> Option<Clip> {
        Clip::from_record(c.to_record())
    }

    #[test]
    fn a_plain_clip_survives_the_trip_to_a_record() {
        let c = Clip::plain(ClipId(7), "hi".into(), 42, true);
        assert_eq!(trip(&c), Some(c));
    }

    #[test]
    fn formatted_text_keeps_its_formats() {
        let c = Clip {
            content: Content::Text { html: Some("Version:0.9".into()), rtf: Some("{\rtf1 hi}".into()) },
            ..Clip::plain(ClipId(1), "hi".into(), 5, false)
        };
        assert_eq!(trip(&c), Some(c));
    }

    #[test]
    fn an_image_survives_and_gets_its_pixel_hash_back() {
        let png = png();
        let hash = image::pixel_hash(&image::decode_png(&png).unwrap());
        let c = Clip {
            id: ClipId(2),
            text: String::new(),
            content: Content::Image { width: 2, height: 1, png: Arc::new(png), hash },
            last_copied_ms: 9,
            pinned: true,
        };
        assert_eq!(trip(&c), Some(c));
    }

    #[test]
    fn files_survive_and_a_broken_png_is_dropped() {
        let c = Clip {
            content: Content::Files { paths: vec!["C:\\a\\b.txt".into(), "D:\\c".into()] },
            text: String::new(),
            ..Clip::plain(ClipId(3), String::new(), 1, false)
        };
        assert_eq!(trip(&c), Some(c));
        let bad = Record { id: 1, last_copied_ms: 1, pinned: false, body: Body::Image { width: 1, height: 1, png: vec![1, 2, 3] } };
        assert_eq!(Clip::from_record(bad), None);
    }

    #[test]
    fn duplicate_keys_ignore_formatting_and_search_reads_file_names() {
        let a = Clip::plain(ClipId(1), "same".into(), 1, false);
        let b = Clip { content: Content::Text { html: Some("<b>".into()), rtf: None }, ..a.clone() };
        assert_eq!(a.dup_key(), b.dup_key());
        let files = Clip {
            content: Content::Files { paths: vec!["C:\\Docs\\Report.pdf".into()] },
            text: String::new(),
            ..a.clone()
        };
        assert_eq!(files.search_text(), "Report.pdf");
        assert_eq!(a.search_text(), "same");
    }
}
