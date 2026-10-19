//! Rule 3 (spec §4): virtual files, `FileGroupDescriptorW` (else ANSI
//! `FileGroupDescriptor`) + `FileContents` at each `lindex`. Browser images
//! and links, zip folders in Explorer, Outlook attachments and mails.
//!
//! In `Drop`: the descriptor is read, every entry gets a sanitized, unique
//! name in a new drop folder (folder trees recreated; `..`, drives and
//! absolute names rejected), and the Item shows at once. The bytes are
//! written on a worker. A source in async mode keeps its data object alive
//! for the worker (`StartOperation`), which reads `FileContents` itself.
//! Otherwise each entry's medium is taken in `Drop` (bytes copied, streams
//! and storages marshalled) and the worker writes them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows::Win32::System::Com::IDataObject;
use windows::Win32::UI::Shell::{
    CFSTR_FILEDESCRIPTORA, CFSTR_FILEDESCRIPTORW, FD_ATTRIBUTES, FD_FILESIZE, FD_FLAGS, FILEDESCRIPTORA, FILEDESCRIPTORW,
};

use super::job::{self, Entry, Writes};
use super::names::{split_relative, Planner};
use super::{dropdir, formats, Park, Parked};
use crate::log::log;
use crate::shelf::model::Kind;

pub fn offered(obj: &IDataObject) -> bool {
    formats::offers_either(obj, formats::registered(CFSTR_FILEDESCRIPTORW), formats::registered(CFSTR_FILEDESCRIPTORA))
}

fn descriptor(obj: &IDataObject) -> Option<Vec<Desc>> {
    if let Some(b) = formats::hglobal_bytes(obj, formats::registered(CFSTR_FILEDESCRIPTORW)) {
        return Some(parse(&b, true));
    }
    formats::hglobal_bytes(obj, formats::registered(CFSTR_FILEDESCRIPTORA)).map(|b| parse(&b, false))
}

pub fn park(obj: &IDataObject) -> Option<Park> {
    let descs = descriptor(obj)?;
    let mut planner = Planner::default();
    // (lindex, relative path, entry)
    let plan: Vec<(i32, PathBuf, &Desc)> = descs
        .iter()
        .enumerate()
        .filter_map(|(i, d)| match split_relative(&d.name) {
            Some(parts) => Some((i as i32, planner.place(&parts, d.is_dir), d)),
            None => {
                log(format!("virtual file {:?} rejected (parent, drive or absolute path)", d.name));
                None
            }
        })
        .collect();
    if plan.is_empty() {
        return None;
    }
    let dir = dropdir::create().inspect_err(|e| log(format!("no drop folder: {e}"))).ok()?;
    let source = job::start_async(obj);
    let entries: Vec<Entry> = match source {
        Some(_) => plan
            .iter()
            .map(|(i, rel, d)| if d.is_dir { Entry::dir(dir.join(rel)) } else { Entry::contents(dir.join(rel), *i, d.size) })
            .collect(),
        None => take_now(obj, &dir, &plan),
    };
    if entries.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
        return None;
    }
    let files = tops(&dir, entries.iter().map(|e| e.path.as_path()));
    Some(Park::Planned(Parked { files: files.clone(), kind: Kind::Materialized }, Writes { dir, files, entries, source }))
}

/// Take each entry's contents now (no async operation keeps the source's
/// data alive past `Drop`). Entries that can't be read are left out.
fn take_now(obj: &IDataObject, dir: &Path, plan: &[(i32, PathBuf, &Desc)]) -> Vec<Entry> {
    plan.iter()
        .filter_map(|(i, rel, d)| {
            let path = dir.join(rel);
            if d.is_dir {
                return Some(Entry::dir(path));
            }
            job::file_contents(obj, *i)
                .and_then(|m| Entry::medium(path, m, d.size))
                .inspect_err(|e| log(format!("FileContents[{i}] ({:?}) unreadable: {e}", d.name)))
                .ok()
        })
        .collect()
}

/// The Item's files: the top-level entries of the drop folder, in order.
fn tops<'a>(dir: &Path, paths: impl Iterator<Item = &'a Path>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .filter_map(|p| p.strip_prefix(dir).ok()?.components().next())
        .filter(|first| seen.insert(*first))
        .map(|first| dir.join(first))
        .collect()
}

/// One entry of a file group descriptor.
#[derive(Debug, PartialEq, Eq)]
pub struct Desc {
    /// As given: may hold a relative path (`folder\a.txt`, zip folders).
    pub name: String,
    pub is_dir: bool,
    /// `nFileSize*`, when `FD_FILESIZE` says it is valid.
    pub size: Option<u64>,
}

