//! The action ledger: a durable, append-only record of what happened here.
//!
//! Before this, GitPulse's provenance was a 200-entry array in browser memory.
//! Closing the window erased it; a crash mid-rebase erased it; and nothing that
//! happened while the app was closed was ever knowable. The ledger makes the UI
//! a *projection* of a log on disk rather than the log itself.
//!
//! # What is recorded, and where
//!
//! One SQLite database per repository, at `.devcouncil/ledger.sqlite`, joining
//! the shared-state convention DevCouncil and Manvi already use. WAL mode, one
//! writer: this process. Importers do not open the database themselves — they
//! go through [`append`], for the ordinary reason that two writers with two
//! notions of the schema is how a store stops being trustworthy.
//!
//! # The rule this module must not break
//!
//! A ledger that could not record something must never be indistinguishable
//! from one that recorded nothing because nothing happened. An append that
//! fails is counted and surfaced through [`status`] — the same treatment
//! `PolicyStatus::Unchecked` gets, and for the same reason: silence about a
//! failure reads exactly like a clean result.
//!
//! # Redaction
//!
//! Every caller-controlled text field is redacted *before* insert, never at
//! display time, through [`redact`]. A secret redacted only on the way to the
//! screen is still on disk, in a file that gets backed up and synced and read
//! by every later consumer.

mod anchor;
pub mod bindings;
mod cap;
mod consolidated;
pub mod freshness;
pub mod ids;
pub mod redact;

#[cfg(test)]
pub(crate) use anchor::forget as forget_identity_state;
pub(crate) use anchor::settle as settle_identity;
pub use cap::ValueCut;
pub(crate) use cap::{bound_evidence_json, ROW_FIELD_CAP_BYTES};
use cap::{cap_object, ObjectCap, OBJECT_TRUNCATED_KEY};
pub use freshness::{
    freshness_readonly, freshness_readonly_at, record_catch_up, CatchUpRecord, CatchUpSummary,
    Freshness, LedgerView, STALE_AFTER_SECS,
};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

/// The schema, exactly as the migration plan froze it for v1.
///
/// The two `CHECK` constraints are load-bearing rather than decorative: they
/// are what stops a future caller inventing a sixth outcome or a fourth actor
/// kind that consumers would then have to guess about. A rejected insert is a
/// bug reported at the seam; an accepted unknown value is a bug discovered
/// months later in a UI that renders it as blank.
const SCHEMA: &str = r#"
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
  detail_json    TEXT,
  occurred_at    TEXT,
  reflog_ordinal INTEGER
);
CREATE INDEX IF NOT EXISTS idx_events_repo_ts ON events(repo_path, ts_utc);
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
  fingerprint    TEXT,
  PRIMARY KEY (worktree_key, file_key)
);
-- CARD 27 (ft-9d043f74...4671), S6-08/S6-09: where reflog replay stopped, per
-- worktree and ref (Wave B walks every family worktree's HEAD and
-- refs/heads/<default>), as the last recorded entry's identity: selector time,
-- oid, message hash and ordinal among identical entries. Written by Wave B.
CREATE TABLE IF NOT EXISTS reflog_watermarks (
  worktree_key        TEXT NOT NULL,
  ref_name            TEXT NOT NULL,
  last_entry_unix     INTEGER,
  last_entry_oid      TEXT,
  last_entry_msg_hash TEXT,
  last_entry_ordinal  INTEGER,
  updated_at          TEXT,
  PRIMARY KEY (worktree_key, ref_name)
);
-- CARD 29 (ft-9d044974...3c3f), S6-13 and S6-11: the last CatchUp per worktree,
-- so a read-only reader can say when it last ran (as_of / last_catch_up /
-- stale), whether it was cut short with the counts (transcripts read, listed
-- and unlisted, so a partial pass never reads as complete), lines skipped, and
-- records that failed to write (S6-08's "a failed record surfaces").
CREATE TABLE IF NOT EXISTS catch_up_state (
  worktree_key        TEXT PRIMARY KEY,
  completed_at        TEXT,
  truncated           INTEGER,
  recorded            INTEGER,
  transcripts         INTEGER,
  transcripts_listed  INTEGER,
  transcripts_unlisted INTEGER,
  skipped_lines       INTEGER,
  reflog_entries      INTEGER,
  failed_records      INTEGER,
  duplicates_refused  INTEGER,
  error               TEXT
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
"#;

/// This build's event schema version, written into every row.
pub const SCHEMA_VERSION: i64 = 1;

/// The writer generation stamped into the ledger (`PRAGMA user_version`).
///
/// Distinct from [`SCHEMA_VERSION`], which versions one event row: this
/// versions the *rules writers follow*, such as which cursors a `session.*`
/// row must move. A build that predates a rule writes rows the newer rules
/// treat as new (a 1.3.1 daemon appended transcript rows without moving 1.4.0's
/// `transcript_cursors`, and the app then re-ingested those bytes as
/// duplicates). Bump this whenever a rule like that changes; every build that
/// knows about the stamp refuses a ledger stamped newer than itself, and a
/// build older than the stamp's introduction is stopped by the daemon lock and
/// the version check in Health instead.
///
/// Generation 2 (OD 1) is the one additive schema bump: `events.occurred_at`,
/// `events.reflog_ordinal`, `transcript_cursors.fingerprint`, the `reflog_watermarks` and
/// `catch_up_state` tables, and the unique reflog identity index
/// ([`migrate_to_writer_2`]). All of it is nullable or new, so no existing
/// read or insert changes. A generation-1 build's queries and inserts name
/// their columns, so they run unchanged against the migrated file; its writable
/// open refuses the file by this very stamp, so a generation-1 process that
/// *starts* after the migration never writes it.
///
/// The stamp is read when a connection opens, not on every write. A
/// generation-1 process that was already running when a generation-2 build
/// migrated the ledger keeps its open connection and keeps writing, un-stamped:
/// its rows carry no `occurred_at` or ordinal, and a duplicate reflog insert
/// from it is refused by the unique index (which that build counts as a dropped
/// append). Nothing can change a binary that is already running, so after an
/// upgrade every running GitPulse process (the app, `gitpulsed`, `gitpulse-mcp`)
/// must be restarted.
pub const WRITER_VERSION: i64 = 2;

/// The unique index a reflog entry's identity is held to (CARD 27 / S6-08):
/// the worktree, `object` (the commit id plus the message, as stored), the
/// event time, for an `object` cut to the append cap the hash of its full text
/// (the message hash), and the ordinal of the entry among identical ones. The
/// worktree, time and ordinal are coalesced because SQLite treats NULLs as
/// distinct in a unique index, and the anchor checkout stores a NULL worktree.
const REFLOG_IDENTITY_INDEX: &str = "idx_events_reflog_identity";
const REFLOG_ROWS: &str = "action LIKE 'reflog.%' AND object IS NOT NULL";

/// The four things a reflog identity is made of, as SQL expressions: column
/// names (the migration, the index) or bound values (the legacy import's
/// lookup). ONE definition of the identity without its ordinal, shared by the
/// index, the migration's numbering and the import's numbering, so none of them
/// can disagree about which rows are "the same entry".
///
/// The cut-object hash is only read from a detail that mentions
/// `object_truncated` at all; `instr` is a plain scan, where `json_valid` would
/// parse every detail of every reflog row.
struct IdentityParts<'a> {
    worktree: &'a str,
    object: &'a str,
    occurred_at: &'a str,
    detail: &'a str,
}

impl IdentityParts<'_> {
    /// The `events` columns themselves.
    const COLUMNS: IdentityParts<'static> = IdentityParts {
        worktree: "worktree_path",
        object: "object",
        occurred_at: "occurred_at",
        detail: "detail_json",
    };

    fn terms(&self) -> [String; 4] {
        let Self {
            worktree,
            object,
            occurred_at,
            detail,
        } = self;
        [
            format!("COALESCE({worktree}, '')"),
            object.to_string(),
            format!("COALESCE({occurred_at}, '')"),
            format!(
                "COALESCE(CASE WHEN instr({detail}, 'object_truncated') > 0 \
                 AND json_valid({detail}) \
                 THEN json_extract({detail}, '$.object_truncated.hash') END, '')"
            ),
        ]
    }

    /// The comma-separated key, without the ordinal.
    fn key(&self) -> String {
        self.terms().join(", ")
    }
}

/// The full `CREATE UNIQUE INDEX` text. Also what a stored index is compared
/// with (whitespace-normalised) before it is trusted.
fn reflog_identity_index_sql() -> String {
    format!(
        "CREATE UNIQUE INDEX {REFLOG_IDENTITY_INDEX} ON events({}, COALESCE(reflog_ordinal, 0)) \
         WHERE {REFLOG_ROWS}",
        IdentityParts::COLUMNS.key()
    )
}

fn normalise_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Upper bound on how long an opener waits for another process's migration.
/// rusqlite's default busy timeout is 5 s, which a large ledger's migration
/// can outlast; only the `BEGIN IMMEDIATE` of a migration waits this long.
const MIGRATION_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// SQL for the event time `detail` carries, or NULL: the interim
/// `detail_json.occurred_at` of a row written before the typed column, taken
/// only when it has the exact canonical shape the writer produces.
fn occurred_at_from_detail(detail: &str) -> String {
    format!(
        "CASE WHEN json_valid({detail}) AND json_extract({detail}, '$.occurred_at') GLOB \
         '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z' \
         THEN json_extract({detail}, '$.occurred_at') END"
    )
}

fn stamped_writer_version(conn: &Connection) -> Result<i64, LedgerError> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|e| LedgerError::new("pragma_failed", format!("user_version: {e}")))
}

fn writer_too_old(db_path: &Path, stamped: i64) -> LedgerError {
    LedgerError::new(
        "writer_too_old",
        format!(
            "{} was written by a newer GitPulse (writer generation {stamped}; this build is {WRITER_VERSION}). Update GitPulse, or restart the old gitpulsed, before writing to it.",
            db_path.display()
        ),
    )
}

/// Refuses a ledger a newer writer has stamped, creates and migrates one an
/// older (or no) writer left, and stamps it. Never lowers the stamp.
///
/// A refused writer alters nothing. A ledger below this generation is migrated
/// in ONE immediate transaction that also holds the schema batch and the stamp,
/// so a crash or an error at any point rolls all of it back and the next open
/// starts again from the old stamp; every step is also safe to repeat, so a
/// ledger some earlier attempt half-changed is completed rather than failed.
/// The stamp is re-read under the write lock, so two processes opening one old
/// ledger migrate it once.
fn ensure_schema(conn: &Connection, db_path: &Path) -> Result<(), LedgerError> {
    let stamped = stamped_writer_version(conn)?;
    if stamped > WRITER_VERSION {
        return Err(writer_too_old(db_path, stamped));
    }
    if stamped == WRITER_VERSION {
        return conn
            .execute_batch(SCHEMA)
            .map_err(|e| LedgerError::new("schema_failed", e.to_string()));
    }
    let failed = |e: rusqlite::Error| LedgerError::new("migration_failed", e.to_string());
    // Another process may be migrating this very ledger. Waiting for its write
    // lock is the one place a long wait is right, so the timeout is raised for
    // that `BEGIN IMMEDIATE` alone and put back; the stamp is re-read below.
    let ordinary_timeout: i64 = conn
        .pragma_query_value(None, "busy_timeout", |row| row.get(0))
        .map_err(failed)?;
    conn.busy_timeout(MIGRATION_BUSY_TIMEOUT).map_err(failed)?;
    let begun = write_tx(conn);
    let restored = conn.busy_timeout(std::time::Duration::from_millis(
        u64::try_from(ordinary_timeout).unwrap_or(0),
    ));
    let tx = begun.map_err(failed)?;
    restored.map_err(failed)?;
    let stamped = stamped_writer_version(&tx)?;
    if stamped > WRITER_VERSION {
        return Err(writer_too_old(db_path, stamped));
    }
    if stamped < WRITER_VERSION {
        tx.execute_batch(SCHEMA).map_err(failed)?;
        migrate_to_writer_2(&tx).map_err(failed)?;
        tx.pragma_update(None, "user_version", WRITER_VERSION)
            .map_err(failed)?;
    }
    tx.commit().map_err(failed)
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> rusqlite::Result<()> {
    let present: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        params![table, column],
        |row| row.get(0),
    )?;
    if present == 0 {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}

/// Writer generation 1 to 2. Runs inside [`ensure_schema`]'s transaction.
///
/// Non-destructive: no row is deleted. An old row cannot tell a double-recorded
/// reflog entry from a genuine repeat (two identical checkouts), so the
/// migration keeps both and numbers them: within each group of reflog rows that
/// share an [`IdentityParts`] key, the later rows get `reflog_ordinal` 1, 2...
/// by ascending `id`. The first row of a group, and every row of a group of
/// one, keeps a NULL ordinal, which the index reads as 0, so the work scales
/// with the duplicates and not with the rows. The unique index is then built
/// over the key plus the ordinal, which cannot fail on existing data.
/// Reconciling old double-writes against the live reflog is Wave B replay work.
/// Ordinals are recomputed from `id`, so a re-run (the stamp was reset) is a
/// no-op.
///
/// Rows from before the typed column carry their event time only in
/// `detail_json`, so it is copied into the column first (reflog rows only, and
/// only in the writer's exact shape); otherwise two entries at different times
/// would be numbered as repeats of one.
///
/// An index of the same name with another definition (an earlier build's) is
/// dropped and rebuilt rather than trusted.
fn migrate_to_writer_2(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_missing(conn, "events", "occurred_at", "TEXT")?;
    add_column_if_missing(conn, "events", "reflog_ordinal", "INTEGER")?;
    add_column_if_missing(conn, "transcript_cursors", "fingerprint", "TEXT")?;
    let wanted = reflog_identity_index_sql();
    let stored: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
            params![REFLOG_IDENTITY_INDEX],
            |row| row.get(0),
        )
        .optional()?;
    let current = stored
        .as_deref()
        .is_some_and(|sql| normalise_sql(sql) == normalise_sql(&wanted));
    if !current && stored.is_some() {
        conn.execute_batch(&format!("DROP INDEX {REFLOG_IDENTITY_INDEX}"))?;
    }
    let key = IdentityParts::COLUMNS.key();
    conn.execute_batch(&format!(
        "UPDATE events SET occurred_at = {from_detail}
         WHERE occurred_at IS NULL AND {REFLOG_ROWS};
         UPDATE events SET reflog_ordinal = numbered.n
         FROM (SELECT id,
                      ROW_NUMBER() OVER (PARTITION BY {key} ORDER BY id) - 1 AS n
               FROM events WHERE {REFLOG_ROWS}) AS numbered
         WHERE events.id = numbered.id AND numbered.n > 0
           AND events.reflog_ordinal IS NOT numbered.n;",
        from_detail = occurred_at_from_detail("detail_json"),
    ))?;
    if !current {
        conn.execute_batch(&wanted)?;
    }
    Ok(())
}

/// Who acted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorKind {
    Human,
    Agent,
    /// GitPulse itself: importers, catch-up synthesis, maintenance. Never
    /// conflated with a human action.
    System,
}

impl ActorKind {
    fn as_str(self) -> &'static str {
        match self {
            ActorKind::Human => "human",
            ActorKind::Agent => "agent",
            ActorKind::System => "system",
        }
    }
}

/// How it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Ok,
    /// It ran and did not succeed.
    Failed,
    /// The gate refused it, and GitPulse did not perform it. Distinct from
    /// `Failed`, which means it ran.
    Blocked,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Failed => "failed",
            Outcome::Blocked => "blocked",
        }
    }
}

/// An event on its way in. `id`, `ulid` and `ts_utc` are assigned by the
/// writer, so a caller cannot accidentally forge a position in the sequence.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    pub repo_path: String,
    pub worktree_path: Option<String>,
    pub actor_kind: Option<ActorKind>,
    pub actor_id: Option<String>,
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    pub action: String,
    pub object: Option<String>,
    pub argv_json: Option<String>,
    pub outcome: Option<Outcome>,
    pub verdict_json: Option<String>,
    pub before_ref: Option<String>,
    pub after_ref: Option<String>,
    pub duration_ms: Option<i64>,
    pub detail_json: Option<String>,
    /// When the event happened and how its task was decided, for the writer to
    /// place in `detail_json`. See [`EventStamp`].
    pub event_stamp: Option<EventStamp>,
}

/// The identity of one row derived from a transcript line: the transcript
/// ([`transcript_key`]), the line's byte offset, the row's ordinal among the
/// calls of that line, and an FNV-1a digest of the line's bytes.
///
/// Two lines that read alike are two identities (the offsets differ), so a
/// command run twice is two rows. A transcript rewritten under the same name
/// keeps the identity of every line that did not change and gets a new one for
/// each line that did. A row recorded before this identity existed carries
/// none, and is not recognised: the history that predates it can still be
/// recorded again after a cursor reset.
///
/// A transcript row carries its identity in `detail_json` under
/// [`TRANSCRIPT_ROW_KEY`], and its `ulid` is derived from that identity, the
/// repository, worktree, actor and session ([`prepare`]). The unique index on
/// `ulid` then refuses a second copy on every path that can read a line twice
/// (a lost or stale cursor, a forget, a second spelling of the root, another
/// process), at the cost of one index probe per insert and no scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRow {
    pub file_key: String,
    pub offset: u64,
    pub ordinal: usize,
    pub digest: u64,
}

impl TranscriptRow {
    /// The text kept in `detail_json` under [`TRANSCRIPT_ROW_KEY`].
    pub fn key(&self) -> String {
        format!(
            "{}:{}:{}:{:016x}",
            self.file_key, self.offset, self.ordinal, self.digest
        )
    }
}

/// The `detail_json` key that holds a transcript row's [`TranscriptRow::key`].
pub const TRANSCRIPT_ROW_KEY: &str = "transcript_row";

/// The [`TranscriptRow::key`] in a draft's `detail_json`, or `None` for any
/// other row.
fn transcript_row_of(detail: &str) -> Option<String> {
    // A key is hex digits and colons, so it holds nothing JSON would escape; a
    // quote inside some other string value is escaped and cannot match here.
    let marker = format!("\"{TRANSCRIPT_ROW_KEY}\":\"");
    let start = detail.find(&marker)? + marker.len();
    let key = &detail[start..start + detail[start..].find('"')?];
    key.bytes()
        .all(|b| b.is_ascii_hexdigit() || b == b':')
        .then(|| key.to_string())
}

/// The three `detail_json` keys that carry event time: `occurred_at`,
/// `occurred_at_basis` and `task_basis`.
///
/// They are the only keys that skip the redactor, which is what keeps a replay
/// of a large corpus from redacting three more fields per row. That is safe
/// only because each value is checked against a closed shape before it is
/// spliced in ([`EventStamp::is_well_formed`]): a canonical 24-character UTC
/// timestamp and two words from fixed lists. A stamp that fails the check is
/// not dropped and not trusted: it is merged into the detail object and
/// redacted with everything else.
///
/// Trust: on a transcript row `occurred_at` comes from the transcript, which
/// the agent wrote, so an agent can date a call into an old binding window and
/// have it attributed to that window's task. `task_basis` and `task_id` on
/// transcript rows are as trustworthy as the transcript itself: provenance, not
/// an authority. Reflog rows take their time from git's own record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventStamp {
    pub occurred_at: String,
    pub occurred_at_basis: String,
    pub task_basis: String,
}

impl EventStamp {
    /// Whether every value has the shape the writer produces, so none can
    /// carry text a caller chose.
    pub fn is_well_formed(&self) -> bool {
        // A real calendar instant, in the exact form the formatter writes: the
        // shape check alone passes `9999-99-99T99:99:99.999Z`.
        valid_utc_timestamp(&self.occurred_at)
            && ids::parse_event_time(&self.occurred_at)
                .map(ids::iso8601_utc)
                .as_deref()
                == Some(self.occurred_at.as_str())
            && matches!(
                self.occurred_at_basis.as_str(),
                "transcript" | "reflog" | "ingest"
            )
            && matches!(self.task_basis.as_str(), "bound" | "unbound" | "unresolved")
    }

    /// The three members as JSON object text, without braces.
    fn members(&self) -> String {
        serde_json::json!({
            "occurred_at": self.occurred_at,
            "occurred_at_basis": self.occurred_at_basis,
            "task_basis": self.task_basis,
        })
        .to_string()
    }
}

/// One server-built member set that skips the redactor when it is spliced into
/// a `detail_json` object: the event-time stamp, or the record of an `object`
/// cut. Each is checked against a closed shape first
/// ([`EventStamp::is_well_formed`], [`ObjectCap::is_well_formed`]).
enum Splice<'a> {
    Stamp(&'a EventStamp),
    ObjectCap(&'a ObjectCap),
}

impl Splice<'_> {
    fn is_well_formed(&self) -> bool {
        match self {
            Splice::Stamp(stamp) => stamp.is_well_formed(),
            Splice::ObjectCap(cap) => cap.is_well_formed(),
        }
    }

    /// The members as JSON object text, without braces.
    fn members(&self) -> String {
        let object = match self {
            Splice::Stamp(stamp) => stamp.members(),
            Splice::ObjectCap(cap) => {
                serde_json::json!({ OBJECT_TRUNCATED_KEY: cap.as_value() }).to_string()
            }
        };
        object[1..object.len() - 1].to_string()
    }

    /// The members inserted into a parsed object, overwriting.
    fn insert_into(&self, map: &mut serde_json::Map<String, serde_json::Value>) {
        match self {
            Splice::Stamp(stamp) => {
                map.insert("occurred_at".into(), stamp.occurred_at.clone().into());
                map.insert(
                    "occurred_at_basis".into(),
                    stamp.occurred_at_basis.clone().into(),
                );
                map.insert("task_basis".into(), stamp.task_basis.clone().into());
            }
            Splice::ObjectCap(cap) => {
                map.insert(OBJECT_TRUNCATED_KEY.into(), cap.as_value());
            }
        }
    }
}

/// `detail` redacted, with `stamp`'s keys and `cap`'s record in it.
///
/// Each well-formed member set is spliced onto the redacted object: the detail
/// is redacted without those keys, and their validated members are appended.
/// Anything else takes the closed path: a member set that fails validation, or
/// a detail that names a spliced key (spelled plainly or through a `\u`
/// escape, which any JSON parser reads as the same key). There the keys go into
/// the parsed object, overwriting, and the whole of it is redacted. A detail
/// that does not parse as a JSON object carries no stamp on that path, but a
/// cut `object` is always recorded: the detail is wrapped as
/// `{"detail": <original>, "object_truncated": {..}}`.
///
/// `object_truncated` is the server's key whether or not an object was cut: a
/// caller who spells it takes the closed path, and the spelling is dropped (and
/// replaced when there was a cut), so a row cannot claim a cut that never
/// happened or choose the hash of one that did.
///
/// Callers pass serde-built objects. The fast path does not parse: text that
/// starts with `{` and ends with `}` is spliced as it is, so malformed brace
/// text (`{}{}`, `{not json}`) is not validated there, and a debug build
/// asserts that the redacted text is an object. A release-mode parse would cost
/// back what the splice saves.
fn redact_detail(
    detail: Option<&str>,
    stamp: Option<&EventStamp>,
    cap: Option<&ObjectCap>,
) -> Option<String> {
    let splices: Vec<Splice<'_>> = stamp
        .map(Splice::Stamp)
        .into_iter()
        .chain(cap.map(Splice::ObjectCap))
        .collect();
    // A detail that already names a key the server writes would be given it
    // twice, or would claim a record the server never made. With nothing to
    // splice only the marker is the server's; its key is reserved always.
    let names_a_key = detail.is_some_and(|text| {
        names_a_reserved_key(
            text,
            if splices.is_empty() {
                &[OBJECT_KEY_SPELLING]
            } else {
                &RESERVED_KEY_SPELLINGS
            },
        )
    });
    if splices.is_empty() && !names_a_key {
        return detail.map(redact::text);
    }
    if !splices.is_empty() && splices.iter().all(Splice::is_well_formed) && !names_a_key {
        let redacted = detail.map_or_else(|| "{}".to_string(), redact::text);
        let trimmed = redacted.trim();
        if let Some(body) = trimmed
            .strip_prefix('{')
            .and_then(|rest| rest.strip_suffix('}'))
        {
            // Nesting past serde's own limit is still an object (the redactor
            // reads it by other means); only a real syntax error is a bug.
            debug_assert!(
                match serde_json::from_str::<serde_json::Value>(trimmed) {
                    Ok(value) => value.is_object(),
                    Err(e) => e.to_string().contains("recursion limit"),
                },
                "the splice was handed text that is not a JSON object: {trimmed}"
            );
            let mut text = format!("{{{body}");
            let mut separator = if body.trim().is_empty() { "" } else { "," };
            for splice in &splices {
                text.push_str(separator);
                text.push_str(&splice.members());
                separator = ",";
            }
            text.push('}');
            return Some(text);
        }
    }
    // The closed path: the keys join the object, and all of it is redacted.
    let mut map = match detail {
        None => serde_json::Map::new(),
        Some(text) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(serde_json::Value::Object(map)) => map,
            // A detail that is not an object has nowhere to carry the record of
            // a cut object, and a cut object with no record is a stored value
            // ending in a suffix with no length and no hash. It is wrapped, as
            // `{"detail": <the original>, "object_truncated": {..}}`: the
            // original stays readable under `detail` (as its JSON value, or as
            // a string when it is not JSON). A stamp alone does not wrap.
            other if cap.is_some() => {
                let original = other.unwrap_or_else(|_| serde_json::Value::String(text.into()));
                serde_json::Map::from_iter([("detail".to_string(), original)])
            }
            _ => return detail.map(redact::text),
        },
    };
    // The marker key is the server's: a spelling of it by the caller is
    // dropped, and replaced below when there was a cut.
    map.remove(OBJECT_TRUNCATED_KEY);
    for splice in &splices {
        splice.insert_into(&mut map);
    }
    Some(redact::text(&serde_json::Value::Object(map).to_string()))
}

/// How a reserved key appears as the start of a JSON key in text, quote first.
const OBJECT_KEY_SPELLING: &str = "\"object_truncated";
const RESERVED_KEY_SPELLINGS: [&str; 3] = ["\"occurred_at", "\"task_basis\"", OBJECT_KEY_SPELLING];

/// Whether `text` names one of `spellings`, plainly or through a `\u` escape,
/// which any JSON parser reads as the same key. The one predicate for "this
/// detail spells a key the server writes".
fn names_a_reserved_key(text: &str, spellings: &[&str]) -> bool {
    text.contains("\\u") || spellings.iter().any(|key| text.contains(key))
}

/// An event read back, in the shape the frontend consumes.
///
/// Named `LedgerEvent` rather than `Event` because it crosses the IPC boundary
/// into TypeScript, where a bare `Event` shadows the DOM type of that name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LedgerEvent {
    pub id: i64,
    pub ulid: String,
    pub ts_utc: String,
    pub schema_version: i64,
    pub repo_path: String,
    pub worktree_path: Option<String>,
    pub actor_kind: String,
    pub actor_id: Option<String>,
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    pub action: String,
    pub object: Option<String>,
    pub argv_json: Option<String>,
    pub outcome: String,
    pub verdict_json: Option<String>,
    pub before_ref: Option<String>,
    pub after_ref: Option<String>,
    pub duration_ms: Option<i64>,
    pub detail_json: Option<String>,
}

fn event_from_row(row: &Row<'_>) -> rusqlite::Result<LedgerEvent> {
    Ok(LedgerEvent {
        id: row.get(0)?,
        ulid: row.get(1)?,
        ts_utc: row.get(2)?,
        schema_version: row.get(3)?,
        repo_path: row.get(4)?,
        worktree_path: row.get(5)?,
        actor_kind: row.get(6)?,
        actor_id: row.get(7)?,
        session_id: row.get(8)?,
        task_id: row.get(9)?,
        action: row.get(10)?,
        object: row.get(11)?,
        argv_json: row.get(12)?,
        outcome: row.get(13)?,
        verdict_json: row.get(14)?,
        before_ref: row.get(15)?,
        after_ref: row.get(16)?,
        duration_ms: row.get(17)?,
        detail_json: row.get(18)?,
    })
}

/// Why an append or a read did not happen.
#[derive(Debug, Clone)]
pub struct LedgerError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl LedgerError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        LedgerError {
            code,
            message: message.into(),
        }
    }
}

/// What the UI shows about the ledger itself.
///
/// This exists so that "no events" and "events we could not write" are
/// different on screen. Without it a read-only checkout, a full disk or a
/// corrupt database would all render as a quiet, empty, apparently healthy
/// history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerStatus {
    /// True when the database for this repo is open and accepting writes.
    pub recording: bool,
    pub path: String,
    /// Appends this process failed to write, ever. Non-zero means the history
    /// on disk is known to be incomplete.
    pub dropped: u64,
    /// Bind/unbind writes, in this process, whose catch-up beforehand failed or
    /// was cut short. Nothing was dropped: the rows it would have recorded wait
    /// for a later pass. Not folded into `dropped`.
    #[serde(default)]
    pub catch_up_failures: u64,
    /// Empty when recording; otherwise why not.
    pub error: String,
    pub error_code: String,
}

/// The app handle used to announce appends, installed once at startup.
///
/// The ledger is written from deep inside the mutation path, where no
/// `AppHandle` is in scope and threading one through every guard call site
/// would be a large change for a notification. Storing it here keeps the write
/// path unchanged; when it is absent — in tests, and before setup runs — the
/// append still happens and only the announcement is skipped.
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

