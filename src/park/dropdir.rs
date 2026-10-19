//! Drop folders: `%TEMP%\Tasca\<drop-id>\`, one per Park that writes files
//! (spec §4). Ids are monotonic and unique, also across resident restarts, so
//! folders sort by age and a new Park never lands in an old folder.
//!
//! TASCA-16 reuses `create` for Materialized files and adds name sanitising,
//! case-insensitive de-duplication and the startup sweep of old folders.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::HSTRING;
use windows::Win32::Storage::FileSystem::{GetLongPathNameW, GetTempPath2W};

/// The last id handed out by this process.
static LAST: AtomicU64 = AtomicU64::new(0);

/// The next id: after `last`, and no earlier than `now_ms` (milliseconds since
/// the Unix epoch), so a restarted resident does not reuse a folder name.
pub fn next_id(last: u64, now_ms: u64) -> u64 {
    (last + 1).max(now_ms)
}

/// The temp folder: `[long form, as GetTempPath2W gives it]`. `%TEMP%` is
/// often in 8.3 form (`C:\Users\MATTIA~1\...`); drop folders use the long
/// form, so their paths match what Explorer reports later.
pub fn temp_dirs() -> &'static [PathBuf; 2] {
    static TEMP: OnceLock<[PathBuf; 2]> = OnceLock::new();
    TEMP.get_or_init(|| {
        let mut buf = [0u16; 261];
        let n = unsafe { GetTempPath2W(Some(&mut buf)) } as usize;
        let given = if n == 0 || n > buf.len() {
            std::env::temp_dir()
        } else {
            PathBuf::from(String::from_utf16_lossy(&buf[..n]))
        };
        [long_name(&given), given]
    })
}

/// The temp folder, long form.
pub fn temp_dir() -> &'static Path {
    &temp_dirs()[0]
}

/// `%TEMP%\Tasca`: every drop folder lives here.
pub fn root() -> PathBuf {
    temp_dir().join("Tasca")
}

/// The long form of an existing path; the path itself if it has no `~` (no
/// short component) or the lookup fails (e.g. it no longer exists).
pub fn long_name(p: &Path) -> PathBuf {
    if !p.as_os_str().to_string_lossy().contains('~') {
        return p.to_path_buf();
    }
    let wide = HSTRING::from(p);
    let mut buf = vec![0u16; 1024];
    loop {
        let n = unsafe { GetLongPathNameW(&wide, Some(&mut buf)) } as usize;
        match n {
            0 => return p.to_path_buf(),
            n if n < buf.len() => return PathBuf::from(String::from_utf16_lossy(&buf[..n])),
            n => buf.resize(n, 0),
        }
    }
}

/// Create a fresh, empty drop folder and return its path.
pub fn create() -> io::Result<PathBuf> {
    let root = root();
    std::fs::create_dir_all(&root)?;
    loop {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let last = LAST.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |l| Some(next_id(l, now))).unwrap_or_default();
        let id = next_id(last, now);
        let dir = root.join(id.to_string());
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            // Another process (or a clock step back) took it: take the next id.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// A path in `dir` for a file called `name` that does not exist yet:
/// `name`, else `stem (2).ext`, `stem (3).ext`, ... The file system answers
/// case-insensitively.
pub fn free_name(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().unwrap_or(name).to_string_lossy();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|c| !c.exists())
        .expect("an unused name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_increase_and_follow_the_clock() {
        assert_eq!(next_id(0, 1_000), 1_000);
        assert_eq!(next_id(1_000, 1_000), 1_001);
        assert_eq!(next_id(1_001, 1_000), 1_002);
        assert_eq!(next_id(5, 9_000), 9_000);
        // The clock stepped back: still after the last one.
        assert_eq!(next_id(9_000, 10), 9_001);
    }

    #[test]
    fn drop_folders_are_fresh_and_under_temp_tasca() {
        let a = create().unwrap();
        let b = create().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.parent(), Some(root().as_path()));
        assert!(a.is_dir() && std::fs::read_dir(&a).unwrap().next().is_none());
        let id = |p: &Path| p.file_name().unwrap().to_str().unwrap().parse::<u64>().unwrap();
        assert!(id(&b) > id(&a));
        std::fs::remove_dir(a).unwrap();
        std::fs::remove_dir(b).unwrap();
    }

    #[test]
    fn free_name_counts_up_past_existing_files() {
        let dir = create().unwrap();
        assert_eq!(free_name(&dir, "a.txt".as_ref()), dir.join("a.txt"));
        std::fs::write(dir.join("a.txt"), b"1").unwrap();
        assert_eq!(free_name(&dir, "A.TXT".as_ref()), dir.join("A (2).TXT"));
        std::fs::write(dir.join("a (2).txt"), b"2").unwrap();
        assert_eq!(free_name(&dir, "a.txt".as_ref()), dir.join("a (3).txt"));
        std::fs::create_dir(dir.join("folder")).unwrap();
        assert_eq!(free_name(&dir, "folder".as_ref()), dir.join("folder (2)"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
