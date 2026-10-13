//! Deadbranch engine - safe stale branch cleanup, squash/rebase merge detection,
//! pre-deletion backups, and restoration for GitPulse.
//!
//! Inspired by and integrating the core safety semantics of `deadbranch` (Armen Gabrielyan):
//! 1. Fast ancestry merge detection followed by deep `git merge-tree` analysis for PR squash/rebase merges.
//! 2. Multi-tier protection: default branches, worktree-checked-out branches, protected lists, and WIP/draft patterns.
//! 3. Pre-deletion backups saved with restorable branch tips (`git branch <name> <sha>`), one
//!    directory per repository (keyed by its git common directory), and every delete recorded to
//!    GitPulse's durable WAL ledger after it runs, with its outcome.
//! 4. Restoration from those backups only, verified against the backed-up commits.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::engine::git_cli::{canonicalize_plain, git_text, validate_repo};
use crate::engine::git_reader::{default_base, GitReader};
use crate::engine::git_writer::{
    checked_out_branches, repo_mutation_lock, validate_oid, validate_ref_name, BranchDeleteJournal,
};
use crate::ledger::Outcome;

/// Largest backup file restore or listing will read. A branch costs about a
/// hundred bytes, so this holds tens of thousands; a larger file is not one
/// GitPulse wrote.
const MAX_BACKUP_BYTES: u64 = 4 * 1024 * 1024;

/// Backups one repository may take within one second before creation fails
/// loudly instead of probing names forever.
const MAX_BACKUPS_PER_SECOND: u32 = 1000;

/// Default list of branches that are never deleted.
pub const DEFAULT_PROTECTED_BRANCHES: &[&str] = &[
    "main",
    "master",
    "develop",
    "staging",
    "production",
    "trunk",
];

/// Default glob patterns for branches representing work-in-progress.
pub const DEFAULT_EXCLUDE_PATTERNS: &[&str] = &["wip/*", "draft/*", "*/draft", "*/wip"];

/// Configuration for scanning and cleaning stale branches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadbranchConfig {
    /// Age threshold in days (branches with last commit older than this count as stale). Default: 30.
    #[serde(default = "default_days_threshold")]
    pub days_threshold: u32,
    /// Branches that must never be deleted.
    #[serde(default = "default_protected_branches")]
    pub protected_branches: Vec<String>,
    /// Glob patterns for branches to exclude from cleanup (e.g. "wip/*").
    #[serde(default = "default_exclude_patterns")]
    pub exclude_patterns: Vec<String>,
    /// Whether to include remote tracking branches in the scan.
    #[serde(default)]
    pub include_remote: bool,
    /// Whether to only include branches that have been merged into the default branch.
    #[serde(default = "default_true")]
    pub merged_only: bool,
    /// Whether to perform second-pass `git merge-tree` check to detect squash/rebase merges.
    #[serde(default = "default_true")]
    pub check_squash: bool,
}

fn default_days_threshold() -> u32 {
    30
}

fn default_true() -> bool {
    true
}

fn default_protected_branches() -> Vec<String> {
    DEFAULT_PROTECTED_BRANCHES
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn default_exclude_patterns() -> Vec<String> {
    DEFAULT_EXCLUDE_PATTERNS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl Default for DeadbranchConfig {
    fn default() -> Self {
        Self {
            days_threshold: default_days_threshold(),
            protected_branches: default_protected_branches(),
            exclude_patterns: default_exclude_patterns(),
            include_remote: false,
            merged_only: true,
            check_squash: true,
        }
    }
}

/// Metadata and classification for a branch evaluated by deadbranch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StaleBranchInfo {
    /// Full branch name (e.g. "feature/login" or "origin/feature/login")
    pub name: String,
    /// Short branch name without remote prefix (e.g. "feature/login")
    pub short_name: String,
    /// Whole days since the branch tip commit
    pub age_days: i64,
    /// Age severity category: "fresh" (0-30), "moderate" (31-90), "stale" (91+)
    pub severity: String,
    /// Whether the branch is merged into the default branch (via ancestry or tree comparison)
    pub is_merged: bool,
    /// True when merge was detected via `git merge-tree` (squash-merge or rebase-merge).
    /// Such branches need `git branch -D` since git's standard ancestry-based `-d` rejects them.
    pub merged_by_tree: bool,
    /// Whether this is a remote tracking branch
    pub is_remote: bool,
    /// True if the branch matches a protected name
    pub is_protected: bool,
    /// True if the branch matches a WIP/draft pattern
    pub is_wip: bool,
    /// True if the branch is currently checked out in the main worktree or any linked worktree
    pub is_current_or_worktree: bool,
    /// SHA of the branch tip commit
    pub last_commit_sha: String,
    /// Unix timestamp (seconds) of the tip commit
    pub last_commit_timestamp: i64,
    /// Author name of the tip commit
    pub last_author: String,
    /// Subject summary of the tip commit
    pub last_summary: String,
}

/// Report summarizing a stale branch scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadbranchScanResult {
    /// Default branch name (e.g. "main") against which staleness and merges were evaluated
    pub default_branch: String,
    /// Candidate branches matching the requested filter
    pub branches: Vec<StaleBranchInfo>,
    /// Total branches examined in the repository
    pub total_scanned: usize,
    /// Total branches older than `days_threshold`
    pub stale_count: usize,
    /// Total branches that have been merged (ancestry + tree)
    pub merged_count: usize,
    /// Branches detected specifically as squash-merged or rebase-merged via tree comparison
    pub squash_merged_count: usize,
    /// Non-fatal diagnostics or warnings encountered during tree comparison
    pub warnings: Vec<String>,
}

/// Result of executing branch cleanup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadbranchCleanResult {
    /// Branch names that were successfully deleted
    pub deleted: Vec<String>,
    /// Branches that failed to delete, with the error reason
    pub failed: Vec<(String, String)>,
    /// Path to the restorable backup file created before deletion, if enabled
    pub backup_path: Option<String>,
    /// Summary message
    pub message: String,
}

/// Information about a saved branch backup file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadbranchBackupInfo {
    pub filename: String,
    pub path: String,
    pub timestamp: u64,
    pub branch_count: usize,
    pub repo_name: String,
}

/// Result of restoring branches from a backup file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadbranchRestoreResult {
    pub restored: Vec<String>,
    pub failed: Vec<(String, String)>,
    pub message: String,
}

