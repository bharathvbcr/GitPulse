//! The collision scan, remembered for a few seconds.
//!
//! `gitpulse-hook collision-guard` runs as a fresh process on every Edit and
//! Write, and each run read `git worktree list` and then `git status` in every
//! worktree of the repository. With a dozen agent worktrees that is a dozen
//! `git` children per edit, from a process that can keep nothing between calls;
//! on 2026-10-17 that fan-out was most of the exec traffic that saturated
//! syspolicyd (every exec is checked). The running app answers instead
//! ([`crate::hooks::serve_collision`]), and it can remember: this is the
//! memory.
//!
//! What is remembered, and what is not:
//!
//! - The worktree listing and each worktree's changed-path read are kept for
//!   [`TTL`]. Nothing cheaper can stamp them: a sibling's unstaged edit moves
//!   neither its index nor its HEAD, so (unlike `ref_cache`'s ref listings)
//!   there is no file to compare, and the age is the whole invalidation rule.
//! - **A read that failed is never kept**, and a failure seen while the listing
//!   came from memory throws the listing away and scans once more: a worktree
//!   removed in the last [`TTL`] must not leave the notice "1 worktree could
//!   not be read" behind.
//! - **A positive is never served from memory.** A scan that finds the edited
//!   file dirty in another worktree is re-read in exactly the worktrees it
//!   names before it is believed. `collision-guard` turns a positive into an
//!   `ask`, which a headless agent's host turns into a deny; a stale claim that
//!   another worktree "already has uncommitted changes" would block an edit on
//!   evidence that no longer holds. The re-read costs one `status` per named
//!   worktree, and only for a file that really is contended. The symbol
//!   verdict that refines that positive (shared symbol or disjoint symbols; a
//!   DevMap open and two diffs per worktree) is kept for [`TTL`] while the
//!   same worktrees are the parties: a hunk that moved in the last few seconds
//!   can leave it one verdict behind, the file-level claim never.
//! - What a *negative* answer can have missed is bounded by [`TTL`]: a sibling
//!   that dirtied the same file in the last few seconds. The unmemoized scan
//!   has the same blind spot over the scan's own duration (the sibling can edit
//!   a moment after it was read), so the memo widens a window that already
//!   existed rather than opening one, and the age is reported in the reply so
//!   the hook can refuse an answer older than it was promised.
//! - Concurrent callers for one key share one read: the slot is held while it
//!   is filled, so a burst of hooks costs one scan, not one each.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{
    collision_from_list_reading, empty_collisions, entity_collision, live_in, live_sessions,
    CollisionRisk, PathsRead,
};
use crate::engine::worktree::{self, changed_paths_within, WorktreeInfo};

/// How long a listing or a worktree's changed paths are served from memory.
///
/// Short on purpose: dirty state has no stamp (see the module note), so this is
/// the longest a sibling's new edit can go unseen by a *negative* answer.
/// `ref_cache`'s 60 s is right for ref listings and far too long for this.
pub const TTL: Duration = Duration::from_secs(5);

/// Repositories whose listing is kept at once.
const MAX_LISTS: usize = 32;
/// Worktrees whose changed paths are kept at once.
const MAX_READS: usize = 512;
/// Symbol verdicts kept at once.
const MAX_VERDICTS: usize = 128;

/// A collision scan and how old the oldest part of it was.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoizedCollision {
    pub risk: CollisionRisk,
    /// Age in milliseconds of the oldest read the answer was assembled from;
    /// 0 when everything was read for this call.
    pub age_ms: u64,
}

struct Slot<T> {
    held: Mutex<Option<(Instant, T)>>,
}

/// Keyed answers that expire by age, filled one caller at a time per key.
struct Memo<T> {
    cap: usize,
    slots: Mutex<HashMap<String, (u64, Arc<Slot<T>>)>>,
    touched: AtomicU64,
}

