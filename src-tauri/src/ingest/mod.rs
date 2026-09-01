//! Catching up on what happened while GitPulse was not watching.
//!
//! Correctness never depends on residency. Two sources are replayed on repo
//! open, and both are things the system observed rather than things an agent
//! reported about itself:
//!
//! * **Agent transcripts** — `~/.claude/projects/**/*.jsonl`, parsed by
//!   [`transcript`]. Measured over the real corpus, 97.3% of mutating tool
//!   calls in repositories that still exist attribute to a repository path.
//! * **The reflog** — git's own record of every ref movement, which is
//!   authoritative for commits and survives GitPulse being uninstalled.
//!
//! # Idempotence
//!
//! Both replays run on every open, so both must be safe to run twice. Each
//! source is watermarked by what is already in the ledger: transcripts by the
//! newest `session.*` timestamp for the repo, the reflog by the newest
//! `reflog.*` object already recorded. Re-running adds nothing.

pub mod transcript;

use crate::ledger::{self, ActorKind, Draft, Outcome};

/// What one catch-up pass found.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CatchUp {
    /// Ledger events written by this pass.
    pub recorded: i64,
    /// Transcript files read.
    pub transcripts: i64,
    /// Transcript lines this build could not read.
    ///
    /// Reported rather than swallowed: a parser that silently skipped
    /// unrecognised records would make a partial history look complete, and
    /// the transcript format is known to move — 17 distinct schema versions
    /// appear across the real corpus.
    pub skipped_lines: i64,
    /// Reflog entries replayed.
    pub reflog_entries: i64,
    /// Empty when the pass completed; otherwise what stopped it.
    pub error: String,
}

/// Where Claude Code keeps its transcripts.
fn transcript_root() -> Option<std::path::PathBuf> {
    std::env::var_os("GITPULSE_TRANSCRIPT_ROOT")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs_home().map(|h| h.join(".claude").join("projects")))
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

/// The newest timestamp already recorded for a source family in this repo.
///
/// The watermark is what makes a replay idempotent. It is read from the ledger
/// rather than stored beside it, so it cannot drift from what was actually
/// written.
fn watermark(repo_path: &str, prefix: &str) -> String {
    let mut cursor = 0i64;
    let mut newest = String::new();
    while let Ok(page) = ledger::tail(repo_path, cursor, 1000) {
        if page.is_empty() {
            break;
        }
        for event in &page {
            if event.action.starts_with(prefix) && event.ts_utc > newest {
                newest = event.ts_utc.clone();
            }
        }
        cursor = page[page.len() - 1].id;
        if page.len() < 1000 {
            break;
        }
    }
    newest
}

/// Replays agent transcripts for `repo_path`.
///
/// Only calls newer than the watermark are recorded, so opening a repository
/// twice does not double its history.
pub fn ingest_transcripts(repo_path: &str) -> CatchUp {
    let mut out = CatchUp::default();
    let Some(root) = transcript_root() else {
        out.error = "no home directory, so transcripts cannot be located".into();
        return out;
    };
    if !root.is_dir() {
        // No transcripts is the ordinary case for a machine that has never run
        // an agent. Not an error.
        return out;
    }

    let since = watermark(repo_path, "session.");
    let since_ms = iso_to_millis(&since);
    let mut files = Vec::new();
    collect_jsonl(&root, &mut files, 0);
    for path in files {
        // A transcript not modified since the watermark cannot hold an event
        // newer than it, so reading it can only reproduce work already done.
        //
        // This is soundness, not a heuristic: the watermark is the newest
        // session timestamp already in the ledger, and a file's events cannot
        // postdate its last write. Without it, catch-up re-read the whole
        // corpus on every repo open — 886 files and 54 seconds on the machine
        // this was measured on, for 0 new rows.
        if since_ms > 0 && !modified_since(&path, since_ms) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            // A transcript being written right now can fail a read; the next
            // open picks it up. Counted so the gap is visible.
            out.skipped_lines += 1;
            continue;
        };
        out.transcripts += 1;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let calls = transcript::parse_line(line);
            if calls.is_empty() {
                // Distinguish "not an assistant record" from "could not read".
                // Only the latter is a gap worth reporting.
                if line.contains("\"type\":\"assistant\"")
                    && serde_json::from_str::<serde_json::Value>(line).is_err()
                {
                    out.skipped_lines += 1;
                }
                continue;
            }
            for call in calls {
                if !transcript::belongs_to(&call, repo_path) {
                    continue;
                }
                if !since.is_empty() && call.ts_utc <= since {
                    continue;
                }
                let detail = serde_json::json!({
                    "source": "transcript",
                    "tool": call.tool,
                    "transcript_version": call.version,
                    "git_branch": call.git_branch,
                })
                .to_string();
                if ledger::record(Draft {
                    repo_path: repo_path.to_string(),
                    action: call.action().to_string(),
                    object: Some(call.object()),
                    session_id: Some(call.session_id.clone()),
                    // Derived from observation, not self-reported: this row
                    // exists because a transcript recorded the call, not
                    // because an agent announced it.
                    actor_kind: Some(ActorKind::Agent),
                    actor_id: Some("claude-code".into()),
                    outcome: Some(Outcome::Ok),
                    // No verdict: GitPulse's gate never saw this action. That
                    // is emphatically not the same as an action that passed,
                    // and the absence is what says so.
                    verdict_json: None,
                    detail_json: Some(detail),
                    ..Default::default()
                })
                .is_some()
                {
                    out.recorded += 1;
                }
            }
        }
    }
    out
}

