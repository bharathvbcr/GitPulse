//! Remote commands: clone, identity, fetch, pull, push, remotes, submodules, repository operations.

use super::*;

/// One step of a clone, sent as the `clone-progress` event while
/// [`cmd_clone_repo`] runs. `id` is the caller's, so a window can tell its
/// clone's progress from another's.
#[derive(Clone, Serialize)]
pub(super) struct CloneProgressEvent {
    id: String,
    #[serde(flatten)]
    progress: CloneProgress,
}

#[tauri::command(async)]
pub async fn cmd_clone_repo(
    app: AppHandle,
    url: String,
    target_dir: String,
    options: Option<CloneOptions>,
    progress_id: Option<String>,
) -> Result<String, IpcError> {
    off_thread(move || {
        use tauri::Emitter;
        let options = options.unwrap_or_default();
        let mut on_progress = |progress: &CloneProgress| {
            if let Some(id) = &progress_id {
                let event = CloneProgressEvent {
                    id: id.clone(),
                    progress: progress.clone(),
                };
                if let Err(error) = app.emit("clone-progress", event) {
                    log::debug!("clone progress event not delivered: {error}");
                }
            }
        };
        GitWriter::clone_repo_with(&url, &target_dir, &options, &mut on_progress)
    })
    .await
}

/// Whether a usable git is installed: missing, older than 2.36, or broken,
/// each with a message the user can act on. Asked once at startup.
#[tauri::command(async)]
pub async fn cmd_git_preflight() -> Result<crate::engine::git_cli::GitPreflight, IpcError> {
    off_thread(|| Ok(crate::engine::git_cli::git_preflight())).await
}

/// Stops the hook-running git command (commit, merge, rebase, …) in flight
/// in this repository, its hook included. Returns how many were stopped;
/// zero when nothing was running, which the UI says rather than pretends.
#[tauri::command(async)]
pub async fn cmd_cancel_git_hooks(repo_path: String) -> Result<usize, IpcError> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        Ok(crate::engine::git_cli::cancel_hooked_git(&repo))
    })
    .await
}

/// The name and email a commit here would be recorded under.
#[tauri::command(async)]
pub async fn cmd_git_identity(repo_path: String) -> Result<GitIdentity, IpcError> {
    off_thread(move || GitWriter::identity(&repo_path)).await
}

