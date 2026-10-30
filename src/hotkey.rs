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

    /// The History's shortcut until Settings can change it: Alt+V.
    pub const HISTORY_DEFAULT: Hotkey = Hotkey {
        ctrl: false,
        alt: true,
        shift: false,
        win: false,
        vk: b'V' as u32,
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

    /// Why this combination must not be a global shortcut, or `None` when it
    /// is fine. One rule for the recorder, `settings.ini` and registration:
    /// every key except F13..F24 needs Ctrl, Alt or Win (Shift alone would grab
    /// ordinary typing), and a few Windows/editing chords are never allowed.
    pub fn unsafe_reason(&self) -> Option<String> {
        if (0x7C..=0x87).contains(&self.vk) {
            return None;
        }
        if !(self.ctrl || self.alt || self.win) {
            return Some(NEED_MODIFIER.to_string());
        }
        let only_ctrl = self.ctrl && !(self.alt || self.shift || self.win);
        let only_alt = self.alt && !(self.ctrl || self.shift || self.win);
        // Alt+F4, Alt+Tab, Alt+Space; Ctrl+C, V, X, Z, A, S.
        let reserved = (only_alt && matches!(self.vk, 0x73 | 0x09 | 0x20))
            || (only_ctrl && matches!(self.vk, 0x43 | 0x56 | 0x58 | 0x5A | 0x41 | 0x53));
        reserved.then(|| {
            format!(
                "{} is reserved by Windows or editing. Pick another.",
                self.text()
            )
        })
    }

    pub fn is_safe(&self) -> bool {
        self.unsafe_reason().is_none()
    }
}

/// Shown by the recorder when no Ctrl, Alt or Win is held.
pub const NEED_MODIFIER: &str =
    "Hold Ctrl, Alt or Win too (Shift alone is not enough), or use F13\u{2013}F24.";

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

/// The two global shortcuts: the Shelf's and the History's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    Shelf,
    History,
}

impl Slot {
    pub const ALL: [Slot; 2] = [Slot::Shelf, Slot::History];

    /// The `RegisterHotKey` id of this shortcut; `WM_HOTKEY`'s `wParam`.
    pub const fn id(self) -> i32 {
        match self {
            Slot::Shelf => REGISTER_ID,
            Slot::History => REGISTER_ID + 1,
        }
    }

    /// Which shortcut a `WM_HOTKEY` id belongs to.
    pub fn from_id(id: i32) -> Option<Slot> {
        Slot::ALL.into_iter().find(|s| s.id() == id)
    }

    fn index(self) -> usize {
        self as usize
    }

    fn other(self) -> Slot {
        match self {
            Slot::Shelf => Slot::History,
            Slot::History => Slot::Shelf,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Slot::Shelf => "shelf",
            Slot::History => "history",
        }
    }
}

/// Why `desired` cannot be this shortcut: unsafe, or the same chord as the
/// other shortcut (`other`). Pure; used by `start`, `change` and the tests.
pub fn refusal(slot: Slot, desired: Option<Hotkey>, other: Option<Hotkey>) -> Option<String> {
    let hk = desired?;
    if let Some(reason) = hk.unsafe_reason() {
        return Some(reason);
    }
    (other == Some(hk)).then(|| {
        format!(
            "{} is already the {} shortcut. Pick another.",
            hk.text(),
            slot.other().name()
        )
    })
}

#[derive(Default, Clone)]
struct State {
    /// Registered right now (None when cleared or when registration failed).
    active: Option<Hotkey>,
    /// What the user chose: what the tray tooltip and settings window show.
    shown: Option<Hotkey>,
    error: Option<String>,
}

thread_local! {
    static STATE: RefCell<[State; 2]> = RefCell::new(Default::default());
}

fn with<R>(slot: Slot, f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()[slot.index()]))
}

/// The shortcut to display and the reason it is not active, if any.
pub fn display(slot: Slot) -> (Option<Hotkey>, Option<String>) {
    with(slot, |s| (s.shown, s.error.clone()))
}

