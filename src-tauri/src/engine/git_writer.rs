use crate::engine::git_cli::{
    git_global, git_text, git_with_stdin, resolve_git_common_dir, sandbox_join, validate_repo,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub(crate) fn repo_mutation_lock(canon: &Path) -> Arc<Mutex<()>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let key = resolve_git_common_dir(canon).unwrap_or_else(|_| canon.to_path_buf());
    let mut map = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Upper bound on commits replayed by one cherry-pick or revert call.
///
/// A replay is a sequence of merges; an unbounded list from a "select all"
/// click would park the repository in a sequencer thousands of steps deep with
/// no realistic way back out.
const MAX_REPLAY_COMMITS: usize = 200;

/// How much of the working state a reset discards.
///
/// An enum rather than a passthrough string: no caller can invent a mode, and
/// the write gate is handed a line whose destructiveness it can rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResetMode {
    /// Move the branch; keep the index and the working tree.
    Soft,
    /// Move the branch and reset the index; keep the working tree.
    Mixed,
    /// Move the branch and reset the index, keeping local changes that do not
    /// collide. Refuses rather than overwriting.
    Keep,
    /// Move the branch and overwrite the index AND the working tree.
    /// Uncommitted work is destroyed and is not recoverable from git.
    Hard,
}

impl ResetMode {
    pub fn flag(self) -> &'static str {
        match self {
            ResetMode::Soft => "--soft",
            ResetMode::Mixed => "--mixed",
            ResetMode::Keep => "--keep",
            ResetMode::Hard => "--hard",
        }
    }

    /// True for the mode that destroys uncommitted work. The UI uses this to
    /// decide whether an extra confirmation is owed; it is derived here so the
    /// answer cannot drift from the flag it describes.
    pub fn discards_working_tree(self) -> bool {
        matches!(self, ResetMode::Hard)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RebaseActionKind {
    Pick,
    Squash,
    Fixup,
    Drop,
    Reword(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebaseStep {
    pub commit_id: String,
    pub action: RebaseActionKind,
}

pub struct GitWriter;

impl GitWriter {
    pub fn stage_file(repo_path: &str, file_path: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = sandbox_join(&repo, file_path)?;
        // `:(literal)` disables pathspec globbing so a user path can never
        // widen its own blast radius ("*" must match a file named "*", never
        // the whole tree). Same convention as the reader side.
        git_text(&repo, &["add", "--", &format!(":(literal){file_path}")])?;
        Ok(())
    }

    pub fn unstage_file(repo_path: &str, file_path: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = sandbox_join(&repo, file_path)?;
        git_text(
            &repo,
            &[
                "restore",
                "--staged",
                "--",
                &format!(":(literal){file_path}"),
            ],
        )?;
        Ok(())
    }

    pub fn commit(repo_path: &str, message: &str, amend: bool) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        if message.trim().is_empty() {
            return Err("Commit message must not be empty".into());
        }
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::commit_inner(&repo, message, amend)
    }

    fn commit_inner(repo: &Path, message: &str, amend: bool) -> Result<String, String> {
        let mut args = vec!["commit"];
        if amend && message.is_empty() {
            args.push("--amend");
            args.push("--no-edit");
        } else {
            args.push("-m");
            args.push(message);
            if amend {
                args.push("--amend");
            }
        }
        git_text(repo, &args)
    }

    /// The add argv [`GitWriter::quick_commit`] runs. The command layer must
    /// judge `git` plus these args, never a paraphrased `-A` / pathspec form.
    pub(crate) const QUICK_COMMIT_ADD_ARGV: &'static [&'static str] = &["add", "--all"];

    /// Stage every tracked change and untracked non-ignored file, then commit,
    /// under one mutation-lock acquisition.
    ///
    /// Interactive "quick commit" uses this rather than `stageAll` + `commit()`,
    /// which spans two lock acquisitions and can absorb a concurrent writer's
    /// index. Unmerged paths refuse before `add` so a conflicted tree is never
    /// silently marked resolved. Ignored untracked files stay untracked —
    /// `git add --all` does not force them.
    pub fn quick_commit(repo_path: &str, message: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        if message.trim().is_empty() {
            return Err("Commit message must not be empty".into());
        }
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::quick_commit_inner(&repo, message)
    }

    fn quick_commit_inner(repo: &Path, message: &str) -> Result<String, String> {
        let unmerged = git_text(repo, &["ls-files", "--unmerged"])?;
        if !unmerged.trim().is_empty() {
            return Err("Resolve merge conflicts before committing.".into());
        }
        git_text(repo, Self::QUICK_COMMIT_ADD_ARGV)?;
        Self::commit_inner(repo, message, false)
    }

    /// Stage exactly `files` and commit them under a single mutation-lock
    /// acquisition.
    ///
    /// `stage_file()` followed by `commit()` spans two lock acquisitions, so a
    /// concurrent writer can commit the shared index in between and absorb the
    /// staged bytes into its own commit. Programmatic callers (automation,
    /// agents, batch tooling) need stage+commit to be one indivisible
    /// mutation; this is that primitive. Interactive flows that intentionally
    /// commit whatever the user staged keep using `stage_file()` + `commit()`.
    pub fn commit_files(
        repo_path: &str,
        message: &str,
        files: &[String],
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        if message.trim().is_empty() {
            return Err("Commit message must not be empty".into());
        }
        if files.is_empty() {
            return Err("commit_files requires at least one path".into());
        }
        for file in files {
            if file.is_empty() {
                return Err("commit_files requires non-empty paths".into());
            }
            sandbox_join(&repo, file)?;
        }
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut add_args: Vec<&str> = Vec::with_capacity(files.len() + 2);
        add_args.push("add");
        add_args.push("--");
        add_args.extend(files.iter().map(String::as_str));
        git_text(&repo, &add_args)?;
        Self::commit_inner(&repo, message, false)
    }

    pub fn checkout_branch(repo_path: &str, branch_name: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(branch_name)?;
        // Two strategies, no more: `switch --guess` covers creating a local
        // branch from a remote twin (git >= 2.23), plain `checkout` covers
        // everything older. The former middle attempt (`checkout --guess`)
        // added a third executable spelling without covering any state the
        // other two miss — and every extra strategy is another argv the
        // policy gate must judge identically.
        let attempts: [&[&str]; 2] = [
            &["switch", "--guess", branch_name],
            &["checkout", branch_name],
        ];
        let mut first_err = None;
        for attempt in attempts {
            match git_text(&repo, attempt) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        Err(first_err.expect("at least one attempt recorded"))
    }

    pub fn create_branch(
        repo_path: &str,
        branch_name: &str,
        start_point: Option<&str>,
    ) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(branch_name)?;
        let mut args = vec!["branch", branch_name];
        if let Some(sp) = start_point {
            // Start points are revisions, not refs-to-create: HEAD~1 and raw
            // oids are legal here. Reflog/peel grammar stays excluded by
            // validate_oid_or_revision on purpose.
            validate_oid_or_revision(sp)?;
            args.push(sp);
        }
        git_text(&repo, args.as_slice())?;
        Ok(())
    }

    pub fn delete_branch(repo_path: &str, branch_name: &str, force: bool) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(branch_name)?;
        if force {
            // `-d` leaves these to git's own safety nets; `-D` bypasses them,
            // so the checks must be re-done server side.
            if is_default_branch(&repo, branch_name) {
                return Err(format!(
                    "refusing to force-delete '{branch_name}': it resolves to the repository's default branch"
                ));
            }
            if is_checked_out_in_any_worktree(&repo, branch_name)? {
                return Err(format!(
                    "refusing to force-delete '{branch_name}': it is checked out in a linked worktree"
                ));
            }
        }
        let flag = if force { "-D" } else { "-d" };
        git_text(&repo, &["branch", flag, branch_name])?;
        Ok(())
    }

    pub fn rename_branch(repo_path: &str, old_name: &str, new_name: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(old_name)?;
        validate_ref_name(new_name)?;
        git_text(&repo, &["branch", "-m", old_name, new_name])?;
        Ok(())
    }

    pub fn apply_patch_to_index(repo_path: &str, patch_content: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        git_with_stdin(
            &repo,
            &["apply", "--cached", "--unidiff-zero", "--recount", "-"],
            patch_content.as_bytes(),
        )?;
        Ok(())
    }

    pub fn execute_rebase_sequence(
        repo_path: &str,
        onto_commit: &str,
        steps: &[RebaseStep],
    ) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_oid_or_revision(onto_commit)?;
        if steps.is_empty() {
            return Err("Rebase sequence is empty".into());
        }
        if let Some(first) = steps.first() {
            if matches!(
                first.action,
                RebaseActionKind::Squash | RebaseActionKind::Fixup
            ) {
                return Err(format!(
                    "Cannot '{}' commit {} without a previous commit to combine into",
                    match first.action {
                        RebaseActionKind::Squash => "squash",
                        RebaseActionKind::Fixup => "fixup",
                        _ => unreachable!(),
                    },
                    first.commit_id
                ));
            }
        }
        for step in steps {
            validate_oid_or_revision(&step.commit_id)?;
        }

        let dirty = git_text(&repo, &["status", "--porcelain"])?;
        if !dirty.trim().is_empty() {
            return Err(
                "Working tree has uncommitted changes; commit or stash before rebasing".into(),
            );
        }

        let original_head = git_text(&repo, &["rev-parse", "HEAD"])?.trim().to_string();
        // A step whose commit is not reachable from the HEAD being rebased
        // would transplant a foreign commit onto the new base; refuse before
        // any state is touched.
        for step in steps {
            let is_ancestor = git_text(
                &repo,
                &[
                    "merge-base",
                    "--is-ancestor",
                    &step.commit_id,
                    &original_head,
                ],
            );
            if is_ancestor.is_err() {
                return Err(format!(
                    "Rebase step {} is not an ancestor of HEAD; refusing to transplant foreign commits",
                    step.commit_id
                ));
            }
        }
        let original_branch = git_text(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Recovery must never swallow its own failure: the checkout outcome is
        // captured so a locked index or full disk cannot hide that HEAD was
        // left detached mid-rebase.
        let restore = |repo: &std::path::Path| -> Result<(), String> {
            if let Some(ref branch) = original_branch {
                git_text(repo, &["checkout", "-f", branch]).map(|_| ())
            } else {
                git_text(repo, &["checkout", "-f", &original_head]).map(|_| ())
            }
        };

        git_text(&repo, &["checkout", "--detach", onto_commit])?;

        let result = (|| -> Result<(), String> {
            for step in steps {
                match &step.action {
                    RebaseActionKind::Pick => {
                        git_text(&repo, &["cherry-pick", &step.commit_id]).map_err(|e| {
                            format!("Rebase step failed (pick {}): {}", step.commit_id, e)
                        })?;
                    }
                    RebaseActionKind::Squash => {
                        git_text(&repo, &["cherry-pick", "-n", &step.commit_id]).map_err(|e| {
                            format!("Rebase step failed (squash {}): {}", step.commit_id, e)
                        })?;
                        Self::commit_inner(&repo, "", true)?;
                    }
                    RebaseActionKind::Fixup => {
                        git_text(&repo, &["cherry-pick", "-n", &step.commit_id]).map_err(|e| {
                            format!("Rebase step failed (fixup {}): {}", step.commit_id, e)
                        })?;
                        git_text(&repo, &["commit", "--amend", "--no-edit"])?;
                    }
                    RebaseActionKind::Drop => {}
                    RebaseActionKind::Reword(new_msg) => {
                        git_text(&repo, &["cherry-pick", &step.commit_id]).map_err(|e| {
                            format!("Rebase step failed (reword {}): {}", step.commit_id, e)
                        })?;
                        // Amending with only the new subject would destroy the
                        // commit body; rewrite the subject line in place.
                        let original =
                            git_text(&repo, &["log", "-1", "--format=%B", &step.commit_id])?;
                        let message = reworded_message(&original, new_msg);
                        Self::commit_inner(&repo, &message, true)?;
                    }
                }
            }
            Ok(())
        })();

        if let Err(e) = result {
            // git_text yields stdout on success; normalize to () so the
            // composer only ever sees recovery outcomes.
            let abort_result = git_text(&repo, &["cherry-pick", "--abort"]).map(|_| ());
            if let Err(abort_err) = &abort_result {
                log::warn!(
                    target: "engine",
                    "rebase recovery: cherry-pick --abort failed in {}: {abort_err}",
                    repo.display()
                );
            }
            let restore_result = restore(&repo);
            if let Err(restore_err) = &restore_result {
                log::warn!(
                    target: "engine",
                    "rebase recovery: checkout -f restore failed in {}: {restore_err}",
                    repo.display()
                );
            }
            return Err(combine_step_failure_with_recovery(
                &e,
                Some(&abort_result),
                &restore_result,
                original_branch.as_deref(),
                &original_head,
            ));
        }

        if let Some(ref branch) = original_branch {
            if let Err(e) = git_text(&repo, &["branch", "-f", branch, "HEAD"]) {
                let restore_result = restore(&repo);
                if let Err(restore_err) = &restore_result {
                    log::warn!(
                        target: "engine",
                        "rebase recovery: checkout -f restore after failed 'branch -f' in {}: {restore_err}",
                        repo.display()
                    );
                }
                // No cherry-pick was mid-flight here, so abort_result is None.
                return Err(combine_step_failure_with_recovery(
                    &e,
                    None,
                    &restore_result,
                    original_branch.as_deref(),
                    &original_head,
                ));
            }
            // The rebase is durably applied once `branch -f` succeeded; only
            // the working-tree checkout remains. Retry once, and if it still
            // fails never claim total failure — name the true end-state so
            // the user knows the branch DID move.
            if let Err(checkout_err) = git_text(&repo, &["checkout", branch]) {
                if git_text(&repo, &["checkout", branch]).is_err() {
                    return Err(format!(
                        "rebase was applied to '{branch}', but working-tree checkout failed: \
                         {checkout_err}"
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn fetch(repo_path: &str, remote: Option<&str>) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(r) = remote {
            validate_ref_name(r)?;
            git_text(&repo, &["fetch", r])
        } else {
            git_text(&repo, &["fetch", "--all", "--prune"])
        }
    }

    pub fn pull(
        repo_path: &str,
        remote: Option<&str>,
        branch: Option<&str>,
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match (remote, branch) {
            (Some(r), Some(b)) => {
                validate_ref_name(r)?;
                validate_ref_name(b)?;
                git_text(&repo, &["pull", r, b])
            }
            (Some(r), None) => {
                validate_ref_name(r)?;
                git_text(&repo, &["pull", r])
            }
            _ => git_text(&repo, &["pull"]),
        }
    }

    pub fn push(
        repo_path: &str,
        remote: Option<&str>,
        branch: Option<&str>,
        force: bool,
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut args = vec!["push"];
        if force {
            args.push("--force-with-lease");
        }
        if let Some(r) = remote {
            validate_ref_name(r)?;
            args.push(r);
        }
        if let Some(b) = branch {
            validate_ref_name(b)?;
            args.push(b);
        }
        git_text(&repo, &args)
    }

    /// Pushes exactly one tag ref. Using a fully-qualified refspec avoids an
    /// ambiguous branch/tag name from publishing the wrong object.
    pub fn push_tag(repo_path: &str, remote: &str, tag: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(remote)?;
        validate_ref_name(tag)?;
        let refspec = format!("refs/tags/{tag}");
        git_text(&repo, &["push", remote, &refspec])
    }

    pub fn merge_branch(
        repo_path: &str,
        branch_name: &str,
        ff_only: bool,
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(branch_name)?;
        if ff_only {
            git_text(&repo, &["merge", "--ff-only", "--no-edit", branch_name])
        } else {
            git_text(&repo, &["merge", "--no-edit", branch_name])
        }
    }

    /// Resolves the upstream for a restack of `branch` onto `onto`: where
    /// `branch` forked, NOT the new base itself. After the parent branch was
    /// rewritten, `onto..branch` still contains the stale pre-image commits
    /// and replaying them conflicts. --fork-point recovers the pre-rewrite
    /// fork from the reflog; plain merge-base covers reflog-less clones;
    /// unrelated histories fall back to `onto`.
    ///
    /// Caller MUST hold the repo mutation lock: the value returned here is
    /// frozen into the argv the harness gate judges AND the argv executed,
    /// closing the plan-vs-execute TOCTOU.
    pub fn prepare_restack(repo_canon: &Path, branch: &str, onto: &str) -> Result<String, String> {
        validate_ref_name(branch)?;
        validate_ref_name(onto)?;
        Ok([
            vec!["merge-base", "--fork-point", onto, branch],
            vec!["merge-base", onto, branch],
        ]
        .into_iter()
        .find_map(|argv| {
            git_text(repo_canon, &argv)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| onto.to_string()))
    }

    /// Executes a restack with full preflight and rollback semantics. Caller
    /// MUST hold the repo mutation lock.
    ///
    /// - Preflight refuses a dirty worktree and any in-progress operation
    ///   (merge, rebase, cherry-pick, revert, `am`, bisect) before any state
    ///   is touched, naming the one it actually found.
    /// - On failure the half-applied rebase is aborted and the user's
    ///   original checkout restored; the error says what was rolled back.
    /// - On success `git rebase <branch>` leaves `branch` checked out, which
    ///   would silently switch the user's working copy; the original branch
    ///   is restored, and a failed restore is reported as partial success —
    ///   never as total failure.
    pub fn execute_restack(
        repo_canon: &Path,
        branch: &str,
        onto: &str,
        upstream: &str,
    ) -> Result<String, String> {
        validate_ref_name(branch)?;
        validate_ref_name(onto)?;
        // Upstream is either the validated `onto` fallback or a git-emitted
        // OID from merge-base; anything else must never reach argv.
        if upstream != onto {
            validate_oid(upstream)?;
        }

        // In-progress detection runs BEFORE the dirty-tree check: a mid-rebase
        // worktree is by definition dirty, and "finish the other rebase" is
        // the actionable cause while "commit your changes" is its symptom.
        // Naming the actual operation matters: "finish the rebase" sends the
        // user hunting for a rebase that is really a parked cherry-pick, and
        // `git rebase --abort` does not clear one.
        if let Some(parked) = crate::engine::repo_op::detect(repo_canon)? {
            return Err(format!(
                "A {} is already in progress in this repository; \
                 finish or abort it before restacking",
                parked.kind.label()
            ));
        }
        let dirty = git_text(repo_canon, &["status", "--porcelain"])?;
        if !dirty.trim().is_empty() {
            return Err(
                "Working tree has uncommitted changes; commit or stash before restacking".into(),
            );
        }

        // symbolic-ref fails on detached HEAD; then there is no branch to restore.
        let original_head = git_text(repo_canon, &["symbolic-ref", "--quiet", "HEAD"])
            .ok()
            .map(|s| {
                s.trim()
                    .trim_start_matches("refs/heads/")
                    .trim()
                    .to_string()
            })
            .filter(|s| !s.is_empty());

        let restore_checkout = |repo_canon: &Path, target: &str| -> Result<(), String> {
            if git_text(repo_canon, &["checkout", target]).is_ok() {
                return Ok(());
            }
            // One retry: index refresh races are the common transient cause.
            if git_text(repo_canon, &["checkout", target]).is_ok() {
                return Ok(());
            }
            Err(format!("checkout '{target}' failed"))
        };

        match git_text(repo_canon, &["rebase", "--onto", onto, upstream, branch]) {
            Ok(output) => {
                if original_head.as_deref() != Some(branch) {
                    if let Some(ref orig) = original_head {
                        if let Err(checkout_err) = restore_checkout(repo_canon, orig) {
                            return Err(format!(
                                "Restack succeeded: '{branch}' was rebased onto '{onto}', but \
                                 restoring your previous branch '{orig}' failed ({checkout_err}). \
                                 The repository is left on '{branch}'."
                            ));
                        }
                    }
                }
                Ok(output)
            }
            Err(rebase_err) => {
                // Abort the half-applied rebase, then put the user back on
                // their original branch (--abort lands on `branch`, not where
                // the user was). Neither cleanup failure hides the primary
                // error; the message names the true end-state instead.
                let _ = git_text(repo_canon, &["rebase", "--abort"]);
                let end_state = match &original_head {
                    Some(orig) if orig != branch => {
                        if restore_checkout(repo_canon, orig).is_err() {
                            format!("'{branch}' is checked out (restore of '{orig}' failed)")
                        } else {
                            format!("the repository is back on '{orig}'")
                        }
                    }
                    _ => format!("'{branch}' is checked out"),
                };
                Err(format!(
                    "Restack of '{branch}' onto '{onto}' failed and was rolled back ({end_state}): {}",
                    summarize_git_failure(&rebase_err)
                ))
            }
        }
    }

    /// Convenience wrapper used outside the gated command path (tests): takes
    /// the mutation lock, resolves the upstream, executes with preflight and
    /// rollback. The production command plans/judges/executes under ONE lock
    /// span via [`Self::prepare_restack`] + [`Self::execute_restack`] instead.
    pub fn restack(repo_path: &str, branch: &str, onto: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _guard = guard;
        let upstream = Self::prepare_restack(&repo, branch, onto)?;
        Self::execute_restack(&repo, branch, onto, &upstream)
    }

    pub fn create_tag(
        repo_path: &str,
        tag_name: &str,
        commit_id: Option<&str>,
        message: Option<&str>,
    ) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(tag_name)?;
        let mut args = vec!["tag"];
        if let Some(msg) = message {
            args.push("-a");
            args.push(tag_name);
            args.push("-m");
            args.push(msg);
        } else {
            args.push(tag_name);
        }
        if let Some(cid) = commit_id {
            validate_oid(cid)?;
            args.push(cid);
        }
        git_text(&repo, &args)?;
        Ok(())
    }

    pub fn delete_tag(repo_path: &str, tag_name: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(tag_name)?;
        git_text(&repo, &["tag", "-d", tag_name])?;
        Ok(())
    }

    /// Discards working-tree changes at `file_path`: `git restore` reverts
    /// tracked modifications, `git clean` removes untracked entries.
    ///
    /// Neither failure may read as success. A failed restore is an error —
    /// except when the path was an untracked file that `clean` then removed
    /// (restore cannot match untracked paths, so that pair of outcomes means
    /// the discard completed). A clean failure after a successful restore is
    /// also an error: the tree is half-discarded, and the caller must know.
    pub fn discard_changes(repo_path: &str, file_path: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dest = sandbox_join(&repo, file_path)?;
        // Existence before the fact is what separates "untracked file that
        // clean will remove" from a pathspec that never matched anything.
        let existed_before = std::fs::symlink_metadata(&dest).is_ok();
        // Same :(literal) convention as stage/unstage: glob metacharacters in
        // a user path (notably "*") must match exactly themselves, never
        // expand across the working tree.
        let spec = format!(":(literal){file_path}");
        let restore_result = git_text(&repo, &["restore", "--", &spec]);
        let clean_result = git_text(&repo, &["clean", "-f", "--", &spec]);
        match (restore_result, clean_result) {
            (Ok(_), Ok(_)) => Ok(()),
            (Ok(_), Err(e)) => Err(format!(
                "restored '{}' but cleaning untracked files failed: {}",
                file_path, e
            )),
            (Err(_restore_err), Ok(_)) if existed_before && !dest.exists() => {
                // Purely untracked path: restore could not match it (expected),
                // and clean removed it, so the requested end state was reached.
                Ok(())
            }
            (Err(restore_err), _) => Err(restore_err),
        }
    }

    pub fn stash_save(repo_path: &str, message: Option<&str>) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(msg) = message {
            git_text(&repo, &["stash", "push", "-u", "-m", msg])
        } else {
            git_text(&repo, &["stash", "push", "-u"])
        }
    }

    pub fn stash_pop(repo_path: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        git_text(&repo, &["stash", "pop"])
    }

    /// Refuses when the worktree is already parked mid-operation, naming it.
    ///
    /// Starting a cherry-pick on top of a parked rebase does not queue behind
    /// it — git refuses with a message about internals, or worse, the second
    /// operation's control files collide with the first's. Consulting the same
    /// detector the recovery banner uses means the refusal names the operation
    /// the user can actually see and abort.
    fn refuse_if_parked(repo_canon: &Path, verb: &str) -> Result<(), String> {
        if let Some(parked) = crate::engine::repo_op::detect(repo_canon)? {
            return Err(format!(
                "Cannot {verb} while a {} is in progress. Finish or abort it first.",
                parked.kind.label()
            ));
        }
        Ok(())
    }

    /// Renders the argv for a cherry-pick or revert of `commits`.
    ///
    /// Shared by the write gate and the executor so the judged line is the run
    /// line. `--` is not applicable (these take revisions, not paths), so the
    /// leading-dash rejection in `validate_oid_or_revision` is what keeps a
    /// commit-ish from turning into a flag.
    pub fn replay_argv<'a>(
        subcommand: &'a str,
        commits: &'a [String],
        no_commit: bool,
    ) -> Vec<&'a str> {
        let mut argv = vec!["git", subcommand];
        if no_commit {
            argv.push("--no-commit");
        } else {
            // Both verbs open an editor for the message by default. The editor
            // is pinned to `true` process-wide, but saying `--no-edit` here
            // makes the intent explicit in the line the gate judges.
            argv.push("--no-edit");
        }
        for commit in commits {
            argv.push(commit.as_str());
        }
        argv
    }

    /// Validates the commit list shared by cherry-pick and revert.
    ///
    /// An empty list would make git operate on `HEAD` implicitly for some
    /// verbs, which is never what a UI meant to send.
    fn validate_replay_commits(commits: &[String]) -> Result<(), String> {
        if commits.is_empty() {
            return Err("No commits were selected".into());
        }
        if commits.len() > MAX_REPLAY_COMMITS {
            return Err(format!(
                "Too many commits selected ({}); the limit is {MAX_REPLAY_COMMITS}",
                commits.len()
            ));
        }
        for commit in commits {
            validate_oid_or_revision(commit)?;
        }
        Ok(())
    }

    /// Replays `commits` onto the current branch.
    ///
    /// A conflict leaves the repository parked mid-cherry-pick, which is a
    /// legitimate outcome rather than a corruption: `repo_op` detects it and
    /// the banner offers continue/skip/abort. The error therefore reports the
    /// conflict without attempting a rollback that would discard the user's
    /// chance to resolve it.
    pub fn cherry_pick(
        repo_path: &str,
        commits: &[String],
        no_commit: bool,
    ) -> Result<String, String> {
        Self::replay(repo_path, "cherry-pick", commits, no_commit)
    }

    /// Records the inverse of `commits` as new commits. Same parked-on-conflict
    /// semantics as [`cherry_pick`].
    pub fn revert(repo_path: &str, commits: &[String], no_commit: bool) -> Result<String, String> {
        Self::replay(repo_path, "revert", commits, no_commit)
    }

    fn replay(
        repo_path: &str,
        subcommand: &str,
        commits: &[String],
        no_commit: bool,
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        Self::validate_replay_commits(commits)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::refuse_if_parked(&repo, subcommand)?;
        let argv = Self::replay_argv(subcommand, commits, no_commit);
        git_text(&repo, &argv[1..])
    }

    /// Renders the argv for a reset. Shared by the gate and the executor.
    pub fn reset_argv(mode: ResetMode, target: &str) -> Vec<&str> {
        vec!["git", "reset", mode.flag(), target]
    }

    /// Moves the current branch to `target`, discarding as much as `mode` says.
    ///
    /// `--hard` destroys uncommitted work irrecoverably, which is why the mode
    /// is an enum rather than a passthrough string: no caller can invent a
    /// fifth mode, and the write gate sees a rendered line it can rank by
    /// destructiveness.
    pub fn reset(repo_path: &str, mode: ResetMode, target: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        validate_oid_or_revision(target)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A reset mid-merge silently abandons the merge's state instead of
        // ending it; the user wants abort, and the banner offers it.
        Self::refuse_if_parked(&repo, "reset")?;
        let argv = Self::reset_argv(mode, target);
        git_text(&repo, &argv[1..])
    }

    pub fn clone_repo(url: &str, target_dir: &str) -> Result<String, String> {
        validate_clone_url(url)?;
        let requested = Path::new(target_dir);
        let dest = resolve_clone_destination(requested)?;
        if dest.join(".git").exists() {
            return Err("Destination is already a Git repository".into());
        }
        let clone_path = if dest.is_dir() {
            resolve_clone_destination(&dest.join(crate::engine::git_cli::repo_name_from_url(url)))?
        } else {
            dest
        };
        if clone_path.join(".git").exists() {
            return Err(format!("Already cloned at {}", clone_path.display()));
        }
        let clone_str = clone_path.to_string_lossy().into_owned();
        if let Err(clone_err) = git_global(&["clone", "--", url, &clone_str]) {
            // git materializes <dest>/.git before transferring objects, so a
            // failed or timed-out clone leaves a skeleton behind that blocks
            // every retry ("already exists"). Its absence was verified just
            // above, so anything present now came from our own attempt:
            // remove it best-effort so a retry starts clean.
            let leftover = clone_path.join(".git");
            if leftover.is_dir() {
                let removal = std::fs::remove_dir_all(&leftover);
                return Err(partial_clone_message(&clone_err, &leftover, removal));
            }
            return Err(clone_err);
        }
        Ok(clone_str)
    }
}

/// Composes the user-facing error for a clone that failed and left a partial
/// `.git` behind, reporting the cleanup that *happened*.
///
/// Claiming a removal that failed is worse than not attempting one: the user
/// retries on the strength of it and the retry dies at the `.git` existence
/// check with "Already cloned at ...", having just been told the path was
/// clear. Taking the removal's own result as an argument is what makes the
/// failing branch reachable from a test — in place it needs a filesystem that
/// refuses to delete a directory git created moments earlier.
fn partial_clone_message(clone_err: &str, leftover: &Path, removal: std::io::Result<()>) -> String {
    match removal {
        Ok(()) => format!(
            "clone failed ({clone_err}); removed the partial '.git' left at {}",
            leftover.display()
        ),
        Err(rm_err) => format!(
            "clone failed ({clone_err}); the partial '.git' at {} could not be removed \
             ({rm_err}) and will block a retry until it is deleted",
            leftover.display()
        ),
    }
}

/// Composes the user-facing error for a failed rebase step together with any
/// recovery failures that occurred while rolling back.
///
/// The step error always leads verbatim. When `abort_result` is
/// `Some(Err(..))` (a cherry-pick was mid-flight and could not be aborted) or
/// `restore_result` is `Err(..)` (the checkout back to the original branch /
/// HEAD failed), an explicit clause names the failure and the true end-state,
/// so a locked index or full disk can never hide behind the primary error.
/// Pure: no git, fully unit-testable.
fn combine_step_failure_with_recovery(
    step_error: &str,
    abort_result: Option<&Result<(), String>>, // None when no cherry-pick was in progress
    restore_result: &Result<(), String>,
    original_branch: Option<&str>,
    original_head: &str,
) -> String {
    let mut message = step_error.to_string();
    if let Some(Err(abort_err)) = abort_result {
        message.push_str(&format!(
            "; additionally, `cherry-pick --abort` failed ({abort_err}) — \
             the repository may still be mid-cherry-pick"
        ));
    }
    if let Err(restore_err) = restore_result {
        let target = match original_branch {
            Some(branch) => branch.to_string(),
            None => format!("HEAD {original_head}"),
        };
        message.push_str(&format!(
            "; additionally, restoring {target} failed ({restore_err}) — \
             HEAD may remain detached at the rebase base"
        ));
    }
    message
}

/// Resolves a clone destination the way [`crate::engine::git_cli::sandbox_join_canonical`]
/// resolves in-repo paths: the parent is canonicalized (resolving every
/// existing symlinked prefix, including macOS `/var` → `/private/var`), an
/// existing final component must not be a symlink itself, and whatever exists
/// must stay under the canonical parent. The returned path is what git is told
/// to create, so a symlinked destination can no longer redirect a clone past
/// the directory the caller picked.
fn resolve_clone_destination(dest: &Path) -> Result<PathBuf, String> {
    if !dest.is_absolute() {
        return Err("Clone destination must be an absolute path".into());
    }
    let name = dest.file_name().ok_or_else(|| {
        format!(
            "Clone destination '{}' does not name a directory entry",
            dest.display()
        )
    })?;
    let parent = dest.parent().ok_or_else(|| {
        format!(
            "Clone destination '{}' has no parent directory",
            dest.display()
        )
    })?;
    let parent_canonical = parent.canonicalize().map_err(|e| {
        format!(
            "Cannot resolve clone destination parent '{}': {}",
            parent.display(),
            e
        )
    })?;
    match std::fs::symlink_metadata(dest) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "Clone destination '{}' is a symlink; refusing so the clone cannot land \
                     outside the chosen location",
                    dest.display()
                ));
            }
            let actual = dest.canonicalize().map_err(|e| {
                format!(
                    "Cannot resolve clone destination '{}': {}",
                    dest.display(),
                    e
                )
            })?;
            if !actual.starts_with(&parent_canonical) {
                return Err(format!(
                    "Clone destination '{}' escapes its parent via symlink",
                    actual.display()
                ));
            }
            Ok(actual)
        }
        // Not yet present: the remaining components stay purely lexical,
        // which is safe because `..`/relative forms were refused above and
        // the parent was resolved through every existing link.
        Err(_) => Ok(parent_canonical.join(name)),
    }
}