/// Tests whether `text` matches `pattern` with simple `*` wildcard support.
pub fn matches_glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }

    let mut remaining = text;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !remaining.starts_with(part) {
                return false;
            }
            remaining = &remaining[part.len()..];
        } else if i == parts.len() - 1 {
            if !remaining.ends_with(part) {
                return false;
            }
            remaining = "";
        } else if let Some(pos) = remaining.find(part) {
            remaining = &remaining[pos + part.len()..];
        } else {
            return false;
        }
    }
    true
}

/// Classifies age in days into severity tier.
pub fn age_severity(days: i64) -> &'static str {
    match days {
        ..=30 => "fresh",
        31..=90 => "moderate",
        _ => "stale",
    }
}

/// Directory every repository's backup directory lives under.
fn backups_root() -> Result<PathBuf, String> {
    if let Ok(explicit) = std::env::var("GITPULSE_BACKUPS_DIR") {
        return Ok(PathBuf::from(explicit));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| "User home directory unavailable for backup storage".to_string())?;
    Ok(PathBuf::from(home).join(".gitpulse").join("backups"))
}

/// Where one repository's backups live.
///
/// The directory is named for the repository and keyed by a hash of its git
/// common directory: two repositories called `app` never share one, and every
/// worktree of one repository — which share its branches — does.
struct BackupLocation {
    dir: PathBuf,
    /// Before the key, backups went to `<root>/<checkout name>/`, shared by
    /// every repository of that name. Files there are read only when their
    /// header names a checkout of this repository.
    legacy_dir: PathBuf,
    common_dir: PathBuf,
    repo_name: String,
}

impl BackupLocation {
    fn of(repo: &Path) -> Result<Self, String> {
        let (_, common_dir) = crate::repository_trust::git_directories(repo)
            .map_err(|e| format!("Cannot identify the repository for its backups: {e}"))?;
        // `<repo>/.git` names the repository by its parent; a bare
        // repository's common directory is the repository itself.
        let named = if common_dir.file_name().is_some_and(|n| n == ".git") {
            common_dir.parent().unwrap_or(&common_dir)
        } else {
            &common_dir
        };
        let repo_name = file_name(named);
        let key = crate::ledger::ids::fnv1a64(common_dir.to_string_lossy().as_bytes());
        let root = backups_root()?;
        Ok(Self {
            dir: root.join(format!("{repo_name}-{key:016x}")),
            legacy_dir: root.join(file_name(repo)),
            common_dir,
            repo_name,
        })
    }

    /// Whether a file from [`Self::legacy_dir`] was written for this
    /// repository: its `# Repo path:` header must still resolve to the same
    /// git common directory.
    fn owns_legacy(&self, content: &str) -> bool {
        content
            .lines()
            .take_while(|line| line.starts_with('#') || line.trim().is_empty())
            .find_map(|line| line.strip_prefix("# Repo path:"))
            .and_then(|path| crate::repository_trust::git_directories(Path::new(path.trim())).ok())
            .is_some_and(|(_, common)| common == self.common_dir)
    }

    /// Reads `backup_path` if, and only if, it is one of this repository's
    /// backups: a `backup-*.txt` regular file directly inside its directory
    /// (or a legacy file this repository owns), no larger than
    /// [`MAX_BACKUP_BYTES`].
    fn read_contained(&self, backup_path: &str) -> Result<String, String> {
        let refuse = |why: &str| {
            format!(
                "Refusing to restore from '{backup_path}': {why}. Backups for this repository \
                 live in {}",
                self.dir.display()
            )
        };
        let file = canonicalize_plain(Path::new(backup_path))
            .map_err(|e| format!("Could not read backup file at '{backup_path}': {e}"))?;
        if !file
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_backup_file_name)
        {
            return Err(refuse("it is not a backup file"));
        }
        let parent = file.parent();
        let is_parent = |dir: &Path| canonicalize_plain(dir).ok().as_deref() == parent;
        let legacy = if is_parent(&self.dir) {
            false
        } else if is_parent(&self.legacy_dir) {
            true
        } else {
            return Err(refuse("it is not one of this repository's backups"));
        };
        let content = crate::repository_trust::bounded_text(&file, MAX_BACKUP_BYTES)
            .map_err(|e| format!("Could not read backup file at '{backup_path}': {e}"))?;
        if legacy && !self.owns_legacy(&content) {
            return Err(refuse("it was written for a different repository"));
        }
        Ok(content)
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("repository")
        .to_string()
}

fn is_backup_file_name(name: &str) -> bool {
    name.starts_with("backup-") && name.ends_with(".txt")
}

/// Resolves the backup directory for the repository at `repo`.
pub fn get_backup_dir(repo: &Path) -> Result<PathBuf, String> {
    BackupLocation::of(repo).map(|location| location.dir)
}

/// Creates `backup-<now>.txt` in `dir`, or `backup-<now>-<n>.txt` when that
/// second already has one. `create_new` makes the claim atomic, so two
/// cleans never write the same file.
fn create_unique_backup(dir: &Path, now_sec: u64) -> Result<(PathBuf, std::fs::File), String> {
    for n in 0..MAX_BACKUPS_PER_SECOND {
        let filename = if n == 0 {
            format!("backup-{now_sec}.txt")
        } else {
            format!("backup-{now_sec}-{n}.txt")
        };
        let path = dir.join(filename);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Failed to create backup file: {e}")),
        }
    }
    Err(format!(
        "Failed to create backup file: {MAX_BACKUPS_PER_SECOND} backups already exist for \
         second {now_sec} in {}",
        dir.display()
    ))
}

/// Check if a branch was squash-merged or rebase-merged into the default branch
/// via `git merge-tree --write-tree --no-messages <default_branch> <branch>`.
///
/// If merging produces the identical tree OID as `default_branch^{tree}`, all changes
/// from `branch` have already been incorporated into `default_branch`.
pub fn is_branch_merged_by_tree(
    repo: &Path,
    default_tree: &str,
    default_branch: &str,
    branch_ref: &str,
) -> Option<bool> {
    let output = git_text(
        repo,
        &[
            "merge-tree",
            "--write-tree",
            "--no-messages",
            default_branch,
            branch_ref,
        ],
    );
    match output {
        Ok(merged_tree) => {
            let tree = merged_tree.trim();
            Some(!tree.is_empty() && tree == default_tree)
        }
        Err(_) => None,
    }
}

