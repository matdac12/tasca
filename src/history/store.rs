//! The History on disk: `%LOCALAPPDATA%\Tasca\history\`.
//!
//! Layout: one index file, `index.bin`, plus a blob file per large field
//! (`clip-<len>-<hash>.bin`, named by its content: images always, big text and
//! formats when over `INLINE_MAX`). The index is binary, little-endian:
//!
//! ```text
//! "TSCH"  u32 version(=2)  u32 count
//! per record, newest first:
//!   u64 id  u64 last_copied_ms  u8 pinned  u8 kind  u32 body_len  body
//! ```
//!
//! The body is length-prefixed, so a reader skips a kind it does not know and a
//! later version can append fields to a body (readers ignore trailing bytes).
//! Kinds: 1 text (text, optional html, optional rtf), 2 image (width, height,
//! png), 3 files (paths). A blob field is `u8 0, u32 len, bytes` inline or
//! `u8 1, u64 hash, u32 len` with the bytes in the blob file of that name.
//!
//! Commit order of a save: (1) new blobs are written under their content name
//! (a name is never reused for other bytes, so the committed index keeps
//! pointing at the right content until it is replaced) and flushed to disk;
//! (2) the index is written to a temp file, flushed and renamed over
//! `index.bin`; (3) only then are blobs and temp files the new index does not
//! name deleted. A crash leaves the old store or the new one.
//!
//! Anything unreadable (truncated, garbled, wrong magic or version, or over a
//! budget: index size, record count, duplicate ids) loads as empty; a Clip
//! whose blob is missing, changed or too big is dropped alone.
//! All of it runs on the worker thread, never the UI thread.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::thread;

/// Unpinned Clips kept; pinned ones are never counted or dropped.
pub const MAX_UNPINNED: usize = 100;

const MAGIC: &[u8; 4] = b"TSCH";
const VERSION: u32 = 2;
/// Budgets for reading: an index bigger than this, more records than this
/// (100 unpinned plus a sane number of pins) or a blob over `MAX_BLOB` (just
/// over the 10 MB Clip cap) is not read.
const MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RECORDS: u32 = 1000;
const MAX_BLOB: u32 = 11 * 1024 * 1024;
const INDEX: &str = "index.bin";
/// A blob field up to this size stays inside the index.
const INLINE_MAX: usize = 16 * 1024;

/// What a Clip holds. The shell converts to and from its own Clip type.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Body {
    Text { text: String, html: Option<String>, rtf: Option<String> },
    Image { width: u32, height: u32, png: Vec<u8> },
    Files { paths: Vec<String> },
}

/// One stored Clip: every field a Clip has, in the order the History shows it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Record {
    pub id: u64,
    pub last_copied_ms: u64,
    pub pinned: bool,
    pub body: Body,
}

/// `%LOCALAPPDATA%\Tasca\history`.
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Tasca").join("history"))
}

/// Keep every pinned record and the first `max_unpinned` unpinned ones; the
/// list is newest first, so the oldest unpinned fall off the end. Order kept.
pub fn cap<T>(records: Vec<T>, pinned: impl Fn(&T) -> bool, max_unpinned: usize) -> Vec<T> {
    let mut unpinned = 0;
    records
        .into_iter()
        .filter(|r| {
            if pinned(r) {
                return true;
            }
            unpinned += 1;
            unpinned <= max_unpinned
        })
        .collect()
}

// ---- encode -------------------------------------------------------------

struct Writer {
    index: Vec<u8>,
    /// Blob files to write: (file name, bytes).
    blobs: Vec<(String, Vec<u8>)>,
}

impl Writer {
    fn u8(&mut self, v: u8) {
        self.index.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.index.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.index.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.index.extend_from_slice(b);
    }
    fn blob(&mut self, b: &[u8], inline_max: usize) {
        if b.len() <= inline_max {
            self.u8(0);
            self.bytes(b);
        } else {
            let (len, h) = (b.len() as u32, hash(b));
            self.u8(1);
            self.u64(h);
            self.u32(len);
            self.blobs.push((blob_name(len, h), b.to_vec()));
        }
    }
    fn opt_blob(&mut self, b: &Option<String>) {
        match b {
            None => self.u8(0),
            Some(s) => {
                self.u8(1);
                self.blob(s.as_bytes(), INLINE_MAX);
            }
        }
    }
}

