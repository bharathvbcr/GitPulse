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
    git_captured, git_supports, git_text, without_lazy_fetch, MERGE_TREE_WRITE_TREE,
};
use crate::engine::git_writer::{is_full_oid, GitWriter};
use crate::engine::object_quarantine::{
    parse_merge_tree_output, refuse_partial_clone, MergeOutcome, QuarantineError,
};
use crate::ipc_error::CommandError;
use std::path::Path;
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

/// What the caller knows about the main checkout, read under the repository lock.
pub(crate) struct CheckoutTouch {
    /// HEAD is the branch being integrated into.
    pub on_target: bool,
    /// The checkout has uncommitted user paths. Meaningful when `on_target`.
    pub dirty: bool,
    /// `.git/index.lock` is present. A checkout this call would detach or
    /// sync cannot proceed while it is.
    pub index_locked: bool,
    /// `on_target` and the dirtiness could not be read. Fail closed before
    /// moving the ref when this call would touch that checkout.
    pub dirtiness_error: Option<String>,
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

/// Moves the target ref, then the checkout when that is safe.
///
/// The checkout is not updated with a merge. The ref is already at the
/// candidate by then, and a merge would also succeed when the target had moved
/// to a commit that still contains the old tip, which the compare-and-swap
/// has already refused. A clean checkout that is on the target is synced with
/// `read-tree` instead. A dirty one is detached first, so moving the branch
/// does not rewrite its index, and it is left there with [`CHECKOUT_NOT_ADVANCED`].
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
    let touches = checkout.on_target && moving;
    if touches && checkout.index_locked {
        return Err(refused_text(format!(
            "The main checkout's index.lock is held, so {target} was not updated and nothing was merged."
        )));
    }
    if touches {
        if let Some(why) = &checkout.dirtiness_error {
            return Err(refused_text(format!(
                "The main checkout's uncommitted files could not be read ({why}), so nothing was merged."
            )));
        }
    }

    let mut detached = false;
    if touches && checkout.dirty {
        let detach = ["checkout", "--detach", "HEAD"];
        gate(&detach).map_err(refused)?;
        if let Err(error) = git_text(repo, &detach) {
            return Err(refused_text(format!(
                "Could not detach the main checkout before moving {target}, so nothing was merged: {}",
                error.message()
            )));
        }
        detached = true;
    }

    let refname = format!("refs/heads/{target}");
    let update = [
        "update-ref".to_string(),
        refname,
        candidate.to_string(),
        target_sha.to_string(),
    ];
    let update_args: Vec<&str> = update.iter().map(String::as_str).collect();
    if let Err(error) = gate(&update_args).and_then(|()| git_text(repo, &update_args)) {
        let mut message = format!(
            "{target} changed while {branch} was being integrated, so the update was refused and nothing was written to refs/heads/{target}: {}",
            error.message()
        );
        if detached {
            match reattach(repo, target, gate) {
                Ok(()) => {}
                Err(why) => message.push_str(&format!(
                    " The checkout was left detached: {}.",
                    why.message()
                )),
            }
        }
        return Err(refused_text(message));
    }

    if !moving {
        let advanced = checkout.on_target && !checkout.dirty && checkout.dirtiness_error.is_none();
        return Ok(ObjectLanded {
            candidate_oid: candidate.to_string(),
            checkout_advanced: advanced,
            checkout_note: if advanced {
                None
            } else {
                Some(idle_note(target, checkout))
            },
        });
    }

    if touches && !checkout.dirty {
        let old_tree = tree_of(repo, target_sha)?;
        let new_tree = tree_of(repo, candidate)?;
        let read = [
            "read-tree".to_string(),
            "-m".to_string(),
            "-u".to_string(),
            old_tree,
            new_tree,
        ];
        let read_args: Vec<&str> = read.iter().map(String::as_str).collect();
        let synced = gate(&read_args).and_then(|()| git_text(repo, &read_args));
        return Ok(match synced {
            Ok(_) => ObjectLanded {
                candidate_oid: candidate.to_string(),
                checkout_note: None,
                checkout_advanced: true,
            },
            Err(error) => ObjectLanded {
                candidate_oid: candidate.to_string(),
                checkout_advanced: false,
                checkout_note: Some(format!(
                    "{CHECKOUT_NOT_ADVANCED}: {target} moved to {candidate}, but the checkout could not be updated ({}).",
                    error.message()
                )),
            },
        });
    }

    let note = if detached {
        format!(
            "{CHECKOUT_NOT_ADVANCED}: the main checkout was left detached at the previous commit so uncommitted files were kept."
        )
    } else {
        format!(
            "{CHECKOUT_NOT_ADVANCED}: the main checkout is not on {target}, so its files were left as they were."
        )
    };
    Ok(ObjectLanded {
        candidate_oid: candidate.to_string(),
        checkout_advanced: false,
        checkout_note: Some(note),
    })
}

fn idle_note(target: &str, checkout: &CheckoutTouch) -> String {
    if checkout.dirty {
        format!(
            "{CHECKOUT_NOT_ADVANCED}: the main checkout has uncommitted files and was left as it was."
        )
    } else {
        format!(
            "{CHECKOUT_NOT_ADVANCED}: the main checkout is not on {target}, so its files were left as they were."
        )
    }
}

fn reattach(
    repo: &Path,
    target: &str,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<(), CommandError> {
    let args = ["checkout", target];
    gate(&args)?;
    git_text(repo, &args).map(|_| ())
}
