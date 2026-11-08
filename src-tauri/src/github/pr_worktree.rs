//! Open a pull request in a new linked worktree.
//!
//! `gh pr checkout --worktree` creates the worktree itself, so it would skip
//! the post-create hooks and the copy-on-write cache copy every other added
//! worktree gets. This fetches `refs/pull/N/head` (advertised for same-repo
//! and fork pull requests) onto a staging ref, then adds the worktree through
//! [`crate::engine::worktree::add_worktree_configured`]. The local branch is
//! `pr/N`, never the head ref name: a fork whose head is `main` must not
//! become this repository's `main`.

use super::{
    bounded_error, gh_repo_flags, gh_unavailable_reason, pick_named_github_remote, probe_gh_cli,
    GitHubRepoRef,
};
use crate::engine::git_cli::{git_text, run_command_in, validate_repo};
use crate::engine::git_writer::validate_ref_name;
use crate::engine::worktree::{
    add_worktree_argv_configured, add_worktree_configured, parse_worktree_list_porcelain,
    prune_worktree_argv, remove_worktree_argv, resolve_worktree_path_for_branch,
    worktree_list_porcelain_argv, WorktreeCreated,
};
use crate::ipc_error::CommandError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Why opening a pull request worktree stopped before anything was left behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrWorktreeRefusalCode {
    GhMissing,
    InvalidNumber,
    FetchFailed,
    TargetExists,
    BranchCheckedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrWorktreeRefusal {
    pub code: PrWorktreeRefusalCode,
    pub message: String,
}

/// A typed refusal, or a failure that already carries its own command error
/// (repository trust, an invalid ref). Refusals stay typed until the command
/// edge, which sends only the message.
#[derive(Debug)]
pub enum PrWorktreeError {
    Refused(PrWorktreeRefusal),
    Command(CommandError),
}

impl From<PrWorktreeError> for CommandError {
    fn from(error: PrWorktreeError) -> Self {
        match error {
            PrWorktreeError::Command(error) => error,
            PrWorktreeError::Refused(refusal) => CommandError::plain(refusal.message),
        }
    }
}

/// A pull request checked out in its own worktree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrWorktreeOpened {
    pub path: String,
    pub branch: String,
    pub reused: bool,
    pub head_oid: String,
    pub cross_repository: bool,
    pub head_ref: String,
    pub head_owner: String,
    /// Present when this call created the worktree. Absent when an existing
    /// worktree for `pr/N` was reused; hooks and cache copies do not re-run.
    pub created: Option<WorktreeCreated>,
}

/// `gh pr view` fields this opener reads. The list is the wire contract.
#[derive(Debug, Deserialize)]
struct GhPrWorktreeView {
    number: u64,
    #[serde(rename = "isCrossRepository")]
    is_cross_repository: bool,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "headRepositoryOwner")]
    head_repository_owner: Option<GhPrHeadOwner>,
}

#[derive(Debug, Deserialize)]
struct GhPrHeadOwner {
    login: String,
}

/// The exact argv of the `gh pr view` this opener runs, program name included.
pub fn pr_worktree_view_argv(remote: &GitHubRepoRef, number: u64) -> Vec<String> {
    let mut args = vec![
        "gh".to_string(),
        "pr".to_string(),
        "view".to_string(),
        number.to_string(),
        "--json".to_string(),
        "number,isCrossRepository,headRefName,headRepositoryOwner".to_string(),
    ];
    args.extend(gh_repo_flags(remote));
    args
}

/// Judges one command line. A refusal is a [`String`] so the command gate's
/// error passes through unchanged; the caller records the verdict.
type Gate<'a> = &'a dyn Fn(&str, &[&str]) -> Result<(), String>;

enum StepError {
    Gate(String),
    Git(CommandError),
}

struct Attempt {
    repo: PathBuf,
    repo_key: String,
    target: String,
    branch: String,
    staging: String,
    add_started: bool,
    created_branch: bool,
    fetched_staging: bool,
}

/// Opens pull request `number` in a new worktree of `repo_path`.
///
/// `gate` judges every git and gh command this runs, in order, before it
/// starts. The `gh --version` probe is the exception, matching every other
/// GitHub command: a missing CLI is not a command to judge.
pub fn open_pull_request_worktree(
    repo_path: &str,
    number: u64,
    cow_caches: bool,
    run_hooks: bool,
    gate: Gate<'_>,
) -> Result<PrWorktreeOpened, PrWorktreeError> {
    if number == 0 {
        return Err(refused(
            PrWorktreeRefusalCode::InvalidNumber,
            "Invalid pull request number",
        ));
    }
    if let Some(reason) = gh_unavailable_reason(&probe_gh_cli()) {
        return Err(refused(PrWorktreeRefusalCode::GhMissing, reason));
    }

    let repo = validate_repo(repo_path).map_err(PrWorktreeError::Command)?;
    let repo_key = utf8_path(&repo, "repository path")?;
    let remote_text = judged_git(&repo, &repo_key, &["remote", "-v"], gate).map_err(step_error)?;
    let named = pick_named_github_remote(&remote_text).ok_or_else(|| {
        refused(
            PrWorktreeRefusalCode::FetchFailed,
            "No GitHub remote configured",
        )
    })?;

    let branch = format!("pr/{number}");
    validate_ref_name(&branch).map_err(PrWorktreeError::Command)?;
    let target_buf = resolve_worktree_path_for_branch(&repo_key, &branch, None)
        .map_err(PrWorktreeError::Command)?;
    let target = utf8_path(&target_buf, "worktree path")?;
    let branch_ref = format!("refs/heads/{branch}");
    let staging = format!("refs/gitpulse/pr-heads/{number}");

    // Judge the shared porcelain argv, then run that same line. The parser
    // decides reuse from what it prints.
    let list_argv = worktree_list_porcelain_argv();
    judge(&repo_key, &list_argv, gate)?;
    let list_args: Vec<&str> = list_argv.iter().skip(1).map(String::as_str).collect();
    let listing = git_text(&repo, &list_args).map_err(PrWorktreeError::Command)?;
    let listed = parse_worktree_list_porcelain(&listing);
    if listed.iter().any(|entry| {
        entry.branch.as_deref() == Some(branch_ref.as_str())
            && same_path(Path::new(&entry.path), &repo)
    }) {
        return Err(refused(
            PrWorktreeRefusalCode::BranchCheckedOut,
            format!("Branch {branch} is already checked out in this worktree"),
        ));
    }
    if let Some(existing) = listed
        .iter()
        .find(|entry| entry.branch.as_deref() == Some(branch_ref.as_str()))
    {
        return reuse_worktree(
            &repo,
            &repo_key,
            &named.name,
            &named.github,
            number,
            &branch,
            existing.path.clone(),
            gate,
        );
    }

    // An existing path is the caller's. Refuse before `gh pr view` and before
    // any fetch so a retry finds the directory and the log untouched.
    if std::fs::symlink_metadata(&target_buf).is_ok() {
        return Err(refused(
            PrWorktreeRefusalCode::TargetExists,
            format!("Worktree path already exists: {target}"),
        ));
    }

    let view = view_pull_request(&repo, &repo_key, &named.github, number, gate)?;
    let existed = ref_exists(&repo, &repo_key, &branch_ref, gate)?;
    let mut attempt = Attempt {
        repo: repo.clone(),
        repo_key: repo_key.clone(),
        target: target.clone(),
        branch: branch.clone(),
        staging: staging.clone(),
        add_started: false,
        created_branch: false,
        fetched_staging: false,
    };

    let (new_branch, start): (Option<&str>, Option<&str>) = if existed {
        // Attach the branch where it is. The fast-forward runs after the
        // worktree exists, and only inside that worktree, so a refused add
        // cannot move this branch.
        (None, Some(branch.as_str()))
    } else {
        fetch_staging(&repo, &repo_key, &named.name, number, &staging, gate)?;
        attempt.fetched_staging = true;
        // The branch does not exist yet. Rollback may delete it only when
        // its tip is still the staging commit this call fetched.
        attempt.created_branch = true;
        (Some(branch.as_str()), Some(staging.as_str()))
    };

    let argv = add_worktree_argv_configured(&target, new_branch, start, false, false);
    if let Err(error) = judge(&repo_key, &argv, gate) {
        return Err(rollback(&attempt, error, gate));
    }
    attempt.add_started = true;
    let mut refuse_sparse = |args: &[&str]| -> Result<(), CommandError> {
        Err(CommandError::plain(format!(
            "pull-request worktrees are full checkouts; refused git {}",
            args.join(" ")
        )))
    };
    let created = match add_worktree_configured(
        &repo_key,
        &target,
        new_branch,
        start,
        false,
        cow_caches,
        run_hooks,
        false,
        None,
        &mut refuse_sparse,
    ) {
        Ok(created) => created,
        Err(error) => return Err(rollback(&attempt, PrWorktreeError::Command(error), gate)),
    };
    if existed {
        let worktree = PathBuf::from(&target);
        if let Err(error) =
            fast_forward_checked_out(&worktree, &repo_key, &named.name, number, gate)
        {
            return Err(rollback(&attempt, error, gate));
        }
    }
    // The worktree is the result, including a hook that failed after the add.
    // A refused cleanup must not spawn git and must not fail the open.
    let _ = delete_ref(
        &repo,
        &repo_key,
        &staging,
        &["update-ref", "-d", &staging],
        gate,
    );
    let head_oid = head_oid(&repo, &repo_key, &branch_ref, gate)?;
    Ok(opened_from(
        target,
        branch,
        false,
        head_oid,
        &view,
        Some(created),
    ))
}

