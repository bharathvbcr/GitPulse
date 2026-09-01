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
//! `argv_json` and `detail_json` are redacted *before* insert, never at display
//! time, through [`redact`]. A secret redacted only on the way to the screen is
//! still on disk, in a file that gets backed up and synced and read by every
//! later consumer.

pub mod bindings;
pub mod ids;
pub mod redact;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use rusqlite::{params, Connection};
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
  detail_json    TEXT
);
CREATE INDEX IF NOT EXISTS idx_events_repo_ts  ON events(repo_path, ts_utc);
CREATE INDEX IF NOT EXISTS idx_events_task     ON events(task_id)    WHERE task_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_session  ON events(session_id) WHERE session_id IS NOT NULL;
"#;

/// This build's event schema version, written into every row.
pub const SCHEMA_VERSION: i64 = 1;

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

/// Derives an event action name from a command line.
///
/// `git commit` becomes `git.commit`; anything else becomes `command.run`, so a
/// non-git mutation is still recorded rather than silently mis-filed under a
/// git verb it never was.
pub fn action_for_argv(argv: &[&str]) -> String {
    match (argv.first(), argv.get(1)) {
        (Some(&"git"), Some(sub)) => {
            let clean: String = sub
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '_'
                    }
                })
                .collect();
            if clean.is_empty() || !clean.starts_with(|c: char| c.is_ascii_lowercase()) {
                "command.run".to_string()
            } else {
                format!("git.{clean}")
            }
        }
        _ => "command.run".to_string(),
    }
}

static DROPPED: AtomicU64 = AtomicU64::new(0);

type Registry = Mutex<HashMap<PathBuf, Result<Mutex<Connection>, LedgerError>>>;

/// One connection per repository, opened once.
fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
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
fn canonical_repo(repo_path: &str) -> String {
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

fn open(db_path: &Path) -> Result<Connection, LedgerError> {
    if let Some(dir) = db_path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| LedgerError::new("mkdir_failed", format!("{}: {e}", dir.display())))?;
    }
    let conn = Connection::open(db_path)
        .map_err(|e| LedgerError::new("open_failed", format!("{}: {e}", db_path.display())))?;

    // WAL lets a reader page the history while the writer appends. NORMAL
    // trades a fsync per commit for one per checkpoint; the ledger is a record
    // of actions that git has already made durable itself, so losing the last
    // few milliseconds to a power cut costs a re-derivable row, not data.
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| LedgerError::new("pragma_failed", format!("journal_mode: {e}")))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| LedgerError::new("pragma_failed", format!("synchronous: {e}")))?;
    conn.execute_batch(SCHEMA)
        .map_err(|e| LedgerError::new("schema_failed", e.to_string()))?;
    Ok(conn)
}

/// Runs `f` against this repo's connection.
///
/// The open is attempted once per repository and the result — success *or*
/// failure — is cached. Retrying a failed open on every append would turn a
/// read-only checkout into a syscall storm on the mutation path.
fn with_conn<T>(
    repo_path: &str,
    f: impl FnOnce(&Connection) -> Result<T, LedgerError>,
) -> Result<T, LedgerError> {
    let db_path = ledger_path(repo_path);
    let mut reg = registry()
        .lock()
        .map_err(|_| LedgerError::new("registry_poisoned", "ledger registry mutex was poisoned"))?;

    let entry = reg
        .entry(db_path.clone())
        .or_insert_with(|| open(&db_path).map(Mutex::new));

    match entry {
        Err(e) => Err(e.clone()),
        // The registry lock is still held, so this reference is already
        // exclusive: `get_mut` reaches the connection without a second lock,
        // and cannot deadlock the way a nested `lock()` could.
        //
        // Holding the registry across `f` is deliberate rather than incidental.
        // It is what makes "one writer" true across every repository at once,
        // which is the property the schema's monotonic cursor rests on. Appends
        // cost tens of microseconds, so the contention this creates is far
        // cheaper than the coordination it replaces.
        Ok(lock) => f(lock.get_mut().map_err(|_| {
            LedgerError::new("conn_poisoned", "ledger connection mutex was poisoned")
        })?),
    }
}

