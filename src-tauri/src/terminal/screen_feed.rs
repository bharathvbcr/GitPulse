//! The hand-off between terminal output delivery and the headless screen parse.
//!
//! A tracked session (an orchestrated child or a plain shell) keeps a `vt100`
//! screen, a command-block recorder and an OSC scanner so `Read` and `Wait` can
//! answer from it. Feeding those costs real CPU: a flood of output spends most
//! of its time in `vt100::Parser::process`. Output delivery runs on one thread
//! shared by every session and must reach the view first, so it never parses.
//! It [`push`](ScreenFeed::push)es the bytes here and returns; one worker
//! [`take`](ScreenFeed::take)s them, in order per session, and applies them.
//!
//! # The bound
//!
//! Bytes awaiting the parse are bounded per session ([`SESSION_BACKLOG_BYTES`])
//! and in all ([`TOTAL_BACKLOG_BYTES`]). Past a bound the **oldest** queued
//! bytes are dropped, never the newest and never a wait: delivery is not
//! slowed and memory does not grow. The drop is a *gap*. A parser fed across a
//! gap can be inside an escape sequence the dropped bytes opened, so the next
//! batch carries `resync`: the worker replaces the screen and the block
//! recorder with fresh ones before applying it. Every dropped byte is counted
//! ([`Lag::gap_bytes`]) and readers report it, so a screen built across a gap
//! says so instead of passing for complete.
//!
//! Counters are in stream bytes: `delivered` counts every byte pushed,
//! `parsed` every byte whose effect is on the screen or was dropped as a gap,
//! so `delivered - parsed` is exactly the bytes still owed to the parse.

use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex};

use crate::supervisor::lock_or_recover;

/// Bytes one session may have queued. At the parser's speed this is a few tens
/// of milliseconds of work, which is the longest a screen can trail the bytes
/// delivered before it starts dropping the oldest of them.
pub(super) const SESSION_BACKLOG_BYTES: usize = 1024 * 1024;

/// Bytes every session together may have queued, so many busy sessions cannot
/// add up to an unbounded amount even though each is within its own bound.
pub(super) const TOTAL_BACKLOG_BYTES: usize = 16 * 1024 * 1024;

/// Most bytes one [`Batch`] carries: the registry lock is held for one batch's
/// parse, so this is the longest a reader of the screen waits behind it.
pub(super) const BATCH_BYTES: usize = 16 * 1024;

/// One run of a session's bytes for the worker to apply.
#[derive(Debug)]
pub(super) struct Batch {
    pub session: String,
    /// Compared with the feed's own before applying: a reset since this was
    /// taken means these bytes belong to a screen that no longer exists.
    pub epoch: u64,
    pub bytes: Vec<u8>,
    /// The screen must be replaced before `bytes` are applied: bytes were
    /// dropped (or a parse failed) ahead of them.
    pub resync: bool,
    /// Bytes dropped ahead of this batch; they count as parsed once it is.
    pub dropped_before: u64,
    /// The session's output is scanned for OSC telemetry here (a host session);
    /// an in-process session is scanned by its own reader thread.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub scan: bool,
}

/// What a reader may learn about one session's screen without waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Lag {
    /// Stream bytes delivered so far.
    pub delivered: u64,
    /// Stream bytes the screen has caught up to.
    pub parsed: u64,
    /// Bytes dropped because the parse could not keep up since the screen was
    /// last rebuilt from a replay. Non-zero means the screen is incomplete.
    pub gap_bytes: u64,
}

impl Lag {
    /// Bytes delivered that the screen does not reflect yet.
    pub fn lag_bytes(&self) -> u64 {
        self.delivered.saturating_sub(self.parsed)
    }

    /// Whether the screen reflects every byte delivered, with none dropped.
    pub fn complete(&self) -> bool {
        self.lag_bytes() == 0 && self.gap_bytes == 0
    }
}

#[derive(Default)]
struct SessionFeed {
    chunks: VecDeque<Vec<u8>>,
    queued: usize,
    delivered: u64,
    parsed: u64,
    gap_bytes: u64,
    /// Dropped since the last batch was taken; carried by the next one.
    unsynced_dropped: u64,
    need_resync: bool,
    scan: bool,
    scheduled: bool,
    epoch: u64,
}

#[derive(Default)]
struct FeedState {
    sessions: HashMap<String, SessionFeed>,
    /// Sessions with work, each once: a busy session goes to the back after one
    /// batch, so a flood cannot starve another session's echo.
    ready: VecDeque<String>,
    queued_total: usize,
    shutdown: bool,
}

/// The queues between delivery and the worker. Holds its own lock for
/// constant-time bookkeeping only; nothing here ever parses.
#[derive(Default)]
pub(super) struct ScreenFeed {
    state: Mutex<FeedState>,
    work: Condvar,
}

