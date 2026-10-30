//! Pure format helpers for the Clips that carry more than plain text: the
//! kind choice, the size cap, the CF_HTML header, the CF_HDROP path list,
//! duplicate keys and the row words for images and files.

// Not wired into the recorder and paste-back yet.
#![allow(dead_code)]

/// What a copy offers, as flags the reader fills from `IsClipboardFormatAvailable`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Offered {
    pub hdrop: bool,
    pub text: bool,
    pub html: bool,
    pub rtf: bool,
    pub dibv5: bool,
    pub dib: bool,
}

/// What kind of Clip a copy becomes, and which formats to read for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Files,
    Text { html: bool, rtf: bool },
    /// `v5`: read `CF_DIBV5` (it carries alpha), else `CF_DIB`.
    Image { v5: bool },
}

/// Files win over text, text (keeping HTML and RTF) over an image, so an
/// image offered next to text is dropped. Nothing recordable gives `None`.
pub fn choose_kind(o: &Offered) -> Option<Kind> {
    if o.hdrop {
        Some(Kind::Files)
    } else if o.text {
        Some(Kind::Text { html: o.html, rtf: o.rtf })
    } else if o.dibv5 || o.dib {
        Some(Kind::Image { v5: o.dibv5 })
    } else {
        None
    }
}

/// A copy whose stored size is above this is skipped.
pub const MAX_STORED: usize = 10 * 1024 * 1024;

/// Whether all kept formats together (the image already as PNG) fit.
pub fn within_cap(parts: &[usize]) -> bool {
    parts.iter().fold(0usize, |a, &n| a.saturating_add(n)) <= MAX_STORED
}

/// What makes two Clips the same copy. Formatting never counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DupKey {
    Text(String),
    Pixels { width: u32, height: u32, hash: u64 },
    Files(Vec<String>),
}

pub fn text_key(text: &str) -> DupKey {
    DupKey::Text(text.to_string())
}

pub fn pixels_key(width: u32, height: u32, hash: u64) -> DupKey {
    DupKey::Pixels { width, height, hash }
}

/// The path list compared case-insensitively, in copy order.
pub fn files_key(paths: &[String]) -> DupKey {
    DupKey::Files(paths.iter().map(|p| p.to_lowercase()).collect())
}

// ---- CF_HTML ----

/// The offsets of a `HTML Format` clipboard block, checked against its length.
#[derive(Debug, PartialEq, Eq)]
pub struct CfHtml {
    pub start_html: usize,
    pub end_html: usize,
    pub start_fragment: usize,
    pub end_fragment: usize,
    pub source_url: Option<String>,
}

fn header_value(head: &str, key: &str) -> Option<String> {
    head.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(':').map(|v| v.trim().to_string()))
}

/// Reads the header of an `HTML Format` block. `None` when an offset is
/// missing, out of range or out of order (the block is then kept as is but
/// not trusted).
pub fn parse_cf_html(data: &[u8]) -> Option<CfHtml> {
    // The header is ASCII and sits first; stop reading at the first `<`.
    let head_end = data.iter().position(|&b| b == b'<').unwrap_or(data.len()).min(1024);
    let head = std::str::from_utf8(&data[..head_end]).ok()?;
    let num = |k: &str| header_value(head, k)?.parse::<usize>().ok();
    let (start_html, end_html) = (num("StartHTML")?, num("EndHTML")?);
    let (start_fragment, end_fragment) = (num("StartFragment")?, num("EndFragment")?);
    let ok = start_html <= start_fragment
        && start_fragment <= end_fragment
        && end_fragment <= end_html
        && end_html <= data.len();
    ok.then(|| CfHtml {
        start_html,
        end_html,
        start_fragment,
        end_fragment,
        source_url: header_value(head, "SourceURL").filter(|u| !u.is_empty()),
    })
}

/// The fragment's text (what was selected), if the block parses and the
/// fragment is UTF-8.
pub fn cf_html_fragment(data: &[u8]) -> Option<&str> {
    let h = parse_cf_html(data)?;
    std::str::from_utf8(&data[h.start_fragment..h.end_fragment]).ok()
}

/// Wraps an HTML fragment in a CF_HTML block (version 0.9, 10-digit offsets).
pub fn build_cf_html(fragment: &str, source_url: Option<&str>) -> Vec<u8> {
    const PRE: &str = "<html><body>\r\n<!--StartFragment-->";
    const POST: &str = "<!--EndFragment-->\r\n</body></html>";
    let url = source_url.map(|u| format!("SourceURL:{u}\r\n")).unwrap_or_default();
    // Every number is 10 digits, so the header length does not depend on them.
    let head_len = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n".len() + url.len();
    let start_html = head_len;
    let start_fragment = start_html + PRE.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + POST.len();
    format!(
        "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n{url}{PRE}{fragment}{POST}"
    )
    .into_bytes()
}

// ---- CF_HDROP ----

const DROPFILES_SIZE: usize = 20;

