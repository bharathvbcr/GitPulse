//! Repository-wide content search over `git grep`.
//!
//! Searches the working tree (tracked plus untracked, never ignored files —
//! the same set the file explorer lists) or one revision's tree. Results are
//! bounded three ways, and a bounded answer always says so with the reason:
//! a match limit (the observer stops git once it has seen one record more
//! than the limit), an output byte cap, and the run's deadline. A cancel from
//! the caller stops the child the same way and is reported as `cancelled`,
//! never as "no more matches".

use crate::engine::git_cli::{
    self, validate_repo, BoundedRun, Incomplete, OutputStream, ProcessObserver,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// Default and ceiling for how many matching lines one search returns.
pub const DEFAULT_MAX_MATCHES: usize = 500;
pub const MAX_MATCHES_CEILING: usize = 5_000;
/// Bytes of `git grep` output read before the search is cut.
pub const MAX_SEARCH_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
/// Characters of one matching line carried to the UI (minified files have
/// megabyte lines; the match is still reported, with its text clipped).
pub const MAX_LINE_CHARS: usize = 400;
/// The longest pattern accepted.
pub const MAX_PATTERN_BYTES: usize = 1024;
/// Below the shared cancel registry's 30 s auto-trip, so a slow search ends
/// at git's deadline and is reported as `deadline`, not as `cancelled`.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(25);

/// One matching line.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentMatch {
    pub path: String,
    /// 1-based line number.
    pub line: u64,
    /// 1-based column of the first match on the line.
    pub column: u64,
    pub text: String,
    /// True when `text` was clipped at [`MAX_LINE_CHARS`].
    pub text_clipped: bool,
}

/// A search's answer. `truncated` is true whenever `matches` is not every
/// match that exists, and `truncated_reason` then names why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentSearchReport {
    pub matches: Vec<ContentMatch>,
    /// Distinct files among `matches`.
    pub files: usize,
    pub truncated: bool,
    /// `match_limit`, `output_cap`, `deadline` or `cancelled`; `None` when the
    /// answer is complete.
    pub truncated_reason: Option<String>,
    /// The revision searched, peeled to a commit oid; `None` for the working tree.
    pub revision: Option<String>,
}

/// What to search for.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentSearchOptions {
    /// Treat the pattern as a literal string rather than an extended regex.
    #[serde(default)]
    pub fixed_strings: bool,
    #[serde(default)]
    pub ignore_case: bool,
    /// Search this revision's tree instead of the working tree.
    #[serde(default)]
    pub revision: Option<String>,
    /// At most this many matches; defaults to [`DEFAULT_MAX_MATCHES`] and is
    /// clamped to [`MAX_MATCHES_CEILING`].
    #[serde(default)]
    pub max_matches: Option<usize>,
}

/// Counts complete records as they stream and stops git one past the limit,
/// or as soon as the caller's cancel flag trips.
struct SearchObserver<'a> {
    limit: usize,
    records: usize,
    over_limit: bool,
    user_cancel: &'a dyn Fn() -> bool,
    user_cancelled: bool,
}

impl ProcessObserver for SearchObserver<'_> {
    fn cancelled(&self) -> bool {
        self.over_limit || self.user_cancelled || (self.user_cancel)()
    }

    fn output(&mut self, stream: OutputStream, bytes: &[u8]) {
        if !matches!(stream, OutputStream::Stdout) {
            return;
        }
        self.records += bytes.iter().filter(|&&b| b == b'\n').count();
        if self.records > self.limit {
            self.over_limit = true;
        }
        if (self.user_cancel)() {
            self.user_cancelled = true;
        }
    }
}

fn validate_pattern(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() {
        return Err("Enter something to search for".into());
    }
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(format!(
            "The search pattern exceeds {MAX_PATTERN_BYTES} bytes"
        ));
    }
    if pattern.chars().any(|c| c == '\0' || c == '\n' || c == '\r') {
        return Err("The search pattern must be a single line".into());
    }
    Ok(())
}

/// The `git grep` argv after `git`. `-e` always introduces the pattern, so a
/// pattern beginning with `-` is a pattern, never an option; `--` closes the
/// options before the (absent) pathspec.
fn grep_args<'a>(
    pattern: &'a str,
    options: &ContentSearchOptions,
    revision: Option<&'a str>,
) -> Vec<&'a str> {
    let mut args = vec![
        "-c",
        "core.quotepath=off",
        "grep",
        "-n",
        "--column",
        "-z",
        "-I",
        "--no-color",
        "--full-name",
    ];
    args.push(if options.fixed_strings { "-F" } else { "-E" });
    if options.ignore_case {
        args.push("-i");
    }
    if revision.is_none() {
        args.push("--untracked");
    }
    args.extend(["-e", pattern]);
    if let Some(revision) = revision {
        args.push(revision);
    }
    args.push("--");
    args
}

