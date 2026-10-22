//! Catching up on what happened while GitPulse was not watching.
//!
//! Correctness never depends on residency. Two sources are replayed on repo
//! open, and both are things the system observed rather than things an agent
//! reported about itself:
//!
//! * **Agent transcripts** — Claude Code's `~/.claude/projects/**/*.jsonl`
//!   (parsed by [`transcript`]) and Codex's
//!   `~/.codex/sessions/**/rollout-*.jsonl` (parsed by [`codex`]), each a
//!   [`source::TranscriptSource`]. Measured over the real Claude corpus, 97.3%
//!   of mutating tool calls in repositories that still exist attribute to a
//!   repository path.
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

mod codex;
mod flight;
mod lines;
mod pass;
mod source;
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
    /// Transcript files the corpus walk saw and did not list: `unlisted_depth`
    /// below the depth bound plus `unlisted_cap` past the file cap. They were
    /// not read, so a non-zero count means the history is not whole; it sets
    /// `truncated` and `error` names the reasons. Never offsets forgotten.
    #[serde(default)]
    pub transcripts_unlisted: i64,
    /// Of `transcripts_unlisted`, the files in directories below the walk's
    /// depth bound.
    #[serde(default)]
    pub unlisted_depth: i64,
    /// Of `transcripts_unlisted`, the files past the walk's file cap.
    #[serde(default)]
    pub unlisted_cap: i64,
    /// True when the walk stopped counting before it had seen everything it
    /// left out: `transcripts_unlisted` is then a floor.
    #[serde(default)]
    pub unlisted_lower_bound: bool,
    /// Symlinks the walk refused because their target is outside the transcript
    /// root. Not unread transcripts of the corpus, so not in
    /// `transcripts_unlisted`.
    #[serde(default)]
    pub refused_outside_corpus: i64,
    /// Whether the corpus walk listed the whole corpus: null when no walk ran
    /// (no transcript root, or the pass was skipped), false when it was cut,
    /// hit an unreadable directory, or left files unlisted.
    #[serde(default)]
    pub walk_complete: Option<bool>,
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
    /// The earlier locations of this repository, as the ledger's rows named
    /// them, that this pass took over because the repository was moved or
    /// renamed. Non-empty on exactly the one pass that did it (the take-over is
    /// also a durable `ledger.reanchor` row), so a moved repository is reported
    /// once and never silently. Not an error: the pass went on.
    #[serde(default)]
    pub reanchored_from: Vec<String>,
    /// Reflog entries replayed.
    pub reflog_entries: i64,
    /// Replayed entries the ledger refused because it already held them (the
    /// unique reflog identity index): another pass or process recorded them
    /// first. `reflog_entries` is these plus the entries recorded plus any that
    /// failed to write, so a shortfall is always accounted for.
    #[serde(default)]
    pub duplicates_refused: i64,
    /// Reflog entries whose ledger write failed (counted by the ledger's dropped
    /// total too). The pass stops at the first, so everything after it in that
    /// ref is `reflog_pending`; the next pass retries from the same place.
    /// Non-zero is never a complete pass.
    #[serde(default)]
    pub failed_records: i64,
    /// Reflog entries the pass knew of and did not record: more than a window
    /// newer than the watermark, or left behind a failed write. They are
    /// recorded, oldest first, by the next passes; non-zero sets `truncated`.
    #[serde(default)]
    pub reflog_pending: i64,
    /// True when a ref's log reaches past what the pass read and its watermark
    /// was not in the read (a first pass over a long log, or more than the depth
    /// bound of new entries): older entries were not examined. Sets `truncated`.
    #[serde(default)]
    pub reflog_unreached: bool,
    /// Reflogs (worktree HEADs, the default branch) the pass did not read: the
    /// per-pass bound on worktrees, a deadline, or an unreadable default branch.
    /// Non-zero sets `truncated`.
    #[serde(default)]
    pub reflog_refs_skipped: i64,
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

/// The `CatchUp::error` of a pass with no home directory to look for
/// transcripts in. There is nothing to read, which is not a failure to read:
/// callers that count failures leave it out, and callers that show errors keep it.
pub(crate) const NO_TRANSCRIPT_ROOT: &str = "no home directory, so transcripts cannot be located";

/// Where each transcript source looks, by name, for the hermetic canary that
/// checks every per-user directory resolves under the test process's own root.
#[cfg(test)]
pub(crate) fn source_roots() -> Vec<(&'static str, Option<std::path::PathBuf>)> {
    source::sources()
        .iter()
        .map(|source| (source.name(), source.root().map(|root| root.path)))
        .collect()
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

    /// The three keys, for the writer to place in `detail_json`. They are
    /// built here from a formatter and two fixed vocabularies, and the writer
    /// checks that before it lets them past the redactor.
    fn stamp(&self) -> ledger::EventStamp {
        ledger::EventStamp {
            occurred_at: self.occurred_at.clone(),
            occurred_at_basis: self.basis.to_string(),
            task_basis: self.task.basis().to_string(),
        }
    }
}