/// Scans the repository for branches, evaluates staleness and merge status (including squash merges),
/// and applies safety protections.
pub fn scan_stale_branches(
    repo_path: &str,
    config: &DeadbranchConfig,
) -> Result<DeadbranchScanResult, String> {
    let repo = validate_repo(repo_path)?;
    let mut warnings = Vec::new();
    let checked_out = checked_out_branches(&repo).unwrap_or_else(|e| {
        warnings.push(format!(
            "Could not list worktrees ({e}); branches checked out in another worktree are not marked"
        ));
        HashSet::new()
    });
    scan_branches(repo_path, &repo, config, &checked_out, None, warnings).map(|scan| scan.result)
}

/// A scan plus the default branch it was measured against, `None` when no
/// candidate resolved (the result's display name then falls back to "main").
struct Scan {
    result: DeadbranchScanResult,
    default_branch: Option<String>,
}

/// The scan itself. `checked_out` is one `git worktree list` the caller ran;
/// `only`, when given, limits the result — and the per-branch merge-tree
/// work — to branches named there by full or short name.
fn scan_branches(
    repo_path: &str,
    repo: &Path,
    config: &DeadbranchConfig,
    checked_out: &HashSet<String>,
    only: Option<&HashSet<&str>>,
    mut warnings: Vec<String>,
) -> Result<Scan, String> {
    let all_branches = GitReader::list_branches(repo_path)?;
    let total_scanned = all_branches.len();

    // The same memoized resolution `list_branches` just used, so this costs
    // no process and cannot disagree with the branches' `is_default`.
    let base = default_base(repo).checked()?.base;
    let default_branch = base.as_ref().map(|(short, _)| short.clone());
    let now_sec = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // Resolve default branch tree OID for squash-merge detection
    let default_tree = match (&base, config.check_squash) {
        (Some((_, base_oid)), true) => {
            git_text(repo, &["rev-parse", &format!("{base_oid}^{{tree}}")])
                .ok()
                .map(|s| s.trim().to_string())
        }
        _ => None,
    };

    // First pass: collect metadata and evaluate fast ancestry merge status
    let mut candidates: Vec<StaleBranchInfo> = Vec::new();

    for branch in all_branches {
        if branch.is_remote && !config.include_remote {
            continue;
        }

        let short_name = if branch.is_remote {
            branch
                .name
                .strip_prefix("origin/")
                .unwrap_or(&branch.name)
                .to_string()
        } else {
            branch.name.clone()
        };

        // Don't treat remote HEAD pointers as candidate branches
        if short_name == "HEAD" || branch.name.ends_with("/HEAD") {
            continue;
        }

        if only.is_some_and(|only| {
            !only.contains(branch.name.as_str()) && !only.contains(short_name.as_str())
        }) {
            continue;
        }

        let is_prot = branch.is_default
            || config.protected_branches.iter().any(|p| {
                p.eq_ignore_ascii_case(&short_name) || p.eq_ignore_ascii_case(&branch.name)
            });

        let is_wip = config
            .exclude_patterns
            .iter()
            .any(|pat| matches_glob(pat, &short_name) || matches_glob(pat, &branch.name));

        let is_curr_or_wt =
            branch.is_current || (!branch.is_remote && checked_out.contains(&short_name));

        let age_days = if branch.last_commit_timestamp > 0 {
            std::cmp::max(0, (now_sec - branch.last_commit_timestamp) / 86400)
        } else {
            0
        };

        let severity = age_severity(age_days).to_string();

        // Fast ancestry merge detection:
        // commits_ahead_of_base == 0 means no commits exist on this branch that are missing from default base
        let is_ancestry_merged = (branch.commits_ahead_of_base == 0
            && branch.commits_behind_base > 0)
            || (branch.is_gone && !is_prot);

        candidates.push(StaleBranchInfo {
            name: branch.name,
            short_name,
            age_days,
            severity,
            is_merged: is_ancestry_merged,
            merged_by_tree: false,
            is_remote: branch.is_remote,
            is_protected: is_prot,
            is_wip,
            is_current_or_worktree: is_curr_or_wt,
            last_commit_sha: branch.tip_commit_id,
            last_commit_timestamp: branch.last_commit_timestamp,
            last_author: branch.last_author,
            last_summary: branch.last_summary,
        });
    }

    // Second pass: deep tree-based squash/rebase merge detection for unmerged non-protected branches
    if let (Some(d_tree), Some((_, base_oid))) = (&default_tree, &base) {
        let tree_check_errors = Arc::new(AtomicUsize::new(0));

        candidates.par_iter_mut().for_each(|branch| {
            if !branch.is_merged && !branch.is_protected && !branch.is_current_or_worktree {
                match is_branch_merged_by_tree(repo, d_tree, base_oid, &branch.name) {
                    Some(true) => {
                        branch.is_merged = true;
                        branch.merged_by_tree = true;
                    }
                    Some(false) => {}
                    None => {
                        tree_check_errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });

        let err_count = tree_check_errors.load(Ordering::Relaxed);
        if err_count > 0 {
            warnings.push(format!(
                "Squash-merge tree comparison could not resolve for {err_count} branch(es); left as unmerged"
            ));
        }
    } else if config.check_squash {
        warnings.push(format!(
            "Could not resolve tree for default branch '{}'; squash-merge detection skipped",
            default_branch.as_deref().unwrap_or("(none)")
        ));
    }

    // Compute summary metrics before final filter
    let stale_count = candidates
        .iter()
        .filter(|b| b.age_days >= config.days_threshold as i64)
        .count();
    let merged_count = candidates.iter().filter(|b| b.is_merged).count();
    let squash_merged_count = candidates.iter().filter(|b| b.merged_by_tree).count();

    // Filter candidate list based on user configuration
    let filtered: Vec<StaleBranchInfo> = candidates
        .into_iter()
        .filter(|b| {
            if config.merged_only && !b.is_merged {
                return false;
            }
            if b.age_days < config.days_threshold as i64 && !b.is_merged {
                return false;
            }
            true
        })
        .collect();

    Ok(Scan {
        result: DeadbranchScanResult {
            default_branch: default_branch.clone().unwrap_or_else(|| "main".to_string()),
            branches: filtered,
            total_scanned,
            stale_count,
            merged_count,
            squash_merged_count,
            warnings,
        },
        default_branch,
    })
}

/// Creates a restorable backup file with branch tips (`git branch <name> <sha>`)
/// in this repository's own backup directory, under a name no other backup
/// has. Nothing is journaled here: the ledger records each delete after it
/// runs (see [`clean_branches_with_gate`]).
pub fn create_backup(repo_path: &str, branches: &[StaleBranchInfo]) -> Result<String, String> {
    let repo = validate_repo(repo_path)?;
    let location = BackupLocation::of(&repo)?;
    std::fs::create_dir_all(&location.dir)
        .map_err(|e| format!("Failed to create backup directory: {e}"))?;

    let now_sec = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut content = format!(
        "# GitPulse Deadbranch backup\n\
         # Created: {}\n\
         # Repository: {}\n\
         # Repo path: {}\n\
         # Git common dir: {}\n\
         #\n\
         # To restore a branch, run the git command shown below:\n\
         #\n\n",
        now_sec,
        location.repo_name,
        repo_path,
        location.common_dir.display()
    );

    for branch in branches {
        content.push_str(&format!("# {}\n", branch.name));
        // The file tells a person to run these lines, so a name git allows but
        // a shell reads as syntax (`fix/a;b`, `x$(y)`) is quoted, and
        // [`restore_backup_with_gate`] reads the quoting back.
        content.push_str(&format!(
            "git branch {} {}\n\n",
            crate::harness::policy::shell_quote(&branch.short_name),
            branch.last_commit_sha
        ));
    }

    let (backup_path, mut file) = create_unique_backup(&location.dir, now_sec)?;
    if let Err(e) = file
        .write_all(content.as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = std::fs::remove_file(&backup_path);
        return Err(format!("Failed to write backup file: {e}"));
    }

    Ok(backup_path.to_string_lossy().to_string())
}

/// The exact `git` argv a cleanup will run for one selected branch.
///
/// Local deletes are `git branch -d/-D <short>`. A squash or rebase merge
/// uses `-D`, matching the execute path, because `-d` refuses a branch git
/// does not consider merged. Remote deletes are `git push <remote> --delete`.
pub fn clean_mutation_argv(branch: &StaleBranchInfo, force: bool) -> Result<Vec<String>, String> {
    if branch.is_remote {
        let Some((remote, remote_branch)) = branch.name.split_once('/') else {
            return Err("Invalid remote branch format".to_string());
        };
        if remote.is_empty() || remote_branch.is_empty() {
            return Err("Invalid remote branch format".to_string());
        }
        return Ok(vec![
            "git".to_string(),
            "push".to_string(),
            remote.to_string(),
            "--delete".to_string(),
            remote_branch.to_string(),
        ]);
    }
    validate_ref_name(&branch.short_name)?;
    let flag = if force || branch.merged_by_tree {
        "-D"
    } else {
        "-d"
    };
    Ok(vec![
        "git".to_string(),
        "branch".to_string(),
        flag.to_string(),
        branch.short_name.clone(),
    ])
}

/// Safely cleans the specified branches with pre-deletion backup.
///
/// Does not consult the command gate. Production callers use
/// [`clean_branches_with_gate`] so each argv is judged before any delete.
pub fn clean_branches(
    repo_path: &str,
    branch_names: &[String],
    force: bool,
    create_backup_file: bool,
) -> Result<DeadbranchCleanResult, String> {
    clean_branches_with_gate(repo_path, branch_names, force, create_backup_file, |_| {
        Ok(())
    })
}

/// Same as [`clean_branches`], but `authorize` sees every `git` argv before
/// the first delete. A refusal returns before any branch is removed. The
/// backup, when requested, is written before that check so a refused cleanup
/// still has a restorable tip list. Ledger rows are written per delete, after
/// it runs, with git's outcome — so a refused or failed clean never records a
/// deletion that did not happen.
///
/// The whole clean costs one worktree listing, one default-branch resolution
/// and one ledger address however many branches it deletes; per branch it
/// spends the delete and, without `force`, the merge-tree that picks `-d` or
/// `-D`.
pub fn clean_branches_with_gate<F>(
    repo_path: &str,
    branch_names: &[String],
    force: bool,
    create_backup_file: bool,
    mut authorize: F,
) -> Result<DeadbranchCleanResult, String>
where
    F: FnMut(&[&str]) -> Result<(), String>,
{
    let repo = validate_repo(repo_path)?;
    let _repo_lock = repo_mutation_lock(&repo);
    let _guard = _repo_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // One listing serves the scan's labels and the refusal below. Unlike the
    // read-only scan, a clean refuses when it cannot list worktrees.
    let checked_out = checked_out_branches(&repo)?;
    let target_set: HashSet<&str> = branch_names.iter().map(String::as_str).collect();

    // Rescan only the targets, for exact tips and squash-merge metadata. With
    // `force` every delete is `-D`, so the merge-tree pass would decide nothing.
    let scan = scan_branches(
        repo_path,
        &repo,
        &DeadbranchConfig {
            merged_only: false,
            days_threshold: 0,
            include_remote: true,
            check_squash: !force,
            ..Default::default()
        },
        &checked_out,
        Some(&target_set),
        Vec::new(),
    )?;
    let to_clean = scan.result.branches;

    if to_clean.is_empty() {
        return Ok(DeadbranchCleanResult {
            deleted: Vec::new(),
            failed: Vec::new(),
            backup_path: None,
            message: "No matching branches to clean".to_string(),
        });
    }

    // Safety checks: reject deleting default branch or any worktree-checked-out branch
    for b in &to_clean {
        if scan.default_branch.as_deref() == Some(b.short_name.as_str()) {
            return Err(format!(
                "Refusing to delete '{name}': it is the repository's default branch",
                name = b.short_name
            ));
        }
        if !b.is_remote && checked_out.contains(&b.short_name) {
            return Err(format!(
                "Refusing to delete '{name}': it is checked out in a linked worktree",
                name = b.short_name
            ));
        }
    }

    // Create backup file before executing mutations
    let backup_path = if create_backup_file {
        Some(create_backup(repo_path, &to_clean)?)
    } else {
        None
    };

    let mut deleted = Vec::new();
    let mut failed = Vec::new();
    let mut planned = Vec::new();

    for b in to_clean {
        match clean_mutation_argv(&b, force) {
            Ok(argv) => planned.push((b, argv)),
            Err(err) if b.is_remote => failed.push((b.name, err)),
            Err(err) => return Err(err),
        }
    }

    // Judge every line before the first delete, so a refusal cannot land
    // halfway through the batch.
    for (_, argv) in &planned {
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        authorize(&refs)?;
    }

    let journal = (!planned.is_empty()).then(|| BranchDeleteJournal::resolve(repo_path));
    for (b, argv) in planned {
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let result = git_text(&repo, &refs[1..]);
        if let Some(journal) = &journal {
            let object = if b.is_remote { &b.name } else { &b.short_name };
            let outcome = if result.is_ok() {
                Outcome::Ok
            } else {
                Outcome::Failed
            };
            journal.record(&refs, object, &b.last_commit_sha, outcome);
        }
        match result {
            Ok(_) => deleted.push(b.name),
            Err(err) => failed.push((b.name, err)),
        }
    }

    let message = format!(
        "Successfully cleaned {} branch(es){}",
        deleted.len(),
        if !failed.is_empty() {
            format!("; {} failed", failed.len())
        } else {
            String::new()
        }
    );

    Ok(DeadbranchCleanResult {
        deleted,
        failed,
        backup_path,
        message,
    })
}

/// Lists this repository's branch backups, newest first: its own backup
/// directory, plus files in the pre-key shared directory whose header names
/// this repository.
pub fn list_backups(repo_path: &str) -> Result<Vec<DeadbranchBackupInfo>, String> {
    let repo = validate_repo(repo_path)?;
    let location = BackupLocation::of(&repo)?;
    let mut backups = Vec::new();
    collect_backups(&location, &location.dir, false, &mut backups)?;
    collect_backups(&location, &location.legacy_dir, true, &mut backups)?;

    // Newest backups first; the file name breaks ties within one second.
    backups.sort_by(|a, b| (b.timestamp, &b.filename).cmp(&(a.timestamp, &a.filename)));
    Ok(backups)
}

fn collect_backups(
    location: &BackupLocation,
    dir: &Path,
    legacy: bool,
    backups: &mut Vec<DeadbranchBackupInfo>,
) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("Could not read backups directory: {e}")),
    };

    for entry in entries.flatten() {
        // Regular files only: a link here could point anywhere.
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let filename = entry.file_name().to_string_lossy().into_owned();
        if !is_backup_file_name(&filename) {
            continue;
        }
        let Ok(content) = crate::repository_trust::bounded_text(&path, MAX_BACKUP_BYTES) else {
            continue;
        };
        if legacy && !location.owns_legacy(&content) {
            continue;
        }

        let mut timestamp = 0u64;
        let mut count = 0;
        for line in content.lines() {
            if let Some(created) = line.strip_prefix("# Created:") {
                if let Ok(ts) = created.trim().parse::<u64>() {
                    timestamp = ts;
                }
            } else if line.starts_with("git branch ") {
                count += 1;
            }
        }
        if timestamp == 0 {
            // Fallback: parse from filename `backup-<ts>[-<n>].txt`
            timestamp = filename
                .trim_start_matches("backup-")
                .trim_end_matches(".txt")
                .split('-')
                .next()
                .and_then(|ts| ts.parse::<u64>().ok())
                .unwrap_or_default();
        }

        backups.push(DeadbranchBackupInfo {
            filename,
            path: path.to_string_lossy().to_string(),
            timestamp,
            branch_count: count,
            repo_name: location.repo_name.clone(),
        });
    }
    Ok(())
}