/// Resident startup: register the saved shortcut. An unsafe one falls back to
/// `fallback`, or none if that is refused too.
pub fn start(
    hwnd: HWND,
    slot: Slot,
    desired: Option<Hotkey>,
    fallback: Option<Hotkey>,
) -> Result<(), String> {
    let other = with(slot.other(), |s| s.shown);
    // An unusable shortcut falls back to the default; the fallback itself must
    // pass the same rule, so a clash with the other shortcut ends with none.
    let desired = match refusal(slot, desired, other) {
        Some(reason) => {
            log(format!("hotkey {}: {reason}, using the default", slot.name()));
            fallback.filter(|_| refusal(slot, fallback, other).is_none())
        }
        None => desired,
    };
    let result = match desired {
        Some(hk) => register(hwnd, slot, hk),
        None => Ok(()),
    };
    match &result {
        Ok(()) => {
            with(slot, |s| {
                s.active = desired;
                s.shown = desired;
                s.error = None;
            });
            match desired {
                Some(hk) => log(format!("hotkey {} registered: {}", slot.name(), hk.text())),
                None => log(format!("hotkey {}: none configured", slot.name())),
            }
        }
        Err(e) => {
            with(slot, |s| {
                s.active = None;
                s.shown = desired;
                s.error = Some(e.clone());
            });
            log(format!("hotkey {} registration failed: {e}", slot.name()));
        }
    }
    result
}

/// Registration failed again for the shortcut the user already had (chosen, but
/// never registered): nothing changed, so the apply that carries it must not
/// fail (Mattia still runs ClipClip, which holds Alt+V).
fn keeps_failing(old_active: Option<Hotkey>, old_shown: Option<Hotkey>, desired: Option<Hotkey>) -> bool {
    desired.is_some() && old_active.is_none() && old_shown == desired
}

/// A slot's whole state, to put back after a failed Settings apply.
#[derive(Clone)]
pub struct Snapshot(State);

pub fn snapshot(slot: Slot) -> Snapshot {
    Snapshot(with(slot, |s| s.clone()))
}

/// Put `slot` back exactly as `snapshot` saw it: re-register only the old
/// `active` shortcut (never the chosen-but-failed one), keep `shown` and the error.
pub fn restore(hwnd: HWND, slot: Slot, snap: Snapshot) {
    let Snapshot(mut old) = snap;
    let now = with(slot, |s| s.active);
    if now != old.active {
        if now.is_some() {
            unsafe {
                let _ = UnregisterHotKey(Some(hwnd), slot.id());
            }
        }
        if let Some(hk) = old.active {
            if let Err(e) = register(hwnd, slot, hk) {
                log(format!("hotkey {}: could not restore {}: {e}", slot.name(), hk.text()));
                old.active = None;
                old.error = Some(e);
            }
        }
    }
    with(slot, |s| *s = old);
}

/// Settings OK: switch to `desired` now. On failure the previously registered
/// shortcut is put back, so a taken combination never leaves Tasca with none.
pub fn change(hwnd: HWND, slot: Slot, desired: Option<Hotkey>) -> Result<(), String> {
    let other = with(slot.other(), |s| s.shown);
    if let Some(reason) = refusal(slot, desired, other) {
        return Err(reason);
    }
    if with(slot, |s| s.active == desired && s.error.is_none()) {
        with(slot, |s| s.shown = desired);
        return Ok(());
    }
    let (old, old_shown, old_error) = with(slot, |s| (s.active, s.shown, s.error.clone()));
    if old.is_some() {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), slot.id());
        }
    }
    let result = match desired {
        Some(hk) => register(hwnd, slot, hk),
        None => Ok(()),
    };
    match &result {
        Ok(()) => {
            with(slot, |s| {
                s.active = desired;
                s.shown = desired;
                s.error = None;
            });
            match desired {
                Some(hk) => log(format!("hotkey {} changed: {}", slot.name(), hk.text())),
                None => log(format!("hotkey {} cleared", slot.name())),
            }
        }
        Err(e) => {
            // Put the previous shortcut back; if that fails too, nothing is
            // registered and the tooltip must say so.
            let restored = match old {
                Some(old) => register(hwnd, slot, old),
                None => Ok(()),
            };
            match restored {
                // The last good state (and a startup failure's message) as it
                // was: the dialog shows this attempt's error.
                Ok(()) => with(slot, |s| {
                    s.active = old;
                    s.shown = old_shown;
                    s.error = old_error;
                }),
                Err(re) => {
                    log(format!(
                        "hotkey {}: could not restore the previous shortcut: {re}",
                        slot.name()
                    ));
                    with(slot, |s| {
                        s.active = None;
                        s.shown = old_shown;
                        s.error = Some(re);
                    });
                }
            }
            log(format!("hotkey {} change failed: {e}", slot.name()));
        }
    }
    if result.is_err() && keeps_failing(old, old_shown, desired) {
        log(format!("hotkey {}: still unavailable, unchanged: not an error for this apply", slot.name()));
        return Ok(());
    }
    result
}

fn register(hwnd: HWND, slot: Slot, hk: Hotkey) -> Result<(), String> {
    unsafe { RegisterHotKey(Some(hwnd), slot.id(), hk.modifiers(), hk.vk) }
        .map_err(|e| describe(hk, &e))
}

