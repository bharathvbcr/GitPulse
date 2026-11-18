//! Repository-wide content search, preferring `dcgrep` when installed for
//! the working tree and falling back to `git grep`.
//!
//! Searches the working tree (tracked plus untracked, never ignored files —
//! the same set the file explorer lists) or one revision's tree. Results are
//! bounded three ways, and a bounded answer always says so with the reason:
//! a match limit (the observer stops the child once it has seen more than
//! the limit), an output byte cap, and the run's deadline. A cancel from
//! the caller stops the child the same way and is reported as `cancelled`,
//! never as "no more matches".

use crate::engine::git_cli::{self, validate_repo};
use crate::engine::process::{BoundedRun, Incomplete, OutputStream, ProcessObserver};
use crate::ipc_error::CommandError;
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
/// at the child's deadline and is reported as `deadline`, not as `cancelled`.
pub(crate) const SEARCH_TIMEOUT: Duration = Duration::from_secs(25);

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

    fn cancellation(&self) -> Option<CommandError> {
        if self.user_cancelled || (self.user_cancel)() {
            Some(crate::engine::process::read_cancel::cancelled())
        } else {
            None
        }
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

fn validate_pattern(pattern: &str) -> Result<(), CommandError> {
    if pattern.is_empty() {
        return Err("Enter something to search for".into());
    }
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(format!("The search pattern exceeds {MAX_PATTERN_BYTES} bytes").into());
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
        let text = text.strip_suffix(b"\r").unwrap_or(text);
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
        let (text, text_clipped) = clip_match_text(&text);
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

pub(crate) fn clip_match_text(text: &str) -> (String, bool) {
    if text.chars().count() > MAX_LINE_CHARS {
        (text.chars().take(MAX_LINE_CHARS).collect(), true)
    } else {
        (text.to_string(), false)
    }
}

fn split_at_byte(bytes: &[u8], separator: u8) -> Option<(&[u8], &[u8])> {
    let index = bytes.iter().position(|&b| b == separator)?;
    Some((&bytes[..index], &bytes[index + 1..]))
}

fn parse_u64(bytes: &[u8]) -> Option<u64> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

/// Peels a revision to a commit oid, refusing anything that is not one.
fn peel_commit(repo: &Path, revision: &str) -> Result<String, CommandError> {
    crate::engine::git_writer::validate_oid_or_revision(revision)?;
    let spec = format!("{revision}^{{commit}}");
    let oid = git_cli::git_text(repo, &["rev-parse", "--verify", "--quiet", spec.as_str()])
        .map_err(|_| format!("'{revision}' does not name a commit"))?;
    let oid = oid.trim().to_string();
    if !matches!(oid.len(), 40 | 64) || !oid.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("'{revision}' does not name a commit").into());
    }
    Ok(oid)
}

fn cancelled_report(revision: Option<String>) -> ContentSearchReport {
    ContentSearchReport {
        matches: Vec::new(),
        files: 0,
        truncated: true,
        truncated_reason: Some("cancelled".into()),
        revision,
    }
}

fn grep_tracked_args<'a>(pattern: &'a str, options: &ContentSearchOptions) -> Vec<&'a str> {
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
    args.extend(["-e", pattern, "--"]);
    args
}

fn run_git_grep(
    repo: &Path,
    args: &[&str],
    limit: usize,
    revision: Option<String>,
    cancelled: &dyn Fn() -> bool,
) -> Result<ContentSearchReport, CommandError> {
    let mut observer = SearchObserver {
        limit,
        records: 0,
        over_limit: false,
        user_cancel: cancelled,
        user_cancelled: false,
    };
    let run = match git_cli::git_observed(
        repo,
        args,
        SEARCH_TIMEOUT,
        MAX_SEARCH_OUTPUT_BYTES,
        &mut observer,
    ) {
        Ok(run) => run,
        // Cancelled while still queued at the spawn gate: nothing was
        // searched, which is a cancelled answer, not a failed one.
        Err(_) if cancelled() => return Ok(cancelled_report(revision)),
        Err(error) => return Err(error),
    };
    let user_cancelled =
        observer.user_cancelled || (run.cancelled && !observer.over_limit && cancelled());
    report_from_run(run, revision, limit, observer.over_limit, user_cancelled)
}

