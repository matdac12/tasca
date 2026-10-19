//! The pure Take out rules (spec §5): which Items can be dragged, which data
//! object carries them, and whether the drag succeeded. No Win32 calls; file
//! existence comes in as a function.

use std::path::{Path, PathBuf};

use windows::Win32::System::Ole::{DROPEFFECT, DROPEFFECT_NONE};

use crate::shelf::model::{path_key, Item, Kind};

/// How the drag loop ended, as our `IDropSource` decided it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// The left button went up, on the Shelf itself or elsewhere.
    Drop { on_shelf: bool },
    /// Esc, or the loop never reached a drop.
    Cancel,
}

/// What a finished drag reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub ending: Ending,
    /// The effect `SHDoDragDrop` returned.
    pub effect: DROPEFFECT,
    /// `Logical Performed DropEffect` read back from the data object, if set.
    pub logical: Option<DROPEFFECT>,
}

/// The success rule: DROP, not on the Shelf, and some sign that a target took
/// it: an effect, a logical effect, or a Reference file gone (asked only when
/// nothing else decides). A same-drive Explorer move reports effect NONE with
/// a logical MOVE.
pub fn succeeded(o: &Outcome, reference_gone: impl FnOnce() -> bool) -> bool {
    o.ending == Ending::Drop { on_shelf: false }
        && (o.effect != DROPEFFECT_NONE || o.logical.is_some_and(|e| e != DROPEFFECT_NONE) || reference_gone())
}

/// The Items to drag, in order, and the ones left out because a file is gone
/// (a Missing Item can't be Taken out; one missing file makes a Stack missing).
pub fn split_missing(items: &[Item], exists: impl Fn(&Path) -> bool) -> (Vec<&Item>, Vec<&Item>) {
    items.iter().partition(|i| i.files.iter().all(|f| exists(f)))
}

/// All files share one parent folder, so the shell item array can make the
/// data object (`BHID_DataObject` is only for flat arrays).
pub fn one_folder(paths: &[PathBuf]) -> bool {
    let mut parents = paths.iter().map(|p| p.parent().map(path_key));
    match parents.next() {
        Some(Some(first)) => parents.all(|p| p.as_ref() == Some(&first)),
        _ => false,
    }
}

/// The files to put on the data object, in order. Items can share a file (a
/// Stack and a single Item of one of its files): each path goes on once, or
/// Explorer would move it and then fail on the second copy.
pub fn drag_paths(items: &[&Item]) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    items.iter().flat_map(|i| &i.files).filter(|f| seen.insert(path_key(f))).cloned().collect()
}

