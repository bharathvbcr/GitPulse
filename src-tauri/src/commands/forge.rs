//! GitHub, Firebase and local CI commands.

use super::*;

#[tauri::command(async)]
pub async fn cmd_github_context(repo_path: String) -> GitHubContext {
    // `load_github_context` shells out to `gh` (up to 45s per call), so it
    // must not occupy an async worker thread. A pool failure surfaces the
    // same degraded context a missing CLI would.
    off_thread(move || Ok::<_, String>(load_github_context(&repo_path)))
        .await
        .map_err(|e| e.message).unwrap_or_else(|e| GitHubContext {
            available: false,
            cli_present: false,
            host: String::new(),
            owner: String::new(),
            repo: String::new(),
            html_url: String::new(),
            pull_requests: Vec::new(),
            prs_truncated: false,
            workflow_runs: Vec::new(),
            runs_error: None,
            runs_truncated: false,
            error: Some(e),
            issues: Vec::new(),
            issues_error: None,
            issues_truncated: false,
            releases: Vec::new(),
            releases_error: None,
            releases_truncated: false,
            warnings: Vec::new(),
        })
}

/// The workflow-run listing alone, for the live delivery poll.
///
/// One `gh` call where [`cmd_github_context`] makes four, because this one is
/// on a repeating timer while a run is in flight. Like every other report
/// here it carries its own failure rather than rejecting, and `checked:
/// false` is what stops a failed poll from rendering as "no runs".
#[tauri::command(async)]
pub async fn cmd_github_runs(repo_path: String) -> crate::github::GitHubRunsReport {
    off_thread(move || Ok::<_, String>(load_workflow_runs_report(&repo_path)))
        .await
        .map_err(|e| e.message).unwrap_or_else(|e| crate::github::GitHubRunsReport {
            available: false,
            checked: false,
            cli_present: false,
            owner: String::new(),
            repo: String::new(),
            runs: Vec::new(),
            truncated: false,
            error: Some(e),
        })
}

/// Open Dependabot alerts for the Health view, via `gh api`. Like
/// [`cmd_github_context`], the report carries its own error state instead of
/// rejecting: "could not check" must stay distinct from "no alerts".
#[tauri::command(async)]
pub async fn cmd_github_dependabot_alerts(repo_path: String) -> DependabotReport {
    off_thread(move || Ok::<_, String>(load_dependabot_alerts(&repo_path)))
        .await
        .map_err(|e| e.message).unwrap_or_else(DependabotReport::transport_failure)
}

/// Open GitHub code scanning alerts for the Health view, via `gh api`. Like
/// [`cmd_github_dependabot_alerts`], the report carries its own error state
/// instead of rejecting: "could not check" must stay distinct from "no alerts".
#[tauri::command(async)]
pub async fn cmd_github_code_scanning_alerts(repo_path: String) -> CodeScanningReport {
    off_thread(move || Ok::<_, String>(load_code_scanning_alerts(&repo_path)))
        .await
        .map_err(|e| e.message).unwrap_or_else(CodeScanningReport::transport_failure)
}

