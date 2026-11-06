use super::{refusal, unsupported_schema_error, Store, UnsupportedSchema, REQUIRED_SCHEMA};
use crate::schema::{
    declared_index_names, declared_index_statements, BUILD_HISTORY_TABLE, COVERAGE_GAPS_TABLE,
    CREATE_SCHEMA_V3, CURRENT_SCHEMA_VERSION, MIGRATION_V10_TO_V11, MIGRATION_V11_TO_V12,
    MIGRATION_V12_TO_V13, MIGRATION_V14_TO_V15, MIGRATION_V15_TO_V16, MIGRATION_V16_TO_V17,
    MIGRATION_V17_TO_V18_BACKFILL_EDGES, MIGRATION_V17_TO_V18_BACKFILL_UNRESOLVED,
    MIGRATION_V17_TO_V18_RENAME_EDGES, MIGRATION_V17_TO_V18_RENAME_UNRESOLVED,
    MIGRATION_V18_TO_V19, MIGRATION_V19_TO_V20, MIGRATION_V20_TO_V21, MIGRATION_V21_TO_V22,
    MIGRATION_V23_TO_V24, MIGRATION_V25_TO_V26, MIGRATION_V26_TO_V27, MIGRATION_V27_TO_V28,
    MIGRATION_V28_TO_V29, MIGRATION_V3_TO_V4, MIGRATION_V4_TO_V5, MIGRATION_V4_TO_V5_EDGE_INDEXES,
    MIGRATION_V5_TO_V6, MIGRATION_V6_TO_V7, MIGRATION_V7_TO_V8, MIGRATION_V8_TO_V9,
    MIGRATION_V9_TO_V10, MIN_READER_SCHEMA_VERSION, READER_COMPAT_TABLE, SEED_REBIND_TABLE,
    VALIDITY_RANGE_TABLES,
};
use rusqlite::OptionalExtension;
use rusqlite::{params, Connection, Result, TransactionBehavior};
use std::path::Path;

