//! Posts a task's verification verdict to GitHub as a commit status, and
//! optionally as a pull-request comment, through the user's own `gh` login.
//!
//! The verdict is Manvi's `work.verify.task` record (`verification_gate`); this
//! module only reports it and never judges anything. The boundaries the owner
//! approved (2026-10-08, option A), each pinned by a test:
//!
//! - Off for a repository until the person opts it in (`tool_config`), and it
//!   runs only from an explicit desktop command. No MCP tool reaches it.
//! - `success` only for a settled `passed` record, for the commit it names,
//!   with a clean worktree sitting at that commit. A verdict that could not be
//!   established (never recorded, could not run, not settled, degraded, dirty
//!   worktree) posts `error`, never `success`. A verdict that cannot be tied
//!   to the commit (head moved, head or status unreadable) posts nothing.
//! - The post carries a fixed vocabulary, counts and criterion numbers. It
//!   never carries command output, environment, diff hunks, or the free text a
//!   criterion or its findings hold.
//! - The request goes through `gh api`. GitPulse never reads, stores or logs
//!   the token, and a status or comment that GitHub refuses is an error, not a
//!   success.

use super::verification_gate::{Record, STATE_COULD_NOT_RUN};
use crate::github::GitHubRepoRef;
use serde::Serialize;
use std::path::Path;

/// The status context GitHub shows beside the commit.
pub(crate) const CONTEXT: &str = "devcouncil/verify";
/// GitHub's own limit on a status description.
const MAX_DESCRIPTION: usize = 140;
/// Open pull requests commented on for one commit.
const MAX_PULLS: usize = 5;

/// What would be posted, decided before anything leaves the process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Plan {
    pub sha: String,
    /// `success`, `failure` or `error`; `pending` is never posted.
    pub state: &'static str,
    pub description: String,
    pub comment: String,
}

/// What the plan is made from, read once by the caller.
pub(crate) struct Facts<'a> {
    pub item_id: &'a str,
    /// The newest verification record: an error when the store could not be
    /// read, `None` when the task was never verified.
    pub record: Result<Option<Record>, String>,
    /// HEAD of the verified worktree now.
    pub head: Result<String, String>,
    /// Whether that worktree has uncommitted changes (tracked or untracked).
    pub dirty: Result<bool, String>,
}

/// A status word from the record, kept only if it is a plain lowercase word.
fn word(text: &str) -> &str {
    let plain = !text.is_empty()
        && text.len() <= 24
        && text.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
    if plain {
        text
    } else {
        "unrecognised"
    }
}

fn describe(headline: &str) -> String {
    let mut text = format!("devcouncil verify: {headline}");
    if text.chars().count() > MAX_DESCRIPTION {
        text = text.chars().take(MAX_DESCRIPTION - 1).collect::<String>() + "…";
    }
    text
}

fn comment(state: &str, headline: &str, sha: &str, record: Option<&Record>) -> String {
    let mut body = format!(
        "### devcouncil verify: {headline}\n\nCommit `{}`. Status posted as `{CONTEXT}` ({state}).\n",
        &sha[..12.min(sha.len())]
    );
    if let Some(record) = record {
        if !record.criteria.is_empty() {
            body.push_str("\n| Criterion | Result |\n| --- | --- |\n");
            for criterion in &record.criteria {
                body.push_str(&format!(
                    "| {} | {} |\n",
                    criterion.index.clamp(0, 9999),
                    word(&criterion.status)
                ));
            }
        }
        if !record.degraded.is_empty() {
            body.push_str(&format!(
                "\n{} item(s) could not be checked.\n",
                record.degraded.len()
            ));
        }
    }
    body.push_str("\nPosted from GitPulse on request. Details are in GitPulse.");
    body
}

fn error_plan(sha: &str, headline: &str, record: Option<&Record>) -> Plan {
    Plan {
        sha: sha.to_string(),
        state: "error",
        description: describe(headline),
        comment: comment("error", headline, sha, record),
    }
}

