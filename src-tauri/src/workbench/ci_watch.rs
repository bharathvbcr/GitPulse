//! Carries CI results on an agent's branch to its task.
//!
//! An agent that pushes `gitpulse/<task>-<id>` and walks away is waiting on a
//! workflow nobody is looking at. This watcher polls GitHub Actions for the
//! repositories that have such a branch, and when a run it saw moving settles
//! it tells the store with `signals.record`. The store makes one inbox notice
//! of it (`run_ci_passed`, `run_ci_failed`, `run_ci_cancelled`) and the
//! existing native notification coordinator delivers that notice: this module
//! has no delivery code and posts nothing itself. A failure also leaves a log
//! block on the task (workflow, run URL, head commit, conclusion; never the
//! run's output) the way a held verification does.
//!
//! The rules, each pinned by a test:
//! - **What is watched.** A run that is live, or ended within
//!   [`FINISHED_WINDOW_SECS`], whose branch is an attempt branch
//!   (`refs/heads/gitpulse/…`). A repository is polled only while it has such a
//!   run, and a finished CI run is attributed to an attempt only by the branch
//!   GitHub says it ran on. Never by commit: an attempt's recorded commit is
//!   where it started, so CI on it is the base branch's own. A signal's branch
//!   is the run's, and a record that would name any other is refused.
//! - **One decision.** What counts as a finish is `github::outcome`: the same
//!   classifier and the same baseline the Remote panel reads. A run already
//!   settled when first seen is history, and `cancelled` is its own kind.
//! - **Trust first.** A repository the user has not trusted is never polled and
//!   is reported as untrusted, not as a `gh` problem.
//! - **Bounded.** At most [`POLLS_PER_SWEEP`] repositories are asked per sweep,
//!   at most [`MAX_CALLS`] `gh` calls per [`CALL_WINDOW`] in all, a failing
//!   repository backs off up to [`BACKOFF_MAX`], and the retry queue holds
//!   [`MAX_RETRY`] results.
//! - **Honest.** `gh` missing or unauthenticated, an untrusted repository, a
//!   spent call budget and a capped listing are each their own state in
//!   [`status`]; none of them reads as "no CI". A result that could not be
//!   recorded is retried and then logged, never dropped silently.
//! - **Idempotent.** The store de-duplicates on (task, source, run, conclusion),
//!   so a re-poll answers `recorded: false` and writes neither a second notice
//!   nor a second log block.
//!
//! Security: this adds a reader of GitHub (through the existing `gh` loader and
//! its trust gate) and a writer of one store method, `signals.record`, which
//! is host-only: the renderer cannot forge a CI verdict through the generic
//! request path (see `WorkbenchState::request`). Nothing here widens what can
//! run, and a signal carries no authority over a task, a run or a proposal.

use super::intake;
use super::{query, WorkbenchError, WorkbenchState};
use crate::github::outcome::{Finished, RunOutcome};
use crate::github::GitHubRunsReport;
use dc_store::Store;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A run that ended this long ago is no longer waited on.
pub(crate) const FINISHED_WINDOW_SECS: i64 = 2 * 60 * 60;
/// How often a repository with a run in flight is asked.
pub(crate) const ACTIVE_POLL: Duration = Duration::from_secs(30);
/// How often a watched repository with nothing in flight is asked.
pub(crate) const IDLE_POLL: Duration = Duration::from_secs(120);
/// The longest a failing repository waits between asks.
pub(crate) const BACKOFF_MAX: Duration = Duration::from_secs(600);
/// Repositories asked in one sweep.
pub(crate) const POLLS_PER_SWEEP: usize = 2;
/// `gh` calls the watcher makes across all repositories in one [`CALL_WINDOW`].
pub(crate) const MAX_CALLS: usize = 40;
pub(crate) const CALL_WINDOW: Duration = Duration::from_secs(600);
/// Results acted on per sweep.
const FINISH_BATCH: usize = 32;
/// Results kept for another try after a store write failed.
pub(crate) const MAX_RETRY: usize = 64;
/// Writes tried before a result is given up on and logged.
const MAX_ATTEMPTS: u8 = 3;
/// Run pages read per sweep, 200 runs each, newest first.
const MAX_PAGES: usize = 3;
const SWEEP_PERIOD: Duration = Duration::from_secs(5);
const ATTEMPT_PREFIX: &str = "refs/heads/gitpulse/";