/// What `ledger-appended` carries. The cursor is enough: a listener asks
/// [`tail`] for everything after the one it last saw, so a dropped
/// notification costs nothing and the payload can never be stale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerAppended {
    pub repo_path: String,
    pub cursor: i64,
}

/// Installs the handle used to announce appends. Called once, from setup.
pub fn set_app_handle(handle: tauri::AppHandle) {
    let _ = APP.set(handle);
}

fn announce(repo_path: &str, cursor: i64) {
    use tauri::Emitter;
    if let Some(app) = APP.get() {
        if let Err(e) = app.emit(
            "ledger-appended",
            LedgerAppended {
                repo_path: repo_path.to_string(),
                cursor,
            },
        ) {
            // A failed announcement is a stale UI, not a lost event: the row is
            // already durable, and the next tail picks it up.
            log::warn!("ledger-appended emit failed: {e}");
        }
    }
}

/// One segment of an event action name: lowercase ASCII letters and digits,
/// every other character folded to `_`.
///
/// The single owner of what an action segment may contain, shared by command
/// lines ([`action_for_argv`]) and the reflog replay, so a verb from either
/// source is spelled the same way in the ledger.
pub(crate) fn action_segment(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Derives an event action name from a command line.
///
/// `git commit` becomes `git.commit`; anything else becomes `command.run`, so a
/// non-git mutation is still recorded rather than silently mis-filed under a
/// git verb it never was.
pub fn action_for_argv(argv: &[&str]) -> String {
    match (argv.first(), argv.get(1)) {
        (Some(&"git"), Some(sub)) => {
            let clean = action_segment(sub);
            if clean.is_empty() || !clean.starts_with(|c: char| c.is_ascii_lowercase()) {
                "command.run".to_string()
            } else {
                format!("git.{clean}")
            }
        }
        _ => "command.run".to_string(),
    }
}

/// One repository's connection, behind its own lock.
///
/// The slot stays empty until an open succeeds: a failed open is returned to
/// its caller and retried by the next one, so one transient `SQLITE_BUSY` or
/// permission race cannot disable the ledger for the process lifetime.
#[derive(Default)]
pub(crate) struct Slot {
    conn: Mutex<Option<Live>>,
}

/// An open connection and the identity of the file it was opened on.
struct Live {
    conn: Connection,
    file: Option<FileId>,
}

/// Device and inode of the database file: enough to notice that
/// `.devcouncil/` was deleted and recreated underneath an open connection.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FileId(u64, u64);

#[cfg(unix)]
fn file_id(path: &Path) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|meta| FileId(meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_id(_path: &Path) -> Option<FileId> {
    None
}

type Registry = Mutex<HashMap<PathBuf, std::sync::Arc<Slot>>>;
type DroppedRegistry = Mutex<HashMap<PathBuf, u64>>;

/// One connection per repository, opened once.
pub(crate) fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Failed appends counted by the canonical database they would have reached.
///
/// Drop accounting is process-local, just as it was when this was one atomic
/// counter, but it must be repository-local: a broken checkout must not make a
/// healthy repository's history look incomplete. The database path is the same
/// canonical key used by the connection registry, so aliases of one checkout
/// cannot split its count.
fn dropped_registry() -> &'static DroppedRegistry {
    static DROPPED: OnceLock<DroppedRegistry> = OnceLock::new();
    DROPPED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_dropped_append(repo_path: &str) {
    let mut dropped = dropped_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let count = dropped.entry(ledger_path(repo_path)).or_default();
    *count = count.saturating_add(1);
}

/// Counts a bind/unbind whose catch-up beforehand did not finish. The write
/// went ahead and nothing was dropped: the rows the catch-up would have
/// recorded are still owed to a later pass. It is counted apart from dropped
/// appends ([`LedgerStatus::catch_up_failures`]), so it never reads as a lost
/// write, and never as a clean bind.
pub(crate) fn record_catch_up_failure(repo_path: &str) {
    let mut failures = catch_up_failure_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let count = failures.entry(ledger_path(repo_path)).or_default();
    *count = count.saturating_add(1);
}

/// [`record_catch_up_failure`]'s counts, by canonical database path.
fn catch_up_failure_registry() -> &'static DroppedRegistry {
    static FAILURES: OnceLock<DroppedRegistry> = OnceLock::new();
    FAILURES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn catch_up_failures(repo_path: &str) -> u64 {
    catch_up_failure_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&ledger_path(repo_path))
        .copied()
        .unwrap_or(0)
}

fn dropped_appends(repo_path: &str) -> u64 {
    dropped_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&ledger_path(repo_path))
        .copied()
        .unwrap_or(0)
}

/// The canonical spelling of a repository path.
///
/// One repository must have exactly one ledger, and a path can be spelled
/// several ways: through a symlink, with a trailing slash, or — on macOS — as
/// `/var/...` where the real path is `/private/var/...`. `validate_repo`
/// canonicalises before running git, so without this the same repository
/// accumulated two ledgers and neither held the whole history.
///
/// Falls back to the path as given when it cannot be canonicalised, which is
/// what a removed repository needs: a caller still gets a stable answer rather
/// than an error on a read.
pub(crate) fn canonical_repo(repo_path: &str) -> String {
    std::fs::canonicalize(repo_path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| repo_path.trim_end_matches('/').to_string())
}

/// Where a repository's ledger lives.
pub fn ledger_path(repo_path: &str) -> PathBuf {
    Path::new(&canonical_repo(repo_path))
        .join(".devcouncil")
        .join("ledger.sqlite")
}

/// Validates one state directory before it is read, created, or chmodded.
///
/// `.devcouncil` is repository-controlled input. Following a symlink there
/// would let a checkout redirect both the SQLite writer and the grant reader
/// to an arbitrary directory, and the writer used to chmod that target before
/// creating its database. The final component is therefore never allowed to
/// be a symlink, and repository-relative state must still resolve beneath the
/// authenticated repository root.
fn checked_state_directory(
    path: &Path,
    allowed_root: Option<&Path>,
    create: bool,
) -> Result<Option<PathBuf>, LedgerError> {
    let inspect = || -> Result<Option<std::fs::Metadata>, LedgerError> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => Ok(Some(metadata)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(LedgerError::new(
                "state_path_unreadable",
                format!("inspect {}: {error}", path.display()),
            )),
        }
    };

    let mut metadata = inspect()?;
    if metadata.is_none() && create {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            if let Err(error) = builder.create(path) {
                // An entry may have appeared between inspection and create.
                // It is accepted only after the same no-symlink validation.
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(LedgerError::new(
                        "mkdir_failed",
                        format!("{}: {error}", path.display()),
                    ));
                }
            }
        }
        #[cfg(not(unix))]
        if let Err(error) = std::fs::create_dir(path) {
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(LedgerError::new(
                    "mkdir_failed",
                    format!("{}: {error}", path.display()),
                ));
            }
        }
        metadata = inspect()?;
    }

    let Some(metadata) = metadata else {
        return Ok(None);
    };
    if metadata.file_type().is_symlink() {
        return Err(LedgerError::new(
            "state_path_symlink",
            format!("refusing symlink at state directory {}", path.display()),
        ));
    }
    if !metadata.is_dir() {
        return Err(LedgerError::new(
            "state_path_invalid",
            format!(
                "state directory path is not a directory: {}",
                path.display()
            ),
        ));
    }

    let resolved = std::fs::canonicalize(path).map_err(|error| {
        LedgerError::new(
            "state_path_unreadable",
            format!("resolve {}: {error}", path.display()),
        )
    })?;
    if let Some(root) = allowed_root {
        let root = std::fs::canonicalize(root).map_err(|error| {
            LedgerError::new(
                "state_root_unreadable",
                format!("resolve {}: {error}", root.display()),
            )
        })?;
        if !resolved.starts_with(&root) {
            return Err(LedgerError::new(
                "state_path_escape",
                format!(
                    "state directory {} resolves outside repository {}",
                    path.display(),
                    root.display()
                ),
            ));
        }
    }
    Ok(Some(resolved))
}

fn checked_state_file_metadata(
    path: &Path,
    allowed_root: Option<&Path>,
) -> Result<Option<std::fs::Metadata>, LedgerError> {
    let parent = path.parent().ok_or_else(|| {
        LedgerError::new(
            "state_path_invalid",
            format!("state file has no parent: {}", path.display()),
        )
    })?;
    if checked_state_directory(parent, allowed_root, false)?.is_none() {
        return Ok(None);
    }

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(LedgerError::new(
                "state_path_unreadable",
                format!("inspect {}: {error}", path.display()),
            ))
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(LedgerError::new(
            "state_path_symlink",
            format!("refusing symlink at state file {}", path.display()),
        ));
    }
    if !metadata.is_file() {
        return Err(LedgerError::new(
            "state_path_invalid",
            format!("state file path is not a regular file: {}", path.display()),
        ));
    }
    if let Some(root) = allowed_root {
        let root = std::fs::canonicalize(root).map_err(|error| {
            LedgerError::new(
                "state_root_unreadable",
                format!("resolve {}: {error}", root.display()),
            )
        })?;
        let resolved = std::fs::canonicalize(path).map_err(|error| {
            LedgerError::new(
                "state_path_unreadable",
                format!("resolve {}: {error}", path.display()),
            )
        })?;
        if !resolved.starts_with(&root) {
            return Err(LedgerError::new(
                "state_path_escape",
                format!(
                    "state file {} resolves outside repository {}",
                    path.display(),
                    root.display()
                ),
            ));
        }
    }
    Ok(Some(metadata))
}

fn same_file(before: &std::fs::Metadata, opened: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        before.dev() == opened.dev() && before.ino() == opened.ino()
    }
    #[cfg(not(unix))]
    {
        // Static symlinks were already refused above. Platforms without a
        // stable file identity in std still get that boundary validation.
        let _ = (before, opened);
        true
    }
}

#[cfg(unix)]
fn open_checked_state_directory(
    path: &Path,
    allowed_root: Option<&Path>,
) -> Result<std::fs::File, LedgerError> {
    checked_state_directory(path, allowed_root, false)?.ok_or_else(|| {
        LedgerError::new(
            "state_path_changed",
            format!("state directory disappeared: {}", path.display()),
        )
    })?;
    let before = std::fs::symlink_metadata(path).map_err(|error| {
        LedgerError::new(
            "state_path_unreadable",
            format!("inspect {}: {error}", path.display()),
        )
    })?;
    let directory = std::fs::File::open(path).map_err(|error| {
        LedgerError::new("state_open_failed", format!("{}: {error}", path.display()))
    })?;
    let opened = directory.metadata().map_err(|error| {
        LedgerError::new(
            "state_path_unreadable",
            format!("inspect opened {}: {error}", path.display()),
        )
    })?;
    checked_state_directory(path, allowed_root, false)?;
    let current = std::fs::symlink_metadata(path).map_err(|error| {
        LedgerError::new(
            "state_path_changed",
            format!(
                "state directory changed during open: {}: {error}",
                path.display()
            ),
        )
    })?;
    if !same_file(&before, &opened) || !same_file(&current, &opened) {
        return Err(LedgerError::new(
            "state_path_changed",
            format!(
                "state directory changed while it was opened: {}",
                path.display()
            ),
        ));
    }
    Ok(directory)
}

fn open_checked_state_file(
    path: &Path,
    allowed_root: Option<&Path>,
    write: bool,
) -> Result<Option<std::fs::File>, LedgerError> {
    let Some(before) = checked_state_file_metadata(path, allowed_root)? else {
        return Ok(None);
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(write)
        .open(path)
        .map_err(|error| {
            LedgerError::new("state_open_failed", format!("{}: {error}", path.display()))
        })?;
    let opened = file.metadata().map_err(|error| {
        LedgerError::new(
            "state_path_unreadable",
            format!("inspect opened {}: {error}", path.display()),
        )
    })?;
    let current = checked_state_file_metadata(path, allowed_root)?;
    if !same_file(&before, &opened)
        || current
            .as_ref()
            .is_none_or(|metadata| !same_file(metadata, &opened))
    {
        return Err(LedgerError::new(
            "state_path_changed",
            format!("state file changed while it was opened: {}", path.display()),
        ));
    }
    Ok(Some(file))
}

/// Reads a state file without following a repository-planted alias.
///
/// `allowed_root` is `Some(repo)` for repository-relative MANVI state and
/// `None` only for an operator-supplied absolute `MANVI_STATE_DIR`.
pub(crate) fn read_checked_state_file(
    path: &Path,
    allowed_root: Option<&Path>,
) -> Result<Option<String>, LedgerError> {
    let Some(mut file) = open_checked_state_file(path, allowed_root, false)? else {
        return Ok(None);
    };
    let mut raw = String::new();
    file.read_to_string(&mut raw).map_err(|error| {
        LedgerError::new("state_read_failed", format!("{}: {error}", path.display()))
    })?;
    Ok(Some(raw))
}

fn open(db_path: &Path) -> Result<Connection, LedgerError> {
    #[cfg(test)]
    tests::LEDGER_WRITE_OPENS.with(|opens| opens.set(opens.get() + 1));
    let dir = db_path.parent().ok_or_else(|| {
        LedgerError::new(
            "state_path_invalid",
            format!("ledger path has no parent: {}", db_path.display()),
        )
    })?;
    let repo = dir.parent().ok_or_else(|| {
        LedgerError::new(
            "state_path_invalid",
            format!(
                "ledger state directory has no repository: {}",
                dir.display()
            ),
        )
    })?;
    checked_state_directory(dir, Some(repo), true)?;
    secure_ledger_directory(dir, repo)?;
    validate_ledger_files(db_path, repo)?;
    precreate_private_ledger(db_path, repo)?;
    validate_ledger_files(db_path, repo)?;
    // Before the connection, never after it. Securing a file opens and closes
    // a descriptor to it, and on POSIX closing ANY descriptor to a file drops
    // every fcntl lock this process holds on that file — including the WAL
    // index lock a live connection depends on. Another process then believes
    // it is alone, reinitialises `-shm`, and this one faults with SIGBUS in
    // `walFindFrame` (sixteen concurrent hook processes reading one ledger died
    // 12–14 at a time). The `-wal`/`-shm` SQLite creates later inherit the
    // database file's mode, so 0600 here is 0600 for them too.
    secure_ledger_files(db_path, repo)?;
    let conn = Connection::open(db_path)
        .map_err(|e| LedgerError::new("open_failed", format!("{}: {e}", db_path.display())))?;
    // Opening by pathname is the one SQLite API available without widening
    // dependencies. Re-check before the first pragma can write, then again
    // after schema/WAL setup, so a swapped path is refused at both edges.
    checked_state_directory(dir, Some(repo), false)?;
    validate_ledger_files(db_path, repo)?;

    // A ledger a newer writer stamped is refused before anything below can touch
    // it: switching a rollback-journal file to WAL rewrites its header.
    let stamped = stamped_writer_version(&conn)?;
    if stamped > WRITER_VERSION {
        return Err(writer_too_old(db_path, stamped));
    }

    // WAL lets a reader page the history while the writer appends. NORMAL
    // trades a fsync per commit for one per checkpoint; the ledger is a record
    // of actions that git has already made durable itself, so losing the last
    // few milliseconds to a power cut costs a re-derivable row, not data.
    crate::engine::sqlite_wal::enter_wal(&conn, crate::engine::sqlite_wal::LEDGER_WAL_PATIENCE)
        .map_err(|e| LedgerError::new("pragma_failed", format!("journal_mode: {e}")))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| LedgerError::new("pragma_failed", format!("synchronous: {e}")))?;
    // Before the schema batch: a refused writer must not alter anything.
    ensure_schema(&conn, db_path)?;
    checked_state_directory(dir, Some(repo), false)?;
    validate_ledger_files(db_path, repo)?;
    Ok(conn)
}

/// Opens an existing ledger read-only, or `Ok(None)` when there is none.
///
/// The read half of [`open`], for a process that must not create, secure or
/// migrate the database: no directory or file is created, nothing is
/// chmodded, and a read-only connection cannot run the WAL pragmas or the
/// schema DDL. The same no-symlink, inside-the-repository validation runs
/// before and after the open, by metadata only — never by opening and closing
/// a descriptor, which would drop this process's SQLite locks (see [`open`]).
fn open_readonly(db_path: &Path) -> Result<Option<Connection>, LedgerError> {
    let dir = db_path.parent().ok_or_else(|| {
        LedgerError::new(
            "state_path_invalid",
            format!("ledger path has no parent: {}", db_path.display()),
        )
    })?;
    let repo = dir.parent().ok_or_else(|| {
        LedgerError::new(
            "state_path_invalid",
            format!(
                "ledger state directory has no repository: {}",
                dir.display()
            ),
        )
    })?;
    if checked_state_file_metadata(db_path, Some(repo))?.is_none() {
        return Ok(None);
    }
    validate_ledger_files(db_path, repo)?;
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| LedgerError::new("open_failed", format!("{}: {e}", db_path.display())))?;
    checked_state_directory(dir, Some(repo), false)?;
    validate_ledger_files(db_path, repo)?;
    Ok(Some(conn))
}

/// Whether a read-only connection's database has `table`. A creator that died
/// between creating the file and its schema leaves a database with none, and
/// a read-only reader cannot add them; such a ledger holds no rows.
fn has_table(conn: &Connection, table: &str) -> Result<bool, LedgerError> {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
    .map_err(|e| LedgerError::new("query_failed", e.to_string()))
}

#[cfg(unix)]
fn secure_ledger_directory(path: &Path, repo: &Path) -> Result<(), LedgerError> {
    use std::os::unix::fs::PermissionsExt;
    let directory = open_checked_state_directory(path, Some(repo))?;
    directory
        .set_permissions(std::fs::Permissions::from_mode(0o700))
        .map_err(|e| {
            LedgerError::new(
                "permissions_failed",
                format!("secure {}: {e}", path.display()),
            )
        })
}

#[cfg(not(unix))]
fn secure_ledger_directory(_path: &Path, _repo: &Path) -> Result<(), LedgerError> {
    Ok(())
}

fn precreate_private_ledger(path: &Path, repo: &Path) -> Result<(), LedgerError> {
    if checked_state_file_metadata(path, Some(repo))?.is_some() {
        return Ok(());
    }
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // A racing creator gets no trust from winning the race.
            checked_state_file_metadata(path, Some(repo))?
                .ok_or_else(|| {
                    LedgerError::new(
                        "state_path_changed",
                        format!(
                            "ledger appeared and disappeared during open: {}",
                            path.display()
                        ),
                    )
                })
                .map(|_| ())
        }
        Err(error) => Err(LedgerError::new(
            "open_failed",
            format!("{}: {error}", path.display()),
        )),
    }
}

fn ledger_files(db_path: &Path) -> [PathBuf; 3] {
    [
        db_path.to_path_buf(),
        PathBuf::from(format!("{}-wal", db_path.display())),
        PathBuf::from(format!("{}-shm", db_path.display())),
    ]
}

fn validate_ledger_files(db_path: &Path, repo: &Path) -> Result<(), LedgerError> {
    for path in ledger_files(db_path) {
        checked_state_file_metadata(&path, Some(repo))?;
    }
    Ok(())
}

#[cfg(unix)]
fn secure_ledger_files(db_path: &Path, repo: &Path) -> Result<(), LedgerError> {
    use std::os::unix::fs::PermissionsExt;
    for path in ledger_files(db_path) {
        // Already private: nothing to change, so no descriptor is opened. A
        // close can cost a connection elsewhere in this process its locks (see
        // `open`), so the open-and-fchmod below runs only for a file an older
        // build left wider — once, not on every process start.
        match checked_state_file_metadata(&path, Some(repo))? {
            Some(metadata) if metadata.permissions().mode() & 0o777 == 0o600 => continue,
            Some(_) => {}
            None => continue,
        }
        let Some(file) = open_checked_state_file(&path, Some(repo), true)? else {
            continue;
        };
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| {
                LedgerError::new(
                    "permissions_failed",
                    format!("secure {}: {e}", path.display()),
                )
            })?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_ledger_files(_db_path: &Path, _repo: &Path) -> Result<(), LedgerError> {
    Ok(())
}

/// Runs `f` against this repo's connection.
///
/// The registry lock covers only the map lookup; the open, the schema DDL and
/// `f` run under the repository's own slot lock, so one busy or slow ledger
/// stalls that repository alone. Cross-process writers are serialised by
/// SQLite (WAL plus the busy timeout set in [`open`]), which is also what the
/// schema's monotonic cursor rests on: it is per database, so no lock shared
/// between repositories ever protected it.
///
/// An open that fails is not remembered. A connection whose database file was
/// deleted or replaced is dropped and reopened, rather than appending to an
/// unlinked inode while [`status`] reports `recording`.
fn with_conn<T>(
    repo_path: &str,
    f: impl FnOnce(&Connection) -> Result<T, LedgerError>,
) -> Result<T, LedgerError> {
    let db_path = ledger_path(repo_path);
    let slot = {
        let mut reg = registry().lock().map_err(|_| {
            LedgerError::new("registry_poisoned", "ledger registry mutex was poisoned")
        })?;
        std::sync::Arc::clone(reg.entry(db_path.clone()).or_default())
    };
    let mut guard = slot
        .conn
        .lock()
        .map_err(|_| LedgerError::new("conn_poisoned", "ledger connection mutex was poisoned"))?;

    if guard
        .as_ref()
        .is_some_and(|live| live.file.is_some() && file_id(&db_path) != live.file)
    {
        *guard = None;
    }
    if guard.is_none() {
        let conn = open(&db_path)?;
        *guard = Some(Live {
            conn,
            file: file_id(&db_path),
        });
    }
    match guard.as_ref() {
        Some(live) => f(&live.conn),
        None => Err(LedgerError::new("open_failed", "ledger connection missing")),
    }
}

/// Runs `f` on this process's already-open connection to the ledger, or returns
/// `None` when there is none (never opened, or its file was replaced). It never
/// opens, creates or caches anything.
///
/// For the small reads every resolve makes (the family's pass marker, the
/// history-only count). Each used to open a fresh read-only connection, and
/// SQLite keeps a descriptor of a connection closed while another connection to
/// the same inode is open until that inode is fully released: a burst of
/// concurrent resolves (a hundred terminal sessions) left one descriptor per
/// concurrent open, for as long as the shared connection lived.
fn with_live_conn<T>(
    repo_path: &str,
    f: impl FnOnce(&Connection) -> Result<T, LedgerError>,
) -> Option<Result<T, LedgerError>> {
    let db_path = ledger_path(repo_path);
    let slot = registry().lock().ok()?.get(&db_path).cloned()?;
    let guard = slot.conn.lock().ok()?;
    let live = guard.as_ref()?;
    if live.file.is_some() && file_id(&db_path) != live.file {
        return None;
    }
    Some(f(&live.conn))
}

/// One read-only fallback connection at a time, for a process with no live one:
/// it bounds the descriptors such opens can leave behind to one.
static FALLBACK_OPEN: Mutex<()> = Mutex::new(());

/// Begins a write transaction that takes the write lock up front.
///
/// A deferred transaction that reads first and writes later must upgrade its
/// lock mid-flight, and SQLite fails that upgrade with `SQLITE_BUSY` at once
/// when another process wrote in between — the busy timeout does not apply.
/// `IMMEDIATE` waits for the lock at `BEGIN`, where the timeout does. Every
/// multi-statement writer goes through here.
fn write_tx(conn: &Connection) -> rusqlite::Result<rusqlite::Transaction<'_>> {
    rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
}

/// How a read reaches the database.
#[derive(Clone, Copy)]
enum Access {
    /// The shared writable connection: creates and migrates on first use.
    Writable,
    /// A fresh `SQLITE_OPEN_READ_ONLY` connection: never creates, migrates,
    /// chmods or checkpoints, so a read-only tool cannot change the ledger.
    ReadOnly,
}

fn with_access<T>(
    repo_path: &str,
    access: Access,
    f: impl FnOnce(&Connection) -> Result<T, LedgerError>,
) -> Result<T, LedgerError> {
    match access {
        Access::Writable => with_conn(repo_path, f),
        Access::ReadOnly => {
            let conn = open_readonly_initialised(&ledger_path(repo_path))?;
            f(&conn)
        }
    }
}

/// [`open_readonly`] for a reader that needs a ledger to read: a missing
/// file or a database with no schema yet is reported, never read as empty.
fn open_readonly_initialised(db_path: &Path) -> Result<Connection, LedgerError> {
    let not_initialised = |message: &str| LedgerError::new("not_initialised", message);
    let conn = open_readonly(db_path)?.ok_or_else(|| {
        not_initialised("no ledger in this repository yet; nothing has been recorded")
    })?;
    if !has_table(&conn, "events")? {
        return Err(not_initialised(
            "the ledger file has no schema yet; nothing has been recorded",
        ));
    }
    Ok(conn)
}

/// Appends one event and returns its cursor id.
///
/// Redaction happens in [`prepare`] rather than at any call site, so a caller
/// cannot forget it. Every new event — here, and in the batches
/// [`advance_transcripts`] commits — passes through [`prepare`] and [`insert`];
/// only the legacy import copies rows, already redacted when first written.
pub fn append(draft: Draft) -> Result<i64, LedgerError> {
    append_numbered(draft, 0)
}

/// [`append`] for a `reflog.*` row that is one of several identical entries of
/// a replay pass: `ordinal` counts them from 0. Two processes replaying one
/// pass number its entries the same way, so the unique reflog identity index
/// refuses the second copy of each (`duplicate_reflog`). The ordinal means
/// nothing to any other action.
pub fn append_numbered(draft: Draft, ordinal: i64) -> Result<i64, LedgerError> {
    let mut event = prepare(&draft)?;
    if event.reflog_ordinal.is_some() {
        event.reflog_ordinal = Some(ordinal);
    }
    let result = with_conn(&draft.repo_path, |conn| insert(conn, &event));

    match &result {
        Ok(cursor) => announce(&draft.repo_path, *cursor),
        // The entry is already in the ledger: nothing was lost.
        Err(e) if e.code == "duplicate_reflog" => {}
        Err(_) => {
            // Counted, not swallowed. `status()` reports it, and the UI can say
            // the history it is showing is known to be incomplete.
            record_dropped_append(&draft.repo_path);
        }
    }
    result
}

/// A draft validated, stamped and redacted: exactly what one row will hold.
///
/// Built before the connection lock is taken, because redaction is the
/// expensive part of an append and the lock is app-wide.
struct Prepared {
    ulid: String,
    ts: String,
    repo_path: String,
    worktree_path: Option<String>,
    actor_kind: &'static str,
    actor_id: Option<String>,
    session_id: Option<String>,
    task_id: Option<String>,
    action: String,
    object: Option<String>,
    argv: Option<String>,
    outcome: &'static str,
    verdict: Option<String>,
    before_ref: Option<String>,
    after_ref: Option<String>,
    duration_ms: Option<i64>,
    detail: Option<String>,
    /// The typed `occurred_at` column: the draft's event-time stamp, only when
    /// it is well-formed (the same check that lets it skip the redactor).
    occurred_at: Option<String>,
    /// `Some(0)` on a reflog row, `None` otherwise. See [`insert`].
    reflog_ordinal: Option<i64>,
    /// [`TranscriptRow::key`], for a transcript row: its `ulid` is derived from
    /// it, and [`insert_transcript_row`] refuses a second row with the same one.
    transcript_row: Option<String>,
}

/// `contracts/event.schema.json`'s action pattern,
/// `^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$`: two or more dot-separated words,
/// each starting with a lowercase letter.
fn valid_action(action: &str) -> bool {
    let mut words = 0;
    for word in action.split('.') {
        let mut chars = word.chars();
        let ok = chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !ok {
            return false;
        }
        words += 1;
    }
    words >= 2
}

fn prepare(draft: &Draft) -> Result<Prepared, LedgerError> {
    if draft.repo_path.is_empty() {
        return Err(LedgerError::new(
            "no_repo",
            "an event must name a repository",
        ));
    }
    if draft.action.is_empty() {
        return Err(LedgerError::new(
            "no_action",
            "an event must name an action",
        ));
    }

    if !valid_action(&draft.action) {
        return Err(LedgerError::new(
            "invalid_action",
            "an action must be a dotted lowercase name such as git.commit",
        ));
    }

    // Stored canonically for the same reason the file is located canonically:
    // otherwise one repository appears under two names in one database.
    let repo_path = canonical_repo(&draft.repo_path);

    let ms = ids::now_millis();

    // Unspecified actor and outcome are recorded as what they are. Defaulting
    // an unknown outcome to `ok` would be a lie the ledger then preserves.
    let actor_kind = draft.actor_kind.unwrap_or(ActorKind::System);
    let outcome = draft.outcome.unwrap_or(Outcome::Ok);

    let (object, detail) = store_object_and_detail(
        draft.object.as_deref(),
        draft.detail_json.as_deref(),
        draft.event_stamp.as_ref(),
    );

    // Redact every caller-controlled string at the one write boundary. The
    // verdict repeats the judged command in `target`, so redacting only argv
    // leaves a second full copy of the same credential in the same row.
    let transcript_row = draft.detail_json.as_deref().and_then(transcript_row_of);
    let ulid = match &transcript_row {
        Some(key) => {
            // The time part is the line's own, so a re-read gives the same
            // ulid; a row stamped at ingest time has none, and takes zero.
            let event_ms = draft
                .event_stamp
                .as_ref()
                .filter(|stamp| stamp.occurred_at_basis == "transcript" && stamp.is_well_formed())
                .and_then(|stamp| ids::parse_event_time(&stamp.occurred_at))
                .unwrap_or(0);
            // Spelled as the repository is: a trailing slash or a symlink is
            // the same worktree.
            let worktree_identity = draft
                .worktree_path
                .as_deref()
                .map(canonical_repo)
                .unwrap_or_default();
            let identity = [
                repo_path.as_str(),
                worktree_identity.as_str(),
                draft.actor_id.as_deref().unwrap_or(""),
                draft.session_id.as_deref().unwrap_or(""),
                key.as_str(),
            ]
            .join("\0");
            ids::identity_ulid(event_ms, identity.as_bytes())
        }
        None => ids::ulid(ms),
    };
    Ok(Prepared {
        ulid,
        ts: ids::iso8601_utc(ms),
        repo_path: redact::text(&repo_path),
        worktree_path: draft.worktree_path.as_deref().map(redact::text),
        actor_kind: actor_kind.as_str(),
        actor_id: draft.actor_id.as_deref().map(redact::text),
        session_id: draft.session_id.as_deref().map(redact::text),
        task_id: draft.task_id.as_deref().map(redact::text),
        action: redact::text(&draft.action),
        object,
        argv: draft.argv_json.as_deref().map(redact::text),
        outcome: outcome.as_str(),
        verdict: draft.verdict_json.as_deref().map(redact::text),
        before_ref: draft.before_ref.as_deref().map(redact::text),
        after_ref: draft.after_ref.as_deref().map(redact::text),
        duration_ms: draft.duration_ms,
        detail,
        occurred_at: draft
            .event_stamp
            .as_ref()
            .filter(|stamp| stamp.is_well_formed())
            .map(|stamp| stamp.occurred_at.clone()),
        reflog_ordinal: draft.action.starts_with("reflog.").then_some(0),
        transcript_row,
    })
}

