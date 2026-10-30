//! Snippets on disk: `%LOCALAPPDATA%\Tasca\snippets\`.
//!
//! One file, `index.bin`, little-endian, newest first:
//!
//! ```text
//! "TSCS"  u32 version(=1)  u32 count
//! per snippet:
//!   u64 id  u64 updated_ms  u32 title_len  title  u32 body_len  body
//! ```
//!
//! The title and body are UTF-8. The file is written to a temp name, flushed
//! and renamed over `index.bin`, so a crash leaves the old list or the new
//! one. Anything unreadable (truncated, garbled, wrong magic or version, over
//! a budget, a duplicate id) loads as empty. All of it runs on a worker
//! thread, never the UI thread: the same shape as the History store, without
//! the blob files (a snippet is text, capped, and fits in the one file).

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::thread;

/// Snippets kept. Newest first, so the oldest fall off the end.
pub const MAX_SNIPPETS: usize = 200;
/// UTF-8 bytes in a body. A title is a short label (`text::TITLE_CHARS`).
pub const MAX_BODY: usize = 64 * 1024;
const MAX_TITLE_BYTES: usize = 512;

const MAGIC: &[u8; 4] = b"TSCS";
const VERSION: u32 = 1;
const MAX_FILE: u64 = 16 * 1024 * 1024;
const INDEX: &str = "index.bin";

/// One stored Snippet. The shell converts to and from its own type.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Record {
    pub id: u64,
    pub updated_ms: u64,
    pub title: String,
    pub body: String,
}

/// `%LOCALAPPDATA%\Tasca\snippets`.
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Tasca").join("snippets"))
}

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        let b = s.as_bytes();
        self.u32(b.len() as u32);
        self.buf.extend_from_slice(b);
    }
}

fn encode(records: &[Record]) -> Vec<u8> {
    let mut w = Writer { buf: Vec::new() };
    w.buf.extend_from_slice(MAGIC);
    w.u32(VERSION);
    w.u32(records.len() as u32);
    for r in records {
        w.u64(r.id);
        w.u64(r.updated_ms);
        w.str(&r.title);
        w.str(&r.body);
    }
    w.buf
}

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
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
    fn record(&mut self) -> Option<Record> {
        Some(Record {
            id: self.u64()?,
            updated_ms: self.u64()?,
            title: self.str()?,
            body: self.str()?,
        })
    }
}

/// `None` when the bytes are not a store we can trust. Trailing bytes are
/// ignored so a later version can append a field a reader skips by not reading.
fn decode(data: &[u8]) -> Option<Vec<Record>> {
    if data.len() as u64 > MAX_FILE {
        return None;
    }
    let mut r = Reader { data, pos: 0 };
    if r.take(4)? != MAGIC || r.u32()? != VERSION {
        return None;
    }
    let n = r.u32()?;
    if n > MAX_SNIPPETS as u32 {
        return None;
    }
    let mut out = Vec::with_capacity(n as usize);
    let mut ids = HashSet::new();
    for _ in 0..n {
        let rec = r.record()?;
        if rec.title.len() > MAX_TITLE_BYTES
            || rec.body.len() > MAX_BODY
            || rec.body.trim().is_empty()
        {
            return None;
        }
        if !ids.insert(rec.id) {
            return None;
        }
        out.push(rec);
    }
    Some(out)
}

fn load_from(dir: &Path) -> Vec<Record> {
    let path = dir.join(INDEX);
    let Ok(meta) = fs::metadata(&path) else {
        return Vec::new();
    };
    if meta.len() > MAX_FILE {
        return Vec::new();
    }
    fs::read(&path)
        .ok()
        .and_then(|b| decode(&b))
        .unwrap_or_default()
}

fn write_atomic(dir: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!("{INDEX}.tmp"));
    let mut f = fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, dir.join(INDEX))
}

/// Drop blank or over-long rows and keep the first `MAX_SNIPPETS` (newest).
fn keep(records: &[Record]) -> Vec<Record> {
    records
        .iter()
        .filter(|r| {
            r.body.trim_end().len() <= MAX_BODY
                && !r.body.trim().is_empty()
                && r.title.len() <= MAX_TITLE_BYTES
        })
        .take(MAX_SNIPPETS)
        .cloned()
        .collect()
}

fn save_to(dir: &Path, records: &[Record]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let records = keep(records);
    write_atomic(dir, &encode(&records))
}

