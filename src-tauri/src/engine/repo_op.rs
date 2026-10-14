//! The canonical owner of "what multi-step operation is this repository in the
//! middle of, and how does the user get out of it".
//!
//! Git's porcelain leaves a repository parked mid-merge, mid-rebase,
//! mid-cherry-pick, mid-revert, mid-`am` or mid-bisect whenever a step needs a
//! human. Every one of those states is recorded as a control file inside the
//! *per-worktree* git directory, and each has a different escape hatch
//! (`--abort` vs `--quit` vs `bisect reset`, `--continue` vs a plain commit).
//!
//! Before this module the codebase carried exactly one partial answer —
//! `GitWriter::rebase_or_merge_in_progress`, which probed `rebase-merge`,
//! `rebase-apply` and `MERGE_HEAD` — so a conflicted `git cherry-pick` or
//! `git revert` (which set `CHERRY_PICK_HEAD` / `REVERT_HEAD` and *nothing*
//! else) read as "no operation in progress". That predicate now delegates
//! here, so the detection lives in one place and every caller sees the whole
//! set.
//!
//! Two rules shape the implementation:
//!
//! * **Per-worktree, not per-repo.** `MERGE_HEAD` and friends live in the
//!   linked worktree's own git dir, never the common dir shared with the main
//!   checkout. The probes are joined onto the worktree's private git dir
//!   (what `git rev-parse --git-path` answers for them, checked against git in
//!   the tests). Joining the common dir instead would report the main
//!   checkout's merge inside every worktree.
//! * **Absence of evidence is never reported as evidence.** A control file
//!   that cannot be read is recorded as a warning on the detected operation,
//!   not silently treated as missing. A detection that could not run must not
//!   be indistinguishable from one that ran and found nothing.

use crate::engine::git_cli::{git_text, validate_repo};
use crate::engine::git_reader::symbolic_ref_target;
use crate::engine::git_writer::repo_mutation_lock;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Cap on any single git control file this module reads.
///
/// The sequencer todo list of a 10 000-commit rebase is the realistic maximum
/// and lands well under this; the bound exists so a corrupt or hostile
/// `.git` entry cannot pull an unbounded allocation into the UI thread pool.
const MAX_CONTROL_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Cap on conflicted paths carried in one report. A conflicted merge of two
/// large divergent trees can list tens of thousands of files; the banner only
/// ever renders a count and a handful of names, so the vector is bounded and
/// the true total is reported separately.
const MAX_LISTED_CONFLICTS: usize = 1000;

/// Which multi-step git operation is parked, waiting on the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationKind {
    Merge,
    /// `git merge --squash` that has staged its changes and not yet committed
    /// them: git records no `MERGE_HEAD`, only `SQUASH_MSG`. Its only escape
    /// is `git reset --merge`, because `merge --abort` needs a `MERGE_HEAD`.
    SquashMerge,
    /// `git rebase` on the merge backend, including every `rebase -i`.
    Rebase,
    /// `git rebase` on the apply backend (`--apply` / older git defaults).
    RebaseApply,
    /// `git am` — shares `rebase-apply/` with the apply-backend rebase and is
    /// told apart by the `applying` marker file.
    ApplyMailbox,
    CherryPick,
    Revert,
    Bisect,
}

impl OperationKind {
    /// The word the UI puts in "… in progress".
    pub fn label(self) -> &'static str {
        match self {
            OperationKind::Merge => "merge",
            OperationKind::SquashMerge => "squash merge",
            OperationKind::Rebase | OperationKind::RebaseApply => "rebase",
            OperationKind::ApplyMailbox => "patch application",
            OperationKind::CherryPick => "cherry-pick",
            OperationKind::Revert => "revert",
            OperationKind::Bisect => "bisect",
        }
    }

    /// The `git` subcommand that owns this operation's recovery verbs.
    fn recovery_subcommand(self) -> &'static str {
        match self {
            OperationKind::Merge | OperationKind::SquashMerge => "merge",
            OperationKind::Rebase | OperationKind::RebaseApply => "rebase",
            OperationKind::ApplyMailbox => "am",
            OperationKind::CherryPick => "cherry-pick",
            OperationKind::Revert => "revert",
            OperationKind::Bisect => "bisect",
        }
    }
}

/// What the user may do to leave the operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationAction {
    /// Undo the whole operation and restore the pre-operation state.
    Abort,
    /// Record the current resolution and move to the next step.
    Continue,
    /// Drop the current step and move to the next one.
    Skip,
}

impl OperationAction {
    pub fn label(self) -> &'static str {
        match self {
            OperationAction::Abort => "abort",
            OperationAction::Continue => "continue",
            OperationAction::Skip => "skip",
        }
    }
}

/// A parked operation and everything the UI needs to describe and escape it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoOperation {
    pub kind: OperationKind,
    /// Step number the operation is parked on, 1-based. `None` when the
    /// operation is inherently single-step (a merge) or the counters were
    /// unreadable.
    pub current_step: Option<usize>,
    /// Total steps in this operation, when it is a sequence.
    pub total_steps: Option<usize>,
    /// The branch the operation is being applied *to* — the rebased branch's
    /// own name, or the checked-out branch for a merge. `None` on detached HEAD,
    /// or when reading it failed (`warnings` says so).
    pub head_ref: Option<String>,
    /// What is coming in: the merged branch, the commit being replayed, or the
    /// new base of a rebase. Short and human-facing, never used as argv.
    pub incoming_ref: Option<String>,
    /// Conflicted paths, capped at [`MAX_LISTED_CONFLICTS`].
    pub conflicted_paths: Vec<String>,
    /// True conflicted-path count, which may exceed `conflicted_paths.len()`.
    pub conflicted_total: usize,
    /// Actions git will accept for this operation right now.
    pub available: Vec<OperationAction>,
    /// Probes that failed while assembling this report. A degraded field is
    /// never allowed to read as an honest empty one.
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl RepoOperation {
    /// True when at least one file still carries conflict markers in the index.
    pub fn has_conflicts(&self) -> bool {
        self.conflicted_total > 0
    }

    pub fn allows(&self, action: OperationAction) -> bool {
        self.available.contains(&action)
    }
}

