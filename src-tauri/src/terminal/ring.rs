//! The newest output of one session, kept whatever happens to its view.
//!
//! A view that takes a session over (`attach_session`) starts from an empty
//! screen. Until this, the only thing it could be shown was what the program
//! drew after a resize nudge: a full-screen program repaints on `SIGWINCH`, but
//! a shell prompt, `cargo test` or `tail -f` does not, and their tab came back
//! blank. Output read while no view held the session was dropped
//! (`Route::Hold`), so there was nothing else to show.
//!
//! The reader appends every chunk here before it routes it, held or not, and
//! `attach` sends the newest of it to the new view first.
//!
//! The ring is one output window (`flow::OUTPUT_WINDOW`) long. A replay is
//! paid for with the new view's credit, and that view acknowledges nothing
//! until `attach` has returned and its channel is bound to the session, so a
//! replay must fit inside one fresh window or it would wait out the stall
//! timeout and be shed. Keeping more than can be replayed is memory nothing
//! reads.

use super::flow::OUTPUT_WINDOW;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

/// Bytes a ring keeps.
pub const RING_BYTES: usize = OUTPUT_WINDOW;

/// Somewhere a session's every output chunk is also copied, before it is
/// routed and whatever the route: the terminal host's durable transcript
/// (`host::transcripts`). The ring is the one place that sees all of a
/// session's output, held, shed or delivered, so it is where a copy is taken.
///
/// `chunk` runs inside [`Ring::push`], which the reader calls with the ring's
/// lock held and while that lock is also held across a wait for the view's
/// credit. An implementation must therefore never block, and never wait on a
/// lock that anything holding the ring's lock could be waiting for.
pub(super) trait Tap: Send + Sync {
    fn chunk(&self, data: &[u8]);
}

/// Taps waiting for the session they were armed for to start. A registry owns
/// one; a session claims its tap when its ring is made, so the first byte it
/// prints is already copied, which arming after the spawn could not promise
/// (a program that prints and exits at once beats the arming).
#[derive(Default)]
pub(super) struct Taps {
    armed: Mutex<HashMap<String, Arc<dyn Tap>>>,
}

impl Taps {
    /// The next session started under `session` copies its output to `tap`.
    pub(super) fn arm(&self, session: &str, tap: Arc<dyn Tap>) {
        self.armed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(session.to_owned(), tap);
    }

    /// A spawn that failed before its ring was made leaves nothing armed.
    pub(super) fn disarm(&self, session: &str) {
        self.armed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(session);
    }

    fn claim(&self, session: &str) -> Option<Arc<dyn Tap>> {
        self.armed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(session)
    }
}

pub(super) struct Ring {
    bytes: VecDeque<u8>,
    capacity: usize,
    /// Every byte ever appended, kept or not.
    written: u64,
    /// `written` when the session's view last went away, while it is away.
    detached_at: Option<u64>,
    /// Where every appended chunk is also copied, when something armed one.
    tap: Option<Arc<dyn Tap>>,
}

/// What a view that takes a session over is shown first.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Replay {
    /// The newest bytes, oldest first.
    pub bytes: Vec<u8>,
    /// Bytes printed while no view held the session that are no longer kept.
    pub lost_while_detached: u64,
}

impl Default for Ring {
    fn default() -> Self {
        Self::with_capacity(RING_BYTES)
    }
}

impl Ring {
    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::with_capacity(capacity.min(64 * 1024)),
            capacity,
            written: 0,
            detached_at: None,
            tap: None,
        }
    }

    /// An empty ring for `session`, copying to whatever tap was armed for it.
    pub(super) fn for_session(taps: &Taps, session: &str) -> Self {
        Self {
            tap: taps.claim(session),
            ..Self::default()
        }
    }

    /// Keeps `data`, dropping the oldest bytes past the capacity.
    pub(super) fn push(&mut self, data: &[u8]) {
        if let Some(tap) = &self.tap {
            tap.chunk(data);
        }
        self.written = self.written.saturating_add(data.len() as u64);
        let data = &data[data.len().saturating_sub(self.capacity)..];
        let overflow = (self.bytes.len() + data.len()).saturating_sub(self.capacity);
        self.bytes.drain(..overflow);
        self.bytes.extend(data);
    }

    /// The session's view went away. A second call while it is still away
    /// changes nothing: the first is when output began to go unseen.
    pub(super) fn detached(&mut self) {
        self.detached_at.get_or_insert(self.written);
    }

    /// A view takes the session over: what it is shown first.
    pub(super) fn attached(&mut self) -> Replay {
        let produced = self
            .detached_at
            .take()
            .map_or(0, |at| self.written.saturating_sub(at));
        Replay {
            bytes: self.bytes.iter().copied().collect(),
            lost_while_detached: produced.saturating_sub(self.bytes.len() as u64),
        }
    }

    /// Every byte ever appended, kept or not.
    #[cfg(test)]
    pub(super) fn written(&self) -> u64 {
        self.written
    }
}

