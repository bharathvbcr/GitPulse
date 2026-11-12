use super::{
    checked_span, fts_damage_reason, fts_match_query, lock_conn, refusal, sqlite_limit,
    validate_head_sha, BuildHistoryRow, CachedSourceFreshness, ConnectionStamp, FileSymbolsPage,
    Keyed, LedgerKey, QuerySourceFreshness, SourceTreeDelta, Store, StoreStatus, StoredSymbol,
    UnresolvedSiteRow, UnresolvedSitesByName, FTS_PAYLOAD_SQL, MAX_PENDING_ATTEMPTS,
};
use crate::coverage::{CoverageGapRow, CoverageGapSample, CoverageGaps, DiscoveryRefusal};
use crate::edge_index::ResolutionSource;
use crate::schema::BUILD_HISTORY_RETENTION;
use devmap_analyze::model::{AnalysisStatus, AnalysisSummary};
use rusqlite::OptionalExtension;
use rusqlite::{params, Result};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

thread_local! {
    /// Full [`Store::status_snapshot`] reads on this thread, for the test that
    /// pins how many one `status` takes.
    #[cfg(test)]
    static STATUS_SNAPSHOTS: Cell<usize> = const { Cell::new(0) };
    static FRESHNESS_INTERLEAVE: Cell<Option<fn(&Store)>> = const { Cell::new(None) };
    static FRESHNESS_INTERLEAVE_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

struct InterleaveGuard;

impl Drop for InterleaveGuard {
    fn drop(&mut self) {
        FRESHNESS_INTERLEAVE_ACTIVE.with(|active| active.set(false));
    }
}

/// Run `hook` after [`Store::query_source_freshness`] has observed an empty
/// queue and before it clones the verdict cache. Production never sets it.
/// A hook that calls `query_source_freshness` again does not re-enter itself.
///
/// Callers live in this crate's tests. A non-test build of the crate (as a
/// dependency of `devmap-query`) therefore sees an unused function; that is
/// the seam, not a dead production path.
#[doc(hidden)]
#[allow(dead_code)]
pub fn set_source_freshness_interleave(hook: Option<fn(&Store)>) {
    FRESHNESS_INTERLEAVE.with(|slot| slot.set(hook));
}

/// `Ok(0)` means the cached verdict may still be returned. Any other count,
/// and any read error, is an unverified answer whose reason names the queue.
fn queue_blocks_cached_freshness(queued: Result<i64>) -> Option<QuerySourceFreshness> {
    match queued {
        Ok(0) => None,
        Ok(count) => Some(QuerySourceFreshness::unverified(format!(
            "{count} changed path(s) are queued and not yet indexed; the last verified \
source freshness no longer describes the tree"
        ))),
        Err(error) => Some(QuerySourceFreshness::unverified(format!(
            "source freshness unverified: could not read the pending queue ({error})"
        ))),
    }
}

/// The coverage-gap label a budget refusal is stored under: it is a
/// `ParseOutcome::Failed`, so it lands in `parse_failed` beside grammar faults.
/// The label has one owner, `ExtractionGap`, which the write side stores and
/// `CoverageGaps::slot` reads, so a rename cannot split the readers.
fn parse_failed_gap() -> &'static str {
    devmap_analyze::ExtractionGap::ParseFailed.label()
}

/// `?1` generation, `?2` gap label, `?3` prefix length, `?4` prefix. `substr`
/// rather than `LIKE`: the prefix is compared literally and case-sensitively,
/// as `starts_with` does.
const BUDGET_GAP_FILTER: &str = "generation_id = ?1 AND gap = ?2
               AND substr(reason, 1, ?3) = ?4";

/// The statement behind `Store::unresolved_sites_keyed` for `key_count` keys.
///
/// A free function so that a test reads the very text the store prepares: the
/// plan guard and the oracle parity test both go through it, and neither can
/// drift from what runs.
///
/// Placeholder order, which the caller binds in: the `key_count` keys, the
/// generation twice (`valid_from`, `valid_to`), then — only when
/// `unattributed_only` — the [`devmap_resolve::UNATTRIBUTED_LABELS`] and the
/// generation once more (the namesake subquery), and finally the per-key cap.
pub(super) fn keyed_ledger_sql(
    key: LedgerKey,
    key_count: usize,
    unattributed_only: bool,
) -> String {
    let placeholders = vec!["?"; key_count].join(", ");
    let (column, key_filter) = match key {
        LedgerKey::Callee => (
            "u.callee_name",
            format!("u.callee_name IN ({placeholders})"),
        ),
        LedgerKey::SourceSymbol => (
            "u.source_symbol",
            format!("u.source_symbol IN ({placeholders})"),
        ),
        LedgerKey::SourceFile => (
            "p.path",
            format!("u.source_file_id IN (SELECT id FROM paths WHERE path IN ({placeholders}))"),
        ),
    };
    // A receiver call whose method name no symbol at this generation carries
    // cannot be a missed edge into it — an edge needs a target, and no target
    // has that name (`rows.length`, `mu.Unlock()`: 126,023 of ScholarLM's
    // 200,790 untyped-receiver rows). A receiver-less call keeps counting
    // without a namesake: `f = make(); f()` may hold any function. In SQL,
    // before the cap, for the reason the class filter is.
    let class_filter = if unattributed_only {
        format!(
            "AND u.classification_id IN (SELECT id FROM unresolved_texts WHERE text IN ({}))
                   AND (u.receiver IS NULL
                        OR u.callee_name IN (SELECT n.name FROM generation_nodes n
                                             WHERE n.generation_id = ?))",
            vec!["?"; devmap_resolve::UNATTRIBUTED_LABELS.len()].join(", ")
        )
    } else {
        String::new()
    };
    format!(
        "SELECT key, path, source_symbol, receiver, class, rn, callee_name FROM (
                 SELECT {column} AS key, p.path AS path,
                        u.source_symbol AS source_symbol, u.receiver AS receiver,
                        c.text AS class, u.callee_name AS callee_name,
                        ROW_NUMBER() OVER (
                            PARTITION BY {column}
                            ORDER BY p.path, u.source_symbol, u.unresolved_id
                        ) AS rn
                 FROM unresolved_rows u
                 JOIN paths p            ON p.id = u.source_file_id
                 JOIN unresolved_texts c ON c.id = u.classification_id
                 WHERE {key_filter}
                   AND u.valid_from <= ?
                   AND (u.valid_to IS NULL OR u.valid_to > ?)
                   {class_filter}
             )
             WHERE rn <= ?
             ORDER BY key, rn"
    )
}

/// How a generation's resolver stamp relates to this binary's
/// `RESOLUTION_REUSE_EPOCH`. The stored bytes never leave the store: each
/// variant has fixed, bounded text, so a hostile or oversized stamp can neither
/// bloat a status field nor spell a phrase `rebuild_required_reason` matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolverStamp {
    /// Equal to this binary's epoch, or there is no generation to judge.
    Current,
    /// No `resolver_epoch` key: a generation written before the stamp existed.
    Unrecorded,
    /// `analysis_json` is not a JSON document, so the stamp cannot be read.
    Unreadable,
    /// More than one `resolver_epoch` key, or a value that is not JSON text.
    Malformed,
    /// An unsigned integer lower than this binary's.
    Older,
    /// An unsigned integer higher than this binary's: a newer devmap wrote it.
    Newer,
    /// Text that is neither equal nor an ordered pair of unsigned integers.
    Different,
}

/// What a status reason for a newer-resolver store begins with; the hook keys
/// its wording on it so it never advises a rebuild that would roll edges back.
pub const RESOLVER_NEWER_REASON: &str = "stored edges were written by a newer devmap";

impl ResolverStamp {
    /// The fixed status text for a stamp that is not current.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Current => "stored edges were resolved by this resolver",
            Self::Unrecorded => "stored edges carry an unrecorded resolver epoch (no stamp: written before it existed); run `devmap build`",
            Self::Unreadable => "stored analysis document is unreadable, so the resolver that wrote the edges is unknown; run `devmap build`",
            Self::Malformed => "stored resolver epoch stamp is malformed; run `devmap build`",
            Self::Older => "stored edges were resolved by an older resolver epoch than this binary's; run `devmap build`",
            Self::Newer => "stored edges were written by a newer devmap (a newer resolver epoch than this binary's); rebuilding with this binary would replace them with an older resolver's edges, so update devmap instead",
            Self::Different => "stored edges were resolved under a different resolver epoch than this binary's; run `devmap build`",
        }
    }
}

/// `(json_valid, stamp key count, stamp JSON type, stamp prefix, stamp length)`.
type StampRow = (
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<i64>,
);

