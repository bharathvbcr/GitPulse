//! The agent-output review gate (`docs/AGENT_OUTPUT_REVIEW.md`).
//!
//! An agent attempt works on its own `gitpulse/<task>-<id>` branch. With the
//! gate on for a repository, a guarded Git action that would land an attempt's
//! commits on another branch — `git merge` in every form (fast-forward,
//! `--squash`, `--no-ff`) or `git cherry-pick` — is refused unless a person
//! recorded a `change_review` approving exactly the commit range being landed,
//! or an explicit `merge_unreviewed` override for it. The record lives in the
//! workbench store beside the run it reviews.
//!
//! It runs inside [`super::guard_command`], before the harness is asked, so the
//! panels, the attempt's **Merge** button and the GitPulse terminal are fenced
//! alike. A `git merge` typed into some other terminal is not; the record still
//! shows which ranges were landed without one.

use std::path::Path;

use serde_json::{json, Value};

use crate::engine::git_cli;

/// The branch prefix GitPulse gives a new attempt worktree
/// (`workbench::agent_worktree`).
pub(crate) const ATTEMPT_BRANCH_PREFIX: &str = "gitpulse/";

/// Upper bound on the attempt branches one gate decision considers. A
/// repository with more is refused rather than judged on a prefix of them.
const MAX_ATTEMPT_BRANCHES: usize = 512;

/// The revisions a guarded command would land on the current branch, or
/// `None` when it lands nothing (a different verb, or `--abort`/`--continue`,
/// which resume or drop an operation the gate already judged when it began).
pub(crate) fn landed_revisions(argv: &[&str]) -> Option<Vec<String>> {
    let (verb, rest) = match argv {
        ["git", verb, rest @ ..] => (*verb, rest),
        _ => return None,
    };
    // Options that consume the next argument, per verb.
    let valued: &[&str] = match verb {
        "merge" => &[
            "-m",
            "-F",
            "--file",
            "-s",
            "--strategy",
            "-X",
            "--strategy-option",
            "--into-name",
            "--cleanup",
        ],
        "cherry-pick" => &[
            "-m",
            "--mainline",
            "-s",
            "--strategy",
            "-X",
            "--strategy-option",
            "--cleanup",
        ],
        _ => return None,
    };
    let mut revisions = Vec::new();
    let mut args = rest.iter();
    let mut options_done = false;
    while let Some(arg) = args.next() {
        if !options_done {
            if matches!(*arg, "--abort" | "--continue" | "--quit" | "--skip") {
                return None;
            }
            if *arg == "--" {
                options_done = true;
                continue;
            }
            if valued.contains(arg) {
                args.next();
                continue;
            }
            if arg.starts_with('-') {
                continue;
            }
        }
        revisions.push((*arg).to_string());
    }
    (!revisions.is_empty()).then_some(revisions)
}

/// One attempt branch a guarded command would land commits from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Landed {
    /// Short branch name, `gitpulse/<task>-<id>`.
    pub branch: String,
    /// The branch tip now.
    pub tip: String,
}