/// Control files probed, in the precedence order `git status` itself uses.
///
/// Order matters and is not cosmetic: an interactive rebase that stops on a
/// conflicted pick can leave `CHERRY_PICK_HEAD` beside `rebase-merge/` on some
/// git versions. Reporting that as a cherry-pick would offer the user
/// `git cherry-pick --abort`, which does **not** unwind the rebase and leaves
/// the repository parked in a state the UI then claims is clean. Rebase is
/// therefore probed first, exactly as git's own `wt_status_get_state` does.
const PROBE_ORDER: &[&str] = &[
    "rebase-merge",
    "rebase-apply",
    "MERGE_HEAD",
    "SQUASH_MSG",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_LOG",
];

// Named offsets into a `PROBE_ORDER`-parallel path vector. A bare numeric index
// reads a different control file the moment the list is reordered — silently,
// with no test failing. `probe_indices_match_probe_order` pins each constant
// to its string, so a reorder fails the build instead of mis-reading state.
const IDX_REBASE_MERGE: usize = 0;
const IDX_REBASE_APPLY: usize = 1;
const IDX_MERGE_HEAD: usize = 2;
const IDX_SQUASH_MSG: usize = 3;
const IDX_CHERRY_PICK_HEAD: usize = 4;
const IDX_REVERT_HEAD: usize = 5;

/// Reports the operation `repo`'s worktree is parked in, or `None` when it is
/// idle.
///
/// `repo` must already be a validated repository path.
pub fn detect(repo: &Path) -> Result<Option<RepoOperation>, String> {
    let paths = resolve_control_paths(repo)?;
    let mut warnings: Vec<String> = Vec::new();

    let Some((kind, marker)) = classify(repo, &paths, &mut warnings)? else {
        return Ok(None);
    };

    let (current_step, total_steps) = progress(kind, &paths, marker, &mut warnings);
    let head_ref = head_ref_for(repo, kind, &paths, &mut warnings);
    let incoming_ref = incoming_ref_for(repo, kind, &paths, &mut warnings);
    let (conflicted_paths, conflicted_total) = conflicted_files(repo, &mut warnings);
    let available = available_actions(kind, conflicted_total);

    Ok(Some(RepoOperation {
        kind,
        current_step,
        total_steps,
        head_ref,
        incoming_ref,
        conflicted_paths,
        conflicted_total,
        available,
        warnings,
    }))
}

/// Which operation `repo`'s worktree is parked in, and nothing else about it.
///
/// A look at the control files and no more: no process is started, except one
/// `git diff --cached` when `SQUASH_MSG` is present (a leftover message file
/// alone is not a parked squash), and none for any other kind. [`detect`] goes
/// on to name the branch, describe the incoming
/// commit and list the conflicts, which costs a child each; a caller that
/// reports only the kind (the insights snapshot, once per worktree) should not
/// pay for the rest. Same precedence, same classification.
pub fn detect_kind(repo: &Path) -> Result<Option<OperationKind>, String> {
    let paths = resolve_control_paths(repo)?;
    let mut warnings: Vec<String> = Vec::new();
    Ok(classify(repo, &paths, &mut warnings)?.map(|(kind, _)| kind))
}

/// Resolves every probe path without starting git.
///
/// This used to be one `git rev-parse --git-path` per probe set — a spawn on
/// every refresh of every tab, spent on arithmetic. `--git-path` only rewrites
/// a name onto the common directory when the name is on git's shared list
/// (`refs`, `logs`, `objects`, `config`, `worktrees`, ...; `common_list` in
/// git's `path.c`). Every name in [`PROBE_ORDER`] is per-worktree state, so
/// each resolves to the worktree's own git directory — `.git/` for the main
/// worktree, `.git/worktrees/<name>/` for a linked one, the repository itself
/// when bare. `control_paths_match_git_rev_parse` holds this against git in
/// all three layouts.
fn resolve_control_paths(repo: &Path) -> Result<Vec<PathBuf>, String> {
    let (private, _common) = crate::repository_trust::git_directories(repo)?;
    Ok(PROBE_ORDER
        .iter()
        .map(|probe| private.join(probe))
        .collect())
}

/// Picks the operation kind from which control paths exist.
///
/// Returns the kind together with the marker directory/file it was found
/// through, so progress and ref lookups do not re-derive it.
fn classify<'a>(
    repo: &Path,
    paths: &'a [PathBuf],
    warnings: &mut Vec<String>,
) -> Result<Option<(OperationKind, &'a Path)>, String> {
    for (index, probe) in PROBE_ORDER.iter().enumerate() {
        let path = paths[index].as_path();
        if !path.exists() {
            continue;
        }
        let kind = match *probe {
            "rebase-merge" => OperationKind::Rebase,
            // `rebase-apply/` is shared by the apply-backend rebase and by
            // `git am`. git tells them apart by which marker the directory
            // carries; guessing wrong offers `git rebase --abort` for a
            // parked `am`, which errors out and strands the user.
            "rebase-apply" => {
                if path.join("applying").exists() {
                    OperationKind::ApplyMailbox
                } else if path.join("rebasing").exists() {
                    OperationKind::RebaseApply
                } else {
                    // Neither marker: git's own fallback treats a bare
                    // rebase-apply as a rebase. Record that the classification
                    // was a fallback rather than a reading.
                    warnings.push(
                        "rebase-apply/ carries neither an 'applying' nor a 'rebasing' marker; \
                         assuming a rebase"
                            .into(),
                    );
                    OperationKind::RebaseApply
                }
            }
            "MERGE_HEAD" => OperationKind::Merge,
            // `SQUASH_MSG` outlives a squash that was committed with `-m` or
            // that staged nothing. It is a parked squash only while the index
            // still differs from HEAD (staged changes or unmerged entries).
            "SQUASH_MSG" => {
                if !squash_is_staged(repo, warnings) {
                    continue;
                }
                OperationKind::SquashMerge
            }
            "CHERRY_PICK_HEAD" => OperationKind::CherryPick,
            "REVERT_HEAD" => OperationKind::Revert,
            "BISECT_LOG" => OperationKind::Bisect,
            other => {
                return Err(format!("unhandled operation probe '{other}'"));
            }
        };
        return Ok(Some((kind, path)));
    }
    Ok(None)
}

