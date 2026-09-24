//! The commit window, and the blob identity that makes a join sound.
//!
//! Two jobs. The first is bounding which commits are even candidates: a
//! regression window is `since..until`, capped so a repository with a hundred
//! thousand commits since the last known-good tag does not become an unbounded
//! walk. The second is establishing a [`BlobIdentity`] — reading the *content*
//! a rev resolves a path to, together with the object id of that content, so
//! the span-to-line join has something it can check rather than assume.
//!
//! Reading the content and the id in **one** resolution is the point. Asking
//! git for the blob id and then separately reading the working tree would pair
//! an id with bytes that are not it, which is the whole failure mode the
//! identity exists to catch, reintroduced one layer down.

use std::path::Path;
use std::time::Duration;

use dc_proc::{git_with_program, run_bounded, Bounds, Failure};
use serde::{Deserialize, Serialize};

use crate::BlobIdentity;

/// Wall clock for one history read.
pub const HISTORY_DEADLINE: Duration = Duration::from_secs(15);

/// Most commits a window will consider.
///
/// Past it the window is a **suffix** of what was asked for and the report
/// says so, rather than silently analysing part of a range and presenting the
/// result as the whole.
pub const COMMIT_CAP: usize = 2_000;

/// Bytes of `git log` output kept.
pub const LOG_OUTPUT_CAP: usize = 4 * 1024 * 1024;

/// Bytes of a single file's content read for a line index.
///
/// A file past this is not analysed. Blame on it would be refused by its own
/// cap anyway, and reading it would be the larger cost of the two.
pub const CONTENT_CAP: usize = 8 * 1024 * 1024;

/// Why a history read produced nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryRefusal {
    GitUnavailable {
        reason: String,
    },
    Deadline {
        what: String,
    },
    OutputCapped {
        what: String,
        cap: usize,
    },
    GitRefused {
        what: String,
        stderr: String,
    },
    /// The content is not UTF-8, so it has no lines to index.
    NotText {
        path: String,
    },
}

impl HistoryRefusal {
    pub fn describe(&self) -> String {
        match self {
            HistoryRefusal::GitUnavailable { reason } => format!("git could not be run: {reason}"),
            HistoryRefusal::Deadline { what } => {
                format!("{what} exceeded {HISTORY_DEADLINE:?} and was killed")
            }
            HistoryRefusal::OutputCapped { what, cap } => {
                format!(
                    "{what} produced more than {cap} bytes and was refused rather than truncated"
                )
            }
            HistoryRefusal::GitRefused { what, stderr } => {
                if stderr.is_empty() {
                    format!("{what} was refused by git")
                } else {
                    format!("{what} was refused by git: {stderr}")
                }
            }
            HistoryRefusal::NotText { path } => {
                format!("{path} is not UTF-8 text, so it has no lines to attribute")
            }
        }
    }
}

/// The commits in a window, newest first, and whether the cap was hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitWindow {
    pub commits: Vec<String>,
    /// True when [`COMMIT_CAP`] was reached, so `commits` is a suffix of the
    /// range rather than all of it.
    pub capped: bool,
}

/// One file's content at a rev, with the object id of exactly those bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBlob {
    pub content: String,
    pub identity: BlobIdentity,
}

fn bounds_with_deadline(stdout_cap: usize, deadline: Duration) -> Bounds {
    Bounds {
        deadline,
        stdout_cap,
        stderr_cap: 8 * 1024,
    }
}

/// Run one bounded git command, refusing a capped read rather than returning
/// its prefix.
///
/// `pub(crate)` rather than private because [`crate::change`] runs `git diff`
/// under exactly these bounds and exactly this refusal order. A second copy
/// there would be a second place for the truncation-before-status rule to be
/// got wrong, and that rule is the one that decides whether a partial read is
/// presented as a whole answer.
pub(crate) fn run_git(
    program: &std::ffi::OsStr,
    repo: &Path,
    args: &[&str],
    what: &str,
    stdout_cap: usize,
) -> Result<dc_proc::Captured, HistoryRefusal> {
    run_git_with_deadline(program, repo, args, what, stdout_cap, HISTORY_DEADLINE)
}

/// [`run_git`] with an explicit wall-clock budget, so a caller walking several
/// paths can spend the remainder of a shared deadline on each next path rather
/// than resetting the clock.
pub(crate) fn run_git_with_deadline(
    program: &std::ffi::OsStr,
    repo: &Path,
    args: &[&str],
    what: &str,
    stdout_cap: usize,
    deadline: Duration,
) -> Result<dc_proc::Captured, HistoryRefusal> {
    let mut command = git_with_program(program, repo);
    command.args(args);
    let captured = match run_bounded(&mut command, bounds_with_deadline(stdout_cap, deadline)) {
        Ok(captured) => captured,
        Err(Failure::Deadline { .. }) => {
            return Err(HistoryRefusal::Deadline {
                what: what.to_string(),
            })
        }
        Err(failure) => {
            return Err(HistoryRefusal::GitUnavailable {
                reason: failure.to_string(),
            })
        }
    };
    // Truncation before status, for the same reason as in `blame`: a capped
    // read can carry a success status, and accepting it would present a prefix
    // as the whole answer.
    if captured.stdout_truncated {
        return Err(HistoryRefusal::OutputCapped {
            what: what.to_string(),
            cap: stdout_cap,
        });
    }
    if !captured.status.success() {
        return Err(HistoryRefusal::GitRefused {
            what: what.to_string(),
            stderr: captured.stderr_trimmed(),
        });
    }
    Ok(captured)
}

