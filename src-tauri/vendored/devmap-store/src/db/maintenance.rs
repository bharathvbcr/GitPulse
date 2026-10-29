use super::{
    lock_conn, refusal, PageSizeConversion, Store, VacuumAction, VacuumOutcome, WalCheckpointMode,
    WalCheckpointResult,
};
use rusqlite::OptionalExtension;
use rusqlite::{params, Connection, Result, TransactionBehavior};

impl Store {
    /// Every file indexed by one named generation.
    ///
    /// Refuses a generation the store does not hold, rather than answering
    /// `[]`. This is the only reader that takes its generation id from the
    /// caller, and every caller resolves that id in a *separate* call —
    /// `devmap-query`'s `savings` does `latest_generation_id()` and then this,
    /// with a prune-capable writer free to commit twice in between. Measured
    /// against the pre-refusal code, all three of these returned the same
    /// `Ok([])`: a live generation holding one file, that same generation once
    /// `prune_generations_except_latest` had removed it, and generation 9999,
    /// which was never written.
    ///
    /// So a lost generation reached `savings` as `corpus_bytes: 0,
    /// corpus_files_unreadable: 0` — a repository with nothing in it — and the
    /// report whose own documentation refuses to count an unreadable file as
    /// zero bytes did exactly that one level up. "Not in this store" and
    /// "indexed no files" are different facts and only one of them is an
    /// answer.
    pub fn list_generation_paths(&self, generation_id: u32) -> Result<Vec<String>> {
        let conn = lock_conn(&self.conn)?;
        let present: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM generations WHERE id = ?1)",
            params![generation_id],
            |row| row.get::<_, i64>(0).map(|found| found != 0),
        )?;
        if !present {
            return Err(refusal(format!(
                "generation {generation_id} is not in this store — it was pruned \
                 or never written — so the files it indexed are unknown, not none; \
                 re-read the latest generation id and ask again"
            )));
        }
        let mut stmt = conn.prepare(
            "SELECT DISTINCT p.path FROM generation_nodes n
             JOIN paths p ON p.id = n.file_id
             WHERE n.generation_id = ?1",
        )?;
        let rows = stmt.query_map(params![generation_id], |row| row.get(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Fraction of the database that must be free before a full `VACUUM` earns
    /// its exclusive lock and whole-file rewrite.
    pub const VACUUM_FREELIST_RATIO: f64 = 0.05;

    /// Whether the current page accounting justifies a `VACUUM`.
    ///
    /// Split out from [`Store::vacuum_if_needed`] because the decision and the
    /// effect are separately wrong-able and only the decision is cheaply
    /// observable. Mutation testing replaced this predicate's `&&` with `||`
    /// and its `/` with `*` and `%` without any test failing: every surviving
    /// mutant still vacuumed in the one scenario under test, and below the
    /// threshold "declined to vacuum" and "vacuumed but reclaimed nothing" are
    /// indistinguishable from page counts alone. Exposed as a pure function so
    /// the policy can be asserted directly instead of inferred from a side
    /// effect it does not reliably produce.
    pub fn should_vacuum(freelist_count: i64, page_count: i64) -> bool {
        page_count > 0 && (freelist_count as f64 / page_count as f64) > Self::VACUUM_FREELIST_RATIO
    }

    /// Rewrite an existing store at [`Self::PAGE_SIZE`].
    ///
    /// Page size is fixed when a database first gets content, so a store
    /// written before the default was raised keeps its old one for life — the
    /// pragma in `configure_connection` is accepted and ignored, and the daemon
    /// reopens whatever it finds, so nothing in the normal course of running
    /// ever converts one. This is the supported way, and it is deliberately an
    /// operator action: the rewrite takes an exclusive lock and leaves WAL for
    /// its duration.
    ///
    /// `VACUUM` alone will not do it. SQLite refuses to change `page_size` on a
    /// WAL database and reports no error when it refuses, so the journal mode
    /// has to come down for the rewrite and go back up after. Measured on a
    /// 299 MB store: 2 s, 299 MB -> 296 MB.
    ///
    /// WAL is restored on the failure path too. A store left in DELETE mode
    /// still works but blocks readers behind every writer, which is a
    /// performance cliff nobody would attribute to a repair that errored.
    pub fn convert_page_size(&self) -> anyhow::Result<PageSizeConversion> {
        let _writer = self.lock_writer(Self::WRITER_LOCK_WAIT)?;
        let conn = lock_conn(&self.conn)?;
        let before: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        if before == Self::PAGE_SIZE {
            return Ok(PageSizeConversion {
                before,
                after: before,
                converted: false,
            });
        }

        let rewrite = (|| -> rusqlite::Result<()> {
            conn.pragma_update(None, "journal_mode", "DELETE")?;
            conn.pragma_update(None, "page_size", Self::PAGE_SIZE)?;
            conn.execute_batch("VACUUM")?;
            Ok(())
        })();
        // Back to WAL whether or not the rewrite worked.
        let restored = conn.pragma_update(None, "journal_mode", "WAL");
        rewrite?;
        restored?;

        let after: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        if after != Self::PAGE_SIZE {
            anyhow::bail!(
                "page size is still {after} after the rewrite; expected {}. The database was \
                 not converted and is unchanged.",
                Self::PAGE_SIZE
            );
        }
        Ok(PageSizeConversion {
            before,
            after,
            converted: true,
        })
    }

    /// Free pages one `vacuum_if_needed` will reclaim at most.
    ///
    /// Incremental vacuum costs time proportional to the pages it moves, so
    /// this bounds a single build's reclaim rather than the database's size.
    /// 65,536 pages is 256 MiB at the default 4 KiB page size — far above the
    /// per-build churn measured here (a prune frees on the order of 5% of the
    /// file), so the steady state reclaims everything in one pass and the cap
    /// only bites when a long-neglected store has accumulated a backlog. That
    /// backlog then drains over consecutive builds instead of stalling one.
    /// Expressed in bytes, then converted to pages against the page size the
    /// database actually has.
    ///
    /// This bound is on *time*, and the doc above says why: incremental vacuum
    /// costs time proportional to the pages it moves. Pages are not a fixed
    /// amount of work — a page is 4 KiB in a store written before the page size
    /// was raised and 16 KiB in one written after, so a constant expressed in
    /// pages means four times the bytes, and four times the stall, depending on
    /// which store it is applied to. It was 65,536 pages, calibrated at 4 KiB;
    /// 256 MiB is that same budget stated in the unit the cost is actually
    /// proportional to.
    pub(super) const INCREMENTAL_VACUUM_MAX_BYTES: i64 = 256 * 1024 * 1024;

    /// The cap above in pages, for a database with `page_size`-byte pages.
    ///
    /// Never zero: a page size larger than the whole budget would otherwise
    /// request a reclaim of nothing and report it as a bounded one, which is a
    /// check that could not run reporting as a check that passed.
    pub fn incremental_vacuum_max_pages(page_size: i64) -> i64 {
        if page_size <= 0 {
            return 1;
        }
        (Self::INCREMENTAL_VACUUM_MAX_BYTES / page_size).max(1)
    }

    /// How long a TRUNCATE checkpoint waits for a reader before falling back to
    /// PASSIVE. See [`Store::checkpoint_wal`] for why it is not zero.
    const CHECKPOINT_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

    /// Reclaim free pages, cheaply where the database allows it.
    ///
    /// **Why not a plain `VACUUM`.** `VACUUM` rebuilds the entire database into
    /// a new file: its cost is proportional to the *database*, not to the waste
    /// being reclaimed, and it takes an exclusive lock for the duration. Because
    /// every build prunes a generation, the freelist crosses
    /// [`Self::VACUUM_FREELIST_RATIO`] on essentially every build — so the
    /// whole-file rewrite ran nearly every time. Measured on DevCouncil's own
    /// store: 937 ms of a 3.40 s incremental build, 28% of the wall time, to
    /// reclaim a few percent of the file.
    ///
    /// `PRAGMA incremental_vacuum(N)` moves only free pages to the end and
    /// truncates, costing what the waste costs. It requires the database to
    /// have been created with `auto_vacuum = INCREMENTAL`; a database in mode
    /// NONE cannot be switched without a full rewrite, so those keep the old
    /// path. That is the honest fallback — an incremental vacuum on a mode-NONE
    /// database is a silent no-op, and a reclaim that quietly reclaims nothing
    /// is exactly the failure `vacuum_returns_freed_pages_to_the_filesystem`
    /// exists to catch.
    ///
    /// **The trade this makes, stated plainly.** A full `VACUUM` compacted the
    /// file to its live size every build; this does not. Measured over 15
    /// consecutive incremental builds of DevCouncil, the store settles at
    /// 295 MB against ~197 MB of live data and *stays there* — the free pages
    /// left by each prune are reused by the next generation's write instead of
    /// being returned to the filesystem and immediately re-allocated. So the
    /// cost is a bounded ~50% space overhead, not unbounded growth, and the
    /// bound is what makes it acceptable: the file did not move off 295 MB
    /// across those 15 builds, and the WAL stayed truncated. Reclaim time went
    /// from 937 ms to 2 ms over the same window.
    pub fn vacuum_if_needed(&self) -> Result<VacuumOutcome> {
        // Checkpoint before reading the page accounting.
        //
        // In WAL mode `PRAGMA freelist_count` reports the *main database file*.
        // Pages freed by the two prunes that run immediately before this live
        // in the WAL until a checkpoint folds them back, so the freelist read
        // here was reporting the state before this build's pruning — and it
        // read *below* the threshold while a third of the file was in fact
        // free. Measured on this repository: eight consecutive builds each
        // declined to reclaim in 0 ms while the freelist sat at 33.2% and the
        // store stayed pinned at 295 MB; a manual `incremental_vacuum` on the
        // same file immediately took it to 0.4% and 50,684 pages.
        //
        // A reclaim policy reading stale accounting does not merely reclaim
        // late — it reports "nothing to reclaim" with perfect confidence, which
        // is the failure mode that hides indefinitely. A checkpoint failure is
        // not fatal here: the decision is then made on the same stale numbers
        // as before, so this can only improve the accuracy of the answer, and
        // refusing to reclaim because bookkeeping was unavailable would be
        // worse than reclaiming on a conservative estimate.
        let checkpoint_before = self.checkpoint_wal().ok();

        let conn = lock_conn(&self.conn)?;
        let freelist_count: i64 = conn.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
        let page_count: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        if !Self::should_vacuum(freelist_count, page_count) {
            return Ok(VacuumOutcome {
                freelist_before: freelist_count,
                page_count_before: page_count,
                action: VacuumAction::Declined,
                // Nothing was reclaimed, so the checkpoint that matters is the
                // one taken above to make the accounting current.
                checkpoint: checkpoint_before,
                pages_freed: 0,
            });
        }
        // 0 = NONE, 1 = FULL, 2 = INCREMENTAL. Only 2 supports the pragma.
        let auto_vacuum: i64 = conn.query_row("PRAGMA auto_vacuum", [], |row| row.get(0))?;
        if auto_vacuum == 2 {
            let page_size: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
            let requested = freelist_count.min(Self::incremental_vacuum_max_pages(page_size));
            // Step the pragma to exhaustion, and count what it moved.
            //
            // `PRAGMA incremental_vacuum(N)` is not a statement that does its
            // work on the first step and then reports: it frees **one page per
            // row stepped**, up to N. Neither `execute` nor `execute_batch`
            // does that. `execute` refuses a statement that returns rows
            // outright (`ExecuteReturnedResults`), and `execute_batch` — the
            // workaround that was here — steps once and moves to the next
            // statement in the batch (rusqlite 0.31 `lib.rs::execute_batch`).
            // So the reclaim freed exactly one page per build, for as long as
            // this code has existed, while printing the number it had asked
            // for. Measured on the live store: 1 ms, one page, four builds in a
            // row, 701 MB unchanged at a 67.7% freelist.
            //
            // A PRAGMA argument cannot be bound as a parameter; `requested` is
            // derived from `PRAGMA freelist_count` and a compile-time constant,
            // never from a caller.
            let pages_freed = {
                let mut stmt = conn.prepare(&format!("PRAGMA incremental_vacuum({requested})"))?;
                let mut rows = stmt.query([])?;
                let mut freed: i64 = 0;
                while rows.next()?.is_some() {
                    freed += 1;
                }
                freed
            };
            // Checkpoint *after* the reclaim, not only before it. The
            // truncation the pragma just performed is a WAL frame; without this
            // it never reaches the main file, and the store reports pages
            // reclaimed while its size does not move. See
            // `VacuumOutcome::checkpoint`.
            drop(conn);
            let checkpoint = self.checkpoint_wal().ok();
            return Ok(VacuumOutcome {
                freelist_before: freelist_count,
                page_count_before: page_count,
                action: VacuumAction::Incremental { requested },
                checkpoint,
                pages_freed,
            });
        }

        // A store already in mode NONE is converted here rather than at open.
        // Switching `auto_vacuum` on a populated database only takes effect on
        // the next full rewrite — and this branch is that rewrite. The
        // conversion is therefore free: this build was going to pay for a
        // `VACUUM` either way, and every build after it takes the bounded path
        // above. Doing it in `open` instead would put a whole-file rewrite in
        // front of read commands like `devmap status`, which must stay cheap.
        conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
        conn.execute("VACUUM", [])?;
        drop(conn);
        let checkpoint = self.checkpoint_wal().ok();
        Ok(VacuumOutcome {
            freelist_before: freelist_count,
            page_count_before: page_count,
            action: VacuumAction::FullConverting,
            checkpoint,
            // A full `VACUUM` rewrites the file without its free pages, so
            // every page that was free is gone.
            pages_freed: freelist_count,
        })
    }

    /// Attempt to truncate the WAL and explicitly fall back to a non-blocking
    /// passive checkpoint when an active reader prevents truncation (S18).
    pub fn checkpoint_wal(&self) -> Result<WalCheckpointResult> {
        fn run(conn: &Connection, pragma: &str) -> Result<(i64, i64, i64)> {
            conn.query_row(pragma, [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
        }

        let conn = lock_conn(&self.conn)?;
        let previous_busy_ms: i64 = conn.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;
        let previous_busy_ms = u64::try_from(previous_busy_ms)
            .map_err(|_| refusal("SQLite returned a negative busy_timeout".to_string()))?;

        // TRUNCATE honors busy_timeout and could otherwise monopolize the
        // store mutex for seconds while a reader holds a snapshot. Bound the
        // wait instead of removing it, then use PASSIVE as the non-blocking
        // fallback.
        //
        // K2: the bound used to be zero, which is not a short wait — it is no
        // wait at all, and it loses to any reader that happens to hold the WAL
        // at that instant. PASSIVE then runs, and PASSIVE *cannot truncate*, so
        // the pages an incremental vacuum just freed stayed in a WAL that grew
        // to 109 MB while the main file never moved. A quarter of a second is
        // long enough to outlast a transient reader and short enough that no
        // build notices it.
        conn.busy_timeout(Self::CHECKPOINT_BUSY_TIMEOUT)?;
        let checkpoint = (|| {
            let (busy, log_frames, checkpointed_frames) =
                run(&conn, "PRAGMA wal_checkpoint(TRUNCATE)")?;
            if busy == 0 {
                return Ok(WalCheckpointResult {
                    mode: WalCheckpointMode::Truncate,
                    busy,
                    log_frames,
                    checkpointed_frames,
                });
            }

            let (busy, log_frames, checkpointed_frames) =
                run(&conn, "PRAGMA wal_checkpoint(PASSIVE)")?;
            Ok(WalCheckpointResult {
                mode: WalCheckpointMode::Passive,
                busy,
                log_frames,
                checkpointed_frames,
            })
        })();
        let restored = conn.busy_timeout(std::time::Duration::from_millis(previous_busy_ms));
        match (checkpoint, restored) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(result), Ok(())) => Ok(result),
        }
    }

    /// Rebuild the full-text index from the latest generation's symbol rows.
    ///
    /// The index is dropped and recreated rather than emptied. `DELETE FROM
    /// nodes_fts` has to read the index to remove each row's postings, so on
    /// exactly the damage this exists for — a corrupt structure record, the
    /// "database disk image is malformed" that `status` and search now name —
    /// it failed with the same error. Dropping an FTS5 table drops its shadow
    /// tables without reading them. The definition is taken from the store's
    /// own `sqlite_master`, so the table comes back as this store had it rather
    /// than as a second copy of the DDL here would say.
    pub fn repair_fts(&self) -> Result<()> {
        let mut conn = lock_conn(&self.conn)?;
        let tx = conn.transaction()?;
        let definition: String = tx
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'nodes_fts'",
                [],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                refusal(
                    "this store has no `nodes_fts` table to repair; it is not a current \
                     DevMap store — run `devmap build`",
                )
            })?;
        tx.execute_batch("DROP TABLE nodes_fts")?;
        tx.execute_batch(&definition)?;
        tx.execute("DELETE FROM nodes_fts_map", [])?;
        let gen: Option<u32> = tx
            .query_row(
                "SELECT id FROM generations ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(g) = gen {
            let mut stmt = tx.prepare(
                "SELECT n.ordinal, n.name, n.qualified_name, p.path
                 FROM generation_nodes n
                 JOIN paths p ON n.file_id = p.id
                 WHERE n.generation_id = ?1",
            )?;
            let rows = stmt.query_map(params![g], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            let collected: Vec<_> = rows.collect::<Result<Vec<_>>>()?;
            drop(stmt);
            for (ord, name, qn, path) in collected {
                let fts_rowid = Self::fts_rowid(g, ord);
                tx.prepare_cached(
                    "INSERT INTO nodes_fts (rowid, name, qualified_name, path) VALUES (?1, ?2, ?3, ?4)",
                )?
                .execute(params![fts_rowid, name, qn, path])?;
                tx.prepare_cached(
                    "INSERT INTO nodes_fts_map (rowid_ref, generation_id) VALUES (?1, ?2)",
                )?
                .execute(params![fts_rowid, g])?;
            }
        }
        tx.commit()?;
        // The memo described the index this just replaced.
        if let Ok(mut cache) = self.fts_reachable.lock() {
            *cache = None;
        }
        Ok(())
    }

    pub fn prune_generations_except_latest(&self, keep_generations: usize) -> Result<usize> {
        let mut conn = lock_conn(&self.conn)?;

        // Always retain the latest generation; the method name promises that
        // older generations are pruned while the current one remains usable.
        let keep_generations = keep_generations.max(1);

        // The candidate list is read inside the write transaction. Choosing the
        // rows to delete and deleting them is one decision: a DEFERRED
        // transaction would let a concurrent writer commit a new generation
        // between the SELECT and the DELETEs, so the stale list could prune a
        // generation that is now within the retention window.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let gen_ids: Vec<u32> = {
            let mut stmt = tx.prepare("SELECT id FROM generations ORDER BY id DESC")?;
            let ids = stmt
                .query_map([], |row| row.get(0))?
                .collect::<Result<Vec<_>>>()?;
            ids
        };

        if gen_ids.len() <= keep_generations {
            return Ok(0);
        }

        let to_prune = &gen_ids[keep_generations..];
        let mut pruned_count = 0;

        for &old_gen in to_prune {
            tx.execute(
                "DELETE FROM nodes_fts WHERE rowid IN (SELECT rowid_ref FROM nodes_fts_map WHERE generation_id = ?1)",
                params![old_gen],
            )?;
            tx.execute(
                "DELETE FROM nodes_fts_map WHERE generation_id = ?1",
                params![old_gen],
            )?;
            tx.execute(
                "DELETE FROM generation_nodes WHERE generation_id = ?1",
                params![old_gen],
            )?;
            tx.execute(
                "DELETE FROM generation_file_rows WHERE generation_id = ?1",
                params![old_gen],
            )?;
            tx.execute(
                "DELETE FROM generation_coverage_gaps WHERE generation_id = ?1",
                params![old_gen],
            )?;
            // The v19 digests go with their generation like every other
            // per-generation copy. Only the newest generation's are ever read —
            // it is the one the live rows belong to — and it is the one
            // retention keeps by construction, so this deletes rows nothing
            // would consult rather than rows something needs.
            tx.execute(
                "DELETE FROM generation_file_digests WHERE generation_id = ?1",
                params![old_gen],
            )?;
            tx.execute(
                "DELETE FROM generation_dead_symbols WHERE generation_id = ?1",
                params![old_gen],
            )?;
            tx.execute(
                "DELETE FROM generation_literals WHERE generation_id = ?1",
                params![old_gen],
            )?;
            tx.execute("DELETE FROM generations WHERE id = ?1", params![old_gen])?;
            pruned_count += 1;
        }

        // Edges and unresolved calls are not deleted per generation: since v18
        // one row covers the whole range of generations it was valid for, and
        // deleting it because *one* of them went away would take it from the
        // retained ones too.
        //
        // What becomes unreachable instead is any row whose validity had already
        // ended by the oldest generation still retained — `valid_to <= cutoff`
        // is exactly "no retained generation can see this". Rows still open, and
        // rows closed later than the cutoff, are untouched. `keep_generations`
        // is at least 1 and the early return above proved there are more
        // generations than that, so `gen_ids[keep_generations - 1]` is the
        // oldest retained id.
        //
        // `idx_edge_rows_closed` and `idx_unresolved_rows_closed` make this a
        // scan of the closed rows rather than of the whole table. They are the
        // only partial indexes v18 keeps: the matching `valid_to IS NULL` half
        // made SQLite plan every *read* as a MULTI-INDEX OR over 102,083 rowid
        // lookups and cost a cold `impact` 40 ms — see `VALIDITY_RANGE_TABLES`.
        let cutoff = gen_ids[keep_generations - 1];
        tx.execute(
            "DELETE FROM edge_rows WHERE valid_to IS NOT NULL AND valid_to <= ?1",
            params![cutoff],
        )?;
        tx.execute(
            "DELETE FROM unresolved_rows WHERE valid_to IS NOT NULL AND valid_to <= ?1",
            params![cutoff],
        )?;

        // A payload outlives its generation only for as long as some *other*
        // generation still names it. Deleting the membership rows above frees
        // nothing on its own — the bytes are in `file_payloads`, and since v17
        // that is where 54% of this store lives — so the orphans go too.
        //
        // Deferred to after the loop rather than run per generation: a payload
        // shared by two pruned generations would otherwise be probed twice, and
        // the anti-join is one index scan either way.
        tx.execute(
            "DELETE FROM file_payloads
              WHERE payload_id NOT IN (SELECT payload_id FROM generation_file_rows)",
            [],
        )?;

        // Interned paths are shared by every retained generation and by edge
        // endpoints that need not have an extraction payload. Retire an id
        // only after every persisted reference has gone. Otherwise repeated
        // renames grow this table forever, and PathRanks reads and sorts the
        // entire history on every edge load. Keep this inside the same
        // transaction so a failed retirement also rolls back the prune.
        tx.execute(
            "DELETE FROM paths WHERE id NOT IN (
                SELECT file_id FROM generation_nodes
                UNION SELECT file_id FROM file_payloads
                UNION SELECT file_id FROM generation_file_rows
                UNION SELECT file_id FROM generation_file_digests
                UNION SELECT source_file_id FROM edge_rows
                UNION SELECT target_file_id FROM edge_rows
                UNION SELECT source_file_id FROM unresolved_rows
             )",
            [],
        )?;

        // The ledger's interning pool, retired the same way and for the same
        // reason. Since v22 `reason` and `classification` are ids into
        // `unresolved_texts`, and a reason text names the file and symbol it is
        // about — so the pool churns as the tree does and would grow for the
        // life of the store if nothing retired it.
        //
        // `NOT IN` over the two id columns, not a probe per candidate: SQLite
        // materialises each subquery once, so this is two scans of
        // `unresolved_rows` and one of the pool, and it needs no index on
        // `reason_id`. An index there would put back exactly the per-row write
        // amplification v21 removed, to serve a statement that runs once a
        // prune.
        //
        // After the deletes above, so the rows whose texts this is deciding
        // about are already gone.
        tx.execute(
            "DELETE FROM unresolved_texts WHERE id NOT IN (
                SELECT reason_id FROM unresolved_rows
                UNION SELECT classification_id FROM unresolved_rows
             )",
            [],
        )?;

        // FTS5 deletes only tombstone their postings; without a merge the freed
        // space stays inside the index and the prune reclaims nothing there.
        //
        // Unconditional by construction: the early return above leaves
        // `gen_ids.len() > keep_generations`, so `to_prune` is never empty and
        // the loop always deleted at least one generation. A `pruned_count > 0`
        // guard here was always true — mutation testing flagged it precisely
        // because no test could distinguish its branches.
        tx.execute("INSERT INTO nodes_fts(nodes_fts) VALUES('optimize')", [])?;

        tx.commit()?;
        Ok(pruned_count)
    }

    /// Drop cached extractions no retained generation can still use (SC7).
    ///
    /// `extraction_cache` is keyed by content hash, so every edit to a file
    /// adds a row for the new content and leaves the old one behind forever —
    /// nothing ever deleted from this table. Measured: five edits to one file
    /// leave five rows, and on a 4,742-file repository the table reached
    /// 198 MiB of a 525 MiB database. An always-on watcher would grow it
    /// without bound.
    ///
    /// Eviction is by reachability, not recency. Recency is actively wrong
    /// here: a file untouched for months has an old `accessed_at` but its
    /// cached entry is precisely the one the next build needs, while the rows
    /// worth dropping are the superseded versions of files being edited right
    /// now. Keying on "is this content still referenced by a generation we
    /// kept" bounds the cache to the retained working set.
    ///
    /// A row is kept only when it is the *only* thing that can answer a lookup
    /// for its content: reachable from a retained generation, and not already
    /// answerable from that generation's own payload.
    ///
    /// S-4: the rule used to be stated as two clauses — drop what no generation
    /// references, and drop what a generation holds under the *same* full
    /// identity — and between them sat the rows an extraction-schema bump
    /// creates. A file cached under `(hash, python, g1, a1)` and re-extracted
    /// after a bump into `(hash, python, g2, a2)` kept its `(hash, python)`
    /// reachability, so clause one spared it, and its identity no longer
    /// matched, so clause two could not touch it — while the *servable* copy
    /// was evicted as a duplicate. Nothing could serve it and nothing could
    /// evict it, so every bump added a full extra copy of every payload to a
    /// table the paragraph above calls bounded.
    ///
    /// Stated as reachability instead: if a retained generation records a
    /// usable identity for this content, [`Self::try_get_cached_extraction`]
    /// answers from that generation, so no cache copy of it is reachable —
    /// whether its identity matches (the generation serves it) or not (nothing
    /// can). Only content whose generation rows carry NULL identity — written
    /// before schema v8, and deliberately never eligible for the fallback —
    /// still needs its cache row, and that row survives.
    ///
    /// Must run *after* `prune_generations_except_latest`, so `generation_files`
    /// already describes only retained generations.
    pub fn prune_extraction_cache(&self) -> Result<usize> {
        let mut conn = lock_conn(&self.conn)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = tx.execute(
            "DELETE FROM extraction_cache
             WHERE (content_hash, language) NOT IN
                   (SELECT content_hash, language FROM generation_files)
                OR EXISTS (SELECT 1 FROM generation_files g
                            WHERE g.content_hash = extraction_cache.content_hash
                              AND g.language     = extraction_cache.language
                              AND g.grammar_version  IS NOT NULL
                              AND g.analyzer_version IS NOT NULL)",
            [],
        )?;
        tx.commit()?;
        Ok(removed)
    }

    #[cfg(feature = "parse")]
    pub fn try_get_cached_extraction(
        &self,
        key: &devmap_extract::cache::CacheKey,
    ) -> Result<Option<devmap_extract::model::Extraction>> {
        self.try_get_cached_extraction_with(key, parse_stored_extraction)
    }

    /// Read one cached extraction, parsing with `parse`.
    ///
    /// The connection guard covers the SELECT only. `parse` runs after that
    /// guard is released, so two cached reads can decode at the same time.
    /// The identity gate is unchanged, and a payload that will not parse is
    /// still a store fault rather than a cache miss.
    #[cfg(feature = "parse")]
    pub(crate) fn try_get_cached_extraction_with<E>(
        &self,
        key: &devmap_extract::cache::CacheKey,
        parse: impl FnOnce(&str) -> std::result::Result<devmap_extract::model::Extraction, E>,
    ) -> Result<Option<devmap_extract::model::Extraction>>
    where
        E: std::fmt::Display,
    {
        // The guard ends with this block. `parse` must not run while it is
        // held: the incremental extraction pass reloads every cached file on
        // rayon, and decoding under the mutex makes that pass serial.
        let payload = {
            let conn = lock_conn(&self.conn)?;
            let payload: Option<String> = conn
                .query_row(
                    "SELECT payload_json FROM extraction_cache
                     WHERE content_hash = ?1 AND language = ?2
                       AND grammar_version = ?3 AND analyzer_version = ?4",
                    params![
                        key.content_hash as i64,
                        key.language,
                        key.grammar_version,
                        key.analyzer_version
                    ],
                    |row| row.get(0),
                )
                .optional()?;

            // Fall back to a retained generation's copy (SC8).
            //
            // `generation_files` holds a byte-identical payload for the same
            // content, so keeping both was storing every extraction twice — 198 MiB
            // of a 525 MiB database on one corpus. The fallback matches on the FULL
            // cache identity, including grammar and analyzer version, so it cannot
            // serve a payload produced by older extraction semantics; rows written
            // before schema v8 carry NULL there and are therefore never eligible.
            // Absence of a recorded identity is not proof of a matching one.
            match payload {
                Some(found) => Some(("extraction_cache", found)),
                None => conn
                    .query_row(
                        "SELECT extraction_json FROM generation_files
                         WHERE content_hash = ?1 AND language = ?2
                           AND grammar_version = ?3 AND analyzer_version = ?4
                         LIMIT 1",
                        params![
                            key.content_hash as i64,
                            key.language,
                            key.grammar_version,
                            key.analyzer_version
                        ],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .map(|json| ("generation_files", json)),
            }
        };
        // S-5: a stored payload that will not parse is a store fault, not a
        // cache miss. `.ok()` here re-extracted the file on every build for
        // ever and threw away the only evidence that a row was corrupt — the
        // one JSON read in this file that stayed quiet while every other names
        // what it could not read.
        payload
            .map(|(table, json)| {
                parse(&json).map_err(|error| {
                    refusal(format!(
                        "stored extraction payload in {table} for content \
                         {hash:#018x} ({language}, grammar {grammar}, analyzer \
                         {analyzer}) is invalid: {error}",
                        hash = key.content_hash,
                        language = key.language,
                        grammar = key.grammar_version,
                        analyzer = key.analyzer_version,
                    ))
                })
            })
            .transpose()
    }

    #[cfg(feature = "parse")]
    pub fn admit_cached_extraction(
        &self,
        key: &devmap_extract::cache::CacheKey,
        ext: &devmap_extract::model::Extraction,
    ) -> Result<()> {
        if !devmap_extract::cache::cache_admits(&ext.parse_outcome) {
            return self.record_extraction_retry(key, "ParseOutcome::Failed");
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let mut cached = ext.for_durable_store();
        // Source text is already identified by the content hash and remains on
        // disk; duplicating it in both cache and generation rows bloats the DB.
        cached.source_code = None;
        let payload = serde_json::to_string(&cached)
            .map_err(|err| refusal(format!("cache serialize failed: {err}")))?;
        let conn = lock_conn(&self.conn)?;
        conn.execute(
            "INSERT INTO extraction_cache (content_hash, language, grammar_version, analyzer_version, payload_json, accessed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(content_hash, language, grammar_version, analyzer_version)
             DO UPDATE SET payload_json = excluded.payload_json, accessed_at = excluded.accessed_at",
            params![
                key.content_hash as i64,
                key.language,
                key.grammar_version,
                key.analyzer_version,
                payload,
                now
            ],
        )?;
        Ok(())
    }

    #[cfg(feature = "parse")]
    pub fn record_extraction_retry(
        &self,
        key: &devmap_extract::cache::CacheKey,
        reason: &str,
    ) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let conn = lock_conn(&self.conn)?;
        conn.execute(
            "INSERT INTO extraction_retry (content_hash, language, attempts, last_reason, updated_at)
             VALUES (?1, ?2, 1, ?3, ?4)
             ON CONFLICT(content_hash) DO UPDATE SET
               attempts = attempts + 1,
               last_reason = excluded.last_reason,
               updated_at = excluded.updated_at",
            params![key.content_hash as i64, key.language, reason, now],
        )?;
        Ok(())
    }

    pub fn extraction_retry_count(&self, content_hash: u64) -> Result<u32> {
        let conn = lock_conn(&self.conn)?;
        conn.query_row(
            "SELECT attempts FROM extraction_retry WHERE content_hash = ?1",
            params![content_hash as i64],
            |row| row.get(0),
        )
        .optional()
        .map(|opt| opt.unwrap_or(0))
    }
}

