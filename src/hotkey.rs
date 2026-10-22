//! The configurable global shortcut: text form (`Ctrl+Alt+D`), the key-name
//! table the settings recorder uses, and `RegisterHotKey` on the resident's UI
//! thread. Pure parsing/serialising lives at the top; no Win32 there.

use std::cell::RefCell;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};

use crate::log::log;

/// RegisterHotKey id. Only this process uses it.
pub const REGISTER_ID: i32 = 0x7A51;

/// Error the shell returns when another app already owns the combination
/// (`ERROR_HOTKEY_ALREADY_REGISTERED`, 1409).
const HOTKEY_TAKEN: u32 = 1409;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// A `VK_*` key, never one of the modifier keys themselves.
    pub vk: u32,
}

impl Hotkey {
    /// The spec's default: Ctrl+Alt+D.
    pub const DEFAULT: Hotkey = Hotkey {
        ctrl: true,
        alt: true,
        shift: false,
        win: false,
        vk: b'D' as u32,
    };

    pub fn modifiers(&self) -> HOT_KEY_MODIFIERS {
        let mut m = MOD_NOREPEAT;
        if self.ctrl {
            m |= MOD_CONTROL;
        }
        if self.alt {
            m |= MOD_ALT;
        }
        if self.shift {
            m |= MOD_SHIFT;
        }
        if self.win {
            m |= MOD_WIN;
        }
        m
    }

    /// Canonical text, e.g. `Ctrl+Alt+Shift+Win+D`.
    pub fn text(&self) -> String {
        let mut parts = Vec::with_capacity(5);
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.win {
            parts.push("Win".to_string());
        }
        if let Some(name) = key_name(self.vk) {
            parts.push(name);
        }
        parts.join("+")
    }

    /// `None` for anything that is not a usable shortcut (unknown key, a
    /// modifier on its own, more than one non-modifier key, empty text).
    pub fn parse(text: &str) -> Option<Hotkey> {
        let (mut ctrl, mut alt, mut shift, mut win) = (false, false, false, false);
        let mut vk = None;
        for part in text.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "alt" => alt = true,
                "shift" => shift = true,
                "win" | "windows" | "meta" => win = true,
                _ => {
                    if vk.is_some() {
                        return None;
                    }
                    vk = Some(key_vk(part)?);
                }
            }
        }
        Some(Hotkey {
            ctrl,
            alt,
            shift,
            win,
            vk: vk?,
        })
    }

    /// Whether the recorder should accept this combination. A bare letter
    /// would be grabbed globally while Tasca runs, so require a modifier;
    /// F13..F24 are dedicated macro keys and are fine on their own.
    pub fn is_safe(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.win || (0x7C..=0x87).contains(&self.vk)
    }
}

/// Keys the recorder can name, and their `VK_*` values. Order matters only for
/// lookup: the first name for a code wins.
const NAMED_KEYS: &[(&str, u32)] = &[
    ("Space", 0x20),
    ("Tab", 0x09),
    ("Enter", 0x0D),
    ("Esc", 0x1B),
    ("Backspace", 0x08),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("PrintScreen", 0x2C),
    ("ScrollLock", 0x91),
    ("Pause", 0x13),
    ("CapsLock", 0x14),
    ("NumLock", 0x90),
    ("Multiply", 0x6A),
    ("Add", 0x6B),
    ("Subtract", 0x6D),
    ("Decimal", 0x6E),
    ("Divide", 0x6F),
    ("Num0", 0x60),
    ("Num1", 0x61),
    ("Num2", 0x62),
    ("Num3", 0x63),
    ("Num4", 0x64),
    ("Num5", 0x65),
    ("Num6", 0x66),
    ("Num7", 0x67),
    ("Num8", 0x68),
    ("Num9", 0x69),
    ("Semicolon", 0xBA),
    ("Plus", 0xBB),
    ("Comma", 0xBC),
    ("Minus", 0xBD),
    ("Period", 0xBE),
    ("Slash", 0xBF),
    ("Backtick", 0xC0),
    ("LeftBracket", 0xDB),
    ("Backslash", 0xDC),
    ("RightBracket", 0xDD),
    ("Quote", 0xDE),
];

