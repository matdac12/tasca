//! The pure History state machine: events in, effects out. It owns no window,
//! clipboard, file or thread; the shell (`mod.rs`) feeds it events and carries
//! out the effects it returns.

use super::clip::{Clip, ClipId};
use super::search;
use super::store;

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
    /// P: pins or unpins with Ctrl (without it the letter is typed).
    Pin,
    /// Delete: deletes the selected Clip with Ctrl.
    Delete,
}

/// What the right-click menu can do to a Clip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Paste,
    PastePlain,
    TogglePin,
    Delete,
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
    /// A right-click menu choice. The Clip is named by id: the list can change
    /// while the menu is up.
    ClipAction { id: ClipId, action: Action },
    /// "Clear" in the footer: every unpinned Clip goes, pins stay.
    Clear,
    /// The saved History was read at startup (newest first).
    Loaded(Vec<Clip>),
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
    /// The Clips changed: write them to the store (once it has been read).
    Save,
    /// An action was ignored; the shell logs why.
    Refused(Refusal),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// Pinning would pass `store::MAX_PINNED`.
    TooManyPins,
    /// Delete and Clear wait for the stored History: it would bring them back.
    NotLoaded,
}

#[derive(Default)]
pub struct Machine {
    /// Newest first.
    clips: Vec<Clip>,
    next_id: u64,
    /// Index into the shown list.
    selected: usize,
    /// The store has been read. Until then nothing is saved: a save would
    /// replace the stored History with only what was copied since start.
    loaded: bool,
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
        let mut shown: Vec<&Clip> = self.clips.iter().filter(|c| search::matches(&words, &c.text)).collect();
        // Pinned Clips first; the sort is stable, so each section stays newest first.
        shown.sort_by_key(|c| !c.pinned);
        shown
    }

    /// How many of the shown Clips, from the top, are pinned.
    pub fn shown_pinned(&self) -> usize {
        self.shown().iter().take_while(|c| c.pinned).count()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The saved History has been merged in.
    pub fn is_loaded(&self) -> bool {
        self.loaded
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
            Event::Loaded(saved) => self.loaded(saved),
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
            Event::ClipAction { id, action } if self.open => self.clip_action(id, action, now_ms),
            Event::Clear => self.clear(),
            Event::Key(..) | Event::Char(_) | Event::Click { .. } | Event::ClipAction { .. } => Vec::new(),
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
                self.clips.insert(0, Clip { id, text, last_copied_ms: now_ms, pinned: false });
                effects.push(Effect::Add(id));
            }
        }
        self.trim();
        self.save(&mut effects);
        if self.open {
            // The highlight stays on the same Clip when the list shifts.
            if let Some(id) = selected_id {
                self.selected = self.shown().iter().position(|c| c.id == id).unwrap_or(0);
            }
            effects.push(Effect::Redraw);
        }
        effects
    }

    /// The 100 newest unpinned Clips and every pinned one stay.
    fn trim(&mut self) {
        let clips = std::mem::take(&mut self.clips);
        self.clips = store::cap(clips, |c| c.pinned, store::MAX_UNPINNED);
        self.selected = self.selected.min(self.shown().len().saturating_sub(1));
    }

    fn save(&self, effects: &mut Vec<Effect>) {
        if self.loaded {
            effects.push(Effect::Save);
        }
    }

    /// The saved History arrived: what was copied since start stays on top,
    /// the saved Clips follow without the ones copied again (the copy keeps
    /// the saved pin). Saved ids are reissued so they cannot meet new ones.
    fn loaded(&mut self, saved: Vec<Clip>) -> Vec<Effect> {
        if self.loaded {
            return Vec::new();
        }
        self.loaded = true;
        let selected_id = self.shown().get(self.selected).map(|c| c.id);
        let recorded = !self.clips.is_empty();
        for mut old in saved {
            match self.clips.iter_mut().find(|c| c.text == old.text) {
                Some(same) => same.pinned |= old.pinned,
                None => {
                    old.id = ClipId(self.next_id);
                    self.next_id += 1;
                    self.clips.push(old);
                }
            }
        }
        let before = self.clips.len();
        self.trim();
        if let Some(id) = selected_id {
            self.selected = self.shown().iter().position(|c| c.id == id).unwrap_or(0);
        }
        let mut effects = Vec::new();
        // Unchanged on disk unless something was recorded or trimmed.
        if recorded || self.clips.len() != before {
            effects.push(Effect::Save);
        }
        if self.open {
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
            Key::Backspace if self.query.is_empty() => Vec::new(),
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
            Key::Pin if mods.ctrl => self.selected_id().map_or_else(Vec::new, |id| self.toggle_pin(id)),
            Key::Delete if mods.ctrl => self.selected_id().map_or_else(Vec::new, |id| self.delete(id)),
            Key::Pin | Key::Delete => Vec::new(),
        }
    }

    fn selected_id(&self) -> Option<ClipId> {
        self.shown().get(self.selected).map(|c| c.id)
    }

    fn clip_action(&mut self, id: ClipId, action: Action, now_ms: u64) -> Vec<Effect> {
        let Some(row) = self.shown().iter().position(|c| c.id == id) else { return Vec::new() };
        match action {
            Action::Paste => self.paste_row(row, false, now_ms),
            Action::PastePlain => self.paste_row(row, true, now_ms),
            Action::TogglePin => self.toggle_pin(id),
            Action::Delete => self.delete(id),
        }
    }

    /// The selection stays on Clip `id` after the list changed (else it keeps
    /// its row number, within the list).
    fn follow(&mut self, id: Option<ClipId>) {
        let shown = self.shown();
        let at = id.and_then(|id| shown.iter().position(|c| c.id == id));
        self.selected = at.unwrap_or(self.selected).min(shown.len().saturating_sub(1));
    }

    /// The Clips changed: save them, and redraw when the History is showing.
    fn changed(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.save(&mut effects);
        if self.open {
            effects.push(Effect::Redraw);
        }
        effects
    }

    /// Pin a Clip, or unpin it (then it falls back among the Recent ones by
    /// its last-copied time, and the cap applies to it again).
    fn toggle_pin(&mut self, id: ClipId) -> Vec<Effect> {
        let pins = self.clips.iter().filter(|c| c.pinned).count();
        if pins >= store::MAX_PINNED && self.clips.iter().any(|c| c.id == id && !c.pinned) {
            return vec![Effect::Refused(Refusal::TooManyPins)];
        }
        let selected = self.selected_id();
        let Some(clip) = self.clips.iter_mut().find(|c| c.id == id) else { return Vec::new() };
        clip.pinned = !clip.pinned;
        self.trim();
        self.follow(selected);
        self.changed()
    }

    fn delete(&mut self, id: ClipId) -> Vec<Effect> {
        if !self.loaded {
            return vec![Effect::Refused(Refusal::NotLoaded)];
        }
        let selected = self.selected_id().filter(|s| *s != id);
        self.clips.retain(|c| c.id != id);
        self.follow(selected);
        self.changed()
    }

    fn clear(&mut self) -> Vec<Effect> {
        if !self.loaded {
            return vec![Effect::Refused(Refusal::NotLoaded)];
        }
        if self.clips.iter().all(|c| c.pinned) {
            return Vec::new();
        }
        self.clips.retain(|c| c.pinned);
        self.selected = 0;
        self.changed()
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
        self.save(&mut effects);
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

    /// The same, with the stored History already read (Delete and Clear wait for it).
    fn open_loaded(items: &[&str]) -> Machine {
        let mut m = open_with(items);
        m.go(Event::Loaded(vec![]));
        m
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
        assert!(m.go(press(Key::Backspace)).is_empty());
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

    fn saved(text: &str, ms: u64, pinned: bool) -> Clip {
        Clip { id: ClipId(900 + ms), text: text.into(), last_copied_ms: ms, pinned }
    }

    fn many(n: usize) -> Machine {
        let mut m = Machine::default();
        m.go(Event::Loaded(vec![]));
        for i in 0..n {
            m.go(copy(i as u32 + 1, &format!("clip {i}")));
        }
        m
    }

    #[test]
    fn the_101st_unpinned_clip_pushes_out_the_oldest() {
        let mut m = many(100);
        assert_eq!(m.clips().len(), 100);
        assert_eq!(m.clips().last().unwrap().text, "clip 0");
        m.go(copy(1000, "newest"));
        assert_eq!(m.clips().len(), 100);
        assert_eq!(m.clips()[0].text, "newest");
        assert_eq!(m.clips().last().unwrap().text, "clip 1");
    }

    #[test]
    fn pins_are_not_counted_and_never_pushed_out() {
        let mut m = Machine::default();
        let mut pins: Vec<Clip> = (0..3).map(|i| saved(&format!("pin {i}"), i, true)).collect();
        pins.extend((0..100).map(|i| saved(&format!("old {i}"), 100 + i, false)));
        m.go(Event::Loaded(pins));
        assert_eq!(m.clips().len(), 103);
        for i in 0..5 {
            m.go(copy(i + 1, &format!("new {i}")));
        }
        assert_eq!(m.clips().len(), 103);
        assert_eq!(m.clips().iter().filter(|c| c.pinned).count(), 3);
        assert!(!m.clips().iter().any(|c| c.text == "old 99"));
    }

    #[test]
    fn loaded_goes_below_what_was_recorded_since_start_without_duplicates() {
        let mut m = with(&["a", "b"]); // b, a
        let fx = m.go(Event::Loaded(vec![saved("a", 5, true), saved("x", 4, false), saved("y", 3, false)]));
        assert_eq!(texts(&m), ["b", "a", "x", "y"]);
        // The re-copied Clip keeps the saved pin.
        assert!(m.clips()[1].pinned);
        assert!(fx.contains(&Effect::Save));
        let mut ids: Vec<u64> = m.clips().iter().map(|c| c.id.0).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 4, "ids are distinct");
        // New Clips keep getting fresh ids.
        m.go(copy(10, "z"));
        assert_eq!(m.clips().iter().map(|c| c.id.0).collect::<std::collections::HashSet<_>>().len(), 5);
    }

    #[test]
    fn nothing_is_saved_before_the_store_is_read() {
        let mut m = Machine::default();
        assert_eq!(m.go(copy(1, "a")), [Effect::Add(ClipId(0))]);
        let fx = m.go(Event::Loaded(vec![]));
        assert_eq!(fx, [Effect::Save], "what was recorded meanwhile is saved");
        assert_eq!(m.go(copy(2, "b")), [Effect::Add(ClipId(1)), Effect::Save]);
        // A second Loaded changes nothing.
        assert!(m.go(Event::Loaded(vec![saved("q", 1, false)])).is_empty());
        assert_eq!(texts(&m), ["b", "a"]);
    }

    #[test]
    fn loading_into_an_untouched_history_saves_nothing_and_redraws_when_open() {
        let mut m = Machine::default();
        m.go(Event::Opened(None));
        assert_eq!(m.go(Event::Loaded(vec![saved("a", 1, false)])), [Effect::Redraw]);
        assert_eq!(texts(&m), ["a"]);
    }

    #[test]
    fn paste_back_saves_the_new_order() {
        let mut m = many(2);
        m.go(Event::Opened(Some(APP)));
        let fx = m.go(press(Key::Down));
        assert_eq!(fx, [Effect::Redraw]);
        let fx = m.go(press(Key::Enter));
        assert!(fx.contains(&Effect::Save));
    }

    fn with_loaded(items: &[&str]) -> Machine {
        let mut m = with(items);
        m.go(Event::Loaded(vec![]));
        m
    }

    fn pin_selected(m: &mut Machine) -> Vec<Effect> {
        m.go(chord(Key::Pin, false, true, false))
    }

    fn delete_selected(m: &mut Machine) -> Vec<Effect> {
        m.go(chord(Key::Delete, false, true, false))
    }

    #[test]
    fn ctrl_p_pins_the_selected_clip_and_it_moves_to_the_pinned_section() {
        let mut m = Machine::default();
        m.go(Event::Loaded(vec![]));
        for (i, t) in ["a", "b", "c"].iter().enumerate() {
            m.go(copy(i as u32 + 1, t));
        }
        m.go(Event::Opened(Some(APP))); // c, b, a
        m.go(press(Key::Down));
        m.go(press(Key::Down)); // "a"
        let fx = pin_selected(&mut m);
        assert_eq!(fx, [Effect::Save, Effect::Redraw]);
        assert_eq!(shown(&m), ["a", "c", "b"]);
        assert_eq!(m.shown_pinned(), 1);
        // The selection follows the Clip, now number 1.
        assert_eq!(m.selected(), 0);
        let fx = m.go(chord(Key::Digit(1), true, false, false));
        assert!(matches!(fx.last(), Some(Effect::Paste { clip, .. }) if clip.text == "a" && clip.pinned));
    }

    #[test]
    fn ctrl_p_again_unpins_and_the_clip_returns_to_recent_by_its_time() {
        let mut m = Machine::default();
        m.step(copy(1, "a"), 1_000);
        m.step(copy(2, "b"), 2_000);
        m.step(copy(3, "c"), 3_000);
        m.go(Event::Opened(Some(APP)));
        m.go(press(Key::Down));
        m.go(press(Key::Down));
        pin_selected(&mut m); // a pinned
        assert_eq!(shown(&m), ["a", "c", "b"]);
        pin_selected(&mut m); // the selection followed "a"
        assert_eq!(shown(&m), ["c", "b", "a"]);
        assert_eq!(m.shown_pinned(), 0);
        assert_eq!(m.selected(), 2);
    }

    #[test]
    fn pinning_needs_ctrl_and_an_open_history_with_a_clip() {
        let mut m = open_with(&["a"]);
        assert!(m.go(press(Key::Pin)).is_empty());
        assert!(!m.clips()[0].pinned);
        let mut m = with(&["a"]);
        assert!(pin_selected(&mut m).is_empty());
        let mut m = Machine::default();
        m.go(Event::Opened(None));
        assert!(pin_selected(&mut m).is_empty());
    }

    #[test]
    fn pins_stay_on_top_while_searching_and_numbering_follows() {
        let mut m = open_with(&["ax", "ay", "az"]); // az, ay, ax
        m.go(press(Key::Down));
        m.go(press(Key::Down));
        pin_selected(&mut m); // ax pinned
        type_text(&mut m, "a");
        assert_eq!(shown(&m), ["ax", "az", "ay"]);
        let fx = m.go(chord(Key::Digit(1), false, true, false));
        assert!(matches!(fx.last(), Some(Effect::Paste { clip, .. }) if clip.text == "ax"));
    }

    #[test]
    fn pasting_a_clip_keeps_a_pin_in_the_pinned_section() {
        let mut m = open_with(&["a", "b", "c"]);
        m.go(press(Key::Down));
        pin_selected(&mut m); // b pinned
        m.go(press(Key::Down)); // c
        m.go(press(Key::Enter));
        m.go(Event::Opened(Some(APP)));
        assert_eq!(shown(&m), ["b", "c", "a"]);
        assert_eq!(m.shown_pinned(), 1);
    }

    #[test]
    fn a_re_copied_pinned_clip_stays_pinned() {
        let mut m = open_with(&["a", "b"]);
        m.go(press(Key::Down));
        pin_selected(&mut m); // a pinned
        m.go(copy(9, "a"));
        assert!(m.clips()[0].pinned);
        assert_eq!(m.shown_pinned(), 1);
    }

    #[test]
    fn a_pin_is_never_pushed_out_by_the_cap() {
        let mut m = many(100);
        m.go(Event::Opened(None));
        for _ in 0..99 {
            m.go(press(Key::Down));
        }
        assert_eq!(m.shown()[m.selected()].text, "clip 0");
        pin_selected(&mut m);
        m.go(copy(1000, "n1"));
        m.go(copy(1001, "n2"));
        assert_eq!(m.clips().len(), 101);
        assert!(m.clips().iter().any(|c| c.text == "clip 0" && c.pinned));
        assert!(!m.clips().iter().any(|c| c.text == "clip 1"));
    }

    #[test]
    fn unpinning_an_old_clip_into_a_full_history_drops_the_oldest_unpinned() {
        let mut m = many(100);
        m.go(Event::Opened(None));
        for _ in 0..99 {
            m.go(press(Key::Down));
        }
        pin_selected(&mut m); // clip 0, the oldest
        m.go(copy(1000, "newest")); // 100 unpinned plus the pin
        assert_eq!(m.clips().len(), 101);
        m.go(Event::Opened(None));
        assert_eq!(m.shown()[0].text, "clip 0");
        pin_selected(&mut m); // unpin: 101 unpinned, so the oldest of them goes
        assert_eq!(m.clips().len(), 100);
        assert!(!m.clips().iter().any(|c| c.text == "clip 0"));
    }

    #[test]
    fn ctrl_delete_deletes_the_selected_clip_pinned_or_not() {
        let mut m = open_loaded(&["a", "b", "c"]); // c, b, a
        m.go(press(Key::Down));
        let fx = delete_selected(&mut m);
        assert_eq!(fx, [Effect::Save, Effect::Redraw]);
        assert_eq!(texts(&m), ["c", "a"]);
        assert_eq!(m.selected(), 1);
        pin_selected(&mut m);
        assert_eq!(shown(&m), ["a", "c"]);
        delete_selected(&mut m);
        assert_eq!(texts(&m), ["c"]);
        assert_eq!(m.selected(), 0);
    }

    #[test]
    fn delete_needs_ctrl_and_something_selected() {
        let mut m = open_loaded(&["a"]);
        assert!(m.go(press(Key::Delete)).is_empty());
        assert_eq!(texts(&m), ["a"]);
        delete_selected(&mut m);
        assert!(m.clips().is_empty());
        assert!(delete_selected(&mut m).is_empty());
    }

    #[test]
    fn delete_acts_on_the_filtered_list() {
        let mut m = open_loaded(&["apple", "banana", "apricot"]);
        type_text(&mut m, "ap");
        m.go(press(Key::Down)); // apple
        delete_selected(&mut m);
        assert_eq!(texts(&m), ["apricot", "banana"]);
    }

    #[test]
    fn changes_are_saved_once_the_store_is_read() {
        let mut m = many(3);
        m.go(Event::Opened(None));
        assert_eq!(pin_selected(&mut m), [Effect::Save, Effect::Redraw]);
        assert_eq!(delete_selected(&mut m), [Effect::Save, Effect::Redraw]);
        assert_eq!(m.go(Event::Clear), [Effect::Save, Effect::Redraw]);
    }

    #[test]
    fn clear_deletes_every_unpinned_clip_and_keeps_the_pins() {
        let mut m = open_loaded(&["a", "b", "c", "d"]); // d, c, b, a
        m.go(press(Key::Down));
        pin_selected(&mut m); // c, now first of c, d, b, a
        for _ in 0..3 {
            m.go(press(Key::Down));
        }
        pin_selected(&mut m); // a
        let fx = m.go(Event::Clear);
        assert_eq!(fx, [Effect::Save, Effect::Redraw]);
        assert_eq!(shown(&m), ["c", "a"]);
        assert!(m.clips().iter().all(|c| c.pinned));
        assert_eq!(m.selected(), 0);
        assert!(m.is_open(), "no confirmation, and the History stays open");
    }

    #[test]
    fn clear_clears_the_unpinned_even_while_searching_and_does_nothing_when_none() {
        let mut m = open_loaded(&["ax", "by"]);
        type_text(&mut m, "a");
        m.go(Event::Clear);
        assert!(m.clips().is_empty());
        assert!(m.go(Event::Clear).is_empty());
        let mut m = open_loaded(&["a"]);
        pin_selected(&mut m);
        assert!(m.go(Event::Clear).is_empty());
    }

    #[test]
    fn clear_works_while_closed_too() {
        let mut m = with_loaded(&["a"]);
        m.go(Event::Clear);
        assert!(m.clips().is_empty());
    }

    #[test]
    fn the_right_click_menu_acts_on_a_clip_by_id() {
        let mut m = open_loaded(&["a", "b", "c"]); // c, b, a
        let b = m.clips()[1].id;
        m.go(Event::ClipAction { id: b, action: Action::TogglePin });
        assert_eq!(shown(&m), ["b", "c", "a"]);
        let fx = m.go(Event::ClipAction { id: b, action: Action::PastePlain });
        assert!(matches!(fx.last(), Some(Effect::Paste { clip, plain: true, .. }) if clip.text == "b"));

        let mut m = open_loaded(&["a", "b", "c"]);
        let a = m.clips()[2].id;
        let fx = m.go(Event::ClipAction { id: a, action: Action::Paste });
        assert!(matches!(fx.last(), Some(Effect::Paste { clip, plain: false, .. }) if clip.text == "a"));

        let mut m = open_loaded(&["a", "b", "c"]);
        let c = m.clips()[0].id;
        m.go(Event::ClipAction { id: c, action: Action::Delete });
        assert_eq!(texts(&m), ["b", "a"]);
    }

    #[test]
    fn a_menu_action_on_a_vanished_clip_or_a_closed_history_does_nothing() {
        let mut m = open_loaded(&["a"]);
        let a = m.clips()[0].id;
        m.go(Event::ClipAction { id: a, action: Action::Delete });
        assert!(m.go(Event::ClipAction { id: a, action: Action::Paste }).is_empty());

        let mut m = open_loaded(&["a"]);
        let a = m.clips()[0].id;
        m.go(Event::Deactivated);
        assert!(m.go(Event::ClipAction { id: a, action: Action::TogglePin }).is_empty());
        assert!(!m.clips()[0].pinned);
    }

    #[test]
    fn deleting_a_clip_above_keeps_the_same_clip_selected() {
        let mut m = open_loaded(&["a", "b", "c"]); // c, b, a
        m.go(press(Key::Down));
        m.go(press(Key::Down)); // a
        let c = m.clips()[0].id;
        m.go(Event::ClipAction { id: c, action: Action::Delete });
        assert_eq!(m.shown()[m.selected()].text, "a");
    }

    #[test]
    fn delete_and_clear_wait_for_the_stored_history() {
        let mut m = open_with(&["a", "b"]); // not loaded
        assert_eq!(delete_selected(&mut m), [Effect::Refused(Refusal::NotLoaded)]);
        assert_eq!(m.go(Event::Clear), [Effect::Refused(Refusal::NotLoaded)]);
        let b = m.clips()[0].id;
        assert_eq!(m.go(Event::ClipAction { id: b, action: Action::Delete }), [Effect::Refused(Refusal::NotLoaded)]);
        assert_eq!(texts(&m), ["b", "a"]);
        // After the load they work, and a loaded Clip is not brought back.
        m.go(Event::Loaded(vec![saved("old", 1, false)]));
        delete_selected(&mut m);
        assert_eq!(texts(&m), ["a", "old"]);
    }

    #[test]
    fn pins_stop_at_the_budget_the_store_can_read() {
        let mut m = Machine::default();
        let pins: Vec<Clip> = (0..store::MAX_PINNED as u64).map(|i| saved(&format!("pin {i}"), i, true)).collect();
        m.go(Event::Loaded(pins));
        m.go(copy(1, "one more"));
        let id = m.clips()[0].id;
        assert_eq!(m.go(Event::Opened(None)), [Effect::Redraw]);
        let fx = m.go(Event::ClipAction { id, action: Action::TogglePin });
        assert_eq!(fx, [Effect::Refused(Refusal::TooManyPins)]);
        assert!(!m.clips()[0].pinned);
        // Select it and press Ctrl+P: refused the same way.
        m.go(Event::Key(Key::Down, Mods::default()));
        for _ in 0..store::MAX_PINNED {
            m.go(Event::Key(Key::Down, Mods::default()));
        }
        assert_eq!(pin_selected(&mut m), [Effect::Refused(Refusal::TooManyPins)]);
        // Unpinning one frees a place.
        let first_pin = m.shown()[0].id;
        m.go(Event::ClipAction { id: first_pin, action: Action::TogglePin });
        assert_eq!(m.go(Event::ClipAction { id, action: Action::TogglePin }), [Effect::Save, Effect::Redraw]);
        assert!(m.clips().iter().find(|c| c.id == id).unwrap().pinned);
    }
}