/// True when `branch_name` is the branch the repository's HEAD resolves to by
/// default: the primary remote's HEAD branch first, then conventional
/// main/master/trunk/develop — the same resolution `list_branches` uses.
fn is_default_branch(repo: &Path, branch_name: &str) -> bool {
    let remote = crate::engine::git_reader::resolve_default_remote(repo);
    let head_ref = crate::engine::git_reader::remote_head_ref(&remote);
    let remote_head = git_text(repo, &["symbolic-ref", "--quiet", head_ref.as_str()]).ok();
    crate::engine::git_reader::resolve_default_base_on(repo, &remote, remote_head.as_deref())
        .is_some_and(|(short, _)| short == branch_name)
}

/// True when any worktree of `repo` (including the main one) has
/// `branch_name` checked out.
fn is_checked_out_in_any_worktree(repo: &Path, branch_name: &str) -> Result<bool, String> {
    let stdout = git_text(repo, &["worktree", "list", "--porcelain"])?;
    let target = format!("refs/heads/{branch_name}");
    Ok(stdout
        .lines()
        .any(|line| line.strip_prefix("branch ").map(str::trim) == Some(target.as_str())))
}

/// Rewrites only the subject line of a commit message, keeping the body —
/// including its blank-line separation from the subject — intact.
fn reworded_message(original: &str, new_subject: &str) -> String {
    match original.split_once('\n') {
        Some((_, rest)) => format!("{new_subject}\n{rest}"),
        None => new_subject.to_string(),
    }
}

