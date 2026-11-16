//! One owner for "has this store changed since I last looked".
//!
//! SQLite's `data_version` changes, on a connection, whenever *another*
//! connection commits, in this process or any other. Two things in the host
//! ask that question of the workbench profile: the external-change watcher
//! (to tell the window that an agent wrote a task) and the verdict memo (to
//! know whether the verdicts it holds are still the verdicts in the store).
//! Both hold a connection of their own that never writes, both read the counter
//! on it, and both must notice that the file under the path has been replaced,
//! because a connection keeps reading the file it opened. This is that
//! connection, its file identity and the counter read; what each does with the
//! answer (announce it, drop a held answer) is theirs.
//!
//! The counter is read in autocommit: the statement ends before the call
//! returns, so the connection never holds a read transaction between calls and
//! never pins the write-ahead log against a checkpoint.

use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;

/// Which file is at a path, as opposed to what is in it: another value means
/// the path now names a different file.
///
/// Windows cannot delete or replace a database another connection holds open,
/// so there the identity is unknown and nothing counts as replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileId(Option<(u64, u64)>);

impl FileId {
    pub(crate) fn of_metadata(metadata: &std::fs::Metadata) -> Self {
        Self(identity(metadata))
    }

    /// The file at `path`; `None` when there is none or it cannot be read.
    pub(crate) fn at(path: &Path) -> Option<Self> {
        std::fs::metadata(path)
            .ok()
            .map(|metadata| Self::of_metadata(&metadata))
    }

    /// True when `self` is a known file and `now` is another one.
    pub(crate) fn replaced_by(&self, now: &Self) -> bool {
        self.0.is_some() && self != now
    }
}

#[cfg(unix)]
fn identity(metadata: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn identity(_metadata: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// A connection to a store file that only asks whether the file changed.
pub(crate) struct ChangeProbe {
    connection: Connection,
    opened_as: FileId,
}

impl ChangeProbe {
    /// Opens `path` with `flags` (never with `SQLITE_OPEN_CREATE`: asking about
    /// a store must not create one).
    pub(crate) fn open(path: &Path, flags: OpenFlags, busy: Duration) -> Result<Self, String> {
        let opened_as = FileId::at(path)
            .ok_or_else(|| format!("cannot open {}: it is not there", path.display()))?;
        let connection = Connection::open_with_flags(path, flags)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        connection
            .busy_timeout(busy)
            .map_err(|e| format!("cannot configure the change watcher: {e}"))?;
        Ok(Self {
            connection,
            opened_as,
        })
    }

    /// The file this connection was opened on.
    pub(crate) fn opened_as(&self) -> FileId {
        self.opened_as
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    /// `PRAGMA data_version` on this connection.
    pub(crate) fn data_version(&self) -> Result<i64, String> {
        self.connection
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .map_err(|error| format!("cannot read the profile's change counter: {error}"))
    }
}