/// A run the watcher is following: where it works and what CI should say about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub run_id: String,
    pub task_id: String,
    /// The branch the attempt works on, without `refs/heads/`.
    pub branch: Option<String>,
    /// The branch is an attempt branch. Only these are matched by branch name
    /// and only these make a repository worth polling.
    pub attempt: bool,
    /// The repository's main checkout, which outlives the attempt's worktree.
    pub checkout: String,
}

/// Whether a run still deserves watching, and what it is, from a `runs.list` item.
///
/// Live is the store's own definition of an active attempt. A finished one
/// stays for [`FINISHED_WINDOW_SECS`] after it finished, since an agent pushes
/// and exits and the workflow outlives it.
pub(crate) fn candidate_of(run: &Value, now: i64) -> Option<Candidate> {
    let state = run["state"].as_str()?;
    let live = match state {
        "starting" | "running" | "unresolved" => true,
        // A launch that was prepared and never claimed before it expired never
        // ran an agent, so it has no branch of its own to wait on.
        "prepared" if run["expires_at"].as_i64().is_some_and(|at| at > now) => true,
        "prepared" => return None,
        _ => false,
    };
    if !live {
        let ended = run["finished_at"]
            .as_i64()
            .or_else(|| run["updated_at"].as_i64())?;
        if now.saturating_sub(ended) > FINISHED_WINDOW_SECS {
            return None;
        }
    }
    let head_ref = run["head_ref"].as_str();
    let branch = head_ref
        .and_then(|name| name.strip_prefix("refs/heads/"))
        .map(str::to_owned);
    let common = Path::new(run["git_common_dir"].as_str()?);
    let checkout = match common.file_name().and_then(|name| name.to_str()) {
        Some(".git") => common.parent()?.to_str()?.to_owned(),
        _ => run["cwd"].as_str()?.to_owned(),
    };
    Some(Candidate {
        run_id: run["id"].as_str()?.to_owned(),
        task_id: run["task_id"].as_str()?.to_owned(),
        attempt: head_ref.is_some_and(|name| name.starts_with(ATTEMPT_PREFIX)),
        branch,
        checkout,
    })
}

/// The runs worth watching, newest first, and whether more were left unread.
fn watch_set(store: &Store, now: i64) -> Result<(Vec<Candidate>, bool), WorkbenchError> {
    let mut found = Vec::new();
    let mut cursor: Option<String> = None;
    let mut more = false;
    for page_number in 0..MAX_PAGES {
        let mut input = json!({"newest": true, "limit": 200});
        if let Some(cursor) = &cursor {
            input["cursor"] = json!(cursor);
        }
        let page = query(store, "runs.list", &input.to_string())?;
        let items = page["items"]
            .as_array()
            .ok_or_else(|| WorkbenchError::new("protocol_error", "Invalid run page."))?;
        found.extend(items.iter().filter_map(|run| candidate_of(run, now)));
        more = page["has_more"] == true;
        match page["next_cursor"].as_str() {
            Some(next) if more && page_number + 1 < MAX_PAGES => cursor = Some(next.into()),
            _ => break,
        }
    }
    Ok((found, more))
}

/// The attempt a finished CI run belongs to: the one whose attempt branch is
/// the branch GitHub says the run ran on.
///
/// Never by commit. A run's `head_oid` is the commit the attempt *started*
/// from, not a tip it pushed, so CI on that commit is the base branch's own
/// (main's push run, say) and is nobody's result here. The store holds no
/// attempt tip, so branch is the only honest key.
pub(crate) fn matching<'a>(
    candidates: &'a [Candidate],
    run: &crate::github::WorkflowRunInfo,
) -> Option<&'a Candidate> {
    candidates.iter().find(|candidate| {
        candidate.attempt
            && !run.head_branch.is_empty()
            && candidate.branch.as_deref() == Some(run.head_branch.as_str())
    })
}

/// How long a repository that failed `failures` times in a row waits.
pub(crate) fn backoff(failures: u32) -> Duration {
    let doubled = IDLE_POLL.saturating_mul(1u32 << failures.min(8));
    doubled.min(BACKOFF_MAX)
}