/// `serde_json::from_str` as a function item is not higher-ranked, so the
/// production call cannot pass it directly. This wrapper is.
#[cfg(feature = "parse")]
fn parse_stored_extraction(json: &str) -> serde_json::Result<devmap_extract::model::Extraction> {
    serde_json::from_str(json)
}

#[cfg(all(test, feature = "parse"))]
mod cached_read_tests {
    use super::super::lock_conn;
    use super::*;
    use devmap_extract::cache::CacheKey;
    use devmap_extract::extract_file;
    use rusqlite::params;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::{Duration, Instant};

    fn admit(store: &Store, path: &str, source: &str) -> CacheKey {
        let extraction = extract_file(path, source);
        let key = CacheKey::for_extraction(&extraction);
        store
            .admit_cached_extraction(&key, &extraction)
            .expect("admit");
        key
    }

    #[test]
    fn two_cached_reads_parse_at_the_same_time() {
        let store = Arc::new(Store::open_in_memory().expect("store"));
        let keys = [
            admit(&store, "a.py", "def a():\n    return 1\n"),
            admit(&store, "b.py", "def b():\n    return 2\n"),
        ];
        let entered = Arc::new(AtomicUsize::new(0));
        let mut joins = Vec::new();
        for key in keys {
            let store = Arc::clone(&store);
            let entered = Arc::clone(&entered);
            joins.push(thread::spawn(move || {
                store.try_get_cached_extraction_with(&key, |json| {
                    // Probe only once every parser has entered. A sibling can
                    // still be inside its SELECT before that, and the probe
                    // itself must not be held or it blocks the others.
                    let ordinal = entered.fetch_add(1, Ordering::SeqCst) + 1;
                    let start = Instant::now();
                    while entered.load(Ordering::SeqCst) < 2
                        && start.elapsed() < Duration::from_secs(2)
                    {
                        thread::yield_now();
                    }
                    assert!(
                        entered.load(Ordering::SeqCst) >= 2,
                        "the other cached read never entered the parser"
                    );
                    if ordinal == 2 {
                        assert!(
                            store.conn.try_lock().is_ok(),
                            "connection guard is held across JSON parse"
                        );
                    }
                    serde_json::from_str(json)
                })
            }));
        }
        let mut paths = Vec::new();
        for join in joins {
            let parsed = join.join().expect("parser thread");
            let extraction = parsed.expect("cached read").expect("a stored row");
            paths.push(extraction.file_path);
        }
        paths.sort();
        assert_eq!(paths, ["a.py".to_string(), "b.py".to_string()]);
    }

