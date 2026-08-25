//! High-signal repository operations used by the MANVI Ops surface.
//!
//! Read-only planning lives here; mutations continue through `GitWriter` and
//! the command gate. Cleanup plans are conservative: only local branches Git
//! itself reports as merged into the default branch are eligible.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::analyzer::ConventionalCommitParser;
use crate::engine::git_cli::{git_text, validate_repo};
use crate::engine::git_writer::validate_ref_name;
use crate::engine::{BranchInfo, GitReader};

const COMMIT_REVIEW_LIMIT: usize = 500;
const MAX_RELEASE_MESSAGE_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchCleanupCandidate {
    pub name: String,
    pub last_summary: String,
    pub last_author: String,
    pub last_commit_timestamp: i64,
    pub upstream_gone: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchCleanupPlan {
    pub default_branch: String,
    pub current_branch: String,
    pub total_local_branches: usize,
    pub protected_branches: usize,
    pub unmerged_branches: usize,
    pub candidates: Vec<BranchCleanupCandidate>,
}

pub fn branch_cleanup_plan(repo_path: &str) -> Result<BranchCleanupPlan, String> {
    let repo = validate_repo(repo_path)?;
    let branches = GitReader::list_branches(repo_path)?;
    let local: Vec<&BranchInfo> = branches.iter().filter(|branch| !branch.is_remote).collect();
    let current = local
        .iter()
        .find(|branch| branch.is_current)
        .ok_or_else(|| "Branch cleanup requires a checked-out local branch".to_string())?;
    let default = local
        .iter()
        .find(|branch| branch.is_default)
        .ok_or_else(|| "Could not determine the repository's default branch".to_string())?;

    validate_ref_name(&default.name)?;
    let merged_output = git_text(
        &repo,
        &[
            "branch",
            "--format=%(refname:short)",
            "--merged",
            &default.name,
        ],
    )?;
    let merged: HashSet<&str> = merged_output
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    let worktree_output = git_text(&repo, &["worktree", "list", "--porcelain"])?;
    let checked_out_elsewhere: HashSet<&str> = worktree_output
        .lines()
        .filter_map(|line| line.strip_prefix("branch refs/heads/"))
        .filter(|name| *name != current.name)
        .collect();

    let mut candidates = Vec::new();
    let mut protected_branches = 0usize;
    let mut unmerged_branches = 0usize;
    for branch in &local {
        if branch.is_current
            || branch.is_default
            || checked_out_elsewhere.contains(branch.name.as_str())
        {
            protected_branches += 1;
        } else if merged.contains(branch.name.as_str()) {
            candidates.push(BranchCleanupCandidate {
                name: branch.name.clone(),
                last_summary: branch.last_summary.clone(),
                last_author: branch.last_author.clone(),
                last_commit_timestamp: branch.last_commit_timestamp,
                upstream_gone: branch.is_gone,
            });
        } else {
            unmerged_branches += 1;
        }
    }
    candidates.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(BranchCleanupPlan {
        default_branch: default.name.clone(),
        current_branch: current.name.clone(),
        total_local_branches: local.len(),
        protected_branches,
        unmerged_branches,
        candidates,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReviewSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitMessageFinding {
    pub commit_id: String,
    pub short_id: String,
    pub subject: String,
    pub severity: ReviewSeverity,
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitReviewReport {
    pub range: String,
    pub total_commits: usize,
    pub reviewed_commits: usize,
    pub truncated: bool,
    pub conventional_commits: usize,
    pub issue_linked_commits: usize,
    pub findings: Vec<CommitMessageFinding>,
}

fn finding(
    commit_id: &str,
    subject: &str,
    severity: ReviewSeverity,
    code: &str,
    detail: &str,
) -> CommitMessageFinding {
    CommitMessageFinding {
        commit_id: commit_id.to_string(),
        short_id: commit_id.chars().take(8).collect(),
        subject: subject.to_string(),
        severity,
        code: code.to_string(),
        detail: detail.to_string(),
    }
}

fn analyze_commit_messages(
    range: String,
    total_commits: usize,
    commits: &[(String, String)],
) -> CommitReviewReport {
    const STANDARD_TYPES: &[&str] = &[
        "build", "chore", "ci", "docs", "feat", "fix", "perf", "refactor", "revert", "style",
        "test",
    ];
    let parser = ConventionalCommitParser::new();
    let mut findings = Vec::new();
    let mut conventional_commits = 0usize;
    let mut issue_linked_commits = 0usize;
    let mut seen_subjects: HashMap<&str, &str> = HashMap::new();

    for (commit_id, subject) in commits {
        if subject.chars().count() > 72 {
            findings.push(finding(
                commit_id,
                subject,
                ReviewSeverity::Warning,
                "subject_too_long",
                "Subject exceeds 72 characters and may be truncated in Git tooling.",
            ));
        }

        match parser.parse(subject) {
            Some(parsed) => {
                conventional_commits += 1;
                if !STANDARD_TYPES.contains(&parsed.commit_type.as_str()) {
                    findings.push(finding(
                        commit_id,
                        subject,
                        ReviewSeverity::Warning,
                        "unknown_type",
                        "Commit uses a non-standard Conventional Commit type.",
                    ));
                }
                if parsed.description.trim().is_empty() {
                    findings.push(finding(
                        commit_id,
                        subject,
                        ReviewSeverity::Error,
                        "empty_description",
                        "Conventional Commit header has no description.",
                    ));
                }
                if parsed.issue_references.is_empty() {
                    if matches!(parsed.commit_type.as_str(), "feat" | "fix") {
                        findings.push(finding(
                            commit_id,
                            subject,
                            ReviewSeverity::Info,
                            "missing_issue_reference",
                            "Feature or fix has no issue reference.",
                        ));
                    }
                } else {
                    issue_linked_commits += 1;
                }
            }
            None => findings.push(finding(
                commit_id,
                subject,
                ReviewSeverity::Warning,
                "non_conventional",
                "Subject does not follow Conventional Commits (type(scope): description).",
            )),
        }

        if let Some(first_id) = seen_subjects.insert(subject.as_str(), commit_id.as_str()) {
            findings.push(finding(
                commit_id,
                subject,
                ReviewSeverity::Warning,
                "duplicate_subject",
                &format!(
                    "Subject duplicates commit {}.",
                    first_id.chars().take(8).collect::<String>()
                ),
            ));
        }
    }

    CommitReviewReport {
        range,
        total_commits,
        reviewed_commits: commits.len(),
        truncated: commits.len() < total_commits,
        conventional_commits,
        issue_linked_commits,
        findings,
    }
}

pub fn review_outgoing_commits(repo_path: &str) -> Result<CommitReviewReport, String> {
    let repo = validate_repo(repo_path)?;
    let branches = GitReader::list_branches(repo_path)?;
    let current = branches
        .iter()
        .find(|branch| !branch.is_remote && branch.is_current)
        .ok_or_else(|| "Commit review requires a checked-out local branch".to_string())?;
    let base = current
        .upstream
        .as_deref()
        .or_else(|| {
            branches
                .iter()
                .find(|branch| branch.is_default)
                .map(|branch| branch.name.as_str())
        })
        .ok_or_else(|| {
            "Could not determine an upstream or default branch for review".to_string()
        })?;
    validate_ref_name(base)?;
    let range = format!("{}..HEAD", base);

    let count_text = git_text(&repo, &["rev-list", "--count", &range])?;
    let total_commits = count_text
        .trim()
        .parse::<usize>()
        .map_err(|_| "Git returned an invalid outgoing commit count".to_string())?;
    if total_commits == 0 {
        return Ok(analyze_commit_messages(range, 0, &[]));
    }

    let limit = format!("-n{}", COMMIT_REVIEW_LIMIT);
    let output = git_text(&repo, &["log", &limit, "--format=%H%x00%s%x00", &range])?;
    let commits = parse_commit_review_log(&output);
    Ok(analyze_commit_messages(range, total_commits, &commits))
}

/// Parses `git log --format=%H%x00%s%x00` output into `(commit_id, subject)`
/// pairs.
///
/// NUL is used as both the field and record separator because a commit
/// subject may contain any byte except NUL (git only forbids NUL in commit
/// data), whereas the `\x01` record separator this format replaced can
/// legitimately appear inside a subject. Git appends a newline after each
/// entry, so the newline ahead of the next hash is trimmed off the id.
fn parse_commit_review_log(output: &str) -> Vec<(String, String)> {
    let mut fields = output.split('\0');
    let mut commits = Vec::new();
    while let (Some(id), Some(subject)) = (fields.next(), fields.next()) {
        let id = id.trim();
        if id.is_empty() {
            break;
        }
        commits.push((id.to_string(), subject.trim().to_string()));
    }
    commits
}

fn release_tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$",
        )
        .expect("release tag regex is valid")
    })
}

pub fn validate_release_tag(tag: &str) -> Result<(), String> {
    if !release_tag_regex().is_match(tag) {
        return Err("Release tag must use vMAJOR.MINOR.PATCH SemVer syntax".into());
    }
    validate_ref_name(tag)
}

#[derive(Debug, Clone)]
pub struct ReleasePlan {
    pub tag: String,
    pub message: String,
    pub remote: String,
    pub create_tag: bool,
}

pub fn prepare_release(repo_path: &str, tag: &str, message: &str) -> Result<ReleasePlan, String> {
    validate_release_tag(tag)?;
    let message = message.trim();
    if message.is_empty() {
        return Err("Release message must not be empty".into());
    }
    if message.len() > MAX_RELEASE_MESSAGE_BYTES {
        return Err(format!(
            "Release message exceeds the {} byte limit",
            MAX_RELEASE_MESSAGE_BYTES
        ));
    }
    let repo = validate_repo(repo_path)?;
    if !GitReader::get_status(repo_path)?.is_empty() {
        return Err("Release publishing requires a clean working tree".into());
    }
    let branches = GitReader::list_branches(repo_path)?;
    let current = branches
        .iter()
        .find(|branch| !branch.is_remote && branch.is_current)
        .ok_or_else(|| "Release publishing requires a checked-out local branch".to_string())?;
    if !current.is_default {
        return Err("Release tags can only be published from the default branch".into());
    }
    if current.ahead_count != 0 || current.behind_count != 0 {
        return Err("Default branch must be fully synchronized before publishing a release".into());
    }
    let upstream = current
        .upstream
        .as_deref()
        .ok_or_else(|| "Default branch has no upstream remote".to_string())?;
    let (remote, _) = upstream
        .split_once('/')
        .ok_or_else(|| "Default branch upstream is not a remote-tracking branch".to_string())?;
    validate_ref_name(remote)?;

    let head = git_text(&repo, &["rev-parse", "HEAD"])?.trim().to_string();
    let local_tag = git_text(&repo, &["rev-list", "-n", "1", tag]).ok();
    let create_tag = match local_tag.map(|value| value.trim().to_string()) {
        Some(target) if target == head => false,
        Some(_) => return Err("Release tag already exists locally at a different commit".into()),
        None => true,
    };
    let tag_ref = format!("refs/tags/{}", tag);
    let peeled_ref = format!("{}^{{}}", tag_ref);
    let remote_tag = git_text(
        &repo,
        &["ls-remote", "--tags", remote, &tag_ref, &peeled_ref],
    )?;
    if !remote_tag.trim().is_empty() {
        return Err("Release tag already exists on the remote".into());
    }

    Ok(ReleasePlan {
        tag: tag.to_string(),
        message: message.to_string(),
        remote: remote.to_string(),
        create_tag,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_AUTHOR_NAME", "GitPulse Test")
            .env("GIT_AUTHOR_EMAIL", "gitpulse@example.test")
            .env("GIT_COMMITTER_NAME", "GitPulse Test")
            .env("GIT_COMMITTER_EMAIL", "gitpulse@example.test")
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn commit_file(path: &Path, name: &str, content: &str, message: &str) {
        std::fs::write(path.join(name), content).expect("fixture write");
        git(path, &["add", "--", name]);
        git(path, &["commit", "-m", message]);
    }

    #[test]
    fn release_tags_are_semver_and_reject_ambiguous_refs() {
        assert!(validate_release_tag("v1.2.3").is_ok());
        assert!(validate_release_tag("v1.2.3-rc.1+build.7").is_ok());
        assert!(validate_release_tag("1.2.3").is_err());
        assert!(validate_release_tag("v01.2.3").is_err());
        assert!(validate_release_tag("v1.2.3^{}").is_err());
    }

    #[test]
    fn commit_review_log_parses_nul_records_with_control_chars_in_subject() {
        // Mirrors `git log --format=%H%x00%s%x00` output: each entry ends
        // with NUL plus git's trailing newline. The subject carrying a raw
        // \x01 byte must not split the record stream.
        let output = "abc12345\x00fix: embed \x01 byte in subject\x00\ndef67890\x00second\x00\n";
        let commits = parse_commit_review_log(output);
        assert_eq!(
            commits,
            vec![
                (
                    "abc12345".to_string(),
                    "fix: embed \x01 byte in subject".to_string()
                ),
                ("def67890".to_string(), "second".to_string()),
            ]
        );
    }

    #[test]
    fn commit_review_log_keeps_empty_subjects_aligned() {
        let output = "aaa11111\x00\x00\nbbb22222\x00real subject\x00\n";
        let commits = parse_commit_review_log(output);
        assert_eq!(
            commits,
            vec![
                ("aaa11111".to_string(), String::new()),
                ("bbb22222".to_string(), "real subject".to_string()),
            ]
        );
    }

    #[test]
    fn commit_review_reports_quality_and_coverage() {
        let commits = vec![
            ("aabbccddeeff".into(), "fix(core): repair race #42".into()),
            ("112233445566".into(), "update everything".into()),
            ("77889900aabb".into(), "update everything".into()),
        ];
        let report = analyze_commit_messages("main..HEAD".into(), 9, &commits);
        assert_eq!(report.reviewed_commits, 3);
        assert!(report.truncated);
        assert_eq!(report.conventional_commits, 1);
        assert_eq!(report.issue_linked_commits, 1);
        assert!(report
            .findings
            .iter()
            .any(|item| item.code == "non_conventional"));
        assert!(report
            .findings
            .iter()
            .any(|item| item.code == "duplicate_subject"));
    }

    #[test]
    fn cleanup_plan_only_selects_branches_merged_into_default() {
        let dir = tempfile::tempdir().expect("temp repo");
        git(dir.path(), &["init", "-b", "main"]);
        commit_file(dir.path(), "base.txt", "base", "feat: initial");

        git(dir.path(), &["switch", "-c", "merged-work"]);
        commit_file(dir.path(), "merged.txt", "merged", "fix: merged work");
        git(dir.path(), &["switch", "main"]);
        git(dir.path(), &["merge", "--ff-only", "merged-work"]);

        git(dir.path(), &["switch", "-c", "unmerged-work"]);
        commit_file(
            dir.path(),
            "unmerged.txt",
            "unmerged",
            "feat: unfinished work",
        );
        git(dir.path(), &["switch", "main"]);

        let plan = branch_cleanup_plan(dir.path().to_str().expect("utf8 path")).expect("plan");
        assert_eq!(plan.total_local_branches, 3);
        assert_eq!(plan.protected_branches, 1);
        assert_eq!(plan.unmerged_branches, 1);
        assert_eq!(plan.candidates.len(), 1);
        assert_eq!(plan.candidates[0].name, "merged-work");
    }

    #[test]
    fn release_preflight_requires_clean_synchronized_default_branch() {
        let remote = tempfile::tempdir().expect("bare remote");
        git(remote.path(), &["init", "--bare", "--initial-branch=main"]);
        let repo = tempfile::tempdir().expect("local repo");
        git(repo.path(), &["init", "-b", "main"]);
        commit_file(repo.path(), "app.txt", "ready", "feat: ready release #1");
        git(
            repo.path(),
            &[
                "remote",
                "add",
                "origin",
                remote.path().to_str().expect("utf8 remote"),
            ],
        );
        git(repo.path(), &["push", "-u", "origin", "main"]);

        let path = repo.path().to_str().expect("utf8 repo");
        let plan = prepare_release(path, "v1.0.0", "Release v1.0.0").expect("ready release");
        assert!(plan.create_tag);
        assert_eq!(plan.remote, "origin");

        std::fs::write(repo.path().join("dirty.txt"), "dirty").expect("fixture write");
        let error = prepare_release(path, "v1.0.0", "Release v1.0.0").unwrap_err();
        assert!(error.contains("clean working tree"));
    }
}