/// Some Reference file of these Items is gone (after the drop). `gone` must
/// be sure: a share that doesn't answer is not a moved file.
pub fn reference_gone(items: &[&Item], gone: impl Fn(&Path) -> bool) -> bool {
    items.iter().filter(|i| i.kind == Kind::Reference).flat_map(|i| &i.files).any(|f| gone(f))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shelf::model::ItemId;
    use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE};

    const NONE: DROPEFFECT = DROPEFFECT_NONE;
    const COPY: DROPEFFECT = DROPEFFECT_COPY;
    const MOVE: DROPEFFECT = DROPEFFECT_MOVE;

    fn out(ending: Ending, effect: DROPEFFECT, logical: Option<DROPEFFECT>) -> Outcome {
        Outcome { ending, effect, logical }
    }

    #[test]
    fn success_rule_truth_table() {
        const OUT: Ending = Ending::Drop { on_shelf: false };
        const SHELF: Ending = Ending::Drop { on_shelf: true };
        const ESC: Ending = Ending::Cancel;
        #[rustfmt::skip]
        let rows: &[(&str, Outcome, bool, bool)] = &[
            // ending, effect, logical; Reference gone => Item leaves?
            ("copy to another drive / Ctrl / Terminal",   out(OUT, COPY, None), false, true),
            ("browser or chat reports MOVE (uploads)",    out(OUT, MOVE, None), false, true),
            ("LINK effect",                               out(OUT, DROPEFFECT_LINK, None), false, true),
            ("same-drive Explorer move (TASCA-8)",        out(OUT, NONE, Some(MOVE)), true, true),
            ("optimized move, logical MOVE only",         out(OUT, NONE, Some(MOVE)), false, true),
            ("move seen only as a gone path",             out(OUT, NONE, None), true, true),
            ("logical COPY, effect NONE",                 out(OUT, NONE, Some(COPY)), false, true),
            ("target refused: everything NONE",           out(OUT, NONE, None), false, false),
            ("logical set to NONE",                       out(OUT, NONE, Some(NONE)), false, false),
            ("dropped on own Shelf",                      out(SHELF, NONE, None), false, false),
            ("on own Shelf, even if signs say taken",     out(SHELF, COPY, Some(MOVE)), true, false),
            ("Esc mid-drag",                              out(ESC, NONE, None), false, false),
            ("Esc, even with a gone path",                out(ESC, NONE, None), true, false),
            ("cancel with stale effect and logical",      out(ESC, COPY, Some(MOVE)), false, false),
        ];
        for (name, o, gone, want) in rows {
            assert_eq!(succeeded(o, || *gone), *want, "{name}: {o:?}, gone {gone}");
        }
    }

    #[test]
    fn the_disk_is_asked_only_when_nothing_else_decides() {
        let asked = std::cell::Cell::new(0);
        let gone = || {
            asked.set(asked.get() + 1);
            true
        };
        assert!(!succeeded(&out(Ending::Cancel, NONE, None), gone));
        assert!(!succeeded(&out(Ending::Drop { on_shelf: true }, NONE, None), gone));
        assert!(succeeded(&out(Ending::Drop { on_shelf: false }, COPY, None), gone));
        assert!(succeeded(&out(Ending::Drop { on_shelf: false }, NONE, Some(MOVE)), gone));
        assert_eq!(asked.get(), 0);
        assert!(succeeded(&out(Ending::Drop { on_shelf: false }, NONE, None), gone));
        assert_eq!(asked.get(), 1);
    }

    fn item(id: ItemId, kind: Kind, files: &[&str]) -> Item {
        Item { id, files: files.iter().map(PathBuf::from).collect(), kind, facts: None }
    }

    fn ids(items: &[&Item]) -> Vec<ItemId> {
        items.iter().map(|i| i.id).collect()
    }

    #[test]
    fn missing_items_are_not_dragged() {
        let a = item(1, Kind::Reference, &[r"C:\x\a.txt"]);
        let gone = item(2, Kind::Reference, &[r"C:\x\gone.txt"]);
        let stack = item(3, Kind::Reference, &[r"C:\x\b.txt", r"C:\x\gone2.txt"]);
        let m = item(4, Kind::Materialized, &[r"C:\t\c.png"]);
        let exists = |p: &Path| !p.to_string_lossy().contains("gone");

        let items = [a, gone, stack, m];
        let (take, missing) = split_missing(&items, exists);
        assert_eq!(ids(&take), [1, 4]);
        assert_eq!(ids(&missing), [2, 3], "one gone file makes the whole Stack missing");

        let (take, missing) = split_missing(&items[1..2], exists);
        assert!(take.is_empty());
        assert_eq!(ids(&missing), [2]);
    }

    #[test]
    fn missing_is_rechecked_on_the_disk_not_taken_from_old_facts() {
        let dir = std::env::temp_dir().join(format!("tasca-takeout-rule-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("here.txt");
        std::fs::write(&real, b"x").unwrap();
        let mut back = item(1, Kind::Reference, &[real.to_str().unwrap()]);
        // The last probe said Missing, but the file is back.
        back.facts = Some(crate::shelf::model::Facts { missing: true, ..Default::default() });
        let gone = item(2, Kind::Reference, &[dir.join("not-here.txt").to_str().unwrap()]);

        let items = [back, gone];
        let (take, missing) = split_missing(&items, |p| p.exists());
        assert_eq!(ids(&take), [1]);
        assert_eq!(ids(&missing), [2]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn one_folder_compares_parents_like_the_shelf_compares_paths() {
        let p = |v: &[&str]| v.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert!(one_folder(&p(&[r"C:\x\a.txt"])));
        assert!(one_folder(&p(&[r"C:\x\a.txt", r"c:\X\b.txt", r"C:/x/c.txt"])));
        assert!(one_folder(&p(&[r"C:\x\sub", r"C:\x\a.txt"])));
        assert!(!one_folder(&p(&[r"C:\x\a.txt", r"C:\y\b.txt"])));
        assert!(!one_folder(&p(&[r"C:\x\a.txt", r"C:\x\sub\b.txt"])));
        assert!(!one_folder(&p(&[r"C:\x\a.txt", r"D:\x\a.txt"])));
        // A drive root has no parent folder to bind through.
        assert!(!one_folder(&p(&[r"C:\"])));
        assert!(!one_folder(&[]));
    }

    #[test]
    fn only_reference_paths_count_as_gone() {
        let r = item(1, Kind::Reference, &[r"C:\x\a.txt", r"C:\x\moved.txt"]);
        let m = item(2, Kind::Materialized, &[r"C:\t\moved.png"]);
        let gone = |p: &Path| p.to_string_lossy().contains("moved");
        assert!(reference_gone(&[&r], gone));
        assert!(!reference_gone(&[&m], gone), "a Materialized file is covered by the effects");
        assert!(!reference_gone(&[&item(3, Kind::Reference, &[r"C:\x\a.txt"])], gone));
    }

    #[test]
    fn a_file_shared_by_two_dragged_items_goes_on_once() {
        let stack = item(1, Kind::Reference, &[r"C:\x\a.txt", r"C:\x\b.txt"]);
        let single = item(2, Kind::Reference, &[r"c:\X\A.txt"]);
        let other = item(3, Kind::Reference, &[r"C:\x\c.txt"]);
        assert_eq!(drag_paths(&[&stack, &single, &other]), [r"C:\x\a.txt", r"C:\x\b.txt", r"C:\x\c.txt"].map(PathBuf::from));
    }
}
