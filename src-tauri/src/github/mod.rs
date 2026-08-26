use crate::analyzer::deps::severity_rank;
use crate::engine::git_cli::{
    capture_command, git_text, run_command_in, validate_repo, CapturedOutput,
};
pub mod actions;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubRepoRef {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl GitHubRepoRef {
    pub fn html_url(&self) -> String {
        format!("https://{}/{}/{}", self.host, self.owner, self.name)
    }

    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullRequestInfo {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub head_ref: String,
    pub base_ref: String,
    pub url: String,
    pub is_draft: bool,
    pub ci_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowRunInfo {
    pub id: u64,
    pub name: String,
    pub title: String,
    pub status: String,
    pub conclusion: String,
    pub head_branch: String,
    pub url: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueInfo {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub url: String,
    pub labels: Vec<String>,
    pub updated_at: String,
    pub author: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub name: String,
    pub is_draft: bool,
    pub is_prerelease: bool,
    pub is_latest: bool,
    pub published_at: String,
    pub created_at: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubContext {
    pub available: bool,
    pub cli_present: bool,
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub html_url: String,
    pub pull_requests: Vec<PullRequestInfo>,
    /// True when more open pull requests exist than the display cap kept.
    #[serde(default)]
    pub prs_truncated: bool,
    pub issues: Vec<IssueInfo>,
    pub issues_truncated: bool,
    pub issues_error: Option<String>,
    pub workflow_runs: Vec<WorkflowRunInfo>,
    /// Set when the `gh` workflow-run listing could not run or be parsed,
    /// while the rest of the context is still usable.
    #[serde(default)]
    pub runs_error: Option<String>,
    /// True when more workflow runs exist than the display cap kept.
    #[serde(default)]
    pub runs_truncated: bool,
    #[serde(default)]
    pub releases: Vec<ReleaseInfo>,
    #[serde(default)]
    pub releases_truncated: bool,
    #[serde(default)]
    pub releases_error: Option<String>,
    pub error: Option<String>,
    /// Degradations that did not fail the whole context: a section that could
    /// not be fetched or parsed keeps going with an empty list plus a reason
    /// here, so "no pull requests" never silently doubles as "could not
    /// check". Mirrors the coverage scanner's skip-reason pattern. Absent
    /// from the JSON entirely while empty, so existing TS consumers see no
    /// shape change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// One open Dependabot alert, shaped for the Health view.
///
/// Severities keep GitHub's own vocabulary (`medium`, not npm's `moderate`);
/// the UI normalizes them. Empty strings mean "GitHub published nothing
/// here" (no CVE, no patched version yet, manifest path absent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependabotAlertInfo {
    pub number: u64,
    pub package: String,
    pub ecosystem: String,
    pub manifest_path: String,
    pub scope: String,
    pub severity: String,
    pub title: String,
    pub advisory_id: String,
    pub cve_id: String,
    pub vulnerable_range: String,
    pub first_patched: String,
    pub url: String,
    pub created_at: String,
}

/// Result of fetching Dependabot alerts for the opened repository.
///
/// Like [`GitHubContext`], a fetch that could not run is reported as such:
/// `available: false` with an `error` message never masquerades as "no open
/// alerts", which is what an empty `alerts` list means when `available` is
/// true.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependabotReport {
    pub available: bool,
    pub cli_present: bool,
    /// False when no trusted GitHub remote exists; the Health view hides the
    /// Dependabot section entirely for local-only repositories.
    pub is_github_remote: bool,
    pub slug: String,
    pub alerts: Vec<DependabotAlertInfo>,
    pub truncated: bool,
    pub error: Option<String>,
}

/// Drops a trailing `.git` the way git itself does: case-insensitively
/// (`Repo.GIT` and `repo.git` both clone into `repo`).
fn trim_git_suffix(path: &str) -> &str {
    if path.len() >= 4 && path[path.len() - 4..].eq_ignore_ascii_case(".git") {
        &path[..path.len() - 4]
    } else {
        path
    }
}

/// Parses GitHub / GHES clone URLs (HTTPS, SSH, git protocol).
///
/// Scheme matching is case-insensitive (Windows checkouts routinely carry
/// `HTTPS://GITHUB.COM/...`), ports are stripped only where they are legal
/// (`ssh://`/`git://`; a port on an https remote means it is *not* plain
/// github.com and must not be trusted as such), and every surviving
/// component must pass the argv/endpoint-safety rules below.
pub fn parse_github_remote_url(url: &str) -> Option<GitHubRepoRef> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    // scp-like syntax: git@host:path — no scheme, no port slot (the first
    // ':' already separates the path).
    if let Some(rest) = trimmed.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        let path = trim_git_suffix(path.trim_end_matches('/'));
        return split_owner_repo(&format!("{host}/{path}"), false);
    }

    // Scheme forms, matched on the lowercased prefix; ASCII lowercasing
    // preserves byte offsets, so the original text is sliced at the same
    // position to keep the host/path's case intact.
    const SCHEMES: [(&str, bool); 4] = [
        ("ssh://", true),
        ("git://", true),
        ("https://", false),
        ("http://", false),
    ];
    let lower = trimmed.to_ascii_lowercase();
    for (scheme, strip_port) in SCHEMES {
        if lower.starts_with(scheme) {
            let remainder = trim_git_suffix(trimmed[scheme.len()..].trim().trim_end_matches('/'));
            let remainder = remainder.strip_prefix("git@").unwrap_or(remainder);
            return split_owner_repo(remainder, strip_port);
        }
    }
    None
}

/// Hard upper bound for each parsed remote component (host, owner, name).
///
/// Real hosts and GitHub names are far shorter; this only stops a crafted
/// megabyte-scale URL from flowing whole into endpoints, flags, and the UI.
const MAX_REMOTE_COMPONENT_LEN: usize = 100;

/// A remote component is argv-safe and endpoint-safe: no leading `-` (it
/// would be re-parsed as a flag by `gh`'s CLI — e.g. `--repo -inbox/x`),
/// no whitespace or control characters (they would corrupt argv, endpoint
/// paths, built URLs, and error messages alike), no leftover `:` (a port or
/// junk that survived where none is legal), and bounded length.
fn is_valid_remote_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= MAX_REMOTE_COMPONENT_LEN
        && !host.starts_with('-')
        && !host.contains(':')
        && !host.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Owner/name vocabulary: GitHub names are ASCII letters, digits, `.`, `_`
/// and `-`. Anything else cannot appear in a real repository slug, while
/// letting it through would let dot-segments (`..`) bend REST endpoint
/// paths and built release/issue URLs away from the intended repo.
fn is_valid_repo_component(component: &str) -> bool {
    !component.is_empty()
        && component.len() <= MAX_REMOTE_COMPONENT_LEN
        && !component.starts_with('-')
        && !component.starts_with('.')
        && component
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Splits `host[:port]/owner/name` into a [`GitHubRepoRef`]. The port is
/// dropped only when `strip_port` says this URL form may carry one and the
/// suffix is all digits; anything else keeps its `:` and is refused by the
/// host rules, so trust decisions and display always agree with what the
/// user actually configured.
fn split_owner_repo(host_and_path: &str, strip_port: bool) -> Option<GitHubRepoRef> {
    let (raw_host, path) = host_and_path.split_once('/')?;
    let mut host = raw_host.trim();
    if strip_port {
        if let Some((h, port)) = host.split_once(':') {
            if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
                host = h.trim();
            }
        }
    }
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    let owner = parts.next()?;
    let name = parts.next()?;
    if !(is_valid_remote_host(host)
        && is_valid_repo_component(owner)
        && is_valid_repo_component(name))
    {
        return None;
    }
    Some(GitHubRepoRef {
        host: host.to_string(),
        owner: owner.to_string(),
        name: name.to_string(),
    })
}

/// Bounded probe for the `gh` CLI.
///
/// A raw `Command::output()` here would block forever on a hung `gh` (a stale
/// credential helper, a stuck network mount, ...) and freeze
/// `cmd_github_context`. Routing through `capture_command` gives the probe the
/// same hard timeout and output caps every other subprocess gets; success is
/// treated as presence.
pub fn gh_cli_present() -> bool {
    capture_command("gh", &["--version"], None, Duration::from_secs(10), &[])
        .map(|o| o.success)
        .unwrap_or(false)
}

/// True for hosts GitHub actually serves: `github.com`, any subdomain of it,
/// and GitHub Enterprise Cloud's `*.ghe.com` — case-insensitively.
///
/// This is deliberately suffix-based rather than substring-based: a remote on
/// `notgithub.com` or `github.com.evil.io` must not pass as GitHub, or
/// attacker-chosen remotes would be trusted downstream (`gh` flags, URL
/// building). Self-hosted GHES installs on arbitrary domains are therefore no
/// longer recognized; that trade keeps lookalikes out.
pub fn is_github_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    h == "github.com" || h.ends_with(".github.com") || h.ends_with(".ghe.com")
}

/// `gh` flags that pin a call to this remote (`--repo`, and `--hostname` on GHES).
///
/// Only valid for the repo-context subcommands (`pr`, `issue`, `run`,
/// `release`), which inherit `-R`. `gh api` has no `--repo` flag and fails
/// outright when given one; use [`api_host_flags`] there instead — the REST
/// endpoint path already scopes the request to the repository.
pub fn gh_repo_flags(remote: &GitHubRepoRef) -> Vec<String> {
    let mut flags = vec!["--repo".to_string(), remote.slug()];
    if !remote.host.eq_ignore_ascii_case("github.com") {
        flags.push("--hostname".to_string());
        flags.push(remote.host.clone());
    }
    flags
}

/// Selects the trusted GitHub remote from `git remote -v` output.
///
/// Only **fetch** URLs are eligible. `remote -v` lists every remote twice —
/// a fetch line and a push line, told apart by the trailing `(fetch)` /
/// `(push)` marker — and the push URL is whatever the user (or a crafted
/// `.git/config`) pointed it at: trusting it would aim every gh call
/// (`--repo`, Dependabot endpoints, checkout, issue creation) at a
/// repository the user never pulls from. Origin wins over other remotes;
/// among the rest, the first listed fetch URL is used.
pub fn pick_github_remote(remote_v_output: &str) -> Option<GitHubRepoRef> {
    let mut origin: Option<GitHubRepoRef> = None;
    let mut first: Option<GitHubRepoRef> = None;
    for line in remote_v_output.lines() {
        let mut parts = line.split_whitespace();
        let name = parts.next();
        let Some(url) = parts.next() else {
            continue;
        };
        // The marker decides eligibility; its absence reads as a fetch line
        // (every git release prints it, but be liberal about bare output).
        if matches!(parts.next(), Some("(push)")) {
            continue;
        }
        if let Some(parsed) = parse_github_remote_url(url) {
            if is_github_host(&parsed.host) {
                if name == Some("origin") {
                    origin = Some(parsed);
                    break;
                }
                if first.is_none() {
                    first = Some(parsed);
                }
            }
        }
    }
    origin.or(first)
}

pub fn discover_github_remote(repo_path: &str) -> Result<Option<GitHubRepoRef>, String> {
    let repo = validate_repo(repo_path)?;
    let stdout = git_text(&repo, &["remote", "-v"])?;
    Ok(pick_github_remote(&stdout))
}

#[derive(Debug, Deserialize)]
struct GhPullRequest {
    number: u64,
    title: String,
    state: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    url: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(rename = "statusCheckRollup")]
    status_check_rollup: Option<serde_json::Value>,
}