/// An epoch as an unsigned integer, only when it is plain digits (no sign, no
/// padding) and short enough to be one; anything else is not orderable.
fn unsigned_epoch(text: &str) -> Option<u64> {
    if text.is_empty() || text.len() > 18 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

impl Store {
    /// Re-measure a build's `build_ms` once all of its work is done.
    ///
    /// The save transaction samples `GenerationWriteOpts::build_started`
    /// *inside* itself, before the row is inserted, so the value it stores
    /// cannot include the commit, the generation and extraction prunes or the
    /// vacuum that follow it: on a frozen GitPulse corpus that left 1.05-8.2 s
    /// of a build unaccounted for (the commit alone was 8 s on a starved
    /// host). The caller, still holding the writer lock, calls this after the
    /// last of them with the same `started`, and the row then covers the whole
    /// locked build. The in-transaction value stays as the crash-safe lower
    /// bound for a build that dies in between.
    ///
    /// Returns whether a row was updated. A row whose `build_ms` is NULL
    /// (unmeasured) is left NULL, and a row a concurrent writer already pruned
    /// is simply absent: neither is an error.
    pub fn finalize_build_ms(
        &self,
        generation_id: u32,
        started: std::time::Instant,
    ) -> Result<bool> {
        let build_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let conn = lock_conn(&self.conn)?;
        let changed = conn.execute(
            "UPDATE build_history SET build_ms = ?2
             WHERE generation_id = ?1 AND build_ms IS NOT NULL",
            params![generation_id, build_ms],
        )?;
        Ok(changed > 0)
    }

    /// Most recent builds, newest first. `limit` is clamped to the retention cap.
    pub fn build_history(&self, limit: usize) -> Result<Vec<BuildHistoryRow>> {
        let conn = lock_conn(&self.conn)?;
        let mut stmt = conn.prepare(
            "SELECT generation_id, built_at, head_sha, files, symbols, edges,
                    dead_confident, dead_ambiguous, parse_failed, languages_covered,
                    build_ms, db_bytes
             FROM build_history
             ORDER BY built_at DESC, generation_id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit.min(BUILD_HISTORY_RETENTION) as i64], |row| {
            Ok(BuildHistoryRow {
                generation_id: row.get(0)?,
                built_at: row.get::<_, f64>(1)? as i64,
                head_sha: row.get(2)?,
                files: row.get::<_, i64>(3)? as u64,
                symbols: row.get::<_, i64>(4)? as u64,
                edges: row.get::<_, i64>(5)? as u64,
                dead_confident: row.get::<_, i64>(6)? as u64,
                dead_ambiguous: row.get::<_, i64>(7)? as u64,
                parse_failed: row.get::<_, i64>(8)? as u64,
                languages_covered: row.get::<_, i64>(9)? as u64,
                build_ms: row
                    .get::<_, Option<i64>>(10)?
                    .map(|value| {
                        u64::try_from(value)
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(10, value))
                    })
                    .transpose()?,
                db_bytes: row.get::<_, i64>(11)? as u64,
            })
        })?;
        rows.collect()
    }

    pub fn latest_generation_id(&self) -> Result<Option<u32>> {
        let conn = lock_conn(&self.conn)?;
        conn.query_row(
            "SELECT id FROM generations ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
    }

    /// Same question as [`Self::latest_generation_head_sha`], under the older
    /// name, and now the same query.
    ///
    /// These were two independent implementations of one read —
    /// `ORDER BY id DESC LIMIT 1` here, `WHERE id = (SELECT max(id) …)` there —
    /// and they had already drifted in the way two copies do: one was gated
    /// `#[cfg(feature = "parse")]` and the other was not, so an embedder
    /// linking the store without the grammars could reach the head through one
    /// name and not the other. The gate was debris — the read is pure SQL and
    /// has nothing to do with parsing — but it stood because nothing pointed
    /// out that the same answer was already available beside it.
    ///
    /// Delegating rather than deleting: both names have callers across four
    /// crates, and which one survives is a rename worth deciding on its own.
    /// One implementation is the part that has to be true today.
    pub fn latest_generation_head(&self) -> Result<Option<String>> {
        self.latest_generation_head_sha()
    }

    /// Absolute root the newest generation was built from, when recorded.
    /// D17: unresolved calls recorded for the latest generation.
    ///
    /// This is the honest denominator for graph completeness — a symbol with no
    /// callers is a different claim depending on whether anything failed to
    /// resolve against it.
    pub fn latest_unresolved(&self, limit: usize) -> Result<Vec<(String, String, String)>> {
        let conn = lock_conn(&self.conn)?;
        let mut stmt = conn.prepare(
            "SELECT source_symbol, callee_name, reason
             FROM generation_unresolved
             WHERE generation_id = (SELECT max(id) FROM generations)
             ORDER BY ordinal
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![sqlite_limit(limit)], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Unresolved call sites at `generation` whose callee is one of `names`, at
    /// most `limit_per_name` per name, each name's rows ordered by
    /// `(file, symbol)` and flagged when the cap cut them.
    ///
    /// The ledger is where a caller the resolver could not bind is kept — an
    /// untyped receiver, a module loaded by path — and until this existed no
    /// query read it by name: `impact` answered from edges alone, and a method
    /// whose only production caller sat here reported no such caller.
    ///
    /// **One scan for every name.** The ledger is not indexed on `callee_name`
    /// (an index is a schema change), so each read is a pass over the ledger —
    /// measured on scholarlm's 558,288 rows at ~40 ms warm. A read per name
    /// made a file-target `impact` pay eight of them: +218 ms end to end.
    /// `ROW_NUMBER()` partitioned by name applies the per-name cap inside the
    /// one pass, and one row past each cap says whether it bit.
    ///
    /// **At the caller's generation, not the latest.** `impact` walks one
    /// generation's edges and reads this beside it; a daemon can commit
    /// between the two. Ledger rows carry `valid_from`/`valid_to`, so any
    /// retained generation is readable exactly. `None` when `generation` is no
    /// longer retained — the pair cannot be made consistent, and the caller
    /// must say so rather than mix two states of the repository.
    pub fn unresolved_sites_naming(
        &self,
        generation: u32,
        names: &[String],
        limit_per_name: usize,
    ) -> Result<Option<UnresolvedSitesByName>> {
        self.unresolved_sites_keyed(generation, LedgerKey::Callee, names, false, limit_per_name)
    }

    /// [`Self::unresolved_sites_naming`], keeping only the sites that may hide
    /// an edge — the classes in [`devmap_resolve::UNATTRIBUTED_LABELS`], minus
    /// receiver calls whose method name no symbol at `generation` carries.
    ///
    /// The filter is in SQL, not applied to the rows afterwards, so the
    /// per-name cap counts only the rows that matter: a name with a hundred
    /// builtin namesakes ahead of one untyped receiver must still report the
    /// receiver.
    pub fn unattributed_sites_naming(
        &self,
        generation: u32,
        names: &[String],
        limit_per_name: usize,
    ) -> Result<Option<UnresolvedSitesByName>> {
        self.unresolved_sites_keyed(generation, LedgerKey::Callee, names, true, limit_per_name)
    }

    /// The unattributed sites whose *caller* is one of `symbols`, keyed by that
    /// caller — the calls inside a walked symbol the resolver could not bind
    /// and that may hide an edge (same filter as
    /// [`Self::unattributed_sites_naming`]).
    ///
    /// The ledger has no index on `source_symbol` (adding one is a schema
    /// change), so this is one scan of it: measured on ScholarLM's 573,716
    /// rows at 51–228 ms. One scan answers every symbol, which is why the caller
    /// passes them all at once.
    pub fn unattributed_sites_within(
        &self,
        generation: u32,
        symbols: &[String],
        limit_per_symbol: usize,
    ) -> Result<Option<UnresolvedSitesByName>> {
        self.unresolved_sites_keyed(
            generation,
            LedgerKey::SourceSymbol,
            symbols,
            true,
            limit_per_symbol,
        )
    }

    /// The unattributed sites written in one of `files`, keyed by file — what a
    /// file's outbound dependency list cannot show. One scan, as above.
    pub fn unattributed_sites_in_files(
        &self,
        generation: u32,
        files: &[String],
        limit_per_file: usize,
    ) -> Result<Option<UnresolvedSitesByName>> {
        self.unresolved_sites_keyed(
            generation,
            LedgerKey::SourceFile,
            files,
            true,
            limit_per_file,
        )
    }

    /// The one ledger read behind the four above: rows at `generation` whose
    /// `key` column is one of `keys`, at most `limit_per_key` per key, ordered
    /// by `(file, symbol)` and flagged when the cap cut them.
    fn unresolved_sites_keyed(
        &self,
        generation: u32,
        key: LedgerKey,
        keys: &[String],
        // Only the sites that may hide an edge: see `unattributed_sites_naming`.
        unattributed_only: bool,
        limit_per_key: usize,
    ) -> Result<Option<UnresolvedSitesByName>> {
        let conn = lock_conn(&self.conn)?;
        let retained: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM generations WHERE id = ?1)",
            params![generation],
            |row| row.get(0),
        )?;
        if !retained {
            return Ok(None);
        }
        let mut found: UnresolvedSitesByName = keys
            .iter()
            .map(|name| (name.clone(), (Vec::new(), false)))
            .collect();
        if keys.is_empty() {
            return Ok(Some(found));
        }
        let sql = keyed_ledger_sql(key, keys.len(), unattributed_only);
        let mut stmt = conn.prepare(&sql)?;
        let generation = i64::from(generation);
        let cap = sqlite_limit(limit_per_key.saturating_add(1));
        let mut values: Vec<rusqlite::types::Value> = keys
            .iter()
            .map(|name| rusqlite::types::Value::Text(name.clone()))
            .collect();
        values.push(rusqlite::types::Value::Integer(generation));
        values.push(rusqlite::types::Value::Integer(generation));
        if unattributed_only {
            values.extend(
                devmap_resolve::UNATTRIBUTED_LABELS
                    .iter()
                    .map(|label| rusqlite::types::Value::Text((*label).to_string())),
            );
            values.push(rusqlite::types::Value::Integer(generation));
        }
        values.push(rusqlite::types::Value::Integer(cap));
        let mut rows = stmt.query(rusqlite::params_from_iter(values.iter()))?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            let rank: i64 = row.get(5)?;
            let Some((sites, truncated)) = found.get_mut(&name) else {
                continue;
            };
            if usize::try_from(rank).unwrap_or(usize::MAX) > limit_per_key {
                *truncated = true;
                continue;
            }
            sites.push(UnresolvedSiteRow {
                source_file: row.get(1)?,
                source_symbol: row.get(2)?,
                receiver: row.get(3)?,
                classification: row.get(4)?,
                callee_name: row.get(6)?,
            });
        }
        Ok(Some(found))
    }

    /// Total unresolved rows across every retained generation. Test-facing:
    /// the point is to prove the table is pruned, not just written.
    pub fn count_unresolved_rows(&self) -> Result<usize> {
        let conn = lock_conn(&self.conn)?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM generation_unresolved", [], |row| {
                row.get(0)
            })?;
        Ok(count as usize)
    }

    /// The git HEAD the latest generation was built from, if any.
    ///
    /// Exists for B5: a commit, branch switch, rebase or stash changes what the
    /// index should contain while touching no watched file. Comparing this
    /// against the working tree's current HEAD is what lets the daemon notice
    /// that its generation describes a tree that no longer exists.
    ///
    /// `None` means no generation has been written. A stored `"unavailable"`
    /// (what the CLI stamps outside a git repository) is returned verbatim
    /// rather than mapped to `None`, because "built outside git" and "never
    /// built" are different facts and only one of them warrants a rebuild.
    pub fn latest_generation_head_sha(&self) -> Result<Option<String>> {
        let conn = lock_conn(&self.conn)?;
        let sha = conn
            .query_row(
                "SELECT head_sha FROM generations WHERE id = (SELECT max(id) FROM generations)",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(sha)
    }

    /// The repository root the latest generation was built from.
    ///
    /// The pair to [`Self::latest_generation_head_sha`], and needed by the
    /// same callers for the same reason: an analysis that joins these spans to
    /// git has to run against *the* repository they were taken from, and a
    /// root supplied by the caller is a second opinion that can disagree.
    /// Where the two differ the generation's own root is the correct one,
    /// because it is the one the byte offsets describe.
    ///
    /// `None` when the generation recorded no root — an older store, or one
    /// built outside a repository. Never an empty string dressed as a path.
    pub fn latest_generation_repo_root(&self) -> Result<Option<String>> {
        if let Some(ref override_root) = self.repo_root_override {
            let conn = lock_conn(&self.conn)?;
            let has_gen: Option<i64> = conn
                .query_row("SELECT 1 FROM generations LIMIT 1", [], |row| row.get(0))
                .optional()?;
            return Ok(
                has_gen.and_then(|_| (!override_root.is_empty()).then(|| override_root.clone()))
            );
        }
        self.stored_latest_generation_repo_root()
    }

    /// [`Self::latest_generation_repo_root`] read from the store itself, whatever
    /// root the caller opened it with. A claim about what a file holds (a stacked
    /// lease's `source_repo_root`) must be checked against the stored row; the
    /// override is the caller's own opinion and would always agree with itself.
    pub fn stored_latest_generation_repo_root(&self) -> Result<Option<String>> {
        let conn = lock_conn(&self.conn)?;
        if let Some(seeded) = Self::seeded_repo_root_in(&conn)? {
            let has_gen: Option<i64> = conn
                .query_row("SELECT 1 FROM generations LIMIT 1", [], |row| row.get(0))
                .optional()?;
            if has_gen.is_some() {
                return Ok(Some(seeded));
            }
        }
        let root: Option<Option<String>> = conn
            .query_row(
                "SELECT repo_root FROM generations WHERE id = (SELECT max(id) FROM generations)",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(root.flatten().filter(|root| !root.is_empty()))
    }

    /// Rewrite the latest generation's git identity without writing a new graph.
    ///
    /// Callers that have already proved the working tree matches this
    /// generation — the CLI skip path, a daemon drain whose HEAD moved but
    /// whose file hashes did not — used to leave `head_sha` on the commit the
    /// generation was first written at. `status` then treated that lag as
    /// "rebuild required", and the rebuild skipped, so freshness could never
    /// recover. This is the missing write: same generation id, same hashes,
    /// current HEAD.
    pub fn restamp_latest_head(&self, head_sha: &str) -> Result<()> {
        self.restamp_head(head_sha, true)
    }

    /// [`Self::restamp_latest_head`] for a store seeded copy-on-write that has
    /// not yet written a generation of its own: the stamp moves, and the seed
    /// rebind and the donor's historical `generations.repo_root` rows stay as
    /// they are, so the history keeps sharing pages with the donor. The first
    /// generation the store writes normalises them, as it does for any seeded
    /// store.
    pub fn restamp_latest_head_keeping_seed_rebind(&self, head_sha: &str) -> Result<()> {
        self.restamp_head(head_sha, false)
    }

    fn restamp_head(&self, head_sha: &str, normalize_seed_rebind: bool) -> Result<()> {
        self.refuse_if_read_only()?;
        validate_head_sha(head_sha)?;
        let mut conn = lock_conn(&self.conn)?;
        let tx = conn.transaction_with_behavior(Self::GENERATION_TX_BEHAVIOR)?;
        let changed = tx.execute(
            "UPDATE generations SET head_sha = ?1
             WHERE id = (SELECT max(id) FROM generations)",
            params![head_sha],
        )?;
        if changed == 0 {
            return Err(refusal(
                "no generation to restamp: nothing has been indexed yet — run `devmap build`",
            ));
        }
        if normalize_seed_rebind {
            let owner: Option<String> = tx
                .query_row(
                    "SELECT NULLIF(repo_root, '') FROM pending_state WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            Self::clear_seed_rebind_and_normalize_generations(&tx, owner.as_deref())?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Whether the latest generation still describes the working tree.
    ///
    /// Payload identity, per-file content hashes, and the refusal inventory
    /// (discovery and the project manifests, through `refusal_inventory`).
    /// Git HEAD is not consulted: it is provenance, restamped by the caller
    /// once this returns true. `false` means a full rebuild (or a drain that
    /// re-reads the tree) is required; an error means the question could not
    /// be asked, which callers must treat as "do not skip".
    ///
    /// Compiled with and without the `parse` feature: `scan_tree`,
    /// `collect_project_manifests` and the payload identities answer without
    /// a grammar, so a query-only embedder gets the same verdict.
    pub fn latest_generation_matches_working_tree(&self) -> Result<bool> {
        // Extractor-only on purpose. The resolver stamp is not asked here, so
        // seeding and stacking judge a donor (including a pre-stamp one) as they
        // did before the stamp existed; a build or a drain that must not trust
        // the stored edges asks `latest_generation_payload_is_current`.
        if !self.latest_generation_extraction_is_current()? {
            return Ok(false);
        }
        let Some(root) = self.latest_repo_root()? else {
            return Ok(false);
        };
        let hashes = self.latest_file_hashes()?;
        let stored = self.latest_discovery_inventory()?;
        let scanned = match devmap_extract::scan_tree(Path::new(&root)) {
            Ok(scanned) => scanned,
            Err(error) => {
                return Err(refusal(format!(
                    "working tree could not be compared to the indexed generation: {error}"
                )))
            }
        };
        if !scanned.matches_file_hashes(&hashes) {
            return Ok(false);
        }
        let manifests =
            devmap_extract::collect_project_manifests(Path::new(&root)).map_err(|error| {
                refusal(format!(
                    "project manifests could not be compared to the indexed generation: {error}"
                ))
            })?;
        Ok(crate::DiscoveryInventory::of(&scanned.report, &manifests) == stored)
    }

    /// Whether the latest generation's stored edges may be trusted as they are:
    /// the extractor that wrote its payloads AND the resolver that wrote its
    /// edges are the ones running now.
    ///
    /// A store with no generation is current, as before. This is the check the
    /// CLI no-change shortcut, `affected_closure` and the daemon drain share, so
    /// a resolver-only change (a `RESOLUTION_REUSE_EPOCH` move) defeats all
    /// three. One lock covers both questions, so they read one snapshot.
    pub fn latest_generation_payload_is_current(&self) -> Result<bool> {
        let conn = lock_conn(&self.conn)?;
        // Held per (generation, stamp, age): see `Store::generation_checks`.
        let stamp = Self::connection_stamp(&conn)?;
        let generation: Option<u32> =
            conn.query_row("SELECT max(id) FROM generations", [], |row| row.get(0))?;
        if let (Some(generation), Ok(checks)) = (generation, self.generation_checks.lock()) {
            if let Some(current) = Keyed::holds(&checks.payload_current, generation, stamp) {
                return Ok(*current);
            }
        }
        let current = Self::payload_is_current_locked(&conn)?;
        if let (Some(generation), Ok(mut checks)) = (generation, self.generation_checks.lock()) {
            checks.payload_current = Some(Keyed::new(generation, stamp, current));
        }
        Ok(current)
    }

    /// The uncached [`Self::latest_generation_payload_is_current`]: the
    /// extractor that wrote the latest generation's payloads and the resolver
    /// that wrote its edges are both the ones running now. The cache above may
    /// key it by generation: the resolver stamp is read from that generation's
    /// row and the epoch it is compared with is compiled in.
    fn payload_is_current_locked(conn: &rusqlite::Connection) -> Result<bool> {
        Ok(Self::extraction_is_current_on(conn)?
            && Self::resolver_stamp_on(conn)? == ResolverStamp::Current)
    }

    /// How the resolver stamp in the latest generation relates to this
    /// binary's `RESOLUTION_REUSE_EPOCH`. Read-only; see [`ResolverStamp`].
    pub fn latest_generation_resolver_stamp(&self) -> Result<ResolverStamp> {
        let conn = lock_conn(&self.conn)?;
        Self::resolver_stamp_on(&conn)
    }

    fn resolver_stamp_on(conn: &rusqlite::Connection) -> Result<ResolverStamp> {
        // `json_each` lists a duplicated key once per occurrence, so the count
        // sees what `json_extract` would silently resolve. Only a bounded prefix
        // of a text stamp is fetched: a hostile stamp must cost no more to read
        // than a real one. Any other JSON type is malformed, never coerced.
        let row: Option<StampRow> = conn
            .query_row(
                "SELECT json_valid(analysis_json),
                        CASE WHEN json_valid(analysis_json) THEN
                             (SELECT count(*) FROM json_each(analysis_json)
                               WHERE key = 'resolver_epoch') END,
                        CASE WHEN json_valid(analysis_json) THEN
                             json_type(analysis_json, '$.resolver_epoch') END,
                        CASE WHEN json_valid(analysis_json) THEN
                             substr(json_extract(analysis_json, '$.resolver_epoch'), 1, 24) END,
                        CASE WHEN json_valid(analysis_json) THEN
                             length(json_extract(analysis_json, '$.resolver_epoch')) END
                   FROM generations WHERE id = (SELECT max(id) FROM generations)",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((valid, keys, kind, prefix, length)) = row else {
            return Ok(ResolverStamp::Current);
        };
        if valid != Some(1) {
            return Ok(ResolverStamp::Unreadable);
        }
        Ok(match (keys.unwrap_or(0), kind.as_deref()) {
            (0, _) => ResolverStamp::Unrecorded,
            (1, Some("text")) => {
                let current = devmap_resolve::resolver::RESOLUTION_REUSE_EPOCH;
                let stored = prefix.unwrap_or_default();
                if usize::try_from(length.unwrap_or(0)).ok() == Some(stored.chars().count())
                    && stored == current
                {
                    ResolverStamp::Current
                } else {
                    match (unsigned_epoch(&stored), unsigned_epoch(current)) {
                        (Some(stored), Some(current)) if stored < current => ResolverStamp::Older,
                        (Some(stored), Some(current)) if stored > current => ResolverStamp::Newer,
                        _ => ResolverStamp::Different,
                    }
                }
            }
            _ => ResolverStamp::Malformed,
        })
    }

    /// Whether the extractor that wrote the latest generation's payloads is the
    /// one running now. See [`Self::latest_generation_payload_is_current`] for
    /// the check that also asks the resolver.
    pub fn latest_generation_extraction_is_current(&self) -> Result<bool> {
        let conn = lock_conn(&self.conn)?;
        Self::extraction_is_current_on(&conn)
    }

    fn extraction_is_current_on(conn: &rusqlite::Connection) -> Result<bool> {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT language, grammar_version, analyzer_version
             FROM generation_files
             WHERE generation_id = (SELECT max(id) FROM generations)",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            let (language, grammar, analyzer) = row?;
            // Answers without the parsing frontend too, from the identities a
            // parsing build of this version stamps (`PAYLOAD_GRAMMAR_IDENTITIES`,
            // pinned to the compiled grammars by a test). A query-only reader
            // such as GitPulse's could otherwise never call a store current.
            let (current_grammar, current_analyzer) =
                devmap_extract::cache::current_payload_identity(&language);
            // A NULL version predates these columns: unknown identity is not a
            // matching one.
            if grammar.as_deref() != Some(current_grammar.as_str())
                || analyzer.as_deref() != Some(current_analyzer.as_str())
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// [`Store::latest_file_hashes`] for the freshness comparison, read once
    /// per generation and shared.
    ///
    /// The generation id and its rows are read in one snapshot, so the map is
    /// the one the id names; `status` compares it to a fresh tree walk on every
    /// call. Held per (generation, stamp, age): see
    /// `Store::generation_checks`.
    fn latest_file_hashes_shared(&self) -> Result<std::sync::Arc<BTreeMap<String, u64>>> {
        let conn = lock_conn(&self.conn)?;
        let stamp = Self::connection_stamp(&conn)?;
        let snapshot = conn.unchecked_transaction()?;
        let Some(generation) = Self::latest_generation_id_locked(&snapshot)? else {
            return Ok(std::sync::Arc::new(BTreeMap::new()));
        };
        if let Ok(checks) = self.generation_checks.lock() {
            if let Some(hashes) = Keyed::holds(&checks.file_hashes, generation, stamp) {
                return Ok(std::sync::Arc::clone(hashes));
            }
        }
        let mut stmt = snapshot.prepare(
            "SELECT p.path, f.content_hash
             FROM generation_files f
             JOIN paths p ON p.id = f.file_id
             WHERE f.generation_id = ?1",
        )?;
        let hashes: BTreeMap<String, u64> = stmt
            .query_map(params![generation], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })?
            .collect::<Result<_>>()?;
        drop(stmt);
        let hashes = std::sync::Arc::new(hashes);
        if let Ok(mut checks) = self.generation_checks.lock() {
            checks.file_hashes = Some(Keyed::new(
                generation,
                stamp,
                std::sync::Arc::clone(&hashes),
            ));
        }
        Ok(hashes)
    }

    /// `(path, content_hash)` for every file in the latest generation.
    ///
    /// Lets a build decide, before resolving anything, whether the tree it just
    /// scanned is the one already committed.
    pub fn latest_file_hashes(&self) -> Result<BTreeMap<String, u64>> {
        let conn = lock_conn(&self.conn)?;
        let mut stmt = conn.prepare(
            "SELECT p.path, f.content_hash
             FROM generation_files f
             JOIN paths p ON p.id = f.file_id
             WHERE f.generation_id = (SELECT max(id) FROM generations)",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })?
            .collect::<Result<BTreeMap<_, _>>>()?;
        Ok(rows)
    }

    /// Symbol *names* per file in the latest generation.
    ///
    /// Names, not qualified names: the resolver's global indexes are keyed by
    /// bare name, so that is the granularity at which a definition moving can
    /// change another file's resolution.
    pub fn latest_symbol_names_by_file(&self) -> Result<BTreeMap<String, BTreeSet<String>>> {
        let conn = lock_conn(&self.conn)?;
        let mut stmt = conn.prepare(
            "SELECT p.path, n.name
             FROM generation_nodes n
             JOIN paths p ON p.id = n.file_id
             WHERE n.generation_id = (SELECT max(id) FROM generations)",
        )?;
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (path, name) = row?;
            out.entry(path).or_default().insert(name);
        }
        Ok(out)
    }

    /// Symbols declared in `path` in the latest generation.
    ///
    /// `None` when the store holds no generation. An empty `rows` with `Some`
    /// means the generation exists and this path has no indexed symbols (or
    /// is not in the generation at all) — a completed read, not a missing map.
    ///
    /// Bounded to one file on purpose: callers that need the whole corpus use
    /// [`Self::all_symbols`]. The generation's `head_sha` travels with the
    /// rows so a mismatch against the SHA the caller asked for is visible.
    pub fn latest_symbols_for_file(&self, path: &str) -> Result<Option<FileSymbolsPage>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        let head_sha: String = snapshot.query_row(
            "SELECT head_sha FROM generations WHERE id = ?1",
            params![generation],
            |row| row.get(0),
        )?;
        let mut stmt = snapshot.prepare(
            "SELECT n.name, n.qualified_name, n.kind, p.path,
                    n.span_start, n.span_end, n.is_exported, f.content_hash
             FROM generations g
             JOIN file_rows r
               ON g.id >= r.valid_from AND (r.valid_to IS NULL OR g.id < r.valid_to)
             JOIN payload_nodes n ON n.payload_id = r.payload_id
             JOIN file_payloads f ON f.payload_id = r.payload_id
             JOIN paths p ON p.id = r.file_id
             WHERE g.id = ?1 AND p.path = ?2
             ORDER BY n.span_start, n.ord_in_file",
        )?;
        let rows = stmt
            .query_map(params![generation, path], |row| {
                let name: String = row.get(0)?;
                let path: String = row.get(3)?;
                let (span_start, span_end) = checked_span(&path, &name, row.get(4)?, row.get(5)?)?;
                Ok(StoredSymbol {
                    name,
                    qualified_name: row.get(1)?,
                    kind: row.get(2)?,
                    path,
                    span_start,
                    span_end,
                    is_exported: row.get::<_, i64>(6)? != 0,
                    content_hash: row.get::<_, i64>(7)? as u64,
                })
            })?
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(FileSymbolsPage {
            generation,
            head_sha,
            rows,
        }))
    }

    /// Every edge of the latest generation, rendered for comparison.
    /// Test-facing: proving incremental output equals cold output needs the
    /// whole edge set, not a count.
    pub fn latest_edges_for_test(&self) -> Result<Vec<String>> {
        let conn = lock_conn(&self.conn)?;
        let mut stmt = conn.prepare(
            "SELECT source_symbol, target_symbol, edge_kind, printf('%.5f', confidence)
             FROM generation_edges
             WHERE generation_id = (SELECT max(id) FROM generations)",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(format!(
                    "{}>{}:{}:{}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?
                ))
            })?
            .collect::<Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn latest_repo_root(&self) -> Result<Option<String>> {
        self.latest_generation_repo_root()
    }

    /// The latest generation's analysis summary.
    ///
    /// `discovery_refused_files` is **derived** from the generation's refusal
    /// inventory rather than read out of the stored JSON, so the number a
    /// consumer acts on and the paths it can ask for are one measurement. The
    /// serialized field survives only as the record of whether discovery was
    /// measured at all: `None` there stays `None` here — nobody walked, and
    /// `Some(0)` would say the corpus was seen in full. `save_generation`
    /// refuses a write whose two halves disagree, so the two can only differ
    /// for a generation written before the inventory existed, whose rows are
    /// genuinely absent.
    pub fn latest_analysis(&self) -> Result<Option<AnalysisSummary>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        let raw: Option<String> = snapshot
            .query_row(
                "SELECT analysis_json FROM generations WHERE id = ?1",
                params![generation],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        let mut summary: AnalysisSummary = serde_json::from_str(&raw)
            .map_err(|error| refusal(format!("stored generation analysis is invalid: {error}")))?;
        if summary.discovery_refused_files.is_some() {
            let refused: usize = snapshot.query_row(
                "SELECT COUNT(*) FROM generation_coverage_gaps
                 WHERE generation_id = ?1 AND gap = ?2",
                params![generation, crate::GAP_DISCOVERY_REFUSED],
                |row| row.get::<_, i64>(0).map(|count| count as usize),
            )?;
            summary.discovery_refused_files = Some(refused);
        }
        Ok(Some(summary))
    }

    /// Node and edge counts for `generation`, counted at most once.
    ///
    /// See [`Store::generation_counts`] for why the generation id is a
    /// sufficient key. Takes the caller's snapshot rather than the raw
    /// connection so the first (uncached) count is still read inside the
    /// transaction that resolved the generation id.
    fn generation_counts_locked(
        &self,
        snapshot: &rusqlite::Transaction<'_>,
        generation: u32,
    ) -> Result<(usize, usize)> {
        if let Ok(cache) = self.generation_counts.lock() {
            if let Some((cached, nodes, edges)) = *cache {
                if cached == generation {
                    return Ok((nodes, edges));
                }
            }
        }
        let nodes: usize = snapshot.query_row(
            "SELECT COUNT(*) FROM generation_nodes WHERE generation_id = ?1",
            params![generation],
            |row| row.get::<_, i64>(0).map(|n| n as usize),
        )?;
        let edges: usize = snapshot.query_row(
            "SELECT COUNT(*) FROM generation_edges WHERE generation_id = ?1",
            params![generation],
            |row| row.get::<_, i64>(0).map(|n| n as usize),
        )?;
        if let Ok(mut cache) = self.generation_counts.lock() {
            *cache = Some((generation, nodes, edges));
        }
        Ok((nodes, edges))
    }

    /// Why the latest generation's full-text index cannot be trusted, if it
    /// cannot.
    ///
    /// `status` never read the index, so a store whose `nodes_fts` was
    /// unreadable — every search failing with "database disk image is
    /// malformed" — or half gone — every search silently returning a subset —
    /// reported itself healthy. Two checks, priced differently:
    ///
    /// - **Readable**, on every call: one MATCH for one of the generation's own
    ///   symbols, which has to walk the index structure and its postings. A
    ///   corrupt read is the unreadable case; ~0.1 ms.
    /// - **Whole**, once per generation (see `fts_reachable`): how many of the
    ///   generation's symbols the map and the index together still reach,
    ///   against how many it has. Fewer is the partial loss
    ///   `require_searchable_index` documents it cannot see.
    ///
    /// A MATCH that finds nothing for a symbol whose row is still reachable is
    /// reported too: the rows survived and the postings that find them did not.
    fn fts_health_locked(
        &self,
        snapshot: &rusqlite::Transaction<'_>,
        generation: u32,
        node_count: usize,
        whole: bool,
    ) -> Result<Option<String>> {
        if node_count == 0 {
            return Ok(None);
        }
        let unreadable = |error: rusqlite::Error| -> Result<Option<String>> {
            if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseCorrupt) {
                Ok(Some(fts_damage_reason(&format!(
                    "the full-text index (`nodes_fts`) could not be read: {error}"
                ))))
            } else {
                Err(error)
            }
        };

        let probe: Option<String> = snapshot
            .query_row(
                "SELECT n.name
                   FROM generations g
                   JOIN file_rows r
                     ON g.id >= r.valid_from AND (r.valid_to IS NULL OR g.id < r.valid_to)
                   JOIN payload_nodes n ON n.payload_id = r.payload_id
                  WHERE g.id = ?1
                  ORDER BY n.payload_id, n.ord_in_file LIMIT 1",
                params![generation],
                |row| row.get(0),
            )
            .optional()?;
        let probe_hits = match &probe {
            Some(name) => {
                let match_query = fts_match_query(name)?;
                match snapshot.query_row(
                    &format!(
                        "SELECT COUNT(*)
                         FROM nodes_fts
                         CROSS JOIN file_rows r ON r.payload_id = {FTS_PAYLOAD_SQL}
                         JOIN generations g
                           ON g.id = ?1 AND g.id >= r.valid_from
                          AND (r.valid_to IS NULL OR g.id < r.valid_to)
                         WHERE nodes_fts MATCH ?2"
                    ),
                    params![generation, match_query],
                    |row| row.get::<_, i64>(0),
                ) {
                    Ok(hits) => Some(hits),
                    Err(error) => return unreadable(error),
                }
            }
            None => None,
        };

        // The whole check walks every symbol of the generation through the
        // index, which on a large store is the bulk of a no-change build's work,
        // so a caller on a hot path (`build`) opts out of it and leaves the
        // partial-loss count to `status`.
        // The MATCH above reads the term index and the postings, and nothing
        // else. The rest of the index is its content and docsize tables, which
        // a damaged page can break without the probe's one symbol ever touching
        // them; a page-by-page scan of each is a fraction of the whole check's
        // per-symbol lookups. Both depths run it, so `status` and `build` can
        // never disagree about a page one of them can read.
        for shadow in ["nodes_fts_content", "nodes_fts_docsize"] {
            if let Err(error) =
                snapshot.query_row(&format!("SELECT COUNT(*) FROM {shadow}"), [], |row| {
                    row.get::<_, i64>(0)
                })
            {
                return unreadable(error);
            }
        }
        if !whole {
            return match (&probe, probe_hits) {
                (Some(name), Some(0)) => Ok(Some(fts_damage_reason(&format!(
                    "the full-text index holds generation {generation}'s rows but finds none \
                     of them: a search for its symbol {name:?} matched nothing"
                )))),
                _ => Ok(None),
            };
        }
        let memo = self
            .fts_reachable
            .lock()
            .ok()
            .and_then(|cache| *cache)
            .filter(|(cached, _)| *cached == generation)
            .map(|(_, reachable)| reachable);
        let reachable = match memo {
            Some(reachable) => reachable,
            None => {
                let counted = snapshot.query_row(
                    "SELECT COUNT(*)
                       FROM generations g
                       JOIN file_rows r
                         ON g.id >= r.valid_from AND (r.valid_to IS NULL OR g.id < r.valid_to)
                       JOIN payload_nodes n ON n.payload_id = r.payload_id
                       JOIN nodes_fts f ON f.rowid = ((n.payload_id << 24) | n.ord_in_file)
                      WHERE g.id = ?1",
                    params![generation],
                    |row| row.get::<_, i64>(0),
                );
                let reachable = match counted {
                    Ok(count) => usize::try_from(count).unwrap_or(0),
                    Err(error) => return unreadable(error),
                };
                if let Ok(mut cache) = self.fts_reachable.lock() {
                    *cache = Some((generation, reachable));
                }
                reachable
            }
        };
        if reachable < node_count {
            return Ok(Some(fts_damage_reason(&format!(
                "the full-text index reaches {reachable} of {node_count} symbols of \
                 generation {generation}, so searches answer with a subset and report it \
                 as the whole"
            ))));
        }
        if let (Some(name), Some(0)) = (&probe, probe_hits) {
            return Ok(Some(fts_damage_reason(&format!(
                "the full-text index holds generation {generation}'s rows but finds none \
                 of them: a search for its symbol {name:?} matched nothing"
            ))));
        }
        Ok(None)
    }

    /// Why the latest generation's search index cannot be trusted, if it cannot:
    /// the probe `status` and `build` share.
    ///
    /// One owner for the verdict, so the two surfaces cannot disagree about a
    /// store: `status` reads the same probe through [`Store::status`]'s
    /// `degraded_reason`, and `build` asks it directly before it decides that
    /// nothing changed. The answer is the reason text, which already names the
    /// remedy; `None` is "no generation" or "the index is readable and whole".
    pub fn search_index_damage(&self) -> Result<Option<String>> {
        self.search_index_damage_at(true)
    }

    /// [`Self::search_index_damage`] without the whole-index count: only whether
    /// the index can be read and finds the generation's own symbol.
    ///
    /// For a caller on a hot path. `devmap build` asks this before it decides
    /// nothing changed, and a no-change build is the common one; the count
    /// behind [`Self::search_index_damage`] walks every symbol through the
    /// index (measured: +15% instructions on an unchanged 1,893-file build).
    /// Both depths read every shadow table of the index page by page, so
    /// neither surface can miss a damaged page the other reports. What the cheap
    /// depth leaves to `status` is the *logical* loss: rows missing from an
    /// index that reads cleanly.
    pub fn search_index_unreadable(&self) -> Result<Option<String>> {
        self.search_index_damage_at(false)
    }

    fn search_index_damage_at(&self, whole: bool) -> Result<Option<String>> {
        let conn = lock_conn(&self.conn)?;
        let snapshot = conn.unchecked_transaction()?;
        let Some(generation) = Self::latest_generation_id_locked(&snapshot)? else {
            return Ok(None);
        };
        // The whole check compares against the symbol count; the readable probe
        // only needs to know whether there is a symbol to look for, which a
        // single index probe answers without counting every row.
        let node_count = if whole {
            self.generation_counts_locked(&snapshot, generation)?.0
        } else {
            snapshot.query_row(
                "SELECT EXISTS(SELECT 1 FROM generation_nodes WHERE generation_id = ?1)",
                params![generation],
                |row| row.get::<_, i64>(0).map(|found| found as usize),
            )?
        };
        self.fts_health_locked(&snapshot, generation, node_count, whole)
    }

    /// The latest generation's analysis **status**, without its summary.
    ///
    /// `devmap status` needs one enum to decide whether the graph is degraded,
    /// and reading it through [`Store::latest_analysis`] deserialises the whole
    /// `AnalysisSummary` to get there — every dead symbol, every community,
    /// every clone-coverage counter. On the ScholarLM corpus that blob is large
    /// enough to cost milliseconds on a surface whose whole budget is a few.
    ///
    /// SQLite's `->` operator returns a *JSON* representation rather than SQL
    /// text, so a unit variant comes back as `"Ok"` and a struct variant as its
    /// object, and both feed straight back into serde. That matters: the
    /// encoding of `AnalysisStatus` stays owned by its derive, and this method
    /// does not hand-decode variant names that a future variant would silently
    /// fall out of.
    ///
    /// Not the same reader as [`devmap_analyze::model::AnalysisDisclosure`],
    /// deliberately, but the difference is no longer where the bytes stop.
    /// When this was written the disclosure still transferred the whole blob
    /// and stepped serde over its vectors; `dead_page` now strips them in
    /// SQLite too, with `json_remove`, so neither reader carries the summary
    /// across. What remains is the shape of the question: the disclosure wants
    /// five fields inside a snapshot `dead_page` is already holding, and this
    /// wants one field on a surface a health check polls often enough to cache
    /// it per generation. Both decode `AnalysisStatus` through its own derive,
    /// so neither can drift from the writer.
    pub fn latest_analysis_status(&self) -> Result<Option<AnalysisStatus>> {
        let conn = lock_conn(&self.conn)?;
        // One snapshot for the generation id and the row it names, for the same
        // reason `status` takes one: resolving the newest generation and then
        // reading its analysis in two separate reads lets a prune between them
        // answer `None` for a store that holds a generation.
        let snapshot = conn.unchecked_transaction()?;
        let Some(generation) = Self::latest_generation_id_locked(&snapshot)? else {
            return Ok(None);
        };
        if let Ok(cache) = self.generation_analysis_status.lock() {
            if let Some((cached, status)) = cache.as_ref() {
                if *cached == generation {
                    return Ok(Some(status.clone()));
                }
            }
        }
        let raw: Option<Option<String>> = snapshot
            .query_row(
                "SELECT analysis_json -> '$.status' FROM generations WHERE id = ?1",
                params![generation],
                |row| row.get(0),
            )
            .optional()?;
        let Some(Some(json)) = raw else {
            return Ok(None);
        };
        let status: AnalysisStatus = serde_json::from_str(&json).map_err(|error| {
            refusal(format!(
                "stored generation analysis status is invalid: {error}"
            ))
        })?;
        if let Ok(mut cache) = self.generation_analysis_status.lock() {
            *cache = Some((generation, status.clone()));
        }
        Ok(Some(status))
    }

    /// Inspect store health and verify its snapshot against the current tree.
    /// A quiet queue alone says nothing about edits made without a watcher.
    pub fn status(&self, db_path: &str) -> Result<StoreStatus> {
        self.status_inner(db_path, None)
    }

    /// [`Self::status`] whose tree walk stops when `cancel` is set.
    ///
    /// A cancelled walk is an error (`devmap_extract::BuildCancelled`, found by
    /// downcast), never a status: the walk did not finish, so no verdict is
    /// remembered and the previous one is left to the pending-queue and
    /// generation guards. Used by the daemon's post-drain verification, which a
    /// shutdown must not wait for.
    pub fn status_cancellable(
        &self,
        db_path: &str,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<StoreStatus> {
        self.status_inner(db_path, Some(cancel))
    }

    fn status_inner(
        &self,
        db_path: &str,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<StoreStatus> {
        let mut status = self.status_snapshot(db_path)?;
        if let Some(generation) = status.latest_generation {
            if status.pending_count == 0 && status.degraded_reason.is_none() {
                // One lock for both analyzer questions: they read one snapshot.
                let (analyzer_freshness, analyzer_reason) = match lock_conn(&self.conn)
                    .and_then(|conn| {
                        Ok((
                            Self::extraction_is_current_on(&conn)?,
                            Self::resolver_stamp_on(&conn)?,
                        ))
                    }) {
                    Ok((true, ResolverStamp::Current)) => (Some(true), None),
                    // A resolver mismatch has its own reason, which
                    // `rebuild_required_reason` does not match: a re-resolve can
                    // outlast the SessionStart budget, so it lands on the next
                    // build instead of an automatic one.
                    Ok((true, stamp)) => (Some(false), Some(stamp.reason().to_string())),
                    Ok((false, _)) => (Some(false), Some(
                        "stored extraction payload is obsolete; rebuild with the current analyzer".to_string())),
                    Err(error) => (None, Some(format!("analyzer freshness unverified: {error}"))),
                };
                let (source_freshness, source_reason, source_delta) =
                    self.source_snapshot_mismatch(generation, cancel)?;
                status.source_freshness = source_freshness;
                status.source_delta = source_delta;
                status.analyzer_freshness = analyzer_freshness;
                status.degraded_reason =
                    devmap_analyze::combine_reasons(source_reason, analyzer_reason);
                // Both checks describe the same generation or neither may certify it.
                let (after_generation, after_pending) = self.generation_and_pending()?;
                if after_generation != Some(generation) || after_pending != 0 {
                    status.source_freshness = None;
                    status.source_delta = None;
                    status.analyzer_freshness = None;
                    status.degraded_reason = Some(
                        "index changed during freshness verification; retry status".to_string(),
                    );
                }
                self.remember_source_freshness(
                    generation,
                    status.source_freshness,
                    status.degraded_reason.clone(),
                );
            }
        }
        Ok(status)
    }

    /// What a query envelope should disclose about whole-tree freshness.
    ///
    /// Status is the surface that walks the tree. Queries attach the last
    /// verified verdict for the generation they answered from when this process
    /// has one, otherwise an explicit unverified reason — never a silent null.
    pub fn query_source_freshness(&self) -> QuerySourceFreshness {
        let latest = match self.latest_generation_id() {
            Ok(Some(id)) => id,
            Ok(None) => {
                return QuerySourceFreshness::unverified(
                    "no persisted generation is available to verify against the working tree",
                )
            }
            Err(error) => {
                return QuerySourceFreshness::unverified(format!(
                    "source freshness unverified: could not read the latest generation ({error})"
                ))
            }
        };
        // A remembered verdict describes the tree as `status` last walked it. Edits
        // the watcher has queued since then end that verdict without changing the
        // generation id, so a non-empty queue means the cache cannot speak.
        let queued: Result<i64> = self.queued_path_count();
        if let Some(blocked) = queue_blocks_cached_freshness(queued) {
            return blocked;
        }
        // Test seam. Production leaves the hook unset. It runs after the
        // queue read has dropped the connection and before the cache is
        // cloned, which is the window an enqueue can commit into.
        if let Some(hook) = FRESHNESS_INTERLEAVE.with(|slot| slot.get()) {
            let entered = FRESHNESS_INTERLEAVE_ACTIVE.with(|active| {
                if active.get() {
                    false
                } else {
                    active.set(true);
                    true
                }
            });
            if entered {
                let _guard = InterleaveGuard;
                hook(self);
            }
        }
        let cache = self
            .source_freshness_cache
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        // The enqueue that landed after the first count is visible now. A
        // generation-matched cache is returned only when this count is still
        // zero. A read error is unverified, not a cached fresh.
        if let Some(blocked) = queue_blocks_cached_freshness(self.queued_path_count()) {
            return blocked;
        }
        match cache {
            Some(cached) if cached.generation_id == latest => QuerySourceFreshness {
                fresh: cached.fresh,
                generation_id: Some(cached.generation_id),
                reason: cached.reason,
            },
            Some(cached) => QuerySourceFreshness::unverified(format!(
                "cached source freshness described generation {}, but this answer is from \
generation {latest}; run `devmap status` to re-verify",
                cached.generation_id
            )),
            None => QuerySourceFreshness::unverified(
                "whole-tree source freshness was not checked for this answer; run `devmap status` \
(or call `devmap_status`) for a verified verdict",
            ),
        }
    }

    pub(super) fn clear_source_freshness_cache(&self) {
        if let Ok(mut cache) = self.source_freshness_cache.lock() {
            *cache = None;
        }
    }

    fn remember_source_freshness(
        &self,
        generation_id: u32,
        fresh: Option<bool>,
        reason: Option<String>,
    ) {
        if let Ok(mut cache) = self.source_freshness_cache.lock() {
            *cache = Some(CachedSourceFreshness {
                generation_id,
                fresh,
                reason,
            });
        }
    }

    /// Runs without holding the SQLite connection during filesystem I/O. The
    /// generation is checked again afterwards, so a writer cannot combine a
    /// newer inventory with the older status snapshot and certify it as fresh.
    fn source_snapshot_mismatch(
        &self,
        generation: u32,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(Option<bool>, Option<String>, Option<SourceTreeDelta>)> {
        let Some(root) = self.latest_repo_root()? else {
            return Ok((
                None,
                Some(
                    "source freshness unverified: this generation has no repository root"
                        .to_string(),
                ),
                None,
            ));
        };
        let hashes = self.latest_file_hashes_shared()?;
        let stored = self.latest_discovery_inventory()?;
        let scan = match cancel {
            Some(flag) => devmap_extract::scan_tree_cancellable(Path::new(&root), flag),
            None => devmap_extract::scan_tree(Path::new(&root)),
        };
        let scanned = match scan {
            Ok(scanned) => scanned,
            // A walk the caller stopped is not a walk that found a problem:
            // it carries no verdict at all, so it is an error and not an
            // "unverified" status that would be remembered for the generation.
            Err(error) if error.is::<devmap_extract::BuildCancelled>() => {
                return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                    devmap_extract::BuildCancelled,
                )))
            }
            Err(error) => {
                return Ok((
                    None,
                    Some(format!("source freshness unverified: {error}")),
                    None,
                ))
            }
        };
        let (delta, sample_paths) =
            scanned.file_delta_with_samples(&hashes, SourceTreeDelta::SAMPLE);
        if !delta.is_unchanged() {
            return Ok((
                Some(false),
                Some(
                    "source tree differs from the indexed generation; rebuild or drain watcher edits"
                        .to_string(),
                ),
                Some(SourceTreeDelta {
                    added: delta.added,
                    changed: delta.changed,
                    removed: delta.removed,
                    over_limit: delta.over_limit,
                    sample_paths,
                }),
            ));
        }
        // The manifests too: the stored inventory holds the ones the resolver
        // could not read, so a comparison against discovery alone would call
        // every such generation stale, and miss a manifest that broke or was
        // repaired while no source moved.
        let manifests = match devmap_extract::collect_project_manifests(Path::new(&root)) {
            Ok(manifests) => manifests,
            Err(error) => {
                return Ok((
                    None,
                    Some(format!(
                        "source freshness unverified: project manifests could not be read ({error})"
                    )),
                    None,
                ))
            }
        };
        let current = crate::DiscoveryInventory::of(&scanned.report, &manifests);
        if current.refusals != stored.refusals {
            return Ok((
                Some(false),
                Some(
                    "source discovery refusals differ from the indexed generation; rebuild required"
                        .to_string(),
                ),
                None,
            ));
        }
        // A file over the limit that the generation never held — created
        // that size, or grown past it and since rebuilt around — leaves the
        // hashes equal, so only this comparison sees it. Reported as the
        // over-limit delta it is, with the paths, rather than as a refusal.
        if current.over_limit != stored.over_limit {
            let differing: std::collections::BTreeSet<&str> = current
                .over_limit
                .iter()
                .filter(|row| !stored.over_limit.contains(row))
                .chain(
                    stored
                        .over_limit
                        .iter()
                        .filter(|row| !current.over_limit.contains(row)),
                )
                .map(|row| row.path.as_str())
                .collect();
            return Ok((
                Some(false),
                Some(
                    "source files over the size limit differ from the indexed generation; \
                     rebuild or drain watcher edits"
                        .to_string(),
                ),
                Some(SourceTreeDelta {
                    added: 0,
                    changed: 0,
                    removed: 0,
                    over_limit: differing.len(),
                    sample_paths: differing
                        .into_iter()
                        .take(SourceTreeDelta::SAMPLE)
                        .map(str::to_string)
                        .collect(),
                }),
            ));
        }
        // HEAD is provenance: an identical tree remains current after an empty
        // commit. A concurrent generation or pending edit invalidates the proof.
        let (after_generation, after_pending) = self.generation_and_pending()?;
        if after_generation != Some(generation) || after_pending != 0 {
            return Ok((
                None,
                Some("index changed during source verification; retry status".to_string()),
                None,
            ));
        }
        Ok((Some(true), None, None))
    }

    /// The latest generation and the pending-edit count, in one snapshot.
    ///
    /// The two re-checks `status` makes after its freshness walk compare
    /// exactly these two fields to the ones it started from. They took a whole
    /// [`Self::status_snapshot`] to do it, which also re-ran the full-text
    /// health probe, the coverage-gap reads and the payload-encoding count
    /// (the last one reads past every payload's blob: ~3 ms per snapshot on a
    /// 16 MiB page cache, and three snapshots per `status`). Those describe the
    /// store, not whether it moved during the walk, and the first snapshot has
    /// already reported them.
    fn generation_and_pending(&self) -> Result<(Option<u32>, usize)> {
        let conn = lock_conn(&self.conn)?;
        let snapshot = conn.unchecked_transaction()?;
        let latest = Self::latest_generation_id_locked(&snapshot)?;
        let pending: usize =
            snapshot.query_row("SELECT COUNT(*) FROM pending_paths", [], |row| {
                row.get::<_, i64>(0).map(|n| n as usize)
            })?;
        Ok((latest, pending))
    }

    fn status_snapshot(&self, db_path: &str) -> Result<StoreStatus> {
        #[cfg(test)]
        STATUS_SNAPSHOTS.with(|taken| taken.set(taken.get() + 1));
        let conn = lock_conn(&self.conn)?;
        // Read before the snapshot, so a commit by another connection after this
        // point can only make a held answer look older than it is.
        let stamp = Self::connection_stamp(&conn)?;
        // Every number below describes one instant. `status` resolves the
        // latest generation and then counts that generation's nodes and
        // edges in separate statements: without a snapshot those are
        // separate reads, so a second process pruning between them reported
        // a live generation holding zero symbols. See `latest_snapshot`.
        let snapshot = conn.unchecked_transaction()?;
        let latest: Option<u32> = snapshot
            .query_row(
                "SELECT id FROM generations ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let pending_count: usize =
            snapshot.query_row("SELECT COUNT(*) FROM pending_paths", [], |row| {
                row.get::<_, i64>(0).map(|n| n as usize)
            })?;
        let (node_count, edge_count) = if let Some(g) = latest {
            // Counted at most once per generation. The two `COUNT(*)`s still
            // run inside the snapshot the first time, so the pair a caller sees
            // is still one instant's; what the memo removes is re-counting a
            // generation whose rows cannot change (measured on a 271k-edge
            // store: 2.87 ms of a 2.9 ms `status`).
            self.generation_counts_locked(&snapshot, g)?
        } else {
            // No generation, nothing to count. Not a cached zero — there are
            // genuinely no rows to describe.
            (0, 0)
        };
        let quarantined_count: usize = snapshot.query_row(
            "SELECT COUNT(*) FROM pending_paths WHERE attempts >= ?1",
            params![MAX_PENDING_ATTEMPTS],
            |row| row.get::<_, i64>(0).map(|count| count as usize),
        )?;
        let quarantined_paths: Vec<String> = {
            let mut stmt = snapshot.prepare(
                "SELECT path FROM pending_paths
                 WHERE attempts >= ?1
                 ORDER BY revision ASC, path ASC
                 LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(
                    params![MAX_PENDING_ATTEMPTS, Self::DEGRADED_SAMPLE as i64],
                    |row| row.get(0),
                )?
                .collect::<Result<Vec<_>>>()?;
            rows
        };
        // Three primary-key range scans over a table whose rows are the
        // exception rather than the rule — on this repository, four rows. The
        // alternative, deriving the two extraction gaps from
        // `generation_files.parse_outcome_json` at read time, has to walk past
        // a ~47 KB `extraction_json` on every row of the generation to reach
        // three small columns; that is the scan the v13 index exists to avoid,
        // and `status` is a surface a health check polls.
        let coverage_gaps = match latest {
            Some(generation) => Self::coverage_gaps_locked(&snapshot, generation)?,
            // No generation, nothing to describe. Empty here means "there is no
            // generation", which `latest_generation: None` already says; it is
            // not a claim that a generation read everything.
            None => CoverageGaps::default(),
        };
        // Inside the same snapshot as the node count it is compared against.
        let fts_reason = match latest {
            Some(generation) => self.fts_health_locked(&snapshot, generation, node_count, true)?,
            None => None,
        };
        let quarantine_reason = if quarantined_count > 0 {
            // Name the paths. See `StoreStatus::quarantined_paths`: the
            // count alone made a permanently degraded store undiagnosable
            // without opening the database by hand.
            let shown = quarantined_paths.join(", ");
            let elided = quarantined_count.saturating_sub(quarantined_paths.len());
            Some(if elided > 0 {
                format!(
                    "{quarantined_count} path(s) exceeded the retry threshold \
                         (attempts >= {MAX_PENDING_ATTEMPTS}): {shown}, and {elided} more \
                         — `devmap repair --pending` drops them"
                )
            } else {
                format!(
                    "{quarantined_count} path(s) exceeded the retry threshold \
                         (attempts >= {MAX_PENDING_ATTEMPTS}): {shown} \
                         — `devmap repair --pending` drops them"
                )
            })
        } else {
            None
        };
        let encoding_reason = self.payload_encoding_reason_cached(&snapshot, latest, stamp)?;
        Ok(StoreStatus {
            db_path: db_path.to_string(),
            latest_generation: latest,
            pending_count,
            node_count,
            edge_count,
            source_freshness: None,
            source_delta: None,
            analyzer_freshness: None,
            degraded_reason: devmap_analyze::combine_reasons(
                devmap_analyze::combine_reasons(quarantine_reason, fts_reason),
                encoding_reason,
            ),
            quarantined_count,
            quarantined_paths,
            coverage_gaps,
        })
    }

    /// [`Self::payload_encoding_reason_locked`], with a clean answer held.
    ///
    /// The count behind it reads `entry_wiring_json`, which sits behind each
    /// row's `extraction_json` blob, so a small page cache walks every payload's
    /// overflow chain to answer it: about 2 ms more per call on a 16 MiB cache
    /// than on a 64 MiB one, on this repository. Only a clean answer is held,
    /// per (generation, stamp, age): a row any connection adds or
    /// breaks ends it, and a fault is always re-read because another
    /// process's writer open can heal it.
    fn payload_encoding_reason_cached(
        &self,
        snapshot: &rusqlite::Transaction<'_>,
        generation: Option<u32>,
        stamp: ConnectionStamp,
    ) -> Result<Option<String>> {
        if let (Some(generation), Ok(checks)) = (generation, self.generation_checks.lock()) {
            if Keyed::holds(&checks.payload_encoding_clean, generation, stamp).is_some() {
                return Ok(None);
            }
        }
        let reason = Self::payload_encoding_reason_locked(snapshot)?;
        if let (None, Some(generation), Ok(mut checks)) =
            (&reason, generation, self.generation_checks.lock())
        {
            checks.payload_encoding_clean = Some(Keyed::new(generation, stamp, ()));
        }
        Ok(reason)
    }

    /// This connection's [`ConnectionStamp`] now.
    ///
    /// Read before the rows a held answer is computed from, so a commit that
    /// lands in between can only make the answer look older than it is.
    fn connection_stamp(conn: &rusqlite::Connection) -> Result<ConnectionStamp> {
        Ok(ConnectionStamp {
            data_version: conn.query_row("PRAGMA data_version", [], |row| row.get(0))?,
            changes: conn.total_changes(),
        })
    }

    /// How many unconverted payloads one status examines. Each examination
    /// decodes a document, and a healthy store has none to examine.
    const ENCODING_EXAMINED: usize = 64;

    /// The stored extractions a writer open could not convert, by path, or
    /// `None` when there are none.
    ///
    /// A writer open converts what it can and leaves the rest as it found it
    /// ([`Store::heal_extraction_encoding`]), and a reader refuses such a row by
    /// name when it reaches it. Neither says so at open, and a store that opened
    /// cleanly with one unreadable payload looked exactly like one with none, so
    /// status reports it. It asks the store rather than remembering the open, so
    /// it also describes a store only ever opened read-only, by running the
    /// heal's own test on the rows the heal would pick: a row not yet converted
    /// but readable (a foreign writer's text document) is not a fault and is not
    /// reported; one that does not decode is. At most
    /// [`Self::ENCODING_EXAMINED`] rows are examined, and when more remain the
    /// reason says so instead of reading the rest as clean.
    ///
    /// **What it does not look at, and says so.** Only the rows the heal would
    /// pick: a compressed frame that already has its entry wiring is never
    /// examined, so one truncated or bit-rotted there is found when a reader
    /// decompresses it, not here. A structural check of every frame's header was
    /// built and measured (the leading bytes of 1,896 rows, no decompression):
    /// status went from 1.9 ms to 5.8 ms, min of 14 per process, n=7 alternating,
    /// because SQLite loads each blob to take its prefix. `status` runs on every
    /// answer, so that was not taken; the reason names the scope instead.
    fn payload_encoding_reason_locked(
        snapshot: &rusqlite::Transaction<'_>,
    ) -> Result<Option<String>> {
        use crate::payload_codec::{decode_value, entry_wiring_of_document};
        let total: i64 = snapshot.query_row(
            "SELECT COUNT(*) FROM file_payloads
              WHERE typeof(extraction_json) <> 'blob' OR entry_wiring_json IS NULL",
            [],
            |row| row.get(0),
        )?;
        if total == 0 {
            return Ok(None);
        }
        let mut stmt = snapshot.prepare(
            "SELECT pa.path, fp.extraction_json
               FROM file_payloads fp JOIN paths pa ON pa.id = fp.file_id
              WHERE typeof(fp.extraction_json) <> 'blob' OR fp.entry_wiring_json IS NULL
              ORDER BY fp.payload_id LIMIT ?1",
        )?;
        let examined = stmt
            .query_map(params![Self::ENCODING_EXAMINED as i64], |row| {
                let path: String = row.get(0)?;
                let verdict = decode_value(row.get_ref(1)?, &path)
                    .and_then(|json| entry_wiring_of_document(&json, &path))
                    .is_ok();
                Ok((path, verdict))
            })?
            .collect::<Result<Vec<_>>>()?;
        let failed: Vec<&str> = examined
            .iter()
            .filter(|(_, convertible)| !convertible)
            .map(|(path, _)| path.as_str())
            .collect();
        let unexamined = usize::try_from(total)
            .unwrap_or(usize::MAX)
            .saturating_sub(examined.len());
        if failed.is_empty() && unexamined == 0 {
            return Ok(None);
        }
        let mut reason = String::new();
        if !failed.is_empty() {
            let shown: Vec<&str> = failed.iter().copied().take(Self::DEGRADED_SAMPLE).collect();
            let more = failed.len().saturating_sub(shown.len());
            reason.push_str(&format!(
                "{} stored extraction(s) cannot be converted to the schema-29 form (compressed, \
                 with entry wiring): {}{}; they are refused by name when read",
                failed.len(),
                shown.join(", "),
                if more > 0 {
                    format!(", and {more} more")
                } else {
                    String::new()
                }
            ));
        }
        if unexamined > 0 {
            if !reason.is_empty() {
                reason.push_str("; ");
            }
            reason.push_str(&format!(
                "{unexamined} more extraction(s) are not in the schema-29 form and were not \
                 examined (the first {} were), so they are not known to be readable",
                examined.len()
            ));
        }
        reason.push_str(
            "; this covers only payloads a writer open would convert (text, or without entry \
             wiring): a compressed frame that already has its entry wiring is not examined, and \
             a damaged one is found only when a reader decompresses it",
        );
        reason.push_str(" -- move or delete the store and run `devmap build`");
        Ok(Some(reason))
    }

    /// The latest generation's coverage-gap inventory, capped per kind.
    ///
    /// Takes the caller's snapshot for the same reason
    /// [`Store::generation_counts_locked`] does: the generation id and the rows
    /// it names have to come from one instant, or a prune between them reports
    /// a live generation with no gaps.
    fn coverage_gaps_locked(
        snapshot: &rusqlite::Transaction<'_>,
        generation: u32,
    ) -> Result<CoverageGaps> {
        let mut gaps = CoverageGaps::default();
        let mut count = snapshot.prepare(
            "SELECT COUNT(*) FROM generation_coverage_gaps
             WHERE generation_id = ?1 AND gap = ?2",
        )?;
        let mut page = snapshot.prepare(
            "SELECT path, reason FROM generation_coverage_gaps
             WHERE generation_id = ?1 AND gap = ?2
             ORDER BY path ASC
             LIMIT ?3",
        )?;
        for label in CoverageGaps::labels() {
            let total: usize = count.query_row(params![generation, label], |row| {
                row.get::<_, i64>(0).map(|total| total as usize)
            })?;
            // Partial parses are listed worst first, so the cap drops the
            // harmless rows. That order needs every row's reason, so this kind
            // is read whole and cut after sorting; every other kind is cut by
            // path in SQL.
            let by_severity = label == devmap_analyze::ExtractionGap::PartialParse.label();
            let limit = if by_severity {
                i64::MAX
            } else {
                crate::COVERAGE_GAP_SAMPLE as i64
            };
            let mut shown: Vec<CoverageGapRow> = page
                .query_map(params![generation, label, limit], |row| {
                    Ok(CoverageGapRow {
                        path: row.get(0)?,
                        reason: row.get(1)?,
                    })
                })?
                .collect::<Result<Vec<_>>>()?;
            if by_severity {
                // Stable, so rows of equal severity stay in path order.
                shown.sort_by_key(|row| {
                    let (charged, lost) = devmap_analyze::partial_parse_severity(&row.reason);
                    (std::cmp::Reverse(charged), std::cmp::Reverse(lost))
                });
                shown.truncate(crate::COVERAGE_GAP_SAMPLE);
            }
            let slot = gaps.slot(label).expect("every label has a slot");
            *slot = CoverageGapSample { total, shown };
        }
        Ok(gaps)
    }

    /// Every path the latest generation's discovery refused, with its verdict.
    ///
    /// The whole inventory, uncapped: the daemon's drain carries it forward
    /// minus the paths this batch re-decided, and a capped read would silently
    /// drop verdicts on every drain until the corpus looked clean.
    pub fn latest_discovery_refusals(&self) -> Result<Vec<DiscoveryRefusal>> {
        Ok(self.latest_discovery_inventory()?.refusals)
    }

    /// The latest generation's whole [`crate::DiscoveryInventory`] — refusals
    /// and over-limit files — read from one snapshot.
    ///
    /// Uncapped, for the reason [`Self::latest_discovery_refusals`] gives, and
    /// one read for both halves so a writer landing between two reads cannot
    /// pair one generation's refusals with another's over-limit rows.
    pub fn latest_discovery_inventory(&self) -> Result<crate::DiscoveryInventory> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(crate::DiscoveryInventory::default());
        };
        let mut stmt = snapshot.prepare(
            "SELECT path, reason FROM generation_coverage_gaps
             WHERE generation_id = ?1 AND gap = ?2
             ORDER BY path ASC",
        )?;
        let mut read = |gap: &str| {
            stmt.query_map(params![generation, gap], |row| {
                Ok(DiscoveryRefusal {
                    path: row.get(0)?,
                    reason: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>>>()
        };
        let inventory = crate::DiscoveryInventory {
            refusals: read(crate::GAP_DISCOVERY_REFUSED)?,
            over_limit: read(crate::GAP_OVER_LIMIT)?,
        };
        drop(stmt);
        drop(snapshot);
        Ok(inventory)
    }

    /// The partial-parse rows of the latest generation whose reason contains
    /// `containing`, by path: at most `limit` of them, and the true count.
    ///
    /// The same rows `status` lists under `coverage_gaps.partial_parse`, but
    /// not sampled at fifty by path: a name lookup has to say how many it did
    /// not look at, and the file holding the hole may be the fifty-first.
    /// `admit` narrows to a caller's scope; `None` admits every row and is
    /// counted in SQL, so an unscoped lookup pays for `limit` rows and one
    /// `COUNT`, not for a walk over the whole inventory. Each row carries the
    /// language and the content hash the generation recorded for the file, so a
    /// reader can tell a file edited since the build from the one that was
    /// parsed.
    pub fn latest_partial_parse_gaps(
        &self,
        containing: &str,
        admit: Option<&dyn Fn(&str) -> bool>,
        limit: usize,
    ) -> Result<(Vec<crate::PartialParseGap>, usize)> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok((Vec::new(), 0));
        };
        let label = devmap_analyze::ExtractionGap::PartialParse.label();
        let filter = "g.generation_id = ?1 AND g.gap = ?2 AND instr(g.reason, ?3) > 0";
        let total_unscoped: Option<usize> = match admit {
            None => Some(snapshot.query_row(
                &format!("SELECT COUNT(*) FROM generation_coverage_gaps g WHERE {filter}"),
                params![generation, label, containing],
                |row| row.get::<_, i64>(0).map(|count| count as usize),
            )?),
            Some(_) => None,
        };
        let mut stmt = snapshot.prepare(&format!(
            "SELECT g.path, g.reason, f.language, f.content_hash
             FROM generation_coverage_gaps g
             JOIN paths p ON p.path = g.path
             JOIN generation_files f ON f.file_id = p.id AND f.generation_id = g.generation_id
             WHERE {filter}
             ORDER BY g.path ASC"
        ))?;
        let mut rows = stmt.query(params![generation, label, containing])?;
        let mut kept = Vec::new();
        let mut seen = 0usize;
        while let Some(row) = rows.next()? {
            let admitted = match admit {
                Some(admit) => admit(row.get_ref(0)?.as_str()?),
                None => true,
            };
            if !admitted {
                continue;
            }
            seen += 1;
            if kept.len() < limit {
                kept.push(crate::PartialParseGap {
                    path: row.get(0)?,
                    reason: row.get(1)?,
                    language: row.get(2)?,
                    content_hash: row.get::<_, i64>(3)? as u64,
                });
            } else if total_unscoped.is_some() {
                break;
            }
        }
        drop(rows);
        drop(stmt);
        drop(snapshot);
        Ok((kept, total_unscoped.unwrap_or(seen)))
    }

    /// Whether the latest generation's edges carry the resolution the resolver
    /// recorded, or a reconstruction standing in for one it never stored.
    ///
    /// One row answers for the generation, and that is a property rather than a
    /// sample: `save_generation` writes every edge of a generation in a single
    /// transaction from one `resolution.edges`, and edges are never carried
    /// forward from an older generation (see the comment above the edge loop).
    /// So the column is present for all of a generation's edges or for none of
    /// them. `None` when there is no generation, or when it holds no edges —
    /// which is "nothing to say", not "reconstructed".
    pub fn latest_edge_resolution_source(&self) -> Result<Option<ResolutionSource>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        let stored: Option<Option<String>> = snapshot
            .query_row(
                "SELECT resolution FROM generation_edges
                 WHERE generation_id = ?1 ORDER BY ordinal LIMIT 1",
                params![generation],
                |row| row.get(0),
            )
            .optional()?;
        Ok(stored.map(|resolution| match resolution {
            Some(_) => ResolutionSource::Stored,
            None => ResolutionSource::Reconstructed,
        }))
    }

    /// The build-time [`devmap_analyze::ResolutionRate`] persisted on the
    /// latest generation, if any.
    ///
    /// Status and the MCP `devmap_status` tool carry this rather than
    /// recomputing it: the unresolved ledger that forms the denominator is not
    /// kept on the generation, so a later reader cannot reconstruct the rate
    /// from edges alone. Extracted with `json_extract` so the dead-symbol and
    /// community arrays never cross into this process.
    ///
    /// `None` when there is no generation, or when the summary was written
    /// before `resolution_rate` existed. Absence is **not measured**, not a
    /// rate of zero — the same rule `Permille = Option` and the build readout
    /// already enforce. A malformed blob is an error, not an invented default.
    pub fn latest_resolution_rate(&self) -> Result<Option<devmap_analyze::ResolutionRate>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        let raw: Option<String> = snapshot
            .query_row(
                "SELECT json_extract(analysis_json, '$.resolution_rate')
                 FROM generations WHERE id = ?1",
                params![generation],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let Some(raw) = raw else {
            return Ok(None);
        };
        serde_json::from_str::<devmap_analyze::ResolutionRate>(&raw)
            .map(Some)
            .map_err(|error| {
                refusal(format!(
                    "stored generation resolution_rate is invalid: {error}"
                ))
            })
    }

    /// Files of the latest generation that were refused for the wall-clock
    /// parse budget, after the build's serial retry.
    ///
    /// The parse budget is a fact about the machine's load as much as about the
    /// file, and `ParseOutcome::Failed` is the only place it is recorded (a new
    /// variant would be a schema change), so `coverage_gaps.parse_failed` lists
    /// these beside grammar faults even though only these are cured by building
    /// again. Read from the generation's `parse_failed` coverage-gap rows by the
    /// reason's [`devmap_extract::model::BUDGET_REFUSAL_PREFIX`] — the same test
    /// `Extraction::is_budget_refusal` applies. Those rows are a per-generation
    /// snapshot of the live extraction, **not** `generation_files`, which is a
    /// view over `file_payloads` rows shared by every generation naming the same
    /// file, content and extractor: reading the payload would report whatever the
    /// last writer left there rather than what this generation was built from.
    /// One snapshot with the generation id, so a file deleted or re-parsed since
    /// is simply absent.
    /// The listing is capped at [`crate::COVERAGE_GAP_SAMPLE`] and carries its
    /// total. `None` when the store holds no generation: nothing was measured.
    pub fn latest_budget_refusals(&self) -> Result<Option<CoverageGapSample>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        let prefix = devmap_extract::model::BUDGET_REFUSAL_PREFIX;
        let total: i64 = snapshot.query_row(
            &format!("SELECT COUNT(*) FROM generation_coverage_gaps WHERE {BUDGET_GAP_FILTER}"),
            params![
                generation,
                parse_failed_gap(),
                prefix.chars().count() as i64,
                prefix
            ],
            |row| row.get(0),
        )?;
        let mut page = snapshot.prepare(&format!(
            "SELECT path, reason FROM generation_coverage_gaps
             WHERE {BUDGET_GAP_FILTER}
             ORDER BY path ASC
             LIMIT ?5"
        ))?;
        let shown = page
            .query_map(
                params![
                    generation,
                    parse_failed_gap(),
                    prefix.chars().count() as i64,
                    prefix,
                    crate::COVERAGE_GAP_SAMPLE as i64
                ],
                |row| {
                    Ok(CoverageGapRow {
                        path: row.get(0)?,
                        reason: row.get(1)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(CoverageGapSample {
            total: total.max(0) as usize,
            shown,
        }))
    }

    /// The latest generation's resolution-budget gap rows (`resolution_budget`),
    /// one per exceeded budget, by path (`resolve:<budget>`). Empty when the
    /// generation was not capped or the store holds none.
    ///
    /// For the build that proves nothing changed: it keeps the generation it
    /// finds, so it must say that generation is capped, as it restates what
    /// discovery refused. At most one row per budget, so there is no cap to
    /// carry.
    pub fn latest_resolution_budget_gaps(&self) -> Result<Vec<CoverageGapRow>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(Vec::new());
        };
        let mut page = snapshot.prepare(
            "SELECT path, reason FROM generation_coverage_gaps
             WHERE generation_id = ?1 AND gap = ?2
             ORDER BY path ASC",
        )?;
        let rows = page
            .query_map(
                params![generation, crate::coverage::GAP_RESOLUTION_BUDGET],
                |row| {
                    Ok(CoverageGapRow {
                        path: row.get(0)?,
                        reason: row.get(1)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every path [`Self::latest_budget_refusals`] counts, uncapped.
    ///
    /// For the build, which must re-extract all of them: a capped read would
    /// leave the files past fifty refused for good. Bounded by the corpus.
    pub fn latest_budget_refused_paths(&self) -> Result<Vec<String>> {
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(Vec::new());
        };
        let prefix = devmap_extract::model::BUDGET_REFUSAL_PREFIX;
        let mut stmt = snapshot.prepare(&format!(
            "SELECT path FROM generation_coverage_gaps
             WHERE {BUDGET_GAP_FILTER}
             ORDER BY path ASC"
        ))?;
        let rows = stmt.query_map(
            params![
                generation,
                parse_failed_gap(),
                prefix.chars().count() as i64,
                prefix
            ],
            |row| row.get::<_, String>(0),
        )?;
        rows.collect()
    }

    /// Files of the latest generation that failed to parse.
    ///
    /// Grammar faults and budget refusals both: every `parse_failed`
    /// coverage-gap row, not only those whose reason starts with
    /// [`devmap_extract::model::BUDGET_REFUSAL_PREFIX`]. The count is the
    /// whole set. The paths are the first eight in path order, so an impact
    /// or dead answer can name the hole without loading every path. The
    /// caller states how many were not listed.
    ///
    /// `(0, [])` when the store holds no generation. A failed read is an
    /// error, never that empty pair.
    pub fn latest_parse_failures(&self) -> Result<(usize, Vec<String>)> {
        const LISTED: i64 = 8;
        let conn = lock_conn(&self.conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok((0, Vec::new()));
        };
        let total: i64 = snapshot.query_row(
            "SELECT COUNT(*) FROM generation_coverage_gaps
             WHERE generation_id = ?1 AND gap = ?2",
            params![generation, parse_failed_gap()],
            |row| row.get(0),
        )?;
        let mut page = snapshot.prepare(
            "SELECT path FROM generation_coverage_gaps
             WHERE generation_id = ?1 AND gap = ?2
             ORDER BY path ASC
             LIMIT ?3",
        )?;
        let paths = page
            .query_map(params![generation, parse_failed_gap(), LISTED], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((total.max(0) as usize, paths))
    }

    /// Stored edges of the latest generation whose confidence contradicts the
    /// resolution kind recorded for them, counted in SQL.
    ///
    /// The same check `GenerationEdges` makes at index-build time, for a
    /// process that holds no index — `devmap status` is a fresh process per
    /// call and must not build a 271k-edge index to answer one number. The
    /// `CASE` table is generated from `ResolutionKind::ALL` so this cannot hold
    /// a second copy of the confidence ladder; a spelling the enum does not
    /// know falls to `-1` and counts as a mismatch, which is the honest reading
    /// of a kind this binary cannot vouch for. Rows without the column are not
    /// judged: a reconstruction cannot convict the row. `None` when there is no
    /// generation.
    pub fn edge_confidence_mismatches(&self) -> Result<Option<usize>> {
        use devmap_resolve::model::ResolutionKind;
        let conn = lock_conn(&self.conn)?;
        let stamp = Self::connection_stamp(&conn)?;
        let Some((snapshot, generation)) = Self::latest_snapshot(&conn)? else {
            return Ok(None);
        };
        if let Ok(checks) = self.generation_checks.lock() {
            if let Some(count) = Keyed::holds(&checks.edge_confidence_mismatches, generation, stamp)
            {
                return Ok(Some(*count));
            }
        }
        let ladder: String = ResolutionKind::ALL
            .iter()
            .map(|kind| {
                format!(
                    " WHEN '{}' THEN {}",
                    kind.label(),
                    kind.confidence().to_millis()
                )
            })
            .collect();
        let sql = format!(
            "SELECT COUNT(*) FROM generation_edges
             WHERE generation_id = ?1 AND resolution IS NOT NULL
               AND CAST(ROUND(confidence * 1000) AS INTEGER) != CASE resolution{ladder} ELSE -1 END"
        );
        let count: i64 = snapshot.query_row(&sql, params![generation], |row| row.get(0))?;
        let count = count.max(0) as usize;
        if let Ok(mut checks) = self.generation_checks.lock() {
            checks.edge_confidence_mismatches = Some(Keyed::new(generation, stamp, count));
        }
        Ok(Some(count))
    }
}

#[cfg(test)]
mod freshness_race {
    use super::*;

    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            set_source_freshness_interleave(None);
        }
    }

    fn seeded() -> Store {
        let store = Store::open_in_memory().expect("store");
        {
            let conn = lock_conn(&store.conn).expect("connection");
            conn.execute(
                "INSERT INTO generations (id, created_at, head_sha, analysis_json)
                 VALUES (7, 1.0, 'seed', '{}')",
                [],
            )
            .expect("generation");
        }
        let id = store
            .latest_generation_id()
            .expect("read")
            .expect("generation id");
        store.remember_source_freshness(id, Some(true), None);
        store
    }

    fn enqueue_one(store: &Store) {
        store
            .enqueue_pending_paths(&["src/名前.rs".to_string()])
            .expect("enqueue");
    }

    #[test]
    fn an_enqueue_between_the_zero_count_and_the_cache_clone_is_not_fresh() {
        let store = seeded();
        let quiet = store.query_source_freshness();
        assert_eq!(
            quiet.fresh,
            Some(true),
            "a seeded empty queue is fresh before the race"
        );

        let _clear = ClearHook;
        set_source_freshness_interleave(Some(enqueue_one));
        let verdict = store.query_source_freshness();
        assert_eq!(
            verdict.fresh, None,
            "a committed enqueue must not stay fresh: {verdict:?}"
        );
        let reason = verdict.reason.expect("reason");
        assert!(
            reason.contains("queued"),
            "the reason must name the queue, got {reason}"
        );
        assert!(
            !reason.contains("index moved"),
            "this race is not an index move, got {reason}"
        );
        let again = store.query_source_freshness();
        assert_eq!(again.fresh, None);
        assert!(again.reason.unwrap_or_default().contains("queued"));
    }

    #[test]
    fn an_empty_enqueue_keeps_the_verified_verdict() {
        let store = seeded();
        store.enqueue_pending_paths(&[]).expect("empty enqueue");
        assert_eq!(store.query_source_freshness().fresh, Some(true));
    }

    #[test]
    fn an_enqueue_under_root_clears_the_cache() {
        struct DirGuard(std::path::PathBuf);
        impl Drop for DirGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let store = seeded();
        let root =
            std::env::temp_dir().join(format!("devmap-fresh-under-root-{}", std::process::id()));
        let _dir = DirGuard(root.clone());
        let rel = "src/odd 'quote' 名前.rs";
        std::fs::create_dir_all(root.join("src")).expect("src");
        std::fs::write(root.join(rel), b"fn x() {}\n").expect("file");
        let report = store
            .enqueue_pending_paths_under_root(&root, &[rel.to_string()])
            .expect("enqueue under root");
        assert!(
            report.enqueued.iter().any(|path| path.contains('名')),
            "the canonical queue entry must keep the unicode name: {report:?}"
        );
        {
            let conn = lock_conn(&store.conn).expect("connection");
            conn.execute("DELETE FROM pending_paths", [])
                .expect("delete");
        }
        let verdict = store.query_source_freshness();
        assert_eq!(verdict.fresh, None, "{verdict:?}");
        let reason = verdict.reason.expect("reason");
        assert!(reason.contains("not checked"), "{reason}");
        assert!(!reason.contains("index moved"), "{reason}");
    }

    #[test]
    fn an_enqueue_clears_the_cache_so_deleting_the_row_does_not_restore_fresh() {
        let store = seeded();
        store
            .enqueue_pending_paths(&["src/odd 'quote' 名前.rs".to_string()])
            .expect("enqueue");
        {
            let conn = lock_conn(&store.conn).expect("connection");
            conn.execute("DELETE FROM pending_paths", [])
                .expect("delete");
        }
        let verdict = store.query_source_freshness();
        assert_eq!(verdict.fresh, None, "{verdict:?}");
        let reason = verdict.reason.expect("reason");
        assert!(reason.contains("not checked"), "{reason}");
        assert!(!reason.contains("index moved"), "{reason}");
    }

    #[test]
    fn a_reentering_hook_runs_once_and_the_outer_answer_names_the_queue() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn hook(store: &Store) {
            CALLS.fetch_add(1, Ordering::SeqCst);
            let _ = store.query_source_freshness();
            store
                .enqueue_pending_paths(&["a".repeat(4096)])
                .expect("long path enqueues");
        }
        CALLS.store(0, Ordering::SeqCst);
        let store = seeded();
        let _clear = ClearHook;
        set_source_freshness_interleave(Some(hook));
        let verdict = store.query_source_freshness();
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(verdict.fresh, None, "{verdict:?}");
        assert!(verdict.reason.unwrap_or_default().contains("queued"));
    }

    #[test]
    fn a_panicking_hook_releases_the_reentry_guard() {
        fn boom(_: &Store) {
            panic!("interleave hook");
        }
        let store = seeded();
        let _clear = ClearHook;
        set_source_freshness_interleave(Some(boom));
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = store.query_source_freshness();
        }));
        assert!(caught.is_err(), "the hook panic must reach the caller");
        set_source_freshness_interleave(Some(enqueue_one));
        let verdict = store.query_source_freshness();
        assert_eq!(
            verdict.fresh, None,
            "a stuck reentry flag would skip the enqueue and stay fresh: {verdict:?}"
        );
        assert!(verdict.reason.unwrap_or_default().contains("queued"));
    }

    #[test]
    fn a_queue_read_error_is_unverified_and_names_the_queue() {
        let blocked =
            queue_blocks_cached_freshness(Err(rusqlite::Error::InvalidQuery)).expect("blocked");
        assert_eq!(blocked.fresh, None);
        let reason = blocked.reason.expect("reason");
        assert!(reason.contains("pending queue"), "{reason}");
        assert!(queue_blocks_cached_freshness(Ok(0)).is_none());
        let counted = queue_blocks_cached_freshness(Ok(2)).expect("counted");
        assert_eq!(counted.fresh, None);
        assert!(counted.reason.unwrap().contains("2 changed path"));
    }
}