    #[test]
    fn a_cache_miss_does_not_parse() {
        let store = Store::open_in_memory().expect("store");
        let key = CacheKey::for_source("python", "def untouched():\n    return 0\n");
        let parsed = store.try_get_cached_extraction_with(
            &key,
            |_json| -> std::result::Result<devmap_extract::model::Extraction, String> {
                panic!("a miss has no payload to parse");
            },
        );
        assert!(parsed.expect("miss is not a fault").is_none());
    }

    #[test]
    fn an_unparseable_cached_payload_is_a_refusal_naming_the_table() {
        let store = Store::open_in_memory().expect("store");
        let key = admit(&store, "c.py", "def c():\n    return 3\n");
        {
            let conn = lock_conn(&store.conn).expect("connection");
            conn.execute(
                "UPDATE extraction_cache SET payload_json = '{'
                  WHERE content_hash = ?1",
                params![key.content_hash as i64],
            )
            .expect("corrupt the stored payload");
        }
        let error = store
            .try_get_cached_extraction(&key)
            .expect_err("corrupt JSON is a store fault");
        let text = error.to_string();
        assert!(
            text.contains("stored extraction payload in extraction_cache"),
            "{text}"
        );
        assert!(text.contains("is invalid:"), "{text}");
        assert!(!text.contains("cache miss"), "{text}");
        assert!(
            text.contains("EOF while parsing"),
            "the serde error must stay in the refusal: {text}"
        );
        assert!(
            !text.contains("{"),
            "the refusal must name the fault, not quote the payload: {text}"
        );
    }

