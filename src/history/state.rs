//! The pure History state machine: events in, effects out. It owns no window,
//! clipboard, file or thread; the shell (`mod.rs`) feeds it events and carries
//! out the effects it returns.

use super::clip::{Clip, ClipId};

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
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    Enter,
    Esc,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
    Copied(Capture),
    /// Our own clipboard write finished; its sequence number is not a copy.
    OwnWrite(u32),
    Key(Key),
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
    Paste { clip: Clip, target: Target },
    /// No app to paste into: only write the Clip to the clipboard.
    Copy { clip: Clip },
    Close,
    Redraw,
}

#[derive(Default)]
pub struct Machine {
    /// Newest first.
    clips: Vec<Clip>,
    next_id: u64,
    selected: usize,
    open: bool,
    target: Option<Target>,
    last_seq: Option<u32>,
    own_seq: Option<u32>,
}

impl Machine {
    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn step(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::Copied(capture) => self.copied(capture),
            Event::OwnWrite(seq) => {
                self.own_seq = Some(seq);
                Vec::new()
            }
            Event::Opened(target) => {
                self.open = true;
                self.target = target;
                self.selected = 0;
                vec![Effect::Redraw]
            }
            Event::Key(key) if self.open => self.key(key),
            Event::Key(_) => Vec::new(),
            Event::Deactivated => self.close(),
        }
    }

    fn copied(&mut self, capture: Capture) -> Vec<Effect> {
        if self.last_seq == Some(capture.seq) {
            return vec![Effect::Skip(Skip::SameUpdate)];
        }
        self.last_seq = Some(capture.seq);
        if self.own_seq == Some(capture.seq) {
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
        let selected_id = self.clips.get(self.selected).map(|c| c.id);
        match self.clips.iter().position(|c| c.text == text) {
            Some(i) => {
                let clip = self.clips.remove(i);
                effects.push(Effect::MoveToTop(clip.id));
                self.clips.insert(0, clip);
            }
            None => {
                let id = ClipId(self.next_id);
                self.next_id += 1;
                self.clips.insert(0, Clip { id, text });
                effects.push(Effect::Add(id));
            }
        }
        if self.open {
            // The highlight stays on the same Clip when the list shifts.
            if let Some(id) = selected_id {
                self.selected = self.clips.iter().position(|c| c.id == id).unwrap_or(0);
            }
            effects.push(Effect::Redraw);
        }
        effects
    }

    fn key(&mut self, key: Key) -> Vec<Effect> {
        match key {
            Key::Up | Key::Down => {
                let last = self.clips.len().saturating_sub(1);
                let to = match key {
                    Key::Up => self.selected.saturating_sub(1),
                    _ => (self.selected + 1).min(last),
                };
                if to == self.selected {
                    return Vec::new();
                }
                self.selected = to;
                vec![Effect::Redraw]
            }
            Key::Enter => self.paste(),
            Key::Esc => self.close(),
        }
    }

    /// Enter: the selected Clip moves to the top and is pasted. Closing comes
    /// before the paste so the window is gone when focus goes back.
    fn paste(&mut self) -> Vec<Effect> {
        if self.selected >= self.clips.len() {
            return self.close();
        }
        let clip = self.clips.remove(self.selected);
        self.clips.insert(0, clip.clone());
        let mut effects = vec![Effect::MoveToTop(clip.id)];
        effects.extend(self.close());
        effects.push(match self.target {
            Some(target) => Effect::Paste { clip, target },
            None => Effect::Copy { clip },
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

    fn copy(seq: u32, text: &str) -> Event {
        Event::Copied(Capture { seq, text: Some(text.to_string()) })
    }

    fn texts(m: &Machine) -> Vec<&str> {
        m.clips().iter().map(|c| c.text.as_str()).collect()
    }

    fn with(items: &[&str]) -> Machine {
        let mut m = Machine::default();
        for (i, t) in items.iter().enumerate() {
            m.step(copy(i as u32 + 1, t));
        }
        m
    }

    const APP: Target = Target(0x1234);

    #[test]
    fn a_text_clip_is_added_on_top() {
        let mut m = Machine::default();
        let fx = m.step(copy(1, "one"));
        assert!(matches!(fx.as_slice(), [Effect::Add(_)]));
        m.step(copy(2, "two"));
        assert_eq!(texts(&m), ["two", "one"]);
    }

    #[test]
    fn a_duplicate_moves_to_the_top_instead_of_being_added() {
        let mut m = with(&["a", "b", "c"]);
        let id = m.clips()[2].id;
        let fx = m.step(copy(9, "a"));
        assert_eq!(fx, [Effect::MoveToTop(id)]);
        assert_eq!(texts(&m), ["a", "c", "b"]);
    }

    #[test]
    fn our_own_write_is_skipped() {
        let mut m = with(&["a"]);
        m.step(Event::OwnWrite(7));
        assert_eq!(m.step(copy(7, "b")), [Effect::Skip(Skip::OwnWrite)]);
        assert_eq!(texts(&m), ["a"]);
        // The next real copy is recorded.
        m.step(copy(8, "b"));
        assert_eq!(texts(&m), ["b", "a"]);
    }

    #[test]
    fn the_same_sequence_number_is_handled_once() {
        let mut m = Machine::default();
        m.step(copy(3, "a"));
        assert_eq!(m.step(copy(3, "a")), [Effect::Skip(Skip::SameUpdate)]);
        assert_eq!(m.clips().len(), 1);
    }

    #[test]
    fn blank_missing_and_huge_text_is_not_recorded() {
        let mut m = Machine::default();
        assert_eq!(m.step(Event::Copied(Capture { seq: 1, text: None })), [Effect::Skip(Skip::NothingRecordable)]);
        assert_eq!(m.step(copy(2, " \r\n\t")), [Effect::Skip(Skip::NothingRecordable)]);
        assert_eq!(m.step(copy(3, &"x".repeat(MAX_CLIP_BYTES + 1))), [Effect::Skip(Skip::TooBig)]);
        assert!(m.clips().is_empty());
    }

    #[test]
    fn the_first_row_is_selected_on_open() {
        let mut m = with(&["a", "b", "c"]);
        m.step(Event::Opened(Some(APP)));
        m.step(Event::Key(Key::Down));
        m.step(Event::Key(Key::Esc));
        m.step(Event::Opened(Some(APP)));
        assert!(m.is_open());
        assert_eq!(m.selected(), 0);
    }

    #[test]
    fn up_and_down_move_the_selection_and_stop_at_the_ends() {
        let mut m = with(&["a", "b", "c"]);
        m.step(Event::Opened(Some(APP)));
        assert!(m.step(Event::Key(Key::Up)).is_empty());
        assert_eq!(m.step(Event::Key(Key::Down)), [Effect::Redraw]);
        m.step(Event::Key(Key::Down));
        assert_eq!(m.selected(), 2);
        assert!(m.step(Event::Key(Key::Down)).is_empty());
        m.step(Event::Key(Key::Up));
        assert_eq!(m.selected(), 1);
    }

    #[test]
    fn enter_pastes_the_selected_clip_and_moves_it_to_the_top() {
        let mut m = with(&["a", "b", "c"]); // c, b, a
        m.step(Event::Opened(Some(APP)));
        m.step(Event::Key(Key::Down));
        let fx = m.step(Event::Key(Key::Enter));
        let b = m.clips()[0].clone();
        assert_eq!(b.text, "b");
        assert_eq!(fx, [Effect::MoveToTop(b.id), Effect::Close, Effect::Paste { clip: b, target: APP }]);
        assert_eq!(texts(&m), ["b", "c", "a"]);
        assert!(!m.is_open());
    }

    #[test]
    fn enter_without_a_target_only_copies() {
        let mut m = with(&["a"]);
        m.step(Event::Opened(None));
        let fx = m.step(Event::Key(Key::Enter));
        assert!(matches!(fx.as_slice(), [Effect::MoveToTop(_), Effect::Close, Effect::Copy { .. }]));
    }

    #[test]
    fn enter_on_an_empty_history_just_closes() {
        let mut m = Machine::default();
        m.step(Event::Opened(Some(APP)));
        assert_eq!(m.step(Event::Key(Key::Enter)), [Effect::Close]);
    }

    #[test]
    fn esc_and_deactivated_close_once() {
        let mut m = with(&["a"]);
        m.step(Event::Opened(Some(APP)));
        assert_eq!(m.step(Event::Key(Key::Esc)), [Effect::Close]);
        // Hiding the window deactivates it: nothing more to do.
        assert!(m.step(Event::Deactivated).is_empty());
        m.step(Event::Opened(Some(APP)));
        assert_eq!(m.step(Event::Deactivated), [Effect::Close]);
        assert!(!m.is_open());
    }

    #[test]
    fn keys_do_nothing_while_closed() {
        let mut m = with(&["a"]);
        assert!(m.step(Event::Key(Key::Enter)).is_empty());
        assert!(m.step(Event::Key(Key::Down)).is_empty());
    }

    #[test]
    fn a_copy_while_open_redraws_and_keeps_the_selection_valid() {
        let mut m = with(&["a", "b"]);
        m.step(Event::Opened(None));
        m.step(Event::Key(Key::Down));
        let fx = m.step(copy(9, "c"));
        assert!(fx.contains(&Effect::Redraw));
        // The list was b, a with "a" selected; the new clip went on top.
        assert_eq!(m.clips()[m.selected()].text, "a");
    }
}