fn has_devcouncil_dcgrep_segment(path: &str) -> bool {
    let mut prev: Option<&str> = None;
    for part in path.split(['/', '\\']) {
        if let Some(prev_part) = prev {
            if prev_part.eq_ignore_ascii_case(".devcouncil") && part.eq_ignore_ascii_case("dcgrep")
            {
                return true;
            }
        }
        prev = Some(part);
    }
    false
}

fn list_tracked_ignored_paths(
    repo: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<Result<Vec<String>, ContentSearchReport>, CommandError> {
    let mut observer = CancelOnlyObserver {
        user_cancel: cancelled,
        user_cancelled: false,
    };
    let run = match git_cli::git_observed(
        repo,
        &[
            "-c",
            "core.quotepath=off",
            "ls-files",
            "-c",
            "-i",
            "--exclude-standard",
            "-z",
        ],
        SEARCH_TIMEOUT,
        MAX_SEARCH_OUTPUT_BYTES,
        &mut observer,
    ) {
        Ok(run) => run,
        Err(_) if cancelled() => return Ok(Err(cancelled_report(None))),
        Err(error) => return Err(error),
    };
    if run.cancelled || observer.user_cancelled || cancelled() {
        return Ok(Err(cancelled_report(None)));
    }
    if let Some(incomplete) = run.incomplete {
        let reason = match incomplete {
            Incomplete::Deadline { .. } => "deadline",
            Incomplete::OverCap(_) | Incomplete::Unread(_) => "output_cap",
        };
        return Ok(Err(ContentSearchReport {
            matches: Vec::new(),
            files: 0,
            truncated: true,
            truncated_reason: Some(reason.into()),
            revision: None,
        }));
    }
    if !run.success {
        let stderr = String::from_utf8_lossy(&run.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("git ls-files failed (exit {})", run.status_code).into()
        } else {
            format!("Search failed: {stderr}").into()
        });
    }
    let mut paths = Vec::new();
    for raw in run.stdout.split(|&b| b == 0) {
        if raw.is_empty() {
            continue;
        }
        let Ok(path) = std::str::from_utf8(raw) else {
            continue;
        };
        if git_cli::sandbox_join(repo, path).is_err() {
            continue;
        }
        if has_devcouncil_dcgrep_segment(path) {
            continue;
        }
        paths.push(path.to_string());
    }
    paths.sort();
    paths.dedup();
    Ok(Ok(paths))
}

struct CancelOnlyObserver<'a> {
    user_cancel: &'a dyn Fn() -> bool,
    user_cancelled: bool,
}

impl ProcessObserver for CancelOnlyObserver<'_> {
    fn cancelled(&self) -> bool {
        self.user_cancelled || (self.user_cancel)()
    }

    fn cancellation(&self) -> Option<CommandError> {
        if self.user_cancelled || (self.user_cancel)() {
            Some(crate::engine::process::read_cancel::cancelled())
        } else {
            None
        }
    }

    fn output(&mut self, _stream: OutputStream, _bytes: &[u8]) {
        if (self.user_cancel)() {
            self.user_cancelled = true;
        }
    }
}