/// Rejects clone URLs whose transport could execute local commands.
///
/// Allowlist: `http(s)://`, `ssh://`, `git://`, `ftp(s)://`, `file://`, plus
/// bare local paths (absolute or relative) and scp-like `user@host:path`
/// shorthand. Everything else is refused — notably git's pseudo-transports,
/// where `<scheme>::<args>` hands the argument string to an arbitrary helper
/// (`ext::sh -c <cmd>` executes it through /bin/sh). A leading `-` is refused
/// so a URL can never be parsed as a git option.
pub fn validate_clone_url(url: &str) -> Result<(), String> {
    if url.is_empty() || url.contains('\0') || url.chars().any(|c| c.is_control()) {
        return Err("Invalid clone URL".into());
    }
    if url.starts_with('-') {
        return Err("Clone URL must not start with '-'".into());
    }
    const ALLOWED_SCHEMES: [&str; 7] = [
        "http://", "https://", "ssh://", "git://", "ftps://", "ftp://", "file://",
    ];
    let lower = url.to_ascii_lowercase();
    if ALLOWED_SCHEMES
        .iter()
        .any(|scheme| lower.starts_with(scheme))
    {
        return Ok(());
    }
    // Any remaining `<scheme>::` pseudo-transport form is rejected wholesale;
    // plain local paths and scp-like syntax carry no `::` and fall through.
    if url.contains("::") {
        let scheme = url.split(':').next().unwrap_or("");
        return Err(format!(
            "Clone URL uses unsupported transport '{scheme}': use http(s), ssh, git, ftp(s), \
             file, or a local path"
        ));
    }
    Ok(())
}