/// FNV-1a: names a blob by its content (and catches a changed blob file on load).
fn hash(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, &x| (h ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

fn blob_name(len: u32, hash: u64) -> String {
    format!("clip-{len:x}-{hash:016x}.bin")
}

fn encode(records: &[Record]) -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
    let mut w = Writer { index: Vec::new(), blobs: Vec::new() };
    w.index.extend_from_slice(MAGIC);
    w.u32(VERSION);
    w.u32(records.len() as u32);
    for r in records {
        w.u64(r.id);
        w.u64(r.last_copied_ms);
        w.u8(r.pinned as u8);
        // The body goes in its own buffer so its length can lead it.
        let outer = std::mem::take(&mut w.index);
        match &r.body {
            Body::Text { text, html, rtf } => {
                w.u8(1);
                w.blob(text.as_bytes(), INLINE_MAX);
                w.opt_blob(html);
                w.opt_blob(rtf);
            }
            Body::Image { width, height, png } => {
                w.u8(2);
                w.u32(*width);
                w.u32(*height);
                w.blob(png, 0);
            }
            Body::Files { paths } => {
                w.u8(3);
                w.u32(paths.len() as u32);
                for p in paths {
                    w.bytes(p.as_bytes());
                }
            }
        }
        let body = std::mem::replace(&mut w.index, outer);
        // `body` starts with the kind byte; split it off for the header.
        w.index.push(body[0]);
        w.bytes(&body[1..]);
    }
    (w.index, w.blobs)
}

// ---- decode -------------------------------------------------------------

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    fn string(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?.to_vec()).ok()
    }
}

/// Outcome of one record: `Err(())` is damage past repair (the rest of the
/// index cannot be trusted); `Ok(None)` is a record to skip (unknown kind, a
/// missing blob file).
type Parsed = Result<Option<Record>, ()>;

fn read_blob(r: &mut Reader, dir: &Path) -> Result<Option<Vec<u8>>, ()> {
    match r.u8().ok_or(())? {
        0 => Ok(Some(r.bytes().ok_or(())?.to_vec())),
        1 => {
            let h = r.u64().ok_or(())?;
            let len = r.u32().ok_or(())?;
            Ok(read_blob_file(dir, len, h))
        }
        _ => Err(()),
    }
}

/// The blob file's bytes, only if it is within budget and is what the index
/// says (size and hash).
fn read_blob_file(dir: &Path, len: u32, h: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    if len > MAX_BLOB {
        return None;
    }
    let file = fs::File::open(dir.join(blob_name(len, h))).ok()?;
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(len as u64 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() == len as usize && hash(&bytes) == h).then_some(bytes)
}

fn read_text_blob(r: &mut Reader, dir: &Path) -> Result<Option<String>, ()> {
    Ok(read_blob(r, dir)?.and_then(|b| String::from_utf8(b).ok()))
}

fn read_opt_text(r: &mut Reader, dir: &Path) -> Result<Option<Option<String>>, ()> {
    match r.u8().ok_or(())? {
        0 => Ok(Some(None)),
        1 => Ok(read_text_blob(r, dir)?.map(Some)),
        _ => Err(()),
    }
}

fn parse_record(r: &mut Reader, dir: &Path) -> Parsed {
    let id = r.u64().ok_or(())?;
    let last_copied_ms = r.u64().ok_or(())?;
    let pinned = match r.u8().ok_or(())? {
        0 => false,
        1 => true,
        _ => return Err(()),
    };
    let kind = r.u8().ok_or(())?;
    let body_bytes = r.bytes().ok_or(())?;
    let mut b = Reader { data: body_bytes, pos: 0 };
    let body = match kind {
        1 => {
            let text = read_text_blob(&mut b, dir)?;
            let html = read_opt_text(&mut b, dir)?;
            let rtf = read_opt_text(&mut b, dir)?;
            match (text, html, rtf) {
                (Some(text), Some(html), Some(rtf)) => Body::Text { text, html, rtf },
                _ => return Ok(None),
            }
        }
        2 => {
            let width = b.u32().ok_or(())?;
            let height = b.u32().ok_or(())?;
            match read_blob(&mut b, dir)? {
                Some(png) => Body::Image { width, height, png },
                None => return Ok(None),
            }
        }
        3 => {
            let n = b.u32().ok_or(())?;
            let mut paths = Vec::new();
            for _ in 0..n {
                paths.push(b.string().ok_or(())?);
            }
            Body::Files { paths }
        }
        // A kind from a newer version: skipped, its length let us step over it.
        _ => return Ok(None),
    };
    Ok(Some(Record { id, last_copied_ms, pinned, body }))
}

