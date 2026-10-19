//! The in-memory Item list (spec §1, §3). Pure: no Win32, no file I/O (file
//! facts arrive from `probe`, on a worker thread).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Stable for the life of the resident, never reused, so background threads
/// can post results by id after the list has changed.
pub type ItemId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A real file or folder, kept by path.
    Reference,
    /// Content written to `%TEMP%\Tasca\<drop-id>\` (TASCA-16).
    Materialized,
}

/// What the file system says about an Item's files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub first_is_dir: bool,
    /// Sum of the files' sizes; folders count 0 (not walked).
    pub total_bytes: u64,
    /// At least one file is gone. A Stack is Taken out as a unit, so one
    /// missing file makes the whole Item missing.
    pub missing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: ItemId,
    /// More than one = a Stack. The first file names the row and gives the tile.
    pub files: Vec<PathBuf>,
    pub kind: Kind,
    /// `None` until the first probe answers.
    pub facts: Option<Facts>,
    /// Files still being written (TASCA-16): not probed, no thumbnail yet.
    pub pending: bool,
}

const IMAGE_EXTS: &[&str] = &["PNG", "JPG", "JPEG", "GIF", "WEBP", "HEIC", "BMP", "TIF", "TIFF", "AVIF"];

impl Item {
    pub fn is_stack(&self) -> bool {
        self.files.len() > 1
    }

    pub fn missing(&self) -> bool {
        self.facts.is_some_and(|f| f.missing)
    }

    pub fn is_folder(&self) -> bool {
        !self.is_stack() && self.facts.is_some_and(|f| f.first_is_dir)
    }

    /// The first file's extension, upper case, without the dot ("" if none).
    pub fn ext(&self) -> String {
        self.files[0].extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default()
    }

    pub fn is_image(&self) -> bool {
        IMAGE_EXTS.contains(&self.ext().as_str())
    }

    /// Row name: the file name; a Stack is "first +N".
    pub fn title(&self) -> String {
        let first = &self.files[0];
        let name = first.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| first.display().to_string());
        if self.is_stack() { format!("{name} +{}", self.files.len() - 1) } else { name }
    }

    /// Row subtitle: "TYPE · size", "Folder", "N items · size", "Missing" or
    /// "Saving…" (pending).
    pub fn subtitle(&self) -> String {
        if self.pending {
            return "Saving…".into();
        }
        if self.missing() {
            return "Missing".into();
        }
        if self.is_folder() {
            return "Folder".into();
        }
        let kind = if self.is_stack() {
            format!("{} items", self.files.len())
        } else {
            let ext = self.ext();
            if ext.is_empty() { "File".into() } else { ext }
        };
        match self.facts {
            Some(f) => format!("{kind} · {}", human_size(f.total_bytes)),
            None => kind,
        }
    }
}

/// A path as compared for equality: lower case (NTFS is case-insensitive),
/// `\` separators, no `\\?\` prefix, no trailing separator.
pub fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\").to_lowercase();
    let s = match s.strip_prefix(r"\\?\unc\") {
        Some(unc) => format!(r"\\{unc}"),
        None => s.strip_prefix(r"\\?\").unwrap_or(&s).to_owned(),
    };
    s.trim_end_matches('\\').to_owned()
}

pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if bytes == 0 {
        return "0 KB".into();
    }
    if b < KB * KB {
        return format!("{:.0} KB", (b / KB).max(1.0));
    }
    let (v, unit) = if b < KB * KB * KB { (b / KB / KB, "MB") } else { (b / KB / KB / KB, "GB") };
    if v < 10.0 { format!("{v:.1} {unit}") } else { format!("{v:.0} {unit}") }
}

/// Modifier keys held during a click (from the mouse message's `wParam`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
}

#[derive(Debug, Default)]
pub struct Model {
    /// Index 0 is the top row, the newest Item.
    items: Vec<Item>,
    /// The last id handed out (ids start at 1).
    last_id: ItemId,
    selected: BTreeSet<ItemId>,
    /// Where a Shift-click range starts.
    anchor: Option<ItemId>,
}