/// The `object` and `detail_json` a row is stored with: the one place an
/// object is redacted, capped, and its cut recorded, shared by every writer
/// ([`prepare`] and the legacy family import) so none can store one that
/// another would have cut.
///
/// The object is redacted first and cut second: cutting the raw text could
/// leave half a credential that the redactor no longer recognises. The cut is
/// recorded in the detail by [`redact_detail`].
fn store_object_and_detail(
    object: Option<&str>,
    detail: Option<&str>,
    stamp: Option<&EventStamp>,
) -> (Option<String>, Option<String>) {
    let (object, cap) = match object.map(redact::text) {
        Some(redacted) => {
            let (object, cap) = cap_object(redacted);
            (Some(object), cap)
        }
        None => (None, None),
    };
    (object, redact_detail(detail, stamp, cap.as_ref()))
}

/// Inserts one prepared row.
///
/// A reflog row is written with the draft's `reflog_ordinal` (0 when it names
/// none), and the unique reflog identity index refuses a second row with the
/// same identity and ordinal; that refusal is `INSERT OR IGNORE` in effect
/// (code `duplicate_reflog`, which [`append`] neither counts as dropped nor
/// announces, and [`record_outcome`] reports as [`Recorded::Duplicate`]). The
/// replay numbers identical entries of a second 0, 1, 2 in the reflog's own
/// order, and carries that numbering across passes through the watermark
/// (`reflog_watermarks`, see `ingest::replay_ref`).
fn insert(conn: &Connection, event: &Prepared) -> Result<i64, LedgerError> {
    insert_with(conn, event, INSERT_SQL)?
        .ok_or_else(|| LedgerError::new("insert_failed", "an insert changed no row"))
}

const INSERT_SQL: &str = "INSERT INTO events (
             ulid, ts_utc, schema_version, repo_path, worktree_path,
             actor_kind, actor_id, session_id, task_id, action, object,
             argv_json, outcome, verdict_json, before_ref, after_ref,
             duration_ms, detail_json, occurred_at, reflog_ordinal
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)";

/// What [`insert_transcript_row`] did.
enum TranscriptInsert {
    Recorded(i64),
    /// The ledger already held this row.
    Held,
    /// The ulid is held by a different row.
    Collision,
}

/// Inserts a transcript row, or finds it already held. Only the unique `ulid`
/// is tolerated: a CHECK or NOT NULL failure is still an error. A held ulid
/// is taken for the same row only if the row there names the same identity;
/// a different one is a collision of the ulid's 80-bit body, reported as
/// [`TranscriptInsert::Collision`] so the caller counts it as a failed write
/// rather than dropping the row as a duplicate.
fn insert_transcript_row(
    conn: &Connection,
    event: &Prepared,
) -> Result<TranscriptInsert, LedgerError> {
    static SQL: OnceLock<String> = OnceLock::new();
    let sql = SQL.get_or_init(|| format!("{INSERT_SQL} ON CONFLICT(ulid) DO NOTHING"));
    if let Some(id) = insert_with(conn, event, sql)? {
        return Ok(TranscriptInsert::Recorded(id));
    }
    // The held row is the same one only if it names the same line AND says the
    // same thing about it: the action and object are functions of the line
    // alone (unlike the task and its basis, which a later binding changes), so
    // a row that differs in either is not what this read would have written.
    let held: Option<(String, Option<String>, Option<String>)> = conn
        .query_row(
            &format!(
                "SELECT action, object,
                        CASE WHEN json_valid(detail_json)
                             THEN json_extract(detail_json, '$.{TRANSCRIPT_ROW_KEY}') END
                 FROM events WHERE ulid = ?1"
            ),
            params![event.ulid],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| LedgerError::new("insert_failed", e.to_string()))?;
    match (held, &event.transcript_row) {
        (Some((action, object, Some(held))), Some(key))
            if held == *key && action == event.action && object == event.object =>
        {
            Ok(TranscriptInsert::Held)
        }
        _ => Ok(TranscriptInsert::Collision),
    }
}

/// Runs `sql` (an [`INSERT_SQL`] with an optional conflict clause) for `event`;
/// `None` when the conflict clause turned the insert into nothing.
fn insert_with(conn: &Connection, event: &Prepared, sql: &str) -> Result<Option<i64>, LedgerError> {
    let changed = conn
        .execute(
            sql,
            params![
                event.ulid,
                event.ts,
                SCHEMA_VERSION,
                event.repo_path,
                event.worktree_path,
                event.actor_kind,
                event.actor_id,
                event.session_id,
                event.task_id,
                event.action,
                event.object,
                event.argv,
                event.outcome,
                event.verdict,
                event.before_ref,
                event.after_ref,
                event.duration_ms,
                event.detail,
                event.occurred_at,
                event.reflog_ordinal,
            ],
        )
        .map_err(|e| match e {
            // Another writer (the app and `gitpulsed` both replay the reflog)
            // recorded this entry first. Not data loss, and not an insert failure.
            rusqlite::Error::SqliteFailure(code, Some(message))
                if code.code == rusqlite::ErrorCode::ConstraintViolation
                    && message.contains(REFLOG_IDENTITY_INDEX) =>
            {
                LedgerError::new("duplicate_reflog", message)
            }
            other => LedgerError::new("insert_failed", other.to_string()),
        })?;
    Ok((changed == 1).then(|| conn.last_insert_rowid()))
}

// --- transcript catch-up progress --------------------------------------
//
// How far transcript catch-up has read, kept in the same database as the
// events it produced and committed in the same transaction as them, so the
// two cannot disagree: a crash either keeps both the rows and the advance,
// or neither.
//
// Paths are never stored. Keys are FNV-1a 64 digests of the raw path bytes,
// the same treatment `ledger_family_imports` gives its sources: a transcript
// path embeds the agent's working directory, and the events table only ever
// holds such a string redacted.

/// The persisted key for a worktree or transcript path.
pub(crate) fn transcript_key(path: &Path) -> String {
    format!("{:016x}", ids::fnv1a64(path.as_os_str().as_encoded_bytes()))
}

/// What transcript catch-up has already done for one worktree.
#[derive(Debug, Default)]
pub(crate) struct TranscriptProgress {
    /// Bytes of each transcript already consumed, by [`transcript_key`].
    pub offsets: HashMap<String, u64>,
    /// The fingerprint of the bytes ending at each offset ([`cursor_fingerprint`]),
    /// by the same key. Absent for a cursor written before fingerprints existed
    /// (a NULL column), which is trusted on its offset alone.
    pub fingerprints: HashMap<String, String>,
    /// The timestamp watermark that governed catch-up before byte offsets
    /// existed, frozen the first time this worktree was caught up under
    /// offsets. `None` until then; empty when there was no watermark.
    pub baseline: Option<String>,
}

#[cfg(test)]
pub(crate) fn transcript_progress(
    repo_path: &str,
    worktree_path: &str,
) -> Result<TranscriptProgress, LedgerError> {
    transcript_progress_many(repo_path, &[worktree_path])?
        .pop()
        .ok_or_else(|| LedgerError::new("query_failed", "no transcript progress was returned"))
}

/// [`transcript_progress`] for several worktrees of one repository under one
/// lock and one pass over the ledger connection, in the order asked.
pub(crate) fn transcript_progress_many(
    repo_path: &str,
    worktree_paths: &[&str],
) -> Result<Vec<TranscriptProgress>, LedgerError> {
    let keys: Vec<String> = worktree_paths
        .iter()
        .map(|path| transcript_key(Path::new(path)))
        .collect();
    with_conn(repo_path, |conn| {
        let failed = |e: rusqlite::Error| LedgerError::new("query_failed", e.to_string());
        let mut baseline_stmt = conn
            .prepare("SELECT since_utc FROM transcript_baselines WHERE worktree_key = ?1")
            .map_err(failed)?;
        let mut offsets_stmt = conn
            .prepare(
                "SELECT file_key, byte_offset, fingerprint FROM transcript_cursors
                 WHERE worktree_key = ?1",
            )
            .map_err(failed)?;
        keys.iter()
            .map(|worktree| {
                let baseline = baseline_stmt
                    .query_row(params![worktree], |row| row.get::<_, String>(0))
                    .optional()
                    .map_err(failed)?;
                let rows = offsets_stmt
                    .query_map(params![worktree], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    })
                    .map_err(failed)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(failed)?;
                let mut offsets = HashMap::new();
                let mut fingerprints = HashMap::new();
                for (key, offset, fingerprint) in rows {
                    if let Some(fingerprint) = fingerprint {
                        fingerprints.insert(key.clone(), fingerprint);
                    }
                    offsets.insert(key, u64::try_from(offset).unwrap_or(0));
                }
                Ok(TranscriptProgress {
                    offsets,
                    fingerprints,
                    baseline,
                })
            })
            .collect()
    })
}

/// Freezes `since` as this worktree's baseline unless one is already frozen,
/// and returns whichever is in force. The app and `gitpulsed` can race here;
/// the first writer wins and both then agree.
pub(crate) fn freeze_transcript_baseline(
    repo_path: &str,
    worktree_path: &str,
    since: &str,
) -> Result<String, LedgerError> {
    let worktree = transcript_key(Path::new(worktree_path));
    with_conn(repo_path, |conn| {
        let failed = |e: rusqlite::Error| LedgerError::new("baseline_failed", e.to_string());
        conn.execute(
            "INSERT INTO transcript_baselines (worktree_key, since_utc) VALUES (?1, ?2)
             ON CONFLICT(worktree_key) DO NOTHING",
            params![worktree, since],
        )
        .map_err(failed)?;
        conn.query_row(
            "SELECT since_utc FROM transcript_baselines WHERE worktree_key = ?1",
            params![worktree],
            |row| row.get(0),
        )
        .map_err(failed)
    })
}

/// One transcript read forward from `from` to `to`, with the events found in
/// the bytes between.
#[derive(Debug)]
pub(crate) struct TranscriptAdvance {
    pub file_key: String,
    /// The offset this read started from, as [`transcript_progress`] reported
    /// it; `None` for a transcript never read for this worktree.
    pub from: Option<u64>,
    pub to: u64,
    /// The fingerprint of the bytes ending at `to` ([`cursor_fingerprint`]),
    /// stored beside the offset so a later read can tell the file it stopped in
    /// from one rewritten past that offset. `None` leaves the check off for this
    /// cursor (the last line was too long to keep, or nothing was read).
    pub fingerprint: Option<String>,
    pub drafts: Vec<Draft>,
}

/// How many bytes each half of a cursor's fingerprint covers, at most.
pub(crate) const CURSOR_FINGERPRINT_BYTES: usize = 64;

/// A file's `(len, mtime in nanoseconds since the epoch)`, as a fingerprint
/// folds it in.
pub(crate) type FileStat = (u64, u64);

/// The fingerprint stored with a transcript cursor:
/// `"v3:{h}:{t}:{len}:{mtime_ns}:{head_hash}:{tail_hash}"`, where the hashes are
/// digests (the ledger's FNV-1a) of the first `h` bytes of the file and of the
/// `t` bytes that end at the cursor, each at most [`CURSOR_FINGERPRINT_BYTES`],
/// and `len`/`mtime_ns` are the file's [`FileStat`] when the cursor was written
/// (`-` when it could not be read). The tail has a digest of its own so a
/// rewrite that moved it (a record prepended, the head changed) can be told from
/// one that replaced it: the new file is searched for the bytes that digest
/// names, and a cursor that finds them resumes after them.
///
/// Both byte halves are needed. The bytes before the cursor are what a resume
/// depends on, but a transcript's lines end alike (the same trailing keys), so
/// a rewrite that changes only what comes earlier in each line leaves that tail
/// as it was; the head of the file is where a rewritten transcript differs. The
/// two lengths are in the text because a cursor near the start of a file covers
/// fewer bytes than one deep in it. A digest, never the bytes: a transcript's
/// text may hold anything.
///
/// The stat is what lets an unchanged file cost one `stat` per pass: a file
/// whose length and mtime are still the recorded ones is not opened. The
/// unversioned `"{h}:{t}:{hash}"` the first build of this column wrote (one
/// digest of both halves, no stat) is still read: it is re-hashed on every pass
/// and cannot be searched for. `None` when either half is empty (nothing read yet).
pub(crate) fn cursor_fingerprint(
    head: &[u8],
    tail: &[u8],
    stat: Option<FileStat>,
) -> Option<String> {
    if head.is_empty() || tail.is_empty() {
        return None;
    }
    let (head_hash, tail_hash) = (ids::fnv1a64(head), cursor_tail_hash(tail));
    Some(match stat {
        Some((len, mtime_ns)) => format!(
            "v3:{}:{}:{len}:{mtime_ns}:{head_hash:016x}:{tail_hash:016x}",
            head.len(),
            tail.len()
        ),
        None => format!(
            "v3:{}:{}:-:-:{head_hash:016x}:{tail_hash:016x}",
            head.len(),
            tail.len()
        ),
    })
}

/// The digest a fingerprint keeps of its tail bytes.
pub(crate) fn cursor_tail_hash(tail: &[u8]) -> u64 {
    ids::fnv1a64(tail)
}

/// Whether `head` and `tail` (read from a file as it is now, `parts.head` and
/// `parts.tail` bytes long) are the bytes `fingerprint` was taken from.
pub(crate) fn cursor_fingerprint_holds(
    fingerprint: &str,
    parts: &FingerprintParts,
    head: &[u8],
    tail: &[u8],
) -> bool {
    if parts.tail_hash.is_some() {
        cursor_fingerprint(head, tail, parts.stat).as_deref() == Some(fingerprint)
    } else {
        let hash = ids::fnv1a64(&[head, tail].concat());
        fingerprint == format!("{}:{}:{hash:016x}", head.len(), tail.len())
    }
}

/// What a stored fingerprint covers: the byte counts of its two halves, the
/// file stat it recorded if it did, and the tail's digest if it kept one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FingerprintParts {
    pub head: usize,
    pub tail: usize,
    pub stat: Option<FileStat>,
    pub tail_hash: Option<u64>,
}

/// Reads [`cursor_fingerprint`]'s text back, or `None` for text this build did
/// not write (which is then not checked).
pub(crate) fn parse_cursor_fingerprint(fingerprint: &str) -> Option<FingerprintParts> {
    let valid = 1..=CURSOR_FINGERPRINT_BYTES;
    let counts = |head: &str, tail: &str| {
        let (head, tail) = (head.parse::<usize>().ok()?, tail.parse::<usize>().ok()?);
        (valid.contains(&head) && valid.contains(&tail)).then_some((head, tail))
    };
    let mut fields = fingerprint.split(':');
    let first = fields.next()?;
    let parts = if first == "v3" {
        let (head, tail) = counts(fields.next()?, fields.next()?)?;
        let (len, mtime) = (fields.next()?, fields.next()?);
        let stat = match (len, mtime) {
            ("-", "-") => None,
            _ => Some((len.parse().ok()?, mtime.parse().ok()?)),
        };
        fields.next()?;
        let tail_hash = u64::from_str_radix(fields.next()?, 16).ok()?;
        FingerprintParts {
            head,
            tail,
            stat,
            tail_hash: Some(tail_hash),
        }
    } else {
        let (head, tail) = counts(first, fields.next()?)?;
        fields.next()?;
        FingerprintParts {
            head,
            tail,
            stat: None,
            tail_hash: None,
        }
    };
    fields.next().is_none().then_some(parts)
}

/// Records each advance's events and moves its offset, all in one
/// transaction.
///
/// Compare-and-set: an advance whose `from` no longer matches the stored
/// offset was overtaken by a concurrent pass (the app and `gitpulsed` both
/// catch up), and is dropped whole — its events are the other pass's to
/// record, and recording them here too would duplicate them. Returns the
/// number of events recorded.
#[cfg(test)]
pub(crate) fn advance_transcripts(
    repo_path: &str,
    worktree_path: &str,
    advances: Vec<TranscriptAdvance>,
) -> Result<i64, LedgerError> {
    advance_transcripts_family(repo_path, vec![(worktree_path, advances)])
        .map(|commit| commit.recorded)
}

/// What [`advance_transcripts_family`] did with the rows it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TranscriptCommit {
    /// Rows written.
    pub recorded: i64,
    /// Rows the ledger already held (same identity, so same `ulid`): a line read
    /// again after its cursor was lost or went stale.
    pub refused: i64,
}