/// The attempt branches whose commits `revisions` would land in `repo`.
///
/// A revision counts when it names an attempt branch, or resolves to a commit
/// an attempt branch contains that the current `HEAD` does not — so landing an
/// attempt's commit by its id, or by a range ending in it, is caught as well as
/// landing it by name. Every lookup that fails is an error: a gate that could
/// not tell must not answer "no attempt involved".
pub(crate) fn attempts_landed(repo: &Path, revisions: &[String]) -> Result<Vec<Landed>, String> {
    let listing = git_cli::git_text(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:short) %(objectname)",
            &format!("refs/heads/{ATTEMPT_BRANCH_PREFIX}"),
        ],
    )?;
    let attempts: Vec<Landed> = listing
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(branch, tip)| Landed {
            branch: branch.to_string(),
            tip: tip.to_string(),
        })
        .collect();
    if attempts.is_empty() {
        return Ok(Vec::new());
    }
    if attempts.len() > MAX_ATTEMPT_BRANCHES {
        return Err(format!(
            "this repository has {} attempt branches, more than the {MAX_ATTEMPT_BRANCHES} \
             the review gate judges",
            attempts.len()
        ));
    }
    let mut landed: Vec<Landed> = Vec::new();
    for revision in revisions {
        // `a..b` and `a...b` land what `b` reaches; a cherry-pick of `^x`
        // excludes rather than lands.
        let end = revision
            .rsplit_once("...")
            .or_else(|| revision.rsplit_once(".."))
            .map_or(revision.as_str(), |(_, end)| end);
        if end.is_empty() || end.starts_with('^') {
            continue;
        }
        let named = end.strip_prefix("refs/heads/").unwrap_or(end);
        if let Some(attempt) = attempts.iter().find(|a| a.branch == named) {
            push_unique(&mut landed, attempt);
            continue;
        }
        let commit = git_cli::git_text(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{end}^{{commit}}"),
            ],
        )
        .map_err(|e| format!("could not resolve {end}: {e}"))?;
        let commit = commit.trim();
        if contains(repo, "HEAD", commit)? {
            continue;
        }
        for attempt in &attempts {
            if contains(repo, &attempt.tip, commit)? {
                push_unique(&mut landed, attempt);
            }
        }
    }
    Ok(landed)
}

fn push_unique(landed: &mut Vec<Landed>, attempt: &Landed) {
    if !landed.iter().any(|known| known.branch == attempt.branch) {
        landed.push(attempt.clone());
    }
}

/// Whether `tip` contains `commit`. Git answers 0 for yes and 1 for no; any
/// other outcome is an error, not a no.
fn contains(repo: &Path, tip: &str, commit: &str) -> Result<bool, String> {
    let run = git_cli::git_captured(repo, &["merge-base", "--is-ancestor", commit, tip])?;
    match (run.cancelled, run.status_code) {
        (false, 0) => Ok(true),
        (false, 1) => Ok(false),
        (_, code) => Err(format!(
            "could not tell whether {tip} contains {commit} (git exited {code}): {}",
            String::from_utf8_lossy(&run.stderr).trim()
        )),
    }
}

/// The rules a review-gate refusal is reported under.
pub(crate) const RULE_UNREVIEWED: &str = "review.unreviewed";
pub(crate) const RULE_STALE: &str = "review.stale";
pub(crate) const RULE_REFUSED: &str = "review.refused";
pub(crate) const RULE_UNAVAILABLE: &str = "review.unavailable";