    #[test]
    fn a_different_analyzer_is_still_a_miss_and_is_not_parsed() {
        let store = Store::open_in_memory().expect("store");
        let key = admit(&store, "e.py", "def e():\n    return 5\n");
        let stale = CacheKey {
            analyzer_version: format!("{}-stale", key.analyzer_version),
            ..key
        };
        let parsed = store.try_get_cached_extraction_with(
            &stale,
            |_json| -> std::result::Result<devmap_extract::model::Extraction, String> {
                panic!("a stale analyzer must not parse");
            },
        );
        assert!(parsed.expect("stale identity is a miss").is_none());
    }

    #[test]
    fn the_parser_receives_the_stored_bytes() {
        let store = Store::open_in_memory().expect("store");
        let key = admit(&store, "f.py", "def f():\n    return 6\n");
        let stored: String = {
            let conn = lock_conn(&store.conn).expect("connection");
            conn.query_row(
                "SELECT payload_json FROM extraction_cache WHERE content_hash = ?1",
                params![key.content_hash as i64],
                |row| row.get(0),
            )
            .expect("stored payload")
        };
        let found = store
            .try_get_cached_extraction_with(&key, |json| {
                assert_eq!(json, stored, "parse must see the stored bytes");
                serde_json::from_str(json)
            })
            .expect("read")
            .expect("row");
        assert_eq!(found.file_path, "f.py");
    }

