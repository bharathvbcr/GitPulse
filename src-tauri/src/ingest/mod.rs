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
//! Both replays run on every open, so both must be safe to run twice. The
//! reflog is keyed by the `reflog.*` objects already recorded. Transcripts are
//! keyed by how many bytes of each file this worktree has consumed, committed
//! in the same transaction as the events read from those bytes. Re-running
//! adds nothing, and a pass cut short by its deadline resumes exactly where it
//! stopped.
//!
//! Transcripts were once watermarked by the newest `session.*` row instead.
//! That compared an agent's timestamps against the ledger's *write* time, so
//! any call older than the last catch-up was taken as already read: a pass cut
//! short, a terminal spawn, or a line still being written lost events for
//! good. A worktree with no such row had no watermark at all and re-read the
//! whole corpus on every call.

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
    /// True when the wall-clock deadline stopped the pass early.
    ///
    /// A truncated pass is a floor: remaining transcripts/reflog entries were
    /// not examined. Never treat `recorded` alone as complete coverage when
    /// this is set.
    #[serde(default)]
    pub truncated: bool,
    /// Empty when the pass completed; otherwise what stopped it.
    pub error: String,
}

/// Whole-pass budget for catch-up. Cold watermarks once walked ~50s of
/// transcripts; the deadline stops the walk and reports `truncated` rather
/// than finishing late on the app-mount path.
pub const CATCH_UP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Where Claude Code keeps its transcripts.
fn transcript_root() -> Option<std::path::PathBuf> {
    std::env::var_os("GITPULSE_TRANSCRIPT_ROOT")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs_home().map(|h| h.join(".claude").join("projects")))
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

fn event_belongs_to_worktree(
    event: &crate::ledger::LedgerEvent,
    ledger_repo: &str,
    worktree_path: &str,
) -> bool {
    if ledger_repo == worktree_path {
        event.worktree_path.is_none() || event.worktree_path.as_deref() == Some(worktree_path)
    } else {
        event.worktree_path.as_deref() == Some(worktree_path)
    }
}

/// When the newest transcript row for this worktree was written, as the
/// pre-offset watermark computed it — or empty when there is none.
///
/// Read once per worktree, to freeze the line the old scheme had drawn. Only
/// actions a transcript produces count: `session.spawn` is GitPulse opening a
/// terminal, and letting it count is how a spawn used to hide agent work.
fn legacy_watermark(ledger_repo: &str, worktree_path: &str) -> Result<String, String> {
    let mut cursor = 0i64;
    let mut newest = String::new();
    loop {
        let page = ledger::tail(ledger_repo, cursor, 1000).map_err(|e| e.to_string())?;
        for event in &page {
            if event_belongs_to_worktree(event, ledger_repo, worktree_path)
                && transcript::ACTIONS.contains(&event.action.as_str())
                && event.ts_utc > newest
            {
                newest = event.ts_utc.clone();
            }
        }
        match page.last() {
            Some(last) if page.len() == 1000 => cursor = last.id,
            _ => return Ok(newest),
        }
    }
}

/// Replays agent transcripts for `repo_path`.
///
/// Only calls newer than the watermark are recorded, so opening a repository
/// twice does not double its history.
pub fn ingest_transcripts(repo_path: &str) -> CatchUp {
    ingest_transcripts_into(repo_path, repo_path)
}

/// Replays transcripts observed in `worktree_path` into the repository-wide
/// ledger at `ledger_repo`.
pub(crate) fn ingest_transcripts_into(ledger_repo: &str, worktree_path: &str) -> CatchUp {
    ingest_transcripts_into_bounded(ledger_repo, worktree_path, None)
}

fn ingest_transcripts_into_bounded(
    ledger_repo: &str,
    worktree_path: &str,
    deadline: Option<std::time::Instant>,
) -> CatchUp {
    ingest_transcripts_until(ledger_repo, worktree_path, &mut |_| {
        deadline.is_some_and(|d| std::time::Instant::now() >= d)
    })
}