/// The review gate: `Ok` when the command lands no agent attempt, the gate is
/// off for this repository, or every attempt landed has a decision that
/// permits landing exactly its current tip.
///
/// A refusal is GitPulse's own (`checked: false` — the harness is not asked),
/// returned as a verdict so [`super::guard_command`] records it in the ledger
/// like every other gate decision.
pub(crate) fn gate(repo_path: &str, argv: &[&str]) -> Result<(), Box<super::PolicyVerdict>> {
    let Some(revisions) = landed_revisions(argv) else {
        return Ok(());
    };
    let repo = Path::new(repo_path);
    let target = super::render_command(argv);
    let refuse = |rule: &str, reason: String| refusal(rule, &target, reason);
    let enabled = common_dir(repo).and_then(|dir| crate::tool_config::review_gate_enabled(&dir));
    if matches!(enabled, Ok(false)) {
        return Ok(());
    }
    let landed = match attempts_landed(repo, &revisions) {
        Ok(landed) => landed,
        // A revision that cannot be resolved lands nothing Git will accept;
        // whether it is an attempt is only the gate's business when it is on.
        Err(_) if enabled.is_err() => return Ok(()),
        Err(error) => {
            return Err(refuse(
                RULE_UNAVAILABLE,
                format!("could not tell whether this lands an agent attempt's commits: {error}"),
            ))
        }
    };
    if landed.is_empty() {
        return Ok(());
    }
    if let Err(error) = enabled {
        return Err(refuse(
            RULE_UNAVAILABLE,
            format!("this lands an agent attempt, and whether the review gate is on could not be read: {error}"),
        ));
    }
    let state = crate::workbench::review::store();
    for attempt in landed {
        let short = &attempt.tip[..attempt.tip.len().min(12)];
        let run = crate::workbench::review::attempt_run(&state, &attempt.branch).map_err(|e| {
            refuse(
                RULE_UNAVAILABLE,
                format!(
                    "could not read the task run for {}: {}",
                    attempt.branch, e.message
                ),
            )
        })?;
        let Some(run) = run else {
            return Err(refuse(
                RULE_UNREVIEWED,
                format!(
                    "{} is an agent attempt branch with no task run recorded, so there is nothing a \
                     review could be attached to. Turn the review gate off for this repository to land it.",
                    attempt.branch
                ),
            ));
        };
        if !crate::workbench::review::ended(&run) {
            return Err(refuse(
                RULE_UNREVIEWED,
                format!(
                    "the attempt on {} is still {}; its commits can be reviewed once it has ended.",
                    attempt.branch,
                    run["state"].as_str().unwrap_or("active")
                ),
            ));
        }
        let run_id = run["id"].as_str().unwrap_or_default();
        let review =
            crate::workbench::review::review_of(&state, run_id, &attempt.tip).map_err(|e| {
                refuse(
                    RULE_UNAVAILABLE,
                    format!(
                        "could not read the reviews of {}: {}",
                        attempt.branch, e.message
                    ),
                )
            })?;
        use crate::workbench::review::Review;
        let with_note =
            |note: Option<String>| note.map(|n| format!(" Note: {n}")).unwrap_or_default();
        match review {
            Review::Approved { .. } | Review::Overridden { .. } => {}
            Review::Unreviewed => {
                return Err(refuse(
                    RULE_UNREVIEWED,
                    format!(
                        "nobody has reviewed {}@{short}. Review the attempt's changes from its task \
                         (approve, request changes or deny), or record a merge without review there, saying why.",
                        attempt.branch
                    ),
                ))
            }
            Review::Stale { reviewed_head } => {
                return Err(refuse(
                    RULE_STALE,
                    format!(
                        "{} was approved at {}, and has moved to {short} since. Review the new commits.",
                        attempt.branch,
                        &reviewed_head[..reviewed_head.len().min(12)]
                    ),
                ))
            }
            Review::ChangesRequested { note } => {
                return Err(refuse(
                    RULE_REFUSED,
                    format!("changes were requested on {}@{short}.{}", attempt.branch, with_note(note)),
                ))
            }
            Review::Denied { note } => {
                return Err(refuse(
                    RULE_REFUSED,
                    format!("{}@{short} was denied in review.{}", attempt.branch, with_note(note)),
                ))
            }
        }
    }
    Ok(())
}

fn refusal(rule: &str, target: &str, reason: String) -> Box<super::PolicyVerdict> {
    Box::new(super::PolicyVerdict {
        status: super::PolicyStatus::Blocked,
        checked: false,
        target: target.to_string(),
        rule: rule.to_string(),
        severity: "hard".to_string(),
        reason,
        demoted: String::new(),
        grant_id: String::new(),
        granted_by: String::new(),
        widened: String::new(),
        degraded: Vec::new(),
        task_id: String::new(),
        detail: "GitPulse's agent-output review gate refused before asking the harness."
            .to_string(),
        detail_code: "review_gate".to_string(),
    })
}

/// The key the review gate setting is stored under: the repository's
/// canonical common Git directory, shared by every linked worktree.
pub(crate) fn common_dir(repo: &Path) -> Result<String, String> {
    let dir = git_cli::resolve_git_common_dir(repo)?;
    let dir = dir.canonicalize().unwrap_or(dir);
    Ok(dir.to_string_lossy().into_owned())
}

