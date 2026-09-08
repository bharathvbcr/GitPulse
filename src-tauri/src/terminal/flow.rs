//! A renderer must acknowledge bytes before the PTY can outrun its webview.
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

pub(super) const OUTPUT_WINDOW: usize = 256 * 1024;
#[derive(Default)]
struct State {
    pending: usize,
    stopped: bool,
}
#[derive(Default)]
pub(super) struct OutputFlow {
    state: Mutex<State>,
    changed: Condvar,
}
impl OutputFlow {
    pub(super) fn reserve(&self, bytes: usize, timeout: Duration) -> Result<(), String> {
        if bytes > OUTPUT_WINDOW {
            return Err("Terminal output chunk exceeds window".into());
        }
        let deadline = Instant::now() + timeout;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Terminal output lock failed")?;
        while !state.stopped && state.pending + bytes > OUTPUT_WINDOW {
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
        state.pending += bytes;
        Ok(())
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
    use super::{OutputFlow, OUTPUT_WINDOW};
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
}