/// Parses `git grep -n --column -z` output: `path\0line\0column\0text\n` per
/// record, with `<rev>:` prefixed to the path when a revision is searched.
/// The path is NUL-terminated, so a name holding a newline still frames.
/// A trailing record without its newline is a cut prefix and is dropped.
fn parse_grep_z(stdout: &[u8], revision: Option<&str>, limit: usize) -> Vec<ContentMatch> {
    let prefix = revision.map(|rev| format!("{rev}:"));
    let mut matches = Vec::new();
    let mut rest = stdout;
    while matches.len() < limit {
        let Some((path, after_path)) = split_at_byte(rest, 0) else {
            break;
        };
        let Some((line, after_line)) = split_at_byte(after_path, 0) else {
            break;
        };
        let Some((column, after_column)) = split_at_byte(after_line, 0) else {
            break;
        };
        let Some((text, after_text)) = split_at_byte(after_column, b'\n') else {
            break;
        };
        rest = after_text;
        let mut path = String::from_utf8_lossy(path).into_owned();
        if let Some(prefix) = &prefix {
            match path.strip_prefix(prefix.as_str()) {
                Some(stripped) => path = stripped.to_string(),
                None => continue,
            }
        }
        let (Some(line), Some(column)) = (parse_u64(line), parse_u64(column)) else {
            continue;
        };
        let text = String::from_utf8_lossy(text);
        let text_clipped = text.chars().count() > MAX_LINE_CHARS;
        let text = if text_clipped {
            text.chars().take(MAX_LINE_CHARS).collect()
        } else {
            text.into_owned()
        };
        matches.push(ContentMatch {
            path,
            line,
            column,
            text,
            text_clipped,
        });
    }
    matches
}

fn split_at_byte(bytes: &[u8], separator: u8) -> Option<(&[u8], &[u8])> {
    let index = bytes.iter().position(|&b| b == separator)?;
    Some((&bytes[..index], &bytes[index + 1..]))
}

fn parse_u64(bytes: &[u8]) -> Option<u64> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

/// Peels a revision to a commit oid, refusing anything that is not one.
fn peel_commit(repo: &Path, revision: &str) -> Result<String, String> {
    crate::engine::git_writer::validate_oid_or_revision(revision)?;
    let spec = format!("{revision}^{{commit}}");
    let oid = git_cli::git_text(repo, &["rev-parse", "--verify", "--quiet", spec.as_str()])
        .map_err(|_| format!("'{revision}' does not name a commit"))?;
    let oid = oid.trim().to_string();
    if !matches!(oid.len(), 40 | 64) || !oid.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("'{revision}' does not name a commit"));
    }
    Ok(oid)
}

/// Searches the repository's content. `cancelled` is polled while git runs;
/// once it returns true the child is stopped and the partial answer comes
/// back marked `truncated` with reason `cancelled`.
pub fn search(
    repo_path: &str,
    pattern: &str,
    options: &ContentSearchOptions,
    cancelled: &dyn Fn() -> bool,
) -> Result<ContentSearchReport, String> {
    validate_pattern(pattern)?;
    let repo = validate_repo(repo_path)?;
    let limit = options
        .max_matches
        .unwrap_or(DEFAULT_MAX_MATCHES)
        .clamp(1, MAX_MATCHES_CEILING);
    let revision = match options
        .revision
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        Some(revision) => Some(peel_commit(&repo, revision)?),
        None => None,
    };
    let args = grep_args(pattern, options, revision.as_deref());
    let mut observer = SearchObserver {
        limit,
        records: 0,
        over_limit: false,
        user_cancel: cancelled,
        user_cancelled: false,
    };
    let run = match git_cli::git_observed(
        &repo,
        &args,
        SEARCH_TIMEOUT,
        MAX_SEARCH_OUTPUT_BYTES,
        &mut observer,
    ) {
        Ok(run) => run,
        // Cancelled while still queued at the spawn gate: nothing was
        // searched, which is a cancelled answer, not a failed one.
        Err(_) if cancelled() => {
            return Ok(ContentSearchReport {
                matches: Vec::new(),
                files: 0,
                truncated: true,
                truncated_reason: Some("cancelled".into()),
                revision,
            })
        }
        Err(error) => return Err(error),
    };
    let user_cancelled =
        observer.user_cancelled || (run.cancelled && !observer.over_limit && cancelled());
    report_from_run(run, revision, limit, observer.over_limit, user_cancelled)
}