    #[test]
    fn an_unparseable_generation_payload_names_generation_files() {
        let store = Store::open_in_memory().expect("store");
        let source = "def helper():\n    return 1\n";
        let extraction = extract_file("src/helper.py", source);
        let key = CacheKey::for_source(&extraction.language, source);
        let mut resolver = devmap_resolve::Resolver::new();
        resolver.index_extractions(std::slice::from_ref(&extraction));
        let resolution = resolver
            .resolve_all(std::slice::from_ref(&extraction))
            .expect("resolve");
        let analysis = devmap_analyze::analyze(std::slice::from_ref(&extraction), &resolution);
        store
            .save_generation(std::slice::from_ref(&extraction), &resolution, &analysis)
            .expect("generation");
        {
            let conn = lock_conn(&store.conn).expect("connection");
            conn.execute("UPDATE file_payloads SET extraction_json = '{'", [])
                .expect("corrupt the generation payload");
        }
        let text = store
            .try_get_cached_extraction(&key)
            .expect_err("corrupt generation JSON is a store fault")
            .to_string();
        assert!(
            text.contains("stored extraction payload in generation_files"),
            "{text}"
        );
        assert!(text.contains("is invalid:"), "{text}");
        assert!(text.contains("EOF while parsing"), "{text}");
    }

