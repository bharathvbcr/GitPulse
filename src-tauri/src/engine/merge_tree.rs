//! Lane integration in the object database.
//!
//! `merge-tree` writes the merged tree into the repository's own object
//! database (not a quarantine: that tree is deleted with the quarantine, so
//! it cannot be committed). `commit-tree` records it, and `update-ref` moves
//! `refs/heads/<target>` only when the tip is still the one this call read.
//! The checkout is fast-forwarded afterwards only when it is clean and already
//! on that branch. A conflict, a rejected candidate, or a tip that moved is
//! reported without changing any worktree.
//!
//! The in-checkout merge stays in `worktree` and is used only when this git
//! has no `merge-tree` write. This module does not build that merge.

use crate::engine::git_cli::{
    git_captured, git_supports, git_text, without_lazy_fetch, HOOK_LIST, MERGE_TREE_WRITE_TREE,
};
use crate::engine::git_writer::{is_full_oid, GitWriter};
use crate::engine::object_quarantine::{
    parse_merge_tree_output, refuse_partial_clone, MergeOutcome, QuarantineError,
};
use crate::ipc_error::CommandError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// How this call decides whether `merge-tree` can write a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteTreeMode {
    /// Ask the installed git's version.
    Detect,
    /// Treat the write as available. Tests use this; production does not.
    #[cfg(test)]
    ForceSupported,
    /// Treat the write as unavailable, so the caller takes the fallback or
    /// refuses. Tests use this so an old git need not be installed.
    #[cfg(test)]
    ForceUnsupported,
}

/// Which integrator a merge-and-teardown uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IntegrateConfig {
    /// The wave sets this: below git 2.38 it refuses instead of merging in
    /// the checkout. The Worktrees panel leaves it false and takes the fallback.
    pub require_write_tree: bool,
    pub write_tree: WriteTreeMode,
}

impl Default for IntegrateConfig {
    fn default() -> Self {
        Self {
            require_write_tree: false,
            write_tree: WriteTreeMode::Detect,
        }
    }
}

/// Whether `mode` may compute the merge in the object database.
pub(crate) fn write_tree_available(mode: WriteTreeMode) -> bool {
    match mode {
        WriteTreeMode::Detect => git_supports(MERGE_TREE_WRITE_TREE),
        #[cfg(test)]
        WriteTreeMode::ForceSupported => true,
        #[cfg(test)]
        WriteTreeMode::ForceUnsupported => false,
    }
}

/// Sentence a result carries when the checkout was not moved to the new tip.
pub(crate) const CHECKOUT_NOT_ADVANCED: &str = "checkout not advanced";

/// The option a refusal and a fallback both name, so a reader can tell which
/// git capability was missing.
pub(crate) const WRITE_TREE_FLAG: &str = "merge-tree --write-tree";

/// What the wave says when this git cannot write a merge tree. Nothing is merged.
pub(crate) fn wave_unsupported_message() -> String {
    format!(
        "This git is older than 2.38, so {WRITE_TREE_FLAG} is unavailable. \
         The wave will not merge in the checkout. Nothing was merged."
    )
}

/// What a successful in-checkout fallback says, so the panel can show it.
pub(crate) fn fallback_note() -> String {
    format!(
        "Git is older than 2.38, so this lane was merged in the checkout. \
         {WRITE_TREE_FLAG} is unavailable."
    )
}

/// A candidate check: sees the detached scratch worktree and the candidate
/// commit, and may reject it before the target ref moves.
pub(crate) type CandidateVerifier<'a> = dyn FnMut(&Path, &str) -> Result<(), String> + 'a;

/// Why a checkout that has the target branch checked out cannot be moved with
/// it. Typed, so the wave can hold the task instead of halting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldReason {
    /// A checkout with the target has uncommitted files.
    CheckoutDirty,
    /// A checkout with the target could not be read, so it is not known clean.
    CheckoutUnreadable,
    /// A checkout with the target has `index.lock` held by someone.
    CheckoutLocked,
    /// A repository hook the object-database commit would skip is active.
    CommitHooks,
}

/// What stops one checkout from being moved.
pub(crate) enum HolderProblem {
    Dirty,
    IndexLocked,
    Unreadable(String),
}

