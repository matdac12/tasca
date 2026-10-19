//! Names of Materialized files (spec §4), pure: sanitising, relative paths
//! from virtual-file descriptors, case-insensitive `name (2).ext`
//! de-duplication, and the names and contents of `.url` and `.txt` files.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::SYSTEMTIME;

/// Longest stem kept, in characters (Chromium's `kTruncatedDisplayNameLength`).
const MAX_STEM: usize = 128;
/// Longer "extensions" are part of the stem.
const MAX_EXT: usize = 16;

const RESERVED: &[&str] = &["CON", "PRN", "AUX", "NUL"];

/// `stem` and `ext` (without the dot) of a file name. A leading dot, an
/// extension over `MAX_EXT` chars or one with a space is no extension.
fn split_ext(name: &str) -> (&str, Option<&str>) {
    match name.rfind('.') {
        Some(i) if i > 0 => {
            let ext = &name[i + 1..];
            let n = ext.chars().count();
            if (1..=MAX_EXT).contains(&n) && !ext.contains(' ') { (&name[..i], Some(ext)) } else { (name, None) }
        }
        _ => (name, None),
    }
}

fn join(stem: &str, ext: Option<&str>) -> String {
    match ext {
        Some(e) => format!("{stem}.{e}"),
        None => stem.to_owned(),
    }
}

/// A device name Windows reserves, whatever follows the first dot.
fn reserved(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    RESERVED.contains(&base.as_str())
        || ((base.starts_with("COM") || base.starts_with("LPT"))
            && base.len() == 4
            && matches!(base.as_bytes()[3], b'1'..=b'9'))
}

fn trim_end(s: &str) -> &str {
    s.trim_end_matches(['.', ' '])
}

/// `sanitize`, but "" when nothing usable is left.
fn clean(name: &str) -> String {
    let mapped: String = name.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let name = trim_end(&mapped);
    let (stem, ext) = split_ext(name);
    let stem = if stem.chars().count() > MAX_STEM {
        let cut: String = stem.chars().take(MAX_STEM).collect();
        trim_end(&cut).to_owned()
    } else {
        stem.to_owned()
    };
    if stem.is_empty() {
        return String::new();
    }
    let name = join(&stem, ext);
    if reserved(&name) { format!("_{name}") } else { name }
}

/// One file or folder name, safe to create: `<>:"/\|?*` and control
/// characters become `_`, trailing dots and spaces go, a reserved device name
/// (`CON`, `COM1.txt`, ...) gets a `_` in front, and the stem is capped at 128
/// characters. Nothing left → `Untitled`.
pub fn sanitize(name: &str) -> String {
    let s = clean(name);
    if s.is_empty() { "Untitled".into() } else { s }
}

/// A virtual file's name as sanitized path components. `None` (rejected) for
/// `..`, a drive, an absolute or UNC path, or nothing at all. `.` and empty
/// components are skipped.
pub fn split_relative(name: &str) -> Option<Vec<String>> {
    const SEPARATORS: [char; 2] = ['\\', '/'];
    if name.starts_with(SEPARATORS) {
        return None;
    }
    let b = name.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return None;
    }
    let mut parts = Vec::new();
    for c in name.split(SEPARATORS) {
        match c {
            "" | "." => {}
            ".." => return None,
            c => parts.push(sanitize(c)),
        }
    }
    (!parts.is_empty()).then_some(parts)
}

/// `name` with ` (n)` before its extension: `a (2).txt`.
pub fn numbered(name: &str, n: u32) -> String {
    let (stem, ext) = split_ext(name);
    join(&format!("{stem} ({n})"), ext)
}

/// Plans the relative paths of one drop folder: each name is unique
/// case-insensitively (`name (2).ext`), and folders named again are merged.
#[derive(Default)]
pub struct Planner {
    /// Every planned relative path, lower case.
    taken: HashSet<String>,
    /// A folder's source path (lower case, `\`) → its planned relative path.
    dirs: HashMap<String, PathBuf>,
}