/// `status` asks two whole-generation integrity questions on every call. Their
/// answers are fixed by the generation id, so a second warm call must be
/// answered without reading the generation's rows again.
///
/// The proof is not a clock: the view each question scans is dropped
/// after the first answer, so a second call that ran its SQL would fail with
/// "no such table" and one that answered from the memo cannot.
#[cfg(test)]
mod generation_checks_memo {
    use super::*;
    use crate::db::GENERATION_CHECK_MAX_AGE;

    /// The `analysis_json` every generation this binary writes carries: its
    /// resolver stamp. Without it a generation reads as
    /// [`ResolverStamp::Unrecorded`] and is never current, whatever its files.
    fn stamped() -> String {
        format!(
            r#"{{"resolver_epoch":"{}"}}"#,
            devmap_resolve::resolver::RESOLUTION_REUSE_EPOCH
        )
    }

    fn seeded() -> Store {
        let store = Store::open_in_memory().expect("store");
        {
            let conn = lock_conn(&store.conn).expect("connection");
            conn.execute(
                "INSERT INTO generations (id, created_at, head_sha, analysis_json)
                 VALUES (7, 1.0, 'seed', ?1)",
                rusqlite::params![stamped()],
            )
            .expect("generation");
        }
        store
    }

    /// Moves the memo's clock past [`GENERATION_CHECK_MAX_AGE`] without
    /// sleeping.
    fn advance_past_the_bound() {
        crate::db::MEMO_CLOCK_SKEW
            .with(|skew| skew.set(GENERATION_CHECK_MAX_AGE + std::time::Duration::from_secs(1)));
    }

