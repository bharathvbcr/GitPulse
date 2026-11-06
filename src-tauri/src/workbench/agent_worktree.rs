//! A fresh linked worktree for one task attempt.
//!
//! The run store gives each checkout one agent at a time, so running several
//! tasks in one repository means giving each its own working tree. This used
//! to happen in the renderer, which created the worktree before the store had
//! accepted anything and never removed it: every refused preparation — a busy
//! checkout, full capacity, a moved revision — left a worktree and an
//! `agent/<id>` branch behind, and the next press made another. It also put
//! the worktree inside whichever checkout was selected, so a linked worktree
//! nested inside another one, and nothing kept the directory out of
//! `git status`, where `git add -A` in the main checkout would stage it.
//!
//! Here it is one native step owned next to the preparation that uses it:
//!
//! - The location is the main checkout's `.gitpulse/worktrees/<slug>-<id>`,
//!   built from the same segments the agent-worktree detector reads
//!   (`engine::worktree::gitpulse_lane_container`), so it is reported as a
//!   GitPulse task worktree (`is_gitpulse_lane`) — and `.gitpulse/worktrees/`
//!   is excluded through `$GIT_COMMON_DIR/info/exclude` with the same
//!   verified writer DevMap uses. Only `worktrees/` is excluded:
//!   `.gitpulse/hooks.toml` is repository configuration a team may commit.
//! - This is the only creator there. The Worktrees panel's "agent lane"
//!   preset filled the hand-made form with a path in this container, which
//!   skipped the exclusion and nested inside whichever checkout was selected;
//!   it is gone, and `cmd_add_worktree` refuses such a target
//!   (`engine::worktree::refuse_gitpulse_lane_target`).
//! - The path and branch derive from the attempt id, so a retried preparation
//!   (the same request after a lost reply) reuses the worktree it already
//!   made instead of failing on an existing directory or making a second.
//! - Both the add and the rollback pass the command gate, like every other
//!   worktree mutation.
//! - When the store refuses the attempt, the worktree and its branch are
//!   removed again. The branch is deleted with `-d`, never `-D`: it starts at
//!   the selected checkout's HEAD, so `-d` succeeds, and if anything did
//!   commit to it in between, `-d` refuses rather than losing that work.
//!
//! - Setup is finished only when the `post_create` hook succeeded, and that is
//!   recorded in the worktree's private Git directory ([`SETUP_COMPLETE`]).
//!   A tree found without it was left by a preparation that died mid-setup;
//!   it is rebuilt rather than handed to an agent as if it were ready. (The
//!   preparation in flight holds its attempt's place for the whole setup, so
//!   a retry never meets a tree another call is still building.)
//!
//! A worktree whose attempt was claimed is never removed here. The agent's
//! work lives in it; ending the run does not end the need for it. A worktree
//! whose attempt ended *unclaimed* — cancelled, or expired — is given back by
//! [`reclaim`], without force: Git keeps a tree with changes, `branch -d`
//! keeps a branch with commits, and a tree something still has open is kept.

use super::WorkbenchError;
use crate::engine::git_cli::{git_text, resolve_git_common_dir, resolve_git_dir};
use crate::engine::worktree::{gitpulse_lane_container, GITPULSE_LANE_DIR as EXCLUDED};
use crate::ipc_error::CommandError;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const EXCLUDE_MARKER: &str =
    "# GitPulse: task agent worktrees (machine-generated, never committed)";
const MAX_SLUG: usize = 40;
/// Written into the worktree's private Git directory (never its files, where
/// `git status` would show it) once the `post_create` hook has succeeded.
/// `git worktree remove` takes it with the rest of that directory.
const SETUP_COMPLETE: &str = "gitpulse-setup-complete";

pub(super) struct Provisioned {
    /// The checkout the worktree was branched from, which runs the git.
    source: String,
    pub path: String,
    pub branch: String,
    /// False when a retry found the worktree this attempt already made.
    created: bool,
}

fn refused(message: impl Into<String>) -> WorkbenchError {
    WorkbenchError::new("worktree_unavailable", message)
}

/// A branch- and directory-safe name from a task title: lowercase ASCII
/// letters and digits joined by single hyphens, at most 40 characters, never
/// empty.
pub(super) fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            if out.len() >= MAX_SLUG {
                break;
            }
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "task".into()
    } else {
        out.into()
    }
}

/// The 8 leading alphanumerics of the attempt id, lowercased.
fn short(run_id: &str) -> Result<String, WorkbenchError> {
    let short: String = run_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if short.len() < 4 {
        return Err(WorkbenchError::new(
            "invalid_input",
            "A worktree attempt needs an identity with at least four letters or digits.",
        ));
    }
    Ok(short)
}

/// The main working tree: the folder holding the repository's common `.git`.
fn main_checkout(selected: &Path) -> Result<PathBuf, WorkbenchError> {
    let common = resolve_git_common_dir(selected).map_err(|e| refused(e.into_message()))?;
    if common.file_name().and_then(|n| n.to_str()) != Some(".git") {
        return Err(refused(
            "This repository has no main checkout beside its Git directory (it is bare or uses a separate Git directory), so GitPulse cannot place an agent worktree. Choose an existing checkout instead.",
        ));
    }
    common
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| refused("The repository's main checkout could not be located."))
}

