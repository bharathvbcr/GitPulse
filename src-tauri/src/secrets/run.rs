//! PATH probe, scrubbed spawn, and fixed argv for Kingfisher.

use super::parse::{parse_kingfisher_json, SecretsReport};
use crate::engine::git_cli::{run_bounded_capped, validate_repo, Incomplete};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Wall-clock budget for one working-tree secrets scan.
pub const SCAN_DEADLINE: Duration = Duration::from_secs(5 * 60);
/// Version probe budget.
pub const VERSION_DEADLINE: Duration = Duration::from_secs(5);
/// Stdout budget. Hitting it fails the scan entirely — never a partial list.
pub const STDOUT_CAP: usize = 8 * 1024 * 1024;
/// Kingfisher exit codes that mean a completed scan (no findings / findings /
/// validated findings). Any other status is a failure.
pub const SUCCESS_EXITS: [i32; 3] = [0, 200, 205];

const INSTALL_HINT: &str =
    "kingfisher is not installed or not on PATH. Install with: brew install kingfisher";

/// Kingfisher's `scan_nested_repos` defaults to true and lacks `ArgAction::Set`,
/// so `--scan-nested-repos false` is not accepted. Our scans therefore walk
/// nested repositories; the report must say so.
pub fn nested_repos_scanning_enabled() -> bool {
    true
}

/// Fixed argv after the binary path. Nothing else — no repo config, no
/// self-update, no validation, no git history.
pub fn build_scan_argv(repo: impl AsRef<Path>) -> Vec<String> {
    vec![
        "--no-update-check".into(),
        "scan".into(),
        repo.as_ref().to_string_lossy().into_owned(),
        "--format".into(),
        "json".into(),
        "--no-validate".into(),
        "--git-history".into(),
        "none".into(),
        "--redact".into(),
        "--confidence".into(),
        "medium".into(),
        "--quiet".into(),
    ]
}

pub fn build_version_argv() -> [&'static str; 1] {
    ["--version"]
}

/// Environment map for the child: `PATH`, `HOME`, and `NO_COLOR` only.
///
/// Ambient credentials (`AWS_*`, `GITHUB_TOKEN`, `KF_*`, `KINGFISHER_*`, …)
/// must not reach the process even if `--no-validate` were ever omitted.
pub fn build_scrubbed_env(
    path: Option<&OsStr>,
    home: Option<&OsStr>,
    _github_token: Option<&OsStr>,
    _aws_key: Option<&OsStr>,
) -> BTreeMap<String, OsString> {
    let mut env = BTreeMap::new();
    if let Some(path) = path {
        env.insert("PATH".into(), path.to_os_string());
    }
    if let Some(home) = home {
        env.insert("HOME".into(), home.to_os_string());
    }
    env.insert("NO_COLOR".into(), OsString::from("1"));
    env
}

fn process_scrubbed_env() -> BTreeMap<String, OsString> {
    build_scrubbed_env(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .as_deref(),
        None,
        None,
    )
}

/// Resolve `kingfisher` on `PATH` only — no GUI-launch fallback directories.
pub fn resolve_kingfisher_on_path() -> Option<PathBuf> {
    resolve_kingfisher_on_path_var(std::env::var_os("PATH").as_deref())
}

pub fn resolve_kingfisher_on_path_var(path_var: Option<&OsStr>) -> Option<PathBuf> {
    let path_var = path_var?;
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(kingfisher_bin_name());
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn kingfisher_bin_name() -> &'static str {
    if cfg!(windows) {
        "kingfisher.exe"
    } else {
        "kingfisher"
    }
}

fn scrubbed_command(binary: &Path) -> Command {
    let mut cmd = Command::new(binary);
    cmd.env_clear();
    for (key, value) in process_scrubbed_env() {
        cmd.env(key, value);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd
}

pub fn missing_binary_report() -> SecretsReport {
    SecretsReport::unavailable(INSTALL_HINT)
}

pub fn failed_report(reason: &str, version: Option<String>) -> SecretsReport {
    SecretsReport::failed(reason, version)
}

/// Map a finished child (any exit) into a report without retaining stdout.
pub fn report_from_exit(status: i32, stdout: &[u8], version: Option<String>) -> SecretsReport {
    if !SUCCESS_EXITS.contains(&status) {
        return failed_report(&format!("kingfisher exited with status {status}"), version);
    }
    match parse_kingfisher_json(stdout, version.clone(), nested_repos_scanning_enabled()) {
        Ok(report) => report,
        Err(_) => failed_report("kingfisher output is not valid JSON", version),
    }
}

/// Probe version, scan the working tree, parse by allowlist, drop stdout.
pub fn scan_secrets(repo_path: &str) -> Result<SecretsReport, String> {
    let repo = validate_repo(repo_path)?;
    let Some(binary) = resolve_kingfisher_on_path() else {
        return Ok(missing_binary_report());
    };

    let version = probe_version(&binary);
    let mut cmd = scrubbed_command(&binary);
    cmd.args(build_scan_argv(&repo));

    let run = match run_bounded_capped(cmd, "kingfisher", SCAN_DEADLINE, None, STDOUT_CAP) {
        Ok(run) => run,
        Err(err) => {
            // Timeouts and spawn failures from the runner carry a fixed shape;
            // strip any accidental payload before surfacing.
            let reason = if err.contains("timed out") || err.contains("TIMEOUT") {
                "kingfisher timed out".to_string()
            } else {
                "kingfisher failed to run".to_string()
            };
            return Ok(failed_report(&reason, version));
        }
    };

    // Destructure so stderr is dropped here and never reaches the report.
    let crate::engine::git_cli::BoundedRun {
        stdout,
        stderr: _stderr,
        status_code,
        incomplete,
        ..
    } = run;

    if let Some(incomplete) = incomplete {
        let reason = match incomplete {
            Incomplete::OverCap(_) => "kingfisher output was truncated",
            Incomplete::Unread(_) => "kingfisher output could not be read to the end",
        };
        return Ok(failed_report(reason, version));
    }

    Ok(report_from_exit(status_code, &stdout, version))
}

fn probe_version(binary: &Path) -> Option<String> {
    let mut cmd = scrubbed_command(binary);
    cmd.args(build_version_argv());
    let run = run_bounded_capped(cmd, "kingfisher", VERSION_DEADLINE, None, 64 * 1024).ok()?;
    if run.incomplete.is_some() || !run.success {
        return None;
    }
    let text = String::from_utf8_lossy(&run.stdout);
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    Some(line.to_string())
}