/// Condenses raw git stderr for user-facing mutation errors: drops the
/// advisory `hint:` lines (they prescribe terminal commands this client does
/// not expose, e.g. `git rebase --continue`), collapses whitespace runs, and
/// caps the length so a pathological failure cannot flood the UI banner.
pub(crate) fn summarize_git_failure(raw: &str) -> String {
    let meaningful: String = raw
        .lines()
        .filter(|line| !line.trim_start().starts_with("hint:"))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    const CAP: usize = 400;
    if meaningful.chars().count() <= CAP {
        meaningful
    } else {
        let truncated: String = meaningful.chars().take(CAP).collect();
        format!("{truncated}…")
    }
}

pub fn validate_ref_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.starts_with('-') || name.contains('\0') {
        return Err("Invalid ref name".into());
    }
    if name.contains("..") {
        return Err("Invalid ref name: contains traversal '..'".into());
    }
    if name.starts_with('.') || name.ends_with('.') || name.ends_with('/') {
        return Err("Invalid ref name: invalid prefix or suffix".into());
    }
    // A ".lock" suffix on ANY component collides with git's lock files under
    // .git/refs/ — not just when the whole ref ends in .lock. The split also
    // covers the single-component case.
    for component in name.split('/') {
        if component.starts_with('.') || component.ends_with(".lock") {
            return Err("Invalid ref name: invalid path component".into());
        }
    }
    if name == "@" || name.contains("@{") {
        return Err("Invalid ref name: invalid '@' sequence".into());
    }
    if name.contains("//") {
        return Err("Invalid ref name: contains '//'".into());
    }
    if name.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                ' ' | '~'
                    | '^'
                    | ':'
                    | '?'
                    | '*'
                    | '['
                    | ']'
                    | '\\'
                    | ';'
                    | '`'
                    | '$'
                    | '|'
                    | '&'
                    | '<'
                    | '>'
                    | '!'
                    | '('
                    | ')'
                    | '{'
                    | '}'
                    | '='
                    | '"'
                    | '\''
            )
    }) {
        return Err("Invalid ref name: contains forbidden characters".into());
    }
    Ok(())
}