impl<T: Clone> Memo<T> {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            slots: Mutex::new(HashMap::new()),
            touched: AtomicU64::new(0),
        }
    }

    fn slot(&self, key: &str) -> Arc<Slot<T>> {
        let tick = self.touched.fetch_add(1, Ordering::Relaxed);
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((last, slot)) = slots.get_mut(key) {
            *last = tick;
            return Arc::clone(slot);
        }
        if slots.len() >= self.cap {
            // The key nobody has asked for longest.
            if let Some(stale) = slots
                .iter()
                .min_by_key(|(_, (last, _))| *last)
                .map(|(key, _)| key.clone())
            {
                slots.remove(&stale);
            }
        }
        let slot = Arc::new(Slot {
            held: Mutex::new(None),
        });
        slots.insert(key.to_string(), (tick, Arc::clone(&slot)));
        slot
    }

    /// The kept answer while it is younger than `ttl`, else `fill`'s, which is
    /// kept only when it succeeded. Returns the answer's age: zero when `fill`
    /// ran.
    fn get_or_fill(
        &self,
        key: &str,
        ttl: Duration,
        fill: impl FnOnce() -> Result<T, String>,
    ) -> (Result<T, String>, Duration) {
        let slot = self.slot(key);
        // Held across the fill: a second caller for this key waits for the
        // first and then finds its answer, which is the single flight.
        let mut held = slot.held.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, value)) = held.as_ref() {
            let age = at.elapsed();
            if age < ttl {
                return (Ok(value.clone()), age);
            }
        }
        match fill() {
            Ok(value) => {
                *held = Some((Instant::now(), value.clone()));
                (Ok(value), Duration::ZERO)
            }
            Err(error) => {
                *held = None;
                (Err(error), Duration::ZERO)
            }
        }
    }

    fn forget(&self, key: &str) {
        let slot = self
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .map(|(_, slot)| Arc::clone(slot));
        if let Some(slot) = slot {
            *slot.held.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
    }
}

pub(crate) struct CollisionMemo {
    ttl: Duration,
    lists: Memo<Vec<WorktreeInfo>>,
    reads: Memo<(Vec<String>, bool)>,
    verdicts: Memo<entity_collision::EntityCollisionVerdict>,
}

impl CollisionMemo {
    pub(crate) fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            lists: Memo::new(MAX_LISTS),
            reads: Memo::new(MAX_READS),
            verdicts: Memo::new(MAX_VERDICTS),
        }
    }

    /// The collision scan for an edit of `focus` in `repo_path`, with the
    /// symbol verdict for `focus` attached when it is contended.
    pub(crate) fn risk_for_path(
        &self,
        repo_path: &str,
        focus: &str,
        observation: &live_sessions::Observation,
    ) -> MemoizedCollision {
        let key = std::fs::canonicalize(repo_path)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| repo_path.to_string());
        let oldest = AtomicU64::new(0);
        let (mut risk, list_age) = self.assemble(&key, repo_path, observation, &oldest);

        // A failure beside a listing served from memory may be the listing
        // itself being out of date. One scan from scratch settles it; a failure
        // that survives is real and is reported as it always was.
        if !risk.ok && list_age > Duration::ZERO {
            self.lists.forget(&key);
            risk = self.assemble(&key, repo_path, observation, &oldest).0;
        }

        // A positive is re-read where it was found before it is believed.
        let contended: Vec<String> = risk
            .items
            .iter()
            .filter(|item| item.path == focus)
            .flat_map(|item| item.worktrees.iter().map(|party| party.path.clone()))
            .collect();
        if !contended.is_empty() {
            for path in &contended {
                self.reads.forget(path);
            }
            risk = self.assemble(&key, repo_path, observation, &oldest).0;
        }

        // The symbol verdict costs a DevMap open and two diffs per party. The
        // file-level positive above was just re-read, so what is kept here is
        // only the refinement of it (shared symbol, or disjoint symbols), and
        // only while the same worktrees are the parties.
        if let Some(item) = risk.items.iter_mut().find(|item| item.path == focus) {
            let mut parties: Vec<&str> = item.worktrees.iter().map(|p| p.path.as_str()).collect();
            parties.sort_unstable();
            let verdict_key = format!("{key}\0{focus}\0{}", parties.join("\0"));
            let (verdict, age) = self.verdicts.get_or_fill(&verdict_key, self.ttl, || {
                Ok(entity_collision::classify_overlapping_path(item))
            });
            oldest.fetch_max(age.as_millis() as u64, Ordering::Relaxed);
            item.entity = verdict.ok();
        }

        MemoizedCollision {
            risk,
            age_ms: oldest.load(Ordering::Relaxed),
        }
    }

    fn assemble(
        &self,
        key: &str,
        repo_path: &str,
        observation: &live_sessions::Observation,
        oldest: &AtomicU64,
    ) -> (CollisionRisk, Duration) {
        let (listed, list_age) = self
            .lists
            .get_or_fill(key, self.ttl, || worktree::list_worktrees_lite(repo_path));
        oldest.fetch_max(list_age.as_millis() as u64, Ordering::Relaxed);
        let list = match listed {
            Ok(list) => list,
            Err(error) => return (empty_collisions(error), list_age),
        };
        let read = |path: &str, _deadline: Option<&worktree::ScanDeadline>| -> PathsRead {
            // No deadline: a memoized read is shared, so one caller's clock must
            // not decide what the next one is told.
            let (read, age) = self
                .reads
                .get_or_fill(path, self.ttl, || changed_paths_within(path, None));
            oldest.fetch_max(age.as_millis() as u64, Ordering::Relaxed);
            read
        };
        let live = live_in(observation, &list);
        (
            collision_from_list_reading(&list, &live, None, &read),
            list_age,
        )
    }
}

