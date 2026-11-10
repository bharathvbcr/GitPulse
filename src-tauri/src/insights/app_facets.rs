//! The insights facets the running app answers a `gitpulse-mcp` process with.
//!
//! Every agent session has its own `gitpulse-mcp`, and each kept its own
//! `ref_cache` and `status_cell`, so N sessions asking `gitpulse_insights`
//! meant N independent scans of the same worktrees. The app already holds one
//! shared status cell and one listing memo ([`super::collision_memo`]) for the
//! collision-guard hook; this answers the git half of a snapshot (`worktrees`,
//! `changes`, `collisions`, and the agent counts attributed over the same
//! listing) from them, over the same mailbox, and the MCP's own scan stays as
//! the fallback for every way the app cannot or will not answer.
//!
//! What is and is not remembered, exactly as for the collision scan:
//!
//! - Trust is re-derived for every request ([`serve`] validates the repository
//!   before it touches a memory), because a listing served from memory skips
//!   the listing's own check.
//! - A status read is accepted from the cell when it began within
//!   [`TTL`](super::collision_memo::TTL); the reply says how old the oldest
//!   read it was assembled from is (`age_ms`), and the asker refuses one older
//!   than it was promised.
//! - **A positive is never served from memory.** When the collision facet
//!   names a worktree as a party (two worktrees dirty on one file, or two live
//!   sessions in one worktree), exactly those worktrees are read again before
//!   the answer is derived.
//! - A facet the app could not read is `None` with its reason in
//!   `not_scanned`. It is never an empty facet standing in for a scan that
//!   did not run, and the asker answers from its own scan instead.

use std::path::Path;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::collision_memo::{memo_key, shared, CollisionMemo};
use super::{
    changes_from_status, complete_snapshot, derive_facets, empty_changes, live_sessions,
    read_listing, AgentSummary, ChangesFacet, CollisionRisk, GitFacets, InsightsSnapshot,
    WorktreeFacet, MAX_SNAPSHOT_WORKTREES,
};
use crate::engine::git_cli::{with_admission, Admission};
use crate::engine::git_reader::GitReader;
use crate::engine::worktree::{self, ScanDeadline, StatusRead, WorktreeInfo};
use crate::ipc_error::CommandError;

/// How long a read or a listing may be, and still be handed to an asker.
///
/// Shorter than the collision scan's 5 s ([`super::collision_memo::TTL`]) on
/// purpose. A *negative* here ("nothing overlaps") is the answer an agent about
/// to edit acts on, and the asker's own scan would have been fresh at the call.
/// 2 s is long enough that the many MCP processes of many sessions asking in the
/// same moment share one set of reads, and short enough that the edit hook,
/// which is the gate and checks again at edit time, is the only place a
/// sibling's brand-new edit can go unseen for as long as 5 s. The reply says
/// how old its oldest read is.
pub const APP_TTL: Duration = Duration::from_secs(2);

/// How long the app spends reading for one request, from the moment it was
/// asked. Status reads that would start after it are not started, and the
/// worktrees they would have covered are reported unread.
pub const APP_BUDGET: Duration = Duration::from_millis(1200);

/// How long the handler waits for its own reading before it gives up and
/// answers that nothing was scanned: under the asker's 2 s wait, so the answer
/// arrives. The reading is abandoned, not cancelled (git has no handle to
/// cancel), so [`MAX_ABANDONED`] bounds how many may still be running.
pub const HANDLER_DEADLINE: Duration = Duration::from_millis(1600);

/// Abandoned readings that may still be running before the app refuses new ones.
const MAX_ABANDONED: usize = 4;

/// A facet the app did not answer, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotScanned {
    pub facet: String,
    pub reason: String,
}

/// The app's answer to [`crate::desktop::instance::Request::InsightsRead`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppFacets {
    /// The repository the answer is about, as the app resolved it (canonical).
    /// The asker refuses a reply about another one.
    pub repo: String,
    /// Age in milliseconds of the oldest read the answer was assembled from;
    /// 0 when everything was read for this request.
    pub age_ms: u64,
    pub worktrees: Option<WorktreeFacet>,
    /// The agent counts attributed over the same listing as `worktrees`.
    pub agents: Option<AgentSummary>,
    pub changes: Option<ChangesFacet>,
    pub collisions: Option<CollisionRisk>,
    pub deadline_expired: bool,
    /// Every facet above that is `None`, with the reason.
    pub not_scanned: Vec<NotScanned>,
}