/// Whether `path` is already a worktree of this repository on `branch`.
fn existing(selected: &Path, path: &Path, branch: &str) -> Result<bool, WorkbenchError> {
    let entries = crate::engine::worktree::read_worktree_entries(selected)
        .map_err(|e| refused(e.into_message()))?;
    let wanted = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    Ok(entries.iter().any(|entry| {
        let listed = Path::new(&entry.path);
        listed
            .canonicalize()
            .unwrap_or_else(|_| listed.to_path_buf())
            == wanted
            && entry.branch.as_deref() == Some(branch)
    }))
}

/// The one naming rule: an attempt's worktree directory name, from the task
/// title it was prepared for and its id. Its branch is `gitpulse/<name>`.
fn lane_name(title: &str, run_id: &str) -> Result<String, WorkbenchError> {
    Ok(format!("{}-{}", slug(title), short(run_id)?))
}

fn lane_branch(name: &str) -> String {
    format!("gitpulse/{name}")
}

/// Whether this worktree's `post_create` setup finished.
fn setup_complete(path: &Path) -> bool {
    resolve_git_dir(path).is_ok_and(|dir| dir.join(SETUP_COMPLETE).is_file())
}

/// `accepted`: the store already holds this attempt, so its worktree was set
/// up in full before it was accepted (preparation is only stored after the
/// hook succeeds) and is reused whatever the marker says — builds before the
/// marker existed wrote none.
pub(super) fn provision(
    selected: &str,
    run_id: &str,
    title: &str,
    accepted: bool,
) -> Result<Provisioned, WorkbenchError> {
    provision_with(
        selected,
        run_id,
        title,
        accepted,
        &crate::engine::cow_clone::CacheSelection::default(),
    )
}