/// Decides what to post, or why nothing may be.
pub(crate) fn plan(facts: &Facts<'_>) -> Result<Plan, String> {
    let head = facts.head.as_ref().map_err(|e| {
        format!("the verified worktree's head could not be read, so nothing was posted: {e}")
    })?;
    if !crate::engine::git_writer::is_full_oid(head) {
        return Err(
            "the verified worktree's head is not a commit id, so nothing was posted".into(),
        );
    }
    let record = match &facts.record {
        Err(_) => return Ok(error_plan(head, "the verdict could not be read", None)),
        Ok(None) => {
            return Ok(error_plan(
                head,
                "no verification was recorded for this commit",
                None,
            ))
        }
        Ok(Some(record)) => record,
    };
    if record.item_id != facts.item_id {
        return Err(format!(
            "the newest verification record is for another task ({:?}); nothing was posted",
            word(&record.item_id)
        ));
    }
    if !crate::engine::git_writer::is_full_oid(&record.head) {
        return Err("the verification record names no commit, so nothing was posted".into());
    }
    let sha = record.head.as_str();
    if record.state == STATE_COULD_NOT_RUN {
        return Ok(error_plan(sha, "verification could not run", Some(record)));
    }
    if record.state != "settled" {
        return Ok(error_plan(
            sha,
            "verification has not finished",
            Some(record),
        ));
    }
    if head != sha {
        return Err(format!(
            "verification was taken at {} but the worktree is now at {}; verify again. Nothing was posted",
            &sha[..12],
            &head[..12]
        ));
    }
    match facts.dirty {
        Err(ref e) => {
            return Err(format!(
                "whether the worktree has uncommitted changes could not be read, so nothing was posted: {e}"
            ))
        }
        Ok(true) => {
            return Ok(error_plan(
                sha,
                "the worktree has uncommitted changes, so the verdict is not about this commit",
                Some(record),
            ))
        }
        Ok(false) => {}
    }
    let met = record.criteria.iter().filter(|c| c.status == "met").count();
    let total = record.criteria.len();
    let (state, headline) = match record.outcome.as_str() {
        "passed" => ("success", format!("passed ({met} of {total} criteria met)")),
        "failed" => ("failure", format!("failed ({met} of {total} criteria met)")),
        _ => (
            "error",
            "verification was degraded and is not a pass".to_string(),
        ),
    };
    Ok(Plan {
        sha: sha.to_string(),
        state,
        description: describe(&headline),
        comment: comment(state, &headline, sha, Some(record)),
    })
}

/// How the requests reach GitHub. The real one is `gh`; a test supplies its own.
pub(crate) trait Gh {
    fn run(&self, args: &[String]) -> Result<String, String>;
}

pub(crate) struct RealGh<'a> {
    pub repo: &'a Path,
}

impl Gh for RealGh<'_> {
    /// Every call is judged by the command gate first, as each other `gh`
    /// mutation GitPulse makes is (`harness::guard_command`), then runs through
    /// the one `gh` runner (`github::actions::run_gh_in`).
    fn run(&self, args: &[String]) -> Result<String, String> {
        let repo = self
            .repo
            .to_str()
            .ok_or_else(|| "the repository path is not UTF-8".to_string())?;
        let mut argv: Vec<String> = Vec::with_capacity(args.len() + 1);
        argv.push("gh".into());
        argv.extend(args.iter().cloned());
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        crate::engine::git_cli::mark_user_action(repo);
        crate::harness::guard_command(repo, &refs)?;
        if let Some(reason) = crate::github::gh_unavailable_reason(&crate::github::probe_gh_cli()) {
            return Err(reason);
        }
        crate::github::actions::run_gh_in(repo, &argv).map_err(|e| bounded(&e))
    }
}

fn bounded(text: &str) -> String {
    text.chars()
        .take(crate::github::MAX_GH_ERROR_BYTES.min(600))
        .collect()
}

fn endpoint(remote: &GitHubRepoRef, tail: &str) -> String {
    format!("repos/{}/{}/{tail}", remote.owner, remote.name)
}

/// The `gh api` argv (program name omitted) that posts the commit status.
pub(crate) fn status_argv(remote: &GitHubRepoRef, plan: &Plan) -> Result<Vec<String>, String> {
    if !crate::engine::git_writer::is_full_oid(&plan.sha) {
        return Err("not a commit id".into());
    }
    if !matches!(plan.state, "success" | "failure" | "error") {
        return Err("not a postable state".into());
    }
    let mut args: Vec<String> = vec![
        "api".into(),
        "-X".into(),
        "POST".into(),
        endpoint(remote, &format!("statuses/{}", plan.sha)),
        "-f".into(),
        format!("state={}", plan.state),
        "-f".into(),
        format!("context={CONTEXT}"),
        "-f".into(),
        format!("description={}", plan.description),
    ];
    args.extend(crate::github::api_host_flags(remote));
    Ok(args)
}