    #[test]
    fn a_generation_fallback_parses_without_the_connection_guard() {
        let store = Store::open_in_memory().expect("store");
        let source = "def helper():\n    return 1\n";
        let extraction = extract_file("src/helper.py", source);
        let key = CacheKey::for_source(&extraction.language, source);
        let mut resolver = devmap_resolve::Resolver::new();
        resolver.index_extractions(std::slice::from_ref(&extraction));
        let resolution = resolver
            .resolve_all(std::slice::from_ref(&extraction))
            .expect("resolve");
        let analysis = devmap_analyze::analyze(std::slice::from_ref(&extraction), &resolution);
        store
            .save_generation(std::slice::from_ref(&extraction), &resolution, &analysis)
            .expect("generation");
        {
            let conn = lock_conn(&store.conn).expect("connection");
            let cached: i64 = conn
                .query_row("SELECT COUNT(*) FROM extraction_cache", [], |row| {
                    row.get(0)
                })
                .expect("count");
            assert_eq!(cached, 0, "the hit must come from generation_files");
        }
        let found = store
            .try_get_cached_extraction_with(&key, |json| {
                assert!(
                    store.conn.try_lock().is_ok(),
                    "generation fallback holds the connection guard across parse"
                );
                serde_json::from_str(json)
            })
            .expect("fallback read")
            .expect("generation row");
        assert_eq!(found.file_path, "src/helper.py");
    }

