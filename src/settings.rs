//! `%LOCALAPPDATA%\Tasca\settings.ini`: a hand-parsed `key=value` file.
//! Missing, unreadable or garbled input falls back to the defaults; nothing
//! here can crash the resident.

use std::path::PathBuf;

use crate::hotkey::Hotkey;

/// Only the shortcut lives in the file today. "Start with Windows" is derived
/// from the Startup shortcut itself, not stored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    /// `None` = the user cleared it ("None"), so Hyperkey users are unaffected.
    pub shortcut: Option<Hotkey>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shortcut: Some(Hotkey::DEFAULT),
        }
    }
}

pub fn dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Tasca"))
}

pub fn path() -> Option<PathBuf> {
    dir().map(|d| d.join("settings.ini"))
}

/// Read the settings; a missing or garbled file yields `Settings::default()`.
pub fn load() -> Settings {
    match path().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(text) => parse(&text),
        None => Settings::default(),
    }
}

/// Write `settings` to disk (worker thread; never on the UI thread).
pub fn save(settings: &Settings) -> Result<(), String> {
    let path = path().ok_or_else(|| "LOCALAPPDATA is not set".to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("ini.tmp");
    std::fs::write(&tmp, render(settings)).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp); // no stray .tmp after a failed save
        format!("{}: {e}", path.display())
    })
}

/// Parse the ini text. Unknown keys and junk lines are ignored; a missing or
/// unparseable `shortcut` value means the default (an explicit `None` or an
/// empty value means no shortcut).
pub fn parse(text: &str) -> Settings {
    // Notepad (and Windows PowerShell 5) like to prepend a UTF-8 byte order mark.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = Settings::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("shortcut") {
            continue;
        }
        let value = value.trim();
        out.shortcut = if value.is_empty() || value.eq_ignore_ascii_case("none") {
            None
        } else {
            // A shortcut we cannot read, or one that is not safe to grab
            // globally (hand-edited file), is the "garbled file" case.
            Some(
                Hotkey::parse(value)
                    .filter(Hotkey::is_safe)
                    .unwrap_or(Hotkey::DEFAULT),
            )
        };
    }
    out
}

/// The tray-tooltip notice for a failed background apply, `None` when both
/// steps worked. `start` is the Startup-shortcut error, `save` the ini error.
pub fn failure_notice(start: Option<&str>, save: Option<&str>) -> Option<String> {
    match (start.is_some(), save.is_some()) {
        (false, false) => None,
        (true, false) => Some("Start with Windows failed".to_string()),
        (false, true) => Some("settings not saved".to_string()),
        (true, true) => Some("settings not saved, Start with Windows failed".to_string()),
    }
}

pub fn render(settings: &Settings) -> String {
    let shortcut = match settings.shortcut {
        Some(hk) => hk.text(),
        None => "None".to_string(),
    };
    format!(
        "# Tasca settings (hand-edited: the app rewrites this file on OK)\nshortcut={shortcut}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_missing_or_empty() {
        assert_eq!(parse(""), Settings::default());
        assert_eq!(parse("\n\n  \n"), Settings::default());
        assert_eq!(
            parse("# only a comment\n; and another\n"),
            Settings::default()
        );
    }

    #[test]
    fn round_trips() {
        for settings in [
            Settings::default(),
            Settings { shortcut: None },
            Settings {
                shortcut: Hotkey::parse("Ctrl+Shift+F13"),
            },
            Settings {
                shortcut: Hotkey::parse("Win+Alt+Num3"),
            },
        ] {
            assert_eq!(parse(&render(&settings)), settings);
        }
    }

    #[test]
    fn garbled_values_fall_back_to_the_default() {
        assert_eq!(parse("shortcut=\n"), Settings { shortcut: None });
        assert_eq!(parse("shortcut=None\n"), Settings { shortcut: None });
        assert_eq!(parse("shortcut=none\n"), Settings { shortcut: None });
        assert_eq!(parse("shortcut=Hyper+Q\n"), Settings::default());
        assert_eq!(parse("shortcut=Ctrl+\n"), Settings::default());
        assert_eq!(parse("shortcut=\u{0}\u{1}\n"), Settings::default());
        assert_eq!(
            parse("garbage without an equals sign\n"),
            Settings::default()
        );
    }

    #[test]
    fn keys_are_case_insensitive_and_trimmed() {
        assert_eq!(
            parse("  SHORTCUT = Ctrl + Alt + D  \r\n"),
            Settings::default()
        );
        assert_eq!(parse("shortcut=ctrl+alt+d\n"), Settings::default());
    }

    #[test]
    fn unknown_keys_are_ignored_and_last_shortcut_wins() {
        let s = parse("version=9\nshortcut=Ctrl+Alt+D\nshortcut=Ctrl+Alt+E\n");
        assert_eq!(
            s,
            Settings {
                shortcut: Hotkey::parse("Ctrl+Alt+E")
            }
        );
    }

    #[test]
    fn a_utf8_bom_is_ignored() {
        let s = parse("\u{feff}shortcut=Ctrl+Alt+E\n");
        assert_eq!(s.shortcut, Hotkey::parse("Ctrl+Alt+E"));
        assert_eq!(parse("\u{feff}shortcut=None\n").shortcut, None);
    }

    #[test]
    fn unsafe_shortcuts_fall_back_to_the_default() {
        for text in ["D", "Shift+D", "Ctrl+C", "Alt+F4", "Alt+Tab", "Ctrl+S"] {
            assert_eq!(
                parse(&format!("shortcut={text}\n")),
                Settings::default(),
                "{text}"
            );
        }
        assert_eq!(parse("shortcut=F13\n").shortcut, Hotkey::parse("F13"));
    }

    #[test]
    fn failure_notices() {
        assert_eq!(failure_notice(None, None), None);
        assert_eq!(
            failure_notice(Some("x"), None).as_deref(),
            Some("Start with Windows failed")
        );
        assert_eq!(
            failure_notice(None, Some("x")).as_deref(),
            Some("settings not saved")
        );
        assert!(failure_notice(Some("x"), Some("y"))
            .unwrap()
            .contains("not saved"));
    }

    #[test]
    fn load_from_a_missing_file_is_the_default() {
        // The real path depends on LOCALAPPDATA; an empty read is the hard case.
        assert_eq!(parse(""), Settings::default());
    }
}
