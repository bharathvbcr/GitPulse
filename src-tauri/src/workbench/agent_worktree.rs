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
use crate::engine::git_cli::{git_captured, resolve_git_common_dir, resolve_git_dir};
use crate::engine::worktree::{gitpulse_lane_container, GITPULSE_LANE_DIR as EXCLUDED};
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
                path.display()
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
    let created = crate::engine::worktree::add_worktree_extended(
        selected,
        &path_text,
        Some(&branch),
        None,
        false,
        true,
    )
    .map_err(refused)?;
    let provisioned = Provisioned {
        source: selected.into(),
        path: path_text,
        branch,
        created: true,
    };
    if let Some(cache) = created.cache_error {
        // Caches only save a rebuild; the agent can work without them.
        log::warn!(target: "workbench", "agent worktree {} has no copied build caches: {cache}", provisioned.path);
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
                    provisioned.path, provisioned.branch
                )
            })
            .unwrap_or_default();
        return Err(refused(format!(
            "The repository's post_create hook failed in the new worktree, so the agent was not started: {hook}.{cleanup}"
        )));
    }
    // Without the record, a later retry of this same attempt rebuilds the
    // tree instead of trusting it: slower, never wrong.
    if let Err(error) = resolve_git_dir(Path::new(&provisioned.path))
        .and_then(|dir| std::fs::write(dir.join(SETUP_COMPLETE), b"").map_err(|e| e.to_string()))
    {
        log::warn!(target: "workbench", "agent worktree {} is set up but could not record it: {error}", provisioned.path);
    }
    Ok(provisioned)
}

/// What [`reclaim`] did with an unclaimed attempt's worktree.
pub(super) struct Reclaimed {
    path: String,
    branch: String,
    removed: bool,
    branch_deleted: bool,
    kept_because: Option<String>,
}

impl Reclaimed {
    fn kept(path: &str, branch: &str, why: String) -> Self {
        Self {
            path: path.into(),
            branch: branch.into(),
            removed: false,
            branch_deleted: false,
            kept_because: Some(why),
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
    open_files: &dyn Fn(&Path) -> Result<(), String>,
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
            format!("something may still be using it ({why})"),
        ));
    }
    let remove = crate::engine::worktree::remove_worktree_argv(&path, false);
    let refs: Vec<&str> = remove.iter().map(String::as_str).collect();
    if let Err(why) = crate::harness::guard_command(&source, &refs)
        .and_then(|_| crate::engine::worktree::remove_worktree(&source, &path, false))
    {
        return Some(Reclaimed::kept(&path, &branch, why));
    }
    let delete = ["git", "branch", "-d", branch.as_str()];
    let deleted = crate::harness::guard_command(&source, &delete).and_then(|_| {
        crate::engine::git_writer::GitWriter::delete_branch(&source, &branch, false).map(|_| ())
    });
    Some(Reclaimed {
        kept_because: deleted
            .as_ref()
            .err()
            .map(|why| format!("its branch {branch} was kept: {why}")),
        branch_deleted: deleted.is_ok(),
        path,
        branch,
        removed: true,
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
    use super::{provision, slug};
    use crate::engine::git_cli::{git_global, git_text};

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
        std::fs::write(
            root.join(".gitpulse/hooks.json"),
            r#"{"worktree":{"post_create":["echo npm ci failed >&2; exit 7"]}}"#,
        )
        .unwrap();
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