/// Everything the watcher reads from outside the store. Tests supply their own.
pub(crate) trait Backend {
    /// `Ok(false)` is "not trusted"; `Err` is "could not tell".
    fn trusted(&self, checkout: &str) -> Result<bool, String>;
    /// The `owner/repo` the checkout's remote names, read from git and never
    /// from GitHub, so a finish can be attributed before this watcher has
    /// asked the repository itself. `Ok(None)` is "the repository has no
    /// GitHub remote"; `Err` is "could not tell". Each costs a `git`
    /// process, so the caller asks rarely.
    fn remote(&self, checkout: &str) -> Result<Option<String>, String>;
    /// One `gh` listing for the repository at `checkout`. It folds a checked
    /// report into the shared baseline (`github::outcome`) before it returns.
    fn runs(&self, checkout: &str) -> GitHubRunsReport;
    /// The finishes the baseline has seen since the last call, oldest first.
    fn take_finished(&self, max: usize) -> Vec<Finished>;
    /// Finishes that were lost because nobody drained them in time.
    fn dropped(&self) -> u64;
    /// Tells the notification coordinator something new is pending.
    fn wake(&self);
    fn now(&self) -> Instant;
    fn unix_now(&self) -> i64;
    fn cancelled(&self) -> bool;
}

/// Why a repository is in the state it is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WatchState {
    /// Found, not asked yet.
    Waiting,
    /// Asked, and the listing was read.
    Watching,
    /// `gh` is missing, not signed in, rate limited or failing. Not "no CI".
    Unavailable,
    /// The repository is not trusted, so it was not asked.
    Untrusted,
    /// Whether the repository is trusted could not be read.
    TrustUnknown,
    /// The call budget for this window is spent; the ask is deferred.
    Throttled,
    /// The checkout is gone, so there is nowhere to ask from.
    NoCheckout,
    /// The repository has no GitHub remote, so there is no CI to read. That is
    /// a fact about the repository, not a fault, and nothing is asked.
    NoRemote,
}

/// What the watcher can say about one repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RepoStatus {
    pub checkout: String,
    pub state: WatchState,
    /// Why, in words; empty while watching normally.
    pub reason: String,
    /// The listing was cut short, so a run may have been missed: one that was
    /// moving and fell off the end can no longer be followed.
    pub capped: bool,
}

struct Repo {
    /// `owner/repo` once git or a listing has said so.
    remote: Option<String>,
    /// Not before this is the remote read from git again. A repository whose
    /// remote could not be found would otherwise cost a process every sweep.
    remote_retry: Instant,
    next_due: Instant,
    failures: u32,
    moving: bool,
    status: RepoStatus,
}

/// What a sweep did, for the log line and for tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub polled: usize,
    pub recorded: usize,
    pub already_recorded: usize,
    pub unmatched: usize,
    pub failed: usize,
}

#[derive(Debug, Clone)]
struct Pending {
    finished: Finished,
    attempts: u8,
}

/// The run table as last read. It is up to `MAX_PAGES` × 200 run bodies and
/// nothing in it changes second by second, so it is read on the beat of a
/// busy repository's ask ([`ACTIVE_POLL`]) while finishes are acted on every sweep.
struct WatchSet {
    read_at: Instant,
    candidates: Vec<Candidate>,
    capped: bool,
}

/// The watcher's memory between sweeps.
#[derive(Default)]
pub(crate) struct Watcher {
    watch: Option<WatchSet>,
    repos: HashMap<String, Repo>,
    calls: VecDeque<Instant>,
    retry: VecDeque<Pending>,
    /// Finishes dropped upstream, as of the last sweep, so a change is reported once.
    dropped_seen: u64,
    /// Whether the watch set was cut off by the page bound, as of the last sweep.
    watch_capped: bool,
}

/// The subsystem name Diagnostics lists while CI cannot be read.
const SUBSYSTEM: &str = "ci-watch";

impl Watcher {
    /// Why CI results cannot currently be trusted to arrive, or `None`. A
    /// repository the user has not trusted is a choice and is not a fault;
    /// `gh` missing, signed out or failing, and a trust store that cannot be
    /// read, are faults, and Diagnostics lists them until they clear.
    pub(crate) fn fault(&self) -> Option<String> {
        let faults: Vec<&RepoStatus> = self
            .repos
            .values()
            .map(|repo| &repo.status)
            .filter(|status| {
                matches!(
                    status.state,
                    WatchState::Unavailable | WatchState::TrustUnknown
                )
            })
            .collect();
        let first = faults.iter().min_by(|a, b| a.checkout.cmp(&b.checkout))?;
        Some(format!(
            "CI results cannot be read for {} repositor{}: {}",
            faults.len(),
            if faults.len() == 1 { "y" } else { "ies" },
            first.reason
        ))
    }