/// The JSON a `change_review` decision binds, built from the repository as it
/// is now: the base the attempt started from and the tip being landed.
pub(crate) fn review_payload(
    repo: &Path,
    repository_id: &str,
    branch: &str,
    base_oid: &str,
    head_oid: &str,
) -> Result<Value, String> {
    let range = format!("{base_oid}..{head_oid}");
    let names = git_cli::git_text(repo, &["diff", "--name-only", "-z", &range])?;
    let files_changed = names.split('\0').filter(|name| !name.is_empty()).count();
    let diff = git_cli::git(
        repo,
        &["diff", "--binary", "--no-color", "--no-ext-diff", &range],
    )?;
    Ok(json!({
        "repository_id": repository_id,
        "base_oid": base_oid,
        "head_oid": head_oid,
        "branch": branch,
        "files_changed": files_changed,
        "diff_digest": crate::tool_install::release::bytes_sha256_hex(&diff)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::git_in;

    #[test]
    fn only_verbs_that_land_commits_are_read_and_resuming_one_lands_nothing_new() {
        let argv = |a: &[&'static str]| landed_revisions(a);
        assert_eq!(
            argv(&["git", "merge", "--no-edit", "gitpulse/x-1"]),
            Some(vec!["gitpulse/x-1".to_string()])
        );
        assert_eq!(
            argv(&["git", "merge", "--squash", "-m", "msg", "-s", "ort", "b"]),
            Some(vec!["b".to_string()])
        );
        assert_eq!(
            argv(&["git", "cherry-pick", "--no-edit", "a1", "b2"]),
            Some(vec!["a1".to_string(), "b2".to_string()])
        );
        assert_eq!(
            argv(&["git", "cherry-pick", "-m", "1", "c3"]),
            Some(vec!["c3".to_string()])
        );
        assert_eq!(argv(&["git", "merge", "--abort"]), None);
        assert_eq!(argv(&["git", "cherry-pick", "--continue"]), None);
        assert_eq!(argv(&["git", "commit", "-m", "x"]), None);
        assert_eq!(argv(&["git", "merge"]), None);
        assert_eq!(argv(&["gh", "pr", "merge", "1"]), None);
    }

    fn repo_with_attempt() -> (tempfile::TempDir, String) {
        let dir = crate::test_support::git_repo();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        git_in(dir.path(), &["add", "a.txt"]);
        git_in(dir.path(), &["commit", "-q", "-m", "base"]);
        git_in(dir.path(), &["switch", "-q", "-c", "gitpulse/fix-1234abcd"]);
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        git_in(dir.path(), &["add", "b.txt"]);
        git_in(dir.path(), &["commit", "-q", "-m", "agent"]);
        let tip = git_cli::git_text(dir.path(), &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        git_in(dir.path(), &["switch", "-q", "main"]);
        (dir, tip)
    }

    /// Landing an attempt by name, by commit id, or by a range ending in it is
    /// one landing; a branch the attempt never touched is none.
    #[test]
    fn an_attempt_is_found_by_name_by_commit_and_by_range() {
        let (dir, tip) = repo_with_attempt();
        let expect = vec![Landed {
            branch: "gitpulse/fix-1234abcd".to_string(),
            tip: tip.clone(),
        }];
        for revision in [
            "gitpulse/fix-1234abcd".to_string(),
            "refs/heads/gitpulse/fix-1234abcd".to_string(),
            tip.clone(),
            format!("main..{tip}"),
        ] {
            assert_eq!(
                attempts_landed(dir.path(), std::slice::from_ref(&revision)).unwrap(),
                expect,
                "{revision}"
            );
        }
        git_in(dir.path(), &["switch", "-q", "-c", "feature"]);
        git_in(dir.path(), &["switch", "-q", "main"]);
        assert!(attempts_landed(dir.path(), &["feature".to_string()])
            .unwrap()
            .is_empty());
        assert!(
            attempts_landed(dir.path(), &["no-such-branch".to_string()]).is_err(),
            "a revision the gate cannot resolve is not 'no attempt involved'"
        );
    }

    #[test]
    fn the_payload_binds_the_exact_range() {
        let (dir, tip) = repo_with_attempt();
        let base = git_cli::git_text(dir.path(), &["rev-parse", "main"])
            .unwrap()
            .trim()
            .to_string();
        let payload =
            review_payload(dir.path(), "local:x", "gitpulse/fix-1234abcd", &base, &tip).unwrap();
        assert_eq!(payload["files_changed"], 1);
        assert_eq!(payload["head_oid"], tip.as_str());
        assert_eq!(payload["diff_digest"].as_str().unwrap().len(), 64);
    }
}