pub fn validate_oid(oid: &str) -> Result<(), String> {
    if oid.is_empty() || oid.len() > 64 || !oid.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Invalid commit id".into());
    }
    Ok(())
}

/// Validates a single revision argument supplied by the UI.
///
/// Caller audit (all four call sites pass exactly one commit-ish each,
/// sourced from the frontend as OIDs or branch names):
///
/// - `execute_rebase_sequence`: `onto_commit` → `checkout --detach`, step
///   `commit_id` → `cherry-pick`;
/// - `worktree::add_worktree`: `start_point` → `worktree add … <start>`;
/// - `git_reader::get_file_blob`: builds `<rev>:<path>` and *independently
///   rejects* any `:` before doing so.
///
/// No caller ever passes ranges (`a..b`), reflog syntax (`@{u}`, `HEAD@{1}`),
/// or peel suffixes (`^{tree}`), so tightening to forbid them is proven safe:
/// `:` would inject into downstream `rev:path` specs, `{}` enables the
/// reflog/peel/search grammar, and `..` turns a single revision into a range.
pub fn validate_oid_or_revision(rev: &str) -> Result<(), String> {
    if rev.is_empty() || rev.starts_with('-') || rev.contains('\0') {
        return Err("Invalid revision".into());
    }
    if rev.contains("..") {
        return Err("Invalid revision: ranges ('..') are not accepted".into());
    }
    if rev.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                ' ' | ';' | '&' | '|' | '`' | '$' | '(' | ')' | '<' | '>' | ':' | '{' | '}'
            )
    }) {
        return Err("Invalid revision".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Git words an empty commit two ways depending on tree state:
    /// "nothing to commit, working tree clean" and "nothing added to commit
    /// but untracked files present". Both exit 1; both mean retry.
    fn is_empty_commit_refusal(error: &str) -> bool {
        let lower = error.to_lowercase();
        lower.contains("nothing to commit") || lower.contains("nothing added to commit")
    }

    #[test]
    fn test_validate_ref_name() {
        assert!(validate_ref_name("feat/auth").is_ok());
        assert!(validate_ref_name("main").is_ok());
        assert!(validate_ref_name("v1.0.0").is_ok());

        assert!(validate_ref_name("-evil").is_err());
        assert!(validate_ref_name("foo bar").is_err());
        assert!(validate_ref_name("refs/../evil").is_err());
        assert!(validate_ref_name("feature.lock").is_err());
        // A ".lock" suffix on ANY component collides with git's lock files,
        // not only when it ends the whole ref.
        assert!(validate_ref_name("feature/foo.lock").is_err());
        assert!(validate_ref_name("foo.lock/bar").is_err());
        assert!(validate_ref_name("feature/lockfile").is_ok());
        assert!(validate_ref_name("foo.lockdown").is_ok());
        assert!(validate_ref_name("branch/").is_err());
        assert!(validate_ref_name(".hidden").is_err());
        assert!(validate_ref_name("foo..bar").is_err());
        assert!(validate_ref_name("@").is_err());
        assert!(validate_ref_name("HEAD@{1}").is_err());
        assert!(validate_ref_name("foo//bar").is_err());
    }

    /// No slash-separated component may start with '.' (git check-ref-format):
    /// "feat/.hidden" must be rejected even though the whole name does not.
    #[test]
    fn test_validate_ref_name_component_dot_rule() {
        assert!(validate_ref_name("feat/.hidden").is_err());
        assert!(validate_ref_name(".hidden/inner").is_err());
        assert!(validate_ref_name("feat/dot.file").is_ok());
        assert!(validate_ref_name("feat/auth").is_ok());
    }

    /// The destination resolver mirrors sandbox_join_canonical: an existing
    /// leaf is canonicalized and must stay under its canonical parent, a
    /// symlinked leaf is refused outright, a missing leaf stays lexical under
    /// the canonical parent, and non-absolute destinations are refused.
    #[cfg(unix)]
    #[test]
    fn resolve_clone_destination_canonicalizes_and_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let base = tempfile::TempDir::new().unwrap();
        let real = base.path().join("real");
        std::fs::create_dir(&real).unwrap();

        // Happy path: existing directory resolves to its canonical spelling
        // (on macOS TempDir paths live behind /var -> /private/var).
        let resolved = resolve_clone_destination(&real).expect("existing dir resolves");
        let canonical_real = real.canonicalize().unwrap();
        assert_eq!(resolved, canonical_real);

        // Missing leaf: joined lexically onto the CANONICAL parent.
        let missing = base.path().join("real").join("fresh-clone");
        let resolved = resolve_clone_destination(&missing).expect("missing leaf resolves");
        assert_eq!(resolved, canonical_real.join("fresh-clone"));

        // Symlinked final component: refused even when it points INSIDE the
        // parent, because git would write through whatever it targets.
        let link = base.path().join("link");
        symlink(&canonical_real, &link).unwrap();
        let err = resolve_clone_destination(&link).expect_err("symlink leaf must refuse");
        assert!(
            err.contains("symlink"),
            "refusal must name the symlink, got: {err}"
        );

        // Relative destination keeps its explicit refusal.
        let err = resolve_clone_destination(Path::new("relative/dest"))
            .expect_err("relative destination must refuse");
        assert!(err.contains("absolute"), "got: {err}");

        // Parent does not exist: cannot establish containment, refuse.
        let orphan = base.path().join("no-such-parent").join("clone");
        assert!(resolve_clone_destination(&orphan).is_err());
    }

    /// Regression (clone destination hardening): a symlinked destination used
    /// to be handed to `git clone` verbatim, so the clone materialized at the
    /// LINK's target — anywhere on disk. The destination must now be refused,
    /// and the link target must stay untouched.
    #[cfg(unix)]
    #[test]
    fn clone_repo_refuses_symlinked_destination_and_leaves_target_untouched() {
        use std::os::unix::fs::symlink;

        let src = init_repo_with_commit();
        let parent = tempfile::TempDir::new().unwrap();
        let elsewhere = tempfile::TempDir::new().unwrap();
        let escape_root = elsewhere.path().join("escape-root");
        std::fs::create_dir(&escape_root).unwrap();

        let link = parent.path().join("innocent-name");
        symlink(&escape_root, &link).unwrap();

        let result = GitWriter::clone_repo(src.path().to_str().unwrap(), link.to_str().unwrap());
        assert!(
            matches!(&result, Err(e) if e.contains("symlink")),
            "symlinked destination must be refused with a reason, got {result:?}"
        );
        let landed: Vec<_> = std::fs::read_dir(&escape_root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert!(
            landed.is_empty(),
            "nothing may be written through the symlink, found {landed:?}"
        );
    }

    #[test]
    fn test_validate_oid() {
        assert!(validate_oid("a1b2c3d4e5f6").is_ok());
        assert!(validate_oid("0123456789abcdef0123456789abcdef01234567").is_ok());
        assert!(validate_oid("").is_err());
        assert!(validate_oid("not-hex!").is_err());
        assert!(validate_oid("; rm -rf /").is_err());
    }

    #[test]
    fn test_validate_oid_or_revision() {
        assert!(validate_oid_or_revision("HEAD~3").is_ok());
        assert!(validate_oid_or_revision("HEAD^").is_ok());
        assert!(validate_oid_or_revision("main").is_ok());
        assert!(validate_oid_or_revision("a1b2c3d4").is_ok());
        assert!(validate_oid_or_revision("; rm -rf /").is_err());
        assert!(validate_oid_or_revision("-evil").is_err());
        // Tightened forms: no caller passes ranges or rev:path / reflog /
        // peel syntax, so they are refused (see the fn's caller audit).
        assert!(validate_oid_or_revision("a..b").is_err());
        assert!(validate_oid_or_revision("@{u}").is_err());
        assert!(validate_oid_or_revision("HEAD@{1}").is_err());
        assert!(validate_oid_or_revision("HEAD^{tree}").is_err());
        assert!(validate_oid_or_revision("rev:path.txt").is_err());
    }

    /// The clone transport allowlist: network schemes, file://, local paths
    /// and scp shorthand pass; pseudo-transports and option-shaped URLs fail.
    #[test]
    fn test_validate_clone_url() {
        for good in [
            "https://github.com/acme/gitpulse.git",
            "http://example.com/repo.git",
            "ssh://git@host/team/repo.git",
            "git://host/repo.git",
            "ftps://host/repo.git",
            "ftp://host/repo.git",
            "file:///tmp/some/repo.git",
            "/tmp/local/path",
            "some-relative-name",
            "git@github.com:acme/gitpulse.git",
            "HTTPS://HOST/REPO.GIT",
        ] {
            assert!(validate_clone_url(good).is_ok(), "{good} must be allowed");
        }
        for evil in [
            "ext::sh -c touch /tmp/gitpulse-pwned",
            "fd::9",
            "vsock::1234",
            "weird-scheme::data",
            "-oProxyCommand=evil",
            "",
            "has\0nul",
            "line\nbreak",
        ] {
            assert!(
                validate_clone_url(evil).is_err(),
                "{evil:?} must be rejected"
            );
        }
    }

    fn configure_identity(dir: &std::path::Path) {
        for (key, value) in [
            ("user.name", "t"),
            ("user.email", "t@t"),
            ("commit.gpgsign", "false"),
        ] {
            let output = std::process::Command::new("git")
                .args(["config", key, value])
                .current_dir(dir)
                .output()
                .expect("git config");
            assert!(
                output.status.success(),
                "git config {key} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    fn init_repo_with_commit() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git init");
        assert!(output.status.success());
        configure_identity(dir.path());
        // Windows runners ship `core.autocrlf=true`, which rewrites the file
        // to CRLF on checkout. That is correct git behaviour, so pin it off
        // here rather than loosening the assertion: what this test is about is
        // that discard restored the content, not what EOLs git converts to.
        let output = std::process::Command::new("git")
            .args(["config", "core.autocrlf", "false"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git config");
        assert!(output.status.success());
        std::fs::write(dir.path().join("tracked.txt"), "base\n").unwrap();
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["add", "--", "tracked.txt"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git add");
        assert!(output.status.success());
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-m",
                "init",
            ])
            .current_dir(dir.path())
            .output()
            .expect("spawn git commit");
        assert!(
            output.status.success(),
            "commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        dir
    }

    /// Regression: a pathspec that matches nothing must surface as an error.
    /// The old body discarded both `restore` and `clean` failures and always
    /// returned Ok, so a typo'd path silently read as "discard succeeded".
    #[test]
    fn discard_changes_errors_when_pathspec_matches_nothing() {
        let dir = init_repo_with_commit();
        let result = GitWriter::discard_changes(dir.path().to_str().unwrap(), "ghost.txt");
        assert!(
            result.is_err(),
            "unknown pathspec must not report success, got {:?}",
            result
        );
    }

    #[test]
    fn discard_changes_reverts_tracked_modification() {
        let dir = init_repo_with_commit();
        std::fs::write(dir.path().join("tracked.txt"), "dirty\n").unwrap();
        GitWriter::discard_changes(dir.path().to_str().unwrap(), "tracked.txt")
            .expect("discard of a modified tracked file should succeed");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "base\n",
            "working-tree content must be restored"
        );
    }

    #[test]
    fn discard_changes_removes_untracked_file() {
        let dir = init_repo_with_commit();
        std::fs::write(dir.path().join("fresh.txt"), "new\n").unwrap();
        // `git restore` fails for an untracked pathspec; the discard still
        // succeeded when `clean` removed the file, so this stays Ok.
        GitWriter::discard_changes(dir.path().to_str().unwrap(), "fresh.txt")
            .expect("discard of an untracked file should succeed");
        assert!(
            !dir.path().join("fresh.txt").exists(),
            "untracked file should be gone"
        );
    }

    fn write_commit(dir: &tempfile::TempDir, file: &str, content: &str, msg: &str) -> String {
        std::fs::write(dir.path().join(file), content).unwrap();
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["add", "--", file])
            .current_dir(dir.path())
            .output()
            .expect("spawn git add");
        assert!(output.status.success());
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-m",
                msg,
            ])
            .current_dir(dir.path())
            .output()
            .expect("spawn git commit");
        assert!(
            output.status.success(),
            "commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output()
            .expect("rev-parse");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn head_message(dir: &tempfile::TempDir) -> String {
        let output = std::process::Command::new("git")
            .args(["log", "-1", "--format=%B"])
            .current_dir(dir.path())
            .output()
            .expect("git log");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    /// Regression (audit B1): Squash folds B into A but must preserve A's
    /// commit message. The old code amended with -m "Squashed commit",
    /// destroying the squashed-into commit's real message.
    #[test]
    fn rebase_squash_preserves_first_commit_message() {
        let dir = init_repo_with_commit();
        let base = write_commit(&dir, "a.txt", "a\n", "base commit");
        let c_a = write_commit(&dir, "b.txt", "b\n", "add feature A\n\nbody of A");
        let _c_b = write_commit(&dir, "c.txt", "c\n", "add feature B");

        let steps = vec![
            RebaseStep {
                commit_id: c_a.clone(),
                action: RebaseActionKind::Pick,
            },
            RebaseStep {
                commit_id: _c_b.clone(),
                action: RebaseActionKind::Squash,
            },
        ];
        GitWriter::execute_rebase_sequence(dir.path().to_str().unwrap(), &base, &steps)
            .expect("pick+squash sequence should succeed");

        let msg = head_message(&dir);
        assert_eq!(
            msg, "add feature A\n\nbody of A",
            "squash must fold into the picked commit without replacing its message"
        );
    }

    /// Regression (audit B2): starting a rebase with uncommitted changes must
    /// be refused up front; the old rollback (`checkout -f`) wiped them.
    #[test]
    fn rebase_refuses_dirty_working_tree_and_leaves_it_intact() {
        let dir = init_repo_with_commit();
        let base = write_commit(&dir, "a.txt", "a\n", "base commit");
        let c_a = write_commit(&dir, "b.txt", "b\n", "commit A");

        std::fs::write(
            dir.path().join("tracked.txt"),
            "precious uncommitted work\n",
        )
        .unwrap();

        let result = GitWriter::execute_rebase_sequence(
            dir.path().to_str().unwrap(),
            &base,
            &[RebaseStep {
                commit_id: c_a,
                action: RebaseActionKind::Pick,
            }],
        );
        assert!(result.is_err(), "dirty tree must refuse to rebase");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "precious uncommitted work\n",
            "uncommitted changes must survive the refusal untouched"
        );
        let branch = std::process::Command::new("git")
            .args(["symbolic-ref", "--short", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(branch.stdout).unwrap().trim(),
            "main",
            "must still be on the original branch after refusal"
        );
    }

    /// Regression (audit A7): when every checkout strategy fails, the first
    /// (most meaningful) error must surface, not the last retry's.
    #[test]
    fn checkout_branch_reports_first_error_when_all_strategies_fail() {
        let dir = init_repo_with_commit();
        std::fs::write(dir.path().join("tracked.txt"), "dirty\n").unwrap();
        let err = GitWriter::checkout_branch(dir.path().to_str().unwrap(), "nonexistent-branch")
            .expect_err("missing branch must fail");
        assert!(
            err.contains("nonexistent-branch") || err.to_lowercase().contains("invalid"),
            "first error should name the failing ref, got: {err}"
        );
    }

    /// The per-repo mutation registry hands out one lock instance per repo
    /// path (canonicalized by validate_repo upstream) and distinct ones for
    /// different repos, so unrelated repos never serialize against each other.
    #[test]
    fn repo_mutation_lock_is_stable_per_repo_and_distinct_across_repos() {
        let dir_a = init_repo_with_commit();
        let dir_b = init_repo_with_commit();
        let canon_a = validate_repo(dir_a.path().to_str().unwrap()).unwrap();
        let canon_b = validate_repo(dir_b.path().to_str().unwrap()).unwrap();
        let l1 = super::repo_mutation_lock(&canon_a);
        let l2 = super::repo_mutation_lock(&canon_a);
        let l3 = super::repo_mutation_lock(&canon_b);
        assert!(Arc::ptr_eq(&l1, &l2), "same repo must yield the same lock");
        assert!(
            !Arc::ptr_eq(&l1, &l3),
            "different repos must not share a lock"
        );
    }

    /// Stress: concurrent mutations on one repo must all land, in either
    /// order, with no lost updates or index.lock failures — the per-repo
    /// mutation lock serializes them before git ever sees a race.
    #[test]
    fn concurrent_commits_on_one_repo_all_land_without_loss() {
        use std::sync::Barrier;
        let dir = init_repo_with_commit();
        let path = dir.path().to_str().unwrap().to_string();
        const THREADS: usize = 8;
        const PER_THREAD: usize = 4;
        let barrier = Arc::new(Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    for i in 0..PER_THREAD {
                        let file = format!("t{t}_f{i}.txt");
                        let content = format!("thread {t} round {i}\n");
                        std::fs::write(std::path::Path::new(&path).join(&file), content.clone())
                            .unwrap();
                        // Stage→commit spans two lock acquisitions, so a
                        // sibling mutation may consume this thread's staged
                        // entry first and leave `git commit` with nothing to
                        // do. Once HEAD carries our exact content the work
                        // has landed (under a sibling's message), and a
                        // re-stage can never make our own commit non-empty
                        // again — that outcome is success, not a retry.
                        let mut attempts = 0;
                        loop {
                            GitWriter::stage_file(&path, &file)
                                .unwrap_or_else(|e| panic!("stage {t}.{i}: {e}"));
                            match GitWriter::commit(&path, &format!("commit t{t}.{i}"), false) {
                                Ok(_) => break,
                                Err(e) if is_empty_commit_refusal(&e) => {
                                    let landed = std::process::Command::new("git")
                                        .args(["show", &format!("HEAD:{file}")])
                                        .current_dir(std::path::Path::new(&path))
                                        .output()
                                        .expect("git show HEAD:path");
                                    if landed.status.success()
                                        && String::from_utf8_lossy(&landed.stdout) == content
                                    {
                                        break;
                                    }
                                    attempts += 1;
                                    assert!(
                                        attempts < 200,
                                        "stage/commit retry never converged for {t}.{i}"
                                    );
                                }
                                Err(e) => panic!("commit {t}.{i}: {e}"),
                            }
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("worker thread must not panic");
        }
        // The invariant is no lost updates, not one commit object per worker
        // round: under the shared index a sibling's commit legitimately
        // carries several workers' staged entries in one commit. Pickaxe over
        // each unique content proves it landed exactly once.
        for t in 0..THREADS {
            for i in 0..PER_THREAD {
                let file = format!("t{t}_f{i}.txt");
                let needle = format!("thread {t} round {i}");
                let log = std::process::Command::new("git")
                    .args(["log", "--format=%H", "-S", &needle, "--", &file])
                    .current_dir(dir.path())
                    .output()
                    .unwrap();
                let stdout = String::from_utf8(log.stdout).unwrap();
                let hits: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
                assert_eq!(
                    hits.len(),
                    1,
                    "content of {file} must appear in exactly one commit, found {hits:?}"
                );
            }
        }
    }

    /// Stress: `commit_files` is the atomic stage+commit primitive. Under
    /// concurrency every round must produce its OWN commit — no sibling can
    /// absorb its staged bytes because stage and commit share one lock
    /// acquisition.
    #[test]
    fn concurrent_commit_files_produce_one_commit_per_round() {
        let dir = init_repo_with_commit();
        let path = dir.path().to_str().unwrap().to_string();
        const THREADS: usize = 8;
        const PER_THREAD: usize = 4;
        let barrier = Arc::new(std::sync::Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    for i in 0..PER_THREAD {
                        let file = format!("t{t}_f{i}.txt");
                        std::fs::write(
                            std::path::Path::new(&path).join(&file),
                            format!("thread {t} round {i}\n"),
                        )
                        .unwrap();
                        GitWriter::commit_files(
                            &path,
                            &format!("commit t{t}.{i}"),
                            std::slice::from_ref(&file),
                        )
                        .unwrap_or_else(|e| panic!("commit_files {t}.{i}: {e}"));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("worker thread must not panic");
        }
        let log = std::process::Command::new("git")
            .args(["log", "--format=%s"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8(log.stdout).unwrap();
        assert_eq!(
            stdout.lines().count(),
            1 + THREADS * PER_THREAD,
            "atomic commit_files must yield exactly one commit per round"
        );
        for t in 0..THREADS {
            for i in 0..PER_THREAD {
                let msg = format!("commit t{t}.{i}");
                assert_eq!(
                    stdout.lines().filter(|l| *l == msg).count(),
                    1,
                    "{msg} must appear exactly once"
                );
            }
        }
    }

    /// `commit_files` must refuse empty inputs instead of silently creating
    /// an empty commit or running `git add` with no pathspec.
    #[test]
    fn commit_files_rejects_empty_inputs() {
        let dir = init_repo_with_commit();
        let path = dir.path().to_str().unwrap().to_string();
        assert!(GitWriter::commit_files(&path, "msg", &[]).is_err());
        assert!(GitWriter::commit_files(&path, "msg", &[String::new()]).is_err());
        assert!(GitWriter::commit_files(&path, "   ", &["a.txt".into()]).is_err());
        assert!(GitWriter::commit_files(&path, "msg", &["../escape.txt".into()]).is_err());
        assert_eq!(head_message(&dir), "init", "no commit may be created");
    }

    #[test]
    fn quick_commit_add_argv_is_add_all() {
        assert_eq!(GitWriter::QUICK_COMMIT_ADD_ARGV, &["add", "--all"]);
    }

    #[test]
    fn quick_commit_refuses_empty_message_and_clean_tree() {
        let dir = init_repo_with_commit();
        configure_identity(dir.path());
        let path = dir.path().to_str().unwrap().to_string();
        assert!(GitWriter::quick_commit(&path, "   ").is_err());
        assert!(
            GitWriter::quick_commit(&path, "feat: nothing").is_err(),
            "a clean tree must not produce a commit"
        );
        assert_eq!(head_message(&dir), "init");
    }

    #[test]
    fn quick_commit_stages_unstaged_untracked_and_deletions_but_not_ignored() {
        let dir = init_repo_with_commit();
        configure_identity(dir.path());
        let path = dir.path().to_str().unwrap().to_string();
        std::fs::write(dir.path().join(".gitignore"), "secret.txt\n").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "do not commit\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "untracked\n").unwrap();
        std::fs::write(dir.path().join("tracked.txt"), "edited\n").unwrap();
        std::fs::write(dir.path().join("doomed.txt"), "gone soon\n").unwrap();
        GitWriter::commit_files(
            &path,
            "chore: seed extra",
            &["doomed.txt".into(), ".gitignore".into()],
        )
        .expect("seed");
        std::fs::remove_file(dir.path().join("doomed.txt")).unwrap();

        GitWriter::quick_commit(&path, "feat: everything").expect("quick commit");
        assert_eq!(head_message(&dir), "feat: everything");

        let show = std::process::Command::new("git")
            .args(["show", "--name-only", "--pretty=format:", "HEAD"])
            .current_dir(dir.path())
            .output()
            .expect("git show");
        let names = String::from_utf8(show.stdout).unwrap();
        assert!(
            names.contains("new.txt"),
            "untracked file must be committed: {names}"
        );
        assert!(
            names.contains("tracked.txt"),
            "unstaged edit must be committed: {names}"
        );
        assert!(
            names.contains("doomed.txt"),
            "deletion must be committed: {names}"
        );
        assert!(
            !names.contains("secret.txt"),
            "ignored untracked file must stay untracked: {names}"
        );
        assert!(dir.path().join("secret.txt").exists());
    }

    /// Linked worktrees must share the mutation lock keyed by the common git
    /// dir, so two tabs of the same repository serialize instead of racing
    /// on refs while each holding a different working-tree mutex.
    #[test]
    fn repo_mutation_lock_is_shared_across_linked_worktrees() {
        let dir = init_repo_with_commit();
        let parent = tempfile::TempDir::new().unwrap();
        let wt = parent.path().join("agent-wt");
        crate::engine::worktree::add_worktree(
            dir.path().to_str().unwrap(),
            wt.to_str().unwrap(),
            Some("agent/lock-share"),
            Some("main"),
            false,
        )
        .expect("add worktree");
        let canon_main = validate_repo(dir.path().to_str().unwrap()).unwrap();
        let canon_wt = validate_repo(wt.to_str().unwrap()).unwrap();
        assert_ne!(
            canon_main, canon_wt,
            "worktree working directory is distinct from the main checkout"
        );
        let l_main = super::repo_mutation_lock(&canon_main);
        let l_wt = super::repo_mutation_lock(&canon_wt);
        assert!(
            Arc::ptr_eq(&l_main, &l_wt),
            "main checkout and linked worktree must share one mutation lock"
        );
    }

    /// A stale `.git/index.lock` from a concurrent agent must be retried, not
    /// surfaced as a hard failure, once the other process drops it.
    #[test]
    fn stage_retries_while_index_lock_is_held_then_released() {
        let dir = init_repo_with_commit();
        std::fs::write(dir.path().join("tracked.txt"), "dirty\n").unwrap();
        let lock_path = dir.path().join(".git").join("index.lock");
        std::fs::write(&lock_path, b"").unwrap();
        let lock_clone = lock_path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(80));
            let _ = std::fs::remove_file(lock_clone);
        });
        GitWriter::stage_file(dir.path().to_str().unwrap(), "tracked.txt")
            .expect("stage must succeed after the contended lock is released");
    }

    #[test]
    fn rebase_rejects_squash_or_fixup_as_the_first_step() {
        let dir = init_repo_with_commit();
        let base = write_commit(&dir, "a.txt", "a\n", "base commit");
        let c_a = write_commit(&dir, "b.txt", "b\n", "commit A");
        let squash_err = GitWriter::execute_rebase_sequence(
            dir.path().to_str().unwrap(),
            &base,
            &[RebaseStep {
                commit_id: c_a.clone(),
                action: RebaseActionKind::Squash,
            }],
        )
        .expect_err("first-step squash must be refused");
        assert!(
            squash_err.to_lowercase().contains("squash"),
            "error must name squash, got: {squash_err}"
        );
        let fixup_err = GitWriter::execute_rebase_sequence(
            dir.path().to_str().unwrap(),
            &base,
            &[RebaseStep {
                commit_id: c_a,
                action: RebaseActionKind::Fixup,
            }],
        )
        .expect_err("first-step fixup must be refused");
        assert!(
            fixup_err.to_lowercase().contains("fixup"),
            "error must name fixup, got: {fixup_err}"
        );
        assert_eq!(head_message(&dir), "commit A", "HEAD must be untouched");
    }

    /// Pure-message tests for rebase recovery reporting: the step error always
    /// leads verbatim, and every recovery failure (abort and/or restore) must
    /// be named with its true end-state instead of being swallowed behind the
    /// primary failure.
    #[test]
    fn combine_step_failure_clean_recovery_returns_step_error_verbatim() {
        let msg = combine_step_failure_with_recovery(
            "Rebase step failed (pick abc123): conflict",
            None,
            &Ok(()),
            Some("main"),
            "0000000000000000000000000000000000000000",
        );
        assert_eq!(
            msg, "Rebase step failed (pick abc123): conflict",
            "clean recovery must not add any clauses"
        );
    }

    #[test]
    fn combine_step_failure_names_failed_cherry_pick_abort() {
        let msg = combine_step_failure_with_recovery(
            "step boom",
            Some(&Err("index.lock exists".to_string())),
            &Ok(()),
            Some("main"),
            "abc",
        );
        assert!(msg.starts_with("step boom"), "step error leads: {msg}");
        assert!(
            msg.contains("`cherry-pick --abort` failed (index.lock exists)"),
            "{msg}"
        );
        assert!(
            msg.contains("may still be mid-cherry-pick"),
            "end-state must warn about mid-cherry-pick: {msg}"
        );
        assert!(!msg.contains("restoring"), "restore succeeded: {msg}");
    }

    #[test]
    fn combine_step_failure_names_failed_branch_restore() {
        let msg = combine_step_failure_with_recovery(
            "step boom",
            None,
            &Err("disk full".to_string()),
            Some("feature/x"),
            "abc",
        );
        assert!(msg.starts_with("step boom"), "step error leads: {msg}");
        assert!(
            msg.contains("restoring feature/x failed (disk full)"),
            "{msg}"
        );
        assert!(
            msg.contains("HEAD may remain detached"),
            "end-state must warn about detached HEAD: {msg}"
        );
        assert!(
            !msg.contains("abort"),
            "no cherry-pick was in flight: {msg}"
        );
    }

    #[test]
    fn combine_step_failure_reports_both_recovery_failures_in_order() {
        let msg = combine_step_failure_with_recovery(
            "step boom",
            Some(&Err("abort err".to_string())),
            &Err("restore err".to_string()),
            Some("main"),
            "def456",
        );
        assert!(msg.starts_with("step boom"), "{msg}");
        let abort_at = msg
            .find("`cherry-pick --abort` failed")
            .expect("abort clause present");
        let restore_at = msg.find("restoring main failed").expect("restore clause");
        assert!(
            abort_at < restore_at,
            "abort clause precedes restore clause: {msg}"
        );
    }

    #[test]
    fn combine_step_failure_detached_head_restore_names_head_oid() {
        let msg = combine_step_failure_with_recovery(
            "step boom",
            None,
            &Err("locked index".to_string()),
            None,
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        );
        assert!(
            msg.contains(
                "restoring HEAD deadbeefdeadbeefdeadbeefdeadbeefdeadbeef failed \
                          (locked index)"
            ),
            "detached HEAD wording must name the oid: {msg}"
        );
    }

    /// A cleanup that could not run must never report what a cleanup that ran
    /// and succeeded reports: the user retries on the strength of the message
    /// and hits "Already cloned at ..." at the existence check.
    #[test]
    fn a_failed_cleanup_is_never_reported_as_a_removal() {
        let leftover = Path::new("/repos/demo/.git");

        let removed = partial_clone_message("timeout", leftover, Ok(()));
        assert!(removed.contains("removed the partial '.git'"), "{removed}");
        assert!(removed.contains("/repos/demo/.git"), "{removed}");

        let kept = partial_clone_message(
            "timeout",
            leftover,
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "denied",
            )),
        );
        assert!(
            !kept.contains("removed the partial"),
            "a failed removal must not claim to have removed anything: {kept}"
        );
        assert!(kept.contains("could not be removed"), "{kept}");
        assert!(kept.contains("denied"), "the reason must survive: {kept}");
        assert!(
            kept.contains("block a retry"),
            "the consequence must be stated, not left to be discovered: {kept}"
        );

        // Both branches keep the primary failure; the cleanup is the footnote.
        for message in [&removed, &kept] {
            assert!(message.contains("clone failed (timeout)"), "{message}");
        }
    }
}