    /// Moves the memo's clock to just inside the bound.
    fn advance_to_just_inside_the_bound() {
        crate::db::MEMO_CLOCK_SKEW
            .with(|skew| skew.set(GENERATION_CHECK_MAX_AGE - std::time::Duration::from_secs(1)));
    }

    fn drop_view(store: &Store, view: &str) {
        let conn = lock_conn(&store.conn).expect("connection");
        conn.execute(&format!("DROP VIEW {view}"), [])
            .expect("drop view");
    }

    /// Within the age bound a warm repeat runs no SQL on the table (it is
    /// dropped, so any would fail); past the bound it is read again and the
    /// failure that a held pass would have hidden is reported.
    #[test]
    fn edge_confidence_is_held_within_the_bound_and_reread_after_it() {
        let store = seeded();
        let first = store.edge_confidence_mismatches().expect("first");
        assert_eq!(first, Some(0));
        drop_view(&store, "generation_edges");
        advance_to_just_inside_the_bound();
        let second = store
            .edge_confidence_mismatches()
            .expect("within the bound a repeat must not touch generation_edges");
        assert_eq!(second, first);
        advance_past_the_bound();
        assert!(
            store.edge_confidence_mismatches().is_err(),
            "past the bound the held pass must not be reported again"
        );
    }

