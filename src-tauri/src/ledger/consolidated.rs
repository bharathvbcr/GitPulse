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

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use super::FileId;
use crate::lru_map::LruMap;

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
    sources: LruMap<PathBuf, SourceStamp>,
}

/// Anchors remembered at once, and sources per anchor. Far above any real
/// family; past either, the one used longest ago is dropped and its next read
/// re-checks, which costs time and never correctness. Dropping only that one
/// keeps every other family warm: a process with more families open than the
/// cap used to empty the whole memory on each new one.
const MAX_ANCHORS: usize = 64;
const MAX_SOURCES: usize = 1024;

fn imported() -> &'static Mutex<LruMap<PathBuf, Imported>> {
    static IMPORTED: OnceLock<Mutex<LruMap<PathBuf, Imported>>> = OnceLock::new();
    IMPORTED.get_or_init(|| Mutex::new(LruMap::new(MAX_ANCHORS)))
}

/// Whether `source` is exactly as it was when its last complete import into
/// `anchor` (whose database is `anchor_db`) finished.
pub(super) fn is_current(
    anchor: &Path,
    anchor_db: Option<FileId>,
    source: &Path,
    stamp: &SourceStamp,
) -> bool {
    let mut held = imported()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    held.get_mut(anchor).is_some_and(|entry| {
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
    let entry = held.get_or_insert_with(anchor.to_path_buf(), || Imported {
        anchor: anchor_db,
        sources: LruMap::new(MAX_SOURCES),
    });
    if entry.anchor != anchor_db {
        entry.anchor = anchor_db;
        entry.sources.clear();
    }
    entry.sources.insert(source.to_path_buf(), stamp);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp() -> SourceStamp {
        let manifest = std::fs::metadata(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .expect("the crate manifest");
        SourceStamp::of(&manifest, None)
    }

    /// A full memory used to empty itself: the first anchor past the cap forgot
    /// every other anchor's imports, so each of them re-opened its siblings on
    /// the next read, and a parallel test's insert wiped a sibling test's warm
    /// entry between its cold and warm read. Only the least recently used
    /// anchor may go, and a read that finds it current is a use.
    #[test]
    fn a_full_memory_forgets_only_the_anchors_gone_unused() {
        let id = Some(FileId(7, 7));
        let source = Path::new("/gp-consolidated-lru-test/source");
        let hot = Path::new("/gp-consolidated-lru-test/hot");
        record(hot, id, source, stamp());

        for i in 0..MAX_ANCHORS * 3 {
            let filler = PathBuf::from(format!("/gp-consolidated-lru-test/filler-{i}"));
            record(&filler, id, source, stamp());
            if i % 8 == 0 {
                assert!(
                    is_current(hot, id, source, &stamp()),
                    "the anchor in use was forgotten after {i} others arrived"
                );
            }
        }
        assert!(
            !is_current(
                Path::new("/gp-consolidated-lru-test/filler-0"),
                id,
                source,
                &stamp()
            ),
            "the cap no longer bounds the memory"
        );
    }

    /// `tests_support::reset_registry_for` stands in for a restart of the
    /// calling test's repository. Its predecessor emptied this process-wide
    /// memory as well, so any test that reset wiped a parallel test's warm
    /// entry between that test's cold and warm read. Anchors are keyed by path
    /// and checked against the anchor database's file identity, so a reset has
    /// nothing here to forget on its caller's behalf.
    #[test]
    fn a_registry_reset_does_not_forget_another_tests_anchor() {
        let id = Some(FileId(9, 9));
        let anchor = Path::new("/gp-consolidated-reset-test/anchor");
        let source = Path::new("/gp-consolidated-reset-test/source");
        record(anchor, id, source, stamp());

        crate::ledger::tests_support::reset_registry_for("/gp-consolidated-reset-test/other");

        assert!(
            is_current(anchor, id, source, &stamp()),
            "a registry reset forgot an anchor it does not own"
        );
    }
}
