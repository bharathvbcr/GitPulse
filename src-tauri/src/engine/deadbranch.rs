//! Deadbranch engine - safe stale branch cleanup, squash/rebase merge detection,
//! pre-deletion backups, and restoration for GitPulse.
//!
//! Inspired by and integrating the core safety semantics of `deadbranch` (Armen Gabrielyan):
//! 1. Fast ancestry merge detection followed by deep `git merge-tree` analysis for PR squash/rebase merges.
//! 2. Multi-tier protection: default branches, worktree-checked-out branches, protected lists, and WIP/draft patterns.
//! 3. Pre-deletion backups saved with restorable branch tips (`git branch <name> <sha>`) and recorded to GitPulse's durable WAL ledger.
//! 4. Full restoration capability from backup points.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::engine::git_cli::{git_text, validate_repo};
use crate::engine::git_reader::GitReader;
use crate::engine::git_writer::{
    is_checked_out_in_any_worktree, is_default_branch, record_deleted_branch_tip,
    repo_mutation_lock, validate_ref_name,
};

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

/// Resolves the global or overridden backup directory for a given repository name.
pub fn get_backup_dir(repo_name: &str) -> Result<PathBuf, String> {
    if let Ok(explicit) = std::env::var("GITPULSE_BACKUPS_DIR") {
        let path = PathBuf::from(explicit).join(repo_name);
        return Ok(path);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| "User home directory unavailable for backup storage".to_string())?;
    Ok(PathBuf::from(home)
        .join(".gitpulse")
        .join("backups")
        .join(repo_name))
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

/// Resolves the default branch name of the repository.
pub fn resolve_default_branch(repo: &Path) -> String {
    let remote = crate::engine::git_reader::resolve_default_remote(repo);
    let head_ref = crate::engine::git_reader::remote_head_ref(&remote);
    if let Ok(sym) = git_text(repo, &["symbolic-ref", "--quiet", head_ref.as_str()]) {
        let trimmed = sym.trim();
        let prefix = format!("refs/remotes/{remote}/");
        if let Some(short) = trimmed.strip_prefix(&prefix) {
            return short.to_string();
        }
    }
    for candidate in &["main", "master", "trunk", "develop"] {
        if git_text(
            repo,
            &["rev-parse", "--verify", &format!("refs/heads/{candidate}")],
        )
        .is_ok()
        {
            return candidate.to_string();
        }
    }
    "main".to_string()
}

/// Scans the repository for branches, evaluates staleness and merge status (including squash merges),
/// and applies safety protections.
pub fn scan_stale_branches(
    repo_path: &str,
    config: &DeadbranchConfig,
) -> Result<DeadbranchScanResult, String> {
    let repo = validate_repo(repo_path)?;
    let all_branches = GitReader::list_branches(repo_path)?;
    let total_scanned = all_branches.len();

    let default_branch = resolve_default_branch(&repo);
    let now_sec = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // Resolve default branch tree OID for squash-merge detection
    let default_tree = if config.check_squash {
        git_text(&repo, &["rev-parse", &format!("{default_branch}^{{tree}}")])
            .ok()
            .map(|s| s.trim().to_string())
    } else {
        None
    };

    // First pass: collect metadata and evaluate fast ancestry merge status
    let mut candidates: Vec<StaleBranchInfo> = Vec::new();
    let mut warnings = Vec::new();

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

        let is_prot = branch.is_default
            || config.protected_branches.iter().any(|p| {
                p.eq_ignore_ascii_case(&short_name) || p.eq_ignore_ascii_case(&branch.name)
            });

        let is_wip = config
            .exclude_patterns
            .iter()
            .any(|pat| matches_glob(pat, &short_name) || matches_glob(pat, &branch.name));

        let is_curr_or_wt = branch.is_current
            || (!branch.is_remote
                && is_checked_out_in_any_worktree(&repo, &short_name).unwrap_or(false));

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
    if let Some(ref d_tree) = default_tree {
        let tree_check_errors = Arc::new(AtomicUsize::new(0));

        candidates.par_iter_mut().for_each(|branch| {
            if !branch.is_merged && !branch.is_protected && !branch.is_current_or_worktree {
                match is_branch_merged_by_tree(&repo, d_tree, &default_branch, &branch.name) {
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
            "Could not resolve tree for default branch '{default_branch}'; squash-merge detection skipped"
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

    Ok(DeadbranchScanResult {
        default_branch,
        branches: filtered,
        total_scanned,
        stale_count,
        merged_count,
        squash_merged_count,
        warnings,
    })
}

/// Creates a restorable backup file with branch tips (`git branch <name> <sha>`)
/// and journals each branch tip into GitPulse's durable WAL ledger.
pub fn create_backup(repo_path: &str, branches: &[StaleBranchInfo]) -> Result<String, String> {
    let repo = validate_repo(repo_path)?;
    let repo_name = repo
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repository");

    let backup_dir = get_backup_dir(repo_name)?;
    std::fs::create_dir_all(&backup_dir)
        .map_err(|e| format!("Failed to create backup directory: {e}"))?;

    let now_sec = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let filename = format!("backup-{now_sec}.txt");
    let backup_path = backup_dir.join(&filename);

    let mut content = format!(
        "# GitPulse Deadbranch backup\n\
         # Created: {}\n\
         # Repository: {}\n\
         # Repo path: {}\n\
         #\n\
         # To restore a branch, run the git command shown below:\n\
         #\n\n",
        now_sec, repo_name, repo_path
    );

    for branch in branches {
        content.push_str(&format!("# {}\n", branch.name));
        content.push_str(&format!(
            "git branch {} {}\n\n",
            branch.short_name, branch.last_commit_sha
        ));

        // Journal to GitPulse durable WAL ledger
        record_deleted_branch_tip(
            repo_path,
            &branch.short_name,
            &branch.last_commit_sha,
            branch.merged_by_tree,
        );
    }

    std::fs::write(&backup_path, content)
        .map_err(|e| format!("Failed to write backup file: {e}"))?;

    Ok(backup_path.to_string_lossy().to_string())
}

/// Safely cleans the specified branches with pre-deletion backup and guard verification.
pub fn clean_branches(
    repo_path: &str,
    branch_names: &[String],
    force: bool,
    create_backup_file: bool,
) -> Result<DeadbranchCleanResult, String> {
    let repo = validate_repo(repo_path)?;
    let _repo_lock = repo_mutation_lock(&repo);
    let _guard = _repo_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // Scan branches to get exact tips and squash-merge metadata
    let scan = scan_stale_branches(
        repo_path,
        &DeadbranchConfig {
            merged_only: false,
            days_threshold: 0,
            include_remote: true,
            check_squash: true,
            ..Default::default()
        },
    )?;

    let target_set: HashSet<&str> = branch_names.iter().map(String::as_str).collect();
    let to_clean: Vec<StaleBranchInfo> = scan
        .branches
        .into_iter()
        .filter(|b| {
            target_set.contains(b.name.as_str()) || target_set.contains(b.short_name.as_str())
        })
        .collect();

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
        if is_default_branch(&repo, &b.short_name) {
            return Err(format!(
                "Refusing to delete '{name}': it is the repository's default branch",
                name = b.short_name
            ));
        }
        if !b.is_remote && is_checked_out_in_any_worktree(&repo, &b.short_name)? {
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

    for b in to_clean {
        if b.is_remote {
            // For remote branches, e.g. "origin/feature" -> push origin --delete feature
            let parts: Vec<&str> = b.name.splitn(2, '/').collect();
            if parts.len() == 2 {
                let remote = parts[0];
                let remote_branch = parts[1];
                match git_text(&repo, &["push", remote, "--delete", remote_branch]) {
                    Ok(_) => deleted.push(b.name),
                    Err(err) => failed.push((b.name, err)),
                }
            } else {
                failed.push((b.name, "Invalid remote branch format".to_string()));
            }
        } else {
            // For local branches: use -D if force requested or if squash/rebase merged by tree
            let use_force = force || b.merged_by_tree;
            let flag = if use_force { "-D" } else { "-d" };

            validate_ref_name(&b.short_name)?;
            match git_text(&repo, &["branch", flag, &b.short_name]) {
                Ok(_) => deleted.push(b.name),
                Err(err) => failed.push((b.name, err)),
            }
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

/// Lists available branch backups for the given repository.
pub fn list_backups(repo_path: &str) -> Result<Vec<DeadbranchBackupInfo>, String> {
    let repo = validate_repo(repo_path)?;
    let repo_name = repo
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repository");

    let backup_dir = get_backup_dir(repo_name)?;
    if !backup_dir.is_dir() {
        return Ok(Vec::new());
    }

    let entries = std::fs::read_dir(&backup_dir)
        .map_err(|e| format!("Could not read backups directory: {e}"))?;

    let mut backups = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();

            if filename.starts_with("backup-") && filename.ends_with(".txt") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let mut timestamp = 0u64;
                    let mut count = 0;

                    for line in content.lines() {
                        if let Some(created) = line.strip_prefix("# Created:") {
                            let trimmed = created.trim();
                            if let Ok(ts) = trimmed.parse::<u64>() {
                                timestamp = ts;
                            }
                        } else if line.starts_with("git branch ") {
                            count += 1;
                        }
                    }

                    if timestamp == 0 {
                        // Fallback: parse from filename `backup-<ts>.txt`
                        let middle = filename
                            .trim_start_matches("backup-")
                            .trim_end_matches(".txt");
                        timestamp = middle.parse::<u64>().unwrap_or_default();
                    }

                    backups.push(DeadbranchBackupInfo {
                        filename,
                        path: path.to_string_lossy().to_string(),
                        timestamp,
                        branch_count: count,
                        repo_name: repo_name.to_string(),
                    });
                }
            }
        }
    }

    // Newest backups first
    backups.sort_by_key(|a| std::cmp::Reverse(a.timestamp));
    Ok(backups)
}

/// Restores branches from a saved backup file.
pub fn restore_backup(
    repo_path: &str,
    backup_path: &str,
    branches_to_restore: Option<Vec<String>>,
) -> Result<DeadbranchRestoreResult, String> {
    let repo = validate_repo(repo_path)?;
    let _repo_lock = repo_mutation_lock(&repo);
    let _guard = _repo_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let content = std::fs::read_to_string(backup_path)
        .map_err(|e| format!("Could not read backup file at '{backup_path}': {e}"))?;

    let filter_set: Option<HashSet<String>> = branches_to_restore.map(|v| v.into_iter().collect());

    let mut restored = Vec::new();
    let mut failed = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if let Some(cmd) = line.strip_prefix("git branch ") {
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            if parts.len() >= 2 {
                let name = parts[0];
                let sha = parts[1];

                if let Some(ref set) = filter_set {
                    if !set.contains(name) {
                        continue;
                    }
                }

                if let Err(e) = validate_ref_name(name) {
                    failed.push((name.to_string(), format!("Invalid branch name: {e}")));
                    continue;
                }

                match git_text(&repo, &["branch", name, sha]) {
                    Ok(_) => restored.push(name.to_string()),
                    Err(err) => failed.push((name.to_string(), err)),
                }
            }
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