/// The app's half: the git facets of `repo`, from its memory where that is
/// young enough. An error means no answer (the asker then scans itself);
/// per-facet gaps are in the reply.
pub fn serve(repo: &str) -> Result<AppFacets, String> {
    // Trust is decided here, before any memory is consulted, for every request.
    // Every read below is of the canonical path validation returned, never of
    // the string the peer sent, so a link swapped in afterwards is not followed.
    let canonical = crate::engine::validate_repo(repo)
        .map_err(CommandError::into_message)?
        .to_string_lossy()
        .into_owned();
    let reading = canonical.clone();
    let budget = scaled(APP_BUDGET);
    serve_bounded(canonical, scaled(HANDLER_DEADLINE), move || {
        // The class the asker's own scan runs in (`gitpulse-mcp` is a
        // background process), so agents' reads do not compete with the UI's.
        with_admission(Admission::Background, || {
            serve_with(
                &reading,
                &live_sessions::observe(),
                shared(),
                APP_TTL,
                budget,
                budget,
            )
        })
    })
}

/// `bound` as production uses it. Under test a literal sub-second bound that a
/// cold read of several worktrees must beat fails on a loaded host (a read of
/// seven worktrees took longer than 1.6 s at load 49), so tests scale it the
/// way the rest of the suite scales wall-clock limits.
#[cfg(not(test))]
fn scaled(bound: Duration) -> Duration {
    bound
}

#[cfg(test)]
fn scaled(bound: Duration) -> Duration {
    crate::test_support::coverage_relaxed(bound)
}

const RUNNING: u8 = 0;
const DONE: u8 = 1;
const ABANDONED_STATE: u8 = 2;

/// Readings the handler gave up on that have not ended yet.
static ABANDONED: AtomicUsize = AtomicUsize::new(0);

/// Runs `work` and waits `wait` for it. Past that the reading is abandoned
/// (its thread ends when git does) and every facet is answered not scanned, so
/// a hung `git` cannot hold the handler, and with it a mailbox slot, for git's
/// own 90 s cap. At most [`MAX_ABANDONED`] abandoned readings may be running
/// before new asks are refused.
fn serve_bounded(
    repo: String,
    wait: Duration,
    work: impl FnOnce() -> AppFacets + Send + 'static,
) -> Result<AppFacets, String> {
    serve_bounded_in(&ABANDONED, repo, wait, work)
}

/// [`serve_bounded`] counting its abandoned readings in `abandoned`.
fn serve_bounded_in(
    abandoned: &'static AtomicUsize,
    repo: String,
    wait: Duration,
    work: impl FnOnce() -> AppFacets + Send + 'static,
) -> Result<AppFacets, String> {
    if abandoned.load(Ordering::Acquire) >= MAX_ABANDONED {
        return Err(format!(
            "{MAX_ABANDONED} earlier reads are still running"
        ));
    }
    let state = Arc::new(AtomicU8::new(RUNNING));
    let (send, receive) = mpsc::channel();
    let worker_state = Arc::clone(&state);
    std::thread::Builder::new()
        .name("insights-read".into())
        .spawn(move || {
            let answer = work();
            let _ = send.send(answer);
            if worker_state
                .compare_exchange(RUNNING, DONE, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                abandoned.fetch_sub(1, Ordering::AcqRel);
            }
        })
        .map_err(|error| format!("cannot start the read: {error}"))?;
    match receive.recv_timeout(wait) {
        Ok(answer) => Ok(answer),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err("the read ended without an answer".into()),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            abandoned.fetch_add(1, Ordering::AcqRel);
            if state
                .compare_exchange(RUNNING, ABANDONED_STATE, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(unscanned(
                    repo,
                    &format!("the read did not finish within {} ms", wait.as_millis()),
                ));
            }
            // It finished between the timeout and the hand-over: take its answer.
            abandoned.fetch_sub(1, Ordering::AcqRel);
            receive
                .try_recv()
                .map_err(|_| "the read ended without an answer".to_string())
        }
    }
}

/// An answer that scanned nothing, with the reason for every facet.
fn unscanned(repo: String, reason: &str) -> AppFacets {
    AppFacets {
        repo,
        age_ms: 0,
        worktrees: None,
        agents: None,
        changes: None,
        collisions: None,
        deadline_expired: true,
        not_scanned: ["worktrees", "agents", "changes", "collisions"]
            .into_iter()
            .map(|facet| NotScanned {
                facet: facet.to_string(),
                reason: reason.to_string(),
            })
            .collect(),
    }
}

