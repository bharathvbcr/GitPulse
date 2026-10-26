//! History-rewriting commands: rebase, merge, restack, stash, cherry-pick, revert, reset.

use super::*;
use crate::ipc_error::CommandError;

#[tauri::command(async)]
pub async fn cmd_rebase_interactive(
    repo_path: String,
    onto_commit: String,
    steps: Vec<RebaseStep>,
) -> Result<Guarded<()>, IpcError> {
    off_thread(move || {
        // The gate must judge the commands that actually run, and
        // execute_rebase_sequence never runs `git rebase`: it replays the
        // steps with checkout --detach / cherry-pick / commit --amend /
        // branch -f (see GitWriter::execute_rebase_sequence). A rendered
        // `git rebase …` line is a command that does not exist; worse, `-i`
        // names an interactive session this client never opens. Derive the
        // exact planned sequence here and put every line through the gate;
        // a refusal on ANY of them refuses the whole rebase before the
        // first mutation.
        let planned = rebase_planned_commands(&repo_path, &onto_commit, &steps)?;
        let fold = VerdictFold::default();
        for argv in &planned {
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            fold.record(guard(&repo_path, &refs)?);
        }
        GitWriter::execute_rebase_sequence(&repo_path, &onto_commit, &steps)?;
        Ok(Guarded {
            // Representative-verdict policy for multi-command actions: the
            // strictest pass travels with the result (Blocked can never be
            // attached — it errors out above). A single "last" verdict would
            // surface the trailing `git checkout`, hiding an earlier warning
            // or demotion on the mutating cherry-picks.
            policy: fold
                .finish()
                .expect("at least one planned command is always gated"),
            output: (),
        })
    })
    .await
}

/// Merges `branch_name` into the checked-out branch as `mode` says
/// (default, fast-forward only, always a merge commit, or squash). The
/// writer asks the gate about each command as it is about to run it, so a
/// squash's commit is judged with the message it will carry.
#[tauri::command(async)]
pub async fn cmd_merge_branch(
    repo_path: String,
    branch_name: String,
    mode: Option<MergeMode>,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        let fold = VerdictFold::default();
        let output = GitWriter::merge_branch(
            &repo_path,
            &branch_name,
            mode.unwrap_or_default(),
            &mut |args| {
                let argv: Vec<&str> = std::iter::once("git").chain(args.iter().copied()).collect();
                fold.record(guard(&repo_path, &argv)?);
                Ok(())
            },
        )?;
        Ok(Guarded {
            policy: fold.finish().unwrap_or_else(no_git_verdict),
            output,
        })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_restack(
    repo_path: String,
    branch: String,
    onto: String,
    fork_point: Option<String>,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        // Plan, judge, and execute under ONE mutation-lock span: the upstream
        // resolved here is frozen into both the argv the gate judges and the
        // argv GitWriter executes, so the verdict can no longer describe a
        // line that did not run (the old plan-then-lock TOCTOU).
        let repo = validate_repo(&repo_path)?;
        let repo_lock = crate::engine::git_writer::repo_mutation_lock(&repo);
        let _lock_guard = repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(&branch)?;
        validate_ref_name(&onto)?;
        // A cascade supplies the parent tip it read the stack at; every other
        // caller passes none and gets the computed fork point.
        let upstream = GitWriter::prepare_restack(&repo, &branch, &onto, fork_point.as_deref())?;
        let planned = [
            "git",
            "rebase",
            "--onto",
            onto.as_str(),
            upstream.as_str(),
            branch.as_str(),
        ];
        let policy = guard(&repo_path, &planned)?;
        let output = GitWriter::execute_restack(&repo, &branch, &onto, &upstream)?;
        Ok(Guarded { policy, output })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_stash_save(
    repo_path: String,
    message: Option<String>,
    options: Option<StashSaveOptions>,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        let options = options.unwrap_or_default();
        let argv = options.argv(message.as_deref());
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = GitWriter::stash_save_with(&repo_path, message.as_deref(), options)?;
        Ok(Guarded { policy, output })
    })
    .await
}

