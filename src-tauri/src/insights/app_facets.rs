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
    complete_snapshot, derive_facets, live_sessions, read_listing, AgentSummary, ChangesFacet,
    CollisionRisk, GitFacets, InsightsSnapshot, WorktreeFacet, MAX_SNAPSHOT_WORKTREES,
};
use crate::engine::git_cli::{with_admission, Admission};
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
    serve_bounded(canonical, HANDLER_DEADLINE, move || {
        // The class the asker's own scan runs in (`gitpulse-mcp` is a
        // background process), so agents' reads do not compete with the UI's.
        with_admission(Admission::Background, || {
            serve_with(
                &reading,
                &live_sessions::observe(),
                shared(),
                APP_TTL,
                APP_BUDGET,
            )
        })
    })
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
) -> AppFacets {
    let asked = Instant::now();
    // A clock that began `ttl` ago: the status cell then hands over reads that
    // began within `ttl`, and nothing older. It ends `budget` after the ask.
    let clock = ScanDeadline::after(asked.checked_sub(ttl).unwrap_or(asked), ttl + budget);
    let key = memo_key(repo);
    let (mut listing, mut list_age) = memo.listing(&key, repo);
    // The memo keeps a listing longer than an asker is promised. One older than
    // `ttl` is read again, so a worktree made or removed in the last few
    // seconds is not missing from a negative answer.
    if list_age > ttl {
        memo.forget_listing(&key);
        (listing, list_age) = memo.listing(&key, repo);
    }
    let mut read = read_listing(repo, &clock, Some(listing));
    // A failure beside a listing served from memory may be the listing being
    // out of date (a worktree removed in the last few seconds). One listing
    // from scratch settles it; a failure that survives is real.
    if list_age > Duration::ZERO && any_unreadable(&read.listed) {
        memo.forget_listing(&key);
        let (listing, age) = memo.listing(&key, repo);
        list_age = age;
        read = read_listing(repo, &clock, Some(listing));
    }
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
            refresh(list, &parties, &ScanDeadline::after(Instant::now(), budget));
            derived = derive_facets(
                repo,
                &clock,
                MAX_SNAPSHOT_WORKTREES,
                observation,
                &read.listed,
                read.deadline_expired,
            );
        }
    }

    let mut age = list_age;
    if let Ok(list) = &read.listed {
        for info in list {
            if let Some(read_age) = worktree::status_age(Path::new(&info.path)) {
                age = age.max(read_age);
            }
        }
    }

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

fn any_unreadable(listed: &Result<Vec<WorktreeInfo>, CommandError>) -> bool {
    listed.as_ref().is_ok_and(|list| {
        list.iter()
            .any(|info| matches!(info.status_read, StatusRead::Failed(_)))
    })
}

/// The paths of every worktree a positive collision names as a party: a file
/// dirty in two worktrees. A worktree that merely holds two live sessions
/// (`shared_worktrees`) is not a positive of this kind, and a busy checkout
/// always has several, so naming it here re-read it on nearly every ask. Its
/// dirty files are served at the age the reply states.
fn positive_parties(collisions: &CollisionRisk) -> Vec<String> {
    let mut parties: Vec<String> = collisions
        .items
        .iter()
        .flat_map(|item| item.worktrees.iter().map(|party| party.path.clone()))
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
