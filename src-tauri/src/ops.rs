//! High-signal repository operations used by the MANVI Ops surface.
//!
//! Read-only planning lives here; mutations continue through `GitWriter` and
//! the command gate. Cleanup plans are conservative: only local branches the
//! dead-branch scan's own rule (`deadbranch::classify_merge`) calls merged into
//! the default branch are eligible.

use crate::ipc_error::CommandError;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::analyzer::ConventionalCommitParser;
use crate::engine::deadbranch::{
    classify_merge, is_ancestry_merged, needs_tree_check, tree_merge_checks, TreeMerge,
};
use crate::engine::git_cli::{git_captured, git_text, validate_repo, BoundedRun};
use crate::engine::git_writer::validate_ref_name;
use crate::engine::{BranchInfo, GitReader};

const COMMIT_REVIEW_LIMIT: usize = 500;

/// Cap on tag rows carried in a cleanup plan. The plan is a to-do list, not an
/// inventory: the counts beside each list stay whole even when the list does not.
const MAX_TAG_CLEANUP_ROWS: usize = 40;
const MAX_RELEASE_MESSAGE_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchCleanupCandidate {
    pub name: String,
    /// Tip SHA before delete — same restore input the sidebar Undo toast uses
    /// with `createBranch(name, tip)`. Survives in the cleanup list so the ops
    /// panel can show it and restore after the toast is gone.
    pub tip_commit_id: String,
    pub last_summary: String,
    pub last_author: String,
    pub last_commit_timestamp: i64,
    pub upstream_gone: bool,
    /// Merged only because its tree is already in the default branch (a squash
    /// or rebase merge): `git branch -d` refuses it, so it is deleted through
    /// the gated dead-branch clean, which re-proves the merge before `-D`.
    pub merged_by_tree: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchCleanupPlan {
    pub default_branch: String,
    pub current_branch: String,
    pub total_local_branches: usize,
    pub protected_branches: usize,
    pub unmerged_branches: usize,
    /// Branches counted in `unmerged_branches` whose squash/rebase tree
    /// comparison did not run or failed (budget, deadline, partial clone,
    /// unreadable default tree). They may in fact be merged; they are left out
    /// of `candidates` on the safe side.
    pub unchecked_branches: usize,
    /// Why, when `unchecked_branches > 0`.
    pub unchecked_reason: Option<String>,
    pub candidates: Vec<BranchCleanupCandidate>,
}

/// One tag in a [`TagCleanupPlan`], on either side of the split.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TagCleanupEntry {
    pub name: String,
    pub commit_id: String,
    pub summary: String,
    pub commits_ahead_of_base: usize,
    pub commits_behind_base: usize,
}

/// What the repository is retaining on tags.
///
/// [`BranchCleanupPlan`] reads `refs/heads` only, so a commit held by a tag and
/// no branch is counted nowhere in it — which is exactly the shape of work
/// parked under a `retired/…` tag. This plan is that missing half. It proposes
/// deletion only for tags whose every commit is already in the base; a tag
/// git could not measure is neither proposed nor counted as retained work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TagCleanupPlan {
    /// Base every tag was measured against. `None` means no comparison was
    /// possible, and then nothing is proposed for deletion.
    pub compared_to: Option<String>,
    pub total_tags: usize,
    /// Tags git could not measure — an old git, or a tag that does not peel to
    /// a commit. Never proposed for deletion, never counted as retained work.
    pub uncompared_tags: usize,
    /// Tags holding commits the base does not have.
    pub retained_count: usize,
    /// Tags whose every commit is already in the base.
    pub deletable_count: usize,
    /// Sample of the retained tags, newest first; at most [`MAX_TAG_CLEANUP_ROWS`].
    pub retained: Vec<TagCleanupEntry>,
    /// Sample of the deletable tags; at most [`MAX_TAG_CLEANUP_ROWS`].
    pub candidates: Vec<TagCleanupEntry>,
    /// The underlying tag listing was itself capped, so every count here
    /// describes a sample of the repository's tags rather than all of them.
    pub truncated: bool,
}

/// Splits the repository's tags into "already in the base" and "holding work".
///
/// Deliberately no total commit count: two tags on the same lane share commits,
/// so summing their ahead counts would report more commits than exist. Each row
/// carries its own exact count instead.
pub fn tag_cleanup_plan(repo_path: &str) -> Result<TagCleanupPlan, CommandError> {
    let listed = GitReader::list_tags(repo_path)?;

    let mut compared_to: Option<String> = None;
    let mut uncompared_tags = 0usize;
    let mut retained_count = 0usize;
    let mut deletable_count = 0usize;
    let mut retained: Vec<TagCleanupEntry> = Vec::new();
    let mut candidates: Vec<TagCleanupEntry> = Vec::new();

    for tag in &listed.tags {
        let Some(base) = tag.compared_to.as_ref() else {
            // "Not asked" is its own answer. Folding it into either side would
            // either propose an unexamined tag for deletion or invent work.
            uncompared_tags += 1;
            continue;
        };
        if compared_to.is_none() {
            compared_to = Some(base.clone());
        }
        let entry = TagCleanupEntry {
            name: tag.name.clone(),
            commit_id: tag.commit_id.clone(),
            summary: tag.message.clone().unwrap_or_default(),
            commits_ahead_of_base: tag.commits_ahead_of_base,
            commits_behind_base: tag.commits_behind_base,
        };
        if tag.commits_ahead_of_base > 0 {
            retained_count += 1;
            if retained.len() < MAX_TAG_CLEANUP_ROWS {
                retained.push(entry);
            }
        } else {
            deletable_count += 1;
            if candidates.len() < MAX_TAG_CLEANUP_ROWS {
                candidates.push(entry);
            }
        }
    }

    Ok(TagCleanupPlan {
        compared_to,
        total_tags: listed.tags.len(),
        uncompared_tags,
        retained_count,
        deletable_count,
        retained,
        candidates,
        truncated: listed.truncated,
    })
}

