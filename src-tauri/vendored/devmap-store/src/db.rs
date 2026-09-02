use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use devmap_analyze::model::*;
use devmap_extract::model::*;
#[cfg(feature = "parse")]
use devmap_resolve::model::*;
use rusqlite::{params, Connection, OptionalExtension, Result, TransactionBehavior};
use std::collections::{BTreeMap, BTreeSet};

use crate::schema::{
    BUILD_HISTORY_RETENTION, BUILD_HISTORY_TABLE, CREATE_SCHEMA_V3, CURRENT_SCHEMA_VERSION,
    MIGRATION_V10_TO_V11, MIGRATION_V3_TO_V4, MIGRATION_V4_TO_V5, MIGRATION_V5_TO_V6,
    MIGRATION_V6_TO_V7, MIGRATION_V7_TO_V8, MIGRATION_V8_TO_V9, MIGRATION_V9_TO_V10,
    UNRESOLVED_TABLE,
};

const MAX_PENDING_ATTEMPTS: u32 = 5;

/// Hard ceiling for the git subprocess. `git` can stall on pathological
/// repositories, network mounts or hook misconfigurations; unbounded, it hung
/// every drain batch and CLI status behind it. On expiry the child is killed
/// and the caller gets an error — `current_git_head`'s callers already treat
/// an unavailable head as "unavailable", so a stalled git degrades honestly
/// instead of wedging the daemon.
const GIT_HEAD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

fn run_git_head_with_deadline(program: &str, root: &Path) -> anyhow::Result<String> {
    use std::io::Read;
    use std::process::Stdio;

    let mut child = std::process::Command::new(program)
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| anyhow::anyhow!("cannot spawn {program}: {error}"))?;

    // Drain both pipes on helper threads: reading them only after exit would
    // deadlock once a pipe buffer filled. Kill on deadline; the readers then
    // see EOF when the child dies.
    let mut stdout_pipe = child.stdout.take().unwrap();
    let mut stderr_pipe = child.stderr.take().unwrap();
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout_pipe.read_to_string(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr_pipe.read_to_string(&mut buf);
        buf
    });

    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() >= GIT_HEAD_DEADLINE {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!(
                        "{program} rev-parse HEAD exceeded \
                         {GIT_HEAD_DEADLINE:?} and was killed"
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => anyhow::bail!("{program} rev-parse HEAD failed: {error}"),
        }
    };
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    if !status.success() {
        anyhow::bail!(
            "git rev-parse HEAD failed for {:?}: {}",
            root,
            stderr.trim()
        );
    }
    let head = stdout.trim().to_string();
    if !(7..=64).contains(&head.len()) || !head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("git returned an invalid HEAD identity for {:?}", root);
    }
    Ok(head)
}

pub fn current_git_head(root: &Path) -> anyhow::Result<String> {
    run_git_head_with_deadline("git", root)
}

/// Fail closed: poisoned mutex is an error, never a panic.
fn lock_conn(
    mutex: &Mutex<Connection>,
) -> std::result::Result<MutexGuard<'_, Connection>, rusqlite::Error> {
    mutex
        .lock()
        .map_err(|_: PoisonError<MutexGuard<'_, Connection>>| {
            rusqlite::Error::InvalidParameterName(
                "store mutex poisoned — refusing to continue (fail-closed)".into(),
            )
        })
}

pub struct Store {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone)]
pub struct StoreStatus {
    pub db_path: String,
    pub latest_generation: Option<u32>,
    pub pending_count: usize,
    pub node_count: usize,
    pub edge_count: usize,
    pub degraded_reason: Option<String>,
    pub quarantined_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalCheckpointMode {
    Truncate,
    Passive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalCheckpointResult {
    pub mode: WalCheckpointMode,
    pub busy: i64,
    pub log_frames: i64,
    pub checkpointed_frames: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredSymbol {
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub path: String,
    pub span_start: usize,
    pub span_end: usize,
    pub is_exported: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEdge {
    pub source_file: String,
    pub target_file: String,
    pub source_symbol: String,
    pub target_symbol: String,
    pub edge_kind: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    pub path: String,
    pub language: String,
    pub content_hash: u64,
    pub parse_outcome: ParseOutcome,
    pub engine: ExtractionEngine,
}

#[derive(Debug, Clone, Default)]
pub struct GenerationWriteOpts {
    /// Files whose rows must be rewritten (differential path). Empty = full rewrite of inputs.
    pub affected_paths: Vec<String>,
    /// Files known deleted from the working tree — must not remain as live nodes (N2).
    pub deleted_paths: Vec<String>,
    /// Start of measured build work. The store samples it while persisting the
    /// history row, so the duration includes generation writes. `None` remains
    /// SQL NULL rather than becoming an ambiguous numeric zero.
    pub build_started: Option<std::time::Instant>,
    /// Absolute root the sources were read from. Node paths are stored
    /// repo-relative, so without this a query process resolves them against its
    /// own working directory and every span read from elsewhere comes back
    /// empty. `None` stays NULL — "root unknown", never a wrong root.
    pub repo_root: Option<String>,
}

/// One committed build, as recorded by [`Store::build_history`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildHistoryRow {
    pub generation_id: u32,
    pub built_at: i64,
    pub head_sha: String,
    pub files: u64,
    pub symbols: u64,
    pub edges: u64,
    pub dead_confident: u64,
    pub dead_ambiguous: u64,
    pub parse_failed: u64,
    pub languages_covered: u64,
    pub build_ms: Option<u64>,
    pub db_bytes: u64,
}

impl Store {
    fn configure_connection(conn: &Connection) -> Result<()> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    /// Put the database into WAL mode, tolerating a concurrent opener (SC28).
    ///
    /// Changing the journal mode needs an exclusive lock, and SQLite returns
    /// `SQLITE_BUSY` for it **without consulting the busy handler** — so the
    /// 5-second `busy_timeout` configured above does not cover this one
    /// statement. Several processes opening a brand-new store at once is
    /// exactly when that happens, and it surfaced as a bare "database is
    /// locked" from four racing builds.
    ///
    /// Losing the race is not an error: the winner sets WAL for everyone. So a
    /// busy result re-reads the mode, and succeeds if the database is already
    /// where it needs to be. Retries are bounded and the final failure is
    /// propagated — falling back to journal mode silently would leave readers
    /// blocking on every write, which is a performance cliff nobody would
    /// attribute to this.
    fn enable_wal(conn: &Connection) -> Result<()> {
        const ATTEMPTS: usize = 10;
        let mut last: Option<rusqlite::Error> = None;
        for attempt in 0..ATTEMPTS {
            match conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0)) {
                Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
                Ok(mode) => {
                    last = Some(rusqlite::Error::InvalidParameterName(format!(
                        "journal_mode is {mode}, not wal"
                    )));
                }
                Err(error) => last = Some(error),
            }
            // Another connection may have set it already while this one lost
            // the lock race.
            if let Ok(mode) =
                conn.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            {
                if mode.eq_ignore_ascii_case("wal") {
                    return Ok(());
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20 * (attempt as u64 + 1)));
        }
        Err(last.unwrap_or_else(|| {
            rusqlite::Error::InvalidParameterName("could not enable WAL mode".to_string())
        }))
    }

    fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
        let mut names = stmt.query_map([], |row| row.get::<_, String>(1))?;
        names.try_fold(false, |found, name| Ok(found || name? == column))
    }