/// Release both shortcuts when the resident quits.
pub fn unregister(hwnd: HWND) {
    for slot in Slot::ALL {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), slot.id());
        }
        with(slot, |s| *s = State::default());
    }
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
    fn history_default_is_alt_v_and_safe() {
        assert_eq!(Hotkey::HISTORY_DEFAULT.text(), "Alt+V");
        assert_eq!(Hotkey::parse("Alt+V"), Some(Hotkey::HISTORY_DEFAULT));
        assert!(Hotkey::HISTORY_DEFAULT.is_safe());
        assert_ne!(Hotkey::HISTORY_DEFAULT, Hotkey::DEFAULT);
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
    fn safety_table() {
        let safe = [
            "F13",
            "F24",
            "Shift+F13",
            "Ctrl+Alt+F24",
            "Ctrl+Alt+D",
            "Win+F",
            "Win+Shift+D",
            "Alt+D",
            "Alt+V",
            "Ctrl+D",
            "Ctrl+Shift+C",
            "Ctrl+Shift+V",
            "Ctrl+Alt+C",
            "Ctrl+Alt+S",
            "Ctrl+Shift+Z",
            "Alt+Shift+F4",
            "Ctrl+Alt+F4",
            "Alt+Shift+Tab",
            "Ctrl+Alt+Space",
            "Win+C",
            "Ctrl+Backtick",
        ];
        for text in safe {
            let hk = Hotkey::parse(text).unwrap();
            assert!(hk.is_safe(), "{text} should be allowed");
            assert_eq!(hk.unsafe_reason(), None);
        }
        let rejected = [
            "D",
            "F1",
            "F12",
            "Space",
            "Shift+D",
            "Shift+F1",
            "Shift+Space",
            "Alt+F4",
            "Alt+Tab",
            "Alt+Space",
            "Ctrl+C",
            "Ctrl+V",
            "Ctrl+X",
            "Ctrl+Z",
            "Ctrl+A",
            "Ctrl+S",
        ];
        for text in rejected {
            let hk = Hotkey::parse(text).unwrap();
            assert!(!hk.is_safe(), "{text} should be rejected");
            assert!(hk.unsafe_reason().is_some(), "{text}");
        }
    }

    #[test]
    fn reasons_name_the_problem() {
        let shift_d = Hotkey::parse("Shift+D").unwrap();
        assert_eq!(shift_d.unsafe_reason().as_deref(), Some(NEED_MODIFIER));
        let r = Hotkey::parse("Alt+F4").unwrap().unsafe_reason().unwrap();
        assert!(r.starts_with("Alt+F4 is reserved"), "{r}");
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

    #[test]
    fn slots_have_distinct_ids_that_map_back() {
        assert_ne!(Slot::Shelf.id(), Slot::History.id());
        assert_eq!(Slot::Shelf.id(), REGISTER_ID);
        for slot in Slot::ALL {
            assert_eq!(Slot::from_id(slot.id()), Some(slot));
        }
        assert_eq!(Slot::from_id(0), None);
    }

    #[test]
    fn same_chord_for_both_is_refused() {
        let alt_v = Hotkey::parse("Alt+V");
        let reason = refusal(Slot::History, alt_v, alt_v).unwrap();
        assert_eq!(reason, "Alt+V is already the shelf shortcut. Pick another.");
        let reason = refusal(Slot::Shelf, alt_v, alt_v).unwrap();
        assert!(reason.contains("history shortcut"), "{reason}");
        assert_eq!(refusal(Slot::History, alt_v, Some(Hotkey::DEFAULT)), None);
        assert_eq!(refusal(Slot::History, alt_v, None), None);
        assert_eq!(
            refusal(Slot::History, None, alt_v),
            None,
            "clearing is fine"
        );
    }

    #[test]
    fn unsafe_chord_is_refused_for_either_slot() {
        let shift_d = Hotkey::parse("Shift+D");
        for slot in Slot::ALL {
            assert_eq!(refusal(slot, shift_d, None).as_deref(), Some(NEED_MODIFIER));
        }
    }

    #[test]
    fn an_unchanged_shortcut_that_never_registered_does_not_fail_the_apply() {
        let alt_v = Some(Hotkey::HISTORY_DEFAULT);
        let other = Some(Hotkey::DEFAULT);
        // Chosen at startup (shown), never registered (active none), same again.
        assert!(keeps_failing(None, alt_v, alt_v));
        // A different shortcut that fails is a real error.
        assert!(!keeps_failing(None, alt_v, other));
        // One that was working and now fails to re-register is a real error.
        assert!(!keeps_failing(alt_v, alt_v, alt_v));
        // Clearing never fails.
        assert!(!keeps_failing(None, alt_v, None));
    }
}
