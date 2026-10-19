//! The pure Park rules (spec §4): drop effect, the temp-path test, one Item
//! per Park, and the Item's kind. No Win32 calls, no file I/O. The duplicate
//! rule lives with the Item list (`Model::same_files`).

use std::path::{Path, PathBuf};

use windows::Win32::System::Ole::{DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_NONE};

use crate::shelf::model::{path_key, Kind};

/// The effect to answer: `COPY` if the source allows it, else `LINK`, else
/// `NONE`. Never `MOVE` (a Park never moves the source), whatever keys are
/// held. `NONE` when no reader handles what is offered, and while the Shelf's
/// own Take out runs (dropping an Item back on the Shelf is a no-op).
pub fn choose_effect(allowed: DROPEFFECT, supported: bool, own_drag: bool) -> DROPEFFECT {
    if !supported || own_drag {
        DROPEFFECT_NONE
    } else if allowed.0 & DROPEFFECT_COPY.0 != 0 {
        DROPEFFECT_COPY
    } else if allowed.0 & DROPEFFECT_LINK.0 != 0 {
        DROPEFFECT_LINK
    } else {
        DROPEFFECT_NONE
    }
}

/// `path` lies inside the temp folder `temp` but not inside `temp\Tasca` (our
/// own drop folders), so its source may delete it right after the drop and
/// the Park must copy it. Case-insensitive; `\\?\` prefixes and trailing
/// separators are ignored. Short (8.3) names must be expanded by the caller.
pub fn is_temp_path(path: &Path, temp: &Path) -> bool {
    let (path, temp) = (path_key(path), path_key(temp));
    if temp.is_empty() {
        return false;
    }
    let Some(rest) = path.strip_prefix(&temp).and_then(|r| r.strip_prefix('\\')) else { return false };
    !rest.is_empty() && rest.split('\\').next() != Some("tasca")
}

/// The files of the one Item a Park makes: one path is a single Item, several
/// are one Stack (in drop order), none is no Park at all.
pub fn item_files(paths: Vec<PathBuf>) -> Option<Vec<PathBuf>> {
    (!paths.is_empty()).then_some(paths)
}

