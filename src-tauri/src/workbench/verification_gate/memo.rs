//! The newest verification record of every task, held between reads.
//!
//! `evidence` has no index on `task_id` (dc-store's schema, which GitPulse does
//! not own), so finding a task's newest `work.verify.task` row is a walk of the
//! whole table. The driver asks every three seconds and the board on every
//! refresh, and on a profile with 328,450 evidence rows that walk, repeated over
//! a table nobody had written to, kept one thread busy while the app was idle.
//!
//! One walk is kept: the newest record of *every* task, not only of the tasks
//! asked about, so the driver (a long-lived connection), the board (a fresh
//! read-only connection per call) and a task sheet are all answered from it, and
//! a task nobody asked about before costs nothing.
//!
//! An answer is served from memory only while it is exact, and every other case
//! is a read:
//!
//! * **Any commit by anyone** (this process, Manvi, the `dcstore` CLI, a
//!   retention pass) appends to the write-ahead log or rewrites the database
//!   file, so the pair's size and modification time move. SQLite's
//!   `data_version` cannot stand in for that here: it is per connection, and
//!   the board opens a new connection for every read, so there is nothing to
//!   compare it with. DevCouncil's reader holds one connection and uses
//!   `data_version` plus the connection's own `total_changes()`; a held answer
//!   shared across connections needs a signal every connection can see, which
//!   is the files.
//! * **A store file replaced or migrated** shows as another device and inode,
//!   or another size.
//! * **A write too recent to trust.** A commit appends its frames to the log
//!   before SQLite's shared index says they are visible, so a read that follows
//!   the first moves of a commit can see the old rows under the new stamp. A
//!   store is held only if neither file had been modified in the last
//!   [`RACY`], the rule git applies to its index; a store being written to is
//!   simply read.
//! * **Damage no stamp shows** (an in-place overwrite that keeps the size and
//!   the stamp, a clock that stepped back) is bounded by [`MAX_AGE`]: a held
//!   answer is never older than that.
//! * **A read that failed** leaves nothing behind. The slot is emptied before
//!   the read starts and filled only by one that completed.
//! * **A store with no file** (in memory) is never held.
//!
//! The stamp is taken before the read, so a commit that lands during it makes
//! the held answer stale by the stamp and the next call reads again; the answer
//! can be newer than its stamp, never older.
//!
//! Another process's append is not applied incrementally. An append-only table
//! would allow reading only the rows past the last one seen, but nothing on a
//! shared profile stops a writer from deleting or rewriting an older row, and
//! no check short of reading them tells an append from a rewrite. A change is
//! therefore read in full, in one pass in id order with no sort.

use dc_store::Store;
use rusqlite::types::ValueRef;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// How long a held answer may stand even when the stamp has not moved.
const MAX_AGE: Duration = Duration::from_secs(60);
/// A file modified more recently than this, when its stamp was taken, is
/// still being written to as far as a held answer is concerned.
const RACY: Duration = Duration::from_secs(2);
/// Bytes of record text one held answer may carry. A store past it is read
/// each time, as it was before this existed.
const MAX_BYTES: usize = 64 * 1024 * 1024;
/// Store files with a held answer at once.
const MAX_STORES: usize = 4;

/// The newest `data_json` of each task, as of one read.
type Newest = HashMap<String, String>;

/// One file as a stamp sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    identity: FileIdentity,
    len: u64,
    modified: Option<SystemTime>,
}

/// Which file is at a path, as opposed to what is in it: another value means
/// the path now names a different file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileIdentity(u64, u64);

impl FileIdentity {
    /// The file at `path`; `None` when there is none or it cannot be read.
    pub(crate) fn of(path: &Path) -> Option<Self> {
        std::fs::metadata(path)
            .ok()
            .map(|metadata| Self::from_metadata(&metadata))
    }

    #[cfg(unix)]
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self(metadata.dev(), metadata.ino())
    }

    #[cfg(not(unix))]
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        let created = metadata
            .created()
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |since| {
                u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
            });
        Self(0, created)
    }
}

/// The database file and its write-ahead log, as they were at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StoreStamp {
    db: FileStamp,
    wal: Option<FileStamp>,
}

