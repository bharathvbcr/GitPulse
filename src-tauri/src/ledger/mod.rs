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

pub mod bindings;
pub mod ids;
pub mod redact;

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
  detail_json    TEXT
);
CREATE INDEX IF NOT EXISTS idx_events_repo_ts  ON events(repo_path, ts_utc);
CREATE INDEX IF NOT EXISTS idx_events_task     ON events(task_id)    WHERE task_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_session  ON events(session_id) WHERE session_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS ledger_family_imports (
  source_key     TEXT PRIMARY KEY,
  source_label   TEXT NOT NULL,
  source_cursor  INTEGER NOT NULL
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

type Registry = Mutex<HashMap<PathBuf, Result<Mutex<Connection>, LedgerError>>>;
type DroppedRegistry = Mutex<HashMap<PathBuf, u64>>;

/// One connection per repository, opened once.
fn registry() -> &'static Registry {
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
    let conn = Connection::open(db_path)
        .map_err(|e| LedgerError::new("open_failed", format!("{}: {e}", db_path.display())))?;
    // Opening by pathname is the one SQLite API available without widening
    // dependencies. Re-check before the first pragma can write, then again
    // after schema/WAL setup, so a swapped path is refused at both edges.
    checked_state_directory(dir, Some(repo), false)?;
    validate_ledger_files(db_path, repo)?;

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
    checked_state_directory(dir, Some(repo), false)?;
    validate_ledger_files(db_path, repo)?;
    secure_ledger_files(db_path, repo)?;
    Ok(conn)
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

    // Redact every caller-controlled string at the one write boundary. The
    // verdict repeats the judged command in `target`, so redacting only argv
    // leaves a second full copy of the same credential in the same row.
    let stored_repo_path = redact::text(&repo_path);
    let worktree_path = draft.worktree_path.as_deref().map(redact::text);
    let actor_id = draft.actor_id.as_deref().map(redact::text);
    let session_id = draft.session_id.as_deref().map(redact::text);
    let task_id = draft.task_id.as_deref().map(redact::text);
    let action = redact::text(&draft.action);
    let object = draft.object.as_deref().map(redact::text);
    let argv = draft.argv_json.as_deref().map(redact::text);
    let verdict = draft.verdict_json.as_deref().map(redact::text);
    let before_ref = draft.before_ref.as_deref().map(redact::text);
    let after_ref = draft.after_ref.as_deref().map(redact::text);
    let detail = draft.detail_json.as_deref().map(redact::text);

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
                stored_repo_path,
                worktree_path,
                actor_kind.as_str(),
                actor_id,
                session_id,
                task_id,
                action,
                object,
                argv,
                outcome.as_str(),
                verdict,
                before_ref,
                after_ref,
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
            record_dropped_append(&draft.repo_path);
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
    let canonical_repo_path = canonical_repo(repo_path);
    let stored_repo_path = redact::text(&canonical_repo_path);
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
            .query_map(params![cursor, limit], event_from_row)
            .map_err(|e| LedgerError::new("query_failed", e.to_string()))?;
        let mut events = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| LedgerError::new("row_failed", e.to_string()))?;
        for event in &mut events {
            if event.repo_path != canonical_repo_path && event.repo_path != stored_repo_path {
                return Err(LedgerError::new(
                    "repository_mismatch",
                    "the ledger contains an event attributed to a different repository",
                ));
            }
            // The caller already supplied this repository identity. Returning
            // that identity keeps a redacted on-disk spelling from becoming a
            // second, unmatched repository in the frontend projection without
            // ever persisting the unredacted spelling in this row.
            event.repo_path.clone_from(&canonical_repo_path);
        }
        Ok(events)
    })
}

fn valid_ulid(value: &str) -> bool {
    value.len() == 26
        && value
            .bytes()
            .all(|byte| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&byte))
}