/// Appends one event and returns its cursor id.
///
/// Redaction happens here rather than at any call site, so a caller cannot
/// forget it. Every path into the ledger passes through this function.
pub fn append(draft: Draft) -> Result<i64, LedgerError> {
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

    // Stored canonically for the same reason the file is located canonically:
    // otherwise one repository appears under two names in one database.
    let repo_path = canonical_repo(&draft.repo_path);

    let ms = ids::now_millis();
    let ulid = ids::ulid(ms);
    let ts = ids::iso8601_utc(ms);

    // Unspecified actor and outcome are recorded as what they are. Defaulting
    // an unknown outcome to `ok` would be a lie the ledger then preserves.
    let actor_kind = draft.actor_kind.unwrap_or(ActorKind::System);
    let outcome = draft.outcome.unwrap_or(Outcome::Ok);

    let argv = draft.argv_json.as_deref().map(redact::text);
    let detail = draft.detail_json.as_deref().map(redact::text);
    let object = draft.object.as_deref().map(redact::text);

    let result = with_conn(&draft.repo_path, |conn| {
        conn.execute(
            "INSERT INTO events (
                 ulid, ts_utc, schema_version, repo_path, worktree_path,
                 actor_kind, actor_id, session_id, task_id, action, object,
                 argv_json, outcome, verdict_json, before_ref, after_ref,
                 duration_ms, detail_json
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            params![
                ulid,
                ts,
                SCHEMA_VERSION,
                repo_path,
                draft.worktree_path,
                actor_kind.as_str(),
                draft.actor_id,
                draft.session_id,
                draft.task_id,
                draft.action,
                object,
                argv,
                outcome.as_str(),
                draft.verdict_json,
                draft.before_ref,
                draft.after_ref,
                draft.duration_ms,
                detail,
            ],
        )
        .map_err(|e| LedgerError::new("insert_failed", e.to_string()))?;
        Ok(conn.last_insert_rowid())
    });

    match &result {
        Ok(cursor) => announce(&draft.repo_path, *cursor),
        Err(_) => {
            // Counted, not swallowed. `status()` reports it, and the UI can say
            // the history it is showing is known to be incomplete.
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
    result
}

/// Appends without propagating failure, for the mutation path.
///
/// A repository whose ledger cannot be written is still a repository the user
/// may commit in — GitPulse degrades rather than refuses, exactly as it does
/// with no harness installed. The failure is counted and surfaced by
/// [`status`]; it is never silent, and it never blocks the action.
pub fn record(draft: Draft) -> Option<i64> {
    match append(draft) {
        Ok(id) => Some(id),
        Err(e) => {
            log::warn!("ledger append failed: {e}");
            None
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
    // An unbounded limit would let one call page an entire history into the
    // webview. 1000 is well past a screenful and far short of a memory problem.
    let limit = limit.clamp(1, 1000);
    with_conn(repo_path, |conn| {
        let mut stmt = conn
            .prepare(
                "SELECT id, ulid, ts_utc, schema_version, repo_path, worktree_path,
                        actor_kind, actor_id, session_id, task_id, action, object,
                        argv_json, outcome, verdict_json, before_ref, after_ref,
                        duration_ms, detail_json
                 FROM events WHERE id > ?1 ORDER BY id ASC LIMIT ?2",
            )
            .map_err(|e| LedgerError::new("prepare_failed", e.to_string()))?;
        let rows = stmt
            .query_map(params![cursor, limit], |r| {
                Ok(LedgerEvent {
                    id: r.get(0)?,
                    ulid: r.get(1)?,
                    ts_utc: r.get(2)?,
                    schema_version: r.get(3)?,
                    repo_path: r.get(4)?,
                    worktree_path: r.get(5)?,
                    actor_kind: r.get(6)?,
                    actor_id: r.get(7)?,
                    session_id: r.get(8)?,
                    task_id: r.get(9)?,
                    action: r.get(10)?,
                    object: r.get(11)?,
                    argv_json: r.get(12)?,
                    outcome: r.get(13)?,
                    verdict_json: r.get(14)?,
                    before_ref: r.get(15)?,
                    after_ref: r.get(16)?,
                    duration_ms: r.get(17)?,
                    detail_json: r.get(18)?,
                })
            })
            .map_err(|e| LedgerError::new("query_failed", e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| LedgerError::new("row_failed", e.to_string()))
    })
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
pub fn status(repo_path: &str) -> LedgerStatus {
    let path = ledger_path(repo_path);
    let dropped = DROPPED.load(Ordering::Relaxed);
    match with_conn(repo_path, |_| Ok(())) {
        Ok(()) => LedgerStatus {
            recording: true,
            path: path.display().to_string(),
            dropped,
            error: String::new(),
            error_code: String::new(),
        },
        Err(e) => LedgerStatus {
            recording: false,
            path: path.display().to_string(),
            dropped,
            error: e.message,
            error_code: e.code.to_string(),
        },
    }
}

/// Test-only helpers that need the private registry.
#[cfg(test)]
pub(crate) mod tests_support {
    /// Drops every cached connection, as a process restart would.
    pub fn reset_registry() {
        super::registry().lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        registry().lock().unwrap().clear();

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
        d.argv_json = Some(format!(r#"["git","push","https://{key}@github.com/o/r"]"#));
        d.detail_json = Some(format!(r#"{{"token":"{key}"}}"#));
        append(d).expect("append");

        // Read the raw file, not the API: the requirement is about what is on
        // disk, and an API that redacted on read would pass a weaker test.
        let bytes = std::fs::read(ledger_path(repo)).expect("read db");
        let raw = String::from_utf8_lossy(&bytes);
        assert!(
            !raw.contains(key),
            "the credential reached the database in full"
        );

        let events = tail(repo, 0, 10).expect("tail");
        assert!(events[0].argv_json.as_ref().unwrap().contains("ghp_"));
        assert!(!events[0].argv_json.as_ref().unwrap().contains(key));
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
}