/// A worktree (the main checkout or a linked one) that has the target branch
/// checked out. Moving `refs/heads/<target>` moves its HEAD with it.
pub(crate) struct CheckoutHolder {
    pub path: PathBuf,
    pub problem: Option<HolderProblem>,
}

/// Every checkout that has the target checked out, read under the repository lock.
pub(crate) struct CheckoutTouch {
    pub holders: Vec<CheckoutHolder>,
}

/// Why the object-database integration did not move the target.
pub(crate) enum IntegrateFailure {
    Conflict {
        message: String,
        paths: Vec<String>,
        total: usize,
        /// The tree `merge-tree` wrote. No commit was created from it.
        candidate: String,
    },
    /// The candidate commit exists and was rejected before the ref moved.
    CandidateFailed {
        message: String,
        candidate: String,
    },
    Refused {
        message: String,
    },
    /// This git rejected the write. The caller falls back or refuses.
    Unsupported {
        message: String,
    },
    /// A checkout that has the target is not safe to move. Nothing was moved.
    Held {
        reason: HeldReason,
        message: String,
        path: Option<String>,
    },
    /// The commit would skip these active repository hooks. The caller takes
    /// the in-checkout merge, which runs them, or refuses.
    CommitHooks {
        hooks: Vec<String>,
    },
}

/// The target ref now points at `candidate_oid`.
pub(crate) struct ObjectLanded {
    pub candidate_oid: String,
    pub checkout_note: Option<String>,
    pub checkout_advanced: bool,
}

/// Computes the merge of `branch_sha` into `target_sha` and, when it is clean
/// and the candidate is accepted, moves `refs/heads/<target>` with
/// compare-and-swap. `verify`, when set, sees a detached scratch worktree of
/// the candidate commit and may reject it before that move.
///
/// Caller holds the repository mutation lock. `target_sha` is the tip read
/// under that lock. A caller that has to wait on verification uses [`prepare`]
/// and [`land`] instead, and does not hold the lock between them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn integrate(
    repo: &Path,
    branch: &str,
    target: &str,
    target_sha: &str,
    branch_sha: &str,
    squash: bool,
    checkout: &CheckoutTouch,
    verify: Option<&mut CandidateVerifier<'_>>,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<ObjectLanded, IntegrateFailure> {
    let candidate = prepare(repo, branch, target, target_sha, branch_sha, squash, gate)?;
    if let Some(verify) = verify {
        verify_in_scratch(repo, &candidate, verify, gate)?;
    }
    land(repo, branch, target, target_sha, &candidate, checkout, gate)
}

