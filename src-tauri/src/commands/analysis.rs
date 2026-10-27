//! Repository analysis: pulse, coverage, dependency health, secrets, storage, hygiene.

use super::*;
use crate::ipc_error::CommandError;

#[tauri::command(async)]
pub async fn cmd_get_pulse_report(
    repo_path: String,
    max_commits: Option<usize>,
) -> Result<PulseReport, IpcError> {
    off_thread(move || GitReader::pulse_report(&repo_path, max_commits)).await
}

#[tauri::command(async)]
pub async fn cmd_get_knowledge_report(
    repo_path: String,
    max_files: Option<usize>,
) -> Result<KnowledgeReport, IpcError> {
    off_thread(move || GitReader::knowledge_report(&repo_path, max_files)).await
}

#[tauri::command(async)]
pub async fn cmd_get_dora_report(
    repo_path: String,
    window_days: Option<u32>,
) -> Result<DoraReport, IpcError> {
    off_thread(move || GitReader::dora_report(&repo_path, window_days)).await
}

#[tauri::command(async)]
pub async fn cmd_record_pulse_snapshot(
    repo_path: String,
    snapshot: crate::ledger::PulseSnapshotInput,
) -> Result<(), IpcError> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        let address = crate::ledger::bindings::repository_address(&repo.to_string_lossy())
            .map_err(|error| error.to_string())?;
        crate::ledger::save_pulse_snapshot(&address.anchor, &snapshot)
            .map_err(|error| error.to_string())
            .map_err(CommandError::from)
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_get_pulse_snapshots(
    repo_path: String,
    limit: Option<usize>,
) -> Result<Vec<crate::ledger::PulseSnapshotEntry>, IpcError> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        let address = crate::ledger::bindings::repository_address(&repo.to_string_lossy())
            .map_err(|error| error.to_string())?;
        crate::ledger::get_pulse_snapshots(&address.anchor, limit)
            .map_err(|error| error.to_string())
            .map_err(CommandError::from)
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_scan_coverage(repo_path: String) -> Result<CoverageReport, IpcError> {
    off_thread(move || CoverageScanner::scan(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_get_file_coverage(
    repo_path: String,
    file_path: String,
) -> Result<FileCoverage, IpcError> {
    off_thread(move || CoverageScanner::file_coverage(&repo_path, &file_path)).await
}

#[tauri::command(async)]
pub async fn cmd_scan_deps_health(repo_path: String) -> Result<DepsHealthReport, IpcError> {
    off_thread(move || DepsScanner::scan(&repo_path)).await
}

/// On-demand Kingfisher secrets scan of the working tree.
///
/// Kingfisher stdout is parsed in memory and dropped — never logged. A missed
/// or truncated scan returns `ok: false` and is never implied clean.
#[tauri::command(async)]
pub async fn cmd_scan_secrets(
    repo_path: String,
) -> Result<crate::secrets::SecretsReport, IpcError> {
    off_thread(move || crate::secrets::scan_secrets(&repo_path)).await
}

/// Full disk-usage scan of the repository (git internals, build/cache
/// artifacts, large files, worktrees, stale-branch weight). The walk is
/// budgeted and never follows symlinks; see `crate::storage`.
#[tauri::command(async)]
pub async fn cmd_storage_scan(
    repo_path: String,
) -> Result<crate::storage::StorageReport, IpcError> {
    off_thread(move || crate::storage::scan_storage(&repo_path)).await
}

/// What the Storage panel's "Write commit-graph" click would run: the exact
/// argv and the common directory it writes under. Reads files only; refused
/// when no write is offered (graph present and fresh, no history, state
/// unknown) and on an untrusted repository.
#[tauri::command(async)]
pub async fn cmd_commit_graph_write_plan(
    repo_path: String,
) -> Result<crate::storage::commit_graph::CommitGraphWritePlan, IpcError> {
    off_thread(move || GitWriter::commit_graph_write_plan(&repo_path)).await
}

/// Writes the commit-graph (Owner Decision 3). Reachable only from the Storage
/// panel's confirm click: `confirmed_argv` is the previewed line echoed back,
/// refused unless it is the one line the writer would run, and the gate judges
/// that same line before it is spawned.
#[tauri::command(async)]
pub async fn cmd_commit_graph_write(
    repo_path: String,
    confirmed_argv: Vec<String>,
) -> Result<Guarded<crate::storage::commit_graph::CommitGraphWriteOutcome>, IpcError> {
    off_thread(move || {
        // Trust, and that a write is still offered, before the gate is asked.
        GitWriter::commit_graph_write_plan(&repo_path)?;
        let argv = GitWriter::commit_graph_write_argv();
        if confirmed_argv != argv {
            return Err(format!(
                "The confirmed command `{}` is not the previewed `{}`; nothing ran",
                confirmed_argv.join(" "),
                argv.join(" ")
            )
            .into());
        }
        let policy = guard(&repo_path, &GitWriter::argv_refs(&argv))?;
        let output = GitWriter::commit_graph_write(&repo_path, &argv)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Stops the graph write running in this repository's common
/// directory. `false` when none is running.
#[tauri::command(async)]
pub async fn cmd_commit_graph_write_cancel(repo_path: String) -> Result<bool, IpcError> {
    off_thread(move || GitWriter::commit_graph_write_cancel(&repo_path)).await
}

#[tauri::command]
pub async fn cmd_cache_inventory() -> Result<crate::storage::hygiene::CacheInventory, IpcError> {
    off_thread(crate::storage::hygiene::cache_inventory).await
}

#[tauri::command]
pub async fn cmd_hygiene_prepare(
    repo_path: String,
    target: String,
    min_age_days: u32,
) -> Result<crate::storage::hygiene::HygienePlan, IpcError> {
    off_thread(move || crate::storage::hygiene::prepare(&repo_path, &target, min_age_days)).await
}

#[tauri::command]
pub async fn cmd_hygiene_cancel(repo_path: String, plan_id: String) -> Result<(), IpcError> {
    off_thread(move || crate::storage::hygiene::cancel(&repo_path, &plan_id)).await
}

#[tauri::command]
pub async fn cmd_hygiene_execute(
    repo_path: String,
    plan_id: String,
) -> Result<Guarded<crate::storage::hygiene::HygieneOutcome>, IpcError> {
    off_thread(move || {
        let (policy, output) =
            crate::storage::hygiene::execute(&repo_path, &plan_id, |argv, path| match argv {
                Some(argv) => {
                    let command_policy = guard(
                        &repo_path,
                        &argv.iter().map(String::as_str).collect::<Vec<_>>(),
                    )?;
                    let file_policy = guard_file(&repo_path, path, "delete")?;
                    Ok(strictest_verdict(command_policy, file_policy))
                }
                None => guard_file(&repo_path, path, "delete").map_err(CommandError::from),
            })?;
        Ok(Guarded { policy, output })
    })
    .await
}
