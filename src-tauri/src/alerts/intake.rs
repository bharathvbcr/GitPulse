//! The producers' side of the worker's queue.
//!
//! Everything that reaches the notification worker goes through [`Intake`]: a
//! PTY reader's bell, the renderer's answer, a hook report from the socket. It
//! is the one place that decides, without ever blocking the caller, whether the
//! worker will see a notice, and it says which of the outcomes happened
//! ([`Offer`]) so the socket can answer honestly instead of acknowledging a
//! report that was thrown away.
//!
//! Three rules live here and nowhere else:
//!
//! * **Bells are the cheap class.** A bare bell restates "something happened"
//!   and the worker coalesces it anyway, so it is coalesced here first (one
//!   queued bell per session and content) and may only ever occupy
//!   [`BELL_SLOTS`] of the queue. The rest is reserved for hook reports and the
//!   renderer's answers, which say something a bell cannot. Without the
//!   reservation, one program writing `BEL` in a loop fills the queue and every
//!   report from every other session is refused.
//! * **Hook reports are ordered** by a stamp (see [`OrderBook`]).
//! * **A refusal is counted** in `dropped_queue` or `stale`, so offered and
//!   accounted-for still reconcile.

use super::{Counters, Notice, QUEUE};
#[cfg(unix)]
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Mutex};

/// Queue slots no bell may take, kept for hook reports and answers.
pub(super) const RESERVED_SLOTS: usize = 16;

/// Queue slots bells may occupy between them.
pub(super) const BELL_SLOTS: usize = QUEUE - RESERVED_SLOTS;

/// What became of a notice offered to the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    /// Queued for the worker.
    Accepted,
    /// Folded into an identical bell already queued for the session.
    Coalesced,
    /// Older than a report already applied for its session; dropped, counted
    /// as `stale`.
    Stale,
    /// No room in the queue; dropped, counted as `dropped_queue`.
    QueueFull,
    /// No worker has been started.
    NotRunning,
}

impl Offer {
    /// Whether the worker has, or will have, what was offered. A coalesced bell
    /// counts: an identical one is already on its way.
    pub fn taken(self) -> bool {
        matches!(self, Offer::Accepted | Offer::Coalesced)
    }
}

/// A bell's identity for coalescing: the session, the convention that carried
/// it and what it said. Two bells that differ in any of these are different
/// requests, and folding one into the other would lose the newer words.
type BellKey = (String, &'static str, Option<String>);

fn bell_key(notice: &Notice) -> BellKey {
    (notice.key.clone(), notice.channel, notice.detail.clone())
}

/// The bells currently in the queue, shared by the producers (which add) and
/// the worker (which removes one as it takes it off the queue).
#[derive(Default)]
pub(super) struct BellGate {
    queued: Mutex<HashSet<BellKey>>,
}

impl BellGate {
    /// The worker has taken `notice` off the queue. A no-op for anything that
    /// was not a bell.
    pub(super) fn release(&self, notice: &Notice) {
        if !notice.is_bare_signal() {
            return;
        }
        self.queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&bell_key(notice));
    }
}

/// The newest order stamp admitted per session key, and where each ended
/// session stopped.
///
/// Every report the bridge offers is state-bearing: each event in
/// [`super::bridge::EVENTS`] is a banner (`permission_prompt`, `idle_prompt`,
/// `agent_needs_input`, `agent_completed`, the two elicitation dialogs,
/// `error`), a state (`permission_request`, `turn_finished`) or a resolution
/// (`prompt_submitted`, `tool_finished`, `session_ended`). Bells carry no
/// stamp and never come through here, nor does the renderer's own `answered`.
///
/// Bounded two ways, each by [`ORDER_BOOK_CAP`]: the live map, whose oldest
/// entry makes room for a new session, and the tombstones.
///
/// **Tombstones.** A session's live entry goes when its `session_ended` is
/// admitted, but the stamp it ended at is kept, in its own map with the same
/// cap (the live map's eviction would have been shared with sessions still
/// running, and a tombstone must not push one of those out). A report for an
/// ended session whose stamp is older than the end is dropped as stale and
/// creates no entry: it was sent before the session ended and arrived after,
/// and applying it would bring the session back. A report newer than the end is
/// a new run under a reused key and clears the tombstone. Forgetting a tombstone
/// costs at most one late report for that session.
#[cfg(unix)]
#[derive(Default)]
pub(crate) struct OrderBook {
    pub(crate) newest: HashMap<String, u64>,
    pub(crate) ended: HashMap<String, u64>,
}