fn report_from_run(
    run: BoundedRun,
    revision: Option<String>,
    limit: usize,
    over_limit: bool,
    user_cancelled: bool,
) -> Result<ContentSearchReport, String> {
    // Exit 1 with nothing on stderr is git grep's "no match". A run we
    // stopped exits by signal, so its status says nothing; anything else
    // non-zero (a bad regex, a missing revision) is git's error.
    if !run.cancelled && !run.success && run.incomplete.is_none() {
        let stderr = String::from_utf8_lossy(&run.stderr).trim().to_string();
        if !(run.status_code == 1 && stderr.is_empty()) {
            return Err(if stderr.is_empty() {
                format!("git grep failed (exit {})", run.status_code)
            } else {
                format!("Search failed: {stderr}")
            });
        }
    }
    let matches = parse_grep_z(&run.stdout, revision.as_deref(), limit);
    let truncated_reason = if user_cancelled {
        Some("cancelled")
    } else if over_limit {
        Some("match_limit")
    } else {
        match &run.incomplete {
            Some(Incomplete::Deadline { .. }) => Some("deadline"),
            Some(Incomplete::OverCap(_)) => Some("output_cap"),
            Some(Incomplete::Unread(_)) => Some("output_cap"),
            None => None,
        }
    };
    let files = matches
        .iter()
        .map(|m| m.path.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
    Ok(ContentSearchReport {
        matches,
        files,
        truncated: truncated_reason.is_some(),
        truncated_reason: truncated_reason.map(String::from),
        revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_grep_z_frames_records_and_strips_the_revision() {
        let out = b"a b.txt\x001\x003\x00let needle = 1;\nweird\nname\x0012\x001\x00needle\n";
        let matches = parse_grep_z(out, None, 10);
        assert_eq!(matches.len(), 2);
        assert_eq!(
            (matches[0].path.as_str(), matches[0].line, matches[0].column),
            ("a b.txt", 1, 3)
        );
        assert_eq!(matches[0].text, "let needle = 1;");
        assert_eq!(
            matches[1].path, "weird\nname",
            "a NUL-framed path may hold a newline"
        );

        let oid = "f".repeat(40);
        let rev = format!("{oid}:src/x.rs\x007\x002\x00 needle\n");
        let at_rev = parse_grep_z(rev.as_bytes(), Some(&oid), 10);
        assert_eq!(at_rev[0].path, "src/x.rs");
    }

    #[test]
    fn parse_grep_z_drops_a_cut_record_and_clips_long_lines() {
        let long = "x".repeat(MAX_LINE_CHARS + 10);
        let out = format!("f\x001\x001\x00{long}\nf\x002\x001\x00half a rec");
        let matches = parse_grep_z(out.as_bytes(), None, 10);
        assert_eq!(
            matches.len(),
            1,
            "the record without its newline is a prefix"
        );
        assert!(matches[0].text_clipped);
        assert_eq!(matches[0].text.chars().count(), MAX_LINE_CHARS);
        assert_eq!(parse_grep_z(out.as_bytes(), None, 0).len(), 0);
    }

    #[test]
    fn grep_args_always_fence_the_pattern() {
        let args = grep_args("-rf /", &ContentSearchOptions::default(), None);
        let at = args.iter().position(|a| *a == "-e").unwrap();
        assert_eq!(args[at + 1], "-rf /");
        assert!(args.contains(&"--untracked") && args.contains(&"-E"));
        assert_eq!(args.last(), Some(&"--"));
        let fixed = ContentSearchOptions {
            fixed_strings: true,
            ignore_case: true,
            ..Default::default()
        };
        let args = grep_args("x", &fixed, Some("abc"));
        assert!(args.contains(&"-F") && args.contains(&"-i") && !args.contains(&"--untracked"));
        assert_eq!(&args[args.len() - 2..], ["abc", "--"]);
    }

    /// The cancel registry this shares trips its flag on a timer; git's own
    /// deadline must come first or a slow search would read as cancelled.
    #[test]
    fn search_deadline_precedes_the_shared_cancel_timer() {
        assert!(SEARCH_TIMEOUT < crate::codeintel::QUERY_CANCEL_DEADLINE);
    }

    #[test]
    fn validate_pattern_refuses_empty_multiline_and_oversized() {
        assert!(validate_pattern("").is_err());
        assert!(validate_pattern("a\nb").is_err());
        assert!(validate_pattern(&"a".repeat(MAX_PATTERN_BYTES + 1)).is_err());
        assert!(validate_pattern("-e").is_ok());
    }
}