fn serve_with(
    repo: &str,
    observation: &live_sessions::Observation,
    memo: &CollisionMemo,
    ttl: Duration,
    budget: Duration,
    refresh_budget: Duration,
) -> AppFacets {
    let asked = Instant::now();
    // A clock that began `ttl` ago: the status cell then hands over reads that
    // began within `ttl`, and nothing older. It ends `budget` after the ask.
    let floor = asked.checked_sub(ttl).unwrap_or(asked);
    let clock = ScanDeadline::after(floor, ttl + budget);
    let key = memo_key(repo);
    let (mut listing, mut list_age) = memo.listing(&key, repo);
    let mut listed_at = Instant::now();
    // The memo keeps a listing longer than an asker is promised. One older than
    // `ttl` is read again, so a worktree made or removed in the last few
    // seconds is not missing from a negative answer.
    if list_age > ttl {
        memo.forget_listing(&key);
        (listing, list_age) = memo.listing(&key, repo);
        listed_at = Instant::now();
    }
    // Each read's start is stamped before it is made (see [`Stamps`]), so the
    // age reported is the age of what this answer used, not of what the cache
    // happens to hold when the answer is sent.
    let mut stamps = Stamps::capture(&listing, repo, asked, floor);
    let mut read = read_listing(repo, &clock, Some(listing));
    // A failure beside a listing served from memory may be the listing being
    // out of date (a worktree removed in the last few seconds). One listing
    // from scratch settles it; a failure that survives is real.
    if list_age > Duration::ZERO && any_unreadable(&read.listed) {
        memo.forget_listing(&key);
        let (listing, age) = memo.listing(&key, repo);
        list_age = age;
        listed_at = Instant::now();
        stamps = Stamps::capture(&listing, repo, asked, floor);
        read = read_listing(repo, &clock, Some(listing));
    }
    let list_began = listed_at.checked_sub(list_age).unwrap_or(floor);
    let mut derived = derive_facets(
        repo,
        &clock,
        MAX_SNAPSHOT_WORKTREES,
        observation,
        &read.listed,
        read.deadline_expired,
    );

    // A positive is read again where it was found before it is believed.
    let parties = positive_parties(&derived.collisions);
    if !parties.is_empty() {
        if let Ok(list) = &mut read.listed {
            let reread = Reread::new(Instant::now(), refresh_budget.min(clock.remaining()));
            refresh(list, &parties, &reread.reading());
            stamps.reread(&parties, reread.begun);
            // The checkout `changes` describes was read before the re-read.
            // When it is one of the positives its count is taken from the
            // re-read, so the row's dirty count and `changes` are one moment;
            // when the re-read did not complete, `changes` is not scanned,
            // never the kept read.
            if parties.iter().any(|party| super::same_path(party, repo)) {
                let reread_ok = list.iter().any(|info| {
                    super::same_path(&info.path, repo)
                        && matches!(info.status_read, StatusRead::Read { .. })
                });
                read.changes = if reread_ok {
                    match GitReader::get_status_since(repo, reread.begun) {
                        Ok(files) => changes_from_status(&files, false),
                        Err(error) => empty_changes(error.into_message()),
                    }
                } else {
                    empty_changes("the re-read of this checkout did not finish in time")
                };
                stamps.changes = reread.begun;
            }
            derived = derive_facets(
                repo,
                &reread.deriving(&clock),
                MAX_SNAPSHOT_WORKTREES,
                observation,
                &read.listed,
                read.deadline_expired,
            );
        }
    }

    // Taken here, with the facets just derived, from the stamps of the reads
    // they stand on: nothing a concurrent asker does to the cache afterwards
    // can lower it.
    let age = stamps.oldest(Instant::now(), list_began, &read.listed);

    let mut not_scanned = Vec::new();
    let mut gap = |facet: &str, reason: &str| {
        not_scanned.push(NotScanned {
            facet: facet.to_string(),
            reason: if reason.is_empty() {
                "unknown".to_string()
            } else {
                reason.to_string()
            },
        });
    };
    let worktrees = if derived.worktrees.ok {
        Some(derived.worktrees)
    } else {
        gap("worktrees", &derived.worktrees.error);
        None
    };
    let agents = if derived.agents.ok {
        Some(derived.agents)
    } else {
        gap("agents", "the worktree listing did not run");
        None
    };
    let changes = if read.changes.ok {
        Some(read.changes)
    } else {
        gap("changes", &read.changes.error);
        None
    };
    // A scan that read no worktree is not a scan; one that read some says so
    // itself (`ok`, `unscanned_worktrees`, `failed_worktrees`).
    let collisions = if derived.collisions.scanned_worktrees > 0 {
        Some(derived.collisions)
    } else {
        gap("collisions", &derived.collisions.error);
        None
    };
    AppFacets {
        repo: repo.to_string(),
        age_ms: age.as_millis() as u64,
        worktrees,
        agents,
        changes,
        collisions,
        deadline_expired: derived.deadline_expired,
        not_scanned,
    }
}