/// [`provision`] with the cache copy's bounds and free-space reader given.
fn provision_with(
    selected: &str,
    run_id: &str,
    title: &str,
    accepted: bool,
    caches: &crate::engine::cow_clone::CacheSelection,
) -> Result<Provisioned, WorkbenchError> {
    let source = Path::new(selected);
    let root = main_checkout(source)?;
    let name = lane_name(title, run_id)?;
    let path = gitpulse_lane_container(&root).join(&name);
    let branch = lane_branch(&name);
    let path_text = path
        .to_str()
        .ok_or_else(|| refused("The worktree path is not valid Unicode."))?
        .to_owned();
    if path.exists() {
        if !existing(source, &path, &branch)? {
            return Err(refused(format!(
                "{} already exists and is not this attempt's worktree. Remove it or prepare a new attempt.",
                path.display()
            )));
        }
        let found = Provisioned {
            source: selected.into(),
            path: path_text.clone(),
            branch: branch.clone(),
            created: false,
        };
        if accepted || setup_complete(&path) {
            return Ok(found);
        }
        // Made for this attempt, never accepted, and its setup never
        // finished: the preparation that built it died during the hook.
        // Nothing was started in it, so it is rebuilt from the beginning.
        log::warn!(target: "workbench", "agent worktree {} was left half set up by an earlier preparation; rebuilding it", found.path);
        discard(&Provisioned {
            created: true,
            ..found
        })
        .map_err(|e| {
            refused(format!(
                "{} was left half set up by an earlier preparation of this attempt and could not be removed to set it up again: {e}",
                path.display(), e = e.message()
            ))
        })?;
    }
    let exclude = crate::devmap::init::ensure_dir_excluded(&root, EXCLUDED, EXCLUDE_MARKER);
    if !exclude.is_clean() {
        return Err(refused(format!(
            "GitPulse could not keep {EXCLUDED}/ out of git status, so it will not create an agent worktree there: {exclude:?}"
        )));
    }
    let argv = crate::engine::worktree::add_worktree_argv(&path_text, Some(&branch), None, false);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    crate::harness::guard_command(selected, &refs)
        .map_err(|e| WorkbenchError::new("policy_refused", e))?;
    // The lane's DevMap store is cloned from the main checkout's (APFS
    // `clonefile` / Linux `FICLONE`, `repo_root` rebound to the lane), never
    // left absent. An absent store is not "no index": DevMap's SessionStart
    // hook answers it with a cold `devmap build` in every new lane — measured
    // on this repository at ~7.6 s, ~38 CPU-s and ~366 MB per lane, against
    // ~1.2 s (debug build) and 7–20 MB for this seed, which `devmap build`
    // then brings up to the lane's tree incrementally with an identical graph
    // (docs/ZERO_COPY_AGENTIC_WORKTREES.md §7). A zero-copy
    // pointer is no substitute: the `devmap` binary agents run does not read
    // `base_store_ref.json`, so the hook cold-builds beside it.
    let created = crate::engine::worktree::add_worktree_with_devmap(
        selected,
        &path_text,
        Some(&branch),
        None,
        false,
        Some(caches),
        // Approved hooks only: an unapproved file is reported, not run.
        true,
        crate::engine::worktree::DevmapMode::Copy,
    )
    .map_err(|e| refused(e.into_message()))?;
    let provisioned = Provisioned {
        source: selected.into(),
        path: path_text,
        branch,
        created: true,
    };
    // Caches only save a rebuild; the agent can work without them, so a
    // skipped or failed copy is advice for the launch surfaces, never a
    // refusal: Health and the ledger carry it.
    // Every outcome is advised: a skipped cache never hides a failed one.
    let mut advised = Vec::new();
    if let Some(low) = &created.low_disk {
        advised.push(crate::sysload::Advisory::LowDisk(low.clone()));
    }
    if let Some(message) = &created.cache_error {
        advised.push(crate::sysload::Advisory::CacheError {
            message: message.clone(),
        });
    }
    if created.disk_unknown {
        advised.push(crate::sysload::Advisory::DiskUnknown);
    }
    let advisory = crate::sysload::Advisory::of(advised);
    if let Some(cache) = &created.cache_error {
        log::warn!(target: "workbench", "agent worktree {} has no copied build caches: {cache}", provisioned.path);
    }
    // Like the caches, the index only saves a rebuild: a lane without a seeded
    // store still gets one from DevMap's own hook, and `devmap status` there
    // says it is missing until it lands. So this is logged, never refused.
    let seeded = matches!(
        created.devmap,
        crate::engine::worktree::DevmapOutcome::Provisioned { .. }
    );
    match &created.devmap {
        crate::engine::worktree::DevmapOutcome::Skipped { reason } => {
            log::info!(target: "workbench", "agent worktree {} has no seeded DevMap store: {reason}", provisioned.path);
        }
        crate::engine::worktree::DevmapOutcome::Failed { error } => {
            log::warn!(target: "workbench", "agent worktree {}: seeding its DevMap store failed: {error}", provisioned.path);
        }
        crate::engine::worktree::DevmapOutcome::Provisioned { .. }
        | crate::engine::worktree::DevmapOutcome::NotRequested => {}
    }
    crate::sysload::note_launch_advisory(selected, advisory);
    if let Some(notice) = created.hook_notice {
        log::warn!(target: "workbench", "agent worktree {}: {notice}", provisioned.path);
    }
    if let Some(hook) = created.hook_error {
        // The repository's own setup did not finish (dependencies, seeds), so
        // an agent started here would work in a tree its owners say is not
        // ready — and nothing would tell it. The attempt does not start, and
        // the tree made for it goes, exactly as when the store refuses it.
        let cleanup = discard(&provisioned)
            .err()
            .map(|e| {
                format!(
                    " The worktree could not be removed and is still at {} on branch {}: {e}",
                    provisioned.path,
                    provisioned.branch,
                    e = e.message()
                )
            })
            .unwrap_or_default();
        return Err(refused(format!(
            "The repository's post_create hook failed in the new worktree, so the agent was not started: {hook}.{cleanup}"
        )));
    }
    // Where the lane was cut from, for the merge to hold the target to. A
    // checkout on a detached HEAD, or on a branch name the writers will not
    // pass to git, has no branch to record; the merge then skips that check
    // and reports no base, rather than guessing one.
    match crate::engine::git_reader::head_branch(source) {
        Ok(Some(base_branch)) => {
            let recorded = git_text(
                Path::new(&provisioned.path),
                &["rev-parse", "--verify", "HEAD"],
            )
            .and_then(|oid| {
                crate::engine::worktree::write_base_record(
                    Path::new(&provisioned.path),
                    &base_branch,
                    oid.trim(),
                )
            });
            if let Err(error) = recorded {
                let cleanup = discard(&provisioned)
                    .err()
                    .map(|e| {
                        format!(
                            " The worktree could not be removed and is still at {} on branch {}: {e}",
                            provisioned.path, provisioned.branch, e = e.message()
                        )
                    })
                    .unwrap_or_default();
                return Err(refused(format!(
                    "GitPulse could not record which branch the new worktree was cut from, so the agent was not started: {error}.{cleanup}", error = error.message()
                )));
            }
        }
        Ok(None) => {}
        Err(error) => {
            log::warn!(target: "workbench", "agent worktree {} has no recorded base branch: {error}", provisioned.path, error = error.message());
        }
    }
    // Without the record, a later retry of this same attempt rebuilds the
    // tree instead of trusting it: slower, never wrong.
    if let Err(error) = resolve_git_dir(Path::new(&provisioned.path)).and_then(|dir| {
        std::fs::write(dir.join(SETUP_COMPLETE), b"")
            .map_err(|e| e.to_string())
            .map_err(CommandError::from)
    }) {
        log::warn!(target: "workbench", "agent worktree {} is set up but could not record it: {error}", provisioned.path, error = error.message());
    }
    if seeded {
        refresh_seeded_index(&provisioned.path);
    }
    Ok(provisioned)
}

/// Bring a seeded lane's DevMap store to the lane's own tree.
///
/// The seed is the main checkout's latest generation, which can trail its HEAD
/// and carry its uncommitted edits. DevMap reports that honestly
/// (`is_fresh: false`), but its SessionStart hook only builds where there is
/// no store, so a seeded lane would otherwise answer from the main
/// checkout's working state until something else built it. This is the
/// GitPulse live refresh every open repository gets: it builds only when
/// `devmap status` says the store is not fresh, incrementally (measured at
/// ~2.5 s on this repository, against ~7.6 s cold), on a supervised background
/// thread holding the lane's writer slot, and it answers without waiting for
/// the build. Called only once setup is complete, so no build runs in a tree a
/// failed setup is about to remove.
fn refresh_seeded_index(path: &str) -> crate::devmap::live::LiveRefreshOutcome {
    let outcome = crate::devmap::live::start_refresh(path, false);
    log::info!(
        target: "workbench",
        "agent worktree {path}: DevMap index refresh {:?}{}",
        outcome.decision,
        outcome
            .reason
            .as_deref()
            .map(|reason| format!(" ({reason})"))
            .unwrap_or_default()
    );
    outcome
}