/// The warning a squash probe leaves when it could not read the index, so a
/// caller can tell "a squash is parked" from "could not tell".
pub(crate) const SQUASH_READ_FAILURE: &str = "could not tell whether a squash is staged";

/// Whether the index differs from HEAD, which is what makes a `SQUASH_MSG` a
/// squash waiting to be committed. A read that could not be made counts as
/// staged, with a warning: an unreadable index must not read as idle.
fn squash_is_staged(repo: &Path, warnings: &mut Vec<String>) -> bool {
    match crate::engine::git_cli::git_captured(repo, &["diff", "--cached", "--quiet"]) {
        Ok(run) if run.success => false,
        Ok(run) if run.status_code == 1 => true,
        Ok(run) => {
            warnings.push(format!(
                "{SQUASH_READ_FAILURE} (git exit {}): {}",
                run.status_code,
                String::from_utf8_lossy(&run.stderr).trim()
            ));
            true
        }
        Err(err) => {
            warnings.push(format!("{SQUASH_READ_FAILURE}: {err}"));
            true
        }
    }
}

/// Reads a bounded control file, folding "absent" into `None` and a genuine
/// read failure into a warning so the two never look alike.
fn read_control(path: &Path, warnings: &mut Vec<String>) -> Option<String> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_CONTROL_FILE_BYTES => {
            warnings.push(format!(
                "{} is {} bytes, over the {} byte control-file cap; skipped",
                path.display(),
                meta.len(),
                MAX_CONTROL_FILE_BYTES
            ));
            None
        }
        Ok(_) => match std::fs::read(path) {
            // Control files are git-written ASCII; lossy decoding keeps a
            // corrupt byte from failing the whole report.
            Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
            Err(err) => {
                warnings.push(format!("cannot read {}: {err}", path.display()));
                None
            }
        },
        // Genuinely absent: expected for most probes, not a degradation.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            warnings.push(format!("cannot stat {}: {err}", path.display()));
            None
        }
    }
}

fn read_counter(path: &Path, warnings: &mut Vec<String>) -> Option<usize> {
    let raw = read_control(path, warnings)?;
    match raw.trim().parse::<usize>() {
        Ok(value) => Some(value),
        Err(_) => {
            warnings.push(format!("{} did not contain a step number", path.display()));
            None
        }
    }
}

/// Counts the executable entries of a sequencer todo/done list.
///
/// Comment and blank lines are not steps; counting them inflates "step 3 of
/// 40" into nonsense on a todo list carrying git's own commentary.
fn count_todo_steps(raw: &str) -> usize {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .count()
}

/// Derives `(current, total)` for the operation.
fn progress(
    kind: OperationKind,
    paths: &[PathBuf],
    marker: &Path,
    warnings: &mut Vec<String>,
) -> (Option<usize>, Option<usize>) {
    match kind {
        // The merge backend keeps a 1-based cursor and an end count.
        OperationKind::Rebase => {
            let current = read_counter(&marker.join("msgnum"), warnings);
            let total = read_counter(&marker.join("end"), warnings);
            (current, total)
        }
        // The apply backend (and `am`) use next/last, where `next` is the
        // patch about to be applied — already 1-based for the parked step.
        OperationKind::RebaseApply | OperationKind::ApplyMailbox => {
            let current = read_counter(&marker.join("next"), warnings);
            let total = read_counter(&marker.join("last"), warnings);
            (current, total)
        }
        // A multi-commit cherry-pick/revert records its plan in the sequencer.
        // `todo` still holds the step that is currently parked, so the 1-based
        // cursor is `done + 1` and the total is `done + todo`. A single-commit
        // pick writes no sequencer at all, which correctly yields no progress
        // rather than a fake "1 of 1".
        OperationKind::CherryPick | OperationKind::Revert => {
            let sequencer = sequencer_dir(paths);
            let todo =
                read_control(&sequencer.join("todo"), warnings).map(|s| count_todo_steps(&s));
            let Some(remaining) = todo else {
                return (None, None);
            };
            let done = read_control(&sequencer.join("done"), warnings)
                .map(|s| count_todo_steps(&s))
                .unwrap_or(0);
            let total = done + remaining;
            if total == 0 {
                return (None, None);
            }
            (Some(done + 1), Some(total))
        }
        OperationKind::Merge | OperationKind::SquashMerge | OperationKind::Bisect => (None, None),
    }
}

/// The sequencer lives beside the other control files in the same git dir.
/// Derived from a probe path rather than re-invoking `rev-parse`.
fn sequencer_dir(paths: &[PathBuf]) -> PathBuf {
    let cherry_pick_head = &paths[IDX_CHERRY_PICK_HEAD];
    cherry_pick_head
        .parent()
        .map(|dir| dir.join("sequencer"))
        .unwrap_or_else(|| PathBuf::from("sequencer"))
}

/// Strips a full ref name down to what a person would call it.
fn short_ref(raw: &str) -> String {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("refs/heads/")
        .or_else(|| trimmed.strip_prefix("refs/remotes/"))
        .or_else(|| trimmed.strip_prefix("refs/tags/"))
        .unwrap_or(trimmed)
        .to_string()
}