/// [`advance_transcripts`] for several worktrees of one repository at once:
/// every worktree's events and cursor move in a single transaction, so a pass
/// that read one transcript for all of them cannot leave some advanced and
/// some not. The compare-and-set is per worktree and per transcript.
pub(crate) fn advance_transcripts_family(
    repo_path: &str,
    advances: Vec<(&str, Vec<TranscriptAdvance>)>,
) -> Result<TranscriptCommit, LedgerError> {
    let advances: Vec<(String, Vec<TranscriptAdvance>)> = advances
        .into_iter()
        .map(|(path, advances)| (transcript_key(Path::new(path)), advances))
        .collect();
    let prepared = advances
        .iter()
        .map(|(_, advances)| {
            advances
                .iter()
                .map(|advance| advance.drafts.iter().map(prepare).collect())
                .collect::<Result<Vec<Vec<Prepared>>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let result = with_conn(repo_path, |conn| {
        let failed = |e: rusqlite::Error| LedgerError::new("insert_failed", e.to_string());
        // IMMEDIATE takes the write lock before the offset is read, so a
        // second process cannot move it between the check and the write.
        let tx = write_tx(conn).map_err(failed)?;
        let mut recorded = 0i64;
        let mut refused = 0i64;
        let mut newest = None;
        for ((worktree, advances), prepared) in advances.iter().zip(&prepared) {
            for (advance, events) in advances.iter().zip(prepared) {
                let stored = tx
                    .query_row(
                        "SELECT byte_offset FROM transcript_cursors
                         WHERE worktree_key = ?1 AND file_key = ?2",
                        params![worktree, advance.file_key],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()
                    .map_err(failed)?
                    .map(|offset| u64::try_from(offset).unwrap_or(0));
                if stored != advance.from {
                    continue;
                }
                for event in events {
                    if event.transcript_row.is_none() {
                        newest = Some(insert(&tx, event)?);
                        recorded += 1;
                        continue;
                    }
                    match insert_transcript_row(&tx, event)? {
                        TranscriptInsert::Recorded(id) => {
                            newest = Some(id);
                            recorded += 1;
                        }
                        TranscriptInsert::Held => refused += 1,
                        TranscriptInsert::Collision => {
                            log::warn!(
                                "transcript row {} collides with a different row",
                                event.ulid
                            );
                            record_dropped_append(repo_path);
                        }
                    }
                }
                tx.execute(
                    "INSERT INTO transcript_cursors (worktree_key, file_key, byte_offset, fingerprint)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(worktree_key, file_key) DO UPDATE SET
                        byte_offset = excluded.byte_offset,
                        fingerprint = excluded.fingerprint",
                    params![
                        worktree,
                        advance.file_key,
                        i64::try_from(advance.to).unwrap_or(i64::MAX),
                        advance.fingerprint
                    ],
                )
                .map_err(failed)?;
            }
        }
        tx.commit().map_err(failed)?;
        Ok((recorded, refused, newest))
    });
    match result {
        Ok((recorded, refused, newest)) => {
            if let Some(cursor) = newest {
                announce(repo_path, cursor);
            }
            Ok(TranscriptCommit { recorded, refused })
        }
        Err(e) => {
            for _ in prepared.iter().flatten().flatten() {
                record_dropped_append(repo_path);
            }
            Err(e)
        }
    }
}

/// Forgets the offsets of transcripts that no longer exist, so the table is
/// bounded by the corpus rather than by its history.
pub(crate) fn forget_transcripts(
    repo_path: &str,
    worktree_path: &str,
    file_keys: &[String],
) -> Result<(), LedgerError> {
    if file_keys.is_empty() {
        return Ok(());
    }
    let worktree = transcript_key(Path::new(worktree_path));
    with_conn(repo_path, |conn| {
        let failed = |e: rusqlite::Error| LedgerError::new("forget_failed", e.to_string());
        let tx = write_tx(conn).map_err(failed)?;
        for key in file_keys {
            tx.execute(
                "DELETE FROM transcript_cursors WHERE worktree_key = ?1 AND file_key = ?2",
                params![worktree, key],
            )
            .map_err(failed)?;
        }
        tx.commit().map_err(failed)
    })
}

/// Appends without propagating failure, for the mutation path.
///
/// A repository whose ledger cannot be written is still a repository the user
/// may commit in — GitPulse degrades rather than refuses, exactly as it does
/// with no harness installed. The failure is counted and surfaced by
/// [`status`]; it is never silent, and it never blocks the action.
pub fn record(draft: Draft) -> Option<i64> {
    match record_outcome(draft, 0) {
        Recorded::Appended(id) => Some(id),
        Recorded::Duplicate | Recorded::Failed => None,
    }
}

/// What [`record_outcome`] did with a draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    /// A new row, at this cursor id.
    Appended(i64),
    /// The ledger already holds this reflog entry (same worktree, object, event
    /// time and ordinal): another pass or process recorded it first. Nothing
    /// was lost and nothing counts as a dropped append, but a caller that
    /// reports how many entries it recorded must say how many it refused.
    Duplicate,
    /// The append failed; it is counted by [`status`] and logged.
    Failed,
}

/// [`record`] that says which of the three things happened, for a row that is
/// the `ordinal`-th of several identical `reflog.*` entries (see
/// [`append_numbered`]; 0 for any other row).
pub fn record_outcome(draft: Draft, ordinal: i64) -> Recorded {
    match append_numbered(draft, ordinal) {
        Ok(id) => Recorded::Appended(id),
        Err(e) if e.code == "duplicate_reflog" => {
            log::debug!("ledger append refused as a duplicate reflog entry: {e}");
            Recorded::Duplicate
        }
        Err(e) => {
            log::warn!("ledger append failed: {e}");
            Recorded::Failed
        }
    }
}

/// Reads events after `cursor`, oldest first, at most `limit`.
///
/// Paging forward from a cursor rather than reading a window from the end is
/// what makes the UI a projection: a consumer holds the last id it has seen and
/// asks for what followed, and the answer is the same whether it has been
/// listening for an hour or has just opened the app.
pub fn tail(repo_path: &str, cursor: i64, limit: u32) -> Result<Vec<LedgerEvent>, LedgerError> {
    read_after(repo_path, cursor, limit, false, Access::Writable, None).map(|(events, _)| events)
}

/// The newest `limit` events after `cursor`, oldest first.
///
/// For a reader that keeps only the most recent rows. Paging [`tail`] forward
/// from 0 to reach the newest rows reads the whole history, one request per
/// page. The highest id returned is still the highest id after `cursor`, so a
/// reader that continues with [`tail`] from it misses nothing new. Rows
/// skipped below the window are not checked for a repository mismatch,
/// because they are not read at all.
pub fn tail_newest(
    repo_path: &str,
    cursor: i64,
    limit: u32,
) -> Result<Vec<LedgerEvent>, LedgerError> {
    read_after(repo_path, cursor, limit, true, Access::Writable, None).map(|(events, _)| events)
}

/// The one authority order of a worktree's bind and unbind rows *within one
/// ledger*, newest first: insertion sequence (`id`), which `write_tx`
/// (`BEGIN IMMEDIATE`) serialises across processes. `ts_utc` is display data
/// here, so a backward clock step or a future-dated row cannot make an older
/// bind outrank a later unbind. Both authority reads ([`latest_binding`] and
/// [`newest_binding_readonly`]) spell it through this macro, so "which row is
/// newest" has one definition.
///
/// Across databases an `id` means nothing, so rows of different ledgers are
/// compared by [`binding_precedence_key`]; see [`NewestBinding`].
macro_rules! binding_order {
    () => {
        "ORDER BY id DESC"
    };
}

/// [`binding_order!`] as a value, for a test to hold every authority read to it.
#[cfg(test)]
pub(crate) const BINDING_ORDER: &str = binding_order!();

/// How far ahead of the clock a bind or unbind's `ts_utc` may sit before the
/// row is flagged (and, across ledgers, clamped): a day covers any real skew.
pub(crate) const FAR_FUTURE_TOLERANCE_MS: u64 = 24 * 60 * 60 * 1000;

/// Whether `ts_utc` is dated beyond [`FAR_FUTURE_TOLERANCE_MS`] ahead of the
/// clock. A timestamp that does not parse is not "in the future", it is
/// malformed, and is judged by the import's own validity check.
pub(crate) fn is_far_future(ts_utc: &str) -> bool {
    ids::parse_event_time(ts_utc)
        .is_some_and(|ms| ms > ids::now_millis().saturating_add(FAR_FUTURE_TOLERANCE_MS))
}

/// The cross-database precedence key of a binding row: `(ts_utc, ulid)`, with a
/// far-future `ts_utc` clamped to the tolerance horizon so it can never win by
/// timestamp alone.
pub(crate) fn binding_precedence_key(ts_utc: &str, ulid: &str) -> (String, String) {
    let horizon = ids::iso8601_utc(ids::now_millis().saturating_add(FAR_FUTURE_TOLERANCE_MS));
    let ts = if is_far_future(ts_utc) {
        horizon
    } else {
        ts_utc.to_string()
    };
    (ts, ulid.to_string())
}

/// How rows of different ledgers are ranked: any row dated believably outranks
/// every far-future one, and rows of the same kind compare by
/// [`binding_precedence_key`]. A far-future date is a runaway clock or a forgery;
/// it must not beat an honest newer row from another sibling.
pub(crate) type BindingRank = (bool, (String, String));

pub(crate) fn binding_rank(ts_utc: &str, ulid: &str) -> BindingRank {
    (!is_far_future(ts_utc), binding_precedence_key(ts_utc, ulid))
}

/// The newest bind/unbind row of a worktree (`?1`, `?2` its two spellings).
pub(crate) const LATEST_BINDING_SQL: &str = concat!(
    "SELECT action, task_id, ts_utc FROM events
     WHERE action IN ('worktree.bind', 'worktree.unbind')
       AND worktree_path IN (?1, ?2)
     ",
    binding_order!(),
    "
     LIMIT 1"
);

/// [`LATEST_BINDING_SQL`] as of a moment (`?3`, a fixed-width `ts_utc`): the
/// row inserted last among those stamped at or before it. A row exactly at `?3`
/// counts. Ordered by [`binding_order!`] like the live read, so ingest cannot
/// attribute a call to a task the gate would not have enforced.
pub(crate) const BINDING_AS_OF_SQL: &str = concat!(
    "SELECT action, task_id FROM events
     WHERE action IN ('worktree.bind', 'worktree.unbind')
       AND worktree_path IN (?1, ?2)
       AND ts_utc <= ?3
     ",
    binding_order!(),
    "
     LIMIT 1"
);

/// Every bind/unbind row of a worktree, newest insertion first.
pub(crate) const BINDING_HISTORY_SQL: &str = concat!(
    "SELECT ts_utc, action, task_id FROM events
     WHERE action IN ('worktree.bind', 'worktree.unbind')
       AND worktree_path IN (?1, ?2)
     ",
    binding_order!()
);

/// The newest bind/unbind row of a worktree in one ledger, with the key
/// [`NewestBinding`] ranks it by across ledgers.
pub(crate) const NEWEST_BINDING_SQL: &str = concat!(
    "SELECT ts_utc, ulid, action, task_id FROM events
     WHERE action IN ('worktree.bind', 'worktree.unbind')
       AND worktree_path IN (?1, ?2)
     ",
    binding_order!(),
    "
     LIMIT 1"
);

/// One bind or unbind row: when it was appended, and the task it names
/// (`None` for an unbind, or a bind naming no task).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BindingRow {
    pub ts_utc: String,
    pub task_id: Option<String>,
}

/// The `WHERE` of any row attributed to a repository other than the `n`
/// spellings `?1 < ?2 < … < ?n` (bound sorted and distinct): the gaps between
/// them and the two ends. See [`latest_binding`] for why this is range probes
/// rather than `NOT IN`.
fn foreign_row_sql(n: usize) -> String {
    let mut clauses = vec!["repo_path < ?1".to_string()];
    clauses.extend((1..n).map(|i| format!("(repo_path > ?{i} AND repo_path < ?{})", i + 1)));
    clauses.push(format!("repo_path > ?{n}"));
    clauses.join(" OR ")
}

/// Any row attributed to a repository other than the spellings in `allowed`
/// (sorted, distinct), one indexed probe.
fn has_foreign_row(conn: &Connection, allowed: &[String]) -> Result<bool, LedgerError> {
    conn.query_row(
        &format!(
            "SELECT 1 FROM events WHERE {} LIMIT 1",
            foreign_row_sql(allowed.len())
        ),
        rusqlite::params_from_iter(allowed),
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .map(|found| found.is_some())
    .map_err(|e| LedgerError::new("query_failed", e.to_string()))
}

/// The two spellings a repository's rows are stored under (its canonical path,
/// and that path as redacted), sorted and distinct.
fn home_spellings(repo_path: &str) -> Vec<String> {
    let canonical = canonical_repo(repo_path);
    let mut spellings = vec![redact::text(&canonical), canonical];
    spellings.sort();
    spellings.dedup();
    spellings
}

/// The newest bind or unbind recorded for a worktree under either spelling of
/// its path: `Some(Some(task))` for a binding, `Some(None)` for an unbind (or
/// a bind naming no task), `None` when neither was ever recorded.
///
/// One indexed query (`idx_events_binding`) in place of paging the whole
/// ledger through [`tail`], which held the app-wide ledger lock once per
/// 1000 rows and ran on every gated mutation. Precedence is insertion order
/// ([`binding_order!`]), never the clock; a sibling's older legacy row takes
/// its place in that order when it is imported (`import_legacy_page`). Like
/// [`tail`], any row attributed to a different
/// repository refuses the read. Unlike paging, a row elsewhere in the ledger
/// that could not be decoded no longer fails it, because no other row is
/// decoded.
fn latest_binding(
    repo_path: &str,
    worktree: &str,
    stored_worktree: &str,
) -> Result<Option<Option<String>>, LedgerError> {
    #[cfg(test)]
    tests::LEDGER_READS.with(|reads| reads.set(reads.get() + 1));
    with_conn(repo_path, |conn| {
        query_latest_binding(conn, repo_path, worktree, stored_worktree)
    })
}

/// [`latest_binding`] over a read-only connection that is opened, queried and
/// dropped, never cached.
///
/// For the agent hook, a fresh process on every Bash call and every edit: the
/// creating path cost it a directory and file chmod, WAL and synchronous
/// pragmas and the whole schema DDL on each call, against a ledger the app is
/// writing. `Ok(None)` when there is no ledger, or one that never received its
/// schema — neither can hold a binding.
pub(crate) fn latest_binding_readonly(
    repo_path: &str,
    worktree: &str,
    stored_worktree: &str,
) -> Result<Option<Option<String>>, LedgerError> {
    #[cfg(test)]
    tests::LEDGER_READS.with(|reads| reads.set(reads.get() + 1));
    let Some(conn) = open_readonly(&ledger_path(repo_path))? else {
        return Ok(None);
    };
    if !has_table(&conn, "events")? {
        return Ok(None);
    }
    query_latest_binding(&conn, repo_path, worktree, stored_worktree)
}

/// Refuses a ledger holding a row attributed to a repository other than
/// `repo_path`: no binding read may be answered from it.
fn refuse_foreign_rows(conn: &Connection, repo_path: &str) -> Result<(), LedgerError> {
    // `NOT IN` cannot seek, and in the normal case — no foreign row — `LIMIT 1`
    // never stops it early, so it scanned all of `idx_events_repo_ts` on every
    // gated mutation and every hook process. Range probes around the
    // spellings ask the same question with index seeks. `repo_path` is
    // `TEXT NOT NULL` under BINARY collation, so `<`/`>` partition it exactly.
    let mut allowed = home_spellings(repo_path);
    if !has_foreign_row(conn, &allowed)? {
        return Ok(());
    }
    // Only a ledger whose repository moved reaches here: a row names a path
    // that a re-anchor row recorded as an earlier location of this one.
    allowed.extend(anchor::aliases(conn, &allowed)?);
    allowed.sort();
    allowed.dedup();
    if has_foreign_row(conn, &allowed)? {
        return Err(LedgerError::new(
            "repository_mismatch",
            "the ledger contains an event attributed to a different repository",
        ));
    }
    Ok(())
}

/// The task a bind/unbind row leaves in force: `Some` only for a bind that
/// names one.
fn bound_task(action: &str, task_id: Option<String>) -> Option<String> {
    task_id.filter(|_| action == bindings::BIND)
}

/// The one binding query behind [`latest_binding`] and
/// [`latest_binding_readonly`].
fn query_latest_binding(
    conn: &Connection,
    repo_path: &str,
    worktree: &str,
    stored_worktree: &str,
) -> Result<Option<Option<String>>, LedgerError> {
    refuse_foreign_rows(conn, repo_path)?;
    conn.query_row(
        LATEST_BINDING_SQL,
        params![worktree, stored_worktree],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )
    .optional()
    .map_err(|e| LedgerError::new("query_failed", e.to_string()))
    .map(|found| {
        found.map(|(action, task_id, ts_utc)| {
            flag_far_future_binding(&action, &ts_utc);
            bound_task(&action, task_id)
        })
    })
}

/// Says so when the binding in force carries a `ts_utc` far ahead of the clock.
/// Insertion order means it cannot win by that date, so this is visibility: a
/// runaway clock or a forged row, worth a person's look.
fn flag_far_future_binding(action: &str, ts_utc: &str) {
    if is_far_future(ts_utc) {
        log::warn!(
            target: "ledger",
            "binding row {action} is dated in the future ({ts_utc}); it is ordered by insertion, not by that date"
        );
    }
}

/// The binding of a worktree as of `at_ts` (a fixed-width `ts_utc`): the
/// newest bind or unbind appended at or before it, in the shape of
/// [`latest_binding`], including its refusal of a ledger holding another
/// repository's rows.
pub(crate) fn binding_as_of(
    repo_path: &str,
    worktree: &str,
    stored_worktree: &str,
    at_ts: &str,
) -> Result<Option<Option<String>>, LedgerError> {
    with_conn(repo_path, |conn| {
        refuse_foreign_rows(conn, repo_path)?;
        conn.query_row(
            BINDING_AS_OF_SQL,
            params![worktree, stored_worktree, at_ts],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(|e| LedgerError::new("query_failed", e.to_string()))
        .map(|found| found.map(|(action, task_id)| bound_task(&action, task_id)))
    })
}

/// Every bind and unbind of a worktree, oldest first, so a pass can answer
/// "which task, at this moment" for thousands of calls from one read.
pub(crate) fn binding_history(
    repo_path: &str,
    worktree: &str,
    stored_worktree: &str,
) -> Result<Vec<BindingRow>, LedgerError> {
    with_conn(repo_path, |conn| {
        refuse_foreign_rows(conn, repo_path)?;
        let mut stmt = conn
            .prepare(BINDING_HISTORY_SQL)
            .map_err(|e| LedgerError::new("prepare_failed", e.to_string()))?;
        let rows = stmt
            .query_map(params![worktree, stored_worktree], |row| {
                let action: String = row.get(1)?;
                Ok(BindingRow {
                    ts_utc: row.get(0)?,
                    task_id: bound_task(&action, row.get(2)?),
                })
            })
            .map_err(|e| LedgerError::new("query_failed", e.to_string()))?;
        let mut history = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| LedgerError::new("row_failed", e.to_string()))?;
        history.reverse();
        Ok(history)
    })
}

/// A binding's [`binding_precedence_key`] and the task it names, if any. The
/// key is how rows of *different* ledgers are ranked: an `id` is local to its
/// database, so it cannot order them.
pub(crate) type NewestBinding = (BindingRank, Option<String>);

/// The newest bind or unbind for a worktree in one *legacy sibling* ledger
/// (newest by [`binding_order!`], the same order [`latest_binding`] uses), with
/// its [`binding_precedence_key`], read through a read-only connection. For
/// resolving a binding while the family's own ledger does not exist yet, so the
/// answer can still be enforced without creating or migrating that ledger from
/// a read.
pub(crate) fn newest_binding_readonly(
    sibling_repo: &str,
    worktree: &str,
    stored_worktree: &str,
) -> Result<Option<NewestBinding>, LedgerError> {
    with_access(sibling_repo, Access::ReadOnly, |conn| {
        conn.query_row(
            NEWEST_BINDING_SQL,
            params![worktree, stored_worktree],
            |row| {
                let action: String = row.get(2)?;
                let task: Option<String> = row.get(3)?;
                let (ts_utc, ulid) = (row.get::<_, String>(0)?, row.get::<_, String>(1)?);
                flag_far_future_binding(&action, &ts_utc);
                Ok((
                    binding_rank(&ts_utc, &ulid),
                    task.filter(|_| action == bindings::BIND),
                ))
            },
        )
        .optional()
        .map_err(|e| LedgerError::new("query_failed", e.to_string()))
    })
}

/// The one reader of event rows, with an optional byte bound on the page.
///
/// With `max_bytes` each row is [`LedgerEvent::bounded`] and the page ends at
/// the last row that fits the budget (always at least one, so a page makes
/// progress); the flag says rows after it were left for the next page. Without
/// it the rows come back whole and the flag is false. [`tail`] and
/// [`tail_newest`] pass `None`: `ingest` reads a short page as the end of
/// history, which a silent cap would break.
fn read_after(
    repo_path: &str,
    cursor: i64,
    limit: u32,
    newest: bool,
    access: Access,
    max_bytes: Option<usize>,
) -> Result<(Vec<LedgerEvent>, bool), LedgerError> {
    let query = EventQuery {
        after: cursor,
        limit,
        descending: newest,
        ..EventQuery::default()
    };
    let page = read_events(repo_path, &query, access, max_bytes)?;
    let mut events = page.events;
    if newest {
        events.reverse();
    }
    Ok((events, page.byte_capped))
}

/// Which events [`read_events`] selects, and in which order.
///
/// Rows are ordered by `id` (the cursor) and each filter is an equality or a
/// lower bound on a column an index covers, so a filtered newest-first read
/// seeks an index and stops at its limit rather than scanning history.
#[derive(Debug, Clone)]
pub struct EventQuery {
    /// Rows with an id greater than this.
    pub after: i64,
    /// Rows with an id less than this; `None` is no upper bound.
    pub before: Option<i64>,
    /// Page size, clamped to 1..=1000.
    pub limit: u32,
    /// Newest id first when true, oldest first otherwise.
    pub descending: bool,
    /// `ts_utc >= since_ts`: the fixed-width UTC spelling the ledger stores.
    pub since_ts: Option<String>,
    pub action: Option<String>,
    pub object: Option<String>,
    pub worktree_path: Option<String>,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub after_ref: Option<String>,
}

impl Default for EventQuery {
    fn default() -> Self {
        Self {
            after: 0,
            before: None,
            limit: 50,
            descending: false,
            since_ts: None,
            action: None,
            object: None,
            worktree_path: None,
            task_id: None,
            session_id: None,
            after_ref: None,
        }
    }
}

/// How a read whose only filter is `since_ts` is planned.
///
/// Rows are ordered by `id`, which the index on `ts_utc` is not, so left to
/// the planner a newest-first read with an old `since_ts` reads and sorts every
/// row since it, and one with a recent `since_ts` that matches little walks the
/// whole table. `idx_events_ts` ends in the rowid (it is the composite
/// `(ts_utc, id)`), so a probe for `limit + 1` rows at or after `since_ts` is
/// answered from the index alone, and it chooses:
/// - few matches (at most `limit`), or oldest-first: read them through the
///   index (a sort of at most the matches);
/// - many matches, newest first: walk the primary key from the newest id and
///   test `ts_utc` row by row, which stops after `limit` rows because the
///   newest rows are the ones that match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SincePlan {
    /// Not a `since_ts`-only read, or `idx_events_ts` is absent (a ledger no
    /// writer has opened since it was added); the planner decides.
    Planner,
    Index,
    KeyWalk,
}

impl EventQuery {
    /// Picks the [`SincePlan`] for this read of `conn`.
    fn since_plan(&self, conn: &Connection, limit: u32) -> Result<SincePlan, LedgerError> {
        let only_since = self.since_ts.is_some()
            && [
                &self.action,
                &self.object,
                &self.worktree_path,
                &self.task_id,
                &self.session_id,
                &self.after_ref,
            ]
            .iter()
            .all(|filter| filter.is_none());
        let Some(since) = self.since_ts.as_deref().filter(|_| only_since) else {
            return Ok(SincePlan::Planner);
        };
        let failed = |e: rusqlite::Error| LedgerError::new("query_failed", e.to_string());
        let indexed = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = 'idx_events_ts'",
                [],
                |_| Ok(()),
            )
            .optional()
            .map_err(failed)?
            .is_some();
        if !indexed {
            return Ok(SincePlan::Planner);
        }
        let matches: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM (SELECT 1 FROM events INDEXED BY idx_events_ts
                                       WHERE ts_utc >= ?1 LIMIT ?2)",
                params![since, i64::from(limit) + 1],
                |row| row.get(0),
            )
            .map_err(failed)?;
        Ok(if self.descending && matches > i64::from(limit) {
            SincePlan::KeyWalk
        } else {
            SincePlan::Index
        })
    }

    /// The SELECT and its parameters. Only the filters that are set appear, so
    /// the planner is never handed an `? IS NULL OR col = ?` it cannot index.
    ///
    /// `since` says how a `since_ts`-only read is planned (see [`SincePlan`]).
    fn sql(&self, limit: u32, since: SincePlan) -> (String, Vec<rusqlite::types::Value>) {
        use rusqlite::types::Value;
        let source = if since == SincePlan::Index {
            "events INDEXED BY idx_events_ts"
        } else {
            "events"
        };
        // `id > 0` is the predicate of the partial index on action (see
        // SCHEMA); ids start at 1, so it removes nothing.
        let mut clauses = vec!["id > 0".to_string(), "id > ?".to_string()];
        let mut values = vec![Value::Integer(self.after)];
        if let Some(before) = self.before {
            clauses.push("id < ?".to_string());
            values.push(Value::Integer(before));
        }
        // A unary plus keeps the key walk from being replaced by the index.
        let since_clause = if since == SincePlan::KeyWalk {
            "+ts_utc >= ?"
        } else {
            "ts_utc >= ?"
        };
        let filters = [
            (since_clause, &self.since_ts),
            ("action = ?", &self.action),
            ("object = ?", &self.object),
            ("worktree_path = ?", &self.worktree_path),
            ("task_id = ?", &self.task_id),
            ("session_id = ?", &self.session_id),
            ("after_ref = ?", &self.after_ref),
        ];
        for (clause, value) in filters {
            if let Some(value) = value {
                clauses.push(clause.to_string());
                values.push(Value::Text(value.clone()));
            }
        }
        values.push(Value::Integer(i64::from(limit)));
        let order = if self.descending { "DESC" } else { "ASC" };
        let sql = format!(
            "SELECT id, ulid, ts_utc, schema_version, repo_path, worktree_path,
                    actor_kind, actor_id, session_id, task_id, action, object,
                    argv_json, outcome, verdict_json, before_ref, after_ref,
                    duration_ms, detail_json
             FROM {source} WHERE {} ORDER BY id {order} LIMIT ?",
            clauses.join(" AND ")
        );
        (sql, values)
    }
}

/// One page of [`read_events`], in the query's own order.
pub struct EventPage {
    pub events: Vec<LedgerEvent>,
    /// Rows after the page's last one were left for the byte budget.
    pub byte_capped: bool,
    /// More rows matched past the limit.
    pub more: bool,
    /// What the page cut from each row's `verdict_json`, by event id; a row
    /// with no entry kept its verdict whole.
    pub verdict_cuts: std::collections::BTreeMap<i64, ValueCut>,
}

/// [`tail_readonly_page`] for a filtered, ordered read; `Ok(None)` is no ledger.
pub fn query_readonly_page(
    repo_path: &str,
    query: &EventQuery,
    max_bytes: usize,
) -> Result<Option<EventPage>, LedgerError> {
    if !is_initialised(repo_path) {
        return Ok(None);
    }
    read_events(repo_path, query, Access::ReadOnly, Some(max_bytes)).map(Some)
}

/// The `EXPLAIN QUERY PLAN` steps of [`EventQuery`]'s statement, for the test
/// that pins every filter to an index.
#[cfg(test)]
fn explain_query(conn: &Connection, query: &EventQuery) -> Vec<String> {
    let since = query.since_plan(conn, query.limit).expect("since plan");
    let (sql, values) = query.sql(query.limit, since);
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("prepare plan");
    stmt.query_map(rusqlite::params_from_iter(values), |row| {
        row.get::<_, String>(3)
    })
    .expect("plan")
    .collect::<Result<_, _>>()
    .expect("plan rows")
}

/// The one reader of event rows. Rows come back in the query's order, one past
/// the limit being read to say whether more follow.
fn read_events(
    repo_path: &str,
    query: &EventQuery,
    access: Access,
    max_bytes: Option<usize>,
) -> Result<EventPage, LedgerError> {
    #[cfg(test)]
    tests::LEDGER_READS.with(|reads| reads.set(reads.get() + 1));
    // An unbounded limit would let one call page an entire history into the
    // webview. 1000 is well past a screenful and far short of a memory problem.
    let limit = query.limit.clamp(1, 1000);
    let canonical_repo_path = canonical_repo(repo_path);
    let stored_repo_path = redact::text(&canonical_repo_path);
    with_access(repo_path, access, |conn| {
        let since = query.since_plan(conn, limit)?;
        let (sql, values) = query.sql(limit + 1, since);
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| LedgerError::new("prepare_failed", e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values), event_from_row)
            .map_err(|e| LedgerError::new("query_failed", e.to_string()))?;
        let mut events = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| LedgerError::new("row_failed", e.to_string()))?;
        let more = events.len() > limit as usize;
        events.truncate(limit as usize);
        // The earlier locations of a repository that moved, read only when a
        // row names a path that is not the repository's own.
        let mut prior: Option<Vec<String>> = None;
        for event in &mut events {
            if event.repo_path != canonical_repo_path && event.repo_path != stored_repo_path {
                if prior.is_none() {
                    prior = Some(anchor::aliases(conn, &home_spellings(repo_path))?);
                }
                if !prior.iter().flatten().any(|path| *path == event.repo_path) {
                    return Err(LedgerError::new(
                        "repository_mismatch",
                        "the ledger contains an event attributed to a different repository",
                    ));
                }
            }
            // The caller already supplied this repository identity. Returning
            // that identity keeps a redacted on-disk spelling from becoming a
            // second, unmatched repository in the frontend projection without
            // ever persisting the unredacted spelling in this row.
            event.repo_path.clone_from(&canonical_repo_path);
        }
        let Some(budget) = max_bytes else {
            return Ok(EventPage {
                events,
                byte_capped: false,
                more,
                verdict_cuts: Default::default(),
            });
        };
        let mut used = 0;
        let mut page = Vec::with_capacity(events.len());
        let mut verdict_cuts = std::collections::BTreeMap::new();
        let mut capped = false;
        for event in events {
            let (event, verdict_cut) = event.bounded_recorded();
            let size = serde_json::to_string(&event).map_or(0, |text| text.len() + 1);
            if !page.is_empty() && used + size > budget {
                capped = true;
                break;
            }
            used += size;
            if let Some(cut) = verdict_cut {
                verdict_cuts.insert(event.id, cut);
            }
            page.push(event);
        }
        Ok(EventPage {
            events: page,
            byte_capped: capped,
            more,
            verdict_cuts,
        })
    })
}

fn valid_ulid(value: &str) -> bool {
    value.len() == 26
        && value
            .bytes()
            .all(|byte| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&byte))
}

pub(crate) fn valid_utc_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 24
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'.'
        && bytes[23] == b'Z'
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10 | 13 | 16 | 19 | 23) || byte.is_ascii_digit()
        })
}

/// The `ledger_family_imports` key of the anchor's initial-pass marker. Not a
/// git object id, so it can never collide with a source key.
const FAMILY_EPOCH_KEY: &str = "family-initial-pass";

/// Where the anchor's initial consolidation pass stands.
///
/// The first pass over a family takes the sibling ledgers that exist then, the
/// per-worktree ledgers older builds wrote, as the history it is migrating: their
/// bind/unbind rows keep setting bindings. Everything that arrives later was
/// written by something that could have written the anchor instead, and is
/// history only. The marker is data in a table the ledger already has, never a
/// schema change: one `family-initial-pass` row in `ledger_family_imports`,
/// cursor 0 while the pass is unfinished and 1 once it has ended. No row means no
/// pass has finished, whatever source rows an older build left: those siblings
/// are still legacy until a pass records the epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FamilyEpoch {
    /// No pass has completed: siblings found now are the legacy ledgers.
    Initial,
    /// The initial pass is over: a sibling's binding rows are history only.
    Settled,
}

fn marker_failed(error: rusqlite::Error) -> LedgerError {
    LedgerError::new("legacy_marker_failed", error.to_string())
}

/// The epoch, and whether its row is recorded.
fn read_family_epoch(conn: &Connection) -> Result<(FamilyEpoch, bool), LedgerError> {
    let epoch: Option<i64> = conn
        .query_row(
            "SELECT source_cursor FROM ledger_family_imports WHERE source_key = ?1",
            params![FAMILY_EPOCH_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(marker_failed)?;
    Ok(match epoch {
        Some(0) => (FamilyEpoch::Initial, true),
        Some(_) => (FamilyEpoch::Settled, true),
        None => (FamilyEpoch::Initial, false),
    })
}

/// Where the family's initial pass stands, and whether its row is recorded.
///
/// A read: it opens the ledger read-only and never creates it, because every
/// resolve passes here, including the read-only MCP tools and the agent hook. A
/// ledger that does not exist yet, or one that has no import table, has had no
/// pass.
fn family_pass_state(anchor: &str) -> Result<(FamilyEpoch, bool), LedgerError> {
    let none = (FamilyEpoch::Initial, false);
    let read = |conn: &Connection| {
        if has_table(conn, "ledger_family_imports")? {
            read_family_epoch(conn)
        } else {
            Ok(none)
        }
    };
    if let Some(state) = with_live_conn(anchor, read) {
        return state;
    }
    let _one_at_a_time = FALLBACK_OPEN.lock().unwrap_or_else(|e| e.into_inner());
    match open_readonly(&ledger_path(anchor))? {
        Some(conn) => read(&conn),
        None => Ok(none),
    }
}

/// Whether recording a marker in the anchor could succeed: a ledger the caller
/// may not write (a read-only mount, a locked-down state directory) is left as
/// it is, and its pass is recorded by the first caller that can write it.
fn anchor_is_writable(anchor: &str) -> bool {
    let db = ledger_path(anchor);
    let directory_open = db
        .parent()
        .and_then(|dir| std::fs::metadata(dir).ok())
        .is_some_and(|dir| !dir.permissions().readonly());
    directory_open && std::fs::OpenOptions::new().write(true).open(&db).is_ok()
}

/// Records the initial pass as in progress, so a pass cut short resumes as the
/// initial one instead of settling with sources unseen. Returns the epoch as
/// the write transaction saw it.
fn record_family_pass(anchor: &str) -> Result<FamilyEpoch, LedgerError> {
    with_conn(anchor, |conn| {
        let tx = write_tx(conn).map_err(marker_failed)?;
        let (epoch, recorded) = read_family_epoch(&tx)?;
        if epoch == FamilyEpoch::Initial && !recorded {
            tx.execute(
                "INSERT OR IGNORE INTO ledger_family_imports
                     (source_key, source_label, source_cursor)
                 VALUES (?1, 'initial consolidation pass', 0)",
                params![FAMILY_EPOCH_KEY],
            )
            .map_err(marker_failed)?;
        }
        tx.commit().map_err(marker_failed)?;
        Ok(epoch)
    })
}

/// Ends the initial pass: every sibling it enumerated has been imported.
fn settle_family_pass(anchor: &str) -> Result<(), LedgerError> {
    with_conn(anchor, |conn| {
        conn.execute(
            "UPDATE ledger_family_imports SET source_cursor = 1 WHERE source_key = ?1",
            params![FAMILY_EPOCH_KEY],
        )
        .map(|_| ())
        .map_err(marker_failed)
    })
}

/// Records a family as settled when its anchor was created by a binding writer
/// and no pass ever ran: a fresh install has no legacy ledger, so the window in
/// which a sibling could be mistaken for one closes with the ledger's creation.
/// A ledger that cannot be written is left for the first caller that can.
pub(crate) fn settle_fresh_family(anchor: &str) -> Result<(), LedgerError> {
    let (epoch, recorded) = family_pass_state(anchor)?;
    if epoch == FamilyEpoch::Settled || recorded || !anchor_is_writable(anchor) {
        return Ok(());
    }
    with_conn(anchor, |conn| {
        let tx = write_tx(conn).map_err(marker_failed)?;
        tx.execute(
            "INSERT OR IGNORE INTO ledger_family_imports (source_key, source_label, source_cursor)
             VALUES (?1, 'initial consolidation pass', 1)",
            params![FAMILY_EPOCH_KEY],
        )
        .map_err(marker_failed)?;
        tx.commit().map_err(marker_failed)
    })
}

/// How many bind/unbind rows the anchor holds as history only (sibling rows
/// imported after the initial pass). `None` when the ledger cannot be read: the
/// caller must not read that as "none".
pub(crate) fn history_only_binding_count(anchor: &str) -> Option<u64> {
    let count = |conn: &Connection| -> Result<Option<u64>, LedgerError> {
        if !has_table(conn, "events")? {
            return Ok(Some(0));
        }
        conn.query_row(
            "SELECT COUNT(*) FROM events WHERE id > 0 AND action IN (?1, ?2)",
            params![bindings::BIND_IMPORTED, bindings::UNBIND_IMPORTED],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| Some(count.max(0) as u64))
        .map_err(|error| LedgerError::new("query_failed", error.to_string()))
    };
    let counted = || -> Result<Option<u64>, LedgerError> {
        if let Some(found) = with_live_conn(anchor, count) {
            return found;
        }
        let _one_at_a_time = FALLBACK_OPEN.lock().unwrap_or_else(|e| e.into_inner());
        match open_readonly(&ledger_path(anchor))? {
            Some(conn) => count(&conn),
            None => Ok(Some(0)),
        }
    };
    counted().unwrap_or_else(|error| {
        log::warn!(target: "ledger", "could not count history-only binding rows: {error}");
        None
    })
}

/// The `source_label` prefix of a sibling enumerated before the initial pass
/// completed: `legacy@<max>|<path>`, `<max>` being that sibling's highest row id
/// when it was first seen.
const LEGACY_LABEL: &str = "legacy@";

/// The legacy bound a stored label records, if it records one.
fn legacy_bound(label: &str) -> Option<i64> {
    label
        .strip_prefix(LEGACY_LABEL)?
        .split_once('|')?
        .0
        .parse()
        .ok()
}

/// Whether a sibling ledger's bind/unbind rows may set a binding in the anchor.
///
/// Decided once per sibling, persisted in its import row, and never re-decided
/// by how far an import got: a sibling enumerated before the initial pass
/// completed is legacy, and the rows it held then (ids up to the bound) keep
/// setting bindings however many passes it takes to import them. Rows past the
/// bound, and every row of a sibling first seen after the pass, are history only.
#[derive(Debug, Clone, Copy)]
struct SourceAuthority {
    legacy_bound: Option<i64>,
}

impl SourceAuthority {
    /// `stored` is the sibling's import label, `None` when it has no row yet;
    /// `source_max` its highest row id now.
    fn of(epoch: FamilyEpoch, stored: Option<&str>, source_max: i64) -> Self {
        let legacy_bound = match stored.and_then(legacy_bound) {
            Some(bound) => Some(bound),
            None if epoch == FamilyEpoch::Initial => Some(source_max),
            None => None,
        };
        Self { legacy_bound }
    }

    /// The label an import of this sibling stores.
    fn label(&self, source_name: &str) -> String {
        match self.legacy_bound {
            Some(bound) => format!("{LEGACY_LABEL}{bound}|{source_name}"),
            None => source_name.to_string(),
        }
    }

    /// `event` (its `id` still the sibling's own), with a bind or unbind this
    /// source may not set a binding by turned into its history-only spelling.
    /// Any other row is untouched.
    fn applied_to(&self, mut event: LedgerEvent) -> LedgerEvent {
        if !self.legacy_bound.is_some_and(|bound| event.id <= bound) {
            if let Some(imported) = bindings::imported_action(&event.action) {
                event.action = imported.to_string();
            }
        }
        event
    }
}

fn normalize_legacy_event(
    mut event: LedgerEvent,
    anchor: &Path,
    source: &Path,
    members: &[PathBuf],
) -> Result<LedgerEvent, LedgerError> {
    if !valid_ulid(&event.ulid) {
        return Err(LedgerError::new(
            "legacy_identity_invalid",
            "a legacy ledger row has an invalid ULID",
        ));
    }
    if !valid_utc_timestamp(&event.ts_utc) {
        return Err(LedgerError::new(
            "legacy_timestamp_invalid",
            format!(
                "legacy ledger row {} has an invalid UTC timestamp",
                event.ulid
            ),
        ));
    }
    if event.schema_version != SCHEMA_VERSION {
        return Err(LedgerError::new(
            "legacy_schema_unsupported",
            format!(
                "legacy ledger row {} uses unsupported schema version {}",
                event.ulid, event.schema_version
            ),
        ));
    }
    if !matches!(event.actor_kind.as_str(), "human" | "agent" | "system") {
        return Err(LedgerError::new(
            "legacy_actor_invalid",
            format!("legacy ledger row {} has an invalid actor kind", event.ulid),
        ));
    }
    if !matches!(event.outcome.as_str(), "ok" | "failed" | "blocked") {
        return Err(LedgerError::new(
            "legacy_outcome_invalid",
            format!("legacy ledger row {} has an invalid outcome", event.ulid),
        ));
    }
    if event.action.is_empty() {
        return Err(LedgerError::new(
            "legacy_action_invalid",
            format!("legacy ledger row {} has no action", event.ulid),
        ));
    }

    let binding_event = matches!(event.action.as_str(), bindings::BIND | bindings::UNBIND);
    event.repo_path = redact::text(&anchor.to_string_lossy());
    event.worktree_path = match event.worktree_path.take() {
        Some(path) => {
            let canonical_member = members
                .iter()
                .find(|member| {
                    let member = member.to_string_lossy();
                    path == member || path == redact::text(&member)
                })
                .cloned()
                .or_else(|| {
                    std::fs::canonicalize(&path)
                        .ok()
                        .filter(|candidate| members.iter().any(|member| member == candidate))
                });
            match canonical_member {
                Some(member) => Some(redact::text(&member.to_string_lossy())),
                None if binding_event => None,
                None => Some(redact::text(&source.to_string_lossy())),
            }
        }
        // Old writers did not consistently distinguish the repository from
        // the active checkout. The database's authenticated location supplies
        // that missing attribution for ordinary rows. A malformed binding with
        // no target remains inert rather than acquiring authority over source.
        None if !binding_event => Some(redact::text(&source.to_string_lossy())),
        None => None,
    };
    event.actor_id = event.actor_id.as_deref().map(redact::text);
    event.session_id = event.session_id.as_deref().map(redact::text);
    event.task_id = event.task_id.as_deref().map(redact::text);
    event.action = redact::text(&event.action);
    event.argv_json = event.argv_json.as_deref().map(redact::text);
    event.verdict_json = event.verdict_json.as_deref().map(redact::text);
    event.before_ref = event.before_ref.as_deref().map(redact::text);
    event.after_ref = event.after_ref.as_deref().map(redact::text);
    // Every row is capped through the function every writer uses, which also
    // drops any `object_truncated` a detail carries: the key is the server's.
    // The one exception is a row that is itself a cut a newer writer made,
    // judged on the REDACTED object (redaction can lengthen one past the cap)
    // and on the record being exactly what that writer writes
    // ([`cap::recorded_cut_is_genuine`]). Its record survives the copy; a
    // forged or inconsistent one does not, and on a `reflog.*` row a forged
    // hash would have changed the dedupe key and had the entry recorded again
    // on every ingest.
    let redacted_object = event.object.as_deref().map(redact::text);
    let genuine = redacted_object
        .as_deref()
        .is_some_and(|object| cap::recorded_cut_is_genuine(object, event.detail_json.as_deref()));
    if genuine {
        event.object = redacted_object;
        event.detail_json = event.detail_json.as_deref().map(redact::text);
    } else {
        let (object, detail) =
            store_object_and_detail(event.object.as_deref(), event.detail_json.as_deref(), None);
        event.object = object;
        event.detail_json = detail;
    }
    Ok(event)
}

fn same_event_payload(left: &LedgerEvent, right: &LedgerEvent) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.id = 0;
    right.id = 0;
    left == right
}

fn family_source_key(anchor: &Path, source: &str) -> Result<String, LedgerError> {
    let output = crate::engine::git_cli::git_with_stdin(
        anchor,
        &["hash-object", "--stdin"],
        source.as_bytes(),
    )
    .map_err(|error| {
        LedgerError::new(
            "legacy_source_key_failed",
            format!("could not identify a legacy ledger source: {error}"),
        )
    })?;
    let key = String::from_utf8_lossy(&output).trim().to_string();
    if !matches!(key.len(), 40 | 64) || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(LedgerError::new(
            "legacy_source_key_failed",
            "git hash-object returned an invalid object id",
        ));
    }
    Ok(key)
}