fn pulls_argv(remote: &GitHubRepoRef, sha: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "api".into(),
        endpoint(remote, &format!("commits/{sha}/pulls")),
        "--jq".into(),
        r#".[] | select(.state == "open") | .number"#.into(),
    ];
    args.extend(crate::github::api_host_flags(remote));
    args
}

fn comment_argv(remote: &GitHubRepoRef, number: u64, body: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "api".into(),
        "-X".into(),
        "POST".into(),
        endpoint(remote, &format!("issues/{number}/comments")),
        "-f".into(),
        format!("body={body}"),
    ];
    args.extend(crate::github::api_host_flags(remote));
    args
}

/// What a post did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PostedVerifyStatus {
    pub sha: String,
    pub state: String,
    pub description: String,
    pub dry_run: bool,
    pub status_posted: bool,
    /// Pull requests a comment was posted on.
    pub commented: Vec<u64>,
    /// Why a requested comment was not posted, when it was not.
    pub comment_note: Option<String>,
    /// Dry run only: the `gh` argv lists that would run, nothing sent.
    pub would_run: Vec<Vec<String>>,
}

/// Posts `plan`. The status goes first and its failure is the call's failure;
/// a comment that fails after a posted status is reported beside it.
pub(crate) fn post(
    remote: &GitHubRepoRef,
    plan: &Plan,
    with_comment: bool,
    dry_run: bool,
    gh: &dyn Gh,
) -> Result<PostedVerifyStatus, String> {
    let status = status_argv(remote, plan)?;
    let mut posted = PostedVerifyStatus {
        sha: plan.sha.clone(),
        state: plan.state.to_string(),
        description: plan.description.clone(),
        dry_run,
        status_posted: false,
        commented: Vec::new(),
        comment_note: None,
        would_run: Vec::new(),
    };
    if dry_run {
        posted.would_run.push(status);
        if with_comment {
            posted.would_run.push(pulls_argv(remote, &plan.sha));
            posted.comment_note = Some("pull requests are looked up when posting for real".into());
        }
        return Ok(posted);
    }
    gh.run(&status)
        .map_err(|e| format!("GitHub did not accept the status: {e}"))?;
    posted.status_posted = true;
    if !with_comment {
        return Ok(posted);
    }
    let numbers = match gh.run(&pulls_argv(remote, &plan.sha)) {
        Ok(out) => out
            .lines()
            .filter_map(|l| l.trim().parse::<u64>().ok())
            .take(MAX_PULLS)
            .collect::<Vec<_>>(),
        Err(e) => {
            posted.comment_note = Some(format!("pull requests could not be listed: {e}"));
            return Ok(posted);
        }
    };
    if numbers.is_empty() {
        posted.comment_note = Some("no open pull request contains this commit".into());
        return Ok(posted);
    }
    for number in numbers {
        match gh.run(&comment_argv(remote, number, &plan.comment)) {
            Ok(_) => posted.commented.push(number),
            Err(e) => {
                posted.comment_note = Some(format!("the comment on #{number} failed: {e}"));
                break;
            }
        }
    }
    Ok(posted)
}

/// The whole request, from a person's click. `consent` is the repository's
/// opt-in as read from settings (an unreadable setting is an error, never a yes).
pub(crate) fn post_for_task(
    store: Result<&dc_store::Store, String>,
    repo_path: &str,
    item_id: &str,
    consent: Result<bool, String>,
    with_comment: bool,
    dry_run: bool,
    gh: &dyn Gh,
) -> Result<PostedVerifyStatus, String> {
    match consent {
        Ok(true) => {}
        Ok(false) => return Err(
            "posting verification results to GitHub is off for this repository; turn it on first"
                .into(),
        ),
        Err(e) => {
            return Err(format!(
                "the opt-in setting could not be read, so nothing was posted: {e}"
            ))
        }
    }
    let local = super::intake::resolve(repo_path).map_err(|e| e.message)?;
    let remote = crate::github::discover_github_remote(repo_path)?
        .ok_or_else(|| "No GitHub remote configured".to_string())?;
    let record = match store {
        Ok(store) => super::verification_gate::latest(store, item_id),
        Err(e) => Err(e),
    };
    // The worktree the verdict was taken in when the record names one of this
    // repository's, else the one the person is looking at.
    let worktree = match &record {
        Ok(Some(r)) if !r.worktree.trim().is_empty() => match super::intake::resolve(&r.worktree) {
            Ok(found) if found.identity == local.identity => found.path,
            _ => {
                return Err(
                    "the verified worktree belongs to another repository; nothing was posted"
                        .into(),
                )
            }
        },
        _ => local.path.clone(),
    };
    let facts = Facts {
        item_id,
        record,
        head: super::verification_gate::current_head(&worktree),
        dirty: crate::engine::git_cli::git_text(Path::new(&worktree), &["status", "--porcelain"])
            .map(|out| !out.trim().is_empty()),
    };
    let plan = plan(&facts)?;
    post(&remote, &plan, with_comment, dry_run, gh)
}