/// The branch the operation is being applied to.
fn head_ref_for(
    repo: &Path,
    kind: OperationKind,
    paths: &[PathBuf],
    warnings: &mut Vec<String>,
) -> Option<String> {
    // A rebase detaches HEAD, so the checked-out "branch" during one is the
    // rebase's own `head-name` record, not what `symbolic-ref` reports.
    let marker_head_name = match kind {
        OperationKind::Rebase => Some(paths[IDX_REBASE_MERGE].join("head-name")),
        OperationKind::RebaseApply | OperationKind::ApplyMailbox => {
            Some(paths[IDX_REBASE_APPLY].join("head-name"))
        }
        _ => None,
    };
    if let Some(path) = marker_head_name {
        if let Some(raw) = read_control(&path, warnings) {
            let name = short_ref(&raw);
            if !name.is_empty() && name != "detached HEAD" {
                return Some(name);
            }
        }
    }
    // Not a rebase (or head-name unreadable): ask for the live branch.
    // A detached HEAD is a legitimate answer of "no branch" (`Ok(None)`); a
    // read that failed is a degraded field and is reported in `warnings`, so
    // the label never presents an unreadable HEAD as a detached one.
    match symbolic_ref_target(repo, "HEAD") {
        Ok(target) => target
            .map(|raw| short_ref(&raw))
            .filter(|name| !name.is_empty()),
        Err(err) => {
            warnings.push(format!("cannot read the checked-out branch: {err}"));
            None
        }
    }
}

/// What is being brought in by the parked step.
fn incoming_ref_for(
    repo: &Path,
    kind: OperationKind,
    paths: &[PathBuf],
    warnings: &mut Vec<String>,
) -> Option<String> {
    match kind {
        // MERGE_MSG holds git's own "Merge branch 'x' into y" sentence; its
        // first line names the merged branch far better than a raw OID.
        OperationKind::Merge => {
            let merge_msg = paths[IDX_MERGE_HEAD]
                .parent()
                .map(|dir| dir.join("MERGE_MSG"))?;
            if let Some(raw) = read_control(&merge_msg, warnings) {
                if let Some(first) = raw.lines().next() {
                    let first = first.trim();
                    if !first.is_empty() {
                        return Some(first.to_string());
                    }
                }
            }
            read_control(&paths[IDX_MERGE_HEAD], warnings)
                .and_then(|raw| describe_oid(repo, raw.trim()))
        }
        // A rebase's "incoming" is the base it is replaying onto.
        OperationKind::Rebase => read_control(&paths[IDX_REBASE_MERGE].join("onto"), warnings)
            .and_then(|raw| describe_oid(repo, raw.trim())),
        OperationKind::RebaseApply | OperationKind::ApplyMailbox => {
            read_control(&paths[IDX_REBASE_APPLY].join("onto"), warnings)
                .and_then(|raw| describe_oid(repo, raw.trim()))
        }
        OperationKind::CherryPick => read_control(&paths[IDX_CHERRY_PICK_HEAD], warnings)
            .and_then(|raw| describe_oid(repo, raw.trim())),
        OperationKind::Revert => read_control(&paths[IDX_REVERT_HEAD], warnings)
            .and_then(|raw| describe_oid(repo, raw.trim())),
        // SQUASH_MSG lists the squashed commits, newest first; the first
        // names what was brought in.
        OperationKind::SquashMerge => {
            read_control(&paths[IDX_SQUASH_MSG], warnings).and_then(|raw| {
                raw.lines()
                    .find_map(|line| line.strip_prefix("commit "))
                    .and_then(|oid| describe_oid(repo, oid))
            })
        }
        OperationKind::Bisect => None,
    }
}

/// Renders an OID as `abc1234 subject`, falling back to the short OID.
///
/// The OID comes from a git-written control file and is passed to git as a
/// revision, so it is shape-checked before it can reach argv.
fn describe_oid(repo: &Path, oid: &str) -> Option<String> {
    let oid = oid.trim();
    if oid.is_empty() || oid.len() > 64 || !oid.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    git_text(repo, &["log", "-1", "--format=%h %s", oid])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| Some(oid.chars().take(7).collect()))
}

/// Unmerged index entries, capped at [`MAX_LISTED_CONFLICTS`], with the true
/// total beside them. A listing that could not be read is an error, never an
/// empty list. A rename-only conflict has stage entries and no markers, so
/// the index is read rather than the working tree.
pub(crate) fn conflicted_paths(repo: &Path) -> Result<(Vec<String>, usize), String> {
    let raw = git_text(repo, &["diff", "--name-only", "--diff-filter=U", "-z"])?;
    let all: Vec<&str> = raw.split('\0').filter(|s| !s.is_empty()).collect();
    let total = all.len();
    let listed = all
        .into_iter()
        .take(MAX_LISTED_CONFLICTS)
        .map(str::to_string)
        .collect();
    Ok((listed, total))
}

/// [`conflicted_paths`], with a failed listing recorded as a warning.
fn conflicted_files(repo: &Path, warnings: &mut Vec<String>) -> (Vec<String>, usize) {
    match conflicted_paths(repo) {
        Ok(found) => found,
        Err(err) => {
            // An unreadable conflict list must not render as "no conflicts",
            // which would offer `continue` on an unresolved tree.
            warnings.push(format!("cannot list conflicted files: {err}"));
            (Vec::new(), 0)
        }
    }
}

/// Which verbs git will accept for this kind right now.
fn available_actions(kind: OperationKind, conflicted_total: usize) -> Vec<OperationAction> {
    match kind {
        // `git merge --continue` refuses while the index has conflicts, and
        // there is no such thing as skipping a merge's single step.
        OperationKind::Merge | OperationKind::SquashMerge => {
            let mut actions = vec![OperationAction::Abort];
            if conflicted_total == 0 {
                actions.push(OperationAction::Continue);
            }
            actions
        }
        OperationKind::Rebase
        | OperationKind::RebaseApply
        | OperationKind::ApplyMailbox
        | OperationKind::CherryPick
        | OperationKind::Revert => {
            let mut actions = vec![OperationAction::Abort, OperationAction::Skip];
            if conflicted_total == 0 {
                actions.push(OperationAction::Continue);
            }
            actions
        }
        // `git bisect reset` is the only escape; there is nothing to continue
        // or skip without a good/bad verdict, which this surface does not take.
        OperationKind::Bisect => vec![OperationAction::Abort],
    }
}

