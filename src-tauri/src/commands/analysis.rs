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

type DepsFlights =
    crate::singleflight::SingleFlight<String, Result<DepsHealthReport, CommandError>>;

/// A scan can spawn every auditor and wait on the registry, so a second panel
/// or a re-click while one runs must join it, not start another full audit of
/// the same repository.
fn deps_flights() -> &'static DepsFlights {
    static FLIGHTS: std::sync::OnceLock<DepsFlights> = std::sync::OnceLock::new();
    FLIGHTS.get_or_init(DepsFlights::default)
}

fn scan_deps_once<F>(
    flights: &DepsFlights,
    repo_path: &str,
    scan: F,
) -> Result<DepsHealthReport, CommandError>
where
    F: FnOnce() -> Result<DepsHealthReport, CommandError>,
{
    flights.run(repo_path.to_string(), scan)
}

#[tauri::command(async)]
pub async fn cmd_scan_deps_health(repo_path: String) -> Result<DepsHealthReport, IpcError> {
    off_thread(move || scan_deps_once(deps_flights(), &repo_path, || DepsScanner::scan(&repo_path)))
        .await
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

/// What the Storage panel's tuning apply would run for `changes`: each exact
/// argv with its scope, its undo and what the person must know first. Reads
/// files and the cached config only; refused on an untrusted repository and for
/// any change that is not currently offered.
#[tauri::command(async)]
pub async fn cmd_tuning_plan(
    repo_path: String,
    changes: Vec<crate::storage::repo_tuning::TuningChange>,
) -> Result<crate::storage::repo_tuning::TuningPlan, IpcError> {
    off_thread(move || crate::engine::tuning_apply::tuning_plan(&repo_path, &changes)).await
}

/// Applies recommended git tuning (owner decision 2026-11-06). Reachable only
/// from the Storage panel's confirm click: `confirmed_argv` is the previewed
/// list echoed back, refused unless it is exactly what a fresh plan runs, and
/// the command gate judges every one of those lines before any is spawned.
#[tauri::command(async)]
pub async fn cmd_tuning_apply(
    repo_path: String,
    changes: Vec<crate::storage::repo_tuning::TuningChange>,
    confirmed_argv: Vec<Vec<String>>,
) -> Result<Guarded<crate::storage::repo_tuning::TuningOutcome>, IpcError> {
    off_thread(move || {
        // Trust, and that these changes are still offered, before the gate.
        let runnable = crate::engine::tuning_apply::tuning_runnable(&repo_path, &changes)?;
        if confirmed_argv != runnable {
            return Err(format!(
                "The confirmed commands are not the previewed ones; nothing ran. Previewed: {}",
                runnable
                    .iter()
                    .map(|argv| argv.join(" "))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
            .into());
        }
        let mut policy = no_git_verdict();
        for argv in &runnable {
            policy = guard(&repo_path, &GitWriter::argv_refs(argv))?;
        }
        let output = crate::engine::tuning_apply::tuning_apply(&repo_path, &changes, &runnable)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Stops the tuning apply running in this repository's common directory.
/// `false` when none is running.
#[tauri::command(async)]
pub async fn cmd_tuning_apply_cancel(repo_path: String) -> Result<bool, IpcError> {
    off_thread(move || crate::engine::tuning_apply::tuning_apply_cancel(&repo_path)).await
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

#[cfg(test)]
mod deps_flight_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn failing() -> Result<DepsHealthReport, CommandError> {
        Err(CommandError::from("scan stub".to_string()))
    }

    /// The defect: every caller ran its own full audit of the repository.
    #[test]
    fn concurrent_scans_of_one_repository_share_one_audit() {
        let flights = Arc::new(DepsFlights::default());
        let runs = Arc::new(AtomicUsize::new(0));
        let leader = {
            let (flights, runs) = (Arc::clone(&flights), Arc::clone(&runs));
            std::thread::spawn(move || {
                scan_deps_once(&flights, "/repo", || {
                    runs.fetch_add(1, Ordering::SeqCst);
                    // Wait for the second caller to either join this scan or
                    // (the defect) start a scan of its own, so a regression
                    // fails the count below instead of hanging here.
                    while flights.followers(&"/repo".to_string()) == 0
                        && runs.load(Ordering::SeqCst) == 1
                    {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    failing()
                })
            })
        };
        while runs.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let follower = scan_deps_once(&flights, "/repo", || {
            runs.fetch_add(1, Ordering::SeqCst);
            failing()
        });
        assert!(follower.is_err());
        assert!(leader.join().unwrap().is_err());
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn different_repositories_scan_independently() {
        let flights = DepsFlights::default();
        let runs = AtomicUsize::new(0);
        for repo in ["/a", "/b"] {
            let _ = scan_deps_once(&flights, repo, || {
                runs.fetch_add(1, Ordering::SeqCst);
                failing()
            });
        }
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }
}
