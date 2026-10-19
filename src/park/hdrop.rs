//! Rule 1 (spec §4): `CF_HDROP` → References. Paths inside the temp folder are
//! copied into a drop folder first, because their source deletes them right
//! after the drop (7-Zip's `%TEMP%\7zE…`, Chromium's `chrome_drag…`).
//!
//! For rule 1 everything here runs inside `Drop`, on the UI thread: the copy
//! must finish before `Drop` returns, since the source deletes its temp files
//! the moment `DoDragDrop` returns. `CF_HDROP` is read in `Drop` only, never
//! cached from `DragEnter` (7-Zip offers a placeholder "Pre" list until the
//! button is up). Rule 2 (`async_hdrop`) runs the same `park` on a worker,
//! inside the source's async operation.

use std::io;
use std::path::{Path, PathBuf};

use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::{ReleaseStgMedium, CF_HDROP};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

use super::formats::{offers, hglobal_format};
use super::{dropdir, rules, Parked};
use crate::log::log;

/// `CF_HDROP` is offered (`QueryGetData` only: safe during the drag).
pub fn offered(obj: &IDataObject) -> bool {
    offers(obj, CF_HDROP.0)
}

/// Read the dropped paths and make the Park: temp paths copied, the rest kept
/// by path. `None` if `CF_HDROP` can't be read (a delay-rendered one: rule 2,
/// TASCA-16) or no path survives.
pub fn park(obj: &IDataObject) -> Option<Parked> {
    let paths = match read(obj) {
        Ok(p) => p,
        Err(e) => {
            log(format!("CF_HDROP unreadable at Drop: {e}"));
            return None;
        }
    };
    let (files, copied) = keep_or_copy(paths);
    let kind = rules::kind_for(copied, files.len());
    rules::item_files(files).map(|files| Parked { files, kind })
}

/// The paths in the data object's `CF_HDROP`, in drop order, in long form (a
/// source may send 8.3 names; the Shelf and the duplicate rule see one form).
fn read(obj: &IDataObject) -> windows::core::Result<Vec<PathBuf>> {
    unsafe {
        let mut m = obj.GetData(&hglobal_format(CF_HDROP.0))?;
        let hdrop = HDROP(m.u.hGlobal.0);
        let n = DragQueryFileW(hdrop, u32::MAX, None);
        let mut v = Vec::with_capacity(n as usize);
        let mut buf = Vec::new();
        for i in 0..n {
            buf.resize(DragQueryFileW(hdrop, i, None) as usize + 1, 0);
            let got = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
            if got > 0 {
                v.push(dropdir::long_name(Path::new(&String::from_utf16_lossy(&buf[..got]))));
            }
        }
        ReleaseStgMedium(&mut m);
        Ok(v)
    }
}

/// Keep each path, or copy it into one drop folder (made on first need) if it
/// is a temp path. A path whose copy fails is dropped. Returns the Item's
/// files, in drop order, and how many of them are copies.
fn keep_or_copy(paths: Vec<PathBuf>) -> (Vec<PathBuf>, usize) {
    let mut dir: Option<PathBuf> = None;
    let mut copied = 0;
    let files = paths
        .into_iter()
        .filter_map(|p| {
            if !is_temp(&p) {
                return Some(p);
            }
            match copy_into(&p, &mut dir) {
                Ok(c) => {
                    copied += 1;
                    Some(c)
                }
                Err(e) => {
                    log(format!("copying temp path {} failed: {e}", p.display()));
                    None
                }
            }
        })
        .collect();
    if let (Some(d), 0) = (dir, copied) {
        let _ = std::fs::remove_dir(d); // every copy failed: leave no empty folder
    }
    (files, copied)
}

/// `path` (long form, from `read`) is inside `GetTempPath2W` but not inside
/// `%TEMP%\Tasca`. Both forms of the temp folder count: a path that no longer
/// exists keeps its 8.3 form.
pub fn is_temp(path: &Path) -> bool {
    dropdir::temp_dirs().iter().any(|t| rules::is_temp_path(path, t))
}

/// Copy a file or folder (recursively) into the drop folder, under its own
/// name (`name (2)` on a clash). On failure nothing partial is left behind.
fn copy_into(src: &Path, dir: &mut Option<PathBuf>) -> io::Result<PathBuf> {
    let name = src.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let dir = match dir {
        Some(d) => d,
        None => dir.insert(dropdir::create()?),
    };
    let dst = dropdir::free_name(dir, name);
    let r = std::fs::metadata(src).and_then(|m| copy_tree(src, &dst, m.is_dir()));
    if r.is_err() {
        let _ = if dst.is_dir() { std::fs::remove_dir_all(&dst) } else { std::fs::remove_file(&dst) };
    }
    r.map(|()| dst)
}

/// `std::fs::copy` for a file; a folder is recreated and walked. Links inside
/// a folder are skipped (a junction loop must not hang `Drop`).
fn copy_tree(src: &Path, dst: &Path, is_dir: bool) -> io::Result<()> {
    if !is_dir {
        return std::fs::copy(src, dst).map(|_| ());
    }
    std::fs::create_dir(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            log(format!("skipped link {}", entry.path().display()));
            continue;
        }
        copy_tree(&entry.path(), &dst.join(entry.file_name()), kind.is_dir())?;
    }
    Ok(())
}