/// The paths in a `CF_HDROP` block (a `DROPFILES` header, then a list ended
/// by an empty entry), wide or ANSI. A damaged block gives what could be read.
pub fn decode_hdrop(data: &[u8]) -> Vec<String> {
    if data.len() < DROPFILES_SIZE {
        return Vec::new();
    }
    let at = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let wide = data[16] != 0 || data[17] != 0 || data[18] != 0 || data[19] != 0;
    let Some(list) = data.get(at..) else { return Vec::new() };
    let mut out = Vec::new();
    if wide {
        let units: Vec<u16> = list.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        for p in units.split(|&u| u == 0) {
            if p.is_empty() {
                break;
            }
            out.push(String::from_utf16_lossy(p));
        }
    } else {
        for p in list.split(|&b| b == 0) {
            if p.is_empty() {
                break;
            }
            out.push(String::from_utf8_lossy(p).into_owned());
        }
    }
    out
}

/// A wide `CF_HDROP` block for these paths.
pub fn encode_hdrop(paths: &[String]) -> Vec<u8> {
    let mut out = vec![0u8; DROPFILES_SIZE];
    out[0..4].copy_from_slice(&(DROPFILES_SIZE as u32).to_le_bytes());
    out[16..20].copy_from_slice(&1u32.to_le_bytes());
    for p in paths {
        for u in p.encode_utf16() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
    }
    out.extend_from_slice(&[0, 0]);
    out
}

// ---- row words ----

/// The type line's kind for an image: "Image · 1920 × 1080".
pub fn image_label(width: u32, height: u32) -> String {
    format!("Image · {width} × {height}")
}

/// The type line's kind for files: "1 file" / "3 files".
pub fn files_label(count: usize) -> String {
    if count == 1 { "1 file".to_string() } else { format!("{count} files") }
}

/// The last component of a path (`C:\a\b.txt` and `C:\a\dir\` give `b.txt`
/// and `dir`; a drive root gives itself).
pub fn file_name(path: &str) -> &str {
    let t = path.trim_end_matches(['\\', '/']);
    let name = t.rsplit(['\\', '/']).next().unwrap_or(t);
    if name.is_empty() || name.ends_with(':') { path } else { name }
}

/// A files Clip's title: the first name, then " +N-1" for the others.
pub fn files_title(paths: &[String]) -> String {
    match paths {
        [] => String::new(),
        [one] => file_name(one).to_string(),
        [first, rest @ ..] => format!("{} +{}", file_name(first), rest.len()),
    }
}