/// The argv an action would execute, for the write gate to judge.
///
/// Rendering and execution share this function so the line the harness judges
/// is exactly the line that runs — the same plan-vs-execute discipline the
/// restack path already follows.
pub fn action_argv(kind: OperationKind, action: OperationAction) -> Vec<&'static str> {
    // Bisect's escape is `reset`, not `--abort`; every other kind takes the
    // long-flag form.
    if kind == OperationKind::Bisect {
        return vec!["git", "bisect", "reset"];
    }
    // A squash merge records no `MERGE_HEAD`, so `merge --abort` refuses it;
    // `reset --merge` is what restores the pre-merge tree. Concluding it is a
    // plain commit, which git takes the message of from `SQUASH_MSG`. It has
    // no step to skip, so `available_actions` never offers `Skip` and
    // `run_action_with` refuses it before this is rendered.
    if kind == OperationKind::SquashMerge {
        return match action {
            OperationAction::Continue => vec!["git", "commit", "--no-edit"],
            OperationAction::Abort | OperationAction::Skip => vec!["git", "reset", "--merge"],
        };
    }
    let sub = kind.recovery_subcommand();
    match action {
        OperationAction::Abort => vec!["git", sub, "--abort"],
        OperationAction::Skip => vec!["git", sub, "--skip"],
        OperationAction::Continue => match kind {
            // `git merge --continue` exists but opens an editor for the merge
            // message; `--no-edit` is not accepted alongside it. Concluding the
            // merge with an explicit no-edit commit is the same operation with
            // a bounded, non-interactive shape.
            OperationKind::Merge => vec!["git", "commit", "--no-edit"],
            // cherry-pick/revert take --no-edit; rebase and am do not accept
            // it on --continue and rely on the neutralized editor instead.
            OperationKind::CherryPick | OperationKind::Revert => {
                vec!["git", sub, "--continue", "--no-edit"]
            }
            _ => vec!["git", sub, "--continue"],
        },
    }
}

