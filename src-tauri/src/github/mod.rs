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
    pub issues: Vec<IssueInfo>,
    pub issues_truncated: bool,
    pub issues_error: Option<String>,
    pub workflow_runs: Vec<WorkflowRunInfo>,
    /// Set when the `gh` workflow-run listing could not run or be parsed,
    /// while the rest of the context is still usable.
    #[serde(default)]
    pub runs_error: Option<String>,
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

/// Parses GitHub / GHES clone URLs (HTTPS, SSH, git protocol).
pub fn parse_github_remote_url(url: &str) -> Option<GitHubRepoRef> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_git = trimmed.strip_prefix("git://").unwrap_or(trimmed);
    let normalized = without_git.trim_end_matches('/').trim_end_matches(".git");

    if let Some(rest) = normalized.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        return split_owner_repo(host, path);
    }

    let rest = normalized
        .strip_prefix("ssh://git@")
        .or_else(|| normalized.strip_prefix("ssh://"))
        .or_else(|| normalized.strip_prefix("https://"))
        .or_else(|| normalized.strip_prefix("http://"))?;

    let rest = rest.strip_prefix("git@").unwrap_or(rest);
    let (host, path) = rest.split_once('/')?;
    split_owner_repo(host, path)
}

/// Hard upper bound for each parsed remote component (host, owner, name).
///
/// Real hosts and GitHub names are far shorter; this only stops a crafted
/// megabyte-scale URL from flowing whole into endpoints, flags, and the UI.
const MAX_REMOTE_COMPONENT_LEN: usize = 100;

/// A remote component is argv-safe and endpoint-safe: no leading `-` (it
/// would be re-parsed as a flag by `gh`'s CLI — e.g. `--repo -inbox/x`),
/// no whitespace or control characters (they would corrupt argv, endpoint
/// paths, built URLs, and error messages alike), and bounded length.
fn is_valid_remote_component(component: &str) -> bool {
    !component.is_empty()
        && component.len() <= MAX_REMOTE_COMPONENT_LEN
        && !component.starts_with('-')
        && !component
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
}

