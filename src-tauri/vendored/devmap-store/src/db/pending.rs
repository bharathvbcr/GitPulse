use super::{
    canonical_pending_entry, classify_pending_entry, lock_conn, refusal, sqlite_limit,
    PendingClaim, PendingEnqueueReport, PendingEntry, PendingReconcile, PendingSupersede,
    PendingWatermark, Store, MAX_PENDING_ATTEMPTS,
};
use rusqlite::OptionalExtension;
use rusqlite::{params, Connection, Result, TransactionBehavior};
use std::collections::BTreeSet;
use std::path::Path;

impl Store {
    pub fn get_or_create_path_id(&self, path: &str) -> Result<u32> {
        let mut conn = lock_conn(&self.conn)?;
        if let Some(id) = conn
            .query_row("SELECT id FROM paths WHERE path = ?1", [path], |row| {
                row.get(0)
            })
            .optional()?
        {
            return Ok(id);
        }
        self.refuse_if_read_only()?;
        // Keep conversion inside the transaction: an allocated SQLite ID can
        // exceed our u32 contract, and a refused intern must leave no row.
        // Acquire the writer before the second lookup so concurrent interns
        // cannot invalidate a deferred read snapshot before insertion.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = Self::ensure_path_id(&tx, path)?;
        tx.commit()?;
        Ok(id)
    }

    #[cfg(feature = "parse")]
    /// `ensure_path_id`, memoised for the life of one generation write.
    ///
    /// Path ids are stable within a transaction — `paths` is insert-only here —
    /// so the second lookup of a path can only return what the first did. The
    /// repetition is severe rather than incidental: every edge names a source
    /// and a target file, and a 73,000-edge generation over 1,280 files asks
    /// for ~146,000 ids drawn from 1,280 distinct values. The cache turns that
    /// into 1,280 queries.
    ///
    /// Deliberately scoped to a single call rather than held on `Store`: a
    /// cache outliving its transaction would hand out ids from a write that
    /// rolled back.
    pub(super) fn ensure_path_id_cached(
        tx: &rusqlite::Transaction<'_>,
        cache: &mut std::collections::HashMap<String, u32>,
        path: &str,
    ) -> Result<u32> {
        if let Some(id) = cache.get(path) {
            return Ok(*id);
        }
        let id = Self::ensure_path_id(tx, path)?;
        cache.insert(path.to_string(), id);
        Ok(id)
    }

    fn ensure_path_id(tx: &rusqlite::Transaction<'_>, path: &str) -> Result<u32> {
        // `prepare_cached`, not `query_row`/`execute`: those compile the SQL
        // afresh on every call, and this is the most-called statement in the
        // writer — twice per edge, so ~146,000 compilations of two 40-character
        // queries in a single DevCouncil generation.
        let mut select = tx.prepare_cached("SELECT id FROM paths WHERE path = ?1")?;
        if let Some(id) = select
            .query_row(params![path], |row| row.get(0))
            .optional()?
        {
            return Ok(id);
        }
        drop(select);
        tx.prepare_cached("INSERT OR IGNORE INTO paths (path) VALUES (?1)")?
            .execute(params![path])?;
        tx.prepare_cached("SELECT id FROM paths WHERE path = ?1")?
            .query_row(params![path], |row| row.get(0))
    }

    /// Enqueue verbatim. Callers that know the repository root must use
    /// [`Store::enqueue_pending_paths_under_root`] instead.
    ///
    /// Kept as the raw primitive because the queue is also written by tests and
    /// by callers replaying rows that are already canonical. It performs no
    /// normalisation and no containment check, which is exactly what made the
    /// queue rot: see K1 on `enqueue_pending_paths_under_root`.
    pub fn enqueue_pending_paths(&self, paths: &[String]) -> Result<()> {
        let now = Self::now_secs();
        self.with_pending_transaction(Self::PENDING_ADMISSION_TIMEOUT, |tx| {
            Self::upsert_pending(tx, paths.iter().map(String::as_str), now)
        })
    }