/// Sets `user.name` and `user.email` for this repository or globally, after
/// the gate has judged both `git config` lines.
#[tauri::command(async)]
pub async fn cmd_set_git_identity(
    repo_path: String,
    name: String,
    email: String,
    scope: IdentityScope,
) -> Result<Guarded<GitIdentity>, IpcError> {
    off_thread(move || {
        GitWriter::validate_identity(&name, &email)?;
        let fold = VerdictFold::default();
        for argv in GitWriter::identity_argv(scope, name.trim(), email.trim()) {
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            fold.record(guard(&repo_path, &refs)?);
        }
        let output = GitWriter::set_identity(&repo_path, &name, &email, scope)?;
        Ok(Guarded {
            policy: fold.finish().unwrap_or_else(no_git_verdict),
            output,
        })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_fetch(
    repo_path: String,
    remote: Option<String>,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        let argv: Vec<String> = match remote.as_deref() {
            Some(remote) => vec!["git".into(), "fetch".into(), remote.into()],
            None => vec![
                "git".into(),
                "fetch".into(),
                "--all".into(),
                "--prune".into(),
            ],
        };
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = GitWriter::fetch(&repo_path, remote.as_deref())?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Fetches on the opt-in auto-fetch timer. Judged by the gate like any
/// fetch, but not as a user action: it runs as background work the spawn
/// gate sheds under load, and it skips a repository another command holds.
#[tauri::command(async)]
pub async fn cmd_auto_fetch(repo_path: String) -> Result<Guarded<AutoFetchOutcome>, IpcError> {
    off_thread(move || {
        let policy = guard_background(&repo_path, &crate::engine::git_writer::AUTO_FETCH_ARGV)?;
        let output = GitWriter::auto_fetch(&repo_path)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// `pull.rebase` as configured for this repository, or `None` when unset.
#[tauri::command(async)]
pub async fn cmd_pull_rebase_config(repo_path: String) -> Result<Option<String>, IpcError> {
    off_thread(move || GitWriter::pull_rebase_config(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_pull(
    repo_path: String,
    remote: Option<String>,
    branch: Option<String>,
    rebase: Option<bool>,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        // A pull merges or rebases into the working tree, which is what the
        // write gate exists to protect. The judged argv is the one
        // GitWriter::pull runs: both come from pull_argv.
        let argv: Vec<&str> = std::iter::once("git")
            .chain(GitWriter::pull_argv(
                remote.as_deref(),
                branch.as_deref(),
                rebase,
            ))
            .collect();
        let policy = guard(&repo_path, &argv)?;
        let output = GitWriter::pull(&repo_path, remote.as_deref(), branch.as_deref(), rebase)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// The line the command gate judges for a planned push: `git` plus exactly the
/// vec [`GitWriter::push_planned`] runs.
fn judged_push_argv(args: &[String]) -> Vec<&str> {
    std::iter::once("git")
        .chain(args.iter().map(String::as_str))
        .collect()
}

/// Pushes, after the command gate has judged it. A force push is refused by
/// the harness's hard rules, which is the whole reason this call is gated.
///
/// Judged argv is the executed argv: [`GitWriter::plan_push`] is the single
/// planner, and [`GitWriter::push_planned`] runs that same vec. An unpublished
/// branch therefore cannot be judged as a bare `git push` and then fail
/// with git's "no upstream" fatal — the gate sees `-u <remote> <branch>`
/// when that is what will run. A force push judges as `--force-with-lease
/// --force-if-includes`; `--force-with-lease` is still the force flag the
/// harness's `command.force_push` hard rule catches.
#[tauri::command(async)]
pub async fn cmd_push(
    repo_path: String,
    remote: Option<String>,
    branch: Option<String>,
    force: Option<bool>,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        let force = force.unwrap_or(false);
        // A user action from here on, so the version probe a force push makes
        // while planning is admitted rather than shed under load; the command
        // guard would only mark it after the plan.
        crate::engine::git_cli::mark_user_action(&repo_path);
        let args = GitWriter::plan_push(&repo_path, remote.as_deref(), branch.as_deref(), force)?;
        let argv = judged_push_argv(&args);
        let policy = guard(&repo_path, &argv)?;
        let output = GitWriter::push_planned(&repo_path, &args)?;
        Ok(Guarded { policy, output })
    })
    .await
}

// --- remotes -----------------------------------------------------------

/// Lists configured remotes with their fetch/push URLs. Read-only and ungated.
#[tauri::command(async)]
pub async fn cmd_list_remotes(repo_path: String) -> Result<RemoteList, IpcError> {
    off_thread(move || crate::engine::remotes::list(&repo_path)).await
}

/// Adds, removes, renames, repoints, or prunes a remote.
///
/// The gate runs inside the engine, under the repository lock and against the
/// argv that executes — the same discipline every other mutating path follows.
#[tauri::command(async)]
pub async fn cmd_remote_change(
    repo_path: String,
    change: RemoteChange,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        let (policy, output) = crate::engine::remotes::apply_with(&repo_path, &change, |argv| {
            guard(&repo_path, argv)
        })?;
        Ok(Guarded { policy, output })
    })
    .await
}

// --- submodules --------------------------------------------------------

/// Lists embedded submodules and whether each is usable. Read-only and ungated.
#[tauri::command(async)]
pub async fn cmd_list_submodules(repo_path: String) -> Result<SubmoduleList, IpcError> {
    off_thread(move || crate::engine::submodules::list(&repo_path)).await
}

/// Initializes, syncs, or deinitializes submodules.
#[tauri::command(async)]
pub async fn cmd_submodule_change(
    repo_path: String,
    change: SubmoduleChange,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        let (policy, output) =
            crate::engine::submodules::apply_with(&repo_path, &change, |argv| {
                guard(&repo_path, argv)
            })?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Reports the multi-step git operation this worktree is parked in, if any.
///
/// Read-only and ungated: it runs on every status refresh, and putting a
/// sidecar round trip in front of a background read would buy no decision.
#[tauri::command(async)]
pub async fn cmd_repo_operation(repo_path: String) -> Result<Option<RepoOperation>, IpcError> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        crate::engine::repo_op::detect(&repo)
    })
    .await
}

/// mtime of `FETCH_HEAD` in the shared git dir (worktrees share one), as epoch ms.
pub(crate) fn last_fetch_at_ms(repo: &std::path::Path) -> Result<Option<i64>, String> {
    use crate::engine::git_cli::resolve_git_common_dir;
    use std::time::UNIX_EPOCH;

    let common = resolve_git_common_dir(repo)?;
    let fetch_head = common.join("FETCH_HEAD");
    match std::fs::metadata(&fetch_head) {
        Ok(meta) => {
            let modified = meta
                .modified()
                .map_err(|error| format!("Could not read FETCH_HEAD mtime: {error}"))?;
            let millis = modified
                .duration_since(UNIX_EPOCH)
                .map_err(|error| format!("FETCH_HEAD mtime is before the epoch: {error}"))?
                .as_millis();
            Ok(Some(millis as i64))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Could not read FETCH_HEAD: {error}")),
    }
}

/// The user switched to this repository's tab: credit the hydrate that follows
/// like the user action it is, so it is not deferred behind background
/// refreshes. Trust-checked like any read; runs no Git.
#[tauri::command(async)]
pub async fn cmd_note_tab_activated(repo_path: String) -> Result<(), IpcError> {
    off_thread(move || crate::engine::git_cli::note_tab_activated(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_last_fetch_at(repo_path: String) -> Result<Option<i64>, IpcError> {
    off_thread(move || {
        let repo = validate_repo(&repo_path)?;
        last_fetch_at_ms(&repo)
    })
    .await
}

/// Aborts, continues, or skips the parked operation.
///
/// The gate judges the argv that [`crate::engine::repo_op::action_argv`]
/// renders, and `run_action` re-renders the same line from the same function
/// under the repo lock — so the judged line is the executed line. The kind is
/// re-detected on both sides rather than trusted from the client: a stale UI
/// asking to abort a rebase that has since become a cherry-pick must not send
/// `git rebase --abort`.
#[tauri::command(async)]
pub async fn cmd_repo_operation_action(
    repo_path: String,
    action: OperationAction,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        // The gate runs inside `run_action_with`, under the repository lock and
        // against the same argv that executes. Detecting and judging out here
        // would let a concurrent abort change the operation in between, so the
        // gate would approve `git rebase --abort` while a cherry-pick abort is
        // what actually ran.
        let (policy, output) =
            crate::engine::repo_op::run_action_with(&repo_path, action, |argv| {
                guard(&repo_path, argv)
            })?;
        Ok(Guarded { policy, output })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git_in, git_repo};

    /// The command gate judges `git` plus the planned vec, and for a force push
    /// that vec carries the lease and includes pair that push_planned runs.
    #[test]
    fn the_gate_judges_the_argv_that_runs_including_the_force_pair() {
        let repo = git_repo();
        git_in(
            repo.path(),
            &["remote", "add", "origin", "https://example.invalid/x.git"],
        );
        let path = repo.path().to_string_lossy().into_owned();
        let args = GitWriter::plan_push(&path, Some("origin"), Some("main"), true).unwrap();
        let judged = judged_push_argv(&args);
        assert_eq!(judged[0], "git");
        assert_eq!(
            judged[1..],
            args.iter().map(String::as_str).collect::<Vec<_>>()[..]
        );
        assert!(judged.contains(&"--force-with-lease") && judged.contains(&"--force-if-includes"));
        let plain = GitWriter::plan_push(&path, Some("origin"), Some("main"), false).unwrap();
        assert_eq!(judged_push_argv(&plain), ["git", "push", "origin", "main"]);
    }
}

#[cfg(test)]
#[path = "remote_adv_l1_tests.rs"]
mod adv_l1_tests;