    #[test]
    fn payload_identity_is_held_within_the_bound_and_reread_after_it() {
        let store = seeded();
        let first = store.latest_generation_payload_is_current().expect("first");
        assert!(first, "a generation with no files has no stale payload");
        drop_view(&store, "generation_files");
        advance_to_just_inside_the_bound();
        let second = store
            .latest_generation_payload_is_current()
            .expect("within the bound a repeat must not touch generation_files");
        assert_eq!(second, first);
        advance_past_the_bound();
        assert!(
            store.latest_generation_payload_is_current().is_err(),
            "past the bound the held pass must not be reported again"
        );
    }

    #[test]
    fn file_hashes_are_held_within_the_bound_and_reread_after_it() {
        let store = seeded();
        let first = store.latest_file_hashes_shared().expect("first");
        drop_view(&store, "generation_files");
        advance_to_just_inside_the_bound();
        let second = store
            .latest_file_hashes_shared()
            .expect("within the bound a repeat must not touch generation_files");
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "the repeat must hand back the held map, not a rebuilt one"
        );
        advance_past_the_bound();
        assert!(
            store.latest_file_hashes_shared().is_err(),
            "past the bound the held map must not be handed back again"
        );
    }

    /// `status` re-reads the generation and the pending count after its freshness
    /// walk to be sure the store did not move. That is two fields, and it used
    /// to be two more whole snapshots (full-text probe, coverage gaps, payload
    /// encoding count): three per call, against one now.
    #[test]
    fn one_status_takes_one_full_snapshot() {
        let root =
            std::env::temp_dir().join(format!("devmap-status-snapshots-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("root");
        let store = seeded();
        {
            let conn = lock_conn(&store.conn).expect("connection");
            conn.execute(
                "UPDATE generations SET repo_root = ?1 WHERE id = 7",
                params![root.to_string_lossy()],
            )
            .expect("bind root");
        }
        STATUS_SNAPSHOTS.with(|taken| taken.set(0));
        let status = store.status("db").expect("status");
        let taken = STATUS_SNAPSHOTS.with(Cell::get);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            status.source_freshness,
            Some(true),
            "the walk must have run to completion for this count to mean anything: {status:?}"
        );
        assert_eq!(taken, 1, "status took {taken} full snapshots");
    }

    #[test]
    fn a_clean_encoding_verdict_is_held_within_the_bound_and_reread_after_it() {
        let store = seeded();
        let ask = |generation: u32| {
            let conn = lock_conn(&store.conn).expect("connection");
            let stamp = Store::connection_stamp(&conn).expect("stamp");
            let snapshot = conn.unchecked_transaction().expect("snapshot");
            store.payload_encoding_reason_cached(&snapshot, Some(generation), stamp)
        };
        assert_eq!(ask(7).expect("first"), None);
        {
            let conn = lock_conn(&store.conn).expect("connection");
            conn.execute("DROP TABLE file_payloads", [])
                .expect("drop table");
        }
        advance_to_just_inside_the_bound();
        assert_eq!(
            ask(7).expect("within the bound a clean repeat must not count the payloads"),
            None
        );
        advance_past_the_bound();
        assert!(
            ask(7).is_err(),
            "past the bound the held clean verdict must not be reported again"
        );
        crate::db::MEMO_CLOCK_SKEW.with(|skew| skew.set(std::time::Duration::ZERO));
        assert!(
            ask(8).is_err(),
            "a different generation was not found clean, so it must be counted"
        );
    }

    /// The integrity checks judge rows another connection can rewrite under a
    /// standing generation id (a kernel upgrade rewriting payload identities, a
    /// repair, an old writer). A commit by any other connection must end every
    /// held answer, or `status` would certify a store it no longer describes.
    #[test]
    fn a_commit_by_another_connection_ends_every_held_answer() {
        let dir = std::env::temp_dir().join(format!("devmap-checks-memo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let db = dir.join("devmap.sqlite");
        let store = Store::open(&db).expect("store");
        let foreign = rusqlite::Connection::open(&db).expect("foreign connection");
        foreign
            .execute(
                "INSERT INTO generations (id, created_at, head_sha, analysis_json)
                 VALUES (7, 1.0, 'seed', ?1)",
                rusqlite::params![stamped()],
            )
            .expect("generation");
        assert_eq!(store.edge_confidence_mismatches().expect("held"), Some(0));
        assert!(store.latest_generation_payload_is_current().expect("held"));
        store.latest_file_hashes_shared().expect("held");
        // Another connection commits after the answers above were held.
        foreign
            .execute("DROP VIEW generation_edges", [])
            .expect("drop");
        foreign
            .execute("DROP VIEW generation_files", [])
            .expect("drop");
        let edges = store.edge_confidence_mismatches();
        let payload = store.latest_generation_payload_is_current();
        let hashes = store.latest_file_hashes_shared();
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(edges.is_err(), "held edge answer survived a foreign commit");
        assert!(
            payload.is_err(),
            "held payload answer survived a foreign commit"
        );
        assert!(hashes.is_err(), "held hashes survived a foreign commit");
    }

    #[test]
    fn a_newer_generation_is_checked_afresh() {
        let store = seeded();
        assert_eq!(store.edge_confidence_mismatches().expect("g7"), Some(0));
        assert!(store.latest_generation_payload_is_current().expect("g7"));
        store.latest_file_hashes_shared().expect("g7");
        {
            let conn = lock_conn(&store.conn).expect("connection");
            conn.execute(
                "INSERT INTO generations (id, created_at, head_sha, analysis_json)
                 VALUES (8, 2.0, 'next', ?1)",
                rusqlite::params![stamped()],
            )
            .expect("generation");
        }
        drop_view(&store, "generation_edges");
        drop_view(&store, "generation_files");
        assert!(
            store.edge_confidence_mismatches().is_err(),
            "generation 8 is not the memoised generation 7, so it must be read"
        );
        assert!(
            store.latest_generation_payload_is_current().is_err(),
            "generation 8 is not the memoised generation 7, so it must be read"
        );
        assert!(
            store.latest_file_hashes_shared().is_err(),
            "generation 8 is not the memoised generation 7, so it must be read"
        );
    }
}

/// The held answers must stay *right*, not just quiet: after the rows they were
/// read from change, the next call returns the new value. Each answer is
/// checked against a write by another connection and a write by this store's
/// own connection, neither of which a generation id can see.
#[cfg(all(test, feature = "parse"))]
mod generation_checks_staleness {
    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum Writer {
        Foreign,
        Own,
    }

    struct Fixture {
        dir: std::path::PathBuf,
        db: std::path::PathBuf,
        store: Store,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn fixture(label: &str) -> Fixture {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "devmap-checks-staleness-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let db = dir.join("devmap.sqlite");
        let store = Store::open(&db).expect("store");
        let sources = [
            (
                "ledger.py",
                "def ledger_total(rows):\n    return sum(rows)\n",
            ),
            (
                "checks.py",
                "from ledger import ledger_total\n\n\ndef summary(rows):\n    return ledger_total(rows)\n",
            ),
        ];
        let extractions: Vec<_> = sources
            .iter()
            .map(|(path, body)| devmap_extract::extract_file(path, body))
            .collect();
        let mut resolver = devmap_resolve::Resolver::new();
        resolver.index_extractions(&extractions);
        let resolution = resolver.resolve_all(&extractions).expect("resolve");
        let analysis = devmap_analyze::analyze(&extractions, &resolution);
        store
            .save_generation_with_opts(
                &extractions,
                &resolution,
                &analysis,
                crate::GenerationWriteOpts::default(),
            )
            .expect("generation");
        Fixture { dir, db, store }
    }

    /// Runs `sql` as `writer` and returns the rows it changed.
    fn write(fixture: &Fixture, writer: Writer, sql: &str) -> usize {
        match writer {
            Writer::Foreign => rusqlite::Connection::open(&fixture.db)
                .expect("foreign connection")
                .execute(sql, [])
                .expect("foreign write"),
            Writer::Own => lock_conn(&fixture.store.conn)
                .expect("connection")
                .execute(sql, [])
                .expect("own write"),
        }
    }

    fn payload_identity_is_reread(writer: Writer) {
        let f = fixture("payload");
        assert!(f
            .store
            .latest_generation_payload_is_current()
            .expect("held"));
        assert!(
            write(
                &f,
                writer,
                "UPDATE file_payloads SET analyzer_version = '0.0.0:stale'"
            ) > 0
        );
        assert!(
            !f.store
                .latest_generation_payload_is_current()
                .expect("reread"),
            "{writer:?}: the held `current` outlived a rewritten payload identity"
        );
    }

    fn file_hashes_are_reread(writer: Writer) {
        let f = fixture("hashes");
        let before = f.store.latest_file_hashes_shared().expect("held");
        assert!(!before.is_empty() && before.values().all(|hash| *hash > 1000));
        assert!(
            write(
                &f,
                writer,
                "UPDATE file_payloads SET content_hash = payload_id"
            ) > 0
        );
        let after = f.store.latest_file_hashes_shared().expect("reread");
        assert!(
            after.len() == before.len() && after.values().all(|hash| *hash < 1000),
            "{writer:?}: the held hash map outlived a rewritten content_hash: {after:?}"
        );
    }

    fn edge_confidence_is_reread(writer: Writer) {
        let f = fixture("edges");
        assert_eq!(f.store.edge_confidence_mismatches().expect("held"), Some(0));
        let changed = write(
            &f,
            writer,
            "UPDATE edge_rows SET confidence = 0.2
              WHERE valid_to IS NULL AND resolution IS NOT NULL",
        );
        assert!(changed > 0, "the fixture must hold a resolved edge");
        assert_eq!(
            f.store.edge_confidence_mismatches().expect("reread"),
            Some(changed),
            "{writer:?}: the held count outlived rewritten edge confidences"
        );
    }

    fn encoding_verdict_is_reread(writer: Writer) {
        let f = fixture("encoding");
        let reason = |f: &Fixture| {
            f.store
                .status(&f.db.to_string_lossy())
                .expect("status")
                .degraded_reason
                .filter(|reason| reason.contains("schema-29 form"))
        };
        assert_eq!(reason(&f), None, "a freshly written store is clean");
        assert!(
            write(
                &f,
                writer,
                "UPDATE file_payloads
                    SET extraction_json = '{not json', entry_wiring_json = NULL
                  WHERE payload_id = (SELECT min(payload_id) FROM file_payloads)",
            ) > 0
        );
        assert!(
            reason(&f).is_some(),
            "{writer:?}: the held clean verdict outlived an undecodable payload"
        );
    }

    #[test]
    fn payload_identity_after_a_foreign_write() {
        payload_identity_is_reread(Writer::Foreign);
    }

    #[test]
    fn payload_identity_after_this_stores_own_write() {
        payload_identity_is_reread(Writer::Own);
    }

    #[test]
    fn file_hashes_after_a_foreign_write() {
        file_hashes_are_reread(Writer::Foreign);
    }

    #[test]
    fn file_hashes_after_this_stores_own_write() {
        file_hashes_are_reread(Writer::Own);
    }

    #[test]
    fn edge_confidence_after_a_foreign_write() {
        edge_confidence_is_reread(Writer::Foreign);
    }

    #[test]
    fn edge_confidence_after_this_stores_own_write() {
        edge_confidence_is_reread(Writer::Own);
    }

    #[test]
    fn encoding_verdict_after_a_foreign_write() {
        encoding_verdict_is_reread(Writer::Foreign);
    }

    #[test]
    fn encoding_verdict_after_this_stores_own_write() {
        encoding_verdict_is_reread(Writer::Own);
    }
}