    /// Acquire the writer before reading queue identity, and restore the
    /// ordinary busy policy on success and failure. One immediate transaction
    /// gives the whole batch one admission wait rather than a timeout per path.
    pub(super) fn with_pending_transaction<T>(
        &self,
        wait: std::time::Duration,
        write: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        self.refuse_if_read_only()?;
        let mut conn = lock_conn(&self.conn)?;
        conn.busy_timeout(wait)?;
        let result = (|| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let value = write(&tx)?;
            tx.commit()?;
            Ok(value)
        })();
        conn.busy_timeout(Self::BUSY_TIMEOUT)?;
        result
    }

    fn now_secs() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    }

    pub(super) fn upsert_pending<'a>(
        tx: &rusqlite::Transaction<'_>,
        paths: impl ExactSizeIterator<Item = &'a str>,
        now: f64,
    ) -> Result<()> {
        if paths.len() == 0 {
            return Ok(());
        }
        // Allocate once per batch while holding the same transaction as its
        // inserts. Every edit still gets a distinct revision, including repeated
        // paths. A refused insert rolls back the reservation and every row.
        let mut revision = Self::reserve_pending_revisions(tx, paths.len())?;
        let mut insert = tx.prepare_cached(
            "INSERT INTO pending_paths (path, queued_at, attempts, revision) VALUES (?1, ?2, 0, ?3)
             ON CONFLICT(path) DO UPDATE SET
               queued_at=excluded.queued_at,
               revision=excluded.revision,
               attempts=0",
        )?;
        for path in paths {
            revision += 1; // the reservation proved the entire range fits i64
            insert.execute(params![path, now, revision])?;
        }
        Ok(())
    }

    /// Reserve `count` revisions and return the position before the range.
    /// One cached UPDATE avoids preparing two statements for every path while
    /// hundreds of sessions contend for SQLite's single writer.
    fn reserve_pending_revisions(conn: &Connection, count: usize) -> Result<i64> {
        let count =
            i64::try_from(count).map_err(|_| refusal("pending revision batch is too large"))?;
        if count <= 0 {
            return Err(refusal("pending revision batch must be nonempty"));
        }
        conn.prepare_cached(
            "UPDATE pending_state SET revision = revision + ?1
             WHERE singleton = 1 AND revision <= 9223372036854775807 - ?1
             RETURNING revision - ?1",
        )?
        .query_row([count], |row| row.get(0))
        .optional()?
        .ok_or_else(|| refusal("pending queue revision exhausted or its state is missing"))
    }

    fn pending_watermark_in(conn: &Connection) -> Result<PendingWatermark> {
        conn.query_row(
            "SELECT epoch, revision FROM pending_state WHERE singleton = 1",
            [],
            |r| {
                Ok(PendingWatermark {
                    epoch: r.get(0)?,
                    revision: r.get(1)?,
                })
            },
        )
    }

    pub fn pending_watermark(&self) -> Result<PendingWatermark> {
        let conn = lock_conn(&self.conn)?;
        Self::pending_watermark_in(&conn)
    }

    fn check_pending_epoch(conn: &Connection, watermark: &PendingWatermark) -> Result<()> {
        if Self::pending_watermark_in(conn)?.epoch != watermark.epoch {
            return Err(refusal(
                "pending acknowledgement belongs to a different store",
            ));
        }
        Ok(())
    }

    /// Check a reader's requested worktree without changing the store.
    pub fn validate_repo_root(&self, root: &Path) -> Result<()> {
        let root = Self::normalized_repo_root(root)?;
        let conn = lock_conn(&self.conn)?;
        Self::check_repo_root_in(&conn, &root)
    }

    pub(super) fn normalized_repo_root(root: &Path) -> Result<String> {
        let absolute = root
            .canonicalize()
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    std::path::absolute(root)
                } else {
                    Err(error)
                }
            })
            .map_err(|error| refusal(format!("cannot resolve worktree root: {error}")))?;
        absolute
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| refusal("worktree root is not valid UTF-8"))
    }

    fn check_repo_root_in(conn: &Connection, root: &str) -> Result<()> {
        let owner: Option<String> = conn.query_row(
            "SELECT repo_root FROM pending_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        if let Some(owner) = owner {
            let owner = Self::normalized_repo_root(Path::new(&owner))?;
            if owner != root {
                return Err(refusal(format!(
                    "DevMap store belongs to worktree {owner:?}, not {root:?}; use a separate --db or DEVMAP_HOME for each worktree")));
            }
        }
        Ok(())
    }

    pub(super) fn bind_repo_root_in(conn: &Connection, root: &str) -> Result<()> {
        Self::check_repo_root_in(conn, root)?;
        conn.execute(
            "UPDATE pending_state SET repo_root = ?1 WHERE singleton = 1",
            [root],
        )?;
        Ok(())
    }

    /// Bind writes to one canonical worktree, including before its first build.
    pub fn bind_repo_root(&self, root: &Path) -> Result<()> {
        self.refuse_if_read_only()?;
        let root = Self::normalized_repo_root(root)?;
        let mut conn = lock_conn(&self.conn)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::bind_repo_root_in(&tx, &root)?;
        tx.commit()
    }

    /// Enqueue changed paths in the queue's canonical form: repo-relative,
    /// forward-slash, deduplicated, and inside `root`.
    ///
    /// K1(a): the queue had two producers writing two different things. The
    /// watcher enqueued **absolute** paths; the connect-time reconcile enqueued
    /// **repo-relative** ones; `enqueue_pending_paths` inserted whichever it was
    /// given, verbatim, with no containment check. Nothing ever reconciled the
    /// two, so a repository that moved on disk left rows naming a directory
    /// that no longer existed — measured on this store as 64 permanently
    /// quarantined rows under `/Users/…/Code/DevCouncil` after the checkout
    /// moved to `/Users/…/Code/devtools/DevCouncil`, which pinned
    /// `devmap status` at `is_fresh=false` forever.
    ///
    /// One canonical form, enforced where rows enter. A path outside `root` is
    /// refused *here*, where the caller can be told, rather than accepted and
    /// then failed forever by a drain that has no way to delete it.
    pub fn enqueue_pending_paths_under_root(
        &self,
        root: &Path,
        paths: &[String],
    ) -> Result<PendingEnqueueReport> {
        let mut report = PendingEnqueueReport::default();
        let mut canonical: BTreeSet<String> = BTreeSet::new();
        let mut caches = devmap_extract::CacheDirectoryCache::default();
        for raw in paths {
            match canonical_pending_entry(root, raw) {
                PendingEntry::Canonical(entry) => {
                    // K7: refuse build caches at the door. The watcher fires on
                    // every write cargo makes into its output directory, and
                    // those events reached this queue as work — 47,000 rows
                    // from `target-serve` and `target-store` on this
                    // repository. Discovery skips the directory, so every one
                    // of those rows was guaranteed to be dropped later or to
                    // index something that is not source.
                    match caches.tagged_ancestor(root, &entry) {
                        devmap_extract::CacheVerdict::Inside(cache) => {
                            report.refused.push((
                                raw.clone(),
                                format!("inside {cache}, a build cache marked with CACHEDIR.TAG"),
                            ));
                            continue;
                        }
                        devmap_extract::CacheVerdict::NotRepoRelative(why) => {
                            report.refused.push((
                                raw.clone(),
                                format!("{why}, so it names nothing inside the repository"),
                            ));
                            continue;
                        }
                        devmap_extract::CacheVerdict::Unreadable { directory, reason } => {
                            report.refused.push((
                                raw.clone(),
                                format!("cannot examine {directory}/CACHEDIR.TAG: {reason}"),
                            ));
                            continue;
                        }
                        devmap_extract::CacheVerdict::Outside => {}
                    }
                    canonical.insert(entry);
                }
                // Not a path, so neither the containment test above nor the
                // build-cache test applies. The daemon's git-HEAD sentinel is
                // the only producer today, and dropping it is how a commit,
                // branch switch or rebase became invisible to the index.
                PendingEntry::ControlToken(token) => {
                    canonical.insert(token);
                }
                PendingEntry::Outside => report.refused.push((
                    raw.clone(),
                    format!("outside the repository root {}", root.display()),
                )),
                // Refused either way — an entry with no canonical spelling
                // cannot be queued — but the reason is the one the reader can
                // act on. "Outside the repository root" sends them after the
                // watcher; the truth is that the root could not be read.
                PendingEntry::Undecidable(error) => report.refused.push((
                    raw.clone(),
                    format!(
                        "could not be checked against the repository root {}: {error}",
                        root.display()
                    ),
                )),
            }
        }
        if canonical.is_empty() {
            return Ok(report);
        }
        let owner = Self::normalized_repo_root(root)?;
        let now = Self::now_secs();
        self.with_pending_transaction(Self::PENDING_ADMISSION_TIMEOUT, |tx| {
            Self::bind_repo_root_in(tx, &owner)?;
            Self::upsert_pending(tx, canonical.iter().map(String::as_str), now)
        })?;
        report.enqueued = canonical.into_iter().collect();
        Ok(report)
    }

    /// Failed drain attempts recorded against `path`, or `None` when it is not
    /// queued. Diagnostic and test-facing: "the queue is stuck" and "the queue
    /// is retrying" look identical from a row count.
    pub fn pending_attempts(&self, path: &str) -> Result<Option<u32>> {
        let conn = lock_conn(&self.conn)?;
        conn.query_row(
            "SELECT attempts FROM pending_paths WHERE path = ?1",
            params![path],
            |row| row.get(0),
        )
        .optional()
    }

    /// Drop pending rows that no amount of retrying can ever process (K1(b)).
    ///
    /// The queue's only deleters were an acknowledgement of *successful* work
    /// and a test-only clear, so a row that could not succeed was retried five
    /// times, quarantined, and then kept forever. The 64 rows measured on this
    /// store were: paths under a previous location of the repository,
    /// directories, `.md`/`.json` files, and a 30 MB vendored `parser.c` that
    /// is over `MAX_SOURCE_BYTES` and therefore could never be extracted by
    /// any number of attempts. None of them was a transient failure; all of
    /// them were structural, and structural failures are deleted, not retried.
    ///
    /// Deletion is **not** applied to a path that is merely absent. A file that
    /// vanished but is still a node in the latest generation is a deletion the
    /// drain has to process, and dropping it would leave the graph asserting a
    /// file that is gone. Only an absent path with nothing indexed under it is
    /// dropped.
    ///
    /// Non-canonical rows are rewritten rather than deleted where they still
    /// name something inside the root, so a queue written by the old absolute
    /// path producer converges instead of being thrown away.
    pub fn reconcile_pending_paths(&self, root: &Path) -> Result<PendingReconcile> {
        self.reconcile_pending_paths_with(root, &|| {})
    }

    pub(super) fn reconcile_pending_paths_with(
        &self,
        root: &Path,
        before_apply: &dyn Fn(),
    ) -> Result<PendingReconcile> {
        self.bind_repo_root(root)?;
        let indexed: BTreeSet<String> = self.latest_file_hashes()?.into_keys().collect();
        let rows: Vec<(String, f64, u32, i64)> = {
            let conn = lock_conn(&self.conn)?;
            let mut stmt = conn.prepare(
                "SELECT path, queued_at, attempts, revision FROM pending_paths ORDER BY path",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .collect::<Result<Vec<_>>>()?;
            rows
        };

        let mut outcome = PendingReconcile::default();
        // One memo for the whole sweep: 51,136 rows were measured on the live
        // store, and without it each would re-`open` every ancestor's tag.
        let mut caches = devmap_extract::CacheDirectoryCache::default();
        let mut deletes: Vec<(String, i64)> = Vec::new();
        let mut rewrites: Vec<(String, String, f64, u32, i64)> = Vec::new();
        for (stored, queued_at, attempts, revision) in rows {
            let canonical = match canonical_pending_entry(root, &stored) {
                PendingEntry::Canonical(entry) => entry,
                // Never path-normalised and never structurally reconciled: it
                // is retired by a build, not by this sweep. This used to be a
                // separate `is_control_token` pre-check here, which left the
                // rule spelled in two places and the other producer with no
                // spelling of it at all.
                PendingEntry::ControlToken(_) => {
                    outcome.retained += 1;
                    continue;
                }
                PendingEntry::Outside => {
                    deletes.push((stored.clone(), revision));
                    outcome.dropped.push((
                        stored,
                        format!("escapes the repository root {}", root.display()),
                    ));
                    continue;
                }
                // The containment test could not run, so this row has not been
                // shown to escape anything. Keeping it costs one non-canonical
                // row until the root is readable again; deleting it on this
                // evidence costs the file.
                PendingEntry::Undecidable(_) => {
                    outcome.retained += 1;
                    continue;
                }
            };
            match classify_pending_entry(root, &canonical, &indexed, &mut caches) {
                Err(reason) => {
                    deletes.push((stored.clone(), revision));
                    outcome.dropped.push((stored, reason));
                }
                Ok(()) => {
                    if canonical != stored {
                        rewrites.push((stored, canonical, queued_at, attempts, revision));
                    }
                    outcome.retained += 1;
                }
            }
        }

        before_apply();
        if !deletes.is_empty() || !rewrites.is_empty() {
            let mut conn = lock_conn(&self.conn)?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            for (path, revision) in &deletes {
                if tx
                    .prepare_cached("DELETE FROM pending_paths WHERE path = ?1 AND revision = ?2")?
                    .execute(params![path, revision])?
                    == 0
                {
                    outcome.dropped.retain(|(dropped, _)| dropped != path);
                    outcome.retained += 1;
                }
            }
            for (stored, canonical, queued_at, attempts, observed_revision) in &rewrites {
                if tx
                    .prepare_cached("DELETE FROM pending_paths WHERE path = ?1 AND revision = ?2")?
                    .execute(params![stored, observed_revision])?
                    == 0
                {
                    continue;
                }
                // A merge is a new event. Claims taken before either spelling
                // was repaired cannot acknowledge the merged work.
                let revision = Self::reserve_pending_revisions(&tx, 1)? + 1;
                tx.prepare_cached(
                    "INSERT INTO pending_paths (path, queued_at, attempts, revision) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(path) DO UPDATE SET
                       queued_at=MAX(pending_paths.queued_at, excluded.queued_at),
                       revision=excluded.revision,
                       attempts=MIN(pending_paths.attempts, excluded.attempts)",
                )?
                .execute(params![canonical, queued_at, attempts, revision])?;
                outcome.rewritten.push((stored.clone(), canonical.clone()));
            }
            tx.commit()?;
        }
        Ok(outcome)
    }

    /// Drop every quarantined row, returning what was dropped (K1(f)).
    pub fn drop_quarantined_pending_paths(&self) -> Result<Vec<String>> {
        let mut conn = lock_conn(&self.conn)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let dropped: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT path FROM pending_paths WHERE attempts >= ?1 ORDER BY queued_at, path",
            )?;
            let rows = stmt
                .query_map(params![MAX_PENDING_ATTEMPTS], |row| row.get(0))?
                .collect::<Result<Vec<_>>>()?;
            rows
        };
        tx.execute(
            "DELETE FROM pending_paths WHERE attempts >= ?1",
            params![MAX_PENDING_ATTEMPTS],
        )?;
        tx.commit()?;
        Ok(dropped)
    }

    /// Retire the pending work a committed build has superseded (K1(e)).
    ///
    /// A build that persisted a generation has answered some set of queued
    /// requests. *Which* set is the question `PendingSupersede` answers, and
    /// getting it wrong is how 918 rows survived a full `dev map` on the live
    /// store: the rule used to be "delete rows whose path is in the extraction
    /// set", and a directory is never an extraction. Every one of those 918
    /// rows named a directory, all of them still existed, so the structural
    /// reconcile correctly kept them and `repair --pending` could not touch
    /// them either — `status` simply reported NOT FRESH forever.
    ///
    /// Quarantined rows and control tokens obey the same revision and coverage
    /// boundary as every other event; unread or newer work must survive.
    pub fn clear_pending_superseded(&self, rule: PendingSupersede<'_>) -> Result<Vec<String>> {
        self.refuse_if_read_only()?;
        let (indexed, through) = match rule {
            PendingSupersede::IndexedPathsThrough(paths, through) => (
                Some(paths.iter().map(String::as_str).collect::<BTreeSet<_>>()),
                through,
            ),
            PendingSupersede::WholeTreeThrough(through) => (None, through),
        };
        let mut conn = lock_conn(&self.conn)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::check_pending_epoch(&tx, through)?;
        let rows: Vec<String> = {
            let mut stmt =
                tx.prepare("SELECT path FROM pending_paths WHERE revision <= ?1 ORDER BY path")?;
            let rows = stmt
                .query_map([through.revision], |row| row.get(0))?
                .collect::<Result<_>>()?;
            rows
        };
        let mut cleared = Vec::new();
        for path in rows {
            if indexed
                .as_ref()
                .is_none_or(|paths| paths.contains(path.as_str()))
            {
                tx.prepare_cached("DELETE FROM pending_paths WHERE path = ?1 AND revision <= ?2")?
                    .execute(params![path, through.revision])?;
                cleared.push(path);
            }
        }
        tx.commit()?;
        Ok(cleared)
    }

    /// Every queued path a drain may still retry, oldest first.
    pub fn get_pending_paths(&self) -> Result<Vec<String>> {
        self.get_pending_paths_limited(usize::MAX)
    }

    /// Return the oldest pending paths, bounded in SQL so a large queue cannot
    /// defeat the daemon's batch limit before application-level truncation.
    pub fn get_pending_paths_limited(&self, limit: usize) -> Result<Vec<String>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = lock_conn(&self.conn)?;
        let mut stmt = conn.prepare(
            "SELECT path FROM pending_paths
             WHERE attempts < ?2
             ORDER BY revision ASC, path ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![sqlite_limit(limit), MAX_PENDING_ATTEMPTS], |row| {
            row.get(0)
        })?;
        let mut paths = Vec::new();
        for r in rows {
            paths.push(r?);
        }
        Ok(paths)
    }

    /// Claim up to `limit` retryable pending rows for one drain attempt.
    ///
    /// Same selection as [`Store::get_pending_paths_limited`], but each row
    /// carries the durable revision it was claimed at so the acknowledgement can be
    /// conditional on the row not having been re-enqueued meanwhile. See
    /// [`PendingClaim`].
    pub fn claim_pending_batch(&self, limit: usize) -> Result<Vec<PendingClaim>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = lock_conn(&self.conn)?;
        let mut stmt = conn.prepare(
            "SELECT path, queued_at, pending_paths.revision, pending_state.epoch
             FROM pending_paths CROSS JOIN pending_state
             WHERE pending_state.singleton = 1 AND attempts < ?2
             ORDER BY pending_paths.revision ASC, path ASC
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![sqlite_limit(limit), MAX_PENDING_ATTEMPTS], |row| {
                Ok(PendingClaim {
                    path: row.get(0)?,
                    queued_at: row.get(1)?,
                    watermark: PendingWatermark {
                        revision: row.get(2)?,
                        epoch: row.get(3)?,
                    },
                })
            })?
            .collect::<Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Acknowledge claimed work, leaving anything re-enqueued since the claim.
    ///
    /// The revision guard replaces timestamp and `attempts > 0` guards, which only
    /// worked because the drain bumped the attempt counter of every path in the
    /// batch *before* doing any work — so a single failure in a later,
    /// batch-wide step (a persist, a prune) charged an attempt to all 64 paths
    /// in the batch and five such failures quarantined the lot. See K1(d).
    pub fn clear_claimed_pending_paths(&self, claims: &[PendingClaim]) -> Result<usize> {
        let mut conn = lock_conn(&self.conn)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut cleared = 0;
        for claim in claims {
            Self::check_pending_epoch(&tx, &claim.watermark)?;
            cleared += tx
                .prepare_cached("DELETE FROM pending_paths WHERE path = ?1 AND revision = ?2")?
                .execute(params![claim.path, claim.watermark.revision])?;
        }
        tx.commit()?;
        Ok(cleared)
    }

    /// Charge only the claimed revision; a repaired/re-enqueued path starts anew.
    pub fn bump_pending_attempts(&self, claims: &[PendingClaim]) -> Result<()> {
        self.refuse_if_read_only()?;
        let mut conn = lock_conn(&self.conn)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for claim in claims {
            Self::check_pending_epoch(&tx, &claim.watermark)?;
            tx.execute(
                "UPDATE pending_paths SET attempts = attempts + 1
                 WHERE path = ?1 AND revision = ?2 AND attempts < ?3",
                params![claim.path, claim.watermark.revision, MAX_PENDING_ATTEMPTS],
            )?;
        }
        tx.commit()
    }

    pub fn clear_pending_paths(&self, paths: &[String]) -> Result<()> {
        let conn = lock_conn(&self.conn)?;
        let tx = conn.unchecked_transaction()?;
        for path in paths {
            tx.execute("DELETE FROM pending_paths WHERE path = ?1", params![path])?;
        }
        tx.commit()
    }
}