/// Writes the candidate commit and does not move the target ref.
///
/// The candidate is the branch tip on a fast-forward, the target tip when a
/// squash would be empty, and a new commit otherwise. A conflict returns the
/// merged tree and writes no commit.
pub(crate) fn prepare(
    repo: &Path,
    branch: &str,
    target: &str,
    target_sha: &str,
    branch_sha: &str,
    squash: bool,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<String, IntegrateFailure> {
    refuse_partial_clone(repo).map_err(refused)?;
    let fast_forward = is_ancestor(repo, target_sha, branch_sha)?;
    // A squash always records a commit, and a non-fast-forward records a merge
    // commit. Both need an identity before any object is written, matching the
    // in-checkout path, which asks before it stages.
    if squash || !fast_forward {
        GitWriter::require_commit_identity(repo).map_err(refused)?;
    }

    let merged_tree = match merge_trees(repo, target_sha, branch_sha, gate)? {
        MergeOutcome::Clean { tree } => tree,
        MergeOutcome::Conflicted {
            tree,
            paths,
            paths_total,
        } => {
            return Err(IntegrateFailure::Conflict {
                message: conflict_message(branch, target, &paths, paths_total),
                paths,
                total: paths_total,
                candidate: tree,
            });
        }
    };

    let target_tree = tree_of(repo, target_sha)?;
    if !squash && fast_forward {
        // The branch tip already contains the target. Recording another commit
        // would hide that it was a fast-forward.
        return Ok(branch_sha.to_string());
    }
    if squash && merged_tree == target_tree {
        // Nothing new: the change is already on the target. An empty commit
        // would also leave a parked squash message behind, which the
        // in-checkout path deletes instead of committing.
        return Ok(target_sha.to_string());
    }
    // `commit-tree` runs no hooks. The in-checkout merge ran pre-commit,
    // pre-merge-commit and commit-msg (secret scanners live there), so a
    // repository that has any of them active does not get its commit written
    // behind them.
    let hooks = active_commit_hooks(repo, gate).map_err(refused)?;
    if !hooks.is_empty() {
        return Err(IntegrateFailure::CommitHooks { hooks });
    }
    let message = if squash {
        format!("Merge branch '{branch}' (squashed)")
    } else {
        format!("Merge branch '{branch}' into {target}")
    };
    let second = if squash { None } else { Some(branch_sha) };
    commit_tree(repo, &merged_tree, target_sha, second, &message, gate)
}

/// Whether `candidate` is already covered by a verdict the caller holds.
///
/// The target tip is not a new tree. The verified tip, and any commit with
/// that tip's tree, was judged before this merge. Anything else is a combined
/// tree and has to be judged on its own before the ref moves.
pub(crate) fn already_covered(
    repo: &Path,
    candidate: &str,
    target_sha: &str,
    verified_tip: Option<&str>,
) -> Result<bool, IntegrateFailure> {
    if candidate == target_sha {
        return Ok(true);
    }
    let Some(verified) = verified_tip else {
        return Ok(false);
    };
    if candidate == verified {
        return Ok(true);
    }
    Ok(tree_of(repo, candidate)? == tree_of(repo, verified)?)
}

fn refused(error: CommandError) -> IntegrateFailure {
    IntegrateFailure::Refused {
        message: error.into_message(),
    }
}

fn refused_text(message: impl Into<String>) -> IntegrateFailure {
    IntegrateFailure::Refused {
        message: message.into(),
    }
}

fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool, IntegrateFailure> {
    let run = git_captured(repo, &["merge-base", "--is-ancestor", ancestor, descendant])
        .map_err(refused)?;
    match run.status_code {
        0 => Ok(true),
        1 => Ok(false),
        code => Err(refused_text(format!(
            "Could not tell whether {descendant} contains {ancestor} (git exit {code}): {}. Nothing was merged.",
            String::from_utf8_lossy(&run.stderr).trim()
        ))),
    }
}

fn merge_trees(
    repo: &Path,
    target_sha: &str,
    branch_sha: &str,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<MergeOutcome, IntegrateFailure> {
    let args = [
        "merge-tree",
        "--write-tree",
        "--name-only",
        "--no-messages",
        "-z",
        target_sha,
        branch_sha,
    ];
    gate(&args).map_err(refused)?;
    let run = without_lazy_fetch(|| git_captured(repo, &args))
        .map_err(refused)?
        .require_complete("git merge-tree")
        .map_err(|error| refused_text(error.message().to_string()))?;
    parse_merge_tree_output(
        &run.stdout,
        run.status_code,
        &String::from_utf8_lossy(&run.stderr),
    )
    .map_err(|error| match error {
        QuarantineError::Unsupported(reason) => IntegrateFailure::Unsupported { message: reason },
        other => refused_text(other.to_string()),
    })
}

fn tree_of(repo: &Path, oid: &str) -> Result<String, IntegrateFailure> {
    let spec = format!("{oid}^{{tree}}");
    let text = git_text(repo, &["rev-parse", "--verify", spec.as_str()]).map_err(refused)?;
    let tree = text.trim();
    if !is_full_oid(tree) {
        return Err(refused_text(format!(
            "{oid} has no tree this call can record. Nothing was merged."
        )));
    }
    Ok(tree.to_string())
}

fn commit_tree(
    repo: &Path,
    tree: &str,
    first_parent: &str,
    second_parent: Option<&str>,
    message: &str,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<String, IntegrateFailure> {
    let mut owned = vec![
        "commit-tree".to_string(),
        tree.to_string(),
        "-p".to_string(),
        first_parent.to_string(),
    ];
    if let Some(second) = second_parent {
        owned.push("-p".to_string());
        owned.push(second.to_string());
    }
    owned.push("-m".to_string());
    owned.push(message.to_string());
    let args: Vec<&str> = owned.iter().map(String::as_str).collect();
    gate(&args).map_err(refused)?;
    let text = git_text(repo, &args).map_err(refused)?;
    let oid = text.trim();
    if !is_full_oid(oid) {
        return Err(refused_text(format!(
            "commit-tree did not return a commit id ({oid}). Nothing was merged."
        )));
    }
    Ok(oid.to_string())
}

static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Checks `candidate` out, detached, somewhere that is not a worktree yet, and
/// removes that worktree before returning. A rejected candidate does not move
/// the target. A scratch worktree that cannot be removed after a pass also
/// does not: the candidate must not stay registered.
pub(crate) fn verify_in_scratch(
    repo: &Path,
    candidate: &str,
    verify: &mut CandidateVerifier<'_>,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<(), IntegrateFailure> {
    let scratch = std::env::temp_dir().join(format!(
        "gitpulse-candidate-{}-{}",
        std::process::id(),
        SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    if scratch.exists() {
        return Err(refused_text(format!(
            "The scratch checkout {} already exists, so {} was not checked. Nothing was merged.",
            scratch.display(),
            candidate
        )));
    }
    let path = scratch.to_str().ok_or_else(|| {
        refused_text(
            "The scratch checkout path is not UTF-8, so the candidate was not checked. Nothing was merged.",
        )
    })?;
    let add = ["worktree", "add", "--detach", path, candidate];
    gate(&add).map_err(refused)?;
    git_text(repo, &add).map_err(refused)?;

    let verdict = verify(Path::new(path), candidate);

    let remove = ["worktree", "remove", "--force", path];
    let removed = gate(&remove).and_then(|()| git_text(repo, &remove));
    let leftover = match &removed {
        Err(error) => format!(
            " The scratch worktree at {path} could not be removed: {}.",
            error.message()
        ),
        Ok(_) => String::new(),
    };
    match verdict {
        Err(why) => Err(IntegrateFailure::CandidateFailed {
            message: format!(
                "The candidate {candidate} for merging into the target failed verification ({why}). \
                 Nothing was merged.{leftover}"
            ),
            candidate: candidate.to_string(),
        }),
        Ok(()) => match removed {
            Ok(_) => Ok(()),
            Err(_) => Err(refused_text(format!(
                "The candidate {candidate} was verified, but its scratch worktree could not be removed.{leftover} Nothing was merged."
            ))),
        },
    }
}

fn conflict_message(branch: &str, target: &str, paths: &[String], total: usize) -> String {
    let shown: Vec<&str> = paths.iter().take(5).map(String::as_str).collect();
    let more = if total > shown.len() {
        format!(" and {} more", total - shown.len())
    } else {
        String::new()
    };
    format!(
        "Merging {branch} into {target} conflicted in {total} file(s): {}{more}. \
         Nothing was merged and no worktree was changed.",
        shown.join(", ")
    )
}

/// The hook events `git commit` and `git merge` run for the commit this
/// integration records, any of which can abort it, and `commit-tree` runs none.
const COMMIT_HOOKS: [&str; 4] = [
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
];

/// The commit hooks that are active for `repo`, as git 2.54 would run them.
/// [`active_commit_hooks_with`] with this git's own `hook list` support.
pub(crate) fn active_commit_hooks(
    repo: &Path,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<Vec<String>, CommandError> {
    active_commit_hooks_with(repo, git_supports(HOOK_LIST), gate)
}

/// The names in [`COMMIT_HOOKS`] that are active for `repo`. Two sources, both
/// fail closed:
/// - the hooks directory git would use (`core.hooksPath` respected): a regular
///   file with an executable bit. A directory that cannot be read is an error,
///   not "none";
/// - `git hook list <event>` when `hook_list` is set (git 2.54+), which also
///   reports hooks defined in configuration with no file anywhere. Exit 0 lists
///   hooks, exit 1 with "no hooks found" is none, and anything else (a failed
///   run, another exit code) counts as hooks present.
///
/// Below 2.54 there are no config hooks, so the directory is the whole answer.
pub(crate) fn active_commit_hooks_with(
    repo: &Path,
    hook_list: bool,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<Vec<String>, CommandError> {
    let spelled = git_text(repo, &["rev-parse", "--git-path", "hooks"])?;
    let dir = repo.join(spelled.trim());
    let mut active: Vec<String> = Vec::new();
    for name in COMMIT_HOOKS {
        match std::fs::metadata(dir.join(name)) {
            Ok(meta) if meta.is_file() && is_executable(&meta) => active.push(name.to_string()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CommandError::plain(format!(
                    "The hook {name} in {} could not be checked ({error}), so nothing was merged.",
                    dir.display()
                )))
            }
        }
        if hook_list {
            if let Some(found) = listed_hook(repo, name, gate)? {
                if !active.iter().any(|seen| seen.starts_with(name)) {
                    active.push(found);
                }
            }
        }
    }
    Ok(active)
}

/// `git hook list <event>` in `repo`: the event (with the reason, when git
/// could not say) if anything may run for it, `None` when git says there is
/// nothing.
fn listed_hook(
    repo: &Path,
    event: &str,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<Option<String>, CommandError> {
    let args = ["hook", "list", event];
    gate(&args)?;
    let run = match git_captured(repo, &args) {
        Ok(run) => run,
        Err(error) => {
            return Ok(Some(format!(
                "{event} (git hook list could not run: {})",
                error.message()
            )))
        }
    };
    Ok(classify_hook_list(
        event,
        run.status_code,
        &String::from_utf8_lossy(&run.stderr),
    ))
}

/// What `git hook list <event>` said: exit 0 lists hooks, exit 1 with "no hooks
/// found" is none, anything else is hooks that may run (fail closed).
pub(crate) fn classify_hook_list(event: &str, status: i32, stderr: &str) -> Option<String> {
    match status {
        0 => Some(event.to_string()),
        1 if stderr.contains("no hooks found") => None,
        code => Some(format!(
            "{event} (git hook list exited {code}: {})",
            stderr.trim()
        )),
    }
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &std::fs::Metadata) -> bool {
    true
}

/// What the panel says when it merged in the checkout because the object
/// database would have skipped these hooks.
pub(crate) fn hooks_fallback_note(hooks: &[String]) -> String {
    format!(
        "This repository has active {} hook(s), which an object-database commit would skip, \
         so this lane was merged in the checkout.",
        hooks.join(", ")
    )
}

/// What the wave says when it will not skip those hooks. Nothing was merged.
pub(crate) fn hooks_refusal_message(hooks: &[String]) -> String {
    format!(
        "This repository has active {} hook(s), which the object-database commit would skip. \
         The wave will not land a commit behind them. Nothing was merged.",
        hooks.join(", ")
    )
}

fn no_checkout_note(target: &str) -> String {
    format!(
        "{CHECKOUT_NOT_ADVANCED}: no checkout has {target} checked out, so no files were changed."
    )
}

/// Refuses, naming the first checkout with `target` that is not safe to move.
fn refuse_unsafe_holders(target: &str, checkout: &CheckoutTouch) -> Result<(), IntegrateFailure> {
    for holder in &checkout.holders {
        let shown = holder.path.display().to_string();
        let (reason, why) = match &holder.problem {
            None => continue,
            Some(HolderProblem::Dirty) => (
                HeldReason::CheckoutDirty,
                "has uncommitted files".to_string(),
            ),
            Some(HolderProblem::IndexLocked) => (
                HeldReason::CheckoutLocked,
                "has its index.lock held".to_string(),
            ),
            Some(HolderProblem::Unreadable(why)) => (
                HeldReason::CheckoutUnreadable,
                format!("could not be checked ({why})"),
            ),
        };
        return Err(IntegrateFailure::Held {
            reason,
            message: format!(
                "{target} is checked out in {shown}, which {why}. Moving {target} would change \
                 that checkout underneath whoever is using it, so nothing was merged and nothing \
                 was moved."
            ),
            path: Some(shown),
        });
    }
    Ok(())
}

/// Brings one checkout's index and files from `from_tree` to `to_tree`. The
/// index is refreshed first, so a file whose bytes are unchanged but whose
/// mtime moved (an editor save) is not mistaken for a local edit.
fn advance_checkout(
    path: &Path,
    from_tree: &str,
    to_tree: &str,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<(), CommandError> {
    let refresh = ["update-index", "-q", "--refresh"];
    gate(&refresh)?;
    git_text(path, &refresh)?;
    let read = ["read-tree", "-m", "-u", from_tree, to_tree];
    gate(&read)?;
    git_text(path, &read).map(|_| ())
}

/// Whether the index of the checkout at `path` matches its HEAD.
fn index_matches_head(path: &Path) -> bool {
    git_captured(path, &["diff-index", "--cached", "--quiet", "HEAD"])
        .map(|run| run.status_code == 0)
        .unwrap_or(false)
}

/// Puts back what a failed [`advance_checkout`] left half done: the
/// checkouts already advanced move back, then the ref returns to
/// `target_sha` (a compare-and-swap from `candidate`, so a ref somebody else
/// has since moved is left alone). Returns the sentence to report, which says
/// so when something could not be restored.
#[allow(clippy::too_many_arguments)]
fn restore_after_failed_advance(
    repo: &Path,
    target: &str,
    target_sha: &str,
    candidate: &str,
    failed: &CheckoutHolder,
    advanced: &[&CheckoutHolder],
    trees: (&str, &str),
    why: &CommandError,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> String {
    let (old_tree, new_tree) = trees;
    let mut unresolved: Vec<String> = Vec::new();
    for holder in advanced.iter().rev() {
        if let Err(error) = advance_checkout(&holder.path, new_tree, old_tree, gate) {
            unresolved.push(format!(
                "{} could not be moved back ({})",
                holder.path.display(),
                error.message()
            ));
        }
    }
    let refname = format!("refs/heads/{target}");
    let back = ["update-ref", refname.as_str(), target_sha, candidate];
    if let Err(error) = gate(&back).and_then(|()| git_text(repo, &back)) {
        unresolved.push(format!(
            "{refname} could not be put back at {target_sha} ({})",
            error.message()
        ));
    }
    if !index_matches_head(&failed.path) {
        unresolved.push(format!(
            "the index of {} does not match its HEAD",
            failed.path.display()
        ));
    }
    let shown = failed.path.display();
    if unresolved.is_empty() {
        format!(
            "The checkout {shown} could not be updated to the new {target} ({}), so {target} was \
             put back at {target_sha} and nothing was merged.",
            why.message()
        )
    } else {
        format!(
            "The checkout {shown} could not be updated to the new {target} ({}), and the \
             restore was incomplete: {}. Run `git status` in each checkout named here before \
             doing anything else; the lane was kept.",
            why.message(),
            unresolved.join("; ")
        )
    }
}

/// Moves the target ref, then every checkout that has it.
///
/// Before anything moves, each checkout with the target must be clean and
/// readable; one that is not refuses the integration with a typed reason
/// naming it. Nothing is detached: a live session keeps its HEAD. The ref
/// moves by compare-and-swap, then each checkout's index and files follow with
/// `read-tree` (not a merge: the CAS has already refused a target that moved).
/// If one cannot follow, the others and the ref are put back and the failure
/// says what could not be, so HEAD, index and files never silently disagree.
pub(crate) fn land(
    repo: &Path,
    branch: &str,
    target: &str,
    target_sha: &str,
    candidate: &str,
    checkout: &CheckoutTouch,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<ObjectLanded, IntegrateFailure> {
    let moving = candidate != target_sha;
    if moving {
        refuse_unsafe_holders(target, checkout)?;
    }
    // Read before the ref moves: nothing after the CAS may fail for want of them.
    let trees = if moving {
        Some((tree_of(repo, target_sha)?, tree_of(repo, candidate)?))
    } else {
        None
    };

    let refname = format!("refs/heads/{target}");
    let update = [
        "update-ref".to_string(),
        refname,
        candidate.to_string(),
        target_sha.to_string(),
    ];
    let update_args: Vec<&str> = update.iter().map(String::as_str).collect();
    if let Err(error) = gate(&update_args).and_then(|()| git_text(repo, &update_args)) {
        return Err(refused_text(format!(
            "{target} changed while {branch} was being integrated, so the update was refused and nothing was written to refs/heads/{target}: {}",
            error.message()
        )));
    }

    if let Some((old_tree, new_tree)) = &trees {
        let mut advanced: Vec<&CheckoutHolder> = Vec::new();
        for holder in &checkout.holders {
            if let Err(why) = advance_checkout(&holder.path, old_tree, new_tree, gate) {
                return Err(refused_text(restore_after_failed_advance(
                    repo,
                    target,
                    target_sha,
                    candidate,
                    holder,
                    &advanced,
                    (old_tree, new_tree),
                    &why,
                    gate,
                )));
            }
            advanced.push(holder);
        }
    }
    let held = !checkout.holders.is_empty();
    Ok(ObjectLanded {
        candidate_oid: candidate.to_string(),
        checkout_advanced: held,
        checkout_note: if held {
            None
        } else {
            Some(no_checkout_note(target))
        },
    })
}