    /// Hands [`Self::fault`] to Diagnostics.
    fn report_health(&self) {
        match self.fault() {
            Some(reason) => crate::supervisor::note_degraded(SUBSYSTEM, &reason),
            None => crate::supervisor::note_recovered(SUBSYSTEM),
        }
    }

    /// One pass: find what to watch, ask the repositories that are due, and act
    /// on what finished. Never panics on a bad run: each result's failure is
    /// that result's only.
    pub(crate) fn sweep(&mut self, store: &Store, backend: &dyn Backend) -> Report {
        let mut report = Report::default();
        if backend.cancelled() {
            return report;
        }
        let now = backend.now();
        let set = match self.watch.take() {
            Some(set) if now.saturating_duration_since(set.read_at) < ACTIVE_POLL => set,
            _ => match watch_set(store, backend.unix_now()) {
                Ok((candidates, capped)) => WatchSet {
                    read_at: now,
                    candidates,
                    capped,
                },
                Err(error) => {
                    log::warn!(target: "workbench", "ci watch could not list runs: {}", error.message);
                    return report;
                }
            },
        };
        let (candidates, capped) = (&set.candidates, set.capped);
        if capped != self.watch_capped {
            self.watch_capped = capped;
            if capped {
                log::warn!(target: "workbench", "ci watch read {MAX_PAGES} pages of runs and more remain; older runs are not watched");
            }
        }
        let by_checkout = group_by_checkout(candidates);
        for (checkout, runs) in &by_checkout {
            if runs.iter().any(|run| run.attempt) {
                self.repos.entry(checkout.clone()).or_insert_with(|| Repo {
                    remote: None,
                    remote_retry: now,
                    next_due: now,
                    failures: 0,
                    moving: false,
                    status: RepoStatus {
                        checkout: checkout.clone(),
                        state: WatchState::Waiting,
                        reason: String::new(),
                        capped: false,
                    },
                });
            }
        }
        // The panel's poll can fold a finish into the shared baseline before
        // this watcher has asked the repository, so which repository a finish
        // belongs to is read from git, not learned from our own listing. That
        // read is a process, so it is made for a trusted checkout only and not
        // more often than once per idle period.
        for (checkout, repo) in &mut self.repos {
            if repo.remote.is_none() && repo.remote_retry <= now {
                Self::find_remote(checkout, repo, backend, now);
            }
        }
        // A repository that lost its last attempt branch is no longer watched.
        self.repos.retain(|checkout, _| {
            by_checkout
                .get(checkout)
                .is_some_and(|runs| runs.iter().any(|run| run.attempt))
        });
        let mut due: Vec<String> = self
            .repos
            .iter()
            .filter(|(_, repo)| {
                repo.next_due <= now
                    && !(repo.status.state == WatchState::NoRemote && repo.remote.is_none())
            })
            .map(|(checkout, _)| checkout.clone())
            .collect();
        due.sort();
        for checkout in due.into_iter().take(POLLS_PER_SWEEP) {
            if backend.cancelled() {
                self.watch = Some(set);
                return report;
            }
            if self.poll(&checkout, backend, now) {
                report.polled += 1;
            }
        }
        self.act(store, backend, &set.candidates, &mut report);
        self.watch = Some(set);
        report
    }

    /// Reads the repository's GitHub remote from git, if the checkout is
    /// trusted. Whatever the answer, the next read is an idle period away.
    fn find_remote(checkout: &str, repo: &mut Repo, backend: &dyn Backend, now: Instant) {
        repo.remote_retry = now + IDLE_POLL;
        if !Path::new(checkout).is_dir() || !matches!(backend.trusted(checkout), Ok(true)) {
            return;
        }
        match backend.remote(checkout) {
            Ok(Some(found)) => {
                repo.remote = Some(found);
                if repo.status.state == WatchState::NoRemote {
                    repo.status.state = WatchState::Waiting;
                    repo.status.reason = String::new();
                    repo.next_due = now;
                }
            }
            Ok(None) => {
                if repo.status.state != WatchState::NoRemote {
                    log::info!(target: "workbench", "ci watch {checkout}: the repository has no GitHub remote");
                }
                repo.status.state = WatchState::NoRemote;
                repo.status.reason =
                    "the repository has no GitHub remote, so there is no CI to read".into();
                repo.status.capped = false;
                repo.next_due = now + IDLE_POLL;
            }
            // Could not tell: the listing's own error will say more if it comes to that.
            Err(error) => {
                log::warn!(target: "workbench", "ci watch {checkout}: the git remote could not be read: {error}");
            }
        }
    }