pub fn branch_cleanup_plan(repo_path: &str) -> Result<BranchCleanupPlan, CommandError> {
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

    let worktrees = crate::engine::worktree::read_worktree_entries(&repo)?;
    let checked_out_elsewhere: HashSet<&str> = worktrees
        .iter()
        .filter_map(|entry| entry.branch.as_deref())
        .filter(|name| *name != current.name)
        .collect();
    let is_protected = |branch: &BranchInfo| {
        branch.is_current
            || branch.is_default
            || checked_out_elsewhere.contains(branch.name.as_str())
    };

    // "Merged" is the dead-branch scan's own rule (`classify_merge`), not a
    // second one: by ancestry, or by a tree comparison for squash and rebase
    // merges. A comparison that cannot run leaves the branch unmerged, and a
    // gone upstream is only reported, never taken for a merge.
    let pending: Vec<&BranchInfo> = local
        .iter()
        .copied()
        .filter(|b| !is_protected(b) && !is_ancestry_merged(b) && needs_tree_check(b))
        .collect();
    let tips: Vec<(&str, &str)> = pending
        .iter()
        .map(|b| (b.name.as_str(), b.tip_commit_id.as_str()))
        .collect();
    let checks = tree_merge_checks(&repo, &default.tip_commit_id, &tips);
    let tree_verdicts: HashMap<&str, &TreeMerge> = pending
        .iter()
        .map(|b| b.name.as_str())
        .zip(checks.verdicts.iter())
        .collect();

    let mut candidates = Vec::new();
    let mut protected_branches = 0usize;
    let mut unmerged_branches = 0usize;
    for branch in &local {
        if is_protected(branch) {
            protected_branches += 1;
        } else {
            let class = classify_merge(
                is_ancestry_merged(branch),
                tree_verdicts.get(branch.name.as_str()).copied(),
            );
            if !class.merged {
                unmerged_branches += 1;
                continue;
            }
            candidates.push(BranchCleanupCandidate {
                name: branch.name.clone(),
                tip_commit_id: branch.tip_commit_id.clone(),
                last_summary: branch.last_summary.clone(),
                last_author: branch.last_author.clone(),
                last_commit_timestamp: branch.last_commit_timestamp,
                upstream_gone: branch.is_gone,
                merged_by_tree: class.by_tree,
            });
        }
    }
    candidates.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(BranchCleanupPlan {
        default_branch: default.name.clone(),
        current_branch: current.name.clone(),
        total_local_branches: local.len(),
        protected_branches,
        unmerged_branches,
        unchecked_branches: checks.unchecked,
        unchecked_reason: checks.reason,
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

pub fn review_outgoing_commits(repo_path: &str) -> Result<CommitReviewReport, CommandError> {
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

pub fn validate_release_tag(tag: &str) -> Result<(), CommandError> {
    if !release_tag_regex().is_match(tag) {
        return Err("Release tag must use vMAJOR.MINOR.PATCH SemVer syntax".into());
    }
    validate_ref_name(tag)
}

#[derive(Debug, Clone)]
pub struct ReleasePlan {
    /// The commit every check above was made against. The tag is created on
    /// exactly this oid, not on whatever HEAD is by then: a commit or branch
    /// switch between the checks and the tag must not publish an unchecked
    /// commit.
    pub head: String,
    pub tag: String,
    pub message: String,
    pub remote: String,
    pub create_tag: bool,
}

/// The commit the local tag `tag` peels to; `None` when no such tag exists.
///
/// Two questions, asked in order, each answered by git's exit status alone:
///
/// 1. Does `refs/tags/<tag>` exist? (`show-ref --verify --quiet`: exit 1 is
///    "missing".) Qualified, so a branch spelled like the tag is not taken for it.
/// 2. Does it peel to a commit? (`rev-parse --verify --quiet <ref>^{commit}`.)
///    A tag that exists but peels to a tree or blob is an error naming the
///    object type, never "missing": reporting it missing would offer to create a
///    tag that is already there.
///
/// Every other outcome (exit 128, a spawn refusal, a deadline, a cut stream) is a
/// read that did not happen and is an `Err`, never "no tag".
fn local_release_tag(repo: &std::path::Path, tag: &str) -> Result<Option<String>, CommandError> {
    local_release_tag_with(tag, |args| git_captured(repo, args))
}

fn local_release_tag_with<F>(tag: &str, mut run: F) -> Result<Option<String>, CommandError>
where
    F: FnMut(&[&str]) -> Result<BoundedRun, CommandError>,
{
    let mut ask = |args: &[&str]| -> Result<(i32, String, String), CommandError> {
        let run = run(args)?;
        if run.incomplete.is_some() || run.cancelled {
            return Err(format!(
                "Could not read the local release tag {tag}: git's answer was cut off"
            )
            .into());
        }
        Ok((
            run.status_code,
            String::from_utf8_lossy(&run.stdout).trim().to_string(),
            String::from_utf8_lossy(&run.stderr).trim().to_string(),
        ))
    };
    let tag_ref = format!("refs/tags/{tag}");
    match ask(&["show-ref", "--verify", "--quiet", &tag_ref])? {
        (0, _, _) => {}
        (1, _, _) => return Ok(None),
        (code, _, stderr) => {
            return Err(format!(
                "Could not read the local release tag {tag} (git exited {code}): {stderr}"
            )
            .into())
        }
    }
    let commit = format!("{tag_ref}^{{commit}}");
    match ask(&["rev-parse", "--verify", "--quiet", &commit])? {
        (0, oid, _) if !oid.is_empty() => Ok(Some(oid)),
        (1, _, _) => {
            // Exists, does not peel to a commit. The type is only for the message.
            let peeled = format!("{tag_ref}^{{}}");
            let kind = match ask(&["cat-file", "-t", &peeled]) {
                Ok((0, kind, _)) if !kind.is_empty() => format!("a {kind}"),
                _ => "not a commit".to_string(),
            };
            Err(format!(
                "Release tag {tag} already exists locally but points at {kind}, not a commit"
            )
            .into())
        }
        (code, _, stderr) => Err(format!(
            "Could not read the local release tag {tag} (git exited {code}): {stderr}"
        )
        .into()),
    }
}

pub fn prepare_release(
    repo_path: &str,
    tag: &str,
    message: &str,
) -> Result<ReleasePlan, CommandError> {
    validate_release_tag(tag)?;
    let message = message.trim();
    if message.is_empty() {
        return Err("Release message must not be empty".into());
    }
    if message.len() > MAX_RELEASE_MESSAGE_BYTES {
        return Err(format!(
            "Release message exceeds the {} byte limit",
            MAX_RELEASE_MESSAGE_BYTES
        )
        .into());
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
    let create_tag = match local_release_tag(&repo, tag)? {
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
        head,
        tag: tag.to_string(),
        message: message.to_string(),
        remote: remote.to_string(),
        create_tag,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::git_writer::GitWriter;
    use crate::procguard::LockedSpawn;
    use std::path::Path;
    use std::process::Command;

    fn git(path: &Path, args: &[&str]) -> String {
        let mut command = Command::new("git");
        crate::test_support::pin_git(&mut command);
        let output = command
            .args(args)
            .current_dir(path)
            .env("GIT_AUTHOR_NAME", "GitPulse Test")
            .env("GIT_AUTHOR_EMAIL", "gitpulse@example.test")
            .env("GIT_COMMITTER_NAME", "GitPulse Test")
            .env("GIT_COMMITTER_EMAIL", "gitpulse@example.test")
            .output_locked()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        if args.first() == Some(&"init") {
            crate::test_support::trust_repo(path);
        }
        String::from_utf8_lossy(&output.stdout).trim().to_string()
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
        // The default branch moves past the merged branch: a branch sitting at
        // the default's own tip is not merged (see `is_ancestry_merged`).
        commit_file(dir.path(), "next.txt", "next", "feat: main moves on");

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
        assert_eq!(
            plan.candidates[0].tip_commit_id.len(),
            40,
            "ops panel restore needs the tip on every candidate"
        );
    }

    /// A branch whose upstream is gone: tracked `origin/<name>`, which then
    /// vanished from the remote.
    fn make_upstream_gone(path: &Path, branch: &str) {
        let remote_ref = format!("refs/remotes/origin/{branch}");
        git(path, &["update-ref", &remote_ref, branch]);
        git(
            path,
            &[
                "branch",
                &format!("--set-upstream-to=origin/{branch}"),
                branch,
            ],
        );
        git(path, &["update-ref", "-d", &remote_ref]);
    }

    /// One repository, five branches, and the two surfaces that decide what is
    /// merged (the ops cleanup plan and the dead-branch scan): they must agree
    /// on every one, and an upstream that is gone must never decide it.
    #[test]
    fn the_ops_plan_and_the_deadbranch_scan_agree_on_what_is_merged() {
        use crate::engine::deadbranch::{scan_stale_branches, DeadbranchConfig};

        let dir = tempfile::tempdir().expect("temp repo");
        let path = dir.path();
        git(path, &["init", "-b", "main"]);
        git(path, &["remote", "add", "origin", "./nowhere.git"]);
        commit_file(path, "base.txt", "base", "feat: initial");

        // Ancestry-merged: merged with a merge commit, main moved past it.
        git(path, &["switch", "-c", "ancestry-merged"]);
        commit_file(path, "a.txt", "a", "feat: a");
        git(path, &["switch", "main"]);
        git(
            path,
            &["merge", "--no-ff", "-m", "merge a", "ancestry-merged"],
        );

        // Gone upstream, unpushed work, never merged.
        git(path, &["switch", "-c", "gone-ahead"]);
        commit_file(path, "ahead.txt", "ahead", "feat: unpushed");
        git(path, &["switch", "main"]);
        make_upstream_gone(path, "gone-ahead");

        // Gone upstream, squash-merged: the change is in main, the commits are not.
        git(path, &["switch", "-c", "gone-squash"]);
        commit_file(path, "squash.txt", "squash", "feat: squashed");
        git(path, &["switch", "main"]);
        git(path, &["merge", "--squash", "gone-squash"]);
        git(path, &["commit", "-m", "feat: squashed (#1)"]);
        make_upstream_gone(path, "gone-squash");

        // Unrelated work, upstream intact.
        git(path, &["switch", "-c", "unrelated"]);
        commit_file(path, "other.txt", "other", "feat: other");
        git(path, &["switch", "main"]);

        // At main's own tip with nothing ahead: a fresh branch, not a merged one.
        git(path, &["branch", "same-tip"]);

        let repo = path.to_str().expect("utf8 path");
        let plan = branch_cleanup_plan(repo).expect("plan");
        let ops_merged: std::collections::BTreeSet<String> =
            plan.candidates.iter().map(|c| c.name.clone()).collect();

        let scan = scan_stale_branches(
            repo,
            &DeadbranchConfig {
                days_threshold: 0,
                merged_only: false,
                ..DeadbranchConfig::default()
            },
        )
        .expect("scan");
        let scan_merged: std::collections::BTreeSet<String> = scan
            .branches
            .iter()
            .filter(|b| b.is_merged && !b.is_protected && !b.is_current_or_worktree)
            .map(|b| b.name.clone())
            .collect();

        let expected: std::collections::BTreeSet<String> =
            ["ancestry-merged", "gone-squash"].map(String::from).into();
        assert_eq!(ops_merged, expected, "ops::branch_cleanup_plan");
        assert_eq!(scan_merged, expected, "deadbranch scan");

        let by_name = |name: &str| {
            scan.branches
                .iter()
                .find(|b| b.name == name)
                .unwrap_or_else(|| panic!("{name} missing from the scan"))
        };
        let gone_ahead = by_name("gone-ahead");
        assert!(gone_ahead.upstream_gone);
        assert!(
            !gone_ahead.is_merged && !gone_ahead.merged_by_tree,
            "unpushed work behind a deleted upstream is not merged (so it is not pre-selected)"
        );
        let gone_squash = by_name("gone-squash");
        assert!(gone_squash.upstream_gone && gone_squash.is_merged && gone_squash.merged_by_tree);
        let same_tip = by_name("same-tip");
        assert!(!same_tip.is_merged && !same_tip.upstream_gone);
        assert!(!by_name("unrelated").is_merged);
        assert!(!by_name("ancestry-merged").merged_by_tree);
        assert_eq!(
            scan.merged_count, 2,
            "merged_count counts only merged branches, not gone upstreams"
        );
        assert_eq!(scan.squash_merged_count, 1);
        let gone_in_plan: Vec<&str> = plan
            .candidates
            .iter()
            .filter(|c| c.upstream_gone)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(gone_in_plan, vec!["gone-squash"]);
    }

    #[test]
    fn tag_cleanup_plan_splits_tags_the_branch_plan_counts_nowhere() {
        let dir = tempfile::tempdir().expect("temp repo");
        git(dir.path(), &["init", "-b", "main"]);
        commit_file(dir.path(), "base.txt", "base", "feat: initial");
        git(dir.path(), &["tag", "already-in-main"]);

        // The shape this exists for: work parked on a tag, on no branch at all.
        git(dir.path(), &["switch", "-c", "throwaway"]);
        commit_file(dir.path(), "kept.txt", "kept", "feat: preserved attempt");
        git(dir.path(), &["tag", "retired/attempt"]);
        git(dir.path(), &["switch", "main"]);
        git(dir.path(), &["branch", "-D", "throwaway"]);
        commit_file(dir.path(), "next.txt", "next", "feat: main moves on");

        let path = dir.path().to_str().expect("utf8 path");
        let plan = tag_cleanup_plan(path).expect("tag plan");

        assert_eq!(plan.compared_to.as_deref(), Some("main"));
        assert_eq!(plan.total_tags, 2);
        assert_eq!(plan.uncompared_tags, 0);
        assert_eq!(plan.retained_count, 1);
        assert_eq!(plan.deletable_count, 1);
        assert_eq!(plan.retained[0].name, "retired/attempt");
        assert_eq!(plan.retained[0].commits_ahead_of_base, 1);
        assert_eq!(plan.candidates[0].name, "already-in-main");
        assert_eq!(plan.candidates[0].commits_ahead_of_base, 0);
        assert!(!plan.truncated);

        // The branch plan sees none of it: that is the gap this closes.
        let branches = branch_cleanup_plan(path).expect("branch plan");
        assert_eq!(branches.unmerged_branches, 0);
        assert!(branches.candidates.is_empty());
    }

    #[test]
    fn tag_cleanup_plan_never_proposes_a_tag_it_could_not_measure() {
        let dir = tempfile::tempdir().expect("temp repo");
        git(dir.path(), &["init", "-b", "main"]);
        commit_file(dir.path(), "payload.txt", "contents", "feat: initial");
        let blob = {
            let out = std::process::Command::new("git")
                .args(["rev-parse", "HEAD:payload.txt"])
                .current_dir(dir.path())
                .output_locked()
                .expect("rev-parse");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        // Does not peel to a commit: `git tag -l` errors on this row and still
        // exits 0, leaving it uncompared.
        git(
            dir.path(),
            &["tag", "-a", "blobbed", "-m", "tag on a blob", &blob],
        );
        git(dir.path(), &["tag", "ordinary"]);

        let plan = tag_cleanup_plan(dir.path().to_str().expect("utf8 path")).expect("tag plan");

        assert_eq!(plan.total_tags, 2);
        assert_eq!(plan.uncompared_tags, 1);
        // Proposing an unexamined tag for deletion is the failure this pins.
        assert_eq!(plan.deletable_count, 1);
        assert!(plan.candidates.iter().all(|c| c.name != "blobbed"));
        assert!(plan.retained.iter().all(|r| r.name != "blobbed"));
        assert_eq!(
            plan.uncompared_tags + plan.retained_count + plan.deletable_count,
            plan.total_tags,
            "every tag lands in exactly one bucket"
        );
    }

    #[test]
    fn tag_cleanup_plan_on_a_repository_with_no_tags_names_no_base() {
        let dir = tempfile::tempdir().expect("temp repo");
        git(dir.path(), &["init", "-b", "main"]);
        commit_file(dir.path(), "base.txt", "base", "feat: initial");

        let plan = tag_cleanup_plan(dir.path().to_str().expect("utf8 path")).expect("tag plan");
        assert_eq!(plan.total_tags, 0);
        assert_eq!(plan.compared_to, None);
        assert!(plan.candidates.is_empty());
        assert!(plan.retained.is_empty());
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

    /// A pushed, synchronized default branch with one commit: the shape
    /// `prepare_release` accepts. The remote is returned to keep it alive.
    fn release_ready_repo() -> (tempfile::TempDir, tempfile::TempDir) {
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
        (remote, repo)
    }

    /// A branch spelled like the release tag is not the tag. `rev-list v1.5.0`
    /// resolves the branch, so the tag was taken for existing and never made.
    #[test]
    fn a_branch_named_like_the_release_tag_is_not_an_existing_tag() {
        let (_remote, repo) = release_ready_repo();
        git(repo.path(), &["branch", "v1.5.0"]);
        let path = repo.path().to_str().expect("utf8 repo");

        let plan = prepare_release(path, "v1.5.0", "Release v1.5.0").expect("ready release");
        assert!(
            plan.create_tag,
            "a same-named branch at HEAD must not stand in for the tag"
        );
    }

    #[test]
    fn a_branch_named_like_the_tag_at_another_commit_does_not_block_the_release() {
        let (_remote, repo) = release_ready_repo();
        git(repo.path(), &["branch", "v1.5.0", "HEAD"]);
        git(repo.path(), &["switch", "-q", "v1.5.0"]);
        commit_file(repo.path(), "other.txt", "x", "feat: elsewhere");
        git(repo.path(), &["switch", "-q", "main"]);
        let path = repo.path().to_str().expect("utf8 repo");

        let plan = prepare_release(path, "v1.5.0", "Release v1.5.0").expect("ready release");
        assert!(plan.create_tag);
    }

    #[test]
    fn a_tag_at_another_commit_is_refused() {
        let (_remote, repo) = release_ready_repo();
        git(repo.path(), &["tag", "v1.5.0"]);
        // HEAD moves on, stays pushed and clean: only the tag is stale.
        commit_file(repo.path(), "next.txt", "next", "feat: next");
        git(repo.path(), &["push", "origin", "main"]);
        let path = repo.path().to_str().expect("utf8 repo");

        let error = prepare_release(path, "v1.5.0", "Release v1.5.0").unwrap_err();
        assert!(
            error.contains("already exists locally at a different commit"),
            "{}",
            error.message()
        );
    }

    fn probe(status_code: i32, stdout: &str, stderr: &str) -> BoundedRun {
        BoundedRun {
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            success: status_code == 0,
            status_code,
            incomplete: None,
            stderr_incomplete: None,
            cancelled: false,
        }
    }

    /// A git whose answers are scripted per subcommand: `(exit, stdout)`.
    fn scripted(
        show_ref: (i32, &'static str),
        rev_parse: (i32, &'static str),
        cat_file: (i32, &'static str),
    ) -> impl FnMut(&[&str]) -> Result<BoundedRun, CommandError> {
        move |args| {
            let (code, out) = match args[0] {
                "show-ref" => show_ref,
                "rev-parse" => rev_parse,
                "cat-file" => cat_file,
                other => panic!("unexpected git {other}"),
            };
            Ok(probe(code, out, "fatal: scripted"))
        }
    }

    /// Only git's "missing" answer (`show-ref --verify --quiet` exit 1) is "no
    /// local tag". Any other failure used to collapse into it, so
    /// `create_tag=true` was reported for a repository git could not read.
    #[test]
    fn only_a_missing_ref_means_no_local_tag() {
        let missing = local_release_tag_with("v1.5.0", |args| {
            assert_eq!(
                args,
                ["show-ref", "--verify", "--quiet", "refs/tags/v1.5.0"]
            );
            Ok(probe(1, "", ""))
        });
        assert_eq!(missing.expect("missing is an answer"), None);

        let found = local_release_tag_with("v1.5.0", scripted((0, ""), (0, "abc123\n"), (0, "")));
        assert_eq!(found.expect("found"), Some("abc123".to_string()));

        for (name, run) in [
            ("show-ref 128", scripted((128, ""), (0, ""), (0, ""))),
            ("rev-parse 128", scripted((0, ""), (128, ""), (0, ""))),
            ("empty success", scripted((0, ""), (0, ""), (0, ""))),
        ] {
            let error = local_release_tag_with("v1.5.0", run).expect_err(name);
            assert!(
                !error.message().contains("points at"),
                "{name}: {}",
                error.message()
            );
        }
        let broken = local_release_tag_with("v1.5.0", scripted((128, ""), (0, ""), (0, "")))
            .expect_err("exit 128 is not 'missing'");
        assert!(broken.message().contains("128"), "{}", broken.message());

        let unspawnable =
            local_release_tag_with("v1.5.0", |_| Err("spawn refused".to_string().into()));
        assert!(unspawnable.is_err());

        let mut cut = probe(1, "", "");
        cut.cancelled = true;
        assert!(local_release_tag_with("v1.5.0", |_| Ok(cut.clone())).is_err());
    }

    /// A tag that exists but peels to a tree or blob is not a missing tag.
    #[test]
    fn a_tag_that_exists_but_is_no_commit_is_an_error_naming_its_type() {
        let error = local_release_tag_with("v1.5.0", scripted((0, ""), (1, ""), (0, "tree\n")))
            .expect_err("exists, not a commit");
        assert!(error.message().contains("a tree"), "{}", error.message());

        let unknown_type = local_release_tag_with("v1.5.0", scripted((0, ""), (1, ""), (128, "")))
            .expect_err("type unknown still an error");
        assert!(
            unknown_type.message().contains("not a commit"),
            "{}",
            unknown_type.message()
        );
    }

    #[test]
    fn a_local_tag_at_head_is_reused_not_recreated() {
        let (_remote, repo) = release_ready_repo();
        git(repo.path(), &["tag", "v1.5.0"]);
        let path = repo.path().to_str().expect("utf8 repo");

        let plan = prepare_release(path, "v1.5.0", "Release v1.5.0").expect("ready release");
        assert!(!plan.create_tag);
    }

    /// A commit landing between the checks and the tag must not be published:
    /// the tag goes on the commit that was checked.
    #[test]
    fn the_release_tag_lands_on_the_commit_that_was_checked() {
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

        // Between prepare and execute, an unchecked commit lands on HEAD.
        commit_file(repo.path(), "late.txt", "unreviewed", "feat: late change");
        let moved = git_text(repo.path(), &["rev-parse", "HEAD"]).expect("head");
        assert_ne!(moved.trim(), plan.head, "the fixture must move HEAD");

        GitWriter::create_tag(path, &plan.tag, Some(&plan.head), Some(&plan.message)).expect("tag");
        let tagged = git_text(repo.path(), &["rev-list", "-n", "1", "v1.0.0"]).expect("tag target");
        assert_eq!(
            tagged.trim(),
            plan.head,
            "the release tag was created on an unchecked commit"
        );
    }

    // ---- moved from the adversary review: release tag ----

    #[test]
    fn an_annotated_tag_at_head_is_reused() {
        let (_remote, repo) = release_ready_repo();
        git(repo.path(), &["tag", "-a", "v1.5.0", "-m", "release"]);
        let plan = prepare_release(repo.path().to_str().unwrap(), "v1.5.0", "Release").unwrap();
        assert!(!plan.create_tag, "annotated tag peels to the commit");
    }

    #[test]
    fn a_packed_tag_at_head_is_reused() {
        let (_remote, repo) = release_ready_repo();
        git(repo.path(), &["tag", "v1.5.0"]);
        git(repo.path(), &["pack-refs", "--all"]);
        let plan = prepare_release(repo.path().to_str().unwrap(), "v1.5.0", "Release").unwrap();
        assert!(!plan.create_tag);
    }

    #[test]
    fn a_tag_at_head_wins_over_a_same_named_branch_elsewhere() {
        let (_remote, repo) = release_ready_repo();
        git(repo.path(), &["tag", "v1.5.0"]);
        git(repo.path(), &["switch", "-q", "-c", "v1.5.0"]);
        commit_file(repo.path(), "o.txt", "o", "feat: o");
        git(repo.path(), &["switch", "-q", "main"]);
        let plan = prepare_release(repo.path().to_str().unwrap(), "v1.5.0", "Release").unwrap();
        assert!(!plan.create_tag, "refs/tags/ qualification, not the branch");
    }

    #[test]
    fn an_annotated_tag_at_another_commit_is_refused() {
        let (_remote, repo) = release_ready_repo();
        git(repo.path(), &["tag", "-a", "v1.5.0", "-m", "old"]);
        commit_file(repo.path(), "n.txt", "n", "feat: next");
        git(repo.path(), &["push", "origin", "main"]);
        let error =
            prepare_release(repo.path().to_str().unwrap(), "v1.5.0", "Release").unwrap_err();
        assert!(error.contains("different commit"), "{}", error.message());
    }

    #[test]
    fn option_and_revision_shaped_tag_names_never_reach_git() {
        let (_remote, repo) = release_ready_repo();
        let path = repo.path().to_str().unwrap();
        for tag in [
            "--all",
            "-v1.0.0",
            "v1.0.0^{}",
            "v1.0.0..v2.0.0",
            "v1.0.0@{0}",
            "v1.0.0^{commit}",
            "v1.0.0\n",
        ] {
            assert!(prepare_release(path, tag, "Release").is_err(), "{tag:?}");
        }
    }

    #[test]
    fn a_shallow_clone_reads_the_tag_like_any_other() {
        let (remote, _repo) = release_ready_repo();
        let shallow = tempfile::tempdir().unwrap();
        let url = format!("file://{}", remote.path().display());
        git(shallow.path(), &["clone", "-q", "--depth=1", &url, "clone"]);
        let clone = shallow.path().join("clone");
        crate::test_support::trust_repo(&clone);
        git(&clone, &["tag", "v1.5.0"]);
        let plan = prepare_release(clone.to_str().unwrap(), "v1.5.0", "Release").unwrap();
        assert!(!plan.create_tag);
    }

    /// A tag at a tree (or blob) cannot be peeled to a commit. `rev-parse --verify
    /// --quiet <tag>^{commit}` exits 1 with empty stdout for that too, so the
    /// classification reads "missing" and `create_tag=true` is reported for a tag
    /// that exists (the later `git tag` then fails). The doc comment promises every
    /// outcome other than "missing" is an Err.
    #[test]
    fn a_tag_that_exists_but_is_no_commit_is_an_error_not_missing() {
        let (_remote, repo) = release_ready_repo();
        git(repo.path(), &["tag", "v1.5.0", "HEAD^{tree}"]);
        let result = prepare_release(repo.path().to_str().unwrap(), "v1.5.0", "Release");
        match result {
            Err(_) => {}
            Ok(plan) => panic!(
                "tag v1.5.0 exists (a tree); plan says create_tag={} instead of refusing",
                plan.create_tag
            ),
        }
    }

    #[test]
    fn a_tag_at_a_blob_is_an_error_not_missing() {
        let (_remote, repo) = release_ready_repo();
        let blob = git(repo.path(), &["rev-parse", "HEAD:app.txt"]);
        git(repo.path(), &["tag", "v1.5.0", &blob]);
        let result = prepare_release(repo.path().to_str().unwrap(), "v1.5.0", "Release");
        assert!(result.is_err(), "blob tag treated as missing: {result:?}");
    }

    // ---- moved from the adversary review: one merged rule ----

    fn names(plan: &BranchCleanupPlan) -> Vec<String> {
        let mut v: Vec<String> = plan.candidates.iter().map(|c| c.name.clone()).collect();
        v.sort();
        v
    }

    #[test]
    fn a_squash_merge_is_found_even_after_main_advanced_past_the_squash() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-b", "main"]);
        commit_file(p, "base.txt", "base", "feat: initial");
        git(p, &["switch", "-c", "sq"]);
        commit_file(p, "s.txt", "s", "feat: s");
        git(p, &["switch", "main"]);
        git(p, &["merge", "--squash", "sq"]);
        git(p, &["commit", "-m", "feat: s (#1)"]);
        commit_file(p, "later.txt", "later", "feat: later");
        // Main also rewrites the squashed file afterwards: still "in main" as
        // history, but the tip tree no longer equals the merge result.
        commit_file(p, "s.txt", "s changed later", "fix: s tweaked");
        let plan = branch_cleanup_plan(p.to_str().unwrap()).unwrap();
        assert_eq!(
            names(&plan),
            Vec::<String>::new(),
            "documented limit: a later rewrite of the squashed file hides the squash (unmerged, safe side)"
        );
    }

    #[test]
    fn a_squash_merged_branch_is_flagged_by_tree_and_an_ancestry_merged_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-b", "main"]);
        commit_file(p, "base.txt", "base", "feat: initial");
        git(p, &["branch", "anc"]);
        git(p, &["switch", "-c", "sq"]);
        commit_file(p, "s.txt", "s", "feat: s");
        git(p, &["switch", "main"]);
        git(p, &["merge", "--squash", "sq"]);
        git(p, &["commit", "-m", "feat: s (#1)"]);
        let plan = branch_cleanup_plan(p.to_str().unwrap()).unwrap();
        let by_tree = |name: &str| {
            plan.candidates
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("{name} not planned: {:?}", names(&plan)))
                .merged_by_tree
        };
        assert!(
            by_tree("sq"),
            "squash-merged: git branch -d would refuse it"
        );
        assert!(!by_tree("anc"), "ancestry-merged: plain -d deletes it");
    }

    #[test]
    fn a_gone_upstream_with_nothing_ahead_and_main_past_it_is_merged_by_ancestry() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-b", "main"]);
        git(p, &["remote", "add", "origin", "./nowhere.git"]);
        commit_file(p, "base.txt", "base", "feat: initial");
        git(p, &["branch", "old"]);
        commit_file(p, "n.txt", "n", "feat: main moves on");
        git(p, &["update-ref", "refs/remotes/origin/old", "old"]);
        git(p, &["branch", "--set-upstream-to=origin/old", "old"]);
        git(p, &["update-ref", "-d", "refs/remotes/origin/old"]);
        let plan = branch_cleanup_plan(p.to_str().unwrap()).unwrap();
        assert_eq!(names(&plan), vec!["old".to_string()]);
    }

    #[test]
    fn a_branch_checked_out_in_another_worktree_is_protected_even_when_merged() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-b", "main"]);
        commit_file(p, "base.txt", "base", "feat: initial");
        git(p, &["branch", "busy"]);
        commit_file(p, "n.txt", "n", "feat: next");
        let wt = tempfile::tempdir().unwrap();
        git(
            p,
            &[
                "worktree",
                "add",
                "-q",
                wt.path().join("w").to_str().unwrap(),
                "busy",
            ],
        );
        let plan = branch_cleanup_plan(p.to_str().unwrap()).unwrap();
        assert!(names(&plan).is_empty(), "{:?}", names(&plan));
        assert_eq!(plan.protected_branches, 2);
    }

    #[test]
    fn a_detached_head_is_refused_not_planned() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-b", "main"]);
        commit_file(p, "base.txt", "base", "feat: initial");
        git(p, &["branch", "old"]);
        commit_file(p, "n.txt", "n", "feat: next");
        git(p, &["switch", "-q", "--detach", "HEAD"]);
        assert!(branch_cleanup_plan(p.to_str().unwrap()).is_err());
    }

    #[test]
    fn no_resolvable_default_branch_plans_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-b", "weird-trunk"]);
        commit_file(p, "base.txt", "base", "feat: initial");
        git(p, &["branch", "old"]);
        commit_file(p, "n.txt", "n", "feat: next");
        match branch_cleanup_plan(p.to_str().unwrap()) {
            Err(_) => {}
            Ok(plan) => assert!(
                names(&plan).is_empty(),
                "an unresolved default must not mark anything merged: {:?}",
                names(&plan)
            ),
        }
    }

    // Spawn measurement: the numbers are printed with --nocapture.

    fn many_branches(count: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-b", "main"]);
        commit_file(p, "base.txt", "base", "feat: initial");
        for i in 0..count {
            let name = format!("feature/b{i:03}");
            git(p, &["switch", "-q", "-c", &name]);
            commit_file(p, &format!("f{i}.txt"), "x", "feat: work");
            git(p, &["switch", "-q", "main"]);
        }
        commit_file(p, "next.txt", "n", "feat: main moves on");
        dir
    }

    fn plan_spawns(count: usize) -> (usize, usize, usize) {
        let dir = many_branches(count);
        let p = dir.path();
        let canonical = std::fs::canonicalize(p).unwrap();
        let log = || crate::engine::git_cli::spawn_log::spawns_in(&canonical);
        let before = log().len();
        let plan = branch_cleanup_plan(p.to_str().unwrap()).unwrap();
        let used: Vec<Vec<String>> = log()[before..].to_vec();
        assert_eq!(plan.unmerged_branches, count);
        // Past the shared budget the rest are left unmerged and counted.
        assert_eq!(
            plan.unchecked_branches,
            count.saturating_sub(crate::engine::deadbranch::MAX_TREE_CHECKS)
        );
        let merge_trees = used
            .iter()
            .filter(|a| a.iter().any(|x| x == "merge-tree"))
            .count();
        let all = used.len();
        eprintln!(
            "SPAWNS branch_cleanup_plan branches={count} total={all} merge-tree={merge_trees}"
        );
        (all, merge_trees, count)
    }

    #[test]
    fn measure_plan_spawns_at_50_branches() {
        let (_, merge_trees, n) = plan_spawns(50);
        assert!(merge_trees <= n);
    }

    /// 200 unmerged branches: the cleanup plan (and so `storage::branch_summary`,
    /// which calls it) used to cost one `git branch --merged`. It now runs one
    /// quarantined `merge-tree` per unmerged branch with no cap.
    #[test]
    fn the_cleanup_plan_does_not_fan_out_one_merge_tree_per_branch() {
        let (total, merge_trees, n) = plan_spawns(200);
        assert!(
            merge_trees <= crate::engine::deadbranch::MAX_TREE_CHECKS,
            "{merge_trees} merge-tree spawns ({total} spawns total) for {n} branches: unbounded fan-out"
        );
    }

    /// A tree comparison that cannot run leaves the branch unmerged with no trace:
    /// the plan type has no field that says "N branches were not checked".
    #[test]
    fn a_skipped_tree_check_is_visible_in_the_plan() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-b", "main"]);
        commit_file(p, "base.txt", "base", "feat: initial");
        git(p, &["switch", "-q", "-c", "sq"]);
        commit_file(p, "s.txt", "s", "feat: s");
        git(p, &["switch", "-q", "main"]);
        git(p, &["merge", "--squash", "sq"]);
        git(p, &["commit", "-m", "feat: s (#1)"]);
        // Make this a partial clone so every tree comparison is refused.
        git(p, &["config", "extensions.partialClone", "origin"]);
        git(p, &["config", "remote.origin.promisor", "true"]);
        git(p, &["config", "remote.origin.url", "./nowhere.git"]);
        let plan = branch_cleanup_plan(p.to_str().unwrap()).unwrap();
        let json = serde_json::to_string(&plan).unwrap().to_lowercase();
        assert!(
            plan.candidates.is_empty()
                && (json.contains("unknown")
                    || json.contains("warn")
                    || json.contains("unchecked")),
            "squash-merged `sq` is silently reported as plain unmerged: {json}"
        );
        assert_eq!(plan.unchecked_branches, 1);
        assert!(
            plan.unchecked_reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty()),
            "{:?}",
            plan.unchecked_reason
        );
    }
}
