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
    PRIMARY KEY (generation_id, ordinal)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS generation_files (
    generation_id      INTEGER NOT NULL,
    file_id            INTEGER NOT NULL REFERENCES paths(id),
    language           TEXT NOT NULL,
    content_hash       INTEGER NOT NULL,
    parse_outcome_json TEXT NOT NULL,
    engine_json        TEXT NOT NULL,
    extraction_json    TEXT NOT NULL,
    grammar_version    TEXT,
    analyzer_version   TEXT,
    PRIMARY KEY (generation_id, file_id)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS generation_edges (
    generation_id  INTEGER NOT NULL,
    ordinal        INTEGER NOT NULL,
    source_file_id INTEGER NOT NULL REFERENCES paths(id),
    target_file_id INTEGER NOT NULL REFERENCES paths(id),
    source_symbol  TEXT NOT NULL,
    target_symbol  TEXT NOT NULL,
    edge_kind      TEXT NOT NULL,
    confidence     REAL NOT NULL,
    PRIMARY KEY (generation_id, ordinal)
) WITHOUT ROWID;

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
CREATE INDEX IF NOT EXISTS idx_generation_edges_source
    ON generation_edges(generation_id, source_file_id);
CREATE INDEX IF NOT EXISTS idx_generation_edges_target
    ON generation_edges(generation_id, target_file_id);

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

CREATE TABLE IF NOT EXISTS generation_files (
    generation_id      INTEGER NOT NULL,
    file_id            INTEGER NOT NULL REFERENCES paths(id),
    language           TEXT NOT NULL,
    content_hash       INTEGER NOT NULL,
    parse_outcome_json TEXT NOT NULL,
    engine_json        TEXT NOT NULL,
    extraction_json    TEXT NOT NULL,
    grammar_version    TEXT,
    analyzer_version   TEXT,
    PRIMARY KEY (generation_id, file_id)
) WITHOUT ROWID;

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

pub const CURRENT_SCHEMA_VERSION: i32 = 11;

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