/// The pass itself, stopped when `expired` says so.
///
/// `expired` sees the pass's progress so far. Production budgets ignore it and
/// read the clock; a test stops the pass at a chosen point of progress, which a
/// wall-clock deadline cannot do deterministically.
fn ingest_transcripts_until(
    ledger_repo: &str,
    worktree_path: &str,
    expired: &mut dyn FnMut(&CatchUp) -> bool,
) -> CatchUp {
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

    let progress = match ledger::transcript_progress(ledger_repo, worktree_path) {
        Ok(progress) => progress,
        Err(e) => {
            out.error = format!("transcript progress could not be read: {e}");
            return out;
        }
    };
    let baseline = match progress.baseline {
        Some(baseline) => baseline,
        None => match legacy_watermark(ledger_repo, worktree_path).and_then(|since| {
            ledger::freeze_transcript_baseline(ledger_repo, worktree_path, &since)
                .map_err(|e| e.to_string())
        }) {
            Ok(baseline) => baseline,
            Err(e) => {
                out.error = format!("transcript baseline could not be set: {e}");
                return out;
            }
        },
    };
    let baseline_ms = iso_to_millis(&baseline);
    let task_id = crate::ledger::bindings::resolve(ledger_repo, worktree_path)
        .ok()
        .flatten();
    let mut files = Vec::new();
    let listed_all = collect_jsonl(&root, &mut files, 0);
    let mut listed = std::collections::HashSet::new();
    let mut pending = Vec::new();
    for path in files {
        if expired(&out) {
            out.truncated = true;
            break;
        }
        let file_key = ledger::transcript_key(&path);
        listed.insert(file_key.clone());
        let from = progress.offsets.get(&file_key).copied();
        let Ok(len) = std::fs::metadata(&path).map(|meta| meta.len()) else {
            out.skipped_lines += 1;
            continue;
        };
        // `floor` applies only to a transcript this worktree has never read
        // under offsets: calls at or before the frozen baseline were the old
        // watermark's to record, and reading them again would duplicate them.
        let (start, floor) = match from {
            // Nothing appended since the last read. This is what keeps a
            // worktree no agent touched from re-reading the corpus every call.
            Some(done) if done == len => continue,
            Some(done) if done < len => (done, None),
            // Shorter than what was read: rewritten rather than appended to.
            // Read it again — a duplicate row is recoverable, a lost one is not.
            Some(_) => (0, None),
            // Untouched since the baseline: the old watermark already read it.
            None if baseline_ms > 0 && !modified_since(&path, baseline_ms) => continue,
            None => (0, (!baseline.is_empty()).then_some(baseline.as_str())),
        };
        let Ok(bytes) = read_range(&path, start, len) else {
            // A transcript being written right now can fail a read; the next
            // pass picks it up. Counted so the gap is visible.
            out.skipped_lines += 1;
            continue;
        };
        out.transcripts += 1;
        // Only whole lines. A last line with no newline yet may still be being
        // written; it is left for the pass that sees it finished.
        let whole = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let mut consumed = 0usize;
        let mut drafts = Vec::new();
        for raw in bytes[..whole].split_inclusive(|b| *b == b'\n') {
            if expired(&out) {
                out.truncated = true;
                break;
            }
            consumed += raw.len();
            let Ok(line) = std::str::from_utf8(raw) else {
                out.skipped_lines += 1;
                continue;
            };
            let line = line.trim_end_matches(['\n', '\r']);
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
                if !transcript::belongs_to(&call, worktree_path) {
                    continue;
                }
                if floor.is_some_and(|floor| call.ts_utc.as_str() <= floor) {
                    continue;
                }
                drafts.push(transcript_draft(
                    &call,
                    ledger_repo,
                    worktree_path,
                    &task_id,
                ));
            }
        }
        let to = start + consumed as u64;
        if from != Some(to) && !(from.is_none() && to == 0) {
            let has_events = !drafts.is_empty();
            pending.push(ledger::TranscriptAdvance {
                file_key,
                from,
                to,
                drafts,
            });
            // Events are committed as soon as their transcript is read, so a
            // deadline never discards them. Offset-only advances are batched:
            // a first read of a corpus holding nothing for this worktree is
            // thousands of them.
            if (has_events || pending.len() >= ADVANCE_BATCH)
                && !commit(ledger_repo, worktree_path, &mut pending, &mut out)
            {
                return out;
            }
        }
        if out.truncated {
            break;
        }
    }
    if !commit(ledger_repo, worktree_path, &mut pending, &mut out) {
        return out;
    }
    // Forget transcripts that are gone, but only on a pass that saw the whole
    // corpus: a file missing from a partial listing may still exist, and
    // forgetting its offset would replay it.
    if listed_all && !out.truncated {
        let gone: Vec<String> = progress
            .offsets
            .into_keys()
            .filter(|key| !listed.contains(key))
            .collect();
        if let Err(e) = ledger::forget_transcripts(ledger_repo, worktree_path, &gone) {
            out.error = format!("vanished transcripts could not be forgotten: {e}");
        }
    }
    out
}

