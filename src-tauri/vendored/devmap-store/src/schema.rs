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

CREATE INDEX IF NOT EXISTS idx_generation_file_rows_payload
    ON generation_file_rows(payload_id);

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

CREATE INDEX IF NOT EXISTS idx_generation_nodes_file
    ON generation_nodes(generation_id, file_id);

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

CREATE INDEX IF NOT EXISTS idx_generation_file_rows_payload
    ON generation_file_rows(payload_id);

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

CREATE INDEX IF NOT EXISTS idx_generation_nodes_file
    ON generation_nodes(generation_id, file_id);
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
/// `tools/fanout.sql`, `tools/soak.sh`, `verify.sh`'s determinism digest and
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

CREATE INDEX IF NOT EXISTS idx_unresolved_rows_callee
    ON unresolved_rows(callee_name);
CREATE INDEX IF NOT EXISTS idx_unresolved_rows_class
    ON unresolved_rows(classification);
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

/// The schema this binary writes.
///
/// # Why there is no v20 putting the nodes on ranges
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

pub const CURRENT_SCHEMA_VERSION: i32 = 20;

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
];

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
pub fn declared_index_statements() -> Vec<String> {
    let mut statements = Vec::new();
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
            statements.push(format!("CREATE {};", chunk[..end].trim()));
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
            if !name.is_empty() {
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