/// What a removal left to say beyond "removed": the files that could not be
/// deleted and the leftovers of earlier removals the sweep could not clear.
/// Logged, and returned for a caller that reports it. `None` when nothing.
fn removal_notice(
    path: &str,
    removal: &crate::engine::worktree::WorktreeRemoval,
) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    parts.extend(removal.cleanup_error.as_deref());
    parts.extend(removal.sweep_problems.iter().map(String::as_str));
    if parts.is_empty() {
        return None;
    }
    let notice = parts.join("; ");
    log::warn!(target: "workbench", "agent worktree {path} was removed, but: {notice}");
    Some(notice)
}

/// What [`reclaim`] did with an unclaimed attempt's worktree.
pub(super) struct Reclaimed {
    path: String,
    branch: String,
    removed: bool,
    branch_deleted: bool,
    kept_because: Option<String>,
    /// The worktree is removed but its files could not all be deleted, or a
    /// leftover of an earlier removal could not be cleared; see
    /// [`removal_notice`].
    cleanup_error: Option<String>,
}

impl Reclaimed {
    fn kept(path: &str, branch: &str, why: String) -> Self {
        Self {
            path: path.into(),
            branch: branch.into(),
            removed: false,
            branch_deleted: false,
            kept_because: Some(why),
            cleanup_error: None,
        }
    }

    /// The optional `worktree` field beside a cancel's `item`.
    pub(super) fn to_json(&self) -> Value {
        json!({
            "path": self.path,
            "branch": self.branch,
            "removed": self.removed,
            "branch_deleted": self.branch_deleted,
            "kept_because": self.kept_because,
            "cleanup_error": self.cleanup_error,
        })
    }

    pub(super) fn removed(&self) -> bool {
        self.removed
    }

    pub(super) fn describe(&self) -> String {
        match (&self.kept_because, self.removed) {
            (None, _) => format!("removed {} and its branch {}", self.path, self.branch),
            (Some(why), true) => format!("removed {}; {why}", self.path),
            (Some(why), false) => format!("kept {}: {why}", self.path),
        }
    }
}

/// The main checkout, path and branch [`provision`] made for `run`, when the
/// run's checkout is that worktree. Derived from the run's own
/// snapshot (its task title as prepared, its id and its recorded Git
/// directory), so a task renamed since does not lose track of it, and a run
/// prepared in any other checkout is never mistaken for one.
fn made_for(run: &Value) -> Option<(PathBuf, String, String)> {
    let name = lane_name(run["task_title"].as_str()?, run["id"].as_str()?).ok()?;
    let common = Path::new(run["git_common_dir"].as_str()?);
    if common.file_name().and_then(|n| n.to_str()) != Some(".git") {
        return None;
    }
    let main = common.parent()?.to_path_buf();
    let path = gitpulse_lane_container(&main).join(&name);
    let branch = lane_branch(&name);
    // The run's directory is the tree itself, or a folder inside it when the
    // attempt was prepared for a subdirectory of its checkout.
    if !Path::new(run["cwd"].as_str()?).starts_with(&path)
        || run["head_ref"].as_str() != Some(format!("refs/heads/{branch}").as_str())
    {
        return None;
    }
    Some((main, path.to_str()?.to_owned(), branch))
}

/// Gives back the worktree made for an attempt that ended without ever being
/// claimed. `None` when the run has no such worktree, or it is already gone.
///
/// Never forced. It is kept, and the result says why, when it is no longer
/// this attempt's worktree, when `open_files` cannot show that nothing has a
/// file or a working directory in it, or when `git worktree remove` refuses
/// (changes or untracked files). Its branch goes with `-d`, which keeps one
/// that has commits of its own.
pub(super) fn reclaim(
    run: &Value,
    open_files: &dyn Fn(&Path) -> Result<(), CommandError>,
) -> Option<Reclaimed> {
    // Cancel and release give a tree back; under memory pressure that is the
    // last work to defer, so its git children run as the user action they
    // are, never shed with the refreshes (`sysload::governor`).
    crate::engine::git_cli::with_admission(crate::engine::git_cli::Admission::Interactive, || {
        reclaim_unshed(run, open_files)
    })
}