#[tauri::command(async)]
pub async fn cmd_github_create_issue(
    repo_path: String,
    title: String,
    body: String,
    labels: Vec<String>,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        // Validate before anything external runs, then judge the exact argv
        // create_issue executes — built once by the shared `issue_create_argv`
        // builder (program name included) and consumed by both the gate and
        // the executor, so the judged line can never drift from the run one.
        validate_issue_payload(&title, &body, &labels)?;
        let remote = discover_github_remote(&repo_path)?
            .ok_or_else(|| "No GitHub remote configured".to_string())?;
        let argv_owned = issue_create_argv(&remote, &title, &body, &labels);
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = create_issue(&repo_path, &remote, &title, &body, &labels)?;
        Ok(Guarded { policy, output })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_github_checkout_pr(
    repo_path: String,
    number: u64,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        // pr_checkout_argv refuses a fabricated number before discovery, so
        // an invalid request never produces a judged-but-unexecutable line.
        // One remote discovery feeds both the gate and the executor: the
        // `--repo` the gate approved is exactly the one gh is pinned to.
        let remote = discover_github_remote(&repo_path)?
            .ok_or_else(|| "No GitHub remote configured".to_string())?;
        let argv_owned = pr_checkout_argv(&remote, number)?;
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = checkout_pull_request(&repo_path, &remote, number)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// One pull request in detail (`gh pr view`). Read-only and ungated, like the
/// listings in [`cmd_github_context`].
#[tauri::command(async)]
pub async fn cmd_github_pr_view(
    repo_path: String,
    number: u64,
) -> Result<crate::github::PullRequestDetail, IpcError> {
    off_thread(move || crate::github::view_pull_request(&repo_path, number)).await
}

/// Creates, reviews or merges a pull request through gh. Each is published
/// on GitHub, so the frontend confirms first. The argv is built once, judged,
/// and that same argv is what runs.
#[tauri::command(async)]
pub async fn cmd_github_pr_action(
    repo_path: String,
    action: crate::github::PrAction,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        let remote = discover_github_remote(&repo_path)?
            .ok_or_else(|| "No GitHub remote configured".to_string())?;
        let head = match action {
            crate::github::PrAction::Create { .. } => GitReader::current_branch(&repo_path)?,
            _ => None,
        };
        let argv = crate::github::pr_action_argv(&remote, &action, head.as_deref())?;
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = crate::github::run_pr_action(&repo_path, &argv)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// The repository's Actions workflows (`gh workflow list`). A separate,
/// lazily-loaded command rather than another section of
/// [`cmd_github_context`]: that call already serializes four gh round trips,
/// and the workflows list is only needed on its own panel.
#[tauri::command(async)]
pub async fn cmd_github_workflows(repo_path: String) -> crate::github::actions::WorkflowsReport {
    off_thread(move || Ok::<_, String>(crate::github::actions::load_workflows_report(&repo_path)))
        .await
        .map_err(|e| e.message).unwrap_or_else(|e| crate::github::actions::WorkflowsReport::unavailable(false, Some(e)))
}

/// Dispatches a workflow (`gh workflow run <selector> --ref <ref>`), after
/// the command gate has judged the exact line. The selector is the workflow's
/// file path from the listing; only dispatchable workflows succeed upstream.
#[tauri::command(async)]
pub async fn cmd_github_trigger_workflow(
    repo_path: String,
    workflow: String,
    git_ref: String,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        let remote = discover_github_remote(&repo_path)?
            .ok_or_else(|| "No GitHub remote configured".to_string())?;
        let argv_owned =
            crate::github::actions::trigger_workflow_argv(&remote, &workflow, &git_ref)?;
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output =
            crate::github::actions::trigger_workflow(&repo_path, &remote, &workflow, &git_ref)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Re-runs a workflow run (`gh run rerun <id>`), gated like every action.
#[tauri::command(async)]
pub async fn cmd_github_rerun_run(
    repo_path: String,
    run_id: u64,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        let remote = discover_github_remote(&repo_path)?
            .ok_or_else(|| "No GitHub remote configured".to_string())?;
        let argv_owned = crate::github::actions::rerun_run_argv(&remote, run_id)?;
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = crate::github::actions::rerun_workflow_run(&repo_path, &remote, run_id)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Cancels an in-flight workflow run (`gh run cancel <id>`), gated.
#[tauri::command(async)]
pub async fn cmd_github_cancel_run(
    repo_path: String,
    run_id: u64,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        let remote = discover_github_remote(&repo_path)?
            .ok_or_else(|| "No GitHub remote configured".to_string())?;
        let argv_owned = crate::github::actions::cancel_run_argv(&remote, run_id)?;
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = crate::github::actions::cancel_workflow_run(&repo_path, &remote, run_id)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// What the working tree says about this repository's Firebase setup.
///
/// Reads `.firebaserc` and `firebase.json` plus a bounded `firebase --version`
/// probe. No network, no credential, no possible mutation — so unlike the
/// listings below it is free to run on panel mount, and it returns a report
/// rather than a `Result`: "this repository does not use Firebase" is an
/// answer, not a failure.
#[tauri::command(async)]
pub async fn cmd_firebase_status(repo_path: String) -> crate::firebase::FirebaseStatus {
    off_thread(move || Ok::<_, String>(crate::firebase::discover_firebase_projects(&repo_path)))
        .await
        // A thread-pool failure is not "no Firebase config": it is a status that
        // could not be read, and it says so rather than rendering as a repository
        // that does not use Firebase.
        .map_err(|e| e.message).unwrap_or_else(crate::firebase::FirebaseStatus::unreadable)
}

/// Lists App Hosting backends for one project.
///
/// Returns `Guarded` because this read can mutate: upstream runs
/// `ensureApiEnabled` before the listing, which enables the App Hosting API on
/// the user's Cloud project when it is off. In this codebase `Guarded<T>` *is*
/// the declaration that a command mutates, and `command-policy-contract`
/// enforces that anything wearing it calls the write gate — so the type is the
/// honest one even though the verb reads like a query.
#[tauri::command(async)]
pub async fn cmd_firebase_backends(
    repo_path: String,
    project_id: String,
) -> Result<Guarded<crate::firebase::apphosting::FirebaseBackendsReport>, IpcError> {
    off_thread(move || {
        let probe = crate::firebase::firebase_cli_probe();
        if !probe.present {
            return Err(probe
                .reason
                .unwrap_or_else(|| "The Firebase CLI is not available".to_string()));
        }
        let argv_owned = crate::firebase::apphosting::backends_list_argv(&project_id)?;
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = crate::firebase::apphosting::load_backends_report(
            &repo_path,
            &project_id,
            &argv_owned,
            probe.present,
        );
        Ok(Guarded { policy, output })
    })
    .await
}

/// Lists rollouts for one App Hosting backend, joined to local commits.
///
/// Gated for the same reason as [`cmd_firebase_backends`].
#[tauri::command(async)]
pub async fn cmd_firebase_rollouts(
    repo_path: String,
    project_id: String,
    backend_id: String,
) -> Result<Guarded<crate::firebase::apphosting::FirebaseRolloutsReport>, IpcError> {
    off_thread(move || {
        let probe = crate::firebase::firebase_cli_probe();
        if !probe.present {
            return Err(probe
                .reason
                .unwrap_or_else(|| "The Firebase CLI is not available".to_string()));
        }
        let argv_owned = crate::firebase::apphosting::rollouts_list_argv(&project_id, &backend_id)?;
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = crate::firebase::apphosting::load_rollouts_report(
            &repo_path,
            &project_id,
            &backend_id,
            &argv_owned,
            probe.present,
        );
        Ok(Guarded { policy, output })
    })
    .await
}

/// Creates an App Hosting rollout pinned to one commit.
///
/// ## Security impact
///
/// This is the only command in the Firebase module that changes what the
/// user's production traffic serves, and it is not reversible by this app:
/// App Hosting publishes no rollback verb, so undoing a rollout means creating
/// another one against an earlier commit. It is also **not idempotent** —
/// upstream allocates the next rollout id per call — so calling it twice
/// deploys twice.
///
/// The containment is the same as every other action here, in the same order
/// `cmd_publish_release` establishes: validate every input before it can reach
/// argv, resolve nothing implicitly, build the argv through the *single*
/// builder that also names the program, hand that exact line to the write gate,
/// and only then execute the very bytes the gate judged. A refused verdict
/// returns `Err` before any process is spawned.
///
/// What it does **not** do: hold a credential (the user's `firebase` login
/// owns that), accept an abbreviated SHA (an ambiguous target for something
/// that reaches production), or pass `--force`. Upstream prompts only when
/// neither a branch nor a commit was named, and this always names a commit —
/// so nothing here suppresses a confirmation the user would otherwise see.
#[tauri::command(async)]
pub async fn cmd_firebase_create_rollout(
    repo_path: String,
    project_id: String,
    backend_id: String,
    git_commit: String,
) -> Result<Guarded<crate::firebase::apphosting::RolloutCreateOutcome>, IpcError> {
    off_thread(move || {
        let probe = crate::firebase::firebase_cli_probe();
        if !probe.present {
            return Err(probe
                .reason
                .unwrap_or_else(|| "The Firebase CLI is not available".to_string()));
        }
        let argv_owned = crate::firebase::apphosting::rollout_create_argv(
            &project_id,
            &backend_id,
            &git_commit,
        )?;
        let refs: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
        // Gate first, spawn second. `?` here is what keeps a refusal from
        // reaching production: nothing below runs on a blocked verdict.
        let policy = guard(&repo_path, &refs)?;
        let output = crate::firebase::apphosting::create_rollout(
            &repo_path,
            &project_id,
            &backend_id,
            &git_commit,
            &argv_owned,
        )?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// CI:local — runs the repository's CI pipeline on this machine.
///
/// Every step is judged by the harness command gate and reaches the ledger
/// with its verdict; a completed run against a clean tree is recorded as a
/// git-native verification note on HEAD. See `ci_local` for both.
#[tauri::command(async)]
pub async fn cmd_ci_local(repo_path: String) -> Result<crate::ci_local::CiLocalReport, IpcError> {
    off_thread(move || crate::ci_local::run_ci_local(&repo_path)).await
}