/// Restores branches from a saved backup file.
///
/// Does not consult the command gate. Production callers use
/// [`restore_backup_with_gate`].
pub fn restore_backup(
    repo_path: &str,
    backup_path: &str,
    branches_to_restore: Option<Vec<String>>,
) -> Result<DeadbranchRestoreResult, String> {
    restore_backup_with_gate(repo_path, backup_path, branches_to_restore, |_| Ok(()))
}

/// One branch a restore will recreate.
struct PlannedRestore {
    name: String,
    oid: String,
    argv: Vec<String>,
}

/// Same as [`restore_backup`], but `authorize` sees each
/// `git branch -- <name> <oid>` before the first restore.
///
/// `backup_path` must be one of this repository's backups (see
/// [`BackupLocation::read_contained`]). Each line must be exactly
/// `git branch <name> <full oid>`; any other shape fails that branch. A
/// branch counts as restored only when, afterwards, it exists at the
/// backed-up commit — git's exit status alone is never the answer.
pub fn restore_backup_with_gate<F>(
    repo_path: &str,
    backup_path: &str,
    branches_to_restore: Option<Vec<String>>,
    mut authorize: F,
) -> Result<DeadbranchRestoreResult, String>
where
    F: FnMut(&[&str]) -> Result<(), String>,
{
    let repo = validate_repo(repo_path)?;
    let _repo_lock = repo_mutation_lock(&repo);
    let _guard = _repo_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let content = BackupLocation::of(&repo)?.read_contained(backup_path)?;

    let filter_set: Option<HashSet<String>> = branches_to_restore.map(|v| v.into_iter().collect());

    let mut restored = Vec::new();
    let mut failed = Vec::new();
    let mut planned: Vec<PlannedRestore> = Vec::new();

    for line in content.lines() {
        let Some(cmd) = line.trim().strip_prefix("git branch ") else {
            continue;
        };
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        let Some(&word) = parts.first() else {
            continue;
        };
        let Some(unquoted) = unquote_backup_word(word) else {
            failed.push((
                word.to_string(),
                "Malformed backup line: the branch name's quoting is not closed".to_string(),
            ));
            continue;
        };
        let name = unquoted.as_str();

        if let Some(ref set) = filter_set {
            if !set.contains(name) {
                continue;
            }
        }

        let [_, oid] = parts[..] else {
            failed.push((
                name.to_string(),
                "Malformed backup line: expected `git branch <name> <commit>`".to_string(),
            ));
            continue;
        };
        if let Err(e) = validate_ref_name(name) {
            failed.push((name.to_string(), format!("Invalid branch name: {e}")));
            continue;
        }
        if let Err(e) = validate_full_oid(oid) {
            failed.push((name.to_string(), format!("Invalid commit in backup: {e}")));
            continue;
        }

        let oid = oid.to_ascii_lowercase();
        planned.push(PlannedRestore {
            name: name.to_string(),
            argv: vec![
                "git".to_string(),
                "branch".to_string(),
                "--".to_string(),
                name.to_string(),
                oid.clone(),
            ],
            oid,
        });
    }

    for plan in &planned {
        let refs: Vec<&str> = plan.argv.iter().map(String::as_str).collect();
        authorize(&refs)?;
    }

    let mut ran = Vec::with_capacity(planned.len());
    for plan in planned {
        let refs: Vec<&str> = plan.argv.iter().map(String::as_str).collect();
        let result = git_text(&repo, &refs[1..]);
        ran.push((plan, result));
    }

    // One listing verifies every branch, rather than a rev-parse apiece.
    let heads = if ran.is_empty() {
        Ok(HashMap::new())
    } else {
        local_branch_tips(&repo)
    };
    for (plan, result) in ran {
        let now = heads
            .as_ref()
            .map(|heads| heads.get(&plan.name).map(String::as_str));
        let at_backup = matches!(now, Ok(Some(oid)) if oid == plan.oid);
        match (result, now) {
            (Ok(_), _) if at_backup => restored.push(plan.name),
            (Ok(_), Ok(Some(oid))) => failed.push((
                plan.name,
                format!("git created the branch, but it points at {oid}, not the backed-up {}", plan.oid),
            )),
            (Ok(_), Ok(None)) => failed.push((
                plan.name,
                "git reported success, but the branch does not exist afterwards".to_string(),
            )),
            (Ok(_), Err(e)) => failed.push((
                plan.name,
                format!("git reported success, but the branch could not be verified: {e}"),
            )),
            (Err(err), _) if at_backup => failed.push((
                plan.name,
                format!("{err} (the existing branch already points at the backed-up commit; left unchanged)"),
            )),
            (Err(err), _) => failed.push((plan.name, err)),
        }
    }

    let message = format!(
        "Restored {} branch(es){}",
        restored.len(),
        if !failed.is_empty() {
            format!("; {} failed", failed.len())
        } else {
            String::new()
        }
    );

    Ok(DeadbranchRestoreResult {
        restored,
        failed,
        message,
    })
}