/// The desktop command's body: the repository's opt-in from settings, the
/// store the app already holds open, and the real `gh`.
pub(crate) fn post_via_app(
    state: &super::WorkbenchState,
    repo_path: &str,
    item_id: &str,
    with_comment: bool,
    dry_run: bool,
) -> Result<PostedVerifyStatus, String> {
    let consent = enabled_for(repo_path);
    let repo = crate::engine::git_cli::validate_repo(repo_path)?;
    let gh = RealGh { repo: &repo };
    let mut outcome = None;
    let read = state.with_store(|store| {
        outcome = Some(post_for_task(
            Ok(store),
            repo_path,
            item_id,
            consent.clone(),
            with_comment,
            dry_run,
            &gh,
        ));
        Ok(())
    });
    match (outcome, read) {
        (Some(result), _) => result,
        (None, Err(e)) => post_for_task(
            Err(e.message),
            repo_path,
            item_id,
            consent,
            with_comment,
            dry_run,
            &gh,
        ),
        (None, Ok(())) => Err("the store was not consulted".into()),
    }
}

/// Whether `repo_path`'s repository has opted in.
pub(crate) fn enabled_for(repo_path: &str) -> Result<bool, String> {
    let dir = crate::harness::review::common_dir(Path::new(repo_path))?;
    crate::tool_config::verify_status_enabled(&dir)
}

