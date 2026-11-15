//! The newest verification record of each task, held between reads.
//!
//! `evidence` has no index on `task_id` (dc-store's schema, which GitPulse does
//! not own), so finding a task's newest `work.verify.task` row means walking the
//! table. The driver asks every three seconds and the board on every refresh,
//! and on a profile with 328,450 evidence rows that walk, repeated over a table
//! nobody had written to, kept one thread busy while the app was idle.
//!
//! What is held is exact, or it is not served. The memo owns one read-only
//! connection per store file and asks it `PRAGMA data_version` before every
//! answer (see `workbench::store_probe`): the counter moves whenever any
//! *other* connection commits, in this process or another, and the memo never
//! writes, so every commit to the file is another connection's. A held answer
//! is served only while the counter is the one it was read under:
//!
//! * the counter is read **before** the walk and again **after** it, and the
//!   answer is held only if the two are equal, so a commit that lands during the
//!   walk is never held under the counter it was not read at;
//! * a file **replaced** under the path (another device or inode) drops the
//!   connection and the answer, because a connection keeps reading the file it
//!   opened;
//! * a read that **fails** leaves nothing held, and the connection is reopened;
//! * a store with **no file** (in memory), or one the memo cannot open
//!   read-only, is read through the caller's own connection and never held.
//!
//! There is no age limit and no "recently written" rule: both existed to cover
//! a gap in a stamp of the files' sizes and times (a commit's frames reach the
//! log before the index that publishes them), and the counter has no such gap.
//! The connection holds no read transaction between calls, so it never pins the
//! write-ahead log against a checkpoint.
//!
//! The walk goes **newest first** and keeps the first row it sees for each task,
//! so a task's record is copied once and nothing is replaced, rowid order
//! backwards needs no sort, and a walk for particular tasks stops as soon as it
//! has found them all. A task that was never verified cannot be proven absent
//! without reading the whole table; once that is done the answer is held, and
//! so is the absence.
//!
//! What is held is bounded: one byte budget across every store this memo holds
//! ([`MAX_BYTES`], counting keys and per-entry overhead). A walk that does not
//! fit keeps only the tasks asked about; if even those do not fit, nothing is
//! held and each call reads.
//!
//! A change is not applied incrementally. Rows above the highest id already
//! seen would be all that is new if `evidence` were append-only, and no writer
//! in DevCouncil, Manvi, Jarvis or this app deletes or rewrites one, but the
//! schema does not forbid it and no check short of reading the rows tells an
//! append from a rewrite. A held answer that can differ from a direct query is
//! not exact, so a change is read again.

use crate::workbench::store_probe::{ChangeProbe, FileId};
use dc_store::Store;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::{parse_record, Record};

/// Bytes of held answers across every store one memo holds. The target machine
/// has 8 GB; a verdict record is a few hundred bytes to a few kilobytes, so this
/// holds tens of thousands of tasks, far more than a profile has.
pub(crate) const MAX_BYTES: usize = 16 * 1024 * 1024;
/// Store files one memo keeps a connection to.
const MAX_STORES: usize = 4;
/// What each held task costs beyond its text: the map slot, the owned strings'
/// headers and allocator slack.
const ENTRY_OVERHEAD: usize = 256;
/// How long the memo's connection waits on a lock another connection holds.
const BUSY: Duration = Duration::from_millis(250);

/// The newest `work.verify.task` row of the walk, read: a record, or why the
/// newest row could not be.
type Entry = Result<Record, String>;

/// Newest first, so the first row of a task is its newest. `id` is the rowid:
/// no sort.
pub(crate) const WALK_SQL: &str =
    "SELECT task_id, data_json FROM evidence WHERE type = ?1 ORDER BY id DESC";

/// What the walk found, and what it can vouch for.
struct Walked {
    known: HashMap<String, Entry>,
    /// Asked about, and proven to have no record. Empty unless the walk read
    /// the whole table.
    absent: HashSet<String>,
    /// `known` holds every task that has a record, so a task not in it has none.
    complete: bool,
    bytes: usize,
}

/// What is held for one store, and the counter it was read under.
struct Held {
    version: i64,
    known: HashMap<String, Entry>,
    absent: HashSet<String>,
    complete: bool,
    bytes: usize,
}

