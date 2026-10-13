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

/// Oldest git that may clone. 2.45.1 is the first release with the fixes for
/// CVE-2024-32002 (a hostile repository's symlinked submodule writes into
/// `.git` during clone and runs a hook), CVE-2024-32004 and CVE-2024-32465
/// (local clones of a crafted repository). The Apple developer-tools git
/// often reports an older version than that, and a version string is the only
/// signal we have, so it is refused with instructions rather than trusted.
pub const MIN_CLONE_GIT_VERSION: (u32, u32, u32) = (2, 45, 1);

/// Oldest git that may initialise submodules: CVE-2025-48384 (a carriage
/// return in a submodule path escapes the checkout) is fixed from 2.50.1.
pub const MIN_SUBMODULE_GIT_VERSION: (u32, u32, u32) = (2, 50, 1);

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

/// Whether `preflight`'s git may run a clone (`submodules` false) or any
/// operation that fetches and checks out submodules (`submodules` true).
///
/// Fails closed: a git that could not be probed, or whose version cannot be
/// read, is refused. The message says what to install, because the person who
/// hits this cannot act on a bare version number.
pub(crate) fn assess_for_clone(preflight: &GitPreflight, submodules: bool) -> Result<(), String> {
    let (need, why) = if submodules {
        (
            MIN_SUBMODULE_GIT_VERSION,
            "recursing into submodules (CVE-2025-48384, CVE-2024-32002)",
        )
    } else {
        (
            MIN_CLONE_GIT_VERSION,
            "cloning (CVE-2024-32002, CVE-2024-32004, CVE-2024-32465)",
        )
    };
    if preflight.status != GitPreflightStatus::Ok {
        return Err(preflight
            .message
            .clone()
            .unwrap_or_else(|| "Git could not be verified, so it was not used to clone".into()));
    }
    let line = preflight.version.clone().unwrap_or_default();
    match parse_git_version(&line) {
        Some(found) if found >= need => Ok(()),
        Some((major, minor, patch)) => Err(format!(
            "Git {major}.{minor}.{patch} has known security flaws when {why}. Update to Git \
             {}.{}.{} or newer (for example `brew install git`), then restart GitPulse.",
            need.0, need.1, need.2
        )),
        None => Err(format!(
            "Git reported an unrecognised version ({line}); refusing to clone with it."
        )),
    }
}

/// Oldest git whose `rev-list`/`log` accept `--since-as-filter`: the date
/// bound that filters commits instead of ending the walk at the first old
/// committer date. A feature floor, not the app's: [`MIN_GIT_VERSION`] stays
/// where it is, and a caller below this one walks by count instead.
///
/// The per-feature capability table that ft-45b8099a plans owns this row; it is
/// kept beside the probe until that table exists, so the two are one move.
pub(crate) const SINCE_AS_FILTER: (u32, u32, u32) = (2, 37, 0);

/// Whether `preflight`'s git is at least `min`. Fails closed: a git that was
/// not probed (`Unchecked`), is missing or broken, or whose version line cannot
/// be read is not "new enough", so the caller takes its fallback rather than
/// passing a flag the git may reject.
pub(crate) fn supports(preflight: &GitPreflight, min: (u32, u32, u32)) -> bool {
    preflight.status == GitPreflightStatus::Ok
        && preflight
            .version
            .as_deref()
            .and_then(parse_git_version)
            .is_some_and(|found| found >= min)
}

/// [`supports`] against the git this app drives.
pub(crate) fn git_supports(min: (u32, u32, u32)) -> bool {
    supports(&git_preflight(), min)
}

/// [`assess_for_clone`] against the git this app drives.
pub fn require_git_for_clone(submodules: bool) -> Result<(), String> {
    assess_for_clone(&git_preflight(), submodules)
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

    fn preflight_of(line: &str) -> GitPreflight {
        assess(ToolProbe::Present(line.to_string()))
    }

    /// A feature floor is inclusive, reads Apple's and Windows' version
    /// strings, and fails closed for every probe that did not end in a
    /// readable passing version.
    #[test]
    fn a_feature_floor_is_inclusive_and_fails_closed() {
        let says = |line: &str| supports(&preflight_of(line), SINCE_AS_FILTER);
        assert!(!says("git version 2.36.9"));
        assert!(says("git version 2.37.0"));
        assert!(says("git version 2.39.3 (Apple Git-146)"));
        assert!(says("git version 2.45.1.windows.1"));
        assert!(!says("git version 2.30.2"));

        let not_passing = |status, version: Option<&str>| GitPreflight {
            status,
            version: version.map(str::to_string),
            message: Some("no".into()),
        };
        for status in [
            GitPreflightStatus::Unchecked,
            GitPreflightStatus::Broken,
            GitPreflightStatus::Missing,
            GitPreflightStatus::Outdated,
        ] {
            assert!(
                !supports(
                    &not_passing(status, Some("git version 2.50.0")),
                    SINCE_AS_FILTER
                ),
                "{status:?} must not read as a capable git, whatever version it carries"
            );
        }
        assert!(
            !supports(&not_passing(GitPreflightStatus::Ok, None), SINCE_AS_FILTER),
            "a passing probe with no version line proves nothing"
        );
        assert!(
            !supports(
                &not_passing(GitPreflightStatus::Ok, Some("git version unknown")),
                SINCE_AS_FILTER
            ),
            "an unreadable version is not a capable one"
        );
    }

    #[test]
    fn cloning_is_refused_below_the_patched_git_with_a_message_that_says_what_to_do() {
        for old in [
            "git version 2.39.3 (Apple Git-146)",
            "git version 2.45.0",
            "git version 2.30.2",
        ] {
            let error = assess_for_clone(&preflight_of(old), false).expect_err(old);
            assert!(
                error.contains("2.45.1") && error.contains("Update"),
                "{old}: {error}"
            );
        }
        for ok in [
            "git version 2.45.1",
            "git version 2.50.1",
            "git version 2.54.0 (Apple Git-157)",
        ] {
            assert!(assess_for_clone(&preflight_of(ok), false).is_ok(), "{ok}");
        }
    }

    #[test]
    fn submodule_recursion_needs_the_newer_git() {
        let error = assess_for_clone(&preflight_of("git version 2.49.0"), true).expect_err("old");
        assert!(
            error.contains("2.50.1") && error.contains("CVE-2025-48384"),
            "{error}"
        );
        assert!(assess_for_clone(&preflight_of("git version 2.49.0"), false).is_ok());
        assert!(assess_for_clone(&preflight_of("git version 2.50.1"), true).is_ok());
    }

    /// A probe that could not run is not a pass.
    #[test]
    fn an_unprobed_or_unreadable_git_is_refused_for_cloning() {
        for probe in [
            ToolProbe::NotFound("no git".into()),
            ToolProbe::FoundButFailed("shim".into()),
            ToolProbe::NotRun("busy".into()),
        ] {
            assert!(assess_for_clone(&assess(probe), false).is_err());
        }
        let unreadable = GitPreflight {
            status: GitPreflightStatus::Ok,
            version: Some("git version banana".into()),
            message: None,
        };
        assert!(assess_for_clone(&unreadable, false).is_err());
    }
}