impl StoreStamp {
    /// `None` when either file cannot be stat'ed for a reason other than the
    /// log not existing: nothing is held on a stamp that was not taken.
    fn take(path: &Path) -> Option<Self> {
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        Some(Self {
            db: stat(path)??,
            wal: stat(Path::new(&wal))?,
        })
    }

    /// Neither file had been modified within [`RACY`] of `taken`.
    fn quiet_at(&self, taken: SystemTime) -> bool {
        let settled = |file: &FileStamp| {
            file.modified
                .and_then(|at| at.checked_add(RACY))
                .is_some_and(|until| until <= taken)
        };
        settled(&self.db) && self.wal.as_ref().is_none_or(settled)
    }
}

/// `Ok(None)` for a file that is not there.
fn stat(path: &Path) -> Option<Option<FileStamp>> {
    match std::fs::metadata(path) {
        Ok(metadata) => Some(Some(FileStamp {
            identity: FileIdentity::from_metadata(&metadata),
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(None),
        Err(_) => None,
    }
}

/// An answer, the stamp it was read under and when.
struct Held {
    stamp: StoreStamp,
    held_at: Instant,
    newest: Newest,
}

type Slot = Arc<Mutex<Option<Held>>>;

/// One slot per store file, least recently used first.
static SLOTS: Mutex<Vec<(PathBuf, Slot)>> = Mutex::new(Vec::new());

fn slot_for(path: &Path) -> Slot {
    let mut slots = crate::supervisor::lock_or_recover(&SLOTS);
    if let Some(at) = slots.iter().position(|(held, _)| held == path) {
        let entry = slots.remove(at);
        let slot = Arc::clone(&entry.1);
        slots.push(entry);
        return slot;
    }
    if slots.len() >= MAX_STORES {
        slots.remove(0);
    }
    let slot = Slot::default();
    slots.push((path.to_path_buf(), Arc::clone(&slot)));
    slot
}

/// The file a store was opened on, when it has one.
fn file_of(store: &Store) -> Option<&Path> {
    store
        .connection()
        .path()
        .filter(|path| !path.is_empty())
        .map(Path::new)
}

/// Runs `answer` over each task's newest record text, read now or held.
///
/// `wanted` names the tasks the caller needs; it matters only when the store
/// is too large to hold, when the walk keeps just those.
///
/// Callers asking at once for a store that has no held answer are served by
/// one walk: the slot is held for the read, and the ones behind it find the
/// answer there. An error from the walk, or from `answer`'s own parse, is the
/// caller's.
pub(super) fn with_newest<T>(
    store: &Store,
    wanted: &HashSet<&str>,
    answer: impl FnOnce(&Newest) -> T,
) -> Result<T, String> {
    let Some(path) = file_of(store) else {
        return Ok(answer(&walk(store, wanted, MAX_BYTES)?.newest));
    };
    let slot = slot_for(path);
    let mut held = crate::supervisor::lock_or_recover(&slot);
    let taken = now_system();
    let stamp = StoreStamp::take(path);
    if let (Some(stamp), Some(current)) = (stamp.as_ref(), held.as_ref()) {
        if current.stamp == *stamp
            && now_instant().saturating_duration_since(current.held_at) < MAX_AGE
        {
            return Ok(answer(&current.newest));
        }
    }
    // Not current, or not provably so. Emptied before the read so that a read
    // that fails leaves no answer behind.
    *held = None;
    let walked = walk(store, wanted, MAX_BYTES)?;
    let out = answer(&walked.newest);
    if let Some(stamp) = stamp.filter(|stamp| walked.complete && stamp.quiet_at(taken)) {
        *held = Some(Held {
            stamp,
            held_at: now_instant(),
            newest: walked.newest,
        });
    }
    Ok(out)
}

/// What the held answer says about `item_id`, without reading anything: `None`
/// when no answer is held for this store or the one held is not current, or
/// when the slot is busy being filled.
pub(super) fn held_text(store: &Store, item_id: &str) -> Option<Option<String>> {
    let path = file_of(store)?;
    let slot = slot_for(path);
    let held = slot.try_lock().ok()?;
    let current = held.as_ref()?;
    let stamp = StoreStamp::take(path)?;
    (current.stamp == stamp && now_instant().saturating_duration_since(current.held_at) < MAX_AGE)
        .then(|| current.newest.get(item_id).cloned())
}

struct Walked {
    newest: Newest,
    /// Every task's newest record is in `newest`, not only the wanted ones.
    complete: bool,
}

/// One pass over the verification rows in id order, keeping each task's last.
///
/// This replaces `MAX(id) ... GROUP BY task_id` joined back to the table, which
/// made SQLite sort the table's task ids through temporary files on every read.
/// The ids come out of the rowid in ascending order, so the last row seen for a
/// task is its newest; the record text is read only for a row that is kept.
fn walk(store: &Store, wanted: &HashSet<&str>, max_bytes: usize) -> Result<Walked, String> {
    #[cfg(test)]
    seam::walked(store);
    let unreadable =
        |e: rusqlite::Error| format!("the verification evidence could not be read: {e}");
    let mut statement = store
        .connection()
        .prepare("SELECT task_id, data_json FROM evidence WHERE type = ?1 ORDER BY id")
        .map_err(unreadable)?;
    let mut rows = statement
        .query(rusqlite::params![super::EVIDENCE_TYPE])
        .map_err(unreadable)?;
    let mut newest = Newest::new();
    let mut bytes = 0usize;
    let mut complete = true;
    while let Some(row) = rows.next().map_err(unreadable)? {
        let ValueRef::Text(task) = row.get_ref(0).map_err(unreadable)? else {
            continue;
        };
        let task = std::str::from_utf8(task)
            .map_err(|e| format!("the verification evidence could not be read: {e}"))?;
        if !complete && !wanted.contains(task) {
            continue;
        }
        let data = row
            .get_ref(1)
            .map_err(unreadable)?
            .as_str()
            .map_err(|e| format!("the verification evidence could not be read: {e}"))?;
        bytes = bytes.saturating_add(data.len());
        if let Some(replaced) = newest.insert(task.to_owned(), data.to_owned()) {
            bytes = bytes.saturating_sub(replaced.len());
        }
        if complete && bytes > max_bytes {
            complete = false;
            newest.retain(|task, _| wanted.contains(task.as_str()));
            bytes = newest.values().map(String::len).sum();
        }
    }
    Ok(Walked { newest, complete })
}

#[cfg(not(test))]
fn now_system() -> SystemTime {
    SystemTime::now()
}

#[cfg(not(test))]
fn now_instant() -> Instant {
    Instant::now()
}

#[cfg(test)]
fn now_system() -> SystemTime {
    SystemTime::now() + seam::skew()
}

#[cfg(test)]
fn now_instant() -> Instant {
    Instant::now() + seam::skew()
}

/// What the tests count and move: how often a store's evidence was walked, and
/// the clock the memo reads (advanced, never slept on).
#[cfg(test)]
pub(crate) mod seam {
    use super::*;

    static WALKS: Mutex<Option<HashMap<PathBuf, usize>>> = Mutex::new(None);

    thread_local! {
        static SKEW: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
    }

    /// macOS names a temp directory `/private/var` through one door and
    /// `/var` through another.
    fn key(path: &Path) -> PathBuf {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }

    pub(super) fn walked(store: &Store) {
        if let Some(path) = file_of(store) {
            *crate::supervisor::lock_or_recover(&WALKS)
                .get_or_insert_with(HashMap::new)
                .entry(key(path))
                .or_default() += 1;
        }
    }

    /// Walks of `path`'s evidence table so far.
    pub(crate) fn walks(path: &Path) -> usize {
        crate::supervisor::lock_or_recover(&WALKS)
            .as_ref()
            .and_then(|walks| walks.get(&key(path)).copied())
            .unwrap_or(0)
    }

    pub(crate) fn advance_clock(by: Duration) {
        SKEW.with(|skew| skew.set(skew.get() + by));
    }

    pub(crate) fn skew() -> Duration {
        SKEW.with(std::cell::Cell::get)
    }

    pub(crate) fn set_skew(to: Duration) {
        SKEW.with(|skew| skew.set(to));
    }

    pub(crate) fn walk_with_cap(
        store: &Store,
        wanted: &HashSet<&str>,
        max_bytes: usize,
    ) -> Result<(Newest, bool), String> {
        walk(store, wanted, max_bytes).map(|walked| (walked.newest, walked.complete))
    }
}