fn transcript_draft(
    call: &transcript::ToolCall,
    actor_id: &str,
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
        task_id: when.task.task_id().map(str::to_string),
        // Derived from observation, not self-reported: this row exists because
        // a transcript recorded the call, not because an agent announced it.
        actor_kind: Some(ActorKind::Agent),
        actor_id: Some(actor_id.into()),
        outcome: Some(Outcome::Ok),
        // No verdict: GitPulse's gate never saw this action. That is
        // emphatically not the same as an action that passed, and the absence
        // is what says so.
        verdict_json: None,
        detail_json: Some(detail),
        event_stamp: Some(when.stamp()),
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

/// Directories the corpus walk reads below its root. Five is what the deepest
/// shape Claude Code writes needs: `slug/session/subagents/workflows/wf_*/` is
/// five directories under the root, and the `agent-*.jsonl` files sit in the
/// last. Deeper directories are not read for listing; they are counted.
const MAX_WALK_DEPTH: usize = 5;

/// Transcript files one walk lists. Past it files are counted, not listed.
const MAX_WALK_FILES: usize = 5000;

/// Directory entries the walk reads, after the file cap or the depth bound has
/// stopped it listing, to count what it leaves out. Bounds a hostile or huge
/// tree; past it the unlisted counts are a floor (`unlisted_lower_bound`).
const MAX_UNLISTED_ENTRIES: usize = 100_000;

/// Where a corpus walk ended.
#[derive(Debug, Default)]
pub(super) struct Walk {
    /// Whether the files listed are the whole corpus: false when a directory
    /// could not be read, the walk was stopped, or files were seen and left
    /// unlisted (the file cap, or directories below the depth bound that hold
    /// transcripts). Only a walk that is complete may forget a transcript's
    /// offset: a file missing from a partial listing may still exist.
    pub complete: bool,
    /// Whether `stop` ended the walk early.
    pub cut: bool,
    /// Transcript files seen below the depth bound and not listed.
    pub unlisted_depth: i64,
    /// Transcript files seen past the file cap and not listed.
    pub unlisted_cap: i64,
    /// True when the walk stopped counting (a bound on entries read) before it
    /// had seen everything it left out: the unlisted counts are a floor.
    pub unlisted_lower_bound: bool,
    /// Symlinks whose target is outside the corpus root, refused: what is
    /// listed is read and recorded, so a link out of the corpus is never
    /// followed. Not part of the corpus, so they do not make the walk
    /// incomplete.
    pub refused_outside_corpus: i64,
}

impl Walk {
    /// What walking nothing found: complete, until a walk says otherwise.
    pub(super) fn none() -> Self {
        Walk {
            complete: true,
            ..Walk::default()
        }
    }

    /// Folds in another root's walk. The caps are per root (each walk has its
    /// own file cap and entry budget), so the counts add; the result is
    /// complete only if every walk was.
    pub(super) fn absorb(&mut self, other: &Walk) {
        self.complete &= other.complete;
        self.cut |= other.cut;
        self.unlisted_depth = self.unlisted_depth.saturating_add(other.unlisted_depth);
        self.unlisted_cap = self.unlisted_cap.saturating_add(other.unlisted_cap);
        self.unlisted_lower_bound |= other.unlisted_lower_bound;
        self.refused_outside_corpus = self
            .refused_outside_corpus
            .saturating_add(other.refused_outside_corpus);
    }
}

/// The identity of a directory, so a walk can tell it has been in one before:
/// a symlink pointing at an ancestor, or two links to one directory, would
/// otherwise be walked as many times as the depth bound lets them.
type DirId = (u64, u64);

#[cfg(unix)]
fn dir_id(meta: &std::fs::Metadata, _path: &std::path::Path) -> Option<DirId> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

/// Without device and inode numbers the canonical path stands in for them.
#[cfg(not(unix))]
fn dir_id(_meta: &std::fs::Metadata, path: &std::path::Path) -> Option<DirId> {
    use std::hash::{Hash, Hasher};
    let canonical = std::fs::canonicalize(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut hasher);
    Some((0, hasher.finish()))
}

/// A directory waiting to be read, with whether it is listed or only counted.
struct Pending {
    dir: std::path::PathBuf,
    depth: usize,
    listing: bool,
}

struct WalkState {
    visited: std::collections::HashSet<DirId>,
    /// Files already listed, by identity: a hard link is the same transcript
    /// under a second name, and would be read and recorded twice.
    seen_files: std::collections::HashSet<DirId>,
    counting_budget: usize,
    incomplete: bool,
    halted: bool,
    walk: Walk,
}

/// Walks `dir` for `.jsonl` files, bounded in depth and count.
///
/// The bounds are not decoration: the transcript root is user-controlled, and a
/// symlink loop under it would otherwise hang repo open forever. What the
/// bounds leave out is counted, never dropped silently.
#[cfg(test)]
fn collect_jsonl(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>, depth: usize) -> bool {
    collect_jsonl_until(dir, out, depth, &mut || false).complete
}

/// [`collect_jsonl`] that asks `stop` before it reads each directory, so a deadline
/// bounds the walk instead of waiting out up to 5000 files.
///
/// `depth` is the depth of `dir` below the root (0 for the root itself).
///
/// Directories are read shallowest first, so a directory reachable by several
/// names is walked by the shallowest and the result does not depend on the
/// order the filesystem lists entries in. Nothing outside `dir` is read: a
/// symlink whose target is outside it is refused and counted
/// (`refused_outside_corpus`), because what is listed is read and recorded.
pub(super) fn collect_jsonl_until(
    dir: &std::path::Path,
    out: &mut Vec<std::path::PathBuf>,
    depth: usize,
    stop: &mut dyn FnMut() -> bool,
) -> Walk {
    let mut state = WalkState {
        visited: std::collections::HashSet::new(),
        seen_files: std::collections::HashSet::new(),
        counting_budget: MAX_UNLISTED_ENTRIES,
        incomplete: false,
        halted: false,
        walk: Walk::default(),
    };
    match std::fs::canonicalize(dir) {
        Ok(corpus) => {
            let mut queue = std::collections::VecDeque::new();
            queue.push_back(Pending {
                dir: dir.to_path_buf(),
                depth,
                listing: depth <= MAX_WALK_DEPTH,
            });
            while let Some(next) = queue.pop_front() {
                if state.halted {
                    break;
                }
                visit_dir(&next, &corpus, out, stop, &mut state, &mut queue);
            }
        }
        // A root that cannot be resolved cannot be confined: nothing is read.
        Err(_) => state.incomplete = true,
    }
    let walk = &mut state.walk;
    walk.complete = !state.incomplete
        && !walk.cut
        && walk.unlisted_depth == 0
        && walk.unlisted_cap == 0
        && !walk.unlisted_lower_bound;
    state.walk
}

/// Reads one directory, queueing its subdirectories. Any entry whose type or
/// target cannot be determined, and any directory that cannot be read, marks
/// the walk incomplete: it is never read as "nothing here".
fn visit_dir(
    pending: &Pending,
    corpus: &std::path::Path,
    out: &mut Vec<std::path::PathBuf>,
    stop: &mut dyn FnMut() -> bool,
    state: &mut WalkState,
    queue: &mut std::collections::VecDeque<Pending>,
) {
    let dir = pending.dir.as_path();
    match std::fs::metadata(dir) {
        Ok(meta) => match dir_id(&meta, dir) {
            Some(id) => {
                if !state.visited.insert(id) {
                    // Already walked, through another name for it: its files
                    // are listed (or counted) once, by the shallowest name.
                    return;
                }
            }
            None => {
                state.incomplete = true;
                return;
            }
        },
        Err(_) => {
            state.incomplete = true;
            return;
        }
    }
    if stop() {
        state.walk.cut = true;
        state.halted = true;
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        state.incomplete = true;
        return;
    };
    for entry in entries {
        if state.halted {
            return;
        }
        let Ok(entry) = entry else {
            state.incomplete = true;
            continue;
        };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            state.incomplete = true;
            continue;
        };
        let (is_dir, is_file, linked) = if file_type.is_symlink() {
            match std::fs::metadata(&path) {
                Ok(target) => (target.is_dir(), target.is_file(), true),
                // Dangling: not a directory. A `.jsonl` one is still listed,
                // so the pass counts the file that vanished.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (false, true, true),
                Err(_) => {
                    state.incomplete = true;
                    continue;
                }
            }
        } else {
            (file_type.is_dir(), file_type.is_file(), false)
        };
        let is_transcript = is_file && path.extension().and_then(|e| e.to_str()) == Some("jsonl");
        if !is_dir && !is_transcript {
            continue;
        }
        if linked {
            match std::fs::canonicalize(&path) {
                Ok(real) if !real.starts_with(corpus) => {
                    state.walk.refused_outside_corpus += 1;
                    continue;
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    state.incomplete = true;
                    continue;
                }
            }
        }
        let capped = out.len() >= MAX_WALK_FILES;
        if !pending.listing || capped {
            if state.counting_budget == 0 {
                state.walk.unlisted_lower_bound = true;
                state.halted = true;
                return;
            }
            state.counting_budget -= 1;
        }
        if is_dir {
            let depth = pending.depth + 1;
            queue.push_back(Pending {
                dir: path,
                depth,
                listing: pending.listing && depth <= MAX_WALK_DEPTH,
            });
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&path) {
            if let Some(id) = dir_id(&meta, &path) {
                if !state.seen_files.insert(id) {
                    continue;
                }
            }
        }
        if !pending.listing {
            state.walk.unlisted_depth += 1;
        } else if capped {
            state.walk.unlisted_cap += 1;
        } else {
            out.push(path);
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
    ingest_reflog_into(repo_path, repo_path, max_entries)
}

/// Reflog entries one ref records per pass, and the window it reads to find
/// them. More than this newer than the watermark is not lost: the pass records
/// the oldest of them, advances the watermark to the last, and says
/// `truncated` with the count it left for the next pass.
const REFLOG_WINDOW: usize = 200;

/// How deep a pass reads for its watermark when the window does not hold it
/// (more than [`REFLOG_WINDOW`] entries since the last pass). Bounded: past it
/// the ref reports `reflog_unreached` instead of reading an unbounded log.
const REFLOG_DEPTH: usize = 2000;

/// How far before the watermark's entry, in seconds, a legacy (untimed) row may
/// have been appended and still be read as already holding a later entry: a
/// generation-1 process writes a row when it sees the move, so well within a
/// clock step of the move itself.
const LEGACY_SEEN_MARGIN_SECS: i64 = 86_400;

/// Worktree HEAD reflogs one pass reads. A family past this reports how many
/// it left (`reflog_refs_skipped`) and is `truncated`, never complete.
const MAX_REFLOG_WORKTREES: usize = 16;

/// Replays one checkout's HEAD reflog into its repository-wide ledger.
pub(crate) fn ingest_reflog_into(
    ledger_repo: &str,
    worktree_path: &str,
    max_entries: usize,
) -> CatchUp {
    replay_ref(ledger_repo, worktree_path, "HEAD", max_entries, None)
}

/// Replays the HEAD reflog of every worktree in the family `worktree_path`
/// belongs to, and the repository's default branch once, into the shared
/// ledger. `family` is [`family_siblings`]'s answer: unresolved, only the
/// trigger's own HEAD is read (the default branch waits for a resolved family).
///
/// Spawns at most `max_worktrees + 1` reflog children (two more per ref that
/// had to look past its window), plus the default-base lookup the rest of the
/// engine already caches.
fn ingest_reflogs(
    ledger_repo: &str,
    worktree_path: &str,
    family: &Result<Vec<String>, String>,
    max_worktrees: usize,
    budget: &Budget,
) -> CatchUp {
    let mut total = CatchUp::default();
    let mut worktrees = vec![worktree_path.to_string()];
    if let Ok(siblings) = family {
        worktrees.extend(siblings.iter().cloned());
    }
    // A fixed order, so a rotation means the same thing whichever worktree
    // triggers the pass.
    worktrees.sort();
    worktrees.dedup();
    let max_worktrees = max_worktrees.max(1);
    if worktrees.len() > max_worktrees {
        // The bound is on one pass, not on the family: the window moves on each
        // pass, so every member's HEAD is read within ceil(N / bound) passes.
        let start = next_rotation_start(ledger_repo, worktrees.len(), max_worktrees);
        worktrees.rotate_left(start);
    }
    let skipped = worktrees.len().saturating_sub(max_worktrees);
    worktrees.truncate(max_worktrees);
    total.reflog_refs_skipped = i64::try_from(skipped).unwrap_or(i64::MAX);

    let mut refs: Vec<(String, String)> = worktrees
        .into_iter()
        .map(|worktree| (worktree, "HEAD".to_string()))
        .collect();
    if family.is_ok() {
        // The default branch's own reflog holds what no HEAD log does: a
        // fast-forward made from another worktree moves it without touching
        // this one's HEAD.
        let base = crate::engine::git_reader::default_base(std::path::Path::new(ledger_repo));
        match base.checked() {
            Ok(base) => {
                if let Some((short, _)) = base.base {
                    refs.push((ledger_repo.to_string(), format!("refs/heads/{short}")));
                }
            }
            Err(reason) => {
                total.reflog_refs_skipped += 1;
                total.absorb_reflog(CatchUp {
                    error: format!("the default branch could not be read: {reason}"),
                    ..CatchUp::default()
                });
            }
        }
    }
    for (index, (worktree, ref_name)) in refs.iter().enumerate() {
        if budget.expired() {
            total.reflog_refs_skipped += i64::try_from(refs.len() - index).unwrap_or(i64::MAX);
            break;
        }
        let one = replay_ref(
            ledger_repo,
            worktree,
            ref_name,
            REFLOG_WINDOW,
            budget.remaining(),
        );
        total.absorb_reflog(one);
    }
    if total.reflog_refs_skipped > 0 {
        total.mark_reflog_incomplete(format!(
            "{} reflog(s) not read this pass",
            total.reflog_refs_skipped
        ));
    }
    total
}

/// Where this pass's window over a family of `members` starts, and the next
/// pass's after it: `window` further on, kept in memory per repository. A
/// process that restarts begins again at 0, which costs one pass's order, not
/// coverage.
fn next_rotation_start(ledger_repo: &str, members: usize, window: usize) -> usize {
    static STARTS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, usize>>> =
        std::sync::OnceLock::new();
    let mut starts = STARTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if starts.len() >= 64 && !starts.contains_key(ledger_repo) {
        starts.clear();
    }
    let next = starts.entry(ledger_repo.to_string()).or_insert(0);
    let start = *next % members;
    *next = (start + window) % members;
    start
}

/// One reflog entry laid out for replay: where it sits among identical
/// entries, and the commit the ref held before it.
struct Step<'a> {
    entry: &'a crate::engine::git_reader::ReflogEntry,
    ordinal: i64,
    before: Option<&'a str>,
}

impl Step<'_> {
    /// `"{commit} {message}"`: the selector (`HEAD@{3}`) is positional and
    /// shifts as the reflog grows, so identity is the commit plus the message.
    fn identity(&self) -> String {
        format!("{} {}", self.entry.commit_id, self.entry.message)
    }

    fn watermark(&self) -> ledger::ReflogWatermark {
        ledger::ReflogWatermark {
            unix: self.entry.timestamp,
            oid: self.entry.commit_id.clone(),
            msg_hash: ledger::reflog_message_hash(&self.identity()),
            ordinal: self.ordinal,
        }
    }
}