impl Planner {
    /// A free name for `name` in the planned folder `parent`.
    fn unique(&mut self, parent: &Path, name: &str) -> PathBuf {
        let key = |p: &Path| p.to_string_lossy().to_lowercase();
        let mut rel = parent.join(name);
        let mut n = 2;
        while self.taken.contains(&key(&rel)) {
            rel = parent.join(numbered(name, n));
            n += 1;
        }
        self.taken.insert(key(&rel));
        rel
    }

    /// The planned relative path of an entry (`parts` from `split_relative`).
    /// Parent folders not planned yet are planned on the way.
    pub fn place(&mut self, parts: &[String], is_dir: bool) -> PathBuf {
        let mut parent = PathBuf::new();
        let mut key = String::new();
        for (i, part) in parts.iter().enumerate() {
            if !key.is_empty() {
                key.push('\\');
            }
            key.push_str(&part.to_lowercase());
            let last = i + 1 == parts.len();
            if last && !is_dir {
                return self.unique(&parent, part);
            }
            parent = match self.dirs.get(&key) {
                Some(d) => d.clone(),
                None => {
                    let d = self.unique(&parent, part);
                    self.dirs.insert(key.clone(), d.clone());
                    d
                }
            };
        }
        parent
    }
}

/// `yyyy-MM-dd HHmmss`.
pub fn stamp(t: &SYSTEMTIME) -> String {
    format!("{:04}-{:02}-{:02} {:02}{:02}{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// The host of a `scheme://` URL, without user info or port.
pub fn host(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if hostport.starts_with('[') {
        hostport.split_inclusive(']').next().unwrap_or("")
    } else {
        hostport.split(':').next().unwrap_or("")
    };
    (!host.is_empty()).then(|| host.to_owned())
}

/// `<title>.url`, else `<host>.url`, else `Link.url`.
pub fn url_name(title: Option<&str>, url: &str) -> String {
    let stem = title.map(clean).filter(|t| !t.is_empty()).or_else(|| host(url).map(|h| clean(&h))).filter(|s| !s.is_empty());
    format!("{}.url", stem.as_deref().unwrap_or("Link"))
}

/// The title in a `text/x-moz-url` (`url\ntitle`): its second line.
pub fn moz_title(moz: &str) -> Option<&str> {
    moz.lines().nth(1).map(str::trim).filter(|t| !t.is_empty())
}

/// The contents of an Internet Shortcut, as Explorer writes it.
pub fn url_file(url: &str) -> String {
    format!("[InternetShortcut]\r\nURL={url}\r\n")
}

/// Longest name taken from a text's first line, in characters.
const TEXT_NAME_CHARS: usize = 40;

/// The first non-blank line, up to 40 characters, sanitized, `.txt`; if that
/// leaves nothing, `Text yyyy-MM-dd HHmmss.txt`.
pub fn text_name(text: &str, now: &SYSTEMTIME) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let cut: String = line.chars().take(TEXT_NAME_CHARS).collect();
    match clean(&cut) {
        s if s.is_empty() => format!("Text {}.txt", stamp(now)),
        s => format!("{s}.txt"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_and_reserved_characters_become_underscores() {
        assert_eq!(sanitize(r#"a<b>c:d"e/f\g|h?i*j.txt"#), "a_b_c_d_e_f_g_h_i_j.txt");
        assert_eq!(sanitize("tab\there\u{1}.txt"), "tab_here_.txt");
        assert_eq!(sanitize("Re: meeting.msg"), "Re_ meeting.msg");
        assert_eq!(sanitize("caffè ☕.png"), "caffè ☕.png");
    }

    #[test]
    fn trailing_dots_and_spaces_go() {
        assert_eq!(sanitize("report. . "), "report");
        assert_eq!(sanitize("notes.txt.  "), "notes.txt");
        assert_eq!(sanitize(" leading stays.txt"), " leading stays.txt");
    }

    #[test]
    fn empty_names_get_a_fallback() {
        assert_eq!(sanitize(""), "Untitled");
        assert_eq!(sanitize("..."), "Untitled");
        assert_eq!(sanitize("   "), "Untitled");
    }

    #[test]
    fn reserved_device_names_are_prefixed_with_any_extension() {
        for n in ["CON", "prn", "Aux", "NUL", "COM1", "com9", "LPT1", "lpt9"] {
            assert_eq!(sanitize(n), format!("_{n}"));
            assert_eq!(sanitize(&format!("{n}.txt")), format!("_{n}.txt"));
            assert_eq!(sanitize(&format!("{n}.tar.gz")), format!("_{n}.tar.gz"));
        }
        assert_eq!(sanitize("CON ."), "_CON");
        // Only whole stems are reserved.
        for n in ["CONSOLE.txt", "COM10", "COM0", "LPT", "nul2.txt", "auxiliary"] {
            assert_eq!(sanitize(n), n);
        }
    }

    #[test]
    fn the_stem_is_capped_at_128_chars_keeping_the_extension() {
        let long = "é".repeat(300);
        let s = sanitize(&format!("{long}.jpeg"));
        assert_eq!(s, format!("{}.jpeg", "é".repeat(128)));
        assert_eq!(sanitize(&"x".repeat(200)), "x".repeat(128));
        // A cut that ends in a space or dot is trimmed again.
        let s = sanitize(&format!("{} {}.txt", "a".repeat(127), "b".repeat(10)));
        assert_eq!(s, format!("{}.txt", "a".repeat(127)));
        // An absurd "extension" is part of the stem.
        let s = sanitize(&format!("a.{}", "b".repeat(200)));
        assert_eq!(s.chars().count(), 128);
    }

    #[test]
    fn numbered_names_go_before_the_extension() {
        assert_eq!(numbered("a.txt", 2), "a (2).txt");
        assert_eq!(numbered("archive.tar.gz", 3), "archive.tar (3).gz");
        assert_eq!(numbered("folder", 2), "folder (2)");
        assert_eq!(numbered(".gitignore", 2), ".gitignore (2)");
    }

    fn parts(name: &str) -> Option<Vec<String>> {
        split_relative(name)
    }

    fn v(p: &[&str]) -> Option<Vec<String>> {
        Some(p.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn relative_names_keep_their_folders() {
        assert_eq!(parts("photo.jpg"), v(&["photo.jpg"]));
        assert_eq!(parts(r"folder\inner\a.txt"), v(&["folder", "inner", "a.txt"]));
        assert_eq!(parts("folder/a.txt"), v(&["folder", "a.txt"]));
        assert_eq!(parts(r"folder\\.\a.txt\"), v(&["folder", "a.txt"]));
        assert_eq!(parts(r"bad:name\x?.txt"), v(&["bad_name", "x_.txt"]));
    }

    #[test]
    fn parent_drive_and_absolute_names_are_rejected() {
        for bad in [
            r"..\evil.txt",
            r"a\..\..\evil.txt",
            "a/../b.txt",
            "..",
            r"C:\Windows\evil.dll",
            "C:evil.txt",
            r"c:",
            r"\evil.txt",
            "/evil.txt",
            r"\\server\share\evil.txt",
            r"\\?\C:\evil.txt",
            "",
            r".\",
        ] {
            assert_eq!(parts(bad), None, "{bad:?}");
        }
    }

    fn plan(entries: &[(&str, bool)]) -> Vec<String> {
        let mut p = Planner::default();
        entries
            .iter()
            .map(|(n, dir)| p.place(&split_relative(n).unwrap(), *dir).to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn duplicates_are_numbered_case_insensitively() {
        assert_eq!(
            plan(&[("Re: hi.msg", false), ("RE: HI.msg", false), ("re: hi.msg", false), ("other.msg", false)]),
            ["Re_ hi.msg", "RE_ HI (2).msg", "re_ hi (3).msg", "other.msg"]
        );
        // A numbered name that is itself taken counts up further.
        assert_eq!(plan(&[("a (2).txt", false), ("a.txt", false), ("a.txt", false)]), ["a (2).txt", "a.txt", "a (3).txt"]);
    }

    #[test]
    fn folder_trees_are_recreated_and_merged() {
        assert_eq!(
            plan(&[("Folder", true), (r"folder\a.txt", false), (r"FOLDER\sub", true), (r"Folder\sub\b.txt", false), (r"folder\a.txt", false)]),
            ["Folder", r"Folder\a.txt", r"Folder\sub", r"Folder\sub\b.txt", r"Folder\a (2).txt"]
        );
        // Without directory entries, the folders come from the file names.
        assert_eq!(plan(&[(r"x\y\z.txt", false), (r"x\w.txt", false)]), [r"x\y\z.txt", r"x\w.txt"]);
        // A file and a folder with one name: the folder is numbered.
        assert_eq!(plan(&[("x", false), (r"x\a.txt", false)]), ["x", r"x (2)\a.txt"]);
    }

    fn at(y: u16, mo: u16, d: u16, h: u16, mi: u16, s: u16) -> SYSTEMTIME {
        SYSTEMTIME { wYear: y, wMonth: mo, wDay: d, wHour: h, wMinute: mi, wSecond: s, ..Default::default() }
    }

    #[test]
    fn stamps_are_zero_padded() {
        assert_eq!(stamp(&at(2026, 10, 9, 8, 5, 3)), "2026-10-09 080503");
        assert_eq!(stamp(&at(2026, 1, 31, 23, 59, 59)), "2026-01-31 235959");
    }

    #[test]
    fn text_is_named_from_its_first_line() {
        let now = at(2026, 10, 19, 10, 15, 30);
        assert_eq!(text_name("Shopping list\r\nmilk\r\n", &now), "Shopping list.txt");
        assert_eq!(text_name("\n\n  hello world  \nmore", &now), "hello world.txt");
        assert_eq!(text_name("what? a/b: c", &now), "what_ a_b_ c.txt");
        let long = "word ".repeat(20);
        assert_eq!(text_name(&long, &now), format!("{}.txt", "word ".repeat(8).trim_end()));
        assert_eq!(text_name(&"é".repeat(50), &now), format!("{}.txt", "é".repeat(40)));
    }

    #[test]
    fn text_without_a_usable_line_is_named_by_time() {
        let now = at(2026, 10, 19, 10, 15, 30);
        assert_eq!(text_name("", &now), "Text 2026-10-19 101530.txt");
        assert_eq!(text_name(" \r\n\t\n", &now), "Text 2026-10-19 101530.txt");
        assert_eq!(text_name("...\nsecond", &now), "Text 2026-10-19 101530.txt");
    }

    #[test]
    fn url_files_are_internet_shortcuts() {
        assert_eq!(url_file("https://example.com/a?b=1"), "[InternetShortcut]\r\nURL=https://example.com/a?b=1\r\n");
    }

    #[test]
    fn url_hosts() {
        assert_eq!(host("https://www.example.com/path?q=1").as_deref(), Some("www.example.com"));
        assert_eq!(host("http://user:pw@Example.com:8080/x").as_deref(), Some("Example.com"));
        assert_eq!(host("https://example.com").as_deref(), Some("example.com"));
        assert_eq!(host("https://[::1]:80/").as_deref(), Some("[::1]"));
        assert_eq!(host("mailto:someone@example.com"), None);
        assert_eq!(host("file:///C:/x.txt"), None);
        assert_eq!(host("not a url"), None);
    }

    #[test]
    fn moz_urls_carry_the_title_on_line_two() {
        assert_eq!(moz_title("https://example.com/\nExample Domain"), Some("Example Domain"));
        assert_eq!(moz_title("https://example.com/\r\n  Spaced  \r\n"), Some("Spaced"));
        assert_eq!(moz_title("https://example.com/\n"), None);
        assert_eq!(moz_title("https://example.com/"), None);
    }

    #[test]
    fn url_names_come_from_the_title_then_the_host() {
        let url = "https://www.example.com/article";
        assert_eq!(url_name(Some("A title: part 2"), url), "A title_ part 2.url");
        assert_eq!(url_name(Some("  "), url), "www.example.com.url");
        assert_eq!(url_name(None, url), "www.example.com.url");
        assert_eq!(url_name(None, "mailto:x@y.z"), "Link.url");
        assert_eq!(url_name(Some("v1.2"), url), "v1.2.url");
    }
}
