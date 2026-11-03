//! What a GitHub Actions run's outcome means, decided once.
//!
//! Two places used to decide this on their own: the Remote panel's banner for
//! a run that "went red", and the work view's "needs attention" mark. They
//! disagreed about `action_required`, and neither could be reached from the
//! native CI watcher (`workbench::ci_watch`), which has to make the same call
//! with no renderer running. This module is the owner. Every run leaves
//! [`parse_workflow_runs`](super) with a [`RunOutcome`], the renderer reads
//! that field and holds no table of its own, and the transition detector
//! below is the only code that decides a run "just finished".
//!
//! # The rule
//!
//! `status` says whether the run is moving; `conclusion` says how it ended and
//! means nothing until it has. A closed set decides each, and anything outside
//! a set is [`RunOutcome::Unknown`], never a verdict:
//!
//! | outcome        | when                                                                  |
//! |----------------|-----------------------------------------------------------------------|
//! | `in_flight`    | status `queued`, `in_progress`, `requested`, `waiting`, `pending`     |
//! | `passed`       | `completed` with conclusion `success`                                 |
//! | `failed`       | `completed` with `failure`, `timed_out` or `startup_failure`          |
//! | `cancelled`    | `completed` with `cancelled`                                          |
//! | `needs_person` | `completed` with `action_required`                                    |
//! | `unknown`      | everything else: `neutral`, `skipped`, `stale`, an empty conclusion,  |
//! |                | a status or conclusion this build has never heard of                  |
//!
//! `action_required` is a run held for a person (an approval, a first-time
//! contributor's workflow). Nothing broke, so it is never `failed`, never red,
//! and never a `ci_failed` signal. It is still attention-worthy: someone has
//! to act before the run can go on, which is why the work view marks a row
//! that has one. `cancelled` is its own outcome because it is neither a pass
//! nor a defect in the change: it is announced as its own signal kind, and a
//! red banner is not raised for it. The delivery timeline's pass rate still
//! counts it as a bad end, to agree with Firebase App Hosting (see
//! `src/lib/github/runLifecycle.ts`), which is a statement about a rate and
//! not about whether to interrupt anyone.
//!
//! `completed` with an empty conclusion is `unknown`: it happens whenever the
//! conclusion could not be read, and an unread conclusion is not a success.
//!
//! # Transitions
//!
//! A run "finished" only if it was seen moving and later seen settled. That
//! one definition is [`diff`], and three rules keep it honest:
//!
//! - **No replay.** The first observation of a repository is its baseline and
//!   announces nothing, and a run first seen already settled is history.
//! - **Keyed by run id**, never by position: the listing is newest-first and
//!   capped, so a new run shifts every row down.
//! - **A run that vanished says nothing.** Rows leave a capped listing for
//!   reasons that have no connection to their outcome. When the listing is
//!   capped, [`Observed::capped`] says so, because a run that was moving and
//!   fell off the end can no longer be followed and will never be announced.
//!
//! [`Observer`] holds one baseline per repository for the whole process, so
//! the panel's poll and the watcher's poll see the same history and a finish
//! is detected once, whichever of them looked first. Detecting it once must
//! not mean telling only one of them: the watcher drains a queue, and the
//! panel reads a bounded log of red finishes past its own cursor, so a failure
//! the watcher crossed first is still the panel's banner. Everything it holds
//! is bounded.
use super::{GitHubRunsReport, WorkflowRunInfo};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Instant;

/// How a run stands. See the module docs for the table this implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    InFlight,
    Passed,
    Failed,
    Cancelled,
    NeedsPerson,
    /// Also the value of a run decoded from a payload that predates this
    /// field: an outcome nobody computed is unknown, never a verdict.
    #[default]
    Unknown,
}

impl RunOutcome {
    /// A finish worth telling someone about: the run ended in a way that is
    /// one of the three verdicts. `needs_person` and `unknown` are settled but
    /// are not announced as a finish.
    pub fn is_verdict(self) -> bool {
        matches!(self, Self::Passed | Self::Failed | Self::Cancelled)
    }
}

const IN_FLIGHT_STATUS: [&str; 5] = ["queued", "in_progress", "requested", "waiting", "pending"];
const FAILED_CONCLUSION: [&str; 3] = ["failure", "timed_out", "startup_failure"];

/// The outcome for the two fields `gh run list` reports.
pub fn classify(status: &str, conclusion: &str) -> RunOutcome {
    let status = status.trim().to_ascii_lowercase();
    let conclusion = conclusion.trim().to_ascii_lowercase();
    if IN_FLIGHT_STATUS.contains(&status.as_str()) {
        return RunOutcome::InFlight;
    }
    if status != "completed" {
        return RunOutcome::Unknown;
    }
    match conclusion.as_str() {
        "success" => RunOutcome::Passed,
        "cancelled" => RunOutcome::Cancelled,
        "action_required" => RunOutcome::NeedsPerson,
        c if FAILED_CONCLUSION.contains(&c) => RunOutcome::Failed,
        _ => RunOutcome::Unknown,
    }
}

