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
) -> Result<Guarded<crate::engine::git_writer::CheckoutOutcome>, String> {
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
) -> Result<Guarded<crate::engine::git_writer::CheckoutOutcome>, String> {
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
) -> Result<Guarded<()>, String> {
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
) -> Result<Guarded<()>, String> {
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
) -> Result<Guarded<()>, String> {
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
) -> Result<crate::engine::deadbranch::DeadbranchScanResult, String> {
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
) -> Result<Guarded<crate::engine::deadbranch::DeadbranchCleanResult>, String> {
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
) -> Result<Vec<crate::engine::deadbranch::DeadbranchBackupInfo>, String> {
    off_thread(move || crate::engine::deadbranch::list_backups(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_deadbranch_restore(
    repo_path: String,
    backup_path: String,
    branches: Option<Vec<String>>,
) -> Result<Guarded<crate::engine::deadbranch::DeadbranchRestoreResult>, String> {
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
) -> Result<crate::ops::BranchCleanupPlan, String> {
    off_thread(move || crate::ops::branch_cleanup_plan(&repo_path)).await
}

/// The half of ref retention `cmd_branch_cleanup_plan` cannot see: a commit
/// held by a tag alone is on no branch, so that plan counts it nowhere.
#[tauri::command(async)]
pub async fn cmd_tag_cleanup_plan(repo_path: String) -> Result<crate::ops::TagCleanupPlan, String> {
    off_thread(move || crate::ops::tag_cleanup_plan(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_review_outgoing_commits(
    repo_path: String,
) -> Result<crate::ops::CommitReviewReport, String> {
    off_thread(move || crate::ops::review_outgoing_commits(&repo_path)).await
}

#[tauri::command(async)]
pub async fn cmd_create_tag(
    repo_path: String,
    tag_name: String,
    commit_id: Option<String>,
    message: Option<String>,
) -> Result<Guarded<()>, String> {
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
pub async fn cmd_delete_tag(repo_path: String, tag_name: String) -> Result<Guarded<()>, String> {
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
#[tauri::command(async)]
pub async fn cmd_publish_release(
    repo_path: String,
    tag: String,
    message: String,
) -> Result<Guarded<ReleasePublishResult>, String> {
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
        let output =
            GitWriter::push_tag(&repo_path, &plan.remote, &plan.tag).map_err(|error| {
                format!(
                    "Release tag '{}' exists locally but could not be pushed: {}. Retry the release action after fixing connectivity.",
                    plan.tag, error
                )
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