/// Runs a recovery verb against `repo_path`, judging it through `judge` first.
///
/// Takes the repository mutation lock for the same reason every other writer
/// does: a concurrent stage or commit landing between the detection and the
/// verb would make the verb act on an index the user never saw.
///
/// `judge` receives the exact argv that is about to run and is invoked **under
/// the lock**, closing the plan-vs-execute gap. Detecting the kind, judging it,
/// and only then taking the lock would let a concurrent abort change the
/// operation in between — so the gate would approve `git rebase --abort` while
/// `git cherry-pick --abort` is what actually ran. This mirrors the discipline
/// `prepare_restack` already documents.
///
/// `judge` is generic so this module stays free of any harness types; the
/// command layer passes the real write gate, tests pass a recorder.
pub fn run_action_with<J, V>(
    repo_path: &str,
    action: OperationAction,
    judge: J,
) -> Result<(V, String), String>
where
    J: FnOnce(&[&str]) -> Result<V, String>,
{
    let repo = validate_repo(repo_path)?;
    let lock = repo_mutation_lock(&repo);
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // Re-detect under the lock. Detecting outside it and acting inside would
    // let a concurrent abort land first, turning this call into
    // `git rebase --abort` against a repository with no rebase — which errors
    // in a way the user cannot act on.
    let Some(operation) = detect(&repo)? else {
        return Err(
            "No merge, rebase, cherry-pick, revert or bisect is in progress in this repository."
                .into(),
        );
    };
    if !operation.allows(action) {
        return Err(format!(
            "Cannot {} this {}{}.",
            action.label(),
            operation.kind.label(),
            if action == OperationAction::Continue && operation.has_conflicts() {
                format!(
                    " while {} file{} still ha{} conflict markers",
                    operation.conflicted_total,
                    if operation.conflicted_total == 1 {
                        ""
                    } else {
                        "s"
                    },
                    if operation.conflicted_total == 1 {
                        "s"
                    } else {
                        "ve"
                    },
                )
            } else {
                String::new()
            }
        ));
    }

    let argv = action_argv(operation.kind, action);
    // Judged inside the lock, from the same rendering that runs below. A
    // refusal returns before anything is executed.
    let verdict = judge(&argv)?;
    if action == OperationAction::Continue {
        // An external editor can `git add` a file that still contains markers:
        // stage zero alone does not establish a finished resolution. Ask Git
        // to check the staged changes, including its configured marker width.
        // Disable ordinary whitespace checks: resolution integrity must not
        // introduce a formatting policy into an existing repository.
        git_text(&repo, &[
            "-c", "core.whitespace=-blank-at-eol,-blank-at-eof,-space-before-tab,-indent-with-non-tab,-tab-in-indent,cr-at-eol",
            "diff", "--cached", "--check", "--no-ext-diff", "--no-color",
        ]).map_err(|error| format!("Cannot continue: the staged integrity check failed. Review the staged changes. {error}"))?;
    }
    // argv[0] is the program name the gate judges; git itself takes the rest.
    let args: Vec<&str> = argv[1..].to_vec();
    // `rebase --continue` and `am --continue` resolve a commit message through
    // the editor and accept no `--no-edit`. They are only non-blocking because
    // `git_command` pins GIT_EDITOR to `true`; without that this call would
    // hang until the command timeout. See git_cli::git_command.
    let output = git_text(&repo, &args)?;
    Ok((verdict, output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procguard::LockedSpawn;

    #[test]
    fn probe_indices_match_probe_order() {
        // The path vector is parallel to PROBE_ORDER, and every lookup below
        // addresses it by these constants. If the list is reordered without
        // updating them, detection reads the wrong control file for refs and
        // progress — a silent wrong answer. This makes that a test failure.
        assert_eq!(PROBE_ORDER[IDX_REBASE_MERGE], "rebase-merge");
        assert_eq!(PROBE_ORDER[IDX_REBASE_APPLY], "rebase-apply");
        assert_eq!(PROBE_ORDER[IDX_MERGE_HEAD], "MERGE_HEAD");
        assert_eq!(PROBE_ORDER[IDX_SQUASH_MSG], "SQUASH_MSG");
        assert_eq!(PROBE_ORDER[IDX_CHERRY_PICK_HEAD], "CHERRY_PICK_HEAD");
        assert_eq!(PROBE_ORDER[IDX_REVERT_HEAD], "REVERT_HEAD");
    }

    #[test]
    fn a_bare_squash_merge_is_a_parked_operation() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let git = |args: &[&str]| crate::test_support::git_in(repo, args);
        git(&["init", "-q", "-b", "main", "."]);
        git(&["config", "user.email", "t@gitpulse.local"]);
        git(&["config", "user.name", "Tester"]);
        std::fs::write(repo.join("a.txt"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        git(&["switch", "-qc", "side"]);
        std::fs::write(repo.join("b.txt"), "side\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "side"]);
        git(&["switch", "-q", "main"]);
        git(&["merge", "--squash", "side"]);
        crate::test_support::trust_repo(repo);
        let parked = detect(repo)
            .unwrap()
            .expect("a staged squash read as an idle repository");
        assert_eq!(parked.kind, OperationKind::SquashMerge);
        assert_eq!(parked.kind.label(), "squash merge");
        assert_eq!(
            parked.available,
            [OperationAction::Abort, OperationAction::Continue]
        );
        assert_eq!(
            action_argv(OperationKind::SquashMerge, OperationAction::Abort),
            ["git", "reset", "--merge"]
        );

        // Continuing is a bare commit, and git writes the squash's own message.
        let continued = action_argv(OperationKind::SquashMerge, OperationAction::Continue);
        assert_eq!(continued, ["git", "commit", "--no-edit"]);
        git(&continued[1..]);
        assert_eq!(detect(repo).unwrap(), None);
        assert!(
            git_text(repo, &["log", "-1", "--format=%B"])
                .unwrap()
                .contains("Squashed commit"),
            "the commit did not take SQUASH_MSG"
        );
    }

    /// `SQUASH_MSG` outlives a squash committed with `-m`: with the index equal
    /// to HEAD that is a leftover file, not a parked squash.
    #[test]
    fn a_leftover_squash_message_with_a_clean_index_is_idle() {
        let repo = crate::test_support::git_repo();
        let root = repo.path().canonicalize().unwrap();
        crate::test_support::git_in(&root, &["commit", "--allow-empty", "-q", "-m", "base"]);
        std::fs::write(
            root.join(".git/SQUASH_MSG"),
            "Squashed commit of the following:\n",
        )
        .unwrap();
        assert_eq!(detect_kind(&root).unwrap(), None);
        assert!(detect(&root).unwrap().is_none());
    }

    #[test]
    fn a_parked_squash_is_undone_by_reset_merge() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let git = |args: &[&str]| crate::test_support::git_in(repo, args);
        git(&["init", "-q", "-b", "main", "."]);
        git(&["config", "user.email", "t@gitpulse.local"]);
        git(&["config", "user.name", "Tester"]);
        std::fs::write(repo.join("a.txt"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        git(&["switch", "-qc", "side"]);
        std::fs::write(repo.join("b.txt"), "side\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "side"]);
        git(&["switch", "-q", "main"]);
        git(&["merge", "--squash", "side"]);
        crate::test_support::trust_repo(repo);
        let abort = action_argv(OperationKind::SquashMerge, OperationAction::Abort);
        git(&abort[1..]);
        assert_eq!(detect(repo).unwrap(), None);
        assert!(git_text(repo, &["status", "--porcelain"])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn probe_order_puts_rebase_ahead_of_the_sequencer_heads() {
        // An interactive rebase parked on a conflicted pick can leave
        // CHERRY_PICK_HEAD beside rebase-merge/. Offering `cherry-pick
        // --abort` there does not unwind the rebase.
        let rebase = PROBE_ORDER
            .iter()
            .position(|p| *p == "rebase-merge")
            .unwrap();
        let cherry = PROBE_ORDER
            .iter()
            .position(|p| *p == "CHERRY_PICK_HEAD")
            .unwrap();
        let revert = PROBE_ORDER
            .iter()
            .position(|p| *p == "REVERT_HEAD")
            .unwrap();
        assert!(rebase < cherry);
        assert!(rebase < revert);
    }

    #[test]
    fn every_probe_classifies() {
        // classify() errors on an unhandled probe rather than silently
        // reporting "idle"; this pins that PROBE_ORDER and the match stay in
        // lockstep when a kind is added.
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = PROBE_ORDER
            .iter()
            .map(|probe| dir.path().join(probe))
            .collect();
        for (index, probe) in PROBE_ORDER.iter().enumerate() {
            let path = &paths[index];
            if *probe == "rebase-merge" || *probe == "rebase-apply" {
                std::fs::create_dir_all(path).unwrap();
            } else {
                std::fs::write(path, "deadbeef\n").unwrap();
            }
            let mut warnings = Vec::new();
            let found = classify(dir.path(), &paths, &mut warnings).unwrap();
            assert!(found.is_some(), "{probe} did not classify");
            std::fs::remove_dir_all(path)
                .or_else(|_| std::fs::remove_file(path))
                .unwrap();
        }
    }

    #[test]
    fn rebase_apply_is_told_apart_from_am_by_its_marker() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = PROBE_ORDER
            .iter()
            .map(|probe| dir.path().join(probe))
            .collect();
        let apply = &paths[IDX_REBASE_APPLY];
        std::fs::create_dir_all(apply).unwrap();

        std::fs::write(apply.join("applying"), "").unwrap();
        let mut warnings = Vec::new();
        assert_eq!(
            classify(dir.path(), &paths, &mut warnings)
                .unwrap()
                .unwrap()
                .0,
            OperationKind::ApplyMailbox
        );
        assert!(warnings.is_empty());

        std::fs::remove_file(apply.join("applying")).unwrap();
        std::fs::write(apply.join("rebasing"), "").unwrap();
        let mut warnings = Vec::new();
        assert_eq!(
            classify(dir.path(), &paths, &mut warnings)
                .unwrap()
                .unwrap()
                .0,
            OperationKind::RebaseApply
        );
        assert!(warnings.is_empty());

        // Neither marker: falls back to rebase AND says so.
        std::fs::remove_file(apply.join("rebasing")).unwrap();
        let mut warnings = Vec::new();
        assert_eq!(
            classify(dir.path(), &paths, &mut warnings)
                .unwrap()
                .unwrap()
                .0,
            OperationKind::RebaseApply
        );
        assert_eq!(warnings.len(), 1, "fallback must be recorded, not silent");
    }

    #[test]
    fn todo_counting_ignores_comments_and_blanks() {
        let todo = "pick abc1 one\n\n# comment\npick abc2 two\n   \n#\npick abc3 three\n";
        assert_eq!(count_todo_steps(todo), 3);
        assert_eq!(count_todo_steps(""), 0);
        assert_eq!(count_todo_steps("# only a comment\n"), 0);
    }

    #[test]
    fn continue_is_withheld_while_conflicts_remain() {
        for kind in [
            OperationKind::Merge,
            OperationKind::Rebase,
            OperationKind::RebaseApply,
            OperationKind::ApplyMailbox,
            OperationKind::CherryPick,
            OperationKind::Revert,
        ] {
            let blocked = available_actions(kind, 3);
            assert!(
                !blocked.contains(&OperationAction::Continue),
                "{kind:?} offered continue with conflicts outstanding"
            );
            assert!(
                blocked.contains(&OperationAction::Abort),
                "{kind:?} must always abort"
            );
            let clear = available_actions(kind, 0);
            assert!(
                clear.contains(&OperationAction::Continue),
                "{kind:?} withheld continue on a clean index"
            );
        }
    }

    #[test]
    fn bisect_only_offers_abort_and_maps_to_reset() {
        assert_eq!(
            available_actions(OperationKind::Bisect, 0),
            vec![OperationAction::Abort]
        );
        assert_eq!(
            action_argv(OperationKind::Bisect, OperationAction::Abort),
            vec!["git", "bisect", "reset"]
        );
    }

    #[test]
    fn merge_never_offers_skip() {
        for conflicts in [0, 5] {
            assert!(!available_actions(OperationKind::Merge, conflicts)
                .contains(&OperationAction::Skip));
        }
    }

    #[test]
    fn action_argv_is_non_interactive_for_every_kind_and_verb() {
        // A verb that opens an editor hangs until the 90s command timeout and
        // then reports a timeout instead of a result. Every rendered line must
        // either carry --no-edit or be a form that takes no message at all.
        for kind in [
            OperationKind::Merge,
            OperationKind::Rebase,
            OperationKind::RebaseApply,
            OperationKind::ApplyMailbox,
            OperationKind::CherryPick,
            OperationKind::Revert,
            OperationKind::Bisect,
        ] {
            for action in [
                OperationAction::Abort,
                OperationAction::Continue,
                OperationAction::Skip,
            ] {
                let argv = action_argv(kind, action);
                assert_eq!(argv[0], "git", "{kind:?}/{action:?} must render a git line");
                assert!(argv.len() >= 3, "{kind:?}/{action:?} rendered {argv:?}");
                assert!(
                    !argv.iter().any(|a| a.is_empty()),
                    "{kind:?}/{action:?} rendered an empty argument"
                );
            }
        }
        // The merge conclusion is a commit, and it must be --no-edit.
        assert_eq!(
            action_argv(OperationKind::Merge, OperationAction::Continue),
            vec!["git", "commit", "--no-edit"]
        );
        assert_eq!(
            action_argv(OperationKind::CherryPick, OperationAction::Continue),
            vec!["git", "cherry-pick", "--continue", "--no-edit"]
        );
    }

    #[test]
    fn short_ref_strips_only_known_namespaces() {
        assert_eq!(short_ref("refs/heads/main\n"), "main");
        assert_eq!(short_ref("refs/remotes/origin/main"), "origin/main");
        assert_eq!(short_ref("refs/tags/v1.0"), "v1.0");
        // A branch literally named like a path keeps its shape.
        assert_eq!(short_ref("feature/refs/heads/x"), "feature/refs/heads/x");
        assert_eq!(short_ref("  detached  "), "detached");
    }

    #[test]
    fn control_reads_separate_absent_from_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let mut warnings = Vec::new();
        // Absent is not a degradation.
        assert_eq!(read_control(&dir.path().join("nope"), &mut warnings), None);
        assert!(warnings.is_empty());

        // Over-cap is a degradation, and is reported rather than read.
        let big = dir.path().join("big");
        std::fs::write(&big, vec![b'x'; (MAX_CONTROL_FILE_BYTES + 1) as usize]).unwrap();
        assert_eq!(read_control(&big, &mut warnings), None);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn counters_reject_non_numeric_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("msgnum");
        std::fs::write(&path, "not-a-number\n").unwrap();
        let mut warnings = Vec::new();
        assert_eq!(read_counter(&path, &mut warnings), None);
        assert_eq!(warnings.len(), 1);

        std::fs::write(&path, "  7\n").unwrap();
        let mut warnings = Vec::new();
        assert_eq!(read_counter(&path, &mut warnings), Some(7));
        assert!(warnings.is_empty());
    }

    #[test]
    fn describe_oid_refuses_anything_that_is_not_an_oid() {
        let dir = tempfile::tempdir().unwrap();
        // Never reaches git: shape check fails first, so no argv injection is
        // possible through a corrupt control file.
        assert_eq!(
            describe_oid(dir.path(), "--upload-pack=touch /tmp/pwn"),
            None
        );
        assert_eq!(describe_oid(dir.path(), ""), None);
        assert_eq!(describe_oid(dir.path(), "refs/heads/main"), None);
        assert_eq!(describe_oid(dir.path(), &"a".repeat(65)), None);
    }

    /// The probe paths are computed, no longer asked of `git rev-parse
    /// --git-path`; git stays the oracle in every layout that resolves them
    /// differently.
    #[test]
    fn control_paths_match_git_rev_parse() {
        use crate::test_support::git_in;
        let main = crate::test_support::git_repo();
        // Plain, not `canonicalize`: `linked` is handed to git as an argument,
        // and git cannot create a worktree at a `\\?\` path.
        let plain =
            |path: &std::path::Path| crate::engine::git_cli::canonicalize_plain(path).unwrap();
        let root = plain(main.path());
        git_in(&root, &["commit", "--allow-empty", "-q", "-m", "base"]);
        let holder = tempfile::tempdir().unwrap();
        let linked = plain(holder.path()).join("linked");
        git_in(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "side",
                linked.to_str().unwrap(),
            ],
        );
        let bare_dir = tempfile::tempdir().unwrap();
        let bare = plain(bare_dir.path());
        git_in(&bare, &["init", "--bare", "-q"]);

        // Probe files mostly do not exist, so compare through their parent.
        let normal = |path: PathBuf| -> PathBuf {
            let parent = path.parent().unwrap().canonicalize().unwrap();
            parent.join(path.file_name().unwrap())
        };
        for repo in [&root, &linked, &bare] {
            let ours: Vec<PathBuf> = resolve_control_paths(repo)
                .expect("computed paths")
                .into_iter()
                .map(normal)
                .collect();
            let mut args = vec!["rev-parse"];
            for probe in PROBE_ORDER {
                args.push("--git-path");
                args.push(probe);
            }
            let out = std::process::Command::new("git")
                .args(&args)
                .current_dir(repo)
                .output_locked()
                .expect("git rev-parse");
            assert!(out.status.success());
            let theirs: Vec<PathBuf> = String::from_utf8(out.stdout)
                .unwrap()
                .lines()
                .map(|line| {
                    let path = Path::new(line);
                    normal(if path.is_absolute() {
                        path.to_path_buf()
                    } else {
                        repo.join(path)
                    })
                })
                .collect();
            assert_eq!(ours, theirs, "layout at {}", repo.display());
        }
        // A linked worktree's state is its own, not the main worktree's.
        assert_ne!(
            resolve_control_paths(&linked).unwrap(),
            resolve_control_paths(&root).unwrap()
        );
    }

    /// Detection spawns nothing on an idle repository any more.
    #[test]
    fn detecting_an_idle_repository_starts_no_process() {
        let repo = crate::test_support::git_repo();
        let root = repo.path().canonicalize().unwrap();
        assert!(detect(&root).unwrap().is_none());
        // The log is keyed by working directory and this repository is fresh,
        // so nothing a concurrent test spawns can land here.
        let spawned = crate::engine::git_cli::spawn_log::spawns_in(&root);
        assert!(spawned.is_empty(), "spawned: {spawned:?}");
    }

    /// The kind alone is the kind the full report names, for every probe, and
    /// is read without starting a process even when an operation is parked.
    #[test]
    fn detect_kind_names_what_detect_names_without_a_process() {
        let repo = crate::test_support::git_repo();
        let root = repo.path().canonicalize().unwrap();
        assert_eq!(detect_kind(&root).unwrap(), None);
        let git_dir = root.join(".git");
        let head = "deadbeef\n";
        for probe in PROBE_ORDER {
            let path = git_dir.join(probe);
            if *probe == "rebase-merge" || *probe == "rebase-apply" {
                std::fs::create_dir_all(&path).unwrap();
            } else {
                std::fs::write(&path, head).unwrap();
            }
            // A leftover SQUASH_MSG is a parked squash only while something is
            // staged, which is the one answer that costs a process.
            let allowed = if *probe == "SQUASH_MSG" {
                std::fs::write(root.join("staged.txt"), "x\n").unwrap();
                crate::test_support::git_in(&root, &["add", "staged.txt"]);
                1
            } else {
                0
            };
            let before = crate::engine::git_cli::spawn_log::spawns_in(&root).len();
            let kind = detect_kind(&root).unwrap();
            assert_eq!(
                crate::engine::git_cli::spawn_log::spawns_in(&root).len(),
                before + allowed,
                "{probe}: reading the kind started the wrong number of processes"
            );
            let full = detect(&root).unwrap().map(|op| op.kind);
            assert!(kind.is_some(), "{probe} was not classified");
            assert_eq!(kind, full, "{probe}");
            if *probe == "SQUASH_MSG" {
                crate::test_support::git_in(&root, &["reset", "-q", "--", "staged.txt"]);
                std::fs::remove_file(root.join("staged.txt")).unwrap();
            }
            std::fs::remove_dir_all(&path)
                .or_else(|_| std::fs::remove_file(&path))
                .unwrap();
        }
        assert_eq!(detect_kind(&root).unwrap(), None);
    }

    fn head_ref_of(repo: &Path) -> (Option<String>, Vec<String>) {
        // A merge reads no control file, so no path is consulted.
        let paths: Vec<PathBuf> = Vec::new();
        let mut warnings = Vec::new();
        let head = head_ref_for(repo, OperationKind::Merge, &paths, &mut warnings);
        (head, warnings)
    }

    #[test]
    fn head_ref_names_the_checked_out_branch_without_warning() {
        let repo = crate::test_support::git_repo();
        let root = repo.path().canonicalize().unwrap();
        assert_eq!(head_ref_of(&root), (Some("main".to_string()), Vec::new()));
    }

    #[test]
    fn a_detached_head_is_no_branch_and_no_warning() {
        use crate::test_support::git_in;
        let repo = crate::test_support::git_repo();
        let root = repo.path().canonicalize().unwrap();
        git_in(&root, &["commit", "--allow-empty", "-q", "-m", "base"]);
        git_in(&root, &["checkout", "-q", "--detach"]);
        assert_eq!(head_ref_of(&root), (None, Vec::new()));
    }

    /// A HEAD that could not be read is not a detached HEAD: the label stays
    /// empty but the report says the read failed.
    #[test]
    fn a_failed_head_read_is_reported_not_taken_for_detached() {
        let dir = tempfile::tempdir().unwrap();
        let (head, warnings) = head_ref_of(dir.path());
        assert_eq!(head, None);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("cannot read the checked-out branch"),
            "{warnings:?}"
        );
    }
}