    /// Asks one repository, unless it must not be asked. True when `gh` ran.
    fn poll(&mut self, checkout: &str, backend: &dyn Backend, now: Instant) -> bool {
        let Some(repo) = self.repos.get_mut(checkout) else {
            return false;
        };
        let defer = |repo: &mut Repo, state: WatchState, reason: String, wait: Duration| {
            if repo.status.state != state {
                log::warn!(target: "workbench", "ci watch {checkout}: {state:?}: {reason}");
            }
            repo.status.state = state;
            repo.status.reason = reason;
            repo.next_due = now + wait;
        };
        if !Path::new(checkout).is_dir() {
            defer(
                repo,
                WatchState::NoCheckout,
                "the repository's checkout no longer exists".into(),
                IDLE_POLL,
            );
            return false;
        }
        match backend.trusted(checkout) {
            Ok(true) => {}
            Ok(false) => {
                defer(
                    repo,
                    WatchState::Untrusted,
                    "the repository is not trusted, so GitHub was not asked".into(),
                    IDLE_POLL,
                );
                return false;
            }
            Err(error) => {
                defer(
                    repo,
                    WatchState::TrustUnknown,
                    format!("whether the repository is trusted could not be read: {error}"),
                    IDLE_POLL,
                );
                return false;
            }
        }
        while self
            .calls
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= CALL_WINDOW)
        {
            self.calls.pop_front();
        }
        if self.calls.len() >= MAX_CALLS {
            defer(
                repo,
                WatchState::Throttled,
                format!(
                    "{MAX_CALLS} GitHub calls were made in the last {} minutes",
                    CALL_WINDOW.as_secs() / 60
                ),
                ACTIVE_POLL,
            );
            return false;
        }
        self.calls.push_back(now);
        let report = backend.runs(checkout);
        if !report.owner.is_empty() {
            repo.remote = Some(crate::github::outcome::repository_key(
                &report.owner,
                &report.repo,
            ));
        }
        if report.checked {
            repo.failures = 0;
            // Any run in flight in the repository keeps the ask frequent, not
            // only the attempt's: which run belongs to whom is settled when it
            // finishes. The call budget bounds the cost of a busy repository.
            repo.moving = report
                .runs
                .iter()
                .any(|run| run.outcome == RunOutcome::InFlight);
            repo.status.state = WatchState::Watching;
            repo.status.reason = String::new();
            repo.status.capped = report.truncated;
            repo.next_due = now + if repo.moving { ACTIVE_POLL } else { IDLE_POLL };
        } else {
            repo.failures = repo.failures.saturating_add(1);
            let reason = report
                .error
                .clone()
                .unwrap_or_else(|| "GitHub could not be read".into());
            defer(
                repo,
                WatchState::Unavailable,
                reason,
                backoff(repo.failures),
            );
        }
        true
    }

    /// Record what finished and attribute it to the attempt it belongs to.
    fn act(
        &mut self,
        store: &Store,
        backend: &dyn Backend,
        candidates: &[Candidate],
        report: &mut Report,
    ) {
        let dropped = backend.dropped();
        if dropped != self.dropped_seen {
            log::warn!(target: "workbench", "ci watch lost {} finished run(s): results were not drained fast enough", dropped - self.dropped_seen);
            self.dropped_seen = dropped;
        }
        let mut work: Vec<Pending> = self.retry.drain(..).collect();
        work.extend(
            backend
                .take_finished(FINISH_BATCH)
                .into_iter()
                .map(|finished| Pending {
                    finished,
                    attempts: 0,
                }),
        );
        let mut woke = false;
        // A finish is only recorded for a repository that is trusted *now*:
        // the shared baseline also folds in the panel's own listings, so a
        // finish can exist for a repository this watcher would not have asked.
        let mut admitted: HashMap<String, bool> = HashMap::new();
        for mut item in work {
            let in_repository: Vec<Candidate> = self
                .repos
                .iter()
                .filter(|(checkout, repo)| {
                    repo.remote.as_deref() == Some(item.finished.repository.as_str())
                        && *admitted
                            .entry((*checkout).clone())
                            .or_insert_with(|| matches!(backend.trusted(checkout), Ok(true)))
                })
                .flat_map(|(checkout, _)| {
                    candidates.iter().filter(move |c| &c.checkout == checkout)
                })
                .cloned()
                .collect();
            let Some(candidate) = matching(&in_repository, &item.finished.run) else {
                report.unmatched += 1;
                continue;
            };
            match record(store, candidate, &item.finished.run) {
                Ok(Recorded::New) => {
                    report.recorded += 1;
                    woke = true;
                }
                Ok(Recorded::Repeat) => report.already_recorded += 1,
                Ok(Recorded::Unrecordable(why)) => {
                    report.failed += 1;
                    log::warn!(target: "workbench", "ci watch could not record run {} for task {}: {why}", item.finished.run.id, candidate.task_id);
                }
                Err(error) => {
                    item.attempts += 1;
                    if item.attempts < MAX_ATTEMPTS && self.retry.len() < MAX_RETRY {
                        self.retry.push_back(item);
                    } else {
                        report.failed += 1;
                        log::warn!(target: "workbench", "ci watch gave up recording run {} for task {}: {}: {}", item.finished.run.id, candidate.task_id, error.code, error.message);
                    }
                }
            }
        }
        if woke {
            backend.wake();
        }
    }
}

