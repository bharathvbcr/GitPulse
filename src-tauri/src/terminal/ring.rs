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
use std::collections::VecDeque;

/// Bytes a ring keeps.
pub const RING_BYTES: usize = OUTPUT_WINDOW;

pub(super) struct Ring {
    bytes: VecDeque<u8>,
    capacity: usize,
    /// Every byte ever appended, kept or not.
    written: u64,
    /// `written` when the session's view last went away, while it is away.
    detached_at: Option<u64>,
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
        }
    }

    /// Keeps `data`, dropping the oldest bytes past the capacity.
    pub(super) fn push(&mut self, data: &[u8]) {
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

    #[test]
    fn the_ring_is_one_output_window() {
        assert_eq!(RING_BYTES, OUTPUT_WINDOW);
        assert!(lost_notice(3 * 1024 * 1024).contains("3.0 MiB"));
        assert!(lost_notice(2048).contains("2 KiB"));
    }
}
