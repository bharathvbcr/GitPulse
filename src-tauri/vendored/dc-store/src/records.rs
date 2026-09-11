//! Python-owned state tables transcribed for additive `dc-store` ownership.
//!
//! These verbs cover the tables Python still writes (`evidence`, `gaps`,
//! `verification_runs`, `agent_handoffs`, …). The DDL lives in [`schema::SCHEMA`];
//! this module is the typed read/write surface the `dcstore` CLI and Go client
//! share. List answers are capped so a runaway table cannot blow a reply.

use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, StoreError};

/// Ceiling on one evidence/gaps list reply. Far past any honest task, short of
/// a reply that would flood the Go client's `maxOutput` budget.
pub const MAX_LIST_ROWS: usize = 10_000;

/// One row of the `evidence` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRow {
    pub id: i64,
    pub kind: String,
    pub task_id: Option<String>,
    pub requirement_id: Option<String>,
    pub acceptance_criterion_id: Option<String>,
    pub data_json: String,
}

/// One row of the `gaps` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapRow {
    pub id: String,
    pub severity: String,
    pub gap_type: String,
    pub requirement_id: Option<String>,
    pub task_id: Option<String>,
    pub description: String,
    pub evidence_json: String,
    pub recommended_fix: String,
    pub blocking: bool,
    pub file: Option<String>,
    pub line: Option<i64>,
    pub suggested_command: Option<String>,
    pub acceptance_criterion_id: Option<String>,
    pub expected_verification_method: Option<String>,
}

/// One row of `verification_runs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationRun {
    pub id: String,
    pub task_id: String,
    pub sandbox: String,
    pub environment_json: String,
    pub commands_json: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// One row of `agent_handoffs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHandoff {
    pub id: String,
    pub task_id: String,
    pub from_agent: String,
    pub to_agent: String,
    pub run_id: String,
    pub manifest_path: String,
    pub status: String,
    pub created_at: String,
}

