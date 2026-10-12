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
//!
//! Nor because the renderer is slow. A page that is alive but not painting —
//! a hidden or minimized window the OS throttles, a renderer busy for half a
//! minute — used to fail the same wait, and the agent was killed for it. A
//! stall now sheds: chunks are dropped without waiting, so the reader keeps
//! draining and the process never blocks, until the view has repaid half the
//! window. Then delivery resumes and the reader has the program repaint. Lost
//! output is reported; a lost process cannot be.
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

pub(super) const OUTPUT_WINDOW: usize = 256 * 1024;
/// Outstanding credit at or below which a shedding session delivers again.
/// Half the window, so one repaid chunk does not flip it back and forth.
const RESUME_AT: usize = OUTPUT_WINDOW / 2;
/// The longest a view that has already lagged once is waited for. A
/// throttled window acknowledges in bursts; giving it the full stall timeout
/// on every window would block the process that long, over and over.
pub(super) const RELAPSE_STALL: Duration = Duration::from_secs(2);
#[derive(Default)]
struct State {
    pending: usize,
    stopped: bool,
    detached: bool,
    /// The view stopped acknowledging; chunks are dropped until it catches up.
    shedding: bool,
    /// This view has shed before; it is waited for `RELAPSE_STALL` at most.
    lagged: bool,
    /// Credit for chunks whose emit failed. A view may or may not have got
    /// one, so an acknowledgement for it is accepted if it ever comes, but it
    /// is not counted as outstanding: a view that never got it would never
    /// repay it, and the window would stay shut past the resume mark.
    uncertain: usize,
}

