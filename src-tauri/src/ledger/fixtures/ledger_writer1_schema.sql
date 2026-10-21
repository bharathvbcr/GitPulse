CREATE TABLE IF NOT EXISTS events (
  id             INTEGER PRIMARY KEY,
  ulid           TEXT NOT NULL UNIQUE,
  ts_utc         TEXT NOT NULL,
  schema_version INTEGER NOT NULL DEFAULT 1,
  repo_path      TEXT NOT NULL,
  worktree_path  TEXT,
  actor_kind     TEXT NOT NULL CHECK (actor_kind IN ('human','agent','system')),
  actor_id       TEXT,
  session_id     TEXT,
  task_id        TEXT,
  action         TEXT NOT NULL,
  object         TEXT,
  argv_json      TEXT,
  outcome        TEXT NOT NULL CHECK (outcome IN ('ok','failed','blocked')),
  verdict_json   TEXT,
  before_ref     TEXT,
  after_ref      TEXT,
  duration_ms    INTEGER,
  detail_json    TEXT
);
CREATE INDEX IF NOT EXISTS idx_events_repo_ts  ON events(repo_path, ts_utc);
CREATE INDEX IF NOT EXISTS idx_events_reflog   ON events(worktree_path, object)
    WHERE action LIKE 'reflog.%';
CREATE INDEX IF NOT EXISTS idx_events_task     ON events(task_id)    WHERE task_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_session  ON events(session_id) WHERE session_id IS NOT NULL;
-- The two indexes the filtered `gitpulse_ledger_events` read adds, for the
-- `since_ts` and `action` filters (`task_id` and `session_id` use the two
-- above). `object`, `worktree_path` and `after_ref` are deliberately not
-- indexed: each index is an append-time cost on every event, and those filters
-- stop at `limit` matches. SQLite appends the rowid to every index key, so
-- `idx_events_ts` is already the composite (ts_utc, id) the `since_ts` read
-- uses. Both columns are NOT NULL, so neither index can be partial on that.
-- Measured cost (ledger::query_tests::bench_append_cost_by_index, 10 000
-- appends, rotated order, min of 5, host load ~20-27): 150.0 us/append with
-- neither, 160.1 us with both (+6.7%); alone each measured +8% to +11%, so the
-- figures are noisy at that size. What it buys: newest-first `action` and
-- `since_ts` reads that stop at `limit` instead of scanning the history.
-- `idx_events_action` carries the predicate `id > 0` (true of every row) and the
-- read states it, because SQLite uses a partial index only when the query
-- contains its predicate: a plain index on `action` competes with the
-- purpose-built ones (`idx_events_binding`, `idx_events_reflog`) and the
-- planner picks it for the binding and re-anchor queries, which it serves worse.
CREATE INDEX IF NOT EXISTS idx_events_ts       ON events(ts_utc);
CREATE INDEX IF NOT EXISTS idx_events_action   ON events(action) WHERE id > 0;
CREATE INDEX IF NOT EXISTS idx_events_binding  ON events(worktree_path, ts_utc)
  WHERE action IN ('worktree.bind', 'worktree.unbind');
CREATE TABLE IF NOT EXISTS ledger_family_imports (
  source_key     TEXT PRIMARY KEY,
  source_label   TEXT NOT NULL,
  source_cursor  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS transcript_cursors (
  worktree_key   TEXT NOT NULL,
  file_key       TEXT NOT NULL,
  byte_offset    INTEGER NOT NULL,
  PRIMARY KEY (worktree_key, file_key)
);
CREATE TABLE IF NOT EXISTS transcript_baselines (
  worktree_key   TEXT PRIMARY KEY,
  since_utc      TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS pulse_snapshots (
  id             INTEGER PRIMARY KEY,
  day            TEXT NOT NULL,
  repo_path      TEXT NOT NULL,
  total_commits  INTEGER NOT NULL,
  total_loc      INTEGER NOT NULL,
  bus_factor     INTEGER NOT NULL,
  coverage_pct   REAL,
  snapshot_json  TEXT NOT NULL,
  UNIQUE(day, repo_path)
);
CREATE INDEX IF NOT EXISTS idx_pulse_snapshots_repo_day ON pulse_snapshots(repo_path, day);
CREATE TABLE IF NOT EXISTS fleet_metrics (
  repo_path                 TEXT PRIMARY KEY,
  loc                       INTEGER,
  loc_language              TEXT,
  loc_truncated             INTEGER NOT NULL DEFAULT 0,
  loc_at                    TEXT,
  storage_bytes             INTEGER,
  storage_git_bytes         INTEGER,
  storage_reclaimable_bytes INTEGER,
  storage_truncated         INTEGER NOT NULL DEFAULT 0,
  storage_at                TEXT,
  vulns_critical            INTEGER,
  vulns_high                INTEGER,
  vulns_moderate            INTEGER,
  vulns_low                 INTEGER,
  vulns_unknown             INTEGER,
  vulns_total               INTEGER,
  health_complete           INTEGER NOT NULL DEFAULT 0,
  health_at                 TEXT,
  coverage_pct              REAL,
  coverage_truncated        INTEGER NOT NULL DEFAULT 0,
  coverage_at               TEXT
);
CREATE TABLE IF NOT EXISTS fleet_history (
  repo_path                 TEXT NOT NULL,
  day                       TEXT NOT NULL,
  loc                       INTEGER,
  storage_bytes             INTEGER,
  vulns_total               INTEGER,
  coverage_pct              REAL,
  PRIMARY KEY (repo_path, day)
);
CREATE TABLE IF NOT EXISTS fleet_languages (
  repo_path                 TEXT NOT NULL,
  language                  TEXT NOT NULL,
  color_hex                 TEXT NOT NULL,
  category                  TEXT NOT NULL,
  code_lines                INTEGER NOT NULL,
  file_count                INTEGER NOT NULL,
  percentage                REAL NOT NULL,
  PRIMARY KEY (repo_path, language)
);
