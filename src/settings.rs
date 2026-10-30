//! `%LOCALAPPDATA%\Tasca\settings.ini`: a hand-parsed `key=value` file.
//! Missing, unreadable or garbled input falls back to the defaults; nothing
//! here can crash the resident.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::hotkey::Hotkey;

/// The shortcuts and the History switches. "Start with Windows" is derived
/// from the Startup shortcut itself, not stored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    /// `None` = the user cleared it ("None"), so Hyperkey users are unaffected.
    pub shortcut: Option<Hotkey>,
    /// The clipboard History records and opens.
    pub history: bool,
    /// The History's shortcut; kept in the file while `history` is off.
    pub history_shortcut: Option<Hotkey>,
    /// Leave out copies that password managers mark as private.
    pub skip_private: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shortcut: Some(Hotkey::DEFAULT),
            history: true,
            history_shortcut: Some(Hotkey::HISTORY_DEFAULT),
            skip_private: true,
        }
    }
}

/// What the resident is running with now: set at startup and on every
/// applied Settings OK. Read by the tray, the Settings window and the
/// clipboard reader thread, so it is a mutex, not a `RefCell`.
static CURRENT: Mutex<Settings> = Mutex::new(Settings {
    shortcut: Some(Hotkey::DEFAULT),
    history: true,
    history_shortcut: Some(Hotkey::HISTORY_DEFAULT),
    skip_private: true,
});

pub fn current() -> Settings {
    *CURRENT.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn set_current(settings: Settings) {
    *CURRENT.lock().unwrap_or_else(|p| p.into_inner()) = settings;
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
        let (key, value) = (key.trim(), value.trim());
        if key.eq_ignore_ascii_case("shortcut") {
            out.shortcut = shortcut_value(value, Hotkey::DEFAULT);
        } else if key.eq_ignore_ascii_case("history_shortcut") {
            out.history_shortcut = shortcut_value(value, Hotkey::HISTORY_DEFAULT);
        } else if key.eq_ignore_ascii_case("history") {
            out.history = switch_value(value).unwrap_or(true);
        } else if key.eq_ignore_ascii_case("skip_private") {
            out.skip_private = switch_value(value).unwrap_or(true);
        }
    }
    out
}

/// A shortcut value. A shortcut we cannot read, or one that is not safe to
/// grab globally (hand-edited file), is the "garbled file" case: `default`.
/// An explicit `None` or an empty value means no shortcut.
fn shortcut_value(value: &str, default: Hotkey) -> Option<Hotkey> {
    if value.is_empty() || value.eq_ignore_ascii_case("none") {
        return None;
    }
    Some(
        Hotkey::parse(value)
            .filter(Hotkey::is_safe)
            .unwrap_or(default),
    )
}

/// An on/off value; `None` when it is neither.
fn switch_value(value: &str) -> Option<bool> {
    let is = |names: &[&str]| names.iter().any(|n| value.eq_ignore_ascii_case(n));
    if is(&["on", "true", "yes", "1"]) {
        Some(true)
    } else if is(&["off", "false", "no", "0"]) {
        Some(false)
    } else {
        None
    }
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

fn shortcut_text(shortcut: Option<Hotkey>) -> String {
    match shortcut {
        Some(hk) => hk.text(),
        None => "None".to_string(),
    }
}

fn switch_text(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

pub fn render(settings: &Settings) -> String {
    format!(
        "# Tasca settings (hand-edited: the app rewrites this file on OK)\nshortcut={}\nhistory={}\nhistory_shortcut={}\nskip_private={}\n",
        shortcut_text(settings.shortcut),
        switch_text(settings.history),
        shortcut_text(settings.history_shortcut),
        switch_text(settings.skip_private),
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
            Settings {
                shortcut: None,
                ..Settings::default()
            },
            Settings {
                shortcut: Hotkey::parse("Ctrl+Shift+F13"),
                ..Settings::default()
            },
            Settings {
                shortcut: Hotkey::parse("Win+Alt+Num3"),
                history: false,
                history_shortcut: None,
                skip_private: false,
            },
            Settings {
                history_shortcut: Hotkey::parse("Ctrl+Alt+H"),
                ..Settings::default()
            },
        ] {
            assert_eq!(parse(&render(&settings)), settings);
        }
    }

    #[test]
    fn garbled_values_fall_back_to_the_default() {
        assert_eq!(parse("shortcut=\n").shortcut, None);
        assert_eq!(parse("shortcut=None\n").shortcut, None);
        assert_eq!(parse("shortcut=none\n").shortcut, None);
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
                shortcut: Hotkey::parse("Ctrl+Alt+E"),
                ..Settings::default()
            }
        );
    }

    #[test]
    fn the_history_keys_default_to_on_alt_v_and_skipping() {
        let d = Settings::default();
        assert!(d.history && d.skip_private);
        assert_eq!(d.history_shortcut, Hotkey::parse("Alt+V"));
        let s = parse("shortcut=Ctrl+Alt+E\n");
        assert_eq!((s.history, s.history_shortcut, s.skip_private), (d.history, d.history_shortcut, d.skip_private));
    }

    #[test]
    fn the_history_keys_parse() {
        let s = parse("history=off\nhistory_shortcut=Ctrl+Alt+H\nskip_private=no\n");
        assert!(!s.history && !s.skip_private);
        assert_eq!(s.history_shortcut, Hotkey::parse("Ctrl+Alt+H"));
        let s = parse("HISTORY = ON\nskip_private=1\nhistory_shortcut=none\n");
        assert!(s.history && s.skip_private);
        assert_eq!(s.history_shortcut, None);
        assert_eq!(parse("history_shortcut=\n").history_shortcut, None);
    }

    #[test]
    fn garbled_history_values_fall_back_to_the_defaults() {
        for text in [
            "history=maybe\n",
            "history=\n",
            "history=\u{0}\n",
            "skip_private=2\n",
            "skip_private=\n",
            "history_shortcut=Hyper+Q\n",
            "history_shortcut=V\n",
            "history_shortcut=Ctrl+C\n",
            "history_shortcut=Alt+Tab\n",
        ] {
            assert_eq!(parse(text), Settings::default(), "{text:?}");
        }
        // A garbled value does not undo an earlier good one's neighbours.
        let s = parse("history=off\nskip_private=maybe\n");
        assert!(!s.history && s.skip_private);
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