/// The canonical name of a virtual key, if the recorder supports it.
pub fn key_name(vk: u32) -> Option<String> {
    if (b'A' as u32..=b'Z' as u32).contains(&vk) || (b'0' as u32..=b'9' as u32).contains(&vk) {
        return char::from_u32(vk).map(|c| c.to_string());
    }
    if (0x70..=0x87).contains(&vk) {
        return Some(format!("F{}", vk - 0x6F));
    }
    NAMED_KEYS
        .iter()
        .find(|(_, code)| *code == vk)
        .map(|(name, _)| (*name).to_string())
}

/// The virtual key for a recorded key name.
pub fn key_vk(name: &str) -> Option<u32> {
    let name = name.trim();
    if name.chars().count() == 1 {
        let c = name.chars().next()?.to_ascii_uppercase();
        if c.is_ascii_alphanumeric() {
            return Some(c as u32);
        }
    }
    if let Some(rest) = name
        .to_ascii_uppercase()
        .strip_prefix('F')
        .map(str::to_string)
    {
        if let Ok(n) = rest.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Some(0x6F + n);
            }
        }
    }
    NAMED_KEYS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, code)| *code)
}

#[derive(Default)]
struct State {
    /// Registered right now (None when cleared or when registration failed).
    active: Option<Hotkey>,
    /// What the user chose: what the tray tooltip and settings window show.
    shown: Option<Hotkey>,
    error: Option<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// The shortcut to display and the reason it is not active, if any.
pub fn display() -> (Option<Hotkey>, Option<String>) {
    with(|s| (s.shown, s.error.clone()))
}

/// Resident startup: register the saved shortcut.
pub fn start(hwnd: HWND, desired: Option<Hotkey>) -> Result<(), String> {
    let result = match desired {
        Some(hk) => register(hwnd, hk),
        None => Ok(()),
    };
    match &result {
        Ok(()) => {
            with(|s| {
                s.active = desired;
                s.shown = desired;
                s.error = None;
            });
            match desired {
                Some(hk) => log(format!("hotkey registered: {}", hk.text())),
                None => log("hotkey: none configured"),
            }
        }
        Err(e) => {
            with(|s| {
                s.active = None;
                s.shown = desired;
                s.error = Some(e.clone());
            });
            log(format!("hotkey registration failed: {e}"));
        }
    }
    result
}

/// Settings OK: switch to `desired` now. On failure the previously registered
/// shortcut is put back, so a taken combination never leaves Tasca with none.
pub fn change(hwnd: HWND, desired: Option<Hotkey>) -> Result<(), String> {
    if with(|s| s.active == desired && s.error.is_none()) {
        with(|s| s.shown = desired);
        return Ok(());
    }
    let (old, old_shown, old_error) = with(|s| (s.active, s.shown, s.error.clone()));
    if old.is_some() {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), REGISTER_ID);
        }
    }
    let result = match desired {
        Some(hk) => register(hwnd, hk),
        None => Ok(()),
    };
    match &result {
        Ok(()) => {
            with(|s| {
                s.active = desired;
                s.shown = desired;
                s.error = None;
            });
            match desired {
                Some(hk) => log(format!("hotkey changed: {}", hk.text())),
                None => log("hotkey cleared"),
            }
        }
        Err(e) => {
            if let Some(old) = old {
                if register(hwnd, old).is_err() {
                    log("hotkey: could not restore the previous shortcut");
                }
            }
            // Leave the last good state (and a startup failure's message)
            // exactly as it was: the dialog shows this attempt's error.
            with(|s| {
                s.active = old;
                s.shown = old_shown;
                s.error = old_error;
            });
            log(format!("hotkey change failed: {e}"));
        }
    }
    result
}

fn register(hwnd: HWND, hk: Hotkey) -> Result<(), String> {
    unsafe { RegisterHotKey(Some(hwnd), REGISTER_ID, hk.modifiers(), hk.vk) }
        .map_err(|e| describe(hk, &e))
}