    #[test]
    fn a_parser_panic_does_not_poison_the_store() {
        let store = Store::open_in_memory().expect("store");
        let key = admit(&store, "d.py", "def d():\n    return 4\n");
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = store.try_get_cached_extraction_with(
                &key,
                |_json| -> std::result::Result<devmap_extract::model::Extraction, String> {
                    panic!("parser blew up");
                },
            );
        }));
        assert!(panicked.is_err(), "the parser panic must surface");
        let read = store
            .try_get_cached_extraction(&key)
            .expect("the store mutex must still lock after a parser panic");
        assert_eq!(read.expect("row").file_path, "d.py");
    }

    /// Same-thread re-entry deadlocks `std::sync::Mutex`. Bound the wait and
    /// exit the process on timeout: a blocked worker would otherwise keep the
    /// test binary alive after the assertion gave up.
    fn run_bounded<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
            let _ = tx.send(outcome);
        });
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(value)) => value,
            Ok(Err(payload)) => std::panic::resume_unwind(payload),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                eprintln!("cached-read stress deadlocked");
                std::process::exit(101);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("cached-read stress worker disappeared");
            }
        }
    }

    fn save_one(store: &Store, path: &str, source: &str) -> CacheKey {
        let extraction = extract_file(path, source);
        let key = CacheKey::for_source(&extraction.language, source);
        let mut resolver = devmap_resolve::Resolver::new();
        resolver.index_extractions(std::slice::from_ref(&extraction));
        let resolution = resolver
            .resolve_all(std::slice::from_ref(&extraction))
            .expect("resolve");
        let analysis = devmap_analyze::analyze(std::slice::from_ref(&extraction), &resolution);
        store
            .save_generation(std::slice::from_ref(&extraction), &resolution, &analysis)
            .expect("generation");
        key
    }

    #[test]
    fn eight_cached_reads_parse_at_the_same_time() {
        const N: usize = 8;
        let store = Arc::new(Store::open_in_memory().expect("store"));
        let keys: Vec<_> = (0..N)
            .map(|index| {
                admit(
                    &store,
                    &format!("n{index}.py"),
                    &format!("def n{index}():\n    return {index}\n"),
                )
            })
            .collect();
        let entered = Arc::new(AtomicUsize::new(0));
        let mut joins = Vec::new();
        for key in keys {
            let store = Arc::clone(&store);
            let entered = Arc::clone(&entered);
            joins.push(thread::spawn(move || {
                store.try_get_cached_extraction_with(&key, |json| {
                    let ordinal = entered.fetch_add(1, Ordering::SeqCst) + 1;
                    let start = Instant::now();
                    while entered.load(Ordering::SeqCst) < N
                        && start.elapsed() < Duration::from_secs(5)
                    {
                        thread::yield_now();
                    }
                    assert!(
                        entered.load(Ordering::SeqCst) >= N,
                        "a cached read never entered the parser"
                    );
                    if ordinal == N {
                        assert!(
                            store.conn.try_lock().is_ok(),
                            "connection guard is held across JSON parse"
                        );
                    }
                    serde_json::from_str(json)
                })
            }));
        }
        let mut paths = Vec::new();
        for join in joins {
            let extraction = join
                .join()
                .expect("parser thread")
                .expect("cached read")
                .expect("a stored row");
            paths.push(extraction.file_path);
        }
        paths.sort();
        let expected: Vec<_> = (0..N).map(|index| format!("n{index}.py")).collect();
        assert_eq!(paths, expected);
    }

    #[test]
    fn the_parser_reenters_admits_and_prunes_without_deadlock() {
        let store = Arc::new(Store::open_in_memory().expect("store"));
        let key = admit(&store, "outer.py", "def outer():\n    return 1\n");
        let store_for_read = Arc::clone(&store);
        let key_for_read = key.clone();
        let found = run_bounded(move || {
            store_for_read.try_get_cached_extraction_with(&key_for_read, |json| {
                let again = store_for_read
                    .try_get_cached_extraction(&key_for_read)
                    .expect("re-entering the same key must not deadlock");
                assert_eq!(again.expect("row").file_path, "outer.py");
                let extra = admit(
                    &store_for_read,
                    "during.py",
                    "def during():\n    return 2\n",
                );
                let admitted = store_for_read
                    .try_get_cached_extraction(&extra)
                    .expect("admit during parse");
                assert_eq!(admitted.expect("row").file_path, "during.py");
                store_for_read
                    .prune_extraction_cache()
                    .expect("prune during parse");
                serde_json::from_str::<devmap_extract::model::Extraction>(json)
            })
        });
        assert_eq!(found.expect("read").expect("row").file_path, "outer.py");
        let after = store
            .try_get_cached_extraction(&key)
            .expect("store still locks");
        assert!(
            after.is_none(),
            "prune committed while the snapshot was still parsing"
        );
    }

    #[test]
    fn a_writer_replaces_the_row_while_the_snapshot_parses() {
        let store = Arc::new(Store::open_in_memory().expect("store"));
        let key = admit(&store, "snap.py", "def snap():\n    return 1\n");
        let entered = Arc::new(AtomicBool::new(false));
        let replaced = Arc::new(AtomicBool::new(false));
        let store_for_read = Arc::clone(&store);
        let entered_for_read = Arc::clone(&entered);
        let replaced_for_read = Arc::clone(&replaced);
        let key_for_read = key.clone();
        let reader = thread::spawn(move || {
            store_for_read.try_get_cached_extraction_with(&key_for_read, |json| {
                entered_for_read.store(true, Ordering::SeqCst);
                let start = Instant::now();
                while !replaced_for_read.load(Ordering::SeqCst)
                    && start.elapsed() < Duration::from_secs(2)
                {
                    thread::yield_now();
                }
                assert!(
                    replaced_for_read.load(Ordering::SeqCst),
                    "writer could not commit while the snapshot was parsing"
                );
                assert_ne!(json, "{", "parse must see the bytes copied at SELECT");
                serde_json::from_str::<devmap_extract::model::Extraction>(json)
            })
        });
        let start = Instant::now();
        while !entered.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(2) {
            thread::yield_now();
        }
        assert!(entered.load(Ordering::SeqCst), "parser never started");
        {
            let conn = lock_conn(&store.conn).expect("writer lock");
            conn.execute(
                "UPDATE extraction_cache SET payload_json = '{' WHERE content_hash = ?1",
                params![key.content_hash as i64],
            )
            .expect("replace the stored payload");
        }
        replaced.store(true, Ordering::SeqCst);
        let parsed = reader.join().expect("reader").expect("snapshot read");
        assert_eq!(parsed.expect("row").file_path, "snap.py");
        let text = store
            .try_get_cached_extraction(&key)
            .expect_err("the replaced row is a fault")
            .to_string();
        assert!(
            text.contains("stored extraction payload in extraction_cache"),
            "{text}"
        );
    }

    #[test]
    fn readers_never_observe_a_torn_payload() {
        let store = Arc::new(Store::open_in_memory().expect("store"));
        let left = admit(&store, "left.py", "def left():\n    return 1\n");
        let right = admit(&store, "right.py", "def right():\n    return 2\n");
        let payload = |key: &CacheKey| -> String {
            let conn = lock_conn(&store.conn).expect("connection");
            conn.query_row(
                "SELECT payload_json FROM extraction_cache WHERE content_hash = ?1",
                params![key.content_hash as i64],
                |row| row.get(0),
            )
            .expect("payload")
        };
        let left_json = payload(&left);
        let right_json = payload(&right);
        let stop = Arc::new(AtomicBool::new(false));
        let faults = Arc::new(AtomicUsize::new(0));
        let mut readers = Vec::new();
        for _ in 0..8 {
            let store = Arc::clone(&store);
            let key = left.clone();
            let stop = Arc::clone(&stop);
            let faults = Arc::clone(&faults);
            readers.push(thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match store.try_get_cached_extraction(&key) {
                        Ok(Some(ext))
                            if ext.file_path == "left.py" || ext.file_path == "right.py" => {}
                        _ => {
                            faults.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
            }));
        }
        let start = Instant::now();
        let mut flip = false;
        while start.elapsed() < Duration::from_millis(250) {
            flip = !flip;
            let json = if flip { &left_json } else { &right_json };
            let conn = lock_conn(&store.conn).expect("writer");
            conn.execute(
                "UPDATE extraction_cache SET payload_json = ?1 WHERE content_hash = ?2",
                params![json, left.content_hash as i64],
            )
            .expect("swap payload");
        }
        stop.store(true, Ordering::SeqCst);
        for reader in readers {
            reader.join().expect("reader");
        }
        assert_eq!(
            faults.load(Ordering::SeqCst),
            0,
            "a reader saw a torn payload"
        );
    }

    #[test]
    fn hostile_cached_payloads_are_refusals_that_name_the_table() {
        let store = Store::open_in_memory().expect("store");
        let key = admit(&store, "bad.py", "def bad():\n    return 1\n");
        let mut deep = "[".repeat(200);
        deep.push_str(&"]".repeat(200));
        let mut huge = String::from("{");
        huge.extend(std::iter::repeat(' ').take(256 * 1024));
        let samples = [
            "",
            "{",
            "null",
            "[]",
            "0",
            "\"nope\"",
            "   ",
            "{}",
            "{\"file_path\":",
            deep.as_str(),
            huge.as_str(),
        ];
        for sample in samples {
            {
                let conn = lock_conn(&store.conn).expect("connection");
                conn.execute(
                    "UPDATE extraction_cache SET payload_json = ?1 WHERE content_hash = ?2",
                    params![sample, key.content_hash as i64],
                )
                .expect("plant a hostile payload");
            }
            let text = store
                .try_get_cached_extraction(&key)
                .expect_err("hostile JSON is a store fault")
                .to_string();
            assert!(
                text.contains("stored extraction payload in extraction_cache"),
                "{sample:?} -> {text}"
            );
            assert!(text.contains("is invalid:"), "{sample:?} -> {text}");
            assert!(!text.contains("cache miss"), "{sample:?} -> {text}");
            let detail = text.split("is invalid:").nth(1).unwrap_or("");
            assert!(
                !detail.trim().is_empty(),
                "{sample:?} dropped the parser error"
            );
        }
    }

    #[test]
    fn a_corrupt_cache_row_does_not_fall_through_to_a_valid_generation() {
        let store = Store::open_in_memory().expect("store");
        let source = "def both():\n    return 1\n";
        let key = save_one(&store, "both.py", source);
        let admitted = admit(&store, "both.py", source);
        assert_eq!(
            admitted.content_hash, key.content_hash,
            "the cache row and the generation row must share an identity"
        );
        {
            let conn = lock_conn(&store.conn).expect("connection");
            let generations: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM generation_files
                     WHERE content_hash = ?1 AND language = ?2
                       AND grammar_version = ?3 AND analyzer_version = ?4",
                    params![
                        key.content_hash as i64,
                        key.language,
                        key.grammar_version,
                        key.analyzer_version
                    ],
                    |row| row.get(0),
                )
                .expect("generation count");
            assert!(generations >= 1, "the fall-through row has to exist");
            conn.execute(
                "UPDATE extraction_cache SET payload_json = '{' WHERE content_hash = ?1",
                params![key.content_hash as i64],
            )
            .expect("corrupt only the cache row");
        }
        let text = store
            .try_get_cached_extraction(&key)
            .expect_err("the cache row wins, and it is a fault")
            .to_string();
        assert!(
            text.contains("stored extraction payload in extraction_cache"),
            "{text}"
        );
        assert!(!text.contains("generation_files"), "{text}");
    }

    #[test]
    fn a_null_generation_identity_is_a_miss_and_is_not_parsed() {
        let store = Store::open_in_memory().expect("store");
        let key = save_one(&store, "old.py", "def old():\n    return 1\n");
        {
            let conn = lock_conn(&store.conn).expect("connection");
            conn.execute(
                "UPDATE file_payloads SET grammar_version = NULL, analyzer_version = NULL",
                [],
            )
            .expect("clear the stored identity");
        }
        let parsed = store.try_get_cached_extraction_with(
            &key,
            |_json| -> std::result::Result<devmap_extract::model::Extraction, String> {
                panic!("a NULL generation identity must not parse");
            },
        );
        assert!(parsed.expect("NULL identity is a miss").is_none());
    }

    #[test]
    fn a_high_bit_content_hash_round_trips_and_does_not_alias() {
        let store = Store::open_in_memory().expect("store");
        let low = admit(&store, "low.py", "def low():\n    return 1\n");
        let high_payload = {
            let donor = admit(&store, "high.py", "def high():\n    return 2\n");
            let conn = lock_conn(&store.conn).expect("connection");
            let payload: String = conn
                .query_row(
                    "SELECT payload_json FROM extraction_cache WHERE content_hash = ?1",
                    params![donor.content_hash as i64],
                    |row| row.get(0),
                )
                .expect("donor payload");
            payload
        };
        for hash in [1u64 << 63, u64::MAX] {
            let key = CacheKey {
                content_hash: hash,
                ..low.clone()
            };
            {
                let conn = lock_conn(&store.conn).expect("connection");
                conn.execute(
                    "INSERT INTO extraction_cache
                        (content_hash, language, grammar_version, analyzer_version, payload_json, accessed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                    params![
                        key.content_hash as i64,
                        key.language,
                        key.grammar_version,
                        key.analyzer_version,
                        high_payload
                    ],
                )
                .expect("insert a high-bit identity");
            }
            let found = store
                .try_get_cached_extraction(&key)
                .expect("high-bit read")
                .expect("high-bit row");
            assert_eq!(found.file_path, "high.py");
        }
        let still_low = store
            .try_get_cached_extraction(&low)
            .expect("low read")
            .expect("low row");
        assert_eq!(still_low.file_path, "low.py");
    }

    #[test]
    fn a_language_with_sql_metacharacters_matches_only_itself() {
        let store = Store::open_in_memory().expect("store");
        let key = admit(&store, "q.py", "def q():\n    return 1\n");
        let hostile = "python' OR language <> 'python";
        {
            let conn = lock_conn(&store.conn).expect("connection");
            conn.execute(
                "UPDATE extraction_cache SET language = ?1 WHERE content_hash = ?2",
                params![hostile, key.content_hash as i64],
            )
            .expect("rename the language");
        }
        let exact = CacheKey {
            language: hostile.to_string(),
            ..key.clone()
        };
        let found = store
            .try_get_cached_extraction(&exact)
            .expect("exact language")
            .expect("row");
        assert_eq!(found.file_path, "q.py");
        let python = CacheKey {
            language: "python".to_string(),
            ..key
        };
        let parsed = store.try_get_cached_extraction_with(
            &python,
            |_json| -> std::result::Result<devmap_extract::model::Extraction, String> {
                panic!("a different language must not parse");
            },
        );
        assert!(parsed.expect("other language is a miss").is_none());
    }

    #[test]
    fn a_poisoned_store_mutex_is_still_a_refusal() {
        let store = Arc::new(Store::open_in_memory().expect("store"));
        let key = admit(&store, "poison.py", "def poison():\n    return 1\n");
        let doomed = Arc::clone(&store);
        let joined = thread::spawn(move || {
            let _guard = lock_conn(&doomed.conn).expect("lock");
            panic!("poison the store");
        })
        .join();
        assert!(joined.is_err(), "the poisoning thread must panic");
        for _ in 0..2 {
            let text = store
                .try_get_cached_extraction(&key)
                .expect_err("poison refuses")
                .to_string();
            assert!(
                text.contains("store mutex poisoned — refusing to continue (fail-closed)"),
                "{text}"
            );
        }
    }

    #[test]
    fn a_cache_hit_and_a_generation_fallback_parse_together() {
        let store = Arc::new(Store::open_in_memory().expect("store"));
        let cached = admit(&store, "hit.py", "def hit():\n    return 1\n");
        let generated = save_one(&store, "miss.py", "def miss():\n    return 2\n");
        let entered = Arc::new(AtomicUsize::new(0));
        let mut joins = Vec::new();
        for key in [cached, generated] {
            let store = Arc::clone(&store);
            let entered = Arc::clone(&entered);
            joins.push(thread::spawn(move || {
                store.try_get_cached_extraction_with(&key, |json| {
                    let ordinal = entered.fetch_add(1, Ordering::SeqCst) + 1;
                    let start = Instant::now();
                    while entered.load(Ordering::SeqCst) < 2
                        && start.elapsed() < Duration::from_secs(2)
                    {
                        thread::yield_now();
                    }
                    assert!(entered.load(Ordering::SeqCst) >= 2);
                    if ordinal == 2 {
                        assert!(
                            store.conn.try_lock().is_ok(),
                            "connection guard is held across JSON parse"
                        );
                    }
                    serde_json::from_str(json)
                })
            }));
        }
        let mut paths = Vec::new();
        for join in joins {
            paths.push(
                join.join()
                    .expect("parser thread")
                    .expect("read")
                    .expect("row")
                    .file_path,
            );
        }
        paths.sort();
        assert_eq!(paths, ["hit.py".to_string(), "miss.py".to_string()]);
    }

    #[test]
    fn moving_a_large_payload_out_is_the_select_allocation() {
        let mut source = String::new();
        for index in 0..400 {
            source.push_str(&format!("def f{index}(x):\n    return x + {index}\n"));
        }
        let store = Store::open_in_memory().expect("store");
        let key = admit(&store, "big.py", &source);
        let mut copy_ns = Vec::new();
        let mut parse_ns = Vec::new();
        let mut payload_bytes = 0usize;
        for _ in 0..5 {
            let started = Instant::now();
            let copied: String = {
                let conn = lock_conn(&store.conn).expect("connection");
                conn.query_row(
                    "SELECT payload_json FROM extraction_cache
                     WHERE content_hash = ?1 AND language = ?2
                       AND grammar_version = ?3 AND analyzer_version = ?4",
                    params![
                        key.content_hash as i64,
                        key.language,
                        key.grammar_version,
                        key.analyzer_version
                    ],
                    |row| row.get(0),
                )
                .expect("payload")
            };
            copy_ns.push(started.elapsed().as_nanos());
            payload_bytes = copied.len();
            let started = Instant::now();
            let parsed: devmap_extract::model::Extraction =
                serde_json::from_str(&copied).expect("parse");
            parse_ns.push(started.elapsed().as_nanos());
            assert_eq!(parsed.file_path, "big.py");
            assert!(copied.len() > 10_000, "payload was {} bytes", copied.len());
        }
        copy_ns.sort_unstable();
        parse_ns.sort_unstable();
        eprintln!(
            "large-payload bytes={} copy_ns_min={} parse_ns_min={}",
            payload_bytes, copy_ns[0], parse_ns[0]
        );
        let via = store
            .try_get_cached_extraction(&key)
            .expect("cached read")
            .expect("row");
        assert_eq!(via.file_path, "big.py");
    }
}