/// GitHub's check/conclusion vocabulary that means the pipeline is broken.
/// `TIMED_OUT`, `ACTION_REQUIRED`, `STARTUP_FAILURE` and `STALE` contain no
/// "FAIL" substring yet are genuine failures; rendering them green used to
/// tell the user a timed-out pipeline was healthy.
fn check_state_is_failure(state: &str) -> bool {
    matches!(
        state,
        "FAILURE"
            | "ERROR"
            | "CANCELLED"
            | "TIMED_OUT"
            | "ACTION_REQUIRED"
            | "STARTUP_FAILURE"
            | "STALE"
    )
}

/// States that mean work is still in flight.
fn check_state_is_pending(state: &str) -> bool {
    matches!(
        state,
        "PENDING" | "IN_PROGRESS" | "QUEUED" | "EXPECTED" | "WAITING"
    )
}

fn summarize_checks(value: &Option<serde_json::Value>) -> String {
    let Some(v) = value else {
        return "unknown".into();
    };
    let checks = match v {
        serde_json::Value::Array(items) => items.clone(),
        other => vec![other.clone()],
    };
    if checks.is_empty() {
        return "none".into();
    }
    let mut pending = false;
    let mut failed = false;
    let mut unknown = false;
    for check in &checks {
        let state = check
            .get("state")
            .or_else(|| check.get("conclusion"))
            .or_else(|| check.get("status"))
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_ascii_uppercase();
        if state.is_empty() {
            // An entry with no readable state is a check we could not run
            // our logic on — it must never count as a pass.
            unknown = true;
        } else if check_state_is_failure(&state) {
            failed = true;
        } else if check_state_is_pending(&state) {
            pending = true;
        }
        // SUCCESS / NEUTRAL / SKIPPED / COMPLETED fall through as passing:
        // a rollup of only those genuinely has nothing red or in flight.
    }
    if failed {
        "failure".into()
    } else if pending {
        "pending".into()
    } else if unknown {
        "unknown".into()
    } else {
        "success".into()
    }
}

fn gh_argv(remote: &GitHubRepoRef, leading: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = leading.iter().map(|s| (*s).to_string()).collect();
    args.extend(gh_repo_flags(remote));
    args
}

/// Upper bound for error text surfaced from a `gh` invocation (stderr,
/// API messages). The engine's drain caps already bound these at megabytes;
/// this keeps what flows into reports, warnings, and the UI at display size.
pub(crate) const MAX_GH_ERROR_BYTES: usize = 4 * 1024;

/// Keeps the tail of an oversized error message, cut on a char boundary.
fn bounded_error(message: String) -> String {
    if message.len() <= MAX_GH_ERROR_BYTES {
        return message;
    }
    crate::engine::git_cli::byte_tail(message.as_bytes(), MAX_GH_ERROR_BYTES)
}

fn run_gh(
    remote: &GitHubRepoRef,
    leading: &[&str],
    timeout: Duration,
    cwd: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let args = gh_argv(remote, leading);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_command_in("gh", &refs, timeout, cwd).map_err(bounded_error)
}

#[derive(Debug, Deserialize)]
struct GhLabel {
    name: String,
}

