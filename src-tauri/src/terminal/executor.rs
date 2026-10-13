//! Where terminal commands run, instead of the pool every git command shares.
//!
//! `commands::off_thread` puts a command on tokio's blocking pool, which a
//! burst of git commands can hold for minutes, and wraps it in the git
//! admission scope. A keystroke or a resize has nothing to do with either, and
//! the frontend gives each of them five seconds before it declares a terminal
//! dead. So terminal commands run on threads of their own:
//!
//! * a **lane** per session and kind of work (input, geometry): one thread, one
//!   FIFO queue. A session's writes therefore reach its PTY in the order they
//!   were submitted, and a session whose child has stopped reading (a write
//!   may spin for its two-second budget) delays only itself.
//! * one **control pool** for the slower commands that are not per keystroke
//!   (reading a session's context, closing, attaching), as wide as the session
//!   cap so a close that waits out a hangup grace holds up neither another
//!   close nor a context read. It is never used for a keystroke or a resize:
//!   a close can occupy a thread for seconds.
//!
//! None of this runs inside `git_cli::run_command_scope`. That scope exists so
//! a pool thread does not carry one git command's admission class and marked
//! repository into the next (`ADMISSION`, `MARKED_REPO`), and only the git spawn
//! gate, `engine::worktree` and `engine::git_reader` read them. Nothing a lane
//! or the control pool runs (a descriptor write, an ioctl, `foreground`'s
//! process-table reads, `procguard::signal`) reaches any of those, and these
//! threads never set them, so there is nothing to leak or to reset.
//!
//! Both are the same [`Executor`]: a bounded queue, threads started on demand
//! and retired when idle, and nothing that grows without bound. A full queue is
//! refused at once with a message; the caller is never parked waiting for room.

use crate::logging::performance::CommandTiming;
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

/// One unit of work. It owns everything it needs, including how to report.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// Worker stack size. The work is a descriptor write or an ioctl, and the
/// slower commands read process tables; none recurses.
const WORKER_STACK_BYTES: usize = 512 * 1024;

struct State {
    jobs: VecDeque<Job>,
    /// Worker threads alive.
    threads: usize,
    /// Workers waiting for a job.
    idle: usize,
    /// No handle is left, so nothing can submit again: workers drain what is
    /// queued and end.
    closed: bool,
}

struct Shared {
    name: &'static str,
    max_threads: usize,
    capacity: usize,
    idle_after: Duration,
    state: Mutex<State>,
    ready: Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // The state is a queue and two counters, each updated whole under the
        // lock; a panic elsewhere cannot leave it half-written, and refusing
        // to read it would end every terminal command until restart.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Closes the queue when the last [`Executor`] clone goes.
struct Handle {
    shared: Arc<Shared>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.ready.notify_all();
    }
}

/// A bounded queue served by up to `max_threads` named threads.
#[derive(Clone)]
pub(crate) struct Executor {
    handle: Arc<Handle>,
}

impl Executor {
    /// `capacity` is how many jobs may wait for a thread; one already running
    /// does not count. Threads start with the first job and end after
    /// `idle_after` without one, so an unused executor costs nothing.
    pub(crate) fn new(
        name: &'static str,
        max_threads: usize,
        capacity: usize,
        idle_after: Duration,
    ) -> Self {
        Self {
            handle: Arc::new(Handle {
                shared: Arc::new(Shared {
                    name,
                    max_threads: max_threads.max(1),
                    capacity: capacity.max(1),
                    idle_after,
                    state: Mutex::new(State {
                        jobs: VecDeque::new(),
                        threads: 0,
                        idle: 0,
                        closed: false,
                    }),
                    ready: Condvar::new(),
                }),
            }),
        }
    }