/// What the reader does with a chunk it has read.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// Credit is reserved; emit it to the renderer.
    Deliver,
    /// No renderer holds this session; the chunk is not delivered.
    Hold,
    /// The view is behind; the chunk is dropped. `began` is true for the
    /// first chunk dropped, which is when the view is told output was lost.
    Shed { began: bool },
    /// The view caught up after shedding. Credit is reserved; emit it, then
    /// have the program repaint what was dropped.
    Resume,
}
#[derive(Default)]
pub(super) struct OutputFlow {
    state: Mutex<State>,
    changed: Condvar,
}
impl OutputFlow {
    /// The counters, as a panic under the lock left them. Every update is a
    /// whole-number adjustment, so there is no half-updated state to refuse:
    /// and the reader treats a failed `reserve` as a dead view and kills the
    /// process, so refusing a poisoned lock ended sessions that were fine.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(super) fn reserve(&self, bytes: usize, timeout: Duration) -> Result<Route, String> {
        if bytes > OUTPUT_WINDOW {
            return Err("Terminal output chunk exceeds window".into());
        }
        let mut state = self.lock();
        let timeout = if state.lagged {
            timeout.min(RELAPSE_STALL)
        } else {
            timeout
        };
        let deadline = Instant::now() + timeout;
        if state.shedding && !state.stopped && !state.detached {
            if state.pending > RESUME_AT || state.pending + bytes > OUTPUT_WINDOW {
                return Ok(Route::Shed { began: false });
            }
            state.shedding = false;
            state.pending += bytes;
            return Ok(Route::Resume);
        }
        while !state.stopped && !state.detached && state.pending + bytes > OUTPUT_WINDOW {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.shedding = true;
                state.lagged = true;
                return Ok(Route::Shed { began: true });
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        let mut state = self.lock();
        state.detached = true;
        state.shedding = false;
        state.lagged = false;
        state.pending = 0;
        state.uncertain = 0;
        self.changed.notify_all();
    }
    /// A view takes the session over. Returns whether it had been detached;
    /// only then is there anything to take over.
    pub(super) fn attach(&self) -> bool {
        let mut state = self.lock();
        let was = state.detached && !state.stopped;
        if was {
            state.detached = false;
            state.shedding = false;
            state.lagged = false;
            state.pending = 0;
            state.uncertain = 0;
            self.changed.notify_all();
        }
        was
    }
    pub(super) fn is_detached(&self) -> bool {
        self.lock().detached
    }
    /// A chunk of `bytes`, already reserved, could not be handed to the event
    /// loop. Whether any view got it is unknown (an emit goes to every webview
    /// and stops at the first failure), so its credit is released from the
    /// outstanding window and kept as `uncertain`: an acknowledgement for it
    /// is still accepted and never counted twice, and one that never comes
    /// cannot hold the window shut. The session sheds, and resumes once
    /// acknowledgements bring the window down, as after any stall. Returns
    /// whether this began the shedding.
    pub(super) fn shed(&self, bytes: usize) -> bool {
        let mut state = self.lock();
        if state.stopped || state.detached {
            return false;
        }
        let released = bytes.min(state.pending);
        state.pending -= released;
        state.uncertain = (state.uncertain + released).min(OUTPUT_WINDOW);
        self.changed.notify_all();
        if state.shedding {
            return false;
        }
        state.shedding = true;
        state.lagged = true;
        true
    }
    #[cfg(test)]
    pub(super) fn is_shedding(&self) -> bool {
        self.lock().shedding
    }
    pub(super) fn acknowledge(&self, bytes: usize) -> Result<(), String> {
        let mut state = self.lock();
        if bytes == 0 || bytes > state.pending + state.uncertain {
            return Err("Invalid terminal output acknowledgement".into());
        }
        let from_uncertain = bytes.min(state.uncertain);
        state.uncertain -= from_uncertain;
        state.pending -= bytes - from_uncertain;
        self.changed.notify_all();
        Ok(())
    }
    pub(super) fn stop(&self) {
        let mut state = self.lock();
        state.stopped = true;
        self.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::{OutputFlow, Route, OUTPUT_WINDOW};
    use std::sync::Arc;
    use std::time::Duration;
    /// The defect: `reserve` and `acknowledge` refused a poisoned lock, while
    /// every other method recovered. The reader treats a failed `reserve` as a
    /// dead view and kills the process, so one panic under this lock (nothing
    /// here can leave the counters half-updated) ended an agent that was fine.
    #[test]
    fn a_poisoned_flow_lock_neither_kills_the_reader_nor_blocks_acknowledgements() {
        let flow = Arc::new(OutputFlow::default());
        assert_eq!(flow.reserve(100, Duration::ZERO).unwrap(), Route::Deliver);
        let poisoner = flow.clone();
        let _ = std::thread::spawn(move || {
            let _held = poisoner.state.lock().unwrap();
            panic!("injected panic under the flow lock");
        })
        .join();
        assert!(
            flow.state.is_poisoned(),
            "positive control: the lock is poisoned"
        );
        assert_eq!(flow.reserve(100, Duration::ZERO).unwrap(), Route::Deliver);
        flow.acknowledge(200).unwrap();
        assert_eq!(
            flow.state.lock().unwrap_or_else(|p| p.into_inner()).pending,
            0
        );
    }

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
        // The deadline still bounds the wait; what it now leads to is
        // shedding (see `a_stalled_view_sheds_output_instead_of_failing`).
        assert_eq!(
            flow.reserve(1, Duration::from_millis(10)).unwrap(),
            Route::Shed { began: true }
        );
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

    /// The defect: a live view that stopped acknowledging for the stall
    /// timeout — a hidden or minimized window whose page the OS throttles, a
    /// renderer busy for half a minute — failed the reservation, and the
    /// reader killed the agent. Slow paint is not a reason to end a process.
    #[test]
    fn a_stalled_view_sheds_output_instead_of_failing() {
        let flow = OutputFlow::default();
        assert_eq!(
            flow.reserve(OUTPUT_WINDOW, Duration::ZERO).unwrap(),
            Route::Deliver
        );
        assert_eq!(
            flow.reserve(1, Duration::from_millis(10)).unwrap(),
            Route::Shed { began: true },
            "the stall turns into shedding, not an error"
        );
        // Shedding never waits again: the reader keeps draining the PTY so
        // the process never blocks on a full terminal.
        let started = std::time::Instant::now();
        for _ in 0..10_000 {
            assert_eq!(
                flow.reserve(4096, Duration::from_secs(30)).unwrap(),
                Route::Shed { began: false }
            );
        }
        assert!(started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(1)));
        assert!(flow.is_shedding());
    }

    #[test]
    fn a_shedding_view_resumes_once_it_repays_half_the_window() {
        let flow = OutputFlow::default();
        flow.reserve(OUTPUT_WINDOW, Duration::ZERO).unwrap();
        flow.reserve(1, Duration::ZERO).unwrap();
        // Repaying a little is not enough: flapping between delivery and
        // shedding would repaint on every chunk.
        flow.acknowledge(1024).unwrap();
        assert_eq!(
            flow.reserve(1, Duration::ZERO).unwrap(),
            Route::Shed { began: false }
        );
        flow.acknowledge(OUTPUT_WINDOW / 2).unwrap();
        assert_eq!(flow.reserve(100, Duration::ZERO).unwrap(), Route::Resume);
        assert!(!flow.is_shedding());
        // Credit stays exact: what was delivered before and after the stall
        // is repayable, and nothing shed was ever counted.
        let outstanding = OUTPUT_WINDOW - 1024 - OUTPUT_WINDOW / 2 + 100;
        flow.acknowledge(outstanding).unwrap();
        assert!(
            flow.acknowledge(1).is_err(),
            "nothing shed was counted as owed"
        );
        assert_eq!(
            flow.reserve(OUTPUT_WINDOW, Duration::ZERO).unwrap(),
            Route::Deliver
        );
    }