fn reuse_worktree(
    repo: &Path,
    repo_key: &str,
    remote_name: &str,
    github: &GitHubRepoRef,
    number: u64,
    branch: &str,
    path: String,
    gate: Gate<'_>,
) -> Result<PrWorktreeOpened, PrWorktreeError> {
    let view = view_pull_request(repo, repo_key, github, number, gate)?;
    // Update only this worktree. A source-only refspec writes FETCH_HEAD and
    // does not move the remote-tracking refs the main checkout uses. A gate
    // refusal stops here; this worktree already existed, so it is not removed.
    let worktree = PathBuf::from(&path);
    fast_forward_checked_out(&worktree, repo_key, remote_name, number, gate)?;
    let head_oid = head_oid(repo, repo_key, &format!("refs/heads/{branch}"), gate)?;
    Ok(opened_from(
        path,
        branch.to_string(),
        true,
        head_oid,
        &view,
        None,
    ))
}

/// Fast-forward the branch checked out in `cwd` to `pull/<number>/head`.
///
/// The fetch names only the source, so it writes `FETCH_HEAD` in `cwd` and
/// does not move remote-tracking refs. A gate refusal is returned. A rejected
/// fast-forward leaves the checked-out tip where it was.
fn fast_forward_checked_out(
    cwd: &Path,
    repo_key: &str,
    remote_name: &str,
    number: u64,
    gate: Gate<'_>,
) -> Result<(), PrWorktreeError> {
    let pull = format!("pull/{number}/head");
    match judged_git(
        cwd,
        repo_key,
        &["fetch", "--no-tags", remote_name, &pull],
        gate,
    ) {
        Err(StepError::Gate(message)) => {
            Err(PrWorktreeError::Command(CommandError::plain(message)))
        }
        Err(StepError::Git(_)) => Ok(()),
        Ok(_) => match judged_git(cwd, repo_key, &["merge", "--ff-only", "FETCH_HEAD"], gate) {
            Err(StepError::Gate(message)) => {
                Err(PrWorktreeError::Command(CommandError::plain(message)))
            }
            Err(StepError::Git(_)) | Ok(_) => Ok(()),
        },
    }
}

fn fetch_staging(
    repo: &Path,
    repo_key: &str,
    remote_name: &str,
    number: u64,
    staging: &str,
    gate: Gate<'_>,
) -> Result<(), PrWorktreeError> {
    let spec = format!("+pull/{number}/head:{staging}");
    match judged_git(
        repo,
        repo_key,
        &["fetch", "--no-tags", remote_name, &spec],
        gate,
    ) {
        Ok(_) => Ok(()),
        Err(StepError::Gate(message)) => {
            Err(PrWorktreeError::Command(CommandError::plain(message)))
        }
        Err(StepError::Git(error)) => {
            let failed = refused(
                PrWorktreeRefusalCode::FetchFailed,
                format!(
                    "Could not fetch the pull request: {}",
                    bounded_error(error.message().to_string())
                ),
            );
            if let Err(message) = delete_ref(
                repo,
                repo_key,
                staging,
                &["update-ref", "-d", staging],
                gate,
            ) {
                return Err(cleanup_refused(failed, message));
            }
            Err(failed)
        }
    }
}

fn view_pull_request(
    repo: &Path,
    repo_key: &str,
    github: &GitHubRepoRef,
    number: u64,
    gate: Gate<'_>,
) -> Result<GhPrWorktreeView, PrWorktreeError> {
    let argv = pr_worktree_view_argv(github, number);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    gate(repo_key, &refs)
        .map_err(|message| PrWorktreeError::Command(CommandError::plain(message)))?;
    let args: Vec<&str> = refs.iter().skip(1).copied().collect();
    let stdout =
        run_command_in("gh", &args, Duration::from_secs(60), Some(repo)).map_err(|error| {
            refused(
                PrWorktreeRefusalCode::FetchFailed,
                format!(
                    "Could not read the pull request: {}",
                    bounded_error(error.message().to_string())
                ),
            )
        })?;
    let view: GhPrWorktreeView = serde_json::from_slice(&stdout).map_err(|error| {
        refused(
            PrWorktreeRefusalCode::FetchFailed,
            format!("Could not read the pull request: {error}"),
        )
    })?;
    if view.number != number {
        return Err(refused(
            PrWorktreeRefusalCode::FetchFailed,
            format!(
                "Could not read the pull request: gh reported {}, not {number}",
                view.number
            ),
        ));
    }
    Ok(view)
}