/// What a search matches in a files Clip: the names, not the folders.
pub fn files_search_text(paths: &[String]) -> String {
    paths.iter().map(|p| file_name(p)).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(f: &[&str]) -> Offered {
        let mut o = Offered::default();
        for n in f {
            match *n {
                "hdrop" => o.hdrop = true,
                "text" => o.text = true,
                "html" => o.html = true,
                "rtf" => o.rtf = true,
                "dibv5" => o.dibv5 = true,
                "dib" => o.dib = true,
                _ => unreachable!(),
            }
        }
        o
    }

    #[test]
    fn excel_like_text_html_rtf_and_dib_is_a_text_clip_without_the_image() {
        let k = choose_kind(&offered(&["text", "html", "rtf", "dib", "dibv5"]));
        assert_eq!(k, Some(Kind::Text { html: true, rtf: true }));
    }

    #[test]
    fn a_browser_image_with_only_html_and_dib_is_an_image() {
        assert_eq!(choose_kind(&offered(&["html", "dib"])), Some(Kind::Image { v5: false }));
        assert_eq!(choose_kind(&offered(&["html", "dibv5", "dib"])), Some(Kind::Image { v5: true }));
    }

    #[test]
    fn explorer_files_win_over_the_text_they_also_offer() {
        assert_eq!(choose_kind(&offered(&["hdrop", "text", "dib"])), Some(Kind::Files));
    }

    #[test]
    fn plain_text_keeps_only_the_formats_offered_and_nothing_offered_is_nothing() {
        assert_eq!(choose_kind(&offered(&["text"])), Some(Kind::Text { html: false, rtf: false }));
        assert_eq!(choose_kind(&offered(&["text", "rtf"])), Some(Kind::Text { html: false, rtf: true }));
        assert_eq!(choose_kind(&offered(&["html", "rtf"])), None);
        assert_eq!(choose_kind(&Offered::default()), None);
    }

    #[test]
    fn the_cap_is_ten_mebibytes_over_all_formats() {
        assert!(within_cap(&[MAX_STORED]));
        assert!(!within_cap(&[MAX_STORED + 1]));
        assert!(within_cap(&[MAX_STORED / 2, MAX_STORED / 2]));
        assert!(!within_cap(&[MAX_STORED / 2, MAX_STORED / 2, 1]));
        assert!(!within_cap(&[usize::MAX, 2]));
    }

    #[test]
    fn duplicate_keys_compare_content_per_kind() {
        assert_eq!(text_key("a"), text_key("a"));
        assert_ne!(text_key("a"), text_key("A"));
        assert_eq!(pixels_key(2, 2, 7), pixels_key(2, 2, 7));
        assert_ne!(pixels_key(2, 2, 7), pixels_key(4, 1, 7));
        assert_ne!(text_key("a"), files_key(&["a".to_string()]));
        let a = vec!["C:\\Docs\\Report.PDF".to_string(), "C:\\x.txt".to_string()];
        let b = vec!["c:\\docs\\report.pdf".to_string(), "C:\\X.TXT".to_string()];
        assert_eq!(files_key(&a), files_key(&b));
        assert_ne!(files_key(&a), files_key(&a[..1]));
    }

    #[test]
    fn cf_html_round_trips_through_build_and_parse() {
        let block = build_cf_html("<b>bold</b> é", Some("https://example.com/a"));
        let h = parse_cf_html(&block).unwrap();
        assert_eq!(h.end_html, block.len());
        assert_eq!(h.source_url.as_deref(), Some("https://example.com/a"));
        assert_eq!(cf_html_fragment(&block), Some("<b>bold</b> é"));
        let bare = build_cf_html("x", None);
        assert_eq!(parse_cf_html(&bare).unwrap().source_url, None);
        assert_eq!(cf_html_fragment(&bare), Some("x"));
    }

    #[test]
    fn cf_html_from_a_browser_parses() {
        let body = "<html><body><!--StartFragment--><i>hi</i><!--EndFragment--></body></html>";
        let head = |s: usize, e: usize, fs: usize, fe: usize| {
            format!("Version:1.0\r\nStartHTML:{s:09}\r\nEndHTML:{e:09}\r\nStartFragment:{fs:09}\r\nEndFragment:{fe:09}\r\nSourceURL:https://x.test/\r\n")
        };
        let len = head(0, 0, 0, 0).len();
        let fs = len + "<html><body><!--StartFragment-->".len();
        let block = format!("{}{body}", head(len, len + body.len(), fs, fs + "<i>hi</i>".len()));
        assert_eq!(cf_html_fragment(block.as_bytes()), Some("<i>hi</i>"));
    }

    #[test]
    fn cf_html_with_bad_offsets_does_not_parse() {
        assert!(parse_cf_html(b"").is_none());
        assert!(parse_cf_html(b"<html>no header</html>").is_none());
        let mut block = build_cf_html("abc", None);
        block.truncate(block.len() - 5); // EndHTML now points past the data
        assert!(parse_cf_html(&block).is_none());
        let swapped = "Version:0.9\r\nStartHTML:0000000050\r\nEndHTML:0000000010\r\nStartFragment:0000000020\r\nEndFragment:0000000030\r\n<html>";
        assert!(parse_cf_html(swapped.as_bytes()).is_none());
    }

    #[test]
    fn hdrop_round_trips_wide_paths() {
        let paths = vec!["C:\\Users\\Mattia\\Documenti\\Relazione è.docx".to_string(), "D:\\x y\\".to_string()];
        assert_eq!(decode_hdrop(&encode_hdrop(&paths)), paths);
        assert!(decode_hdrop(&encode_hdrop(&[])).is_empty());
    }

    #[test]
    fn hdrop_ansi_and_damaged_blocks_decode_what_they_can() {
        let mut ansi = vec![0u8; DROPFILES_SIZE];
        ansi[0] = DROPFILES_SIZE as u8;
        ansi.extend_from_slice(b"C:\\a.txt\0C:\\b.txt\0\0");
        assert_eq!(decode_hdrop(&ansi), ["C:\\a.txt", "C:\\b.txt"]);
        assert!(decode_hdrop(&[1, 2, 3]).is_empty());
        let mut bad = encode_hdrop(&["C:\\a".to_string()]);
        bad[0] = 0xFF; // list offset beyond the data
        assert!(decode_hdrop(&bad).is_empty());
        // No closing double NUL: the last path is still read.
        let mut open = encode_hdrop(&["C:\\a".to_string()]);
        open.truncate(open.len() - 2);
        assert_eq!(decode_hdrop(&open), ["C:\\a"]);
    }

    #[test]
    fn file_rows_read_first_name_plus_the_rest() {
        let p = |s: &str| s.to_string();
        assert_eq!(files_label(1), "1 file");
        assert_eq!(files_label(3), "3 files");
        assert_eq!(files_title(&[p("C:\\a\\report.pdf")]), "report.pdf");
        assert_eq!(files_title(&[p("C:\\a\\report.pdf"), p("C:\\b.txt"), p("C:\\c")]), "report.pdf +2");
        assert_eq!(files_title(&[p("C:\\a\\dir\\"), p("C:\\b")]), "dir +1");
        assert_eq!(files_title(&[p("C:\\")]), "C:\\");
        assert_eq!(files_search_text(&[p("C:\\a\\Report.pdf"), p("D:\\notes.txt")]), "Report.pdf\nnotes.txt");
        assert_eq!(image_label(1920, 1080), "Image · 1920 × 1080");
    }
}