/// A sibling's import row: how far it got, and its stored label. `None` when the
/// sibling has never had a row.
fn family_import_state(
    anchor: &str,
    source_key: &str,
) -> Result<Option<(i64, String)>, LedgerError> {
    with_conn(anchor, |conn| {
        conn.query_row(
            "SELECT source_cursor, source_label FROM ledger_family_imports WHERE source_key = ?1",
            params![source_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| LedgerError::new("legacy_marker_failed", error.to_string()))
    })
}

/// `event` as its history-only spelling when it is a bind or unbind that a row
/// already in the anchor outranks, `None` when it stands as it is.
///
/// A legacy row is placed in insertion order by arriving, so one older than the
/// binding in force (a second sibling's earlier bind, imported after a later
/// one) must not arrive as the newest: it is ranked against that binding by the
/// cross-database [`binding_precedence_key`], the one place ids cannot decide.
fn superseded_binding(
    transaction: &rusqlite::Transaction<'_>,
    event: &LedgerEvent,
) -> Result<Option<LedgerEvent>, LedgerError> {
    let (Some(_), Some(worktree)) = (
        bindings::imported_action(&event.action),
        event.worktree_path.as_deref(),
    ) else {
        return Ok(None);
    };
    let in_force = transaction
        .query_row(NEWEST_BINDING_SQL, params![worktree, worktree], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .optional()
        .map_err(|error| LedgerError::new("legacy_import_failed", error.to_string()))?;
    let outranked = in_force.is_some_and(|(ts_utc, ulid)| {
        binding_rank(&ts_utc, &ulid) > binding_rank(&event.ts_utc, &event.ulid)
    });
    Ok(outranked.then(|| {
        let mut demoted = event.clone();
        if let Some(imported) = bindings::imported_action(&event.action) {
            demoted.action = imported.to_string();
        }
        demoted
    }))
}

fn import_legacy_page(
    anchor: &str,
    source_key: &str,
    source_label: &str,
    page: &[LedgerEvent],
    source_cursor: i64,
) -> Result<(usize, Option<i64>), LedgerError> {
    let source_label = redact::text(source_label);
    with_conn(anchor, |conn| {
        let transaction = write_tx(conn)
            .map_err(|error| LedgerError::new("legacy_import_failed", error.to_string()))?;
        let mut imported = 0usize;
        let mut newest_cursor = None;

        // A copied reflog row keeps its interim event time in the typed column
        // too, and takes the next free ordinal of its identity group in the
        // anchor: rows arrive in source id order, so two identical source rows
        // import as 0 and 1 exactly as the migration numbers them in place, and
        // a re-import is stopped by the ULID before the ordinal matters. The
        // group is [`IdentityParts`] over the bound values, written to match the
        // index's own expressions so its leading columns serve the lookup.
        let imported_time = format!("CASE WHEN ?10 LIKE 'reflog.%' THEN {} END", {
            occurred_at_from_detail("?18")
        });
        let group = IdentityParts::COLUMNS
            .terms()
            .into_iter()
            .zip(
                IdentityParts {
                    worktree: "?5",
                    object: "?11",
                    occurred_at: &format!("({imported_time})"),
                    detail: "?18",
                }
                .terms(),
            )
            .map(|(column, bound)| format!("{column} = {bound}"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let insert_sql = format!(
            "INSERT INTO events (
                 ulid, ts_utc, schema_version, repo_path, worktree_path,
                 actor_kind, actor_id, session_id, task_id, action, object,
                 argv_json, outcome, verdict_json, before_ref, after_ref,
                 duration_ms, detail_json, occurred_at, reflog_ordinal
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,
                 {imported_time},
                 CASE WHEN ?10 LIKE 'reflog.%' AND ?11 IS NOT NULL THEN
                     (SELECT COALESCE(MAX(COALESCE(reflog_ordinal, 0)) + 1, 0)
                      FROM events WHERE {REFLOG_ROWS} AND {group})
                 WHEN ?10 LIKE 'reflog.%' THEN 0 END)
             ON CONFLICT(ulid) DO NOTHING"
        );
        for source_event in page {
            let demoted;
            let event = match superseded_binding(&transaction, source_event)? {
                Some(event) => {
                    demoted = event;
                    &demoted
                }
                None => source_event,
            };
            let changed = transaction
                .execute(
                    &insert_sql,
                    params![
                        event.ulid,
                        event.ts_utc,
                        event.schema_version,
                        event.repo_path,
                        event.worktree_path,
                        event.actor_kind,
                        event.actor_id,
                        event.session_id,
                        event.task_id,
                        event.action,
                        event.object,
                        event.argv_json,
                        event.outcome,
                        event.verdict_json,
                        event.before_ref,
                        event.after_ref,
                        event.duration_ms,
                        event.detail_json,
                    ],
                )
                .map_err(|error| LedgerError::new("legacy_import_failed", error.to_string()))?;

            if changed == 1 {
                imported += 1;
                newest_cursor = Some(transaction.last_insert_rowid());
                continue;
            }

            let existing = transaction
                .query_row(
                    "SELECT id, ulid, ts_utc, schema_version, repo_path, worktree_path,
                            actor_kind, actor_id, session_id, task_id, action, object,
                            argv_json, outcome, verdict_json, before_ref, after_ref,
                            duration_ms, detail_json
                     FROM events WHERE ulid = ?1",
                    params![event.ulid],
                    event_from_row,
                )
                .optional()
                .map_err(|error| LedgerError::new("legacy_import_failed", error.to_string()))?;
            // Whether a binding row was demoted depends on what the anchor held
            // when it arrived, so a retry may spell its action either way.
            let same_row = existing.as_ref().is_some_and(|existing| {
                let mut existing = existing.clone();
                existing.action = bindings::base_action(&existing.action).to_string();
                let mut event = event.clone();
                event.action = bindings::base_action(&event.action).to_string();
                same_event_payload(&existing, &event)
            });
            // A transcript row's ulid is derived from its identity, so two
            // ledgers that each read one line hold it under one ulid, with
            // their own `ts_utc`. The same identity is the same row, already
            // here. A different identity under that ulid is a collision of its
            // 80-bit body: the row is not imported, nothing is overwritten, and
            // the lost row is counted as a failed write. Neither stops the
            // consolidation of everything else.
            let transcript_key =
                |event: &LedgerEvent| event.detail_json.as_deref().and_then(transcript_row_of);
            if !same_row {
                if let Some(incoming) = transcript_key(event) {
                    // The same line AND the same content (action, object: see
                    // `insert_transcript_row`) is the row already here.
                    let same_line = existing.as_ref().is_some_and(|held| {
                        transcript_key(held).as_ref() == Some(&incoming)
                            && bindings::base_action(&held.action)
                                == bindings::base_action(&event.action)
                            && held.object == event.object
                    });
                    if !same_line {
                        log::warn!(
                            "legacy transcript row {} collides with a different row",
                            event.ulid
                        );
                        record_dropped_append(anchor);
                    }
                    continue;
                }
                return Err(LedgerError::new(
                    "legacy_ulid_conflict",
                    format!(
                        "legacy ledger ULID {} names different events in two databases",
                        event.ulid
                    ),
                ));
            }
        }

        transaction
            .execute(
                "INSERT INTO ledger_family_imports (source_key, source_label, source_cursor)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(source_key) DO UPDATE SET
                    source_label = excluded.source_label,
                    source_cursor = excluded.source_cursor",
                params![source_key, source_label, source_cursor],
            )
            .map_err(|error| LedgerError::new("legacy_marker_failed", error.to_string()))?;
        transaction
            .commit()
            .map_err(|error| LedgerError::new("legacy_import_failed", error.to_string()))?;
        Ok((imported, newest_cursor))
    })
}

/// Consolidates ledgers written by pre-family versions of GitPulse.
///
/// Only active worktrees authenticated by `git worktree list` are considered.
/// Rows retain their ULID and timestamp but receive the family anchor as their
/// repository and the source checkout when old data omitted that distinction.
/// A per-source cursor keyed by Git's opaque object digest records how far each
/// import got without persisting a credential-shaped path or collapsing two
/// redaction-equivalent names. The unique ULID plus a payload comparison makes
/// retries idempotent without hiding collisions.
///
/// A source is imported when something changed, not on every read: one whose
/// database and write-ahead log are as they were at its last complete import
/// is skipped without a Git process or a database open (see [`consolidated`]).
/// A sibling's first ledger, its next row, and a worktree added to the family
/// all change what is on disk, so none of them is skipped.
pub(crate) fn consolidate_worktree_ledgers(
    anchor: &str,
    members: &[PathBuf],
) -> Result<usize, LedgerError> {
    let anchor_path = std::fs::canonicalize(anchor).map_err(|error| {
        LedgerError::new(
            "legacy_anchor_invalid",
            format!("could not resolve family ledger anchor: {error}"),
        )
    })?;
    let anchor = anchor_path.to_string_lossy().into_owned();
    let mut imported_total = 0usize;
    let (mut epoch, mut recorded) = family_pass_state(&anchor)?;
    if epoch == FamilyEpoch::Initial
        && !recorded
        && is_initialised(&anchor)
        && anchor_is_writable(&anchor)
    {
        epoch = record_family_pass(&anchor)?;
        recorded = epoch == FamilyEpoch::Initial;
    }

    for source in members {
        if source == &anchor_path {
            continue;
        }
        let source_db = ledger_path(&source.to_string_lossy());
        let Some(database) = checked_state_file_metadata(&source_db, Some(source))? else {
            continue;
        };
        // Taken before the import reads anything: a write that lands during it
        // changes the files, so the next read sees a different stamp.
        let wal = PathBuf::from(format!("{}-wal", source_db.display()));
        let stamp = consolidated::SourceStamp::of(
            &database,
            checked_state_file_metadata(&wal, Some(source))?.as_ref(),
        );
        if consolidated::is_current(&anchor_path, file_id(&ledger_path(&anchor)), source, &stamp) {
            continue;
        }
        // The import creates the anchor if it is missing: the pass starts here.
        if epoch == FamilyEpoch::Initial && !recorded {
            epoch = record_family_pass(&anchor)?;
            recorded = epoch == FamilyEpoch::Initial;
        }
        imported_total += import_source(&anchor_path, &anchor, source, &source_db, members, epoch)?;
        consolidated::record(&anchor_path, file_id(&ledger_path(&anchor)), source, stamp);
    }
    if epoch == FamilyEpoch::Initial && recorded {
        settle_family_pass(&anchor)?;
    }

    Ok(imported_total)
}

/// Imports everything one sibling ledger holds that the anchor has not seen.
fn import_source(
    anchor_path: &Path,
    anchor: &str,
    source: &Path,
    source_db: &Path,
    members: &[PathBuf],
    epoch: FamilyEpoch,
) -> Result<usize, LedgerError> {
    let mut imported_total = 0usize;
    let mut demoted_total = 0usize;
    {
        let source_name = source.to_string_lossy().into_owned();
        let source_key = family_source_key(anchor_path, &source_name)?;
        let stored = family_import_state(anchor, &source_key)?;
        let mut cursor = stored.as_ref().map_or(0, |(cursor, _)| *cursor);
        #[cfg(test)]
        tests::LEDGER_SIBLING_OPENS.with(|opens| opens.set(opens.get() + 1));
        let source_conn = Connection::open_with_flags(source_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| {
                LedgerError::new(
                    "legacy_read_failed",
                    format!("could not open {}: {error}", source_db.display()),
                )
            })?;
        checked_state_file_metadata(source_db, Some(source))?.ok_or_else(|| {
            LedgerError::new(
                "state_path_changed",
                format!(
                    "legacy ledger disappeared during open: {}",
                    source_db.display()
                ),
            )
        })?;
        let source_transaction = source_conn.unchecked_transaction().map_err(|error| {
            LedgerError::new(
                "legacy_read_failed",
                format!("could not snapshot {}: {error}", source_db.display()),
            )
        })?;
        let source_max: i64 = source_transaction
            .query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |row| {
                row.get(0)
            })
            .map_err(|error| {
                LedgerError::new(
                    "legacy_read_failed",
                    format!("could not read {}: {error}", source_db.display()),
                )
            })?;
        if source_max < cursor {
            // The legacy database was replaced or truncated after a previous
            // import. Re-scan from the start; ULID comparison keeps this safe.
            cursor = 0;
        }
        let authority = SourceAuthority::of(
            epoch,
            stored.as_ref().map(|(_, label)| label.as_str()),
            source_max,
        );
        let source_label = authority.label(&source_name);

        while cursor < source_max {
            let mut statement = source_transaction
                .prepare(
                    "SELECT id, ulid, ts_utc, schema_version, repo_path, worktree_path,
                            actor_kind, actor_id, session_id, task_id, action, object,
                            argv_json, outcome, verdict_json, before_ref, after_ref,
                            duration_ms, detail_json
                     FROM events WHERE id > ?1 AND id <= ?2 ORDER BY id ASC LIMIT 1000",
                )
                .map_err(|error| LedgerError::new("legacy_read_failed", error.to_string()))?;
            let source_page = statement
                .query_map(params![cursor, source_max], event_from_row)
                .map_err(|error| LedgerError::new("legacy_read_failed", error.to_string()))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| LedgerError::new("legacy_read_failed", error.to_string()))?;
            if source_page.is_empty() {
                return Err(LedgerError::new(
                    "legacy_read_gap",
                    format!(
                        "legacy ledger {} ended before its reported cursor {}",
                        source_db.display(),
                        source_max
                    ),
                ));
            }
            cursor = source_page.last().map(|event| event.id).unwrap_or(cursor);
            let normalized = source_page
                .into_iter()
                .map(|event| {
                    normalize_legacy_event(event, anchor_path, source, members)
                        .map(|event| authority.applied_to(event))
                })
                .collect::<Result<Vec<_>, _>>()?;
            demoted_total += normalized
                .iter()
                .filter(|event| bindings::base_action(&event.action) != event.action)
                .count();
            let (imported, newest_cursor) =
                import_legacy_page(anchor, &source_key, &source_label, &normalized, cursor)?;
            imported_total += imported;
            if let Some(newest_cursor) = newest_cursor {
                announce(anchor, newest_cursor);
            }
        }
    }
    if demoted_total > 0 {
        log::warn!(
            target: "ledger",
            "{demoted_total} bind/unbind row(s) from sibling ledger {} were imported as history only: \
             this sibling was not part of the family's initial consolidation pass (or the row is newer than it), so it sets no binding",
            source.display()
        );
    }

    Ok(imported_total)
}

/// Where reflog replay stopped for one ref of one checkout: the identity of
/// the last entry it recorded, as `reflog_watermarks` holds it. The four
/// fields are what tell that entry from every other one of the same ref: the
/// selector's time, the commit, a hash of the stored identity (commit and
/// message), and the entry's ordinal among identical ones of that second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReflogWatermark {
    pub unix: i64,
    pub oid: String,
    pub msg_hash: String,
    pub ordinal: i64,
}

/// The hash a watermark keeps of a reflog entry's identity (`"{oid} {message}"`).
/// Taken over the stored spelling, so a message that carried a secret leaves
/// no digest of the secret in this table.
pub(crate) fn reflog_message_hash(identity: &str) -> String {
    format!("{:016x}", ids::fnv1a64(stored_object(identity).as_bytes()))
}

/// The watermark for `ref_name` of `worktree_path`'s checkout, if replay has
/// recorded any entry of it. A read error is an error, never "no watermark":
/// the caller would then replay from the top of the window.
pub(crate) fn reflog_watermark(
    repo_path: &str,
    worktree_path: &str,
    ref_name: &str,
) -> Result<Option<ReflogWatermark>, LedgerError> {
    let key = transcript_key(Path::new(worktree_path));
    with_conn(repo_path, |conn| {
        conn.query_row(
            "SELECT last_entry_unix, last_entry_oid, last_entry_msg_hash, last_entry_ordinal
             FROM reflog_watermarks WHERE worktree_key = ?1 AND ref_name = ?2",
            params![key, ref_name],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()
        .map(|row| match row {
            // A row with a missing part cannot name an entry; it is no mark.
            Some((Some(unix), Some(oid), Some(msg_hash), Some(ordinal))) => Some(ReflogWatermark {
                unix,
                oid,
                msg_hash,
                ordinal,
            }),
            _ => None,
        })
        .map_err(|e| LedgerError::new("watermark_read_failed", e.to_string()))
    })
}

/// One reflog entry of a [`record_reflog_batch`].
pub(crate) struct ReflogRow {
    /// The row to write; `None` for an entry replay settles without one (the
    /// ledger already holds it as a row an old build wrote).
    pub draft: Option<Draft>,
    pub ordinal: i64,
    /// The watermark that names this entry, written if it is the last settled.
    pub mark: ReflogWatermark,
    /// The entry's time was not believed (absent, or ahead of the clock), so its
    /// `occurred_at` is ingest time: part of the unique identity, and different
    /// on every replay, so the index alone cannot refuse a second recording. The
    /// batch counts the rows already holding the entry and records only an
    /// ordinal past them.
    pub untimed: bool,
}

/// Records `rows` (one ref's entries, oldest first) and moves that ref's
/// watermark to the last settled one, all in ONE transaction: a process killed
/// anywhere in it leaves the rows and the watermark both written or neither.
///
/// Stops at the first row that fails to write and commits what settled before
/// it. Returns one outcome per row attempted (`None`: settled without a row),
/// so the caller's tallies and its "settled prefix" agree with what is in the
/// ledger. `Err` only when the transaction could not be opened or committed, in
/// which case nothing was written.
pub(crate) fn record_reflog_batch(
    repo_path: &str,
    worktree_path: &str,
    ref_name: &str,
    rows: Vec<ReflogRow>,
) -> Result<Vec<Option<Recorded>>, LedgerError> {
    let key = transcript_key(Path::new(worktree_path));
    let prepared: Vec<Option<Result<Prepared, LedgerError>>> = rows
        .iter()
        .map(|row| row.draft.as_ref().map(prepare))
        .collect();
    let attempted = prepared.iter().flatten().count();
    let result = with_conn(repo_path, |conn| {
        let failed = |e: rusqlite::Error| LedgerError::new("insert_failed", e.to_string());
        let tx = write_tx(conn).map_err(failed)?;
        let mut outcomes: Vec<Option<Recorded>> = Vec::with_capacity(rows.len());
        let mut settled: Option<&ReflogWatermark> = None;
        let mut newest = None;
        for (row, prepared) in rows.iter().zip(prepared) {
            let outcome = match prepared {
                None => None,
                Some(Err(e)) => {
                    log::warn!("ledger append failed: {e}");
                    Some(Recorded::Failed)
                }
                Some(Ok(mut event)) => {
                    event.reflog_ordinal = event.reflog_ordinal.map(|_| row.ordinal);
                    let held = row.untimed && untimed_rows_held(&tx, &event) > row.ordinal;
                    Some(if held {
                        Recorded::Duplicate
                    } else {
                        match insert(&tx, &event) {
                            Ok(id) => {
                                newest = Some(id);
                                #[cfg(all(test, unix))]
                                crash_point(
                                    1 + i64::try_from(
                                        outcomes
                                            .iter()
                                            .flatten()
                                            .filter(|o| matches!(o, Recorded::Appended(_)))
                                            .count(),
                                    )
                                    .unwrap_or(0),
                                );
                                Recorded::Appended(id)
                            }
                            Err(e) if e.code == "duplicate_reflog" => Recorded::Duplicate,
                            Err(e) => {
                                log::warn!("ledger append failed: {e}");
                                Recorded::Failed
                            }
                        }
                    })
                }
            };
            let stop = outcome == Some(Recorded::Failed);
            outcomes.push(outcome);
            if stop {
                break;
            }
            settled = Some(&row.mark);
        }
        if let Some(mark) = settled {
            upsert_reflog_watermark(&tx, &key, ref_name, mark)
                .map_err(|e| LedgerError::new("watermark_write_failed", e.to_string()))?;
        }
        tx.commit().map_err(failed)?;
        Ok((outcomes, newest))
    });
    match result {
        Ok((outcomes, newest)) => {
            for outcome in &outcomes {
                if *outcome == Some(Recorded::Failed) {
                    record_dropped_append(repo_path);
                }
            }
            if let Some(cursor) = newest {
                announce(repo_path, cursor);
            }
            Ok(outcomes)
        }
        Err(e) => {
            for _ in 0..attempted {
                record_dropped_append(repo_path);
            }
            Err(e)
        }
    }
}

/// How many `reflog.*` rows already hold `event`'s entry, of an untimed one
/// (see [`ReflogRow::untimed`]): same worktree and object, stamped with ingest
/// time. A read that fails counts as none held, so the entry is attempted and a
/// failure to write it surfaces as one.
fn untimed_rows_held(conn: &Connection, event: &Prepared) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM events
         WHERE action LIKE 'reflog.%' AND object = ?1 AND worktree_path IS ?2
           AND instr(detail_json, '\"occurred_at_basis\":\"ingest\"') > 0",
        params![event.object, event.worktree_path],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

/// Test-only: a child replay process kills itself (SIGKILL, no unwinding, no
/// destructors) once a batch has written `GITPULSE_TEST_KILL_AFTER_RECORDED`
/// rows, inside the transaction that has not committed them.
#[cfg(all(test, unix))]
fn crash_point(written: i64) {
    let after = std::env::var("GITPULSE_TEST_KILL_AFTER_RECORDED")
        .ok()
        .and_then(|n| n.parse::<i64>().ok());
    if after == Some(written) {
        // SAFETY: a signal to this very process; SIGKILL cannot be handled.
        unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
    }
}

/// Moves a watermark inside the transaction that wrote the rows it names.
fn upsert_reflog_watermark(
    conn: &Connection,
    key: &str,
    ref_name: &str,
    mark: &ReflogWatermark,
) -> rusqlite::Result<()> {
    let updated_at = ids::iso8601_utc(ids::now_millis());
    {
        conn.execute(
            "INSERT INTO reflog_watermarks (
                 worktree_key, ref_name, last_entry_unix, last_entry_oid,
                 last_entry_msg_hash, last_entry_ordinal, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(worktree_key, ref_name) DO UPDATE SET
                 last_entry_unix = excluded.last_entry_unix,
                 last_entry_oid = excluded.last_entry_oid,
                 last_entry_msg_hash = excluded.last_entry_msg_hash,
                 last_entry_ordinal = excluded.last_entry_ordinal,
                 updated_at = excluded.updated_at",
            params![
                key,
                ref_name,
                mark.unix,
                mark.oid,
                mark.msg_hash,
                mark.ordinal,
                updated_at
            ],
        )
        .map(|_| ())
    }
}

/// The query behind [`recorded_reflog_objects`], one text so the test that
/// holds its plan to the partial index runs the statement production runs.
const RECORDED_REFLOG_SQL: &str = "SELECT object,
        CASE WHEN json_valid(detail_json)
             THEN json_extract(detail_json, '$.object_truncated.hash') END
 FROM events INDEXED BY idx_events_reflog
 WHERE action LIKE 'reflog.%' AND object IS NOT NULL
   AND occurred_at IS NULL
   AND (worktree_path IN (?1, ?2) OR (?3 AND worktree_path IS NULL))";

/// The `object` of every *legacy* `reflog.*` event recorded for one checkout,
/// in the form it is stored (redacted), for the reflog catch-up's "already
/// recorded" test.
///
/// Legacy means no `occurred_at`: a row written before replay typed its event
/// time. Those cannot be matched by the unique reflog identity index (their
/// time part is empty), so replay still skips an entry whose identity one of
/// them holds. A row with an `occurred_at` is deliberately absent: the index
/// refuses its replayed copy exactly, and counting it here would hide the
/// second of two identical entries after a pass that stopped between them.
///
/// One query on the partial index `idx_events_reflog` in place of paging the whole ledger
/// through [`tail`]. A read error is an error: the caller must not read "could
/// not find out" as "nothing is recorded" and write the history a second time.
/// Callers compare against [`stored_object`] of an identity, never the raw one,
/// because the raw spelling is not what is stored. What the set holds is each
/// row's identity key, not its column: see [`stored_object`].
pub(crate) fn recorded_reflog_objects(
    repo_path: &str,
    worktree_path: &str,
    is_anchor: bool,
) -> Result<std::collections::HashSet<String>, LedgerError> {
    legacy_reflog_objects(repo_path, worktree_path, is_anchor, None)
}

/// The query behind [`recorded_reflog_objects_since`]: the same rows, read
/// through the append-time index from a time on, so its cost follows what was
/// appended since and not the checkout's whole reflog history.
const RECORDED_REFLOG_SINCE_SQL: &str = "SELECT object,
        CASE WHEN json_valid(detail_json)
             THEN json_extract(detail_json, '$.object_truncated.hash') END
 FROM events INDEXED BY idx_events_ts
 WHERE ts_utc >= ?4 AND action LIKE 'reflog.%' AND object IS NOT NULL
   AND occurred_at IS NULL
   AND (worktree_path IN (?1, ?2) OR (?3 AND worktree_path IS NULL))";

/// [`recorded_reflog_objects`] for the legacy rows appended at or after
/// `since_unix` (Unix seconds, append time).
///
/// A generation-1 process that was already running when the ledger migrated
/// keeps writing reflog rows with no `occurred_at`, which the unique index
/// cannot match, so replay must keep reading such rows after its watermark
/// exists. Only recent ones can matter: such a row is appended no earlier than
/// the entry it records happened, which is after the watermark's entry minus
/// any clock skew, so the caller passes that time less a margin.
pub(crate) fn recorded_reflog_objects_since(
    repo_path: &str,
    worktree_path: &str,
    is_anchor: bool,
    since_unix: i64,
) -> Result<std::collections::HashSet<String>, LedgerError> {
    let since = ids::iso8601_utc(u64::try_from(since_unix).unwrap_or(0).saturating_mul(1000));
    legacy_reflog_objects(repo_path, worktree_path, is_anchor, Some(&since))
}

fn legacy_reflog_objects(
    repo_path: &str,
    worktree_path: &str,
    is_anchor: bool,
    since: Option<&str>,
) -> Result<std::collections::HashSet<String>, LedgerError> {
    let raw = worktree_path.to_string();
    let stored = redact::text(worktree_path);
    with_conn(repo_path, |conn| {
        let mut stmt = conn
            .prepare(if since.is_some() {
                RECORDED_REFLOG_SINCE_SQL
            } else {
                RECORDED_REFLOG_SQL
            })
            .map_err(|e| LedgerError::new("prepare_failed", e.to_string()))?;
        let map = |row: &Row<'_>| {
            Ok(cap::object_key_of_row(
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        };
        let rows = match since {
            Some(since) => stmt.query_map(params![raw, stored, is_anchor, since], map),
            None => stmt.query_map(params![raw, stored, is_anchor], map),
        }
        .map_err(|e| LedgerError::new("query_failed", e.to_string()))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| LedgerError::new("row_failed", e.to_string()))
    })
}

/// Events kept by [`prune_events`] whatever their age: the newest ones, so
/// the cursor never moves backwards or repeats an id.
pub const RETAIN_NEWEST_EVENTS: i64 = 10_000;
/// The age past which [`prune_events`] may delete an event.
pub const RETAIN_EVENT_DAYS: u32 = 365;
/// Rows one prune deletes, so it holds the write lock for a bounded time.
const PRUNE_BATCH: i64 = 5_000;

/// Deletes events older than `older_than_days`, never the newest
/// `keep_newest`, and never a worktree bind or unbind (they are what a
/// worktree's task scope is enforced from, however old). One bounded batch per
/// call; returns how many were deleted, so a caller loops until it is 0.
///
/// "Older" is the append clock, `ts_utc`, never `detail_json.occurred_at`.
/// [`recorded_reflog_objects`] rebuilds the reflog's already-seen set from the
/// rows that are stored, so a row pruned while git still holds its entry would
/// be recorded again by the next catch-up, for ever.
///
/// Catch-up ([`prune_expired`]) runs it on the default policy; a host may
/// still call it with its own. Keeping the newest rows means the highest
/// cursor survives, so ids are never reused.
pub fn prune_events(
    repo_path: &str,
    older_than_days: u32,
    keep_newest: i64,
) -> Result<usize, LedgerError> {
    let keep_newest = keep_newest.max(1);
    let age_ms = u64::from(older_than_days).saturating_mul(86_400_000);
    let cutoff = ids::iso8601_utc(ids::now_millis().saturating_sub(age_ms));
    with_conn(repo_path, |conn| {
        let failed = |e: rusqlite::Error| LedgerError::new("prune_failed", e.to_string());
        let tx = write_tx(conn).map_err(failed)?;
        let deleted = tx
            .execute(
                "DELETE FROM events WHERE id IN (
                     SELECT id FROM events
                     WHERE ts_utc < ?1
                       AND action NOT IN ('worktree.bind', 'worktree.unbind')
                       AND id <= (SELECT COALESCE(MAX(id), 0) FROM events) - ?2
                     ORDER BY id LIMIT ?3)",
                params![cutoff, keep_newest, PRUNE_BATCH],
            )
            .map_err(failed)?;
        tx.commit().map_err(failed)?;
        Ok(deleted)
    })
}

