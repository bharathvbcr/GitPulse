//! Which engine answers a working-tree secrets scan.
//!
//! The in-process scanner (`crate::leak_scan::worktree`) is the primary one:
//! it needs no binary, starts no process, and reports every input it could not
//! read. Kingfisher is its second opinion, run only when a person asks for a
//! scan (Rescan) and it is installed: it recognises many more credential
//! shapes, at the cost of a process start. Its findings are merged into the
//! primary's, and whatever happens to it — failure, kill, deadline, truncated
//! output — the primary result stands and the report says the second opinion
//! was unavailable, and why.

use super::parse::{
    ScanCompleteness, SecondOpinion, SecretFinding, SecretLocation, SecretSkip, SecretsReport,
};
use super::report::{rank, MAX_FINDINGS};
use super::run::{resolve_kingfisher, scan_with_binary, ScanTicket, SCAN_DEADLINE};
use crate::engine::git_cli::validate_repo;
use crate::ipc_error::CommandError;
use crate::leak_scan::worktree::{scan_worktree, WorktreeLimits, WorktreeScan};
use crate::leak_scan::{ENGINE, MAX_REPORTED_SKIPS};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Scan the working tree of `repo_path` with the in-process scanner and, when
/// `second_opinion` is asked for and Kingfisher is installed, with Kingfisher.
pub fn scan_working_tree(
    repo_path: &str,
    second_opinion: bool,
) -> Result<SecretsReport, CommandError> {
    scan_working_tree_with(repo_path, second_opinion, resolve_kingfisher)
}

/// [`scan_working_tree`] with the Kingfisher lookup injected, so a stub binary
/// can stand in for it without touching `PATH`.
pub(super) fn scan_working_tree_with(
    repo_path: &str,
    second_opinion: bool,
    resolve: impl FnOnce() -> Option<PathBuf>,
) -> Result<SecretsReport, CommandError> {
    // Validate before anything else: a request the trust gate refuses must
    // not claim a scan slot or read a file.
    let repo = validate_repo(repo_path)?;
    // Finding the binary stats directories; it never starts it.
    let binary = resolve();
    let mut report = scan_primary(&repo, WorktreeLimits::DEFAULT, binary.is_some());
    // A primary that failed has nothing to add to, and an automatic scan
    // starts no process (exec start is over a second on a loaded host).
    let (true, true, Some(binary)) = (second_opinion, report.ok, binary) else {
        return Ok(report);
    };
    // Claimed after the primary scan, and only for a run that will happen, so
    // a refused request or a missing binary cannot cancel a running scan.
    let ticket = ScanTicket::claim();
    let second = scan_with_binary(&repo, &binary, ticket, SCAN_DEADLINE);
    merge_second_opinion(&mut report, second);
    Ok(report)
}

/// Fold Kingfisher's report into the primary's.
///
/// A Kingfisher row at a (path, line) the primary already reported is the same
/// finding found twice: one row stays and `confirmed` counts it. Every other
/// row is added. A Kingfisher that did not finish changes nothing but the
/// `second_opinion` field.
pub(super) fn merge_second_opinion(primary: &mut SecretsReport, second: SecretsReport) {
    if !second.ok {
        primary.second_opinion = SecondOpinion::Unavailable {
            reason: second
                .error
                .unwrap_or_else(|| "kingfisher did not finish".to_string()),
        };
        return;
    }
    let known: HashSet<(String, u32)> = primary
        .findings
        .iter()
        .filter_map(|f| f.line.map(|line| (f.path.clone(), line)))
        .collect();
    let listed = second.findings.len();
    let (mut confirmed, mut added) = (0u32, 0u32);
    for finding in second.findings {
        let seen = finding
            .line
            .is_some_and(|line| known.contains(&(finding.path.clone(), line)));
        if seen {
            confirmed += 1;
        } else {
            primary.findings.push(finding);
            added += 1;
        }
    }
    // Rows that were counted but cannot be listed, on either side, cannot be
    // matched: Kingfisher rows past its own cap, and Kingfisher rows that
    // match a primary hit past the in-process cap (`findings_truncated` was
    // already set), are counted as additions. Either way the union total is an
    // upper bound, never below the rows listed, and the list is marked cut.
    let unlisted = (second.findings_total as usize).saturating_sub(listed);
    let unlisted = u32::try_from(unlisted).unwrap_or(u32::MAX);
    primary.findings.sort_by(|a, b| {
        rank(a.location)
            .cmp(&rank(b.location))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.rule_id.cmp(&b.rule_id))
    });
    let dropped = primary.findings.len() > MAX_FINDINGS;
    primary.findings.truncate(MAX_FINDINGS);
    primary.findings_total = primary
        .findings_total
        .saturating_add(added)
        .saturating_add(unlisted);
    primary.findings_unreadable = primary
        .findings_unreadable
        .saturating_add(second.findings_unreadable);
    primary.findings_truncated |= second.findings_truncated || unlisted > 0 || dropped;
    if added > 0 {
        primary.git_status_known &= second.git_status_known;
    }
    primary.kingfisher_version = second.kingfisher_version.clone();
    primary.kingfisher_version_rejected = second.kingfisher_version_rejected;
    primary.duration_ms = primary.duration_ms.saturating_add(second.duration_ms);
    primary.scanned_at_ms = primary.scanned_at_ms.max(second.scanned_at_ms);
    primary.second_opinion = SecondOpinion::Ran {
        completeness: second.completeness,
        version: second.kingfisher_version,
        findings_total: second.findings_total,
        confirmed,
        added,
    };
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
            report.second_opinion = not_asked(kingfisher_present);
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
    let nested_unread = !nested_repos.is_empty();
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
        // Every skip is a file or a stretch of the tree that was not read,
        // and so is every nested repository: it is named, never entered.
        completeness: if tally.skipped_total == 0 && !nested_unread {
            ScanCompleteness::Complete
        } else {
            ScanCompleteness::Partial
        },
        findings,
        findings_total: u32::try_from(tally.hits_total).unwrap_or(u32::MAX),
        findings_unreadable: 0,
        // The tally counts every hit and keeps only the first
        // `MAX_REPORTED_HITS` rows.
        findings_truncated: tally.hits_total > tally.hits.len() as u64,
        git_status_known: true,
        max_file_size_mb: u32::try_from(limits.file_bytes / (1024 * 1024)).unwrap_or(u32::MAX),
        scanned_at_ms: now_ms(),
        duration_ms,
        skipped_total: u32::try_from(tally.skipped_total).unwrap_or(u32::MAX),
        skipped,
        second_opinion: not_asked(kingfisher_present),
    }
}

/// The second opinion of a scan that has not asked Kingfisher yet.
fn not_asked(kingfisher_present: bool) -> SecondOpinion {
    if kingfisher_present {
        SecondOpinion::NotRun
    } else {
        SecondOpinion::NotInstalled
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