impl Held {
    /// The answer for `wanted`, when everything asked is known.
    fn answer(&self, wanted: &HashSet<&str>) -> Option<HashMap<String, Entry>> {
        let mut found = HashMap::new();
        for task in wanted {
            match self.known.get(*task) {
                Some(entry) => {
                    found.insert((*task).to_owned(), entry.clone());
                }
                None if self.complete || self.absent.contains(*task) => {}
                None => return None,
            }
        }
        Some(found)
    }
}

/// One store file: the connection that watches it and what it last answered.
struct Slot {
    probe: Option<ChangeProbe>,
    held: Option<Held>,
    /// Bytes of `held` charged to the memo's budget.
    charged: usize,
    used: Arc<AtomicUsize>,
    max_bytes: usize,
}

impl Slot {
    /// Replaces what is held, charging the memo's budget; what does not fit is
    /// not held.
    fn hold(&mut self, held: Option<Held>) {
        let new = held.as_ref().map_or(0, |held| held.bytes);
        let old = self.charged;
        let max = self.max_bytes;
        let fits = self
            .used
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                let others = used.saturating_sub(old);
                (others + new <= max).then_some(others + new)
            })
            .is_ok();
        if fits {
            self.charged = new;
            self.held = held;
        } else {
            self.used.fetch_sub(old, Ordering::SeqCst);
            self.charged = 0;
            self.held = None;
        }
    }

    fn reset(&mut self) {
        self.hold(None);
        self.probe = None;
    }

    /// `wanted`'s newest records, read through the caller's own connection and
    /// held for nobody: the fallback for a store the memo cannot watch.
    fn read_direct(
        &mut self,
        store: &Store,
        wanted: &HashSet<&str>,
    ) -> Result<HashMap<String, Entry>, String> {
        self.reset();
        walk(store.connection(), wanted, self.max_bytes).map(|walked| walked.found(wanted))
    }

    fn resolve(
        &mut self,
        path: &Path,
        store: &Store,
        wanted: &HashSet<&str>,
    ) -> Result<HashMap<String, Entry>, String> {
        let Some(now) = FileId::at(path) else {
            return self.read_direct(store, wanted);
        };
        if self
            .probe
            .as_ref()
            .is_some_and(|probe| probe.opened_as().replaced_by(&now))
        {
            self.reset();
        }
        if self.probe.is_none() {
            let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
            match ChangeProbe::open(path, flags, BUSY) {
                Ok(probe) => self.probe = Some(probe),
                Err(_) => return self.read_direct(store, wanted),
            }
        }
        let before = match self.probe.as_ref().map(ChangeProbe::data_version) {
            Some(Ok(version)) => version,
            _ => return self.read_direct(store, wanted),
        };
        let served = match &self.held {
            Some(held) if held.version == before => held.answer(wanted),
            _ => None,
        };
        if let Some(found) = served {
            return Ok(found);
        }
        if self
            .held
            .as_ref()
            .is_some_and(|held| held.version != before)
        {
            self.hold(None);
        }
        let walked = match self
            .probe
            .as_ref()
            .map(|probe| walk(probe.connection(), wanted, self.max_bytes))
        {
            Some(Ok(walked)) => walked,
            Some(Err(error)) => {
                self.reset();
                return Err(error);
            }
            None => return self.read_direct(store, wanted),
        };
        let found = walked.found(wanted);
        let after = self
            .probe
            .as_ref()
            .and_then(|probe| probe.data_version().ok());
        if after != Some(before) {
            // Something committed while the walk ran. The answer is what the
            // walk read; it is not held under a counter it was not read at.
            self.hold(None);
            return Ok(found);
        }
        let next = match self.held.take() {
            Some(mut held) if !walked.complete && held.version == before => {
                // The same version again: what both read is one state of the
                // store, so the two merge.
                held.known.extend(walked.known);
                held.absent.extend(walked.absent);
                held.bytes = held.bytes.saturating_add(walked.bytes);
                held
            }
            _ => Held {
                version: before,
                known: walked.known,
                absent: walked.absent,
                complete: walked.complete,
                bytes: walked.bytes,
            },
        };
        self.hold(Some(next));
        Ok(found)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.used.fetch_sub(self.charged, Ordering::SeqCst);
    }
}

impl Walked {
    /// What was asked about that has a record.
    fn found(&self, wanted: &HashSet<&str>) -> HashMap<String, Entry> {
        wanted
            .iter()
            .filter_map(|task| {
                self.known
                    .get(*task)
                    .map(|entry| ((*task).to_owned(), entry.clone()))
            })
            .collect()
    }
}