impl ScreenFeed {
    /// Starts (or restarts, empty) the feed for a tracked session.
    pub(super) fn register(&self, session: &str) {
        let mut st = lock_or_recover(&self.state);
        let epoch = match st.sessions.remove(session) {
            Some(old) => {
                st.queued_total = st.queued_total.saturating_sub(old.queued);
                old.epoch + 1
            }
            None => 0,
        };
        st.sessions.insert(
            session.to_owned(),
            SessionFeed {
                epoch,
                ..SessionFeed::default()
            },
        );
    }

    /// Drops a session's queue; its screen is gone.
    pub(super) fn forget(&self, session: &str) {
        let mut st = lock_or_recover(&self.state);
        if let Some(old) = st.sessions.remove(session) {
            st.queued_total = st.queued_total.saturating_sub(old.queued);
        }
    }

    /// Queues `bytes` for the parse and returns at once. Unknown sessions are
    /// ignored (an untracked session has no screen). Never blocks on the
    /// worker: past a bound the oldest queued bytes are dropped as a gap.
    pub(super) fn push(&self, session: &str, bytes: &[u8], scan: bool) {
        if bytes.is_empty() {
            return;
        }
        let mut guard = lock_or_recover(&self.state);
        let FeedState {
            sessions,
            ready,
            queued_total,
            ..
        } = &mut *guard;
        let Some(feed) = sessions.get_mut(session) else {
            return;
        };
        feed.delivered = feed.delivered.saturating_add(bytes.len() as u64);
        feed.scan = scan;
        let mut dropped = 0u64;
        let mut keep = bytes;
        if keep.len() > SESSION_BACKLOG_BYTES {
            let cut = keep.len() - SESSION_BACKLOG_BYTES;
            dropped += cut as u64;
            keep = &keep[cut..];
        }
        // Make room by dropping the oldest of this session's own bytes.
        while feed.queued + keep.len() > SESSION_BACKLOG_BYTES
            || *queued_total + keep.len() > TOTAL_BACKLOG_BYTES
        {
            let Some(old) = feed.chunks.pop_front() else {
                break;
            };
            feed.queued -= old.len();
            *queued_total -= old.len();
            dropped += old.len() as u64;
        }
        // Still over the total with nothing of its own left to drop: the other
        // sessions hold the budget, so these bytes are the gap.
        if *queued_total + keep.len() > TOTAL_BACKLOG_BYTES {
            dropped += keep.len() as u64;
            keep = &[];
        }
        if dropped > 0 {
            feed.gap_bytes = feed.gap_bytes.saturating_add(dropped);
            feed.unsynced_dropped = feed.unsynced_dropped.saturating_add(dropped);
            feed.need_resync = true;
        }
        if !keep.is_empty() {
            feed.chunks.push_back(keep.to_vec());
            feed.queued += keep.len();
            *queued_total += keep.len();
        }
        if !feed.scheduled && (feed.queued > 0 || feed.need_resync) {
            feed.scheduled = true;
            ready.push_back(session.to_owned());
            drop(guard);
            self.work.notify_one();
        }
    }

    /// Blocks until a session has bytes to apply and returns up to
    /// [`BATCH_BYTES`] of them, or `None` once the feed is shut down.
    pub(super) fn take(&self) -> Option<Batch> {
        let mut guard = lock_or_recover(&self.state);
        loop {
            if guard.shutdown {
                return None;
            }
            let st = &mut *guard;
            let Some(session) = st.ready.pop_front() else {
                guard = self
                    .work
                    .wait(guard)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                continue;
            };
            let Some(feed) = st.sessions.get_mut(&session) else {
                continue;
            };
            let mut bytes = Vec::new();
            while bytes.len() < BATCH_BYTES {
                let Some(front) = feed.chunks.front_mut() else {
                    break;
                };
                let room = BATCH_BYTES - bytes.len();
                if front.len() <= room {
                    bytes.extend_from_slice(front);
                    feed.chunks.pop_front();
                } else {
                    bytes.extend_from_slice(&front[..room]);
                    front.drain(..room);
                }
            }
            feed.queued -= bytes.len();
            st.queued_total -= bytes.len();
            let batch = Batch {
                session: session.clone(),
                epoch: feed.epoch,
                resync: std::mem::take(&mut feed.need_resync),
                dropped_before: std::mem::take(&mut feed.unsynced_dropped),
                scan: feed.scan,
                bytes,
            };
            if feed.queued > 0 {
                st.ready.push_back(session);
            } else {
                feed.scheduled = false;
            }
            return Some(batch);
        }
    }