    fn validate_schema(conn: &Connection) -> Result<()> {
        const REQUIRED: &[(&str, &[&str])] = &[
            ("paths", &["id", "path"]),
            (
                "generations",
                &["id", "created_at", "head_sha", "analysis_json", "repo_root"],
            ),
            (
                "generation_nodes",
                &[
                    "generation_id",
                    "ordinal",
                    "file_id",
                    "name",
                    "qualified_name",
                    "kind",
                    "span_start",
                    "span_end",
                    "is_exported",
                ],
            ),
            (
                "generation_files",
                &[
                    "generation_id",
                    "file_id",
                    "language",
                    "content_hash",
                    "parse_outcome_json",
                    "engine_json",
                    "extraction_json",
                ],
            ),
            (
                "generation_edges",
                &[
                    "generation_id",
                    "ordinal",
                    "source_file_id",
                    "target_file_id",
                    "source_symbol",
                    "target_symbol",
                    "edge_kind",
                    "confidence",
                ],
            ),
            (
                "generation_unresolved",
                &[
                    "generation_id",
                    "ordinal",
                    "source_file",
                    "source_symbol",
                    "callee_name",
                    "reason",
                ],
            ),
            (
                "generation_dead_symbols",
                &[
                    "generation_id",
                    "ordinal",
                    "file_path",
                    "symbol_name",
                    "confidence",
                    "is_exempt",
                    "exemption_reason",
                ],
            ),
            ("nodes_fts", &["name", "qualified_name", "path"]),
            ("nodes_fts_map", &["rowid_ref", "generation_id"]),
            ("pending_paths", &["path", "queued_at", "attempts"]),
            (
                "extraction_cache",
                &[
                    "content_hash",
                    "language",
                    "grammar_version",
                    "analyzer_version",
                    "payload_json",
                    "accessed_at",
                ],
            ),
            (
                "extraction_retry",
                &[
                    "content_hash",
                    "language",
                    "attempts",
                    "last_reason",
                    "updated_at",
                ],
            ),
            (
                "build_history",
                &[
                    "generation_id",
                    "built_at",
                    "head_sha",
                    "files",
                    "symbols",
                    "edges",
                    "dead_confident",
                    "dead_ambiguous",
                    "parse_failed",
                    "languages_covered",
                    "build_ms",
                    "db_bytes",
                ],
            ),
        ];

        for (table, required_columns) in REQUIRED {
            let object_type: Option<String> = conn
                .query_row(
                    "SELECT type FROM sqlite_master WHERE name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .optional()?;
            if object_type.as_deref() != Some("table") {
                return Err(rusqlite::Error::InvalidParameterName(format!(
                    "required schema object {table:?} is not a table"
                )));
            }

            let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
            let columns: std::collections::BTreeSet<String> = stmt
                .query_map([], |row| row.get(1))?
                .collect::<Result<_>>()?;
            for column in *required_columns {
                if !columns.contains(*column) {
                    return Err(rusqlite::Error::InvalidParameterName(format!(
                        "required column {table}.{column} is missing"
                    )));
                }
            }
        }
        Ok(())
    }

    fn migrate(conn: &mut Connection) -> Result<()> {
        let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > CURRENT_SCHEMA_VERSION {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "unsupported future schema version {version}"
            )));
        }
        let mut version = version;
        if version == 0 {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // SC28: re-read inside the write lock. `version` was sampled before
            // the transaction, so two processes racing to create the same store
            // both observed 0 — the second then reached the unconditional
            // `ADD COLUMN repo_root` below and died with "duplicate column
            // name". `Immediate` serialises the writers but does not make a
            // stale read current, and a fresh store is exactly when a daemon,
            // an editor hook and a manual build are most likely to collide.
            let observed: i32 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            if observed == 0 {
                tx.execute_batch(CREATE_SCHEMA_V3)?;
                tx.execute_batch(BUILD_HISTORY_TABLE)?;
                // Probed rather than unconditional, for the same reason the
                // v6→v7 step probes: `ADD COLUMN` is not idempotent, so a
                // partially-created store must not make this fatal.
                if !Self::has_column(&tx, "generations", "repo_root")? {
                    tx.execute_batch(MIGRATION_V6_TO_V7)?;
                }
                // A fresh database stamps CURRENT_SCHEMA_VERSION directly and
                // never runs the migration chain, so every table added by a
                // later migration must also be created here.
                tx.execute_batch(UNRESOLVED_TABLE)?;
                Self::validate_schema(&tx)?;
                tx.execute(
                    &format!("PRAGMA user_version = {}", CURRENT_SCHEMA_VERSION),
                    [],
                )?;
                tx.commit()?;
                return Ok(());
            }
            // Another process created the schema while this one waited for the
            // write lock. Continue down the chain from what it actually left,
            // rather than from the stale zero.
            tx.rollback()?;
            if observed > CURRENT_SCHEMA_VERSION {
                return Err(rusqlite::Error::InvalidParameterName(format!(
                    "unsupported future schema version {observed}"
                )));
            }
            version = observed;
        }
        if version == 3 {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(CREATE_SCHEMA_V3)?;
            tx.execute_batch(MIGRATION_V3_TO_V4)?;
            tx.execute("PRAGMA user_version = 4", [])?;
            tx.commit()?;
            version = 4;
        }
        if version == 4 {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(CREATE_SCHEMA_V3)?;
            tx.execute_batch(MIGRATION_V4_TO_V5)?;
            let has_analysis_json = {
                let mut stmt = tx.prepare("PRAGMA table_info(generations)")?;
                let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
                let mut found = false;
                for column in columns {
                    if column? == "analysis_json" {
                        found = true;
                        break;
                    }
                }
                found
            };
            if !has_analysis_json {
                tx.execute(
                    "ALTER TABLE generations ADD COLUMN analysis_json TEXT NOT NULL
                     DEFAULT '{\"total_files\":0,\"total_symbols\":0,\"total_edges\":0,\"dead_symbols\":[],\"communities\":[],\"status\":\"Ok\"}'",
                    [],
                )?;
            }
            // Stamp exactly 5, never `CURRENT_SCHEMA_VERSION`. Stamping the
            // moving target would mark this database as carrying every later
            // migration's tables while creating none of them.
            tx.execute("PRAGMA user_version = 5", [])?;
            tx.commit()?;
            version = 5;
        }
        if version == 5 {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(MIGRATION_V5_TO_V6)?;
            tx.execute("PRAGMA user_version = 6", [])?;
            // No validation mid-chain: `validate_schema` asserts the *current*
            // schema, which a v6 database legitimately does not satisfy yet.
            // The end-of-migration check below is the authoritative gate.
            tx.commit()?;
            version = 6;
        }
        if version == 6 {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // `ADD COLUMN` is not idempotent, and a database can reach this step
            // already carrying the column (a re-stamped user_version, or a fresh
            // create that applied the current schema before migrating). Probe
            // first so re-running the step is safe rather than fatal.
            if !Self::has_column(&tx, "generations", "repo_root")? {
                tx.execute_batch(MIGRATION_V6_TO_V7)?;
            }
            tx.execute("PRAGMA user_version = 7", [])?;
            // No mid-chain validation: `validate_schema` asserts the *current*
            // schema, which a v7 database legitimately does not satisfy yet.
            tx.commit()?;
            version = 7;
        }
        if version == 7 {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Same idempotency probe as v7: `ADD COLUMN` is not repeatable, and
            // a database can arrive here already carrying the columns from a
            // fresh create that applied the current schema before migrating.
            if !Self::has_column(&tx, "generation_files", "grammar_version")? {
                tx.execute_batch(MIGRATION_V7_TO_V8)?;
            }
            tx.execute("PRAGMA user_version = 8", [])?;
            // No mid-chain validation, for the same reason as v7 above:
            // `validate_schema` asserts the *current* schema, and a v8 database
            // legitimately does not satisfy it until v9 adds
            // `generation_unresolved`. The final validation below covers it.
            tx.commit()?;
            version = 8;
        }
        if version == 8 {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // `CREATE TABLE IF NOT EXISTS` is idempotent, so this needs no
            // probe — unlike the ADD COLUMN migrations above.
            tx.execute_batch(MIGRATION_V8_TO_V9)?;
            tx.execute("PRAGMA user_version = 9", [])?;
            // No mid-chain validation: `validate_schema` asserts the *current*
            // schema, and a v9 database legitimately lacks the v10
            // `classification` column until the next step adds it.
            tx.commit()?;
            version = 9;
        }
        if version == 9 {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Same idempotency probe as v7/v8: `ADD COLUMN` is not repeatable,
            // and a fresh create applies the current `UNRESOLVED_TABLE`, which
            // already carries the column, before this chain runs.
            if !Self::has_column(&tx, "generation_unresolved", "classification")? {
                tx.execute_batch(MIGRATION_V9_TO_V10)?;
            }
            tx.execute("PRAGMA user_version = 10", [])?;
            // No mid-chain validation: a v10 database legitimately lacks the
            // v11 `receiver` column until the next step adds it.
            tx.commit()?;
            version = 10;
        }
        if version == 10 {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if !Self::has_column(&tx, "generation_unresolved", "receiver")? {
                tx.execute_batch(MIGRATION_V10_TO_V11)?;
            }
            tx.execute("PRAGMA user_version = 11", [])?;
            Self::validate_schema(&tx)?;
            tx.commit()?;
            version = 11;
        }
        if version != CURRENT_SCHEMA_VERSION {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "unsupported schema version {version}"
            )));
        }
        Self::validate_schema(conn)?;
        Ok(())
    }

    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let path = db_path.as_ref();
        let mut conn = Connection::open(path)?;
        Self::configure_connection(&conn)?;
        Self::enable_wal(&conn)?;
        Self::migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open a store **without creating one**, for read commands.
    ///
    /// `Store::open` uses `Connection::open`, which creates the file — so every
    /// read was also a write. `devmap status` against a repository with no
    /// store left an empty database behind, and that file is what let
    /// `DevMapClient._start_daemon` spawn `devmap serve` on the *next* call,
    /// which built a generation in the background. An identical command then
    /// failed on the first invocation and succeeded on the second: "unavailable"
    /// was a race, not a state.
    ///
    /// A read answers from what exists, or reports that nothing is there. It
    /// does not create the thing it is reading.
    pub fn open_existing<P: AsRef<Path>>(db_path: P) -> Result<Option<Self>> {
        let path = db_path.as_ref();
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(Self::open(path)?))
    }

    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        Self::configure_connection(&conn)?;
        Self::migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn get_or_create_path_id(&self, path: &str) -> Result<u32> {
        let conn = lock_conn(&self.conn)?;
        if let Some(id) = conn
            .query_row(
                "SELECT id FROM paths WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        conn.execute(
            "INSERT OR IGNORE INTO paths (path) VALUES (?1)",
            params![path],
        )?;
        conn.query_row(
            "SELECT id FROM paths WHERE path = ?1",
            params![path],
            |row| row.get(0),
        )
    }

    #[cfg(feature = "parse")]
    fn ensure_path_id(tx: &rusqlite::Transaction<'_>, path: &str) -> Result<u32> {
        if let Some(id) = tx
            .query_row(
                "SELECT id FROM paths WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        tx.execute(
            "INSERT OR IGNORE INTO paths (path) VALUES (?1)",
            params![path],
        )?;
        tx.query_row(
            "SELECT id FROM paths WHERE path = ?1",
            params![path],
            |row| row.get(0),
        )
    }

    pub fn enqueue_pending_paths(&self, paths: &[String]) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let conn = lock_conn(&self.conn)?;
        let tx = conn.unchecked_transaction()?;
        for path in paths {
            tx.execute(
                "INSERT INTO pending_paths (path, queued_at, attempts) VALUES (?1, ?2, 0)
                 ON CONFLICT(path) DO UPDATE SET
                   queued_at=excluded.queued_at,
                   attempts=0",
                params![path, now],
            )?;
        }
        tx.commit()
    }

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
             ORDER BY queued_at ASC, path ASC
             LIMIT ?1",
        )?;
        let sqlite_limit = limit.min(i64::MAX as usize) as i64;
        let rows = stmt.query_map(params![sqlite_limit, MAX_PENDING_ATTEMPTS], |row| {
            row.get(0)
        })?;
        let mut paths = Vec::new();
        for r in rows {
            paths.push(r?);
        }
        Ok(paths)
    }

    pub fn bump_pending_attempts(&self, paths: &[String]) -> Result<()> {
        let conn = lock_conn(&self.conn)?;
        let tx = conn.unchecked_transaction()?;
        for path in paths {
            tx.execute(
                "UPDATE pending_paths SET attempts = attempts + 1 WHERE path = ?1",
                params![path],
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

    /// Acknowledge work claimed by a daemon attempt without deleting a newer
    /// watcher event. Re-enqueueing a path resets `attempts` to zero, while a
    /// claimed item has a positive attempt count; therefore a concurrent
    /// requeue remains pending for the next generation.
    pub fn clear_pending_paths_after_attempt(&self, paths: &[String]) -> Result<()> {
        let conn = lock_conn(&self.conn)?;
        let tx = conn.unchecked_transaction()?;
        for path in paths {
            tx.execute(
                "DELETE FROM pending_paths WHERE path = ?1 AND attempts > 0",
                params![path],
            )?;
        }
        tx.commit()
    }

    /// Generation-scoped FTS rowid: high 32 bits = generation, low 32 = ordinal.
    fn fts_rowid(gen_id: u32, node_ord: u32) -> i64 {
        ((gen_id as i64) << 32) | (node_ord as i64)
    }

    /// Writes a generation. Part of the build path, so it needs the parsing
    /// frontend's grammar-identity stamps and is gated with it.
    #[cfg(feature = "parse")]
    pub fn save_generation(
        &self,
        extractions: &[Extraction],
        resolution: &ResolutionResult,
        analysis: &AnalysisSummary,
    ) -> Result<u32> {
        self.save_generation_with_opts(
            extractions,
            resolution,
            analysis,
            GenerationWriteOpts::default(),
        )
    }

    /// Differential membership write with deletion reconciliation (B3 + N2).
    ///
    /// Steps:
    /// 1. Carry forward prior-generation rows whose source file is not in affected∪deleted
    /// 2. Insert freshly resolved rows for affected (from `extractions`)
    /// 3. Deleted paths contribute zero rows (explicit absence — N2)
    #[cfg(feature = "parse")]
    pub fn save_generation_with_opts(
        &self,
        extractions: &[Extraction],
        resolution: &ResolutionResult,
        analysis: &AnalysisSummary,
        opts: GenerationWriteOpts,
    ) -> Result<u32> {
        self.save_generation_with_metadata(extractions, resolution, analysis, opts, "unknown")
    }

    #[cfg(feature = "parse")]
    pub fn save_generation_with_metadata(
        &self,
        extractions: &[Extraction],
        resolution: &ResolutionResult,
        analysis: &AnalysisSummary,
        opts: GenerationWriteOpts,
        head_sha: &str,
    ) -> Result<u32> {
        if head_sha.is_empty() || head_sha.len() > 128 || head_sha.chars().any(char::is_whitespace)
        {
            return Err(rusqlite::Error::InvalidParameterName(
                "head_sha must be non-empty, whitespace-free, and at most 128 characters".into(),
            ));
        }
        let mut unique_paths = std::collections::BTreeSet::new();
        for extraction in extractions {
            if !unique_paths.insert(extraction.file_path.as_str()) {
                return Err(rusqlite::Error::InvalidParameterName(format!(
                    "duplicate extraction path in generation input: {}",
                    extraction.file_path
                )));
            }
        }
        let mut conn = lock_conn(&self.conn)?;
        let tx = conn.transaction()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let durable_analysis = analysis.clone();
        // Keep the summary semantically complete even though dead rows also
        // have a normalized table. An authoritative-looking empty list makes
        // latest_analysis() disagree with latest_dead_symbols().
        let analysis_json = serde_json::to_string(&durable_analysis).map_err(|error| {
            rusqlite::Error::InvalidParameterName(format!("analysis serialization failed: {error}"))
        })?;

        tx.execute(
            "INSERT INTO generations (created_at, head_sha, analysis_json, repo_root)
             VALUES (?1, ?2, ?3, ?4)",
            params![now, head_sha, analysis_json, opts.repo_root],
        )?;
        let gen_id: u32 = tx.last_insert_rowid() as u32;

        let prev_gen: Option<u32> = tx
            .query_row(
                "SELECT id FROM generations WHERE id < ?1 ORDER BY id DESC LIMIT 1",
                params![gen_id],
                |row| row.get(0),
            )
            .optional()?;

        let affected: std::collections::HashSet<String> =
            opts.affected_paths.iter().cloned().collect();
        let deleted: std::collections::HashSet<String> =
            opts.deleted_paths.iter().cloned().collect();
        let full_rewrite = affected.is_empty() && deleted.is_empty();

        // Which prior rows may be reused at all.
        //
        // "Unaffected" used to be the whole test, and unaffected meant only
        // "content hash unchanged". That is not enough to make a stored payload
        // reusable: it must also have been produced by the extractor and
        // grammar this build is running. The extraction *cache* has always
        // known that — its key carries both versions — but the generation
        // carry-forward did not, so after two schema bumps DevCouncil's store
        // still held 1,152 `extract-v23` rows under a `v25` binary, and the
        // first changed build was refused by the edge/analysis equality below
        // (65,615 stored against 65,798 analysed) with no way forward but
        // deleting the database.
        //
        // Same three fields the cache keys on, asked of the same owner, so the
        // two cannot drift: content hash, grammar version, analyzer version.
        // A NULL version is a row from before those columns existed — unknown
        // identity is not a matching identity, so it is not reused.
        let current_hashes: std::collections::HashMap<&str, i64> = extractions
            .iter()
            .map(|ext| (ext.file_path.as_str(), ext.content_hash as i64))
            .collect();
        let mut carry: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut stale_identity: Vec<String> = Vec::new();
        if let Some(prev) = prev_gen {
            if !full_rewrite {
                let mut stmt = tx.prepare(
                    "SELECT p.path, f.language, f.content_hash, f.grammar_version, f.analyzer_version
                     FROM generation_files f
                     JOIN paths p ON p.id = f.file_id
                     WHERE f.generation_id = ?1",
                )?;
                let rows = stmt.query_map(params![prev], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                })?;
                for row in rows {
                    let (path, language, content_hash, grammar, analyzer) = row?;
                    if deleted.contains(&path) || affected.contains(&path) {
                        continue;
                    }
                    let (current_grammar, current_analyzer) =
                        devmap_extract::cache::current_payload_identity(&language);
                    let identity_matches = grammar.as_deref() == Some(current_grammar.as_str())
                        && analyzer.as_deref() == Some(current_analyzer.as_str());
                    // A content hash that moved without the path being declared
                    // affected means the caller's affected set is wrong; the
                    // stored payload describes different bytes either way.
                    let content_matches = current_hashes
                        .get(path.as_str())
                        .is_none_or(|hash| *hash == content_hash);
                    if identity_matches && content_matches {
                        carry.insert(path);
                    } else {
                        stale_identity.push(path);
                    }
                }
            }
        }
        // A stale path this write cannot replace would simply vanish from the
        // generation — the file silently absent from the map rather than out of
        // date. Refused loudly instead, naming the remedy, because the callers
        // that can rebuild it (the CLI's cold-build closure, the daemon's
        // full resync) both check the identity first and never reach here.
        let unreplaceable: Vec<&String> = stale_identity
            .iter()
            .filter(|path| !current_hashes.contains_key(path.as_str()))
            .collect();
        if !unreplaceable.is_empty() {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "cannot carry forward {} file(s) whose stored payload was produced by a different \
                 extractor or grammar (for example {}); rebuild this generation from a full \
                 extraction rather than a differential write",
                unreplaceable.len(),
                unreplaceable[0]
            )));
        }

        if let Some(prev) = prev_gen {
            if !full_rewrite {
                let mut stmt = tx.prepare(
                    "SELECT p.path, f.language, f.content_hash,
                            f.parse_outcome_json, f.engine_json, f.extraction_json,
                            f.grammar_version, f.analyzer_version
                     FROM generation_files f
                     JOIN paths p ON p.id = f.file_id
                     WHERE f.generation_id = ?1",
                )?;
                let rows = stmt.query_map(params![prev], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                })?;
                for row in rows {
                    let (
                        path,
                        language,
                        content_hash,
                        parse_json,
                        engine_json,
                        extraction_json,
                        grammar_version,
                        analyzer_version,
                    ) = row?;
                    if !carry.contains(&path) {
                        continue;
                    }
                    let file_id = Self::ensure_path_id(&tx, &path)?;
                    tx.execute(
                        "INSERT INTO generation_files
                         (generation_id, file_id, language, content_hash, parse_outcome_json, engine_json, extraction_json, grammar_version, analyzer_version)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            gen_id,
                            file_id,
                            language,
                            content_hash,
                            parse_json,
                            engine_json,
                            extraction_json,
                            grammar_version,
                            analyzer_version
                        ],
                    )?;
                }
            }
        }

        for extraction in extractions {
            // Not "is it affected" but "was it carried". They differ exactly
            // when a prior payload failed the identity gate: the file is
            // unaffected, nothing was carried for it, and its fresh rows are
            // the only ones this generation will have.
            if !full_rewrite && carry.contains(&extraction.file_path) {
                continue;
            }
            if deleted.contains(&extraction.file_path) {
                continue;
            }
            // SQLite has no unsigned integer type. Preserve all 64 bits using
            // the same two's-complement representation as the extraction cache.
            let content_hash = extraction.content_hash as i64;
            let parse_json = serde_json::to_string(&extraction.parse_outcome).map_err(|error| {
                rusqlite::Error::InvalidParameterName(format!(
                    "parse outcome serialization failed for {}: {error}",
                    extraction.file_path
                ))
            })?;
            let engine_json = serde_json::to_string(&extraction.engine).map_err(|error| {
                rusqlite::Error::InvalidParameterName(format!(
                    "extraction engine serialization failed for {}: {error}",
                    extraction.file_path
                ))
            })?;
            let mut durable_extraction = extraction.for_durable_store();
            durable_extraction.source_code = None;
            let extraction_json = serde_json::to_string(&durable_extraction).map_err(|error| {
                rusqlite::Error::InvalidParameterName(format!(
                    "extraction serialization failed for {}: {error}",
                    extraction.file_path
                ))
            })?;
            let file_id = Self::ensure_path_id(&tx, &extraction.file_path)?;
            tx.execute(
                "INSERT INTO generation_files
                 (generation_id, file_id, language, content_hash, parse_outcome_json, engine_json, extraction_json, grammar_version, analyzer_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    gen_id,
                    file_id,
                    extraction.language,
                    content_hash,
                    parse_json,
                    engine_json,
                    extraction_json,
                    // Stamp the identity this payload was produced with, so the
                    // row is usable as a cache fallback without discarding the
                    // staleness guarantee the cache key exists to enforce (SC8).
                    devmap_extract::cache::CacheKey::for_extraction(extraction).grammar_version,
                    devmap_extract::cache::CacheKey::for_extraction(extraction).analyzer_version
                ],
            )?;
        }

        let mut node_ord: u32 = 0;

        // Carry forward unchanged files from previous generation (differential).
        if let Some(prev) = prev_gen {
            if !full_rewrite {
                let mut stmt = tx.prepare(
                    "SELECT p.path, n.name, n.qualified_name, n.kind, n.span_start, n.span_end, n.is_exported
                     FROM generation_nodes n
                     JOIN paths p ON p.id = n.file_id
                     WHERE n.generation_id = ?1",
                )?;
                let rows = stmt.query_map(params![prev], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                })?;
                for row in rows {
                    let (path, name, qn, kind, start, end, exported) = row?;
                    if !carry.contains(&path) {
                        continue;
                    }
                    let file_id = Self::ensure_path_id(&tx, &path)?;
                    tx.execute(
                        "INSERT INTO generation_nodes (generation_id, ordinal, file_id, name, qualified_name, kind, span_start, span_end, is_exported)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![gen_id, node_ord, file_id, name, qn, kind, start, end, exported],
                    )?;
                    let fts_rowid = Self::fts_rowid(gen_id, node_ord);
                    tx.execute(
                        "INSERT INTO nodes_fts (rowid, name, qualified_name, path) VALUES (?1, ?2, ?3, ?4)",
                        params![fts_rowid, name, qn, path],
                    )?;
                    tx.execute(
                        "INSERT INTO nodes_fts_map (rowid_ref, generation_id) VALUES (?1, ?2)",
                        params![fts_rowid, gen_id],
                    )?;
                    node_ord += 1;
                }
            }
        }

        // Insert fresh rows for every extraction whose file was not carried.
        for ext in extractions {
            if !full_rewrite && carry.contains(&ext.file_path) {
                continue;
            }
            if deleted.contains(&ext.file_path) {
                continue;
            }
            let file_id = Self::ensure_path_id(&tx, &ext.file_path)?;
            for sym in &ext.symbols {
                tx.execute(
                    "INSERT INTO generation_nodes (generation_id, ordinal, file_id, name, qualified_name, kind, span_start, span_end, is_exported)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        gen_id,
                        node_ord,
                        file_id,
                        sym.name,
                        sym.qualified_name,
                        format!("{:?}", sym.kind),
                        sym.span.start_byte,
                        sym.span.end_byte,
                        sym.is_exported as i32
                    ],
                )?;
                let fts_rowid = Self::fts_rowid(gen_id, node_ord);
                tx.execute(
                    "INSERT INTO nodes_fts (rowid, name, qualified_name, path) VALUES (?1, ?2, ?3, ?4)",
                    params![fts_rowid, sym.name, sym.qualified_name, ext.file_path],
                )?;
                tx.execute(
                    "INSERT INTO nodes_fts_map (rowid_ref, generation_id) VALUES (?1, ?2)",
                    params![fts_rowid, gen_id],
                )?;
                node_ord += 1;
            }
        }

        // Edges come from this build's resolution, always — never from the
        // previous generation.
        //
        // Carrying them forward was sound only while a changed build resolved
        // just the changed files. It no longer does: the build resolves the
        // whole tree so that the analysis means the same thing on both paths,
        // which means `resolution.edges` already holds the current, correct
        // edge for every file, unaffected ones included. Copying the prior
        // generation's rows over the top of that was not a saving — it read
        // rows and re-inserted the same number — it was only a way to keep an
        // older answer.
        //
        // And the answer did drift, in two ways the affected-set closure
        // cannot see. A payload produced by an older extractor stayed until its
        // file's bytes changed. An edge from an unchanged file into a target
        // whose *identity* moved without its name changing — a Go package
        // renamed, an import alias repointed — resolves differently today while
        // the source file itself never entered the affected set. Both showed up
        // as the same symptom: the equality below refusing the write.
        //
        // Writing every resolved edge makes that equality true by construction
        // rather than by argument. It stays below as a regression check.
        let mut edge_ord: u32 = 0;

        for edge in &resolution.edges {
            // Deleted paths are not extracted, so a resolution over the current
            // tree has no edge touching one. Kept as an explicit guard for
            // callers that pass a resolution computed before the deletion.
            if deleted.contains(&edge.source_file) || deleted.contains(&edge.target_file) {
                continue;
            }
            let src_f_id = Self::ensure_path_id(&tx, &edge.source_file)?;
            let tgt_f_id = Self::ensure_path_id(&tx, &edge.target_file)?;
            tx.execute(
                "INSERT INTO generation_edges (generation_id, ordinal, source_file_id, target_file_id, source_symbol, target_symbol, edge_kind, confidence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    gen_id,
                    edge_ord,
                    src_f_id,
                    tgt_f_id,
                    edge.source_symbol,
                    edge.target_symbol,
                    format!("{:?}", edge.edge_kind),
                    edge.confidence.persist_real()
                ],
            )?;
            edge_ord += 1;
        }

        // The analysis must have been computed over the edge set being stored.
        //
        // These two numbers come from different places: `edge_ord` counts the
        // rows this generation will hold, while `total_edges` is what the
        // analyser actually saw. Every consumer of `dead_symbols` and
        // `communities` assumes they are the same set. They once were not. A
        // build that resolved only the changed files handed the analyser 63 of
        // 15,017 edges and committed a generation with 433 dead-code candidates
        // instead of 14; the graph was intact and only the analysis of it was
        // wrong, so nothing failed and `devmap dead` reported plainly-called
        // symbols as callerless.
        //
        // Now that every resolved edge is stored, agreement is structural: both
        // sides count the same `resolution.edges`. The check stays because it
        // costs one comparison and it is the thing that caught the carry-forward
        // drift — a generation whose stored edges came from an older extractor
        // than its analysis. It should now be unfailable; if it ever fires
        // again, a *new* asymmetry has been introduced between what this
        // function stores and what the caller analysed.
        //
        // Deletions are covered too, rather than exempted. The worry was that
        // `--deleted` drops rows the analyser had counted, but it cannot: a
        // deleted file is not extracted, so a resolution over the current tree
        // has no edge touching it, and the carried-forward rows that did are
        // dropped on both sides of this equality. Checked as well as argued —
        // 30 randomised deletion builds (6–25 files, up to a third removed)
        // held it exactly. Exempting the case would have left the watcher, the
        // most frequent writer of all, unguarded precisely when it deletes.
        if edge_ord as usize != analysis.total_edges {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "generation would store {edge_ord} edges but its analysis was computed over {}; \
                 dead-code and community results would describe a different graph than the one stored",
                analysis.total_edges
            )));
        }

        for (ordinal, dead) in analysis.dead_symbols.iter().enumerate() {
            let ordinal = u32::try_from(ordinal).map_err(|_| {
                rusqlite::Error::InvalidParameterName(
                    "dead-symbol row count exceeds SQLite generation ordinal capacity".into(),
                )
            })?;
            tx.execute(
                "INSERT INTO generation_dead_symbols
                 (generation_id, ordinal, file_path, symbol_name, confidence, is_exempt, exemption_reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    gen_id,
                    ordinal,
                    dead.file_path,
                    dead.symbol_name,
                    confidence_millis(dead.confidence) as f64 / 1000.0,
                    dead.is_exempt as i32,
                    dead.exemption_reason
                ],
            )?;
        }

        // D17: the unresolved-call ledger. Written inside the same transaction
        // as everything else, so a generation can never be observable while
        // claiming a completeness it did not record.
        // One prepared statement for the whole ledger. A repository of this size
        // produces tens of thousands of unresolved calls per generation, and
        // re-preparing the INSERT for each one cost seconds of the build — the
        // self-build gate caught it as a regression the moment this table
        // landed.
        {
            let mut insert = tx.prepare(
                "INSERT INTO generation_unresolved
                 (generation_id, ordinal, source_file, source_symbol, callee_name, reason,
                  classification, receiver)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for (ordinal, unresolved) in resolution.unresolved.iter().enumerate() {
                let ordinal = u32::try_from(ordinal).map_err(|_| {
                    rusqlite::Error::InvalidParameterName(
                        "unresolved row count exceeds SQLite generation ordinal capacity".into(),
                    )
                })?;
                insert.execute(params![
                    gen_id,
                    ordinal,
                    unresolved.source_file,
                    unresolved.source_symbol,
                    unresolved.callee_name,
                    format!("{:?}", unresolved.resolution),
                    unresolved.class.label(),
                    unresolved.receiver.as_deref(),
                ])?;
            }
        }

        // The history row is written inside the generation's own transaction.
        // A build is therefore never observable without its history entry, and
        // a rolled-back generation leaves no phantom row behind.
        let symbols: i64 = tx.query_row(
            "SELECT COUNT(*) FROM generation_nodes WHERE generation_id = ?1",
            params![gen_id],
            |row| row.get(0),
        )?;
        let edges: i64 = tx.query_row(
            "SELECT COUNT(*) FROM generation_edges WHERE generation_id = ?1",
            params![gen_id],
            |row| row.get(0),
        )?;
        let files: i64 = tx.query_row(
            "SELECT COUNT(*) FROM generation_files WHERE generation_id = ?1",
            params![gen_id],
            |row| row.get(0),
        )?;
        // "Confident" and "ambiguous" are the two tiers a reader acts on:
        // an exempt symbol is one liveness could not rule out, so counting it
        // as confidently dead is exactly the dishonesty D6 removed.
        let is_ambiguous = |dead: &&DeadSymbolReport| {
            dead.exemption_reason.as_deref() == Some("only_ambiguous_callers")
        };
        let dead_confident = durable_analysis
            .dead_symbols
            .iter()
            .filter(|dead| !dead.is_exempt && !is_ambiguous(dead))
            .count() as i64;
        let dead_ambiguous = durable_analysis
            .dead_symbols
            .iter()
            .filter(is_ambiguous)
            .count() as i64;
        let parse_failed = extractions
            .iter()
            .filter(|extraction| matches!(extraction.parse_outcome, ParseOutcome::Failed { .. }))
            .count() as i64;
        let languages_covered = extractions
            .iter()
            .map(|extraction| extraction.language.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len() as i64;
        let page_count: i64 = tx.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let page_size: i64 = tx.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        let build_ms = opts
            .build_started
            .map(|started| i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX));

        tx.execute(
            "INSERT OR REPLACE INTO build_history
             (generation_id, built_at, head_sha, files, symbols, edges,
              dead_confident, dead_ambiguous, parse_failed, languages_covered,
              build_ms, db_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                gen_id,
                now,
                head_sha,
                files,
                symbols,
                edges,
                dead_confident,
                dead_ambiguous,
                parse_failed,
                languages_covered,
                build_ms,
                page_count.saturating_mul(page_size),
            ],
        )?;
        // Retention is by row count, not by surviving generation: history must
        // outlive the graphs it describes or it cannot show a trend.
        tx.execute(
            "DELETE FROM build_history WHERE generation_id NOT IN
             (SELECT generation_id FROM build_history ORDER BY built_at DESC, generation_id DESC LIMIT ?1)",
            params![BUILD_HISTORY_RETENTION as i64],
        )?;

        tx.commit()?;
        Ok(gen_id)
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

    pub fn latest_generation_head(&self) -> Result<Option<String>> {
        let conn = lock_conn(&self.conn)?;
        conn.query_row(
            "SELECT head_sha FROM generations ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
    }

    /// Absolute root the newest generation was built from, when recorded.
    /// D17: unresolved calls recorded for the latest generation.
    ///
    /// This is the honest denominator for graph completeness — a symbol with no
    /// callers is a different claim depending on whether anything failed to
    /// resolve against it.
    pub fn latest_unresolved(&self, limit: usize) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT source_symbol, callee_name, reason
             FROM generation_unresolved
             WHERE generation_id = (SELECT max(id) FROM generations)
             ORDER BY ordinal
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Total unresolved rows across every retained generation. Test-facing:
    /// the point is to prove the table is pruned, not just written.
    pub fn count_unresolved_rows(&self) -> Result<usize> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM generation_unresolved", [], |row| {
                row.get(0)
            })?;
        Ok(count as usize)
    }

    /// Whether every row in the latest generation was produced by the extractor
    /// and grammars this build is running.
    ///
    /// A build asks this *before* deciding to go differential. The extraction
    /// cache re-extracts a file whose analyzer or grammar version moved, but a
    /// generation used to carry its stored rows forward on content hash alone,
    /// so an upgraded kernel kept committing generations made of old payloads
    /// until a changed file finally made the stored edges disagree with the
    /// fresh analysis — at which point every incremental build failed and the
    /// only way out was deleting the database. Answering false here turns that
    /// into one full build.
    ///
    /// True when there is no generation yet: a cold build carries nothing.
    /// Whether the stored payload was produced by the current grammars.
    /// A build-path question: it compares against grammar identities only
    /// the parsing frontend can supply.
    #[cfg(feature = "parse")]
    pub fn latest_generation_payload_is_current(&self) -> Result<bool> {
        let conn = lock_conn(&self.conn)?;
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

    /// `(path, content_hash)` for every file in the latest generation.
    ///
    /// Lets a build decide, before resolving anything, whether the tree it just
    /// scanned is the one already committed.
    pub fn latest_file_hashes(&self) -> Result<BTreeMap<String, u64>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
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
        let conn = self.conn.lock().expect("store mutex poisoned");
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

    /// Every edge of the latest generation, rendered for comparison.
    /// Test-facing: proving incremental output equals cold output needs the
    /// whole edge set, not a count.
    pub fn latest_edges_for_test(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
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
        let conn = lock_conn(&self.conn)?;
        let root: Option<Option<String>> = conn
            .query_row(
                "SELECT repo_root FROM generations ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(root.flatten().filter(|root| !root.is_empty()))
    }

    pub fn latest_analysis(&self) -> Result<Option<AnalysisSummary>> {
        let conn = lock_conn(&self.conn)?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT analysis_json FROM generations ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        raw.map(|json| {
            serde_json::from_str(&json).map_err(|error| {
                rusqlite::Error::InvalidParameterName(format!(
                    "stored generation analysis is invalid: {error}"
                ))
            })
        })
        .transpose()
    }

    pub fn status(&self, db_path: &str) -> Result<StoreStatus> {
        let conn = lock_conn(&self.conn)?;
        let latest: Option<u32> = conn
            .query_row(
                "SELECT id FROM generations ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let pending_count: usize =
            conn.query_row("SELECT COUNT(*) FROM pending_paths", [], |row| {
                row.get::<_, i64>(0).map(|n| n as usize)
            })?;
        let (node_count, edge_count) = if let Some(g) = latest {
            let nodes: usize = conn.query_row(
                "SELECT COUNT(*) FROM generation_nodes WHERE generation_id = ?1",
                params![g],
                |row| row.get::<_, i64>(0).map(|n| n as usize),
            )?;
            let edges: usize = conn.query_row(
                "SELECT COUNT(*) FROM generation_edges WHERE generation_id = ?1",
                params![g],
                |row| row.get::<_, i64>(0).map(|n| n as usize),
            )?;
            (nodes, edges)
        } else {
            (0, 0)
        };
        let quarantined_count: usize = conn.query_row(
            "SELECT COUNT(*) FROM pending_paths WHERE attempts >= ?1",
            params![MAX_PENDING_ATTEMPTS],
            |row| row.get::<_, i64>(0).map(|count| count as usize),
        )?;
        Ok(StoreStatus {
            db_path: db_path.to_string(),
            latest_generation: latest,
            pending_count,
            node_count,
            edge_count,
            degraded_reason: if quarantined_count > 0 {
                Some(format!(
                    "{quarantined_count} path(s) exceeded the retry threshold"
                ))
            } else {
                None
            },
            quarantined_count,
        })
    }

    pub fn search_fts(&self, query: &str, limit: usize) -> Result<Vec<(String, String, String)>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let conn = lock_conn(&self.conn)?;
        let gen = match Store::latest_generation_id_locked(&conn)? {
            Some(g) => g,
            None => return Ok(vec![]),
        };
        let mut stmt = conn.prepare(
            "SELECT name, qualified_name, path
             FROM nodes_fts
             WHERE rowid IN (SELECT rowid_ref FROM nodes_fts_map WHERE generation_id = ?1)
               AND nodes_fts MATCH ?2
             ORDER BY rowid
             LIMIT ?3",
        )?;
        // Treat the complete user input as a quoted prefix phrase. Doubling
        // internal quotes is the FTS5 escape, so operators, column filters,
        // parentheses, wildcards, and hyphens remain data rather than syntax.
        let escaped_query = query.replace('"', "\"\"");
        let match_q = format!("\"{}\"*", escaped_query);
        let sqlite_limit = limit.min(i64::MAX as usize) as i64;
        let rows = stmt.query_map(params![gen, match_q, sqlite_limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Search only the latest persisted generation. This never reads or parses
    /// the source tree, so callers cannot accidentally turn a query into a build.
    pub fn search_symbols(&self, query: &str, limit: usize) -> Result<Vec<StoredSymbol>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let conn = lock_conn(&self.conn)?;
        let gen = match Self::latest_generation_id_locked(&conn)? {
            Some(generation) => generation,
            None => return Ok(Vec::new()),
        };
        let escaped_query = query.replace('"', "\"\"");
        let match_query = format!("\"{}\"*", escaped_query);
        let sqlite_limit = limit.min(i64::MAX as usize) as i64;
        let mut stmt = conn.prepare(
            "SELECT n.name, n.qualified_name, n.kind, p.path,
                    n.span_start, n.span_end, n.is_exported
             FROM nodes_fts
             CROSS JOIN nodes_fts_map m ON m.rowid_ref = nodes_fts.rowid
             JOIN generation_nodes n
               ON n.generation_id = m.generation_id
              AND n.ordinal = (nodes_fts.rowid & 4294967295)
             JOIN paths p ON p.id = n.file_id
             WHERE m.generation_id = ?1 AND nodes_fts MATCH ?2
             ORDER BY bm25(nodes_fts), p.path, n.name, n.span_start
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![gen, match_query, sqlite_limit], |row| {
            let start: i64 = row.get(4)?;
            let end: i64 = row.get(5)?;
            if start < 0 || end < start {
                return Err(rusqlite::Error::IntegralValueOutOfRange(4, start));
            }
            Ok(StoredSymbol {
                name: row.get(0)?,
                qualified_name: row.get(1)?,
                kind: row.get(2)?,
                path: row.get(3)?,
                span_start: start as usize,
                span_end: end as usize,
                is_exported: row.get::<_, i64>(6)? != 0,
            })
        })?;
        rows.collect()
    }

    pub fn count_search_symbols(&self, query: &str) -> Result<u32> {
        if query.trim().is_empty() {
            return Ok(0);
        }
        let conn = lock_conn(&self.conn)?;
        let Some(gen) = Self::latest_generation_id_locked(&conn)? else {
            return Ok(0);
        };
        let escaped_query = query.replace('"', "\"\"");
        let match_query = format!("\"{}\"*", escaped_query);
        // CROSS JOIN pins the FTS table as the outer loop. As a plain JOIN,
        // SQLite 3.45 (the bundled version) leads with `nodes_fts_map` on
        // `generation_id` and re-scans full-text storage once per mapped row:
        // 12.7s at 200k rows, against 1.7ms for the match alone. A subquery
        // does not help because the planner flattens it. `search_symbols`
        // avoids this only by accident, via `ORDER BY bm25(...)`.
        let count: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM nodes_fts
             CROSS JOIN nodes_fts_map m
               ON m.rowid_ref = nodes_fts.rowid AND m.generation_id = ?1
             WHERE nodes_fts MATCH ?2",
            params![gen, match_query],
            |row| row.get(0),
        )?;
        u32::try_from(count).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, count))
    }

    pub fn latest_path_is_indexed(&self, path: &str) -> Result<bool> {
        let conn = lock_conn(&self.conn)?;
        let Some(gen) = Self::latest_generation_id_locked(&conn)? else {
            return Ok(false);
        };
        conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM generation_files f
                JOIN paths p ON p.id = f.file_id
                WHERE f.generation_id = ?1 AND p.path = ?2
             )",
            params![gen, path],
            |row| row.get::<_, i64>(0).map(|value| value != 0),
        )
    }

    pub fn latest_file(&self, path: &str) -> Result<Option<StoredFile>> {
        let conn = lock_conn(&self.conn)?;
        let Some(gen) = Self::latest_generation_id_locked(&conn)? else {
            return Ok(None);
        };
        let raw: Option<(String, String, i64, String, String)> = conn
            .query_row(
                "SELECT p.path, f.language, f.content_hash,
                        f.parse_outcome_json, f.engine_json
                 FROM generation_files f
                 JOIN paths p ON p.id = f.file_id
                 WHERE f.generation_id = ?1 AND p.path = ?2",
                params![gen, path],
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
        raw.map(|(path, language, content_hash, parse_json, engine_json)| {
            let parse_outcome = serde_json::from_str(&parse_json).map_err(|error| {
                rusqlite::Error::InvalidParameterName(format!(
                    "stored parse outcome for {path} is invalid: {error}"
                ))
            })?;
            let engine = serde_json::from_str(&engine_json).map_err(|error| {
                rusqlite::Error::InvalidParameterName(format!(
                    "stored extraction engine for {path} is invalid: {error}"
                ))
            })?;
            Ok(StoredFile {
                path,
                language,
                content_hash: content_hash as u64,
                parse_outcome,
                engine,
            })
        })
        .transpose()
    }

    /// Load the canonical extraction payloads for the latest generation. This
    /// supports differential re-resolution without touching unchanged files.
    pub fn latest_extractions(&self) -> Result<Vec<Extraction>> {
        let conn = lock_conn(&self.conn)?;
        let Some(gen) = Self::latest_generation_id_locked(&conn)? else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare(
            "SELECT f.extraction_json, p.path
             FROM generation_files f
             JOIN paths p ON p.id = f.file_id
             WHERE f.generation_id = ?1
             ORDER BY p.path",
        )?;
        let rows = stmt.query_map(params![gen], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut extractions = Vec::new();
        for row in rows {
            let (json, path) = row?;
            let extraction = serde_json::from_str(&json).map_err(|error| {
                rusqlite::Error::InvalidParameterName(format!(
                    "stored extraction for {path} is invalid: {error}"
                ))
            })?;
            extractions.push(extraction);
        }
        Ok(extractions)
    }

    pub fn latest_edges_for_file(
        &self,
        path: &str,
        min_confidence: f32,
    ) -> Result<Vec<StoredEdge>> {
        let conn = lock_conn(&self.conn)?;
        let Some(gen) = Self::latest_generation_id_locked(&conn)? else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare(
            "SELECT sp.path, tp.path, e.source_symbol, e.target_symbol,
                    e.edge_kind, e.confidence
             FROM generation_edges e
             JOIN paths sp ON sp.id = e.source_file_id
             JOIN paths tp ON tp.id = e.target_file_id
             WHERE e.generation_id = ?1
               AND (sp.path = ?2 OR tp.path = ?2)
               AND CAST(ROUND(e.confidence * 1000) AS INTEGER) >= CAST(ROUND(?3 * 1000) AS INTEGER)
             ORDER BY e.confidence DESC, sp.path, tp.path,
                      e.source_symbol, e.target_symbol, e.edge_kind",
        )?;
        let rows = stmt.query_map(params![gen, path, min_confidence], |row| {
            Ok(StoredEdge {
                source_file: row.get(0)?,
                target_file: row.get(1)?,
                source_symbol: row.get(2)?,
                target_symbol: row.get(3)?,
                edge_kind: row.get(4)?,
                confidence: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    pub fn latest_edges(&self, min_confidence: f32) -> Result<Vec<StoredEdge>> {
        let conn = lock_conn(&self.conn)?;
        let Some(gen) = Self::latest_generation_id_locked(&conn)? else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare(
            "SELECT sp.path, tp.path, e.source_symbol, e.target_symbol,
                    e.edge_kind, e.confidence
             FROM generation_edges e
             JOIN paths sp ON sp.id = e.source_file_id
             JOIN paths tp ON tp.id = e.target_file_id
             WHERE e.generation_id = ?1 AND CAST(ROUND(e.confidence * 1000) AS INTEGER) >= CAST(ROUND(?2 * 1000) AS INTEGER)
             ORDER BY e.confidence DESC, sp.path, tp.path,
                      e.source_symbol, e.target_symbol, e.edge_kind",
        )?;
        let rows = stmt.query_map(params![gen, min_confidence], |row| {
            Ok(StoredEdge {
                source_file: row.get(0)?,
                target_file: row.get(1)?,
                source_symbol: row.get(2)?,
                target_symbol: row.get(3)?,
                edge_kind: row.get(4)?,
                confidence: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    pub fn latest_dead_symbols(&self) -> Result<Vec<DeadSymbolReport>> {
        let conn = lock_conn(&self.conn)?;
        let Some(gen) = Self::latest_generation_id_locked(&conn)? else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare(
            "SELECT symbol_name, file_path, confidence, is_exempt, exemption_reason
             FROM generation_dead_symbols
             WHERE generation_id = ?1
             ORDER BY is_exempt, confidence DESC, file_path, symbol_name, ordinal",
        )?;
        let rows = stmt.query_map(params![gen], |row| {
            Ok(DeadSymbolReport {
                symbol_name: row.get(0)?,
                file_path: row.get(1)?,
                confidence: row.get(2)?,
                is_exempt: row.get::<_, i64>(3)? != 0,
                exemption_reason: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// Count dead-symbol rows whose persisted confidence is at least `min`
    /// in milliconfidence space, so `0.9` matches HIGH rows SQLite REAL
    /// cannot round-trip from `f32`.
    pub fn count_dead_at_least(&self, min: f32) -> Result<u32> {
        let conn = lock_conn(&self.conn)?;
        let Some(gen) = Self::latest_generation_id_locked(&conn)? else {
            return Ok(0);
        };
        conn.query_row(
            "SELECT COUNT(*) FROM generation_dead_symbols
             WHERE generation_id = ?1
               AND CAST(ROUND(confidence * 1000) AS INTEGER)
                   >= CAST(ROUND(?2 * 1000) AS INTEGER)",
            params![gen, min],
            |row| row.get(0),
        )
    }

    fn latest_generation_id_locked(conn: &Connection) -> Result<Option<u32>> {
        conn.query_row(
            "SELECT id FROM generations ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
    }

    pub fn list_generation_paths(&self, generation_id: u32) -> Result<Vec<String>> {
        let conn = lock_conn(&self.conn)?;
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

    pub fn vacuum_if_needed(&self) -> Result<()> {
        let conn = lock_conn(&self.conn)?;
        let freelist_count: i64 = conn.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
        let page_count: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        if Self::should_vacuum(freelist_count, page_count) {
            conn.execute("VACUUM", [])?;
        }
        Ok(())
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
        let previous_busy_ms = u64::try_from(previous_busy_ms).map_err(|_| {
            rusqlite::Error::InvalidParameterName(
                "SQLite returned a negative busy_timeout".to_string(),
            )
        })?;

        // TRUNCATE honors busy_timeout and could otherwise monopolize the
        // store mutex for seconds while a reader holds a snapshot. Probe
        // without waiting, then use PASSIVE as the non-blocking fallback.
        conn.busy_timeout(std::time::Duration::ZERO)?;
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

    pub fn repair_fts(&self) -> Result<()> {
        let mut conn = lock_conn(&self.conn)?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM nodes_fts", [])?;
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
                tx.execute(
                    "INSERT INTO nodes_fts (rowid, name, qualified_name, path) VALUES (?1, ?2, ?3, ?4)",
                    params![fts_rowid, name, qn, path],
                )?;
                tx.execute(
                    "INSERT INTO nodes_fts_map (rowid_ref, generation_id) VALUES (?1, ?2)",
                    params![fts_rowid, g],
                )?;
            }
        }
        tx.commit()?;
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
                "DELETE FROM generation_files WHERE generation_id = ?1",
                params![old_gen],
            )?;
            tx.execute(
                "DELETE FROM generation_edges WHERE generation_id = ?1",
                params![old_gen],
            )?;
            tx.execute(
                "DELETE FROM generation_unresolved WHERE generation_id = ?1",
                params![old_gen],
            )?;
            tx.execute(
                "DELETE FROM generation_dead_symbols WHERE generation_id = ?1",
                params![old_gen],
            )?;
            tx.execute("DELETE FROM generations WHERE id = ?1", params![old_gen])?;
            pruned_count += 1;
        }

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
    /// Must run *after* `prune_generations_except_latest`, so `generation_files`
    /// already describes only retained generations.
    pub fn prune_extraction_cache(&self) -> Result<usize> {
        let mut conn = lock_conn(&self.conn)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = tx.execute(
            "DELETE FROM extraction_cache
             WHERE (content_hash, language) NOT IN
                   (SELECT content_hash, language FROM generation_files)
                OR (content_hash, language, grammar_version, analyzer_version) IN
                   (SELECT content_hash, language, grammar_version, analyzer_version
                    FROM generation_files
                    WHERE grammar_version IS NOT NULL AND analyzer_version IS NOT NULL)",
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
        let payload = match payload {
            Some(found) => Some(found),
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
                    |row| row.get(0),
                )
                .optional()?,
        };
        Ok(payload.and_then(|json| serde_json::from_str(&json).ok()))
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
        let payload = serde_json::to_string(&cached).map_err(|err| {
            rusqlite::Error::InvalidParameterName(format!("cache serialize failed: {err}"))
        })?;
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

#[cfg(test)]
mod connection_tests {
    use super::*;

    #[test]
    fn store_connections_enable_integrity_and_contention_pragmas() {
        let store = Store::open_in_memory().expect("store");
        let conn = lock_conn(&store.conn).expect("connection");
        let foreign_keys: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("foreign_keys pragma");
        let busy_timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("busy_timeout pragma");
        assert_eq!(foreign_keys, 1);
        assert!(busy_timeout >= 5_000, "busy timeout was {busy_timeout} ms");
    }
}

#[cfg(test)]
mod git_head_tests {
    use super::*;

    /// A stalled git must be killed at the deadline, not waited on forever.
    ///
    /// `current_git_head` used `.output()`, which waits however long the child
    /// feels like taking; a hung git (network mount, wedged hook) stalled every
    /// drain batch behind it. The bounded runner kills at
    /// [`GIT_HEAD_DEADLINE`]; this test proves the error arrives near the
    /// deadline rather than after the sleeper's own 30s exit.
    #[test]
    fn a_stalled_git_is_killed_at_the_deadline() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("devmap-gitdeadline-{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("stalledgit");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let started = std::time::Instant::now();
        let result =
            run_git_head_with_deadline(&script.to_string_lossy(), std::path::Path::new("/tmp"));
        let elapsed = started.elapsed();

        let error = result.expect_err("a stalled git must produce an error");
        assert!(
            error.to_string().contains("killed"),
            "the error must say the child was killed: {error}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(GIT_HEAD_DEADLINE.as_secs() + 2),
            "kill must land near the deadline, took {elapsed:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_real_git_head_still_validates_normally() {
        // Positive control: the deadline path must not have broken honest git.
        // Any directory works — /tmp is outside a repo only if git errors, so
        // use this crate's own manifest dir which IS in a repository when the
        // workspace is checked out; fall back to asserting the failure shape
        // otherwise. Either way it must return quickly and cleanly.
        let started = std::time::Instant::now();
        let result = current_git_head(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
        assert!(started.elapsed() < GIT_HEAD_DEADLINE);
        match result {
            Ok(head) => assert!(
                (7..=64).contains(&head.len()) && head.bytes().all(|b| b.is_ascii_hexdigit()),
                "a real HEAD must pass validation: {head:?}"
            ),
            Err(error) => assert!(
                !error.to_string().contains("killed"),
                "an honest fast failure must not be a kill: {error}"
            ),
        }
    }
}
