//! Which engine answers a working-tree secrets scan.
//!
//! The in-process scanner (`crate::leak_scan::worktree`) is the primary one:
//! it needs no binary, starts no process, and reports every input it could not
//! read. Kingfisher is the deep scan, run only when a person asks for it, and
//! the one that also reads the files git ignores. A missing Kingfisher
//! therefore costs the deep scan and nothing else — the everyday scan always
//! has an answer.

use super::parse::{ScanCompleteness, SecretFinding, SecretLocation, SecretSkip, SecretsReport};
use super::run::{
    missing_binary_report, resolve_kingfisher, scan_with_binary, ScanTicket, SCAN_DEADLINE,
};
use crate::engine::git_cli::validate_repo;
use crate::ipc_error::CommandError;
use crate::leak_scan::worktree::{scan_worktree, WorktreeLimits, WorktreeScan};
use crate::leak_scan::{ENGINE, MAX_REPORTED_SKIPS};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Scan the working tree of `repo_path`: with the in-process scanner, or with
/// Kingfisher when `deep` is asked for.
pub fn scan_working_tree(repo_path: &str, deep: bool) -> Result<SecretsReport, CommandError> {
    scan_working_tree_with(repo_path, deep, resolve_kingfisher)
}

/// [`scan_working_tree`] with the Kingfisher lookup injected, so a stub binary
/// can stand in for it without touching `PATH`.
pub(super) fn scan_working_tree_with(
    repo_path: &str,
    deep: bool,
    resolve: impl FnOnce() -> Option<PathBuf>,
) -> Result<SecretsReport, CommandError> {
    // Validate before anything else: a request the trust gate refuses must
    // not claim a scan slot or read a file.
    let repo = validate_repo(repo_path)?;
    if deep {
        // Claimed after validation so a refused request cannot cancel a
        // trusted repository's deep scan on its way out.
        let ticket = ScanTicket::claim();
        let Some(binary) = resolve() else {
            return Ok(missing_binary_report());
        };
        return Ok(scan_with_binary(&repo, &binary, ticket, SCAN_DEADLINE));
    }
    // Only whether it is installed: the panel offers the deep scan on this.
    // Finding the binary stats directories; it never starts it.
    let kingfisher_present = resolve().is_some();
    Ok(scan_primary(
        &repo,
        WorktreeLimits::DEFAULT,
        kingfisher_present,
    ))
}

/// The in-process scan, as the report the panel renders.
pub(super) fn scan_primary(
    repo: &Path,
    limits: WorktreeLimits,
    kingfisher_present: bool,
) -> SecretsReport {
    match scan_worktree(repo, limits) {
        Ok(scan) => report_from_worktree(scan, limits, kingfisher_present),
        Err(failure) => {
            let mut report = SecretsReport::failed(failure.0, None);
            report.engine = ENGINE.to_string();
            report.kingfisher_present = kingfisher_present;
            report.nested_repos_scanned = false;
            report
        }
    }
}

fn report_from_worktree(
    scan: WorktreeScan,
    limits: WorktreeLimits,
    kingfisher_present: bool,
) -> SecretsReport {
    let WorktreeScan {
        tally,
        untracked,
        nested_repos,
        duration_ms,
    } = scan;
    let mut findings: Vec<SecretFinding> = tally
        .hits
        .iter()
        .map(|hit| SecretFinding {
            // The shape name is the only identifier dc-redact has.
            rule_id: hit.rule.to_string(),
            rule_name: String::new(),
            path: hit.path.clone(),
            line: Some(hit.line),
            // dc-redact has no confidence, and no two findings are known to
            // hold the same value: both stay empty rather than invented.
            confidence: String::new(),
            location: if untracked.contains(&hit.path) {
                SecretLocation::Untracked
            } else {
                SecretLocation::Tracked
            },
            secret_group: 0,
        })
        .collect();
    // Tracked rows first: already in history, so the most actionable.
    findings.sort_by(|a, b| {
        (a.location != SecretLocation::Tracked)
            .cmp(&(b.location != SecretLocation::Tracked))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.rule_id.cmp(&b.rule_id))
    });
    let skipped: Vec<SecretSkip> = tally
        .skipped
        .iter()
        .take(MAX_REPORTED_SKIPS)
        .map(|skip| SecretSkip {
            path: skip.path.clone(),
            reason: skip.reason.describe().to_string(),
            bytes: skip.bytes,
        })
        .collect();
    SecretsReport {
        ok: true,
        engine: ENGINE.to_string(),
        error: None,
        diagnostic: None,
        kingfisher_present,
        kingfisher_version: None,
        kingfisher_version_rejected: false,
        // Nested repositories are named in `excluded_paths`, not read.
        nested_repos_scanned: false,
        excluded_paths: nested_repos,
        // Every skip is a file or a stretch of the tree that was not read.
        completeness: if tally.skipped_total == 0 {
            ScanCompleteness::Complete
        } else {
            ScanCompleteness::Partial
        },
        findings,
        findings_total: u32::try_from(tally.hits_total).unwrap_or(u32::MAX),
        findings_unreadable: 0,
        findings_truncated: false,
        git_status_known: true,
        max_file_size_mb: u32::try_from(limits.file_bytes / (1024 * 1024)).unwrap_or(u32::MAX),
        scanned_at_ms: now_ms(),
        duration_ms,
        skipped_total: u32::try_from(tally.skipped_total).unwrap_or(u32::MAX),
        skipped,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
