//! The DevCouncil schema this crate reads and writes.
//!
//! Transcribed from a live `.devcouncil/state.sqlite` rather than from the
//! SQLModel class definitions, because what the harness has to interoperate
//! with is the file on disk, not the ORM's intent. Every statement is
//! `IF NOT EXISTS`, so opening an existing DevCouncil store leaves it exactly
//! as it was.
//!
//! `ux_task_leases_active` is the important line. It is the partial unique
//! index that makes two builders racing for one task resolve to exactly one
//! winner, and it must exist before the store is used concurrently — the
//! check-then-insert in `acquire` is a friendlier error message, not the
//! safety mechanism.
//!
//! Because the statement that creates it is `IF NOT EXISTS`, applying this DDL
//! is not evidence that the index in the file is the one written here — the
//! match is on the name alone. [`verify_exclusion_index`] reads the schema the
//! connection actually opened and refuses anything else, so "the index must
//! exist" is a checked precondition rather than an assumption.

use rusqlite::{Connection, OptionalExtension};

/// Idempotent DDL for the tables the harness touches.
pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tasks (
    id VARCHAR NOT NULL,
    title VARCHAR NOT NULL,
    description VARCHAR NOT NULL,
    requirement_ids_json VARCHAR NOT NULL DEFAULT '[]',
    acceptance_criterion_ids_json VARCHAR NOT NULL DEFAULT '[]',
    planned_files_json VARCHAR NOT NULL DEFAULT '[]',
    expected_tests_json VARCHAR NOT NULL DEFAULT '[]',
    allowed_commands_json VARCHAR NOT NULL DEFAULT '[]',
    forbidden_changes_json VARCHAR NOT NULL DEFAULT '[]',
    status VARCHAR NOT NULL DEFAULT 'planned',
    difficulty VARCHAR,
    agent_appended_expected_tests_json VARCHAR NOT NULL DEFAULT '[]',
    agent_appended_allowed_commands_json VARCHAR NOT NULL DEFAULT '[]',
    priority VARCHAR,
    agent_appended_planned_files_json VARCHAR NOT NULL DEFAULT '[]',
    PRIMARY KEY (id)
);

CREATE TABLE IF NOT EXISTS task_leases (
    id VARCHAR NOT NULL,
    task_id VARCHAR NOT NULL,
    owner VARCHAR NOT NULL,
    agent VARCHAR,
    client_id VARCHAR,
    run_id VARCHAR,
    branch VARCHAR,
    lease_token VARCHAR NOT NULL,
    status VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL,
    expires_at VARCHAR,
    released_at VARCHAR,
    PRIMARY KEY (id)
);

CREATE INDEX IF NOT EXISTS ix_task_leases_task_id ON task_leases (task_id);
CREATE INDEX IF NOT EXISTS ix_task_leases_status ON task_leases (status);

-- The mutual-exclusion primitive. Without this, two concurrent acquires both
-- pass their pre-check and both insert.
CREATE UNIQUE INDEX IF NOT EXISTS ux_task_leases_active
    ON task_leases (task_id) WHERE status = 'active';

-- schema_version is the Python SQLModel singleton. dc-store refuses to open a
-- database whose version is newer than SCHEMA_VERSION; older or missing rows
-- are upgraded additively (every statement below is IF NOT EXISTS).
CREATE TABLE IF NOT EXISTS schema_version (
    id VARCHAR NOT NULL,
    version INTEGER NOT NULL,
    PRIMARY KEY (id)
);

CREATE TABLE IF NOT EXISTS requirements (
    id VARCHAR NOT NULL,
    title VARCHAR NOT NULL,
    description VARCHAR NOT NULL,
    priority VARCHAR NOT NULL,
    source VARCHAR NOT NULL,
    acceptance_criteria_json VARCHAR NOT NULL DEFAULT '[]',
    PRIMARY KEY (id)
);

CREATE TABLE IF NOT EXISTS assumptions (
    id VARCHAR NOT NULL,
    statement VARCHAR NOT NULL,
    confidence VARCHAR NOT NULL,
    impact VARCHAR NOT NULL,
    reversible BOOLEAN NOT NULL,
    requires_user_confirmation BOOLEAN NOT NULL,
    linked_requirement_ids_json VARCHAR NOT NULL DEFAULT '[]',
    status VARCHAR NOT NULL DEFAULT 'open',
    PRIMARY KEY (id)
);