/// Appends one evidence row. Returns the new row id.
pub fn evidence_append(
    conn: &Connection,
    kind: &str,
    task_id: Option<&str>,
    requirement_id: Option<&str>,
    acceptance_criterion_id: Option<&str>,
    data_json: &str,
) -> Result<i64> {
    if kind.trim().is_empty() {
        return Err(StoreError::BadScope {
            reason: "evidence type is empty".into(),
        });
    }
    if !data_json.trim_start().starts_with('{') && !data_json.trim_start().starts_with('[') {
        return Err(StoreError::BadScope {
            reason: "evidence data_json must be a JSON object or array".into(),
        });
    }
    conn.execute(
        "INSERT INTO evidence (type, task_id, requirement_id, acceptance_criterion_id, data_json)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            kind,
            task_id,
            requirement_id,
            acceptance_criterion_id,
            data_json
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Lists evidence rows, newest first, optionally filtered by task.
///
/// `truncated` is true when more than [`MAX_LIST_ROWS`] matched.
pub fn evidence_list(
    conn: &Connection,
    task_id: Option<&str>,
) -> Result<(Vec<EvidenceRow>, bool)> {
    let limit = (MAX_LIST_ROWS + 1) as i64;
    let map_row = |row: &rusqlite::Row<'_>| {
        Ok(EvidenceRow {
            id: row.get(0)?,
            kind: row.get(1)?,
            task_id: row.get(2)?,
            requirement_id: row.get(3)?,
            acceptance_criterion_id: row.get(4)?,
            data_json: row.get(5)?,
        })
    };
    let rows: Vec<EvidenceRow> = if let Some(tid) = task_id {
        let mut stmt = conn.prepare(
            "SELECT id, type, task_id, requirement_id, acceptance_criterion_id, data_json
             FROM evidence WHERE task_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        stmt.query_map(params![tid, limit], map_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, type, task_id, requirement_id, acceptance_criterion_id, data_json
             FROM evidence ORDER BY id DESC LIMIT ?1",
        )?;
        stmt.query_map(params![limit], map_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let truncated = rows.len() > MAX_LIST_ROWS;
    Ok((rows.into_iter().take(MAX_LIST_ROWS).collect(), truncated))
}

/// Inserts or replaces one gap row.
pub fn gap_upsert(conn: &Connection, gap: &GapRow) -> Result<()> {
    if gap.id.trim().is_empty() {
        return Err(StoreError::BadScope {
            reason: "gap id is required".into(),
        });
    }
    let blocking: i64 = if gap.blocking { 1 } else { 0 };
    conn.execute(
        "INSERT INTO gaps
            (id, severity, gap_type, requirement_id, task_id, description, evidence_json,
             recommended_fix, blocking, file, line, suggested_command,
             acceptance_criterion_id, expected_verification_method)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(id) DO UPDATE SET
            severity = excluded.severity,
            gap_type = excluded.gap_type,
            requirement_id = excluded.requirement_id,
            task_id = excluded.task_id,
            description = excluded.description,
            evidence_json = excluded.evidence_json,
            recommended_fix = excluded.recommended_fix,
            blocking = excluded.blocking,
            file = excluded.file,
            line = excluded.line,
            suggested_command = excluded.suggested_command,
            acceptance_criterion_id = excluded.acceptance_criterion_id,
            expected_verification_method = excluded.expected_verification_method",
        params![
            gap.id,
            gap.severity,
            gap.gap_type,
            gap.requirement_id,
            gap.task_id,
            gap.description,
            gap.evidence_json,
            gap.recommended_fix,
            blocking,
            gap.file,
            gap.line,
            gap.suggested_command,
            gap.acceptance_criterion_id,
            gap.expected_verification_method,
        ],
    )?;
    Ok(())
}

/// Deletes every gap for a task, then upserts the replacement set.
///
/// Used by the verifier so a reconnecting agent sees the latest run rather than
/// a union of historical findings.
pub fn gaps_replace(conn: &Connection, task_id: &str, gaps: &[GapRow]) -> Result<()> {
    if task_id.trim().is_empty() {
        return Err(StoreError::BadScope {
            reason: "task_id is required to replace gaps".into(),
        });
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM gaps WHERE task_id = ?1", params![task_id])?;
    for gap in gaps {
        let blocking: i64 = if gap.blocking { 1 } else { 0 };
        tx.execute(
            "INSERT INTO gaps
                (id, severity, gap_type, requirement_id, task_id, description, evidence_json,
                 recommended_fix, blocking, file, line, suggested_command,
                 acceptance_criterion_id, expected_verification_method)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                gap.id,
                gap.severity,
                gap.gap_type,
                gap.requirement_id,
                gap.task_id,
                gap.description,
                gap.evidence_json,
                gap.recommended_fix,
                blocking,
                gap.file,
                gap.line,
                gap.suggested_command,
                gap.acceptance_criterion_id,
                gap.expected_verification_method,
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Lists gaps, optionally filtered by task. Cap + truncate flag as evidence.
pub fn gaps_list(conn: &Connection, task_id: Option<&str>) -> Result<(Vec<GapRow>, bool)> {
    let limit = (MAX_LIST_ROWS + 1) as i64;
    let map_row = |row: &rusqlite::Row<'_>| {
        let blocking: i64 = row.get(8)?;
        Ok(GapRow {
            id: row.get(0)?,
            severity: row.get(1)?,
            gap_type: row.get(2)?,
            requirement_id: row.get(3)?,
            task_id: row.get(4)?,
            description: row.get(5)?,
            evidence_json: row.get(6)?,
            recommended_fix: row.get(7)?,
            blocking: blocking != 0,
            file: row.get(9)?,
            line: row.get(10)?,
            suggested_command: row.get(11)?,
            acceptance_criterion_id: row.get(12)?,
            expected_verification_method: row.get(13)?,
        })
    };
    let rows: Vec<GapRow> = if let Some(tid) = task_id {
        let mut stmt = conn.prepare(
            "SELECT id, severity, gap_type, requirement_id, task_id, description, evidence_json,
                    recommended_fix, blocking, file, line, suggested_command,
                    acceptance_criterion_id, expected_verification_method
             FROM gaps WHERE task_id = ?1 ORDER BY id LIMIT ?2",
        )?;
        stmt.query_map(params![tid, limit], map_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, severity, gap_type, requirement_id, task_id, description, evidence_json,
                    recommended_fix, blocking, file, line, suggested_command,
                    acceptance_criterion_id, expected_verification_method
             FROM gaps ORDER BY id LIMIT ?1",
        )?;
        stmt.query_map(params![limit], map_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let truncated = rows.len() > MAX_LIST_ROWS;
    Ok((rows.into_iter().take(MAX_LIST_ROWS).collect(), truncated))
}

/// Inserts or replaces a verification run.
pub fn run_record(conn: &Connection, run: &VerificationRun) -> Result<()> {
    if run.id.trim().is_empty() || run.task_id.trim().is_empty() {
        return Err(StoreError::BadScope {
            reason: "verification run id and task_id are required".into(),
        });
    }
    conn.execute(
        "INSERT INTO verification_runs
            (id, task_id, sandbox, environment_json, commands_json, status, started_at, finished_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
            task_id = excluded.task_id,
            sandbox = excluded.sandbox,
            environment_json = excluded.environment_json,
            commands_json = excluded.commands_json,
            status = excluded.status,
            started_at = excluded.started_at,
            finished_at = excluded.finished_at",
        params![
            run.id,
            run.task_id,
            run.sandbox,
            run.environment_json,
            run.commands_json,
            run.status,
            run.started_at,
            run.finished_at,
        ],
    )?;
    Ok(())
}

fn map_verification_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<VerificationRun> {
    Ok(VerificationRun {
        id: row.get(0)?,
        task_id: row.get(1)?,
        sandbox: row.get(2)?,
        environment_json: row.get(3)?,
        commands_json: row.get(4)?,
        status: row.get(5)?,
        started_at: row.get(6)?,
        finished_at: row.get(7)?,
    })
}

/// Reads one verification run by id.
pub fn run_get(conn: &Connection, id: &str) -> Result<Option<VerificationRun>> {
    conn.query_row(
        "SELECT id, task_id, sandbox, environment_json, commands_json, status, started_at, finished_at
         FROM verification_runs WHERE id = ?1",
        [id],
        map_verification_run,
    )
    .optional()
    .map_err(StoreError::from)
}

/// Lists verification runs, newest first, optionally filtered by task.
///
/// `truncated` is true when more than [`MAX_LIST_ROWS`] matched.
pub fn runs_list(
    conn: &Connection,
    task_id: Option<&str>,
) -> Result<(Vec<VerificationRun>, bool)> {
    let limit = (MAX_LIST_ROWS + 1) as i64;
    let mut rows = match task_id {
        Some(task) => {
            let mut stmt = conn.prepare(
                "SELECT id, task_id, sandbox, environment_json, commands_json, status,
                        started_at, finished_at
                 FROM verification_runs WHERE task_id = ?1
                 ORDER BY started_at DESC, id DESC LIMIT ?2",
            )?;
            stmt.query_map(params![task, limit], map_verification_run)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT id, task_id, sandbox, environment_json, commands_json, status,
                        started_at, finished_at
                 FROM verification_runs
                 ORDER BY started_at DESC, id DESC LIMIT ?1",
            )?;
            stmt.query_map([limit], map_verification_run)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        }
    };
    let truncated = rows.len() > MAX_LIST_ROWS;
    if truncated {
        rows.truncate(MAX_LIST_ROWS);
    }
    Ok((rows, truncated))
}

/// Inserts or replaces an agent handoff.
pub fn handoff_record(conn: &Connection, handoff: &AgentHandoff) -> Result<()> {
    if handoff.id.trim().is_empty() || handoff.task_id.trim().is_empty() {
        return Err(StoreError::BadScope {
            reason: "handoff id and task_id are required".into(),
        });
    }
    conn.execute(
        "INSERT INTO agent_handoffs
            (id, task_id, from_agent, to_agent, run_id, manifest_path, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
            task_id = excluded.task_id,
            from_agent = excluded.from_agent,
            to_agent = excluded.to_agent,
            run_id = excluded.run_id,
            manifest_path = excluded.manifest_path,
            status = excluded.status,
            created_at = excluded.created_at",
        params![
            handoff.id,
            handoff.task_id,
            handoff.from_agent,
            handoff.to_agent,
            handoff.run_id,
            handoff.manifest_path,
            handoff.status,
            handoff.created_at,
        ],
    )?;
    Ok(())
}

/// Reads one handoff by id.
pub fn handoff_get(conn: &Connection, id: &str) -> Result<Option<AgentHandoff>> {
    conn.query_row(
        "SELECT id, task_id, from_agent, to_agent, run_id, manifest_path, status, created_at
         FROM agent_handoffs WHERE id = ?1",
        [id],
        |row| {
            Ok(AgentHandoff {
                id: row.get(0)?,
                task_id: row.get(1)?,
                from_agent: row.get(2)?,
                to_agent: row.get(3)?,
                run_id: row.get(4)?,
                manifest_path: row.get(5)?,
                status: row.get(6)?,
                created_at: row.get(7)?,
            })
        },
    )
    .optional()
    .map_err(StoreError::from)
}

#[cfg(test)]
mod tests {
    use crate::{SCHEMA_VERSION, Store, schema};

    #[test]
    fn evidence_round_trip_and_cap() {
        let store = Store::open_in_memory().expect("open");
        let id = store
            .evidence_append("command", Some("T1"), None, None, r#"{"cmd":"go test"}"#)
            .expect("append");
        assert!(id > 0);
        let (rows, truncated) = store.evidence_list(Some("T1")).expect("list");
        assert_eq!(rows.len(), 1);
        assert!(!truncated);
        assert_eq!(rows[0].kind, "command");
    }

    #[test]
    fn runs_list_filters_and_orders() {
        let store = Store::open_in_memory().expect("open");
        store
            .run_record(&crate::records::VerificationRun {
                id: "r1".into(),
                task_id: "T1".into(),
                sandbox: "local".into(),
                environment_json: "{}".into(),
                commands_json: "[]".into(),
                status: "passed".into(),
                started_at: "2026-01-01T00:00:00Z".into(),
                finished_at: Some("2026-01-01T00:01:00Z".into()),
            })
            .expect("r1");
        store
            .run_record(&crate::records::VerificationRun {
                id: "r2".into(),
                task_id: "T2".into(),
                sandbox: "local".into(),
                environment_json: "{}".into(),
                commands_json: "[]".into(),
                status: "failed".into(),
                started_at: "2026-01-02T00:00:00Z".into(),
                finished_at: None,
            })
            .expect("r2");
        let (all, truncated) = store.runs_list(None).expect("all");
        assert!(!truncated);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "r2");
        let (filtered, _) = store.runs_list(Some("T1")).expect("T1");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "r1");
    }

    #[test]
    fn refuse_non_json_evidence() {
        let store = Store::open_in_memory().expect("open");
        let err = store
            .evidence_append("diff", None, None, None, "not-json")
            .unwrap_err();
        assert!(err.to_string().contains("JSON"));
    }

    #[test]
    fn schema_too_new_refuses_open() {
        let store = Store::open_in_memory().expect("open");
        store
            .connection()
            .execute(
                "UPDATE schema_version SET version = ?1 WHERE id = 'singleton'",
                [(SCHEMA_VERSION as i64) + 1],
            )
            .expect("bump");
        let err = schema::ensure_schema_version(store.connection(), SCHEMA_VERSION).unwrap_err();
        assert!(err.contains("unsupported"));
    }
}
