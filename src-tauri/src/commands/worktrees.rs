//! Worktree lifecycle commands: list, add, remove, lock, merge teardown, routes, summaries.

use super::*;
use crate::engine::worktree::MergeTeardownError;

// ---------------------------------------------------------------------------
// Linked worktrees: how agents parallelize, so they are first-class here.
// ---------------------------------------------------------------------------

/// `depth` is what the caller reads, and absent means `listing`: one
/// `git worktree list`, every count "not scanned". Each level past it costs
/// git children per worktree — `dirty` one `git status` each, `full` adds a
/// diff of every changed worktree, divergence from main and routes — so a
/// caller asks for what it renders. The default used to be `full`, and a
/// caller that read only heads or dirty counts paid for all of it: 18
/// worktrees were 37 children per listing through an 8-a-second spawn budget.
#[tauri::command(async)]
pub async fn cmd_list_worktrees(
    repo_path: String,
    depth: Option<crate::engine::worktree::ScanDepth>,
) -> Result<Vec<WorktreeInfo>, IpcError> {
    // Past `listing` it spawns git per worktree (capped), so it belongs on
    // the blocking pool like every other reader.
    let depth = depth.unwrap_or(crate::engine::worktree::ScanDepth::Listing);
    off_thread(move || crate::engine::worktree::list_worktrees_scanned(&repo_path, depth)).await
}

/// Creates a linked worktree, after the command gate has judged the exact
/// `git worktree add` line this would run.
#[tauri::command(async)]
pub async fn cmd_add_worktree(
    repo_path: String,
    target_path: String,
    new_branch: Option<String>,
    start_point: Option<String>,
    detach: bool,
    cow_caches: Option<bool>,
    run_hooks: Option<bool>,
) -> Result<Guarded<crate::engine::worktree::WorktreeCreated>, IpcError> {
    off_thread(move || {
        // GitPulse's task-worktree container has one creator, the task
        // provisioner, which keeps it out of `git status` first.
        crate::engine::worktree::refuse_gitpulse_lane_target(&target_path)?;
        let argv_owned = crate::engine::worktree::add_worktree_argv(
            &target_path,
            new_branch.as_deref(),
            start_point.as_deref(),
            detach,
        );
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let cow = cow_caches.unwrap_or(false);
        let created = crate::engine::worktree::add_worktree_extended(
            &repo_path,
            &target_path,
            new_branch.as_deref(),
            start_point.as_deref(),
            detach,
            cow,
            // Off unless asked for; and even then only hooks a person
            // approved for this exact file run.
            run_hooks.unwrap_or(false),
        )?;
        Ok(Guarded {
            policy,
            output: created,
        })
    })
    .await
}

/// The repository's `.gitpulse/hooks` file as the approval dialog shows it:
/// its exact text, what it would run, and whether that exact text is approved.
/// `None` when the repository has no such file. Reads only.
#[tauri::command(async)]
pub async fn cmd_preview_worktree_hooks(
    repo_path: String,
) -> Result<Option<crate::engine::worktree_hooks::WorktreeHooksPreview>, IpcError> {
    off_thread(move || {
        let repo = crate::engine::git_cli::validate_repo(&repo_path)?;
        crate::engine::preview_worktree_hooks(&repo)
    })
    .await
}

/// Records consent to run the hooks file whose text is `content` — the text
/// the person was shown. Refused if the file on disk is no longer exactly that.
#[tauri::command(async)]
pub async fn cmd_approve_worktree_hooks(repo_path: String, content: String) -> Result<(), IpcError> {
    off_thread(move || {
        let repo = crate::engine::git_cli::validate_repo(&repo_path)?;
        crate::engine::approve_worktree_hooks(&repo, &content)
    })
    .await
}