/// Epoch milliseconds for an ISO-8601 timestamp, or 0 when it cannot be read.
///
/// Deliberately tolerant: a timestamp this cannot parse yields 0, which
/// disables the skip and makes the pass read everything. Failing *open* here is
/// the safe direction — the cost is time, and the alternative is silently
/// skipping files that should have been read.
fn iso_to_millis(iso: &str) -> u64 {
    if iso.len() < 20 || !iso.ends_with('Z') {
        return 0;
    }
    let num = |a: usize, b: usize| iso[a..b].parse::<i64>().ok();
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(sec)) = (
        num(0, 4),
        num(5, 7),
        num(8, 10),
        num(11, 13),
        num(14, 16),
        num(17, 19),
    ) else {
        return 0;
    };
    let ms = iso
        .get(20..23)
        .and_then(|m| m.parse::<i64>().ok())
        .unwrap_or(0);
    // Days from the civil date, the inverse of the ledger's own conversion.
    let (y, mo) = if mo <= 2 { (y - 1, mo + 12) } else { (y, mo) };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (mo - 3) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let total = days * 86_400_000 + h * 3_600_000 + mi * 60_000 + sec * 1000 + ms;
    if total < 0 {
        0
    } else {
        total as u64
    }
}

/// Whether `path` was written at or after `since_ms`.
///
/// An unreadable mtime returns true, so the file is read. The skip is an
/// optimisation and must never be the reason something goes unattributed.
fn modified_since(path: &std::path::Path, since_ms: u64) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return true;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH) else {
        return true;
    };
    // A second of slack: filesystem timestamps and the transcript's own clock
    // are not the same clock, and losing an event to rounding is worse than
    // reading one file twice.
    since_epoch.as_millis() as u64 + 1000 >= since_ms
}

/// Walks `dir` for `.jsonl` files, bounded in depth.
///
/// The bound is not decoration: the transcript root is user-controlled, and a
/// symlink loop under it would otherwise hang repo open forever.
fn collect_jsonl(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>, depth: usize) {
    const MAX_DEPTH: usize = 4;
    const MAX_FILES: usize = 5000;
    if depth > MAX_DEPTH || out.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, out, depth + 1);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
            if out.len() >= MAX_FILES {
                return;
            }
        }
    }
}

