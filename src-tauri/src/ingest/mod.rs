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
//! # One pass for a repository's worktrees
//!
//! Progress is still kept per worktree, but a pass serves every worktree of the
//! repository at once ([`pass`]): each transcript is read once, from the lowest
//! offset any of them still owes it, each line is parsed once and offered to
//! every worktree whose cursor is at or below it, and all the cursors advance in
//! one transaction. The whole call runs under one budget and one flight per
//! repository ([`flight`]), so a stalled phase cannot hold the caller past its
//! deadline and a tab switch joins the pass that is already running.
//!
//! Transcripts were once watermarked by the newest `session.*` row instead.
//! That compared an agent's timestamps against the ledger's *write* time, so
//! any call older than the last catch-up was taken as already read: a pass cut
//! short, a terminal spawn, or a line still being written lost events for
//! good. A worktree with no such row had no watermark at all and re-read the
//! whole corpus on every call.

mod flight;
mod lines;
mod pass;
pub mod transcript;

use crate::ledger::{self, ActorKind, Draft, Outcome};
use flight::{Budget, Phase, Progress};

/// What one catch-up pass found.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CatchUp {
    /// Ledger events written by this pass.
    pub recorded: i64,
    /// Transcript files read.
    pub transcripts: i64,
    /// Transcript files the corpus walk listed. A pass the deadline cut short
    /// reads fewer than it listed; carrying both numbers is what keeps a
    /// partial pass from reading as a complete one.
    #[serde(default)]
    pub transcripts_listed: i64,
    /// Transcript lines this build could not read.
    ///
    /// Reported rather than swallowed: a parser that silently skipped
    /// unrecognised records would make a partial history look complete, and
    /// the transcript format is known to move — 17 distinct schema versions
    /// appear across the real corpus.
    pub skipped_lines: i64,
    /// Calls whose worktree had no slot in this pass (more worktrees than one
    /// pass serves) or was removed, and which another worktree's transaction
    /// carried. The row is the owner's either way (its path, its task); this
    /// counts only the ones that needed a host beyond the participant cap. Zero
    /// for any real family.
    #[serde(default)]
    pub rehosted_calls: i64,
    /// Calls from removed worktrees past the per-pass bound on new ones. Their
    /// rows are recorded under the removed worktree's path, but its own cursor
    /// was not written, so a later pass of its own could record them again.
    #[serde(default)]
    pub departed_overflow: i64,
    /// True when the worktree family was not resolved this cycle. For a Git
    /// checkout (an untrusted one, a failed worktree listing) the transcript
    /// pass was SKIPPED: no row written, no cursor moved, `error` says why, and
    /// the next cycle catches up. For a directory that is not a Git checkout
    /// the pass ran for that one directory, which is its whole family.
    #[serde(default)]
    pub family_unresolved: bool,
    /// Consecutive cycles, in this process, a Git checkout's transcript pass was
    /// skipped because its family could not be resolved. Zero once it resolves;
    /// a number that only grows is a family that never will.
    #[serde(default)]
    pub family_unresolved_cycles: i64,
    /// Reflog entries replayed.
    pub reflog_entries: i64,
    /// True when the wall-clock deadline stopped the pass early.
    ///
    /// A truncated pass is a floor: remaining transcripts/reflog entries were
    /// not examined. Never treat `recorded` alone as complete coverage when
    /// this is set.
    #[serde(default)]
    pub truncated: bool,
    /// Ledger events the retention policy deleted at the end of this pass.
    #[serde(default)]
    pub pruned: i64,
    /// Empty when the pass completed; otherwise what stopped it.
    pub error: String,
}