fn group_by_checkout(candidates: &[Candidate]) -> HashMap<String, Vec<Candidate>> {
    let mut grouped: HashMap<String, Vec<Candidate>> = HashMap::new();
    for candidate in candidates {
        grouped
            .entry(candidate.checkout.clone())
            .or_default()
            .push(candidate.clone());
    }
    grouped
}

/// What the store did with one result.
#[derive(Debug, PartialEq, Eq)]
enum Recorded {
    New,
    Repeat,
    /// The result cannot be expressed as a signal (a field the store would
    /// refuse), and sending it again would not change that.
    Unrecordable(String),
}

static REQUESTS: AtomicU64 = AtomicU64::new(0);

/// A request id no earlier call used. The store's own de-duplication is by the
/// result, not the request, so a fresh id per attempt is right.
fn fresh_request_id() -> String {
    format!(
        "ci-{}-{}",
        intake::now_millis(),
        REQUESTS.fetch_add(1, Ordering::Relaxed)
    )
}

fn conclusion_ok(conclusion: &str) -> bool {
    !conclusion.is_empty()
        && conclusion.len() <= 32
        && conclusion
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

fn kind_of(outcome: RunOutcome) -> Option<&'static str> {
    match outcome {
        RunOutcome::Passed => Some("ci_passed"),
        RunOutcome::Failed => Some("ci_failed"),
        RunOutcome::Cancelled => Some("ci_cancelled"),
        _ => None,
    }
}

/// The log block a failure leaves on the task. Names the workflow, the run, the
/// commit and the conclusion; it never carries the run's output, which can hold
/// secrets a workflow printed.
pub(crate) fn failure_block(
    branch: &str,
    workflow: &str,
    run: &crate::github::WorkflowRunInfo,
    at_ms: i64,
) -> String {
    let line = |value: &str| {
        value
            .chars()
            .filter(|c| !c.is_control())
            .collect::<String>()
    };
    format!(
        "--- CI failed on {} ({}) ---\nworkflow: {}\nconclusion: {}\nrun: {}\nhead: {}\nThis is GitHub's result for the attempt's branch; open the run for its output.",
        line(branch),
        intake::timestamp(at_ms),
        line(workflow),
        line(&run.conclusion),
        line(&run.url),
        line(&run.head_sha),
    )
}

/// The agent attempt a checkout is the worktree of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Attempt {
    pub task_id: String,
    pub run_id: String,
    pub branch: String,
}