/// Removes a linked worktree, gated like every other destructive write.
#[tauri::command(async)]
pub async fn cmd_remove_worktree(
    repo_path: String,
    target_path: String,
    force: bool,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<Guarded<crate::engine::worktree::WorktreeRemoval>, IpcError> {
    let workbench = workbench.inner().clone();
    off_thread(move || {
        remove_worktree_guarded(
            &workbench,
            &repo_path,
            &target_path,
            force,
            &|repo, argv| guard(repo, argv),
        )
    })
    .await
}

/// The command gate as a worktree teardown is handed it: the repository and
/// the full argv (program first). The command supplies [`guard`]; a test
/// supplies the harness gate without the user-action mark.
pub(crate) type WorktreeGate<'a> =
    &'a dyn Fn(&str, &[&str]) -> Result<crate::harness::PolicyVerdict, String>;

/// Body of [`cmd_remove_worktree`].
pub(crate) fn remove_worktree_guarded(
    workbench: &crate::workbench::WorkbenchState,
    repo_path: &str,
    target_path: &str,
    force: bool,
    gate: WorktreeGate<'_>,
) -> Result<Guarded<crate::engine::worktree::WorktreeRemoval>, String> {
    // A task run's agent, a dock tab's agent or any process may be working in
    // it; removal would pull its files out from under it.
    workbench.refuse_if_in_use(target_path)?;
    let argv_owned = crate::engine::worktree::remove_worktree_argv(target_path, force);
    let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
    let policy = gate(repo_path, &refs)?;
    let output = crate::engine::worktree::remove_worktree(repo_path, target_path, force)?;
    Ok(Guarded { policy, output })
}

#[tauri::command(async)]
pub async fn cmd_lock_worktree(
    repo_path: String,
    target_path: String,
    reason: Option<String>,
) -> Result<Guarded<()>, IpcError> {
    off_thread(move || {
        let argv_owned =
            crate::engine::worktree::lock_worktree_argv(&target_path, reason.as_deref());
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        crate::engine::worktree::lock_worktree(&repo_path, &target_path, reason.as_deref())?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_unlock_worktree(
    repo_path: String,
    target_path: String,
) -> Result<Guarded<()>, IpcError> {
    off_thread(move || {
        let argv_owned = crate::engine::worktree::unlock_worktree_argv(&target_path);
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        crate::engine::worktree::unlock_worktree(&repo_path, &target_path)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_prune_worktree(repo_path: String) -> Result<Guarded<()>, IpcError> {
    off_thread(move || {
        let argv_owned = crate::engine::worktree::prune_worktree_argv();
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        crate::engine::worktree::prune_worktree(&repo_path)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

/// Merges a worktree branch into target_branch (defaulting to main), tears down
/// the worktree cleanly, and prunes the merged branch.
///
/// `expected_tip` is the branch commit the caller verified: when it is given
/// and the branch is anywhere else once the merge holds its lock, nothing is
/// merged. The error is the typed [`MergeTeardownError`] (`{code, message,
/// ...}`), so a caller can tell a conflict from a refusal.
#[tauri::command(async)]
pub async fn cmd_worktree_merge_teardown(
    repo_path: String,
    worktree_path: String,
    target_branch: Option<String>,
    squash: Option<bool>,
    expected_tip: Option<String>,
    workbench: State<'_, crate::workbench::WorkbenchState>,
) -> Result<Guarded<crate::engine::worktree::MergeTeardownResult>, MergeTeardownError> {
    let workbench = workbench.inner().clone();
    // The pool carries `String` errors; the typed one rides inside the `Ok`.
    off_thread(move || {
        Ok(merge_teardown_guarded(
            &workbench,
            &repo_path,
            &worktree_path,
            target_branch.as_deref(),
            squash.unwrap_or(false),
            expected_tip.as_deref(),
            &|repo, argv| guard(repo, argv),
        ))
    })
    .await.map_err(|e| e.message)?
}

/// Body of [`cmd_worktree_merge_teardown`].
pub(crate) fn merge_teardown_guarded(
    workbench: &crate::workbench::WorkbenchState,
    repo_path: &str,
    worktree_path: &str,
    target_branch: Option<&str>,
    is_squash: bool,
    expected_tip: Option<&str>,
    judge: WorktreeGate<'_>,
) -> Result<Guarded<crate::engine::worktree::MergeTeardownResult>, MergeTeardownError> {
    // Before the merge, not only before the removal: merging a branch an
    // agent is still committing to takes half its work.
    workbench.refuse_if_in_use(worktree_path)?;
    // Each step is judged as it runs, with what runs (see the engine's
    // doc). The verdict reported is the merge's: the first mutation, and
    // the one the rest depend on.
    let mut policy = None;
    let mut gate = |args: &[&str]| -> Result<(), String> {
        let argv: Vec<&str> = std::iter::once("git").chain(args.iter().copied()).collect();
        let verdict = judge(repo_path, &argv)?;
        policy.get_or_insert(verdict);
        Ok(())
    };
    let result = crate::engine::worktree::merge_and_teardown_worktree(
        repo_path,
        worktree_path,
        target_branch,
        is_squash,
        expected_tip,
        &mut gate,
    )?;
    Ok(Guarded {
        policy: policy.ok_or_else(|| {
            MergeTeardownError::from(
                "The merge ran without being judged by the policy gate.".to_string(),
            )
        })?,
        output: result,
    })
}

/// Reflink-copies the repository's ignored dependency caches into one of its
/// worktrees. A cache that could not be copied whole fails the call.
#[tauri::command(async)]
pub async fn cmd_worktree_cow_sync(
    repo_path: String,
    target_path: String,
) -> Result<Vec<crate::engine::cow_clone::ReflinkResult>, IpcError> {
    off_thread(move || {
        let repo = crate::engine::validate_repo(&repo_path)?;
        // The target comes from the webview. Only a checkout git lists as a
        // worktree of this repository receives caches — one `git worktree
        // list`, no scan of any worktree. Any other existing directory used to
        // be filled with the anchor's caches and links.
        let family = crate::engine::worktree::resolve_worktree_family(&repo_path, &target_path)?;
        crate::engine::cow_clone::reflink_ignored_caches(
            &repo,
            &family.worktree,
            &crate::engine::cow_clone::CacheSelection::default(),
        )
    })
    .await
}

/// Discovers live portless and localhost dev server routes for a worktree.
#[tauri::command(async)]
pub async fn cmd_worktree_routes(
    worktree_path: String,
    branch: Option<String>,
) -> Result<Vec<crate::engine::portless::WorktreeRouteInfo>, IpcError> {
    off_thread(move || {
        Ok(crate::engine::portless::detect_worktree_routes(
            &worktree_path,
            branch.as_deref(),
        ))
    })
    .await
}

/// Provides a high-density 1-line diff & status summary of a worktree.
#[tauri::command(async)]
pub async fn cmd_worktree_ai_summary(
    repo_path: String,
    worktree_path: String,
    branch: Option<String>,
) -> Result<String, IpcError> {
    off_thread(move || worktree_ai_summary(&repo_path, &worktree_path, branch.as_deref())).await
}

/// Body of [`cmd_worktree_ai_summary`].
pub(super) fn worktree_ai_summary(
    repo_path: &str,
    worktree_path: &str,
    branch: Option<&str>,
) -> Result<String, String> {
    let wt = std::path::Path::new(worktree_path);
    if !wt.exists() {
        return Err("Worktree path does not exist".to_string());
    }
    // Each measurement below reads a failed git as "absent", and with all
    // three absent the summary used to say "Worktree clean". A read that
    // produced no answer fails the summary instead, with its own cause —
    // a deferral keeps its marker, so the call is asked again later.
    let failures = crate::engine::git_cli::process_failures();
    let diff_stat = crate::engine::worktree::measure_diff_stat(wt);
    let repo = crate::engine::validate_repo(repo_path).ok();
    let divergence = repo
        .as_ref()
        .and_then(|r| crate::engine::worktree::measure_main_divergence(r, branch));
    let log_out = git_text(wt, &["log", "-1", "--pretty=%s"]).unwrap_or_default();
    if crate::engine::git_cli::process_failures() != failures {
        return Err(crate::engine::git_cli::last_process_failure()
            .unwrap_or_else(|| "a git read for the worktree summary produced no answer".into()));
    }
    let last_msg = log_out.trim();

    let mut parts = Vec::new();
    if let Some(stat) = diff_stat {
        if stat.files_changed > 0 {
            parts.push(format!(
                "HEAD±: {} files (+{}, -{})",
                stat.files_changed, stat.insertions, stat.deletions
            ));
        } else {
            parts.push("HEAD±: clean".to_string());
        }
    }
    if let Some(div) = divergence {
        if div.ahead > 0 || div.behind > 0 {
            parts.push(format!("base↕: +{} -{}", div.ahead, div.behind));
        } else {
            parts.push("base↕: synced".to_string());
        }
    }
    if !last_msg.is_empty() {
        parts.push(format!("tip: \"{last_msg}\""));
    }
    if parts.is_empty() {
        Ok("Worktree clean".to_string())
    } else {
        Ok(parts.join(" · "))
    }
}