/// Opts `repo_path`'s repository in or out.
pub(crate) fn set_enabled(repo_path: &str, enabled: bool) -> Result<(), String> {
    let dir = crate::harness::review::common_dir(Path::new(repo_path))?;
    crate::tool_config::set_verify_status(&dir, enabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::verification_gate::Criterion;
    use std::cell::RefCell;

    const SHA: &str = "1111111111111111111111111111111111111111";
    const OTHER: &str = "2222222222222222222222222222222222222222";
    const SECRET: &str = "AWS_SECRET_ACCESS_KEY=abcd1234 diff --git a/x b/x @@ -1 +1 @@";

    fn remote() -> GitHubRepoRef {
        GitHubRepoRef {
            host: "github.com".into(),
            owner: "o".into(),
            name: "r".into(),
        }
    }

    fn record(outcome: &str, state: &str) -> Record {
        Record {
            item_id: "task".into(),
            state: state.into(),
            outcome: outcome.into(),
            head: SHA.into(),
            worktree: "/w".into(),
            criteria: vec![
                Criterion {
                    index: 1,
                    text: format!("exports csv {SECRET}"),
                    status: "met".into(),
                    findings: vec![],
                },
                Criterion {
                    index: 2,
                    text: "second".into(),
                    status: if outcome == "passed" { "met" } else { "unmet" }.into(),
                    findings: vec![SECRET.into()],
                },
            ],
            degraded: if outcome == "degraded" {
                vec![format!("critic failed: {SECRET}")]
            } else {
                vec![]
            },
            critic_summary: SECRET.into(),
            ..Record::default()
        }
    }

    fn facts(record: Result<Option<Record>, String>) -> Facts<'static> {
        Facts {
            item_id: "task",
            record,
            head: Ok(SHA.into()),
            dirty: Ok(false),
        }
    }

    fn planned(record: Record) -> Plan {
        plan(&facts(Ok(Some(record)))).unwrap()
    }

    #[test]
    fn a_settled_pass_for_the_current_clean_commit_posts_success() {
        let p = planned(record("passed", "settled"));
        assert_eq!((p.state, p.sha.as_str()), ("success", SHA));
        assert_eq!(
            p.description,
            "devcouncil verify: passed (2 of 2 criteria met)"
        );
    }

    #[test]
    fn a_failed_verdict_posts_failure_and_names_only_counts() {
        let p = planned(record("failed", "settled"));
        assert_eq!(p.state, "failure");
        assert!(p.description.contains("1 of 2"), "{}", p.description);
    }

    #[test]
    fn a_verdict_that_was_not_established_never_posts_success() {
        for (label, f) in [
            ("degraded", facts(Ok(Some(record("degraded", "settled"))))),
            (
                "could not run",
                facts(Ok(Some(record("", "could_not_run")))),
            ),
            ("not settled", facts(Ok(Some(record("passed", "running"))))),
            ("never recorded", facts(Ok(None))),
            (
                "store unreadable",
                facts(Err("disk I/O error at /secret/path".into())),
            ),
            (
                "outcome unknown",
                facts(Ok(Some(record("maybe", "settled")))),
            ),
        ] {
            let p = plan(&f).unwrap_or_else(|e| panic!("{label}: {e}"));
            assert_eq!(p.state, "error", "{label}");
            assert!(
                !p.description.contains("passed"),
                "{label}: {}",
                p.description
            );
        }
    }

    #[test]
    fn a_dirty_worktree_posts_error_naming_it_never_success() {
        let mut f = facts(Ok(Some(record("passed", "settled"))));
        f.dirty = Ok(true);
        let p = plan(&f).unwrap();
        assert_eq!(p.state, "error");
        assert!(p.description.contains("uncommitted"), "{}", p.description);
    }

    #[test]
    fn a_verdict_that_cannot_be_tied_to_the_commit_posts_nothing() {
        let mut moved = facts(Ok(Some(record("passed", "settled"))));
        moved.head = Ok(OTHER.into());
        assert!(plan(&moved).unwrap_err().contains("verify again"));
        let mut unreadable_head = facts(Ok(Some(record("passed", "settled"))));
        unreadable_head.head = Err("not a repository".into());
        assert!(plan(&unreadable_head).is_err());
        let mut unreadable_status = facts(Ok(Some(record("passed", "settled"))));
        unreadable_status.dirty = Err("status failed".into());
        assert!(plan(&unreadable_status).is_err());
        let mut other_task = record("passed", "settled");
        other_task.item_id = "another".into();
        assert!(plan(&facts(Ok(Some(other_task)))).is_err());
        let mut no_commit = record("passed", "settled");
        no_commit.head = "not-a-sha".into();
        assert!(plan(&facts(Ok(Some(no_commit)))).is_err());
    }

    #[test]
    fn what_is_posted_never_carries_command_output_environment_or_diff_text() {
        for (outcome, state) in [
            ("passed", "settled"),
            ("failed", "settled"),
            ("degraded", "settled"),
            ("", "could_not_run"),
        ] {
            let p = planned(record(outcome, state));
            for text in [&p.description, &p.comment] {
                for leaked in [
                    "AWS_SECRET",
                    "abcd1234",
                    "diff --git",
                    "@@",
                    "exports csv",
                    "critic failed",
                ] {
                    assert!(
                        !text.contains(leaked),
                        "{outcome}/{state}: {leaked} in {text}"
                    );
                }
            }
            assert!(p.description.chars().count() <= 140);
        }
        let mut hostile = record("passed", "settled");
        hostile.criteria[0].status = "met\n`; rm -rf /`".into();
        let p = planned(hostile);
        assert!(!p.comment.contains("rm -rf"), "{}", p.comment);
    }

    #[test]
    fn the_description_never_exceeds_githubs_limit() {
        assert!(describe(&"x".repeat(500)).chars().count() <= 140);
    }

    struct FakeGh {
        calls: RefCell<Vec<Vec<String>>>,
        /// 1-based call numbers that fail.
        fail: Vec<usize>,
        pulls: String,
    }
    impl FakeGh {
        fn new(pulls: &str, fail: &[usize]) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                fail: fail.to_vec(),
                pulls: pulls.into(),
            }
        }
    }
    impl Gh for FakeGh {
        fn run(&self, args: &[String]) -> Result<String, String> {
            self.calls.borrow_mut().push(args.to_vec());
            let n = self.calls.borrow().len();
            if self.fail.contains(&n) {
                return Err("HTTP 403: Resource not accessible".into());
            }
            Ok(if args.iter().any(|a| a.contains("/pulls")) {
                self.pulls.clone()
            } else {
                String::new()
            })
        }
    }

    fn success_plan() -> Plan {
        planned(record("passed", "settled"))
    }

    #[test]
    fn the_status_goes_through_gh_api_with_the_exact_commit_and_context() {
        let gh = FakeGh::new("", &[]);
        let posted = post(&remote(), &success_plan(), false, false, &gh).unwrap();
        assert!(posted.status_posted);
        let calls = gh.calls.borrow();
        assert_eq!(calls.len(), 1);
        let argv = &calls[0];
        assert_eq!(
            &argv[..4],
            ["api", "-X", "POST", &format!("repos/o/r/statuses/{SHA}")]
        );
        assert!(argv.contains(&"state=success".to_string()));
        assert!(argv.contains(&"context=devcouncil/verify".to_string()));
    }

    #[test]
    fn a_github_failure_is_an_error_not_a_success() {
        let gh = FakeGh::new("", &[1]);
        let error = post(&remote(), &success_plan(), true, false, &gh).unwrap_err();
        assert!(error.contains("did not accept the status"), "{error}");
        assert_eq!(
            gh.calls.borrow().len(),
            1,
            "no comment after a refused status"
        );
    }

    #[test]
    fn a_dry_run_runs_nothing() {
        let gh = FakeGh::new("", &[]);
        let posted = post(&remote(), &success_plan(), true, true, &gh).unwrap();
        assert!(posted.dry_run && !posted.status_posted);
        assert!(gh.calls.borrow().is_empty());
        assert_eq!(posted.would_run.len(), 2);
    }

    #[test]
    fn a_comment_lands_on_each_open_pull_request_and_its_failure_is_reported() {
        let gh = FakeGh::new("7\n9\n", &[]);
        let posted = post(&remote(), &success_plan(), true, false, &gh).unwrap();
        assert_eq!(posted.commented, vec![7, 9]);
        let comment_calls: Vec<_> = gh
            .calls
            .borrow()
            .iter()
            .filter(|a| a[3].contains("issues/"))
            .cloned()
            .collect();
        assert_eq!(comment_calls.len(), 2);
        let failing = FakeGh::new("7\n", &[3]);
        let posted = post(&remote(), &success_plan(), true, false, &failing).unwrap();
        assert!(posted.status_posted && posted.commented.is_empty());
        assert!(posted.comment_note.unwrap().contains("#7"));
        let none = FakeGh::new("", &[]);
        let posted = post(&remote(), &success_plan(), true, false, &none).unwrap();
        assert!(posted
            .comment_note
            .unwrap()
            .contains("no open pull request"));
    }

    #[test]
    fn a_state_or_commit_gh_should_not_be_given_is_refused_before_building_argv() {
        let mut plan = success_plan();
        plan.sha = "--paginate".into();
        assert!(status_argv(&remote(), &plan).is_err());
        let mut plan = success_plan();
        plan.state = "pending";
        assert!(status_argv(&remote(), &plan).is_err());
    }

    #[test]
    fn an_enterprise_host_is_pinned() {
        let ghe = GitHubRepoRef {
            host: "example.ghe.com".into(),
            owner: "o".into(),
            name: "r".into(),
        };
        let argv = status_argv(&ghe, &success_plan()).unwrap();
        assert!(argv
            .windows(2)
            .any(|w| w == ["--hostname", "example.ghe.com"]));
    }

    fn repo_with_commit() -> tempfile::TempDir {
        let repo = crate::test_support::git_repo();
        crate::test_support::git_in(
            repo.path(),
            &["commit", "-q", "--allow-empty", "-m", "init"],
        );
        crate::test_support::git_in(
            repo.path(),
            &["remote", "add", "origin", "https://github.com/o/r.git"],
        );
        repo
    }

    fn evidence_store(
        repo: &tempfile::TempDir,
        outcome: &str,
    ) -> (tempfile::TempDir, dc_store::Store, String) {
        use crate::workbench::intake::gate_tests::{head_of, record, store_record};
        let dir = tempfile::tempdir().unwrap();
        let store = dc_store::Store::open(dir.path().join("w.sqlite")).unwrap();
        let head = head_of(repo);
        store_record(
            &store,
            "task",
            &record("task", outcome, "settled", repo, &head),
        );
        (dir, store, head)
    }

    #[test]
    fn it_never_posts_without_the_repositorys_opt_in() {
        let repo = repo_with_commit();
        let (_d, store, _) = evidence_store(&repo, "passed");
        let path = repo.path().to_str().unwrap();
        for consent in [Ok(false), Err("unreadable".to_string())] {
            let gh = FakeGh::new("", &[]);
            let error =
                post_for_task(Ok(&store), path, "task", consent, true, false, &gh).unwrap_err();
            assert!(
                error.contains("off for this repository") || error.contains("could not be read"),
                "{error}"
            );
            assert!(gh.calls.borrow().is_empty());
        }
    }

    #[test]
    fn a_recorded_pass_on_a_clean_checkout_posts_success_for_its_head() {
        let repo = repo_with_commit();
        let (_d, store, head) = evidence_store(&repo, "passed");
        let gh = FakeGh::new("", &[]);
        let posted = post_for_task(
            Ok(&store),
            repo.path().to_str().unwrap(),
            "task",
            Ok(true),
            false,
            false,
            &gh,
        )
        .unwrap();
        assert_eq!(
            (posted.state.as_str(), posted.sha.as_str()),
            ("success", head.as_str())
        );
        assert!(gh.calls.borrow()[0].contains(&format!("repos/o/r/statuses/{head}")));
    }

    #[test]
    fn an_edit_after_the_pass_downgrades_it_to_error() {
        let repo = repo_with_commit();
        let (_d, store, _) = evidence_store(&repo, "passed");
        std::fs::write(repo.path().join("scratch.txt"), "uncommitted").unwrap();
        let gh = FakeGh::new("", &[]);
        let posted = post_for_task(
            Ok(&store),
            repo.path().to_str().unwrap(),
            "task",
            Ok(true),
            false,
            false,
            &gh,
        )
        .unwrap();
        assert_eq!(posted.state, "error");
        assert!(gh.calls.borrow()[0].contains(&"state=error".to_string()));
    }

    #[test]
    fn an_unreadable_store_posts_error_for_the_current_head_not_success() {
        let repo = repo_with_commit();
        let gh = FakeGh::new("", &[]);
        let posted = post_for_task(
            Err("down".into()),
            repo.path().to_str().unwrap(),
            "task",
            Ok(true),
            false,
            false,
            &gh,
        )
        .unwrap();
        assert_eq!(posted.state, "error");
    }

    #[test]
    fn a_repository_without_a_github_remote_posts_nothing() {
        let repo = crate::test_support::git_repo();
        crate::test_support::git_in(
            repo.path(),
            &["commit", "-q", "--allow-empty", "-m", "init"],
        );
        let gh = FakeGh::new("", &[]);
        let error = post_for_task(
            Err("x".into()),
            repo.path().to_str().unwrap(),
            "task",
            Ok(true),
            false,
            false,
            &gh,
        )
        .unwrap_err();
        assert!(error.contains("No GitHub remote"), "{error}");
        assert!(gh.calls.borrow().is_empty());
    }

    #[test]
    fn no_mcp_tool_posts_to_github() {
        for tool in crate::mcp::tools() {
            let name = tool["name"].as_str().unwrap_or_default().to_lowercase();
            assert!(
                !name.contains("verify_status") && !name.contains("post_verify"),
                "{name}"
            );
            let text = tool.to_string().to_lowercase();
            assert!(
                !text.contains("devcouncil/verify"),
                "{name} mentions the status context"
            );
        }
    }
    /// The real `gh` runner goes through the command gate like every other
    /// mutation GitPulse makes: a `gh api` POST that skipped it would be an
    /// unjudged, unrecorded write to GitHub. Whether or not `gh` is installed,
    /// the gate row must exist before anything is spawned.
    #[test]
    fn the_real_gh_runner_leaves_a_gate_row_before_it_runs() {
        let _serial = crate::harness::sidecar::test_serial();
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().canonicalize().expect("canonical path");
        let repo_str = repo.to_str().expect("utf8 path");
        let before = crate::ledger::latest_cursor(repo_str).expect("cursor");
        let argv = status_argv(&remote(), &success_plan()).unwrap();
        let _ = RealGh { repo: &repo }.run(&argv);
        let events = crate::ledger::tail(repo_str, before, 10).expect("tail");
        assert_eq!(events.len(), 1, "the gh call reached the gate once");
        let row = &events[0];
        assert_eq!(row.action, "command.run");
        let recorded: Vec<String> =
            serde_json::from_str(row.argv_json.as_deref().expect("argv recorded")).unwrap();
        assert_eq!(recorded.first().map(String::as_str), Some("gh"));
        assert_eq!(&recorded[1..], argv.as_slice());
    }
}