/// The verdicts of the tasks of the workbench profiles it has been asked
/// about. The host has one ([`Memo::shared`]); a test builds its own.
pub(crate) struct Memo {
    slots: Mutex<Vec<(PathBuf, Arc<Mutex<Slot>>)>>,
    used: Arc<AtomicUsize>,
    max_bytes: usize,
}

impl Memo {
    pub(crate) fn new() -> Self {
        Self::with_budget(MAX_BYTES)
    }

    pub(crate) fn with_budget(max_bytes: usize) -> Self {
        Self {
            slots: Mutex::new(Vec::new()),
            used: Arc::new(AtomicUsize::new(0)),
            max_bytes,
        }
    }

    /// The host's memo: the driver, the board and the gate share it.
    pub(crate) fn shared() -> &'static Memo {
        static SHARED: OnceLock<Memo> = OnceLock::new();
        SHARED.get_or_init(Memo::new)
    }

    /// Bytes held across every store.
    #[cfg(test)]
    pub(crate) fn held_bytes(&self) -> usize {
        self.used.load(Ordering::SeqCst)
    }

    /// Each of `item_ids`' newest verification record, absent for a task that
    /// has none. One snapshot answers every id. An unreadable newest record is
    /// an error for the whole call, as a verdict nobody could read is not the
    /// older one before it.
    pub(crate) fn latest_many(
        &self,
        store: &Store,
        item_ids: &[String],
    ) -> Result<HashMap<String, Record>, String> {
        if item_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let wanted: HashSet<&str> = item_ids.iter().map(String::as_str).collect();
        let mut records = HashMap::new();
        for (task, entry) in self.resolve(store, &wanted)? {
            records.insert(task, entry?);
        }
        Ok(records)
    }

    /// One task's newest verification record.
    pub(crate) fn latest(&self, store: &Store, item_id: &str) -> Result<Option<Record>, String> {
        let wanted: HashSet<&str> = std::iter::once(item_id).collect();
        self.resolve(store, &wanted)?.remove(item_id).transpose()
    }

    fn resolve(
        &self,
        store: &Store,
        wanted: &HashSet<&str>,
    ) -> Result<HashMap<String, Entry>, String> {
        let Some(path) = file_of(store) else {
            return walk(store.connection(), wanted, self.max_bytes)
                .map(|walked| walked.found(wanted));
        };
        let slot = self.slot_for(path);
        let mut slot = crate::supervisor::lock_or_recover(&slot);
        slot.resolve(path, store, wanted)
    }

    fn slot_for(&self, path: &Path) -> Arc<Mutex<Slot>> {
        let mut slots = crate::supervisor::lock_or_recover(&self.slots);
        if let Some(at) = slots.iter().position(|(held, _)| held == path) {
            let entry = slots.remove(at);
            let slot = Arc::clone(&entry.1);
            slots.push(entry);
            return slot;
        }
        if slots.len() >= MAX_STORES {
            slots.remove(0);
        }
        let slot = Arc::new(Mutex::new(Slot {
            probe: None,
            held: None,
            charged: 0,
            used: Arc::clone(&self.used),
            max_bytes: self.max_bytes,
        }));
        slots.push((path.to_path_buf(), Arc::clone(&slot)));
        slot
    }
}

/// The file a store was opened on, when it has one.
fn file_of(store: &Store) -> Option<&Path> {
    store
        .connection()
        .path()
        .filter(|path| !path.is_empty())
        .map(Path::new)
}

/// What `entry` and its key cost to hold.
fn weight(task: &str, entry: &Entry) -> usize {
    ENTRY_OVERHEAD
        + task.len()
        + match entry {
            Ok(record) => record.heap_bytes(),
            Err(why) => why.len(),
        }
}