/// Searches the repository's content. `cancelled` is polled while git runs;
/// once it returns true the child is stopped and the partial answer comes
/// back marked `truncated` with reason `cancelled`.
pub fn search(
    repo_path: &str,
    pattern: &str,
    options: &ContentSearchOptions,
    cancelled: &dyn Fn() -> bool,
) -> Result<ContentSearchReport, CommandError> {
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
    if revision.is_none() {
        if let Some(report) =
            crate::engine::dcgrep::search_working_tree(&repo, pattern, options, limit, cancelled)?
        {
            return Ok(report);
        }
    }
    const EXCLUDED_DCGREP_PATHSPECS: &[&str] = &[
        ":(exclude,icase).devcouncil/dcgrep",
        ":(exclude,glob,icase)**/.devcouncil/dcgrep",
        ":(exclude,glob,icase)**/.devcouncil/dcgrep/**",
    ];
    const MAX_TRACKED_SPECS_PER_BATCH: usize = 200;
    const MAX_TRACKED_SPEC_BYTES_PER_BATCH: usize = 32 * 1024;
    const MAX_TRACKED_SPEC_BATCHES: usize = 8;

    let mut args = grep_args(pattern, options, revision.as_deref());
    if revision.is_none() {
        args.extend_from_slice(EXCLUDED_DCGREP_PATHSPECS);
    }
    let mut report = run_git_grep(&repo, &args, limit, revision.clone(), cancelled)?;
    if revision.is_some()
        || cancelled()
        || report.truncated_reason.as_deref() == Some("cancelled")
        || (report.truncated && report.truncated_reason.as_deref() != Some("match_limit"))
    {
        return Ok(report);
    }

    // `git grep --untracked` skips files tracked in `.git/index` whose names
    // match `.gitignore` (e.g. `AGENTS.md`, `CLAUDE.md`). Query tracked
    // ignored paths via `git ls-files -c -i --exclude-standard -z` and search
    // them through the tracked index view so File Explorer and Content Search
    // cover the exact same working-tree files.
    let tracked_ignored = match list_tracked_ignored_paths(&repo, cancelled)? {
        Ok(paths) => paths,
        Err(early_report) => {
            if early_report.truncated_reason.as_deref() == Some("cancelled") || cancelled() {
                return Ok(cancelled_report(None));
            }
            report.truncated = true;
            if matches!(
                early_report.truncated_reason.as_deref(),
                Some("deadline" | "output_cap")
            ) || report.truncated_reason.is_none()
            {
                report.truncated_reason = early_report.truncated_reason;
            }
            return Ok(report);
        }
    };
    if tracked_ignored.is_empty() {
        return Ok(report);
    }

    let mut extra_matches = Vec::new();
    let mut extra_truncated_reason: Option<String> = None;
    if let Some(batches) = chunk_tracked_pathspecs(
        &tracked_ignored,
        MAX_TRACKED_SPECS_PER_BATCH,
        MAX_TRACKED_SPEC_BYTES_PER_BATCH,
        MAX_TRACKED_SPEC_BATCHES,
    ) {
        for batch in batches {
            if cancelled() {
                return Ok(cancelled_report(None));
            }
            let mut tracked_args = grep_tracked_args(pattern, options);
            tracked_args.extend(batch.iter().map(String::as_str));
            let batch_report = run_git_grep(&repo, &tracked_args, limit, None, cancelled)?;
            if batch_report.truncated_reason.as_deref() == Some("cancelled") || cancelled() {
                return Ok(cancelled_report(None));
            }
            extra_matches.extend(batch_report.matches);
            if batch_report.truncated {
                extra_truncated_reason = batch_report.truncated_reason;
                break;
            }
        }
    } else {
        let mut tracked_args = grep_tracked_args(pattern, options);
        tracked_args.extend_from_slice(EXCLUDED_DCGREP_PATHSPECS);
        let extra = run_git_grep(&repo, &tracked_args, limit, None, cancelled)?;
        if extra.truncated_reason.as_deref() == Some("cancelled") || cancelled() {
            return Ok(cancelled_report(None));
        }
        extra_matches = extra.matches;
        if extra.truncated {
            extra_truncated_reason = extra.truncated_reason;
        }
    }

    merge_extra_into_report(&mut report, extra_matches, extra_truncated_reason, limit);
    Ok(report)
}