// --- stash -------------------------------------------------------------

/// Lists the stash stack. Read-only and ungated.
#[tauri::command(async)]
pub async fn cmd_stash_list(
    repo_path: String,
) -> Result<crate::engine::stash::StashList, IpcError> {
    off_thread(move || crate::engine::stash::list(&repo_path)).await
}

/// Renders the diff a stash entry holds, addressed by object id.
#[tauri::command(async)]
pub async fn cmd_stash_show(repo_path: String, oid: String) -> Result<DiffPayload, IpcError> {
    off_thread(move || crate::engine::stash::show(&repo_path, &oid)).await
}

/// Applies, pops, or drops a stash entry.
///
/// `expected_oid` is what the client believed `index` held. The engine
/// re-resolves the index under the repository lock and refuses on a mismatch,
/// so a stale list can fail loudly but can never destroy an entry the user did
/// not choose — the stash stack is shared with every other worktree, client
/// and agent touching this repository.
#[tauri::command(async)]
pub async fn cmd_stash_action(
    repo_path: String,
    action: StashAction,
    index: usize,
    expected_oid: String,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        let (policy, output) = crate::engine::stash::run_action_with(
            &repo_path,
            action,
            index,
            &expected_oid,
            |argv| guard(&repo_path, argv).map_err(CommandError::from),
        )?;
        Ok(Guarded { policy, output })
    })
    .await
}

// --- replaying and rewinding commits -----------------------------------

