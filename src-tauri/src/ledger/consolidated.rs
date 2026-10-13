//! Which sibling ledgers consolidation has already imported, and as of when.
//!
//! Consolidating the family's pre-family ledgers is a migration: it matters the
//! first time a sibling holds rows, and again only if that sibling changes. It
//! used to run, in full, on every read — for each sibling a `git hash-object`
//! spawn, a database open, a snapshot transaction and `MAX(id)` — so a status
//! call cost a Git process and a SQLite open per worktree (6.8 s observed with
//! eighteen).
//!
//! A source is skipped only while its files are exactly as they were when an
//! import of it *completed*: same length and modification time for the
//! database and its write-ahead log (a WAL-mode database changes its `-wal`
//! file long before its main file), and the same anchor database underneath. A
//! sibling that grows, is replaced, or appears for the first time has a
//! different stamp and is imported. A failed import records nothing, so it is
//! retried, loudly, on the next read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use super::FileId;

/// One file as the filesystem described it.
#[derive(Clone, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    id: Option<FileId>,
}

impl FileStamp {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            id: {
                use std::os::unix::fs::MetadataExt;
                Some(FileId(metadata.dev(), metadata.ino()))
            },
            #[cfg(not(unix))]
            id: None,
        }
    }
}

/// A sibling ledger's database and write-ahead log, as last imported.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct SourceStamp {
    database: FileStamp,
    wal: Option<FileStamp>,
}

impl SourceStamp {
    pub fn of(database: &std::fs::Metadata, wal: Option<&std::fs::Metadata>) -> Self {
        Self {
            database: FileStamp::of(database),
            wal: wal.map(FileStamp::of),
        }
    }
}

struct Imported {
    /// The anchor database the imports went into; a different file means the
    /// markers they relied on are gone.
    anchor: Option<FileId>,
    sources: HashMap<PathBuf, SourceStamp>,
}

/// Anchors remembered at once, and sources per anchor. Far above any real
/// family; past either the memory is dropped and the next read re-checks,
/// which costs time and never correctness.
const MAX_ANCHORS: usize = 64;
const MAX_SOURCES: usize = 1024;

fn imported() -> &'static Mutex<HashMap<PathBuf, Imported>> {
    static IMPORTED: OnceLock<Mutex<HashMap<PathBuf, Imported>>> = OnceLock::new();
    IMPORTED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether `source` is exactly as it was when its last complete import into
/// `anchor` (whose database is `anchor_db`) finished.
pub(super) fn is_current(
    anchor: &Path,
    anchor_db: Option<FileId>,
    source: &Path,
    stamp: &SourceStamp,
) -> bool {
    let held = imported()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    held.get(anchor).is_some_and(|entry| {
        entry.anchor.is_some()
            && entry.anchor == anchor_db
            && entry.sources.get(source) == Some(stamp)
    })
}

/// Remembers that `source`, as `stamp` describes it, has been imported into
/// the anchor database `anchor_db`. `stamp` is the one taken *before* the
/// import read the source: a write during the import changes the files, so the
/// next read sees a different stamp and imports again.
pub(super) fn record(anchor: &Path, anchor_db: Option<FileId>, source: &Path, stamp: SourceStamp) {
    let mut held = imported()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if held.len() >= MAX_ANCHORS && !held.contains_key(anchor) {
        held.clear();
    }
    let entry = held
        .entry(anchor.to_path_buf())
        .or_insert_with(|| Imported {
            anchor: anchor_db,
            sources: HashMap::new(),
        });
    if entry.anchor != anchor_db || entry.sources.len() >= MAX_SOURCES {
        entry.anchor = anchor_db;
        entry.sources.clear();
    }
    entry.sources.insert(source.to_path_buf(), stamp);
}

/// Forgets everything, as a process restart would.
#[cfg(test)]
pub(super) fn clear() {
    imported()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}