/// A reflog read (newest first, at most `limit + 1` entries) laid out oldest
/// first with each entry's ordinal among identical ones.
///
/// Identical entries of one second (a script's `git reset` twice) are told
/// apart by their occurrence index, 0, 1, 2, in the reflog's own order, so two
/// processes replaying the same log number the same entries the same way and
/// the ledger's unique identity index refuses the second process's copies.
///
/// An occurrence index is only absolute when every identical entry is in the
/// read. The read asks for one entry more than `limit`: that extra is kept as
/// the before-image of the oldest entry, and when it shares a second with the
/// entries just after it, the group may continue past what was read, so that
/// run is left unrecorded (`start` skips it) rather than numbered wrongly.
struct Laid<'a> {
    /// Oldest first; with `capped`, `steps[0]` is the extra entry.
    steps: Vec<Step<'a>>,
    /// The read hit its limit: older entries exist.
    capped: bool,
    /// First index that may be recorded.
    start: usize,
}

impl<'a> Laid<'a> {
    fn new(entries: &'a [crate::engine::git_reader::ReflogEntry], limit: usize) -> Self {
        let capped = entries.len() > limit;
        let mut occurrences: std::collections::HashMap<(String, i64), i64> =
            std::collections::HashMap::new();
        let mut steps: Vec<Step<'a>> = Vec::with_capacity(entries.len());
        for entry in entries.iter().rev() {
            let identity = format!("{} {}", entry.commit_id, entry.message);
            let occurrence = occurrences.entry((identity, entry.timestamp)).or_insert(0);
            let before = steps.last().map(|older| older.entry.commit_id.as_str());
            steps.push(Step {
                entry,
                ordinal: *occurrence,
                before,
            });
            *occurrence += 1;
        }
        let mut start = usize::from(capped);
        if let Some(extra) = steps.first().filter(|_| capped) {
            let second = extra.entry.timestamp;
            while steps
                .get(start)
                .is_some_and(|s| s.entry.timestamp == second)
            {
                start += 1;
            }
        }
        Laid {
            steps,
            capped,
            start,
        }
    }