/// Most sessions the order book remembers, live and ended alike. The same bound
/// as the notifier's own tracking, which is what a report is about to be
/// tracked against.
#[cfg(unix)]
pub(crate) const ORDER_BOOK_CAP: usize = super::MAX_TRACKED;

#[cfg(unix)]
fn evict_oldest(map: &mut HashMap<String, u64>) {
    let oldest = map
        .iter()
        .min_by_key(|(_, seen)| **seen)
        .map(|(key, _)| key.clone());
    if let Some(oldest) = oldest {
        map.remove(&oldest);
    }
}

#[cfg(unix)]
impl OrderBook {
    /// Whether a report with `stamp` for `key` is current, and if so records it.
    pub(crate) fn admit(&mut self, key: &str, stamp: u64, ends_session: bool) -> bool {
        if let Some(&ended_at) = self.ended.get(key) {
            if stamp < ended_at {
                return false;
            }
            self.ended.remove(key);
        }
        if let Some(newest) = self.newest.get_mut(key) {
            if stamp < *newest {
                return false;
            }
            *newest = stamp;
        } else {
            if self.newest.len() >= ORDER_BOOK_CAP {
                evict_oldest(&mut self.newest);
            }
            self.newest.insert(key.to_owned(), stamp);
        }
        if ends_session {
            let ended_at = self.newest.remove(key).unwrap_or(stamp);
            if self.ended.len() >= ORDER_BOOK_CAP {
                evict_oldest(&mut self.ended);
            }
            self.ended.insert(key.to_owned(), ended_at);
        }
        true
    }
}

/// The sending half of the worker's queue, with the rules above.
pub(super) struct Intake {
    sender: mpsc::SyncSender<Notice>,
    counters: Arc<Counters>,
    bells: Arc<BellGate>,
    /// Held across the check, the high-water update and the hand-off to the
    /// queue, so no report with a lower stamp can slip in between a higher one
    /// being admitted and queued.
    #[cfg(unix)]
    order: Mutex<OrderBook>,
}

impl Intake {
    /// A queue of [`QUEUE`] slots, its intake and the worker's receiving half.
    pub(super) fn channel(counters: Arc<Counters>) -> (Self, mpsc::Receiver<Notice>) {
        let (sender, receiver) = mpsc::sync_channel(QUEUE);
        let intake = Self {
            sender,
            counters,
            bells: Arc::default(),
            #[cfg(unix)]
            order: Mutex::default(),
        };
        (intake, receiver)
    }

    /// What the worker calls as it takes a bell off the queue.
    pub(super) fn bells(&self) -> Arc<BellGate> {
        self.bells.clone()
    }

    /// Offers a notice without ever blocking the caller.
    pub(super) fn offer(&self, notice: Notice) -> Offer {
        if notice.is_bare_signal() {
            return self.offer_bell(notice);
        }
        self.enqueue(notice)
    }

    fn enqueue(&self, notice: Notice) -> Offer {
        if self.sender.try_send(notice).is_err() {
            self.counters.dropped_queue.fetch_add(1, Ordering::Relaxed);
            return Offer::QueueFull;
        }
        Offer::Accepted
    }

    fn offer_bell(&self, notice: Notice) -> Offer {
        let key = bell_key(&notice);
        let mut queued = self
            .bells
            .queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if queued.contains(&key) {
            self.counters.coalesced.fetch_add(1, Ordering::Relaxed);
            return Offer::Coalesced;
        }
        if queued.len() >= BELL_SLOTS {
            self.counters.dropped_queue.fetch_add(1, Ordering::Relaxed);
            return Offer::QueueFull;
        }
        // Recorded before the send, under the lock the worker's `release` also
        // takes, so a worker that dequeues it at once finds it recorded.
        queued.insert(key.clone());
        let offer = self.enqueue(notice);
        if offer != Offer::Accepted {
            queued.remove(&key);
        }
        offer
    }

    /// Offers a hook report with the stamp that orders it among its session's.
    ///
    /// The bridge reads connections in parallel, so two reports from one
    /// session that arrive back to back can reach here in either order, and a
    /// late `tool_finished` erases the "Finished" a `turn_finished` just set. A
    /// report older than the newest already seen for its session is dropped and
    /// counted as `stale`, never applied late and never waited for.
    #[cfg(unix)]
    pub(super) fn offer_stamped(&self, notice: Notice, stamp: u64) -> Offer {
        let mut book = self
            .order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ends = notice.event == Some("session_ended");
        if !book.admit(&notice.key, stamp, ends) {
            self.counters.stale.fetch_add(1, Ordering::Relaxed);
            return Offer::Stale;
        }
        self.enqueue(notice)
    }
}