/// How old the status read kept for `dir` is, as the cache reports it.
#[cfg(not(test))]
fn kept_age(dir: &Path) -> Option<Duration> {
    worktree::status_age(dir)
}

/// [`kept_age`] with the test seam: a test installs a probe to say how old the
/// reads are, so an age can be driven without waiting for it.
#[cfg(test)]
fn kept_age(dir: &Path) -> Option<Duration> {
    test_ages::probe(dir).unwrap_or_else(|| worktree::status_age(dir))
}

#[cfg(test)]
mod test_ages {
    use std::cell::RefCell;
    use std::path::Path;
    use std::time::Duration;

    type Probe = Box<dyn Fn(&Path) -> Option<Duration>>;

    thread_local! {
        static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
    }

    /// The installed probe's answer, if one is installed on this thread.
    pub(super) fn probe(dir: &Path) -> Option<Option<Duration>> {
        PROBE.with(|probe| probe.borrow().as_ref().map(|answer| answer(dir)))
    }

    /// Runs `body` with `answer` as the cache's report of every read's age.
    pub(super) fn with_ages<T>(
        answer: impl Fn(&Path) -> Option<Duration> + 'static,
        body: impl FnOnce() -> T,
    ) -> T {
        PROBE.with(|probe| *probe.borrow_mut() = Some(Box::new(answer)));
        let result = body();
        PROBE.with(|probe| *probe.borrow_mut() = None);
        result
    }
}

/// When each read this answer stands on began.
///
/// `status_age` says what the shared cache holds *now*, which is wrong as the
/// age of an answer on both sides: a concurrent asker refreshing it lowers it,
/// and a re-read of one checkout says nothing of the others. So each
/// ingredient is stamped before it is read: a kept read that the clock will
/// accept began `age` before the stamp (a newer one may be handed over, which
/// only makes the stamp conservative), and one it will not accept is read for
/// this ask, which begins no earlier than the ask. Re-read ingredients are
/// restamped with the instant their re-read began.
struct Stamps {
    changes: Instant,
    worktrees: Vec<(String, Instant)>,
}

