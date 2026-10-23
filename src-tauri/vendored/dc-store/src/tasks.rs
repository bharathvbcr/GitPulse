//! Which workbench items are tasks this store's host may hand out.
//!
//! A task is an item in `work_items` whose primary repository is
//! [`workbench::LOCAL_REPOSITORY_ID`]. What is *ready* is a status question (the
//! item is `ready`, not deleted, not archived) and a lease question (nobody
//! holds it), answered together here so a caller cannot act on a task that was
//! claimed between two reads. Whether it is *blocked* is neither: it is derived
//! from the task's blocking gaps on every read, never stored.

use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, Store, StoreError, workbench};

/// How the gate mode orders and filters ready work. The mode is the caller's
/// (the host reads it from config); the ordering lives here so every client
/// gets the same answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyMode {
    /// A task with a blocking gap is not offered at all.
    Enforce,
    /// Unblocked tasks first, then blocked ones flagged not ready to check out.
    Advisory,
    /// As advisory: the gate is off, but a blocked task is still last and flagged.
    Off,
}

impl ReadyMode {
    /// Parses the wire spelling. Anything but the three names is refused: a
    /// mistyped mode must not read as the permissive one.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "enforce" => Some(Self::Enforce),
            "advisory" => Some(Self::Advisory),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enforce => "enforce",
            Self::Advisory => "advisory",
            Self::Off => "off",
        }
    }
}

/// One ready task, with what makes it more or less ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyTask {
    pub id: String,
    /// Blocking gaps the task has now. Zero means it can be checked out
    /// without first clearing something.
    pub blocking_gap_count: i64,
}

impl ReadyTask {
    /// False when the task has a blocking gap: it is listed (advisory, off) so
    /// the work is visible, but picking it up is not the next step.
    pub fn ready_to_checkout(&self) -> bool {
        self.blocking_gap_count == 0
    }
}

/// True when the profile tables exist at all. A store that has never held an
/// item has none of them, which is "no tasks", not an error.
pub(crate) fn items_present(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='work_items')",
        [],
        |r| r.get(0),
    )?)
}

/// True when `task_id` is a live item of this repository.
pub(crate) fn is_local_item(conn: &Connection, task_id: &str) -> Result<bool> {
    if !items_present(conn)? {
        return Ok(false);
    }
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM work_items WHERE id = ?1 AND deleted = 0 AND primary_repository_id = ?2)",
        params![task_id, workbench::LOCAL_REPOSITORY_ID],
        |r| r.get(0),
    )?)
}

impl Store {
    /// Lists tasks that are ready to be worked, excluding any already held.
    ///
    /// Like every other read of a lease in this store, it is a *writing* read:
    /// expiry is applied first. Without that, "held" meant `status = 'active'`
    /// as stored, so a builder that crashed left its task hidden from the ready
    /// list forever — the TTL had passed, but nothing had touched the row to
    /// notice. `devcouncil_next_task` calls this and nothing else.
    ///
    /// Order is: unblocked before blocked, then the item's priority (1 is
    /// highest), then its board position, then its id. Under
    /// [`ReadyMode::Enforce`] a blocked task is left out.
    pub fn ready_tasks(&self, mode: ReadyMode) -> Result<Vec<ReadyTask>> {
        self.sweep_expired()?;
        if !items_present(&self.conn)? {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT i.id, (SELECT count(*) FROM gaps g WHERE g.task_id = i.id AND g.blocking = 1) AS blocking
             FROM work_items i
             WHERE i.deleted = 0 AND i.archived = 0 AND i.status = 'ready'
               AND i.primary_repository_id = ?1
               AND NOT EXISTS (
                 SELECT 1 FROM task_leases l WHERE l.task_id = i.id AND l.status = 'active')
             ORDER BY blocking > 0, coalesce(json_extract(i.body, '$.priority'), 2),
                      coalesce(i.position, 0), i.id",
        )?;
        let rows = stmt.query_map(params![workbench::LOCAL_REPOSITORY_ID], |r| {
            Ok(ReadyTask {
                id: r.get(0)?,
                blocking_gap_count: r.get(1)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            let task = row?;
            if mode == ReadyMode::Enforce && !task.ready_to_checkout() {
                continue;
            }
            out.push(task);
        }
        Ok(out)
    }

    /// Refuses a lease on anything that is not live, workable work of this
    /// repository: no such item, an archived one, or one whose status is not
    /// `ready` or `in_progress` (a lapsed lease on a task that was already
    /// started is re-acquirable; one in review or done is not).
    pub(crate) fn require_acquirable(&self, task_id: &str) -> Result<()> {
        let unavailable = |code: &'static str, detail: String| StoreError::TaskUnavailable {
            task_id: task_id.to_string(),
            code,
            detail,
        };
        let found: Option<(String, i64)> = if items_present(&self.conn)? {
            self.conn
                .query_row(
                    "SELECT status, archived FROM work_items
                     WHERE id = ?1 AND deleted = 0 AND primary_repository_id = ?2",
                    params![task_id, workbench::LOCAL_REPOSITORY_ID],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
        } else {
            None
        };
        match found {
            None => Err(unavailable(
                "no_such_task",
                format!("task {task_id} is not a workbench item of this repository"),
            )),
            Some((_, archived)) if archived != 0 => Err(unavailable(
                "task_archived",
                format!("task {task_id} is archived and cannot be leased"),
            )),
            Some((status, _)) if status != "ready" && status != "in_progress" => Err(unavailable(
                "task_not_ready",
                format!(
                    "task {task_id} is {status}, not ready or in progress, and cannot be leased"
                ),
            )),
            Some(_) => Ok(()),
        }
    }
}
