//! The pure Snippets state machine: events in, effects out. It owns no window,
//! clipboard, file or thread; the shell feeds it events and carries out the
//! effects it returns.

use std::collections::HashSet;

use super::model::{Snippet, SnippetId};
use super::store;
use super::text;

/// What the editor hands back, and what opens it. `id` is `None` for a new
/// Snippet.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Draft {
    pub id: Option<SnippetId>,
    pub title: String,
    pub body: String,
}

/// What the right-click menu can do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Copy,
    Edit,
    Delete,
}

/// The keys the panel acts on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    PageUp,
    PageDown,
    Enter,
    Esc,
    Delete,
    /// Ctrl+N.
    New,
    /// Ctrl+E.
    Edit,
    /// 1..=9, with Alt or Ctrl.
    Digit(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mods {
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
    /// The saved list was read at startup (newest first).
    Loaded(Vec<Snippet>),
    Opened,
    Deactivated,
    Key(Key, Mods),
    /// A left click on a row.
    Click {
        row: usize,
    },
    /// "New" in the footer.
    New,
    /// A right-click menu choice. The Snippet is named by id: the list can
    /// change while the menu is up.
    Action {
        id: SnippetId,
        action: Action,
    },
    Select(SnippetId),
    /// The editor closed with Save.
    Commit {
        id: Option<SnippetId>,
        title: String,
        body: String,
    },
    /// A Clip's text, saved from the History.
    Import(String),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Effect {
    Redraw,
    /// The list changed and the store has been read: write it.
    Save,
    Close,
    /// Put this text on the clipboard.
    Copy(String),
    Editor(Draft),
    Refused(Refusal),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// Delete waits for the stored list: it would bring the row back.
    NotLoaded,
    Empty,
    TooLong,
}

const PAGE: usize = 5;

pub struct Machine {
    /// Newest first.
    snippets: Vec<Snippet>,
    next_id: u64,
    selected: usize,
    /// The store has been read. Until then a save would replace the file
    /// with only what was added since start, so saves wait.
    loaded: bool,
    open: bool,
}

impl Default for Machine {
    fn default() -> Self {
        Self {
            snippets: Vec::new(),
            next_id: 1,
            selected: 0,
            loaded: false,
            open: false,
        }
    }
}

impl Machine {
    pub fn snippets(&self) -> &[Snippet] {
        &self.snippets
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// `now_ms` is the shell's clock (ms since the Unix epoch).
    pub fn step(&mut self, event: Event, now_ms: u64) -> Vec<Effect> {
        match event {
            Event::Loaded(saved) => self.loaded(saved),
            Event::Opened => {
                self.open = true;
                self.selected = 0;
                vec![Effect::Redraw]
            }
            Event::Deactivated => self.close(),
            Event::Key(key, mods) if self.open => self.key(key, mods, now_ms),
            Event::Click { row } if self.open => self.copy_row(row, now_ms),
            Event::New if self.open => self.editor_new(),
            Event::Action { id, action } if self.open => self.action(id, action, now_ms),
            Event::Select(id) if self.open => match self.snippets.iter().position(|s| s.id == id) {
                Some(row) => self.select(row),
                None => Vec::new(),
            },
            Event::Commit { id, title, body } => self.commit(id, title, body, now_ms),
            Event::Import(body) => self.import(body, now_ms),
            Event::Key(..)
            | Event::Click { .. }
            | Event::New
            | Event::Action { .. }
            | Event::Select(_) => Vec::new(),
        }
    }

    fn key(&mut self, key: Key, mods: Mods, now_ms: u64) -> Vec<Effect> {
        let last = self.snippets.len().saturating_sub(1);
        match key {
            Key::Up => self.select(self.selected.saturating_sub(1)),
            Key::Down => self.select((self.selected + 1).min(last)),
            Key::PageUp => self.select(self.selected.saturating_sub(PAGE)),
            Key::PageDown => self.select((self.selected + PAGE).min(last)),
            Key::Enter if self.snippets.is_empty() => self.close(),
            Key::Enter => self.copy_row(self.selected, now_ms),
            Key::Esc => self.close(),
            Key::Delete => self
                .selected_id()
                .map_or_else(Vec::new, |id| self.delete(id)),
            Key::New if mods.ctrl => self.editor_new(),
            Key::Edit if mods.ctrl => self
                .selected_id()
                .map_or_else(Vec::new, |id| self.editor_for(id)),
            Key::Digit(n) if (mods.alt || mods.ctrl) && (1..=9).contains(&n) => {
                self.copy_row(n as usize - 1, now_ms)
            }
            Key::New | Key::Edit | Key::Digit(_) => Vec::new(),
        }
    }

    fn selected_id(&self) -> Option<SnippetId> {
        self.snippets.get(self.selected).map(|s| s.id)
    }

    fn action(&mut self, id: SnippetId, action: Action, now_ms: u64) -> Vec<Effect> {
        match action {
            Action::Copy => match self.snippets.iter().position(|s| s.id == id) {
                Some(row) => self.copy_row(row, now_ms),
                None => Vec::new(),
            },
            Action::Edit => self.editor_for(id),
            Action::Delete => self.delete(id),
        }
    }

    fn editor_new(&self) -> Vec<Effect> {
        vec![Effect::Editor(Draft {
            id: None,
            title: String::new(),
            body: String::new(),
        })]
    }

    fn editor_for(&self, id: SnippetId) -> Vec<Effect> {
        let Some(s) = self.snippets.iter().find(|s| s.id == id) else {
            return Vec::new();
        };
        vec![Effect::Editor(Draft {
            id: Some(id),
            title: s.title.clone(),
            body: s.body.clone(),
        })]
    }

    /// Copying moves the Snippet to the top, like a paste in the History, and
    /// closes the panel so the text can be pasted into the app underneath.
    fn copy_row(&mut self, row: usize, now_ms: u64) -> Vec<Effect> {
        let Some(id) = self.snippets.get(row).map(|s| s.id) else {
            return Vec::new();
        };
        let Some(i) = self.snippets.iter().position(|s| s.id == id) else {
            return Vec::new();
        };
        let mut snip = self.snippets.remove(i);
        snip.updated_ms = now_ms;
        let text = snip.body.clone();
        self.snippets.insert(0, snip);
        self.selected = 0;
        let mut effects = Vec::new();
        self.save(&mut effects);
        effects.extend(self.close());
        effects.push(Effect::Copy(text));
        effects
    }

    fn delete(&mut self, id: SnippetId) -> Vec<Effect> {
        if !self.loaded {
            return vec![Effect::Refused(Refusal::NotLoaded)];
        }
        let keep = self.selected_id().filter(|s| *s != id);
        let before = self.snippets.len();
        self.snippets.retain(|s| s.id != id);
        if self.snippets.len() == before {
            return Vec::new();
        }
        self.follow(keep);
        self.changed()
    }

    fn commit(
        &mut self,
        id: Option<SnippetId>,
        title: String,
        body: String,
        now_ms: u64,
    ) -> Vec<Effect> {
        let Some(body) = accept_body(&body) else {
            return vec![Effect::Refused(refusal_for(&body))];
        };
        let title = {
            let t = title.trim();
            if t.is_empty() {
                text::title_from_body(&body)
            } else {
                text::limit_chars(t, text::TITLE_CHARS)
            }
        };
        if let Some(id) = id {
            let Some(pos) = self.snippets.iter().position(|s| s.id == id) else {
                return Vec::new();
            };
            self.snippets[pos].title = title;
            self.snippets[pos].body = body;
            self.snippets[pos].updated_ms = now_ms;
            self.move_front(id);
            self.follow(Some(id));
            return self.changed();
        }
        let snip = Snippet {
            id: self.alloc(),
            title,
            body,
            updated_ms: now_ms,
        };
        self.snippets.insert(0, snip);
        self.selected = 0;
        self.cap();
        self.changed()
    }

    fn import(&mut self, body: String, now_ms: u64) -> Vec<Effect> {
        let Some(body) = accept_body(&body) else {
            return vec![Effect::Refused(refusal_for(&body))];
        };
        if let Some(i) = self
            .snippets
            .iter()
            .position(|s| s.body.trim() == body.trim())
        {
            self.snippets[i].updated_ms = now_ms;
            let id = self.snippets[i].id;
            self.move_front(id);
            if self.open {
                self.selected = 0;
            }
            return self.changed();
        }
        let snip = Snippet {
            id: self.alloc(),
            title: text::title_from_body(&body),
            body,
            updated_ms: now_ms,
        };
        self.snippets.insert(0, snip);
        if self.open {
            self.selected = 0;
        }
        self.cap();
        self.changed()
    }

    fn select(&mut self, to: usize) -> Vec<Effect> {
        if to == self.selected || self.snippets.is_empty() {
            return Vec::new();
        }
        self.selected = to.min(self.snippets.len() - 1);
        vec![Effect::Redraw]
    }

    fn close(&mut self) -> Vec<Effect> {
        if !self.open {
            return Vec::new();
        }
        self.open = false;
        vec![Effect::Close]
    }

    fn follow(&mut self, id: Option<SnippetId>) {
        let at = id.and_then(|id| self.snippets.iter().position(|s| s.id == id));
        self.selected = at
            .unwrap_or(self.selected)
            .min(self.snippets.len().saturating_sub(1));
    }

    fn move_front(&mut self, id: SnippetId) {
        if let Some(i) = self.snippets.iter().position(|s| s.id == id) {
            let snip = self.snippets.remove(i);
            self.snippets.insert(0, snip);
        }
    }

    fn cap(&mut self) {
        if self.snippets.len() > store::MAX_SNIPPETS {
            self.snippets.truncate(store::MAX_SNIPPETS);
        }
        self.selected = self.selected.min(self.snippets.len().saturating_sub(1));
    }

    fn alloc(&mut self) -> SnippetId {
        let id = SnippetId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    fn save(&self, effects: &mut Vec<Effect>) {
        if self.loaded {
            effects.push(Effect::Save);
        }
    }

    fn changed(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.save(&mut effects);
        if self.open {
            effects.push(Effect::Redraw);
        }
        effects
    }

    /// Saved rows follow whatever was added since start. The same body is not
    /// stored twice: the in-memory one wins and keeps its title. A plain load
    /// does not rewrite the file.
    fn loaded(&mut self, saved: Vec<Snippet>) -> Vec<Effect> {
        if self.loaded {
            return Vec::new();
        }
        self.loaded = true;
        let selected = self.selected_id();
        let pending = !self.snippets.is_empty();
        let mut skipped = false;
        let mut used: HashSet<u64> = self.snippets.iter().map(|s| s.id.0).collect();
        for old in saved {
            if self
                .snippets
                .iter()
                .any(|s| s.body.trim() == old.body.trim())
            {
                skipped = true;
                continue;
            }
            let mut old = old;
            old.id = self.take_id(&mut used, old.id.0);
            self.snippets.push(old);
        }
        let merged = self.snippets.len();
        self.cap();
        self.follow(selected);
        let mut effects = Vec::new();
        if pending || skipped || self.snippets.len() != merged {
            effects.push(Effect::Save);
        }
        if self.open {
            effects.push(Effect::Redraw);
        }
        effects
    }

    fn take_id(&mut self, used: &mut HashSet<u64>, id: u64) -> SnippetId {
        if used.insert(id) {
            self.next_id = self.next_id.max(id.saturating_add(1));
            SnippetId(id)
        } else {
            let fresh = self.alloc();
            used.insert(fresh.0);
            fresh
        }
    }
}

fn refusal_for(body: &str) -> Refusal {
    if body.trim().is_empty() {
        Refusal::Empty
    } else {
        Refusal::TooLong
    }
}

fn accept_body(body: &str) -> Option<String> {
    let kept = body.trim_end();
    if text::body_error(kept).is_some() {
        None
    } else {
        Some(kept.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Machine {
        fn go(&mut self, event: Event) -> Vec<Effect> {
            self.step(event, 1_000)
        }
    }

    fn bodies(m: &Machine) -> Vec<&str> {
        m.snippets().iter().map(|s| s.body.as_str()).collect()
    }

    fn snip(id: u64, body: &str, ms: u64) -> Snippet {
        Snippet {
            id: SnippetId(id),
            title: text::title_from_body(body),
            body: body.into(),
            updated_ms: ms,
        }
    }

    fn commit(body: &str) -> Event {
        Event::Commit {
            id: None,
            title: String::new(),
            body: body.into(),
        }
    }

    const NONE: Mods = Mods {
        alt: false,
        ctrl: false,
        shift: false,
    };

    #[test]
    fn a_load_of_a_clean_list_does_not_rewrite_the_file() {
        let mut m = Machine::default();
        assert_eq!(
            m.go(Event::Loaded(vec![snip(4, "a", 1), snip(9, "b", 2)])),
            []
        );
        assert_eq!(bodies(&m), ["a", "b"]);
        assert_eq!(m.snippets()[0].id, SnippetId(4));
        // The next id is past the saved ones.
        assert_eq!(m.go(commit("c")), [Effect::Save]);
        assert_eq!(m.snippets()[0].id, SnippetId(10));
        assert_eq!(bodies(&m), ["c", "a", "b"]);
    }

    #[test]
    fn saving_the_same_text_again_moves_it_and_keeps_its_title() {
        let mut m = Machine::default();
        m.go(Event::Loaded(vec![]));
        m.go(Event::Commit {
            id: None,
            title: "Greeting".into(),
            body: "hello".into(),
        });
        let id = m.snippets()[0].id;
        m.go(commit("other"));
        assert_eq!(bodies(&m), ["other", "hello"]);
        let fx = m.go(Event::Import("  hello \n".into()));
        assert!(fx.contains(&Effect::Save));
        assert_eq!(bodies(&m), ["hello", "other"]);
        assert_eq!(m.snippets()[0].id, id);
        assert_eq!(m.snippets()[0].title, "Greeting");
        assert_eq!(m.snippets()[0].updated_ms, 1_000);
    }

    #[test]
    fn the_editor_creates_and_updates() {
        let mut m = Machine::default();
        m.go(Event::Loaded(vec![]));
        m.go(Event::Opened);
        assert_eq!(
            m.go(Event::Key(Key::New, Mods { ctrl: true, ..NONE })),
            [Effect::Editor(Draft {
                id: None,
                title: String::new(),
                body: String::new()
            })]
        );
        m.go(Event::Commit {
            id: None,
            title: String::new(),
            body: "first line\nrest".into(),
        });
        assert_eq!(m.snippets()[0].title, "first line");
        let id = m.snippets()[0].id;
        assert_eq!(
            m.go(Event::Action {
                id,
                action: Action::Edit
            }),
            [Effect::Editor(Draft {
                id: Some(id),
                title: "first line".into(),
                body: "first line\nrest".into()
            })]
        );
        m.go(Event::Commit {
            id: Some(id),
            title: "Renamed".into(),
            body: "changed".into(),
        });
        assert_eq!(m.snippets()[0].title, "Renamed");
        assert_eq!(bodies(&m), ["changed"]);
        assert_eq!(
            m.go(Event::Key(Key::Delete, NONE)),
            [Effect::Save, Effect::Redraw]
        );
        assert!(m.snippets().is_empty());
    }

    #[test]
    fn copy_puts_the_text_out_and_closes() {
        let mut m = Machine::default();
        m.go(Event::Loaded(vec![snip(1, "one", 1), snip(2, "two", 1)]));
        m.go(Event::Opened);
        assert_eq!(
            m.go(Event::Click { row: 1 }),
            [Effect::Save, Effect::Close, Effect::Copy("two".into())]
        );
        assert!(!m.is_open());
        assert_eq!(bodies(&m), ["two", "one"]);
        assert_eq!(m.snippets()[0].updated_ms, 1_000);
    }

    #[test]
    fn delete_waits_until_the_store_has_been_read() {
        let mut m = Machine::default();
        m.go(Event::Import("keep".into()));
        m.go(Event::Opened);
        let id = m.snippets()[0].id;
        assert_eq!(
            m.go(Event::Action {
                id,
                action: Action::Delete
            }),
            [Effect::Refused(Refusal::NotLoaded)]
        );
        assert_eq!(bodies(&m), ["keep"]);
        // The import stays on top of what was saved, and the duplicate body is not stored twice.
        let fx = m.go(Event::Loaded(vec![snip(8, "keep", 1), snip(3, "old", 1)]));
        assert_eq!(fx, [Effect::Save, Effect::Redraw]);
        assert_eq!(bodies(&m), ["keep", "old"]);
        assert_eq!(
            m.go(Event::Key(Key::Delete, NONE)),
            [Effect::Save, Effect::Redraw]
        );
        assert_eq!(bodies(&m), ["old"]);
    }

    #[test]
    fn empty_and_over_long_text_is_refused() {
        let mut m = Machine::default();
        m.go(Event::Loaded(vec![]));
        assert_eq!(m.go(commit("  \n")), [Effect::Refused(Refusal::Empty)]);
        assert_eq!(
            m.go(Event::Import("x".repeat(store::MAX_BODY + 1))),
            [Effect::Refused(Refusal::TooLong)]
        );
        assert!(m.snippets().is_empty());
    }

    #[test]
    fn the_list_stops_at_the_cap_and_keeps_the_newest() {
        let mut m = Machine::default();
        m.go(Event::Loaded(vec![]));
        for i in 0..store::MAX_SNIPPETS + 3 {
            m.go(commit(&format!("n{i}")));
        }
        assert_eq!(m.snippets().len(), store::MAX_SNIPPETS);
        assert_eq!(
            m.snippets()[0].body,
            format!("n{}", store::MAX_SNIPPETS + 2)
        );
        assert_eq!(m.snippets().last().unwrap().body, "n3");
    }

    #[test]
    fn keys_move_the_selection_and_esc_closes() {
        let mut m = Machine::default();
        m.go(Event::Loaded(vec![
            snip(1, "a", 1),
            snip(2, "b", 1),
            snip(3, "c", 1),
        ]));
        m.go(Event::Opened);
        assert_eq!(m.go(Event::Key(Key::Down, NONE)), [Effect::Redraw]);
        assert_eq!(m.selected(), 1);
        assert_eq!(m.go(Event::Key(Key::Esc, NONE)), [Effect::Close]);
        assert!(!m.is_open());
        // A key while closed does nothing, and closing twice does nothing.
        assert_eq!(m.go(Event::Key(Key::Down, NONE)), []);
        assert_eq!(m.go(Event::Deactivated), []);
    }

    #[test]
    fn number_keys_copy_that_row() {
        let mut m = Machine::default();
        m.go(Event::Loaded(vec![snip(1, "a", 1), snip(2, "b", 1)]));
        m.go(Event::Opened);
        assert_eq!(
            m.go(Event::Key(Key::Digit(2), Mods { alt: true, ..NONE })),
            [Effect::Save, Effect::Close, Effect::Copy("b".into())]
        );
    }
}