fn chunk_tracked_pathspecs(
    paths: &[String],
    max_specs: usize,
    max_bytes: usize,
    max_batches: usize,
) -> Option<Vec<Vec<String>>> {
    let mut batches: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_bytes = 0usize;
    for path in paths {
        let spec = format!(":(literal){path}");
        let spec_len = spec.len();
        if !current.is_empty()
            && (current.len() >= max_specs || current_bytes.saturating_add(spec_len) > max_bytes)
        {
            batches.push(current);
            if batches.len() >= max_batches {
                return None;
            }
            current = Vec::new();
            current_bytes = 0;
        }
        current_bytes = current_bytes.saturating_add(spec_len);
        current.push(spec);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    if batches.len() > max_batches {
        None
    } else {
        Some(batches)
    }
}

fn merge_extra_into_report(
    report: &mut ContentSearchReport,
    extra_matches: Vec<ContentMatch>,
    extra_truncated_reason: Option<String>,
    limit: usize,
) {
    if !extra_matches.is_empty() {
        let mut seen: std::collections::HashSet<(String, u64, u64)> = report
            .matches
            .iter()
            .map(|m| (m.path.clone(), m.line, m.column))
            .collect();
        let mut combined = std::mem::take(&mut report.matches);
        for m in extra_matches {
            if has_devcouncil_dcgrep_segment(&m.path) {
                continue;
            }
            if seen.insert((m.path.clone(), m.line, m.column)) {
                combined.push(m);
            }
        }
        combined.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then_with(|| a.line.cmp(&b.line))
                .then_with(|| a.column.cmp(&b.column))
        });
        if combined.len() > limit {
            combined.truncate(limit);
            report.truncated = true;
            if report.truncated_reason.is_none() {
                report.truncated_reason = Some("match_limit".into());
            }
        }
        report.files = combined
            .iter()
            .map(|m| m.path.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len();
        report.matches = combined;
    }
    if matches!(
        extra_truncated_reason.as_deref(),
        Some("deadline" | "output_cap")
    ) {
        report.truncated = true;
        report.truncated_reason = extra_truncated_reason;
    } else if !report.truncated {
        if let Some(reason) = extra_truncated_reason {
            report.truncated = true;
            report.truncated_reason = Some(reason);
        }
    }
}

