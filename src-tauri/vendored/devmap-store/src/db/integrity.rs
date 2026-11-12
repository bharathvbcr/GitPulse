//! Recognising a store file whose pages are damaged.
//!
//! Damage inside the full-text index's shadow tables made every remedy the
//! store named fail with the same "database disk image is malformed", while a
//! plain `build` compared hashes, found nothing changed and exited 0 over a
//! store whose search errored. The store now reports it (`status`, through
//! [`Store::search_index_damage`]) and `build` refuses to write to it. Setting a
//! damaged file aside and building again was tried and parked: see
//! `docs/devmap/design/auto-quarantine-parked.md`.

use super::{FtsDamage, Store};

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

    /// Whether the damage is confined to the full-text index: the error is the
    /// re-worded read of `nodes_fts` (see [`FtsDamage`]). A store with this
    /// damage still answers every question that does not read the index, so a
    /// caller must not treat the whole file as lost.
    pub fn is_full_text_damage(error: &anyhow::Error) -> bool {
        error.chain().any(|cause| {
            cause
                .downcast_ref::<rusqlite::Error>()
                .is_some_and(Self::sqlite_error_is_full_text_damage)
        })
    }

    /// [`Self::is_full_text_damage`] for an error not yet wrapped, such as the
    /// one a failed open returns.
    pub fn sqlite_error_is_full_text_damage(error: &rusqlite::Error) -> bool {
        matches!(
            error,
            rusqlite::Error::ToSqlConversionFailure(inner)
                if inner.downcast_ref::<FtsDamage>().is_some()
        )
    }

    /// Whether reading the full-text index ALONE fails with `SQLITE_CORRUPT`:
    /// the virtual table, then each of its storage tables that exists.
    ///
    /// This is the evidence a corrupt error needs before it may be called
    /// index damage. A statement that reads the index also reads other tables
    /// (`payload_nodes`, `file_rows`, `paths`), so its failure says nothing about
    /// which of them is damaged; a probe that touches only the index does.
    /// A probe that passes proves nothing about the index being whole (a count
    /// does not read overflow pages), and that is the safe direction: the caller
    /// then treats the damage as the file's.
    pub(super) fn full_text_index_unreadable(conn: &rusqlite::Connection) -> bool {
        use rusqlite::OptionalExtension;
        let corrupt = |result: rusqlite::Result<i64>| {
            matches!(result, Err(error) if error.sqlite_error_code()
                == Some(rusqlite::ErrorCode::DatabaseCorrupt))
        };
        if corrupt(conn.query_row("SELECT COUNT(*) FROM nodes_fts", [], |row| row.get(0))) {
            return true;
        }
        for table in [
            "nodes_fts_data",
            "nodes_fts_idx",
            "nodes_fts_content",
            "nodes_fts_docsize",
            "nodes_fts_config",
        ] {
            let present = conn
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get::<_, i64>(0),
                )
                .optional();
            if matches!(present, Ok(Some(_)))
                && corrupt(conn.query_row(
                    &format!("SELECT COUNT(*) FROM \"{table}\""),
                    [],
                    |row| row.get(0),
                ))
            {
                return true;
            }
        }
        false
    }

    /// The verdict on a corrupt error from a statement that reads `nodes_fts`
    /// among other tables: index damage (typed, with the repair) only if the
    /// index alone cannot be read, otherwise the error unchanged, which is
    /// damage to the file as a whole. Any error that is not corruption passes
    /// through.
    pub(super) fn fts_verdict(
        conn: &rusqlite::Connection,
        error: rusqlite::Error,
    ) -> rusqlite::Error {
        if error.sqlite_error_code() != Some(rusqlite::ErrorCode::DatabaseCorrupt) {
            return error;
        }
        if Self::full_text_index_unreadable(conn) {
            super::fts_failure(error)
        } else {
            error
        }
    }

    /// Validate the schema of a connection just opened, telling a store whose
    /// only fault is its full-text index from every other fault.
    ///
    /// `Ok(None)`: valid. `Ok(Some(typed))`: invalid, and the damage is the
    /// index's alone (the index cannot be read, and every other check passes
    /// without it); `typed` is the full-text error a caller can recognise with
    /// [`Self::is_full_text_damage`]. `Err`: anything else, including corruption
    /// the index does not account for; never served.
    pub(super) fn validate_schema_naming_index_damage(
        conn: &rusqlite::Connection,
    ) -> rusqlite::Result<Option<rusqlite::Error>> {
        match Self::validate_schema(conn) {
            Ok(()) => Ok(None),
            Err(error)
                if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseCorrupt) =>
            {
                if Self::full_text_index_unreadable(conn)
                    && Self::validate_schema_without_full_text(conn).is_ok()
                {
                    Ok(Some(super::fts_failure(error)))
                } else {
                    Err(error)
                }
            }
            Err(error) => Err(error),
        }
    }

    /// [`Self::is_database_corrupt`] for an error not yet wrapped.
    ///
    /// A corrupt read of the full-text index arrives re-worded as a
    /// [`FtsDamage`] that keeps the SQLite error as its source; it is damage
    /// by construction, so it answers here too.
    pub fn sqlite_error_is_corrupt(error: &rusqlite::Error) -> bool {
        error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseCorrupt)
            || matches!(
                error,
                rusqlite::Error::ToSqlConversionFailure(inner)
                    if inner.downcast_ref::<FtsDamage>().is_some()
            )
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
        // A corrupt read of the full-text index is re-worded for the reader and
        // is still damage, through the anyhow chain a tool call carries.
        let corrupt = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            Some("database disk image is malformed".into()),
        );
        let reworded = crate::db::fts_failure(corrupt);
        assert!(Store::sqlite_error_is_corrupt(&reworded));
        assert!(Store::is_database_corrupt(
            &anyhow::Error::from(reworded).context("devmap_search")
        ));
        // Any other error passes through fts_failure unchanged and is not damage.
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        );
        assert!(!Store::sqlite_error_is_corrupt(&crate::db::fts_failure(
            busy
        )));
        // A file that is not a database at all is not called damage on this evidence.
        let not_a_db = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOTADB),
            None,
        );
        assert!(!Store::is_database_corrupt(&anyhow::Error::from(not_a_db)));
    }
}