/// A full object id (SHA-1 or SHA-256), which is what a backup records. An
/// abbreviation could resolve to a different commit than the one backed up.
fn validate_full_oid(oid: &str) -> Result<(), String> {
    validate_oid(oid)?;
    if !matches!(oid.len(), 40 | 64) {
        return Err("Invalid commit id: expected a full object id".into());
    }
    Ok(())
}

/// The branch name in a backup line, with the quoting
/// [`crate::harness::policy::shell_quote`] wrote taken off: a bare word (every
/// backup before names were quoted) is itself, and quoted runs are joined with
/// `\'`. `None` when a quote is left open.
///
/// A ref name never contains a backslash or whitespace, so splitting the line on
/// whitespace first cannot cut a quoted name in two.
fn unquote_backup_word(word: &str) -> Option<String> {
    let mut name = String::with_capacity(word.len());
    let mut quoted = false;
    let mut chars = word.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => quoted = !quoted,
            '\\' if !quoted && chars.peek() == Some(&'\'') => {
                name.push('\'');
                chars.next();
            }
            _ => name.push(c),
        }
    }
    (!quoted).then_some(name)
}

/// Every local branch and the commit it points at, from one listing.
fn local_branch_tips(repo: &Path) -> Result<HashMap<String, String>, String> {
    let listed = git_text(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)",
            "refs/heads/",
        ],
    )?;
    Ok(listed
        .lines()
        .filter_map(|line| line.split_once('\0'))
        .filter_map(|(refname, oid)| {
            refname
                .strip_prefix("refs/heads/")
                .map(|name| (name.to_string(), oid.trim().to_ascii_lowercase()))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git_in, git_repo};

    /// `GITPULSE_BACKUPS_DIR` is process-wide; the binding holds the process's
    /// environment lock and restores the variable when the store drops, including
    /// on a failing test.
    struct BackupStore {
        dir: tempfile::TempDir,
        _env: crate::test_support::env::EnvBinding<'static>,
    }

    impl BackupStore {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let env =
                crate::test_support::env::bind_env_owned(crate::test_support::env::env_serial())
                    .set("GITPULSE_BACKUPS_DIR", dir.path());
            Self { dir, _env: env }
        }
    }

    fn rev(repo: &Path, name: &str) -> Option<String> {
        git_text(repo, &["rev-parse", "--verify", "--quiet", name])
            .ok()
            .map(|s| s.trim().to_string())
    }

    fn seeded_repo() -> (tempfile::TempDir, PathBuf) {
        let dir = git_repo();
        git_in(dir.path(), &["commit", "-q", "--allow-empty", "-m", "init"]);
        let canon = dir.path().canonicalize().unwrap();
        (dir, canon)
    }

    fn repo_named(parent: &Path, name: &str) -> PathBuf {
        let path = parent.join(name);
        std::fs::create_dir_all(&path).unwrap();
        git_in(&path, &["init", "-q", "-b", "main"]);
        git_in(&path, &["commit", "-q", "--allow-empty", "-m", "init"]);
        crate::test_support::trust_repo(&path);
        path.canonicalize().unwrap()
    }

    fn branch_delete_rows(repo: &Path) -> Vec<crate::ledger::LedgerEvent> {
        crate::ledger::tail(repo.to_str().unwrap(), 0, 1000)
            .unwrap_or_default()
            .into_iter()
            .filter(|event| {
                event
                    .detail_json
                    .as_deref()
                    .is_some_and(|d| d.contains("branch.delete"))
            })
            .collect()
    }

    /// A backup line whose "sha" is an option must never reach git as one,
    /// even when the file sits where this repository's backups live.
    #[test]
    fn restore_refuses_an_option_shaped_sha_inside_the_backup_dir() {
        let _store = BackupStore::new();
        let (_dir, repo) = seeded_repo();
        git_in(&repo, &["branch", "foo"]);
        let backup = create_backup(repo.to_str().unwrap(), &[]).unwrap();
        std::fs::write(&backup, "git branch foo -D\n").unwrap();

        let result = restore_backup(repo.to_str().unwrap(), &backup, None);
        assert!(
            rev(&repo, "refs/heads/foo").is_some(),
            "restore of 'git branch foo -D' deleted branch foo; result = {result:?}"
        );
        let result = result.expect("a bad line is a per-branch failure, not an error");
        assert!(result.restored.is_empty(), "{result:?}");
        assert_eq!(result.failed.len(), 1, "{result:?}");
    }

    /// The brief's repro: a file outside the backup directory is refused.
    #[test]
    fn restore_refuses_a_file_outside_the_backup_dir() {
        let _store = BackupStore::new();
        let (_dir, repo) = seeded_repo();
        git_in(&repo, &["branch", "foo"]);
        let elsewhere = tempfile::tempdir().unwrap();
        let file = elsewhere.path().join("backup-1.txt");
        std::fs::write(&file, "git branch foo -D\n").unwrap();

        let result = restore_backup(repo.to_str().unwrap(), file.to_str().unwrap(), None);
        assert!(rev(&repo, "refs/heads/foo").is_some(), "{result:?}");
        assert!(result.is_err(), "{result:?}");
    }

    /// Two repositories with the same basename must not share backups, and
    /// two backups in one second must not share a file.
    #[test]
    fn backups_of_same_basename_repos_do_not_mix() {
        let _store = BackupStore::new();
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let repo_a = repo_named(a.path(), "app");
        let repo_b = repo_named(b.path(), "app");

        let first = create_backup(repo_a.to_str().unwrap(), &[]).unwrap();
        let second = create_backup(repo_a.to_str().unwrap(), &[]).unwrap();
        assert_ne!(first, second, "same-second backups share a file name");
        assert!(Path::new(&first).is_file() && Path::new(&second).is_file());

        let seen_by_a = list_backups(repo_a.to_str().unwrap()).unwrap();
        let seen_by_b = list_backups(repo_b.to_str().unwrap()).unwrap();
        assert_eq!(seen_by_a.len(), 2, "{seen_by_a:?}");
        assert!(
            seen_by_b.is_empty(),
            "repo B lists repo A's backups: {seen_by_b:?}"
        );

        let crossed = restore_backup(repo_b.to_str().unwrap(), &first, None);
        assert!(
            crossed.is_err(),
            "repo B restored repo A's backup: {crossed:?}"
        );
    }

    /// Backups written before the per-repository key sit in a directory
    /// shared by every repository of that name. Each repository sees only
    /// the files whose header names it.
    #[test]
    fn legacy_backups_are_read_only_by_the_repository_they_name() {
        let store = BackupStore::new();
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let repo_a = repo_named(a.path(), "app");
        let repo_b = repo_named(b.path(), "app");
        let tip = rev(&repo_a, "HEAD").unwrap();
        let legacy = store.dir.path().join("app");
        std::fs::create_dir_all(&legacy).unwrap();
        let file = legacy.join("backup-1700000000.txt");
        std::fs::write(
            &file,
            format!(
                "# GitPulse Deadbranch backup\n# Created: 1700000000\n# Repository: app\n\
                 # Repo path: {}\n#\n\ngit branch old {tip}\n",
                repo_a.display()
            ),
        )
        .unwrap();

        let listed = list_backups(repo_a.to_str().unwrap()).unwrap();
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].timestamp, 1_700_000_000);
        assert!(list_backups(repo_b.to_str().unwrap()).unwrap().is_empty());

        let file = file.to_str().unwrap();
        assert!(restore_backup(repo_b.to_str().unwrap(), file, None).is_err());
        let restored = restore_backup(repo_a.to_str().unwrap(), file, None).unwrap();
        assert_eq!(restored.restored, vec!["old".to_string()], "{restored:?}");
        assert_eq!(rev(&repo_a, "refs/heads/old"), Some(tip));
    }

    /// Only full object ids restore, the gate sees `--` before the name, and
    /// a branch counts as restored only when it is at the backed-up commit.
    #[test]
    fn restore_reports_what_happened_to_each_branch() {
        let _store = BackupStore::new();
        let (_dir, repo) = seeded_repo();
        let first = rev(&repo, "HEAD").unwrap();
        git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "second"]);
        let second = rev(&repo, "HEAD").unwrap();
        git_in(&repo, &["branch", "taken", &second]);
        let backup = create_backup(repo.to_str().unwrap(), &[]).unwrap();
        std::fs::write(
            &backup,
            format!(
                "git branch back {first}\ngit branch short {}\ngit branch taken {first}\n\
                 git branch extra {first} --force\n",
                &first[..12]
            ),
        )
        .unwrap();

        let mut judged = Vec::new();
        let result = restore_backup_with_gate(repo.to_str().unwrap(), &backup, None, |argv| {
            judged.push(argv.join(" "));
            Ok(())
        })
        .unwrap();

        assert_eq!(
            judged,
            vec![
                format!("git branch -- back {first}"),
                format!("git branch -- taken {first}"),
            ]
        );
        assert_eq!(result.restored, vec!["back".to_string()], "{result:?}");
        let failed: Vec<&str> = result.failed.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(failed, vec!["short", "extra", "taken"], "{result:?}");
        assert_eq!(rev(&repo, "refs/heads/back"), Some(first));
        assert_eq!(rev(&repo, "refs/heads/taken"), Some(second));
        assert!(rev(&repo, "refs/heads/short").is_none());
        assert!(rev(&repo, "refs/heads/extra").is_none());
    }

    fn stale(name: &str, sha: &str) -> StaleBranchInfo {
        StaleBranchInfo {
            name: name.to_string(),
            short_name: name.to_string(),
            age_days: 400,
            severity: "stale".to_string(),
            is_merged: true,
            merged_by_tree: false,
            is_remote: false,
            is_protected: false,
            is_wip: false,
            is_current_or_worktree: false,
            last_commit_sha: sha.to_string(),
            last_commit_timestamp: 0,
            last_author: "t".to_string(),
            last_summary: "s".to_string(),
        }
    }

    /// The backup file says "run the git command shown below". A branch name
    /// git allows but a shell reads as syntax must be written quoted, so a
    /// person pasting the line cannot run part of the name; and the app's own
    /// restore must read the quoting back to the exact name.
    #[test]
    fn a_backup_quotes_names_a_shell_would_run_and_restores_them_exactly() {
        let _store = BackupStore::new();
        let (_dir, repo) = seeded_repo();
        let tip = rev(&repo, "HEAD").unwrap();
        let names = [
            "fix/a&b",
            "x;touch${IFS}owned",
            "q$(id)",
            "it's",
            "plain/ok",
        ];
        let branches: Vec<StaleBranchInfo> = names.iter().map(|n| stale(n, &tip)).collect();
        let backup = create_backup(repo.to_str().unwrap(), &branches).unwrap();

        let text = std::fs::read_to_string(&backup).unwrap();
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("git branch "))
            .collect();
        assert_eq!(lines.len(), names.len(), "{text}");
        assert!(
            lines.contains(&format!("git branch 'fix/a&b' {tip}").as_str()),
            "{text}"
        );
        assert!(lines.contains(&format!("git branch 'x;touch${{IFS}}owned' {tip}").as_str()));
        assert!(lines.contains(&format!("git branch 'q$(id)' {tip}").as_str()));
        assert!(lines.contains(&format!("git branch 'it'\\''s' {tip}").as_str()));
        assert!(lines.contains(&format!("git branch plain/ok {tip}").as_str()));

        let result =
            restore_backup_with_gate(repo.to_str().unwrap(), &backup, None, |_| Ok(())).unwrap();
        assert!(result.failed.is_empty(), "{result:?}");
        let mut restored = result.restored.clone();
        restored.sort();
        let mut expected: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        expected.sort();
        assert_eq!(restored, expected);
        for name in names {
            assert_eq!(
                rev(&repo, &format!("refs/heads/{name}")),
                Some(tip.clone()),
                "{name}"
            );
        }
    }

    #[test]
    fn an_unclosed_quote_in_a_backup_line_fails_that_line_only() {
        let _store = BackupStore::new();
        let (_dir, repo) = seeded_repo();
        let tip = rev(&repo, "HEAD").unwrap();
        let backup = create_backup(repo.to_str().unwrap(), &[]).unwrap();
        std::fs::write(
            &backup,
            format!("git branch 'open {tip}\ngit branch fine {tip}\n"),
        )
        .unwrap();
        let result =
            restore_backup_with_gate(repo.to_str().unwrap(), &backup, None, |_| Ok(())).unwrap();
        assert_eq!(result.restored, vec!["fine".to_string()], "{result:?}");
        assert_eq!(result.failed.len(), 1, "{result:?}");
        assert!(result.failed[0].1.contains("quoting"), "{result:?}");
    }

    /// A refused clean deletes nothing, so it journals no deletion.
    #[test]
    fn a_refused_clean_journals_no_deletes() {
        let _store = BackupStore::new();
        let (_dir, repo) = seeded_repo();
        git_in(&repo, &["branch", "gone"]);
        git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "ahead"]);

        let refused = clean_branches_with_gate(
            repo.to_str().unwrap(),
            &["gone".to_string()],
            false,
            true,
            |_| Err("refused by test".to_string()),
        );
        assert!(refused.is_err());
        assert!(rev(&repo, "refs/heads/gone").is_some());
        let rows = branch_delete_rows(&repo);
        assert!(rows.is_empty(), "refused clean journaled deletes: {rows:?}");
    }

    /// Each delete row carries what actually happened to that branch.
    #[test]
    fn delete_rows_carry_each_branch_outcome() {
        let (_dir, repo) = seeded_repo();
        git_in(&repo, &["branch", "merged"]);
        git_in(&repo, &["checkout", "-q", "-b", "unmerged"]);
        std::fs::write(repo.join("only-here.txt"), "work\n").unwrap();
        git_in(&repo, &["add", "only-here.txt"]);
        git_in(&repo, &["commit", "-q", "-m", "unmerged work"]);
        git_in(&repo, &["checkout", "-q", "main"]);
        git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "ahead"]);
        let unmerged_tip = rev(&repo, "refs/heads/unmerged").unwrap();

        let result = clean_branches(
            repo.to_str().unwrap(),
            &["merged".to_string(), "unmerged".to_string()],
            false,
            false,
        )
        .unwrap();
        assert_eq!(result.deleted, vec!["merged".to_string()], "{result:?}");
        assert_eq!(result.failed.len(), 1, "{result:?}");

        let rows = branch_delete_rows(&repo);
        let outcome = |name: &str| {
            rows.iter()
                .filter(|row| row.object.as_deref() == Some(name))
                .map(|row| row.outcome.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(outcome("merged"), vec!["ok"], "{rows:?}");
        assert_eq!(outcome("unmerged"), vec!["failed"], "{rows:?}");
        let failed_row = rows
            .iter()
            .find(|row| row.object.as_deref() == Some("unmerged"))
            .unwrap();
        assert_eq!(
            failed_row.before_ref.as_deref(),
            Some(unmerged_tip.as_str())
        );
    }

    /// Fifty squash-merged branches: one worktree listing and one default
    /// branch resolution for the whole clean, and no per-branch fan-out
    /// beyond the merge-tree that picks `-d`/`-D` and the delete itself.
    #[test]
    fn a_fifty_branch_clean_spawns_once_per_branch_plus_a_constant() {
        const BRANCHES: usize = 50;
        let (_dir, repo) = seeded_repo();
        let root = rev(&repo, "HEAD").unwrap();
        let tree = rev(&repo, "HEAD^{tree}").unwrap();
        let mut names = Vec::new();
        for i in 0..BRANCHES {
            let name = format!("feature/b{i:02}");
            let commit = git_text(&repo, &["commit-tree", &tree, "-p", &root, "-m", &name])
                .unwrap()
                .trim()
                .to_string();
            git_in(
                &repo,
                &["update-ref", &format!("refs/heads/{name}"), &commit],
            );
            names.push(name);
        }

        let spawned = || crate::engine::git_cli::spawn_log::spawns_in(&repo);
        let before = spawned().len();
        let result = clean_branches(repo.to_str().unwrap(), &names, false, true);
        let used: Vec<Vec<String>> = spawned()[before..].to_vec();
        let result = result.unwrap();
        assert_eq!(result.deleted.len(), BRANCHES, "{result:?}");

        let count = |pred: &dyn Fn(&Vec<String>) -> bool| used.iter().filter(|a| pred(a)).count();
        let tail = |a: &Vec<String>, end: &[&str]| {
            a.iter().rev().zip(end.iter().rev()).all(|(x, y)| x == y) && a.len() >= end.len()
        };
        // Two listings, both `-z` now that every reader of the worktree list
        // goes through `worktree::read_worktree_entries`: the clean's own,
        // behind its labels and its refusal, and the ledger authenticating the
        // worktree family for its rows. Still one each, whatever the number of
        // branches.
        let worktree_lists = count(&|a| tail(a, &["worktree", "list", "--porcelain", "-z"]));
        let any_worktree = count(&|a| a.iter().any(|x| x == "worktree"));
        let default_lookups = count(&|a| a.iter().any(|x| x == "refs/remotes/origin/HEAD"));
        let merge_trees = count(&|a| a.iter().any(|x| x == "merge-tree"));
        let deletes = count(&|a| a.iter().any(|x| x == "-D" || x == "-d"));
        assert_eq!(worktree_lists, 2, "worktree listings: {used:#?}");
        assert_eq!(any_worktree, 2, "no other worktree listing: {used:#?}");
        assert_eq!(default_lookups, 1, "default-branch lookups: {used:#?}");
        assert_eq!(merge_trees, BRANCHES, "merge-tree per target: {used:#?}");
        assert_eq!(deletes, BRANCHES);
        const CONSTANT: usize = 16;
        assert!(
            used.len() <= 2 * BRANCHES + CONSTANT,
            "{} spawns for {BRANCHES} branches: {used:#?}",
            used.len()
        );
    }
}
