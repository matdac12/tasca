//! The pure History state machine: events in, effects out. It owns no window,
//! clipboard, file or thread; the shell (`mod.rs`) feeds it events and carries
//! out the effects it returns.

use super::clip::{Clip, ClipId};
use super::search;

/// A single copy larger than this is skipped (spec: 10 MB).
const MAX_CLIP_BYTES: usize = 10 * 1024 * 1024;

/// The window a paste goes back to (an `HWND` as an integer, so this stays pure).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Target(pub isize);

/// What one clipboard update offered, as read by `record`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Capture {
    /// `GetClipboardSequenceNumber` at read time.
    pub seq: u32,
    /// `CF_UNICODETEXT`, if the copy offered it.
    pub text: Option<String>,
    /// The clipboard's owner was the History window: one of our own writes.
    /// The sequence number alone is not enough (the update can report a later
    /// number than the one taken at write time).
    pub own: bool,
}

/// The keys the History acts on. Typing arrives as `Event::Char`, not here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    PageUp,
    PageDown,
    Enter,
    Esc,
    Backspace,
    /// 1..=9 on the main row; acts only with Alt or Ctrl (a bare digit is typed).
    Digit(u8),
}

/// Modifiers held when a key went down.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mods {
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
}

/// Rows a PgUp / PgDn moves (the mockup's).
const PAGE: usize = 5;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
    Copied(Capture),
    /// Our own clipboard write finished; its sequence number is not a copy.
    OwnWrite(u32),
    Key(Key, Mods),
    /// Typed text (a printable character).
    Char(char),
    /// A left click on a row of the shown list.
    Click { row: usize, shift: bool },
    /// The History was opened; `None` when there is no app to paste into.
    Opened(Option<Target>),
    /// The window lost activation, or Alt+V was pressed again.
    Deactivated,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Skip {
    /// Same sequence number as the update handled last.
    SameUpdate,
    /// Tasca's own paste-back write.
    OwnWrite,
    TooBig,
    /// No text (or blank text).
    NothingRecordable,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Effect {
    Add(ClipId),
    MoveToTop(ClipId),
    Skip(Skip),
    /// Write the Clip to the clipboard, give focus to `target`, send Ctrl+V.
    /// `plain`: write only its plain text.
    Paste { clip: Clip, target: Target, plain: bool },
    /// No app to paste into: only write the Clip to the clipboard.
    Copy { clip: Clip, plain: bool },
    Close,
    Redraw,
}

#[derive(Default)]
pub struct Machine {
    /// Newest first.
    clips: Vec<Clip>,
    next_id: u64,
    /// Index into the shown list.
    selected: usize,
    /// The search text; the caret is always at its end.
    query: String,
    open: bool,
    target: Option<Target>,
    last_seq: Option<u32>,
    own_seq: Option<u32>,
}

impl Machine {
    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    /// The Clips on show: all of them, or those matching the search. Numbers
    /// 1..9 and `selected` count in this list.
    pub fn shown(&self) -> Vec<&Clip> {
        let words = search::words(&self.query);
        self.clips.iter().filter(|c| search::matches(&words, &c.text)).collect()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// `now_ms` is the shell's clock (ms since the Unix epoch); the machine
    /// only stamps Clips with it.
    pub fn step(&mut self, event: Event, now_ms: u64) -> Vec<Effect> {
        match event {
            Event::Copied(capture) => self.copied(capture, now_ms),
            Event::OwnWrite(seq) => {
                self.own_seq = Some(seq);
                Vec::new()
            }
            Event::Opened(target) => {
                self.open = true;
                self.target = target;
                self.selected = 0;
                self.query.clear();
                vec![Effect::Redraw]
            }
            Event::Key(key, mods) if self.open => self.key(key, mods, now_ms),
            Event::Char(c) if self.open && !c.is_control() => {
                self.query.push(c);
                self.edited()
            }
            Event::Click { row, shift } if self.open => self.paste_row(row, shift, now_ms),
            Event::Key(..) | Event::Char(_) | Event::Click { .. } => Vec::new(),
            Event::Deactivated => self.close(),
        }
    }

    fn copied(&mut self, capture: Capture, now_ms: u64) -> Vec<Effect> {
        if self.last_seq == Some(capture.seq) {
            return vec![Effect::Skip(Skip::SameUpdate)];
        }
        self.last_seq = Some(capture.seq);
        if capture.own || self.own_seq == Some(capture.seq) {
            return vec![Effect::Skip(Skip::OwnWrite)];
        }
        let text = match capture.text {
            Some(t) if !t.trim().is_empty() => t,
            _ => return vec![Effect::Skip(Skip::NothingRecordable)],
        };
        if text.len() > MAX_CLIP_BYTES {
            return vec![Effect::Skip(Skip::TooBig)];
        }
        let mut effects = Vec::new();
        let selected_id = self.shown().get(self.selected).map(|c| c.id);
        match self.clips.iter().position(|c| c.text == text) {
            Some(i) => {
                let mut clip = self.clips.remove(i);
                clip.last_copied_ms = now_ms;
                effects.push(Effect::MoveToTop(clip.id));
                self.clips.insert(0, clip);
            }
            None => {
                let id = ClipId(self.next_id);
                self.next_id += 1;
                self.clips.insert(0, Clip { id, text, last_copied_ms: now_ms });
                effects.push(Effect::Add(id));
            }
        }
        if self.open {
            // The highlight stays on the same Clip when the list shifts.
            if let Some(id) = selected_id {
                self.selected = self.shown().iter().position(|c| c.id == id).unwrap_or(0);
            }
            effects.push(Effect::Redraw);
        }
        effects
    }

    fn key(&mut self, key: Key, mods: Mods, now_ms: u64) -> Vec<Effect> {
        let last = self.shown().len().saturating_sub(1);
        match key {
            Key::Up => self.select(self.selected.saturating_sub(1)),
            Key::Down => self.select((self.selected + 1).min(last)),
            Key::PageUp => self.select(self.selected.saturating_sub(PAGE)),
            Key::PageDown => self.select((self.selected + PAGE).min(last)),
            Key::Enter if self.shown().is_empty() => {
                // Nothing to paste: an empty History closes, a search with no match stays.
                if self.query.is_empty() { self.close() } else { Vec::new() }
            }
            Key::Enter => self.paste_row(self.selected, mods.shift, now_ms),
            Key::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.edited()
            }
            Key::Esc => self.close(),
            Key::Backspace => {
                if mods.ctrl {
                    search::delete_word(&mut self.query);
                } else {
                    self.query.pop();
                }
                self.edited()
            }
            Key::Digit(n) if (mods.alt || mods.ctrl) && (1..=9).contains(&n) => {
                self.paste_row(n as usize - 1, false, now_ms)
            }
            Key::Digit(_) => Vec::new(),
        }
    }

    /// Move the selection; redraw only when it moved.
    fn select(&mut self, to: usize) -> Vec<Effect> {
        if to == self.selected {
            return Vec::new();
        }
        self.selected = to;
        vec![Effect::Redraw]
    }

    /// The search text changed: the first row is selected again.
    fn edited(&mut self) -> Vec<Effect> {
        self.selected = 0;
        vec![Effect::Redraw]
    }

    /// Paste row `row` of the shown list (nothing when there is no such row):
    /// the Clip moves to the top and is pasted. Closing comes before the paste
    /// so the window is gone when focus goes back.
    fn paste_row(&mut self, row: usize, plain: bool, now_ms: u64) -> Vec<Effect> {
        let Some(id) = self.shown().get(row).map(|c| c.id) else { return Vec::new() };
        let Some(i) = self.clips.iter().position(|c| c.id == id) else { return Vec::new() };
        let mut clip = self.clips.remove(i);
        clip.last_copied_ms = now_ms;
        self.clips.insert(0, clip.clone());
        let mut effects = vec![Effect::MoveToTop(clip.id)];
        effects.extend(self.close());
        effects.push(match self.target {
            Some(target) => Effect::Paste { clip, target, plain },
            None => Effect::Copy { clip, plain },
        });
        effects
    }

    /// Nothing happens when already closed: hiding the window raises its own
    /// deactivation, which must not close twice.
    fn close(&mut self) -> Vec<Effect> {
        if !self.open {
            return Vec::new();
        }
        self.open = false;
        vec![Effect::Close]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `step` at a fixed time, for the tests that do not care about it.
    impl Machine {
        fn go(&mut self, event: Event) -> Vec<Effect> {
            self.step(event, 1_000)
        }
    }

    fn copy(seq: u32, text: &str) -> Event {
        Event::Copied(Capture { seq, text: Some(text.to_string()), own: false })
    }

    fn texts(m: &Machine) -> Vec<&str> {
        m.clips().iter().map(|c| c.text.as_str()).collect()
    }

    fn with(items: &[&str]) -> Machine {
        let mut m = Machine::default();
        for (i, t) in items.iter().enumerate() {
            m.go(copy(i as u32 + 1, t));
        }
        m
    }

    const APP: Target = Target(0x1234);

    fn press(key: Key) -> Event {
        Event::Key(key, Mods::default())
    }

    fn chord(key: Key, alt: bool, ctrl: bool, shift: bool) -> Event {
        Event::Key(key, Mods { alt, ctrl, shift })
    }

    fn type_text(m: &mut Machine, text: &str) {
        for c in text.chars() {
            m.go(Event::Char(c));
        }
    }

    /// The shown list as texts.
    fn shown(m: &Machine) -> Vec<&str> {
        m.shown().iter().map(|c| c.text.as_str()).collect()
    }

    /// An open History (with a paste target) holding `items`, the last one newest.
    fn open_with(items: &[&str]) -> Machine {
        let mut m = with(items);
        m.go(Event::Opened(Some(APP)));
        m
    }

    #[test]
    fn a_text_clip_is_added_on_top() {
        let mut m = Machine::default();
        let fx = m.go(copy(1, "one"));
        assert!(matches!(fx.as_slice(), [Effect::Add(_)]));
        m.go(copy(2, "two"));
        assert_eq!(texts(&m), ["two", "one"]);
    }

    #[test]
    fn a_duplicate_moves_to_the_top_instead_of_being_added() {
        let mut m = with(&["a", "b", "c"]);
        let id = m.clips()[2].id;
        let fx = m.go(copy(9, "a"));
        assert_eq!(fx, [Effect::MoveToTop(id)]);
        assert_eq!(texts(&m), ["a", "c", "b"]);
    }

    #[test]
    fn our_own_write_is_skipped() {
        let mut m = with(&["a"]);
        m.go(Event::OwnWrite(7));
        assert_eq!(m.go(copy(7, "b")), [Effect::Skip(Skip::OwnWrite)]);
        assert_eq!(texts(&m), ["a"]);
        // The next real copy is recorded.
        m.go(copy(8, "b"));
        assert_eq!(texts(&m), ["b", "a"]);
    }

    #[test]
    fn a_copy_owned_by_the_history_window_is_skipped_whatever_its_sequence_number() {
        let mut m = with(&["a"]);
        m.go(Event::OwnWrite(7));
        // The update reports a later number than the one taken at write time.
        let ours = Capture { seq: 9, text: Some("b".into()), own: true };
        assert_eq!(m.go(Event::Copied(ours)), [Effect::Skip(Skip::OwnWrite)]);
        assert_eq!(texts(&m), ["a"]);
    }

    #[test]
    fn the_same_sequence_number_is_handled_once() {
        let mut m = Machine::default();
        m.go(copy(3, "a"));
        assert_eq!(m.go(copy(3, "a")), [Effect::Skip(Skip::SameUpdate)]);
        assert_eq!(m.clips().len(), 1);
    }

    #[test]
    fn blank_missing_and_huge_text_is_not_recorded() {
        let mut m = Machine::default();
        assert_eq!(m.go(Event::Copied(Capture { seq: 1, text: None, own: false })), [Effect::Skip(Skip::NothingRecordable)]);
        assert_eq!(m.go(copy(2, " \r\n\t")), [Effect::Skip(Skip::NothingRecordable)]);
        assert_eq!(m.go(copy(3, &"x".repeat(MAX_CLIP_BYTES + 1))), [Effect::Skip(Skip::TooBig)]);
        assert!(m.clips().is_empty());
    }

    #[test]
    fn the_first_row_is_selected_on_open() {
        let mut m = with(&["a", "b", "c"]);
        m.go(Event::Opened(Some(APP)));
        m.go(press(Key::Down));
        m.go(press(Key::Esc));
        m.go(Event::Opened(Some(APP)));
        assert!(m.is_open());
        assert_eq!(m.selected(), 0);
    }

    #[test]
    fn up_and_down_move_the_selection_and_stop_at_the_ends() {
        let mut m = with(&["a", "b", "c"]);
        m.go(Event::Opened(Some(APP)));
        assert!(m.go(press(Key::Up)).is_empty());
        assert_eq!(m.go(press(Key::Down)), [Effect::Redraw]);
        m.go(press(Key::Down));
        assert_eq!(m.selected(), 2);
        assert!(m.go(press(Key::Down)).is_empty());
        m.go(press(Key::Up));
        assert_eq!(m.selected(), 1);
    }

    #[test]
    fn enter_pastes_the_selected_clip_and_moves_it_to_the_top() {
        let mut m = with(&["a", "b", "c"]); // c, b, a
        m.go(Event::Opened(Some(APP)));
        m.go(press(Key::Down));
        let fx = m.go(press(Key::Enter));
        let b = m.clips()[0].clone();
        assert_eq!(b.text, "b");
        assert_eq!(fx, [Effect::MoveToTop(b.id), Effect::Close, Effect::Paste { clip: b, target: APP, plain: false }]);
        assert_eq!(texts(&m), ["b", "c", "a"]);
        assert!(!m.is_open());
    }

    #[test]
    fn enter_without_a_target_only_copies() {
        let mut m = with(&["a"]);
        m.go(Event::Opened(None));
        let fx = m.go(press(Key::Enter));
        assert!(matches!(fx.as_slice(), [Effect::MoveToTop(_), Effect::Close, Effect::Copy { plain: false, .. }]));
    }

    #[test]
    fn enter_on_an_empty_history_just_closes() {
        let mut m = Machine::default();
        m.go(Event::Opened(Some(APP)));
        assert_eq!(m.go(press(Key::Enter)), [Effect::Close]);
    }

    #[test]
    fn esc_and_deactivated_close_once() {
        let mut m = with(&["a"]);
        m.go(Event::Opened(Some(APP)));
        assert_eq!(m.go(press(Key::Esc)), [Effect::Close]);
        // Hiding the window deactivates it: nothing more to do.
        assert!(m.go(Event::Deactivated).is_empty());
        m.go(Event::Opened(Some(APP)));
        assert_eq!(m.go(Event::Deactivated), [Effect::Close]);
        assert!(!m.is_open());
    }

    #[test]
    fn keys_do_nothing_while_closed() {
        let mut m = with(&["a"]);
        assert!(m.go(press(Key::Enter)).is_empty());
        assert!(m.go(press(Key::Down)).is_empty());
    }

    #[test]
    fn a_copy_while_open_redraws_and_keeps_the_selection_valid() {
        let mut m = with(&["a", "b"]);
        m.go(Event::Opened(None));
        m.go(press(Key::Down));
        let fx = m.go(copy(9, "c"));
        assert!(fx.contains(&Effect::Redraw));
        // The list was b, a with "a" selected; the new clip went on top.
        assert_eq!(m.shown()[m.selected()].text, "a");
    }

    #[test]
    fn a_new_clip_remembers_when_it_was_copied() {
        let mut m = Machine::default();
        m.step(copy(1, "a"), 5_000);
        m.step(copy(2, "b"), 9_000);
        assert_eq!(m.clips()[0].last_copied_ms, 9_000);
        assert_eq!(m.clips()[1].last_copied_ms, 5_000);
    }

    #[test]
    fn copying_a_clip_again_refreshes_its_time() {
        let mut m = Machine::default();
        m.step(copy(1, "a"), 5_000);
        m.step(copy(2, "b"), 6_000);
        m.step(copy(3, "a"), 20_000);
        assert_eq!(texts(&m), ["a", "b"]);
        assert_eq!((m.clips()[0].last_copied_ms, m.clips()[1].last_copied_ms), (20_000, 6_000));
    }

    #[test]
    fn pasting_a_clip_moves_it_to_the_top_with_the_time_of_the_paste() {
        let mut m = Machine::default();
        m.step(copy(1, "a"), 5_000);
        m.step(copy(2, "b"), 6_000);
        m.step(Event::Opened(None), 30_000);
        m.step(press(Key::Down), 31_000);
        m.step(press(Key::Enter), 32_000);
        assert_eq!(texts(&m), ["a", "b"]);
        assert_eq!(m.clips()[0].last_copied_ms, 32_000);
        assert_eq!(m.clips()[1].last_copied_ms, 6_000);
    }

    #[test]
    fn typing_filters_at_once_every_word_in_any_order() {
        let mut m = open_with(&["March invoice for Rossi", "lunch with Anna", "invoice April"]);
        type_text(&mut m, "inv march");
        assert_eq!(shown(&m), ["March invoice for Rossi"]);
        assert_eq!(m.query(), "inv march");
        let mut m = open_with(&["March invoice for Rossi"]);
        type_text(&mut m, "ROSSI Inv");
        assert_eq!(shown(&m), ["March invoice for Rossi"]);
    }

    #[test]
    fn typing_redraws_and_selects_the_first_row_again() {
        let mut m = open_with(&["ab", "ac", "ad"]);
        m.go(press(Key::Down));
        m.go(press(Key::Down));
        assert_eq!(m.selected(), 2);
        assert_eq!(m.go(Event::Char('a')), [Effect::Redraw]);
        assert_eq!(m.selected(), 0);
        m.go(press(Key::Down));
        m.go(press(Key::Backspace));
        assert_eq!(m.selected(), 0);
    }

    #[test]
    fn bare_digits_go_into_the_search() {
        let mut m = open_with(&["report 2026", "other", "2026 plan"]);
        type_text(&mut m, "2026");
        assert_eq!(m.query(), "2026");
        assert_eq!(shown(&m), ["2026 plan", "report 2026"]);
        // A digit key without Alt or Ctrl pastes nothing.
        assert!(m.go(press(Key::Digit(2))).is_empty());
        assert!(m.is_open());
    }

    #[test]
    fn control_characters_are_not_typed() {
        let mut m = open_with(&["a"]);
        assert!(m.go(Event::Char('\u{8}')).is_empty());
        assert!(m.go(Event::Char('\u{1b}')).is_empty());
        assert_eq!(m.query(), "");
    }

    #[test]
    fn backspace_deletes_a_character_and_ctrl_backspace_a_word() {
        let mut m = open_with(&["a"]);
        type_text(&mut m, "inv march");
        m.go(press(Key::Backspace));
        assert_eq!(m.query(), "inv marc");
        m.go(chord(Key::Backspace, false, true, false));
        assert_eq!(m.query(), "inv ");
        m.go(chord(Key::Backspace, false, true, false));
        assert_eq!(m.query(), "");
        assert_eq!(m.go(press(Key::Backspace)), [Effect::Redraw]);
    }

    #[test]
    fn the_numbers_follow_the_shown_list() {
        let mut m = open_with(&["apple", "banana", "apricot", "cherry"]);
        type_text(&mut m, "ap");
        assert_eq!(shown(&m), ["apricot", "apple"]);
        let fx = m.go(chord(Key::Digit(2), true, false, false));
        assert!(matches!(fx.last(), Some(Effect::Paste { clip, plain: false, .. }) if clip.text == "apple"));
    }

    #[test]
    fn alt_held_from_the_opening_chord_and_ctrl_both_paste_clip_three() {
        for (alt, ctrl) in [(true, false), (false, true), (true, true)] {
            let mut m = open_with(&["one", "two", "three", "four"]); // four, three, two, one
            let fx = m.go(chord(Key::Digit(3), alt, ctrl, false));
            let two = m.clips()[0].clone();
            assert_eq!(two.text, "two");
            assert_eq!(fx, [Effect::MoveToTop(two.id), Effect::Close, Effect::Paste { clip: two, target: APP, plain: false }]);
            assert!(!m.is_open());
        }
    }

    #[test]
    fn a_number_with_no_such_row_does_nothing() {
        let mut m = open_with(&["a", "b"]);
        assert!(m.go(chord(Key::Digit(3), true, false, false)).is_empty());
        assert!(m.is_open());
        assert_eq!(texts(&m), ["b", "a"]);
    }

    #[test]
    fn the_number_shortcuts_never_paste_plain() {
        let mut m = open_with(&["a", "b"]);
        let fx = m.go(chord(Key::Digit(1), true, false, true));
        assert!(matches!(fx.last(), Some(Effect::Paste { plain: false, .. })));
    }

    #[test]
    fn shift_enter_and_shift_click_paste_plain_text() {
        let mut m = open_with(&["a", "b", "c"]);
        let fx = m.go(chord(Key::Enter, false, false, true));
        assert!(matches!(fx.last(), Some(Effect::Paste { clip, plain: true, .. }) if clip.text == "c"));
        let mut m = open_with(&["a", "b", "c"]);
        let fx = m.go(Event::Click { row: 2, shift: true });
        assert!(matches!(fx.last(), Some(Effect::Paste { clip, plain: true, .. }) if clip.text == "a"));
        let mut m = open_with(&["a", "b", "c"]);
        let fx = m.go(Event::Click { row: 1, shift: false });
        assert!(matches!(fx.last(), Some(Effect::Paste { clip, plain: false, .. }) if clip.text == "b"));
        assert_eq!(texts(&m), ["b", "c", "a"]);
    }

    #[test]
    fn a_click_pastes_the_clicked_row_of_the_filtered_list() {
        let mut m = open_with(&["apple", "banana", "apricot"]);
        type_text(&mut m, "ap");
        let fx = m.go(Event::Click { row: 1, shift: false });
        assert!(matches!(fx.last(), Some(Effect::Paste { clip, .. }) if clip.text == "apple"));
    }

    #[test]
    fn a_click_past_the_last_row_does_nothing() {
        let mut m = open_with(&["a"]);
        assert!(m.go(Event::Click { row: 4, shift: false }).is_empty());
        assert!(m.is_open());
    }

    #[test]
    fn enter_pastes_the_selected_row_of_the_filtered_list() {
        let mut m = open_with(&["apple", "banana", "apricot"]);
        type_text(&mut m, "ap");
        m.go(press(Key::Down));
        let fx = m.go(press(Key::Enter));
        assert!(matches!(fx.last(), Some(Effect::Paste { clip, plain: false, .. }) if clip.text == "apple"));
    }

    #[test]
    fn esc_clears_the_search_then_closes() {
        let mut m = open_with(&["a", "b"]);
        type_text(&mut m, "zz");
        assert!(shown(&m).is_empty());
        assert_eq!(m.go(press(Key::Esc)), [Effect::Redraw]);
        assert_eq!(m.query(), "");
        assert_eq!(shown(&m), ["b", "a"]);
        assert!(m.is_open());
        assert_eq!(m.go(press(Key::Esc)), [Effect::Close]);
        assert!(!m.is_open());
    }

    #[test]
    fn the_search_is_empty_each_time_the_history_opens() {
        let mut m = open_with(&["a"]);
        type_text(&mut m, "a");
        m.go(Event::Deactivated);
        m.go(Event::Opened(Some(APP)));
        assert_eq!(m.query(), "");
    }

    #[test]
    fn enter_with_no_match_keeps_the_search_open() {
        let mut m = open_with(&["a"]);
        type_text(&mut m, "zz");
        assert!(m.go(press(Key::Enter)).is_empty());
        assert!(m.is_open());
    }

    #[test]
    fn page_keys_move_by_a_page_and_stop_at_the_ends() {
        let items: Vec<String> = (0..12).map(|i| format!("clip {i:02}")).collect();
        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
        let mut m = open_with(&refs);
        assert!(m.go(press(Key::PageUp)).is_empty());
        m.go(press(Key::PageDown));
        assert_eq!(m.selected(), 5);
        m.go(press(Key::PageDown));
        m.go(press(Key::PageDown));
        assert_eq!(m.selected(), 11);
        assert!(m.go(press(Key::PageDown)).is_empty());
        m.go(press(Key::PageUp));
        assert_eq!(m.selected(), 6);
    }

    #[test]
    fn a_copy_while_searching_keeps_the_same_clip_selected() {
        let mut m = open_with(&["ax", "ay"]);
        type_text(&mut m, "a");
        m.go(press(Key::Down));
        m.go(copy(9, "az"));
        assert_eq!(m.shown()[m.selected()].text, "ax");
    }

    #[test]
    fn typing_and_clicks_do_nothing_while_closed() {
        let mut m = with(&["a"]);
        assert!(m.go(Event::Char('a')).is_empty());
        assert!(m.go(Event::Click { row: 0, shift: false }).is_empty());
        assert!(m.go(chord(Key::Digit(1), true, false, false)).is_empty());
        assert_eq!(m.query(), "");
    }
}