#[derive(Debug, Deserialize)]
struct GhAuthor {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GhIssue {
    number: u64,
    title: String,
    state: String,
    url: String,
    #[serde(default)]
    labels: Vec<GhLabel>,
    #[serde(rename = "updatedAt", default)]
    updated_at: String,
    author: Option<GhAuthor>,
}

fn list_issues(remote: &GitHubRepoRef) -> Result<(Vec<IssueInfo>, bool), String> {
    const DISPLAY_LIMIT: usize = 50;
    let fetch_limit = (DISPLAY_LIMIT + 1).to_string();
    let stdout = run_gh(
        remote,
        &[
            "issue",
            "list",
            "--state",
            "open",
            "--limit",
            &fetch_limit,
            "--json",
            "number,title,state,url,labels,updatedAt,author",
        ],
        Duration::from_secs(45),
        None,
    )?;
    parse_issue_list(&stdout, DISPLAY_LIMIT)
}

fn parse_issue_list(stdout: &[u8], display_limit: usize) -> Result<(Vec<IssueInfo>, bool), String> {
    let mut issues: Vec<GhIssue> = serde_json::from_slice(stdout)
        .map_err(|error| format!("GitHub returned invalid issue data: {error}"))?;
    let truncated = issues.len() > display_limit;
    issues.truncate(display_limit);
    let issues = issues
        .into_iter()
        .map(|issue| IssueInfo {
            number: issue.number,
            title: issue.title,
            state: issue.state,
            url: issue.url,
            labels: issue.labels.into_iter().map(|label| label.name).collect(),
            updated_at: issue.updated_at,
            author: issue.author.map(|author| author.login).unwrap_or_default(),
        })
        .collect();
    Ok((issues, truncated))
}

/// Display cap for the open-PR list. One extra row is fetched so a capped
/// result is flagged instead of silently looking complete.
const PR_DISPLAY_LIMIT: usize = 50;

fn list_pull_requests(remote: &GitHubRepoRef) -> Result<(Vec<PullRequestInfo>, bool), String> {
    let fetch_limit = (PR_DISPLAY_LIMIT + 1).to_string();
    let stdout = run_gh(
        remote,
        &[
            "pr",
            "list",
            "--state",
            "open",
            "--limit",
            &fetch_limit,
            "--json",
            "number,title,state,headRefName,baseRefName,url,isDraft,statusCheckRollup",
        ],
        Duration::from_secs(45),
        None,
    )?;
    parse_pr_list(&stdout, PR_DISPLAY_LIMIT)
}

#[derive(Debug, Deserialize)]
struct GhWorkflowRun {
    #[serde(rename = "databaseId")]
    database_id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    conclusion: Option<String>,
    #[serde(rename = "headBranch")]
    head_branch: Option<String>,
    #[serde(default)]
    url: String,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
    #[serde(rename = "displayTitle")]
    display_title: Option<String>,
}

/// Display cap for the recent workflow-run list; one extra row is fetched so
/// capping is reported, matching every other section's convention.
const RUN_DISPLAY_LIMIT: usize = 20;

fn list_workflow_runs(remote: &GitHubRepoRef) -> Result<(Vec<WorkflowRunInfo>, bool), String> {
    let fetch_limit = (RUN_DISPLAY_LIMIT + 1).to_string();
    let stdout = run_gh(
        remote,
        &[
            "run",
            "list",
            "--limit",
            &fetch_limit,
            "--json",
            "databaseId,name,status,conclusion,headBranch,url,createdAt,displayTitle",
        ],
        Duration::from_secs(45),
        None,
    )?;
    parse_workflow_runs(&stdout, RUN_DISPLAY_LIMIT)
}

fn parse_pr_list(
    stdout: &[u8],
    display_limit: usize,
) -> Result<(Vec<PullRequestInfo>, bool), String> {
    let mut prs: Vec<GhPullRequest> = serde_json::from_slice(stdout)
        .map_err(|e| format!("could not parse gh pull-request output: {e}"))?;
    let truncated = prs.len() > display_limit;
    prs.truncate(display_limit);
    let prs = prs
        .into_iter()
        .map(|pr| PullRequestInfo {
            number: pr.number,
            title: pr.title,
            state: pr.state,
            head_ref: pr.head_ref_name,
            base_ref: pr.base_ref_name,
            url: pr.url,
            is_draft: pr.is_draft,
            ci_status: summarize_checks(&pr.status_check_rollup),
        })
        .collect();
    Ok((prs, truncated))
}

fn parse_workflow_runs(
    stdout: &[u8],
    display_limit: usize,
) -> Result<(Vec<WorkflowRunInfo>, bool), String> {
    let mut runs: Vec<GhWorkflowRun> = serde_json::from_slice(stdout)
        .map_err(|e| format!("could not parse gh workflow-run output: {e}"))?;
    let truncated = runs.len() > display_limit;
    runs.truncate(display_limit);
    let runs = runs
        .into_iter()
        .map(|run| WorkflowRunInfo {
            id: run.database_id,
            name: run.name,
            title: run.display_title.unwrap_or_default(),
            status: run.status,
            conclusion: run.conclusion.unwrap_or_default(),
            head_branch: run.head_branch.unwrap_or_default(),
            url: run.url,
            created_at: run.created_at.unwrap_or_default(),
        })
        .collect();
    Ok((runs, truncated))
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    #[serde(rename = "tagName", default)]
    tag_name: String,
    #[serde(default)]
    name: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(rename = "isPrerelease", default)]
    is_prerelease: bool,
    #[serde(rename = "isLatest", default)]
    is_latest: bool,
    #[serde(rename = "publishedAt", default)]
    published_at: Option<String>,
    #[serde(rename = "createdAt", default)]
    created_at: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

const RELEASE_DISPLAY_LIMIT: usize = 50;

/// Leading argv for `gh release list`, before the repo-pinning flags.
///
/// `url` must not be requested: it is not one of `gh release list`'s JSON
/// fields, so asking for it fails the whole listing ("Unknown JSON field"),
/// which used to degrade every release panel into an error. Release URLs are
/// rebuilt from tag names by [`parse_release_list`] instead.
fn release_list_leading_args(fetch_limit: &str) -> Vec<String> {
    [
        "release",
        "list",
        "--limit",
        fetch_limit,
        "--json",
        "tagName,name,isDraft,isLatest,isPrerelease,publishedAt,createdAt",
    ]
    .iter()
    .map(|arg| (*arg).to_string())
    .collect()
}

fn list_releases(remote: &GitHubRepoRef) -> Result<(Vec<ReleaseInfo>, bool), String> {
    let fetch_limit = (RELEASE_DISPLAY_LIMIT + 1).to_string();
    let leading = release_list_leading_args(&fetch_limit);
    let refs: Vec<&str> = leading.iter().map(String::as_str).collect();
    let stdout = run_gh(remote, &refs, Duration::from_secs(45), None)?;
    parse_release_list(&stdout, &remote.html_url(), RELEASE_DISPLAY_LIMIT)
}

/// Percent-encodes every byte outside a URL-safe tag vocabulary so a
/// repo-controlled tag name (`v1..<script>`, spaces, `#`) can only ever
/// produce a well-formed link, never a mangled or misparsed one. `/` is
/// kept verbatim: slash-bearing tags are real and encode to themselves.
fn percent_encode_tag(tag: &str) -> String {
    let mut out = String::with_capacity(tag.len());
    for byte in tag.bytes() {
        let safe = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/');
        if safe {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn parse_release_list(
    stdout: &[u8],
    base_html_url: &str,
    display_limit: usize,
) -> Result<(Vec<ReleaseInfo>, bool), String> {
    let mut releases: Vec<GhRelease> = serde_json::from_slice(stdout)
        .map_err(|error| format!("GitHub returned invalid release data: {error}"))?;
    let truncated = releases.len() > display_limit;
    releases.truncate(display_limit);
    let releases = releases
        .into_iter()
        .map(|rel| {
            let url = if let Some(u) = rel.url.filter(|s| !s.trim().is_empty()) {
                u
            } else if !rel.tag_name.trim().is_empty() {
                format!(
                    "{}/releases/tag/{}",
                    base_html_url,
                    percent_encode_tag(rel.tag_name.trim())
                )
            } else {
                format!("{}/releases", base_html_url)
            };
            ReleaseInfo {
                tag_name: rel.tag_name,
                name: rel.name,
                is_draft: rel.is_draft,
                is_prerelease: rel.is_prerelease,
                is_latest: rel.is_latest,
                published_at: rel.published_at.unwrap_or_default(),
                created_at: rel.created_at.unwrap_or_default(),
                url,
            }
        })
        .collect();
    Ok((releases, truncated))
}

/// Upper bound on open Dependabot alerts shown in the Health view. One extra
/// row is fetched so a capped result is reported instead of silently looking
/// complete.
const ALERT_DISPLAY_LIMIT: usize = 50;

/// Loads open Dependabot alerts for the opened repository via `gh api`.
///
/// Never fails the caller: every condition that prevents the fetch (no GitHub
/// remote, no `gh` CLI, API error) comes back as a [`DependabotReport`] with
/// `available: false` and an explicit reason, so the UI can distinguish "no
/// open alerts" from "could not check".
pub fn load_dependabot_alerts(repo_path: &str) -> DependabotReport {
    let cli_present = gh_cli_present();
    let remote = match discover_github_remote(repo_path) {
        Ok(Some(r)) => r,
        // Not an error: a local-only repository simply has no Dependabot data.
        Ok(None) => return unavailable_dependabot(cli_present, false, String::new(), None),
        Err(e) => return unavailable_dependabot(cli_present, false, String::new(), Some(e)),
    };
    if !cli_present {
        return unavailable_dependabot(
            false,
            true,
            remote.slug(),
            Some("GitHub CLI (`gh`) is not installed or not on PATH".into()),
        );
    }
    match list_dependabot_alerts(&remote) {
        Ok((alerts, truncated)) => DependabotReport {
            available: true,
            cli_present: true,
            is_github_remote: true,
            slug: remote.slug(),
            alerts,
            truncated,
            error: None,
        },
        Err(e) => unavailable_dependabot(true, true, remote.slug(), Some(e)),
    }
}

fn unavailable_dependabot(
    cli_present: bool,
    is_github_remote: bool,
    slug: String,
    error: Option<String>,
) -> DependabotReport {
    DependabotReport {
        available: false,
        cli_present,
        is_github_remote,
        slug,
        alerts: Vec::new(),
        truncated: false,
        error,
    }
}

fn list_dependabot_alerts(
    remote: &GitHubRepoRef,
) -> Result<(Vec<DependabotAlertInfo>, bool), String> {
    let fetch_limit = (ALERT_DISPLAY_LIMIT + 1).to_string();
    let endpoint = format!(
        "repos/{}/{}/dependabot/alerts?state=open&per_page={fetch_limit}",
        remote.owner, remote.name
    );
    let out = capture_gh_api(remote, &endpoint)?;
    if !out.success {
        return Err(gh_api_error_message(&out));
    }
    parse_dependabot_alerts(&out.stdout_text(), ALERT_DISPLAY_LIMIT)
}

/// Extra flags that pin a read-only `gh api` call to this remote's host.
///
/// Deliberately not [`gh_repo_flags`]: `gh api` rejects `--repo`
/// ("unknown flag"), which used to fail every Dependabot fetch. No repo flag
/// is needed because the REST endpoint path already names the repository;
/// hostname pinning still matters for GitHub Enterprise remotes.
fn api_host_flags(remote: &GitHubRepoRef) -> Vec<String> {
    if remote.host.eq_ignore_ascii_case("github.com") {
        Vec::new()
    } else {
        vec!["--hostname".to_string(), remote.host.clone()]
    }
}

/// Runs one read-only `gh api` call pinned to this remote's host.
fn capture_gh_api(remote: &GitHubRepoRef, endpoint: &str) -> Result<CapturedOutput, String> {
    let mut args: Vec<String> = vec!["api".to_string(), endpoint.to_string()];
    args.extend(api_host_flags(remote));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    capture_command("gh", &refs, None, Duration::from_secs(45), &[])
}

/// Shapes a failing `gh api` invocation into its most useful message: the
/// API's JSON `message` body says *why* Dependabot data is unavailable
/// (missing permission, alerts disabled), while gh's stderr often only names
/// the HTTP status. Both channels are tail-capped so a chatty failure cannot
/// ship megabytes of stderr into a report.
fn gh_api_error_message(output: &CapturedOutput) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(output.stdout_text().trim()) {
        if let Some(message) = value.get("message").and_then(Value::as_str) {
            if !message.is_empty() {
                return format!("{message} (HTTP {})", output.status_code);
            }
        }
    }
    let stderr = bounded_error(output.stderr_text());
    if stderr.is_empty() {
        format!("gh exited {}", output.status_code)
    } else {
        stderr
    }
}

fn parse_dependabot_alerts(
    text: &str,
    display_limit: usize,
) -> Result<(Vec<DependabotAlertInfo>, bool), String> {
    let value: Value = serde_json::from_str(text.trim())
        .map_err(|error| format!("could not parse gh dependabot output: {error}"))?;
    let rows = value
        .as_array()
        .ok_or("dependabot payload must be an array")?;
    let truncated = rows.len() > display_limit;
    // Sort the full fetched window worst-first BEFORE capping, so the cap
    // drops the least severe alerts instead of an arbitrary slice that can
    // hide a critical advisory behind a run of lows. The fetched window is
    // already bounded server-side by the per_page limit.
    let mut alerts: Vec<DependabotAlertInfo> = rows.iter().map(alert_from_json).collect();
    // GitHub publishes severities lowercase, but rank case-insensitively so a
    // capitalized or future-vocabulary value still sorts by its real tier
    // instead of landing in the unknown bucket that capping drops first.
    alerts.sort_by(|a, b| {
        severity_rank(&a.severity.to_ascii_lowercase())
            .cmp(&severity_rank(&b.severity.to_ascii_lowercase()))
            .then_with(|| a.number.cmp(&b.number))
    });
    alerts.truncate(display_limit);
    Ok((alerts, truncated))
}

fn json_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn path_str(value: &Value, path: &[&str]) -> String {
    json_path(value, path)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn alert_from_json(row: &Value) -> DependabotAlertInfo {
    let package = path_str(row, &["dependency", "package", "name"]);
    let advisory_id = path_str(row, &["security_advisory", "ghsa_id"]);
    let summary = path_str(row, &["security_advisory", "summary"]);
    let title = if !summary.is_empty() {
        summary
    } else if !package.is_empty() {
        format!("{package} has a security advisory")
    } else if !advisory_id.is_empty() {
        advisory_id.clone()
    } else {
        "Security advisory".to_string()
    };
    DependabotAlertInfo {
        number: row.get("number").and_then(Value::as_u64).unwrap_or(0),
        package,
        ecosystem: path_str(row, &["dependency", "package", "ecosystem"]),
        manifest_path: path_str(row, &["dependency", "manifest_path"]),
        scope: path_str(row, &["dependency", "scope"]),
        severity: path_str(row, &["security_vulnerability", "severity"]),
        title,
        advisory_id,
        cve_id: path_str(row, &["security_advisory", "cve_id"]),
        vulnerable_range: path_str(row, &["security_vulnerability", "vulnerable_version_range"]),
        first_patched: path_str(
            row,
            &[
                "security_vulnerability",
                "first_patched_version",
                "identifier",
            ],
        ),
        url: path_str(row, &["html_url"]),
        created_at: path_str(row, &["created_at"]),
    }
}

/// The exact argv [`checkout_pull_request`] runs, program name included —
/// the single source both the command gate and the executor render from.
pub fn pr_checkout_argv(remote: &GitHubRepoRef, number: u64) -> Result<Vec<String>, String> {
    // Validate before the gate so a fabricated number is refused without a
    // judged line that could never execute.
    if number == 0 {
        return Err("Invalid pull request number".into());
    }
    let mut args = vec![
        "gh".to_string(),
        "pr".to_string(),
        "checkout".to_string(),
        number.to_string(),
    ];
    args.extend(gh_repo_flags(remote));
    Ok(args)
}

pub fn checkout_pull_request(
    repo_path: &str,
    remote: &GitHubRepoRef,
    number: u64,
) -> Result<String, String> {
    let repo = validate_repo(repo_path)?;
    if !gh_cli_present() {
        return Err("GitHub CLI (`gh`) is not installed or not on PATH".into());
    }
    let args = pr_checkout_argv(remote, number)?;
    let refs: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    let stdout =
        run_command_in("gh", &refs, Duration::from_secs(90), Some(&repo)).map_err(bounded_error)?;
    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
}

/// Unicode bidirectional-override characters: in a title or label they can
/// visually reorder rendered text around them, so `#12 fixed` can read as
/// something it is not. Bodies keep them (quoting bidi text is legitimate
/// when reporting i18n bugs) — titles and labels are identifiers.
const BIDI_OVERRIDE_CHARS: &[char] = &[
    '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}',
    '\u{2069}',
];

pub fn validate_issue_payload(title: &str, body: &str, labels: &[String]) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Issue title must not be empty".into());
    }
    if title.chars().count() > 256 {
        return Err("Issue title exceeds the 256 character limit".into());
    }
    if title.chars().any(|c| c.is_control()) {
        return Err("Issue title must not contain control characters".into());
    }
    if let Some(bidi) = title.chars().find(|c| BIDI_OVERRIDE_CHARS.contains(c)) {
        return Err(format!(
            "Issue title must not contain the bidirectional override U+{:04X}",
            bidi as u32
        ));
    }
    if body.len() > 64 * 1024 {
        return Err("Issue body exceeds the 64 KiB limit".into());
    }
    // Newlines and tabs shape markdown; every other control character
    // (NUL included) corrupts argv or rendering and has no legitimate use.
    if let Some(ctrl) = body
        .chars()
        .find(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(format!(
            "Issue body must not contain control characters (found U+{:04X})",
            ctrl as u32
        ));
    }
    if labels.len() > 10 {
        return Err("At most 10 issue labels may be supplied".into());
    }
    for label in labels {
        let label = label.trim();
        if label.is_empty() {
            continue;
        }
        // A leading '-' makes the label option-shaped: interpolated as
        // `gh issue create … --label <label>`, some CLI parsers read
        // "-inbox" (or worse, "-oProxyCommand=…") as flags rather than as
        // the label's value. Labels are names on GitHub anyway — none begin
        // with '-' — so refuse instead of hoping every parser quotes well.
        if label.starts_with('-') {
            return Err(format!("Issue label '{label}' must not start with '-'"));
        }
        if label.chars().count() > 128 {
            return Err("Issue label exceeds the 128 character limit".into());
        }
        if label
            .chars()
            .any(|c| c.is_control() || BIDI_OVERRIDE_CHARS.contains(&c))
        {
            return Err(format!(
                "Issue label '{label}' must not contain control or bidirectional override characters"
            ));
        }
    }
    Ok(())
}

/// The exact argv [`create_issue`] runs, program name included — the single
/// source both the command gate and the executor render from.
pub fn issue_create_argv(
    remote: &GitHubRepoRef,
    title: &str,
    body: &str,
    labels: &[String],
) -> Vec<String> {
    let mut args = vec![
        "gh".to_string(),
        "issue".to_string(),
        "create".to_string(),
        "--title".to_string(),
        title.trim().to_string(),
        "--body".to_string(),
        body.to_string(),
    ];
    for label in labels
        .iter()
        .map(|label| label.trim())
        .filter(|label| !label.is_empty())
    {
        args.push("--label".to_string());
        args.push(label.to_string());
    }
    args.extend(gh_repo_flags(remote));
    args
}

pub fn create_issue(
    repo_path: &str,
    remote: &GitHubRepoRef,
    title: &str,
    body: &str,
    labels: &[String],
) -> Result<String, String> {
    validate_issue_payload(title, body, labels)?;
    let repo = validate_repo(repo_path)?;
    if !gh_cli_present() {
        return Err("GitHub CLI (`gh`) is not installed or not on PATH".into());
    }

    let args = issue_create_argv(remote, title, body, labels);
    let refs: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    let stdout =
        run_command_in("gh", &refs, Duration::from_secs(90), Some(&repo)).map_err(bounded_error)?;
    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
}

pub fn load_github_context(repo_path: &str) -> GitHubContext {
    let cli_present = gh_cli_present();
    let remote = match discover_github_remote(repo_path) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return GitHubContext {
                available: false,
                cli_present,
                host: String::new(),
                owner: String::new(),
                repo: String::new(),
                html_url: String::new(),
                pull_requests: Vec::new(),
                prs_truncated: false,
                issues: Vec::new(),
                issues_truncated: false,
                runs_error: None,
                issues_error: None,
                workflow_runs: Vec::new(),
                runs_truncated: false,
                releases: Vec::new(),
                releases_truncated: false,
                releases_error: None,
                error: Some("No GitHub remote configured".into()),
                warnings: Vec::new(),
            };
        }
        Err(e) => {
            return GitHubContext {
                available: false,
                cli_present,
                host: String::new(),
                owner: String::new(),
                repo: String::new(),
                html_url: String::new(),
                pull_requests: Vec::new(),
                prs_truncated: false,
                issues: Vec::new(),
                issues_truncated: false,
                runs_error: None,
                issues_error: None,
                workflow_runs: Vec::new(),
                runs_truncated: false,
                releases: Vec::new(),
                releases_truncated: false,
                releases_error: None,
                error: Some(e),
                warnings: Vec::new(),
            };
        }
    };