/// What one comparison of two observations of a repository found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    /// Runs that were seen moving and are now settled in a verdict, newest
    /// listing order.
    pub finished: Vec<WorkflowRunInfo>,
    /// The listing was cut short, so runs beyond it could not be seen.
    pub capped: bool,
}

/// The baseline kept between observations: the outcome last seen per run id.
pub type Baseline = HashMap<u64, RunOutcome>;

/// The baseline `runs` leaves behind. First occurrence of an id wins: the
/// listing is newest-first, so a duplicate id is believed in its newest row.
pub fn baseline_of(runs: &[WorkflowRunInfo]) -> Baseline {
    let mut seen = Baseline::with_capacity(runs.len());
    for run in runs {
        seen.entry(run.id).or_insert(run.outcome);
    }
    seen
}

/// Which of `runs` finished since `previous`, a pure function of the two.
///
/// `previous` of `None` is the first look at this repository: its baseline,
/// announcing nothing.
pub fn diff(previous: Option<&Baseline>, runs: &[WorkflowRunInfo], truncated: bool) -> Observed {
    let Some(previous) = previous else {
        return Observed {
            capped: truncated,
            ..Observed::default()
        };
    };
    let mut finished = Vec::new();
    let mut counted = std::collections::HashSet::new();
    for run in runs {
        if !counted.insert(run.id) {
            continue;
        }
        if previous.get(&run.id) == Some(&RunOutcome::InFlight) && run.outcome.is_verdict() {
            finished.push(run.clone());
        }
    }
    Observed {
        finished,
        capped: truncated,
    }
}

/// Repositories the process remembers a baseline for. A baseline is at most one
/// listing (twenty runs), so this bounds the whole structure.
pub const MAX_REPOSITORIES: usize = 64;
/// Finishes queued for the watcher at once. Past it the oldest is dropped and
/// counted, never the newest and never silently.
pub const MAX_PENDING: usize = 256;
/// Red finishes kept per repository for the panel's banner. Past it the oldest
/// is dropped and counted ([`Observer::red_overflow`]).
pub const MAX_RED_LOG: usize = 32;

/// A finish the watcher has yet to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    /// `owner/repo`, the key the baseline is held under.
    pub repository: String,
    pub run: WorkflowRunInfo,
}

struct RepositoryBaseline {
    runs: Baseline,
    observed_at: Instant,
    /// Position in the observation order, for evicting the stalest repository.
    turn: u64,
    /// Red finishes in the order they were seen, each with its sequence
    /// number. The panel's cursor reads this; the watcher's queue does not.
    red: VecDeque<(u64, WorkflowRunInfo)>,
    /// The sequence number the panel has been told up to. `None` until its
    /// first look at this repository, which announces nothing.
    panel_cursor: Option<u64>,
}

/// One baseline per repository, shared by every poll in the process.
///
/// A finish is *detected* once, against that one baseline, whichever poll
/// looked first. It is then *delivered* to each consumer from its own place:
/// the watcher drains [`Self::take_pending`], and the panel reads the
/// repository's red log past its own cursor. Neither consumer can use up the
/// other's news.
#[derive(Default)]
pub struct Observer {
    repositories: HashMap<String, RepositoryBaseline>,
    pending: VecDeque<Finished>,
    dropped: u64,
    red_overflow: u64,
    turn: u64,
    /// Sequence number of the latest red finish seen anywhere.
    seq: u64,
}

impl Observer {
    /// Fold one successful listing in. `at` is when the request was made, not
    /// when it came back: a slow call that returns after a newer one must not
    /// move the baseline backwards and announce the same finish twice, so a
    /// listing older than the baseline is ignored and reported as such.
    pub fn observe(
        &mut self,
        repository: &str,
        runs: &[WorkflowRunInfo],
        truncated: bool,
        at: Instant,
    ) -> Option<Observed> {
        if self
            .repositories
            .get(repository)
            .is_some_and(|held| at < held.observed_at)
        {
            return None;
        }
        self.turn += 1;
        let observed = diff(
            self.repositories.get(repository).map(|held| &held.runs),
            runs,
            truncated,
        );
        let held = self
            .repositories
            .entry(repository.to_owned())
            .or_insert_with(|| RepositoryBaseline {
                runs: Baseline::new(),
                observed_at: at,
                turn: self.turn,
                red: VecDeque::new(),
                panel_cursor: None,
            });
        held.runs = baseline_of(runs);
        held.observed_at = at;
        held.turn = self.turn;
        for run in observed
            .finished
            .iter()
            .filter(|run| run.outcome == RunOutcome::Failed)
        {
            self.seq += 1;
            if held.red.len() >= MAX_RED_LOG {
                held.red.pop_front();
                self.red_overflow += 1;
            }
            held.red.push_back((self.seq, run.clone()));
        }
        self.evict();
        for run in &observed.finished {
            if self.pending.len() >= MAX_PENDING {
                self.pending.pop_front();
                self.dropped += 1;
            }
            self.pending.push_back(Finished {
                repository: repository.to_owned(),
                run: run.clone(),
            });
        }
        Some(observed)
    }