    /// Where `mark` sits among the recordable steps.
    fn position_of(&self, mark: &ledger::ReflogWatermark) -> Option<usize> {
        (self.start..self.steps.len()).rev().find(|&i| {
            let step = &self.steps[i];
            step.entry.timestamp == mark.unix
                && step.ordinal == mark.ordinal
                && step.entry.commit_id == mark.oid
                && ledger::reflog_message_hash(&step.identity()) == mark.msg_hash
        })
    }
}

/// Replays one ref's reflog (`HEAD` of the checkout at `worktree_path`, or a
/// full ref such as `refs/heads/main` read from it) into the ledger at
/// `ledger_repo`, from where the last pass stopped.
///
/// The watermark (`reflog_watermarks`) is the last entry recorded, by selector
/// time, commit, message hash and ordinal. Everything newer is recorded
/// oldest first, and the watermark advances only over entries that are in the
/// ledger, so a pass that ends anywhere (a failed write, a deadline, a killed
/// process) leaves nothing unrecorded behind it. The unique reflog identity
/// index is what makes a re-read harmless, so the watermark is a bound on the
/// work, never the thing exactly-once rests on.
///
/// More than `window` entries newer than the watermark are recorded `window`
/// at a time, oldest first, and counted in `reflog_pending` with `truncated`.
fn replay_ref(
    ledger_repo: &str,
    worktree_path: &str,
    ref_name: &str,
    window: usize,
    timeout: Option<std::time::Duration>,
) -> CatchUp {
    let mut out = CatchUp::default();
    let mark = match ledger::reflog_watermark(ledger_repo, worktree_path, ref_name) {
        Ok(mark) => mark,
        Err(e) => {
            out.error = format!("could not read the reflog watermark for {ref_name}: {e}");
            return out;
        }
    };
    let read = |limit: usize| {
        crate::engine::git_reader::GitReader::get_reflog_within(
            worktree_path,
            ref_name,
            limit + 1,
            timeout,
        )
    };
    let mut limit = window;
    let mut entries = match read(limit) {
        Ok(entries) => entries,
        Err(e) => {
            out.error = e;
            return out;
        }
    };
    let mut laid = Laid::new(&entries, limit);
    // The watermark is not in the window: more than a window of entries came
    // since it. Look once, bounded, deeper for it.
    if mark
        .as_ref()
        .is_some_and(|mark| laid.capped && laid.position_of(mark).is_none())
        && limit < REFLOG_DEPTH
    {
        limit = REFLOG_DEPTH;
        entries = match read(limit) {
            Ok(entries) => entries,
            Err(e) => {
                out.error = e;
                return out;
            }
        };
        laid = Laid::new(&entries, limit);
    }
    let found = mark.as_ref().and_then(|mark| laid.position_of(mark));
    let first = found.map_or(laid.start, |position| position + 1);
    // A capped read that did not reach its watermark (or had none) cannot say
    // what lies before it.
    let unreached = laid.capped && found.is_none();
    let recordable = &laid.steps[first.min(laid.steps.len())..];
    let take = recordable.len().min(window.max(1));
    let steps = &recordable[..take];
    // Entries the read held but this pass will not record: past the per-pass
    // bound, or the oldest second a capped read cut in two (when that second
    // is newer than the watermark, which only a missing watermark leaves).
    let mut pending = recordable.len() - take;
    if found.is_none() {
        pending += laid.start - usize::from(laid.capped);
    }

    // Entries an old build recorded without an event time cannot be matched by
    // the unique index, so replay skips an identity one of them holds. A
    // generation-1 process that was running when the ledger migrated keeps
    // writing such rows, so this is read on every pass: all of them when the
    // watermark did not place the read, else the ones appended since the
    // watermark's entry (less a margin for clock skew), which bounds the query
    // to recent appends.
    let is_anchor = ledger_repo == worktree_path;
    let seen = match (&mark, found) {
        (Some(mark), Some(_)) => ledger::recorded_reflog_objects_since(
            ledger_repo,
            worktree_path,
            is_anchor,
            mark.unix.saturating_sub(LEGACY_SEEN_MARGIN_SECS),
        ),
        _ => ledger::recorded_reflog_objects(ledger_repo, worktree_path, is_anchor),
    };
    // The rows and the watermark are one transaction (`record_steps`).
    let (settled, mut out) = record_steps(ledger_repo, worktree_path, ref_name, steps, seen, out);
    pending += steps.len() - settled;
    out.reflog_pending = i64::try_from(pending).unwrap_or(i64::MAX);
    out.reflog_unreached = unreached;
    if out.failed_records > 0 {
        out.mark_reflog_incomplete(format!(
            "{} reflog entr(ies) of {ref_name} could not be written",
            out.failed_records
        ));
    }
    if pending > 0 || unreached {
        out.mark_reflog_incomplete(format!(
            "{ref_name}: {pending} entr(ies) not yet recorded{}",
            if unreached {
                ", older entries beyond the read were not examined"
            } else {
                ""
            }
        ));
    }
    out
}