/// Whole-pass budget for catch-up. Cold watermarks once walked ~50s of
/// transcripts; the deadline stops the walk and reports `truncated` rather
/// than finishing late on the app-mount path.
pub const CATCH_UP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Where Claude Code keeps its transcripts.
pub(crate) fn transcript_root() -> Option<std::path::PathBuf> {
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

/// When the newest transcript row for each of `worktrees` was written, as the
/// pre-offset watermark computed it — empty for one with none — in the order
/// asked.
///
/// Read once per worktree, to freeze the line the old scheme had drawn, and for
/// all the worktrees that need it in one scan of the ledger. Only actions a
/// transcript produces count: `session.spawn` is GitPulse opening a terminal,
/// and letting it count is how a spawn used to hide agent work. `None` when
/// `expired` cut the scan: a line drawn from part of the history is the wrong
/// line.
fn legacy_watermarks(
    ledger_repo: &str,
    worktrees: &[&str],
    expired: &mut dyn FnMut() -> bool,
) -> Result<Option<Vec<String>>, String> {
    let mut cursor = 0i64;
    let mut newest = vec![String::new(); worktrees.len()];
    loop {
        if expired() {
            return Ok(None);
        }
        let page = ledger::tail(ledger_repo, cursor, 1000).map_err(|e| e.to_string())?;
        for event in &page {
            if !transcript::ACTIONS.contains(&event.action.as_str()) {
                continue;
            }
            for (path, newest) in worktrees.iter().zip(newest.iter_mut()) {
                if event_belongs_to_worktree(event, ledger_repo, path) && event.ts_utc > *newest {
                    newest.clone_from(&event.ts_utc);
                }
            }
        }
        match page.last() {
            Some(last) if page.len() == 1000 => cursor = last.id,
            _ => return Ok(Some(newest)),
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
    ingest_transcripts_into_bounded(ledger_repo, worktree_path, &Budget::unbounded())
}

fn ingest_transcripts_into_bounded(
    ledger_repo: &str,
    worktree_path: &str,
    budget: &Budget,
) -> CatchUp {
    ingest_transcripts_until(ledger_repo, worktree_path, &mut |_| budget.expired())
}

/// One worktree's pass, stopped when `expired` says so: a family pass of one.
///
/// `expired` sees the pass's progress so far. Production budgets ignore it and
/// read the clock; a test stops the pass at a chosen point of progress, which a
/// wall-clock deadline cannot do deterministically.
fn ingest_transcripts_until(
    ledger_repo: &str,
    worktree_path: &str,
    expired: &mut dyn FnMut(&CatchUp) -> bool,
) -> CatchUp {
    pass::ingest_family_until(ledger_repo, worktree_path, &[], expired)
}

/// Every other checkout that shares `worktree_path`'s repository, so one pass
/// can read each transcript once for all of them.
///
/// `Err` when the family could not be resolved (not a Git repository, an
/// untrusted one, a failed worktree listing). That is not an empty family: the
/// caller must not conclude from it that no other worktree is live. For a Git
/// checkout it skips the transcript pass for the cycle; only a directory that
/// is not a Git checkout gets a pass for itself alone. Either way it reports
/// `family_unresolved`.
fn family_siblings(ledger_repo: &str, worktree_path: &str) -> Result<Vec<String>, String> {
    crate::engine::worktree::resolve_worktree_family(ledger_repo, worktree_path)
        .map(|family| {
            family
                .members
                .iter()
                .map(|member| member.to_string_lossy().into_owned())
                .filter(|member| member != worktree_path)
                .collect()
        })
        .map_err(|e| e.to_string())
}

/// Whether `path` is a Git checkout, by the one definition the engine uses.
fn is_git_checkout(path: &str) -> bool {
    std::path::Path::new(path)
        .canonicalize()
        .is_ok_and(|canonical| crate::engine::git_cli::is_git_repository(&canonical))
}

/// Consecutive cycles in this process whose family could not be resolved, per
/// checkout: `unresolved` extends the run and returns its length, anything else
/// ends it. A family that never resolves shows as a count that only grows.
fn unresolved_streak(key: &str, unresolved: bool) -> i64 {
    static STREAKS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, i64>>> =
        std::sync::OnceLock::new();
    let mut streaks = STREAKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !unresolved {
        streaks.remove(key);
        return 0;
    }
    let streak = streaks.entry(key.to_string()).or_insert(0);
    *streak += 1;
    *streak
}

/// How far past the ingest clock an event's own time may lie before it is not
/// believed: a clock that is a minute fast is a clock, not a fact.
const MAX_FUTURE_SKEW_MS: u64 = 60_000;

/// When something happened and which task was bound then, as a row's
/// `detail_json` records them: `occurred_at`, `occurred_at_basis` and
/// `task_basis`. They live there, and not as columns or event fields, because
/// the event contract is closed to new fields.
struct EventTime {
    occurred_at: String,
    /// `transcript` or `reflog` when the source's own time was used, `ingest`
    /// when it was missing, unreadable or in the future.
    basis: &'static str,
    task: ledger::bindings::TaskAtTime,
}

impl EventTime {
    /// `source` timed at `at_ms` (UTC milliseconds), if it said, with the
    /// binding that held then from `task_at`.
    ///
    /// A time that is absent, or more than [`MAX_FUTURE_SKEW_MS`] ahead of
    /// `now_ms`, is not trusted: the row is stamped with ingest time and no
    /// task. It never falls back to the binding that holds now, which would
    /// attribute the work to whatever was bound when the pass happened to run.
    fn at(
        source: &'static str,
        at_ms: Option<u64>,
        now_ms: u64,
        task_at: impl FnOnce(u64) -> ledger::bindings::TaskAtTime,
    ) -> Self {
        match at_ms.filter(|ms| *ms <= now_ms.saturating_add(MAX_FUTURE_SKEW_MS)) {
            Some(ms) => EventTime {
                occurred_at: ledger::ids::iso8601_utc(ms),
                basis: source,
                task: task_at(ms),
            },
            None => EventTime {
                occurred_at: ledger::ids::iso8601_utc(now_ms),
                basis: "ingest",
                task: ledger::bindings::TaskAtTime::Unresolved,
            },
        }
    }

    /// Adds the three keys to a detail object.
    fn stamp(&self, detail: &mut serde_json::Value) {
        detail["occurred_at"] = self.occurred_at.clone().into();
        detail["occurred_at_basis"] = self.basis.into();
        detail["task_basis"] = self.task.basis().into();
    }
}

fn transcript_draft(
    call: &transcript::ToolCall,
    ledger_repo: &str,
    worktree_path: &str,
    history: &ledger::bindings::BindingHistory,
) -> Draft {
    let when = EventTime::at(
        "transcript",
        ledger::ids::parse_event_time(&call.ts_utc),
        ledger::ids::now_millis(),
        |ms| history.as_of(ms),
    );
    let mut detail = serde_json::json!({
        "source": "transcript",
        "tool": call.tool,
        "transcript_version": call.version,
        "git_branch": call.git_branch,
    });
    when.stamp(&mut detail);
    let detail = detail.to_string();
    Draft {
        repo_path: ledger_repo.to_string(),
        worktree_path: (ledger_repo != worktree_path).then(|| worktree_path.to_string()),
        action: call.action().to_string(),
        object: Some(call.object()),
        session_id: Some(call.session_id.clone()),
        task_id: when.task.task_id().map(str::to_string),
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
/// skipping files that should have been read. What "cannot be read" means is
/// [`ledger::ids::parse_event_time`]'s: the one strict parser.
fn iso_to_millis(iso: &str) -> u64 {
    ledger::ids::parse_event_time(iso).unwrap_or(0)
}

/// When `meta`'s file was last written, in epoch milliseconds; `None` when the
/// filesystem will not say.
fn modified_ms(meta: &std::fs::Metadata) -> Option<u64> {
    let since_epoch = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(since_epoch.as_millis() as u64)
}

/// Whether a file last written at `modified_ms` was written at or after
/// `since_ms`.
///
/// An unreadable mtime returns true, so the file is read. The skip is an
/// optimisation and must never be the reason something goes unattributed.
fn written_since(modified_ms: Option<u64>, since_ms: u64) -> bool {
    // A second of slack: filesystem timestamps and the transcript's own clock
    // are not the same clock, and losing an event to rounding is worse than
    // reading one file twice.
    modified_ms.is_none_or(|modified| modified + 1000 >= since_ms)
}

/// Where a corpus walk ended.
struct Walk {
    /// Whether the files listed are the whole corpus: false when a directory
    /// could not be read, the file cap cut the walk short, or it was stopped.
    /// Beyond the depth bound counts as complete — those files are never
    /// listed, so never have an offset to lose.
    complete: bool,
    /// Whether `stop` ended the walk early.
    cut: bool,
}

/// Walks `dir` for `.jsonl` files, bounded in depth.
///
/// The bound is not decoration: the transcript root is user-controlled, and a
/// symlink loop under it would otherwise hang repo open forever.
#[cfg(test)]
fn collect_jsonl(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>, depth: usize) -> bool {
    collect_jsonl_until(dir, out, depth, &mut || false).complete
}

/// [`collect_jsonl`] that asks `stop` before it reads each directory, so a deadline
/// bounds the walk instead of waiting out up to 5000 files.
fn collect_jsonl_until(
    dir: &std::path::Path,
    out: &mut Vec<std::path::PathBuf>,
    depth: usize,
    stop: &mut dyn FnMut() -> bool,
) -> Walk {
    const MAX_DEPTH: usize = 4;
    const MAX_FILES: usize = 5000;
    if depth > MAX_DEPTH {
        return Walk {
            complete: true,
            cut: false,
        };
    }
    if stop() {
        return Walk {
            complete: false,
            cut: true,
        };
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Walk {
            complete: false,
            cut: false,
        };
    };
    let mut walk = Walk {
        complete: true,
        cut: false,
    };
    for entry in entries {
        let Ok(entry) = entry else {
            walk.complete = false;
            continue;
        };
        let path = entry.path();
        if path.is_dir() {
            let inner = collect_jsonl_until(&path, out, depth + 1, stop);
            walk.complete &= inner.complete;
            if inner.cut {
                walk.cut = true;
                return walk;
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            if out.len() >= MAX_FILES {
                walk.complete = false;
                return walk;
            }
            out.push(path);
        }
    }
    walk
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
    ingest_reflog_within(ledger_repo, worktree_path, max_entries, None)
}

/// [`ingest_reflog_into`] with the reflog's Git child held to `timeout`.
fn ingest_reflog_within(
    ledger_repo: &str,
    worktree_path: &str,
    max_entries: usize,
    timeout: Option<std::time::Duration>,
) -> CatchUp {
    let mut out = CatchUp::default();
    let entries = match crate::engine::git_reader::GitReader::get_reflog_within(
        worktree_path,
        max_entries,
        timeout,
    ) {
        Ok(entries) => entries,
        Err(e) => {
            out.error = e;
            return out;
        }
    };

    // Already-recorded identities, so a second open adds nothing. Stored rows
    // hold the redacted spelling, so that is what each entry is compared as. A
    // failed read stops the replay: recording against an unknown "seen" set is
    // how the same history would be written twice.
    let seen =
        ledger::recorded_reflog_objects(ledger_repo, worktree_path, ledger_repo == worktree_path);
    replay_reflog(ledger_repo, worktree_path, entries, seen, out)
}

fn replay_reflog(
    ledger_repo: &str,
    worktree_path: &str,
    entries: Vec<crate::engine::git_reader::ReflogEntry>,
    seen: Result<std::collections::HashSet<String>, ledger::LedgerError>,
    mut out: CatchUp,
) -> CatchUp {
    let seen = match seen {
        Ok(seen) => seen,
        Err(e) => {
            out.error = format!("could not read the recorded reflog history: {e}");
            return out;
        }
    };

    // The task each entry carries is the one bound when the ref moved, so the
    // history is read once, not the binding that holds now. A history that
    // cannot be read decides nothing, and is said so.
    let history = match ledger::bindings::BindingHistory::load(ledger_repo, worktree_path) {
        Ok(history) => history,
        Err(e) => {
            out.error = format!("the task binding history could not be read: {e}");
            ledger::bindings::BindingHistory::unreadable()
        }
    };
    let now_ms = ledger::ids::now_millis();
    // Oldest first, so the ledger's order matches the order things happened.
    for entry in entries.iter().rev() {
        // The selector (`HEAD@{3}`) is positional and shifts as the reflog
        // grows, so identity is the commit plus the message.
        let identity = format!("{} {}", entry.commit_id, entry.message);
        if seen.contains(&ledger::redact::text(&identity)) {
            continue;
        }
        out.reflog_entries += 1;
        let action = format!(
            "reflog.{}",
            if entry.action.is_empty() {
                "move".to_string()
            } else {
                ledger::action_segment(&entry.action)
            }
        );
        // Git records the move to the second, so a bind or unbind inside that
        // second cannot be ordered against it.
        let when = EventTime::at(
            "reflog",
            u64::try_from(entry.timestamp)
                .ok()
                .filter(|seconds| *seconds > 0)
                .and_then(|seconds| seconds.checked_mul(1000)),
            now_ms,
            |ms| history.as_of_second(ms),
        );
        let mut detail = serde_json::json!({
            "source": "reflog",
            "selector": entry.selector,
            "message": entry.message,
        });
        when.stamp(&mut detail);
        let detail = detail.to_string();
        if ledger::record(Draft {
            repo_path: ledger_repo.to_string(),
            worktree_path: (ledger_repo != worktree_path).then(|| worktree_path.to_string()),
            task_id: when.task.task_id().map(str::to_string),
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

impl CatchUp {
    /// This reflog replay's result with the transcript pass's folded in.
    fn with_transcripts(mut self, transcripts: &CatchUp) -> CatchUp {
        self.recorded += transcripts.recorded;
        self.transcripts = transcripts.transcripts;
        self.transcripts_listed = transcripts.transcripts_listed;
        self.skipped_lines = transcripts.skipped_lines;
        self.rehosted_calls = transcripts.rehosted_calls;
        self.departed_overflow = transcripts.departed_overflow;
        self.family_unresolved = transcripts.family_unresolved;
        self.family_unresolved_cycles = transcripts.family_unresolved_cycles;
        self.truncated = self.truncated || transcripts.truncated;
        if self.error.is_empty() {
            self.error = if transcripts.truncated && transcripts.error.is_empty() {
                // Both numbers, so a cut pass cannot pass for a finished one.
                format!(
                    "catch-up truncated: deadline elapsed during transcripts \
                     ({} read, {} listed)",
                    transcripts.transcripts, transcripts.transcripts_listed
                )
            } else {
                transcripts.error.clone()
            };
        }
        self
    }
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
///
/// A budget bounds the call, not only the transcript loops: the pass runs on a
/// worker and this returns when it finishes or the budget elapses, whichever is
/// first, whatever a phase is stuck on (a hung Git, a ledger locked by another
/// process). A pass cut off this way reports `truncated` with what it had done;
/// the worker stops itself at its next check.
pub fn catch_up_into_bounded(
    ledger_repo: &str,
    worktree_path: &str,
    budget: Option<std::time::Duration>,
) -> CatchUp {
    let Some(budget) = budget else {
        return catch_up_with(
            ledger_repo,
            worktree_path,
            &Budget::unbounded(),
            &Progress::default(),
        );
    };
    let (ledger_repo, worktree_path) = (ledger_repo.to_string(), worktree_path.to_string());
    flight::bounded(budget, move |budget, progress| {
        catch_up_with(&ledger_repo, &worktree_path, budget, progress)
    })
}

/// The app's catch-up for `repo_path`: bounded like [`catch_up_into_bounded`],
/// and answered from the last pass when one finished inside the last minute
/// (ten seconds after one that was cut short), or joined when one is running.
///
/// The app asks on every repository change; the daemon already runs the same
/// pass on its own interval. The repository's address — a `git worktree list`
/// and the sibling-ledger consolidation — is resolved inside the budget too.
pub fn catch_up_coalesced(repo_path: &str) -> CatchUp {
    let repo = repo_path.to_string();
    flight::gate().run(repo_path, CATCH_UP_DEADLINE, move |budget, progress| {
        progress.enter(Phase::Resolving);
        match ledger::bindings::repository_address(&repo) {
            Ok(address) => catch_up_with(&address.anchor, &address.worktree, budget, progress),
            Err(error) => CatchUp {
                error: format!("the repository could not be resolved: {error}"),
                ..CatchUp::default()
            },
        }
    })
}

/// Passes in this process one repository's ledger is being caught up by, so
/// two never read the same history at once (the second would only find the
/// first's cursors already advanced, or worse, record a reflog entry twice).
fn serialize(ledger_repo: &str, budget: &Budget) -> Option<Serial> {
    loop {
        if running()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(ledger_repo.to_string())
        {
            return Some(Serial(ledger_repo.to_string()));
        }
        if budget.expired() {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn running() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static RUNNING: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    RUNNING.get_or_init(Default::default)
}

/// Holds one ledger's place in [`serialize`] until dropped.
struct Serial(String);

impl Drop for Serial {
    fn drop(&mut self) {
        running()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}

fn catch_up_with(
    ledger_repo: &str,
    worktree_path: &str,
    budget: &Budget,
    progress: &Progress,
) -> CatchUp {
    let Some(_serial) = serialize(ledger_repo, budget) else {
        return CatchUp {
            truncated: true,
            error: "catch-up truncated: deadline elapsed waiting for a pass already running".into(),
            ..CatchUp::default()
        };
    };
    progress.enter(Phase::Reflog);
    let mut total = ingest_reflog_within(ledger_repo, worktree_path, 200, budget.remaining());
    progress.publish(&total);
    if budget.expired() {
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

    progress.enter(Phase::Transcripts);
    let family = family_siblings(ledger_repo, worktree_path);
    let base = total.clone();
    let mut checks = 0u32;
    let mut expired = |out: &CatchUp| {
        // Published often enough to say what a stalled pass had done, not on
        // every line of a large transcript.
        if checks.is_multiple_of(256) {
            progress.publish(&base.clone().with_transcripts(out));
        }
        checks = checks.wrapping_add(1);
        budget.expired()
    };
    let streak_key = format!("{ledger_repo}\n{worktree_path}");
    let transcripts = match &family {
        Ok(siblings) => {
            unresolved_streak(&streak_key, false);
            pass::ingest_family_until(ledger_repo, worktree_path, siblings, &mut expired)
        }
        Err(reason) if is_git_checkout(worktree_path) => {
            // A checkout whose family cannot be listed may well have live
            // worktrees nested under it, and a pass for the trigger alone
            // would record their calls under it and move only its cursor: the
            // next pass that can see them would record the same calls again.
            // Nothing is written; the next cycle catches up.
            CatchUp {
                family_unresolved: true,
                family_unresolved_cycles: unresolved_streak(&streak_key, true),
                error: format!(
                    "the worktree family could not be resolved, so the transcript pass was \
                     skipped this cycle: {reason}"
                ),
                ..CatchUp::default()
            }
        }
        // Not a Git checkout: there is no family to resolve, and the worktree is
        // the whole of it.
        Err(_) => pass::ingest_unresolved_family_until(ledger_repo, worktree_path, &mut expired),
    };
    let mut total = total.with_transcripts(&transcripts);
    // Retention rides the pass every host already runs (the app on open, the
    // daemon on its interval), so the events table is bounded without a
    // second scheduler. A pass the deadline cut short spends no more time.
    if !total.truncated {
        progress.enter(Phase::Pruning);
        let deadline = budget
            .remaining()
            .map(|left| std::time::Instant::now() + left);
        let pruned = ledger::prune_expired(ledger_repo, deadline);
        total.pruned = i64::try_from(pruned.deleted).unwrap_or(i64::MAX);
        if !pruned.error.is_empty() {
            total.error = if total.error.is_empty() {
                pruned.error
            } else {
                format!("{}; {}", total.error, pruned.error)
            };
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::git_in;

    /// Runs `f` with the transcript root pointed at `root`, restoring it after.
    ///
    /// "After" includes the case where `f` panics: a `f()` that failed used to
    /// skip the restore and hand every later test in this process a transcript
    /// root pointing into a deleted `TempDir`.
    fn with_transcript_root<T>(root: &std::path::Path, f: impl FnOnce() -> T) -> T {
        let serial = crate::test_support::env::env_serial();
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
    fn a_reflog_message_carrying_a_secret_is_not_recorded_again_on_every_catch_up() {
        let main = tempfile::tempdir().expect("checkout");
        git_in(main.path(), &["init", "-b", "main"]);
        std::fs::write(main.path().join("seed.txt"), "seed").expect("seed");
        git_in(main.path(), &["add", "seed.txt"]);
        git_in(
            main.path(),
            &[
                "commit",
                "-m",
                "rotate ghp_0123456789abcdefghijklmnopqrstuvwxyzA",
            ],
        );
        crate::test_support::trust_repo(main.path());
        let repo = main.path().canonicalize().expect("canonical");
        let repo = repo.to_string_lossy();

        let first = ingest_reflog_into(&repo, &repo, 200);
        assert!(first.recorded > 0, "{first:?}");
        let second = ingest_reflog_into(&repo, &repo, 200);
        assert_eq!(
            second.recorded, 0,
            "the redacted stored identity was compared with the raw one"
        );
    }

    #[test]
    fn an_unreadable_recorded_history_records_nothing() {
        let main = tempfile::tempdir().expect("checkout");
        git_in(main.path(), &["init", "-b", "main"]);
        std::fs::write(main.path().join("seed.txt"), "seed").expect("seed");
        git_in(main.path(), &["add", "seed.txt"]);
        git_in(main.path(), &["commit", "-m", "seed"]);
        crate::test_support::trust_repo(main.path());
        let repo = main.path().canonicalize().expect("canonical");
        let repo = repo.to_string_lossy();
        let entries = crate::engine::git_reader::GitReader::get_reflog(&repo, 200).expect("reflog");
        assert!(!entries.is_empty());

        let out = replay_reflog(
            &repo,
            &repo,
            entries,
            Err(ledger::LedgerError::new("query_failed", "injected")),
            CatchUp::default(),
        );
        assert_eq!(out.recorded, 0);
        assert!(out.error.contains("injected"), "{out:?}");
        assert!(
            ledger::tail_readonly(&repo, 0, 10).expect("tail").is_none(),
            "a replay against an unknown history created or wrote the ledger"
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
            // Once the transcript is open (it counts as read), the checks are
            // before line 1 and before line 2 — so the pass stops with exactly
            // one line read. The checks the walk and the baseline make before
            // that are the budget's, not this test's.
            let mut checks = 0;
            let first = ingest_transcripts_until(repo, repo, &mut |out| {
                if out.transcripts < 1 {
                    return false;
                }
                checks += 1;
                checks > 1
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

    /// The skip decision for the file at `path`, as the pass makes it from the
    /// file's metadata.
    fn modified_since(path: &std::path::Path, since_ms: u64) -> bool {
        let modified = std::fs::metadata(path).ok().as_ref().and_then(modified_ms);
        written_since(modified, since_ms)
    }

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

#[cfg(test)]
mod event_time_tests;
#[cfg(test)]
mod family_tests;