    /// Runs `body` on this executor's threads and resolves with its result.
    ///
    /// The job is queued on the first poll, before the future first returns
    /// `Pending`, so calls are queued in the order they are made. A full queue
    /// resolves at once with an error rather than waiting for room.
    ///
    /// Cancellation: when the future is dropped before the job starts, the job
    /// is skipped; after it started, it finishes and its result is discarded.
    /// Either way the worker moves on. A panic in `body` is contained and
    /// reported as the error.
    ///
    /// `operation` is the label in the slow-command log (`queue_ms` is the wait
    /// for a thread here, `work_ms` the body), the same accounting
    /// `commands::off_thread` gives every other command.
    pub(crate) async fn run<T, F>(&self, operation: &'static str, body: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, String> + Send + 'static,
    {
        // A one-slot channel used once: the worker sends without blocking, the
        // future awaits. `None` from `recv` means the job was dropped unrun.
        let (reply, mut answer) = tauri::async_runtime::channel::<Result<T, String>>(1);
        let mut timing = CommandTiming::queue(operation);
        self.submit(Box::new(move || {
            if reply.is_closed() {
                return;
            }
            timing.start();
            let result = catch_unwind(AssertUnwindSafe(body)).unwrap_or_else(|payload| {
                Err(format!(
                    "{operation} panicked: {}",
                    crate::supervisor::payload_text(payload.as_ref())
                ))
            });
            timing.finish(&result);
            // A dropped receiver is a cancelled caller; the result has no one.
            let _ = reply.try_send(result);
        }))?;
        answer
            .recv()
            .await
            .unwrap_or_else(|| Err(dropped_unrun(operation)))
    }

    fn submit(&self, job: Job) -> Result<(), String> {
        let shared = &self.handle.shared;
        let mut state = shared.lock();
        if state.closed {
            return Err(shut_down(shared.name));
        }
        if state.jobs.len() >= shared.capacity {
            return Err(format!(
                "The terminal {} queue is full ({} waiting); try again in a moment",
                shared.name, shared.capacity
            ));
        }
        state.jobs.push_back(job);
        // A new thread only when more jobs are waiting than idle workers are
        // about to take, so two quick submissions do not queue behind one
        // worker that has been woken but not yet scheduled.
        if state.jobs.len() > state.idle && state.threads < shared.max_threads {
            let worker = shared.clone();
            let spawned = thread::Builder::new()
                .name(format!("terminal-{}", shared.name))
                .stack_size(WORKER_STACK_BYTES)
                .spawn(move || work(&worker));
            match spawned {
                Ok(_) => state.threads += 1,
                Err(error) if state.threads == 0 => {
                    // Nothing could ever serve it.
                    state.jobs.pop_back();
                    return Err(no_thread(shared.name, &error));
                }
                // An existing worker will get to it.
                Err(_) => {}
            }
        }
        drop(state);
        shared.ready.notify_one();
        Ok(())
    }

    /// Jobs waiting for a thread, for tests and diagnostics.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.handle.shared.lock().jobs.len()
    }

    /// Worker threads alive, for tests and diagnostics.
    pub(crate) fn workers(&self) -> usize {
        self.handle.shared.lock().threads
    }
}

// The two answers below come only from a lane whose session has left the map
// (a lane closes when its entry and every caller have let go, and a job goes
// unrun only when its lane has ended), so they say so in the words the
// frontend ends a tab's input on (`SESSION_GONE_MARKERS`). Neither is
// reachable through `run` while a caller holds the lane, which is every call;
// they are worded for the day that changes, not left to read as a dropped
// chunk of a live session. The other refusals must not carry those words.

fn shut_down(name: &str) -> String {
    format!("The terminal {name} has shut down: the terminal session is closed")
}

fn dropped_unrun(operation: &str) -> String {
    format!("{operation} was dropped before it ran: the terminal session is closed")
}

fn no_thread(name: &str, error: &std::io::Error) -> String {
    format!("Could not start a terminal {name} thread: {error}")
}

