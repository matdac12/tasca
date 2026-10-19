//! The startup sweep (spec §4): `%TEMP%\Tasca` entries older than 24 h are
//! deleted, on a thread of their own, when the resident starts. Nothing is
//! persisted, so at that moment no Item refers to them. Materialized files
//! are never deleted at Take out: targets read them later.

use std::path::Path;
use std::time::{Duration, SystemTime};

use super::dropdir;
use crate::log::log;

/// Entries last changed longer ago than this go.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Last changed more than `MAX_AGE` before `now`. A time in the future (the
/// clock stepped back) is not expired.
pub fn expired(modified: SystemTime, now: SystemTime) -> bool {
    now.duration_since(modified).is_ok_and(|age| age > MAX_AGE)
}

/// Delete the expired entries directly under `root` (files, or folders with
/// all they hold). Returns how many went. Blocking: not on the UI thread.
pub fn sweep_in(root: &Path, now: SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else { return 0 };
    let mut gone = 0;
    for e in entries.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if !meta.modified().is_ok_and(|m| expired(m, now)) {
            continue;
        }
        let p = e.path();
        let r = if meta.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        match r {
            Ok(()) => gone += 1,
            Err(err) => log(format!("sweep: could not delete {}: {err}", p.display())),
        }
    }
    gone
}

/// Sweep `%TEMP%\Tasca` on a thread of its own (resident start).
pub fn start() {
    let spawned = std::thread::Builder::new().name("tasca-sweep".into()).spawn(|| {
        let root = dropdir::root();
        let n = sweep_in(&root, SystemTime::now());
        log(format!("sweep: {n} old entr{} deleted from {}", if n == 1 { "y" } else { "ies" }, root.display()));
    });
    if let Err(e) = spawned {
        log(format!("sweep: could not start: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn only_entries_older_than_a_day_expire() {
        let now = SystemTime::now();
        assert!(expired(now - 25 * HOUR, now));
        assert!(expired(now - 24 * HOUR - Duration::from_secs(1), now));
        assert!(!expired(now - 24 * HOUR, now));
        assert!(!expired(now - 23 * HOUR, now));
        assert!(!expired(now, now));
        // A clock that stepped back: the entry looks new, so it stays.
        assert!(!expired(now + HOUR, now));
    }

    #[test]
    fn the_sweep_deletes_old_files_and_folders_under_the_root_only() {
        let root = std::env::temp_dir().join(format!("tasca-sweep-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("1700000000000").join("inner")).unwrap();
        std::fs::write(root.join("1700000000000").join("inner").join("a.txt"), b"a").unwrap();
        std::fs::write(root.join("stray.txt"), b"s").unwrap();

        // Everything was just written: nothing goes.
        assert_eq!(sweep_in(&root, SystemTime::now()), 0);
        assert!(root.join("1700000000000").join("inner").join("a.txt").exists());

        // A day and a bit later, both entries go; the root stays.
        assert_eq!(sweep_in(&root, SystemTime::now() + 25 * HOUR), 2);
        assert!(root.is_dir());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);

        // No root yet (first run): nothing to do.
        std::fs::remove_dir(&root).unwrap();
        assert_eq!(sweep_in(&root, SystemTime::now()), 0);
    }
}
