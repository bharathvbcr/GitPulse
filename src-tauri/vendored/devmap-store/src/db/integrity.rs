//! Recognising a store file whose pages are damaged.
//!
//! Damage inside the full-text index's shadow tables made every remedy the
//! store named fail with the same "database disk image is malformed", while a
//! plain `build` compared hashes, found nothing changed and exited 0 over a
//! store whose search errored. The store now reports it (`status`, through
//! [`Store::search_index_damage`]) and `build` refuses to write to it. Setting a
//! damaged file aside and building again was tried and parked: see
//! `docs/devmap/design/auto-quarantine-parked.md`.

use super::Store;

impl Store {
    /// Whether `error`, or anything it wraps, is SQLite's `SQLITE_CORRUPT`.
    ///
    /// By error *code*, never by message: `fts_failure` and `refusal` re-wrap
    /// an error into text, and a substring match on that text would call a
    /// refusal that merely quotes the words "database disk image is malformed"
    /// a damaged file. `SQLITE_NOTADB` is deliberately not included: a file
    /// that is not a database at all may be a file the caller pointed `--db`
    /// at by mistake, and calling that damage is not this function's call.
    pub fn is_database_corrupt(error: &anyhow::Error) -> bool {
        error.chain().any(|cause| {
            cause
                .downcast_ref::<rusqlite::Error>()
                .is_some_and(Self::sqlite_error_is_corrupt)
        })
    }

    /// [`Self::is_database_corrupt`] for an error not yet wrapped.
    pub fn sqlite_error_is_corrupt(error: &rusqlite::Error) -> bool {
        error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseCorrupt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corruption_is_recognised_by_code_and_not_by_the_words() {
        let corrupt = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            Some("database disk image is malformed".into()),
        );
        assert!(Store::is_database_corrupt(
            &anyhow::Error::from(corrupt).context("while building")
        ));
        // A refusal that merely quotes the message is not damage.
        let quoted = anyhow::anyhow!("note: database disk image is malformed (quoted from a log)");
        assert!(!Store::is_database_corrupt(&quoted));
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        );
        assert!(!Store::is_database_corrupt(&anyhow::Error::from(busy)));
        // A file that is not a database at all is not called damage on this evidence.
        let not_a_db = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOTADB),
            None,
        );
        assert!(!Store::is_database_corrupt(&anyhow::Error::from(not_a_db)));
    }
}