impl Store {
    pub(super) fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
        let mut names = stmt.query_map([], |row| row.get::<_, String>(1))?;
        names.try_fold(false, |found, name| Ok(found || name? == column))
    }

    /// The v27 rung: the dead-row evidence columns, added only when absent.
    ///
    /// Probed for the reason every `ADD COLUMN` rung here is: the statement is
    /// not repeatable, and the ladder must be re-entrant — racing openers of a
    /// mid-chain store, and a store walked up from an old version through
    /// batches that already recreated the table, both reach this rung on a
    /// table that may have the columns. The three are added by one batch, so
    /// the first is the probe for all of them.
    fn add_dead_evidence_columns(conn: &Connection) -> Result<()> {
        if !Self::has_column(conn, "generation_dead_symbols", "evidence_sites")? {
            conn.execute_batch(MIGRATION_V26_TO_V27)?;
        }
        Ok(())
    }

    /// Whether `name` exists and is a base table rather than a view.
    ///
    /// The migration chain runs over both a genuine old store and a
    /// freshly-created one carrying the current shape, so a step that is legal
    /// only against a table has to ask. Absent counts as "not a table": a step
    /// guarded by this must be skipped when its target does not exist either.
    /// Whether `name` names anything at all — table, view or index.
    ///
    /// [`Self::relation_is_table`] cannot answer this: it reads absent and view
    /// as the same "no", which is right for a step that only works on a table
    /// and wrong for one that must be skipped when the relation exists *in any
    /// shape*. `MIGRATION_V8_TO_V9` is the second kind.
    fn relation_exists(conn: &Connection, name: &str) -> Result<bool> {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub(super) fn relation_is_table(conn: &Connection, name: &str) -> Result<bool> {
        let kind: Option<String> = conn
            .query_row(
                "SELECT type FROM sqlite_master WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .optional()?;
        Ok(kind.as_deref() == Some("table"))
    }

    /// Recreate every index the fresh schema declares, before the gate below
    /// demands them.
    ///
    /// The ladder runs only the rungs above a store's stamp, and an index
    /// added to the fresh schema *within* a version number is on none of them.
    /// This repository's own store — v17 from a build whose v17 had no
    /// `idx_file_payloads_cache_identity` — walked 17→18→19 and was refused by
    /// the gate, whose remedy was `devmap build`: the command that had just
    /// refused. Every statement is `IF NOT EXISTS`, so on a complete store
    /// this is a handful of catalogue lookups.
    fn heal_declared_indexes(conn: &Connection) -> Result<()> {
        for statement in declared_index_statements() {
            conn.execute_batch(&statement)?;
        }
        Ok(())
    }

    /// Record which reader schemas can read this store
    /// ([`MIN_READER_SCHEMA_VERSION`]). Run by every writer open, on the fresh
    /// path and at the end of the ladder, inside the migration transaction.
    ///
    /// Overwritten rather than kept: only the binary that stamped the current
    /// `user_version` knows what its own bump means, and a writer that reached
    /// this point is exactly that binary — a newer store is refused before it.
    /// The `WHERE` keeps an already-correct row from dirtying a page on every
    /// open.
    pub(super) fn stamp_reader_compat(conn: &Connection) -> Result<()> {
        conn.execute_batch(READER_COMPAT_TABLE)?;
        conn.execute(
            "INSERT INTO reader_compat (singleton, min_reader_schema) VALUES (1, ?1)
             ON CONFLICT (singleton) DO UPDATE SET min_reader_schema = excluded.min_reader_schema
             WHERE min_reader_schema IS NOT excluded.min_reader_schema",
            params![MIN_READER_SCHEMA_VERSION],
        )?;
        Ok(())
    }

    /// The floor a writer recorded in this store, or `None` when it records
    /// none — a store written before [`READER_COMPAT_TABLE`] existed.
    ///
    /// A row that is present but unusable (several rows, a floor below 3, a
    /// floor above the store's own stamp) is a refusal, never `None`: a missing
    /// floor means "exact match", which is safe, but a damaged one must not be
    /// silently treated as missing.
    pub(super) fn recorded_reader_floor(conn: &Connection, stamped: i32) -> Result<Option<i32>> {
        if !Self::relation_is_table(conn, "reader_compat")? {
            return Ok(None);
        }
        let floors: Vec<rusqlite::types::Value> = {
            let mut stmt = conn.prepare("SELECT min_reader_schema FROM reader_compat")?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            rows.collect::<Result<_>>()?
        };
        let floor = match floors.as_slice() {
            [] => return Ok(None),
            [rusqlite::types::Value::Integer(floor)] => *floor,
            [other] => {
                return Err(refusal(format!(
                    "reader_compat.min_reader_schema is {other:?}, not an integer; refusing a \
                     floor this binary cannot read"
                )))
            }
            many => {
                return Err(refusal(format!(
                    "reader_compat holds {} rows; refusing an ambiguous reader floor",
                    many.len()
                )))
            }
        };
        if floor < 3 || floor > i64::from(stamped) {
            return Err(refusal(format!(
                "reader_compat.min_reader_schema = {floor} is outside 3..={stamped} for a store \
                 at schema {stamped}; refusing a floor that cannot be true"
            )));
        }
        Ok(Some(floor as i32))
    }

    /// The v28 rung: move nodes, literals, full-text rows, file membership and
    /// digests onto payloads and validity ranges ([`MIGRATION_V27_TO_V28`]).
    ///
    /// Guarded on the shape rather than the stamp, like the v22 rung: a store
    /// whose `generation_nodes` is already a view has had this rung applied,
    /// and re-running it would read tables it no longer has.
    ///
    /// This rung moves rows, so it counts them. The latest generation's symbol,
    /// literal, file and digest rows are counted through the v27 tables before
    /// and through the v28 views after, and any difference refuses the whole
    /// migration. Every statement is in the caller's one transaction, so the
    /// refusal leaves the v27 store as it was. Only the latest generation is
    /// compared: an older retained generation may name a payload the newest
    /// also names, and a payload stores one copy of its rows.
    fn migrate_to_v28(conn: &Connection) -> Result<()> {
        if !Self::relation_is_table(conn, "generation_nodes")? {
            // Applied already. A store re-stamped to an early version and walked
            // up has re-run `CREATE_SCHEMA_V3` over it, which recreates the
            // identity index v28 replaced; left in place it would refuse a
            // second payload under one key, which is what v28 exists to store.
            conn.execute_batch("DROP INDEX IF EXISTS idx_file_payloads_identity;")?;
            return Ok(());
        }
        let counts = |conn: &Connection| -> Result<Option<[i64; 4]>> {
            let latest: Option<i64> =
                conn.query_row("SELECT MAX(id) FROM generations", [], |row| row.get(0))?;
            let Some(latest) = latest else {
                return Ok(None);
            };
            let mut counted = [0i64; 4];
            for (slot, relation) in [
                "generation_nodes",
                "generation_literals",
                "generation_file_rows",
                "generation_file_digests",
            ]
            .iter()
            .enumerate()
            {
                counted[slot] = conn.query_row(
                    &format!("SELECT COUNT(*) FROM {relation} WHERE generation_id = ?1"),
                    params![latest],
                    |row| row.get(0),
                )?;
            }
            Ok(Some(counted))
        };
        let before = counts(conn)?;
        conn.execute_batch(MIGRATION_V27_TO_V28)?;
        Self::backfill_payload_digests(conn)?;
        let after = counts(conn)?;
        if before != after {
            return Err(refusal(format!(
                "moving the store to schema 28 changed the latest generation's rows \
                 (nodes, literals, file rows, digests): {before:?} before, {after:?} after; \
                 the store has been left unmigrated"
            )));
        }
        Ok(())
    }

    /// Give every payload a v27 store holds the digest the writer would have
    /// stamped on it, computed from the documents as stored.
    ///
    /// A payload left at the empty digest would never match an extraction in
    /// hand, so the first `--full` after the migration would write a second
    /// copy of every file's payload and rows. One row at a time, so the
    /// migration holds one payload's documents and not the store's.
    fn backfill_payload_digests(conn: &Connection) -> Result<()> {
        let ids: Vec<i64> = conn
            .prepare("SELECT payload_id FROM file_payloads WHERE payload_digest = x''")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_>>()?;
        // The extraction is read through the codec: a store that is being
        // walked up from v27 holds text here, and one re-stamped down from v29
        // holds frames. The digest covers the text either way, so a payload
        // keeps the identity it had before it was compressed.
        let mut read = conn.prepare(
            "SELECT fp.parse_outcome_json, fp.engine_json, fp.extraction_json, pa.path
               FROM file_payloads fp JOIN paths pa ON pa.id = fp.file_id
              WHERE fp.payload_id = ?1",
        )?;
        let mut stamp =
            conn.prepare("UPDATE file_payloads SET payload_digest = ?2 WHERE payload_id = ?1")?;
        for id in ids {
            let digest = read.query_row(params![id], |row| {
                let path: String = row.get(3)?;
                let extraction = crate::payload_codec::decode_value(row.get_ref(2)?, &path)?;
                Ok(crate::db::payload_digest(
                    &row.get::<_, String>(0)?,
                    &row.get::<_, String>(1)?,
                    &extraction,
                ))
            })?;
            stamp.execute(params![id, digest.as_slice()])?;
        }
        Ok(())
    }

    /// The v29 rung: store every payload's extraction compressed and give every
    /// payload its `entry_wiring_json` ([`MIGRATION_V28_TO_V29`]).
    ///
    /// Refuses, leaving the v28 store whole (the caller's one transaction
    /// rolls back), when any payload could not be converted: a store being
    /// migrated is converted entirely or not at all, and the remedy for a store
    /// that holds a document nothing can read is to move or delete it and run
    /// `devmap build`.
    fn migrate_to_v29(conn: &Connection) -> Result<()> {
        let payloads: i64 =
            conn.query_row("SELECT COUNT(*) FROM file_payloads", [], |row| row.get(0))?;
        let unconverted = Self::heal_extraction_encoding(conn)?;
        if let Some((path, reason)) = unconverted.first() {
            return Err(refusal(format!(
                "moving the store to schema 29 could not convert {} of {payloads} stored \
                 extractions, the first for {path}: {reason}; the store has been left unmigrated",
                unconverted.len()
            )));
        }
        // The invariant of the rung, checked rather than inferred from the pass
        // having returned nothing: every payload is a frame with its entry wiring.
        let left: i64 = conn.query_row(
            "SELECT COUNT(*) FROM file_payloads
              WHERE typeof(extraction_json) <> 'blob' OR entry_wiring_json IS NULL",
            [],
            |row| row.get(0),
        )?;
        if left != 0 {
            return Err(refusal(format!(
                "moving the store to schema 29 left {left} of {payloads} payloads uncompressed \
                 or without entry wiring; the store has been left unmigrated"
            )));
        }
        Self::release_free_pages(conn)
    }

    /// Compress every stored extraction that is still text and give every
    /// payload that has none its `entry_wiring_json`, one payload at a time.
    ///
    /// The migration's work and the same-version heal in one function: it
    /// selects only what is not yet converted, so it resumes a pass that was
    /// interrupted, converges a store a writer left half converted, and costs a
    /// single index-free probe when there is nothing to do (`typeof` reads the
    /// record header, not the document). Each compressed document is
    /// decompressed again and compared with the text it came from before it
    /// replaces it, so a conversion that did not round-trip never reaches the
    /// store.
    ///
    /// Returns the payloads it could not convert, `(path, reason)`, and leaves
    /// each as it found it: a document that is not valid JSON or a frame that
    /// will not decompress. The reader refuses those by name when it reaches
    /// them, and the migration refuses the store ([`Self::migrate_to_v29`]), so
    /// the list is never read as "converted".
    fn heal_extraction_encoding(conn: &Connection) -> Result<Vec<(String, String)>> {
        use crate::payload_codec::{
            compress_document, decode_value, entry_wiring_of_document, inflate_document,
        };
        if !Self::has_column(conn, "file_payloads", "entry_wiring_json")? {
            conn.execute_batch(MIGRATION_V28_TO_V29)?;
        }
        let ids: Vec<i64> = conn
            .prepare(
                "SELECT payload_id FROM file_payloads
                  WHERE typeof(extraction_json) <> 'blob' OR entry_wiring_json IS NULL",
            )?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_>>()?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut read = conn.prepare(
            "SELECT fp.extraction_json, pa.path
               FROM file_payloads fp JOIN paths pa ON pa.id = fp.file_id
              WHERE fp.payload_id = ?1",
        )?;
        let mut write = conn.prepare(
            "UPDATE file_payloads SET extraction_json = ?2, entry_wiring_json = ?3
              WHERE payload_id = ?1",
        )?;
        let mut keep_frame =
            conn.prepare("UPDATE file_payloads SET entry_wiring_json = ?2 WHERE payload_id = ?1")?;
        let mut unconverted = Vec::new();
        for id in ids {
            let converted = read.query_row(params![id], |row| {
                let path: String = row.get(1)?;
                let value = row.get_ref(0)?;
                let was_text = matches!(value, rusqlite::types::ValueRef::Text(_));
                let outcome = (|| -> Result<(Option<Vec<u8>>, String)> {
                    let json = decode_value(value, &path)?;
                    let entries = entry_wiring_of_document(&json, &path)?;
                    if !was_text {
                        return Ok((None, entries));
                    }
                    let frame = compress_document(&json)?;
                    if inflate_document(&frame, &path)? != *json {
                        return Err(refusal(format!(
                            "the compressed extraction for {path} did not decompress to the \
                             document it was made from"
                        )));
                    }
                    Ok((Some(frame), entries))
                })();
                Ok((path, outcome))
            })?;
            match converted {
                (_, Ok((Some(frame), entries))) => {
                    write.execute(params![id, frame, entries])?;
                }
                (_, Ok((None, entries))) => {
                    keep_frame.execute(params![id, entries])?;
                }
                (path, Err(error)) => unconverted.push((path, error.to_string())),
            }
        }
        Ok(unconverted)
    }

    /// Give a store stamped 28 by the first v28 prototype the `payload_digest`
    /// column it lacks, and stamp every row that still reads the empty digest.
    ///
    /// Serves stores written by the 480aab5c prototype; no released writer
    /// produced one. The same case as an index added to the fresh schema within
    /// a version number ([`Self::heal_declared_indexes`]): the ladder runs no
    /// rung for a store already at the current version, and the declared index
    /// over the missing column would otherwise fail the open with a bare "no
    /// such column". The digests are stamped from the stored documents, as the
    /// v27 rung does, and the old identity index goes, so the store is the one
    /// a fresh open would have made.
    ///
    /// The stamping runs whether or not the column was just added: a store
    /// that has the column and rows still at the empty digest (a prototype
    /// writer stopped between the two steps) would otherwise open and never
    /// match an extraction in hand, and the next build would write a second
    /// copy of every such payload. A store with every row stamped has nothing
    /// to do and pays one probe.
    fn heal_payload_digest(conn: &Connection) -> Result<()> {
        if !Self::has_column(conn, "file_payloads", "payload_digest")? {
            conn.execute_batch(
                "ALTER TABLE file_payloads ADD COLUMN payload_digest BLOB NOT NULL DEFAULT x'';
                 DROP INDEX IF EXISTS idx_file_payloads_identity;",
            )?;
        }
        Self::backfill_payload_digests(conn)
    }

    fn heal_seed_rebind_table(conn: &Connection) -> Result<()> {
        if !conn.is_readonly(rusqlite::MAIN_DB)? {
            conn.execute_batch(SEED_REBIND_TABLE)?;
        }
        Ok(())
    }

    /// Read the seeded worktree `repo_root` override recorded by `rebind_staged`
    /// (`None` when `seed_rebind` is absent or empty), validating that any
    /// stored row is a non-empty string (`F6`, `F8`, Critique 3).
    pub(super) fn seeded_repo_root_in(conn: &Connection) -> Result<Option<String>> {
        if !Self::relation_is_table(conn, "seed_rebind")? {
            return Ok(None);
        }
        let rows: Vec<rusqlite::types::Value> = {
            let mut stmt = conn.prepare("SELECT seeded_repo_root FROM seed_rebind")?;
            let mapped = stmt.query_map([], |row| row.get(0))?;
            mapped.collect::<Result<_>>()?
        };
        match rows.as_slice() {
            [] => Ok(None),
            [rusqlite::types::Value::Text(root)] if !root.is_empty() => Ok(Some(root.clone())),
            _ => Err(refusal(
                "seed_rebind row is malformed; run `devmap build` or `devmap seed` to repair",
            )),
        }
    }

    /// When a seeded store commits or restamps its own generation during an
    /// explicit build, retire `seed_rebind` and normalize any inherited
    /// `generations.repo_root` rows (`rebind_staged` itself never touches
    /// `generations`, keeping provisioning CoW-zero-overflow).
    pub(super) fn clear_seed_rebind(tx: &Connection, target_root: Option<&str>) -> Result<()> {
        if let Some(seeded_root) = Self::seeded_repo_root_in(tx)? {
            let root = target_root
                .filter(|r| !r.is_empty())
                .unwrap_or(seeded_root.as_str());
            tx.execute(
                "UPDATE generations SET repo_root = ?1 WHERE repo_root IS NOT ?1",
                params![root],
            )?;
            tx.execute("DELETE FROM seed_rebind", [])?;
        }
        Ok(())
    }

    /// Hand every free page back to the filesystem, stepping the pragma to
    /// exhaustion (it frees one page per row stepped, and `execute_batch`
    /// steps once). A store whose `auto_vacuum` is not incremental has nothing
    /// to hand back and the pragma is a no-op.
    fn release_free_pages(conn: &Connection) -> Result<()> {
        let mut stmt = conn.prepare("PRAGMA incremental_vacuum")?;
        let mut rows = stmt.query([])?;
        while rows.next()?.is_some() {}
        Ok(())
    }

    /// Refuse a store whose schema carries a trigger.
    ///
    /// No version of this schema has ever declared one, so a trigger in
    /// `sqlite_master` was put there by whoever produced the file. It runs with
    /// this process's authority on the next write: one survived `devmap build`
    /// and rewrote what the build stored, another made the build spin. A store
    /// is derived data; the remedy is the one for any untrusted store, to
    /// quarantine it and rebuild.
    pub(super) fn reject_triggers(conn: &Connection) -> Result<()> {
        let trigger: Option<String> = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'trigger' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        match trigger {
            None => Ok(()),
            Some(name) => Err(refusal(format!(
                "the store carries a trigger ({name:?}), which no devmap schema declares; \
                 refusing to run SQL somebody else put in the file — move or delete the \
                 store and run `devmap build`"
            ))),
        }
    }

    /// Refuse a store whose core views are not the ones this binary declares.
    ///
    /// A view is SQL the store runs on every read of it. The column gate
    /// answers for a view as it does for a table — the name and the columns
    /// exist — so a store that redefines `generation_edges` with the same
    /// columns and a `WHERE 0` passes it and answers every question from
    /// nothing, or from rows somebody else chose. The expectation is the
    /// schema's own, the views a fresh store builds, compared with whitespace
    /// collapsed: SQLite keeps the text it was given, so a reflowed definition
    /// is the same view.
    ///
    /// It runs from [`Self::validate_schema`], so after a migration and not
    /// before it: a store below the current schema legitimately carries the
    /// views of its own version until the ladder rebuilds them, and judging it
    /// before that would refuse every store that needs migrating. What the
    /// open reads through a view, it reads after this.
    pub(super) fn reject_redefined_views(conn: &Connection) -> Result<()> {
        for (name, declared) in declared_views() {
            let stored: Option<String> = conn
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE type = 'view' AND name = ?1",
                    params![name],
                    |row| row.get(0),
                )
                .optional()?;
            // Absent is the required-object gate's refusal to make, with its
            // own remedy; only a view that is present and different is ours.
            if let Some(stored) = stored {
                if collapse_whitespace(&stored) != collapse_whitespace(declared) {
                    return Err(refusal(format!(
                        "the store defines the view {name:?} differently from this binary's \
                         schema; refusing to read through SQL somebody else put in the file — \
                         move or delete the store and run `devmap build`"
                    )));
                }
            }
        }
        Ok(())
    }

    pub(super) fn validate_schema(conn: &Connection) -> Result<()> {
        Self::reject_triggers(conn)?;
        for (table, required_columns) in REQUIRED_SCHEMA {
            let object_type: Option<String> = conn
                .query_row(
                    "SELECT type FROM sqlite_master WHERE name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .optional()?;
            // A view satisfies this contract as fully as a table does, and
            // `generation_files` became one in v17 so that twenty-five read
            // sites could keep asking the same question after its payload moved
            // to a content-addressed table. What this validates is that the
            // *relation* exists and carries the columns readers name — which
            // `PRAGMA table_info` answers for a view exactly as for a table.
            if !matches!(object_type.as_deref(), Some("table") | Some("view")) {
                return Err(refusal(format!(
                    "required schema object {table:?} is neither a table nor a view"
                )));
            }

            let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
            let columns: std::collections::BTreeSet<String> = stmt
                .query_map([], |row| row.get(1))?
                .collect::<Result<_>>()?;
            for column in *required_columns {
                if !columns.contains(*column) {
                    return Err(refusal(format!(
                        "required column {table}.{column} is missing"
                    )));
                }
            }
        }

        // After the column gate, whose refusal is the more specific one when a
        // view lost a column, and before the first query that reads a view.
        Self::reject_redefined_views(conn)?;

        let identity_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_state WHERE singleton = 1
             AND length(epoch) = 32 AND epoch NOT GLOB '*[^0-9a-f]*'
             AND typeof(revision) = 'integer' AND revision >= 0",
            [],
            |row| row.get(0),
        )?;
        if identity_count != 1 {
            return Err(refusal(
                "pending queue identity is missing or invalid; refusing an unexamined queue",
            ));
        }

        // Indexes, which this gate did not look at until an absent one cost
        // every migrated store a full scan of `file_payloads` per cache miss.
        //
        // A missing index is not a correctness fault, which is exactly why it
        // needs a gate: nothing fails, every answer stays right, and the store
        // silently costs orders of magnitude more to read. That is the shape of
        // defect a test suite is worst at noticing.
        //
        // The expectation is derived from the DDL that creates them
        // (`declared_index_names`), never listed here, so this cannot drift the
        // way `REQUIRED_SCHEMA` would have.
        let present: std::collections::BTreeSet<String> = {
            let mut stmt = conn.prepare(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND name NOT LIKE 'sqlite_%'",
            )?;
            let names = stmt
                .query_map([], |row| row.get(0))?
                .collect::<Result<_>>()?;
            names
        };
        for index in declared_index_names() {
            if !present.contains(&index) {
                return Err(refusal(format!(
                    "required index {index} is missing and was not recreated; the store \
                     would answer correctly and scan for every answer — move or delete the \
                     store and run `devmap build`"
                )));
            }
        }
        Ok(())
    }

    /// Refusal text for a store whose schema this binary cannot handle.
    ///
    /// K3: the old messages were `unsupported future schema version 99` and
    /// `unsupported schema version 2` — no store path, no statement of what
    /// this binary supports, and no remedy. An operator with several stores on
    /// disk could not tell which one was refused, and nothing said whether the
    /// fix was to rebuild the kernel or to rebuild the database. Those are
    /// opposite actions and getting them the wrong way round destroys an index.
    ///
    /// The first phrase of the "older binary" remedy is what the Python seam
    /// matches to file the failure under `schema_newer_than_kernel`
    /// (`devmap_engine._FUTURE_SCHEMA_MARKER`, pinned to this source by a
    /// parity test); change it there and here together.
    pub(super) fn unsupported_schema(store: &str, found: i32) -> rusqlite::Error {
        unsupported_schema_error(store, found)
    }

    /// Downcast a store-open error to the stamped/expected schema versions.
    ///
    /// MCP and other shared readers must classify schema-behind by this typed
    /// path, not by matching `"schema"` / `"migrate"` substrings in Display text.
    pub fn unsupported_schema_versions(err: &rusqlite::Error) -> Option<(i32, i32)> {
        match err {
            rusqlite::Error::ToSqlConversionFailure(inner) => inner
                .downcast_ref::<UnsupportedSchema>()
                .map(|typed| (typed.found, typed.expected)),
            _ => None,
        }
    }

    /// Whether `err` is a typed unsupported-schema refusal from this store.
    pub fn is_unsupported_schema(err: &rusqlite::Error) -> bool {
        Self::unsupported_schema_versions(err).is_some()
    }

    /// The schema version stamped on an existing store, without migrating it.
    ///
    /// K3: `Store::open` runs the migration chain under an exclusive
    /// transaction from *every* open, so a read-only command like
    /// `devmap status` silently upgraded the store it was asked to describe.
    /// The probe is the owner peek: immutable when no `-wal` exists, because a
    /// read-only open of a WAL database creates `-wal` and `-shm` beside a
    /// store this command was only asked to describe. A `-wal` that is already
    /// there is opened read-only, so a frame that has not been checkpointed is
    /// still visible, and that open cannot migrate.
    ///
    /// `None` when no store exists at `db_path`. A file that exists but is not
    /// a database is an error, not a `None` — "there is nothing here" and "what
    /// is here is not readable" are different answers.
    pub fn stored_schema_version<P: AsRef<Path>>(db_path: P) -> Result<Option<i32>> {
        let path = db_path.as_ref();
        if !path.is_file() {
            return Ok(None);
        }
        let conn = Self::open_owner_peek(path)?;
        // The same wait every other connection gets. A store locked for a
        // moment — a vacuum, a competing opener — must make `status` and
        // `doctor` wait, not report a failure.
        conn.busy_timeout(Self::BUSY_TIMEOUT)?;
        let version: i32 = match conn.query_row("PRAGMA user_version", [], |row| row.get(0)) {
            Ok(version) => version,
            // A WAL store in a directory this process cannot write: the same
            // shape `Store::open` handles, reached here first because `status`
            // and `doctor` probe the schema before opening.
            Err(error) if Self::directory_refused_the_wal(&error) => {
                let conn = Self::open_immutable(path)?;
                conn.busy_timeout(Self::BUSY_TIMEOUT)?;
                conn.query_row("PRAGMA user_version", [], |row| row.get(0))?
            }
            Err(error) => return Err(error),
        };
        Ok(Some(version))
    }

    /// Whether the migration chain has a path from `version` to
    /// [`CURRENT_SCHEMA_VERSION`].
    ///
    /// The single owner of that question. It was previously implicit in the
    /// shape of [`Self::migrate`] — a version with no `if` arm fell through to
    /// the final equality check — which meant the only way to *ask* was to run
    /// the migration, and running the migration meant having already written to
    /// the file. `Store::open` needs the answer before it writes anything, so
    /// the predicate is stated once and consulted from both places.
    ///
    /// 0 is a store with no schema yet; 1 and 2 are the Python engine's
    /// databases, which this kernel never wrote and cannot read.
    pub fn schema_is_migratable(version: i32) -> bool {
        version == 0 || (3..=CURRENT_SCHEMA_VERSION).contains(&version)
    }

    pub(super) fn migrate(conn: &mut Connection, store: &str) -> Result<()> {
        // Hold one SQLite writer transaction from the version read through
        // validation. Per-rung transactions let a slow opener stamp an older
        // version over a peer's completed upgrade. Failure rolls back the whole
        // migration; no reader can observe a partially upgraded schema.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::migrate_locked(&tx, store)?;
        tx.commit()
    }

    fn migrate_locked(conn: &Connection, store: &str) -> Result<()> {
        let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if !Self::schema_is_migratable(version) {
            return Err(Self::unsupported_schema(store, version));
        }
        let mut version = version;
        if version == 0 {
            let tx = conn;
            tx.execute_batch(CREATE_SCHEMA_V3)?;
            tx.execute_batch(BUILD_HISTORY_TABLE)?;
            // Probed rather than unconditional, for the same reason the
            // v6→v7 step probes: `ADD COLUMN` is not idempotent, so a
            // partially-created store must not make this fatal.
            if !Self::has_column(tx, "generations", "repo_root")? {
                tx.execute_batch(MIGRATION_V6_TO_V7)?;
            }
            // A fresh database stamps CURRENT_SCHEMA_VERSION directly and
            // never runs the migration chain, so every table added by a
            // later migration must also be created here.
            //
            // `VALIDITY_RANGE_TABLES` stands where `UNRESOLVED_TABLE` used
            // to: since v18 the unresolved ledger *is* a view over
            // `unresolved_rows`, and applying the v9 batch here would try to
            // index that view. `UNRESOLVED_TABLE` remains the v8→v9 rung for
            // stores old enough to need it.
            tx.execute_batch(VALIDITY_RANGE_TABLES)?;
            tx.execute_batch(COVERAGE_GAPS_TABLE)?;
            tx.execute_batch(MIGRATION_V18_TO_V19)?;
            tx.execute_batch(MIGRATION_V19_TO_V20)?;
            // A fresh store never creates the two indexes v21 drops, so this is
            // a no-op here. Applied anyway, because the fresh path and the
            // ladder must land on the same schema: the one thing this batch
            // list exists to guarantee is that a store built from scratch and a
            // store migrated up are indistinguishable, and a rung skipped here
            // "because it cannot matter" is how that stops being true.
            tx.execute_batch(MIGRATION_V20_TO_V21)?;
            // Applied on the fresh path too, over the empty v18 shape
            // `VALIDITY_RANGE_TABLES` just created. Its backfill copies nothing
            // and its `DROP TABLE`/`RENAME` land the same columns a migrated
            // store gets, which is the property `FRESH_SCHEMA_BATCHES` exists
            // to hold: a store built from scratch and one walked up the ladder
            // are indistinguishable.
            tx.execute_batch(MIGRATION_V21_TO_V22)?;
            // After v21's drop of the same index, as on the ladder.
            tx.execute_batch(MIGRATION_V23_TO_V24)?;
            tx.execute_batch(MIGRATION_V25_TO_V26)?;
            Self::add_dead_evidence_columns(tx)?;
            // Over the empty v27 shape just built, like the v22 rung above:
            // the fresh path and the ladder land on one schema because they run
            // one batch.
            Self::migrate_to_v28(tx)?;
            Self::migrate_to_v29(tx)?;
            // That rung drops the six flat tables the batches above just made
            // empty, which on a store with no content yet leaves their pages
            // on the freelist: a brand-new file that already reads as "worth
            // reclaiming" to `vacuum_if_needed`. Nothing is referenced by
            // them, so give the pages back before the file is first used.
            Self::release_free_pages(tx)?;
            Self::stamp_reader_compat(tx)?;
            Self::heal_seed_rebind_table(tx)?;
            Self::validate_schema(tx)?;
            tx.execute(
                &format!("PRAGMA user_version = {}", CURRENT_SCHEMA_VERSION),
                [],
            )?;
            return Ok(());
        }
        if version == 3 {
            let tx = conn;
            tx.execute_batch(CREATE_SCHEMA_V3)?;
            tx.execute_batch(MIGRATION_V3_TO_V4)?;
            tx.execute("PRAGMA user_version = 4", [])?;
            version = 4;
        }
        if version == 4 {
            let tx = conn;
            tx.execute_batch(CREATE_SCHEMA_V3)?;
            tx.execute_batch(MIGRATION_V4_TO_V5)?;
            // v5's two edge indexes name `generation_edges`, which v18 turned
            // into a view — and `CREATE INDEX` on a view is an error, not a
            // no-op. Same probe, same reason, as the v12→v13 step below: this
            // rung meets whatever `CREATE_SCHEMA_V3` above left, and on a store
            // that already carries the current shape that is a view.
            if Self::relation_is_table(tx, "generation_edges")? {
                tx.execute_batch(MIGRATION_V4_TO_V5_EDGE_INDEXES)?;
            }
            let has_analysis_json = {
                let mut stmt = tx.prepare("PRAGMA table_info(generations)")?;
                let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
                let mut found = false;
                for column in columns {
                    if column? == "analysis_json" {
                        found = true;
                        break;
                    }
                }
                found
            };
            if !has_analysis_json {
                tx.execute(
                    "ALTER TABLE generations ADD COLUMN analysis_json TEXT NOT NULL
                     DEFAULT '{\"total_files\":0,\"total_symbols\":0,\"total_edges\":0,\"dead_symbols\":[],\"communities\":[],\"status\":\"Ok\"}'",
                    [],
                )?;
            }
            // Stamp exactly 5, never `CURRENT_SCHEMA_VERSION`. Stamping the
            // moving target would mark this database as carrying every later
            // migration's tables while creating none of them.
            tx.execute("PRAGMA user_version = 5", [])?;
            version = 5;
        }
        if version == 5 {
            let tx = conn;
            tx.execute_batch(MIGRATION_V5_TO_V6)?;
            tx.execute("PRAGMA user_version = 6", [])?;
            // No validation mid-chain: `validate_schema` asserts the *current*
            // schema, which a v6 database legitimately does not satisfy yet.
            // The end-of-migration check below is the authoritative gate.
            version = 6;
        }
        if version == 6 {
            let tx = conn;
            // `ADD COLUMN` is not idempotent, and a database can reach this step
            // already carrying the column (a re-stamped user_version, or a fresh
            // create that applied the current schema before migrating). Probe
            // first so re-running the step is safe rather than fatal.
            if !Self::has_column(tx, "generations", "repo_root")? {
                tx.execute_batch(MIGRATION_V6_TO_V7)?;
            }
            tx.execute("PRAGMA user_version = 7", [])?;
            // No mid-chain validation: `validate_schema` asserts the *current*
            // schema, which a v7 database legitimately does not satisfy yet.
            version = 7;
        }
        if version == 7 {
            let tx = conn;
            // Same idempotency probe as v7: `ADD COLUMN` is not repeatable, and
            // a database can arrive here already carrying the columns from a
            // fresh create that applied the current schema before migrating.
            if !Self::has_column(tx, "generation_files", "grammar_version")? {
                tx.execute_batch(MIGRATION_V7_TO_V8)?;
            }
            tx.execute("PRAGMA user_version = 8", [])?;
            // No mid-chain validation, for the same reason as v7 above:
            // `validate_schema` asserts the *current* schema, and a v8 database
            // legitimately does not satisfy it until v9 adds
            // `generation_unresolved`. The final validation below covers it.
            version = 8;
        }
        if version == 8 {
            let tx = conn;
            // `CREATE TABLE IF NOT EXISTS` is idempotent, so this needs no
            // probe — but the two indexes beside it are not: since v18
            // `generation_unresolved` may already be a view, and indexing one
            // is an error. Skipped whole rather than split, because the table
            // and its indexes are one shape: if the relation is not a table,
            // none of this batch applies.
            if !Self::relation_exists(tx, "generation_unresolved")? {
                tx.execute_batch(MIGRATION_V8_TO_V9)?;
            }
            tx.execute("PRAGMA user_version = 9", [])?;
            // No mid-chain validation: `validate_schema` asserts the *current*
            // schema, and a v9 database legitimately lacks the v10
            // `classification` column until the next step adds it.
            version = 9;
        }
        if version == 9 {
            let tx = conn;
            // Same idempotency probe as v7/v8: `ADD COLUMN` is not repeatable,
            // and a fresh create applies the current `UNRESOLVED_TABLE`, which
            // already carries the column, before this chain runs.
            //
            // The `relation_is_table` half is v18's: the batch both adds a
            // column and creates an index, and neither is legal against the
            // view `generation_unresolved` became.
            if Self::relation_is_table(tx, "generation_unresolved")?
                && !Self::has_column(tx, "generation_unresolved", "classification")?
            {
                tx.execute_batch(MIGRATION_V9_TO_V10)?;
            }
            tx.execute("PRAGMA user_version = 10", [])?;
            // No mid-chain validation: a v10 database legitimately lacks the
            // v11 `receiver` column until the next step adds it.
            version = 10;
        }
        if version == 10 {
            let tx = conn;
            if Self::relation_is_table(tx, "generation_unresolved")?
                && !Self::has_column(tx, "generation_unresolved", "receiver")?
            {
                tx.execute_batch(MIGRATION_V10_TO_V11)?;
            }
            tx.execute("PRAGMA user_version = 11", [])?;
            // No mid-chain validation: a v11 database legitimately lacks the
            // v12 body-signature columns until the next step adds them.
            version = 11;
        }
        if version == 11 {
            let tx = conn;
            if !Self::has_column(tx, "generation_nodes", "body_exact")? {
                tx.execute_batch(MIGRATION_V11_TO_V12)?;
            }
            tx.execute("PRAGMA user_version = 12", [])?;
            // No mid-chain validation: v13 adds the extraction-cache index
            // below, and the end-of-chain check is the authoritative one.
            version = 12;
        }
        if version == 12 {
            let tx = conn;
            // `CREATE INDEX IF NOT EXISTS` is idempotent, but it is not legal
            // on a view, and `generation_files` became one in v17. A fresh
            // store applies `CREATE_SCHEMA_V3` — which carries the current
            // shape, as every later migration's probe assumes — and then walks
            // this chain, so this step *does* meet a view and must ask first.
            // The index it creates has a successor there:
            // `idx_file_payloads_identity`, on the same four columns, over one
            // row per distinct payload instead of one per generation and file.
            if Self::relation_is_table(tx, "generation_files")? {
                tx.execute_batch(MIGRATION_V12_TO_V13)?;
            }
            tx.execute("PRAGMA user_version = 13", [])?;
            // No mid-chain validation: v14 adds the coverage-gap inventory and
            // the edge resolution column below, and the end-of-chain check is
            // the authoritative one.
            version = 13;
        }
        if version == 13 {
            let tx = conn;
            // `CREATE TABLE IF NOT EXISTS` is idempotent, so this needs no
            // probe — unlike the ADD COLUMN migrations above.
            tx.execute_batch(COVERAGE_GAPS_TABLE)?;
            tx.execute("PRAGMA user_version = 14", [])?;
            // No mid-chain validation: v15 adds the edge resolution column
            // below, and the end-of-chain check is the authoritative one.
            version = 14;
        }
        if version == 14 {
            let tx = conn;
            // Same idempotency probe as v7/v8/v10/v11: `ADD COLUMN` is not
            // repeatable, and a fresh create applies `CREATE_SCHEMA_V3`, which
            // already carries the column, before this chain runs.
            // The `relation_is_table` half is v18's: `ALTER TABLE ... ADD
            // COLUMN` cannot name the view `generation_edges` became.
            if Self::relation_is_table(tx, "generation_edges")?
                && !Self::has_column(tx, "generation_edges", "resolution")?
            {
                tx.execute_batch(MIGRATION_V14_TO_V15)?;
            }
            tx.execute("PRAGMA user_version = 15", [])?;
            // No mid-chain validation, for the same reason as every step above:
            // `validate_schema` asserts the *current* schema, and a v15 database
            // legitimately lacks `generation_edges.candidate_total` until v16
            // adds it and the `file_payloads` split until v17. The call that
            // stood here made `Store::open` fail outright — "required column
            // generation_edges.candidate_total is missing" — for every store
            // stamped 5 through 14, which is every installation that had not
            // already been migrated. The end-of-chain check below is the
            // authoritative gate, and `migration_ladder.rs` walks every rung.
            version = 15;
        }
        if version == 15 {
            let tx = conn;
            // Same idempotency probe as v7/v8/v10/v11/v14.
            if Self::relation_is_table(tx, "generation_edges")?
                && !Self::has_column(tx, "generation_edges", "candidate_total")?
            {
                tx.execute_batch(MIGRATION_V15_TO_V16)?;
            }
            tx.execute("PRAGMA user_version = 16", [])?;
            // No mid-chain validation: a v16 database legitimately predates the
            // v17 payload split. It happened to satisfy `validate_schema`
            // because `REQUIRED_SCHEMA` names no v17-only column — which is an
            // accident of that list, not a property of the schema, and is
            // exactly the kind of accident the rule exists to stop relying on.
            version = 16;
        }
        if version == 16 {
            let tx = conn;
            // Not an `ADD COLUMN`, so the idempotency probe is different: the
            // step is complete exactly when `generation_files` has become a
            // view. A fresh create applies `CREATE_SCHEMA_V3`, which already
            // carries the split, before this chain runs.
            let already_split: bool = tx
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                      WHERE name = 'generation_files' AND type = 'view'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count > 0)?;
            if !already_split {
                tx.execute_batch(MIGRATION_V16_TO_V17)?;
            }
            tx.execute("PRAGMA user_version = 17", [])?;
            // No mid-chain validation: `validate_schema` asserts the *current*
            // schema, and a v17 database legitimately has `generation_edges` as
            // a table and no `edge_rows` until the step below runs.
            version = 17;
        }
        if version == 17 {
            let tx = conn;
            // Each relation is asked about separately, and "is it still a
            // base table?" is the whole question: absent means there is
            // nothing to carry, a view means this rung already ran, and only a
            // table has rows that need moving onto ranges.
            //
            // A single probe over `generation_edges` was the first shape of
            // this step and it was wrong for a store that has one relation and
            // not the other — a hand-built v3 fixture picks up
            // `generation_unresolved` at rung 9 and never acquires a
            // `generation_edges` at all, and the single probe read that as
            // "already migrated" and left the store with no edge relation.
            let carry_edges = Self::relation_is_table(tx, "generation_edges")?;
            let carry_unresolved = Self::relation_is_table(tx, "generation_unresolved")?;
            if carry_edges {
                tx.execute_batch(MIGRATION_V17_TO_V18_RENAME_EDGES)?;
            }
            if carry_unresolved {
                tx.execute_batch(MIGRATION_V17_TO_V18_RENAME_UNRESOLVED)?;
            }
            // Unconditional, and idempotent by `IF NOT EXISTS`: the v18 shape
            // must exist at the end of this rung however the store arrived.
            tx.execute_batch(VALIDITY_RANGE_TABLES)?;
            if carry_edges {
                tx.execute_batch(MIGRATION_V17_TO_V18_BACKFILL_EDGES)?;
            }
            if carry_unresolved {
                tx.execute_batch(MIGRATION_V17_TO_V18_BACKFILL_UNRESOLVED)?;
            }
            tx.execute("PRAGMA user_version = 18", [])?;
            // No mid-chain validation: `validate_schema` asserts the *current*
            // schema, and a v18 database legitimately has no
            // `generation_file_digests` until the step below runs.
            version = 18;
        }
        if version == 18 {
            let tx = conn;
            // Purely additive, and idempotent by `IF NOT EXISTS`. There is no
            // backfill and there deliberately cannot be one: a digest is a
            // function of the resolver's output for a file, and SQL cannot
            // re-derive that from the stored rows without deciding, per file,
            // which of them the *next* build would still want — which is the
            // question the write path answers and this table only caches. An
            // absent digest reads as "unknown" and makes the next build compare
            // that file's rows exactly as v18 did, so the empty table is a
            // correct starting state rather than a gap to be filled.
            tx.execute_batch(MIGRATION_V18_TO_V19)?;
            tx.execute("PRAGMA user_version = 19", [])?;
            version = 19;
        }
        if version == 19 {
            if !Self::has_column(conn, "generations", "repo_root")? {
                return Err(refusal("required column generations.repo_root is missing"));
            }
            if !Self::has_column(conn, "pending_paths", "revision")? {
                conn.execute_batch(MIGRATION_V19_TO_V20)?;
            } else if !Self::relation_is_table(conn, "pending_state")? {
                return Err(refusal(
                    "pending revisions exist without their durable store identity",
                ));
            }
            // A re-entered migration must preserve an already established epoch
            // and counter, including claims held by another process.
            conn.execute("PRAGMA user_version = 20", [])?;
            // 20, not `CURRENT_SCHEMA_VERSION`. This rung leaves the store at
            // exactly the version it just stamped, and the next `if` decides
            // what follows; assigning the constant here would have made every
            // future rung unreachable for a store arriving at v19, which is the
            // silent kind of migration bug — the store reports current and is
            // missing the work.
            version = 20;
        }
        if version == 20 {
            // Idempotent (`DROP INDEX IF EXISTS`) and outside a transaction of
            // its own for the same reason the rungs above are: `run_migrations`
            // is already called inside one.
            conn.execute_batch(MIGRATION_V20_TO_V21)?;
            conn.execute("PRAGMA user_version = 21", [])?;
            version = 21;
        }
        if version == 21 {
            // The ledger's three interned columns. Unlike every rung above it
            // this one *moves rows*, so it counts them.
            //
            // The backfill is three joins — path, reason text, classification
            // text — and a join drops the rows it cannot match rather than
            // failing. Every value was inserted into its pool one statement
            // earlier so none can miss, but "cannot miss" is an argument and a
            // silently shorter ledger is the failure it would be making: the
            // store would open clean, and the missing rows would read as calls
            // that resolved. Counted before and after, and refused loudly on
            // any difference, inside the same transaction that did the move.
            // Guarded on the shape, not on the stamp, the way the v19 rung is.
            //
            // A rung reached with its work already done is not hypothetical
            // here: two processes opening one mid-chain store race through this
            // ladder, and the loser arrives with `version` read before the
            // winner committed. This batch reads `unresolved_rows.reason`, a
            // column the winner has just removed, so unguarded it fails the
            // whole open with `no such column: reason` — which is exactly what
            // `concurrent_openers_of_a_mid_chain_store_converge_on_one_schema`
            // and `the_extraction_cache_fallback_uses_an_index_rather_than_
            // scanning` caught, the latter by re-stamping a current store to
            // v12 and walking it up.
            //
            // The stamp still advances in both arms. A store that already has
            // the interned shape *is* a v22 store; refusing to say so would
            // leave it walking this rung on every open forever.
            if Self::has_column(conn, "unresolved_rows", "reason")? {
                let before: i64 =
                    conn.query_row("SELECT COUNT(*) FROM unresolved_rows", [], |row| row.get(0))?;
                conn.execute_batch(MIGRATION_V21_TO_V22)?;
                let after: i64 =
                    conn.query_row("SELECT COUNT(*) FROM unresolved_rows", [], |row| row.get(0))?;
                if before != after {
                    return Err(refusal(format!(
                        "interning the unresolved-call ledger lost rows: {before} before, \
                         {after} after; the store has been left unmigrated"
                    )));
                }
            }
            conn.execute("PRAGMA user_version = 22", [])?;
            version = 22;
        }
        if version == 22 {
            // Version-only rung: LanguageServer / LanguageServerDispatch kinds
            // are free TEXT on the existing resolution column. Advancing the
            // stamp makes an older binary refuse the store rather than
            // reconstructing those rows as neighbouring tiers.
            conn.execute("PRAGMA user_version = 23", [])?;
            version = 23;
        }
        if version == 23 {
            // `CREATE INDEX IF NOT EXISTS`, so a racing opener that already
            // built it makes this a no-op rather than a failure.
            conn.execute_batch(MIGRATION_V23_TO_V24)?;
            conn.execute("PRAGMA user_version = 24", [])?;
            version = 24;
        }
        if version == 24 {
            // Version-only rung: `Registers` edges (route middleware) are free
            // TEXT in the existing `edge_kind` column. Advancing the stamp
            // makes an older binary refuse the store at open instead of
            // failing every edge read on a kind it cannot parse.
            conn.execute("PRAGMA user_version = 25", [])?;
            version = 25;
        }
        if version == 25 {
            // Not over a view: a store re-stamped to an earlier version and
            // walked up carries v28's `generation_literals` view, and indexing
            // a view is an error. The table is absent (a real v25 store) or the
            // rung's own.
            if Self::relation_is_table(conn, "generation_literals")?
                || !Self::relation_exists(conn, "generation_literals")?
            {
                conn.execute_batch(MIGRATION_V25_TO_V26)?;
            }
            conn.execute("PRAGMA user_version = 26", [])?;
            version = 26;
        }
        if version == 26 {
            Self::add_dead_evidence_columns(conn)?;
            conn.execute("PRAGMA user_version = 27", [])?;
            version = 27;
        }
        if version == 27 {
            Self::migrate_to_v28(conn)?;
            conn.execute("PRAGMA user_version = 28", [])?;
            version = 28;
        }
        if version == 28 {
            Self::migrate_to_v29(conn)?;
            conn.execute("PRAGMA user_version = 29", [])?;
            version = 29;
        }
        if version != CURRENT_SCHEMA_VERSION {
            return Err(Self::unsupported_schema(store, version));
        }
        // A store already at the current version can still be missing what a
        // later build of the same version added to the fresh schema.
        Self::heal_payload_digest(conn)?;
        Self::heal_seed_rebind_table(conn)?;
        // Whatever a same-version writer left unconverted stays so, and the
        // readers refuse it by name; only the migration refuses the store. The
        // list is not dropped: `Store::status` asks the same question of the
        // store (`payload_encoding_reason_locked`) and names the paths in
        // `degraded_reason`, so an open that could not convert is visible.
        Self::heal_extraction_encoding(conn)?;
        Self::heal_declared_indexes(conn)?;
        Self::stamp_reader_compat(conn)?;
        Self::validate_schema(conn)?;
        Ok(())
    }
}

fn collapse_whitespace(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The views a fresh store defines, `(name, sql)`, built once by running the
/// schema's own batches in a throwaway in-memory database.
fn declared_views() -> &'static [(String, String)] {
    static VIEWS: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    VIEWS.get_or_init(|| {
        let conn = Connection::open_in_memory().expect("an in-memory database opens");
        for batch in crate::schema::FRESH_SCHEMA_BATCHES {
            conn.execute_batch(batch)
                .expect("the schema's own batches run on an empty database");
        }
        let mut stmt = conn
            .prepare("SELECT name, sql FROM sqlite_schema WHERE type = 'view' ORDER BY name")
            .expect("sqlite_schema is readable");
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("sqlite_schema is readable")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("sqlite_schema rows decode");
        rows
    })
}