fn report_from_run(
    run: BoundedRun,
    revision: Option<String>,
    limit: usize,
    over_limit: bool,
    user_cancelled: bool,
) -> Result<ContentSearchReport, CommandError> {
    // Exit 1 with nothing on stderr is git grep's "no match". A run we
    // stopped exits by signal, so its status says nothing; anything else
    // non-zero (a bad regex, a missing revision) is git's error.
    if !run.cancelled && !run.success && run.incomplete.is_none() {
        let stderr = String::from_utf8_lossy(&run.stderr).trim().to_string();
        if !(run.status_code == 1 && stderr.is_empty()) {
            return Err(if stderr.is_empty() {
                format!("git grep failed (exit {})", run.status_code).into()
            } else {
                format!("Search failed: {stderr}").into()
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

    #[test]
    fn has_devcouncil_dcgrep_segment_catches_nested_and_mixed_case_only_for_dcgrep() {
        assert!(has_devcouncil_dcgrep_segment(".devcouncil/dcgrep/idx.bin"));
        assert!(has_devcouncil_dcgrep_segment("sub/.DevCouncil/DcGrep/blob"));
        assert!(has_devcouncil_dcgrep_segment("a\\b\\.devcouncil\\dcgrep"));
        assert!(!has_devcouncil_dcgrep_segment(".devcouncil/config.yaml"));
        assert!(!has_devcouncil_dcgrep_segment(
            "sub/.devcouncil/agents/review.md"
        ));
        assert!(!has_devcouncil_dcgrep_segment("src/dcgrep.rs"));
    }

    #[test]
    fn chunk_tracked_pathspecs_batches_within_limits_and_falls_back_past_cap() {
        let small: Vec<String> = (0..405).map(|i| format!("ignored/f_{i}.txt")).collect();
        let batches = chunk_tracked_pathspecs(&small, 200, 32 * 1024, 8).expect("3 batches fit");
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 200);
        assert_eq!(batches[1].len(), 200);
        assert_eq!(batches[2].len(), 5);
        assert_eq!(batches[0][0], ":(literal)ignored/f_0.txt");

        let huge: Vec<String> = (0..1700).map(|i| format!("ignored/f_{i}.txt")).collect();
        assert!(chunk_tracked_pathspecs(&huge, 200, 32 * 1024, 8).is_none());

        let long_paths: Vec<String> = (0..10)
            .map(|i| format!("{}/{i}.txt", "a".repeat(20)))
            .collect();
        assert!(chunk_tracked_pathspecs(&long_paths, 200, 32, 2).is_none());
    }

    #[test]
    fn merge_extra_into_report_preserves_existing_matches_filters_dcgrep_and_propagates_truncation()
    {
        let mut report = ContentSearchReport {
            matches: vec![ContentMatch {
                path: "b.txt".into(),
                line: 1,
                column: 1,
                text: "match b".into(),
                text_clipped: false,
            }],
            files: 1,
            truncated: true,
            truncated_reason: Some("match_limit".into()),
            revision: None,
        };
        let extra_matches = vec![
            ContentMatch {
                path: "a.txt".into(),
                line: 2,
                column: 1,
                text: "match a".into(),
                text_clipped: false,
            },
            ContentMatch {
                path: "sub/.devcouncil/dcgrep/idx".into(),
                line: 1,
                column: 1,
                text: "match dcgrep".into(),
                text_clipped: false,
            },
        ];
        merge_extra_into_report(&mut report, extra_matches, Some("output_cap".into()), 10);
        let paths: Vec<&str> = report.matches.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "b.txt"]);
        assert!(report.truncated);
        assert_eq!(report.truncated_reason.as_deref(), Some("output_cap"));
    }

    #[test]
    fn search_with_over_200_tracked_ignored_files_finds_all_via_git_grep_fallback() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let cmd = crate::engine::process::build_capture_command(
                "git",
                args,
                Some(root),
                &[],
                None,
                None,
            );
            let run = crate::engine::process::run_bounded_capped(
                cmd,
                "git test helper",
                std::time::Duration::from_secs(10),
                None,
                1024 * 1024,
            )
            .expect("run git");
            assert!(
                run.success,
                "git {:?} failed: stdout={} stderr={}",
                args,
                String::from_utf8_lossy(&run.stdout),
                String::from_utf8_lossy(&run.stderr)
            );
            if args.first() == Some(&"init") {
                crate::test_support::trust_repo(root);
            }
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@e.com"]);
        git(&["config", "user.name", "T"]);
        std::fs::write(root.join(".gitignore"), "ign/\n").unwrap();
        std::fs::create_dir_all(root.join("ign")).unwrap();
        for i in 0..215 {
            std::fs::write(root.join(format!("ign/f_{i:03}.txt")), b"common_needle\n").unwrap();
        }
        std::fs::create_dir_all(root.join("sub/.devcouncil/dcgrep")).unwrap();
        std::fs::write(
            root.join("sub/.devcouncil/dcgrep/idx.txt"),
            b"common_needle\n",
        )
        .unwrap();
        std::fs::write(
            root.join("sub/.devcouncil/config.yaml"),
            b"key: common_needle\n",
        )
        .unwrap();
        git(&["add", ".gitignore", "sub/.devcouncil/config.yaml"]);
        git(&["add", "-f", "ign", "sub/.devcouncil/dcgrep/idx.txt"]);
        git(&["commit", "-q", "-m", "init"]);

        let tracked = list_tracked_ignored_paths(root, &|| false)
            .unwrap()
            .expect("tracked paths ok");
        assert_eq!(tracked.len(), 215);
        assert!(!tracked.iter().any(|p| p.contains("dcgrep")));

        // Run git-grep batch merge directly (testing >200 pathspec chunking regardless of dcgrep).
        let opts = ContentSearchOptions::default();
        let mut args = grep_args("common_needle", &opts, None);
        args.push(":(exclude,icase).devcouncil/dcgrep");
        args.push(":(exclude,glob,icase)**/.devcouncil/dcgrep");
        args.push(":(exclude,glob,icase)**/.devcouncil/dcgrep/**");
        let mut report = run_git_grep(root, &args, 500, None, &|| false).unwrap();
        let batches = chunk_tracked_pathspecs(&tracked, 200, 32 * 1024, 8).unwrap();
        assert_eq!(batches.len(), 2);
        let mut extra = Vec::new();
        for batch in batches {
            let mut t_args = grep_tracked_args("common_needle", &opts);
            t_args.extend(batch.iter().map(String::as_str));
            extra.extend(
                run_git_grep(root, &t_args, 500, None, &|| false)
                    .unwrap()
                    .matches,
            );
        }
        merge_extra_into_report(&mut report, extra, None, 500);
        assert_eq!(report.matches.len(), 216);
        assert!(report
            .matches
            .iter()
            .any(|m| m.path == "sub/.devcouncil/config.yaml"));
        assert!(!report.matches.iter().any(|m| m.path.contains("dcgrep")));
    }
}