    /// The defect: an emit that failed killed the process. Now it sheds,
    /// once, and a detached or stopped session is left as it is.
    #[test]
    fn a_failed_emit_sheds_once_and_resumes_like_a_stall() {
        let flow = OutputFlow::default();
        assert_eq!(flow.reserve(4096, Duration::ZERO).unwrap(), Route::Deliver);
        assert!(flow.shed(4096));
        assert!(!flow.shed(0), "already shedding: no second notice");
        assert_eq!(flow.reserve(1, Duration::ZERO).unwrap(), Route::Resume);
        flow.acknowledge(4097).unwrap();
        flow.detach();
        assert!(!flow.shed(1), "a detached session holds, it does not shed");
        flow.stop();
        assert!(!flow.shed(1));
    }

    /// The defect: a failed emit kept its reserved bytes as outstanding, so
    /// after enough of them (RESUME_AT / 4 KiB chunks) the window stayed past
    /// the resume mark and `reserve` never returned `Resume` until the session
    /// was detached and attached again, whatever the view acknowledged.
    #[test]
    fn failed_emits_do_not_leak_credit() {
        let flow = OutputFlow::default();
        for emit in 0..64 {
            let route = flow.reserve(4096, Duration::ZERO).unwrap();
            assert!(
                matches!(route, Route::Deliver | Route::Resume),
                "emit {emit}: {route:?}; failed emits leaked credit"
            );
            flow.shed(4096);
        }
        // One chunk a view did get and acknowledges, and the late
        // acknowledgement of a failed one, are both accepted, once.
        assert!(matches!(
            flow.reserve(4096, Duration::ZERO).unwrap(),
            Route::Deliver | Route::Resume
        ));
        flow.acknowledge(4096).unwrap();
        flow.acknowledge(4096).unwrap();
        assert!(
            flow.acknowledge(OUTPUT_WINDOW).is_err(),
            "over-acknowledged"
        );
    }

    /// A view that lagged once is not waited the full stall timeout again:
    /// that would block the process for it on every window.
    #[test]
    fn a_view_that_lagged_is_waited_briefly_next_time() {
        let flow = OutputFlow::default();
        flow.reserve(OUTPUT_WINDOW, Duration::ZERO).unwrap();
        assert_eq!(
            flow.reserve(1, Duration::ZERO).unwrap(),
            Route::Shed { began: true }
        );
        flow.acknowledge(OUTPUT_WINDOW).unwrap();
        assert_eq!(
            flow.reserve(OUTPUT_WINDOW, Duration::ZERO).unwrap(),
            Route::Resume
        );
        let started = std::time::Instant::now();
        assert_eq!(
            flow.reserve(1, Duration::from_secs(60)).unwrap(),
            Route::Shed { began: true }
        );
        let waited = started.elapsed();
        assert!(
            waited >= super::RELAPSE_STALL && waited < super::RELAPSE_STALL * 3,
            "{waited:?}"
        );
        // A new view (attach) starts with the full allowance again.
        flow.detach();
        assert!(flow.attach());
        flow.reserve(OUTPUT_WINDOW, Duration::ZERO).unwrap();
        assert_eq!(
            flow.reserve(1, Duration::from_millis(50)).unwrap(),
            Route::Shed { began: true }
        );
    }

    #[test]
    fn stop_still_ends_a_shedding_reader_and_detach_supersedes_it() {
        let flow = OutputFlow::default();
        flow.reserve(OUTPUT_WINDOW, Duration::ZERO).unwrap();
        flow.reserve(1, Duration::ZERO).unwrap();
        flow.detach();
        assert!(
            !flow.is_shedding(),
            "a detached session holds; it is not shedding"
        );
        assert_eq!(flow.reserve(1, Duration::ZERO).unwrap(), Route::Hold);
        assert!(flow.attach());
        assert_eq!(flow.reserve(1, Duration::ZERO).unwrap(), Route::Deliver);
        flow.stop();
        assert!(flow.reserve(1, Duration::ZERO).is_err());
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
