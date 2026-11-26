//! Branch, tag and release commands, plus the cleanup planners that act on refs.

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleasePublishResult {
    pub tag: String,
    pub remote: String,
    pub created_tag: bool,
    pub tag_policy: Option<crate::harness::PolicyVerdict>,
    pub output: String,
}

#[tauri::command(async)]
pub async fn cmd_checkout_branch(
    repo_path: String,
    branch_name: String,
) -> Result<Guarded<crate::engine::git_writer::CheckoutOutcome>, IpcError> {
    off_thread(move || {
        // The writer resolves exactly one argv (switch to a local branch or to
        // the local twin of a remote-tracking name, `--track` to create it, or
        // `--guess`), and the gate judges that line, not a guess at it.
        let argv = GitWriter::checkout_branch_argv(&repo_path, &branch_name)?;
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = GitWriter::checkout_branch_judged(&repo_path, &branch_name, &argv)?;
        Ok(Guarded { policy, output })
    })
    .await
}

/// Checks out a commit, tag or other revision with HEAD detached. Kept apart
/// from `cmd_checkout_branch`, which never detaches.
#[tauri::command(async)]
pub async fn cmd_checkout_detached(
    repo_path: String,
    revision: String,
) -> Result<Guarded<crate::engine::git_writer::CheckoutOutcome>, IpcError> {
    off_thread(move || {
        let argv = GitWriter::checkout_detached_argv(&revision)?;
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let policy = guard(&repo_path, &refs)?;
        let output = GitWriter::checkout_detached(&repo_path, &revision)?;
        Ok(Guarded { policy, output })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_create_branch(
    repo_path: String,
    branch_name: String,
    start_point: Option<String>,
) -> Result<Guarded<()>, IpcError> {
    off_thread(move || {
        let argv = GitWriter::create_branch_argv(&branch_name, start_point.as_deref())?;
        let policy = guard(&repo_path, &GitWriter::argv_refs(&argv))?;
        GitWriter::create_branch(&repo_path, &branch_name, start_point.as_deref())?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_delete_branch(
    repo_path: String,
    branch_name: String,
    force: bool,
) -> Result<Guarded<()>, IpcError> {
    off_thread(move || {
        let argv = GitWriter::delete_branch_argv(&branch_name, force)?;
        let policy = guard(&repo_path, &GitWriter::argv_refs(&argv))?;
        // Tip is journalled inside delete_branch as ledger before_ref; callers
        // that still hold the name (sidebar toast / cleanup candidate) restore
        // with create_branch and do not need the SHA on this wire.
        let _tip = GitWriter::delete_branch(&repo_path, &branch_name, force)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_rename_branch(
    repo_path: String,
    old_name: String,
    new_name: String,
) -> Result<Guarded<()>, IpcError> {
    off_thread(move || {
        let argv = GitWriter::rename_branch_argv(&old_name, &new_name)?;
        let policy = guard(&repo_path, &GitWriter::argv_refs(&argv))?;
        GitWriter::rename_branch(&repo_path, &old_name, &new_name)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_deadbranch_scan(
    repo_path: String,
    config: Option<crate::engine::deadbranch::DeadbranchConfig>,
) -> Result<crate::engine::deadbranch::DeadbranchScanResult, IpcError> {
    off_thread(move || {
        let conf = config.unwrap_or_default();
        crate::engine::deadbranch::scan_stale_branches(&repo_path, &conf)
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_deadbranch_clean(
    repo_path: String,
    branches: Vec<String>,
    force: bool,
    create_backup: bool,
) -> Result<Guarded<crate::engine::deadbranch::DeadbranchCleanResult>, IpcError> {
    off_thread(move || {
        let fold = VerdictFold::default();
        let result = crate::engine::deadbranch::clean_branches_with_gate(
            &repo_path,
            &branches,
            force,
            create_backup,
            |argv| {
                fold.record(guard(&repo_path, argv)?);
                Ok(())
            },
        )?;
        Ok(Guarded {
            policy: fold.finish().unwrap_or_else(no_git_verdict),
            output: result,
        })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_deadbranch_list_backups(
    repo_path: String,
) -> Result<Vec<crate::engine::deadbranch::DeadbranchBackupInfo>, IpcError> {
    off_thread(move || crate::engine::deadbranch::list_backups(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_deadbranch_restore(
    repo_path: String,
    backup_path: String,
    branches: Option<Vec<String>>,
) -> Result<Guarded<crate::engine::deadbranch::DeadbranchRestoreResult>, IpcError> {
    off_thread(move || {
        let fold = VerdictFold::default();
        let result = crate::engine::deadbranch::restore_backup_with_gate(
            &repo_path,
            &backup_path,
            branches,
            |argv| {
                fold.record(guard(&repo_path, argv)?);
                Ok(())
            },
        )?;
        Ok(Guarded {
            policy: fold.finish().unwrap_or_else(no_git_verdict),
            output: result,
        })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_branch_cleanup_plan(
    repo_path: String,
) -> Result<crate::ops::BranchCleanupPlan, IpcError> {
    off_thread(move || crate::ops::branch_cleanup_plan(&repo_path)).await
}

/// The half of ref retention `cmd_branch_cleanup_plan` cannot see: a commit
/// held by a tag alone is on no branch, so that plan counts it nowhere.
#[tauri::command(async)]
pub async fn cmd_tag_cleanup_plan(
    repo_path: String,
) -> Result<crate::ops::TagCleanupPlan, IpcError> {
    off_thread(move || crate::ops::tag_cleanup_plan(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_review_outgoing_commits(
    repo_path: String,
) -> Result<crate::ops::CommitReviewReport, IpcError> {
    off_thread(move || crate::ops::review_outgoing_commits(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_create_tag(
    repo_path: String,
    tag_name: String,
    commit_id: Option<String>,
    message: Option<String>,
) -> Result<Guarded<()>, IpcError> {
    off_thread(move || {
        let argv = GitWriter::create_tag_argv(&tag_name, commit_id.as_deref(), message.as_deref())?;
        let policy = guard(&repo_path, &GitWriter::argv_refs(&argv))?;
        GitWriter::create_tag(
            &repo_path,
            &tag_name,
            commit_id.as_deref(),
            message.as_deref(),
        )?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_delete_tag(repo_path: String, tag_name: String) -> Result<Guarded<()>, IpcError> {
    off_thread(move || {
        let argv = GitWriter::delete_tag_argv(&tag_name)?;
        let policy = guard(&repo_path, &GitWriter::argv_refs(&argv))?;
        GitWriter::delete_tag(&repo_path, &tag_name)?;
        Ok(Guarded { policy, output: () })
    })
    .await
}

/// Creates (or resumes) an annotated release tag and pushes exactly that tag.
/// Both commands are policy-checked before the first mutation, so a refusal
/// cannot strand a tag created by a previously unchecked second step.
///
/// The commits the tag would send are scanned for credentials before the
/// push. A refusal leaves the tag local and comes back as the secret scan's
/// own typed error; `allow_secrets` is the person's "Publish anyway".
#[tauri::command(async)]
pub async fn cmd_publish_release(
    repo_path: String,
    tag: String,
    message: String,
    allow_secrets: Option<bool>,
) -> Result<Guarded<ReleasePublishResult>, IpcError> {
    off_thread(move || {
        let plan = crate::ops::prepare_release(&repo_path, &tag, &message)?;
        let tag_policy = if plan.create_tag {
            let argv = GitWriter::create_tag_argv(
                &plan.tag,
                Some(plan.head.as_str()),
                Some(plan.message.as_str()),
            )?;
            Some(guard(&repo_path, &GitWriter::argv_refs(&argv))?)
        } else {
            None
        };
        let push_argv = GitWriter::push_tag_argv(&plan.remote, &plan.tag)?;
        let push_policy = guard(&repo_path, &GitWriter::argv_refs(&push_argv))?;

        if plan.create_tag {
            GitWriter::create_tag(
                &repo_path,
                &plan.tag,
                Some(plan.head.as_str()),
                Some(&plan.message),
            )?;
        }
        let output = GitWriter::push_tag_with_policy(
            &repo_path,
            &plan.remote,
            &plan.tag,
            super::staging::secret_policy(allow_secrets),
        )
        .map_err(|error| {
            if error.kind() == crate::ipc_error::IpcErrorKind::SecretScanBlocked {
                return error;
            }
            format!(
                "Release tag '{}' exists locally but could not be pushed: {}. Retry the release action after fixing connectivity.",
                plan.tag, error.message()
            )
            .into()
        })?;
        Ok(Guarded {
            policy: push_policy,
            output: ReleasePublishResult {
                tag: plan.tag,
                remote: plan.remote,
                created_tag: plan.create_tag,
                tag_policy,
                output,
            },
        })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_list_op_snapshots(
    repo_path: String,
    limit: Option<usize>,
) -> Result<crate::engine::OpSnapshotList, IpcError> {
    off_thread(move || GitWriter::list_op_snapshots(&repo_path, limit.unwrap_or(50))).await
}

#[tauri::command(async)]
pub async fn cmd_undo_op_snapshot(
    repo_path: String,
    plan: crate::engine::OpUndoPlan,
) -> Result<Guarded<crate::engine::OpUndoOutcome>, IpcError> {
    off_thread(move || {
        let fold = VerdictFold::default();
        let outcome = GitWriter::undo_op_snapshot_with_gate(&repo_path, &plan, &mut |argv| {
            fold.record(guard(&repo_path, argv)?);
            Ok(())
        })?;
        Ok(Guarded {
            policy: fold.finish().unwrap_or_else(no_git_verdict),
            output: outcome,
        })
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_restore_turn_checkpoint(
    repo_path: String,
    worktree_path: Option<String>,
    checkpoint_ref: String,
    force: Option<bool>,
) -> Result<crate::engine::turn_checkpoint::TurnCheckpointRestore, IpcError> {
    off_thread(move || {
        let wt_str = worktree_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&repo_path);
        let argv = ["git", "read-tree", "--reset", "-u", &checkpoint_ref];
        let _ = guard(&repo_path, &argv)?;
        crate::engine::turn_checkpoint::restore_turn_checkpoint(
            std::path::Path::new(&repo_path),
            std::path::Path::new(wt_str),
            &checkpoint_ref,
            force.unwrap_or(false),
        )
        .map_err(CommandError::from)
    })
    .await
}

#[tauri::command(async)]
pub async fn cmd_non_standard_ref_inventory(
    repo_path: String,
) -> Result<crate::ops::NonStandardRefInventory, IpcError> {
    off_thread(move || crate::ops::non_standard_ref_inventory(&repo_path)).await
}

#[cfg(test)]
#[path = "refs_tests.rs"]
mod tests;