/// Offset-only advances held before one commit.
const ADVANCE_BATCH: usize = 512;

/// Commits `pending`, emptying it. False, with the error in `out`, when the
/// ledger refused it — the pass stops there rather than read further into
/// events it could not keep.
fn commit(
    ledger_repo: &str,
    worktree_path: &str,
    pending: &mut Vec<ledger::TranscriptAdvance>,
    out: &mut CatchUp,
) -> bool {
    if pending.is_empty() {
        return true;
    }
    match ledger::advance_transcripts(ledger_repo, worktree_path, std::mem::take(pending)) {
        Ok(recorded) => {
            out.recorded += recorded;
            true
        }
        Err(e) => {
            out.error = format!("transcript events could not be recorded: {e}");
            false
        }
    }
}

/// The bytes of `path` in `start..len`. Bounded by the length observed before
/// the read, so a transcript growing under the reader is not chased.
fn read_range(path: &std::path::Path, start: u64, len: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek};
    let mut file = std::fs::File::open(path)?;
    file.seek(std::io::SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(len.saturating_sub(start))
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn transcript_draft(
    call: &transcript::ToolCall,
    ledger_repo: &str,
    worktree_path: &str,
    task_id: &Option<String>,
) -> Draft {
    let detail = serde_json::json!({
        "source": "transcript",
        "tool": call.tool,
        "transcript_version": call.version,
        "git_branch": call.git_branch,
    })
    .to_string();
    Draft {
        repo_path: ledger_repo.to_string(),
        worktree_path: (ledger_repo != worktree_path).then(|| worktree_path.to_string()),
        action: call.action().to_string(),
        object: Some(call.object()),
        session_id: Some(call.session_id.clone()),
        task_id: task_id.clone(),
        // Derived from observation, not self-reported: this row exists because
        // a transcript recorded the call, not because an agent announced it.
        actor_kind: Some(ActorKind::Agent),
        actor_id: Some("claude-code".into()),
        outcome: Some(Outcome::Ok),
        // No verdict: GitPulse's gate never saw this action. That is
        // emphatically not the same as an action that passed, and the absence
        // is what says so.
        verdict_json: None,
        detail_json: Some(detail),
        ..Default::default()
    }
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
    // `get` rather than `iso[a..b]`: the length check above counts bytes, so a
    // non-ASCII string long enough to pass it would otherwise panic on a
    // mid-character index instead of failing open with 0.
    let num = |a: usize, b: usize| iso.get(a..b).and_then(|s| s.parse::<i64>().ok());
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
///
/// Returns whether `out` is the whole corpus: false when a directory could not
/// be read or the file cap cut the walk short. Beyond the depth bound counts as
/// complete — those files are never listed, so never have an offset to lose.
fn collect_jsonl(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>, depth: usize) -> bool {
    const MAX_DEPTH: usize = 4;
    const MAX_FILES: usize = 5000;
    if depth > MAX_DEPTH {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut complete = true;
    for entry in entries {
        let Ok(entry) = entry else {
            complete = false;
            continue;
        };
        let path = entry.path();
        if path.is_dir() {
            complete &= collect_jsonl(&path, out, depth + 1);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            if out.len() >= MAX_FILES {
                return false;
            }
            out.push(path);
        }
    }
    complete
}

/// Replays git's own reflog into the ledger.
///
/// Git is the recovery source: it recorded every ref movement whether or not
/// GitPulse was running, and it keeps doing so if GitPulse is uninstalled. This
/// is what makes "the history is complete" true rather than "complete for as
/// long as the app was open".
pub fn ingest_reflog(repo_path: &str, max_entries: usize) -> CatchUp {
    ingest_reflog_into(repo_path, repo_path, max_entries)
}

/// Replays one checkout's HEAD reflog into its repository-wide ledger.
pub(crate) fn ingest_reflog_into(
    ledger_repo: &str,
    worktree_path: &str,
    max_entries: usize,
) -> CatchUp {
    let mut out = CatchUp::default();
    let entries = match crate::engine::git_reader::GitReader::get_reflog(worktree_path, max_entries)
    {
        Ok(entries) => entries,
        Err(e) => {
            out.error = e;
            return out;
        }
    };

    // Already-recorded selectors, so a second open adds nothing.
    let mut seen = std::collections::HashSet::new();
    let mut cursor = 0i64;
    while let Ok(page) = ledger::tail(ledger_repo, cursor, 1000) {
        if page.is_empty() {
            break;
        }
        for event in &page {
            if event_belongs_to_worktree(event, ledger_repo, worktree_path)
                && event.action.starts_with("reflog.")
            {
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
    let task_id = crate::ledger::bindings::resolve(ledger_repo, worktree_path)
        .ok()
        .flatten();
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
            repo_path: ledger_repo.to_string(),
            worktree_path: (ledger_repo != worktree_path).then(|| worktree_path.to_string()),
            task_id: task_id.clone(),
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
    catch_up_into(repo_path, repo_path)
}

/// Replays one checkout into the shared repository ledger. Calling this again
/// through either the main or linked spelling is idempotent because each
/// source worktree has its own watermark inside that shared log.
pub fn catch_up_into(ledger_repo: &str, worktree_path: &str) -> CatchUp {
    catch_up_into_bounded(ledger_repo, worktree_path, Some(CATCH_UP_DEADLINE))
}

/// Like [`catch_up_into`] with an explicit budget; `None` means unbounded (tests).
pub fn catch_up_into_bounded(
    ledger_repo: &str,
    worktree_path: &str,
    budget: Option<std::time::Duration>,
) -> CatchUp {
    let started = std::time::Instant::now();
    let deadline = budget.map(|d| started + d);
    let expired = || deadline.is_some_and(|d| std::time::Instant::now() >= d);

    let mut total = ingest_reflog_into(ledger_repo, worktree_path, 200);
    if expired() {
        total.truncated = true;
        if total.error.is_empty() {
            total.error = "catch-up truncated: deadline elapsed after reflog".into();
        } else if !total.error.contains("truncated") {
            total.error = format!(
                "{}; catch-up truncated: deadline elapsed after reflog",
                total.error
            );
        }
        return total;
    }
    let transcripts = ingest_transcripts_into_bounded(ledger_repo, worktree_path, deadline);
    total.recorded += transcripts.recorded;
    total.transcripts = transcripts.transcripts;
    total.skipped_lines = transcripts.skipped_lines;
    total.truncated = total.truncated || transcripts.truncated;
    if total.error.is_empty() {
        total.error = if transcripts.truncated && transcripts.error.is_empty() {
            "catch-up truncated: deadline elapsed during transcripts".into()
        } else {
            transcripts.error
        };
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::git_in;

    /// Serialises the tests that override the transcript root.
    ///
    /// The override is an environment variable, which is process-global: two
    /// tests setting and clearing it in parallel let one of them run against
    /// the developer's real `~/.claude/projects`. That is a 1.8 GB scan in the
    /// corpus this was measured on, and — worse — a test whose result depends
    /// on what happens to be on the machine.
    static ROOT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Runs `f` with the transcript root pointed at `root`, restoring it after.
    ///
    /// "After" includes the case where `f` panics: a `f()` that failed used to
    /// skip the restore and hand every later test in this process a transcript
    /// root pointing into a deleted `TempDir`.
    fn with_transcript_root<T>(root: &std::path::Path, f: impl FnOnce() -> T) -> T {
        let serial = ROOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env =
            crate::test_support::env::bind_env(&serial).set("GITPULSE_TRANSCRIPT_ROOT", root);
        f()
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

    #[test]
    fn linked_reflog_catch_up_is_shared_attributed_and_idempotent() {
        let main = tempfile::tempdir().expect("main checkout");
        git_in(main.path(), &["init", "-b", "main"]);
        std::fs::write(main.path().join("seed.txt"), "seed").expect("seed repository");
        git_in(main.path(), &["add", "seed.txt"]);
        git_in(main.path(), &["commit", "-m", "seed"]);

        let parent = tempfile::tempdir().expect("worktree parent");
        let linked = parent.path().join("linked");
        git_in(
            main.path(),
            &[
                "worktree",
                "add",
                "--detach",
                linked.to_str().expect("utf8 worktree"),
            ],
        );
        crate::test_support::trust_repo(&linked);
        std::fs::write(linked.join("linked.txt"), "linked").expect("linked change");
        git_in(&linked, &["add", "linked.txt"]);
        git_in(&linked, &["commit", "-m", "linked change"]);

        let anchor_path = main.path().canonicalize().expect("canonical main");
        let worktree_path = linked.canonicalize().expect("canonical worktree");
        let anchor = anchor_path.to_string_lossy();
        let worktree = worktree_path.to_string_lossy();
        let first = ingest_reflog_into(&anchor, &worktree, 200);
        assert!(
            first.recorded > 0,
            "the linked HEAD reflog was not imported"
        );
        let events = ledger::tail(&anchor, 0, 1000).expect("family ledger");
        assert_eq!(events.len() as i64, first.recorded);
        assert!(events.iter().all(|event| event.repo_path == anchor));
        assert!(events
            .iter()
            .all(|event| event.worktree_path.as_deref() == Some(worktree.as_ref())));

        let second = ingest_reflog_into(&anchor, &worktree, 200);
        assert_eq!(second.recorded, 0, "a repeated catch-up duplicated rows");
        assert_eq!(
            ledger::tail(&anchor, 0, 1000).expect("family ledger").len(),
            events.len(),
            "the second pass changed the shared ledger"
        );
    }

    #[test]
    fn catch_up_deadline_reports_truncated_rather_than_finishing_late() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().to_str().unwrap();
        let tdir = tempfile::tempdir().unwrap();
        // Many transcript files so the walk has work to interrupt.
        for i in 0..200 {
            transcript_fixture(
                tdir.path(),
                repo,
                &format!("S{i}"),
                "2026-09-01T12:00:00.000Z",
                &format!("{repo}/src/a{i}.rs"),
            );
        }
        let out = with_transcript_root(tdir.path(), || {
            catch_up_into_bounded(repo, repo, Some(std::time::Duration::ZERO))
        });
        assert!(
            out.truncated,
            "a zero budget must report truncated, got {out:?}"
        );
        assert!(
            out.error.contains("truncated"),
            "truncated must be named in error, got {:?}",
            out.error
        );
    }

    /// Moves a fixture's mtime an hour into the past, so a pass that ran just
    /// after writing it is not inside `modified_since`'s one-second slack.
    fn backdate(path: &std::path::Path) {
        let an_hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(an_hour_ago)
            .unwrap();
    }

    fn backdate_all(dir: &std::path::Path) {
        let mut files = Vec::new();
        collect_jsonl(dir, &mut files, 0);
        for f in &files {
            backdate(f);
        }
    }

    #[test]
    fn a_worktree_with_no_attributed_work_does_not_reread_the_corpus() {
        // A checkout no agent has touched has no `session.*` row to anchor a
        // skip on. The corpus is still the same corpus the last pass read, and
        // reading it again on every call can only find the same nothing. On a
        // real machine that is a 5 s budget burned on every catch-up, forever.
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().to_str().unwrap();
        let tdir = tempfile::tempdir().unwrap();
        for i in 0..20 {
            transcript_fixture(
                tdir.path(),
                "/somewhere/else",
                &format!("F{i}"),
                "2026-09-01T12:00:00.000Z",
                &format!("/somewhere/else/a{i}.rs"),
            );
        }
        backdate_all(tdir.path());

        let (first, second) = with_transcript_root(tdir.path(), || {
            (ingest_transcripts(repo), ingest_transcripts(repo))
        });
        assert_eq!(first.transcripts, 20, "the first pass must read the corpus");
        assert_eq!(first.recorded, 0);
        assert_eq!(
            second.transcripts, 0,
            "nothing changed since the first pass, yet the second re-read {} files",
            second.transcripts
        );
    }

    #[test]
    fn a_truncated_pass_does_not_lose_what_it_never_reached() {
        // Two transcripts, each holding one edit to this repository. The first
        // pass is stopped right after it records one of them. The edit in the
        // file it never reached is still owed, and the next pass must deliver it.
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().to_str().unwrap();
        let tdir = tempfile::tempdir().unwrap();
        for (session, file) in [("A", "a.rs"), ("B", "b.rs")] {
            transcript_fixture(
                tdir.path(),
                repo,
                session,
                "2026-09-01T12:00:00.000Z",
                &format!("{repo}/src/{file}"),
            );
        }
        backdate_all(tdir.path());

        let (first, second, third) = with_transcript_root(tdir.path(), || {
            let first = ingest_transcripts_until(repo, repo, &mut |out| out.recorded >= 1);
            (first, ingest_transcripts(repo), ingest_transcripts(repo))
        });
        assert!(first.truncated, "the first pass was meant to be cut short");
        assert_eq!(first.recorded, 1);
        assert_eq!(
            second.recorded, 1,
            "the edit in the transcript the truncated pass never reached was dropped"
        );
        assert_eq!(third.recorded, 0, "a completed catch-up must be idempotent");
        let sessions: Vec<_> = ledger::tail(repo, 0, 100)
            .unwrap()
            .into_iter()
            .filter_map(|e| e.session_id)
            .collect();
        assert_eq!(
            sessions.len(),
            2,
            "expected one row per edit, got {sessions:?}"
        );
    }

    #[test]
    fn a_terminal_spawn_does_not_hide_transcript_work() {
        // `session.spawn` is GitPulse opening a terminal, not a transcript
        // replay. It says nothing about which transcript calls have been read.
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
        ledger::record(Draft {
            repo_path: repo.to_string(),
            actor_kind: Some(ActorKind::Human),
            actor_id: Some("/bin/zsh".into()),
            session_id: Some("pty-1".into()),
            action: "session.spawn".into(),
            object: Some("/bin/zsh".into()),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .expect("spawn row");

        let out = with_transcript_root(tdir.path(), || ingest_transcripts(repo));
        assert_eq!(
            out.recorded, 1,
            "a terminal opened in GitPulse hid an agent edit from catch-up"
        );
    }

    fn assistant_line(repo: &str, session: &str, ts: &str, file: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "sessionId": session,
            "timestamp": ts,
            "version": "2.1.241",
            "cwd": repo,
            "message": { "content": [
                { "type": "tool_use", "name": "Edit", "input": { "file_path": file } }
            ]}
        })
        .to_string()
    }

    #[test]
    fn a_line_still_being_written_is_read_once_it_is_complete() {
        // An agent appends to its transcript while catch-up reads it. A pass
        // that lands mid-write sees a line with no newline yet; that line is
        // owed to the next pass, not judged and forgotten.
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().to_str().unwrap();
        let tdir = tempfile::tempdir().unwrap();
        let slug = tdir.path().join("slug");
        std::fs::create_dir_all(&slug).unwrap();
        let file = slug.join("S1.jsonl");
        let first = assistant_line(
            repo,
            "S1",
            "2026-09-01T12:00:00.000Z",
            &format!("{repo}/a.rs"),
        );
        let second = assistant_line(
            repo,
            "S1",
            "2026-09-01T12:00:01.000Z",
            &format!("{repo}/b.rs"),
        );
        let (head, tail) = second.split_at(second.len() / 2);
        std::fs::write(&file, format!("{first}\n{head}")).unwrap();

        let (before, after) = with_transcript_root(tdir.path(), || {
            let before = ingest_transcripts(repo);
            let mut f = std::fs::File::options().append(true).open(&file).unwrap();
            std::io::Write::write_all(&mut f, format!("{tail}\n").as_bytes()).unwrap();
            (before, ingest_transcripts(repo))
        });
        assert_eq!(before.recorded, 1);
        assert_eq!(
            after.recorded, 1,
            "the line that finished after the first read was never recorded"
        );
    }

    #[test]
    fn upgrading_does_not_replay_what_the_old_watermark_already_recorded() {
        // A ledger written by the timestamp watermark holds a `session.edit`
        // row for the edit in `old.jsonl`. The first pass after an upgrade
        // must not record that edit a second time.
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().to_str().unwrap();
        let tdir = tempfile::tempdir().unwrap();
        transcript_fixture(
            tdir.path(),
            repo,
            "old",
            "2026-09-01T12:00:00.000Z",
            &format!("{repo}/src/a.rs"),
        );
        backdate_all(tdir.path());
        ledger::record(Draft {
            repo_path: repo.to_string(),
            actor_kind: Some(ActorKind::Agent),
            actor_id: Some("claude-code".into()),
            session_id: Some("old".into()),
            action: "session.edit".into(),
            object: Some(format!("{repo}/src/a.rs")),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .expect("legacy row");
        // Written after the legacy row, holding one call the old watermark
        // already covered and one it did not.
        let slug = tdir.path().join("project-slug");
        std::fs::write(
            slug.join("new.jsonl"),
            format!(
                "{}\n{}\n",
                assistant_line(
                    repo,
                    "new",
                    "2026-09-01T12:00:00.000Z",
                    &format!("{repo}/x.rs")
                ),
                assistant_line(
                    repo,
                    "new",
                    "2099-01-01T00:00:00.000Z",
                    &format!("{repo}/y.rs")
                ),
            ),
        )
        .unwrap();

        let out = with_transcript_root(tdir.path(), || ingest_transcripts(repo));
        assert_eq!(out.error, "");
        assert_eq!(
            out.recorded, 1,
            "expected only the call after the old watermark"
        );
        let objects: Vec<_> = ledger::tail(repo, 0, 100)
            .unwrap()
            .into_iter()
            .filter_map(|e| e.object)
            .collect();
        assert_eq!(objects.len(), 2, "history was replayed: {objects:?}");
    }

    #[test]
    fn a_pass_cut_short_mid_transcript_resumes_at_the_next_line() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().to_str().unwrap();
        let tdir = tempfile::tempdir().unwrap();
        let slug = tdir.path().join("slug");
        std::fs::create_dir_all(&slug).unwrap();
        let lines: Vec<String> = (0..3)
            .map(|i| {
                assistant_line(
                    repo,
                    "S1",
                    &format!("2026-09-01T12:00:0{i}.000Z"),
                    &format!("{repo}/f{i}.rs"),
                )
            })
            .collect();
        std::fs::write(slug.join("S1.jsonl"), lines.join("\n") + "\n").unwrap();

        let (first, second, third) = with_transcript_root(tdir.path(), || {
            // Checks: before the file, before line 1, before line 2 — so the
            // pass stops with exactly one line read.
            let mut checks = 0;
            let first = ingest_transcripts_until(repo, repo, &mut |_| {
                checks += 1;
                checks > 2
            });
            (first, ingest_transcripts(repo), ingest_transcripts(repo))
        });
        assert!(first.truncated);
        assert_eq!(first.recorded, 1, "the line read before the stop is kept");
        assert_eq!(second.recorded, 2, "the rest of the transcript is owed");
        assert_eq!(third.recorded, 0);
        let mut objects: Vec<_> = ledger::tail(repo, 0, 100)
            .unwrap()
            .into_iter()
            .filter_map(|e| e.object)
            .collect();
        objects.sort();
        assert_eq!(
            objects,
            (0..3)
                .map(|i| format!("{repo}/f{i}.rs"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_advance_overtaken_by_another_pass_records_nothing() {
        // The app and `gitpulsed` both catch up. Two passes that read the same
        // bytes from the same offset must not both record what they found.
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().to_str().unwrap();
        let advance = || ledger::TranscriptAdvance {
            file_key: "f".into(),
            from: None,
            to: 10,
            drafts: vec![Draft {
                repo_path: repo.to_string(),
                action: "session.edit".into(),
                object: Some("x".into()),
                ..Default::default()
            }],
        };
        assert_eq!(
            ledger::advance_transcripts(repo, repo, vec![advance()]).unwrap(),
            1
        );
        assert_eq!(
            ledger::advance_transcripts(repo, repo, vec![advance()]).unwrap(),
            0,
            "a stale advance recorded its events a second time"
        );
        assert_eq!(ledger::tail(repo, 0, 100).unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_that_cannot_be_listed_keeps_its_offsets() {
        use std::os::unix::fs::PermissionsExt;
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
        let slug = tdir.path().join("project-slug");
        let set_mode =
            |mode| std::fs::set_permissions(&slug, std::fs::Permissions::from_mode(mode)).unwrap();

        let (first, hidden, back) = with_transcript_root(tdir.path(), || {
            let first = ingest_transcripts(repo);
            set_mode(0o000);
            let hidden = ingest_transcripts(repo);
            set_mode(0o755);
            (first, hidden, ingest_transcripts(repo))
        });
        assert_eq!(first.recorded, 1);
        assert_eq!(hidden.recorded, 0);
        assert_eq!(
            back.recorded, 0,
            "a transcript hidden from one listing was forgotten and replayed"
        );
        assert_eq!(ledger::tail(repo, 0, 100).unwrap().len(), 1);
    }

    #[test]
    fn a_transcript_that_is_gone_is_forgotten() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().to_str().unwrap();
        let tdir = tempfile::tempdir().unwrap();
        transcript_fixture(
            tdir.path(),
            "/elsewhere",
            "S1",
            "2026-09-01T12:00:00.000Z",
            "/elsewhere/a",
        );
        transcript_fixture(
            tdir.path(),
            "/elsewhere",
            "S2",
            "2026-09-01T12:00:00.000Z",
            "/elsewhere/b",
        );
        with_transcript_root(tdir.path(), || {
            ingest_transcripts(repo);
            assert_eq!(
                ledger::transcript_progress(repo, repo)
                    .unwrap()
                    .offsets
                    .len(),
                2
            );
            std::fs::remove_file(tdir.path().join("project-slug").join("S1.jsonl")).unwrap();
            ingest_transcripts(repo);
        });
        assert_eq!(
            ledger::transcript_progress(repo, repo)
                .unwrap()
                .offsets
                .len(),
            1,
            "offsets must be bounded by the corpus, not by its history"
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
        // The last two are long enough in *bytes* to clear the length guard
        // while their multi-byte characters straddle the fields it then reads.
        for bad in [
            "",
            "not a date",
            "2026-09-01",
            "2026-09-01T12:00:00+01:00",
            "2026\u{2014}09-01T12:00:00Z",
            "\u{1F680}026-09-01T12:00:00Z",
        ] {
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