    fn evict(&mut self) {
        while self.repositories.len() > MAX_REPOSITORIES {
            let stalest = self
                .repositories
                .iter()
                .min_by_key(|(_, held)| held.turn)
                .map(|(key, _)| key.clone());
            match stalest {
                Some(key) => {
                    self.repositories.remove(&key);
                }
                None => break,
            }
        }
    }

    /// The panel's look: fold the listing in like any other poll, then return
    /// the red finishes this repository has logged past the panel's own cursor
    /// (whoever saw them first) and move the cursor to the newest.
    ///
    /// The panel's first look at a repository announces nothing: its cursor
    /// starts where the log stood *before* this listing, so a finish this very
    /// listing reveals is news and an older one is history. A listing ignored
    /// as stale still delivers what the log already holds.
    pub fn panel_look(
        &mut self,
        repository: &str,
        runs: &[WorkflowRunInfo],
        truncated: bool,
        at: Instant,
    ) -> Vec<WorkflowRunInfo> {
        let before = self.seq;
        self.observe(repository, runs, truncated, at);
        let Some(held) = self.repositories.get_mut(repository) else {
            return Vec::new();
        };
        let cursor = *held.panel_cursor.get_or_insert(before);
        held.panel_cursor = Some(self.seq);
        held.red
            .iter()
            .filter(|(seq, _)| *seq > cursor)
            .map(|(_, run)| run.clone())
            .collect()
    }

    /// Up to `max` finishes, oldest first, removed from the queue.
    pub fn take_pending(&mut self, max: usize) -> Vec<Finished> {
        let count = max.min(self.pending.len());
        self.pending.drain(..count).collect()
    }

    /// Finishes dropped because the queue was full. Nonzero means a CI result
    /// was never acted on.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Red finishes that left a repository's log before any consumer could
    /// have read them. Nonzero means the panel may have missed a banner.
    pub fn red_overflow(&self) -> u64 {
        self.red_overflow
    }
}

/// The process-wide observer.
pub fn observer() -> &'static Mutex<Observer> {
    static OBSERVER: OnceLock<Mutex<Observer>> = OnceLock::new();
    OBSERVER.get_or_init(|| Mutex::new(Observer::default()))
}

/// Runs `body` on the process-wide observer. A poisoned lock is recovered: the
/// observer holds only counters and bounded maps that a panic cannot leave
/// half-written in a way that matters more than losing every baseline.
pub fn with_observer<T>(body: impl FnOnce(&mut Observer) -> T) -> T {
    let mut guard = observer().lock().unwrap_or_else(PoisonError::into_inner);
    body(&mut guard)
}

/// The key a repository's baseline is held under.
pub fn repository_key(owner: &str, repo: &str) -> String {
    format!("{owner}/{repo}")
}

/// The panel's look at a successful report: fold it into the process-wide
/// baseline and return the runs that finished *red* since the panel last
/// looked, including ones the watcher saw cross first. Only `failed` is red:
/// see the module docs for why `cancelled` and `needs_person` are not.
///
/// Only the Remote panel's poll calls this, because it moves the panel's
/// cursor. The watcher's poll uses [`observe_report`].
pub fn went_red(report: &GitHubRunsReport, requested_at: Instant) -> Vec<WorkflowRunInfo> {
    if !report.checked {
        return Vec::new();
    }
    let key = repository_key(&report.owner, &report.repo);
    with_observer(|observer| {
        observer.panel_look(&key, &report.runs, report.truncated, requested_at)
    })
}

/// The watcher's look at a successful report: fold it into the baseline, which
/// queues any finish for [`Observer::take_pending`] and logs a red one for the
/// panel, and leave the panel's cursor where it is.
pub fn observe_report(report: &GitHubRunsReport, requested_at: Instant) {
    if !report.checked {
        return;
    }
    let key = repository_key(&report.owner, &report.repo);
    with_observer(|observer| {
        observer.observe(&key, &report.runs, report.truncated, requested_at);
    });
}

#[cfg(test)]
#[path = "outcome_tests.rs"]
mod tests;