fn shared() -> &'static CollisionMemo {
    static MEMO: OnceLock<CollisionMemo> = OnceLock::new();
    MEMO.get_or_init(|| CollisionMemo::new(TTL))
}

/// [`super::collision_risk_for_path`], answered from the app's memory where it
/// is young enough. Only the running app calls this: a hook process has no
/// memory between calls, so for it this would be the unmemoized scan.
pub fn collision_risk_for_path_memoized(repo_path: &str, path: &str) -> MemoizedCollision {
    shared().risk_for_path(repo_path, path, &live_sessions::observe())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_answer_is_served_until_it_is_old_and_a_failure_is_never_kept() {
        let memo: Memo<u32> = Memo::new(4);
        let fills = std::cell::Cell::new(0u32);
        let fill = |value: Result<u32, String>| {
            fills.set(fills.get() + 1);
            value
        };
        let ttl = Duration::from_secs(60);

        assert_eq!(memo.get_or_fill("a", ttl, || fill(Ok(1))).0, Ok(1));
        let (again, age) = memo.get_or_fill("a", ttl, || fill(Ok(2)));
        assert_eq!(again, Ok(1), "served from memory");
        assert!(age < ttl);
        assert_eq!(fills.get(), 1);

        // Past its age it is read again.
        assert_eq!(
            memo.get_or_fill("a", Duration::ZERO, || fill(Ok(3))).0,
            Ok(3)
        );

        // A failure is returned, not kept, and drops what was there.
        assert!(memo
            .get_or_fill("a", Duration::ZERO, || fill(Err("x".into())))
            .0
            .is_err());
        assert_eq!(memo.get_or_fill("a", ttl, || fill(Ok(4))).0, Ok(4));

        memo.forget("a");
        assert_eq!(memo.get_or_fill("a", ttl, || fill(Ok(5))).0, Ok(5));
    }

    #[test]
    fn the_memo_is_bounded_and_drops_the_key_asked_for_longest_ago() {
        let memo: Memo<u32> = Memo::new(2);
        let ttl = Duration::from_secs(60);
        let _ = memo.get_or_fill("old", ttl, || Ok(1));
        let _ = memo.get_or_fill("mid", ttl, || Ok(2));
        let _ = memo.get_or_fill("old", ttl, || Ok(9)); // touch: "mid" is now the stale one
        let _ = memo.get_or_fill("new", ttl, || Ok(3));
        assert_eq!(memo.slots.lock().unwrap().len(), 2);
        assert_eq!(memo.get_or_fill("old", ttl, || Ok(7)).0, Ok(1), "kept");
        assert_eq!(memo.get_or_fill("mid", ttl, || Ok(8)).0, Ok(8), "evicted");
    }

    #[test]
    fn concurrent_callers_for_one_key_share_one_fill() {
        let memo: Arc<Memo<u32>> = Arc::new(Memo::new(4));
        let fills = Arc::new(AtomicU64::new(0));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let (memo, fills) = (Arc::clone(&memo), Arc::clone(&fills));
                std::thread::spawn(move || {
                    memo.get_or_fill("k", Duration::from_secs(60), || {
                        fills.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(30));
                        Ok(1)
                    })
                    .0
                })
            })
            .collect();
        for worker in workers {
            assert_eq!(worker.join().unwrap(), Ok(1));
        }
        assert_eq!(fills.load(Ordering::SeqCst), 1);
    }
}