/// The commits reachable from `until` but not from `since`, newest first.
pub fn commit_window(
    repo: &Path,
    since: &str,
    until: &str,
) -> Result<CommitWindow, HistoryRefusal> {
    commit_window_with_program(std::ffi::OsStr::new("git"), repo, since, until)
}

#[doc(hidden)]
pub fn commit_window_with_program(
    program: &std::ffi::OsStr,
    repo: &Path,
    since: &str,
    until: &str,
) -> Result<CommitWindow, HistoryRefusal> {
    // One past the cap, so hitting it is distinguishable from exactly filling
    // it. Asking for exactly `COMMIT_CAP` and getting `COMMIT_CAP` back cannot
    // tell "the range is this long" from "the range is longer".
    let max_count = format!("--max-count={}", COMMIT_CAP + 1);
    let range = format!("{since}..{until}");
    let captured = run_git(
        program,
        repo,
        &["log", &max_count, "--format=%H", "--no-merges", &range],
        "git log",
        LOG_OUTPUT_CAP,
    )?;
    let mut commits: Vec<String> = captured
        .stdout_lossy()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    let capped = commits.len() > COMMIT_CAP;
    commits.truncate(COMMIT_CAP);
    Ok(CommitWindow { commits, capped })
}

/// Read `path` at `rev`: its bytes and the object id of those bytes.
///
/// `git rev-parse <rev>:<path>` and `git show <rev>:<path>` name the same
/// object, so the id returned is the id *of the content returned* — which is
/// what lets [`crate::join::attribute`] trust the pairing.
pub fn resolve_blob(repo: &Path, rev: &str, path: &str) -> Result<ResolvedBlob, HistoryRefusal> {
    resolve_blob_with_program(std::ffi::OsStr::new("git"), repo, rev, path)
}

#[doc(hidden)]
pub fn resolve_blob_with_program(
    program: &std::ffi::OsStr,
    repo: &Path,
    rev: &str,
    path: &str,
) -> Result<ResolvedBlob, HistoryRefusal> {
    let spec = format!("{rev}:{path}");
    let id = run_git(
        program,
        repo,
        &["rev-parse", &spec],
        "git rev-parse",
        4 * 1024,
    )?;
    let oid = id.stdout_lossy().trim().to_string();
    let shown = run_git(program, repo, &["show", &spec], "git show", CONTENT_CAP)?;
    let content = match String::from_utf8(shown.stdout.clone()) {
        Ok(content) => content,
        Err(_) => {
            return Err(HistoryRefusal::NotText {
                path: path.to_string(),
            })
        }
    };
    Ok(ResolvedBlob {
        content,
        identity: BlobIdentity::Blob(oid),
    })
}

/// The files a commit changed, as repository-relative paths.
pub fn files_changed(repo: &Path, commit: &str) -> Result<Vec<String>, HistoryRefusal> {
    files_changed_with_program(std::ffi::OsStr::new("git"), repo, commit)
}

#[doc(hidden)]
pub fn files_changed_with_program(
    program: &std::ffi::OsStr,
    repo: &Path,
    commit: &str,
) -> Result<Vec<String>, HistoryRefusal> {
    let captured = run_git(
        program,
        repo,
        &[
            "show",
            "--name-only",
            "--format=",
            "--no-renames",
            // NUL-separated and unquoted, so a non-ASCII path arrives as its
            // own bytes rather than as a C-escaped rendering that would never
            // match a graph's `file_path`.
            "-z",
            commit,
        ],
        "git show --name-only",
        LOG_OUTPUT_CAP,
    )?;
    Ok(captured
        .stdout_lossy()
        .split('\0')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_describes_which_command_it_was() {
        let refusal = HistoryRefusal::Deadline {
            what: "git log".into(),
        };
        assert!(refusal.describe().contains("git log"));
    }

    #[test]
    fn a_capped_read_is_named_as_capped_not_as_a_short_answer() {
        let refusal = HistoryRefusal::OutputCapped {
            what: "git show".into(),
            cap: 10,
        };
        let described = refusal.describe();
        assert!(described.contains("refused rather than truncated"));
    }

    #[test]
    fn a_non_text_file_says_it_has_no_lines() {
        let refusal = HistoryRefusal::NotText {
            path: "logo.png".into(),
        };
        assert!(refusal.describe().contains("no lines"));
    }
}
