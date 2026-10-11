//! Ledger, task binding and catch-up commands.

use super::*;

// --- the action ledger -------------------------------------------------

/// Reads durable ledger events after `cursor`, oldest first. With `newest`,
/// the newest `limit` of them, so a reader that keeps only recent rows takes
/// one request rather than paging the whole history.
///
/// The frontend's action list is a projection of this, not a store of its own:
/// it holds the last cursor it saw and asks for what followed. That is what
/// makes the history survive a reload, a crash, and a restart — and what makes
/// "everything visible is a projection of the log" true rather than aspirational.
#[tauri::command(async)]
pub async fn cmd_ledger_tail(
    repo_path: String,
    cursor: i64,
    limit: u32,
    newest: Option<bool>,
) -> Result<Vec<crate::ledger::LedgerEvent>, String> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        let address = crate::ledger::bindings::repository_address(&repo.to_string_lossy())
            .map_err(|e| e.to_string())?;
        let read = if newest.unwrap_or(false) {
            crate::ledger::tail_newest
        } else {
            crate::ledger::tail
        };
        read(&address.anchor, cursor, limit).map_err(|e| e.to_string())
    })
    .await
}

/// Reports whether the ledger is recording for this repository.
///
/// Separate from the events themselves on purpose. A repository whose ledger
/// cannot be opened returns an empty history, and an empty history is exactly
/// what a repository with nothing in it returns; without this, the two would be
/// the same picture.
#[tauri::command(async)]
pub async fn cmd_ledger_status(repo_path: String) -> Result<crate::ledger::LedgerStatus, String> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        crate::ledger::bindings::repository_status(&repo.to_string_lossy())
            .map_err(|e| e.to_string())
    })
    .await
}

// --- task binding ------------------------------------------------------

/// DevCouncil tasks currently leased in this repository.
///
/// Read-only. GitPulse never acquires, renews or releases a lease: those
/// contend with an active agent's writer lease, and a UI process that takes one
/// strands the task when the window closes.
#[tauri::command(async)]
pub async fn cmd_task_view(repo_path: String) -> Result<crate::tasks::TaskView, String> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        let address = crate::ledger::bindings::repository_address(&repo.to_string_lossy())
            .map_err(|e| e.to_string())?;
        Ok(crate::tasks::view(&address.anchor))
    })
    .await
}

/// The scopes several tasks declare. A task the store does not hold, or a
/// repository with no store, is simply absent from the list.
///
/// One call for the whole set, because finding the store authenticates the
/// repository with a `git worktree list`: a per-task command started one git
/// process for every task title the Work view showed, on every refresh.
#[tauri::command(async)]
pub async fn cmd_task_scopes(
    repo_path: String,
    task_ids: Vec<String>,
) -> Result<Vec<crate::tasks::TaskScope>, String> {
    off_thread(move || task_scopes(&repo_path, &task_ids)).await
}

pub(super) fn task_scopes(
    repo_path: &str,
    task_ids: &[String],
) -> Result<Vec<crate::tasks::TaskScope>, String> {
    let repo = validate_repo(repo_path)?;
    if task_ids.is_empty() {
        return Ok(Vec::new());
    }
    let address = crate::ledger::bindings::repository_address(&repo.to_string_lossy())
        .map_err(|e| e.to_string())?;
    crate::tasks::scopes(&address.anchor, task_ids)
}

/// Binds a worktree to a task, so every later mutation in it is judged against
/// that task's plan.
///
/// Recorded as a ledger event rather than a setting, so a later audit can say
/// which task a commit was made under — not merely which task the worktree is
/// bound to now.
#[tauri::command(async)]
pub async fn cmd_bind_worktree_task(
    repo_path: String,
    worktree_path: String,
    task_id: String,
) -> Result<i64, String> {
    off_thread(move || {
        crate::ledger::bindings::bind(&repo_path, &worktree_path, &task_id)
            .map_err(|e| e.to_string())
    })
    .await
}

/// Clears a worktree's task binding. The bind stays on the record.
#[tauri::command(async)]
pub async fn cmd_unbind_worktree_task(
    repo_path: String,
    worktree_path: String,
) -> Result<i64, String> {
    off_thread(move || {
        crate::ledger::bindings::unbind(&repo_path, &worktree_path).map_err(|e| e.to_string())
    })
    .await
}

/// The task a worktree is bound to, or `null`.
#[tauri::command(async)]
pub async fn cmd_worktree_task(
    repo_path: String,
    worktree_path: String,
) -> Result<Option<String>, String> {
    off_thread(move || {
        crate::ledger::bindings::resolve(&repo_path, &worktree_path).map_err(|e| e.to_string())
    })
    .await
}

// --- catching up ------------------------------------------------------

/// Replays what happened while GitPulse was closed.
///
/// Two sources, both observed rather than self-reported: git's reflog, which is
/// authoritative for ref movements and survives GitPulse being uninstalled, and
/// agent transcripts, which attribute file edits and commands to a session.
/// Both replays are idempotent, so this is safe to run on every repo open.
#[tauri::command(async)]
pub async fn cmd_catch_up(repo_path: String) -> Result<crate::ingest::CatchUp, String> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        let address = crate::ledger::bindings::repository_address(&repo.to_string_lossy())
            .map_err(|e| e.to_string())?;
        Ok(crate::ingest::catch_up_into(
            &address.anchor,
            &address.worktree,
        ))
    })
    .await
}