/// The entries of a `FILEGROUPDESCRIPTORW` (`wide`) or `FILEGROUPDESCRIPTORA`,
/// as many as `cItems` says and the buffer holds.
pub fn parse(bytes: &[u8], wide: bool) -> Vec<Desc> {
    let Some(count) = bytes.get(..4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize) else {
        return Vec::new();
    };
    let size = if wide { size_of::<FILEDESCRIPTORW>() } else { size_of::<FILEDESCRIPTORA>() };
    bytes[4..]
        .chunks_exact(size)
        .take(count)
        .map(|c| {
            // Packed structs: read unaligned, copy the fields out.
            let (flags, attrs, high, low, name) = if wide {
                let d = unsafe { std::ptr::read_unaligned(c.as_ptr() as *const FILEDESCRIPTORW) };
                let n = d.cFileName;
                let len = n.iter().position(|&u| u == 0).unwrap_or(n.len());
                (d.dwFlags, d.dwFileAttributes, d.nFileSizeHigh, d.nFileSizeLow, String::from_utf16_lossy(&n[..len]))
            } else {
                let d = unsafe { std::ptr::read_unaligned(c.as_ptr() as *const FILEDESCRIPTORA) };
                let n = d.cFileName.map(|b| b as u8);
                (d.dwFlags, d.dwFileAttributes, d.nFileSizeHigh, d.nFileSizeLow, formats::ansi_z(&n))
            };
            let has = |f: FD_FLAGS| flags & f.0 as u32 != 0;
            Desc {
                name,
                is_dir: has(FD_ATTRIBUTES) && attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
                size: has(FD_FILESIZE).then_some((high as u64) << 32 | low as u64),
            }
        })
        .collect()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    fn as_bytes<T>(v: &T) -> &[u8] {
        unsafe { std::slice::from_raw_parts(v as *const T as *const u8, size_of::<T>()) }
    }

    /// A `FILEGROUPDESCRIPTORW`: (name, flags, attributes, size).
    pub fn group_w(entries: &[(&str, i32, u32, u64)], claimed: u32) -> Vec<u8> {
        let mut out = claimed.to_le_bytes().to_vec();
        for &(name, flags, attrs, size) in entries {
            let mut d = FILEDESCRIPTORW { dwFlags: flags as u32, dwFileAttributes: attrs, ..Default::default() };
            d.nFileSizeHigh = (size >> 32) as u32;
            d.nFileSizeLow = size as u32;
            let mut n = [0u16; 260];
            for (i, c) in name.encode_utf16().enumerate() {
                n[i] = c;
            }
            d.cFileName = n;
            out.extend_from_slice(as_bytes(&d));
        }
        out
    }

    fn group_a(entries: &[(&str, i32)]) -> Vec<u8> {
        let mut out = (entries.len() as u32).to_le_bytes().to_vec();
        for &(name, flags) in entries {
            let mut d = FILEDESCRIPTORA { dwFlags: flags as u32, ..Default::default() };
            for (i, b) in name.bytes().enumerate() {
                d.cFileName[i] = b as i8;
            }
            out.extend_from_slice(as_bytes(&d));
        }
        out
    }

    const DIR: u32 = FILE_ATTRIBUTE_DIRECTORY.0;

    #[test]
    fn wide_descriptors_give_names_folders_and_sizes() {
        let bytes = group_w(
            &[
                ("folder", FD_ATTRIBUTES.0, DIR, 0),
                (r"folder\a.txt", FD_ATTRIBUTES.0 | FD_FILESIZE.0, 0, 5_000_000_000),
                ("caffè.png", 0, DIR, 7),
            ],
            3,
        );
        assert_eq!(
            parse(&bytes, true),
            vec![
                Desc { name: "folder".into(), is_dir: true, size: None },
                Desc { name: r"folder\a.txt".into(), is_dir: false, size: Some(5_000_000_000) },
                // No FD_ATTRIBUTES / FD_FILESIZE: those fields mean nothing.
                Desc { name: "caffè.png".into(), is_dir: false, size: None },
            ]
        );
    }

    #[test]
    fn a_short_buffer_gives_only_the_entries_that_fit() {
        let bytes = group_w(&[("a.txt", 0, 0, 0), ("b.txt", 0, 0, 0)], 5);
        assert_eq!(parse(&bytes, true).len(), 2);
        assert_eq!(parse(&bytes[..bytes.len() - 1], true).len(), 1);
        assert!(parse(&[1, 0], true).is_empty());
        assert!(parse(&[], false).is_empty());
    }

    #[test]
    fn ansi_descriptors_are_read_too() {
        let bytes = group_a(&[("Re hi.msg", 0), ("photo.jpg", 0)]);
        let names: Vec<String> = parse(&bytes, false).into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["Re hi.msg", "photo.jpg"]);
    }
}