impl Stamps {
    fn capture(
        listed: &Result<Vec<WorktreeInfo>, CommandError>,
        repo: &str,
        asked: Instant,
        floor: Instant,
    ) -> Self {
        let stamped = Instant::now();
        let began = |path: &str| match kept_age(Path::new(path)) {
            // Accepted by a clock that began at `floor`.
            Some(age) if stamped.checked_sub(age).is_some_and(|began| began >= floor) => {
                stamped.checked_sub(age).unwrap_or(floor)
            }
            _ => asked,
        };
        Self {
            changes: began(repo),
            worktrees: listed
                .as_ref()
                .map(|list| {
                    list.iter()
                        .filter(|info| !info.is_bare)
                        .map(|info| (info.path.clone(), began(&info.path)))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// The worktrees in `named` were read again, beginning at `begun`.
    fn reread(&mut self, named: &[String], begun: Instant) {
        for (path, began) in &mut self.worktrees {
            if named.contains(path) {
                *began = begun;
            }
        }
    }

    /// The age at `now` of the oldest read the facets stand on: the listing,
    /// the `changes` read, and every worktree status that was read.
    fn oldest(
        &self,
        now: Instant,
        list_began: Instant,
        listed: &Result<Vec<WorktreeInfo>, CommandError>,
    ) -> Duration {
        let mut began = list_began.min(self.changes);
        if let Ok(list) = listed {
            for info in list {
                if !matches!(info.status_read, StatusRead::Read { .. }) {
                    continue;
                }
                if let Some((_, at)) = self.worktrees.iter().find(|(path, _)| *path == info.path) {
                    began = began.min(*at);
                }
            }
        }
        now.saturating_duration_since(began)
    }
}

/// The one end both the re-read of a positive and the derive that follows it
/// answer to. The derive may not read past the re-read's own bound (a positive
/// that could not be read again in time is reported unread, never read late
/// and never served from the kept read the re-read replaced), so the two
/// deadlines are built from this single instant and cannot be given different
/// ends.
struct Reread {
    begun: Instant,
    end: Instant,
}

impl Reread {
    fn new(begun: Instant, allowed: Duration) -> Self {
        Self {
            begun,
            end: begun.checked_add(allowed).unwrap_or(begun),
        }
    }

    /// The deadline the re-read runs under.
    fn reading(&self) -> ScanDeadline {
        ScanDeadline::after(self.begun, self.end.saturating_duration_since(self.begun))
    }

    /// The deadline the derive runs under: the pass's own start, this end.
    fn deriving(&self, clock: &ScanDeadline) -> ScanDeadline {
        ScanDeadline::after(
            clock.started(),
            self.end.saturating_duration_since(clock.started()),
        )
    }
}

fn any_unreadable(listed: &Result<Vec<WorktreeInfo>, CommandError>) -> bool {
    listed.as_ref().is_ok_and(|list| {
        list.iter()
            .any(|info| matches!(info.status_read, StatusRead::Failed(_)))
    })
}

/// The paths of every worktree a positive names as a party: a file dirty in
/// two worktrees, or a worktree two or more live sessions share whose kept read
/// shows it dirty. A shared worktree whose kept read is clean is not a
/// positive and is not read again (a busy checkout always has several
/// sessions, so naming it regardless re-read it on every ask for nothing).
fn positive_parties(collisions: &CollisionRisk) -> Vec<String> {
    let mut parties: Vec<String> = collisions
        .items
        .iter()
        .flat_map(|item| item.worktrees.iter().map(|party| party.path.clone()))
        .chain(
            collisions
                .shared_worktrees
                .iter()
                .filter(|shared| shared.scanned && !shared.files.is_empty())
                .map(|shared| shared.path.clone()),
        )
        .collect();
    parties.sort_unstable();
    parties.dedup();
    parties
}

/// Reads `named` worktrees of `list` again, whatever the cell holds.
fn refresh(list: &mut [WorktreeInfo], named: &[String], fresh: &ScanDeadline) {
    let mut again: Vec<WorktreeInfo> = list
        .iter()
        .filter(|info| named.contains(&info.path))
        .cloned()
        .collect();
    worktree::read_dirty(&mut again, fresh);
    for done in again {
        if let Some(slot) = list.iter_mut().find(|info| info.path == done.path) {
            slot.dirty_files = done.dirty_files;
            slot.scan_note = done.scan_note;
            slot.status_read = done.status_read;
        }
    }
}

/// The asker's half: a snapshot from the app's answer, with the ledger and code
/// graph read here (neither starts a `git` child). All four git facets must be
/// present; one the app did not scan is an error naming it, and the asker then
/// scans for itself rather than splice a partial answer.
pub fn snapshot_from_app(
    repo_path: &str,
    started: Instant,
    facets: AppFacets,
) -> Result<InsightsSnapshot, String> {
    let AppFacets {
        worktrees,
        agents,
        changes,
        collisions,
        deadline_expired,
        not_scanned,
        ..
    } = facets;
    match (worktrees, agents, changes, collisions) {
        (Some(worktrees), Some(agents), Some(changes), Some(collisions)) => {
            // The asker's own scan has a longer deadline than the app's read,
            // so an answer with worktrees unread, failed or cut off is worse
            // than the scan it would replace.
            let mut partial = Vec::new();
            if deadline_expired {
                partial.push("the read ran out of time".to_string());
            }
            if collisions.unscanned_worktrees > 0 {
                partial.push(format!(
                    "{} worktree(s) unscanned for collisions",
                    collisions.unscanned_worktrees
                ));
            }
            if collisions.failed_worktrees > 0 {
                partial.push(format!(
                    "{} worktree(s) failed to read",
                    collisions.failed_worktrees
                ));
            }
            if worktrees.dirty_unknown > 0 {
                partial.push(format!(
                    "{} worktree(s) with no dirty count",
                    worktrees.dirty_unknown
                ));
            }
            if !partial.is_empty() {
                return Err(format!("the app's answer is partial: {}", partial.join(", ")));
            }
            Ok(complete_snapshot(
                repo_path,
                started,
                GitFacets {
                    worktrees,
                    agents,
                    changes,
                    collisions,
                    deadline_expired,
                },
            ))
        }
        _ => Err(format!(
            "the app did not scan: {}",
            not_scanned
                .iter()
                .map(|gap| format!("{} ({})", gap.facet, gap.reason))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[cfg(test)]
#[path = "app_facets_tests.rs"]
mod tests;