/// One pass over the verification rows, newest first, keeping the first row
/// seen for each task. The record text is read, and parsed, only for a row that
/// is kept.
///
/// It keeps every task while that fits in `max_bytes`, so the answer can say
/// that a task not in it has none; past that it keeps only the tasks in
/// `wanted`. It stops as soon as every task in `wanted` is found.
fn walk(conn: &Connection, wanted: &HashSet<&str>, max_bytes: usize) -> Result<Walked, String> {
    #[cfg(test)]
    seam::walked(conn);
    let unreadable =
        |e: rusqlite::Error| format!("the verification evidence could not be read: {e}");
    let mut statement = conn.prepare(WALK_SQL).map_err(unreadable)?;
    let mut rows = statement
        .query(rusqlite::params![super::EVIDENCE_TYPE])
        .map_err(unreadable)?;
    let mut known: HashMap<String, Entry> = HashMap::new();
    let mut bytes = 0usize;
    let mut keep_all = true;
    let mut found_wanted = 0usize;
    let mut stopped_early = false;
    #[cfg(test)]
    let (mut looked_at, mut copied) = (0usize, 0usize);
    while let Some(row) = rows.next().map_err(unreadable)? {
        #[cfg(test)]
        {
            looked_at += 1;
        }
        let ValueRef::Text(task) = row.get_ref(0).map_err(unreadable)? else {
            continue;
        };
        let task = std::str::from_utf8(task)
            .map_err(|e| format!("the verification evidence could not be read: {e}"))?;
        if known.contains_key(task) {
            continue;
        }
        let is_wanted = wanted.contains(task);
        if !keep_all && !is_wanted {
            continue;
        }
        let data = row
            .get_ref(1)
            .map_err(unreadable)?
            .as_str()
            .map_err(|e| format!("the verification evidence could not be read: {e}"))?;
        let entry = parse_record(data);
        #[cfg(test)]
        {
            copied += 1;
        }
        bytes = bytes.saturating_add(weight(task, &entry));
        known.insert(task.to_owned(), entry);
        if is_wanted {
            found_wanted += 1;
            if found_wanted == wanted.len() {
                stopped_early = true;
                break;
            }
        }
        if keep_all && bytes > max_bytes {
            keep_all = false;
            known.retain(|task, _| wanted.contains(task.as_str()));
            bytes = known.iter().map(|(task, entry)| weight(task, entry)).sum();
        }
    }
    drop(rows);
    drop(statement);
    #[cfg(test)]
    seam::finished(conn, looked_at, copied);
    let absent = if stopped_early {
        HashSet::new()
    } else {
        wanted
            .iter()
            .filter(|task| !known.contains_key(**task))
            .map(|task| (*task).to_owned())
            .collect()
    };
    bytes = bytes.saturating_add(absent.iter().map(|task| task.len() + ENTRY_OVERHEAD).sum());
    Ok(Walked {
        known,
        absent,
        complete: keep_all && !stopped_early,
        bytes,
    })
}

/// What the tests count: how often a store file's evidence was walked, how many
/// rows a walk looked at and how many of them it copied out.
#[cfg(test)]
pub(crate) mod seam {
    use super::*;

    /// macOS names a temp directory `/private/var` through one door and `/var`
    /// through another.
    fn key(path: &Path) -> PathBuf {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }

    static COUNTS: Mutex<Option<HashMap<(PathBuf, &'static str), usize>>> = Mutex::new(None);

    fn bump(conn: &Connection, what: &'static str, by: usize) {
        if let Some(path) = conn.path().filter(|path| !path.is_empty()) {
            *crate::supervisor::lock_or_recover(&COUNTS)
                .get_or_insert_with(HashMap::new)
                .entry((key(Path::new(path)), what))
                .or_default() += by;
        }
    }

    pub(super) fn walked(conn: &Connection) {
        bump(conn, "walks", 1);
    }

    /// Once per walk, not once per row: a per-row count would cost more than
    /// the walk and distort a measurement of it.
    pub(super) fn finished(conn: &Connection, rows: usize, kept: usize) {
        bump(conn, "rows", rows);
        bump(conn, "kept", kept);
    }

    fn count(path: &Path, what: &'static str) -> usize {
        crate::supervisor::lock_or_recover(&COUNTS)
            .as_ref()
            .and_then(|counts| counts.get(&(key(path), what)).copied())
            .unwrap_or(0)
    }

    /// Walks of `path`'s evidence table so far.
    pub(crate) fn walks(path: &Path) -> usize {
        count(path, "walks")
    }

    /// Rows those walks looked at.
    pub(crate) fn rows(path: &Path) -> usize {
        count(path, "rows")
    }

    /// Rows those walks copied out of SQLite.
    pub(crate) fn kept_rows(path: &Path) -> usize {
        count(path, "kept")
    }
}
