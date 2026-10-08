//! Is there a git this app can drive?
//!
//! Every feature assumes one, and without this probe the first sign of a
//! missing or too-old git was whichever command happened to run first:
//! "Failed to spawn git: no such program", or `git: 'switch' is not a git
//! command` from a git older than `restore`/`switch` (2.23). The probe runs
//! the same resolved binary every other call does and says, in one sentence
//! a user can act on, what is wrong.

use super::{classify_tool_probe, git_command, run_bounded, CapturedOutput, ToolProbe};
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// Oldest git this app supports: 2.23 added `git restore` and `git switch`,
/// which the checkout and discard paths run.
pub const MIN_GIT_VERSION: (u32, u32, u32) = (2, 23, 0);

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitPreflightStatus {
    /// Git ran and is new enough.
    Ok,
    /// Nothing named `git` exists where the app looks.
    Missing,
    /// Git ran and is older than [`MIN_GIT_VERSION`].
    Outdated,
    /// Git exists but did not report a version (a broken install, the
    /// macOS developer-tools shim with no tools behind it).
    Broken,
    /// The probe never started (the app was busy). Says nothing either way.
    Unchecked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPreflight {
    pub status: GitPreflightStatus,
    /// `git version` output's first line, when git ran.
    pub version: Option<String>,
    /// What to tell the user; `None` when `status` is `Ok`.
    pub message: Option<String>,
}

/// `2.39.3` from `git version 2.39.3 (Apple Git-146)` or
/// `git version 2.45.1.windows.1`. Missing minor or patch read as zero.
pub(crate) fn parse_git_version(line: &str) -> Option<(u32, u32, u32)> {
    let rest = line.trim().strip_prefix("git version ")?;
    let mut parts = rest
        .split(|c: char| !c.is_ascii_digit())
        .take_while(|part| !part.is_empty())
        .map(|part| part.parse::<u32>().ok());
    let major = parts.next()??;
    let minor = parts.next().flatten().unwrap_or(0);
    let patch = parts.next().flatten().unwrap_or(0);
    Some((major, minor, patch))
}

fn minimum_text() -> String {
    let (major, minor, _) = MIN_GIT_VERSION;
    format!("{major}.{minor}")
}

/// Turns one probe run into the answer the UI shows.
pub(crate) fn assess(probe: ToolProbe) -> GitPreflight {
    let min = minimum_text();
    match probe {
        ToolProbe::Present(line) => match parse_git_version(&line) {
            Some(version) if version >= MIN_GIT_VERSION => GitPreflight {
                status: GitPreflightStatus::Ok,
                version: Some(line),
                message: None,
            },
            Some((major, minor, patch)) => GitPreflight {
                status: GitPreflightStatus::Outdated,
                message: Some(format!(
                    "Git {major}.{minor}.{patch} is too old. GitPulse needs Git {min} or newer \
                     (for git switch and git restore). Update Git, then restart GitPulse."
                )),
                version: Some(line),
            },
            None => GitPreflight {
                status: GitPreflightStatus::Broken,
                message: Some(format!(
                    "Git ran but reported an unrecognised version ({line}). \
                     GitPulse needs Git {min} or newer."
                )),
                version: Some(line),
            },
        },
        ToolProbe::NotFound(_) => GitPreflight {
            status: GitPreflightStatus::Missing,
            version: None,
            message: Some(format!(
                "Git was not found. Install Git {min} or newer (https://git-scm.com/downloads), \
                 then restart GitPulse."
            )),
        },
        ToolProbe::FoundButFailed(detail) => GitPreflight {
            status: GitPreflightStatus::Broken,
            version: None,
            message: Some(format!(
                "Git is installed but could not run: {detail}. On macOS, run \
                 `xcode-select --install` or install Git {min} or newer."
            )),
        },
        ToolProbe::NotRun(detail) => GitPreflight {
            status: GitPreflightStatus::Unchecked,
            version: None,
            message: Some(format!("Git could not be checked yet: {detail}")),
        },
    }
}

/// Probes git, remembering only a passing answer: a user who installs or
/// updates git after a failed probe is re-checked on the next call.
pub fn git_preflight() -> GitPreflight {
    static PASSED: Mutex<Option<GitPreflight>> = Mutex::new(None);
    if let Some(passed) = PASSED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
    {
        return passed;
    }
    let result =
        run_bounded(git_command(None, &["version"]), "git", PROBE_TIMEOUT, None).map(|run| {
            CapturedOutput {
                stdout: run.stdout,
                stderr: run.stderr,
                success: run.success,
                status_code: run.status_code,
            }
        });
    let answer = assess(classify_tool_probe("git", result));
    if answer.status == GitPreflightStatus::Ok {
        *PASSED.lock().unwrap_or_else(PoisonError::into_inner) = Some(answer.clone());
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_version_lines_git_prints_on_each_platform() {
        assert_eq!(
            parse_git_version("git version 2.39.3 (Apple Git-146)"),
            Some((2, 39, 3))
        );
        assert_eq!(
            parse_git_version("git version 2.45.1.windows.1"),
            Some((2, 45, 1))
        );
        assert_eq!(parse_git_version("git version 2.23.0"), Some((2, 23, 0)));
        assert_eq!(parse_git_version("git version 3"), Some((3, 0, 0)));
        assert_eq!(parse_git_version("git version 2.22.5\n"), Some((2, 22, 5)));
        assert_eq!(parse_git_version("hub version 2.14.2"), None);
        assert_eq!(parse_git_version(""), None);
    }

    #[test]
    fn the_minimum_is_inclusive_and_older_is_outdated() {
        let ok = assess(ToolProbe::Present("git version 2.23.0".into()));
        assert_eq!(ok.status, GitPreflightStatus::Ok);
        assert_eq!(ok.message, None);
        let old = assess(ToolProbe::Present("git version 2.22.5".into()));
        assert_eq!(old.status, GitPreflightStatus::Outdated);
        let message = old.message.unwrap();
        assert!(
            message.contains("2.22.5") && message.contains("2.23"),
            "{message}"
        );
    }

    #[test]
    fn absence_is_only_reported_when_the_os_found_nothing() {
        let missing = assess(ToolProbe::NotFound(
            "Failed to spawn git: no such program".into(),
        ));
        assert_eq!(missing.status, GitPreflightStatus::Missing);
        assert!(missing.message.unwrap().contains("Git was not found"));
        let busy = assess(ToolProbe::NotRun("git deferred under load".into()));
        assert_eq!(
            busy.status,
            GitPreflightStatus::Unchecked,
            "a shed probe is not a missing git"
        );
        let shim = assess(ToolProbe::FoundButFailed(
            "xcrun: error: invalid active developer path".into(),
        ));
        assert_eq!(shim.status, GitPreflightStatus::Broken);
        assert!(shim.message.unwrap().contains("xcrun: error"));
    }

    #[test]
    fn the_git_on_this_machine_passes() {
        let answer = git_preflight();
        assert_eq!(answer.status, GitPreflightStatus::Ok, "{answer:?}");
        assert!(answer.version.unwrap().starts_with("git version "));
    }
}
