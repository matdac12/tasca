//! `%LOCALAPPDATA%\Tasca\tasca.log`: truncated when a resident starts, appended
//! to by helpers. Timestamps are QueryPerformanceCounter milliseconds since the
//! process started.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

static CLOCK: OnceLock<(i64, f64)> = OnceLock::new();
static ROLE: OnceLock<&'static str> = OnceLock::new();

/// Counter value at first call, and ticks per millisecond.
fn clock() -> (i64, f64) {
    *CLOCK.get_or_init(|| unsafe {
        let (mut t, mut f) = (0i64, 0i64);
        let _ = QueryPerformanceCounter(&mut t);
        let _ = QueryPerformanceFrequency(&mut f);
        (t, f as f64 / 1000.0)
    })
}

/// Milliseconds since the process started (the first call starts the clock).
pub fn now_ms() -> f64 {
    let (t0, per_ms) = clock();
    let mut t = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut t);
    }
    (t - t0) as f64 / per_ms
}

pub fn set_role(role: &'static str) {
    let _ = ROLE.set(role);
}

fn log_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Tasca"))
}

/// Start a fresh log, keeping the previous run's as `tasca.log.1` (TASCA-25:
/// the evidence of a failed save used to vanish on restart). Only the
/// resident calls this, once, at start.
pub fn truncate() {
    if let Some(dir) = log_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::rename(dir.join("tasca.log"), dir.join("tasca.log.1"));
        let _ = std::fs::File::create(dir.join("tasca.log"));
    }
}

pub fn format_line(role: &str, pid: u32, ms: f64, msg: &str) -> String {
    format!("+{ms:>9.1}ms {role:<8} pid {pid:>6}  {msg}\n")
}

pub fn log(msg: impl AsRef<str>) {
    let line = format_line(ROLE.get().copied().unwrap_or("?"), std::process::id(), now_ms(), msg.as_ref());
    if let Some(dir) = log_dir() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("tasca.log")) {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_has_ms_timestamp_role_and_pid() {
        let l = format_line("resident", 42, 12.34, "hello");
        assert_eq!(l, "+     12.3ms resident pid     42  hello\n");
    }

    #[test]
    fn clock_is_monotonic() {
        let a = now_ms();
        let b = now_ms();
        assert!(b >= a);
    }
}