fn reclaim_unshed(
    run: &Value,
    open_files: &dyn Fn(&Path) -> Result<(), CommandError>,
) -> Option<Reclaimed> {
    if !run["owner_id"].is_null() {
        return None;
    }
    let (main, path, branch) = made_for(run)?;
    if !Path::new(&path).exists() {
        return None;
    }
    let source = main.to_str()?.to_owned();
    match existing(&main, Path::new(&path), &branch) {
        Ok(true) => {}
        Ok(false) => {
            return Some(Reclaimed::kept(
                &path,
                &branch,
                format!(
                    "it is no longer a worktree on {branch}, so it is not this attempt's to remove"
                ),
            ))
        }
        Err(error) => {
            return Some(Reclaimed::kept(
                &path,
                &branch,
                format!("its worktree could not be confirmed: {}", error.message),
            ))
        }
    }
    if let Err(why) = open_files(Path::new(&path)) {
        return Some(Reclaimed::kept(
            &path,
            &branch,
            format!(
                "something may still be using it ({why})",
                why = why.message()
            ),
        ));
    }
    let remove = crate::engine::worktree::remove_worktree_argv(&path, false);
    let refs: Vec<&str> = remove.iter().map(String::as_str).collect();
    let removal = match crate::harness::guard_command(&source, &refs)
        .map_err(CommandError::from)
        .and_then(|_| crate::engine::worktree::remove_worktree(&source, &path, false))
    {
        Ok(removal) => removal,
        Err(why) => return Some(Reclaimed::kept(&path, &branch, why.into_message())),
    };
    let cleanup_error = removal_notice(&path, &removal);
    let delete = ["git", "branch", "-d", branch.as_str()];
    let deleted = crate::harness::guard_command(&source, &delete)
        .map_err(CommandError::from)
        .and_then(|_| {
            crate::engine::git_writer::GitWriter::delete_branch(&source, &branch, false).map(|_| ())
        });
    Some(Reclaimed {
        kept_because: deleted
            .as_ref()
            .err()
            .map(|why| format!("its branch {branch} was kept: {}", why.message())),
        branch_deleted: deleted.is_ok(),
        path,
        branch,
        removed: true,
        cleanup_error,
    })
}