fn decode(data: &[u8], dir: &Path) -> Vec<Record> {
    let mut r = Reader { data, pos: 0 };
    if r.take(4) != Some(MAGIC) || r.u32() != Some(VERSION) {
        return Vec::new();
    }
    let Some(count) = r.u32() else { return Vec::new() };
    if count > MAX_RECORDS {
        return Vec::new();
    }
    let mut out: Vec<Record> = Vec::new();
    let mut ids = HashSet::new();
    for _ in 0..count {
        match parse_record(&mut r, dir) {
            Ok(Some(rec)) if !ids.insert(rec.id) => return Vec::new(),
            Ok(Some(rec)) => out.push(rec),
            Ok(None) => {}
            // Truncated or garbled: the whole store is distrusted.
            Err(()) => return Vec::new(),
        }
    }
    out
}

// ---- files --------------------------------------------------------------

/// Temp file, flushed to disk, then renamed over `name`.
fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let tmp = dir.join(format!("{name}.tmp"));
    let mut f = fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, dir.join(name))
}

/// Writes the store in the commit order of the module comment: blobs flushed,
/// index flushed and renamed, then reclaim every file of ours the new index no
/// longer names (removed Clips, stray `.tmp` files).
pub fn save_to(dir: &Path, records: &[Record]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let (index, blobs) = encode(records);
    let mut keep: HashSet<String> = HashSet::new();
    keep.insert(INDEX.to_string());
    for (name, bytes) in &blobs {
        // Same name, same bytes: a blob already on disk is not written again.
        let present = fs::metadata(dir.join(name)).is_ok_and(|m| m.len() == bytes.len() as u64);
        if !present {
            write_atomic(dir, name, bytes)?;
        }
        keep.insert(name.clone());
    }
    write_atomic(dir, INDEX, &index)?;
    for entry in fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let ours = name.ends_with(".tmp") || (name.starts_with("clip-") && name.ends_with(".bin"));
        if ours && !keep.contains(&name) {
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}

/// Reads the store. Missing, partial or garbled is an empty History.
pub fn load_from(dir: &Path) -> Vec<Record> {
    let path = dir.join(INDEX);
    if fs::metadata(&path).map_or(true, |m| m.len() > MAX_INDEX_BYTES) {
        return Vec::new();
    }
    match fs::read(path) {
        Ok(data) => decode(&data, dir),
        Err(_) => Vec::new(),
    }
}

// ---- the worker ---------------------------------------------------------

enum Job {
    Save(Vec<Record>),
    Load(Box<dyn FnOnce(Vec<Record>) + Send>),
    /// Answered when every job before it is done.
    Flush(Sender<()>),
}

/// The worker thread that owns all disk access for one folder. Jobs run in the
/// order sent; saves queued behind a newer save are skipped (only the latest
/// state matters). The thread ends when the `Store` is dropped, after the
/// queued jobs are done.
pub struct Store {
    tx: Option<Sender<Job>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Store {
    pub fn spawn(dir: PathBuf) -> Store {
        let (tx, rx) = channel::<Job>();
        let worker = thread::spawn(move || {
            let mut pending: Option<Job> = None;
            loop {
                let job = match pending.take() {
                    Some(j) => j,
                    None => match rx.recv() {
                        Ok(j) => j,
                        Err(_) => return,
                    },
                };
                match job {
                    Job::Save(mut records) => {
                        // Coalesce: take newer saves already queued.
                        while let Ok(next) = rx.try_recv() {
                            match next {
                                Job::Save(newer) => records = newer,
                                other => {
                                    pending = Some(other);
                                    break;
                                }
                            }
                        }
                        if let Err(e) = save_to(&dir, &records) {
                            crate::log::log(format!("history: save failed: {e}"));
                        }
                    }
                    Job::Load(done) => done(load_from(&dir)),
                    Job::Flush(ack) => {
                        let _ = ack.send(());
                    }
                }
            }
        });
        Store { tx: Some(tx), worker: Some(worker) }
    }

    fn send(&self, job: Job) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(job);
        }
    }

    /// Block until every queued job (a pending load included) has run.
    pub fn flush(&self) {
        let (ack, done) = channel();
        self.send(Job::Flush(ack));
        let _ = done.recv();
    }

    /// Save these records (the caller has already capped them).
    pub fn save(&self, records: Vec<Record>) {
        self.send(Job::Save(records));
    }

    /// Load on the worker; `done` runs there with the result.
    pub fn load(&self, done: impl FnOnce(Vec<Record>) + Send + 'static) {
        self.send(Job::Load(Box::new(done)));
    }
}