/// The attempt whose working directory is `checkout` and whose branch is
/// `branch`, if the store holds one. Any state, any age: a person may run CI
/// in an attempt's worktree long after the agent left. Newest first, bounded
/// by [`MAX_PAGES`]; an attempt older than that is unknown, which is not the
/// same as "not an attempt", and is the caller's to report.
pub(crate) fn attempt_in(
    store: &Store,
    checkout: &Path,
    branch: &str,
) -> Result<Option<Attempt>, WorkbenchError> {
    let head_ref = format!("refs/heads/{branch}");
    if !head_ref.starts_with(ATTEMPT_PREFIX) {
        return Ok(None);
    }
    let root = checkout
        .canonicalize()
        .unwrap_or_else(|_| checkout.to_path_buf());
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut input = json!({"newest": true, "limit": 200});
        if let Some(cursor) = &cursor {
            input["cursor"] = json!(cursor);
        }
        let page = query(store, "runs.list", &input.to_string())?;
        let items = page["items"]
            .as_array()
            .ok_or_else(|| WorkbenchError::new("protocol_error", "Invalid run page."))?;
        for run in items {
            let (Some(cwd), Some(id), Some(task)) = (
                run["cwd"].as_str(),
                run["id"].as_str(),
                run["task_id"].as_str(),
            ) else {
                continue;
            };
            let cwd = Path::new(cwd);
            let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
            if run["head_ref"].as_str() == Some(head_ref.as_str()) && cwd.starts_with(&root) {
                return Ok(Some(Attempt {
                    task_id: task.to_owned(),
                    run_id: id.to_owned(),
                    branch: branch.to_owned(),
                }));
            }
        }
        match page["next_cursor"].as_str() {
            Some(next) if page["has_more"] == true => cursor = Some(next.into()),
            _ => break,
        }
    }
    Ok(None)
}

/// The attempt `repo_path` is the worktree of, for CI:local. `Ok(None)` when it
/// is not one: no attempt branch checked out, or no task profile yet. A profile
/// is never created by asking.
pub(crate) fn attempt_of(
    host: &WorkbenchState,
    repo_path: &str,
) -> Result<Option<Attempt>, WorkbenchError> {
    let local = intake::resolve(repo_path)?;
    let branch = match crate::engine::git_cli::git_text(
        Path::new(&local.path),
        &["symbolic-ref", "--short", "-q", "HEAD"],
    ) {
        Ok(branch) => branch.trim().to_owned(),
        // A detached HEAD is not on an attempt branch.
        Err(_) => return Ok(None),
    };
    if branch.is_empty() || !host.profile_path()?.exists() {
        return Ok(None);
    }
    host.with_store(|store| attempt_in(store, Path::new(&local.path), &branch))
}

/// Tell the store that a local CI run on an attempt's checkout finished.
/// `Ok(true)` when it made a new notice. The commit is the one the run's
/// verification note attests, so the signal never describes a tree the run did
/// not test.
pub(crate) fn record_local(
    host: &WorkbenchState,
    attempt: &Attempt,
    kind: &str,
    conclusion: &str,
    head_sha: &str,
) -> Result<bool, WorkbenchError> {
    let input = json!({
        "request_id": fresh_request_id(),
        "run_id": attempt.run_id,
        "task_id": attempt.task_id,
        "source": "ci_local",
        "external_id": head_sha,
        "kind": kind,
        "conclusion": conclusion,
        "workflow": "ci.local",
        "branch": attempt.branch,
        "head_sha": head_sha,
    });
    let answer = host.with_store(|store| query(store, "signals.record", &input.to_string()))?;
    let recorded = answer["recorded"] == true;
    if recorded {
        host.wake_notifications();
    }
    Ok(recorded)
}

/// Tell the store, and on a new failure leave the log block.
fn record(
    store: &Store,
    candidate: &Candidate,
    run: &crate::github::WorkflowRunInfo,
) -> Result<Recorded, WorkbenchError> {
    let Some(kind) = kind_of(run.outcome) else {
        return Ok(Recorded::Unrecordable(format!(
            "a {:?} run is not a result",
            run.outcome
        )));
    };
    let conclusion = run.conclusion.trim().to_ascii_lowercase();
    if !conclusion_ok(&conclusion) {
        return Ok(Recorded::Unrecordable(format!(
            "conclusion {:?} is not one the store accepts",
            run.conclusion
        )));
    }
    let head_sha = run.head_sha.trim().to_ascii_lowercase();
    if ![40, 64].contains(&head_sha.len()) || !head_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(Recorded::Unrecordable(
            "GitHub reported no usable head commit for the run".into(),
        ));
    }
    // The branch is what GitHub reported for the run, and it has to be the
    // attempt's: a signal must never name a branch the run was not on.
    let branch = run.head_branch.clone();
    if branch.is_empty() || candidate.branch.as_deref() != Some(branch.as_str()) {
        return Ok(Recorded::Unrecordable(format!(
            "the run ran on {:?}, not on the attempt's branch {:?}",
            run.head_branch, candidate.branch
        )));
    }
    let workflow = if run.name.trim().is_empty() {
        "workflow".to_owned()
    } else {
        run.name
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect()
    };
    let mut input = json!({
        "request_id": fresh_request_id(),
        "run_id": candidate.run_id,
        "task_id": candidate.task_id,
        "source": "github",
        "external_id": run.id.to_string(),
        "kind": kind,
        "conclusion": conclusion,
        "workflow": workflow,
        "branch": branch,
        "head_sha": head_sha,
    });
    if run.url.starts_with("https://")
        && !run.url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        input["url"] = json!(run.url);
    }
    let answer = query(store, "signals.record", &input.to_string())?;
    if answer["recorded"] != true {
        return Ok(Recorded::Repeat);
    }
    if run.outcome == RunOutcome::Failed {
        let block = failure_block(&branch, &workflow, run, intake::now_millis());
        if let Err(error) = leave_block(store, &candidate.task_id, &block) {
            // The notice exists; only the card's log is missing. Retrying the
            // signal would find it recorded and skip the block, so say so here.
            log::warn!(target: "workbench", "ci watch recorded the failure of task {} but could not write its log block: {}", candidate.task_id, error.message);
        }
    }
    Ok(Recorded::New)
}