fn valid_utc_timestamp(value: &str) -> bool {
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
    event.object = event.object.as_deref().map(redact::text);
    event.argv_json = event.argv_json.as_deref().map(redact::text);
    event.verdict_json = event.verdict_json.as_deref().map(redact::text);
    event.before_ref = event.before_ref.as_deref().map(redact::text);
    event.after_ref = event.after_ref.as_deref().map(redact::text);
    event.detail_json = event.detail_json.as_deref().map(redact::text);
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

fn family_import_cursor(anchor: &str, source_key: &str) -> Result<i64, LedgerError> {
    with_conn(anchor, |conn| {
        conn.query_row(
            "SELECT source_cursor FROM ledger_family_imports WHERE source_key = ?1",
            params![source_key],
            |row| row.get(0),
        )
        .optional()
        .map(|cursor| cursor.unwrap_or(0))
        .map_err(|error| LedgerError::new("legacy_marker_failed", error.to_string()))
    })
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
        let transaction = conn
            .unchecked_transaction()
            .map_err(|error| LedgerError::new("legacy_import_failed", error.to_string()))?;
        let mut imported = 0usize;
        let mut newest_cursor = None;

        for event in page {
            let changed = transaction
                .execute(
                    "INSERT INTO events (
                         ulid, ts_utc, schema_version, repo_path, worktree_path,
                         actor_kind, actor_id, session_id, task_id, action, object,
                         argv_json, outcome, verdict_json, before_ref, after_ref,
                         duration_ms, detail_json
                     ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
                     ON CONFLICT(ulid) DO NOTHING",
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
            if existing
                .as_ref()
                .is_none_or(|existing| !same_event_payload(existing, event))
            {
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
/// A per-source cursor keyed by Git's opaque object digest makes steady-state
/// checks cheap without persisting a credential-shaped path or collapsing two
/// redaction-equivalent names. The unique ULID plus a payload comparison makes
/// retries idempotent without hiding collisions.
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

    for source in members {
        if source == &anchor_path {
            continue;
        }
        let source_db = ledger_path(&source.to_string_lossy());
        if checked_state_file_metadata(&source_db, Some(source))?.is_none() {
            continue;
        }

        let source_name = source.to_string_lossy().into_owned();
        let source_key = family_source_key(&anchor_path, &source_name)?;
        let mut cursor = family_import_cursor(&anchor, &source_key)?;
        let source_conn = Connection::open_with_flags(&source_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| {
                LedgerError::new(
                    "legacy_read_failed",
                    format!("could not open {}: {error}", source_db.display()),
                )
            })?;
        checked_state_file_metadata(&source_db, Some(source))?.ok_or_else(|| {
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
                .map(|event| normalize_legacy_event(event, &anchor_path, source, members))
                .collect::<Result<Vec<_>, _>>()?;
            let (imported, newest_cursor) =
                import_legacy_page(&anchor, &source_key, &source_name, &normalized, cursor)?;
            imported_total += imported;
            if let Some(newest_cursor) = newest_cursor {
                announce(&anchor, newest_cursor);
            }
        }
    }

    Ok(imported_total)
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
    let dropped = dropped_appends(repo_path);
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
    })
}

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
        conn.execute(
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
        Ok(())
    })
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
    }
    Ok(found)
}

/// Test-only helpers that need the private registry.
#[cfg(test)]
pub(crate) mod tests_support {
    /// Drops every cached connection, as a process restart would.
    pub fn reset_registry() {
        super::registry().lock().unwrap().clear();
        super::dropped_registry().lock().unwrap().clear();
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

    #[cfg(unix)]
    fn real_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(dir.path())
            .status()
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
        registry().lock().unwrap().clear();

        // Not an error: a missing table is "nothing was ever recorded", and a
        // read-only connection could not create it anyway.
        assert!(read_fleet_metrics(repo).expect("read").is_none());
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
        assert!(
            !raw.contains(key),
            "the credential reached the database in full"
        );

        let events = tail(repo, 0, 10).expect("tail");
        assert!(events[0].argv_json.as_ref().unwrap().contains("ghp_"));
        assert!(!events[0].argv_json.as_ref().unwrap().contains(key));
        let event_json = serde_json::to_string(&events[0]).expect("serialize event");
        assert!(
            event_json.contains("ghp_"),
            "credential shape stays identifiable"
        );
        assert!(
            !event_json.contains(key),
            "a credential survived in a non-argv ledger field: {event_json}"
        );
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
        for secret in [auth, cookie, password, cli_password, private_key] {
            assert!(
                !raw.contains(secret),
                "contextual secret reached disk: {secret}"
            );
        }

        let event = tail(repo, 0, 10).expect("tail").pop().expect("event");
        assert!(event.object.as_deref().unwrap().contains("<redacted>"));
        for json in [event.argv_json, event.verdict_json, event.detail_json] {
            let json = json.expect("serialized field");
            serde_json::from_str::<serde_json::Value>(&json).unwrap_or_else(|error| {
                panic!("redaction corrupted serialized JSON: {error}: {json}")
            });
            assert!(
                json.contains("<redacted>"),
                "redaction marker missing: {json}"
            );
        }
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