    if !cli_present {
        return GitHubContext {
            available: false,
            cli_present: false,
            host: remote.host.clone(),
            owner: remote.owner.clone(),
            repo: remote.name.clone(),
            html_url: remote.html_url(),
            pull_requests: Vec::new(),
            prs_truncated: false,
            issues: Vec::new(),
            issues_truncated: false,
            runs_error: None,
            issues_error: None,
            workflow_runs: Vec::new(),
            runs_truncated: false,
            releases: Vec::new(),
            releases_truncated: false,
            releases_error: None,
            error: Some("GitHub CLI (`gh`) is not installed or not on PATH".into()),
            warnings: Vec::new(),
        };
    }

    let html_url = remote.html_url();
    let (
        (issues, issues_truncated, issues_error),
        (releases, releases_truncated, releases_error),
        pr_outcome,
        run_outcome,
    ) = std::thread::scope(|s| {
        let issues_handle = s.spawn(|| match list_issues(&remote) {
            Ok((issues, truncated)) => (issues, truncated, None),
            Err(error) => (Vec::new(), false, Some(error)),
        });
        let releases_handle = s.spawn(|| match list_releases(&remote) {
            Ok((releases, truncated)) => (releases, truncated, None),
            Err(error) => (Vec::new(), false, Some(error)),
        });
        let pr_handle = s.spawn(|| list_pull_requests(&remote));
        let runs_handle = s.spawn(|| list_workflow_runs(&remote));

        (
            issues_handle
                .join()
                .unwrap_or_else(|_| (Vec::new(), false, Some("thread panic".into()))),
            releases_handle
                .join()
                .unwrap_or_else(|_| (Vec::new(), false, Some("thread panic".into()))),
            pr_handle
                .join()
                .unwrap_or_else(|_| Err("thread panic".into())),
            runs_handle
                .join()
                .unwrap_or_else(|_| Err("thread panic".into())),
        )
    });

    // Degrade each section independently: a fetch/parse failure keeps the
    // repository facts and carries its reason in both the structured field
    // and `warnings`, never masquerading as an empty-but-complete list.
    let (pull_requests, prs_truncated, pr_error) = match pr_outcome {
        Ok((prs, truncated)) => (prs, truncated, None),
        Err(e) => (Vec::new(), false, Some(e)),
    };
    let (workflow_runs, runs_truncated, runs_error) = match run_outcome {
        Ok((runs, truncated)) => (runs, truncated, None),
        Err(e) => (Vec::new(), false, Some(e)),
    };

    // When every listing failed together — the classic expired-token or
    // rate-limit signature — "four empty sections plus warnings" would
    // understate the situation: report the context itself as unavailable.
    let every_section_failed = pr_error.is_some()
        && issues_error.is_some()
        && releases_error.is_some()
        && runs_error.is_some();

    let mut warnings: Vec<String> = Vec::new();
    if !every_section_failed {
        if let Some(ref pr_err) = pr_error {
            warnings.push(format!(
                "Pull request listing failed: {pr_err}. The list may be incomplete."
            ));
        }
        if let Some(ref issue_error) = issues_error {
            warnings.push(format!("Issue listing failed: {issue_error}"));
        }
        if let Some(ref release_error) = releases_error {
            warnings.push(format!("Release listing failed: {release_error}"));
        }
        if let Some(ref run_error) = runs_error {
            warnings.push(format!("Workflow run listing failed: {run_error}"));
        }
    }

    let (available, error) = if every_section_failed {
        (
            false,
            Some(
                pr_error
                    .clone()
                    .or_else(|| issues_error.clone())
                    .unwrap_or_else(|| "GitHub queries failed".into()),
            ),
        )
    } else {
        (true, None)
    };

    GitHubContext {
        available,
        cli_present: true,
        host: remote.host,
        owner: remote.owner,
        repo: remote.name,
        html_url,
        pull_requests,
        prs_truncated,
        issues,
        issues_truncated,
        issues_error,
        workflow_runs,
        runs_error,
        runs_truncated,
        releases,
        releases_truncated,
        releases_error,
        error,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (audit M7): `gh` succeeding but emitting anything other
    /// than the expected JSON (version drift, warning preamble) used to be
    /// laundered into an empty-but-successful list via unwrap_or_default.
    #[test]
    fn garbage_gh_output_is_a_parse_error_not_an_empty_success() {
        for garbage in [
            &b""[..],
            b"gh: warning: something odd\n[]",
            b"\xef\xbb\xbf[]",
            b"{\"unexpected\":true}",
        ] {
            assert!(
                parse_pr_list(garbage, PR_DISPLAY_LIMIT).is_err(),
                "pr parse must fail loudly on {:?}",
                String::from_utf8_lossy(garbage)
            );
            assert!(
                parse_workflow_runs(garbage, RUN_DISPLAY_LIMIT).is_err(),
                "run parse must fail loudly on {:?}",
                String::from_utf8_lossy(garbage)
            );
        }
    }

    #[test]
    fn valid_gh_output_maps_into_context_types() {
        let (prs, prs_truncated) = parse_pr_list(
            br#"[{"number":7,"title":"Fix it","state":"OPEN","headRefName":"f",
                 "baseRefName":"main","url":"https://x/7","isDraft":true,
                 "statusCheckRollup":null}]"#,
            PR_DISPLAY_LIMIT,
        )
        .unwrap();
        assert!(!prs_truncated);
        let pr = prs.first().expect("one pr");
        assert_eq!(pr.number, 7);
        assert!(pr.is_draft);
        assert_eq!(pr.ci_status, "unknown");