impl Drop for Store {
    /// Quit: queued jobs finish before the process goes.
    fn drop(&mut self) {
        self.tx = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tasca-store-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn text(id: u64, s: &str) -> Record {
        Record {
            id,
            last_copied_ms: 1000 + id,
            pinned: false,
            body: Body::Text { text: s.to_string(), html: None, rtf: None },
        }
    }

    fn mixed() -> Vec<Record> {
        let mut pinned = text(1, "pinned ünïcode ✓");
        pinned.pinned = true;
        vec![
            text(5, "plain"),
            Record {
                id: 4,
                last_copied_ms: 77,
                pinned: false,
                body: Body::Text {
                    text: "formatted".into(),
                    html: Some("<b>formatted</b>".into()),
                    rtf: Some("x".repeat(INLINE_MAX + 1)),
                },
            },
            Record {
                id: 3,
                last_copied_ms: 5,
                pinned: true,
                body: Body::Image { width: 3, height: 2, png: vec![0, 1, 2, 255] },
            },
            Record {
                id: 2,
                last_copied_ms: u64::MAX,
                pinned: false,
                body: Body::Files { paths: vec!["C:\\a b\\c.txt".into(), "D:\\é".into()] },
            },
            pinned,
            text(0, &"big".repeat(INLINE_MAX)),
        ]
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> =
            fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn round_trip_keeps_every_field_and_the_order() {
        let dir = temp("round");
        let records = mixed();
        save_to(&dir, &records).unwrap();
        assert_eq!(load_from(&dir), records);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_store_loads_empty() {
        let dir = temp("missing");
        assert!(load_from(&dir).is_empty());
    }

    #[test]
    fn truncated_store_loads_empty_at_every_length() {
        let dir = temp("trunc");
        save_to(&dir, &mixed()).unwrap();
        let full = fs::read(dir.join(INDEX)).unwrap();
        for n in 0..full.len() {
            fs::write(dir.join(INDEX), &full[..n]).unwrap();
            assert!(load_from(&dir).is_empty(), "length {n} loaded something");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn garbled_store_loads_empty_or_survives() {
        let dir = temp("garbled");
        fs::create_dir_all(&dir).unwrap();
        for junk in [&b""[..], b"hello world", b"TSCH", b"TSCH\x01\0\0\0\xff\xff\xff\xff", &[0xff; 64]] {
            fs::write(dir.join(INDEX), junk).unwrap();
            assert!(load_from(&dir).is_empty());
        }
        // Flipping any single byte never panics.
        save_to(&dir, &mixed()).unwrap();
        let full = fs::read(dir.join(INDEX)).unwrap();
        for i in 0..full.len() {
            let mut bad = full.clone();
            bad[i] ^= 0xa5;
            fs::write(dir.join(INDEX), &bad).unwrap();
            let _ = load_from(&dir);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_newer_version_loads_empty() {
        let dir = temp("version");
        save_to(&dir, &mixed()).unwrap();
        let mut data = fs::read(dir.join(INDEX)).unwrap();
        data[4] = VERSION as u8 + 1;
        fs::write(dir.join(INDEX), &data).unwrap();
        assert!(load_from(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_blob_drops_only_its_record() {
        let dir = temp("blob");
        let records = mixed();
        save_to(&dir, &records).unwrap();
        fs::remove_file(dir.join(blob_name(4, hash(&[0, 1, 2, 255])))).unwrap();
        let loaded = load_from(&dir);
        assert_eq!(loaded.len(), records.len() - 1);
        assert!(loaded.iter().all(|r| r.id != 3));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn removed_clips_leave_no_files() {
        let dir = temp("cleanup");
        save_to(&dir, &mixed()).unwrap();
        assert!(files_in(&dir).len() > 1);
        fs::write(dir.join("stray.tmp"), b"x").unwrap();
        fs::write(dir.join("notes.txt"), b"mine").unwrap();
        save_to(&dir, &[text(9, "only")]).unwrap();
        assert_eq!(files_in(&dir), vec![INDEX.to_string(), "notes.txt".to_string()]);
        save_to(&dir, &[]).unwrap();
        assert!(load_from(&dir).is_empty());
        assert_eq!(files_in(&dir), vec![INDEX.to_string(), "notes.txt".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cap_keeps_pins_and_the_newest_unpinned() {
        let mut list: Vec<Record> = (0..105).map(|i| text(200 - i, "x")).collect();
        list[3].pinned = true; // newest-first positions 0..105
        list[104].pinned = true; // the very oldest, pinned
        let kept = cap(list.clone(), |r| r.pinned, MAX_UNPINNED);
        assert_eq!(kept.len(), 102); // 100 unpinned + 2 pins
        assert!(kept.iter().any(|r| r.id == list[104].id));
        assert!(kept.iter().any(|r| r.id == list[3].id));
        // The oldest unpinned went first: positions 100..104 are out.
        assert!(!kept.iter().any(|r| r.id == list[103].id));
        // Order is kept.
        let ids: Vec<u64> = kept.iter().map(|r| r.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(ids, sorted);
        assert_eq!(cap(Vec::<Record>::new(), |r| r.pinned, 100), vec![]);
        assert_eq!(cap(list.clone(), |r| r.pinned, 1000), list);
    }

    fn image(id: u64, byte: u8) -> Record {
        Record {
            id,
            last_copied_ms: 1,
            pinned: false,
            body: Body::Image { width: 1, height: 1, png: vec![byte; 8] },
        }
    }

    #[test]
    fn a_save_never_rewrites_a_blob_the_committed_index_names() {
        let dir = temp("immutable");
        // Clip 0 holds A; a later save gives id 0 different content B.
        save_to(&dir, &[image(0, 1)]).unwrap();
        let committed = fs::read(dir.join(INDEX)).unwrap();
        let a_name = blob_name(8, hash(&[1; 8]));
        let b_name = blob_name(8, hash(&[2; 8]));
        assert_ne!(a_name, b_name);
        // The crash window: B's blob is on disk, the index is still the old one.
        let (_, blobs) = encode(&[image(0, 2)]);
        fs::write(dir.join(&blobs[0].0), &blobs[0].1).unwrap();
        assert_eq!(fs::read(dir.join(INDEX)).unwrap(), committed);
        assert_eq!(load_from(&dir), vec![image(0, 1)]);
        save_to(&dir, &[image(0, 2)]).unwrap();
        assert_eq!(load_from(&dir), vec![image(0, 2)]);
        assert!(!dir.join(a_name).exists() && dir.join(b_name).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_blob_file_drops_its_clip() {
        let dir = temp("tamper");
        save_to(&dir, &mixed()).unwrap();
        fs::write(dir.join(blob_name(4, hash(&[0, 1, 2, 255]))), [9, 9, 9, 9]).unwrap();
        let loaded = load_from(&dir);
        assert_eq!(loaded.len(), mixed().len() - 1);
        assert!(loaded.iter().all(|r| r.id != 3));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn over_budget_stores_load_empty() {
        let dir = temp("budget");
        // Too many records.
        let many: Vec<Record> = (0..=MAX_RECORDS as u64).map(|i| text(i, "x")).collect();
        save_to(&dir, &many).unwrap();
        assert!(load_from(&dir).is_empty());
        // Duplicate ids.
        save_to(&dir, &[text(1, "a"), text(1, "b")]).unwrap();
        assert!(load_from(&dir).is_empty());
        // A blob claimed over the size budget drops its Clip, not the store
        // (the length is the last 4 bytes of the image's blob reference).
        save_to(&dir, &[text(1, "ok"), image(2, 7)]).unwrap();
        let mut data = fs::read(dir.join(INDEX)).unwrap();
        let end = data.len();
        data[end - 4..].copy_from_slice(&(1u32 << 30).to_le_bytes());
        fs::write(dir.join(INDEX), &data).unwrap();
        let loaded = load_from(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, 1);
        // An oversized index file is not read at all.
        let f = fs::File::create(dir.join(INDEX)).unwrap();
        f.set_len(MAX_INDEX_BYTES + 1).unwrap();
        drop(f);
        assert!(load_from(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flush_waits_for_a_pending_load_and_save() {
        let dir = temp("flush");
        let store = Store::spawn(dir.clone());
        let (tx, rx) = mpsc::channel();
        store.save(mixed());
        store.load(move |r| tx.send(r).unwrap());
        store.flush();
        assert_eq!(rx.try_recv().unwrap(), mixed());
        drop(store);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_worker_saves_then_loads_and_keeps_the_latest_save() {
        let dir = temp("worker");
        let store = Store::spawn(dir.clone());
        store.save(vec![text(1, "old")]);
        store.save(mixed());
        let (tx, rx) = mpsc::channel();
        store.load(move |r| tx.send(r).unwrap());
        let loaded = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert_eq!(loaded, mixed());
        drop(store);
        let _ = fs::remove_dir_all(&dir);
    }
}