/// Replays commits onto the current branch.
///
/// A conflict parks the repository mid-cherry-pick, which `cmd_repo_operation`
/// then reports and the banner offers continue/skip/abort for. That is a
/// supported outcome, not a failure to roll back.
#[tauri::command(async)]
pub async fn cmd_cherry_pick(
    repo_path: String,
    commits: Vec<String>,
    no_commit: Option<bool>,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        let no_commit = no_commit.unwrap_or(false);
        let argv = GitWriter::replay_argv("cherry-pick", &commits, no_commit);
        let policy = guard(&repo_path, &argv)?;
        let output = GitWriter::cherry_pick(&repo_path, &commits, no_commit)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Records the inverse of the given commits as new commits.
#[tauri::command(async)]
pub async fn cmd_revert(
    repo_path: String,
    commits: Vec<String>,
    no_commit: Option<bool>,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        let no_commit = no_commit.unwrap_or(false);
        let argv = GitWriter::replay_argv("revert", &commits, no_commit);
        let policy = guard(&repo_path, &argv)?;
        let output = GitWriter::revert(&repo_path, &commits, no_commit)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Moves the current branch to `target`.
///
/// `ResetMode::Hard` destroys uncommitted work irrecoverably; the mode is an
/// enum so the rendered line the gate judges names the exact flag, and the UI
/// can require its own confirmation from `discards_working_tree`.
#[tauri::command(async)]
pub async fn cmd_reset(
    repo_path: String,
    mode: ResetMode,
    target: String,
) -> Result<Guarded<String>, IpcError> {
    off_thread(move || {
        let argv = GitWriter::reset_argv(mode, &target);
        let policy = guard(&repo_path, &argv)?;
        let output = GitWriter::reset(&repo_path, mode, &target)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// The exact mutating commands [`GitWriter::execute_rebase_sequence`] will run
/// for this rebase, in execution order — derived here because engine/ cannot
/// own a shared argv builder without this file growing a second gate seam.
///
/// Mirrored decision points (keep in lockstep with the writer):
/// - validations first (onto, steps non-empty, every step oid);
/// - `checkout --detach <onto>`;
/// - per step: `cherry-pick [cid|-n cid]`, plus the amend each action makes
///   (Squash amends preserving the message via `--amend --no-edit`, Fixup the
///   same, Reword amends `-m <composed> --amend`);
/// - rollback pair that runs whenever a step fails: `cherry-pick --abort`,
///   then `checkout -f <branch|head>` — gated proactively because they are
///   part of what this invocation may execute;
/// - on a branch, the success tail: `branch -f <branch> HEAD`, then
///   `checkout <branch>` (the retry reruns the same argv).
///
/// Read-only probes (status, merge-base ancestry, symbolic-ref, log) are not
/// gated, matching the existing fetch/status precedent. A failing reword
/// composition refuses up front — strictly safer than the writer's
/// mid-sequence failure.
pub(super) fn rebase_planned_commands(
    repo_path: &str,
    onto_commit: &str,
    steps: &[RebaseStep],
) -> Result<Vec<Vec<String>>, CommandError> {
    let repo = validate_repo(repo_path)?;
    validate_oid_or_revision(onto_commit)?;
    if steps.is_empty() {
        return Err("Rebase sequence is empty".into());
    }
    // Mirror of the writer's own input validations, so a sequence it would
    // refuse never reaches the gate or the subprocess layer.
    if let Some(first) = steps.first() {
        if matches!(
            first.action,
            crate::engine::git_writer::RebaseActionKind::Squash
                | crate::engine::git_writer::RebaseActionKind::Fixup
        ) {
            return Err(format!(
                "Cannot '{}' commit {} without a previous commit to combine into",
                match first.action {
                    crate::engine::git_writer::RebaseActionKind::Squash => "squash",
                    _ => "fixup",
                },
                first.commit_id
            )
            .into());
        }
    }
    for step in steps {
        validate_oid_or_revision(&step.commit_id)?;
    }

    let original_head = git_text(&repo, &["rev-parse", "HEAD"])?.trim().to_string();
    // Must agree with the writer this previews: a refused read is an error,
    // never "detached".
    let original_branch = crate::engine::git_reader::head_branch(&repo)?;

    let mut planned: Vec<Vec<String>> = vec![vec![
        "git".into(),
        "checkout".into(),
        "--detach".into(),
        onto_commit.into(),
    ]];

    for step in steps {
        match &step.action {
            crate::engine::git_writer::RebaseActionKind::Pick
            | crate::engine::git_writer::RebaseActionKind::Reword(_) => {
                planned.push(vec![
                    "git".into(),
                    "cherry-pick".into(),
                    step.commit_id.clone(),
                ]);
            }
            crate::engine::git_writer::RebaseActionKind::Squash
            | crate::engine::git_writer::RebaseActionKind::Fixup => {
                planned.push(vec![
                    "git".into(),
                    "cherry-pick".into(),
                    "-n".into(),
                    step.commit_id.clone(),
                ]);
            }
            crate::engine::git_writer::RebaseActionKind::Drop => {}
        }
        match &step.action {
            // Squash folds into the picked commit keeping its message;
            // Fixup keeps it too, discarding the folded commit's own.
            crate::engine::git_writer::RebaseActionKind::Squash
            | crate::engine::git_writer::RebaseActionKind::Fixup => {
                planned.push(commit_amend_argv("", true));
            }
            crate::engine::git_writer::RebaseActionKind::Reword(new_msg) => {
                let original = git_text(&repo, &["log", "-1", "--format=%B", &step.commit_id])?;
                let message = reworded_message(&original, new_msg);
                planned.push(commit_amend_argv(&message, true));
            }
            _ => {}
        }
    }

    // Rollback commands: executed iff some step fails. Judged regardless so a
    // posture that denies them refuses the whole rebase before HEAD moves.
    planned.push(vec!["git".into(), "cherry-pick".into(), "--abort".into()]);
    let restore_target = original_branch
        .clone()
        .unwrap_or_else(|| original_head.clone());
    planned.push(vec![
        "git".into(),
        "checkout".into(),
        "-f".into(),
        restore_target,
    ]);

    // Success tail, only when the session started on a branch.
    if let Some(branch) = original_branch {
        planned.push(vec![
            "git".into(),
            "branch".into(),
            "-f".into(),
            branch.clone(),
            "HEAD".into(),
        ]);
        planned.push(vec!["git".into(), "checkout".into(), branch]);
    }

    Ok(planned)
}