fn opened_from(
    path: String,
    branch: String,
    reused: bool,
    head_oid: String,
    view: &GhPrWorktreeView,
    created: Option<WorktreeCreated>,
) -> PrWorktreeOpened {
    PrWorktreeOpened {
        path,
        branch,
        reused,
        head_oid,
        cross_repository: view.is_cross_repository,
        head_ref: view.head_ref_name.clone(),
        head_owner: view
            .head_repository_owner
            .as_ref()
            .map(|owner| owner.login.clone())
            .unwrap_or_default(),
        created,
    }
}

fn head_oid(
    repo: &Path,
    repo_key: &str,
    branch_ref: &str,
    gate: Gate<'_>,
) -> Result<String, PrWorktreeError> {
    let text = judged_git(repo, repo_key, &["rev-parse", branch_ref], gate).map_err(step_error)?;
    let oid = text.trim();
    if oid.is_empty() {
        return Err(PrWorktreeError::Command(CommandError::plain(format!(
            "git rev-parse {branch_ref} returned no commit"
        ))));
    }
    Ok(oid.to_string())
}

fn ref_exists(
    repo: &Path,
    repo_key: &str,
    spec: &str,
    gate: Gate<'_>,
) -> Result<bool, PrWorktreeError> {
    let text = judged_git(
        repo,
        repo_key,
        &["for-each-ref", "--format=%(refname)", spec],
        gate,
    )
    .map_err(step_error)?;
    Ok(text.lines().any(|line| line.trim() == spec))
}

/// The object name of `spec`, or `None` when that ref is absent. A refused
/// gate is returned; a git failure is treated as absent so cleanup can stop
/// without spawning a deleting command it could not check.
fn ref_object(
    repo: &Path,
    repo_key: &str,
    spec: &str,
    gate: Gate<'_>,
) -> Result<Option<String>, PrWorktreeError> {
    match judged_git(
        repo,
        repo_key,
        &["for-each-ref", "--format=%(objectname)", spec],
        gate,
    ) {
        Err(StepError::Gate(message)) => {
            Err(PrWorktreeError::Command(CommandError::plain(message)))
        }
        Err(StepError::Git(_)) => Ok(None),
        Ok(text) => Ok(text.lines().find_map(|line| {
            let oid = line.trim();
            if oid.is_empty() {
                None
            } else {
                Some(oid.to_string())
            }
        })),
    }
}

fn judged_git(
    cwd: &Path,
    repo_key: &str,
    args: &[&str],
    gate: Gate<'_>,
) -> Result<String, StepError> {
    let mut owned = Vec::with_capacity(args.len() + 1);
    owned.push("git".to_string());
    owned.extend(args.iter().copied().map(str::to_string));
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    gate(repo_key, &refs).map_err(StepError::Gate)?;
    git_text(cwd, args).map_err(StepError::Git)
}

fn judge(repo_key: &str, argv: &[String], gate: Gate<'_>) -> Result<(), PrWorktreeError> {
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    gate(repo_key, &refs).map_err(|message| PrWorktreeError::Command(CommandError::plain(message)))
}

fn run_git(cwd: &Path, argv: &[String]) {
    let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
    let _ = git_text(cwd, &args);
}

/// Deletes `spec` when it is present. A refused gate returns the refusal and
/// does not spawn the deleting git. A git failure after an allowed gate is
/// ignored: the caller decides whether that refusal fails the open.
fn delete_ref(
    repo: &Path,
    repo_key: &str,
    spec: &str,
    delete_args: &[&str],
    gate: Gate<'_>,
) -> Result<(), String> {
    match judged_git(
        repo,
        repo_key,
        &["for-each-ref", "--format=%(refname)", spec],
        gate,
    ) {
        Err(StepError::Gate(message)) => return Err(message),
        Err(StepError::Git(_)) => return Ok(()),
        Ok(text) if !text.lines().any(|line| line.trim() == spec) => return Ok(()),
        Ok(_) => {}
    }
    match judged_git(repo, repo_key, delete_args, gate) {
        Err(StepError::Gate(message)) => Err(message),
        Err(StepError::Git(_)) | Ok(_) => Ok(()),
    }
}

fn rollback(attempt: &Attempt, origin: PrWorktreeError, gate: Gate<'_>) -> PrWorktreeError {
    if attempt.add_started {
        let remove = remove_worktree_argv(&attempt.target, true);
        if let Err(error) = judge(&attempt.repo_key, &remove, gate) {
            return cleanup_refused(origin, error_text(&error));
        }
        run_git(&attempt.repo, &remove);
        let target = Path::new(&attempt.target);
        if target.symlink_metadata().is_ok() && !same_path(target, &attempt.repo) {
            let _ = std::fs::remove_dir_all(target);
        }
        let prune = prune_worktree_argv();
        if let Err(error) = judge(&attempt.repo_key, &prune, gate) {
            return cleanup_refused(origin, error_text(&error));
        }
        run_git(&attempt.repo, &prune);
    }
    if attempt.created_branch {
        // Delete the branch only when it still points at the staging commit
        // this call fetched. A branch someone else created, including one
        // planted at a different commit before `git worktree add` ran, stays.
        let branch_ref = format!("refs/heads/{}", attempt.branch);
        let branch_tip = match ref_object(&attempt.repo, &attempt.repo_key, &branch_ref, gate) {
            Ok(tip) => tip,
            Err(error) => return cleanup_refused(origin, error_text(&error)),
        };
        if let Some(branch_tip) = branch_tip {
            let staging_tip =
                match ref_object(&attempt.repo, &attempt.repo_key, &attempt.staging, gate) {
                    Ok(tip) => tip,
                    Err(error) => return cleanup_refused(origin, error_text(&error)),
                };
            if staging_tip.as_deref() == Some(branch_tip.as_str()) {
                if let Err(message) = delete_ref(
                    &attempt.repo,
                    &attempt.repo_key,
                    &branch_ref,
                    &["branch", "-D", &attempt.branch],
                    gate,
                ) {
                    return cleanup_refused(origin, message);
                }
            }
        }
    }
    if attempt.fetched_staging {
        if let Err(message) = delete_ref(
            &attempt.repo,
            &attempt.repo_key,
            &attempt.staging,
            &["update-ref", "-d", &attempt.staging],
            gate,
        ) {
            return cleanup_refused(origin, message);
        }
    }
    origin
}

