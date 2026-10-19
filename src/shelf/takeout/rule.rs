//! The pure Take out rules (spec §5): which Items can be dragged, which data
//! object carries them, and whether the drag succeeded. No Win32 calls; file
//! existence comes in as a function.

use std::path::{Path, PathBuf};

use windows::Win32::System::Ole::{DROPEFFECT, DROPEFFECT_NONE};

use crate::shelf::model::{path_key, Item, ItemId, Kind};

/// How the drag loop ended, as our `IDropSource` decided it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// The left button went up.
    Drop,
    /// Esc, or the loop never reached a drop.
    Cancel,
}

/// What a finished drag reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub ending: Ending,
    /// The drop landed on the Shelf itself.
    pub on_shelf: bool,
    /// The effect `SHDoDragDrop` returned.
    pub effect: DROPEFFECT,
    /// `Logical Performed DropEffect` read back from the data object, if set.
    pub logical: Option<DROPEFFECT>,
    /// A Reference file no longer exists at its path.
    pub reference_gone: bool,
}

/// The success rule: DROP, not on the Shelf, and some sign that a target took
/// it. A same-drive Explorer move reports effect NONE with a logical MOVE.
pub fn succeeded(o: &Outcome) -> bool {
    let taken = o.effect != DROPEFFECT_NONE || o.logical.is_some_and(|e| e != DROPEFFECT_NONE) || o.reference_gone;
    o.ending == Ending::Drop && !o.on_shelf && taken
}

/// The Items to drag, in order, and the ids left out because a file is gone
/// (a Missing Item can't be Taken out; one missing file makes a Stack missing).
pub fn split_missing<'a>(items: &[&'a Item], exists: impl Fn(&Path) -> bool) -> (Vec<&'a Item>, Vec<ItemId>) {
    let (take, missing): (Vec<&Item>, Vec<&Item>) = items.iter().partition(|i| i.files.iter().all(|f| exists(f)));
    (take, missing.iter().map(|i| i.id).collect())
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

/// Some Reference file of these Items is gone (after the drop).
pub fn reference_gone(items: &[Item], exists: impl Fn(&Path) -> bool) -> bool {
    items.iter().filter(|i| i.kind == Kind::Reference).flat_map(|i| &i.files).any(|f| !exists(f))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE};

    const NONE: DROPEFFECT = DROPEFFECT_NONE;
    const COPY: DROPEFFECT = DROPEFFECT_COPY;
    const MOVE: DROPEFFECT = DROPEFFECT_MOVE;

    fn out(ending: Ending, on_shelf: bool, effect: DROPEFFECT, logical: Option<DROPEFFECT>, gone: bool) -> Outcome {
        Outcome { ending, on_shelf, effect, logical, reference_gone: gone }
    }

    #[test]
    fn success_rule_truth_table() {
        use Ending::{Cancel, Drop};
        #[rustfmt::skip]
        let rows: &[(&str, Outcome, bool)] = &[
            // ending, on Shelf, effect, logical, Reference gone => Item leaves?
            ("copy to another drive / Ctrl / Terminal",   out(Drop, false, COPY, None, false), true),
            ("browser or chat reports MOVE (uploads)",    out(Drop, false, MOVE, None, false), true),
            ("LINK effect",                               out(Drop, false, DROPEFFECT_LINK, None, false), true),
            ("same-drive Explorer move (TASCA-8)",        out(Drop, false, NONE, Some(MOVE), true), true),
            ("optimized move, logical MOVE only",         out(Drop, false, NONE, Some(MOVE), false), true),
            ("move seen only as a gone path",             out(Drop, false, NONE, None, true), true),
            ("logical COPY, effect NONE",                 out(Drop, false, NONE, Some(COPY), false), true),
            ("target refused: everything NONE",           out(Drop, false, NONE, None, false), false),
            ("logical set to NONE",                       out(Drop, false, NONE, Some(NONE), false), false),
            ("dropped on own Shelf",                      out(Drop, true, NONE, None, false), false),
            ("on own Shelf, even if signs say taken",     out(Drop, true, COPY, Some(MOVE), true), false),
            ("Esc mid-drag",                              out(Cancel, false, NONE, None, false), false),
            ("Esc, even with a gone path",                out(Cancel, false, NONE, None, true), false),
            ("cancel with stale effect and logical",      out(Cancel, false, COPY, Some(MOVE), false), false),
            ("cancel on the Shelf",                       out(Cancel, true, NONE, None, false), false),
        ];
        for (name, o, want) in rows {
            assert_eq!(succeeded(o), *want, "{name}: {o:?}");
        }
    }

    fn item(id: ItemId, kind: Kind, files: &[&str]) -> Item {
        Item { id, files: files.iter().map(PathBuf::from).collect(), kind, facts: None }
    }

    #[test]
    fn missing_items_are_not_dragged() {
        let a = item(1, Kind::Reference, &[r"C:\x\a.txt"]);
        let gone = item(2, Kind::Reference, &[r"C:\x\gone.txt"]);
        let stack = item(3, Kind::Reference, &[r"C:\x\b.txt", r"C:\x\gone2.txt"]);
        let m = item(4, Kind::Materialized, &[r"C:\t\c.png"]);
        let exists = |p: &Path| !p.to_string_lossy().contains("gone");

        let (take, missing) = split_missing(&[&a, &gone, &stack, &m], exists);
        assert_eq!(take.iter().map(|i| i.id).collect::<Vec<_>>(), [1, 4]);
        assert_eq!(missing, [2, 3], "one gone file makes the whole Stack missing");

        let (take, missing) = split_missing(&[&gone], exists);
        assert!(take.is_empty());
        assert_eq!(missing, [2]);
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

        let (take, missing) = split_missing(&[&back, &gone], |p| p.exists());
        assert_eq!(take.iter().map(|i| i.id).collect::<Vec<_>>(), [1]);
        assert_eq!(missing, [2]);
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
        let exists = |p: &Path| !p.to_string_lossy().contains("moved");
        assert!(reference_gone(&[r], exists));
        assert!(!reference_gone(&[m], exists), "a Materialized file is covered by the effects");
        assert!(!reference_gone(&[item(3, Kind::Reference, &[r"C:\x\a.txt"])], exists));
    }
}