/// A Park whose every file was copied into a drop folder made Materialized
/// files; any file kept by its own path makes the Item a Reference.
pub fn kind_for(copied: usize, total: usize) -> Kind {
    if copied > 0 && copied == total { Kind::Materialized } else { Kind::Reference }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Ole::{DROPEFFECT_MOVE, DROPEFFECT_SCROLL};

    const ALL: DROPEFFECT = DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0 | DROPEFFECT_LINK.0);

    fn eff(bits: &[DROPEFFECT]) -> DROPEFFECT {
        DROPEFFECT(bits.iter().fold(0, |a, b| a | b.0))
    }

    #[test]
    fn effect_prefers_copy_then_link_never_move() {
        assert_eq!(choose_effect(ALL, true, false), DROPEFFECT_COPY);
        assert_eq!(choose_effect(eff(&[DROPEFFECT_COPY, DROPEFFECT_MOVE]), true, false), DROPEFFECT_COPY);
        assert_eq!(choose_effect(eff(&[DROPEFFECT_MOVE, DROPEFFECT_LINK]), true, false), DROPEFFECT_LINK);
        assert_eq!(choose_effect(DROPEFFECT_LINK, true, false), DROPEFFECT_LINK);
        assert_eq!(choose_effect(DROPEFFECT_MOVE, true, false), DROPEFFECT_NONE);
        assert_eq!(choose_effect(DROPEFFECT_NONE, true, false), DROPEFFECT_NONE);
        // The scroll bit is not an effect.
        assert_eq!(choose_effect(eff(&[DROPEFFECT_SCROLL, DROPEFFECT_MOVE]), true, false), DROPEFFECT_NONE);
    }

    #[test]
    fn effect_is_none_for_unsupported_formats_and_during_own_take_out() {
        for allowed in [ALL, DROPEFFECT_COPY, DROPEFFECT_LINK] {
            assert_eq!(choose_effect(allowed, false, false), DROPEFFECT_NONE);
            assert_eq!(choose_effect(allowed, true, true), DROPEFFECT_NONE);
            assert_eq!(choose_effect(allowed, false, true), DROPEFFECT_NONE);
        }
    }

    #[test]
    fn effect_never_moves_for_any_allowed_set() {
        for bits in 0..8u32 {
            let e = choose_effect(DROPEFFECT(bits), true, false);
            assert_ne!(e.0 & DROPEFFECT_MOVE.0, DROPEFFECT_MOVE.0, "allowed {bits:#b}");
            assert!(e == DROPEFFECT_NONE || bits & e.0 == e.0, "answered an effect not allowed: {bits:#b}");
        }
    }

    const TEMP: &str = r"C:\Users\mattia\AppData\Local\Temp\";

    fn temp(p: &str) -> bool {
        is_temp_path(Path::new(p), Path::new(TEMP))
    }

    #[test]
    fn temp_paths_are_inside_the_temp_folder() {
        assert!(temp(r"C:\Users\mattia\AppData\Local\Temp\7zE1234\photo.jpg"));
        assert!(temp(r"C:\Users\mattia\AppData\Local\Temp\chrome_drag42\report.pdf"));
        assert!(temp(r"C:\Users\mattia\AppData\Local\Temp\loose.txt"));
        assert!(!temp(r"C:\Users\mattia\Documents\photo.jpg"));
        assert!(!temp(r"D:\Temp\photo.jpg"));
        // The temp folder itself is not a file in it.
        assert!(!temp(r"C:\Users\mattia\AppData\Local\Temp"));
        assert!(!temp(r"C:\Users\mattia\AppData\Local\Temp\"));
    }

    #[test]
    fn temp_rule_matches_whole_components() {
        assert!(!temp(r"C:\Users\mattia\AppData\Local\Temporary\x.txt"));
        assert!(!temp(r"C:\Users\mattia\AppData\Local\Temp2\x.txt"));
        assert!(!temp(r"C:\Users\mattia\AppData\Local\TempTasca\x.txt"));
    }

    #[test]
    fn our_own_drop_folders_are_not_temp_paths() {
        assert!(!temp(r"C:\Users\mattia\AppData\Local\Temp\Tasca\17\photo.jpg"));
        assert!(!temp(r"C:\Users\mattia\AppData\Local\Temp\tasca\17\photo.jpg"));
        assert!(!temp(r"C:\Users\mattia\AppData\Local\Temp\Tasca"));
        // Only the Tasca folder at the top of temp is ours.
        assert!(temp(r"C:\Users\mattia\AppData\Local\Temp\7zE1\Tasca\photo.jpg"));
        assert!(temp(r"C:\Users\mattia\AppData\Local\Temp\Tasca2\photo.jpg"));
    }

    #[test]
    fn temp_rule_ignores_case_prefixes_and_trailing_separators() {
        assert!(temp(r"c:\users\MATTIA\appdata\local\temp\7zE1\x.txt"));
        assert!(temp(r"\\?\C:\Users\mattia\AppData\Local\Temp\7zE1\x.txt"));
        assert!(!temp(r"\\?\C:\Users\mattia\AppData\Local\Temp\Tasca\1\x.txt"));
        assert!(is_temp_path(
            Path::new(r"C:\Users\mattia\AppData\Local\Temp\x.txt"),
            Path::new(r"\\?\C:\Users\mattia\AppData\Local\Temp")
        ));
        assert!(is_temp_path(Path::new(r"\\server\share\tmp\x.txt"), Path::new(r"\\?\UNC\server\share\tmp\")));
        assert!(is_temp_path(Path::new(r"C:/Users/mattia/AppData/Local/Temp/x.txt"), Path::new(TEMP)));
    }

    #[test]
    fn an_empty_temp_folder_matches_nothing() {
        assert!(!is_temp_path(Path::new(r"C:\x.txt"), Path::new("")));
    }

    #[test]
    fn one_path_is_one_item_several_are_one_stack() {
        assert_eq!(item_files(vec![]), None);
        assert_eq!(item_files(vec![r"C:\a.txt".into()]), Some(vec![PathBuf::from(r"C:\a.txt")]));
        let three: Vec<PathBuf> = vec![r"C:\b.jpg".into(), r"C:\a.jpg".into(), r"C:\c".into()];
        assert_eq!(item_files(three.clone()), Some(three));
    }

    #[test]
    fn an_item_is_materialized_only_if_every_file_was_copied() {
        assert_eq!(kind_for(0, 1), Kind::Reference);
        assert_eq!(kind_for(0, 3), Kind::Reference);
        assert_eq!(kind_for(1, 3), Kind::Reference);
        assert_eq!(kind_for(1, 1), Kind::Materialized);
        assert_eq!(kind_for(3, 3), Kind::Materialized);
    }
}