/// What a view is told when output printed while it was away is gone.
pub(super) fn lost_notice(lost: u64) -> String {
    format!(
        "Output while detached was not kept: {} of it came before the last {} KiB shown here.",
        human_bytes(lost),
        RING_BYTES / 1024
    )
}

fn human_bytes(bytes: u64) -> String {
    match bytes {
        b if b >= 1024 * 1024 => format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0)),
        b if b >= 1024 => format!("{} KiB", b / 1024),
        b => format!("{b} bytes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_bytes_in_order() {
        let mut ring = Ring::with_capacity(8);
        ring.push(b"abc");
        ring.push(b"defgh");
        ring.push(b"ij");
        assert_eq!(ring.attached().bytes, b"cdefghij");
        assert_eq!(ring.written(), 10);
    }

    #[test]
    fn a_chunk_larger_than_the_ring_keeps_its_own_tail() {
        let mut ring = Ring::with_capacity(4);
        ring.push(b"xy");
        ring.push(b"0123456789");
        assert_eq!(ring.attached().bytes, b"6789");
    }

    #[test]
    fn only_output_printed_while_away_and_since_dropped_counts_as_lost() {
        let mut ring = Ring::with_capacity(8);
        ring.push(b"seen-by-view");
        // Never detached: nothing was lost, however much scrolled out.
        assert_eq!(ring.attached().lost_while_detached, 0);

        ring.detached();
        ring.push(b"12345");
        let replay = ring.attached();
        assert_eq!(replay.lost_while_detached, 0, "all five bytes are kept");
        assert_eq!(replay.bytes, b"iew12345");

        ring.detached();
        ring.push(b"abcdefghijkl");
        ring.detached(); // A second detach does not move the mark.
        let replay = ring.attached();
        assert_eq!(replay.bytes, b"efghijkl");
        assert_eq!(replay.lost_while_detached, 4);
        // Taken over once; the next attach starts a fresh count.
        assert_eq!(ring.attached().lost_while_detached, 0);
    }

    struct Copy(Mutex<Vec<u8>>);
    impl Tap for Copy {
        fn chunk(&self, data: &[u8]) {
            self.0.lock().unwrap().extend_from_slice(data);
        }
    }

    #[test]
    fn an_armed_tap_sees_every_byte_the_ring_drops_and_only_its_own_session_claims_it() {
        let taps = Taps::default();
        let copy = Arc::new(Copy(Mutex::default()));
        taps.arm("term-1-a", copy.clone());
        let mut other = Ring::for_session(&taps, "term-1-b");
        other.push(b"not copied");
        assert!(copy.0.lock().unwrap().is_empty());
        let mut ring = Ring::for_session(&taps, "term-1-a");
        // Past the capacity, and a chunk larger than the whole ring: the
        // ring keeps only the newest, the tap keeps everything.
        let big = vec![b'z'; RING_BYTES + 10];
        ring.push(b"first;");
        ring.push(&big);
        ring.push(b";last");
        let copied = copy.0.lock().unwrap().clone();
        assert_eq!(copied.len(), 6 + big.len() + 5);
        assert!(copied.starts_with(b"first;") && copied.ends_with(b";last"));
        assert_eq!(ring.attached().bytes.len(), RING_BYTES);
        // Claimed once: a second ring under the same name copies nothing.
        let mut again = Ring::for_session(&taps, "term-1-a");
        again.push(b"more");
        assert_eq!(copy.0.lock().unwrap().len(), copied.len());
    }

    #[test]
    fn a_disarmed_tap_is_never_claimed() {
        let taps = Taps::default();
        let copy = Arc::new(Copy(Mutex::default()));
        taps.arm("term-1-a", copy.clone());
        taps.disarm("term-1-a");
        Ring::for_session(&taps, "term-1-a").push(b"x");
        assert!(copy.0.lock().unwrap().is_empty());
    }

    #[test]
    fn the_ring_is_one_output_window() {
        assert_eq!(RING_BYTES, OUTPUT_WINDOW);
        assert!(lost_notice(3 * 1024 * 1024).contains("3.0 MiB"));
        assert!(lost_notice(2048).contains("2 KiB"));
    }
}