/// Batches one maintenance pass may run, so a ledger that fell far behind is
/// brought back over several passes instead of one long write-lock hold.
pub const PRUNE_MAX_BATCHES: usize = 20;

/// What one [`prune_expired`] pass did. `error` is empty when it ran clean;
/// a pass that could not prune must not read the same as one with nothing to
/// prune.
#[derive(Debug, Clone, Default)]
pub struct PruneReport {
    pub deleted: usize,
    pub error: String,
}

/// The default retention policy ([`RETAIN_EVENT_DAYS`], [`RETAIN_NEWEST_EVENTS`])
/// applied in bounded batches: at most [`PRUNE_MAX_BATCHES`] of [`PRUNE_BATCH`]
/// rows, no batch starts once `deadline` has passed, and a ledger a pass
/// left with nothing to prune is not scanned again for [`PRUNE_CLEAN_INTERVAL`].
pub fn prune_expired(repo_path: &str, deadline: Option<std::time::Instant>) -> PruneReport {
    prune_with_policy(repo_path, RETAIN_EVENT_DAYS, RETAIN_NEWEST_EVENTS, deadline)
}

/// How long a ledger a pass left with nothing to prune is not scanned again by
/// this process. Finding the candidates reads every row below the newest
/// [`RETAIN_NEWEST_EVENTS`] under the write lock — close to a second on a
/// year of events — and catch-up runs on every open and every daemon cycle,
/// while the policy is measured in days.
const PRUNE_CLEAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// When each ledger, by database path, was last found with nothing to prune.
fn prune_clean_at() -> &'static Mutex<HashMap<PathBuf, std::time::Instant>> {
    static CLEAN: OnceLock<Mutex<HashMap<PathBuf, std::time::Instant>>> = OnceLock::new();
    CLEAN.get_or_init(|| Mutex::new(HashMap::new()))
}

fn prune_with_policy(
    repo_path: &str,
    older_than_days: u32,
    keep_newest: i64,
    deadline: Option<std::time::Instant>,
) -> PruneReport {
    let mut report = PruneReport::default();
    let db_path = ledger_path(repo_path);
    let clean = prune_clean_at()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&db_path)
        .is_some_and(|at| at.elapsed() < PRUNE_CLEAN_INTERVAL);
    if clean {
        return report;
    }
    for _ in 0..PRUNE_MAX_BATCHES {
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            break;
        }
        match prune_events(repo_path, older_than_days, keep_newest) {
            // Only a pass that reached the end is remembered; one cut short by
            // its deadline, its batch cap or an error leaves the rest to the
            // next pass.
            Ok(0) => {
                prune_clean_at()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(db_path, std::time::Instant::now());
                break;
            }
            Ok(n) => report.deleted += n,
            Err(error) => {
                log::error!(target: "ledger", "event retention failed for {repo_path}: {error}");
                report.error = format!("ledger retention failed: {error}");
                break;
            }
        }
    }
    report
}

/// The first sibling ledger holding rows [`consolidate_worktree_ledgers`] has
/// not imported into `anchor`, found without importing them.
///
/// The read-only twin of consolidation's steady-state check, for a lookup
/// that must fail closed on unmigrated history rather than run the migration:
/// a legacy sibling may hold the binding event that decides the answer.
/// Sources persist after import, so existence proves nothing; a source is
/// consolidated when it is empty or the anchor's marker for it has reached its
/// newest row — exactly where a completed import leaves the marker. A source
/// that shrank below its marker was replaced, and consolidation re-reads it,
/// so it counts as unimported until then.
pub(crate) fn unconsolidated_legacy_ledger(
    anchor: &str,
    members: &[PathBuf],
) -> Result<Option<PathBuf>, LedgerError> {
    let anchor_path = std::fs::canonicalize(anchor).map_err(|error| {
        LedgerError::new(
            "legacy_anchor_invalid",
            format!("could not resolve family ledger anchor: {error}"),
        )
    })?;
    let mut anchor_conn: Option<Option<Connection>> = None;
    for source in members {
        if source == &anchor_path {
            continue;
        }
        let source_db = ledger_path(&source.to_string_lossy());
        let Some(source_conn) = open_readonly(&source_db)? else {
            continue;
        };
        let source_max: i64 = if has_table(&source_conn, "events")? {
            source_conn
                .query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |row| {
                    row.get(0)
                })
                .map_err(|error| {
                    LedgerError::new(
                        "legacy_read_failed",
                        format!("could not read {}: {error}", source_db.display()),
                    )
                })?
        } else {
            0
        };
        if source_max == 0 {
            continue;
        }
        if anchor_conn.is_none() {
            anchor_conn = Some(open_readonly(&ledger_path(anchor))?);
        }
        let marker = match anchor_conn.as_ref().and_then(Option::as_ref) {
            Some(conn) if has_table(conn, "ledger_family_imports")? => {
                let key = family_source_key(&anchor_path, &source.to_string_lossy())?;
                conn.query_row(
                    "SELECT source_cursor FROM ledger_family_imports WHERE source_key = ?1",
                    params![key],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(|error| LedgerError::new("legacy_marker_failed", error.to_string()))?
                .unwrap_or(0)
            }
            _ => 0,
        };
        if marker != source_max {
            return Ok(Some(source.clone()));
        }
    }
    Ok(None)
}

/// The highest cursor in this repo's ledger, or 0 when it is empty.
///
/// Phase 3's reflog catch-up starts here: everything git recorded after this
/// point happened while GitPulse was not watching.
pub fn latest_cursor(repo_path: &str) -> Result<i64, LedgerError> {
    with_conn(repo_path, |conn| {
        conn.query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |r| r.get(0))
            .map_err(|e| LedgerError::new("query_failed", e.to_string()))
    })
}

/// Whether this repo's ledger is recording, and what has been lost if not.
/// Does this repository already have a ledger?
///
/// The read-only surfaces ask before touching it, because [`with_conn`] opens
/// the database and opening *creates* it — directory, journal and all.
pub fn is_initialised(repo_path: &str) -> bool {
    ledger_path(repo_path).exists()
}

/// [`status`] for a caller that must not create anything.
///
/// `gitpulse-mcp` annotates its tools `readOnlyHint: true, destructiveHint:
/// false`, and MCP clients gate user approval on exactly those. A "read" that
/// created `.devcouncil/ledger.sqlite` in the user's repository — verified: a
/// fresh checkout gained the database, its `-wal` and its `-shm` after a single
/// `gitpulse_insights` call — made that annotation false.
///
/// The desktop app deliberately keeps the creating path: opening a repository
/// there is when the ledger is meant to come into being. This is the read half,
/// and "there is no ledger yet" is a distinct, reported state rather than one
/// conjured into existence so the answer can be "recording".
pub fn status_readonly(repo_path: &str) -> LedgerStatus {
    let path = ledger_path(repo_path);
    let (recording, error, error_code) = if !is_initialised(repo_path) {
        (
            false,
            "no ledger in this repository yet; nothing has been recorded".to_string(),
            "not_initialised".to_string(),
        )
    } else {
        match open_readonly_initialised(&path) {
            Ok(_) => (true, String::new(), String::new()),
            Err(e) => (false, e.message, e.code.to_string()),
        }
    };
    LedgerStatus {
        recording,
        path: path.display().to_string(),
        dropped: if recording {
            dropped_appends(repo_path)
        } else {
            0
        },
        catch_up_failures: if recording {
            catch_up_failures(repo_path)
        } else {
            0
        },
        error,
        error_code,
    }
}

/// [`tail`] for a caller that must not create anything.
///
/// `Ok(None)` means there is no ledger — which is not the same as a ledger
/// holding no events, and must not be rendered as one.
pub fn tail_readonly(
    repo_path: &str,
    cursor: i64,
    limit: u32,
) -> Result<Option<Vec<LedgerEvent>>, LedgerError> {
    if !is_initialised(repo_path) {
        return Ok(None);
    }
    read_after(repo_path, cursor, limit, false, Access::ReadOnly, None)
        .map(|(events, _)| Some(events))
}

/// What a page of the ledger may weigh once serialized, before it is wrapped in
/// a tool result. A quarter of the transport's frame, so the page, its text
/// mirror and the envelope fit with room to spare.
pub const PAGE_MAX_BYTES: usize = 1024 * 1024;

/// [`tail_readonly`] for a page that crosses a size-limited transport: rows are
/// [`LedgerEvent::bounded`] and the page holds at most `max_bytes` (at least
/// one row). The flag is true when rows past the page's last one were left out
/// for that reason.
pub fn tail_readonly_page(
    repo_path: &str,
    cursor: i64,
    limit: u32,
    max_bytes: usize,
) -> Result<Option<(Vec<LedgerEvent>, bool)>, LedgerError> {
    if !is_initialised(repo_path) {
        return Ok(None);
    }
    read_after(
        repo_path,
        cursor,
        limit,
        false,
        Access::ReadOnly,
        Some(max_bytes),
    )
    .map(Some)
}

/// The identity key of an object as it would be stored: redacted, cut to the
/// append cap, and for a cut one tied to the hash of the redacted full text.
///
/// This is not the `object` column. The column holds the capped text, which two
/// different long identities can share; the key adds the full-text hash so they
/// differ. An ingest that compares a freshly built identity with stored rows
/// must compare this key against [`recorded_reflog_objects`], which derives the
/// same key from a row stored after the cap (capped text and recorded hash) or
/// before it (the full redacted text). One function owns both sides.
pub fn stored_object(raw: &str) -> String {
    cap::object_key(redact::text(raw))
}

pub fn status(repo_path: &str) -> LedgerStatus {
    let path = ledger_path(repo_path);
    let dropped = dropped_appends(repo_path);
    let catch_up_failures = catch_up_failures(repo_path);
    match with_conn(repo_path, |_| Ok(())) {
        Ok(()) => LedgerStatus {
            recording: true,
            path: path.display().to_string(),
            dropped,
            catch_up_failures,
            error: String::new(),
            error_code: String::new(),
        },
        Err(e) => LedgerStatus {
            recording: false,
            path: path.display().to_string(),
            dropped,
            catch_up_failures,
            error: e.message,
            error_code: e.code.to_string(),
        },
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PulseSnapshotInput {
    pub day: String,
    pub total_commits: usize,
    pub total_loc: usize,
    pub bus_factor: usize,
    pub coverage_pct: Option<f64>,
    pub snapshot_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PulseSnapshotEntry {
    pub id: i64,
    pub day: String,
    pub repo_path: String,
    pub total_commits: usize,
    pub total_loc: usize,
    pub bus_factor: usize,
    pub coverage_pct: Option<f64>,
    pub snapshot_json: String,
}

pub fn save_pulse_snapshot(repo_path: &str, input: &PulseSnapshotInput) -> Result<(), LedgerError> {
    let repo_identity = redact::text(&canonical_repo(repo_path));
    let day = redact::text(&input.day);
    let snapshot_json = redact::text(&input.snapshot_json);
    with_conn(repo_path, |conn| {
        conn.execute(
            r#"
            INSERT INTO pulse_snapshots (day, repo_path, total_commits, total_loc, bus_factor, coverage_pct, snapshot_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(day, repo_path) DO UPDATE SET
                total_commits = excluded.total_commits,
                total_loc = excluded.total_loc,
                bus_factor = excluded.bus_factor,
                coverage_pct = excluded.coverage_pct,
                snapshot_json = excluded.snapshot_json
            "#,
            params![
                day,
                repo_identity,
                input.total_commits as i64,
                input.total_loc as i64,
                input.bus_factor as i64,
                input.coverage_pct,
                snapshot_json,
            ],
        )
        .map_err(|e| LedgerError::new("insert_snapshot_failed", e.to_string()))?;
        Ok(())
    })
}

pub fn get_pulse_snapshots(
    repo_path: &str,
    limit: Option<usize>,
) -> Result<Vec<PulseSnapshotEntry>, LedgerError> {
    let lim = limit.unwrap_or(90).clamp(1, 365) as i64;
    let canonical_repo_path = canonical_repo(repo_path);
    let repo_identity = redact::text(&canonical_repo_path);
    with_conn(repo_path, |conn| {
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, day, repo_path, total_commits, total_loc, bus_factor, coverage_pct, snapshot_json
                FROM pulse_snapshots
                WHERE repo_path = ?1
                ORDER BY day DESC
                LIMIT ?2
                "#,
            )
            .map_err(|e| LedgerError::new("query_snapshots_failed", e.to_string()))?;

        let rows = stmt
            .query_map(params![repo_identity, lim], |row| {
                Ok(PulseSnapshotEntry {
                    id: row.get(0)?,
                    day: row.get(1)?,
                    repo_path: row.get(2)?,
                    total_commits: row.get::<_, i64>(3)? as usize,
                    total_loc: row.get::<_, i64>(4)? as usize,
                    bus_factor: row.get::<_, i64>(5)? as usize,
                    coverage_pct: row.get(6)?,
                    snapshot_json: row.get(7)?,
                })
            })
            .map_err(|e| LedgerError::new("query_snapshots_failed", e.to_string()))?;

        let mut entries = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| LedgerError::new("snapshot_row_failed", error.to_string()))?;
        for entry in &mut entries {
            entry.repo_path.clone_from(&canonical_repo_path);
        }
        Ok(entries)
    })
}

/// One repository's cached expensive-scan results, as the Fleet grid reads them.
///
/// Every family carries its own nullable value AND its own timestamp. `None`
/// with a `None` timestamp is "never scanned"; a value with a timestamp is a
/// measurement whose age can be shown. There is deliberately no encoding for
/// "scanned, but we forgot when" — that is how a stale number comes to be read
/// as a current one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FleetMetrics {
    pub repo_path: String,
    pub loc: Option<i64>,
    pub loc_language: Option<String>,
    pub loc_truncated: bool,
    pub loc_at: Option<String>,
    pub storage_bytes: Option<i64>,
    pub storage_git_bytes: Option<i64>,
    pub storage_reclaimable_bytes: Option<i64>,
    pub storage_truncated: bool,
    pub storage_at: Option<String>,
    pub vulns_critical: Option<i64>,
    pub vulns_high: Option<i64>,
    pub vulns_moderate: Option<i64>,
    pub vulns_low: Option<i64>,
    pub vulns_unknown: Option<i64>,
    pub vulns_total: Option<i64>,
    pub health_complete: bool,
    pub health_at: Option<String>,
    pub coverage_pct: Option<f64>,
    pub coverage_truncated: bool,
    pub coverage_at: Option<String>,
    /// The language breakdown recorded by the same scan that set `loc`.
    ///
    /// Empty means no breakdown is on file — either nothing was ever scanned,
    /// or the row predates this table. It never means "this repository has no
    /// languages": that case is a scan that ran and stored an empty list, and
    /// `loc_at` is what tells the two apart.
    pub languages: Vec<FleetLanguageStat>,

    /* ── The previous measurement, so a number can carry a direction ─────── */
    //
    // Same shape as every family above: a value and the day it was taken,
    // together or not at all. `None` means there is no earlier measurement on
    // file — a first scan, or a build predating `fleet_history` — and a caller
    // must render no delta rather than a delta of zero, because "unchanged" and
    // "never measured before" are the two facts this whole file exists to keep
    // apart.
    //
    // Deliberately the *previous distinct day*, not "a week ago": nobody
    // guarantees a scan happened a week ago, and interpolating one would invent
    // a measurement. The day travels so the delta can name its own baseline —
    // "+180 MB since Sep 2" rather than an unqualified arrow.
    pub loc_prev: Option<i64>,
    pub loc_prev_day: Option<String>,
    pub storage_prev_bytes: Option<i64>,
    pub storage_prev_day: Option<String>,
    pub vulns_prev_total: Option<i64>,
    pub vulns_prev_day: Option<String>,
    pub coverage_prev_pct: Option<f64>,
    pub coverage_prev_day: Option<String>,
}

/// One language's share of a repository, as the Fleet grid caches it.
///
/// Mirrors [`crate::engine::git_reader::RepoLanguageStat`] field for field on
/// purpose: the frontend folds these through the same
/// `pickLanguageBarStats` the per-repository language bar uses, and a shape
/// that needed adapting would be a second, drifting definition of what a
/// language reading is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetLanguageStat {
    pub language: String,
    pub color_hex: String,
    pub category: String,
    pub code_lines: i64,
    pub file_count: i64,
    pub percentage: f64,
}

/// One family's freshly scanned numbers, on their way into the ledger.
///
/// Exactly one group is populated per call and the rest arrive as `None`,
/// which leaves whatever was already recorded untouched — a storage scan must
/// not blank out last week's audit result just by not being one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FleetMetricsInput {
    pub loc: Option<i64>,
    pub loc_language: Option<String>,
    pub loc_truncated: bool,
    pub storage_bytes: Option<i64>,
    pub storage_git_bytes: Option<i64>,
    pub storage_reclaimable_bytes: Option<i64>,
    pub storage_truncated: bool,
    pub vulns_critical: Option<i64>,
    pub vulns_high: Option<i64>,
    pub vulns_moderate: Option<i64>,
    pub vulns_low: Option<i64>,
    pub vulns_unknown: Option<i64>,
    pub vulns_total: Option<i64>,
    pub health_complete: bool,
    pub coverage_pct: Option<f64>,
    pub coverage_truncated: bool,
    /// The breakdown this call recorded, if it is a language scan.
    ///
    /// Three states, deliberately: `None` is "this call is not about
    /// languages" and leaves the stored breakdown alone, `Some(vec![])` is "a
    /// scan ran and found nothing" and clears it, and a non-empty list
    /// replaces it. Collapsing the first two would make every storage scan
    /// quietly erase the last language scan.
    #[serde(default)]
    pub languages: Option<Vec<FleetLanguageStat>>,
}

fn fleet_metrics_from_row(row: &Row<'_>) -> rusqlite::Result<FleetMetrics> {
    Ok(FleetMetrics {
        repo_path: row.get(0)?,
        loc: row.get(1)?,
        loc_language: row.get(2)?,
        loc_truncated: row.get::<_, i64>(3)? != 0,
        loc_at: row.get(4)?,
        storage_bytes: row.get(5)?,
        storage_git_bytes: row.get(6)?,
        storage_reclaimable_bytes: row.get(7)?,
        storage_truncated: row.get::<_, i64>(8)? != 0,
        storage_at: row.get(9)?,
        vulns_critical: row.get(10)?,
        vulns_high: row.get(11)?,
        vulns_moderate: row.get(12)?,
        vulns_low: row.get(13)?,
        vulns_unknown: row.get(14)?,
        vulns_total: row.get(15)?,
        health_complete: row.get::<_, i64>(16)? != 0,
        health_at: row.get(17)?,
        coverage_pct: row.get(18)?,
        coverage_truncated: row.get::<_, i64>(19)? != 0,
        coverage_at: row.get(20)?,
        // Filled by the caller from `fleet_languages` and `fleet_history`;
        // this mapper only sees the metrics row.
        languages: Vec::new(),
        loc_prev: None,
        loc_prev_day: None,
        storage_prev_bytes: None,
        storage_prev_day: None,
        vulns_prev_total: None,
        vulns_prev_day: None,
        coverage_prev_pct: None,
        coverage_prev_day: None,
    })
}

/// How many days of history one lookup reads before giving up on a baseline.
///
/// The read wants the newest earlier measurement per family, and families are
/// scanned independently — a repository whose storage was scanned yesterday
/// and whose coverage was last scanned in March needs enough rows to find
/// both. Sixty days is generous for that and still a bounded read; past it a
/// family simply reports no baseline, which renders as no delta rather than as
/// a wrong one.
const FLEET_HISTORY_LOOKBACK_DAYS: usize = 60;

/// The UTC calendar day an ISO-8601 stamp falls on, `YYYY-MM-DD`.
///
/// UTC rather than local, matching every other day the ledger records: a
/// machine that crosses midnight during a trip must not file two scans of the
/// same afternoon under two different days.
fn utc_day(iso: &str) -> String {
    iso.chars().take(10).collect()
}

/// How many language rows one repository may cache.
///
/// The frontend draws six plus an aggregate, and folds the tail itself. The
/// cap is here as well so a caller cannot make one repository's breakdown
/// unbounded, and it is generous enough that the fold still has real
/// remainders to work with rather than a pre-truncated list.
pub const MAX_FLEET_LANGUAGES: usize = 16;

fn fleet_language_from_row(row: &Row<'_>) -> rusqlite::Result<FleetLanguageStat> {
    Ok(FleetLanguageStat {
        language: row.get(0)?,
        color_hex: row.get(1)?,
        category: row.get(2)?,
        code_lines: row.get(3)?,
        file_count: row.get(4)?,
        percentage: row.get(5)?,
    })
}

const FLEET_LANGUAGE_COLUMNS: &str =
    "language, color_hex, category, code_lines, file_count, percentage";

const FLEET_METRICS_COLUMNS: &str = "repo_path, loc, loc_language, loc_truncated, loc_at, \
     storage_bytes, storage_git_bytes, storage_reclaimable_bytes, storage_truncated, storage_at, \
     vulns_critical, vulns_high, vulns_moderate, vulns_low, vulns_unknown, vulns_total, \
     health_complete, health_at, coverage_pct, coverage_truncated, coverage_at";

