//! GitHub Actions CI/CD: listing, dispatching, re-running and cancelling.
//!
//! Read-only context (runs, releases) lives in `super`; everything here is an
//! action against the repository's workflows, so every mutating call is
//! policy-gated: the command wrappers build the exact argv the executor will
//! run via the same `*_argv` builders this module owns, and a refusal happens
//! before the first byte leaves the process.
//!
//! Repo pinning uses gh's documented `[HOST/]OWNER/REPO` form for `-R` —
//! these subcommands inherit only `--repo` (verified against gh 2.95.0), not
//! the separate `--hostname` flag the older repo-context calls use.

use super::{discover_github_remote, gh_cli_present, GitHubRepoRef};
use crate::engine::git_cli::{capture_command, validate_repo};
use crate::engine::git_writer::validate_ref_name;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const GH_CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Upper bound on workflows shown. One extra row is fetched so a capped list
/// is reported instead of silently looking complete.
pub const WORKFLOW_DISPLAY_LIMIT: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowInfo {
    pub id: u64,
    pub name: String,
    /// Repository-relative workflow file path (e.g.
    /// `.github/workflows/ci.yml`). This — not the human name — is what the
    /// UI passes back to `workflow run`: it is stable and argv-safe.
    pub path: String,
    /// GitHub's state vocabulary: `active`, `disabled_manually`,
    /// `disabled_inactivity`.
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowsReport {
    pub available: bool,
    pub cli_present: bool,
    pub workflows: Vec<WorkflowInfo>,
    pub truncated: bool,
    pub error: Option<String>,
}

impl WorkflowsReport {
    pub fn unavailable(cli_present: bool, error: Option<String>) -> Self {
        WorkflowsReport {
            available: false,
            cli_present,
            workflows: Vec::new(),
            truncated: false,
            error,
        }
    }
}

/// The value of gh's `-R [HOST/]OWNER/REPO` flag for this remote.
///
/// The host prefix pins GHES calls to the right instance; on github.com it is
/// omitted so the line stays exactly as documented.
fn repo_flag_value(remote: &GitHubRepoRef) -> String {
    if remote.host.eq_ignore_ascii_case("github.com") {
        remote.slug()
    } else {
        format!("{}/{}/{}", remote.host, remote.owner, remote.name)
    }
}

fn append_repo_flags(args: &mut Vec<String>, remote: &GitHubRepoRef) {
    args.push("--repo".to_string());
    args.push(repo_flag_value(remote));
}

#[derive(Debug, Deserialize)]
struct GhWorkflow {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    state: String,
}

/// Leading argv for `gh workflow list`, before the repo-pinning flags.
fn workflow_list_leading_args(fetch_limit: &str) -> Vec<String> {
    [
        "workflow",
        "list",
        "--limit",
        fetch_limit,
        "--json",
        "id,name,path,state",
    ]
    .iter()
    .map(|arg| (*arg).to_string())
    .collect()
}

pub fn parse_workflow_list(
    stdout: &[u8],
    display_limit: usize,
) -> Result<(Vec<WorkflowInfo>, bool), String> {
    let mut workflows: Vec<GhWorkflow> = serde_json::from_slice(stdout)
        .map_err(|error| format!("could not parse gh workflow output: {error}"))?;
    let truncated = workflows.len() > display_limit;
    workflows.truncate(display_limit);
    Ok((
        workflows
            .into_iter()
            .map(|wf| WorkflowInfo {
                id: wf.id,
                name: if wf.name.trim().is_empty() {
                    // A workflow file with no top-level `name:` falls back to
                    // its file name in the UI; empty would render as nothing.
                    workflow_file_label(&wf.path)
                } else {
                    wf.name
                },
                path: wf.path,
                state: wf.state,
            })
            .collect(),
        truncated,
    ))
}

/// `.github/workflows/release.yml` → `release.yml`.
fn workflow_file_label(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

pub fn list_workflows(remote: &GitHubRepoRef) -> Result<(Vec<WorkflowInfo>, bool), String> {
    let fetch_limit = (WORKFLOW_DISPLAY_LIMIT + 1).to_string();
    let mut args = workflow_list_leading_args(&fetch_limit);
    append_repo_flags(&mut args, remote);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let stdout = capture_command("gh", &refs, None, GH_CALL_TIMEOUT, &[])?;
    if !stdout.success {
        let err = stdout.stderr_text();
        return Err(if err.is_empty() {
            format!("gh exited {}", stdout.status_code)
        } else {
            err
        });
    }
    parse_workflow_list(&stdout.stdout, WORKFLOW_DISPLAY_LIMIT)
}

/// Upper bound for a workflow selector passed to `gh workflow run`. Real
/// paths and names are far shorter; this caps crafted input before it can
/// reach argv or logs.
const MAX_SELECTOR_LEN: usize = 200;

/// Validates a `gh workflow run` selector (workflow file path or name).
///
/// The selector is one argv element — never shell-interpolated — so spaces
/// inside workflow *names* are legitimate and allowed. What must never pass:
/// flag-shaped values (`-f`, `--repo=evil`) that gh would re-parse as flags,
/// control characters that corrupt argv and logs, and unbounded payloads.
pub fn validate_workflow_selector(selector: &str) -> Result<String, String> {
    let trimmed = selector.trim();
    if trimmed.is_empty() {
        return Err("Workflow selector must not be empty".into());
    }
    if trimmed.len() > MAX_SELECTOR_LEN {
        return Err(format!(
            "Workflow selector exceeds the {MAX_SELECTOR_LEN} character limit"
        ));
    }
    if trimmed.starts_with('-') {
        return Err("Workflow selector must not start with '-'".into());
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err("Workflow selector contains control characters".into());
    }
    Ok(trimmed.to_string())
}

/// The exact argv [`trigger_workflow`] runs. Kept separate from execution so
/// the command gate judges the real line, not a rendering of it.
pub fn trigger_workflow_argv(
    remote: &GitHubRepoRef,
    selector: &str,
    r#ref: &str,
) -> Result<Vec<String>, String> {
    let selector = validate_workflow_selector(selector)?;
    // A ref reaches gh as `--ref <value>`; git's own ref rules are the right
    // vocabulary here and already refuse flag-shaped or malformed names.
    validate_ref_name(r#ref)?;
    let mut args = vec![
        "workflow".to_string(),
        "run".to_string(),
        selector,
        "--ref".to_string(),
        r#ref.to_string(),
    ];
    append_repo_flags(&mut args, remote);
    Ok(args)
}

pub fn trigger_workflow(
    repo_path: &str,
    remote: &GitHubRepoRef,
    selector: &str,
    r#ref: &str,
) -> Result<String, String> {
    let args = trigger_workflow_argv(remote, selector, r#ref)?;
    run_gh_in(repo_path, &args)
}

/// The exact argv [`rerun_workflow_run`] runs.
pub fn rerun_run_argv(remote: &GitHubRepoRef, run_id: u64) -> Result<Vec<String>, String> {
    validate_run_id(run_id)?;
    let mut args = vec!["run".to_string(), "rerun".to_string(), run_id.to_string()];
    append_repo_flags(&mut args, remote);
    Ok(args)
}

pub fn rerun_workflow_run(
    repo_path: &str,
    remote: &GitHubRepoRef,
    run_id: u64,
) -> Result<String, String> {
    let args = rerun_run_argv(remote, run_id)?;
    run_gh_in(repo_path, &args)
}

/// The exact argv [`cancel_workflow_run`] runs.
pub fn cancel_run_argv(remote: &GitHubRepoRef, run_id: u64) -> Result<Vec<String>, String> {
    validate_run_id(run_id)?;
    let mut args = vec!["run".to_string(), "cancel".to_string(), run_id.to_string()];
    append_repo_flags(&mut args, remote);
    Ok(args)
}

pub fn cancel_workflow_run(
    repo_path: &str,
    remote: &GitHubRepoRef,
    run_id: u64,
) -> Result<String, String> {
    let args = cancel_run_argv(remote, run_id)?;
    run_gh_in(repo_path, &args)
}

/// Run ids are positive database ids; zero would be a fabricated target.
fn validate_run_id(run_id: u64) -> Result<(), String> {
    if run_id == 0 {
        return Err("Invalid workflow run id".into());
    }
    Ok(())
}

/// Runs one gh invocation inside the repository with the module's timeout.
/// Non-zero exits surface gh's stderr, which carries the API's own reason.
fn run_gh_in(repo_path: &str, args: &[String]) -> Result<String, String> {
    let repo = validate_repo(repo_path)?;
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = capture_command("gh", &refs, Some(&repo), GH_CALL_TIMEOUT, &[])?;
    if !output.success {
        let err = output.stderr_text();
        return Err(if err.is_empty() {
            format!("gh exited {}", output.status_code)
        } else {
            err
        });
    }
    Ok(output.stdout_text().trim().to_string())
}

/// Loads the workflow list for the opened repository, degrading exactly like
/// [`super::load_github_context`] does: every preventing condition comes back
/// as an unavailable report with an explicit reason, never as "no workflows".
pub fn load_workflows_report(repo_path: &str) -> WorkflowsReport {
    let cli_present = gh_cli_present();
    let remote = match discover_github_remote(repo_path) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return WorkflowsReport::unavailable(
                cli_present,
                Some("No GitHub remote configured".into()),
            )
        }
        Err(e) => return WorkflowsReport::unavailable(cli_present, Some(e)),
    };
    if !cli_present {
        return WorkflowsReport::unavailable(
            false,
            Some("GitHub CLI (`gh`) is not installed or not on PATH".into()),
        );
    }
    match list_workflows(&remote) {
        Ok((workflows, truncated)) => WorkflowsReport {
            available: true,
            cli_present: true,
            workflows,
            truncated,
            error: None,
        },
        Err(e) => WorkflowsReport::unavailable(true, Some(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(host: &str) -> GitHubRepoRef {
        GitHubRepoRef {
            host: host.to_string(),
            owner: "acme".to_string(),
            name: "gitpulse".to_string(),
        }
    }

    #[test]
    fn repo_flag_value_prefixes_host_only_for_ghes() {
        assert_eq!(repo_flag_value(&remote("github.com")), "acme/gitpulse");
        assert_eq!(
            repo_flag_value(&remote("acme.ghe.com")),
            "acme.ghe.com/acme/gitpulse"
        );
    }

    #[test]
    fn valid_workflow_output_maps_and_caps() {
        let rows: Vec<serde_json::Value> = (1..=3)
            .map(|i| {
                serde_json::json!({
                    "id": i,
                    "name": if i == 2 { "".to_string() } else { format!("WF {i}") },
                    "path": format!(".github/workflows/w{i}.yml"),
                    "state": "active"
                })
            })
            .collect();
        let json = serde_json::to_vec(&rows).unwrap();
        let (workflows, truncated) = parse_workflow_list(&json, 2).unwrap();
        assert_eq!(workflows.len(), 2);
        assert!(truncated);
        // Empty-name workflows fall back to their file label.
        assert_eq!(workflows[1].name, "w2.yml");
    }

    #[test]
    fn garbage_workflow_output_is_a_parse_error_not_an_empty_success() {
        for garbage in [&b""[..], b"warning preamble\n[]", b"{\"no\":true}"] {
            assert!(
                parse_workflow_list(garbage, 50).is_err(),
                "workflow parse must fail loudly on {:?}",
                String::from_utf8_lossy(garbage)
            );
        }
    }

    #[test]
    fn selector_validation_refuses_hostile_shapes() {
        assert!(validate_workflow_selector("").is_err());
        assert!(validate_workflow_selector("   ").is_err());
        assert!(validate_workflow_selector("-f").is_err());
        assert!(validate_workflow_selector("--repo=evil").is_err());
        assert!(validate_workflow_selector("ci\u{7}.yml").is_err());
        assert!(validate_workflow_selector(&"x".repeat(201)).is_err());
        // Ordinary shapes stay valid: file paths, and names with spaces
        // (one argv element, never shell-interpolated).
        assert_eq!(
            validate_workflow_selector(".github/workflows/ci.yml").unwrap(),
            ".github/workflows/ci.yml"
        );
        assert_eq!(
            validate_workflow_selector("  Code Coverage  ").unwrap(),
            "Code Coverage"
        );
    }

    #[test]
    fn trigger_argv_is_exact_and_validated() {
        let argv = trigger_workflow_argv(&remote("github.com"), ".github/workflows/ci.yml", "main")
            .unwrap();
        assert_eq!(
            argv,
            vec![
                "workflow",
                "run",
                ".github/workflows/ci.yml",
                "--ref",
                "main",
                "--repo",
                "acme/gitpulse"
            ]
        );
        // Flag-shaped selectors and malformed refs are refused before argv
        // exists at all.
        assert!(trigger_workflow_argv(&remote("github.com"), "-f", "main").is_err());
        assert!(trigger_workflow_argv(&remote("github.com"), "ci.yml", "bad ref").is_err());
        assert!(trigger_workflow_argv(&remote("github.com"), "ci.yml", "").is_err());
    }

    #[test]
    fn rerun_and_cancel_argv_pin_the_repo() {
        let ghes = remote("acme.ghe.com");
        assert_eq!(
            rerun_run_argv(&ghes, 42).unwrap(),
            vec!["run", "rerun", "42", "--repo", "acme.ghe.com/acme/gitpulse"]
        );
        assert_eq!(
            cancel_run_argv(&remote("github.com"), 7).unwrap(),
            vec!["run", "cancel", "7", "--repo", "acme/gitpulse"]
        );
        assert_eq!(
            rerun_run_argv(&remote("github.com"), 0).unwrap_err(),
            "Invalid workflow run id"
        );
        assert_eq!(
            cancel_run_argv(&remote("github.com"), 0).unwrap_err(),
            "Invalid workflow run id"
        );
    }
}