/// Release the shortcut when the resident quits.
pub fn unregister(hwnd: HWND) {
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), REGISTER_ID);
    }
    with(|s| {
        s.active = None;
        s.shown = None;
        s.error = None;
    });
}

fn describe(hk: Hotkey, e: &windows::core::Error) -> String {
    let raw = e.code().0 as u32;
    if raw & 0xffff == HOTKEY_TAKEN {
        format!("{} is already used by another app", hk.text())
    } else {
        format!("{} could not be registered ({raw:#010x})", hk.text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_ctrl_alt_d() {
        assert_eq!(Hotkey::DEFAULT.text(), "Ctrl+Alt+D");
        assert_eq!(Hotkey::parse("Ctrl+Alt+D"), Some(Hotkey::DEFAULT));
        assert!(Hotkey::DEFAULT.is_safe());
    }

    #[test]
    fn text_round_trips_every_supported_key() {
        let mut keys: Vec<u32> = (b'A' as u32..=b'Z' as u32)
            .chain(b'0' as u32..=b'9' as u32)
            .collect();
        keys.extend(NAMED_KEYS.iter().map(|(_, vk)| *vk));
        keys.extend(0x70..=0x87);
        assert_eq!(keys.len(), 26 + 10 + NAMED_KEYS.len() + 24);
        for vk in keys {
            for (ctrl, alt, shift, win) in [
                (false, false, false, false),
                (true, false, false, false),
                (false, true, true, false),
                (true, true, false, true),
                (true, true, true, true),
            ] {
                let hk = Hotkey {
                    ctrl,
                    alt,
                    shift,
                    win,
                    vk,
                };
                let text = hk.text();
                assert!(!text.is_empty());
                assert_eq!(Hotkey::parse(&text), Some(hk), "{text}");
            }
        }
    }

    #[test]
    fn canonical_modifier_order_and_case() {
        let a = Hotkey::parse("win+SHIFT+alt+ctrl+d").unwrap();
        assert_eq!(a.text(), "Ctrl+Alt+Shift+Win+D");
        assert_eq!(Hotkey::parse("control + alt + d"), Some(Hotkey::DEFAULT));
        assert_eq!(Hotkey::parse("Ctrl+Alt+D").unwrap().vk, 0x44);
        assert_eq!(Hotkey::parse("Ctrl+Alt+Num3").unwrap().vk, 0x63);
        assert_eq!(Hotkey::parse("Ctrl+F24").unwrap().vk, 0x87);
        assert_eq!(Hotkey::parse("Ctrl+Semicolon").unwrap().vk, 0xBA);
    }

    #[test]
    fn garbage_is_rejected() {
        for text in [
            "",
            "Ctrl",
            "Ctrl+Alt",
            "Ctrl+Alt+",
            "Hyper+D",
            "Ctrl+Alt+D+E",
            "Ctrl+Alt++",
        ] {
            assert_eq!(Hotkey::parse(text), None, "{text:?}");
        }
        assert_eq!(Hotkey::parse("Ctrl+Alt+F25"), None);
    }

    #[test]
    fn only_dedicated_keys_need_no_modifier() {
        assert!(!Hotkey::parse("D").unwrap().is_safe());
        assert!(Hotkey::parse("F13").unwrap().is_safe());
        assert!(Hotkey::parse("Shift+D").unwrap().is_safe());
        assert!(Hotkey::parse("Win+F").unwrap().is_safe());
    }

    #[test]
    fn modifiers_map_to_win32_flags() {
        let all = Hotkey {
            ctrl: true,
            alt: true,
            shift: true,
            win: true,
            vk: 0x44,
        };
        let m = all.modifiers();
        assert!(m.contains(MOD_CONTROL));
        assert!(m.contains(MOD_ALT));
        assert!(m.contains(MOD_SHIFT));
        assert!(m.contains(MOD_WIN));
        assert!(m.contains(MOD_NOREPEAT));
        assert!(!Hotkey::DEFAULT.modifiers().contains(MOD_WIN));
    }
}