fn work(shared: &Shared) {
    loop {
        let job = {
            let mut state = shared.lock();
            loop {
                if let Some(job) = state.jobs.pop_front() {
                    break job;
                }
                if state.closed {
                    state.threads -= 1;
                    return;
                }
                state.idle += 1;
                let (guard, waited) = shared
                    .ready
                    .wait_timeout(state, shared.idle_after)
                    .unwrap_or_else(PoisonError::into_inner);
                state = guard;
                state.idle -= 1;
                // Retired under the same lock that found the queue empty, so a
                // submission either sees this thread (and it takes the job) or
                // sees it gone (and starts another): a lane never has two.
                if waited.timed_out() && state.jobs.is_empty() {
                    state.threads -= 1;
                    return;
                }
            }
        };
        // Jobs contain their own panics; this is for the ones that could not.
        let _ = catch_unwind(AssertUnwindSafe(job));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Instant;

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tauri::async_runtime::block_on(future)
    }

    fn wait_until(within: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        done()
    }

    #[test]
    fn a_single_thread_executor_runs_jobs_in_submission_order() {
        let lane = Executor::new("test-lane", 1, 64, Duration::from_secs(5));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut pending = Vec::new();
        let mut finished = Vec::new();
        for n in 0..50 {
            let seen = seen.clone();
            let lane = lane.clone();
            // Polled in order on one thread: each call queues before the next.
            let mut call = Box::pin(async move {
                lane.run("test_lane", move || {
                    seen.lock().unwrap().push(n);
                    Ok(n)
                })
                .await
            });
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            // A job may already be done by the time the next is submitted.
            match std::future::Future::poll(call.as_mut(), &mut cx) {
                std::task::Poll::Ready(result) => finished.push((n, result)),
                std::task::Poll::Pending => pending.push((n, call)),
            }
        }
        for (n, call) in pending {
            finished.push((n, block_on(call)));
        }
        assert_eq!(finished.len(), 50);
        for (n, result) in finished {
            assert_eq!(result.unwrap(), n);
        }
        assert_eq!(*seen.lock().unwrap(), (0..50).collect::<Vec<_>>());
        assert_eq!(lane.workers(), 1, "one lane never has two threads");
    }

    #[test]
    fn a_full_queue_is_refused_at_once_and_the_rest_still_run() {
        let lane = Executor::new("test-full", 1, 3, Duration::from_secs(5));
        let (gate, hold) = mpsc::channel::<()>();
        let hold = Arc::new(Mutex::new(hold));
        let first = {
            let hold = hold.clone();
            let lane = lane.clone();
            Box::pin(async move {
                lane.run("test_full", move || {
                    let _ = hold.lock().unwrap().recv();
                    Ok(0)
                })
                .await
            })
        };
        let mut first = first;
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(std::future::Future::poll(first.as_mut(), &mut cx).is_pending());
        assert!(wait_until(Duration::from_secs(5), || lane.waiting() == 0));
        let mut queued = Vec::new();
        for n in 1..=3 {
            let lane = lane.clone();
            let mut call = Box::pin(async move { lane.run("test_full", move || Ok(n)).await });
            assert!(std::future::Future::poll(call.as_mut(), &mut cx).is_pending());
            queued.push(call);
        }
        let started = Instant::now();
        let refused = block_on(lane.run("test_full", || Ok(99)));
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_millis(500))
        );
        let message = refused.unwrap_err();
        assert!(message.contains("queue is full"), "{message}");
        drop(gate);
        assert_eq!(block_on(first).unwrap(), 0);
        for (n, call) in queued.into_iter().enumerate() {
            assert_eq!(block_on(call).unwrap(), n + 1);
        }
    }

    #[test]
    fn a_dropped_caller_skips_a_queued_job_without_wedging_the_lane() {
        let lane = Executor::new("test-cancel", 1, 8, Duration::from_secs(5));
        let (gate, hold) = mpsc::channel::<()>();
        let ran = Arc::new(AtomicUsize::new(0));
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let mut first = Box::pin({
            let lane = lane.clone();
            async move {
                lane.run("test_cancel", move || {
                    let _ = hold.recv();
                    Ok(())
                })
                .await
            }
        });
        assert!(std::future::Future::poll(first.as_mut(), &mut cx).is_pending());
        let mut abandoned = Box::pin({
            let (lane, ran) = (lane.clone(), ran.clone());
            async move {
                lane.run("test_cancel", move || {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
            }
        });
        assert!(std::future::Future::poll(abandoned.as_mut(), &mut cx).is_pending());
        drop(abandoned);
        gate.send(()).unwrap();
        block_on(first).unwrap();
        assert_eq!(block_on(lane.run("test_cancel", || Ok(7))).unwrap(), 7);
        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "a job whose caller was dropped before it started must not run"
        );
    }

    #[test]
    fn a_panicking_job_is_an_error_and_the_lane_keeps_serving() {
        let lane = Executor::new("test-panic", 1, 8, Duration::from_secs(5));
        let error =
            block_on(lane.run::<(), _>("test_panic", || panic!("injected panic"))).unwrap_err();
        assert!(error.contains("panicked"), "{error}");
        assert!(error.contains("injected panic"), "{error}");
        assert_eq!(block_on(lane.run("test_panic", || Ok(1))).unwrap(), 1);
    }

    #[test]
    fn idle_threads_retire_and_the_next_job_starts_one_again() {
        let lane = Executor::new("test-idle", 2, 8, Duration::from_millis(50));
        assert_eq!(lane.workers(), 0, "an unused executor owns no thread");
        block_on(lane.run("test_idle", || Ok(()))).unwrap();
        assert!(wait_until(Duration::from_secs(5), || lane.workers() == 0));
        block_on(lane.run("test_idle", || Ok(()))).unwrap();
    }

    #[test]
    fn dropping_the_last_handle_ends_the_threads() {
        let lane = Executor::new("test-drop", 1, 8, Duration::from_secs(60));
        block_on(lane.run("test_drop", || Ok(()))).unwrap();
        let shared = lane.handle.shared.clone();
        assert_eq!(shared.lock().threads, 1);
        drop(lane);
        assert!(wait_until(Duration::from_secs(5), || shared.lock().threads == 0));
    }

    /// A closed lane is one whose session entry and every caller are gone, so
    /// its refusal must read as a gone session to the frontend
    /// (`SESSION_GONE_MARKERS`), and the refusals of a live lane must not: a
    /// full queue, a panic or a thread that would not start drop one chunk of
    /// a session that is still there.
    #[test]
    fn refusals_say_whether_the_session_is_gone() {
        let gone = |message: &str| crate::terminal::reads_as_session_gone(message);

        let lane = Executor::new("input", 1, 1, Duration::from_secs(5));
        lane.handle.shared.lock().closed = true;
        let closed = lane.submit(Box::new(|| {})).unwrap_err();
        assert!(gone(&closed), "{closed}");
        assert!(gone(&dropped_unrun("terminal_write")));
        lane.handle.shared.lock().closed = false;

        let (gate, hold) = mpsc::channel::<()>();
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let mut busy = Box::pin({
            let lane = lane.clone();
            async move {
                lane.run("terminal_write", move || {
                    let _ = hold.recv();
                    Ok(())
                })
                .await
            }
        });
        assert!(std::future::Future::poll(busy.as_mut(), &mut cx).is_pending());
        assert!(wait_until(Duration::from_secs(5), || lane.waiting() == 0));
        let mut queued = Box::pin({
            let lane = lane.clone();
            async move { lane.run("terminal_write", || Ok(())).await }
        });
        assert!(std::future::Future::poll(queued.as_mut(), &mut cx).is_pending());
        let full = block_on(lane.run("terminal_write", || Ok(()))).unwrap_err();
        assert!(full.contains("queue is full") && !gone(&full), "{full}");
        drop(gate);
        block_on(busy).unwrap();
        block_on(queued).unwrap();

        let panicked =
            block_on(lane.run::<(), _>("terminal_write", || panic!("injected"))).unwrap_err();
        assert!(!gone(&panicked), "{panicked}");
        let no_thread = no_thread(
            "input",
            &std::io::Error::from(std::io::ErrorKind::OutOfMemory),
        );
        assert!(!gone(&no_thread), "{no_thread}");
    }

    #[test]
    fn a_wider_pool_runs_a_slow_job_beside_a_quick_one() {
        let pool = Executor::new("test-wide", 4, 8, Duration::from_secs(5));
        let (gate, hold) = mpsc::channel::<()>();
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let mut slow = Box::pin({
            let pool = pool.clone();
            async move {
                pool.run("test_wide", move || {
                    let _ = hold.recv();
                    Ok(())
                })
                .await
            }
        });
        assert!(std::future::Future::poll(slow.as_mut(), &mut cx).is_pending());
        let started = Instant::now();
        block_on(pool.run("test_wide", || Ok(()))).unwrap();
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)),
            "a quick job waited for a slow one"
        );
        gate.send(()).unwrap();
        block_on(slow).unwrap();
    }
}
