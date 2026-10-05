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
//! - The location is the main checkout's `.gitpulse/worktrees/<slug>-<id>` —
//!   the layout the Worktrees panel's agent lane already uses, which the agent
//!   worktree detector recognises — and `.gitpulse/worktrees/` is excluded
//!   through `$GIT_COMMON_DIR/info/exclude` with the same verified writer
//!   DevMap uses. Only `worktrees/` is excluded: `.gitpulse/hooks.toml` is
//!   repository configuration a team may commit.
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
//! A worktree whose attempt was accepted is never removed here. The agent's
//! work lives in it; ending the run does not end the need for it.

use super::WorkbenchError;
use crate::engine::git_cli::{git_captured, resolve_git_common_dir};
use std::path::{Path, PathBuf};

const EXCLUDED: &str = ".gitpulse/worktrees";
const EXCLUDE_MARKER: &str =
    "# GitPulse: task agent worktrees (machine-generated, never committed)";
const MAX_SLUG: usize = 40;

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
    let common = resolve_git_common_dir(selected).map_err(refused)?;
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
    let run = git_captured(selected, &["worktree", "list", "--porcelain"])
        .and_then(|r| r.require_complete("Worktree listing"))
        .map_err(refused)?;
    if run.status_code != 0 {
        return Err(refused("Could not list this repository's worktrees."));
    }
    let text = String::from_utf8_lossy(&run.stdout);
    let wanted = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut here = false;
    for line in text.lines() {
        if let Some(entry) = line.strip_prefix("worktree ") {
            let entry = Path::new(entry);
            here = entry.canonicalize().unwrap_or_else(|_| entry.to_path_buf()) == wanted;
        } else if here && line == format!("branch refs/heads/{branch}") {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn provision(
    selected: &str,
    run_id: &str,
    title: &str,
) -> Result<Provisioned, WorkbenchError> {
    let source = Path::new(selected);
    let root = main_checkout(source)?;
    let name = format!("{}-{}", slug(title), short(run_id)?);
    let path = root.join(".gitpulse").join("worktrees").join(&name);
    let branch = format!("gitpulse/{name}");
    let path_text = path
        .to_str()
        .ok_or_else(|| refused("The worktree path is not valid Unicode."))?
        .to_owned();
    if path.exists() {
        if existing(source, &path, &branch)? {
            return Ok(Provisioned {
                source: selected.into(),
                path: path_text,
                branch,
                created: false,
            });
        }
        return Err(refused(format!(
            "{} already exists and is not this attempt's worktree. Remove it or prepare a new attempt.",
            path.display()
        )));
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
    crate::engine::worktree::add_worktree_extended(
        selected,
        &path_text,
        Some(&branch),
        None,
        false,
        true,
    )
    .map_err(refused)?;
    Ok(Provisioned {
        source: selected.into(),
        path: path_text,
        branch,
        created: true,
    })
}

/// Undoes `provision` after the store refused the attempt. A worktree a retry
/// merely reused is left alone: it belongs to the attempt that made it.
pub(super) fn discard(provisioned: &Provisioned) -> Result<(), String> {
    if !provisioned.created {
        return Ok(());
    }
    let remove = crate::engine::worktree::remove_worktree_argv(&provisioned.path, true);
    let refs: Vec<&str> = remove.iter().map(String::as_str).collect();
    crate::harness::guard_command(&provisioned.source, &refs)?;
    crate::engine::worktree::remove_worktree(&provisioned.source, &provisioned.path, true)?;
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
    use super::slug;

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
}