/// A complete reflog, newest first, replayed as one pass with no watermark:
/// what the unit tests of the recording rules feed.
#[cfg(test)]
fn replay_reflog(
    ledger_repo: &str,
    worktree_path: &str,
    entries: Vec<crate::engine::git_reader::ReflogEntry>,
    seen: Result<std::collections::HashSet<String>, ledger::LedgerError>,
    out: CatchUp,
) -> CatchUp {
    let laid = Laid::new(&entries, entries.len());
    record_steps(ledger_repo, worktree_path, "HEAD", &laid.steps, seen, out).1
}

/// Records `steps` (oldest first) and returns how many leading ones are settled
/// (in the ledger already or now) with the pass's tallies. Stops at the first
/// write that fails, so the settled prefix is contiguous: a watermark moved to
/// its end leaves no gap behind it.
fn record_steps(
    ledger_repo: &str,
    worktree_path: &str,
    ref_name: &str,
    steps: &[Step<'_>],
    seen: Result<std::collections::HashSet<String>, ledger::LedgerError>,
    mut out: CatchUp,
) -> (usize, CatchUp) {
    let seen = match seen {
        Ok(seen) => seen,
        Err(e) => {
            out.error = format!("could not read the recorded reflog history: {e}");
            return (0, out);
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
    let mut rows = Vec::with_capacity(steps.len());
    for step in steps {
        let entry = step.entry;
        let identity = step.identity();
        // Compared as stored: redacted, and cut to the ledger's `object` cap,
        // or an identity past the cap would never match its own row.
        if seen.contains(&ledger::stored_object(&identity)) {
            rows.push(ledger::ReflogRow {
                draft: None,
                ordinal: step.ordinal,
                mark: step.watermark(),
                untimed: false,
            });
            continue;
        }
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
        let detail = serde_json::json!({
            "source": "reflog",
            "ref": ref_name,
            "selector": entry.selector,
            "message": entry.message,
        })
        .to_string();
        let untimed = when.basis == "ingest";
        let draft = Draft {
            repo_path: ledger_repo.to_string(),
            worktree_path: (ledger_repo != worktree_path).then(|| worktree_path.to_string()),
            task_id: when.task.task_id().map(str::to_string),
            action,
            object: Some(identity),
            // What the ref held before this move: the commit the next
            // older entry moved it to. Absent for the first entry of a
            // log, which has no earlier value to name.
            before_ref: step.before.map(str::to_string),
            after_ref: Some(entry.commit_id.clone()),
            // Git does not record which actor moved the ref, and inventing one
            // would be a guess written to disk. `system` says GitPulse
            // synthesised this row from git's record.
            actor_kind: Some(ActorKind::System),
            actor_id: Some("reflog".into()),
            outcome: Some(Outcome::Ok),
            verdict_json: None,
            detail_json: Some(detail),
            event_stamp: Some(when.stamp()),
            ..Default::default()
        };
        rows.push(ledger::ReflogRow {
            draft: Some(draft),
            ordinal: step.ordinal,
            mark: step.watermark(),
            untimed,
        });
    }
    let attempted = rows.iter().filter(|row| row.draft.is_some()).count();
    // One transaction for the rows and the watermark over them: a kill leaves
    // both or neither, never rows past the mark.
    match ledger::record_reflog_batch(ledger_repo, worktree_path, ref_name, rows) {
        Ok(outcomes) => {
            let mut settled = 0;
            for outcome in outcomes {
                match outcome {
                    None => {}
                    Some(ledger::Recorded::Appended(_)) => {
                        out.reflog_entries += 1;
                        out.recorded += 1;
                    }
                    Some(ledger::Recorded::Duplicate) => {
                        out.reflog_entries += 1;
                        out.duplicates_refused += 1;
                    }
                    Some(ledger::Recorded::Failed) => {
                        // Surfaced, and the pass stops here: the watermark never
                        // passes an entry that is not in the ledger.
                        out.reflog_entries += 1;
                        out.failed_records += 1;
                        return (settled, out);
                    }
                }
                settled += 1;
            }
            (settled, out)
        }
        Err(e) => {
            out.reflog_entries += i64::try_from(attempted).unwrap_or(i64::MAX);
            out.failed_records += i64::try_from(attempted.max(1)).unwrap_or(i64::MAX);
            out.error = format!("the reflog entries of {ref_name} could not be written: {e}");
            (0, out)
        }
    }
}

impl CatchUp {
    /// Folds another ref's reflog replay into this pass's total.
    fn absorb_reflog(&mut self, other: CatchUp) {
        self.recorded += other.recorded;
        self.reflog_entries += other.reflog_entries;
        self.duplicates_refused += other.duplicates_refused;
        self.failed_records += other.failed_records;
        self.reflog_pending += other.reflog_pending;
        self.reflog_refs_skipped += other.reflog_refs_skipped;
        self.reflog_unreached |= other.reflog_unreached;
        self.truncated |= other.truncated;
        if !other.error.is_empty() {
            self.error = if self.error.is_empty() {
                other.error
            } else {
                format!("{}; {}", self.error, other.error)
            };
        }
    }

    /// Marks a reflog pass that did not examine or record everything it knew
    /// of: `truncated`, with `reason` in `error` beside the counts. A
    /// shortfall never reads as a complete pass.
    fn mark_reflog_incomplete(&mut self, reason: String) {
        self.truncated = true;
        let note = format!("catch-up truncated: {reason}");
        self.error = if self.error.is_empty() {
            note
        } else {
            format!("{}; {note}", self.error)
        };
    }

    /// This reflog replay's result with the transcript pass's folded in.
    fn with_transcripts(mut self, transcripts: &CatchUp) -> CatchUp {
        self.recorded += transcripts.recorded;
        self.transcripts = transcripts.transcripts;
        self.transcripts_listed = transcripts.transcripts_listed;
        self.transcripts_unlisted = transcripts.transcripts_unlisted;
        self.unlisted_depth = transcripts.unlisted_depth;
        self.unlisted_cap = transcripts.unlisted_cap;
        self.unlisted_lower_bound = transcripts.unlisted_lower_bound;
        self.refused_outside_corpus = transcripts.refused_outside_corpus;
        self.walk_complete = transcripts.walk_complete;
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

impl CatchUp {
    /// True when the only thing that cut this pass short is transcripts the
    /// walk left unlisted: every other part of it finished (no deadline, no
    /// failed write, no reflog backlog, no other error). The unlisted counts
    /// say what it left; it is a completed pass over what it could list.
    pub fn is_unlisted_only_truncation(&self) -> bool {
        (self.transcripts_unlisted > 0 || self.unlisted_lower_bound)
            && self.truncated
            && self.failed_records == 0
            && self.reflog_pending == 0
            && !self.reflog_unreached
            && self.reflog_refs_skipped == 0
            && !self.family_unresolved
            && self
                .error
                .split("; ")
                .all(|part| part.starts_with("catch-up truncated: the transcript walk left"))
    }

    /// Whether this pass finished the work it set out to do, for the ledger's
    /// freshness: a pass cut by its deadline, one that could not run, or one
    /// with a failed write has been attempted, not completed. Unlisted
    /// transcripts alone do not undo a completion (see
    /// [`CatchUp::is_unlisted_only_truncation`]).
    pub fn finished(&self) -> bool {
        if self.family_unresolved || self.failed_records > 0 {
            return false;
        }
        if self.truncated {
            return self.is_unlisted_only_truncation();
        }
        self.error.is_empty()
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
    // Resolved once: the reflogs of every worktree and the transcript pass
    // below both need the family.
    let family = family_siblings(ledger_repo, worktree_path);
    let mut total = ingest_reflogs(
        ledger_repo,
        worktree_path,
        &family,
        MAX_REFLOG_WORKTREES,
        budget,
    );
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
        persist_pass(ledger_repo, worktree_path, &mut total);
        return total;
    }

    progress.enter(Phase::Transcripts);
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
    persist_pass(ledger_repo, worktree_path, &mut total);
    total
}

/// Leaves this pass's outcome in the ledger (`catch_up_state`, per worktree),
/// so a read-only reader in another process can say when a catch-up last ran
/// and what it covered.
///
/// A pass that did not run (the family could not be resolved, there was no
/// transcript root, the ledger's identity could not be confirmed) is recorded
/// with its counts and error but is not a completion: `completed_at` keeps its
/// last value. A failure to write the state is added to the pass's own error
/// rather than swallowed.
fn persist_pass(ledger_repo: &str, worktree_path: &str, total: &mut CatchUp) {
    let record = ledger::CatchUpRecord {
        completed: total.finished(),
        truncated: total.truncated,
        recorded: total.recorded,
        transcripts: total.transcripts,
        transcripts_listed: total.transcripts_listed,
        transcripts_unlisted: total.transcripts_unlisted,
        skipped_lines: total.skipped_lines,
        reflog_entries: total.reflog_entries,
        failed_records: total.failed_records,
        duplicates_refused: total.duplicates_refused,
        error: total.error.clone(),
    };
    if let Err(error) = ledger::record_catch_up(
        ledger_repo,
        worktree_path,
        &record,
        ledger::ids::now_millis(),
    ) {
        let note = format!("catch-up state could not be recorded: {error}");
        total.error = if total.error.is_empty() {
            note
        } else {
            format!("{}; {note}", total.error)
        };
    }
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
    fn a_reflog_message_longer_than_the_object_cap_is_not_recorded_again() {
        let main = tempfile::tempdir().expect("checkout");
        git_in(main.path(), &["init", "-b", "main"]);
        std::fs::write(main.path().join("seed.txt"), "seed").expect("seed");
        git_in(main.path(), &["add", "seed.txt"]);
        git_in(
            main.path(),
            &["commit", "-m", &format!("long {}", "w".repeat(5000))],
        );
        crate::test_support::trust_repo(main.path());
        let repo = main.path().canonicalize().expect("canonical");
        let repo = repo.to_string_lossy();

        let first = ingest_reflog_into(&repo, &repo, 200);
        assert!(first.recorded > 0, "{first:?}");
        let second = ingest_reflog_into(&repo, &repo, 200);
        assert_eq!(
            second.recorded, 0,
            "the capped stored identity was compared with the full one"
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
            fingerprint: None,
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
            "2026-09-01T12:00:00",
            "2026-09-01t12:00:00z",
            "2026-02-30T12:00:00Z",
            "2026-09-01T12:00:60Z",
            "2026\u{2014}09-01T12:00:00Z",
            "\u{1F680}026-09-01T12:00:00Z",
        ] {
            assert_eq!(iso_to_millis(bad), 0, "{bad:?} should not parse");
        }
    }

    #[test]
    fn an_offset_timestamp_is_read_as_its_utc_instant() {
        // A transcript may spell its own time with an offset; it names the same
        // instant as the `Z` form, which is what the skip compares.
        assert_eq!(
            iso_to_millis("2026-09-01T13:00:00+01:00"),
            iso_to_millis("2026-09-01T12:00:00.000Z"),
        );
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
mod codex_tests;
#[cfg(test)]
mod event_time_tests;
#[cfg(test)]
mod family_tests;
#[cfg(test)]
mod od1_adv_tests;
#[cfg(test)]
mod reanchor_tests;
#[cfg(test)]
mod reflog_bench_tests;
#[cfg(test)]
mod reflog_coverage_tests;
#[cfg(test)]
mod reflog_replay_tests;
#[cfg(test)]
mod source_tests;
#[cfg(test)]
mod transcript_bench_tests;
#[cfg(test)]
mod walk_tests;

#[cfg(test)]
mod adv2_bench_tests;
#[cfg(test)]
mod adv_l5_tests;