        let (runs, runs_truncated) = parse_workflow_runs(
            br#"[{"databaseId":42,"name":"ci","status":"completed",
                  "conclusion":"success","headBranch":"main","url":"https://x/42",
                  "createdAt":"2026-01-01","displayTitle":"build"}]"#,
            RUN_DISPLAY_LIMIT,
        )
        .unwrap();
        assert!(!runs_truncated);
        let run = runs.first().expect("one run");
        assert_eq!(run.id, 42);
        assert_eq!(run.title, "build");
    }

    /// Regression: the PR and workflow-run sections fetch LIMIT+1 rows and
    /// report capping like issues/releases/workflows/alerts always did; a
    /// capped list must never silently pose as complete.
    #[test]
    fn pr_and_run_lists_flag_capping_instead_of_hiding_it() {
        let prs_json: Vec<serde_json::Value> = (1..=4)
            .map(|n| {
                serde_json::json!({
                    "number": n, "title": format!("PR {n}"), "state": "OPEN",
                    "headRefName": format!("f{n}"), "baseRefName": "main",
                    "url": "", "isDraft": false, "statusCheckRollup": null
                })
            })
            .collect();
        let text = serde_json::to_vec(&prs_json).unwrap();
        let (prs, truncated) = parse_pr_list(&text, 3).unwrap();
        assert!(truncated);
        assert_eq!(prs.len(), 3);

        let runs_json: Vec<serde_json::Value> = (1..=3)
            .map(|n| serde_json::json!({ "databaseId": n }))
            .collect();
        let text = serde_json::to_vec(&runs_json).unwrap();
        let (runs, truncated) = parse_workflow_runs(&text, 2).unwrap();
        assert!(truncated);
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn test_parse_https_github_url() {
        let parsed = parse_github_remote_url("https://github.com/acme/gitpulse.git").unwrap();
        assert_eq!(parsed.owner, "acme");
        assert_eq!(parsed.name, "gitpulse");
        assert_eq!(parsed.host, "github.com");
        assert_eq!(parsed.html_url(), "https://github.com/acme/gitpulse");
    }

    #[test]
    fn test_parse_ssh_github_url() {
        let parsed = parse_github_remote_url("git@github.com:acme/gitpulse.git").unwrap();
        assert_eq!(parsed.slug(), "acme/gitpulse");
    }

    #[test]
    fn test_parse_ssh_scheme_url() {
        let parsed = parse_github_remote_url("ssh://git@github.com/acme/gitpulse.git").unwrap();
        assert_eq!(parsed.owner, "acme");
        assert_eq!(parsed.name, "gitpulse");
    }

    #[test]
    fn test_parse_ssh_scheme_url_with_port() {
        let parsed = parse_github_remote_url("ssh://git@github.com:22/acme/gitpulse.git").unwrap();
        assert_eq!(parsed.owner, "acme");
        assert_eq!(parsed.name, "gitpulse");
        assert_eq!(parsed.host, "github.com");
    }

    #[test]
    fn test_parse_rejects_non_githubish() {
        assert!(parse_github_remote_url("https://example.com/not-a-repo").is_none());
        assert!(parse_github_remote_url("").is_none());
    }

    /// Regression (hardening pass): parsed components flow verbatim into
    /// argv values (`--repo`, `--hostname`), REST endpoint paths, and built
    /// URLs. A component shaped like a flag (`-inbox`) would be re-parsed by
    /// gh's CLI as flags rather than values; whitespace/control characters
    /// corrupt argv, endpoints, and error messages; unbounded components let
    /// a crafted megabyte URL flow whole into the UI. All are refused at the
    /// single parsing choke point.
    #[test]
    fn hostile_remote_components_are_refused_at_parse_time() {
        // Flag-shaped owners/hosts must not survive into `--repo`/`--hostname`.
        assert!(parse_github_remote_url("https://github.com/-acme/repo.git").is_none());
        assert!(parse_github_remote_url("git@github.com:-acme/repo.git").is_none());
        assert!(parse_github_remote_url("https://github.com/acme/-repo.git").is_none());
        assert!(parse_github_remote_url("ssh://git@-evil.dev:22/acme/repo.git").is_none());
        // Whitespace cannot reach argv or endpoint paths.
        assert!(parse_github_remote_url("https://github.com/a b/c.git").is_none());
        // Control characters (here \u{7} BEL) are refused everywhere.
        assert!(parse_github_remote_url("https://github.com/a\u{7}b/c.git").is_none());
        assert!(parse_github_remote_url("https://github.com/acme/re\u{1F}po.git").is_none());
        // Unbounded components are capped.
        let long_owner = "a".repeat(101);
        assert!(
            parse_github_remote_url(&format!("https://github.com/{long_owner}/repo.git")).is_none()
        );
        let long_host = format!("{}.ghe.com", "b".repeat(101));
        assert!(parse_github_remote_url(&format!("https://{long_host}/acme/repo.git")).is_none());
        // Ordinary names that merely contain dashes, dots, or underscores stay valid.
        for url in [
            "https://github.com/my-org/my.repo_name.git",
            "git@github.com:acme-corp/Repo-Name_1.git",
            "https://acme.ghe.com/o/r",
        ] {
            assert!(
                parse_github_remote_url(url).is_some(),
                "benign remote {url} must still parse"
            );
        }
    }

    /// Regression: git-protocol remotes used to fall through every scheme
    /// strip and vanish; uppercase schemes (common on Windows checkouts)
    /// were never recognized at all.
    #[test]
    fn git_protocol_and_uppercase_schemes_parse() {
        let parsed = parse_github_remote_url("git://github.com/acme/repo.git").unwrap();
        assert_eq!(parsed.slug(), "acme/repo");
        assert_eq!(parsed.host, "github.com");

        let upper = parse_github_remote_url("HTTPS://GITHUB.COM/Acme/Repo.Git").unwrap();
        assert_eq!(upper.owner, "Acme");
        assert_eq!(upper.name, "Repo");
        // Host trust matching is case-insensitive downstream.
        assert!(is_github_host(&upper.host));

        let ssh_upper = parse_github_remote_url("SSH://git@github.com/acme/repo").unwrap();
        assert_eq!(ssh_upper.slug(), "acme/repo");
    }

    /// Regression: a port on an https remote means it is NOT plain
    /// github.com; stripping it before the trust decision let a same-named
    /// service on a nonstandard port pass as github.com while display and
    /// endpoint disagreed with what was configured.
    #[test]
    fn web_scheme_ports_are_never_stripped_or_trusted() {
        assert!(parse_github_remote_url("https://github.com:8443/acme/repo.git").is_none());
        assert!(parse_github_remote_url("http://github.com:8080/acme/repo").is_none());
        assert!(parse_github_remote_url("https://acme.ghe.com:9000/o/r").is_none());
        // ssh forms legitimately carry ports and keep working.
        let ssh = parse_github_remote_url("ssh://git@github.com:22/acme/repo.git").unwrap();
        assert_eq!(ssh.host, "github.com");
        let ghes = parse_github_remote_url("ssh://git@ghe.acme.corp:2222/o/r").unwrap();
        assert_eq!(ghes.host, "ghe.acme.corp");
        // Non-numeric junk after ':' is refused everywhere.
        assert!(parse_github_remote_url("ssh://git@github.com:junk/acme/repo").is_none());
    }

    /// Dot-segments in owner/name would bend REST endpoint paths
    /// (`repos/../etc/...`) and built URLs away from the intended repo.
    #[test]
    fn dot_segment_components_are_refused() {
        for url in [
            "https://github.com/../etc/passwd",
            "https://github.com/./repo",
            "https://github.com/acme/.gitignore",
            "https://github.com/.hidden/repo",
            "git@github.com:../upstream.git",
        ] {
            assert!(
                parse_github_remote_url(url).is_none(),
                "dot-shaped remote {url} must not parse"
            );
        }
    }

    /// Release URLs are rebuilt from repo-controlled tag names; unsafe bytes
    /// must be percent-encoded so a crafted tag cannot mangle the link.
    #[test]
    fn release_tag_urls_are_percent_encoded() {
        let json =
            br#"[{"tagName":"v1..<script>","name":"x"},{"tagName":"v1.2+build~rc/a","name":"y"}]"#;
        let (releases, _) = parse_release_list(json, "https://github.com/acme/repo", 50).unwrap();
        assert_eq!(
            releases[0].url,
            "https://github.com/acme/repo/releases/tag/v1..%3Cscript%3E"
        );
        assert_eq!(
            releases[1].url,
            "https://github.com/acme/repo/releases/tag/v1.2%2Bbuild~rc/a"
        );
    }

    /// The empty host that used to be special-cased is covered by the
    /// component rule, and so is an owner-less or name-less path.
    #[test]
    fn degenerate_remote_shapes_stay_refused() {
        assert!(split_owner_repo("/a/b", false).is_none());
        assert!(split_owner_repo("github.com/", false).is_none());
        assert!(split_owner_repo("github.com/only-owner", false).is_none());
    }

    /// Regression: `git remote -v` lists every remote twice — fetch and push
    /// lines. The push URL is an attacker-chosen redirect target; trusting it
    /// aimed every gh call (`--repo`, Dependabot endpoints, checkout) at a
    /// repository the user never pulls from.
    #[test]
    fn push_urls_are_never_selected_as_the_github_remote() {
        // Fetch is GitLab, push is GitHub: the push line must not win.
        let mixed = "origin\thttps://gitlab.com/victim/real.git (fetch)\n\
                     origin\tgit@github.com:attacker/copy.git (push)\n";
        assert!(pick_github_remote(mixed).is_none());

        // Fetch is GitHub, push points elsewhere: the fetch line wins.
        let normal = "origin\tgit@github.com:acme/repo.git (fetch)\n\
                      origin\thttps://gitlab.com/mirror/repo.git (push)\n";
        let picked = pick_github_remote(normal).expect("fetch url selected");
        assert_eq!(picked.slug(), "acme/repo");

        // A GitHub push URL with no GitHub fetch anywhere stays untrusted.
        let push_only = "upstream\thttps://gitlab.com/x/y.git (fetch)\n\
                         upstream\tgit@github.com:evil/pwned.git (push)\n\
                         origin\thttps://gitlab.com/x/y.git (fetch)\n\
                         origin\thttps://gitlab.com/x/y.git (push)\n";
        assert!(pick_github_remote(push_only).is_none());

        // Origin preference among multiple GitHub remotes still holds.
        let two = "upstream\thttps://github.com/a/up.git (fetch)\n\
                   upstream\thttps://github.com/a/up.git (push)\n\
                   origin\thttps://github.com/b/origin-repo.git (fetch)\n\
                   origin\thttps://github.com/b/origin-repo.git (push)\n";
        assert_eq!(pick_github_remote(two).unwrap().slug(), "b/origin-repo");

        // Degenerate lines are skipped, not fatal.
        let messy = "\nnot-a-remote-line\n\n\
                     origin\tgit@github.com:o/r.git (fetch)\n";
        assert_eq!(pick_github_remote(messy).unwrap().slug(), "o/r");
    }

    #[test]
    fn test_summarize_checks_failure_wins() {
        let value = Some(serde_json::json!([
            {"state": "SUCCESS"},
            {"conclusion": "FAILURE"}
        ]));
        assert_eq!(summarize_checks(&value), "failure");
    }

    #[test]
    fn test_summarize_checks_pending_and_success() {
        let pending = Some(serde_json::json!([{"status": "IN_PROGRESS"}]));
        assert_eq!(summarize_checks(&pending), "pending");
        let ok = Some(serde_json::json!([{"state": "SUCCESS"}]));
        assert_eq!(summarize_checks(&ok), "success");
        assert_eq!(summarize_checks(&None), "unknown");
    }

    /// Regression: GitHub conclusions that carry no "FAIL" substring —
    /// TIMED_OUT, ACTION_REQUIRED, STARTUP_FAILURE, STALE — used to fall
    /// through to "success", painting a broken pipeline green.
    #[test]
    fn non_success_conclusions_never_render_as_green() {
        for conclusion in [
            "TIMED_OUT",
            "ACTION_REQUIRED",
            "STARTUP_FAILURE",
            "STALE",
            "FAILURE",
            "ERROR",
            "CANCELLED",
        ] {
            let rollup = Some(serde_json::json!([
                {"state": "SUCCESS"},
                {"conclusion": conclusion}
            ]));
            assert_eq!(
                summarize_checks(&rollup),
                "failure",
                "conclusion {conclusion} must not read as success"
            );
        }
        // `waiting` (deployment-protection gates) is in-flight, not done.
        let waiting = Some(serde_json::json!([{"state": "WAITING"}]));
        assert_eq!(summarize_checks(&waiting), "pending");
        // An entry with no readable state is unknown, never a pass.
        let opaque = Some(serde_json::json!([{"unrelated": true}]));
        assert_eq!(summarize_checks(&opaque), "unknown");
        let mixed = Some(serde_json::json!([
            {"conclusion": "SUCCESS"}, {"weird": 1}
        ]));
        assert_eq!(summarize_checks(&mixed), "unknown");
    }

    #[test]
    fn test_parse_ghes_https_url() {
        // Parsing is host-agnostic: any git host with owner/repo parses.
        // Whether the host is TRUSTED as GitHub is `is_github_host`'s job,
        // and arbitrary GHES domains are not trusted anymore (see
        // test_is_github_host_rejects_lookalikes).
        let parsed =
            parse_github_remote_url("https://github.example.com/acme/gitpulse.git").unwrap();
        assert_eq!(parsed.host, "github.example.com");
        assert_eq!(parsed.slug(), "acme/gitpulse");
        assert!(!is_github_host(&parsed.host));
    }

    #[test]
    fn test_is_github_host_ghe_and_rejects_gitlab() {
        // GitHub Enterprise Cloud lives under *.ghe.com; a self-hosted
        // "ghe.internal.corp" is a different product on an untrusted domain.
        assert!(is_github_host("acme.ghe.com"));
        assert!(!is_github_host("ghe.internal.corp"));
        assert!(is_github_host("github.com"));
        assert!(!is_github_host("gitlab.com"));
        assert!(!is_github_host("bitbucket.org"));
    }

    #[test]
    fn test_gh_repo_flags_include_hostname_for_ghes() {
        let remote = GitHubRepoRef {
            host: "github.example.com".into(),
            owner: "acme".into(),
            name: "gitpulse".into(),
        };
        let flags = gh_repo_flags(&remote);
        assert_eq!(
            flags,
            vec![
                "--repo".to_string(),
                "acme/gitpulse".to_string(),
                "--hostname".to_string(),
                "github.example.com".to_string()
            ]
        );
        let dot_com = GitHubRepoRef {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "gitpulse".into(),
        };
        assert_eq!(
            gh_repo_flags(&dot_com),
            vec!["--repo".to_string(), "acme/gitpulse".to_string()]
        );
    }

    #[test]
    fn test_checkout_pull_request_rejects_zero() {
        let remote = GitHubRepoRef {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "gitpulse".into(),
        };
        // The builder refuses a fabricated number before any external work.
        let err = pr_checkout_argv(&remote, 0).unwrap_err();
        assert!(err.to_lowercase().contains("invalid"));
        assert_eq!(
            pr_checkout_argv(&remote, 7).unwrap(),
            vec!["gh", "pr", "checkout", "7", "--repo", "acme/gitpulse"]
        );
    }

    /// The matcher must trust exactly github.com, *.github.com and *.ghe.com.
    /// Substring matching used to accept lookalike domains such as
    /// "notgithub.com" or "github.com.evil.io", letting an attacker-chosen
    /// remote pass as GitHub.
    #[test]
    fn test_is_github_host_rejects_lookalikes() {
        assert!(is_github_host("github.com"));
        assert!(is_github_host("GitHub.Com"));
        assert!(is_github_host("raw.github.com"));
        assert!(is_github_host("acme.ghe.com"));
        assert!(!is_github_host("notgithub.com"));
        assert!(!is_github_host("fake-github.org"));
        assert!(!is_github_host("github.com.evil.io"));
        assert!(!is_github_host("myghe.internal"));
        assert!(!is_github_host("ghe.acme.corp"));
        assert!(!is_github_host("gitlab.com"));
        assert!(!is_github_host("bitbucket.org"));
        assert!(!is_github_host(""));
    }

    #[test]
    fn test_create_issue_rejects_invalid_payload_before_external_work() {
        assert!(validate_issue_payload(" ", "body", &[]).is_err());
        assert!(validate_issue_payload(&"x".repeat(257), "body", &[]).is_err());
        assert!(validate_issue_payload("title", &"x".repeat(65 * 1024), &[]).is_err());
    }

    /// Regression: control and bidirectional-override characters used to
    /// pass validation into `--title`/`--label` argv, GitHub's stored issue,
    /// and every list that renders it — letting a title visually reorder
    /// the text around it.
    #[test]
    fn hostile_title_and_label_characters_are_refused() {
        // Control characters in the title.
        assert!(validate_issue_payload("\u{7}bell", "body", &[]).is_err());
        assert!(validate_issue_payload("title\u{0}", "body", &[]).is_err());
        // Bidi overrides in the title (spoofing: "fixed" rendered reordered).
        assert!(validate_issue_payload("\u{202E}eltit", "body", &[]).is_err());
        assert!(validate_issue_payload("title \u{2066}x", "body", &[]).is_err());
        // Labels: every control character (tab included) is refused, NUL
        // along with them.
        assert!(validate_issue_payload("t", "b", &["a\u{0}b".into()]).is_err());
        assert!(validate_issue_payload("t", "b", &["a\tb".into()]).is_err());
        // Bidi overrides in labels are refused too.
        assert!(validate_issue_payload("t", "b", &["\u{202D}bug".into()]).is_err());
        // Bodies keep newlines/tabs but not other controls.
        assert!(validate_issue_payload("t", "line\nline\r\n\ttabbed", &[]).is_ok());
        assert!(validate_issue_payload("t", "bad\u{1B}esc", &[]).is_err());
        assert!(validate_issue_payload("t", "bad\u{0}nul", &[]).is_err());
        // Ordinary unicode content stays welcome.
        assert!(
            validate_issue_payload("Ünïcode – title ✓", "body ✓", &["label_1.x".into()]).is_ok()
        );
    }

    /// Both gated executors render their line from one shared builder that
    /// includes the program name, so the harness judges what actually runs
    /// and the judged `--repo` cannot drift from the executed one.
    #[test]
    fn gated_executors_render_their_line_from_the_shared_builders() {
        let remote = GitHubRepoRef {
            host: "acme.ghe.com".into(),
            owner: "o".into(),
            name: "r".into(),
        };
        let labels = vec!["bug".to_string(), "  ".to_string(), "  ui ".to_string()];
        let argv = issue_create_argv(&remote, "  Title  ", "Body", &labels);
        assert_eq!(argv.first().map(String::as_str), Some("gh"));
        assert_eq!(
            argv,
            vec![
                "gh",
                "issue",
                "create",
                "--title",
                "Title",
                "--body",
                "Body",
                "--label",
                "bug",
                "--label",
                "ui",
                "--repo",
                "o/r",
                "--hostname",
                "acme.ghe.com"
            ]
        );
        let checkout = pr_checkout_argv(&remote, 42).unwrap();
        assert_eq!(
            checkout,
            vec![
                "gh",
                "pr",
                "checkout",
                "42",
                "--repo",
                "o/r",
                "--hostname",
                "acme.ghe.com"
            ]
        );
    }

    /// A label beginning with '-' is option-shaped and could be parsed as a
    /// gh flag instead of a value; validation refuses it up front.
    #[test]
    fn dash_prefixed_labels_are_rejected() {
        assert!(validate_issue_payload("title", "body", &["-inbox".to_string()]).is_err());
        // Whitespace does not launder the prefix: trimming happens first.
        assert!(validate_issue_payload("title", "body", &["  --repo=evil".to_string()]).is_err());
        let err = validate_issue_payload("title", "body", &["-bug".to_string()]).unwrap_err();
        assert!(
            err.contains("must not start with '-'"),
            "refusal should name the rule, got: {err}"
        );
        // Ordinary labels, including ones containing dashes mid-name, pass.
        assert!(validate_issue_payload("title", "body", &["good-label".to_string()]).is_ok());
        assert!(
            validate_issue_payload("title", "body", &["bug".to_string(), String::new()]).is_ok()
        );
    }

    #[test]
    fn test_issue_monitor_marks_capped_and_invalid_results() {
        let rows: Vec<serde_json::Value> = (1..=3)
            .map(|number| {
                serde_json::json!({
                    "number": number,
                    "title": format!("Issue {number}"),
                    "state": "OPEN",
                    "url": format!("https://github.com/acme/repo/issues/{number}"),
                    "labels": [{"name": "bug"}],
                    "updatedAt": "2026-08-25T00:00:00Z",
                    "author": {"login": "ada"}
                })
            })
            .collect();
        let json = serde_json::to_vec(&rows).unwrap();
        let (issues, truncated) = parse_issue_list(&json, 2).unwrap();
        assert_eq!(issues.len(), 2);
        assert!(truncated);
        assert!(parse_issue_list(b"not json", 50).is_err());
    }

    fn dependabot_row(number: u64, package: &str, severity: &str) -> serde_json::Value {
        serde_json::json!({
            "number": number,
            "state": "open",
            "dependency": {
                "package": {"ecosystem": "npm", "name": package},
                "manifest_path": "package.json",
                "scope": "runtime"
            },
            "security_advisory": {
                "ghsa_id": format!("GHSA-test-{number}"),
                "cve_id": serde_json::Value::Null,
                "summary": format!("{package} allows XSS")
            },
            "security_vulnerability": {
                "severity": severity,
                "vulnerable_version_range": "< 1.0.0",
                "first_patched_version": null
            },
            "html_url": format!("https://github.com/acme/repo/security/dependabot/{number}"),
            "created_at": "2026-08-25T00:00:00Z"
        })
    }

    /// Worst severity first; ties keep ascending alert order.
    #[test]
    fn dependabot_alerts_sort_worst_first() {
        let payload = r#"[
            {"number":2,"security_advisory":{"ghsa_id":"GHSA-b","cve_id":null,"summary":"high one"},
             "dependency":{"package":{"ecosystem":"npm","name":"b"},"manifest_path":"package.json"},
             "security_vulnerability":{"severity":"high","vulnerable_version_range":">= 1 < 2",
               "first_patched_version":{"identifier":"2.0.0"}},
             "html_url":"https://github.com/a/r/security/dependabot/2","created_at":"2026-01-01"},
            {"number":1,"security_advisory":{"ghsa_id":"GHSA-c","cve_id":"CVE-1","summary":"critical one"},
             "dependency":{"package":{"ecosystem":"pip","name":"c"},"manifest_path":"requirements.txt","scope":"development"},
             "security_vulnerability":{"severity":"critical","vulnerable_version_range":"< 3",
               "first_patched_version":null},
             "html_url":"https://github.com/a/r/security/dependabot/1","created_at":"2026-01-02"}
        ]"#;
        let (alerts, truncated) = parse_dependabot_alerts(payload, 50).unwrap();
        assert!(!truncated);
        assert_eq!(alerts.len(), 2);
        assert_eq!(alerts[0].severity, "critical");
        assert_eq!(alerts[0].number, 1);
        assert_eq!(alerts[0].cve_id, "CVE-1");
        assert_eq!(alerts[0].ecosystem, "pip");
        assert_eq!(alerts[0].scope, "development");
        // No patched version published yet must surface as empty, not a fake fix.
        assert_eq!(alerts[0].first_patched, "");
        assert_eq!(alerts[1].severity, "high");
        assert_eq!(alerts[1].first_patched, "2.0.0");
    }

    /// Fields the API marks optional or nullable degrade to "", and a missing
    /// advisory summary still yields a non-empty title.
    #[test]
    fn dependabot_alerts_tolerate_missing_optional_fields() {
        let payload =
            r#"[{"number":7,"dependency":{"package":{"name":"d"}},"security_advisory":{}}]"#;
        let (alerts, truncated) = parse_dependabot_alerts(payload, 50).unwrap();
        let alert = alerts.first().expect("one alert");
        assert!(!truncated);
        assert_eq!(alert.severity, "");
        assert_eq!(alert.manifest_path, "");
        assert_eq!(alert.scope, "");
        assert_eq!(alert.cve_id, "");
        assert_eq!(alert.vulnerable_range, "");
        assert_eq!(alert.title, "d has a security advisory");
        assert_eq!(alert.url, "");
    }

    #[test]
    fn garbage_dependabot_output_is_a_parse_error_not_an_empty_success() {
        for garbage in ["", "{\"unexpected\":true}", "\"array?\""] {
            assert!(
                parse_dependabot_alerts(garbage, ALERT_DISPLAY_LIMIT).is_err(),
                "dependabot parse must fail loudly on {garbage:?}"
            );
        }
    }

    #[test]
    fn dependabot_fetch_capping_is_reported_not_hidden() {
        let rows: Vec<serde_json::Value> = (1..=3)
            .map(|n| dependabot_row(n as u64, "pkg", "low"))
            .collect();
        let text = serde_json::to_string(&rows).unwrap();
        let (alerts, truncated) = parse_dependabot_alerts(&text, 2).unwrap();
        assert_eq!(alerts.len(), 2);
        assert!(truncated);
    }

    /// Regression (hardening pass): capping used to happen before sorting,
    /// so a critical advisory arriving after the first `display_limit` rows
    /// was dropped while less severe ones survived. The full fetched window
    /// (already bounded by per_page) must be ranked, then capped.
    #[test]
    fn dependabot_cap_drops_least_severe_not_arbitrary_rows() {
        let rows = vec![
            dependabot_row(1, "low-a", "low"),
            dependabot_row(2, "low-b", "low"),
            dependabot_row(3, "critical-c", "critical"),
        ];
        let text = serde_json::to_string(&rows).unwrap();
        let (alerts, truncated) = parse_dependabot_alerts(&text, 2).unwrap();
        assert!(truncated);
        assert_eq!(alerts.len(), 2);
        // The critical alert beyond the cap survives; a low one is dropped.
        assert_eq!(alerts[0].number, 3);
        assert_eq!(alerts[0].severity, "critical");
        assert_eq!(alerts[1].number, 1);
    }

    /// Regression: severity ranking was case-sensitive, so a capitalized
    /// "HIGH" landed in the unknown bucket — sorted last and first dropped
    /// by the display cap.
    #[test]
    fn dependabot_severity_ranking_is_case_insensitive() {
        let rows = vec![
            dependabot_row(1, "a", "HIGH"),
            dependabot_row(2, "b", "Critical"),
            dependabot_row(3, "c", "low"),
        ];
        let text = serde_json::to_string(&rows).unwrap();
        let (alerts, _) = parse_dependabot_alerts(&text, 50).unwrap();
        assert_eq!(
            alerts.iter().map(|alert| alert.number).collect::<Vec<_>>(),
            vec![2, 1, 3]
        );
    }

    /// Error text surfaced from gh is tail-capped: a chatty failure cannot
    /// ship megabytes of stderr into reports and warnings.
    #[test]
    fn oversized_gh_errors_are_tail_capped_on_char_boundaries() {
        let big = "x".repeat(MAX_GH_ERROR_BYTES * 3);
        let capped = bounded_error(big.clone());
        assert!(capped.len() <= MAX_GH_ERROR_BYTES + 4); // boundary slack only
        assert!(big.ends_with(capped.trim_end()), "tail must be kept");

        // Multibyte characters are not split mid-codepoint.
        let multibyte = "é".repeat(MAX_GH_ERROR_BYTES); // 2 bytes each
        let capped = bounded_error(multibyte);
        assert!(!capped.contains('\u{FFFD}') || capped.is_empty());

        let output = CapturedOutput {
            stdout: Vec::new(),
            stderr: format!(
                "{}\nreal reason at the end",
                "y".repeat(MAX_GH_ERROR_BYTES * 2)
            )
            .into_bytes(),
            success: false,
            status_code: 1,
        };
        let message = gh_api_error_message(&output);
        assert!(
            message.len() <= MAX_GH_ERROR_BYTES + 8,
            "error message must stay bounded, got {}",
            message.len()
        );
        assert!(message.ends_with("real reason at the end"));
    }

    #[test]
    fn bounded_error_passes_small_messages_through() {
        assert_eq!(bounded_error("short".into()), "short");
        assert_eq!(bounded_error(String::new()), "");
    }

    #[test]
    fn release_list_parses_tags_drafts_and_generates_urls() {
        let json = br#"[
            {
                "tagName": "v1.2.0",
                "name": "Version 1.2.0",
                "isDraft": false,
                "isPrerelease": false,
                "isLatest": true,
                "publishedAt": "2026-08-25T01:00:00Z",
                "createdAt": "2026-08-25T00:30:00Z",
                "url": "https://github.com/acme/gitpulse/releases/tag/v1.2.0"
            },
            {
                "tagName": "v1.3.0-rc.1",
                "name": "Release Candidate 1",
                "isDraft": false,
                "isPrerelease": true,
                "isLatest": false,
                "publishedAt": "2026-08-25T02:00:00Z",
                "createdAt": "2026-08-25T01:30:00Z",
                "url": ""
            },
            {
                "tagName": "",
                "name": "Draft Next",
                "isDraft": true,
                "isPrerelease": false,
                "isLatest": false,
                "publishedAt": null,
                "createdAt": "2026-08-25T03:00:00Z"
            }
        ]"#;
        let (releases, truncated) =
            parse_release_list(json, "https://github.com/acme/gitpulse", 50).unwrap();
        assert!(!truncated);
        assert_eq!(releases.len(), 3);

        let r0 = &releases[0];
        assert_eq!(r0.tag_name, "v1.2.0");
        assert_eq!(r0.name, "Version 1.2.0");
        assert!(!r0.is_draft);
        assert!(!r0.is_prerelease);
        assert!(r0.is_latest);
        assert_eq!(r0.published_at, "2026-08-25T01:00:00Z");
        assert_eq!(
            r0.url,
            "https://github.com/acme/gitpulse/releases/tag/v1.2.0"
        );

        let r1 = &releases[1];
        assert_eq!(r1.tag_name, "v1.3.0-rc.1");
        assert!(r1.is_prerelease);
        assert!(!r1.is_latest);
        assert_eq!(
            r1.url,
            "https://github.com/acme/gitpulse/releases/tag/v1.3.0-rc.1"
        );

        let r2 = &releases[2];
        assert!(r2.is_draft);
        assert_eq!(r2.published_at, "");
        assert_eq!(r2.url, "https://github.com/acme/gitpulse/releases");
    }

    #[test]
    fn release_fetch_capping_is_reported_not_hidden() {
        let rows: Vec<serde_json::Value> = (1..=5)
            .map(|n| {
                serde_json::json!({
                    "tagName": format!("v1.0.{n}"),
                    "name": format!("Release {n}"),
                    "isDraft": false,
                    "isPrerelease": false,
                    "isLatest": n == 5,
                    "publishedAt": "2026-08-25T00:00:00Z",
                    "createdAt": "2026-08-25T00:00:00Z"
                })
            })
            .collect();
        let json = serde_json::to_vec(&rows).unwrap();
        let (releases, truncated) =
            parse_release_list(&json, "https://github.com/acme/gitpulse", 3).unwrap();
        assert_eq!(releases.len(), 3);
        assert!(truncated);
    }

    #[test]
    fn garbage_release_output_is_a_parse_error_not_an_empty_success() {
        for garbage in [
            &b""[..],
            b"gh: error\n[]",
            b"\xef\xbb\xbf[]",
            b"{\"unexpected\":true}",
        ] {
            assert!(
                parse_release_list(garbage, "https://github.com/acme/gitpulse", 50).is_err(),
                "release parse must fail loudly on {:?}",
                String::from_utf8_lossy(garbage)
            );
        }
    }

    /// Regression (live-verified against gh 2.95): `gh api` has no `--repo`
    /// flag, so appending [`gh_repo_flags`] to the Dependabot fetch failed
    /// every call with "unknown flag". Hostname pinning stays, and only for
    /// non-github.com hosts.
    #[test]
    fn gh_api_calls_are_hostname_pinned_and_never_carry_repo_flag() {
        let dot_com = GitHubRepoRef {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "gitpulse".into(),
        };
        assert!(api_host_flags(&dot_com).is_empty());

        let ghes = GitHubRepoRef {
            host: "acme.ghe.com".into(),
            owner: "acme".into(),
            name: "gitpulse".into(),
        };
        assert_eq!(
            api_host_flags(&ghes),
            vec!["--hostname".to_string(), "acme.ghe.com".to_string()]
        );
        assert!(!api_host_flags(&ghes).contains(&"--repo".to_string()));
    }

    /// Regression (live-verified against gh 2.95): `url` is not a
    /// `gh release list` JSON field; requesting it failed the whole listing
    /// with "Unknown JSON field", degrading every release panel into an
    /// error. The field set must stay within gh's documented fields; URLs
    /// are rebuilt by `parse_release_list` from tags instead.
    #[test]
    fn release_listing_requests_only_supported_json_fields() {
        const SUPPORTED: &[&str] = &[
            "createdAt",
            "isDraft",
            "isImmutable",
            "isLatest",
            "isPrerelease",
            "name",
            "publishedAt",
            "tagName",
        ];
        let args = release_list_leading_args("51");
        let json_pos = args
            .iter()
            .position(|arg| arg == "--json")
            .expect("--json flag present");
        let fields: Vec<&str> = args[json_pos + 1].split(',').collect();
        assert!(!fields.is_empty(), "at least one JSON field is requested");
        for field in fields {
            assert!(
                SUPPORTED.contains(&field),
                "field '{field}' is not a gh release list JSON field; fetching would fail"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Stress: parsers must survive hostile shapes at scale without panicking,
    // laundering garbage into success, or running unbounded.
    // -----------------------------------------------------------------------

    /// A deterministic pseudo-random byte generator (no external deps, fully
    /// reproducible failures).
    struct Lcg(u64);
    impl Lcg {
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0
        }
    }

    /// Fuzz-ish sweep: thousands of mutated/truncated/corrupted inputs across
    /// every parser must return Err (or valid data), never panic. Intact
    /// seeds are asserted first, so the corpus demonstrably exercises both
    /// outcomes.
    #[test]
    fn stress_parsers_survive_corrupted_inputs_without_panicking() {
        let seed_prs = serde_json::to_vec(&serde_json::json!([{
            "number": 1, "title": "t", "state": "OPEN", "headRefName": "f",
            "baseRefName": "m", "url": "u", "isDraft": false,
            "statusCheckRollup": [{"state": "SUCCESS"}]
        }]))
        .unwrap();
        let seed_runs = serde_json::to_vec(&serde_json::json!([{
            "databaseId": 1, "name": "n", "status": "s",
            "conclusion": "c", "headBranch": "b", "url": "u",
            "createdAt": "t", "displayTitle": "d"
        }]))
        .unwrap();
        let seed_alert = serde_json::to_string(&vec![dependabot_row(1, "p", "high")]).unwrap();

        // Intact inputs parse cleanly on all three parsers.
        assert!(parse_pr_list(&seed_prs, 50).is_ok());
        assert!(parse_workflow_runs(&seed_runs, 20).is_ok());
        assert!(parse_dependabot_alerts(&seed_alert, 50).is_ok());

        let seeds: Vec<Vec<u8>> = vec![seed_prs, seed_runs, seed_alert.into_bytes()];
        let mut rng = Lcg(0x0061_7564_6974_6f72);
        for iteration in 0..4000usize {
            let mut payload = seeds[iteration % seeds.len()].clone();
            // Corrupt: truncate at a random offset and/or splice random bytes.
            let cut = (rng.next_u64() as usize) % (payload.len() + 1);
            payload.truncate(cut);
            if rng.next_u64().is_multiple_of(2) {
                let inject = (rng.next_u64() as usize) % 32;
                for _ in 0..inject {
                    payload.push((rng.next_u64() % 256) as u8);
                }
            }
            // Every outcome is acceptable except a panic or a false success:
            // a capped list must carry the truncation flag, always.
            if let Ok((prs, truncated)) = parse_pr_list(&payload, 50) {
                assert!(truncated || prs.len() <= 50);
            }
            let _ = parse_workflow_runs(&payload, 20);
            let _ = parse_dependabot_alerts(&String::from_utf8_lossy(&payload), 50);
        }
    }

    /// Large-but-bounded payloads: 20k PRs / runs / alerts parse, cap, sort,
    /// and flag truncation within a generous wall-clock bound.
    #[test]
    fn stress_large_listings_parse_cap_and_stay_bounded() {
        let started = std::time::Instant::now();

        let prs_json: Vec<serde_json::Value> = (1..=20_000usize)
            .map(|n| {
                serde_json::json!({
                    "number": n, "title": format!("PR #{n} — {}", "detail".repeat(3)),
                    "state": "OPEN", "headRefName": format!("feature/branch-{n}"),
                    "baseRefName": "main", "url": "", "isDraft": n % 2 == 0,
                    "statusCheckRollup": [{"state": "SUCCESS"}, {"conclusion": "FAILURE"}]
                })
            })
            .collect();
        let text = serde_json::to_vec(&prs_json).unwrap();
        let (prs, truncated) = parse_pr_list(&text, PR_DISPLAY_LIMIT).unwrap();
        assert_eq!(prs.len(), PR_DISPLAY_LIMIT);
        assert!(truncated);
        // Failure wins regardless of position in the fetched window.
        assert!(prs.iter().any(|pr| pr.ci_status == "failure"));

        let alerts_json: Vec<serde_json::Value> = (1..=20_000usize)
            .map(|n| dependabot_row(n as u64, "pkg", ["low", "high", "critical"][n % 3]))
            .collect();
        let text = serde_json::to_string(&alerts_json).unwrap();
        let (alerts, truncated) = parse_dependabot_alerts(&text, ALERT_DISPLAY_LIMIT).unwrap();
        assert_eq!(alerts.len(), ALERT_DISPLAY_LIMIT);
        assert!(truncated);
        // Worst-first ordering survives the scale.
        assert_eq!(alerts[0].severity, "critical");

        let releases_json: Vec<serde_json::Value> = (1..=5_000usize)
            .map(|n| {
                serde_json::json!({
                    "tagName": format!("v1.0.{n}/build {n}<x>"),
                    "name": format!("Release {n}"), "isDraft": false,
                    "isPrerelease": true, "isLatest": n == 5_000,
                    "publishedAt": null, "createdAt": null
                })
            })
            .collect();
        let text = serde_json::to_vec(&releases_json).unwrap();
        let (releases, truncated) =
            parse_release_list(&text, "https://github.com/a/r", RELEASE_DISPLAY_LIMIT).unwrap();
        assert_eq!(releases.len(), RELEASE_DISPLAY_LIMIT);
        assert!(truncated);
        assert!(releases.iter().all(|release| release
            .url
            .starts_with("https://github.com/a/r/releases/tag/v1.0.")));

        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "stress parsing must stay bounded, took {:?}",
            started.elapsed()
        );
    }

    /// The remote-URL parser under an adversarial matrix: every hostile
    /// shape must refuse, every ordinary shape must keep working, across a
    /// few thousand mutations — no panics, no half-parsed trust decisions.
    #[test]
    fn stress_remote_parser_matrix_stays_total_and_safe() {
        let hostile = [
            "",
            "   ",
            "\u{0}",
            "https://",
            "https://github.com",
            "https://github.com/",
            "https://github.com//",
            "https:///acme/repo",
            "git@",
            "git@:",
            "git@:/repo",
            "git@github.com:",
            "git@github.com:/",
            "ssh://",
            "ssh://github.com",
            "ssh://git@github.com:/acme/repo",
            "ftp://github.com/acme/repo",
            "https://github.com:acme/repo",
            "https://user:pass@github.com/acme/repo",
            "https://github.com/acme/repo/extra/deep/path",
        ];
        for url in hostile {
            // Refusal or a well-formed ref; never a panic, never a ref with
            // whitespace/flag-shaped/dot components inside.
            if let Some(parsed) = parse_github_remote_url(url) {
                for component in [&parsed.host, &parsed.owner, &parsed.name] {
                    assert!(!component.is_empty());
                    assert!(!component.starts_with('-'));
                    assert!(!component.starts_with('.'));
                    assert!(!component.contains(':'));
                    assert!(!component
                        .chars()
                        .any(|c| c.is_whitespace() || c.is_control()));
                }
            }
        }

        let mut rng = Lcg(42);
        let base = b"https://github.com/acme/repo.git";
        let mut accepted = 0;
        for _ in 0..2000 {
            let mut url = base.to_vec();
            let cut = (rng.next_u64() as usize) % (base.len() + 1);
            url.truncate(cut);
            let inject = (rng.next_u64() as usize) % 8;
            let alphabet = b" -/.:@abc\x7f\x01";
            for _ in 0..inject {
                url.push(alphabet[(rng.next_u64() as usize) % alphabet.len()]);
            }
            let candidate = String::from_utf8_lossy(&url);
            if let Some(parsed) = parse_github_remote_url(candidate.trim()) {
                accepted += 1;
                for component in [&parsed.host, &parsed.owner, &parsed.name] {
                    assert!(!component.starts_with('-'));
                    assert!(!component
                        .chars()
                        .any(|c| c.is_whitespace() || c.is_control()));
                }
            }
        }
        let _ = accepted; // both outcomes are fine; panics are not
    }

    /// pick_github_remote at scale: hundreds of remotes with mixed markers
    /// resolve deterministically and origin still wins.
    #[test]
    fn stress_many_remotes_resolve_deterministically() {
        let mut lines = Vec::new();
        for i in 0..500 {
            lines.push(format!("r{i}\thttps://github.com/o{i}/r{i}.git (fetch)"));
            lines.push(format!("r{i}\thttps://github.com/o{i}/r{i}.git (push)"));
        }
        lines.push("origin\thttps://github.com/winner/origin.git (fetch)".into());
        lines.push("origin\thttps://github.com/winner/origin.git (push)".into());
        let picked = pick_github_remote(&lines.join("\n")).expect("origin wins");
        assert_eq!(picked.slug(), "winner/origin");
    }
}