    /// Whether `batch` still belongs to its session's screen. The caller holds
    /// the registry lock, which every reset also holds, so the answer stays
    /// true until it has applied the batch.
    pub(super) fn is_current(&self, batch: &Batch) -> bool {
        lock_or_recover(&self.state)
            .sessions
            .get(&batch.session)
            .is_some_and(|f| f.epoch == batch.epoch)
    }

    /// Records that `batch` is on the screen.
    pub(super) fn applied(&self, batch: &Batch) {
        if let Some(feed) = lock_or_recover(&self.state)
            .sessions
            .get_mut(&batch.session)
            .filter(|f| f.epoch == batch.epoch)
        {
            feed.parsed = feed
                .parsed
                .saturating_add(batch.dropped_before)
                .saturating_add(batch.bytes.len() as u64);
        }
    }

    /// The parse of `batch` failed. Its bytes are lost, so the screen is
    /// untrustworthy: they count as a gap and the next batch resyncs.
    pub(super) fn failed(&self, batch: &Batch) {
        let lost = batch.dropped_before + batch.bytes.len() as u64;
        let mut st = lock_or_recover(&self.state);
        if let Some(feed) = st
            .sessions
            .get_mut(&batch.session)
            .filter(|f| f.epoch == batch.epoch)
        {
            feed.parsed = feed.parsed.saturating_add(lost);
            feed.gap_bytes = feed.gap_bytes.saturating_add(lost);
            feed.need_resync = true;
        }
    }

