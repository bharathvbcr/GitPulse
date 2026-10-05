//! A renderer must acknowledge bytes before the PTY can outrun its webview.
//!
//! Unless there is no renderer. A webview reload discards the page that was
//! acknowledging, while the processes it started keep running in this
//! process. Before `detach`, such a session filled its window, waited the
//! stall timeout for an acknowledgement that could never come, and was
//! killed as "renderer did not acknowledge output" — a working agent ended
//! by a reload. A detached session's output is not delivered (no page is
//! listening for it) and nothing waits on credit; `attach` hands it to a new
//! view with a fresh window, because credit the old page held is never
//! repaid.
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

pub(super) const OUTPUT_WINDOW: usize = 256 * 1024;
#[derive(Default)]
struct State {
    pending: usize,
    stopped: bool,
    detached: bool,
}

/// What the reader does with a chunk it has read.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// Credit is reserved; emit it to the renderer.
    Deliver,
    /// No renderer holds this session; the chunk is not delivered.
    Hold,
}
#[derive(Default)]
pub(super) struct OutputFlow {
    state: Mutex<State>,
    changed: Condvar,
}
impl OutputFlow {
    pub(super) fn reserve(&self, bytes: usize, timeout: Duration) -> Result<Route, String> {
        if bytes > OUTPUT_WINDOW {
            return Err("Terminal output chunk exceeds window".into());
        }
        let deadline = Instant::now() + timeout;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Terminal output lock failed")?;
        while !state.stopped && !state.detached && state.pending + bytes > OUTPUT_WINDOW {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("Terminal output stalled: renderer did not acknowledge output".into());
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| "Terminal output wait failed")?
                .0;
        }
        if state.stopped {
            return Err("Terminal output stopped".into());
        }
        if state.detached {
            return Ok(Route::Hold);
        }
        state.pending += bytes;
        Ok(Route::Deliver)
    }
    /// The renderer that held this session is gone. Wakes a reader waiting
    /// on its credit, which then holds rather than stalls.
    pub(super) fn detach(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.detached = true;
        state.pending = 0;
        self.changed.notify_all();
    }
    /// A view takes the session over. Returns whether it had been detached;
    /// only then is there anything to take over.
    pub(super) fn attach(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let was = state.detached && !state.stopped;
        if was {
            state.detached = false;
            state.pending = 0;
            self.changed.notify_all();
        }
        was
    }
    pub(super) fn is_detached(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .detached
    }
    pub(super) fn acknowledge(&self, bytes: usize) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Terminal output lock failed")?;
        if bytes == 0 || bytes > state.pending {
            return Err("Invalid terminal output acknowledgement".into());
        }
        state.pending -= bytes;
        self.changed.notify_all();
        Ok(())
    }
    pub(super) fn stop(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.stopped = true;
        self.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::{OutputFlow, Route, OUTPUT_WINDOW};
    use std::sync::Arc;
    use std::time::Duration;
    #[test]
    fn output_window_blocks_until_ack_and_cancel_wakes_it() {
        let flow = Arc::new(OutputFlow::default());
        flow.reserve(OUTPUT_WINDOW, Duration::from_secs(1)).unwrap();
        let worker = flow.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            send.send(worker.reserve(1, Duration::from_secs(2)))
                .unwrap();
        });
        assert!(receive.recv_timeout(Duration::from_millis(30)).is_err());
        flow.acknowledge(1).unwrap();
        receive
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        thread.join().unwrap();
        flow.stop();
        assert!(flow.reserve(1, Duration::from_secs(1)).is_err());
    }
    #[test]
    fn output_deadline_and_ack_bounds_are_enforced() {
        let flow = OutputFlow::default();
        assert!(flow.acknowledge(1).is_err());
        assert!(flow.acknowledge(0).is_err());
        assert!(flow.reserve(OUTPUT_WINDOW + 1, Duration::ZERO).is_err());
        flow.reserve(OUTPUT_WINDOW, Duration::ZERO).unwrap();
        assert!(flow
            .reserve(1, Duration::from_millis(10))
            .unwrap_err()
            .contains("stalled"));
        flow.acknowledge(OUTPUT_WINDOW).unwrap();
        assert!(flow.acknowledge(1).is_err());
    }
    #[test]
    fn million_chunks_never_accumulate_credit() {
        let flow = OutputFlow::default();
        for _ in 0..1_000_000 {
            flow.reserve(4096, Duration::ZERO).unwrap();
            flow.acknowledge(4096).unwrap();
        }
        assert_eq!(flow.state.lock().unwrap().pending, 0);
    }

    /// The defect: a reload left the window full and unacknowledged, and the
    /// reader's wait ended in "renderer did not acknowledge output" — which
    /// kills the process.
    #[test]
    fn a_reader_stalled_on_a_vanished_renderer_holds_instead_of_failing() {
        let flow = Arc::new(OutputFlow::default());
        assert_eq!(
            flow.reserve(OUTPUT_WINDOW, Duration::ZERO).unwrap(),
            Route::Deliver
        );
        let worker = flow.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            send.send(worker.reserve(1, Duration::from_secs(30)))
                .unwrap();
        });
        assert!(receive.recv_timeout(Duration::from_millis(30)).is_err());
        flow.detach();
        assert_eq!(
            receive
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap(),
            Route::Hold,
            "detaching wakes the waiter well inside its 30 s stall timeout"
        );
        thread.join().unwrap();
        // Detached, nothing waits and nothing accumulates, however much the
        // process prints.
        for _ in 0..10_000 {
            assert_eq!(
                flow.reserve(OUTPUT_WINDOW, Duration::ZERO).unwrap(),
                Route::Hold
            );
        }
        assert!(flow.is_detached());
    }

    #[test]
    fn attaching_gives_a_new_view_a_fresh_window_once() {
        let flow = OutputFlow::default();
        assert!(
            !flow.attach(),
            "a session its view still holds is not taken over"
        );
        flow.reserve(OUTPUT_WINDOW, Duration::ZERO).unwrap();
        flow.detach();
        assert!(flow.attach());
        assert!(!flow.attach(), "only one view takes it over");
        // The old page's credit is gone: the full window is available again,
        // and a stale acknowledgement for it is refused rather than counted.
        assert!(flow.acknowledge(1).is_err());
        assert_eq!(
            flow.reserve(OUTPUT_WINDOW, Duration::ZERO).unwrap(),
            Route::Deliver
        );
        flow.acknowledge(OUTPUT_WINDOW).unwrap();
        // A stopped session is not revived by attaching.
        flow.detach();
        flow.stop();
        assert!(!flow.attach());
        assert!(flow.reserve(1, Duration::ZERO).is_err());
    }
}