/// Append `block` to the task's logs, re-reading the card when a person saved
/// it in between.
fn leave_block(store: &Store, task_id: &str, block: &str) -> Result<(), WorkbenchError> {
    for _ in 0..3 {
        let item = intake::get_item(store, task_id)?
            .ok_or_else(|| WorkbenchError::new("not_found", "The task no longer exists."))?;
        let revision = item["revision"].as_i64().unwrap_or_default();
        match intake::annotate_task(store, task_id, revision, None, block) {
            Ok(_) => return Ok(()),
            Err(error) if error.code == "revision_conflict" => continue,
            Err(error) => return Err(error),
        }
    }
    Err(WorkbenchError::new(
        "revision_conflict",
        "The task kept changing while its log was written.",
    ))
}

/// The live backend: GitHub through the existing loader, trust through the
/// repository trust gate, the clock, and the host's notification coordinator.
pub(crate) struct HostBackend(pub WorkbenchState);

impl Backend for HostBackend {
    fn trusted(&self, checkout: &str) -> Result<bool, String> {
        crate::repository_trust::inspect(checkout)
            .map(|preview| preview.scope.admits())
            .map_err(|error| error.into_message())
    }
    fn remote(&self, checkout: &str) -> Result<Option<String>, String> {
        crate::github::discover_github_remote(checkout)
            .map(|found| {
                found.map(|found| crate::github::outcome::repository_key(&found.owner, &found.name))
            })
            .map_err(|error| error.into_message())
    }
    fn runs(&self, checkout: &str) -> GitHubRunsReport {
        // Not the panel's loader: that one moves the panel's banner cursor.
        crate::github::watch_workflow_runs_report(checkout)
    }
    fn take_finished(&self, max: usize) -> Vec<Finished> {
        crate::github::outcome::with_observer(|observer| observer.take_pending(max))
    }
    fn dropped(&self) -> u64 {
        crate::github::outcome::with_observer(|observer| observer.dropped())
    }
    fn wake(&self) {
        self.0.wake_notifications();
    }
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn unix_now(&self) -> i64 {
        intake::now_millis() / 1000
    }
    fn cancelled(&self) -> bool {
        self.0.check_open().is_err()
    }
}

/// Start the watcher thread. A thread that fails to start costs automatic CI
/// news only: the Remote panel still shows runs.
pub(crate) fn install(host: WorkbenchState) {
    if let Err(error) = crate::supervisor::spawn_service("workbench-ci-watch", move || {
        let backend = HostBackend(host.clone());
        let mut watcher = Watcher::default();
        // Its own connection, like the verification driver's: a sweep waits on
        // `gh`, and the board's reads must not queue behind that.
        let mut store: Option<Store> = None;
        while host.check_open().is_ok() {
            if store.is_none() {
                store = host
                    .profile_path()
                    .ok()
                    .filter(|path| path.exists())
                    .and_then(|path| Store::open(&path).ok());
            }
            if let Some(open) = &store {
                let report = watcher.sweep(open, &backend);
                watcher.report_health();
                if report != Report::default() {
                    log::info!(target: "workbench", "ci watch: {report:?}");
                }
            }
            std::thread::sleep(SWEEP_PERIOD);
        }
    }) {
        log::warn!(target: "workbench", "ci watch did not start: {error}");
    }
}

#[cfg(test)]
#[path = "ci_watch_tests.rs"]
mod tests;