/// Undoes `provision` after the store refused the attempt. A worktree a retry
/// merely reused is left alone: it belongs to the attempt that made it.
pub(super) fn discard(provisioned: &Provisioned) -> Result<(), CommandError> {
    if !provisioned.created {
        return Ok(());
    }
    let remove = crate::engine::worktree::remove_worktree_argv(&provisioned.path, true);
    let refs: Vec<&str> = remove.iter().map(String::as_str).collect();
    crate::harness::guard_command(&provisioned.source, &refs)?;
    let removal =
        crate::engine::worktree::remove_worktree(&provisioned.source, &provisioned.path, true)?;
    // The worktree is gone; files left to delete are not a failed discard, but
    // they are not silent either.
    removal_notice(&provisioned.path, &removal);
    let delete = ["git", "branch", "-d", provisioned.branch.as_str()];
    crate::harness::guard_command(&provisioned.source, &delete)?;
    crate::engine::git_writer::GitWriter::delete_branch(
        &provisioned.source,
        &provisioned.branch,
        false,
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::{provision, provision_with, slug};
    use crate::engine::cow_clone::CacheSelection;
    use crate::engine::git_cli::{git_global, git_text};
    use crate::sysload::{Advisory, LowDiskCause, Reading, Why};
    use std::path::Path;

    fn plenty(_: &Path) -> Reading<u64> {
        Reading::Value(500 << 30)
    }
    fn nearly_full(_: &Path) -> Reading<u64> {
        Reading::Value(1 << 30)
    }
    fn unreadable(_: &Path) -> Reading<u64> {
        Reading::Unavailable(Why::Failed)
    }

    /// A repository with an ignored `node_modules` holding one file.
    fn repo_with_cache(dir: &Path) -> std::path::PathBuf {
        let root = dir.join("repo");
        git_global(&["init", "-b", "main", root.to_str().unwrap()]).unwrap();
        crate::test_support::trust_repo(&root);
        std::fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        std::fs::write(root.join("README.md"), "x\n").unwrap();
        git_text(&root, &["add", "."]).unwrap();
        git_text(
            &root,
            &[
                "-c",
                "user.name=Workbench Test",
                "-c",
                "user.email=workbench@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "init",
            ],
        )
        .unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(
            root.join("node_modules/pkg/index.js"),
            "module.exports = 1;\n",
        )
        .unwrap();
        root
    }

    fn with_reader(free_space: crate::sysload::FreeSpace) -> CacheSelection {
        CacheSelection {
            free_space,
            ..CacheSelection::default()
        }
    }

    /// Low disk is advice: the worktree is made and the agent is admitted,
    /// only the optional cache copy is skipped, and the advisory is typed.
    #[test]
    fn low_disk_skips_the_cache_clone_but_still_creates_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let root = repo_with_cache(dir.path());
        let made = provision_with(
            root.to_str().unwrap(),
            "d15c0000-1",
            "Low disk lane",
            false,
            &with_reader(nearly_full),
        )
        .expect("low disk must never refuse a worktree");
        assert!(Path::new(&made.path).join("README.md").is_file());
        assert!(
            !Path::new(&made.path).join("node_modules").exists(),
            "the cache was cloned onto a nearly full volume"
        );
        let report = crate::sysload::governor_report();
        let Some(Advisory::LowDisk(low)) = report.advisory else {
            panic!("no low_disk advisory: {:?}", report.advisory);
        };
        assert_eq!(low.cause, LowDiskCause::BelowFloor);
        assert_eq!(low.free_bytes, Some(1 << 30));
        assert_eq!(low.skipped, vec!["node_modules".to_string()]);
        // Advice never changes the verdict.
        assert!(report.verdict.admit);
    }

    #[test]
    fn plenty_of_space_clones_the_cache_and_advises_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = repo_with_cache(dir.path());
        let made = provision_with(
            root.to_str().unwrap(),
            "d15c0000-2",
            "Roomy lane",
            false,
            &with_reader(plenty),
        )
        .unwrap();
        assert!(Path::new(&made.path)
            .join("node_modules/pkg/index.js")
            .is_file());
        assert_eq!(crate::sysload::governor_report().advisory, None);
    }

    /// An unreadable volume is unknown, not plenty and not full: nothing is
    /// skipped on its account, and the advice says it could not be read.
    #[test]
    fn an_unreadable_volume_forces_no_skip_and_is_advised_as_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let root = repo_with_cache(dir.path());
        let made = provision_with(
            root.to_str().unwrap(),
            "d15c0000-3",
            "Unknown lane",
            false,
            &with_reader(unreadable),
        )
        .unwrap();
        assert!(Path::new(&made.path)
            .join("node_modules/pkg/index.js")
            .is_file());
        assert_eq!(
            crate::sysload::governor_report().advisory,
            Some(Advisory::DiskUnknown)
        );
    }

    /// A cache that fails to copy is typed into the advisory, not only
    /// logged, and the worktree is still made.
    #[test]
    fn a_cache_error_is_surfaced_as_an_advisory() {
        let dir = tempfile::tempdir().unwrap();
        let root = repo_with_cache(dir.path());
        // A node_modules whose entries cannot be walked: a depth bound of
        // zero fails the copy rather than skipping it.
        let selection = CacheSelection {
            caps: crate::engine::cow_clone::CowCaps {
                max_depth: 0,
                ..crate::engine::cow_clone::CowCaps::DEFAULT
            },
            free_space: plenty,
            ..CacheSelection::default()
        };
        let made = provision_with(
            root.to_str().unwrap(),
            "d15c0000-4",
            "Cache error lane",
            false,
            &selection,
        )
        .expect("a cache error must not refuse a worktree");
        assert!(Path::new(&made.path).join("README.md").is_file());
        let Some(Advisory::CacheError { message }) = crate::sysload::governor_report().advisory
        else {
            panic!("cache_error was only logged");
        };
        assert!(message.contains("node_modules"), "{message}");
    }

    /// The repository's own setup failed in the new tree, so the agent does
    /// not start there, and the tree and branch made for it are removed. The
    /// failure used to be discarded: the agent started in a tree whose
    /// dependencies were never installed, and nothing said so.
    #[cfg(unix)]
    #[test]
    fn a_failed_setup_hook_refuses_the_agent_and_removes_its_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        git_global(&["init", "-b", "main", root.to_str().unwrap()]).unwrap();
        crate::test_support::trust_repo(&root);
        std::fs::create_dir_all(root.join(".gitpulse")).unwrap();
        let hooks = r#"{"worktree":{"post_create":["echo npm ci failed >&2; exit 7"]}}"#;
        std::fs::write(root.join(".gitpulse/hooks.json"), hooks).unwrap();
        crate::engine::approve_worktree_hooks(&root, hooks).unwrap();
        git_text(&root, &["add", "."]).unwrap();
        git_text(
            &root,
            &[
                "-c",
                "user.name=Workbench Test",
                "-c",
                "user.email=workbench@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "hooks",
            ],
        )
        .unwrap();
        let Err(refused) = provision(root.to_str().unwrap(), "f00dcafe-1", "Preserve E42", false)
        else {
            panic!("an agent worktree whose setup hook failed was provisioned");
        };
        assert_eq!(refused.code, "worktree_unavailable");
        assert!(
            refused.message.contains("post_create") && refused.message.contains("npm ci failed"),
            "{}",
            refused.message
        );
        assert!(
            !refused.message.contains("could not be removed"),
            "{}",
            refused.message
        );
        let left = std::fs::read_dir(root.join(".gitpulse/worktrees"))
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(left, 0, "the refused attempt's worktree was left behind");
        let branches = git_text(&root, &["branch", "--list", "gitpulse/*"]).unwrap();
        assert!(branches.trim().is_empty(), "branch left behind: {branches}");
    }

    /// A provisioned task worktree never shows up in the main checkout's
    /// `git status` — where `git add -A` would stage it — and is reported as
    /// GitPulse's own task worktree rather than as an external agent.
    #[test]
    fn a_provisioned_worktree_is_excluded_from_status_and_named_a_gitpulse_task() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        git_global(&["init", "-b", "main", root.to_str().unwrap()]).unwrap();
        crate::test_support::trust_repo(&root);
        std::fs::write(root.join("README.md"), "x\n").unwrap();
        git_text(&root, &["add", "."]).unwrap();
        git_text(
            &root,
            &[
                "-c",
                "user.name=Workbench Test",
                "-c",
                "user.email=workbench@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "init",
            ],
        )
        .unwrap();
        let made = provision(
            root.to_str().unwrap(),
            "c0ffee00-1",
            "Fix the watcher",
            false,
        )
        .unwrap();
        assert!(std::path::Path::new(&made.path).is_dir(), "{}", made.path);
        // The branch and commit the lane was cut from are recorded in its
        // private Git directory, where the merge reads them.
        let base = std::fs::read_to_string(
            crate::engine::git_cli::resolve_git_dir(std::path::Path::new(&made.path))
                .unwrap()
                .join(crate::engine::worktree::BASE_RECORD),
        )
        .expect("no base record was written");
        let head = git_text(&root, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(base, format!("main\n{}\n", head.trim()));
        let status = git_text(&root, &["status", "--porcelain", "--untracked-files=all"]).unwrap();
        // The command gate's ledger also writes into the repository; only the
        // worktree container is this test's concern. Unexcluded, it reads
        // `?? .gitpulse/` here.
        assert!(
            !status.contains(".gitpulse"),
            "the task worktree leaked into git status:\n{status}"
        );
        // And the exclusion really is what hides it: with the rule removed
        // from info/exclude, the container shows up.
        let exclude = std::path::Path::new(
            &git_text(&root, &["rev-parse", "--git-common-dir"])
                .unwrap()
                .trim()
                .to_string(),
        )
        .join("info/exclude");
        let exclude = if exclude.is_absolute() {
            exclude
        } else {
            root.join(exclude)
        };
        let rules = std::fs::read_to_string(&exclude).unwrap();
        assert!(rules.contains(".gitpulse/worktrees"), "{rules}");
        std::fs::write(&exclude, rules.replace(".gitpulse/worktrees", "")).unwrap();
        let unexcluded = git_text(&root, &["status", "--porcelain"]).unwrap();
        assert!(unexcluded.contains(".gitpulse/"), "{unexcluded}");
        std::fs::write(&exclude, rules).unwrap();
        let layout = crate::engine::worktree::agent_layout(&made.path).expect("agent layout");
        assert!(
            crate::engine::worktree::is_gitpulse_lane(&layout.kind),
            "{layout:?}"
        );
        assert_eq!(layout.slug, "fix-the-watcher-c0ffee00");
        assert_eq!(made.branch, "gitpulse/fix-the-watcher-c0ffee00");
        // The hand-made path into the same container is refused.
        assert!(crate::engine::worktree::refuse_gitpulse_lane_target(&made.path).is_err());
    }

    /// `codeintel`'s one-generation fixture, trusted, with its source
    /// committed: an indexed main checkout a lane can be cut from.
    fn indexed_checkout() -> tempfile::TempDir {
        let dir = crate::codeintel::tests::repo_with_one_generation();
        let root = dir.path();
        crate::test_support::trust_repo(root);
        // The fixture writes its source but commits nothing; a lane is cut
        // from a commit.
        git_text(root, &["add", "src"]).unwrap();
        git_text(
            root,
            &[
                "-c",
                "user.name=Workbench Test",
                "-c",
                "user.email=workbench@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "init",
            ],
        )
        .unwrap();
        dir
    }

    /// A `devmap` that answers every `status` "not fresh" and every `build`
    /// "ok", logging `<cwd> <argv>` to a file outside any worktree (so it can
    /// never show in a lane's `git status`).
    #[cfg(unix)]
    fn stale_devmap(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let log = dir.join("devmap-argv.log");
        let bin = dir.join("devmap");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\n\
                 printf '%s %s\\n' \"$PWD\" \"$*\" >> '{log}'\n\
                 if [ \"$1\" = status ]; then printf '%s\\n' '{{\"is_fresh\":false,\"schema_outdated\":false}}'; exit 0; fi\n\
                 if [ \"$1\" = build ]; then printf '%s\\n' '{{\"ok\":true}}'; exit 0; fi\n\
                 exit 2\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        (bin, log)
    }

    /// The seed is the main checkout's generation, which DevMap reports as not
    /// fresh for the lane — and its SessionStart hook builds only where there
    /// is no store. So the lane's own incremental build is started here, in
    /// the lane, once setup is done; without it the lane answers from the main
    /// checkout's working state until something else happens to build it.
    #[cfg(unix)]
    #[test]
    fn a_seeded_lane_whose_index_is_not_fresh_gets_its_own_build_started() {
        let bin_dir = tempfile::tempdir().unwrap();
        let (bin, log) = stale_devmap(bin_dir.path());
        let _devmap = crate::devmap::cli::bind_test_binary(bin.to_string_lossy().into_owned());
        let anchor_dir = indexed_checkout();
        let made = provision_with(
            anchor_dir.path().to_str().unwrap(),
            "d15c0000-8",
            "Stale seed lane",
            false,
            &with_reader(plenty),
        )
        .expect("an indexed checkout must still give a lane");
        let lane = Path::new(&made.path).canonicalize().unwrap();
        assert!(devmap_query::paths::store_path(&lane).is_file());

        let deadline = std::time::Instant::now()
            + crate::test_support::env::coverage_relaxed(std::time::Duration::from_secs(10));
        let built_in_lane = |text: &str| {
            text.lines().any(|line| {
                line.split_once(' ').is_some_and(|(cwd, argv)| {
                    Path::new(cwd).canonicalize().ok().as_deref() == Some(lane.as_path())
                        && argv.split_whitespace().next() == Some("build")
                })
            })
        };
        loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if built_in_lane(&text) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no devmap build was started in the seeded lane {}; devmap saw:\n{text}",
                lane.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// A lane cut from an indexed checkout starts with a DevMap store of its
    /// own — cloned, bound to the lane, queryable before any build — instead
    /// of none, which DevMap's SessionStart hook answers with a cold
    /// full-repository build in every lane.
    #[test]
    fn a_lane_cut_from_an_indexed_checkout_gets_its_own_seeded_devmap_store() {
        // The seeded lane asks for a refresh. No real `devmap` runs in a unit
        // test: the bound binary is one that is not there, so the ask fails
        // harmlessly (logged) and the seed stays exactly as placed.
        let bin_dir = tempfile::tempdir().unwrap();
        let _devmap = crate::devmap::cli::bind_test_binary(
            bin_dir
                .path()
                .join("no-devmap")
                .to_string_lossy()
                .into_owned(),
        );
        let anchor_dir = indexed_checkout();
        let made = provision_with(
            anchor_dir.path().to_str().unwrap(),
            "d15c0000-7",
            "Indexed lane",
            false,
            &with_reader(plenty),
        )
        .expect("an indexed checkout must still give a lane");
        let lane = Path::new(&made.path);
        let db = devmap_query::paths::store_path(lane);
        assert!(
            db.is_file(),
            "the lane has no DevMap store of its own at {}",
            db.display()
        );

        let status = crate::codeintel::status(&made.path);
        assert!(
            status.available,
            "the seeded store does not open: {status:?}"
        );
        assert_eq!(status.generation_id, Some(1), "{status:?}");

        // Bound to the lane, not the checkout it was cloned from: a store that
        // still names a live sibling is one DevMap refuses to open.
        let conn =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let bound: String = conn
            .query_row(
                "SELECT repo_root FROM generations ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert_eq!(
            Path::new(&bound).canonicalize().unwrap(),
            lane.canonicalize().unwrap()
        );

        // Its own inode — two worktrees writing one SQLite file corrupts it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(&db).unwrap().nlink(), 1);
        }

        // And it never shows in the lane's `git status`.
        let porcelain = git_text(lane, &["status", "--porcelain"]).unwrap();
        assert_eq!(porcelain.trim(), "", "seeding dirtied the lane");
    }

    #[test]
    fn slugs_are_branch_and_path_safe_and_bounded() {
        assert_eq!(
            slug("Fix the flaky watcher test!"),
            "fix-the-flaky-watcher-test"
        );
        assert_eq!(slug("  ../../etc/passwd  "), "etc-passwd");
        assert_eq!(slug("ÜBER 🚀 café"), "ber-caf");
        assert_eq!(slug(""), "task");
        assert_eq!(slug("---"), "task");
        assert_eq!(slug("feat: a.lock@{1}"), "feat-a-lock-1");
        let long = slug(&"abc ".repeat(100));
        assert!(long.len() <= 40, "{long}");
        assert!(!long.ends_with('-'));
        for s in [slug("x"), long, slug("A b")] {
            assert!(s
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
            assert!(!s.starts_with('-') && !s.contains("--"));
        }
    }

    // ---- adversarial W4-L3 (adv-w4l3) ----

    fn panicking_reader(_: &Path) -> Reading<u64> {
        panic!("statvfs reader blew up")
    }

    /// Attack 1: a panic in the free-space reader must not abort
    /// provisioning: the worktree is still made.
    #[test]
    fn adv_a_panicking_free_space_reader_does_not_abort_provisioning() {
        let dir = tempfile::tempdir().unwrap();
        let root = repo_with_cache(dir.path());
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            provision_with(
                root.to_str().unwrap(),
                "adv-panic-1",
                "Panic lane",
                false,
                &with_reader(panicking_reader),
            )
        }));
        let made = outcome
            .expect("the reader panic unwound out of provisioning")
            .expect("provisioning refused");
        assert!(Path::new(&made.path).join("README.md").is_file());
    }

    /// Attack 5: a skipped cache and a failed cache in one launch are both
    /// advised; the low-disk advisory must not swallow the failure.
    #[test]
    fn adv_a_low_disk_skip_does_not_hide_a_cache_failure_in_the_same_launch() {
        let dir = tempfile::tempdir().unwrap();
        let root = repo_with_cache(dir.path());
        std::fs::write(root.join(".git/info/exclude"), "node_modules/\n.turbo/\n").unwrap();
        std::fs::write(root.join("node_modules/pkg/index.js"), vec![b'x'; 100]).unwrap();
        std::fs::create_dir_all(root.join(".turbo/a/b")).unwrap();
        std::fs::write(root.join(".turbo/a/b/f"), "x").unwrap();
        let selection = CacheSelection {
            caps: crate::engine::cow_clone::CowCaps {
                max_bytes: 10,
                max_depth: 1,
                ..crate::engine::cow_clone::CowCaps::DEFAULT
            },
            free_space: plenty,
            ..CacheSelection::default()
        };
        crate::engine::cow_clone::with_forced_byte_copy(|| {
            provision_with(
                root.to_str().unwrap(),
                "adv-both-1",
                "Both lane",
                false,
                &selection,
            )
        })
        .unwrap();
        let advisory = crate::sysload::governor_report().advisory.expect("advice");
        let text = advisory.message();
        assert!(
            text.contains(".turbo") && text.contains("node_modules"),
            "only one of the two cache outcomes is advised: {advisory:?}"
        );
    }
}