/// Replays git's own reflog into the ledger.
///
/// Git is the recovery source: it recorded every ref movement whether or not
/// GitPulse was running, and it keeps doing so if GitPulse is uninstalled. This
/// is what makes "the history is complete" true rather than "complete for as
/// long as the app was open".
pub fn ingest_reflog(repo_path: &str, max_entries: usize) -> CatchUp {
    let mut out = CatchUp::default();
    let entries = match crate::engine::git_reader::GitReader::get_reflog(repo_path, max_entries) {
        Ok(entries) => entries,
        Err(e) => {
            out.error = e;
            return out;
        }
    };

    // Already-recorded selectors, so a second open adds nothing.
    let mut seen = std::collections::HashSet::new();
    let mut cursor = 0i64;
    while let Ok(page) = ledger::tail(repo_path, cursor, 1000) {
        if page.is_empty() {
            break;
        }
        for event in &page {
            if event.action.starts_with("reflog.") {
                if let Some(object) = &event.object {
                    seen.insert(object.clone());
                }
            }
        }
        cursor = page[page.len() - 1].id;
        if page.len() < 1000 {
            break;
        }
    }

    // Oldest first, so the ledger's order matches the order things happened.
    for entry in entries.iter().rev() {
        // The selector (`HEAD@{3}`) is positional and shifts as the reflog
        // grows, so identity is the commit plus the message.
        let identity = format!("{} {}", entry.commit_id, entry.message);
        if seen.contains(&identity) {
            continue;
        }
        out.reflog_entries += 1;
        let action = format!(
            "reflog.{}",
            if entry.action.is_empty() {
                "move".to_string()
            } else {
                entry
                    .action
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() {
                            c.to_ascii_lowercase()
                        } else {
                            '_'
                        }
                    })
                    .collect::<String>()
            }
        );
        let detail = serde_json::json!({
            "source": "reflog",
            "selector": entry.selector,
            "message": entry.message,
        })
        .to_string();
        if ledger::record(Draft {
            repo_path: repo_path.to_string(),
            action,
            object: Some(identity),
            after_ref: Some(entry.commit_id.clone()),
            // Git does not record which actor moved the ref, and inventing one
            // would be a guess written to disk. `system` says GitPulse
            // synthesised this row from git's record.
            actor_kind: Some(ActorKind::System),
            actor_id: Some("reflog".into()),
            outcome: Some(Outcome::Ok),
            verdict_json: None,
            detail_json: Some(detail),
            ..Default::default()
        })
        .is_some()
        {
            out.recorded += 1;
        }
    }
    out
}