impl Model {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, id: ItemId) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn index_of(&self, id: ItemId) -> Option<usize> {
        self.items.iter().position(|i| i.id == id)
    }

    /// Add a new Item on top and return its id. `files` must not be empty.
    pub fn add_top(&mut self, files: Vec<PathBuf>, kind: Kind) -> ItemId {
        debug_assert!(!files.is_empty());
        self.last_id += 1;
        let id = self.last_id;
        self.items.insert(0, Item { id, files, kind, facts: None, pending: false });
        id
    }

    /// `add_top` for an Item whose files are still being written.
    pub fn add_pending(&mut self, files: Vec<PathBuf>, kind: Kind) -> ItemId {
        let id = self.add_top(files, kind);
        self.items[0].pending = true;
        id
    }

    /// A pending Item's files are written: keep these (not empty). False if
    /// the Item is gone.
    pub fn fill(&mut self, id: ItemId, files: Vec<PathBuf>) -> bool {
        debug_assert!(!files.is_empty());
        let Some(i) = self.items.iter_mut().find(|i| i.id == id) else { return false };
        i.files = files;
        i.pending = false;
        true
    }

    /// Move an existing Item to the top. False if it is not on the Shelf.
    pub fn move_to_top(&mut self, id: ItemId) -> bool {
        let Some(i) = self.index_of(id) else { return false };
        let item = self.items.remove(i);
        self.items.insert(0, item);
        true
    }

    /// The duplicate-Park rule (spec §4): the Item whose files are exactly
    /// this path set (any order, case-insensitive), if one is on the Shelf.
    pub fn same_files(&self, files: &[PathBuf]) -> Option<ItemId> {
        fn key(files: &[PathBuf]) -> BTreeSet<String> {
            files.iter().map(|f| path_key(f)).collect()
        }
        let want = key(files);
        if want.is_empty() {
            return None;
        }
        self.items.iter().find(|i| key(&i.files) == want).map(|i| i.id)
    }

    /// Remove these Items (unknown ids are ignored). Returns how many went.
    pub fn remove(&mut self, ids: &[ItemId]) -> usize {
        let before = self.items.len();
        self.items.retain(|i| !ids.contains(&i.id));
        for id in ids {
            self.selected.remove(id);
        }
        if self.anchor.is_some_and(|a| ids.contains(&a)) {
            self.anchor = None;
        }
        before - self.items.len()
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.selected.clear();
        self.anchor = None;
    }

    /// Store probe results. True if they differ from what the Item had (false
    /// for an Item that is gone).
    pub fn set_facts(&mut self, id: ItemId, facts: Facts) -> bool {
        let Some(i) = self.items.iter_mut().find(|i| i.id == id) else { return false };
        i.facts.replace(facts) != Some(facts)
    }

    pub fn is_selected(&self, id: ItemId) -> bool {
        self.selected.contains(&id)
    }

    /// Selected ids, top row first.
    pub fn selected_ids(&self) -> Vec<ItemId> {
        self.items.iter().map(|i| i.id).filter(|id| self.selected.contains(id)).collect()
    }

    /// A click on row `index`: plain selects only it, Ctrl toggles it, Shift
    /// selects the range from the anchor (Ctrl+Shift adds the range).
    pub fn click(&mut self, index: usize, mods: Mods) {
        let Some(id) = self.items.get(index).map(|i| i.id) else { return };
        let anchor = self.anchor.and_then(|a| self.index_of(a));
        match (mods.shift, anchor) {
            (true, Some(a)) => {
                if !mods.ctrl {
                    self.selected.clear();
                }
                let (lo, hi) = (a.min(index), a.max(index));
                self.selected.extend(self.items[lo..=hi].iter().map(|i| i.id));
            }
            _ if mods.ctrl => {
                if !self.selected.remove(&id) {
                    self.selected.insert(id);
                }
                self.anchor = Some(id);
            }
            _ => {
                self.selected.clear();
                self.selected.insert(id);
                self.anchor = Some(id);
            }
        }
    }

    /// Button down on row `index`. A plain press on an already selected row
    /// keeps the selection (so the selection can be dragged out) and returns
    /// true: the caller then calls `click` on release if no drag started.
    /// Anything else is a click right away.
    pub fn press(&mut self, index: usize, mods: Mods) -> bool {
        let selected = self.items.get(index).is_some_and(|i| self.selected.contains(&i.id));
        if selected && mods == Mods::default() {
            return true;
        }
        self.click(index, mods);
        false
    }

    /// Click on empty panel space. True if something was selected.
    pub fn select_none(&mut self) -> bool {
        self.anchor = None;
        let had = !self.selected.is_empty();
        self.selected.clear();
        had
    }

    /// What Remove (or a Take out) acts on for row `index`: the selection if
    /// the row is part of it, otherwise just that row.
    pub fn targets(&self, index: usize) -> Vec<ItemId> {
        match self.items.get(index) {
            Some(i) if self.selected.contains(&i.id) => self.selected_ids(),
            Some(i) => vec![i.id],
            None => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { ctrl: false, shift: false };
    const CTRL: Mods = Mods { ctrl: true, shift: false };
    const SHIFT: Mods = Mods { ctrl: false, shift: true };
    const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true };

    fn item(files: &[&str], facts: Option<Facts>) -> Item {
        Item { id: 1, files: files.iter().map(PathBuf::from).collect(), kind: Kind::Reference, facts, pending: false }
    }

    fn file(bytes: u64) -> Option<Facts> {
        Some(Facts { first_is_dir: false, total_bytes: bytes, missing: false })
    }

    /// Five Items; row 0 is "e" (added last), row 4 is "a".
    fn five() -> (Model, Vec<ItemId>) {
        let mut m = Model::new();
        let mut ids: Vec<ItemId> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|n| m.add_top(vec![PathBuf::from(n)], Kind::Reference))
            .collect();
        ids.reverse(); // ids[i] = row i
        (m, ids)
    }

    fn sel(m: &Model) -> Vec<ItemId> {
        m.selected_ids()
    }

    #[test]
    fn a_pending_item_says_saving_until_its_files_are_filled_in() {
        let mut m = Model::new();
        let id = m.add_pending(vec!["a.png".into(), "b.png".into()], Kind::Materialized);
        let it = m.get(id).unwrap();
        assert!(it.pending && it.is_stack());
        assert_eq!(it.subtitle(), "Saving…");
        assert_eq!(m.items()[0].id, id);

        assert!(m.fill(id, vec!["b.png".into()]));
        let it = m.get(id).unwrap();
        assert!(!it.pending);
        assert_eq!(it.files, vec![PathBuf::from("b.png")]);
        assert_eq!(it.subtitle(), "PNG");
        assert!(!m.fill(id + 1, vec!["x".into()]), "unknown id");

        let plain = m.add_top(vec!["c.txt".into()], Kind::Reference);
        assert!(!m.get(plain).unwrap().pending);
    }

    #[test]
    fn newest_on_top_with_stable_increasing_ids() {
        let mut m = Model::new();
        let a = m.add_top(vec!["a".into()], Kind::Reference);
        let b = m.add_top(vec!["b".into()], Kind::Materialized);
        assert!(b > a);
        assert_eq!(m.items().iter().map(|i| i.id).collect::<Vec<_>>(), vec![b, a]);
        assert_eq!(m.get(b).unwrap().kind, Kind::Materialized);
        assert_eq!(m.index_of(a), Some(1));
        // Ids are never reused, even after a clear.
        m.clear();
        let c = m.add_top(vec!["c".into()], Kind::Reference);
        assert!(c > b);
    }

    #[test]
    fn move_to_top_and_remove() {
        let (mut m, ids) = five();
        assert!(m.move_to_top(ids[3]));
        assert_eq!(m.items()[0].id, ids[3]);
        assert_eq!(m.len(), 5);
        assert!(!m.move_to_top(999));
        assert_eq!(m.remove(&[ids[0], ids[3], 999]), 2);
        assert_eq!(m.len(), 3);
        assert_eq!(m.get(ids[0]), None);
    }

    #[test]
    fn same_files_finds_the_item_with_exactly_that_path_set() {
        let mut m = Model::new();
        let single = m.add_top(vec![r"C:\x\a.txt".into()], Kind::Reference);
        let stack = m.add_top(vec![r"C:\x\1.jpg".into(), r"C:\x\2.jpg".into(), r"C:\x\3.jpg".into()], Kind::Reference);
        let p = |v: &[&str]| v.iter().map(PathBuf::from).collect::<Vec<_>>();

        assert_eq!(m.same_files(&p(&[r"C:\x\a.txt"])), Some(single));
        // Order and case don't matter (NTFS paths are case-insensitive).
        assert_eq!(m.same_files(&p(&[r"c:\X\3.JPG", r"C:\x\1.jpg", r"C:\x\2.jpg"])), Some(stack));
        // A subset, a superset or another file is a new Park.
        assert_eq!(m.same_files(&p(&[r"C:\x\1.jpg", r"C:\x\2.jpg"])), None);
        assert_eq!(m.same_files(&p(&[r"C:\x\a.txt", r"C:\x\b.txt"])), None);
        assert_eq!(m.same_files(&p(&[r"C:\x\b.txt"])), None);
        assert_eq!(m.same_files(&[]), None);
        // Spellings of the same path match too.
        assert_eq!(m.same_files(&p(&[r"\\?\C:\x\a.txt"])), Some(single));
        assert_eq!(m.same_files(&p(&["C:/x/a.txt"])), Some(single));
        // A path listed twice in one drop is still the same set.
        assert_eq!(m.same_files(&p(&[r"C:\x\a.txt", r"C:\x\a.txt"])), Some(single));
    }

    #[test]
    fn remove_and_clear_drop_the_selection() {
        let (mut m, ids) = five();
        m.click(1, NONE);
        m.remove(&[ids[1]]);
        assert!(sel(&m).is_empty());
        m.click(0, NONE);
        m.clear();
        assert!(m.is_empty());
        assert!(sel(&m).is_empty());
    }

    #[test]
    fn facts_by_id() {
        let (mut m, ids) = five();
        let f = Facts { first_is_dir: true, total_bytes: 0, missing: false };
        assert!(m.set_facts(ids[2], f));
        assert_eq!(m.get(ids[2]).unwrap().facts, Some(f));
        assert!(!m.set_facts(ids[2], f), "same facts again: no change");
        assert!(!m.set_facts(999, f));
    }

    #[test]
    fn click_selects_one() {
        let (mut m, ids) = five();
        m.click(1, NONE);
        assert_eq!(sel(&m), vec![ids[1]]);
        m.click(3, NONE);
        assert_eq!(sel(&m), vec![ids[3]]);
    }

    #[test]
    fn ctrl_click_toggles() {
        let (mut m, ids) = five();
        m.click(1, NONE);
        m.click(3, CTRL);
        assert_eq!(sel(&m), vec![ids[1], ids[3]]);
        m.click(1, CTRL);
        assert_eq!(sel(&m), vec![ids[3]]);
        m.click(3, CTRL);
        assert!(sel(&m).is_empty());
    }

    #[test]
    fn shift_click_selects_a_range_from_the_anchor() {
        let (mut m, ids) = five();
        m.click(1, NONE);
        m.click(3, SHIFT);
        assert_eq!(sel(&m), vec![ids[1], ids[2], ids[3]]);
        // The anchor stays: a second Shift-click replaces the range.
        m.click(0, SHIFT);
        assert_eq!(sel(&m), vec![ids[0], ids[1]]);
        // Ctrl-click moves the anchor.
        m.click(4, CTRL);
        m.click(2, SHIFT);
        assert_eq!(sel(&m), vec![ids[2], ids[3], ids[4]]);
    }

    #[test]
    fn shift_click_without_anchor_selects_one() {
        let (mut m, ids) = five();
        m.click(2, SHIFT);
        assert_eq!(sel(&m), vec![ids[2]]);
    }

    #[test]
    fn ctrl_shift_click_adds_the_range() {
        let (mut m, ids) = five();
        m.click(0, NONE);
        m.click(3, CTRL);
        m.click(4, CTRL_SHIFT);
        assert_eq!(sel(&m), vec![ids[0], ids[3], ids[4]]);
    }

    #[test]
    fn shift_range_follows_the_anchor_item_when_rows_move() {
        let (mut m, ids) = five();
        m.click(2, NONE); // anchor = ids[2]
        m.add_top(vec!["new".into()], Kind::Reference); // ids[2] is now row 3
        m.click(4, SHIFT);
        assert_eq!(sel(&m), vec![ids[2], ids[3]]);
    }

    #[test]
    fn press_on_a_selected_row_defers_the_click() {
        let (mut m, ids) = five();
        m.click(1, NONE);
        m.click(2, CTRL);
        assert!(m.press(1, NONE));
        assert_eq!(sel(&m), vec![ids[1], ids[2]], "kept for a drag of the selection");
        m.click(1, NONE); // released without dragging
        assert_eq!(sel(&m), vec![ids[1]]);
        // A press elsewhere, or with a modifier, is a click at once.
        assert!(!m.press(3, NONE));
        assert_eq!(sel(&m), vec![ids[3]]);
        assert!(!m.press(3, CTRL));
        assert!(sel(&m).is_empty());
    }

    #[test]
    fn select_none_reports_a_change() {
        let (mut m, _) = five();
        assert!(!m.select_none());
        m.click(1, NONE);
        assert!(m.select_none());
        assert!(sel(&m).is_empty());
    }

    #[test]
    fn targets_are_the_selection_or_the_row() {
        let (mut m, ids) = five();
        assert_eq!(m.targets(2), vec![ids[2]]);
        m.click(0, NONE);
        m.click(3, CTRL);
        assert_eq!(m.targets(3), vec![ids[0], ids[3]]);
        assert_eq!(m.targets(1), vec![ids[1]]);
        assert!(m.targets(9).is_empty());
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 KB");
        assert_eq!(human_size(1), "1 KB");
        assert_eq!(human_size(214 * 1024), "214 KB");
        assert_eq!(human_size(1024 * 1024 - 1), "1024 KB");
        assert_eq!(human_size(1_258_291), "1.2 MB");
        assert_eq!(human_size(18 * 1024 * 1024 + 300_000), "18 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
        assert_eq!(human_size(40 * 1024 * 1024 * 1024), "40 GB");
    }

    #[test]
    fn single_file_rows() {
        let i = item(&[r"C:\shots\snap-0919.png"], file(214 * 1024));
        assert_eq!(i.title(), "snap-0919.png");
        assert_eq!(i.subtitle(), "PNG · 214 KB");
        assert!(i.is_image());
        assert!(!i.is_stack());
        let pdf = item(&[r"C:\d\Offerta_Rachello.pdf"], file(1_258_291));
        assert_eq!(pdf.subtitle(), "PDF · 1.2 MB");
        assert_eq!(pdf.ext(), "PDF");
        assert!(!pdf.is_image());
        let bare = item(&[r"C:\d\Makefile"], file(10));
        assert_eq!(bare.subtitle(), "File · 1 KB");
        assert_eq!(bare.ext(), "");
    }

    #[test]
    fn folder_row() {
        let f = item(&[r"C:\d\clips"], Some(Facts { first_is_dir: true, total_bytes: 0, missing: false }));
        assert!(f.is_folder());
        assert_eq!(f.title(), "clips");
        assert_eq!(f.subtitle(), "Folder");
    }

    #[test]
    fn stack_row() {
        let s = item(&[r"C:\p\IMG_2041.jpg", r"C:\p\IMG_2042.jpg", r"C:\p\notes.txt"], file(18 * 1024 * 1024));
        assert!(s.is_stack());
        assert_eq!(s.title(), "IMG_2041.jpg +2");
        assert_eq!(s.subtitle(), "3 items · 18 MB");
        assert!(s.is_image(), "the first file gives the tile");
    }

    #[test]
    fn missing_row() {
        let m = item(&[r"C:\gone\old.docx"], Some(Facts { first_is_dir: false, total_bytes: 0, missing: true }));
        assert!(m.missing());
        assert_eq!(m.title(), "old.docx");
        assert_eq!(m.subtitle(), "Missing");
    }

    #[test]
    fn before_the_first_probe() {
        let i = item(&[r"C:\shots\a.png"], None);
        assert!(!i.missing());
        assert!(!i.is_folder());
        assert_eq!(i.subtitle(), "PNG");
        assert_eq!(item(&[r"C:\x\a", r"C:\x\b"], None).subtitle(), "2 items");
        assert_eq!(item(&[r"C:\x\noext"], None).subtitle(), "File");
    }

    #[test]
    fn drive_root_title_falls_back_to_the_path() {
        assert_eq!(item(&[r"D:\"], None).title(), r"D:\");
    }
}