CREATE TABLE IF NOT EXISTS evidence (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    type VARCHAR NOT NULL,
    task_id VARCHAR,
    requirement_id VARCHAR,
    acceptance_criterion_id VARCHAR,
    data_json VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS gaps (
    id VARCHAR NOT NULL,
    severity VARCHAR NOT NULL,
    gap_type VARCHAR NOT NULL,
    requirement_id VARCHAR,
    task_id VARCHAR,
    description VARCHAR NOT NULL,
    evidence_json VARCHAR NOT NULL DEFAULT '[]',
    recommended_fix VARCHAR NOT NULL,
    blocking BOOLEAN NOT NULL,
    file VARCHAR,
    line INTEGER,
    suggested_command VARCHAR,
    acceptance_criterion_id VARCHAR,
    expected_verification_method VARCHAR,
    -- Added after schema 9 shipped; a store created earlier gains them in
    -- `migrate`, which adds every column this DDL defines that the table lacks.
    stdout_path VARCHAR,
    stderr_path VARCHAR,
    PRIMARY KEY (id)
);

-- How many times a task's gaps have been replaced.
--
-- `gaps` is current-state by design: `gaps_replace` deletes the task's rows and
-- writes the new set, so a reconnecting agent sees the latest run rather than a
-- union of historical findings. That is the right answer to "what blocks me
-- now" and it is the reason the one fact worth keeping across runs cannot be
-- recovered from it — a gap that was raised, reported fixed, and raised again
-- is indistinguishable there from one nobody ever addressed.
--
-- The counter is its own table rather than derived from gap_history because a
-- run that reports NO gaps still has to advance it. Deriving the run number
-- from the history's own maximum would skip those runs, and a gap that
-- disappeared for exactly one clean run and came back would then look like it
-- had never gone away.
CREATE TABLE IF NOT EXISTS gap_runs (
    task_id VARCHAR NOT NULL,
    run INTEGER NOT NULL,
    PRIMARY KEY (task_id)
);

-- One row per (task, gap id) ever seen, kept when `gaps` is cleared.
--
-- This is the negative memory: `resurfaces` counts how many times a gap went
-- away and came back, which is the highest-signal event in a task and the one
-- the current-state table cannot hold. It is a record of what was reported, not
-- of what was true — a gap can also "resurface" because a run could not measure
-- it, which is why the run numbers are kept rather than only the count.
CREATE TABLE IF NOT EXISTS gap_history (
    task_id VARCHAR NOT NULL,
    gap_id VARCHAR NOT NULL,
    gap_type VARCHAR NOT NULL,
    first_seen_run INTEGER NOT NULL,
    last_seen_run INTEGER NOT NULL,
    occurrences INTEGER NOT NULL,
    resurfaces INTEGER NOT NULL,
    PRIMARY KEY (task_id, gap_id)
);

CREATE TABLE IF NOT EXISTS critique_findings (
    id VARCHAR NOT NULL,
    source_agent VARCHAR NOT NULL,
    target_plan_id VARCHAR NOT NULL,
    severity VARCHAR NOT NULL,
    finding_type VARCHAR NOT NULL,
    claim VARCHAR NOT NULL,
    linked_requirement_id VARCHAR,
    suggested_requirement VARCHAR,
    suggested_task VARCHAR,
    falsifiable_check VARCHAR NOT NULL,
    status VARCHAR NOT NULL DEFAULT 'open',
    PRIMARY KEY (id)
);

CREATE TABLE IF NOT EXISTS project_state (
    id VARCHAR NOT NULL,
    current_phase VARCHAR NOT NULL,
    history_json VARCHAR NOT NULL DEFAULT '[]',
    PRIMARY KEY (id)
);

CREATE TABLE IF NOT EXISTS shell_sessions (
    id VARCHAR NOT NULL,
    task_id VARCHAR NOT NULL,
    lease_id VARCHAR,
    shell VARCHAR NOT NULL,
    cwd VARCHAR NOT NULL,
    status VARCHAR NOT NULL,
    started_at VARCHAR NOT NULL,
    ended_at VARCHAR,
    PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS ix_shell_sessions_task_id ON shell_sessions (task_id);

CREATE TABLE IF NOT EXISTS shell_command_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id VARCHAR NOT NULL,
    session_id VARCHAR,
    command VARCHAR NOT NULL,
    status VARCHAR NOT NULL,
    exit_code INTEGER,
    reason VARCHAR NOT NULL DEFAULT '',
    stdout_path VARCHAR NOT NULL DEFAULT '',
    stderr_path VARCHAR NOT NULL DEFAULT '',
    created_at VARCHAR NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_shell_command_events_task_id ON shell_command_events (task_id);

CREATE TABLE IF NOT EXISTS file_change_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id VARCHAR,
    lease_id VARCHAR,
    session_id VARCHAR,
    path VARCHAR NOT NULL,
    operation VARCHAR NOT NULL,
    allowed BOOLEAN NOT NULL,
    reason VARCHAR NOT NULL DEFAULT '',
    created_at VARCHAR NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_file_change_events_task_id ON file_change_events (task_id);

CREATE TABLE IF NOT EXISTS semantic_diffs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id VARCHAR NOT NULL,
    before_snapshot_path VARCHAR NOT NULL,
    after_snapshot_path VARCHAR NOT NULL,
    classifications_json VARCHAR NOT NULL DEFAULT '[]',
    summary VARCHAR NOT NULL DEFAULT '',
    created_at VARCHAR NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_semantic_diffs_task_id ON semantic_diffs (task_id);

CREATE TABLE IF NOT EXISTS agent_handoffs (
    id VARCHAR NOT NULL,
    task_id VARCHAR NOT NULL,
    from_agent VARCHAR NOT NULL,
    to_agent VARCHAR NOT NULL,
    run_id VARCHAR NOT NULL,
    manifest_path VARCHAR NOT NULL,
    status VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL,
    PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS ix_agent_handoffs_task_id ON agent_handoffs (task_id);

CREATE TABLE IF NOT EXISTS correction_manifests (
    id VARCHAR NOT NULL,
    task_id VARCHAR NOT NULL,
    run_id VARCHAR,
    manifest_path VARCHAR NOT NULL,
    retry_budget INTEGER NOT NULL DEFAULT 3,
    attempt INTEGER NOT NULL DEFAULT 0,
    status VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL,
    PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS ix_correction_manifests_task_id ON correction_manifests (task_id);

CREATE TABLE IF NOT EXISTS verification_runs (
    id VARCHAR NOT NULL,
    task_id VARCHAR NOT NULL,
    sandbox VARCHAR NOT NULL,
    environment_json VARCHAR NOT NULL DEFAULT '{}',
    commands_json VARCHAR NOT NULL DEFAULT '[]',
    status VARCHAR NOT NULL,
    started_at VARCHAR NOT NULL,
    finished_at VARCHAR,
    PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS ix_verification_runs_task_id ON verification_runs (task_id);
"#;

/// The name of the partial unique index the lease's mutual exclusion rests on.
pub const EXCLUSION_INDEX: &str = "ux_task_leases_active";

/// Checks that the exclusion index in the *opened database* is the partial
/// unique index this crate's concurrency claims are made about.
///
/// Applying the DDL above is not evidence that it took. `CREATE UNIQUE INDEX
/// IF NOT EXISTS` matches on the **name** only, so a database that already
/// carries an index called `ux_task_leases_active` — over a different column,
/// without `UNIQUE`, or without the `status = 'active'` predicate — makes the
/// statement a silent no-op. Every acquire then falls back to the
/// check-then-insert in `Store::acquire`, which passes every single-threaded
/// test and hands two builders the same task under contention; a 24-way race
/// against such a database elected two winners while `health` answered `ok`.
///
/// So this reads the schema that is actually there rather than asserting a
/// compile-time constant. The check is semantic, not textual: DevCouncil's
/// SQLAlchemy emits this index with its own spacing and quoting, and comparing
/// DDL strings would reject the very database this crate exists to share.
/// `PRAGMA index_list` answers whether it is unique and partial, `PRAGMA
/// index_info` answers which column it covers, and only the `WHERE` clause —
/// which no pragma exposes — is read from `sqlite_master`, normalised.
pub fn verify_exclusion_index(conn: &Connection) -> std::result::Result<(), String> {
    let mut list = conn
        .prepare("PRAGMA index_list(task_leases)")
        .map_err(|e| format!("the index list could not be read: {e}"))?;
    let rows = list
        .query_map([], |row| {
            Ok((
                row.get::<_, String>("name")?,
                row.get::<_, i64>("unique")?,
                row.get::<_, i64>("partial")?,
            ))
        })
        .map_err(|e| format!("the index list could not be read: {e}"))?;

    let mut found = None;
    for row in rows {
        let (name, unique, partial) =
            row.map_err(|e| format!("the index list could not be read: {e}"))?;
        if name == EXCLUSION_INDEX {
            found = Some((unique, partial));
            break;
        }
    }
    let Some((unique, partial)) = found else {
        return Err(format!(
            "no index named {EXCLUSION_INDEX} exists on task_leases"
        ));
    };
    if unique != 1 {
        return Err(format!("{EXCLUSION_INDEX} is not UNIQUE"));
    }
    if partial != 1 {
        return Err(format!(
            "{EXCLUSION_INDEX} is not a partial index, so it would forbid a second lease on a task \
             that has already been released"
        ));
    }

    let mut info = conn
        .prepare(&format!("PRAGMA index_info({EXCLUSION_INDEX})"))
        .map_err(|e| format!("the index columns could not be read: {e}"))?;
    let columns: Vec<String> = info
        .query_map([], |row| row.get::<_, Option<String>>("name"))
        .map_err(|e| format!("the index columns could not be read: {e}"))?
        .collect::<rusqlite::Result<Vec<Option<String>>>>()
        .map_err(|e| format!("the index columns could not be read: {e}"))?
        .into_iter()
        .map(|c| c.unwrap_or_else(|| "<expression>".to_string()))
        .collect();
    if columns != ["task_id"] {
        return Err(format!(
            "{EXCLUSION_INDEX} covers {columns:?}, not [\"task_id\"]"
        ));
    }

    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
            [EXCLUSION_INDEX],
            |row| row.get(0),
        )
        .map_err(|e| format!("the index definition could not be read: {e}"))?;
    let sql = sql.unwrap_or_default();
    if !normalise_ddl(&sql).contains("wherestatus='active'") {
        return Err(format!(
            "{EXCLUSION_INDEX} is partial over some other predicate than status = 'active': {sql:?}"
        ));
    }
    Ok(())
}

/// Reduces DDL to the form the predicate check compares against: no
/// whitespace, no identifier quoting, one spelling of equality. Producers
/// differ in all three — SQLAlchemy quotes identifiers this crate does not —
/// and a check that tripped over spacing would reject a correct schema.
fn normalise_ddl(sql: &str) -> String {
    let lowered = sql.to_ascii_lowercase().replace("==", "=");
    lowered
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '"' && *c != '`' && *c != '[' && *c != ']')
        .collect()
}

/// One column as `PRAGMA table_info` reports it.
struct Column {
    name: String,
    ty: String,
    not_null: bool,
    default: Option<String>,
    primary_key: bool,
}

fn table_columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<Column>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| {
        Ok(Column {
            name: row.get("name")?,
            ty: row.get("type")?,
            not_null: row.get::<_, i64>("notnull")? != 0,
            default: row.get("dflt_value")?,
            primary_key: row.get::<_, i64>("pk")? != 0,
        })
    })?;
    rows.collect()
}

/// Every column [`SCHEMA`] defines that `conn`'s tables lack, as
/// `(table, column)`, in a stable order.
///
/// Derived from [`SCHEMA`] itself — created in a scratch database and read back
/// through `PRAGMA table_info` — rather than from a second hand-kept list, so
/// a column added to the DDL above is ensured on old stores without anyone
/// remembering to name it twice. `CREATE TABLE IF NOT EXISTS` leaves an
/// existing table as it is; before this, only the two `gaps` output columns
/// were added, so a store from before schema 7 passed health, was stamped as
/// current, and then failed every `task` read with "no such column".
fn missing_columns(conn: &Connection) -> rusqlite::Result<Vec<(String, Column)>> {
    let scratch = Connection::open_in_memory()?;
    scratch.execute_batch(SCHEMA)?;
    let mut tables = scratch.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let tables: Vec<String> = tables
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut missing = Vec::new();
    for table in tables {
        let have: Vec<String> = table_columns(conn, &table)?
            .into_iter()
            .map(|c| c.name)
            .collect();
        for column in table_columns(&scratch, &table)? {
            if !have.contains(&column.name) {
                missing.push((table.clone(), column));
            }
        }
    }
    Ok(missing)
}

fn add_missing_columns(
    conn: &Connection,
    budget: &mut Option<usize>,
) -> std::result::Result<(), String> {
    let missing =
        missing_columns(conn).map_err(|e| format!("columns could not be compared: {e}"))?;
    for (table, column) in missing {
        // SQLite refuses to add a primary key, and a NOT NULL column with no
        // default (existing rows would have nothing to hold). Neither can be
        // repaired in place, so say so rather than half-migrate.
        if column.primary_key || (column.not_null && column.default.is_none()) {
            return Err(format!(
                "table {table} lacks column {}, which cannot be added to an existing table",
                column.name
            ));
        }
        spend(budget)?;
        let mut ddl = format!(
            "ALTER TABLE {table} ADD COLUMN {} {}",
            column.name, column.ty
        );
        if column.not_null {
            ddl.push_str(" NOT NULL");
        }
        if let Some(default) = &column.default {
            ddl.push_str(&format!(" DEFAULT {default}"));
        }
        conn.execute_batch(&ddl)
            .map_err(|e| format!("column {table}.{} could not be added: {e}", column.name))?;
    }
    Ok(())
}

/// Brings an existing store's columns and `schema_version` up to this build,
/// as one transaction.
///
/// The columns and the stamp commit together or not at all: stamping first, or
/// stamping without the columns, is what left a v6 store reading as v9 while
/// missing the column `task` selects. A newer-than-known version is refused
/// before anything is touched. An up-to-date store takes no write lock — this
/// runs on every open.
pub fn migrate(conn: &Connection, known: u32) -> std::result::Result<(), String> {
    migrate_inner(conn, known, &mut None)
}

/// Fails the migration after `steps` write steps (each column added, then the
/// version stamp), so a test can crash it at every point and check that the
/// store is left exactly as it was found.
#[doc(hidden)]
pub fn migrate_failing_after(
    conn: &Connection,
    known: u32,
    steps: usize,
) -> std::result::Result<(), String> {
    migrate_inner(conn, known, &mut Some(steps))
}

fn spend(budget: &mut Option<usize>) -> std::result::Result<(), String> {
    match budget {
        Some(0) => Err("injected failure".to_string()),
        Some(n) => {
            *n -= 1;
            Ok(())
        }
        None => Ok(()),
    }
}

fn migrate_inner(
    conn: &Connection,
    known: u32,
    budget: &mut Option<usize>,
) -> std::result::Result<(), String> {
    let read_version = |conn: &Connection| -> std::result::Result<Option<i64>, String> {
        conn.query_row(
            "SELECT version FROM schema_version WHERE id = 'singleton'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("schema_version could not be read: {e}"))
    };
    let current = read_version(conn)?;
    if let Some(v) = current {
        check_version(v, known)?;
    }
    let missing =
        missing_columns(conn).map_err(|e| format!("columns could not be compared: {e}"))?;
    if missing.is_empty() && current == Some(known as i64) {
        return Ok(());
    }

    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| format!("migration could not take the write lock: {e}"))?;
    let outcome = (|| {
        // Re-read under the lock: another process may have migrated, or a
        // newer binary may have stamped a higher version, since the look above.
        if let Some(v) = read_version(conn)? {
            check_version(v, known)?;
        }
        add_missing_columns(conn, budget)?;
        spend(budget)?;
        ensure_schema_version(conn, known)
    })();
    match outcome {
        Ok(()) => conn
            .execute_batch("COMMIT")
            .map_err(|e| format!("migration could not commit: {e}")),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

pub(crate) fn gaps_has_column(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(gaps)")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>("name")? == name {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Ensures `schema_version` matches this build, refusing a newer unknown row.
///
/// Aligns with Python `storage/db.py` (`SCHEMA_VERSION = 9`): create/upgrade
/// when missing or behind; fail closed when the file is ahead of this binary.
pub fn ensure_schema_version(conn: &Connection, known: u32) -> std::result::Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            id VARCHAR NOT NULL,
            version INTEGER NOT NULL,
            PRIMARY KEY (id)
        );",
    )
    .map_err(|e| format!("schema_version table could not be ensured: {e}"))?;

    let current: Option<i64> = conn
        .query_row(
            "SELECT version FROM schema_version WHERE id = 'singleton'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("schema_version could not be read: {e}"))?;

    match current {
        None => {
            // Two processes can race the insert; the loser still has a row.
            match conn.execute(
                "INSERT INTO schema_version (id, version) VALUES ('singleton', ?1)",
                [known as i64],
            ) {
                Ok(_) => Ok(()),
                Err(rusqlite::Error::SqliteFailure(f, _))
                    if f.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    let again: i64 = conn
                        .query_row(
                            "SELECT version FROM schema_version WHERE id = 'singleton'",
                            [],
                            |row| row.get(0),
                        )
                        .map_err(|e| format!("schema_version race could not be re-read: {e}"))?;
                    check_version(again, known)
                }
                Err(e) => Err(format!("schema_version could not be written: {e}")),
            }
        }
        Some(v) => {
            check_version(v, known)?;
            if (v as u32) < known {
                conn.execute(
                    "UPDATE schema_version SET version = ?1 WHERE id = 'singleton'",
                    [known as i64],
                )
                .map_err(|e| format!("schema_version could not be upgraded: {e}"))?;
            }
            Ok(())
        }
    }
}

fn check_version(found: i64, known: u32) -> std::result::Result<(), String> {
    if found < 0 {
        return Err(format!("schema_version {found} is not a usable revision"));
    }
    let found_u = found as u32;
    if found_u > known {
        return Err(format!(
            "unsupported DevCouncil schema version {found_u}; this binary knows {known}"
        ));
    }
    Ok(())
}