/// Records one family's scan result, stamping it with the moment it landed.
///
/// The `COALESCE`s are what keep the families independent: a column arriving
/// as `NULL` means "this call is not about that family" and the stored value
/// survives, so four separate scans accumulate into one row instead of each
/// erasing the last. A family that IS in this call always overwrites, value
/// and timestamp together — a value can never outlive its own stamp.
pub fn save_fleet_metrics(repo_path: &str, input: &FleetMetricsInput) -> Result<(), LedgerError> {
    let repo_identity = redact::text(&canonical_repo(repo_path));
    let now = ids::iso8601_utc(ids::now_millis());
    let loc_at = input.loc.map(|_| now.clone());
    let storage_at = input.storage_bytes.map(|_| now.clone());
    let health_at = input.vulns_total.map(|_| now.clone());
    let coverage_at = input.coverage_pct.map(|_| now.clone());
    let loc_language = input.loc_language.as_deref().map(redact::text);
    with_conn(repo_path, |conn| {
        // One transaction for the metrics row, the language rows and the
        // day's history: a failure part-way leaves none of them, rather than a
        // fresh scalar row beside last scan's languages.
        let tx = write_tx(conn)
            .map_err(|e| LedgerError::new("fleet_metrics_tx_failed", e.to_string()))?;
        tx.execute(
            r#"
            INSERT INTO fleet_metrics (
                repo_path, loc, loc_language, loc_truncated, loc_at,
                storage_bytes, storage_git_bytes, storage_reclaimable_bytes, storage_truncated, storage_at,
                vulns_critical, vulns_high, vulns_moderate, vulns_low, vulns_unknown, vulns_total,
                health_complete, health_at,
                coverage_pct, coverage_truncated, coverage_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
            ON CONFLICT(repo_path) DO UPDATE SET
                loc                       = COALESCE(excluded.loc, fleet_metrics.loc),
                loc_language              = COALESCE(excluded.loc_language, fleet_metrics.loc_language),
                loc_truncated             = CASE WHEN excluded.loc_at IS NULL
                                                 THEN fleet_metrics.loc_truncated
                                                 ELSE excluded.loc_truncated END,
                loc_at                    = COALESCE(excluded.loc_at, fleet_metrics.loc_at),
                storage_bytes             = COALESCE(excluded.storage_bytes, fleet_metrics.storage_bytes),
                storage_git_bytes         = COALESCE(excluded.storage_git_bytes, fleet_metrics.storage_git_bytes),
                storage_reclaimable_bytes = COALESCE(excluded.storage_reclaimable_bytes, fleet_metrics.storage_reclaimable_bytes),
                storage_truncated         = CASE WHEN excluded.storage_at IS NULL
                                                 THEN fleet_metrics.storage_truncated
                                                 ELSE excluded.storage_truncated END,
                storage_at                = COALESCE(excluded.storage_at, fleet_metrics.storage_at),
                vulns_critical            = COALESCE(excluded.vulns_critical, fleet_metrics.vulns_critical),
                vulns_high                = COALESCE(excluded.vulns_high, fleet_metrics.vulns_high),
                vulns_moderate            = COALESCE(excluded.vulns_moderate, fleet_metrics.vulns_moderate),
                vulns_low                 = COALESCE(excluded.vulns_low, fleet_metrics.vulns_low),
                vulns_unknown             = COALESCE(excluded.vulns_unknown, fleet_metrics.vulns_unknown),
                vulns_total               = COALESCE(excluded.vulns_total, fleet_metrics.vulns_total),
                health_complete           = CASE WHEN excluded.health_at IS NULL
                                                 THEN fleet_metrics.health_complete
                                                 ELSE excluded.health_complete END,
                health_at                 = COALESCE(excluded.health_at, fleet_metrics.health_at),
                coverage_pct              = COALESCE(excluded.coverage_pct, fleet_metrics.coverage_pct),
                coverage_truncated        = CASE WHEN excluded.coverage_at IS NULL
                                                 THEN fleet_metrics.coverage_truncated
                                                 ELSE excluded.coverage_truncated END,
                coverage_at               = COALESCE(excluded.coverage_at, fleet_metrics.coverage_at)
            "#,
            params![
                repo_identity,
                input.loc,
                loc_language,
                input.loc_truncated as i64,
                loc_at,
                input.storage_bytes,
                input.storage_git_bytes,
                input.storage_reclaimable_bytes,
                input.storage_truncated as i64,
                storage_at,
                input.vulns_critical,
                input.vulns_high,
                input.vulns_moderate,
                input.vulns_low,
                input.vulns_unknown,
                input.vulns_total,
                input.health_complete as i64,
                health_at,
                input.coverage_pct,
                input.coverage_truncated as i64,
                coverage_at,
            ],
        )
        .map_err(|e| LedgerError::new("insert_fleet_metrics_failed", e.to_string()))?;
        write_fleet_languages(&tx, &repo_identity, input.languages.as_deref())?;
        write_fleet_history(&tx, &repo_identity, &utc_day(&now), input)?;
        tx.commit()
            .map_err(|e| LedgerError::new("fleet_metrics_commit_failed", e.to_string()))
    })
}

/// Records today's value for whichever family this call carries.
///
/// Deliberately part of the scan write rather than a scheduled job: history
/// then accrues exactly when a measurement is taken, and **a day nobody
/// scanned has no row at all**. That is what keeps a flat line honest — it
/// means "not measured", never "unchanged", and no background task can quietly
/// fill the gap with a repeat of yesterday's number.
///
/// Rescanning the same family twice in a day overwrites the day's value rather
/// than appending, so a day holds one measurement per family: the latest.
fn write_fleet_history(
    conn: &Connection,
    repo_identity: &str,
    day: &str,
    input: &FleetMetricsInput,
) -> Result<(), LedgerError> {
    // Nothing measurable in this call — a languages-only write, say — leaves
    // the day's row untouched rather than creating an all-null one that would
    // read as a day something was measured.
    if input.loc.is_none()
        && input.storage_bytes.is_none()
        && input.vulns_total.is_none()
        && input.coverage_pct.is_none()
    {
        return Ok(());
    }
    conn.execute(
        r#"
        INSERT INTO fleet_history (repo_path, day, loc, storage_bytes, vulns_total, coverage_pct)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ON CONFLICT(repo_path, day) DO UPDATE SET
            loc           = COALESCE(excluded.loc, fleet_history.loc),
            storage_bytes = COALESCE(excluded.storage_bytes, fleet_history.storage_bytes),
            vulns_total   = COALESCE(excluded.vulns_total, fleet_history.vulns_total),
            coverage_pct  = COALESCE(excluded.coverage_pct, fleet_history.coverage_pct)
        "#,
        params![
            repo_identity,
            day,
            input.loc,
            input.storage_bytes,
            input.vulns_total,
            input.coverage_pct,
        ],
    )
    .map_err(|e| LedgerError::new("insert_fleet_history_failed", e.to_string()))?;
    Ok(())
}

/// Replaces one repository's cached language breakdown, or leaves it alone.
///
/// `None` means this call was not a language scan. Only a `Some` clears and
/// rewrites, so the four families stay as independent here as the `COALESCE`s
/// above make them for the scalar columns.
fn write_fleet_languages(
    conn: &Connection,
    repo_identity: &str,
    languages: Option<&[FleetLanguageStat]>,
) -> Result<(), LedgerError> {
    let Some(languages) = languages else {
        return Ok(());
    };
    // Delete-then-insert inside the caller's transaction, so a failure part-way
    // cannot leave a repository with half of one scan and half of the last.
    conn.execute(
        "DELETE FROM fleet_languages WHERE repo_path = ?1",
        params![repo_identity],
    )
    .map_err(|e| LedgerError::new("delete_fleet_languages_failed", e.to_string()))?;
    for stat in languages.iter().take(MAX_FLEET_LANGUAGES) {
        conn.execute(
            r#"
            INSERT INTO fleet_languages
                (repo_path, language, color_hex, category, code_lines, file_count, percentage)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(repo_path, language) DO UPDATE SET
                color_hex  = excluded.color_hex,
                category   = excluded.category,
                code_lines = excluded.code_lines,
                file_count = excluded.file_count,
                percentage = excluded.percentage
            "#,
            params![
                repo_identity,
                redact::text(&stat.language),
                redact::text(&stat.color_hex),
                redact::text(&stat.category),
                stat.code_lines,
                stat.file_count,
                stat.percentage,
            ],
        )
        .map_err(|e| LedgerError::new("insert_fleet_languages_failed", e.to_string()))?;
    }
    Ok(())
}

/// Reads one repository's cached metrics, creating nothing.
///
/// The Fleet grid shows rows for repositories that are merely in the recents
/// list — never opened in this session, possibly never opened at all. Going
/// through [`with_conn`] would create `.devcouncil/ledger.sqlite` inside each
/// of them just for asking a question, which is a write into someone else's
/// repository as a side effect of rendering a row. So this opens read-only and
/// returns `Ok(None)` for a database that is not there, exactly as the family
/// importer does for its source ledgers.
///
/// A read failure is an error, never `Ok(None)`: "this repository has never
/// been scanned" and "we could not find out" must not arrive as the same value.
pub fn read_fleet_metrics(repo_path: &str) -> Result<Option<FleetMetrics>, LedgerError> {
    let canonical_repo_path = canonical_repo(repo_path);
    let repo_identity = redact::text(&canonical_repo_path);
    let db_path = ledger_path(repo_path);
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| LedgerError::new("fleet_metrics_open_failed", e.to_string()))?;
    // A ledger written by a build that predates this table has no
    // `fleet_metrics`, and a read-only connection cannot create it. That is
    // "never scanned", not "unreadable" — checked explicitly rather than by
    // pattern-matching a prepare error's message, which is not ours to parse.
    let has_table: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'fleet_metrics'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| LedgerError::new("fleet_metrics_query_failed", e.to_string()))?;
    if has_table == 0 {
        return Ok(None);
    }
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {FLEET_METRICS_COLUMNS} FROM fleet_metrics WHERE repo_path = ?1"
        ))
        .map_err(|e| LedgerError::new("fleet_metrics_query_failed", e.to_string()))?;
    let mut found = stmt
        .query_row(params![repo_identity], fleet_metrics_from_row)
        .optional()
        .map_err(|e| LedgerError::new("fleet_metrics_row_failed", e.to_string()))?;
    if let Some(metrics) = found.as_mut() {
        // Hand back the path the caller asked about, not the redacted identity
        // the row is keyed by.
        metrics.repo_path.clone_from(&canonical_repo_path);
        // Same read-only connection, same repository: a breakdown that cannot
        // be read is an error, not an empty list, for the same reason the
        // metrics row itself is.
        metrics.languages = read_fleet_languages(&conn, &repo_identity)?;
        read_fleet_baselines(&conn, &repo_identity, metrics)?;
    }
    Ok(found)
}

/// Fills in each family's previous measurement, from `fleet_history`.
///
/// "Previous" means the newest row on a day *strictly before* the newest day
/// on file for that family — so rescanning twice today shows no delta, which
/// is correct: nothing has changed since the last measurement, because this is
/// the last measurement.
///
/// A family with no earlier row is left `None` and renders as no delta. That is
/// not the same as a delta of zero, and it is exactly the distinction a first
/// scan needs: an arrow saying "unchanged" the first time you ever measure
/// something is a claim about a past that was never observed.
fn read_fleet_baselines(
    conn: &Connection,
    repo_identity: &str,
    metrics: &mut FleetMetrics,
) -> Result<(), LedgerError> {
    let has_table: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'fleet_history'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| LedgerError::new("fleet_history_query_failed", e.to_string()))?;
    if has_table == 0 {
        return Ok(());
    }
    // One bounded read, folded per family below, rather than four queries that
    // each walk the same index.
    let mut stmt = conn
        .prepare(&format!(
            "SELECT day, loc, storage_bytes, vulns_total, coverage_pct FROM fleet_history              WHERE repo_path = ?1 ORDER BY day DESC LIMIT {FLEET_HISTORY_LOOKBACK_DAYS}"
        ))
        .map_err(|e| LedgerError::new("fleet_history_query_failed", e.to_string()))?;
    let rows = stmt
        .query_map(params![repo_identity], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<f64>>(4)?,
            ))
        })
        .map_err(|e| LedgerError::new("fleet_history_query_failed", e.to_string()))?;

    // Newest first. For each family the first row carrying a value is its
    // latest day, and the next distinct day carrying one is its baseline.
    let mut latest_day: [Option<String>; 4] = [None, None, None, None];
    for row in rows {
        let (day, loc, storage, vulns, coverage) =
            row.map_err(|e| LedgerError::new("fleet_history_row_failed", e.to_string()))?;
        if let Some(value) = loc {
            fill(
                &mut latest_day[0],
                &day,
                &mut metrics.loc_prev,
                &mut metrics.loc_prev_day,
                value,
            );
        }
        if let Some(value) = storage {
            fill(
                &mut latest_day[1],
                &day,
                &mut metrics.storage_prev_bytes,
                &mut metrics.storage_prev_day,
                value,
            );
        }
        if let Some(value) = vulns {
            fill(
                &mut latest_day[2],
                &day,
                &mut metrics.vulns_prev_total,
                &mut metrics.vulns_prev_day,
                value,
            );
        }
        if let Some(value) = coverage {
            if latest_day[3].is_none() {
                latest_day[3] = Some(day.clone());
            } else if metrics.coverage_prev_pct.is_none()
                && latest_day[3].as_deref() != Some(day.as_str())
            {
                metrics.coverage_prev_pct = Some(value);
                metrics.coverage_prev_day = Some(day.clone());
            }
        }
    }
    Ok(())
}

/// Records the first row for a family as its latest day, and the first row on
/// a *different* day as its baseline. Anything after that is older still and
/// is ignored.
fn fill(
    latest_day: &mut Option<String>,
    day: &str,
    prev_value: &mut Option<i64>,
    prev_day: &mut Option<String>,
    value: i64,
) {
    if latest_day.is_none() {
        *latest_day = Some(day.to_string());
        return;
    }
    if prev_value.is_none() && latest_day.as_deref() != Some(day) {
        *prev_value = Some(value);
        *prev_day = Some(day.to_string());
    }
}

/// The cached language breakdown for one repository, largest first.
///
/// Returns an empty list for a ledger written before `fleet_languages`
/// existed. That is the same fact as a repository whose `loc` was never
/// scanned — no breakdown on file — and the caller distinguishes it from a
/// measured absence through `loc_at`, exactly as it does for every other
/// family.
fn read_fleet_languages(
    conn: &Connection,
    repo_identity: &str,
) -> Result<Vec<FleetLanguageStat>, LedgerError> {
    let has_table: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'fleet_languages'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| LedgerError::new("fleet_languages_query_failed", e.to_string()))?;
    if has_table == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {FLEET_LANGUAGE_COLUMNS} FROM fleet_languages \
             WHERE repo_path = ?1 ORDER BY code_lines DESC, language ASC LIMIT {MAX_FLEET_LANGUAGES}"
        ))
        .map_err(|e| LedgerError::new("fleet_languages_query_failed", e.to_string()))?;
    let rows = stmt
        .query_map(params![repo_identity], fleet_language_from_row)
        .map_err(|e| LedgerError::new("fleet_languages_query_failed", e.to_string()))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| LedgerError::new("fleet_languages_row_failed", e.to_string()))?);
    }
    Ok(out)
}

#[cfg(test)]
mod contract_tests;

#[cfg(test)]
mod reliability_tests;

#[cfg(test)]
mod query_tests;

#[cfg(test)]
mod schema_bump_tests;

#[cfg(test)]
mod schema_bump_adv_tests;

/// Test-only helpers that need the private registry.
#[cfg(test)]
pub(crate) mod tests_support {
    /// Drops `repo_path`'s cached connection, as a process restart would for
    /// that repository.
    ///
    /// Scoped to the caller's own repository on purpose. The registry is
    /// process-wide and every test runs in parallel against it, so emptying it
    /// closed the last connection to another test's sibling ledger between that
    /// test's cold and warm read; closing a WAL database's last connection
    /// checkpoints it and deletes its `-wal`, which changes the stamp
    /// consolidation compares, and the warm read then re-imported the sibling
    /// (the `family_cost_tests` flake). The consolidation memory is left alone
    /// for the same reason: it is keyed by anchor path and checked against the
    /// anchor database's file identity, so nothing in it belongs to the caller.
    pub fn reset_registry_for(repo_path: &str) {
        let db = super::ledger_path(repo_path);
        super::registry().lock().unwrap().remove(&db);
        super::dropped_registry().lock().unwrap().remove(&db);
    }

    /// Read-write ledger opens this thread has made so far.
    pub fn write_opens() -> usize {
        super::tests::LEDGER_WRITE_OPENS.with(std::cell::Cell::get)
    }

    /// Sibling (legacy) ledgers consolidation has opened on this thread.
    pub fn sibling_opens() -> usize {
        super::tests::LEDGER_SIBLING_OPENS.with(std::cell::Cell::get)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn action_segments_fold_to_lowercase_ascii_tokens() {
        assert_eq!(action_segment("Cherry-Pick"), "cherry_pick");
        assert_eq!(action_segment("é!"), "__");
        assert_eq!(action_for_argv(&["git", "Cherry-Pick"]), "git.cherry_pick");
        assert_eq!(action_for_argv(&["git", "9lives"]), "command.run");
    }

    use super::*;
    #[cfg(unix)]
    use crate::procguard::LockedSpawn;

    thread_local! {
        /// Ledger queries this thread has issued through [`read_after`] or
        /// [`latest_binding`]; a test reads it to count what a lookup costs.
        pub(super) static LEDGER_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        /// Read-write opens ([`open`]) this thread has made: each one is the
        /// chmod pass, the WAL pragmas and the schema DDL.
        pub(super) static LEDGER_WRITE_OPENS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        /// Sibling (legacy) ledgers consolidation has opened on this thread.
        pub(super) static LEDGER_SIBLING_OPENS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// The env var that turns [`shm_lock_probe_child`] from a no-op into the
    /// probe, naming the `-shm` file it inspects.
    #[cfg(unix)]
    const SHM_PROBE_ENV: &str = "GITPULSE_LEDGER_SHM_LOCK_PROBE";

    /// SQLite's unix VFS takes a shared lock on the WAL index's "DMS" byte for
    /// as long as a connection uses it: byte `UNIX_SHM_BASE + SQLITE_SHM_NLOCK`
    /// = `(22 + 8) * 4 + 8` = 128. A process that sees no lock there treats the
    /// index as abandoned and reinitialises it.
    #[cfg(unix)]
    const SHM_DMS_BYTE: libc::off_t = 128;

    /// Not a test on its own: the child half of
    /// [`a_live_ledger_connection_keeps_its_wal_index_lock`]. A lock held by
    /// another process is only visible from another process, so the parent
    /// re-runs this binary on exactly this test with the path set.
    #[cfg(unix)]
    #[test]
    fn shm_lock_probe_child() {
        use std::os::unix::io::AsRawFd;
        let Ok(path) = std::env::var(SHM_PROBE_ENV) else {
            return;
        };
        let file = std::fs::File::open(&path).expect("open -shm");
        // SAFETY: a zeroed flock is a valid value of a plain C struct, and
        // every field the call reads is set below before it is made.
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as _;
        lock.l_whence = libc::SEEK_SET as _;
        lock.l_start = SHM_DMS_BYTE;
        lock.l_len = 1;
        // SAFETY: the descriptor is open for the call's duration and `lock`
        // is a valid, exclusively borrowed flock.
        let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut lock) };
        assert_eq!(rc, 0, "F_GETLK failed: {}", std::io::Error::last_os_error());
        println!("PROBE type={} pid={}", lock.l_type, lock.l_pid);
    }