fn refused(code: PrWorktreeRefusalCode, message: impl Into<String>) -> PrWorktreeError {
    PrWorktreeError::Refused(PrWorktreeRefusal {
        code,
        message: message.into(),
    })
}

fn step_error(error: StepError) -> PrWorktreeError {
    match error {
        StepError::Gate(message) => PrWorktreeError::Command(CommandError::plain(message)),
        StepError::Git(error) => PrWorktreeError::Command(error),
    }
}

fn cleanup_refused(origin: PrWorktreeError, gate_message: String) -> PrWorktreeError {
    PrWorktreeError::Command(CommandError::plain(format!(
        "{}; cleanup was refused: {gate_message}",
        error_text(&origin)
    )))
}

fn error_text(error: &PrWorktreeError) -> String {
    match error {
        PrWorktreeError::Refused(refusal) => refusal.message.clone(),
        PrWorktreeError::Command(error) => error.message().to_string(),
    }
}

fn utf8_path(path: &Path, what: &str) -> Result<String, PrWorktreeError> {
    path.to_str().map(str::to_string).ok_or_else(|| {
        PrWorktreeError::Command(CommandError::plain(format!("{what} is not UTF-8")))
    })
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn refusal(error: &PrWorktreeError) -> &PrWorktreeRefusal {
        match error {
            PrWorktreeError::Refused(refusal) => refusal,
            PrWorktreeError::Command(error) => {
                panic!("expected a typed refusal, got {}", error.message())
            }
        }
    }

    #[test]
    fn view_argv_pins_the_host_inside_repo_and_never_passes_hostname() {
        let ghes = GitHubRepoRef {
            host: "acme.ghe.com".into(),
            owner: "o".into(),
            name: "r".into(),
        };
        let argv = pr_worktree_view_argv(&ghes, 7);
        assert_eq!(
            argv,
            [
                "gh",
                "pr",
                "view",
                "7",
                "--json",
                "number,isCrossRepository,headRefName,headRepositoryOwner",
                "--repo",
                "acme.ghe.com/o/r",
            ]
        );
        let dotcom = GitHubRepoRef {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "gitpulse".into(),
        };
        let argv = pr_worktree_view_argv(&dotcom, 8);
        assert!(argv
            .windows(2)
            .any(|pair| pair == ["--repo", "acme/gitpulse"]));
        assert!(!argv.iter().any(|arg| arg == "--hostname"));
    }

    #[test]
    fn a_zero_pull_request_number_is_refused_before_any_command() {
        let calls = AtomicUsize::new(0);
        let gate = |_: &str, _: &[&str]| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        let error =
            open_pull_request_worktree("/tmp/gitpulse-pr-worktree-zero", 0, false, false, &gate)
                .expect_err("zero is not a pull request");
        let refusal = refusal(&error);
        assert_eq!(refusal.code, PrWorktreeRefusalCode::InvalidNumber);
        assert_eq!(refusal.message, "Invalid pull request number");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_missing_gh_refuses_before_git_and_leaves_nothing() {
        let serial = crate::harness::sidecar::test_serial();
        let _env = crate::test_support::env::bind_env(&serial)
            .invalidating(|| super::super::GH_PROBES.clear());
        let repo = crate::test_support::git_repo();
        let repo_buf = repo.path().canonicalize().expect("canonical repo");
        let repo_s = repo_buf.to_str().expect("utf8").to_string();
        let calls = AtomicUsize::new(0);
        crate::engine::git_cli::with_forced_spawn_failure_of("--version", || {
            super::super::GH_PROBES.clear();
            let gate = |_: &str, _: &[&str]| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            };
            let error = open_pull_request_worktree(&repo_s, 7, false, false, &gate)
                .expect_err("gh must be refused");
            let refusal = refusal(&error);
            assert_eq!(refusal.code, PrWorktreeRefusalCode::GhMissing);
            assert!(
                refusal.message.contains("GitHub CLI"),
                "{}",
                refusal.message
            );
        });
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(
            git_text(
                &repo_buf,
                &["show-ref", "--verify", "--quiet", "refs/heads/pr/7"]
            )
            .is_err(),
            "a missing gh must not create the branch"
        );
    }

    fn real_gate(repo: &str, argv: &[&str]) -> Result<(), String> {
        crate::harness::guard_command(repo, argv).map(|_| ())
    }

    fn open_at(
        repo: &str,
        number: u64,
        cow_caches: bool,
        run_hooks: bool,
    ) -> Result<PrWorktreeOpened, PrWorktreeError> {
        open_pull_request_worktree(repo, number, cow_caches, run_hooks, &real_gate)
    }

    #[cfg(unix)]
    fn install_fake_gh(dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let log = dir.join("argv.log");
        let script = r#"#!/bin/sh
echo "$*" >> '__LOG__'
case "$1" in
  --version) echo 'gh version 2.102.0'; exit 0 ;;
esac
for a in "$@"; do
  if [ "$a" = "--hostname" ]; then
    echo 'unknown flag: --hostname' >&2
    exit 1
  fi
done
num=""
prev=""
for a in "$@"; do
  if [ "$prev" = "view" ]; then
    num="$a"
  fi
  prev="$a"
done
case "$num" in
  7) printf '%s\n' '{"number":7,"isCrossRepository":false,"headRefName":"feature","headRepositoryOwner":{"login":"acme"}}' ;;
  8) printf '%s\n' '{"number":8,"isCrossRepository":true,"headRefName":"main","headRepositoryOwner":{"login":"contrib"}}' ;;
  9) printf '%s\n' '{"number":9,"isCrossRepository":false,"headRefName":"feature","headRepositoryOwner":{"login":"acme"}}' ;;
  99) printf '%s\n' '{"number":99,"isCrossRepository":false,"headRefName":"missing","headRepositoryOwner":{"login":"acme"}}' ;;
  *) echo 'no such pull request' >&2; exit 1 ;;
esac
"#
        .replace("__LOG__", &log.display().to_string());
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let gh = bin.join("gh");
        std::fs::write(&gh, script).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        log
    }

    /// `git remote -v` prints the `insteadOf` rewrite, so a repo-local rewrite
    /// would hide the GitHub URL the opener trusts. This stands in front of
    /// git and sends only `fetch origin` at the bare repository.
    #[cfg(unix)]
    fn install_fake_git(dir: &std::path::Path, bare: &str) {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            !bare.contains('\'') && !bare.contains('\n'),
            "bare path must be shell-safe"
        );
        let script = r#"#!/bin/sh
