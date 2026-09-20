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

/// One row of `gap_history`: what became of one gap across a task's runs.
///
/// Deliberately thin. It carries identity and counts, not the gap's description
/// or evidence — those belong to the current-state row in `gaps` and change
/// between runs, and a history that also stored them would be a second,
/// diverging copy of a record that already has an owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapHistoryRow {
    pub task_id: String,
    pub gap_id: String,
    pub gap_type: String,
    /// The run this gap was first reported in.
    pub first_seen_run: i64,
    /// The most recent run that reported it.
    pub last_seen_run: i64,
    /// How many runs have reported it.
    pub occurrences: i64,
    /// How many times it was absent for at least one run and came back.
    ///
    /// Nonzero is the signal worth acting on: something reported this gap
    /// fixed, and a later run disagreed. It is a statement about the reports,
    /// not about the code — a run that could not measure the gap also makes it
    /// disappear — which is why the run numbers are kept beside the count.
    pub resurfaces: i64,
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
pub fn evidence_list(conn: &Connection, task_id: Option<&str>) -> Result<(Vec<EvidenceRow>, bool)> {
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
///
/// Also records the sighting in `gap_history`, against the run already in
/// progress. That is not redundant with [`gaps_replace`]: the Go client spells
/// a replacement as `gaps-clear` followed by one `gap-upsert` per gap, so
/// `gaps_replace` is only ever reached with an *empty* set through the path the
/// host actually uses. Recording history only there would have left every real
/// verification looking like a clean run, and the history permanently empty —
/// a table that is written by a code path nobody takes.
///
/// A gap with no task cannot be recorded: the history is keyed by (task, gap),
/// and there is no run to attribute an unattached gap to. It is still written
/// to `gaps`, because that table has always accepted one.
pub fn gap_upsert(conn: &Connection, gap: &GapRow) -> Result<()> {
    if gap.id.trim().is_empty() {
        return Err(StoreError::BadScope {
            reason: "gap id is required".into(),
        });
    }
    let tx = conn.unchecked_transaction()?;
    if let Some(task_id) = gap
        .task_id
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        let run = current_run(&tx, task_id)?;
        record_sighting(&tx, task_id, &gap.id, &gap.gap_type, run)?;
    }
    let blocking: i64 = if gap.blocking { 1 } else { 0 };
    tx.execute(
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
    tx.commit()?;
    Ok(())
}

/// Deletes every gap for a task, then upserts the replacement set.
///
/// Used by the verifier so a reconnecting agent sees the latest run rather than
/// a union of historical findings. That replacement is deliberate and is kept:
/// "what blocks me now" is the question this table answers.
///
/// It also advances `gap_runs` and folds the new set into `gap_history`, in the
/// same transaction. The history exists because the replacement destroys one
/// fact worth keeping — whether a gap has been reported before, gone away, and
/// come back. An agent that fixes a gap, verifies, and re-introduces it is the
/// single most informative thing a task can do, and against the current-state
/// table alone it looks identical to steady progress.
///
/// The counter advances on every call, including one with no gaps, which is
/// what makes a disappearance observable at all.
pub fn gaps_replace(conn: &Connection, task_id: &str, gaps: &[GapRow]) -> Result<()> {
    if task_id.trim().is_empty() {
        return Err(StoreError::BadScope {
            reason: "task_id is required to replace gaps".into(),
        });
    }
    let tx = conn.unchecked_transaction()?;

    // Advance this task's run counter first, so every sighting below agrees on
    // which run it is recording.
    let run = advance_run(&tx, task_id)?;
    for gap in gaps {
        record_sighting(&tx, task_id, &gap.id, &gap.gap_type, run)?;
    }

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

/// Starts a new run for a task and returns its number.
///
/// Advancing on a run that reports no gaps at all is the whole reason the
/// counter is stored rather than derived from `gap_history`. A derived number
/// would not move across a clean run, so a gap that disappeared for exactly one
/// run and came back would land adjacent to its previous sighting and read as
/// having never left — which is precisely the event the history exists to keep.
fn advance_run(tx: &Connection, task_id: &str) -> Result<i64> {
    let run: i64 = tx.query_row(
        "SELECT COALESCE(MAX(run), 0) + 1 FROM gap_runs WHERE task_id = ?1",
        params![task_id],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO gap_runs (task_id, run) VALUES (?1, ?2)
         ON CONFLICT(task_id) DO UPDATE SET run = excluded.run",
        params![task_id, run],
    )?;
    Ok(run)
}

/// The run a sighting outside [`gaps_replace`] belongs to.
///
/// [`gap_upsert`] records against the run already in progress rather than
/// starting one, because the Go client spells a replacement as `gaps-clear`
/// followed by one `gap-upsert` per gap: the clear opens the run and each
/// upsert is a sighting within it. A function that advanced here would make
/// every gap of one verification land in a run of its own, and a gap's
/// `last_seen_run` would then always be adjacent to the previous one — no
/// resurface would ever be detectable through the path the host actually uses.
fn current_run(tx: &Connection, task_id: &str) -> Result<i64> {
    let run: i64 = tx.query_row(
        "SELECT COALESCE(MAX(run), 0) FROM gap_runs WHERE task_id = ?1",
        params![task_id],
        |row| row.get(0),
    )?;
    if run > 0 {
        return Ok(run);
    }
    // A bare upsert against a task no run has opened. Open run 1 rather than
    // recording against run 0, so the numbering a later resurface is measured
    // against starts where `gaps_replace` would have started it.
    advance_run(tx, task_id)
}

/// Records that one gap was reported in one run.
///
/// Idempotent within a run: `occurrences` moves only when the sighting is newer
/// than the last one recorded. Without that, the host's clear-then-upsert-each
/// path would count a gap once per upsert, and a task whose verification
/// reported the same gap twice in one run would show two occurrences of a gap
/// that was found once.
///
/// One statement, so the read and the write cannot straddle anything, and
/// `resurfaces` increments from the stored value rather than being recomputed:
/// the condition is about the row's state *before* this run, and that state is
/// gone the moment `last_seen_run` moves.
fn record_sighting(
    tx: &Connection,
    task_id: &str,
    gap_id: &str,
    gap_type: &str,
    run: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO gap_history
            (task_id, gap_id, gap_type, first_seen_run, last_seen_run, occurrences, resurfaces)
         VALUES (?1, ?2, ?3, ?4, ?4, 1, 0)
         ON CONFLICT(task_id, gap_id) DO UPDATE SET
            gap_type = excluded.gap_type,
            occurrences = gap_history.occurrences
                + (CASE WHEN gap_history.last_seen_run < ?4 THEN 1 ELSE 0 END),
            -- Strictly `< run - 1`: `== run - 1` is the previous run, which
            -- means the gap never left.
            resurfaces = gap_history.resurfaces
                + (CASE WHEN gap_history.last_seen_run < ?4 - 1 THEN 1 ELSE 0 END),
            last_seen_run = max(gap_history.last_seen_run, ?4)",
        params![task_id, gap_id, gap_type, run],
    )?;
    Ok(())
}

/// Lists gap history, optionally filtered by task. Cap + truncate flag, as
/// [`gaps_list`].
///
/// Ordered by `resurfaces` descending before id, so the gaps that were reported
/// fixed and came back are at the top of a capped read rather than wherever
/// their identifier happens to sort. A truncated list that dropped exactly the
/// rows worth acting on would be worse than no list.
pub fn gap_history_list(
    conn: &Connection,
    task_id: Option<&str>,
) -> Result<(Vec<GapHistoryRow>, bool)> {
    let limit = (MAX_LIST_ROWS + 1) as i64;
    let map_row = |row: &rusqlite::Row<'_>| {
        Ok(GapHistoryRow {
            task_id: row.get(0)?,
            gap_id: row.get(1)?,
            gap_type: row.get(2)?,
            first_seen_run: row.get(3)?,
            last_seen_run: row.get(4)?,
            occurrences: row.get(5)?,
            resurfaces: row.get(6)?,
        })
    };
    const COLUMNS: &str = "task_id, gap_id, gap_type, first_seen_run, last_seen_run,
                           occurrences, resurfaces";
    let rows: Vec<GapHistoryRow> = if let Some(tid) = task_id {
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM gap_history WHERE task_id = ?1
             ORDER BY resurfaces DESC, gap_id LIMIT ?2"
        ))?;
        stmt.query_map(params![tid, limit], map_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM gap_history
             ORDER BY resurfaces DESC, task_id, gap_id LIMIT ?1"
        ))?;
        stmt.query_map(params![limit], map_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let truncated = rows.len() > MAX_LIST_ROWS;
    Ok((rows.into_iter().take(MAX_LIST_ROWS).collect(), truncated))
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
pub fn runs_list(conn: &Connection, task_id: Option<&str>) -> Result<(Vec<VerificationRun>, bool)> {
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

    /// Builds the gap rows a run reports, by id, for one task.
    ///
    /// `gaps.id` is the primary key across the whole table rather than per
    /// task, so two tasks never share an identifier — which is why the host
    /// builds one with `StableGapID(taskID, …)`. The tests follow that.
    /// `gap_history` keys on the (task, gap) pair instead, which is strictly
    /// more permissive and cannot collide where the current-state table would.
    fn gaps_for(task: &str, ids: &[&str]) -> Vec<crate::records::GapRow> {
        ids.iter()
            .map(|id| crate::records::GapRow {
                id: format!("{task}-{id}"),
                severity: "high".into(),
                gap_type: "stub_detected".into(),
                requirement_id: None,
                task_id: Some(task.to_string()),
                description: "d".into(),
                evidence_json: "[]".into(),
                recommended_fix: "f".into(),
                blocking: true,
                file: None,
                line: None,
                suggested_command: None,
                acceptance_criterion_id: None,
                expected_verification_method: None,
            })
            .collect()
    }

    /// The path the host actually takes.
    ///
    /// `dc/store.GapsReplace` does not call `gaps_replace` with the new set. It
    /// runs `gaps-clear` — which is `gaps_replace` with an EMPTY set — and then
    /// one `gap-upsert` per gap. So a history recorded only in `gaps_replace`
    /// would see every real verification as a clean run and stay permanently
    /// empty, while every test that drove `gaps_replace` directly passed.
    ///
    /// This asserts the two spellings agree, because only one of them is used
    /// in production and it is not the one that is convenient to test.
    #[test]
    fn the_hosts_clear_then_upsert_path_records_the_same_history() {
        let store = Store::open_in_memory().expect("open");
        let replace_like = |ids: &[&str]| {
            // gaps-clear, then one upsert per gap: exactly what the Go client
            // emits for one verification.
            store.gaps_replace("T1", &[]).expect("clear");
            for gap in gaps_for("T1", ids) {
                store.gap_upsert(&gap).expect("upsert");
            }
        };

        replace_like(&["G1"]); // run 1: reported
        replace_like(&[]); // run 2: clean
        replace_like(&["G1"]); // run 3: back

        let (history, _) = store.gap_history_list(Some("T1")).expect("history");
        assert_eq!(history.len(), 1, "{history:#?}");
        assert_eq!(history[0].gap_id, "T1-G1");
        assert_eq!(history[0].first_seen_run, 1);
        assert_eq!(history[0].last_seen_run, 3);
        assert_eq!(history[0].occurrences, 2);
        assert_eq!(
            history[0].resurfaces, 1,
            "the host's own path must see the resurface the direct call does"
        );
    }

    /// Two sightings of one gap inside a single run count once.
    ///
    /// Reachable through the host path — a verification that reported the same
    /// gap id twice would upsert it twice — and without the guard each upsert
    /// would add an occurrence, so a gap found once would read as found twice.
    #[test]
    fn repeated_upserts_within_one_run_count_as_one_occurrence() {
        let store = Store::open_in_memory().expect("open");
        store.gaps_replace("T1", &[]).expect("clear");
        let gaps = gaps_for("T1", &["G1"]);
        store.gap_upsert(&gaps[0]).expect("first");
        store.gap_upsert(&gaps[0]).expect("second");

        let (history, _) = store.gap_history_list(Some("T1")).expect("history");
        assert_eq!(history[0].occurrences, 1, "{history:#?}");
        assert_eq!(history[0].resurfaces, 0);
    }

    /// A gap upserted against a task no run has opened still gets a run.
    ///
    /// Recording it at run 0 would make the first later run adjacent to it and
    /// hide a resurface; refusing to record it would lose the sighting.
    #[test]
    fn a_bare_upsert_opens_the_first_run() {
        let store = Store::open_in_memory().expect("open");
        let gaps = gaps_for("T1", &["G1"]);
        store.gap_upsert(&gaps[0]).expect("upsert");

        let (history, _) = store.gap_history_list(Some("T1")).expect("history");
        assert_eq!(history[0].first_seen_run, 1, "{history:#?}");
        assert_eq!(history[0].occurrences, 1);
    }

    /// The fact the current-state table cannot hold.
    ///
    /// A gap raised, reported fixed, then raised again is the highest-signal
    /// event in a task, and against `gaps` alone it is indistinguishable from a
    /// gap that was never addressed — both are simply "present now".
    #[test]
    fn a_gap_that_goes_away_and_comes_back_is_recorded_as_a_resurface() {
        let store = Store::open_in_memory().expect("open");
        // Run 1: reported. Run 2: clean — the fix appeared to land. Run 3: back.
        store
            .gaps_replace("T1", &gaps_for("T1", &["G1"]))
            .expect("run 1");
        store.gaps_replace("T1", &[]).expect("run 2");
        store
            .gaps_replace("T1", &gaps_for("T1", &["G1"]))
            .expect("run 3");

        let (history, truncated) = store.gap_history_list(Some("T1")).expect("history");
        assert!(!truncated);
        assert_eq!(history.len(), 1, "{history:#?}");
        let row = &history[0];
        assert_eq!(row.gap_id, "T1-G1");
        assert_eq!(row.first_seen_run, 1);
        assert_eq!(row.last_seen_run, 3);
        assert_eq!(row.occurrences, 2);
        assert_eq!(
            row.resurfaces, 1,
            "the clean run between the two sightings is what makes this a resurface"
        );

        // And the current-state table still answers only the present, which is
        // the property the history was added to preserve rather than replace.
        let (current, _) = store.gaps_list(Some("T1")).expect("gaps");
        assert_eq!(current.len(), 1);
    }

    /// The counterpart, and the one that makes the assertion above mean
    /// something: a gap nobody has fixed must not be reported as resurfacing.
    #[test]
    fn a_gap_present_in_every_run_never_resurfaces() {
        let store = Store::open_in_memory().expect("open");
        for _ in 0..4 {
            store
                .gaps_replace("T1", &gaps_for("T1", &["G1"]))
                .expect("run");
        }
        let (history, _) = store.gap_history_list(Some("T1")).expect("history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].occurrences, 4);
        assert_eq!(history[0].last_seen_run, 4);
        assert_eq!(
            history[0].resurfaces, 0,
            "a gap reported by every run never went away"
        );
    }

    /// A clean run must advance the counter even though it writes no history.
    ///
    /// This is why `gap_runs` exists as its own table. Deriving the run number
    /// from the history's own maximum would leave it unchanged across a clean
    /// run, and the gap above would come back at run 2 with its last sighting
    /// at run 1 — adjacent, and therefore invisible.
    #[test]
    fn a_clean_run_still_advances_the_run_counter() {
        let store = Store::open_in_memory().expect("open");
        store
            .gaps_replace("T1", &gaps_for("T1", &["G1"]))
            .expect("run 1");
        store.gaps_replace("T1", &[]).expect("run 2");
        store.gaps_replace("T1", &[]).expect("run 3");
        store
            .gaps_replace("T1", &gaps_for("T1", &["G1"]))
            .expect("run 4");

        let (history, _) = store.gap_history_list(Some("T1")).expect("history");
        assert_eq!(history[0].last_seen_run, 4, "{history:#?}");
        assert_eq!(history[0].resurfaces, 1);
    }

    /// History is per task, and a read for one task must not see another's.
    #[test]
    fn history_is_scoped_to_its_task() {
        let store = Store::open_in_memory().expect("open");
        store
            .gaps_replace("T1", &gaps_for("T1", &["G1"]))
            .expect("t1");
        store
            .gaps_replace("T2", &gaps_for("T2", &["G1"]))
            .expect("t2");

        let (only_t1, _) = store.gap_history_list(Some("T1")).expect("t1 history");
        assert_eq!(only_t1.len(), 1);
        assert_eq!(only_t1[0].task_id, "T1");
        // Each task numbers its own runs, so T2's first sighting is its run 1
        // and not a continuation of T1's.
        let (only_t2, _) = store.gap_history_list(Some("T2")).expect("t2 history");
        assert_eq!(only_t2[0].first_seen_run, 1);

        let (all, _) = store.gap_history_list(None).expect("all history");
        assert_eq!(all.len(), 2);
    }

    /// A capped read must keep the rows worth acting on.
    ///
    /// The list is ordered by `resurfaces` before identifier for this reason: a
    /// truncation that dropped exactly the repeatedly-reintroduced gaps would
    /// be worse than no history at all, because the remaining rows would look
    /// like the whole story.
    #[test]
    fn resurfacing_gaps_sort_ahead_of_quiet_ones() {
        let store = Store::open_in_memory().expect("open");
        // "zzz" sorts last by id, and is the one that came back.
        store
            .gaps_replace("T1", &gaps_for("T1", &["zzz"]))
            .expect("run 1");
        store.gaps_replace("T1", &[]).expect("run 2");
        store
            .gaps_replace("T1", &gaps_for("T1", &["aaa", "zzz"]))
            .expect("run 3");

        let (history, _) = store.gap_history_list(Some("T1")).expect("history");
        assert_eq!(history[0].gap_id, "T1-zzz", "{history:#?}");
        assert_eq!(history[0].resurfaces, 1);
        assert_eq!(history[1].gap_id, "T1-aaa");
        assert_eq!(history[1].resurfaces, 0);
    }

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