    /// Opening the ledger used to secure its files AFTER the connection
    /// existed, opening and closing a descriptor to `-shm` — and on POSIX that
    /// close drops every fcntl lock this process holds on the file, the
    /// connection's WAL-index lock included. Other processes then reinitialised
    /// the index under it: concurrent `gitpulse-hook` processes reading one
    /// ledger died with SIGBUS in `walFindFrame`. The lock must survive open.
    #[cfg(unix)]
    #[test]
    fn a_live_ledger_connection_keeps_its_wal_index_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_string_lossy().into_owned();
        // `open` itself, holding the connection here: the cached registry
        // connection is shared with every other test, any of which may reset
        // the registry and close it — and the last close deletes `-shm`.
        let conn = open(&ledger_path(&repo)).expect("open the ledger");
        let shm = dir.path().join(".devcouncil").join("ledger.sqlite-shm");
        assert!(shm.exists(), "a WAL ledger in use has an index file");
        // Securing no longer runs after the connection, so the files SQLite
        // creates must already be private: they inherit the database's mode.
        {
            use std::os::unix::fs::PermissionsExt;
            for name in ["ledger.sqlite-wal", "ledger.sqlite-shm"] {
                let path = dir.path().join(".devcouncil").join(name);
                let mode = std::fs::metadata(&path)
                    .unwrap_or_else(|e| panic!("{name}: {e}"))
                    .permissions()
                    .mode();
                assert_eq!(
                    mode & 0o777,
                    0o600,
                    "{name} was created wider than the ledger"
                );
            }
        }

        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "ledger::tests::shm_lock_probe_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(SHM_PROBE_ENV, &shm)
            .output_locked()
            .expect("run the probe");
        let stdout = String::from_utf8_lossy(&output.stdout);
        // libtest prints the child's line after its own "test … " prefix.
        let line = stdout
            .split("PROBE ")
            .nth(1)
            .and_then(|rest| rest.lines().next())
            .map(|rest| format!("PROBE {}", rest.trim_end()))
            .unwrap_or_else(|| {
                panic!(
                    "the probe printed nothing: {stdout} {}",
                    String::from_utf8_lossy(&output.stderr)
                )
            });
        assert_eq!(
            line,
            format!("PROBE type={} pid={}", libc::F_RDLCK, std::process::id()),
            "this process no longer holds the WAL index lock its live connection needs"
        );
        drop(conn);
    }

    /// A read must never bring the thing it is reading into existence.
    ///
    /// `gitpulse-mcp` annotates every tool `readOnlyHint: true,
    /// destructiveHint: false`, and MCP clients gate user approval on exactly
    /// those. Verified before the fix: a fresh checkout gained
    /// `.devcouncil/ledger.sqlite` plus its `-wal` and `-shm` after one
    /// `gitpulse_insights` call, because `status` probes by opening and opening
    /// creates.
    #[test]
    fn a_read_only_status_leaves_no_ledger_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_string_lossy().into_owned();

        let before = status_readonly(&repo);
        assert!(!before.recording, "an absent ledger is not recording");
        assert_eq!(before.error_code, "not_initialised");
        assert!(
            !dir.path().join(".devcouncil").exists(),
            "a read created state in the repository"
        );
        assert!(!is_initialised(&repo));

        // The same call, any number of times, still creates nothing.
        for _ in 0..3 {
            let _ = status_readonly(&repo);
        }
        assert!(!dir.path().join(".devcouncil").exists());
    }

    #[test]
    fn a_read_only_tail_separates_no_ledger_from_an_empty_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_string_lossy().into_owned();

        // No ledger at all: `None`, and nothing created.
        assert!(tail_readonly(&repo, 0, 10).expect("reads").is_none());
        assert!(!dir.path().join(".devcouncil").exists());

        // A ledger that exists and holds nothing: `Some([])`. The two must not
        // render alike — one means "nothing has happened here", the other means
        // "this repository has never been recorded into".
        let created = status(&repo);
        assert!(created.recording, "{}", created.error);
        let empty = tail_readonly(&repo, 0, 10).expect("reads");
        assert_eq!(empty.as_deref(), Some(&[][..]));
        assert!(is_initialised(&repo));
    }

    /// The one way a redaction test reports a leak: it names the field and
    /// which secret, never the secret and never the haystack it was found in,
    /// because a failing assert's message goes to the CI log.
    fn assert_secret_free(haystack: &str, secrets: &[&str], what: &str) {
        for (index, secret) in secrets.iter().enumerate() {
            assert!(!haystack.contains(secret), "secret #{index} reached {what}");
        }
    }

    fn temp_repo() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn draft(repo: &str, action: &str) -> Draft {
        Draft {
            repo_path: repo.to_string(),
            action: action.to_string(),
            actor_kind: Some(ActorKind::Human),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        }
    }

    #[cfg(unix)]
    fn real_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(dir.path())
            .status_locked()
            .expect("run git init");
        assert!(status.success(), "git init failed");
        dir
    }

    #[test]
    fn reading_metrics_for_a_never_opened_repository_creates_nothing() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        let before: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .map(|e| e.expect("entry").file_name())
            .collect();

        assert!(read_fleet_metrics(repo).expect("read").is_none());

        let after: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .map(|e| e.expect("entry").file_name())
            .collect();
        // The Fleet grid renders rows for repositories the user merely has in
        // a recents list. Asking one a question must not write a state
        // directory into it.
        assert_eq!(before, after, "reading metrics must not create .devcouncil");
        assert!(!dir.path().join(".devcouncil").exists());
    }

    fn language(name: &str, lines: i64, pct: f64) -> FleetLanguageStat {
        FleetLanguageStat {
            language: name.to_string(),
            color_hex: "#dea584".to_string(),
            category: "programming".to_string(),
            code_lines: lines,
            file_count: 3,
            percentage: pct,
        }
    }

    /// Writes a metrics row and files its history under an explicit day.
    ///
    /// The production write always stamps *today*, which is right and is what
    /// makes a baseline honest — but it leaves no way to test a baseline
    /// without waiting a day. This moves the row the write just made onto the
    /// day the test means, merging rather than inserting so two scans "on the
    /// same day" behave exactly as two scans on one real day would.
    fn save_on_day(repo: &str, day: &str, input: &FleetMetricsInput) {
        let repo_identity = redact::text(&canonical_repo(repo));
        let today = utc_day(&ids::iso8601_utc(ids::now_millis()));
        save_fleet_metrics(repo, input).expect("save");
        with_conn(repo, |conn| {
            conn.execute(
                r#"
                INSERT INTO fleet_history (repo_path, day, loc, storage_bytes, vulns_total, coverage_pct)
                SELECT repo_path, ?2, loc, storage_bytes, vulns_total, coverage_pct
                  FROM fleet_history WHERE repo_path = ?1 AND day = ?3
                ON CONFLICT(repo_path, day) DO UPDATE SET
                    loc           = COALESCE(excluded.loc, fleet_history.loc),
                    storage_bytes = COALESCE(excluded.storage_bytes, fleet_history.storage_bytes),
                    vulns_total   = COALESCE(excluded.vulns_total, fleet_history.vulns_total),
                    coverage_pct  = COALESCE(excluded.coverage_pct, fleet_history.coverage_pct)
                "#,
                params![&repo_identity, day, &today],
            )
            .map_err(|e| LedgerError::new("reday_failed", e.to_string()))?;
            if day != today {
                conn.execute(
                    "DELETE FROM fleet_history WHERE repo_path = ?1 AND day = ?2",
                    params![&repo_identity, &today],
                )
                .map_err(|e| LedgerError::new("reday_cleanup_failed", e.to_string()))?;
            }
            Ok(())
        })
        .expect("re-day");
    }

    #[test]
    fn a_first_scan_reports_no_baseline_rather_than_a_delta_of_zero() {
        // "Unchanged" the first time you ever measure something is a claim
        // about a past nobody observed.
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                storage_bytes: Some(1_000),
                ..Default::default()
            },
        )
        .expect("save");

        let found = read_fleet_metrics(repo).expect("read").expect("a row");
        assert_eq!(found.storage_bytes, Some(1_000));
        assert!(found.storage_prev_bytes.is_none());
        assert!(found.storage_prev_day.is_none());
    }

    #[test]
    fn the_baseline_is_the_previous_day_a_family_was_measured() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        save_on_day(
            repo,
            "2026-08-01",
            &FleetMetricsInput {
                storage_bytes: Some(1_000),
                ..Default::default()
            },
        );
        save_on_day(
            repo,
            "2026-09-01",
            &FleetMetricsInput {
                storage_bytes: Some(1_500),
                ..Default::default()
            },
        );

        let found = read_fleet_metrics(repo).expect("read").expect("a row");
        assert_eq!(found.storage_bytes, Some(1_500));
        assert_eq!(found.storage_prev_bytes, Some(1_000));
        assert_eq!(found.storage_prev_day.as_deref(), Some("2026-08-01"));
    }

    #[test]
    fn rescanning_the_same_day_leaves_the_baseline_where_it_was() {
        // Two scans on one day are one measurement: the latest. A baseline of
        // "this morning's value" would show a delta for work that is the same
        // measurement taken twice.
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        save_on_day(
            repo,
            "2026-08-01",
            &FleetMetricsInput {
                storage_bytes: Some(1_000),
                ..Default::default()
            },
        );
        save_on_day(
            repo,
            "2026-09-01",
            &FleetMetricsInput {
                storage_bytes: Some(1_500),
                ..Default::default()
            },
        );
        save_on_day(
            repo,
            "2026-09-01",
            &FleetMetricsInput {
                storage_bytes: Some(1_700),
                ..Default::default()
            },
        );

        let found = read_fleet_metrics(repo).expect("read").expect("a row");
        assert_eq!(found.storage_bytes, Some(1_700));
        assert_eq!(
            found.storage_prev_bytes,
            Some(1_000),
            "still yesterday's value"
        );
        assert_eq!(found.storage_prev_day.as_deref(), Some("2026-08-01"));
    }

    #[test]
    fn each_family_keeps_its_own_baseline_day() {
        // Families are scanned independently, so a repository can have a
        // storage baseline from yesterday and a coverage baseline from March.
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        save_on_day(
            repo,
            "2026-03-01",
            &FleetMetricsInput {
                coverage_pct: Some(50.0),
                ..Default::default()
            },
        );
        save_on_day(
            repo,
            "2026-08-31",
            &FleetMetricsInput {
                storage_bytes: Some(1_000),
                ..Default::default()
            },
        );
        save_on_day(
            repo,
            "2026-09-01",
            &FleetMetricsInput {
                storage_bytes: Some(1_500),
                ..Default::default()
            },
        );
        save_on_day(
            repo,
            "2026-09-02",
            &FleetMetricsInput {
                coverage_pct: Some(70.0),
                ..Default::default()
            },
        );

        let found = read_fleet_metrics(repo).expect("read").expect("a row");
        assert_eq!(found.storage_prev_day.as_deref(), Some("2026-08-31"));
        assert_eq!(found.coverage_prev_pct, Some(50.0));
        assert_eq!(found.coverage_prev_day.as_deref(), Some("2026-03-01"));
    }

    #[test]
    fn a_scan_of_one_family_never_records_a_history_value_for_another() {
        // The COALESCE that keeps the families independent in `fleet_metrics`
        // has to hold for history too, or a storage scan would file a repeat of
        // last week's coverage under today and invent a flat line.
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        save_on_day(
            repo,
            "2026-09-01",
            &FleetMetricsInput {
                coverage_pct: Some(70.0),
                ..Default::default()
            },
        );
        save_on_day(
            repo,
            "2026-09-02",
            &FleetMetricsInput {
                storage_bytes: Some(1_000),
                ..Default::default()
            },
        );
        save_on_day(
            repo,
            "2026-09-03",
            &FleetMetricsInput {
                storage_bytes: Some(1_200),
                ..Default::default()
            },
        );

        let found = read_fleet_metrics(repo).expect("read").expect("a row");
        assert!(
            found.coverage_prev_pct.is_none(),
            "coverage was measured once; there is no earlier coverage day to compare with"
        );
    }

    #[test]
    fn a_languages_only_write_records_no_history_day() {
        // An all-null history row would read as a day on which something was
        // measured, and would become a baseline that means nothing.
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                languages: Some(vec![language("Rust", 10, 100.0)]),
                ..Default::default()
            },
        )
        .expect("save");
        let rows: i64 = with_conn(repo, |conn| {
            conn.query_row("SELECT COUNT(*) FROM fleet_history", [], |row| row.get(0))
                .map_err(|e| LedgerError::new("count_failed", e.to_string()))
        })
        .expect("count");
        assert_eq!(rows, 0);
    }

    #[test]
    fn a_ledger_predating_the_history_table_reads_as_no_baseline_not_an_error() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                loc: Some(10),
                ..Default::default()
            },
        )
        .expect("save");
        with_conn(repo, |conn| {
            conn.execute("DROP TABLE IF EXISTS fleet_history", [])
                .map_err(|e| LedgerError::new("drop_failed", e.to_string()))?;
            Ok(())
        })
        .expect("drop");
        tests_support::reset_registry_for(repo);

        let found = read_fleet_metrics(repo).expect("read").expect("a row");
        assert_eq!(found.loc, Some(10));
        assert!(found.loc_prev.is_none());
    }

    #[test]
    fn fleet_languages_round_trip_largest_first() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                loc: Some(300),
                loc_language: Some("Rust".to_string()),
                languages: Some(vec![
                    language("TypeScript", 100, 33.3),
                    language("Rust", 200, 66.7),
                ]),
                ..Default::default()
            },
        )
        .expect("save");

        let found = read_fleet_metrics(repo).expect("read").expect("a row");
        let names: Vec<&str> = found
            .languages
            .iter()
            .map(|stat| stat.language.as_str())
            .collect();
        assert_eq!(names, vec!["Rust", "TypeScript"], "largest first");
        assert_eq!(found.languages[0].code_lines, 200);
        assert_eq!(found.languages[0].category, "programming");
        assert_eq!(found.languages[0].file_count, 3);
    }

    #[test]
    fn a_scan_of_another_family_leaves_the_language_breakdown_alone() {
        // The `None` case. A storage scan carries no languages, and quietly
        // erasing the last language scan is exactly the cross-family erasure
        // the COALESCEs prevent for the scalar columns.
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                loc: Some(300),
                languages: Some(vec![language("Rust", 200, 100.0)]),
                ..Default::default()
            },
        )
        .expect("save loc");
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                storage_bytes: Some(1024),
                ..Default::default()
            },
        )
        .expect("save storage");

        let found = read_fleet_metrics(repo).expect("read").expect("a row");
        assert_eq!(found.storage_bytes, Some(1024));
        assert_eq!(found.languages.len(), 1, "the breakdown survived");
    }

    #[test]
    fn a_language_scan_that_found_nothing_clears_the_breakdown() {
        // The `Some(vec![])` case, which is a different fact from `None`: a
        // repository emptied of source has no languages, and leaving last
        // week's list on the row would report code that is no longer there.
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                loc: Some(300),
                languages: Some(vec![language("Rust", 200, 100.0)]),
                ..Default::default()
            },
        )
        .expect("save loc");
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                loc: Some(0),
                languages: Some(Vec::new()),
                ..Default::default()
            },
        )
        .expect("save empty loc");

        let found = read_fleet_metrics(repo).expect("read").expect("a row");
        assert!(found.languages.is_empty());
    }

    #[test]
    fn a_rescan_replaces_the_breakdown_rather_than_accumulating_it() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                loc: Some(300),
                languages: Some(vec![language("Rust", 200, 66.0), language("Go", 100, 34.0)]),
                ..Default::default()
            },
        )
        .expect("first scan");
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                loc: Some(200),
                languages: Some(vec![language("Rust", 200, 100.0)]),
                ..Default::default()
            },
        )
        .expect("second scan");

        let found = read_fleet_metrics(repo).expect("read").expect("a row");
        assert_eq!(
            found.languages.len(),
            1,
            "a language that is gone must not linger from the previous scan"
        );
        assert_eq!(found.languages[0].language, "Rust");
    }

    #[test]
    fn the_breakdown_is_capped_so_one_repository_cannot_grow_without_bound() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        let many: Vec<FleetLanguageStat> = (0..MAX_FLEET_LANGUAGES + 8)
            .map(|i| language(&format!("Lang{i}"), (1000 - i) as i64, 1.0))
            .collect();
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                loc: Some(9000),
                languages: Some(many),
                ..Default::default()
            },
        )
        .expect("save");

        let found = read_fleet_metrics(repo).expect("read").expect("a row");
        assert_eq!(found.languages.len(), MAX_FLEET_LANGUAGES);
    }

    #[test]
    fn a_ledger_predating_the_language_table_reads_as_no_breakdown_not_an_error() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                loc: Some(300),
                languages: Some(vec![language("Rust", 200, 100.0)]),
                ..Default::default()
            },
        )
        .expect("save");
        with_conn(repo, |conn| {
            conn.execute("DROP TABLE IF EXISTS fleet_languages", [])
                .map_err(|e| LedgerError::new("drop_failed", e.to_string()))?;
            Ok(())
        })
        .expect("drop");
        tests_support::reset_registry_for(repo);

        let found = read_fleet_metrics(repo).expect("read").expect("a row");
        assert_eq!(found.loc, Some(300), "the metrics row still reads");
        assert!(found.languages.is_empty());
    }

    #[test]
    fn fleet_metrics_round_trip_carries_a_timestamp_with_every_value() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                loc: Some(4200),
                loc_language: Some("Rust".to_string()),
                ..Default::default()
            },
        )
        .expect("save");

        let found = read_fleet_metrics(repo).expect("read").expect("a row");
        assert_eq!(found.loc, Some(4200));
        assert_eq!(found.loc_language.as_deref(), Some("Rust"));
        // A value with no stamp is a number nobody can date; the writer stamps
        // exactly the families it carries.
        assert!(found.loc_at.is_some());
        assert!(found.storage_at.is_none());
        assert!(found.storage_bytes.is_none());
    }

    #[test]
    fn one_family_scan_never_erases_another() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                vulns_total: Some(3),
                vulns_high: Some(3),
                health_complete: true,
                ..Default::default()
            },
        )
        .expect("save health");
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                storage_bytes: Some(2048),
                storage_git_bytes: Some(1024),
                ..Default::default()
            },
        )
        .expect("save storage");

        let found = read_fleet_metrics(repo).expect("read").expect("a row");
        // A storage scan is not an audit result. Letting it blank the audit
        // would turn "3 high" into "never scanned" for free.
        assert_eq!(found.vulns_total, Some(3));
        assert!(found.health_complete);
        assert!(found.health_at.is_some());
        assert_eq!(found.storage_bytes, Some(2048));
        assert!(found.storage_at.is_some());
    }

    #[test]
    fn a_rescan_replaces_its_own_family_flags() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                vulns_total: Some(0),
                health_complete: false,
                ..Default::default()
            },
        )
        .expect("save partial audit");
        save_fleet_metrics(
            repo,
            &FleetMetricsInput {
                vulns_total: Some(0),
                health_complete: true,
                ..Default::default()
            },
        )
        .expect("save complete audit");

        let found = read_fleet_metrics(repo).expect("read").expect("a row");
        // The flag has to follow its own family both ways, or a repository
        // stays marked "coverage incomplete" forever after one bad scan.
        assert!(found.health_complete);
    }

    /// The defect: the ledger had no version stamp, so an older writer was
    /// never refused. A fresh ledger is stamped; a ledger stamped by a newer
    /// writer is refused without being altered; an unstamped (pre-stamp)
    /// ledger is adopted and stamped.
    #[test]
    fn a_ledger_stamped_by_a_newer_writer_is_refused_unaltered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().to_string_lossy().into_owned();
        let db = ledger_path(&repo);
        let version = |db: &Path| -> i64 {
            Connection::open(db)
                .unwrap()
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .unwrap()
        };
        drop(open(&db).expect("a fresh ledger opens"));
        assert_eq!(version(&db), WRITER_VERSION, "a fresh ledger is stamped");

        // A pre-stamp ledger (stamp 0) is adopted.
        Connection::open(&db)
            .unwrap()
            .pragma_update(None, "user_version", 0)
            .unwrap();
        drop(open(&db).expect("an unstamped ledger is adopted"));
        assert_eq!(version(&db), WRITER_VERSION);

        // A newer writer's ledger is refused, and left exactly as found.
        Connection::open(&db)
            .unwrap()
            .pragma_update(None, "user_version", WRITER_VERSION + 1)
            .unwrap();
        let error = open(&db).expect_err("an older writer must be refused");
        assert_eq!(error.code, "writer_too_old", "{error:?}");
        assert_eq!(
            version(&db),
            WRITER_VERSION + 1,
            "a refused writer changed the stamp"
        );
    }

    /// `reset_registry` stands in for a restart of one repository. It used to
    /// close *every* cached connection in the process, so a parallel test's
    /// reset closed the last connection to another test's sibling ledger
    /// between that test's cold and warm read. Closing the last connection to
    /// a WAL database checkpoints and deletes its `-wal` file, which changes the
    /// stamp consolidation compares: the warm read then re-imported the
    /// sibling, spawning `git hash-object` and opening it again.
    #[test]
    fn resetting_one_repository_leaves_another_repositorys_connection_alone() {
        let mine = temp_repo();
        let theirs = temp_repo();
        let mine = mine.path().to_str().unwrap();
        let theirs = theirs.path().to_str().unwrap();
        append(draft(mine, "git.commit")).expect("append");
        append(draft(theirs, "git.commit")).expect("append");

        tests_support::reset_registry_for(mine);

        let held = registry().lock().unwrap();
        assert!(
            !held.contains_key(&ledger_path(mine)),
            "the reset must close the caller's own connection"
        );
        assert!(
            held.contains_key(&ledger_path(theirs)),
            "a reset closed a connection belonging to another repository"
        );
    }

    #[test]
    fn a_ledger_without_the_table_reads_as_never_scanned() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        // A ledger written by a build predating fleet_metrics: create the
        // database with the events schema only.
        append(draft(repo, "git.commit")).expect("append");
        let db = ledger_path(repo);
        {
            let conn = Connection::open(&db).expect("open");
            conn.execute("DROP TABLE IF EXISTS fleet_metrics", [])
                .expect("drop");
        }
        tests_support::reset_registry_for(repo);

        // Not an error: a missing table is "nothing was ever recorded", and a
        // read-only connection could not create it anyway.
        assert!(read_fleet_metrics(repo).expect("read").is_none());
    }

    #[test]
    fn zz_characterize_tail_returns_oldest_and_hides_clamping() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        for i in 0..60 {
            append(draft(repo, &format!("git.commit{i}"))).expect("append");
        }
        let page = tail(repo, 0, 10).expect("tail");
        println!(
            "CHARACTERIZE tail(cursor=0, limit=10) -> first={:?} last={:?} len={}",
            page.first().map(|e| e.action.clone()),
            page.last().map(|e| e.action.clone()),
            page.len()
        );
        let big = tail(repo, 0, 100_000).expect("tail");
        println!(
            "CHARACTERIZE tail(limit=100000) -> len={} (asked for 100000, nothing says it was capped)",
            big.len()
        );
    }

    #[test]
    fn appends_and_reads_back_in_order() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();

        for i in 0..5 {
            append(draft(repo, &format!("git.commit{i}"))).expect("append");
        }

        let events = tail(repo, 0, 100).expect("tail");
        assert_eq!(events.len(), 5);
        for w in events.windows(2) {
            assert!(w[0].id < w[1].id, "ids must ascend");
            assert!(w[0].ulid < w[1].ulid, "ulids must ascend with ids");
        }
        assert_eq!(events[0].schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn survives_the_process_that_wrote_it() {
        // The done-when for this phase: kill the app, reopen, history intact.
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap().to_string();
        append(draft(&repo, "git.rebase")).expect("append");

        // Drop the cached connection, as a restart would.
        tests_support::reset_registry_for(&repo);

        let events = tail(&repo, 0, 100).expect("tail after reopen");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "git.rebase");

        // And it continues appending rather than starting over.
        append(draft(&repo, "git.commit")).expect("append after reopen");
        let events = tail(&repo, 0, 100).expect("tail");
        assert_eq!(events.len(), 2);
        assert!(events[1].id > events[0].id, "the sequence continues");
    }

    #[test]
    fn paging_from_a_cursor_returns_only_what_followed() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        for i in 0..10 {
            append(draft(repo, &format!("a.b{i}"))).expect("append");
        }
        let first = tail(repo, 0, 4).expect("tail");
        assert_eq!(first.len(), 4);
        let next = tail(repo, first.last().unwrap().id, 100).expect("tail");
        assert_eq!(next.len(), 6);
        assert!(next[0].id > first[3].id);
    }

    #[test]
    fn secrets_are_redacted_before_they_reach_the_disk() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        let key = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";

        let mut d = draft(repo, "git.push");
        d.worktree_path = Some(format!("/tmp/{key}"));
        d.actor_id = Some(format!("actor-{key}"));
        d.session_id = Some(format!("session-{key}"));
        d.task_id = Some(format!("task-{key}"));
        d.object = Some(format!("https://{key}@github.com/o/r"));
        d.argv_json = Some(format!(r#"["git","push","https://{key}@github.com/o/r"]"#));
        d.verdict_json = Some(format!(
            r#"{{"status":"unchecked","target":"git push https://{key}@github.com/o/r","detail":"Authorization: Bearer {key}"}}"#,
        ));
        d.before_ref = Some(format!("before-{key}"));
        d.after_ref = Some(format!("after-{key}"));
        d.detail_json = Some(format!(r#"{{"token":"{key}"}}"#));
        append(d).expect("append");

        // Read the raw file, not the API: the requirement is about what is on
        // disk, and an API that redacted on read would pass a weaker test.
        // SQLite may keep the newest page in WAL, so inspect every durable
        // byte rather than accidentally proving only that the checkpoint has
        // not happened yet.
        let db = ledger_path(repo);
        let mut bytes = std::fs::read(&db).expect("read db");
        for suffix in ["-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{suffix}", db.display()));
            if let Ok(extra) = std::fs::read(path) {
                bytes.extend(extra);
            }
        }
        let raw = String::from_utf8_lossy(&bytes);
        assert_secret_free(&raw, &[key], "the database");

        let events = tail(repo, 0, 10).expect("tail");
        assert!(events[0].argv_json.as_ref().unwrap().contains("ghp_"));
        assert_secret_free(
            events[0].argv_json.as_ref().unwrap(),
            &[key],
            "the argv field",
        );
        let event_json = serde_json::to_string(&events[0]).expect("serialize event");
        assert!(
            event_json.contains("ghp_"),
            "credential shape stays identifiable"
        );
        assert_secret_free(&event_json, &[key], "a ledger field read back");
    }

    #[test]
    fn serialized_contextual_secrets_are_redacted_on_disk_without_corrupting_json() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        let auth = "opaque-auth-secret";
        let cookie = "opaque-cookie-secret";
        let password = "opaque-password-secret";
        let cli_password = "opaque-cli-password-secret";
        let private_key = "opaque-private-key-body";

        let mut d = draft(repo, "git.fetch");
        d.object = Some(format!(
            "git -c 'http.extraHeader=Authorization: Bearer {auth}' fetch"
        ));
        d.argv_json = Some(format!(
            r#"["git","-c","http.extraHeader=Cookie: session={cookie}","fetch"]"#
        ));
        d.verdict_json = Some(format!(
            r#"{{"target":"Authorization: Bearer {auth}\\nnext","phase":"gate"}}"#
        ));
        d.detail_json = Some(format!(
            r#"{{"error":"password={password}","argv":["tool","--password","{cli_password}","next"],"key":"-----BEGIN PRIVATE KEY-----\\n{private_key}\\n-----END PRIVATE KEY-----","phase":"gate"}}"#
        ));
        append(d).expect("append");

        let db = ledger_path(repo);
        let mut bytes = std::fs::read(&db).expect("read db");
        for suffix in ["-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{suffix}", db.display()));
            if let Ok(extra) = std::fs::read(path) {
                bytes.extend(extra);
            }
        }
        let raw = String::from_utf8_lossy(&bytes);
        let secrets = [auth, cookie, password, cli_password, private_key];
        assert_secret_free(&raw, &secrets, "disk");

        let event = tail(repo, 0, 10).expect("tail").pop().expect("event");
        assert!(event.object.as_deref().unwrap().contains("<redacted>"));
        for json in [event.argv_json, event.verdict_json, event.detail_json] {
            let json = json.expect("serialized field");
            assert_secret_free(&json, &secrets, "a ledger field read back");
            serde_json::from_str::<serde_json::Value>(&json).unwrap_or_else(|error| {
                panic!("redaction corrupted serialized JSON: {error}: {json}")
            });
            assert!(
                json.contains("<redacted>"),
                "redaction marker missing: {json}"
            );
        }
    }

    #[test]
    fn redaction_failure_messages_do_not_echo_the_secret() {
        let secret = "opaque-leaked-secret-value";
        let haystack = format!(r#"{{"field":"{secret}","other":"x"}}"#);

        // Drive the failure path itself and read everything it emits: the
        // panic payload is what the test runner prints into the log.
        let caught = std::panic::catch_unwind(|| {
            assert_secret_free(&haystack, &[secret], "disk");
        })
        .expect_err("a leaked secret must fail the assertion");
        let emitted = caught
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| caught.downcast_ref::<&str>().map(|s| (*s).to_string()))
            .expect("the failure carries a message");

        assert!(
            emitted.contains("disk"),
            "the failure must still say where the leak was: {emitted}"
        );
        assert!(
            !emitted.contains(secret) && !emitted.contains(&haystack),
            "the failure message echoed the secret or the text holding it"
        );

        // Control: a clean haystack does not trip it.
        assert_secret_free("nothing here", &[secret], "disk");
    }

    #[cfg(unix)]
    #[test]
    fn ledger_database_and_sidecars_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        append(draft(repo, "git.commit")).expect("append");

        let db = ledger_path(repo);
        let state_dir = db.parent().expect("state dir");
        assert_eq!(
            std::fs::metadata(state_dir)
                .expect("state dir metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "the directory protects SQLite sidecars created after open"
        );
        assert_eq!(
            std::fs::metadata(&db)
                .expect("db metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "the ledger is private even when the process umask is permissive"
        );
        for suffix in ["-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{suffix}", db.display()));
            if path.exists() {
                assert_eq!(
                    std::fs::metadata(&path)
                        .expect("sidecar metadata")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600,
                    "{} must be private",
                    path.display()
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn repository_state_symlink_cannot_redirect_or_repermission_the_ledger() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let repo = real_repo();
        let outside = tempfile::tempdir().expect("outside dir");
        std::fs::set_permissions(outside.path(), std::fs::Permissions::from_mode(0o755))
            .expect("set outside mode");
        symlink(outside.path(), repo.path().join(".devcouncil")).expect("plant state symlink");

        let result = append(draft(repo.path().to_str().unwrap(), "git.commit"));
        assert!(
            result.is_err(),
            "a repository-controlled symlink was followed"
        );
        let status = status(repo.path().to_str().unwrap());
        assert!(!status.recording, "an unsafe ledger path looked healthy");
        assert_eq!(status.error_code, "state_path_symlink");
        assert!(
            !outside.path().join("ledger.sqlite").exists(),
            "the ledger was created outside the repository"
        );
        assert_eq!(
            std::fs::metadata(outside.path())
                .expect("outside metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755,
            "the repository caused GitPulse to chmod an outside directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ledger_file_symlink_cannot_redirect_the_database() {
        use std::os::unix::fs::symlink;

        let repo = real_repo();
        let state = repo.path().join(".devcouncil");
        std::fs::create_dir(&state).expect("state dir");
        let outside = tempfile::NamedTempFile::new().expect("outside file");
        std::fs::write(outside.path(), b"outside stays unchanged").expect("outside marker");
        symlink(outside.path(), state.join("ledger.sqlite")).expect("plant ledger symlink");

        let result = append(draft(repo.path().to_str().unwrap(), "git.commit"));
        assert!(result.is_err(), "a symlinked database was opened");
        assert_eq!(
            std::fs::read(outside.path()).expect("outside body"),
            b"outside stays unchanged",
            "SQLite modified the symlink target before refusing it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ledger_sidecar_symlink_is_refused_before_sqlite_opens() {
        use std::os::unix::fs::symlink;

        let repo = real_repo();
        let state = repo.path().join(".devcouncil");
        std::fs::create_dir(&state).expect("state dir");
        let outside = tempfile::NamedTempFile::new().expect("outside file");
        std::fs::write(outside.path(), b"outside stays unchanged").expect("outside marker");
        symlink(outside.path(), state.join("ledger.sqlite-wal")).expect("plant WAL symlink");

        let result = append(draft(repo.path().to_str().unwrap(), "git.commit"));
        assert!(result.is_err(), "a symlinked SQLite sidecar was accepted");
        assert_eq!(
            std::fs::read(outside.path()).expect("outside body"),
            b"outside stays unchanged",
            "SQLite modified the sidecar symlink target before refusing it"
        );
    }

    #[test]
    fn an_event_must_name_a_repo_and_an_action() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();

        let mut d = draft(repo, "");
        assert_eq!(append(d).unwrap_err().code, "no_action");

        d = draft("", "git.commit");
        assert_eq!(append(d).unwrap_err().code, "no_repo");
    }

    #[test]
    fn the_schema_refuses_an_outcome_it_does_not_define() {
        // The CHECK constraint is the reason a consumer can switch on `outcome`
        // exhaustively. Proven by going around the typed API.
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        append(draft(repo, "git.commit")).expect("append");

        let err = with_conn(repo, |conn| {
            conn.execute(
                "INSERT INTO events (ulid, ts_utc, schema_version, repo_path,
                     actor_kind, action, outcome)
                 VALUES ('X', '2026-01-01T00:00:00.000Z', 1, ?1, 'human', 'a.b', 'maybe')",
                params![repo],
            )
            .map_err(|e| LedgerError::new("insert_failed", e.to_string()))
        });
        assert!(
            err.is_err(),
            "'maybe' is not an outcome and must be refused"
        );

        let err = with_conn(repo, |conn| {
            conn.execute(
                "INSERT INTO events (ulid, ts_utc, schema_version, repo_path,
                     actor_kind, action, outcome)
                 VALUES ('Y', '2026-01-01T00:00:00.000Z', 1, ?1, 'robot', 'a.b', 'ok')",
                params![repo],
            )
            .map_err(|e| LedgerError::new("insert_failed", e.to_string()))
        });
        assert!(
            err.is_err(),
            "'robot' is not an actor kind and must be refused"
        );
    }

    #[test]
    fn a_blocked_action_is_recorded_as_blocked_not_failed() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        let mut d = draft(repo, "git.push");
        d.outcome = Some(Outcome::Blocked);
        d.verdict_json = Some(r#"{"rule":"command.force_push","action":"deny"}"#.into());
        append(d).expect("append");

        let events = tail(repo, 0, 10).expect("tail");
        assert_eq!(events[0].outcome, "blocked");
        assert!(events[0]
            .verdict_json
            .as_ref()
            .unwrap()
            .contains("force_push"));
    }

    #[test]
    fn status_reports_recording_for_a_writable_repo() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        append(draft(repo, "a.b")).expect("append");
        let s = status(repo);
        assert!(s.recording);
        assert!(s.error.is_empty());
        assert!(s.path.ends_with("ledger.sqlite"));
    }

    #[test]
    fn append_failures_are_counted_only_for_the_affected_repository() {
        let broken = temp_repo();
        let healthy = temp_repo();
        let broken_repo = broken.path().to_str().unwrap();
        let healthy_repo = healthy.path().to_str().unwrap();

        // Establish the healthy repository before provoking a write failure in
        // a different checkout. A regular file at the state-directory path is
        // a deterministic boundary failure on every supported platform.
        assert!(status(healthy_repo).recording);
        std::fs::write(broken.path().join(".devcouncil"), b"not a directory")
            .expect("plant invalid state path");
        assert!(
            append(draft(broken_repo, "git.commit")).is_err(),
            "the invalid ledger path unexpectedly accepted an event"
        );

        let broken_status = status(broken_repo);
        let healthy_status = status(healthy_repo);
        assert_eq!(
            broken_status.dropped, 1,
            "the failed append was not counted"
        );
        assert_eq!(
            healthy_status.dropped, 0,
            "a failure in one repository contaminated another repository's status"
        );
    }

    #[test]
    fn latest_cursor_tracks_the_end_of_the_log() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        assert_eq!(latest_cursor(repo).expect("cursor"), 0);
        let id = append(draft(repo, "a.b")).expect("append");
        assert_eq!(latest_cursor(repo).expect("cursor"), id);
    }

    #[test]
    fn tail_limit_is_bounded_in_both_directions() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        for _ in 0..3 {
            append(draft(repo, "a.b")).expect("append");
        }
        // Zero would otherwise return nothing forever and look like "no events".
        assert_eq!(tail(repo, 0, 0).expect("tail").len(), 1);
        assert_eq!(tail(repo, 0, u32::MAX).expect("tail").len(), 3);
    }

    /// The journal keeps the newest rows. Reaching them by paging `tail`
    /// from 0 was one request per 200 rows, and one `git worktree list`
    /// each: ~148 at launch on a 29,573-row ledger.
    #[test]
    fn tail_newest_returns_the_most_recent_window_oldest_first() {
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap();
        for i in 0..6 {
            append(draft(repo, &format!("a.b{i}"))).expect("append");
        }
        let actions = |events: Vec<LedgerEvent>| -> Vec<String> {
            events.into_iter().map(|event| event.action).collect()
        };
        assert_eq!(
            actions(tail_newest(repo, 0, 2).expect("newest")),
            ["a.b4", "a.b5"]
        );
        let window = tail_newest(repo, 0, 2).expect("newest");
        assert_eq!(
            window.last().map(|event| event.id),
            Some(latest_cursor(repo).expect("cursor")),
            "the window ends at the high-water mark, so continuing from it misses nothing"
        );
        let after_first = tail(repo, 0, 1).expect("tail")[0].id;
        assert_eq!(
            actions(tail_newest(repo, after_first, 100).expect("newest")),
            ["a.b1", "a.b2", "a.b3", "a.b4", "a.b5"],
            "a window larger than what follows the cursor is all of it"
        );
        assert_eq!(tail_newest(repo, 0, 0).expect("newest").len(), 1);
    }

    #[test]
    fn concurrent_appends_all_land() {
        // One writer, many callers. Every mutation path in the app can append,
        // and none of them may lose an event to a lock.
        let dir = temp_repo();
        let repo = dir.path().to_str().unwrap().to_string();
        std::thread::scope(|s| {
            for t in 0..8 {
                let repo = repo.clone();
                s.spawn(move || {
                    for i in 0..25 {
                        append(draft(&repo, &format!("t{t}.i{i}"))).expect("append");
                    }
                });
            }
        });
        assert_eq!(tail(&repo, 0, 1000).expect("tail").len(), 200);
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    /// One repository, one ledger — however its path is spelled.
    ///
    /// `validate_repo` canonicalises before running git, so a caller that
    /// passed the raw path and a caller that passed the canonical one wrote to
    /// two different databases. Each held part of the history and neither knew
    /// about the other, which is worse than either being empty.
    #[test]
    fn a_symlinked_or_uncanonical_path_uses_the_same_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().to_str().unwrap().to_string();
        let canonical = std::fs::canonicalize(&raw)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        append(Draft {
            repo_path: raw.clone(),
            action: "git.commit".into(),
            actor_kind: Some(ActorKind::Human),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .expect("append via the raw path");

        // The canonical spelling must see it.
        let events = tail(&canonical, 0, 10).expect("tail");
        assert_eq!(
            events.len(),
            1,
            "the canonical path opened a different ledger from the raw one"
        );
        assert_eq!(events[0].repo_path, canonical, "rows store one spelling");

        // ...and so must a trailing slash.
        assert_eq!(tail(&format!("{raw}/"), 0, 10).expect("tail").len(), 1);
        assert_eq!(ledger_path(&raw), ledger_path(&canonical));
    }

    #[test]
    fn a_path_that_cannot_be_canonicalised_still_resolves() {
        // A removed repository must not turn a read into an error.
        let p = ledger_path("/definitely/not/here");
        assert!(p.ends_with("ledger.sqlite"));
        assert_eq!(ledger_path("/definitely/not/here/"), p);
    }

    #[test]
    fn a_redacted_storage_path_round_trips_as_the_requested_repository_identity() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("password=opaque-repo-secret");
        std::fs::create_dir(&repo).unwrap();
        let raw = repo.to_string_lossy().into_owned();
        let canonical = std::fs::canonicalize(&repo)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        append(Draft {
            repo_path: raw.clone(),
            action: "git.commit".into(),
            actor_kind: Some(ActorKind::Human),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .expect("append from a path whose spelling is secret-shaped");

        let events = tail(&raw, 0, 10).expect("tail");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].repo_path, canonical,
            "storage redaction must not break the repository identity returned to its caller"
        );
    }

    #[test]
    fn tail_refuses_a_row_with_a_foreign_repository_identity() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_string_lossy().into_owned();
        append(Draft {
            repo_path: repo.clone(),
            action: "git.commit".into(),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .expect("append");
        with_conn(&repo, |conn| {
            conn.execute("UPDATE events SET repo_path = '/different/repository'", [])
                .expect("tamper fixture");
            Ok(())
        })
        .expect("tamper row");

        let error = tail(&repo, 0, 10).expect_err("foreign row must be refused");
        assert_eq!(error.code, "repository_mismatch");
    }

    #[test]
    fn pulse_snapshots_persist_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        let input = PulseSnapshotInput {
            day: "2026-09-02".to_string(),
            total_commits: 42,
            total_loc: 1337,
            bus_factor: 2,
            coverage_pct: Some(88.5),
            snapshot_json: "{\"test\":true}".to_string(),
        };

        save_pulse_snapshot(path, &input).expect("save snapshot");

        let snapshots = get_pulse_snapshots(path, Some(10)).expect("get snapshots");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].day, "2026-09-02");
        assert_eq!(snapshots[0].total_commits, 42);
        assert_eq!(snapshots[0].total_loc, 1337);
        assert_eq!(snapshots[0].bus_factor, 2);
        assert_eq!(snapshots[0].coverage_pct, Some(88.5));
    }

    #[test]
    fn pulse_snapshots_redact_storage_without_splitting_repository_identity() {
        let parent = tempfile::tempdir().unwrap();
        let repo = parent.path().join("password=opaque-repo-secret");
        std::fs::create_dir(&repo).unwrap();
        let repo = repo.canonicalize().unwrap().to_string_lossy().into_owned();
        let secret = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
        save_pulse_snapshot(
            &repo,
            &PulseSnapshotInput {
                day: "2026-09-03".to_string(),
                total_commits: 1,
                total_loc: 2,
                bus_factor: 1,
                coverage_pct: None,
                snapshot_json: format!(r#"{{"token":"{secret}"}}"#),
            },
        )
        .expect("save redacted snapshot");

        let snapshots = get_pulse_snapshots(&repo, Some(10)).expect("read snapshot");
        assert_eq!(snapshots[0].repo_path, repo);
        assert!(!snapshots[0].snapshot_json.contains(secret));

        let db = ledger_path(&repo);
        let mut bytes = std::fs::read(&db).expect("read database");
        for suffix in ["-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{suffix}", db.display()));
            if let Ok(extra) = std::fs::read(path) {
                bytes.extend(extra);
            }
        }
        let raw = String::from_utf8_lossy(&bytes);
        assert!(
            !raw.contains(secret),
            "snapshot secret reached durable storage"
        );
        assert!(
            !raw.contains("opaque-repo-secret"),
            "secret-shaped repository path reached the snapshot row"
        );
    }
}

#[cfg(test)]
mod stamp_tests;

#[cfg(test)]
mod object_cap_tests;

#[cfg(test)]
mod adversarial_cap_tests;

#[cfg(test)]
mod adv_l5_tests;

#[cfg(test)]
mod adv2_w4l1_tests;