/// Runs both replays for a repository.
pub fn catch_up(repo_path: &str) -> CatchUp {
    let mut total = ingest_reflog(repo_path, 200);
    let transcripts = ingest_transcripts(repo_path);
    total.recorded += transcripts.recorded;
    total.transcripts = transcripts.transcripts;
    total.skipped_lines = transcripts.skipped_lines;
    if total.error.is_empty() {
        total.error = transcripts.error;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the tests that override the transcript root.
    ///
    /// The override is an environment variable, which is process-global: two
    /// tests setting and clearing it in parallel let one of them run against
    /// the developer's real `~/.claude/projects`. That is a 1.8 GB scan in the
    /// corpus this was measured on, and — worse — a test whose result depends
    /// on what happens to be on the machine.
    static ROOT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Runs `f` with the transcript root pointed at `root`, restoring it after.
    fn with_transcript_root<T>(root: &std::path::Path, f: impl FnOnce() -> T) -> T {
        let _guard = ROOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("GITPULSE_TRANSCRIPT_ROOT", root);
        let out = f();
        std::env::remove_var("GITPULSE_TRANSCRIPT_ROOT");
        out
    }

    fn transcript_fixture(dir: &std::path::Path, repo: &str, session: &str, ts: &str, file: &str) {
        let slug = dir.join("project-slug");
        std::fs::create_dir_all(&slug).unwrap();
        let line = serde_json::json!({
            "type": "assistant",
            "sessionId": session,
            "timestamp": ts,
            "version": "2.1.241",
            "cwd": repo,
            "gitBranch": "main",
            "message": { "content": [
                { "type": "tool_use", "name": "Edit", "input": { "file_path": file } }
            ]}
        })
        .to_string();
        std::fs::write(slug.join(format!("{session}.jsonl")), line + "\n").unwrap();
    }

    #[test]
    fn attributes_an_agent_edit_that_happened_while_closed() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().to_str().unwrap();
        let tdir = tempfile::tempdir().unwrap();
        transcript_fixture(
            tdir.path(),
            repo,
            "S1",
            "2026-09-01T12:00:00.000Z",
            &format!("{repo}/src/a.rs"),
        );

        let out = with_transcript_root(tdir.path(), || ingest_transcripts(repo));

        assert_eq!(out.recorded, 1, "the edit was not attributed");
        let events = ledger::tail(repo, 0, 10).unwrap();
        assert_eq!(events[0].action, "session.edit");
        assert_eq!(events[0].actor_kind, "agent");
        assert_eq!(events[0].actor_id.as_deref(), Some("claude-code"));
        assert_eq!(events[0].session_id.as_deref(), Some("S1"));
        assert!(
            events[0].verdict_json.is_none(),
            "GitPulse's gate never saw this; a verdict here would be a fabrication"
        );
    }

    #[test]
    fn a_second_pass_adds_nothing() {
        // Catch-up runs on every open, so it must be idempotent or a history
        // doubles every time the app starts.
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().to_str().unwrap();
        let tdir = tempfile::tempdir().unwrap();
        transcript_fixture(
            tdir.path(),
            repo,
            "S1",
            "2026-09-01T12:00:00.000Z",
            &format!("{repo}/src/a.rs"),
        );

        with_transcript_root(tdir.path(), || {
            assert_eq!(ingest_transcripts(repo).recorded, 1);
            assert_eq!(
                ingest_transcripts(repo).recorded,
                0,
                "the replay was not idempotent"
            );
        });
        assert_eq!(ledger::tail(repo, 0, 100).unwrap().len(), 1);
    }

    #[test]
    fn work_in_another_repository_is_not_attributed_here() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().to_str().unwrap();
        let tdir = tempfile::tempdir().unwrap();
        transcript_fixture(
            tdir.path(),
            "/somewhere/else",
            "S2",
            "2026-09-01T12:00:00.000Z",
            "/somewhere/else/a.rs",
        );

        let out = with_transcript_root(tdir.path(), || ingest_transcripts(repo));
        assert_eq!(out.recorded, 0);
    }

    #[test]
    fn no_transcripts_is_not_an_error() {
        let repo_dir = tempfile::tempdir().unwrap();
        let tdir = tempfile::tempdir().unwrap();
        let out = with_transcript_root(tdir.path(), || {
            ingest_transcripts(repo_dir.path().to_str().unwrap())
        });
        assert_eq!(out.recorded, 0);
        assert!(out.error.is_empty(), "an agent-free machine is normal");
    }

    #[test]
    fn unreadable_lines_are_counted_rather_than_hidden() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().to_str().unwrap();
        let tdir = tempfile::tempdir().unwrap();
        let slug = tdir.path().join("slug");
        std::fs::create_dir_all(&slug).unwrap();
        // A record that claims to be an assistant turn but cannot be parsed.
        std::fs::write(
            slug.join("broken.jsonl"),
            "{\"type\":\"assistant\", this is not json}\n",
        )
        .unwrap();

        let out = with_transcript_root(tdir.path(), || ingest_transcripts(repo));
        assert_eq!(out.recorded, 0);
        assert!(
            out.skipped_lines > 0,
            "a line we could not read must be reported, not silently dropped"
        );
    }
}

#[cfg(test)]
mod watermark_tests {
    use super::*;

    #[test]
    fn iso_round_trips_through_the_ledgers_own_formatter() {
        // The two conversions must agree, or the skip window is wrong and
        // events fall on the floor.
        for ms in [
            0u64,
            1_000,
            1_788_957_296_789,
            1_709_164_800_000,
            4_107_456_000_000,
        ] {
            let iso = crate::ledger::ids::iso8601_utc(ms);
            assert_eq!(iso_to_millis(&iso), ms, "round trip failed for {iso}");
        }
    }

    #[test]
    fn an_unparseable_timestamp_disables_the_skip() {
        // Failing open: the cost is time, and the alternative is silently not
        // reading a file that should have been read.
        for bad in ["", "not a date", "2026-09-01", "2026-09-01T12:00:00+01:00"] {
            assert_eq!(iso_to_millis(bad), 0, "{bad:?} should not parse");
        }
    }

    #[test]
    fn a_file_with_no_readable_mtime_is_read_anyway() {
        assert!(modified_since(std::path::Path::new("/does/not/exist"), 1));
    }

    #[test]
    fn a_freshly_written_file_is_not_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("t.jsonl");
        std::fs::write(&f, "{}").unwrap();
        let now = crate::ledger::ids::now_millis();
        assert!(modified_since(&f, now), "a file written now must be read");
        assert!(modified_since(&f, now - 60_000));
    }
}