enum Job {
    Save(Vec<Record>),
    Load(Box<dyn FnOnce(Vec<Record>) + Send>),
    Flush(Sender<()>),
}

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
                            crate::log::log(format!("snippets: save failed: {e}"));
                        }
                    }
                    Job::Load(done) => done(load_from(&dir)),
                    Job::Flush(ack) => {
                        let _ = ack.send(());
                    }
                }
            }
        });
        Store {
            tx: Some(tx),
            worker: Some(worker),
        }
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
    use std::time::Duration;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tasca-snippets-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn rec(id: u64, title: &str, body: &str) -> Record {
        Record {
            id,
            updated_ms: 1000 + id,
            title: title.into(),
            body: body.into(),
        }
    }

    fn sample() -> Vec<Record> {
        vec![
            rec(3, "Hello", "Hello\nthere ünïcode ✓"),
            rec(1, "Paths", "C:\\a b\\c.txt\nD:\\é"),
            rec(2, "", "no title stored as empty"),
        ]
    }

    #[test]
    fn round_trip_keeps_every_field_and_the_order() {
        let dir = temp("round");
        save_to(&dir, &sample()).unwrap();
        assert_eq!(load_from(&dir), sample());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_store_loads_empty() {
        assert!(load_from(&temp("missing")).is_empty());
    }

    #[test]
    fn truncated_garbled_and_newer_versions_load_empty() {
        let dir = temp("bad");
        save_to(&dir, &sample()).unwrap();
        let full = fs::read(dir.join(INDEX)).unwrap();
        for n in 0..full.len() {
            fs::write(dir.join(INDEX), &full[..n]).unwrap();
            assert!(load_from(&dir).is_empty(), "length {n}");
        }
        for junk in [
            &b""[..],
            b"hello",
            b"TSCS",
            b"TSCS\x02\0\0\0\0\0\0\0",
            &[0xff; 32],
        ] {
            fs::write(dir.join(INDEX), junk).unwrap();
            assert!(load_from(&dir).is_empty());
        }
        let mut data = full.clone();
        data[4] = VERSION as u8 + 1;
        fs::write(dir.join(INDEX), &data).unwrap();
        assert!(load_from(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_ids_over_budget_and_a_huge_body_load_empty() {
        let dir = temp("budget");
        fs::create_dir_all(&dir).unwrap();
        let mut dup = encode(&[rec(1, "a", "a"), rec(1, "b", "b")]);
        // encode would not write a duplicate; patch the second id's first byte back to 1.
        // Both records share the same prefix length, so find the second id by decoding
        // a good file and flipping is covered below. Write count=2 with two id=1 via encode
        // of distinct ids, then set the second id to the first.
        let good = encode(&[rec(1, "a", "aa"), rec(2, "b", "bb")]);
        dup.clone_from(&good);
        // version and count sit after the magic: 4 + 4 + 4, then id at byte 12, second record follows.
        let first_len = {
            let mut r = Reader {
                data: &good,
                pos: 12,
            };
            let _ = r.record();
            r.pos
        };
        dup[first_len..first_len + 8].copy_from_slice(&1u64.to_le_bytes());
        fs::write(dir.join(INDEX), &dup).unwrap();
        assert!(load_from(&dir).is_empty(), "duplicate id");

        let mut over = encode(&[]);
        over[8..12].copy_from_slice(&((MAX_SNIPPETS as u32) + 1).to_le_bytes());
        fs::write(dir.join(INDEX), &over).unwrap();
        assert!(load_from(&dir).is_empty(), "count over budget");

        save_to(&dir, &[rec(1, "t", &"x".repeat(MAX_BODY + 1))]).unwrap();
        assert!(load_from(&dir).is_empty(), "over-long body is not written");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_newest_two_hundred_are_kept() {
        let dir = temp("cap");
        let records: Vec<_> = (0..MAX_SNIPPETS + 5)
            .rev()
            .map(|id| rec(id as u64, "t", &format!("body {id}")))
            .collect();
        save_to(&dir, &records).unwrap();
        let loaded = load_from(&dir);
        assert_eq!(loaded.len(), MAX_SNIPPETS);
        assert_eq!(loaded[0].id, (MAX_SNIPPETS + 4) as u64);
        assert_eq!(loaded.last().unwrap().id, 5);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_worker_saves_and_loads() {
        let dir = temp("worker");
        let store = Store::spawn(dir.clone());
        store.save(sample());
        let (tx, rx) = mpsc::channel();
        store.save(sample());
        store.load(move |records| {
            let _ = tx.send(records);
        });
        store.flush();
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), sample());
        drop(store);
        let _ = fs::remove_dir_all(&dir);
    }
}