fn split_owner_repo(host: &str, path: &str) -> Option<GitHubRepoRef> {
    let host = host.split(':').next().unwrap_or(host).trim();
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    let owner = parts.next()?;
    let name = parts.next()?;
    if !(is_valid_remote_component(host)
        && is_valid_remote_component(owner)
        && is_valid_remote_component(name))
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

pub fn discover_github_remote(repo_path: &str) -> Result<Option<GitHubRepoRef>, String> {
    let repo = validate_repo(repo_path)?;
    let stdout = git_text(&repo, &["remote", "-v"])?;
    let mut first: Option<GitHubRepoRef> = None;
    let mut origin: Option<GitHubRepoRef> = None;
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let name = parts.next();
        if let Some(url) = parts.next() {
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
    }
    Ok(origin.or(first))
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
    for check in &checks {
        let state = check
            .get("state")
            .or_else(|| check.get("conclusion"))
            .or_else(|| check.get("status"))
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_ascii_uppercase();
        if state.contains("FAIL") || state == "FAILURE" || state == "ERROR" || state == "CANCELLED"
        {
            failed = true;
        } else if state == "PENDING"
            || state == "IN_PROGRESS"
            || state == "QUEUED"
            || state == "EXPECTED"
        {
            pending = true;
        }
    }
    if failed {
        "failure".into()
    } else if pending {
        "pending".into()
    } else {
        "success".into()
    }
}

fn gh_argv(remote: &GitHubRepoRef, leading: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = leading.iter().map(|s| (*s).to_string()).collect();
    args.extend(gh_repo_flags(remote));
    args
}

fn run_gh(
    remote: &GitHubRepoRef,
    leading: &[&str],
    timeout: Duration,
    cwd: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let args = gh_argv(remote, leading);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_command_in("gh", &refs, timeout, cwd)
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
    url: String,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
    #[serde(rename = "displayTitle")]
    display_title: Option<String>,
}

fn list_workflow_runs(remote: &GitHubRepoRef) -> Result<Vec<WorkflowRunInfo>, String> {
    let stdout = run_gh(
        remote,
        &[
            "run",
            "list",
            "--limit",
            "20",
            "--json",
            "databaseId,name,status,conclusion,headBranch,url,createdAt,displayTitle",
        ],
        Duration::from_secs(45),
        None,
    )?;
    parse_workflow_runs(&stdout)
}

fn parse_pr_list(stdout: &[u8]) -> Result<Vec<PullRequestInfo>, String> {
    let prs: Vec<GhPullRequest> = serde_json::from_slice(stdout)
        .map_err(|e| format!("could not parse gh pull-request output: {e}"))?;
    Ok(prs
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
        .collect())
}

fn parse_workflow_runs(stdout: &[u8]) -> Result<Vec<WorkflowRunInfo>, String> {
    let runs: Vec<GhWorkflowRun> = serde_json::from_slice(stdout)
        .map_err(|e| format!("could not parse gh workflow-run output: {e}"))?;
    Ok(runs
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
        .collect())
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
                format!("{}/releases/tag/{}", base_html_url, rel.tag_name.trim())
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
/// the HTTP status.
fn gh_api_error_message(output: &CapturedOutput) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(output.stdout_text().trim()) {
        if let Some(message) = value.get("message").and_then(Value::as_str) {
            if !message.is_empty() {
                return format!("{message} (HTTP {})", output.status_code);
            }
        }
    }
    let stderr = output.stderr_text();
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
    alerts.sort_by(|a, b| {
        severity_rank(&a.severity)
            .cmp(&severity_rank(&b.severity))
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

pub fn checkout_pull_request(repo_path: &str, number: u64) -> Result<String, String> {
    if number == 0 {
        return Err("Invalid pull request number".into());
    }
    let repo = validate_repo(repo_path)?;
    let remote = discover_github_remote(repo_path)?
        .ok_or_else(|| "No GitHub remote configured".to_string())?;
    if !gh_cli_present() {
        return Err("GitHub CLI (`gh`) is not installed or not on PATH".into());
    }
    let n = number.to_string();
    let stdout = run_gh(
        &remote,
        &["pr", "checkout", &n],
        Duration::from_secs(90),
        Some(&repo),
    )?;
    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
}

pub fn validate_issue_payload(title: &str, body: &str, labels: &[String]) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Issue title must not be empty".into());
    }
    if title.chars().count() > 256 {
        return Err("Issue title exceeds the 256 character limit".into());
    }
    if body.len() > 64 * 1024 {
        return Err("Issue body exceeds the 64 KiB limit".into());
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
        if label.chars().count() > 128 || label.contains(['\n', '\r', '\0']) {
            return Err("Issue label is invalid or exceeds 128 characters".into());
        }
    }
    Ok(())
}

pub fn create_issue(
    repo_path: &str,
    title: &str,
    body: &str,
    labels: &[String],
) -> Result<String, String> {
    validate_issue_payload(title, body, labels)?;
    let repo = validate_repo(repo_path)?;
    let remote = discover_github_remote(repo_path)?
        .ok_or_else(|| "No GitHub remote configured".to_string())?;
    if !gh_cli_present() {
        return Err("GitHub CLI (`gh`) is not installed or not on PATH".into());
    }

    let mut args = vec![
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
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let stdout = run_gh(&remote, &refs, Duration::from_secs(90), Some(&repo))?;
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
                issues: Vec::new(),
                issues_truncated: false,
                runs_error: None,
                issues_error: None,
                workflow_runs: Vec::new(),
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
                issues: Vec::new(),
                issues_truncated: false,
                runs_error: None,
                issues_error: None,
                workflow_runs: Vec::new(),
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
            issues: Vec::new(),
            issues_truncated: false,
            runs_error: None,
            issues_error: None,
            workflow_runs: Vec::new(),
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
        (workflow_runs, runs_error),
    ) = std::thread::scope(|s| {
        let issues_handle = s.spawn(|| match list_issues(&remote) {
            Ok((issues, truncated)) => (issues, truncated, None),
            Err(error) => (Vec::new(), false, Some(error)),
        });
        let releases_handle = s.spawn(|| match list_releases(&remote) {
            Ok((releases, truncated)) => (releases, truncated, None),
            Err(error) => (Vec::new(), false, Some(error)),
        });
        let pr_handle = s.spawn(|| {
            run_gh(
                &remote,
                &[
                    "pr",
                    "list",
                    "--state",
                    "open",
                    "--limit",
                    "50",
                    "--json",
                    "number,title,state,headRefName,baseRefName,url,isDraft,statusCheckRollup",
                ],
                Duration::from_secs(45),
                None,
            )
        });
        let runs_handle = s.spawn(|| match list_workflow_runs(&remote) {
            Ok(runs) => (runs, None),
            Err(e) => (Vec::new(), Some(e)),
        });

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
                .unwrap_or_else(|_| (Vec::new(), Some("thread panic".into()))),
        )
    });

    let mut warnings: Vec<String> = Vec::new();
    if let Some(ref issue_error) = issues_error {
        warnings.push(format!("Issue listing failed: {issue_error}"));
    }
    if let Some(ref release_error) = releases_error {
        warnings.push(format!("Release listing failed: {release_error}"));
    }
    if let Some(ref run_error) = runs_error {
        warnings.push(format!("Workflow run listing failed: {run_error}"));
    }

    match pr_outcome {
        Ok(stdout) => {
            // A parse failure no longer poisons the whole context: the
            // repository facts are still real, so degrade the PR section and
            // carry the reason in `warnings` where the UI can show it.
            let pull_requests = match parse_pr_list(&stdout) {
                Ok(prs) => prs,
                Err(e) => {
                    warnings.push(format!(
                        "Pull request listing failed: {e}. The list may be incomplete."
                    ));
                    Vec::new()
                }
            };
            GitHubContext {
                available: true,
                cli_present: true,
                host: remote.host,
                owner: remote.owner,
                repo: remote.name,
                html_url,
                pull_requests,
                issues,
                issues_truncated,
                issues_error,
                workflow_runs,
                runs_error,
                releases,
                releases_truncated,
                releases_error,
                error: None,
                warnings,
            }
        }
        Err(e) => GitHubContext {
            available: false,
            cli_present: true,
            host: remote.host,
            owner: remote.owner,
            repo: remote.name,
            html_url,
            pull_requests: Vec::new(),
            issues,
            issues_truncated,
            issues_error,
            workflow_runs: Vec::new(),
            runs_error: None,
            releases,
            releases_truncated,
            releases_error,
            error: Some(e),
            warnings,
        },
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
                parse_pr_list(garbage).is_err(),
                "pr parse must fail loudly on {:?}",
                String::from_utf8_lossy(garbage)
            );
            assert!(
                parse_workflow_runs(garbage).is_err(),
                "run parse must fail loudly on {:?}",
                String::from_utf8_lossy(garbage)
            );
        }
    }

    #[test]
    fn valid_gh_output_maps_into_context_types() {
        let prs = parse_pr_list(
            br#"[{"number":7,"title":"Fix it","state":"OPEN","headRefName":"f",
                 "baseRefName":"main","url":"https://x/7","isDraft":true,
                 "statusCheckRollup":null}]"#,
        )
        .unwrap();
        let pr = prs.first().expect("one pr");
        assert_eq!(pr.number, 7);
        assert!(pr.is_draft);
        assert_eq!(pr.ci_status, "unknown");

        let runs = parse_workflow_runs(
            br#"[{"databaseId":42,"name":"ci","status":"completed",
                  "conclusion":"success","headBranch":"main","url":"https://x/42",
                  "createdAt":"2026-01-01","displayTitle":"build"}]"#,
        )
        .unwrap();
        let run = runs.first().expect("one run");
        assert_eq!(run.id, 42);
        assert_eq!(run.title, "build");
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

    /// The empty host that used to be special-cased is covered by the
    /// component rule, and so is an owner-less or name-less path.
    #[test]
    fn degenerate_remote_shapes_stay_refused() {
        assert!(split_owner_repo("", "a/b").is_none());
        assert!(split_owner_repo("github.com", "").is_none());
        assert!(split_owner_repo("github.com", "only-owner").is_none());
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
        let err = checkout_pull_request("/tmp", 0).unwrap_err();
        assert!(err.to_lowercase().contains("invalid"));
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
        assert!(create_issue("/tmp", " ", "body", &[]).is_err());
        assert!(create_issue("/tmp", &"x".repeat(257), "body", &[]).is_err());
        assert!(create_issue("/tmp", "title", &"x".repeat(65 * 1024), &[]).is_err());
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
}