    /// The screen was rebuilt from a replay (or from a transcript): everything
    /// queued is covered by it, so it is dropped, and the gap count restarts.
    /// Counted as parsed so a reader waiting on those bytes is released.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(super) fn reset(&self, session: &str) {
        let mut guard = lock_or_recover(&self.state);
        let st = &mut *guard;
        if let Some(feed) = st.sessions.get_mut(session) {
            st.queued_total = st.queued_total.saturating_sub(feed.queued);
            feed.chunks.clear();
            feed.queued = 0;
            feed.epoch += 1;
            feed.parsed = feed.delivered;
            feed.gap_bytes = 0;
            feed.unsynced_dropped = 0;
            feed.need_resync = false;
        }
    }

    /// The counters for `session`, or `None` if it has no feed.
    pub(super) fn lag(&self, session: &str) -> Option<Lag> {
        lock_or_recover(&self.state)
            .sessions
            .get(session)
            .map(|f| Lag {
                delivered: f.delivered,
                parsed: f.parsed,
                gap_bytes: f.gap_bytes,
            })
    }

    /// Stops the worker: [`take`](Self::take) returns `None` from now on.
    pub(super) fn shutdown(&self) {
        lock_or_recover(&self.state).shutdown = true;
        self.work.notify_all();
    }

    /// Bytes queued across all sessions.
    #[cfg(test)]
    pub(super) fn queued_total(&self) -> usize {
        lock_or_recover(&self.state).queued_total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(feed: &ScreenFeed, session: &str) -> (Vec<u8>, bool, u64) {
        let mut all = Vec::new();
        let (mut resync, mut dropped) = (false, 0);
        loop {
            let pending = {
                let st = lock_or_recover(&feed.state);
                st.sessions
                    .get(session)
                    .is_some_and(|f| f.queued > 0 || f.need_resync)
            };
            if !pending {
                return (all, resync, dropped);
            }
            let batch = feed.take().expect("not shut down");
            assert!(batch.bytes.len() <= BATCH_BYTES);
            resync |= batch.resync;
            dropped += batch.dropped_before;
            all.extend_from_slice(&batch.bytes);
            feed.applied(&batch);
        }
    }

    #[test]
    fn bytes_come_out_in_the_order_they_went_in_across_batches() {
        let feed = ScreenFeed::default();
        feed.register("a");
        let mut expected = Vec::new();
        for i in 0..40u32 {
            let chunk: Vec<u8> = (0..1500).map(|j| ((i + j) % 251) as u8).collect();
            feed.push("a", &chunk, true);
            expected.extend_from_slice(&chunk);
        }
        let (got, resync, dropped) = drain(&feed, "a");
        assert_eq!(got, expected);
        assert!(!resync);
        assert_eq!(dropped, 0);
        let lag = feed.lag("a").unwrap();
        assert!(lag.complete(), "{lag:?}");
        assert_eq!(lag.delivered, expected.len() as u64);
    }

    #[test]
    fn a_flood_past_the_bound_drops_the_oldest_counts_the_gap_and_never_grows() {
        let feed = ScreenFeed::default();
        feed.register("a");
        let chunk = vec![b'x'; 64 * 1024];
        let pushes = 100; // 6.4 MiB against a 1 MiB bound, with nothing taking
        for _ in 0..pushes {
            feed.push("a", &chunk, true);
            assert!(feed.queued_total() <= SESSION_BACKLOG_BYTES);
        }
        let lag = feed.lag("a").unwrap();
        assert_eq!(lag.delivered, (pushes * chunk.len()) as u64);
        assert_eq!(
            lag.gap_bytes,
            lag.delivered - SESSION_BACKLOG_BYTES as u64,
            "everything beyond the bound is counted as dropped: {lag:?}"
        );
        assert!(!lag.complete());
        let (got, resync, dropped) = drain(&feed, "a");
        assert_eq!(got.len(), SESSION_BACKLOG_BYTES);
        assert!(resync, "the screen must be rebuilt across a gap");
        assert_eq!(dropped, lag.gap_bytes);
        let after = feed.lag("a").unwrap();
        assert_eq!(after.lag_bytes(), 0, "{after:?}");
        assert_eq!(after.gap_bytes, lag.gap_bytes, "the gap stays reported");
    }

    #[test]
    fn the_newest_bytes_survive_a_gap() {
        let feed = ScreenFeed::default();
        feed.register("a");
        for i in 0..40u8 {
            feed.push("a", &vec![i; 64 * 1024], true);
        }
        let (got, _, _) = drain(&feed, "a");
        assert_eq!(*got.last().unwrap(), 39, "the tail is what a reader wants");
        assert!(got.windows(2).all(|w| w[0] <= w[1]), "still in order");
    }

    #[test]
    fn one_session_cannot_take_the_whole_total() {
        let feed = ScreenFeed::default();
        let ids: Vec<String> = (0..40).map(|i| format!("s{i}")).collect();
        for id in &ids {
            feed.register(id);
        }
        let chunk = vec![b'y'; SESSION_BACKLOG_BYTES];
        for id in &ids {
            feed.push(id, &chunk, true);
            assert!(feed.queued_total() <= TOTAL_BACKLOG_BYTES);
        }
        // The first 16 fit; the rest could not and are gaps, not growth.
        let gapped = ids
            .iter()
            .filter(|id| feed.lag(id).unwrap().gap_bytes > 0)
            .count();
        assert_eq!(gapped, 40 - TOTAL_BACKLOG_BYTES / SESSION_BACKLOG_BYTES);
        for id in &ids {
            drain(&feed, id);
            assert_eq!(feed.lag(id).unwrap().lag_bytes(), 0, "{id}");
        }
    }

    #[test]
    fn a_busy_session_takes_turns_with_another() {
        let feed = ScreenFeed::default();
        feed.register("flood");
        feed.register("echo");
        feed.push("flood", &vec![b'f'; 8 * BATCH_BYTES], true);
        feed.push("echo", b"k", true);
        let first = feed.take().unwrap();
        let second = feed.take().unwrap();
        assert_eq!(first.session, "flood");
        assert_eq!(second.session, "echo", "echo waits one batch, not the flood");
    }

    #[test]
    fn a_reset_discards_what_was_queued_and_releases_waiters() {
        let feed = ScreenFeed::default();
        feed.register("a");
        feed.push("a", &vec![b'z'; 3 * BATCH_BYTES], true);
        let stale = feed.take().unwrap();
        feed.reset("a");
        assert!(!feed.is_current(&stale), "a batch taken before a reset is stale");
        feed.applied(&stale);
        let lag = feed.lag("a").unwrap();
        assert_eq!((lag.lag_bytes(), lag.gap_bytes), (0, 0), "{lag:?}");
        assert_eq!(feed.queued_total(), 0);
    }

    #[test]
    fn a_failed_parse_is_a_gap_and_the_next_batch_resyncs() {
        let feed = ScreenFeed::default();
        feed.register("a");
        feed.push("a", b"abc", true);
        let batch = feed.take().unwrap();
        feed.failed(&batch);
        let lag = feed.lag("a").unwrap();
        assert_eq!((lag.lag_bytes(), lag.gap_bytes), (0, 3));
        feed.push("a", b"def", true);
        let next = feed.take().unwrap();
        assert!(next.resync);
        assert_eq!(next.bytes, b"def");
    }

    #[test]
    fn shutdown_releases_a_waiting_take() {
        let feed = std::sync::Arc::new(ScreenFeed::default());
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = {
            let feed = feed.clone();
            crate::supervisor::spawn_once("screen-feed-test", "screen feed test", move || {
                let _ = tx.send(feed.take().is_none());
            })
            .expect("spawn")
        };
        feed.shutdown();
        assert!(rx
            .recv_timeout(crate::test_support::coverage_relaxed(
                std::time::Duration::from_secs(10)
            ))
            .expect("take returned"));
        handle.join().unwrap();
    }
}