real=/usr/bin/git
bare='__BARE__'
seen=0
fetch=0
set -- "$@" END
while [ "$1" != "END" ]; do
  a=$1
  shift
  if [ "$seen" -eq 0 ]; then
    case "$a" in
      -c|-C)
        b=$1
        shift
        set -- "$@" "$a" "$b"
        ;;
      --)
        seen=1
        set -- "$@" "$a"
        ;;
      -*)
        set -- "$@" "$a"
        ;;
      *)
        seen=1
        if [ "$a" = "fetch" ]; then
          fetch=1
        fi
        set -- "$@" "$a"
        ;;
    esac
  elif [ "$fetch" -eq 1 ] && [ "$a" = "origin" ]; then
    set -- "$@" "$bare"
  else
    set -- "$@" "$a"
  fi
done
shift
exec "$real" "$@"
"#
        .replace("__BARE__", bare);
        let git = dir.join("bin").join("git");
        std::fs::write(&git, script).unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    struct Case {
        repo: PathBuf,
        repo_s: String,
        log: PathBuf,
        oid_a: String,
        oid_b: String,
    }

    #[cfg(unix)]
    fn with_pr_repo(remote_url: &str, dirty: bool, hooks: bool, body: impl FnOnce(&Case)) {
        let serial = crate::harness::sidecar::test_serial();
        let parent = tempfile::tempdir().expect("parent");
        let log = install_fake_gh(parent.path());
        let path = format!("{}:/usr/bin:/bin", parent.path().join("bin").display());
        let _env = crate::test_support::env::bind_env(&serial)
            .set("PATH", &path)
            .invalidating(|| super::super::GH_PROBES.clear());

        let repo = parent.path().join("repo");
        let bare = parent.path().join("origin.git");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&bare).unwrap();
        crate::test_support::git_in(&repo, &["init", "-b", "main"]);
        // `git_in` trusts its working directory after `init`. The bare remote
        // has to be that directory; passing it as an argument trusts the parent.
        crate::test_support::git_in(&bare, &["init", "--bare", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "base\n").unwrap();
        // `.devcouncil/` is where the command gate writes its ledger. This
        // repository ignores it, so the checkout status stays the user's files.
        std::fs::write(repo.join(".gitignore"), "node_modules/\n.devcouncil/\n").unwrap();
        crate::test_support::git_in(&repo, &["add", "README.md", ".gitignore"]);
        crate::test_support::git_in(&repo, &["commit", "-m", "base"]);
        let oid_a = git_text(&repo, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        let bare_s = bare.canonicalize().unwrap();
        let bare_s = bare_s.to_str().unwrap();
        install_fake_git(parent.path(), bare_s);
        crate::test_support::git_in(&repo, &["remote", "add", "origin", bare_s]);
        crate::test_support::git_in(&repo, &["push", "origin", "main"]);
        crate::test_support::git_in(&repo, &["push", "origin", "main:refs/pull/7/head"]);
        crate::test_support::git_in(&repo, &["push", "origin", "main:refs/pull/9/head"]);
        crate::test_support::git_in(&repo, &["checkout", "-b", "fork-head"]);
        std::fs::write(repo.join("FORK.md"), "fork\n").unwrap();
        crate::test_support::git_in(&repo, &["add", "FORK.md"]);
        crate::test_support::git_in(&repo, &["commit", "-m", "fork"]);
        let oid_b = git_text(&repo, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        crate::test_support::git_in(&repo, &["push", "origin", "fork-head:refs/pull/8/head"]);
        crate::test_support::git_in(&repo, &["checkout", "main"]);
        crate::test_support::git_in(&repo, &["branch", "-D", "fork-head"]);
        crate::test_support::git_in(&repo, &["remote", "set-url", "origin", remote_url]);
        crate::test_support::trust_repo(&repo);
        if hooks {
            let hooks_json = "{\"worktree\":{\"post_create\":[\"touch ran.marker\"]}}\n";
            std::fs::create_dir_all(repo.join(".gitpulse")).unwrap();
            std::fs::write(repo.join(".gitpulse/hooks.json"), hooks_json).unwrap();
            crate::engine::approve_worktree_hooks(&repo, hooks_json).expect("approve hooks");
        }
        if dirty {
            std::fs::write(repo.join("README.md"), "dirty\n").unwrap();
            std::fs::write(repo.join("staged.txt"), "staged\n").unwrap();
            crate::test_support::git_in(&repo, &["add", "staged.txt"]);
            std::fs::write(repo.join("notes.txt"), "notes\n").unwrap();
            std::fs::create_dir_all(repo.join("node_modules/pkg")).unwrap();
            std::fs::write(repo.join("node_modules/pkg/index.js"), "cached\n").unwrap();
        }
        let repo_buf = repo.canonicalize().unwrap();
        let case = Case {
            repo_s: repo_buf.to_str().unwrap().to_string(),
            repo: repo_buf,
            log,
            oid_a,
            oid_b,
        };
        assert_ne!(
            case.oid_a, case.oid_b,
            "the fork commit must differ from main"
        );
        body(&case);
    }

    #[cfg(unix)]
    struct Snap {
        head: String,
        branch: String,
        status: String,
        diff: String,
        cached: String,
        index: String,
        readme: Vec<u8>,
        staged: Vec<u8>,
        notes: Vec<u8>,
        cache: Vec<u8>,
    }

    #[cfg(unix)]
    fn snap(repo: &Path) -> Snap {
        Snap {
            head: git_text(repo, &["rev-parse", "HEAD"]).unwrap(),
            branch: git_text(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
            status: git_text(repo, &["status", "--porcelain=v1", "-uall"]).unwrap(),
            diff: git_text(repo, &["diff"]).unwrap(),
            cached: git_text(repo, &["diff", "--cached"]).unwrap(),
            index: git_text(repo, &["ls-files", "-s"]).unwrap(),
            readme: std::fs::read(repo.join("README.md")).unwrap(),
            staged: std::fs::read(repo.join("staged.txt")).unwrap(),
            notes: std::fs::read(repo.join("notes.txt")).unwrap(),
            cache: std::fs::read(repo.join("node_modules/pkg/index.js")).unwrap(),
        }
    }

    #[cfg(unix)]
    fn assert_snap(before: &Snap, after: &Snap) {
        assert_eq!(before.head, after.head);
        assert_eq!(before.branch, after.branch);
        assert_eq!(before.status, after.status);
        assert_eq!(before.diff, after.diff);
        assert_eq!(before.cached, after.cached);
        assert_eq!(before.index, after.index);
        assert_eq!(before.readme, after.readme);
        assert_eq!(before.staged, after.staged);
        assert_eq!(before.notes, after.notes);
        assert_eq!(before.cache, after.cache);
    }

    #[cfg(unix)]
    fn ref_absent(repo: &Path, spec: &str) -> bool {
        git_text(repo, &["show-ref", "--verify", "--quiet", spec]).is_err()
    }

    #[cfg(unix)]
    fn worktree_count(repo: &Path) -> usize {
        let text = git_text(repo, &["worktree", "list", "--porcelain"]).unwrap();
        parse_worktree_list_porcelain(&text).len()
    }

    #[cfg(unix)]
    #[test]
    fn opening_a_same_repo_pull_request_leaves_the_main_checkout_unchanged() {
        with_pr_repo("https://github.com/acme/gitpulse.git", true, true, |case| {
            let before = snap(&case.repo);
            let cursor = crate::ledger::latest_cursor(&case.repo_s).unwrap();
            let opened = open_at(&case.repo_s, 7, true, true).expect("open pr 7");
            assert_snap(&before, &snap(&case.repo));
            assert_eq!(opened.branch, "pr/7");
            assert!(!opened.reused);
            assert!(!opened.cross_repository);
            assert_eq!(opened.head_ref, "feature");
            assert_eq!(opened.head_owner, "acme");
            assert_eq!(opened.head_oid, case.oid_a);
            let created = opened.created.as_ref().expect("the worktree was created");
            assert!(created.hook_error.is_none(), "{:?}", created.hook_error);
            let worktree = PathBuf::from(&opened.path);
            assert_eq!(
                git_text(&worktree, &["rev-parse", "HEAD"]).unwrap().trim(),
                case.oid_a
            );
            assert_eq!(
                std::fs::read_to_string(worktree.join("README.md")).unwrap(),
                "base\n"
            );
            assert!(!worktree.join("FORK.md").exists());
            assert_eq!(
                std::fs::read_to_string(worktree.join("node_modules/pkg/index.js")).unwrap(),
                "cached\n"
            );
            assert!(
                worktree.join("ran.marker").is_file(),
                "approved post_create hook runs"
            );
            assert!(!case.repo.join("ran.marker").exists());
            assert!(ref_absent(&case.repo, "refs/heads/feature"));
            assert!(ref_absent(&case.repo, "refs/gitpulse/pr-heads/7"));
            assert_eq!(
                git_text(&case.repo, &["rev-parse", "refs/remotes/origin/main"])
                    .unwrap()
                    .trim(),
                case.oid_a
            );
            assert_eq!(worktree_count(&case.repo), 2);

            let calls = std::fs::read_to_string(&case.log).unwrap();
            assert!(
                calls.lines().any(|line| {
                    line.contains("pr view") && line.contains("--repo acme/gitpulse")
                }),
                "{calls}"
            );
            assert!(!calls.contains("--hostname"), "{calls}");
            assert!(!calls.contains("pr checkout"), "{calls}");

            let events = crate::ledger::tail(&case.repo_s, cursor, 200).unwrap();
            assert!(
                events.iter().any(|event| {
                    event.action == "git.fetch"
                        && event.outcome == "ok"
                        && event.argv_json.as_deref().is_some_and(|argv| {
                            argv.contains("pull/7/head")
                                && argv.contains("refs/gitpulse/pr-heads/7")
                        })
                }),
                "fetch was not judged: {events:?}"
            );
            assert!(
                events.iter().any(|event| {
                    event.action == "command.run"
                        && event.outcome == "ok"
                        && event.argv_json.as_deref().is_some_and(|argv| {
                            argv.contains("\"pr\"") && argv.contains("\"view\"")
                        })
                }),
                "gh pr view was not judged: {events:?}"
            );
            assert!(
                events.iter().any(|event| {
                    event.action == "git.worktree"
                        && event.outcome == "ok"
                        && event.argv_json.as_deref().is_some_and(|argv| {
                            argv.contains("\"add\"")
                                && argv.contains("\"-b\"")
                                && argv.contains("\"pr/7\"")
                        })
                }),
                "worktree add was not judged: {events:?}"
            );

            let again = open_at(&case.repo_s, 7, true, true).expect("reuse pr 7");
            assert!(again.reused);
            assert!(again.created.is_none());
            assert_eq!(again.branch, "pr/7");
            assert_eq!(again.head_oid, case.oid_a);
            assert_eq!(worktree_count(&case.repo), 2);
            assert_snap(&before, &snap(&case.repo));
            assert_eq!(
                git_text(&case.repo, &["rev-parse", "refs/remotes/origin/main"])
                    .unwrap()
                    .trim(),
                case.oid_a,
                "reusing the worktree must not move origin/main"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn a_fork_pull_request_checks_out_pr_n_at_the_fork_commit() {
        with_pr_repo(
            "https://github.com/acme/gitpulse.git",
            true,
            false,
            |case| {
                let before = snap(&case.repo);
                let opened = open_at(&case.repo_s, 8, true, false).expect("open pr 8");
                assert_snap(&before, &snap(&case.repo));
                assert_eq!(opened.branch, "pr/8");
                assert!(opened.cross_repository);
                assert_eq!(opened.head_ref, "main");
                assert_eq!(opened.head_owner, "contrib");
                assert_eq!(opened.head_oid, case.oid_b);
                assert_ne!(opened.head_oid, case.oid_a);
                let worktree = PathBuf::from(&opened.path);
                assert_eq!(
                    git_text(&worktree, &["rev-parse", "HEAD"]).unwrap().trim(),
                    case.oid_b
                );
                assert_eq!(
                    std::fs::read_to_string(worktree.join("FORK.md")).unwrap(),
                    "fork\n"
                );
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "refs/heads/main"])
                        .unwrap()
                        .trim(),
                    case.oid_a,
                    "the fork head named main must not move local main"
                );
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "refs/remotes/origin/main"])
                        .unwrap()
                        .trim(),
                    case.oid_a
                );
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn hooks_and_cache_copies_follow_the_flags() {
        with_pr_repo("https://github.com/acme/gitpulse.git", true, true, |case| {
            let opened = open_at(&case.repo_s, 9, false, false).expect("open pr 9");
            let worktree = PathBuf::from(&opened.path);
            assert!(!worktree.join("node_modules").exists());
            assert!(!worktree.join("ran.marker").exists());
            let created = opened.created.as_ref().expect("the worktree was created");
            assert!(created.hook_error.is_none(), "{:?}", created.hook_error);
            assert!(created.hook_notice.is_none(), "{:?}", created.hook_notice);
        });
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_view_or_fetch_leaves_no_branch_directory_or_staging_ref() {
        with_pr_repo(
            "https://github.com/acme/gitpulse.git",
            false,
            false,
            |case| {
                for number in [3_u64, 99] {
                    let error =
                        open_at(&case.repo_s, number, true, false).expect_err("must refuse");
                    assert_eq!(refusal(&error).code, PrWorktreeRefusalCode::FetchFailed);
                    assert!(ref_absent(&case.repo, &format!("refs/heads/pr/{number}")));
                    assert!(ref_absent(
                        &case.repo,
                        &format!("refs/gitpulse/pr-heads/{number}")
                    ));
                    let target = resolve_worktree_path_for_branch(
                        &case.repo_s,
                        &format!("pr/{number}"),
                        None,
                    )
                    .unwrap();
                    assert!(!target.exists(), "{}", target.display());
                }
                let calls = std::fs::read_to_string(&case.log).unwrap();
                assert!(
                    calls.lines().any(|line| line.contains("pr view 99")),
                    "{calls}"
                );
                assert!(
                    calls.lines().any(|line| line.contains("pr view 3")),
                    "{calls}"
                );
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_target_path_is_refused_before_fetch_and_left_in_place() {
        with_pr_repo(
            "https://github.com/acme/gitpulse.git",
            false,
            false,
            |case| {
                let target = resolve_worktree_path_for_branch(&case.repo_s, "pr/7", None).unwrap();
                std::fs::create_dir_all(&target).unwrap();
                std::fs::write(target.join("keep-me"), "keep").unwrap();
                let error = open_at(&case.repo_s, 7, true, false).expect_err("path exists");
                assert_eq!(refusal(&error).code, PrWorktreeRefusalCode::TargetExists);
                assert_eq!(
                    std::fs::read_to_string(target.join("keep-me")).unwrap(),
                    "keep"
                );
                assert!(ref_absent(&case.repo, "refs/heads/pr/7"));
                let calls = std::fs::read_to_string(&case.log).unwrap();
                assert!(
                    !calls.lines().any(|line| line.contains("pr view")),
                    "{calls}"
                );
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_branch_already_checked_out_here_is_not_moved() {
        with_pr_repo(
            "https://github.com/acme/gitpulse.git",
            false,
            false,
            |case| {
                crate::test_support::git_in(&case.repo, &["switch", "-c", "pr/7"]);
                let head = git_text(&case.repo, &["rev-parse", "HEAD"]).unwrap();
                let error = open_at(&case.repo_s, 7, true, false).expect_err("checked out");
                let refusal = refusal(&error);
                assert_eq!(refusal.code, PrWorktreeRefusalCode::BranchCheckedOut);
                assert_eq!(
                    refusal.message,
                    "Branch pr/7 is already checked out in this worktree"
                );
                assert_eq!(git_text(&case.repo, &["rev-parse", "HEAD"]).unwrap(), head);
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "--abbrev-ref", "HEAD"])
                        .unwrap()
                        .trim(),
                    "pr/7"
                );
                assert_eq!(worktree_count(&case.repo), 1);
                assert!(ref_absent(&case.repo, "refs/gitpulse/pr-heads/7"));
                let calls = std::fs::read_to_string(&case.log).unwrap();
                assert!(
                    !calls.lines().any(|line| line.contains("pr view")),
                    "{calls}"
                );
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_diverged_branch_is_attached_without_deleting_its_commits() {
        with_pr_repo(
            "https://github.com/acme/gitpulse.git",
            false,
            false,
            |case| {
                std::fs::write(case.repo.join("C.txt"), "c\n").unwrap();
                crate::test_support::git_in(&case.repo, &["add", "C.txt"]);
                crate::test_support::git_in(&case.repo, &["commit", "-m", "local tip"]);
                let oid_c = git_text(&case.repo, &["rev-parse", "HEAD"]).unwrap();
                let oid_c = oid_c.trim();
                assert_ne!(oid_c, case.oid_a);
                crate::test_support::git_in(&case.repo, &["branch", "pr/7"]);
                let opened = open_at(&case.repo_s, 7, false, false).expect("attach pr/7");
                assert!(!opened.reused);
                assert_eq!(opened.branch, "pr/7");
                assert_eq!(opened.head_oid, oid_c);
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "refs/heads/pr/7"])
                        .unwrap()
                        .trim(),
                    oid_c
                );
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "--abbrev-ref", "HEAD"])
                        .unwrap()
                        .trim(),
                    "main"
                );
                let worktree = PathBuf::from(&opened.path);
                assert_eq!(
                    git_text(&worktree, &["rev-parse", "HEAD"]).unwrap().trim(),
                    oid_c
                );
                assert_eq!(
                    std::fs::read_to_string(worktree.join("C.txt")).unwrap(),
                    "c\n"
                );
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "refs/remotes/origin/main"])
                        .unwrap()
                        .trim(),
                    case.oid_a
                );
            },
        );
    }

    /// `pr/7` sits on the current main commit. The pull-request head published
    /// on the bare remote is one commit ahead of it, so a fast-forward is
    /// possible and the main checkout can stay on `main`.
    #[cfg(unix)]
    fn publish_fast_forward(case: &Case) -> String {
        crate::test_support::git_in(&case.repo, &["branch", "pr/7"]);
        crate::test_support::git_in(&case.repo, &["checkout", "-b", "ahead"]);
        std::fs::write(case.repo.join("AHEAD.md"), "ahead\n").unwrap();
        crate::test_support::git_in(&case.repo, &["add", "AHEAD.md"]);
        crate::test_support::git_in(&case.repo, &["commit", "-m", "ahead"]);
        let ahead = git_text(&case.repo, &["rev-parse", "HEAD"]).unwrap();
        let ahead = ahead.trim().to_string();
        assert_ne!(ahead, case.oid_a);
        let bare = case.repo.parent().unwrap().join("origin.git");
        let bare_s = bare.to_str().expect("bare path is utf8");
        crate::test_support::git_in(&case.repo, &["push", bare_s, "ahead:refs/pull/7/head"]);
        crate::test_support::git_in(&case.repo, &["checkout", "main"]);
        crate::test_support::git_in(&case.repo, &["branch", "-D", "ahead"]);
        assert_eq!(
            git_text(&case.repo, &["rev-parse", "refs/heads/pr/7"])
                .unwrap()
                .trim(),
            case.oid_a
        );
        assert_eq!(
            git_text(&case.repo, &["rev-parse", "HEAD"]).unwrap().trim(),
            case.oid_a
        );
        ahead
    }

    #[cfg(unix)]
    fn refuses_message(error: &PrWorktreeError, needle: &str) {
        match error {
            PrWorktreeError::Command(error) => {
                assert!(error.message().contains(needle), "{}", error.message());
            }
            PrWorktreeError::Refused(refusal) => {
                panic!("expected a command-gate refusal, got {}", refusal.message)
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_add_does_not_fast_forward_an_existing_branch() {
        with_pr_repo(
            "https://github.com/acme/gitpulse.git",
            false,
            false,
            |case| {
                let ahead = publish_fast_forward(case);
                let gate = |repo: &str, argv: &[&str]| {
                    if argv.first() == Some(&"git")
                        && argv.get(1) == Some(&"worktree")
                        && argv.iter().any(|arg| *arg == "add")
                    {
                        return Err("worktree add refused".into());
                    }
                    real_gate(repo, argv)
                };
                let error = open_pull_request_worktree(&case.repo_s, 7, false, false, &gate)
                    .expect_err("the add gate must refuse");
                refuses_message(&error, "worktree add refused");
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "refs/heads/pr/7"])
                        .unwrap()
                        .trim(),
                    case.oid_a,
                    "a refused add must leave the existing branch at {oid}, not {ahead}",
                    oid = case.oid_a
                );
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "HEAD"]).unwrap().trim(),
                    case.oid_a
                );
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "--abbrev-ref", "HEAD"])
                        .unwrap()
                        .trim(),
                    "main"
                );
                let target = resolve_worktree_path_for_branch(&case.repo_s, "pr/7", None).unwrap();
                assert!(!target.exists(), "{}", target.display());
                assert!(ref_absent(&case.repo, "refs/gitpulse/pr-heads/7"));
                assert_eq!(worktree_count(&case.repo), 1);
                assert_ne!(ahead, case.oid_a);
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_branch_fast_forwards_only_inside_the_new_worktree() {
        with_pr_repo(
            "https://github.com/acme/gitpulse.git",
            false,
            false,
            |case| {
                let ahead = publish_fast_forward(case);
                let opened = open_at(&case.repo_s, 7, false, false).expect("fast-forward pr/7");
                assert!(!opened.reused);
                assert_eq!(opened.branch, "pr/7");
                assert_eq!(opened.head_oid, ahead);
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "refs/heads/pr/7"])
                        .unwrap()
                        .trim(),
                    ahead
                );
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "HEAD"]).unwrap().trim(),
                    case.oid_a
                );
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "--abbrev-ref", "HEAD"])
                        .unwrap()
                        .trim(),
                    "main"
                );
                assert!(!case.repo.join("AHEAD.md").exists());
                let worktree = PathBuf::from(&opened.path);
                assert_eq!(
                    git_text(&worktree, &["rev-parse", "HEAD"]).unwrap().trim(),
                    ahead
                );
                assert_eq!(
                    std::fs::read_to_string(worktree.join("AHEAD.md")).unwrap(),
                    "ahead\n"
                );
                assert!(ref_absent(&case.repo, "refs/gitpulse/pr-heads/7"));
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_fast_forward_removes_the_new_worktree_and_leaves_the_branch() {
        with_pr_repo(
            "https://github.com/acme/gitpulse.git",
            false,
            false,
            |case| {
                let _ahead = publish_fast_forward(case);
                let gate = |repo: &str, argv: &[&str]| {
                    if argv.first() == Some(&"git") && argv.get(1) == Some(&"merge") {
                        return Err("fast-forward refused".into());
                    }
                    real_gate(repo, argv)
                };
                let error = open_pull_request_worktree(&case.repo_s, 7, false, false, &gate)
                    .expect_err("the fast-forward gate must refuse");
                refuses_message(&error, "fast-forward refused");
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "refs/heads/pr/7"])
                        .unwrap()
                        .trim(),
                    case.oid_a
                );
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "--abbrev-ref", "HEAD"])
                        .unwrap()
                        .trim(),
                    "main"
                );
                let target = resolve_worktree_path_for_branch(&case.repo_s, "pr/7", None).unwrap();
                assert!(!target.exists(), "{}", target.display());
                assert_eq!(worktree_count(&case.repo), 1);
                assert!(ref_absent(&case.repo, "refs/gitpulse/pr-heads/7"));
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_add_does_not_delete_a_branch_it_did_not_create() {
        with_pr_repo(
            "https://github.com/acme/gitpulse.git",
            false,
            false,
            |case| {
                let foreign = case.oid_b.clone();
                let gate = move |repo: &str, argv: &[&str]| {
                    let adding = argv.first() == Some(&"git")
                        && argv.get(1) == Some(&"worktree")
                        && argv.iter().any(|arg| *arg == "add");
                    if adding && argv.iter().any(|arg| *arg == "pr/9") {
                        return Err("worktree add refused".into());
                    }
                    if adding && argv.iter().any(|arg| *arg == "pr/7") {
                        crate::test_support::git_in(
                            Path::new(repo),
                            &["branch", "pr/7", foreign.as_str()],
                        );
                        return Err("worktree add refused".into());
                    }
                    real_gate(repo, argv)
                };
                let missing = open_pull_request_worktree(&case.repo_s, 9, false, false, &gate)
                    .expect_err("add must refuse");
                refuses_message(&missing, "worktree add refused");
                assert!(ref_absent(&case.repo, "refs/heads/pr/9"));
                assert!(ref_absent(&case.repo, "refs/gitpulse/pr-heads/9"));
                let missing_target =
                    resolve_worktree_path_for_branch(&case.repo_s, "pr/9", None).unwrap();
                assert!(!missing_target.exists(), "{}", missing_target.display());

                let planted = open_pull_request_worktree(&case.repo_s, 7, false, false, &gate)
                    .expect_err("add must refuse");
                refuses_message(&planted, "worktree add refused");
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "refs/heads/pr/7"])
                        .unwrap()
                        .trim(),
                    case.oid_b,
                    "rollback must not delete a branch this call did not create"
                );
                assert!(ref_absent(&case.repo, "refs/gitpulse/pr-heads/7"));
                let target = resolve_worktree_path_for_branch(&case.repo_s, "pr/7", None).unwrap();
                assert!(!target.exists(), "{}", target.display());
                assert_eq!(
                    git_text(&case.repo, &["rev-parse", "HEAD"]).unwrap().trim(),
                    case.oid_a
                );
                assert_eq!(worktree_count(&case.repo), 1);
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_repository_without_a_github_remote_is_refused_before_a_worktree_exists() {
        with_pr_repo(
            "https://github.com/acme/gitpulse.git",
            false,
            false,
            |case| {
                let empty = case.repo.parent().unwrap().join("empty");
                std::fs::create_dir_all(&empty).unwrap();
                crate::test_support::git_in(&empty, &["init", "-b", "main"]);
                crate::test_support::trust_repo(&empty);
                let empty_s = empty.canonicalize().unwrap();
                let empty_s = empty_s.to_str().unwrap();
                let error = open_at(empty_s, 7, false, false).expect_err("no remote");
                let refusal = refusal(&error);
                assert_eq!(refusal.code, PrWorktreeRefusalCode::FetchFailed);
                assert_eq!(refusal.message, "No GitHub remote configured");
                assert!(ref_absent(&empty, "refs/heads/pr/7"));
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_ghes_pull_request_is_pinned_with_repo_and_never_hostname() {
        with_pr_repo("https://acme.ghe.com/o/r.git", false, false, |case| {
            let opened = open_at(&case.repo_s, 7, false, false).expect("ghes open");
            assert_eq!(opened.branch, "pr/7");
            assert_eq!(opened.head_oid, case.oid_a);
            let calls = std::fs::read_to_string(&case.log).unwrap();
            assert!(
                calls.lines().any(|line| {
                    line.contains("pr view") && line.contains("--repo acme.ghe.com/o/r")
                }),
                "{calls}"
            );
            assert!(!calls.contains("--hostname"), "{calls}");
        });
    }
}
