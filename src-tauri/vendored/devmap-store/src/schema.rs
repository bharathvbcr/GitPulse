pub const CREATE_SCHEMA_V3: &str = r#"
CREATE TABLE IF NOT EXISTS paths (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    path TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS generations (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at REAL NOT NULL,
    head_sha   TEXT,
    analysis_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS generation_nodes (
    generation_id  INTEGER NOT NULL,
    ordinal        INTEGER NOT NULL,
    file_id        INTEGER NOT NULL REFERENCES paths(id),
    name           TEXT NOT NULL,
    qualified_name TEXT NOT NULL,
    kind           TEXT NOT NULL,
    span_start     INTEGER NOT NULL,
    span_end       INTEGER NOT NULL,
    is_exported    INTEGER NOT NULL,
    body_exact     INTEGER,
    body_structural INTEGER,
    body_nodes     INTEGER,
    PRIMARY KEY (generation_id, ordinal)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS file_payloads (
    payload_id         INTEGER PRIMARY KEY,
    file_id            INTEGER NOT NULL REFERENCES paths(id),
    content_hash       INTEGER NOT NULL,
    language           TEXT NOT NULL,
    grammar_version    TEXT,
    analyzer_version   TEXT,
    parse_outcome_json TEXT NOT NULL,
    engine_json        TEXT NOT NULL,
    extraction_json    TEXT NOT NULL
);

-- The payload's identity: **the file** plus the four fields the extraction
-- cache keys on, NULL-safe.
--
-- `file_id` is in the key and must be. A payload is a serialized `Extraction`,
-- and an `Extraction` carries its own `file_path` — so content-addressing
-- alone collapses two files with identical bytes into one payload and makes
-- both membership rows report the *same* path. That is not hypothetical:
-- `a_cold_build_indexes_an_in_root_symlink_and_a_drain_of_it_keeps_the_symbol`
-- caught it on the first run, because a symlink and its target are byte-
-- identical by construction and the linked path vanished from the generation.
--
-- Nothing is lost. What B3 deduplicates is the *same file, unchanged, across
-- generations*, which is 1,530 of the 1,530 duplicate rows measured on this
-- repository. Two different files that happen to share content have genuinely
-- different payloads.
--
-- `grammar_version` and `analyzer_version` are nullable — NULL means "stored by
-- a build with no parsing frontend", which is a real state — and SQLite treats
-- NULLs as distinct inside a UNIQUE index, so a plain unique constraint would
-- let two identical NULL-version payloads both insert and defeat the point. The
-- COALESCE expressions make the index NULL-safe; the write path probes with the
-- same expressions.
CREATE UNIQUE INDEX IF NOT EXISTS idx_file_payloads_identity
    ON file_payloads(file_id, content_hash, language,
                     COALESCE(grammar_version, ''), COALESCE(analyzer_version, ''));

-- The extraction-cache fallback asks by content identity alone — it has no
-- path, because `CacheKey` has none — so it needs its own index. This is v13's
-- index, over one row per (file, content) instead of one per (generation,
-- file): the relation v13 described as a scan "whose rows each carry a ~47 KB
-- `extraction_json` the scan must skip past".
CREATE INDEX IF NOT EXISTS idx_file_payloads_cache_identity
    ON file_payloads(content_hash, language, grammar_version, analyzer_version);

CREATE TABLE IF NOT EXISTS generation_file_rows (
    generation_id INTEGER NOT NULL,
    file_id       INTEGER NOT NULL REFERENCES paths(id),
    payload_id    INTEGER NOT NULL REFERENCES file_payloads(payload_id),
    PRIMARY KEY (generation_id, file_id)
) WITHOUT ROWID;

-- (v28 removed `idx_generation_file_rows_payload` and
-- `idx_generation_nodes_file` from this batch with the tables they indexed. A
-- store walked up the ladder is re-entrant over the current shape, where those
-- two names are views, and `CREATE INDEX` on a view is an error.)

-- `generation_files` keeps its name and its exact column set, as a view.
--
-- Twenty-five read sites across five crates, `tools/fanout.sql` and a dozen
-- tests query this relation by name. Splitting the payload out under a *new*
-- name would have meant rewriting every one of them for a change none of them
-- cares about: what a generation holds for a file is unchanged, only where the
-- bytes live.
CREATE VIEW IF NOT EXISTS generation_files AS
SELECT m.generation_id      AS generation_id,
       m.file_id            AS file_id,
       p.language           AS language,
       p.content_hash       AS content_hash,
       p.parse_outcome_json AS parse_outcome_json,
       p.engine_json        AS engine_json,
       p.extraction_json    AS extraction_json,
       p.grammar_version    AS grammar_version,
       p.analyzer_version   AS analyzer_version
  FROM generation_file_rows m
  JOIN file_payloads p ON p.payload_id = m.payload_id;

CREATE TABLE IF NOT EXISTS generation_coverage_gaps (
    generation_id INTEGER NOT NULL,
    gap           TEXT NOT NULL,
    path          TEXT NOT NULL,
    reason        TEXT NOT NULL,
    PRIMARY KEY (generation_id, gap, path)
) WITHOUT ROWID;

-- v18's two relations are created by `VALIDITY_RANGE_TABLES`, which the fresh
-- path applies straight after this batch. They are not inlined here for the
-- reason `MIGRATION_V16_TO_V17` learned the hard way: v17 kept a second copy of
-- the payload split's DDL in this constant and the migration's copy silently
-- lost an index. One owner, applied by both paths.

CREATE TABLE IF NOT EXISTS generation_dead_symbols (
    generation_id    INTEGER NOT NULL,
    ordinal          INTEGER NOT NULL,
    file_path        TEXT NOT NULL,
    symbol_name      TEXT NOT NULL,
    confidence       REAL NOT NULL,
    is_exempt        INTEGER NOT NULL,
    exemption_reason TEXT,
    PRIMARY KEY (generation_id, ordinal)
) WITHOUT ROWID;

CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
    name, qualified_name, path, tokenize='unicode61'
);

CREATE TABLE IF NOT EXISTS nodes_fts_map (
    rowid_ref     INTEGER NOT NULL,
    generation_id INTEGER NOT NULL,
    PRIMARY KEY (generation_id, rowid_ref)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS pending_paths (
    path       TEXT PRIMARY KEY,
    queued_at  REAL NOT NULL,
    attempts   INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS extraction_cache (
    content_hash     INTEGER NOT NULL,
    language         TEXT NOT NULL,
    grammar_version  TEXT NOT NULL,
    analyzer_version TEXT NOT NULL,
    payload_json     TEXT NOT NULL,
    accessed_at      REAL NOT NULL,
    PRIMARY KEY (content_hash, language, grammar_version, analyzer_version)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS extraction_retry (
    content_hash INTEGER PRIMARY KEY,
    language     TEXT NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 0,
    last_reason  TEXT NOT NULL,
    updated_at   REAL NOT NULL
) WITHOUT ROWID;

"#;

pub const MIGRATION_V3_TO_V4: &str = r#"
CREATE TABLE IF NOT EXISTS extraction_retry (
    content_hash INTEGER PRIMARY KEY,
    language     TEXT NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 0,
    last_reason  TEXT NOT NULL,
    updated_at   REAL NOT NULL
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS extraction_cache_v4 (
    content_hash     INTEGER NOT NULL,
    language         TEXT NOT NULL,
    grammar_version  TEXT NOT NULL,
    analyzer_version TEXT NOT NULL,
    payload_json     TEXT NOT NULL,
    accessed_at      REAL NOT NULL,
    PRIMARY KEY (content_hash, language, grammar_version, analyzer_version)
) WITHOUT ROWID;

INSERT OR IGNORE INTO extraction_cache_v4 (content_hash, language, grammar_version, analyzer_version, payload_json, accessed_at)
SELECT content_hash, 'unknown', 'legacy', 'legacy', payload_json, accessed_at
FROM extraction_cache;

DROP TABLE IF EXISTS extraction_cache;
ALTER TABLE extraction_cache_v4 RENAME TO extraction_cache;
"#;

pub const MIGRATION_V4_TO_V5: &str = r#"
CREATE TABLE IF NOT EXISTS generations (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at REAL NOT NULL,
    head_sha   TEXT
);

CREATE TABLE IF NOT EXISTS file_payloads (
    payload_id         INTEGER PRIMARY KEY,
    file_id            INTEGER NOT NULL REFERENCES paths(id),
    content_hash       INTEGER NOT NULL,
    language           TEXT NOT NULL,
    grammar_version    TEXT,
    analyzer_version   TEXT,
    parse_outcome_json TEXT NOT NULL,
    engine_json        TEXT NOT NULL,
    extraction_json    TEXT NOT NULL
);

-- The payload's identity: **the file** plus the four fields the extraction
-- cache keys on, NULL-safe.
--
-- `file_id` is in the key and must be. A payload is a serialized `Extraction`,
-- and an `Extraction` carries its own `file_path` — so content-addressing
-- alone collapses two files with identical bytes into one payload and makes
-- both membership rows report the *same* path. That is not hypothetical:
-- `a_cold_build_indexes_an_in_root_symlink_and_a_drain_of_it_keeps_the_symbol`
-- caught it on the first run, because a symlink and its target are byte-
-- identical by construction and the linked path vanished from the generation.
--
-- Nothing is lost. What B3 deduplicates is the *same file, unchanged, across
-- generations*, which is 1,530 of the 1,530 duplicate rows measured on this
-- repository. Two different files that happen to share content have genuinely
-- different payloads.
--
-- `grammar_version` and `analyzer_version` are nullable — NULL means "stored by
-- a build with no parsing frontend", which is a real state — and SQLite treats
-- NULLs as distinct inside a UNIQUE index, so a plain unique constraint would
-- let two identical NULL-version payloads both insert and defeat the point. The
-- COALESCE expressions make the index NULL-safe; the write path probes with the
-- same expressions.
CREATE UNIQUE INDEX IF NOT EXISTS idx_file_payloads_identity
    ON file_payloads(file_id, content_hash, language,
                     COALESCE(grammar_version, ''), COALESCE(analyzer_version, ''));

-- The extraction-cache fallback asks by content identity alone — it has no
-- path, because `CacheKey` has none — so it needs its own index. This is v13's
-- index, over one row per (file, content) instead of one per (generation,
-- file): the relation v13 described as a scan "whose rows each carry a ~47 KB
-- `extraction_json` the scan must skip past".
CREATE INDEX IF NOT EXISTS idx_file_payloads_cache_identity
    ON file_payloads(content_hash, language, grammar_version, analyzer_version);

CREATE TABLE IF NOT EXISTS generation_file_rows (
    generation_id INTEGER NOT NULL,
    file_id       INTEGER NOT NULL REFERENCES paths(id),
    payload_id    INTEGER NOT NULL REFERENCES file_payloads(payload_id),
    PRIMARY KEY (generation_id, file_id)
) WITHOUT ROWID;

-- (no `idx_generation_file_rows_payload`: see `CREATE_SCHEMA_V3`.)

-- `generation_files` keeps its name and its exact column set, as a view.
--
-- Twenty-five read sites across five crates, `tools/fanout.sql` and a dozen
-- tests query this relation by name. Splitting the payload out under a *new*
-- name would have meant rewriting every one of them for a change none of them
-- cares about: what a generation holds for a file is unchanged, only where the
-- bytes live.
CREATE VIEW IF NOT EXISTS generation_files AS
SELECT m.generation_id      AS generation_id,
       m.file_id            AS file_id,
       p.language           AS language,
       p.content_hash       AS content_hash,
       p.parse_outcome_json AS parse_outcome_json,
       p.engine_json        AS engine_json,
       p.extraction_json    AS extraction_json,
       p.grammar_version    AS grammar_version,
       p.analyzer_version   AS analyzer_version
  FROM generation_file_rows m
  JOIN file_payloads p ON p.payload_id = m.payload_id;

CREATE TABLE IF NOT EXISTS generation_dead_symbols (
    generation_id    INTEGER NOT NULL,
    ordinal          INTEGER NOT NULL,
    file_path        TEXT NOT NULL,
    symbol_name      TEXT NOT NULL,
    confidence       REAL NOT NULL,
    is_exempt        INTEGER NOT NULL,
    exemption_reason TEXT,
    PRIMARY KEY (generation_id, ordinal)
) WITHOUT ROWID;

"#;

/// The v5 edge indexes, split out of [`MIGRATION_V4_TO_V5`] because they are
/// legal only while `generation_edges` is still a base table.
///
/// The v3 and v4 rungs re-run `CREATE_SCHEMA_V3`, which carries the *current*
/// shape — so by the time this batch would run on a fresh-then-migrated store,
/// `generation_edges` is v18's view and `CREATE INDEX` on a view is an error,
/// `IF NOT EXISTS` or not. Exactly the shape `MIGRATION_V12_TO_V13` hit when
/// `generation_files` became a view in v17, and guarded the same way: the
/// caller asks `relation_is_table` first. The successors on `edge_rows` live in
/// [`VALIDITY_RANGE_TABLES`].
pub const MIGRATION_V4_TO_V5_EDGE_INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS idx_generation_edges_source
    ON generation_edges(generation_id, source_file_id);
CREATE INDEX IF NOT EXISTS idx_generation_edges_target
    ON generation_edges(generation_id, target_file_id);
"#;

/// One durable row per committed generation, written inside the generation's
/// own transaction so a build can never be counted without its history entry
/// (or vice versa). Retention is capped independently of generation pruning:
/// history rows are ~100 B and outlive the graph they describe, which is the
/// entire point of a longitudinal view.
pub const BUILD_HISTORY_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS build_history (
    generation_id     INTEGER PRIMARY KEY,
    built_at          REAL NOT NULL,
    head_sha          TEXT NOT NULL,
    files             INTEGER NOT NULL,
    symbols           INTEGER NOT NULL,
    edges             INTEGER NOT NULL,
    dead_confident    INTEGER NOT NULL,
    dead_ambiguous    INTEGER NOT NULL,
    parse_failed      INTEGER NOT NULL,
    languages_covered INTEGER NOT NULL,
    build_ms          INTEGER CHECK (build_ms IS NULL OR build_ms >= 0),
    db_bytes          INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_build_history_built_at
    ON build_history(built_at DESC);
"#;

pub const MIGRATION_V5_TO_V6: &str = BUILD_HISTORY_TABLE;

/// Longest history the store retains. Rows are tiny, but the cap keeps an
/// always-on watcher from growing the table without bound.
pub const BUILD_HISTORY_RETENTION: usize = 500;

/// Generations retained after each committed build.
///
/// Every generation carries a full carry-forward copy of the repository's
/// extraction payloads, nodes and edges, so an unpruned store grows by
/// O(repository size) per build forever — measured at +327 MiB per one-line
/// edit on a 4,731-file repository (SC1).
///
/// One is the minimum that is actually correct: the differential builder reads
/// exactly one prior generation to carry rows forward, and nothing else in the
/// tree reads a non-latest generation. The second is deliberate headroom for
/// rename-alias chaining and for inspecting the previous build after a bad one,
/// and matches the Python incumbent's `retain_generations = 2`.
pub const GENERATION_RETENTION: usize = 2;

/// v7: record the absolute root a generation was built from.
///
/// Node paths are stored repo-relative. Without the root, a query process
/// resolves them against its own working directory, so every source span read
/// from anywhere but the repo root silently comes back empty. `ALTER TABLE ADD
/// COLUMN` is the migration: existing rows keep NULL, which reads as "root
/// unknown" rather than as a wrong root.
pub const MIGRATION_V6_TO_V7: &str = r#"
ALTER TABLE generations ADD COLUMN repo_root TEXT;
"#;

/// v8: record the grammar and analyzer identity a generation's payload was
/// produced with.
///
/// `extraction_cache` is keyed `(content_hash, language, grammar_version,
/// analyzer_version)` precisely so a payload produced by older extraction
/// semantics can never be reused. `generation_files` held byte-identical
/// payloads but recorded only `(language, content_hash)`, so it could not be
/// used as a fallback source without silently discarding that guarantee —
/// exactly the staleness `EXTRACTION_SCHEMA_VERSION` exists to prevent, and how
/// fixed false positives would come back. Carrying the identity here lets the
/// cache and the generation store hold one copy between them instead of two
/// (SC8). Existing rows keep NULL, which reads as "identity unknown" and is
/// therefore never eligible as a fallback — absence of proof, not proof.
pub const MIGRATION_V7_TO_V8: &str = r#"
ALTER TABLE generation_files ADD COLUMN grammar_version TEXT;
ALTER TABLE generation_files ADD COLUMN analyzer_version TEXT;
"#;

/// D17: calls seen but never attributed to a target.
///
/// The resolver already computes these — they are the honest denominator for
/// any "how complete is this graph" question — but they lived only in memory,
/// so nothing could ask why a symbol had no callers. Stored per generation and
/// pruned with it, like `generation_dead_symbols`.
pub const UNRESOLVED_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS generation_unresolved (
    generation_id INTEGER NOT NULL,
    ordinal       INTEGER NOT NULL,
    source_file   TEXT NOT NULL,
    source_symbol TEXT NOT NULL,
    callee_name   TEXT NOT NULL,
    reason        TEXT NOT NULL,
    classification TEXT NOT NULL DEFAULT 'unresolved',
    receiver       TEXT,
    PRIMARY KEY (generation_id, ordinal)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_generation_unresolved_callee
    ON generation_unresolved(generation_id, callee_name);

CREATE INDEX IF NOT EXISTS idx_generation_unresolved_class
    ON generation_unresolved(generation_id, classification);
"#;

pub const MIGRATION_V8_TO_V9: &str = UNRESOLVED_TABLE;

/// SC18: `classification` splits calls that *cannot* resolve — language
/// builtins, and names an import proves come from outside the corpus — from the
/// genuine failures that indicate a defect. Without it every consumer reads one
/// undifferentiated count, which is what made 380k expected rows hide the two
/// extraction bugs closed as SC17.
///
/// The default backfills existing rows as `unresolved`, which is exactly what
/// they meant when they were written: the classifier had not run, so claiming
/// any of them were expected would assert something never measured.
pub const MIGRATION_V9_TO_V10: &str = r#"
ALTER TABLE generation_unresolved
    ADD COLUMN classification TEXT NOT NULL DEFAULT 'unresolved';

CREATE INDEX IF NOT EXISTS idx_generation_unresolved_class
    ON generation_unresolved(generation_id, classification);
"#;

/// SC25: the receiver expression a call was made on, or NULL for a bare call.
///
/// Added because the classification could not be *audited* without it. Asking
/// "is `uninferred_receiver` really all method calls, and is the `unresolved`
/// tier really all bare names" required instrumenting a build, since the row
/// recorded only the callee. A classification nobody can check is a claim, and
/// this table exists precisely to be the honest denominator.
///
/// Nullable rather than defaulted: a bare call has no receiver, and writing an
/// empty string would make "no receiver" indistinguishable from "a receiver
/// whose text we failed to capture". Existing rows backfill to NULL, which is
/// truthful — the column did not exist when they were written.
pub const MIGRATION_V10_TO_V11: &str = r#"
ALTER TABLE generation_unresolved ADD COLUMN receiver TEXT;
"#;

/// SC26: body signatures for clone detection.
///
/// Three nullable columns rather than a `generation_clones` table, because a
/// clone group is not a fact about the tree — it is a join over facts about
/// symbols. Storing the groups would mean storing a derived, truncated view
/// that has to be kept in step with the rows it came from; storing the hashes
/// lets any generation be grouped on demand, in full, by the one grouping
/// implementation in `devmap-analyze`.
///
/// No covering index. `generation_nodes` is `WITHOUT ROWID` on
/// `(generation_id, ordinal)`, so reading one generation's symbols is already a
/// primary-key range scan; an index on the hashes would add store size — the
/// thing this schema works to bound — to save nothing on a scan that has to
/// touch every row of the generation anyway.
///
/// Nullable, and null means "no signature was computed for this symbol": a body
/// under the size floor, a kind with no comparable body, or a file no grammar
/// parsed. Rows written before this column existed backfill to null, which says
/// the same true thing about them.
pub const MIGRATION_V11_TO_V12: &str = r#"
ALTER TABLE generation_nodes ADD COLUMN body_exact INTEGER;
ALTER TABLE generation_nodes ADD COLUMN body_structural INTEGER;
ALTER TABLE generation_nodes ADD COLUMN body_nodes INTEGER;
"#;

/// v13: make the SC8 extraction-cache fallback a lookup instead of a scan.
///
/// `try_get_cached_extraction` misses `extraction_cache` and falls back to
/// `generation_files`, matching on the full cache identity. `generation_files`
/// is `WITHOUT ROWID` keyed `(generation_id, file_id)` and had **no index on
/// `content_hash`**, so `EXPLAIN QUERY PLAN` reported `SCAN generation_files`
/// for that fallback — once per file, on every build.
///
/// The fallback is not the exceptional path, it is the *only* path: SC7's
/// `prune_extraction_cache` deletes every `extraction_cache` row that a
/// retained generation already holds with a matching identity, which is all of
/// them. Measured on a cold-built store, `extraction_cache` holds **0 rows**
/// after every build across 10 consecutive builds — so the first query always
/// misses and every file pays a scan whose rows each carry a ~47 KB
/// `extraction_json` the scan must skip past to reach the identity columns.
///
/// Measured against the release binary, an 8,001-file synthetic corpus, no-op
/// build (nothing changed — the case a watcher hits on every tick), four runs:
///
/// | | min | median | max |
/// |---|---|---|---|
/// | before | 2.311 s | 2.619 s | 4.543 s |
/// | after  | 0.690 s | 0.724 s | 0.826 s |
///
/// **3.3x on the median.** Those two rows were taken when the index landed and
/// are not re-measured here; the "before" one cannot be without reverting the
/// migration.
///
/// The *scaling* was re-measured on 2026-09-05 against the merged tree, release
/// binary, `benchmarks/map_bench.py --synthetic N --repeat 5`, minimum
/// reported: the no-op build is **110 ms over 2,001 files and 444 ms over
/// 8,001** — 4.0x the time for 4.0x the files, so the cost is linear in corpus
/// size rather than in corpus size times stored bytes. Cold build over the same
/// pair is 657 ms -> 3.37 s (5.1x), and throughput falls only 3,047 -> 2,375
/// files/s across the 4x.
///
/// `CREATE INDEX IF NOT EXISTS` is idempotent, so this step needs no probe —
/// unlike the `ADD COLUMN` migrations. Index build cost measured at 24 ms on a
/// 1,333-row store, with no measurable file growth.
pub const MIGRATION_V12_TO_V13: &str = r#"
CREATE INDEX IF NOT EXISTS idx_generation_files_cache_identity
    ON generation_files(content_hash, language, grammar_version, analyzer_version);
"#;

/// v14: the inventory of what a generation could not read.
///
/// One row per path, not a number. `AnalysisSummary.discovery_refused_files`
/// was a count, and a count cannot be *maintained* — only replaced. That is
/// what forced the daemon's incremental drain, which never re-walks discovery,
/// to carry the previous generation's number forward and take
/// `max(previous, this_batch)` as a floor. The floor bought "a resync must not
/// erase a recorded refusal" with two wrong answers: a repaired file stayed
/// counted until a full re-extraction, and a refusal this batch met vanished
/// into a larger carried number — the second an over-claim, the shape this
/// codebase treats as the expensive one.
///
/// With the paths stored, the drain carries the inventory *minus every path in
/// this batch's affected set*, plus what this batch was turned away from: a
/// path nothing touched keeps its verdict, a path this batch touched is
/// re-decided by `candidate_kind`. The count is then `COUNT(*)` and cannot
/// drift from the set it counts.
///
/// The same table holds the two extraction gaps — files a grammar was wanted
/// for and did not read, and files recovered by line pattern — for a different
/// reason: they *are* derivable from `generation_files`, but only by
/// deserializing `parse_outcome_json` for every file in the generation, and
/// those rows carry a ~47 KB `extraction_json` each that the scan has to walk
/// past. Measured on this repository that is the difference between a `status`
/// costing under a millisecond and one costing tens. The write path derives
/// them from `devmap_analyze::extraction_gaps`, the same owner
/// `extraction_coverage` folds, so the stored list cannot disagree with the
/// counts the analysis reported.
///
/// `WITHOUT ROWID` and keyed `(generation_id, gap, path)`: reading one
/// generation's gaps of one kind is a primary-key range scan, which is what
/// `status` does three times.
pub const COVERAGE_GAPS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS generation_coverage_gaps (
    generation_id INTEGER NOT NULL,
    gap           TEXT NOT NULL,
    path          TEXT NOT NULL,
    reason        TEXT NOT NULL,
    PRIMARY KEY (generation_id, gap, path)
) WITHOUT ROWID;
"#;

/// v15: the evidence tier each edge was built from.
///
/// `ResolvedEdge::new` is the only constructor the resolver uses, so an edge's
/// `confidence` cannot disagree with its `Resolution` on the way in. On the way
/// back out there was nothing: no column held the resolution, so
/// `devmap-query`'s `stored_edge_to_resolved` rebuilt every edge with
/// `resolution: None` and the honesty invariant rested on the round trip plus
/// the write-side constructor — never on a second, independent reading of the
/// same fact.
///
/// Nullable, and NULL is not a tier. It means "written before this column
/// existed", which is why the read path labels such an edge
/// `ResolutionSource::Reconstructed`: a variant guessed from the row's file
/// layout must never be indistinguishable from one the resolver actually
/// recorded.
pub const MIGRATION_V14_TO_V15: &str = r#"
ALTER TABLE generation_edges ADD COLUMN resolution TEXT;
"#;

/// How many candidates an ambiguous resolution actually held.
///
/// `AMBIGUOUS_FANOUT_CAP` (audit R-7) bounds how many **edges** one ambiguous
/// site emits — 16. It does not bound the site's candidate list, which the
/// `Arc<Resolution>` still holds in full, deliberately: that list is what keeps
/// `impact` answerable on candidates 2..N. So resolver memory is proportional
/// to *candidates* while every number derivable from the store counted
/// *emitted edges*, and since R-7 the two have not been the same quantity.
///
/// `verify.sh` step 6 has been red because of it. Its three coefficient caps
/// were calibrated on a resolver with no cap, and re-deriving them needs the
/// denominator the memory actually tracks — which was not in the store at all:
/// the candidate list lives only on the in-memory `ResolvedEdge`, and
/// `generation_edges` had no column that could carry any part of it. Fixing the
/// gate by moving a coefficient instead would have been the "raised to fit"
/// this repository refuses.
///
/// One integer, on the ambiguous rows only. NULL means one of two things and
/// the reader must not conflate them: the edge is not an `AmbiguousGlobal` (no
/// candidate list exists), or the row predates this column. `resolution` tells
/// them apart — an ambiguous row written by this binary always carries a
/// count, so `resolution = 'AmbiguousGlobal' AND candidate_total IS NULL` is an
/// older row and a query that needs the denominator must refuse rather than
/// treat it as zero.
pub const MIGRATION_V15_TO_V16: &str = r#"
ALTER TABLE generation_edges ADD COLUMN candidate_total INTEGER;
"#;

/// v17: store one extraction payload per *content*, not per generation (B3).
///
/// Measured on this repository, two generations apart by a single edited line:
/// `generation_files` held 3,062 rows totalling **164.5 MB of
/// `extraction_json`, 54% of a 302.8 MB store — and 1,530 of those rows were
/// byte-identical duplicates.** One edited file caused ~82 MB of JSON to be
/// read out of SQLite, moved through Rust one row at a time, and written back.
/// That is the whole of B3's measured cost: the carry-forward this store has
/// done since B3's first half landed avoids re-*deriving* an unchanged payload,
/// but still re-*materialises* it under the new generation id.
///
/// The split is by the identity the extraction cache already keys on —
/// `(content_hash, language, grammar_version, analyzer_version)`, which v13
/// indexed on `generation_files` for exactly this lookup and described as a
/// scan "whose rows each carry a ~47 KB `extraction_json` the scan must skip
/// past to reach the identity columns". Those columns now live in a table with
/// one row per distinct payload, so that lookup stops skipping past anything.
///
/// `generation_files` keeps its name and its exact column set as a view over
/// the join, so all twenty-five read sites, `tools/fanout.sql` and the tests
/// are unchanged: what a generation holds for a file has not changed, only
/// where the bytes live.
///
/// The backfill deduplicates as it copies. `INSERT OR IGNORE` against the
/// NULL-safe unique index keeps the first payload of each identity; the
/// membership rows then join back to it, so a store with N generations of an
/// unchanged file collapses to one payload and N 16-byte rows.
pub const MIGRATION_V16_TO_V17: &str = r#"
-- Identical to the fresh-create shape above, `file_id` included. It was omitted
-- here and present there, so a store created by this build worked and a store
-- *migrated* by it could not: the INSERT below names `file_id`, and every
-- runtime probe keys on it. No test migrated a real v16 store, so 1,842 of them
-- passed over it — a fresh store never walks this step.
--
-- The column is not cosmetic. Without it the identity is content-addressed
-- alone, which collapses two byte-identical files into one payload and makes
-- both membership rows report the same path.
CREATE TABLE IF NOT EXISTS file_payloads (
    payload_id         INTEGER PRIMARY KEY,
    file_id            INTEGER NOT NULL REFERENCES paths(id),
    content_hash       INTEGER NOT NULL,
    language           TEXT NOT NULL,
    grammar_version    TEXT,
    analyzer_version   TEXT,
    parse_outcome_json TEXT NOT NULL,
    engine_json        TEXT NOT NULL,
    extraction_json    TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_file_payloads_identity
    ON file_payloads(file_id, content_hash, language,
                     COALESCE(grammar_version, ''), COALESCE(analyzer_version, ''));

-- The third payload index, and the one this step shipped without.
--
-- `DROP INDEX idx_generation_files_cache_identity` below removes v13's index,
-- whose whole job was the extraction-cache fallback's lookup *by content
-- identity alone* (`db.rs::payload_for_cache_key`) — no `file_id`, because
-- `CacheKey` has none. The surviving unique index leads with `file_id` and
-- cannot serve that query, so a migrated store full-scanned `file_payloads` on
-- every cache miss: the exact cost v13 was introduced to remove, reintroduced
-- for existing installations only, and invisible because `validate_schema`
-- checks columns and never indexes.
--
-- Fresh stores were always fine, which is why nothing caught it.
-- `migration_ladder.rs::a_migrated_store_carries_the_same_schema_as_a_fresh_one`
-- now compares the two schemas object by object, so the next index added to
-- `CREATE_SCHEMA_V3` and forgotten here fails rather than degrading quietly.
CREATE INDEX IF NOT EXISTS idx_file_payloads_cache_identity
    ON file_payloads(content_hash, language, grammar_version, analyzer_version);

CREATE TABLE IF NOT EXISTS generation_file_rows (
    generation_id INTEGER NOT NULL,
    file_id       INTEGER NOT NULL REFERENCES paths(id),
    payload_id    INTEGER NOT NULL REFERENCES file_payloads(payload_id),
    PRIMARY KEY (generation_id, file_id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_generation_file_rows_payload
    ON generation_file_rows(payload_id);

INSERT OR IGNORE INTO file_payloads
    (file_id, content_hash, language, grammar_version, analyzer_version,
     parse_outcome_json, engine_json, extraction_json)
SELECT file_id, content_hash, language, grammar_version, analyzer_version,
       parse_outcome_json, engine_json, extraction_json
  FROM generation_files;

INSERT OR IGNORE INTO generation_file_rows (generation_id, file_id, payload_id)
SELECT f.generation_id, f.file_id, p.payload_id
  FROM generation_files f
  JOIN file_payloads p
    ON p.file_id = f.file_id
   AND p.content_hash = f.content_hash
   AND p.language = f.language
   AND COALESCE(p.grammar_version, '') = COALESCE(f.grammar_version, '')
   AND COALESCE(p.analyzer_version, '') = COALESCE(f.analyzer_version, '');

DROP INDEX IF EXISTS idx_generation_files_cache_identity;
DROP TABLE generation_files;

-- `IF NOT EXISTS` to match `CREATE_SCHEMA_V3`. The `already_split` probe in
-- `db.rs` means this batch never runs against a store that has the view, so the
-- guard changes no behaviour today — it removes the asymmetry that made the
-- step's safety depend on a probe in a different file rather than on the
-- statement itself.
CREATE VIEW IF NOT EXISTS generation_files AS
SELECT m.generation_id      AS generation_id,
       m.file_id            AS file_id,
       p.language           AS language,
       p.content_hash       AS content_hash,
       p.parse_outcome_json AS parse_outcome_json,
       p.engine_json        AS engine_json,
       p.extraction_json    AS extraction_json,
       p.grammar_version    AS grammar_version,
       p.analyzer_version   AS analyzer_version
  FROM generation_file_rows m
  JOIN file_payloads p ON p.payload_id = m.payload_id;
"#;

/// v18: edges and unresolved calls live for a *range* of generations.
///
/// Measured on this repository, release kernel at schema 17, six consecutive
/// builds with one line appended to one file between each:
///
/// ```text
///   cold    store 158,416,896  generation_edges 102,078  generation_unresolved  89,743
///   edit 1  store 228,196,352  generation_edges 204,157  generation_unresolved 179,486
/// ```
///
/// **One changed file rewrote 191,822 rows and added 70 MB to the store.** What
/// actually differed between those two generations, compared NULL-safe over the
/// whole stored tuple, was **one edge and zero unresolved calls**. The rows were
/// not re-derived because they had changed; they were re-derived because the
/// relation was keyed by generation and nothing else could express "still true".
///
/// v17 solved this shape for *files* by content-addressing the payload. Edges
/// cannot get that treatment — they are deliberately never carried forward,
/// because an edge's target depends on the whole corpus and copying a prior
/// row preserves a stale answer across extractor upgrades and moved-identity
/// targets (see the comment above the edge loop in `db.rs`). A validity range
/// keeps that property exactly: the build still resolves the whole tree and
/// still compares the whole resolved tuple multiset, but the *write* is the
/// difference between that multiset and the one already valid.
///
/// `generation_edges` and `generation_unresolved` keep their names and their
/// exact column sets as views over the ranges, so all twenty-odd read sites,
/// `tools/fanout.sql`, the soak's digest (`tools/soak`), `verify.sh`'s determinism digest and
/// the tests are unchanged. `ordinal` is the row's own id: it is still unique
/// within a generation, and nothing reads it as a position any more — see
/// `edge_read_order`, whose final key was the resolver's emission ordinal and
/// is now `resolution`, the last column a reader can observe.
///
/// # What the ranges mean
///
/// A row is valid for generation `g` when `valid_from <= g AND (valid_to IS
/// NULL OR g < valid_to)`. Half-open on purpose: `valid_to` is the generation
/// that *stopped* seeing the row, so closing a row and inserting its successor
/// in the same build gives them adjacent ranges rather than an overlap, and the
/// `CHECK` refuses the inverted case outright rather than letting a reader
/// silently see nothing where a row should be.
pub const VALIDITY_RANGE_TABLES: &str = r#"
CREATE TABLE IF NOT EXISTS edge_rows (
    edge_id         INTEGER PRIMARY KEY,
    source_file_id  INTEGER NOT NULL REFERENCES paths(id),
    target_file_id  INTEGER NOT NULL REFERENCES paths(id),
    source_symbol   TEXT NOT NULL,
    target_symbol   TEXT NOT NULL,
    edge_kind       TEXT NOT NULL,
    confidence      REAL NOT NULL,
    resolution      TEXT,
    candidate_total INTEGER,
    valid_from      INTEGER NOT NULL,
    valid_to        INTEGER,
    CHECK (valid_to IS NULL OR valid_to > valid_from)
);

CREATE TABLE IF NOT EXISTS unresolved_rows (
    unresolved_id  INTEGER PRIMARY KEY,
    source_file    TEXT NOT NULL,
    source_symbol  TEXT NOT NULL,
    callee_name    TEXT NOT NULL,
    reason         TEXT NOT NULL,
    classification TEXT NOT NULL DEFAULT 'unresolved',
    receiver       TEXT,
    valid_from     INTEGER NOT NULL,
    valid_to       INTEGER,
    CHECK (valid_to IS NULL OR valid_to > valid_from)
);

-- The successors of `idx_generation_edges_source`/`_target`, which led with
-- `generation_id` because the row carried one. A range row does not, and the
-- generation is now supplied by the view's join, so the file id leads.
CREATE INDEX IF NOT EXISTS idx_edge_rows_source ON edge_rows(source_file_id);
CREATE INDEX IF NOT EXISTS idx_edge_rows_target ON edge_rows(target_file_id);

-- The prune's index, and **only** the prune's.
--
-- The design this came from called for a partial index on `valid_to IS NULL`
-- to serve the write path's diff scan. Measured, that index made the *reads*
-- 36% slower and was withdrawn. With both halves of `valid_to IS NULL OR
-- valid_to > ?` indexed, SQLite plans the reader's scan as a MULTI-INDEX OR:
--
--   |--SEARCH g USING INTEGER PRIMARY KEY (rowid=?)
--   `--MULTI-INDEX OR
--      |--SEARCH e USING INDEX idx_edge_rows_open (valid_from<?)
--      `--SEARCH e USING INDEX idx_edge_rows_closed (valid_to>?)
--
-- — 102,083 rowid lookups instead of one sequential pass, and a cold
-- `devmap impact` on this repository went 111 ms to 151 ms (p50, n=21,
-- interleaved, half-run min drift 1-2 ms). Leaving only the `IS NOT NULL` half
-- indexed makes the OR unindexable, the plan `SCAN e`, and the reader whole,
-- while the prune's `valid_to <= ?` still gets its index.
--
-- The diff scan wants every live row, so a sequential pass is the right plan
-- for it too: an index on `valid_to IS NULL` would have read the same rows in
-- rowid order through one more level of indirection.
--
-- These stay tiny by construction: with two retained generations the closed
-- set is one build's churn, and the prune empties it.
CREATE INDEX IF NOT EXISTS idx_edge_rows_closed
    ON edge_rows(valid_to) WHERE valid_to IS NOT NULL;

-- The ledger's prune index, and only the prune's, for the reason the edge
-- comment above gives.
--
-- There were two more here — `idx_unresolved_rows_callee` on `callee_name` and
-- `idx_unresolved_rows_class` on `classification` — dropped in v21 because
-- nothing read them. They are the v18 residue of
-- `idx_generation_unresolved_callee`/`_class`, which indexed
-- `(generation_id, callee_name)` and `(generation_id, classification)` on the
-- per-generation *table*. v18 turned that table into a view over
-- `unresolved_rows`, the leading `generation_id` column stopped existing, and
-- the indexes were carried across as single-column ones. What survived the
-- rename was the half that never had a query.
--
-- Confirmed two ways before removing them, because a static "no caller" is
-- weak evidence for an index: no *production* statement filters this relation
-- on either column — every statement in `db.rs` that touches it restricts on
-- `valid_to` or `unresolved_id`, reaches it through the `generation_unresolved`
-- view's join to `generations`, or counts — and `EXPLAIN QUERY PLAN` picks
-- neither index for any of them (they plan `SCAN unresolved_rows`; the prune
-- uses `_closed`). There is no ad-hoc SQL surface, and no reader outside this
-- workspace.
--
-- One statement in the tree does filter `callee_name`, and saying so is the
-- point of writing the evidence down rather than the conclusion:
-- `devmap-cli/tests/incremental_equivalence.rs` plants a deliberately wrong
-- classification on one named call. It runs once, against a store holding a
-- handful of rows, where a scan is the correct plan — an index earns nothing
-- there and would still cost a b-tree insertion on each of the 180,679 rows a
-- real build writes. A fixture is not a reader an index exists for.
--
-- They were not free. This repository writes 180,679 ledger rows on a cold
-- build, so the pair cost two b-tree insertions per row and 8 MB of a 162 MB
-- store, to serve nothing. `classification` could not have earned it in any
-- case: eight distinct values over 180,679 rows is a key that selects a
-- twentieth of the table.
--
-- **Superseded for `callee_name` in v24.** "No production statement filters
-- this relation on `callee_name`" stopped being true when `impact` began
-- listing the unresolved call sites that name its target:
-- `Store::unresolved_sites_naming` filters `WHERE u.callee_name IN (…)`, and
-- without an index `EXPLAIN QUERY PLAN` shows it as `SCAN u` over the whole
-- ledger on every `impact`. `MIGRATION_V23_TO_V24` re-adds
-- `idx_unresolved_rows_callee`; the measurement is recorded there. The
-- `classification` index stays dropped: nothing filters on it.
CREATE INDEX IF NOT EXISTS idx_unresolved_rows_closed
    ON unresolved_rows(valid_to) WHERE valid_to IS NOT NULL;

-- The one owner of the range predicate. Every reader keyed on
-- `generation_id = ?` keeps that spelling and gets the range semantics from
-- here, rather than each one carrying its own copy of
-- `valid_from <= ? AND (valid_to IS NULL OR valid_to > ?)` to get wrong
-- separately.
CREATE VIEW IF NOT EXISTS generation_edges AS
SELECT g.id            AS generation_id,
       e.edge_id       AS ordinal,
       e.source_file_id  AS source_file_id,
       e.target_file_id  AS target_file_id,
       e.source_symbol   AS source_symbol,
       e.target_symbol   AS target_symbol,
       e.edge_kind       AS edge_kind,
       e.confidence      AS confidence,
       e.resolution      AS resolution,
       e.candidate_total AS candidate_total
  FROM edge_rows e
  JOIN generations g
    ON g.id >= e.valid_from
   AND (e.valid_to IS NULL OR g.id < e.valid_to);

CREATE VIEW IF NOT EXISTS generation_unresolved AS
SELECT g.id             AS generation_id,
       u.unresolved_id  AS ordinal,
       u.source_file    AS source_file,
       u.source_symbol  AS source_symbol,
       u.callee_name    AS callee_name,
       u.reason         AS reason,
       u.classification AS classification,
       u.receiver       AS receiver
  FROM unresolved_rows u
  JOIN generations g
    ON g.id >= u.valid_from
   AND (u.valid_to IS NULL OR g.id < u.valid_to);
"#;

/// Move a v17 store's per-generation rows onto ranges, without recomputing
/// anything.
///
/// The rows are copied as they stand, each generation's set becoming the range
/// `[g, next_g)` — so a row present in two generations becomes two rows and the
/// store is no smaller the instant it migrates. That is deliberate. Collapsing
/// them would mean deciding, in SQL, which of two generations' rows are "the
/// same edge", and the answer to that is precisely what the *write* path
/// computes from a freshly resolved tuple multiset. The next build closes and
/// reclaims what has genuinely gone; the migration only changes where the rows
/// live, which is the one thing it can do without inventing an answer.
///
/// `MIN(g2.id) WHERE g2.id > e.generation_id` is NULL for the newest
/// generation, which is exactly "still valid" — the same NULL the write path
/// leaves open.
/// The two relations are moved **independently**, because a store can arrive at
/// this rung with one of them and not the other.
///
/// `test_s2_migration_v3_to_v4_preserves_cache_rows` is exactly that store: a
/// hand-built v3 fixture with an `extraction_cache` and nothing else. It walks
/// the chain, picks up `generation_unresolved` as a table at rung 9, and never
/// acquires a `generation_edges` at all — `CREATE_SCHEMA_V3` stopped creating
/// one in v18. A single "is the old shape here?" probe would have read that
/// store as already migrated and left it with no edge relation whatsoever,
/// which `validate_schema` then refuses by name at the end of the chain. Each
/// half asks about its own relation.
pub const MIGRATION_V17_TO_V18_RENAME_EDGES: &str = r#"
ALTER TABLE generation_edges RENAME TO generation_edges_v17;
"#;

pub const MIGRATION_V17_TO_V18_RENAME_UNRESOLVED: &str = r#"
ALTER TABLE generation_unresolved RENAME TO generation_unresolved_v17;
"#;

/// The edge backfill, applied after [`VALIDITY_RANGE_TABLES`] has created the
/// new shape beside the renamed original.
pub const MIGRATION_V17_TO_V18_BACKFILL_EDGES: &str = r#"
INSERT INTO edge_rows
    (source_file_id, target_file_id, source_symbol, target_symbol, edge_kind,
     confidence, resolution, candidate_total, valid_from, valid_to)
SELECT e.source_file_id, e.target_file_id, e.source_symbol, e.target_symbol,
       e.edge_kind, e.confidence, e.resolution, e.candidate_total,
       e.generation_id,
       (SELECT MIN(g.id) FROM generations g WHERE g.id > e.generation_id)
  FROM generation_edges_v17 e
 ORDER BY e.generation_id, e.ordinal;

DROP TABLE generation_edges_v17;
"#;

pub const MIGRATION_V17_TO_V18_BACKFILL_UNRESOLVED: &str = r#"
INSERT INTO unresolved_rows
    (source_file, source_symbol, callee_name, reason, classification, receiver,
     valid_from, valid_to)
SELECT u.source_file, u.source_symbol, u.callee_name, u.reason,
       u.classification, u.receiver,
       u.generation_id,
       (SELECT MIN(g.id) FROM generations g WHERE g.id > u.generation_id)
  FROM generation_unresolved_v17 u
 ORDER BY u.generation_id, u.ordinal;

DROP TABLE generation_unresolved_v17;
"#;

/// v19: each generation records, per source file, a digest of the rows it holds
/// in the two ranged relations.
///
/// v18 made the *write* the difference between the freshly resolved multiset
/// and the one already valid. It did not make the *comparison* a difference:
/// deciding which stored rows are still wanted read back every live row of
/// `edge_rows` and `unresolved_rows` and compared it field by field against the
/// resolver's output. On this repository that is 107,257 edge rows and 91,703
/// ledger rows re-read on a build that stores one, and `save_generation_timed`
/// charges it at 66% of `persist:write` — the number the v19-for-nodes note
/// below points at.
///
/// An edge belongs to its source file, and so does an unresolved call. A file
/// whose freshly resolved rows digest to what the previous generation recorded
/// holds exactly the rows already stored, so there is nothing in it to compare
/// and nothing to write. The digest is over the resolver's *output*, not over
/// the file's bytes, which is why it may be trusted where the affected set may
/// not: an edge from an unchanged file into a target whose identity moved
/// resolves differently today, its source file's digest moves with it, and the
/// comparison for that file runs. That case is the carry-forward staleness the
/// edge loop in `db.rs` refuses by construction, and it is why this rung keys
/// on what was resolved rather than on what was edited.
///
/// # Why a table of its own rather than columns on `generation_file_rows`
///
/// That row is already per file per generation and would have held the columns.
/// It is also the base table under the `generation_files` view, which every
/// payload read joins, and it is `WITHOUT ROWID` — so six more columns widen
/// the b-tree that a read walks. 2b98fef is the precedent for what a change of
/// that shape costs when it is not measured: one index on this store's hottest
/// read path cost every read 36%. A separate table is read by the write path
/// and by nothing else, so no reader's plan can change.
///
/// # What a missing row means
///
/// Absent, never assumed. There is no backfill: a v18 store migrates with an
/// empty digest table, every file reads as "unknown", and the first build after
/// the migration compares every row exactly as v18 did — then records the
/// digests it computed on the way. Absence is the safe direction at every
/// point, which is what lets this rung be additive.
pub const MIGRATION_V18_TO_V19: &str = r#"
CREATE TABLE IF NOT EXISTS generation_file_digests (
    generation_id   INTEGER NOT NULL,
    file_id         INTEGER NOT NULL REFERENCES paths(id),
    edge_rows       INTEGER NOT NULL,
    edge_lo         INTEGER NOT NULL,
    edge_hi         INTEGER NOT NULL,
    unresolved_rows INTEGER NOT NULL,
    unresolved_lo   INTEGER NOT NULL,
    unresolved_hi   INTEGER NOT NULL,
    PRIMARY KEY (generation_id, file_id)
) WITHOUT ROWID;
"#;

/// Drop the two unread indexes on the unresolved-call ledger.
///
/// See the comment beside `idx_unresolved_rows_closed` in
/// [`VALIDITY_RANGE_TABLES`] for what they were and how "nothing reads them"
/// was established. This rung is what takes them off stores that already have
/// them; removing the `CREATE INDEX` statements only stops new stores getting
/// them.
///
/// `DROP INDEX IF EXISTS`, so the rung is idempotent and a store built by a
/// binary that never created them migrates as cleanly as one that did.
///
/// The pages the indexes held return to the freelist rather than to the
/// filesystem. `auto_vacuum = INCREMENTAL` is set on every connection and
/// `vacuum_if_needed` reclaims on its own schedule, which is the same path
/// every other deletion in this store takes; a `VACUUM` here would rewrite the
/// whole database inside a migration, which is the one place it must not
/// happen.
pub const MIGRATION_V20_TO_V21: &str = r#"
DROP INDEX IF EXISTS idx_unresolved_rows_callee;
DROP INDEX IF EXISTS idx_unresolved_rows_class;
"#;

/// Intern the ledger's three repeating columns out of `unresolved_rows`.
///
/// `unresolved_rows` was 50.8 MB of a 147 MB store — a third of it — and three
/// of its columns were the same handful of strings written over and over. On
/// this repository's 181,163 rows, measured:
///
/// ```text
///   reason          130.1 B/row   23.5 MB    46,978 distinct
///   source_file      40.4 B/row    7.3 MB     1,093 distinct
///   classification   14.2 B/row    2.6 MB         8 distinct
/// ```
///
/// `source_file` becomes a `paths(id)`, which is what `edge_rows` has always
/// stored and what the ledger should have taken in v18 — the two relations now
/// key their per-file digests the same way instead of one by id and one by
/// text. `reason` and `classification` become ids into `unresolved_texts`.
///
/// **One pool for both**, not one table each. The id is what the row stores and
/// uniqueness is on the text, so a classification label that happened to equal
/// a reason would share an id and mean the same string either way. Two tables
/// would be two schemas, two prunes and two lookups for one question: "what
/// integer stands for this text".
///
/// `AUTOINCREMENT`, like `paths`, and load-bearing rather than decorative: the
/// row identity the write path compares — and the digest it folds into
/// `generation_file_digests` — is now built from these ids, so an id that was
/// retired and later handed to *different* text would make a stale digest
/// compare equal to a changed ledger. AUTOINCREMENT is what makes an id mean
/// one string for the life of the store.
///
/// The first build after this migration re-derives every digest, because the
/// stored ones were folded from text and the fresh ones are folded from ids.
/// That costs one full comparison pass and rewrites nothing: the rows still
/// match, so they are kept and only the digests are replaced. Failing that way
/// round is the safe one — a digest that no longer matches makes the next build
/// do more work, never less.
///
/// # The three `REFERENCES` are not free, and are kept anyway
///
/// `foreign_keys` is `ON` for every connection, so each of this repository's
/// 181,163 ledger rows costs three parent lookups on insert — 543,489 of them,
/// measured at 56 ms of a cold build. Dropping the three declarations takes the
/// ledger write from 215 ms to 158 ms, and that was measured rather than
/// guessed before deciding to keep them.
///
/// They are kept because of what they catch. `prune_generations_except_latest`
/// retires a `paths` row once no relation names it, as an anti-join listing
/// every such relation by hand — and v22 adds one to that list. Get that list
/// wrong and the delete succeeds, the ledger's `source_file_id` dangles, and
/// `generation_unresolved` inner-joins those rows straight out of every
/// reader's view: unresolved calls silently stop existing, which is the exact
/// shape of dishonesty this store is built to refuse. With the foreign key the
/// delete fails loudly instead.
///
/// The constraint is enforced on every write. The prune is the one exception:
/// none of the three columns is indexed, so with the constraint on, each `paths`
/// or `unresolved_texts` row it retires scans the whole ledger, and it turns off
/// `foreign_keys` for its own transaction and runs `PRAGMA foreign_key_check`
/// for those two parents before it commits (`refuse_dangling_references`). A
/// drifted anti-join still fails loudly and rolls back; it just does so once per
/// prune instead of once per retired row.
///
/// Every other relation keyed on `paths` declares the same constraint —
/// `edge_rows` twice, and it carries the cost on the same builds — so this is
/// the house rule rather than an exception made for the ledger. The cost falls
/// on cold builds alone: an incremental write inserts few rows and pays it few
/// times.
///
/// # What interning costs the *reader*
///
/// Recorded because the write-side gain is the number everyone will quote and
/// it is only half the trade. `generation_unresolved` now resolves three
/// `INTEGER PRIMARY KEY` lookups per row — path, reason, classification — that
/// the flat table did not do at all. Measured on this repository's store
/// (181,303 rows), interleaved, min of five, v20 store against v22:
///
/// | query over the view          | v20     | v22     |       |
/// |------------------------------|---------|---------|-------|
/// | `COUNT(*)`                   | 0.015 s | 0.047 s | 3.19x |
/// | project all six columns      | 0.108 s | 0.157 s | 1.45x |
/// | the same, ordered            | 0.111 s | 0.149 s | 1.34x |
///
/// `COUNT(*)` suffers most because the joins are inner joins and SQLite may
/// not omit them: every row has to be shown to have a parent even when no
/// column of that parent is selected.
///
/// Accepted, on two grounds that are facts rather than judgement. Nothing in
/// this workspace reads the view in production — `latest_unresolved` (which is
/// `LIMIT`ed) and `count_unresolved_rows` are its only callers and both are
/// reached only from tests — and no crate outside `devmap-store` touches the
/// relation at all. And the shape measured above is a whole-view scan, which
/// no caller performs.
///
/// It is written down anyway because the view's column names are the contract
/// an outside reader holds, so one may appear; if a caller ever does scan the
/// whole view, this table is where the cost was already known. Making the three
/// joins `LEFT` would let SQLite omit them and take `COUNT(*)` back, and is
/// deliberately not done: an inner join makes a row with a dangling id vanish
/// and fail a count, where a left join would hand the reader a plausible
/// `NULL`.
pub const MIGRATION_V21_TO_V22: &str = r#"
CREATE TABLE IF NOT EXISTS unresolved_texts (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    text TEXT NOT NULL UNIQUE
);

CREATE TABLE unresolved_rows_v22 (
    unresolved_id     INTEGER PRIMARY KEY,
    source_file_id    INTEGER NOT NULL REFERENCES paths(id),
    source_symbol     TEXT NOT NULL,
    callee_name       TEXT NOT NULL,
    reason_id         INTEGER NOT NULL REFERENCES unresolved_texts(id),
    classification_id INTEGER NOT NULL REFERENCES unresolved_texts(id),
    receiver          TEXT,
    valid_from        INTEGER NOT NULL,
    valid_to          INTEGER,
    CHECK (valid_to IS NULL OR valid_to > valid_from)
);

INSERT OR IGNORE INTO unresolved_texts (text) SELECT DISTINCT reason FROM unresolved_rows;
INSERT OR IGNORE INTO unresolved_texts (text) SELECT DISTINCT classification FROM unresolved_rows;
INSERT OR IGNORE INTO paths (path) SELECT DISTINCT source_file FROM unresolved_rows;

INSERT INTO unresolved_rows_v22
    (unresolved_id, source_file_id, source_symbol, callee_name,
     reason_id, classification_id, receiver, valid_from, valid_to)
SELECT u.unresolved_id, p.id, u.source_symbol, u.callee_name,
       r.id, c.id, u.receiver, u.valid_from, u.valid_to
  FROM unresolved_rows u
  JOIN paths p            ON p.path = u.source_file
  JOIN unresolved_texts r ON r.text = u.reason
  JOIN unresolved_texts c ON c.text = u.classification;

DROP VIEW IF EXISTS generation_unresolved;
DROP TABLE unresolved_rows;
ALTER TABLE unresolved_rows_v22 RENAME TO unresolved_rows;

CREATE INDEX IF NOT EXISTS idx_unresolved_rows_closed
    ON unresolved_rows(valid_to) WHERE valid_to IS NOT NULL;

CREATE VIEW generation_unresolved AS
SELECT g.id             AS generation_id,
       u.unresolved_id  AS ordinal,
       p.path           AS source_file,
       u.source_symbol  AS source_symbol,
       u.callee_name    AS callee_name,
       r.text           AS reason,
       c.text           AS classification,
       u.receiver       AS receiver
  FROM unresolved_rows u
  JOIN generations g
    ON g.id >= u.valid_from
   AND (u.valid_to IS NULL OR g.id < u.valid_to)
  JOIN paths p            ON p.id = u.source_file_id
  JOIN unresolved_texts r ON r.id = u.reason_id
  JOIN unresolved_texts c ON c.id = u.classification_id;
"#;

/// The schema this binary writes.
///
/// # Why there is no v20 putting the nodes on ranges
///
/// **Superseded by v28, which did it.** This section is kept because the
/// measurement it records is what v28 had to beat, not because its conclusion
/// stands. The 54 ms write and 39 ms prune it prices predate
/// `generation_literals` (v26), a relation of 172,040 rows per retained
/// generation on this repository; measured on the v27 binary against a
/// `git archive` of this repository, a steady-state one-file edit spent 0.21 s
/// of `persist:write` copying literals and 0.57 s in
/// `persist:prune_generations` deleting the previous generation's copy of every
/// relation. The "re-keyed full-text index on the hottest read" it feared is the
/// `payload_id << 24 | ord_in_file` rowid of [`MIGRATION_V27_TO_V28`], which the
/// readers decode in one place. The test that pinned the state described below,
/// `an_incremental_build_still_copies_every_node_and_full_text_row`, is now
/// `a_one_file_edit_writes_one_files_worth_of_rows_to_every_relation`.
///
/// v18 ranged the edges and the unresolved ledger and left `generation_nodes`,
/// `nodes_fts`/`nodes_fts_map`, `generation_file_rows`, `generation_dead_symbols`
/// and `generation_coverage_gaps` as full per-generation copies. The obvious
/// next rung is to give the nodes and the full-text map the same treatment,
/// and it was designed and then declined on a measurement rather than on
/// taste. Recorded here because the argument for doing it is visible in the
/// schema and the argument against it is not.
///
/// Measured on a `git archive` corpus of this repository — 1,608 files, 18,501
/// symbols, 106,420 edges — release binary, one-file incremental builds, p50.
/// `persist:write` is 304 ms, and `save_generation_timed` charges it:
///
/// | relation      | ms  | share |
/// |---------------|-----|-------|
/// | `unresolved`  | 118 | 39%   |
/// | `edges`       |  81 | 27%   |
/// | `fts`         |  34 | 11%   |
/// | `nodes`       |  20 | 6.6%  |
/// | everything else | 33 | 11%  |
///
/// The prune that follows costs a further 78 ms, of which 39 ms is the three
/// node relations' `DELETE`s (31 ms of it `nodes_fts`, timed statement by
/// statement against a byte copy of the store).
///
/// So the whole of what ranging the nodes and the full-text map could return
/// is **54 ms of the write plus 39 ms of the prune — 93 ms of a 1,155 ms
/// build**, and that is an upper bound: a ranged write still inserts the
/// changed files' nodes, still closes the replaced ranges, and a ranged
/// `nodes_fts` still deletes rows as ranges fall out of retention.
///
/// Against that: `nodes_fts` is an FTS5 virtual table whose rowid is
/// `(generation << 32) | ordinal`, and `latest_search_page` joins
/// `nodes_fts.rowid & 4294967295` back to `generation_nodes.ordinal`. A row
/// valid across a range of generations cannot carry a generation in its rowid,
/// so ranging the nodes means re-keying the full-text index and rewriting that
/// join — on the hottest read in the store. 2b98fef is the precedent for the
/// risk: one index on exactly this shape of range predicate cost every read
/// 36%, and it was found only because it was measured.
///
/// A rung that returns 8% of a build for a re-keyed full-text index is not
/// worth its migration, and a half-applied one is worse than the copy it
/// replaces. `an_incremental_build_still_copies_every_node_and_full_text_row`
/// pins the state this describes, so the next person to reach for v19 has to
/// come past this note rather than rediscover it.
///
/// **The number that would justify a rung is elsewhere.** The two relations
/// that dominate the write are the two already ranged, and their cost is not
/// copying: it is the diff scan reading back 106,420 edge rows and 89,743
/// ledger rows on every build to decide what is still valid. That is 66% of
/// `persist:write`, and no further ranging touches it.
///
/// v19 is that rung — [`MIGRATION_V18_TO_V19`] — and it went where this
/// paragraph pointed rather than where the schema's shape suggested.
/// Durable queue identity and revisions replace wall-clock acknowledgement.
/// The epoch prevents a claim from one store acknowledging another store's
/// identically named path. A counter survives deletion of the last queue row.
pub const MIGRATION_V19_TO_V20: &str = r#"
ALTER TABLE pending_paths ADD COLUMN revision INTEGER NOT NULL DEFAULT 0
    CHECK (typeof(revision) = 'integer' AND revision >= 0);
CREATE TABLE pending_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    epoch TEXT NOT NULL CHECK (length(epoch) = 32),
    revision INTEGER NOT NULL CHECK (typeof(revision) = 'integer' AND revision >= 0),
    repo_root TEXT
);
INSERT INTO pending_state (singleton, epoch, revision, repo_root)
VALUES (1, lower(hex(randomblob(16))), 1,
        (SELECT repo_root FROM generations ORDER BY id DESC LIMIT 1));
UPDATE pending_paths SET revision = 1;
"#;

/// v22 → v23 advances `user_version` only. The bump exists so an older binary
/// that does not know [`ResolutionKind::LanguageServer`] /
/// [`LanguageServerDispatch`] refuses the store rather than reconstructing
/// those rows as neighbouring tiers. No DDL changes.
pub const MIGRATION_V22_TO_V23: &str = "";

/// Re-add the `callee_name` index v21 dropped, because a reader now needs it.
///
/// `impact` lists the unresolved call sites that name its target, through
/// `Store::unresolved_sites_naming` — `WHERE u.callee_name IN (…)` against the
/// whole ledger. v21 dropped `idx_unresolved_rows_callee` when nothing filtered
/// on that column (see the comment beside `idx_unresolved_rows_closed`); the
/// reader arrived afterwards, and the comment went on asserting otherwise.
///
/// Query plan for that statement, recorded on this repository's store
/// (155,711 ledger rows):
///
/// ```text
/// before:  SCAN u
///          SEARCH p USING INTEGER PRIMARY KEY (rowid=?)
///          SEARCH c USING INTEGER PRIMARY KEY (rowid=?)
///          USE TEMP B-TREE FOR ORDER BY
/// after:   SEARCH u USING INDEX idx_unresolved_rows_callee (callee_name=?)
///          SEARCH p USING INTEGER PRIMARY KEY (rowid=?)
///          SEARCH c USING INTEGER PRIMARY KEY (rowid=?)
///          USE TEMP B-TREE FOR LAST 3 TERMS OF ORDER BY
/// ```
///
/// `devmap-query/examples/unresolved_callee_index_ab.rs`, release, two copies of
/// that store differing only in this index, answers checked identical, 21
/// interleaved rounds of five `impact` calls: min 36.50 ms → 11.74 ms, p50
/// 37.77 ms → 12.69 ms, so 7.55 ms → 2.54 ms per call at p50; a second run gave
/// 8.55 ms → 3.00 ms. The index adds 2.4 MB (1.6%) to the file and took 42 to
/// 88 ms to build over the existing rows.
///
/// What it costs a cold build, measured 2026-10-07 with
/// `benchmarks/cold_build_ab.py`: two release binaries differing only in this
/// statement (the patch is under
/// `benchmarks/results/competition/20261007-build-cost/patches/`), real
/// `devmap build --full` of a frozen DevCouncil tree (1,310 files, 163,126
/// unresolved calls) into a new store, 12 ABBA rounds, identical counts on
/// both arms. With the index → without: `persist:write`'s unresolved part min
/// 378 → 263 ms, median 505 → 445 ms; `persist:write` min 906 → 814 ms; the
/// whole build min 2.89 → 2.93 s, inside the noise (1-minute load 30–45 on 18
/// cores). So the index costs about 60–115 ms of a cold persist and 2.83 MB of
/// store, against 5.0 ms saved on every `impact` call at p50.
pub const MIGRATION_V23_TO_V24: &str = r#"
CREATE INDEX IF NOT EXISTS idx_unresolved_rows_callee
    ON unresolved_rows(callee_name);
"#;

/// v24 → v25 advances `user_version` only. The bump exists so an older binary
/// that does not know `EdgeKind::Registers` — route middleware, stored as the
/// `Registers` spelling in the free-TEXT `edge_kind` column — refuses the store
/// at open with a schema message, rather than failing every edge read with
/// `UnknownEdgeKind` once a build has written one. No DDL changes. (Written as
/// v23 → v24 on its branch; renumbered at merge because main had already
/// claimed v24 for the `callee_name` index.)
pub const MIGRATION_V24_TO_V25: &str = "";

/// String literals and the symbol that encloses each one.
///
/// A name query cannot see a string-keyed protocol: event actions, IPC
/// command names, config keys. The coupling is the value, not a call edge.
/// `value` is compared with `=` and a byte-range prefix, never `LIKE`, so the
/// match stays case-sensitive. The index is `(generation_id, value)` because
/// that is the predicate both the count and the page lead with.
pub const MIGRATION_V25_TO_V26: &str = r#"
CREATE TABLE IF NOT EXISTS generation_literals (
    generation_id INTEGER NOT NULL,
    file_id INTEGER NOT NULL,
    line INTEGER NOT NULL,
    span_start INTEGER NOT NULL,
    value TEXT NOT NULL,
    qualified_name TEXT NOT NULL,
    symbol_name TEXT NOT NULL,
    PRIMARY KEY (generation_id, file_id, span_start, value, qualified_name)
);
CREATE INDEX IF NOT EXISTS idx_generation_literals_value
    ON generation_literals (generation_id, value);
"#;

/// v27: three nullable columns on `generation_dead_symbols` carrying the site
/// evidence an unconfirmed dead row names — how many unbound sites name the
/// symbol and where the first one is. Additive and never selected by an older
/// reader, so the reader floor stays where v26 left it.
///
/// Added by `ALTER TABLE` on the fresh path too, because the table itself is
/// created by the v3 batch the fresh path also runs: declaring the columns in
/// `CREATE TABLE` as well would make the fresh path's `ALTER` fail.
pub const MIGRATION_V26_TO_V27: &str = r#"
ALTER TABLE generation_dead_symbols ADD COLUMN evidence_sites INTEGER;
ALTER TABLE generation_dead_symbols ADD COLUMN evidence_path TEXT;
ALTER TABLE generation_dead_symbols ADD COLUMN evidence_byte INTEGER;
"#;

/// Bits of a symbol's identity that say *where in its payload it is*.
///
/// A symbol is `(payload_id, ord_in_file)`. The pair is packed into one integer
/// as `payload_id << NODE_ORD_BITS | ord_in_file`, which is at once the row's
/// `nodes_fts` rowid and the `ordinal` the `generation_nodes` view reports, so
/// the full-text index and the symbol table are joined by arithmetic on a key
/// that no generation appears in. 24 bits is 16,777,216 symbols in one file;
/// the extractor's one-MiB input limit cannot reach a tenth of that, and a
/// write that did would refuse loudly rather than wrap into a neighbour's id.
pub const NODE_ORD_BITS: u32 = 24;

/// One past the largest `ord_in_file` a payload may hold.
pub const MAX_NODES_PER_PAYLOAD: u32 = 1 << NODE_ORD_BITS;

/// The indexes earlier schemas declared that v28 removed along with the tables
/// they indexed, and whose `CREATE INDEX` is still in a batch of
/// [`FRESH_SCHEMA_BATCHES`].
///
/// The fresh schema is the old batches followed by [`MIGRATION_V27_TO_V28`], so
/// that text would be both demanded by the index gate and replayed against a
/// view by `heal_declared_indexes`. Named here once and filtered out of both.
/// `idx_generation_nodes_file` and `idx_generation_file_rows_payload` are not
/// listed because v28 took their statements out of the batches themselves: the
/// v3 and v4 rungs re-run `CREATE_SCHEMA_V3` over stores that already carry the
/// current shape, where those names are views.
pub const INDEXES_REMOVED_BY_V28: &[&str] = &[
    "idx_generation_literals_value",
    "idx_file_payloads_identity",
];

/// v28: symbols, literals and full-text rows belong to a *payload*, and a
/// file's membership and digests live for a *range* of generations.
///
/// Measured on this repository (2,816 files, 25,424 symbols), release kernel at
/// schema 27, one appended line to one file between builds:
///
/// ```text
///   generation_literals   172,040 rows for generation 2, the same 172,040 for 3
///   edge_rows              96,928 rows in all, one changed
///   a one-file edit        0.21 s copying literals, 0.57 s pruning the
///                          previous generation's copy of every relation
/// ```
///
/// v17 solved this shape for the payload's JSON by keying it on the file and
/// the extractor identity; the relations *derived from* one file's payload were
/// left keyed by generation and copied wholesale on every build. They depend on
/// nothing but that payload, so they are keyed by it now:
///
/// * `payload_nodes(payload_id, ord_in_file, ...)` and
///   `payload_literals(payload_id, ...)`, and the `nodes_fts` rows, are written
///   once, when the payload is. A payload is the same file at the same content
///   under the same extractor *and with the same output*: the identity carries
///   `payload_digest`, a 128-bit digest of the three documents the row holds,
///   because the extractor's version strings name a binary and not what it
///   produced. Two outputs for one key are two payloads, so the rows a retained
///   generation reads are never rewritten underneath it, and a payload's rows
///   are the extraction's by identity. Fable ruling 1 (2026-10-29): the first
///   v28 keyed on the version strings alone and kept a stored payload's rows
///   whenever the symbol count matched.
/// * `file_rows(file_id, payload_id, valid_from, valid_to)` is the membership
///   relation as a validity range, exactly as `edge_rows` is for edges. An
///   unchanged file's row stays open and nothing is written for it.
/// * `digest_rows` does the same for `generation_file_digests`.
///
/// The old relations keep their names and column sets as views, the v17/v18
/// playbook, so every reader that asks "what did generation g hold" is
/// unchanged. Two things a view cannot keep: the `nodes_fts` rowid, which no
/// longer carries a generation, and `ordinal`, which is now
/// `payload_id << 24 | ord_in_file` instead of a position in the generation.
/// Both are re-derived in one place (`Store::fts_rowid`), and the readers that
/// joined on the old encoding were rewritten against it. That is also why the
/// reader floor is raised to 28: a v27 reader would join the new rowids by the
/// old arithmetic and answer from the wrong rows without an error.
///
/// `generation_dead_symbols` and `generation_coverage_gaps` stay
/// per-generation. They are whole-graph results rather than derivations of one
/// file, and at 8k and 60 rows they are not what a one-file edit pays for.
///
/// # Row order is preserved by `file_rows.row_id`
///
/// The v27 write order put a build's carried files first and its fresh files
/// after, so `ORDER BY ordinal` was "unchanged files, then what this build
/// rewrote", and semantic ranking and clone grouping consume symbols in it.
/// `file_rows.row_id` is allocated when a membership row is opened, so
/// `ORDER BY row_id, ord_in_file` is that same order, including after a revert
/// (a reverted file gets a new row and goes last, as it did before).
///
/// # The migration
///
/// One transaction, so a crash leaves the v27 store whole. File rows become the
/// maximal runs of consecutive generations that held the same payload for a
/// file, ordered by `(valid_from, position in that generation)` so `row_id`
/// reproduces v27's write order. Nodes and literals are copied once per
/// payload from the newest generation that names it, `ord_in_file` being the
/// rank of the v27 ordinal within the file. `nodes_fts` is dropped and rebuilt
/// under the new rowids. Digests are collapsed into runs the same way. The
/// retained generations' relation sizes are compared before and after by the
/// caller; a difference refuses the migration.
pub const MIGRATION_V27_TO_V28: &str = r#"
-- The payload's identity gains the digest of what was extracted. Existing rows
-- read as the empty digest here and are given their real one by
-- `Store::migrate_to_v28` straight after this batch, which is Rust because SQL
-- has no hash of its own to agree with the writer's. The unique index replaces
-- `idx_file_payloads_identity` under a new name: two statements declaring one
-- name with different definitions is the contradiction the index-gate test
-- exists to refuse.
ALTER TABLE file_payloads ADD COLUMN payload_digest BLOB NOT NULL DEFAULT x'';

DROP INDEX IF EXISTS idx_file_payloads_identity;

CREATE UNIQUE INDEX IF NOT EXISTS idx_file_payloads_digest_identity
    ON file_payloads(file_id, content_hash, language,
                     COALESCE(grammar_version, ''), COALESCE(analyzer_version, ''),
                     payload_digest);

CREATE TABLE IF NOT EXISTS payload_nodes (
    payload_id      INTEGER NOT NULL REFERENCES file_payloads(payload_id),
    ord_in_file     INTEGER NOT NULL CHECK (ord_in_file >= 0 AND ord_in_file < 16777216),
    name            TEXT NOT NULL,
    qualified_name  TEXT NOT NULL,
    kind            TEXT NOT NULL,
    span_start      INTEGER NOT NULL,
    span_end        INTEGER NOT NULL,
    is_exported     INTEGER NOT NULL,
    body_exact      INTEGER,
    body_structural INTEGER,
    body_nodes      INTEGER,
    PRIMARY KEY (payload_id, ord_in_file)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS payload_literals (
    payload_id     INTEGER NOT NULL REFERENCES file_payloads(payload_id),
    line           INTEGER NOT NULL,
    span_start     INTEGER NOT NULL,
    value          TEXT NOT NULL,
    qualified_name TEXT NOT NULL,
    symbol_name    TEXT NOT NULL,
    PRIMARY KEY (payload_id, span_start, value, qualified_name)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_payload_literals_value
    ON payload_literals (value);

CREATE TABLE IF NOT EXISTS file_rows (
    row_id     INTEGER PRIMARY KEY,
    file_id    INTEGER NOT NULL REFERENCES paths(id),
    payload_id INTEGER NOT NULL REFERENCES file_payloads(payload_id),
    valid_from INTEGER NOT NULL,
    valid_to   INTEGER,
    CHECK (valid_to IS NULL OR valid_to > valid_from)
);

-- At most one open row per file, and at most one row per file closed at a given
-- generation. Not a partial index: the v18 note beside `idx_edge_rows_closed`
-- records what a `valid_to IS NULL` index did to the readers' plan. The
-- expression keeps the index total, so the planner cannot offer it for the
-- range predicate, and the write path reads the open rows with one scan of a
-- table that has one row per file.
CREATE UNIQUE INDEX IF NOT EXISTS idx_file_rows_file
    ON file_rows (file_id, COALESCE(valid_to, -1));

CREATE INDEX IF NOT EXISTS idx_file_rows_payload
    ON file_rows (payload_id);

CREATE TABLE IF NOT EXISTS digest_rows (
    row_id          INTEGER PRIMARY KEY,
    file_id         INTEGER NOT NULL REFERENCES paths(id),
    edge_rows       INTEGER NOT NULL,
    edge_lo         INTEGER NOT NULL,
    edge_hi         INTEGER NOT NULL,
    unresolved_rows INTEGER NOT NULL,
    unresolved_lo   INTEGER NOT NULL,
    unresolved_hi   INTEGER NOT NULL,
    valid_from      INTEGER NOT NULL,
    valid_to        INTEGER,
    CHECK (valid_to IS NULL OR valid_to > valid_from)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_digest_rows_file
    ON digest_rows (file_id, COALESCE(valid_to, -1));

INSERT INTO file_rows (file_id, payload_id, valid_from, valid_to)
WITH g AS (
    SELECT id, ROW_NUMBER() OVER (ORDER BY id) AS seq FROM generations
), m AS (
    SELECT r.file_id AS file_id, r.payload_id AS payload_id, g.id AS gid,
           g.seq - ROW_NUMBER() OVER (
               PARTITION BY r.file_id, r.payload_id ORDER BY g.seq) AS island
      FROM generation_file_rows r
      JOIN g ON g.id = r.generation_id
), isl AS (
    SELECT file_id, payload_id, MIN(gid) AS vf, MAX(gid) AS vl
      FROM m GROUP BY file_id, payload_id, island
)
SELECT i.file_id, i.payload_id, i.vf,
       (SELECT MIN(g2.id) FROM generations g2 WHERE g2.id > i.vl)
  FROM isl i
  LEFT JOIN (SELECT generation_id AS gid, file_id AS fid, MIN(ordinal) AS first_ord
               FROM generation_nodes GROUP BY generation_id, file_id) o
         ON o.gid = i.vf AND o.fid = i.file_id
 ORDER BY i.vf, COALESCE(o.first_ord, 9223372036854775807), i.file_id;

-- Nodes and literals are walked from the per-generation side and looked up in
-- membership by its primary key, `(generation_id, file_id)`. The other direction
-- would probe the nodes by `(generation_id, file_id)`, and the index that
-- served that probe, `idx_generation_nodes_file`, is not on every store that
-- reaches this rung.
INSERT INTO payload_nodes
    (payload_id, ord_in_file, name, qualified_name, kind, span_start, span_end,
     is_exported, body_exact, body_structural, body_nodes)
SELECT r.payload_id,
       ROW_NUMBER() OVER (PARTITION BY r.payload_id ORDER BY n.ordinal) - 1,
       n.name, n.qualified_name, n.kind, n.span_start, n.span_end, n.is_exported,
       n.body_exact, n.body_structural, n.body_nodes
  FROM generation_nodes n
  CROSS JOIN generation_file_rows r
          ON r.generation_id = n.generation_id AND r.file_id = n.file_id
  CROSS JOIN (SELECT payload_id, MAX(generation_id) AS gid
                FROM generation_file_rows GROUP BY payload_id) s
          ON s.payload_id = r.payload_id AND s.gid = r.generation_id;

INSERT INTO payload_literals
    (payload_id, line, span_start, value, qualified_name, symbol_name)
SELECT r.payload_id, l.line, l.span_start, l.value, l.qualified_name, l.symbol_name
  FROM generation_literals l
  CROSS JOIN generation_file_rows r
          ON r.generation_id = l.generation_id AND r.file_id = l.file_id
  CROSS JOIN (SELECT payload_id, MAX(generation_id) AS gid
                FROM generation_file_rows GROUP BY payload_id) s
          ON s.payload_id = r.payload_id AND s.gid = r.generation_id;

INSERT INTO digest_rows
    (file_id, edge_rows, edge_lo, edge_hi, unresolved_rows, unresolved_lo,
     unresolved_hi, valid_from, valid_to)
WITH g AS (
    SELECT id, ROW_NUMBER() OVER (ORDER BY id) AS seq FROM generations
), m AS (
    SELECT d.file_id AS file_id, d.edge_rows AS edge_rows, d.edge_lo AS edge_lo,
           d.edge_hi AS edge_hi, d.unresolved_rows AS unresolved_rows,
           d.unresolved_lo AS unresolved_lo, d.unresolved_hi AS unresolved_hi,
           g.id AS gid,
           g.seq - ROW_NUMBER() OVER (
               PARTITION BY d.file_id, d.edge_rows, d.edge_lo, d.edge_hi,
                            d.unresolved_rows, d.unresolved_lo, d.unresolved_hi
               ORDER BY g.seq) AS island
      FROM generation_file_digests d
      JOIN g ON g.id = d.generation_id
)
SELECT file_id, edge_rows, edge_lo, edge_hi, unresolved_rows, unresolved_lo,
       unresolved_hi, MIN(gid),
       (SELECT MIN(g2.id) FROM generations g2 WHERE g2.id > MAX(m.gid))
  FROM m
 GROUP BY file_id, edge_rows, edge_lo, edge_hi, unresolved_rows, unresolved_lo,
          unresolved_hi, island;

DROP TABLE generation_literals;
DROP TABLE generation_nodes;
DROP TABLE generation_file_digests;
DROP TABLE generation_file_rows;
DROP TABLE nodes_fts_map;
DROP TABLE nodes_fts;

CREATE VIRTUAL TABLE nodes_fts USING fts5(
    name, qualified_name, path, tokenize='unicode61'
);

-- The one owner of the range predicate, as for `generation_edges`.
CREATE VIEW generation_file_rows AS
SELECT g.id         AS generation_id,
       r.file_id    AS file_id,
       r.payload_id AS payload_id
  FROM file_rows r
  JOIN generations g
    ON g.id >= r.valid_from
   AND (r.valid_to IS NULL OR g.id < r.valid_to);

CREATE VIEW generation_nodes AS
SELECT g.id AS generation_id,
       (n.payload_id << 24) | n.ord_in_file AS ordinal,
       r.file_id         AS file_id,
       n.name            AS name,
       n.qualified_name  AS qualified_name,
       n.kind            AS kind,
       n.span_start      AS span_start,
       n.span_end        AS span_end,
       n.is_exported     AS is_exported,
       n.body_exact      AS body_exact,
       n.body_structural AS body_structural,
       n.body_nodes      AS body_nodes
  FROM file_rows r
  JOIN generations g
    ON g.id >= r.valid_from
   AND (r.valid_to IS NULL OR g.id < r.valid_to)
  JOIN payload_nodes n ON n.payload_id = r.payload_id;

CREATE VIEW nodes_fts_map AS
SELECT (n.payload_id << 24) | n.ord_in_file AS rowid_ref,
       g.id                                 AS generation_id
  FROM file_rows r
  JOIN generations g
    ON g.id >= r.valid_from
   AND (r.valid_to IS NULL OR g.id < r.valid_to)
  JOIN payload_nodes n ON n.payload_id = r.payload_id;

CREATE VIEW generation_literals AS
SELECT g.id          AS generation_id,
       r.file_id     AS file_id,
       l.line        AS line,
       l.span_start  AS span_start,
       l.value       AS value,
       l.qualified_name AS qualified_name,
       l.symbol_name AS symbol_name
  FROM file_rows r
  JOIN generations g
    ON g.id >= r.valid_from
   AND (r.valid_to IS NULL OR g.id < r.valid_to)
  JOIN payload_literals l ON l.payload_id = r.payload_id;

CREATE VIEW generation_file_digests AS
SELECT g.id              AS generation_id,
       d.file_id         AS file_id,
       d.edge_rows       AS edge_rows,
       d.edge_lo         AS edge_lo,
       d.edge_hi         AS edge_hi,
       d.unresolved_rows AS unresolved_rows,
       d.unresolved_lo   AS unresolved_lo,
       d.unresolved_hi   AS unresolved_hi
  FROM digest_rows d
  JOIN generations g
    ON g.id >= d.valid_from
   AND (d.valid_to IS NULL OR g.id < d.valid_to);

INSERT INTO nodes_fts (rowid, name, qualified_name, path)
SELECT (n.payload_id << 24) | n.ord_in_file, n.name, n.qualified_name, p.path
  FROM payload_nodes n
  JOIN file_payloads fp ON fp.payload_id = n.payload_id
  JOIN paths p ON p.id = fp.file_id
 ORDER BY n.payload_id, n.ord_in_file;
"#;

/// v29: a payload's extraction document is stored zstd-compressed.
///
/// Measured on this repository's own store at schema 28 (1,896 payloads):
/// `file_payloads` is 196 MB of 349 MB, and all but 0.15 MB of that is
/// `extraction_json`, ~106 KB a row as JSON text. Per-row zstd takes it ~11x
/// smaller; the measurements that decided whether that is worth a schema bump
/// are in `docs/devmap/design/store-write-cost-proposal.md`, Appendix C.
///
/// What the bump changes, and what it does not:
///
/// * `extraction_json` keeps its name and its `TEXT NOT NULL` declaration, and
///   `generation_files` keeps exposing it, but a v29 row holds a zstd frame as a
///   `BLOB`. SQLite does not convert a blob stored in a `TEXT` column, so no DDL
///   is needed, and the storage class tells the two forms apart (see
///   [`crate::payload_codec`]). That is also why the floor is raised: a v28
///   reader would read the blob with `get::<String>`, and while that fails
///   loudly the failure is "invalid column type" and not "this store is newer".
/// * Payload identity is unchanged. `payload_digest` covers the JSON text, taken
///   before compression.
/// * `json_extract` cannot see inside a compressed value, and
///   `latest_test_entry_symbols` ran `json_extract(extraction_json, '$.wiring')`
///   on every hook-driven `affected_tests`. What it needs is promoted into
///   `entry_wiring_json`: the `RuntimeEntryPoint` wiring annotations as a JSON
///   array, written beside the payload, so the query reads a few bytes and
///   decompresses nothing. `NULL` means "not computed" (a row a foreign writer
///   inserted) and the reader falls back to decoding that one document; `'[]'`
///   means "computed, none".
///
/// # The migration
///
/// One transaction, like every rung. Each uncompressed payload is read,
/// compressed, and written back with its `entry_wiring_json`, one row at a time
/// so the migration holds one document and not the store's. The rung is guarded
/// on the shape and the pass selects only what is not yet converted, so a
/// store re-entered after a crash, or one that a writer left half converted,
/// converges: the same pass runs at every writer open (`Store::heal_extraction_encoding`) and
/// costs one probe when there is nothing to do. The pages the rewrite frees go
/// back to the filesystem through the incremental vacuum the store already
/// uses.
pub const MIGRATION_V28_TO_V29: &str = r#"
ALTER TABLE file_payloads ADD COLUMN entry_wiring_json TEXT;
"#;

pub const CURRENT_SCHEMA_VERSION: i32 = 29;

/// The oldest reader schema that can read a store this binary writes.
///
/// `user_version` alone forced an exact match on every reader, so each bump —
/// including v26, which only *added* a table no older reader selects — blinded
/// every embedding reader (GitPulse links `devmap-store` read-only) until it was
/// re-vendored. The writer is the only party that knows whether a bump is safe
/// for an older reader, so the writer says so, in the store, through
/// [`READER_COMPAT_TABLE`]; [`Store::open_read_only`](crate::Store::open_read_only)
/// admits a store newer than itself only when this floor reaches down to it.
///
/// When raising [`CURRENT_SCHEMA_VERSION`], add a row to
/// [`SCHEMA_READER_FLOORS`] and set this to its floor:
///
/// * **keep the previous floor** when the bump only adds tables, indexes or
///   nullable columns that older readers never select (v26:
///   `generation_literals`; v27: the dead-row evidence columns);
/// * **raise it to the new version** when the bump changes what an existing
///   column means, writes a value an older binary would misdecode, or removes
///   or renames anything an older reader selects. v23 and v25 were exactly
///   this — version-only bumps that exist so an older binary *refuses* rather
///   than reconstructing `LanguageServer` rows or `Registers` edges as
///   something else. v28 is this too, and not for want of compatible views:
///   the `nodes_fts` rowid no longer carries a generation, so a v27 reader
///   joining it by `rowid & 4294967295` would resolve a payload's symbols to
///   the wrong rows and answer without an error. v29 is this as well: it
///   changes the storage class of `file_payloads.extraction_json` from `TEXT`
///   to a compressed `BLOB`, which no earlier binary can decode.
///
/// Writers stay exact-match: a binary never migrates or writes a store newer
/// than itself, whatever this floor says.
pub const MIN_READER_SCHEMA_VERSION: i32 = 29;

/// `(schema, floor)` for every schema since the floor was introduced, oldest
/// first. The last row must be `(CURRENT_SCHEMA_VERSION,
/// MIN_READER_SCHEMA_VERSION)`; a unit test holds that, so bumping the schema
/// without deciding the new floor fails rather than silently inheriting one.
pub const SCHEMA_READER_FLOORS: &[(i32, i32)] = &[(26, 25), (27, 25), (28, 28), (29, 29)];

/// Where a writer records [`MIN_READER_SCHEMA_VERSION`].
///
/// Added without a `user_version` bump, on purpose: the table is invisible to
/// every reader that does not look for it, so a schema-26 binary that predates
/// it reads such a store exactly as before. A store without the row — written
/// before this table existed — is read with the exact-match rule it always had.
pub const READER_COMPAT_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS reader_compat (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    min_reader_schema INTEGER NOT NULL
        CHECK (typeof(min_reader_schema) = 'integer' AND min_reader_schema >= 3)
);
"#;

/// The `user_version` the Python engine's `index.sqlite` carries — a database
/// this kernel never wrote and cannot read. Named once, here, so the store's
/// refusal and the CLI's `status` report the same number for the same file
/// (the CLI spelled its own `2` until 2026-09-07).
pub const PYTHON_INDEX_SCHEMA_VERSION: i32 = 2;

/// Every DDL batch a fresh store applies, in the order `Store::migrate` applies
/// them.
///
/// One owner for "what the current schema is". The create path names these
/// constants and so does [`declared_index_names`], so the gate cannot come to
/// assert a schema the creator does not build. `MIGRATION_V6_TO_V7` is here
/// because a fresh store really does run it — probed, because `ADD COLUMN` is
/// not idempotent — and leaving it out would make this list a near-copy of the
/// truth rather than the truth.
///
/// `UNRESOLVED_TABLE` left this list in v18. It is still `MIGRATION_V8_TO_V9`
/// and an old store still walks it, but a *fresh* store no longer applies it:
/// `generation_unresolved` is a view over `unresolved_rows` now, and a
/// `CREATE INDEX` naming a view is an error rather than a no-op. Leaving the
/// constant listed here would have made [`declared_index_names`] demand two
/// indexes v18 replaces, which `validate_schema` would then refuse every store
/// for.
pub const FRESH_SCHEMA_BATCHES: &[&str] = &[
    CREATE_SCHEMA_V3,
    BUILD_HISTORY_TABLE,
    MIGRATION_V6_TO_V7,
    VALIDITY_RANGE_TABLES,
    COVERAGE_GAPS_TABLE,
    MIGRATION_V18_TO_V19,
    MIGRATION_V19_TO_V20,
    MIGRATION_V20_TO_V21,
    MIGRATION_V21_TO_V22,
    // Empty: v23 only stamps `user_version`. Listed so a fresh store and a
    // migrated one land on the same version by the same batch list.
    MIGRATION_V22_TO_V23,
    // After `MIGRATION_V20_TO_V21`, which drops this same index: the order is
    // the ladder's, so a fresh store ends where a migrated one does.
    MIGRATION_V23_TO_V24,
    // Empty: v25 only stamps `user_version`.
    MIGRATION_V24_TO_V25,
    MIGRATION_V25_TO_V26,
    MIGRATION_V26_TO_V27,
    // Over the empty v27 shape the batches above just built, on the fresh path;
    // over a real store, on the ladder (see `Store::migrate_to_v28`).
    MIGRATION_V27_TO_V28,
    // The one column v29 adds; the compression of existing rows is Rust, and a
    // fresh store has none (see `Store::migrate_to_v29`).
    MIGRATION_V28_TO_V29,
    // Not a rung: no `user_version` bump. Every writer open creates it — the
    // fresh path and the ladder alike — so both still land on one schema.
    READER_COMPAT_TABLE,
];

/// The name in the body of a `CREATE [UNIQUE] INDEX [IF NOT EXISTS] <name> ...`
/// chunk, where `body` starts at `INDEX `.
fn index_name_of(body: &str) -> &str {
    let rest = body.strip_prefix("INDEX ").unwrap_or(body).trim_start();
    let rest = rest
        .strip_prefix("IF NOT EXISTS ")
        .unwrap_or(rest)
        .trim_start();
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '(' || c == ';')
        .unwrap_or(rest.len());
    &rest[..end]
}

/// Strip SQL line comments so a scan of DDL text cannot read prose as code.
fn without_sql_comments(sql: &str) -> String {
    sql.lines()
        .map(|line| match line.find("--") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The indexes the current schema declares, read out of the DDL that creates
/// them.
///
/// **Derived, because the alternative rotted once already.** `REQUIRED_SCHEMA`
/// is a hand-written second copy of the column list and stayed correct only
/// because a test compares it to a live store; an index list written the same
/// way would need the same test and would have had none, which is precisely how
/// `MIGRATION_V16_TO_V17` came to drop `idx_generation_files_cache_identity`
/// and create no successor for it. Nothing reported that, because
/// `validate_schema` checked columns and an index is not a column.
///
/// Parsing our own `const` is a real derivation rather than a guess: the text
/// scanned is the text executed, in this crate, and
/// `the_index_gate_reads_every_index_the_schema_creates` pins the parse against
/// a store SQLite actually built.
/// Every `CREATE [UNIQUE] INDEX IF NOT EXISTS …;` statement the fresh schema
/// runs, as text, so an open can *recreate* what [`declared_index_names`] lets
/// the gate *demand*.
///
/// The ladder runs only the rungs above a store's stamp. An index added to
/// the fresh schema within a version number — `idx_file_payloads_cache_identity`
/// joined v17 after some v17 stores already existed — is on none of them, so
/// such a store walked every later rung and was then refused by the index
/// gate, with `devmap build` as the remedy: the command that had just
/// refused. Measured on this repository's own store, 2026-09-07. Every
/// statement here is `IF NOT EXISTS`, so replaying them on a store that has
/// the index is a no-op; `every_declared_index_statement_is_idempotent` pins
/// that.
///
/// One statement per index, even when two batches declare the same one.
///
/// [`declared_index_names`] has always deduplicated and this did not, which was
/// invisible while no index appeared in two batches. v22 makes one appear in
/// two: it drops `unresolved_rows` to reinterned columns and so has to recreate
/// `idx_unresolved_rows_closed`, which `VALIDITY_RANGE_TABLES` also declares
/// because that is where the table is first built. Both declarations are
/// right, and replaying an identical `CREATE … IF NOT EXISTS` twice is simply
/// nothing happening twice.
///
/// Deduplicated on the **whole statement**, not on the index name. Two batches
/// declaring one name with different definitions is a genuine contradiction
/// about what the index is, and it must keep reaching
/// `every_declared_index_statement_is_idempotent_and_names_a_gated_index` as
/// the mismatch it is rather than being collapsed away here.
///
/// Insertion order is kept rather than sorted: these are replayed as DDL, and
/// DDL that reads in schema order is easier to follow when one of them fails.
pub fn declared_index_statements() -> Vec<String> {
    let mut statements = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for batch in FRESH_SCHEMA_BATCHES {
        let sql = without_sql_comments(batch);
        for chunk in sql.split("CREATE ").skip(1) {
            let body = chunk.strip_prefix("UNIQUE ").unwrap_or(chunk);
            if !body.starts_with("INDEX ") {
                continue;
            }
            let Some(end) = chunk.find(';') else {
                continue;
            };
            let statement = format!("CREATE {};", chunk[..end].trim());
            if INDEXES_REMOVED_BY_V28
                .iter()
                .any(|removed| index_name_of(body) == *removed)
            {
                continue;
            }
            if seen.insert(statement.clone()) {
                statements.push(statement);
            }
        }
    }
    statements
}

pub fn declared_index_names() -> Vec<String> {
    let mut names = Vec::new();
    for batch in FRESH_SCHEMA_BATCHES {
        let sql = without_sql_comments(batch);
        for chunk in sql.split("CREATE ").skip(1) {
            let rest = match chunk.strip_prefix("UNIQUE ") {
                Some(rest) => rest,
                None => chunk,
            };
            let Some(rest) = rest.strip_prefix("INDEX ") else {
                continue;
            };
            let rest = rest.trim_start();
            let rest = rest.strip_prefix("IF NOT EXISTS ").unwrap_or(rest);
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '(' && *c != ';')
                .collect();
            if !name.is_empty() && !INDEXES_REMOVED_BY_V28.contains(&name.as_str()) {
                names.push(name);
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod retention_constant_tests {
    /// `devmap-extract` cannot depend on this crate, so its steady-state size
    /// budget mirrors `GENERATION_RETENTION` in its own constant. SC15 is the
    /// precedent for what happens when a policy number lives in two places and
    /// nothing compares them: the copy nobody runs goes stale silently.
    #[test]
    fn retention_matches_the_store_constant() {
        assert_eq!(
            u64::try_from(super::GENERATION_RETENTION).unwrap(),
            devmap_extract::model::DB_SIZE_GATE_RETAINED_GENERATIONS,
            "the steady-state size budget assumes a different retention count \
             than the store actually keeps"
        );
    }
}
