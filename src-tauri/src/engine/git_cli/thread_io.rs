//! Blocking-platform fallback. Workers retain the spawn permit until their
//! descriptors close, even if an OS refuses cancellation. Captured prefixes
//! live in shared bounded storage, not an all-or-nothing completion channel.
//!
//! A reader never parks inside `read` on a pipe that has no data: it waits for
//! readability in short slices and checks its cancel flag between them. A
//! detached reader whose pipe is still held open (a daemonized grandchild)
//! therefore observes the cancellation `Worker::drop` leaves behind, returns,
//! closes its own descriptor and only then drops the permit. The parent never
//! closes a descriptor a worker is reading: closing an fd does not wake a read
//! blocked on it, and a recycled fd number could close someone else's file.

use super::{Drained, ProcessObserver, SpawnPermit, Stop};
use std::io::{self, Read, Write};
use std::process::{ChildStderr, ChildStdin, ChildStdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const CANCEL_GRACE: Duration = Duration::from_millis(50);
/// How long a reader waits for data before it re-checks cancellation. Bounds
/// how long an abandoned reader keeps its permit after the parent gives up.
const READ_SLICE: Duration = Duration::from_millis(10);

/// Waits up to `timeout` for `pipe` to have data or reach EOF. `Ok(false)`
/// means nothing arrived; `Ok(true)` means a `read` will not block.
#[cfg(unix)]
fn readable(pipe: &impl std::os::fd::AsRawFd, timeout: Duration) -> io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: one valid pollfd that outlives the call; the caller owns the fd.
    let ready = unsafe { libc::poll(&mut descriptor, 1, millis) };
    match ready {
        n if n > 0 => Ok(true),
        0 => Ok(false),
        _ => {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }
}

/// Windows has no poll for anonymous pipes; `PeekNamedPipe` reports buffered
/// bytes without blocking. Any failure (a broken pipe is EOF, a non-pipe
/// handle cannot be peeked) hands the decision to `read`, which reports it.
#[cfg(windows)]
fn readable(pipe: &impl std::os::windows::io::AsRawHandle, timeout: Duration) -> io::Result<bool> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn PeekNamedPipe(
            pipe: *mut std::ffi::c_void,
            buffer: *mut std::ffi::c_void,
            size: u32,
            read: *mut u32,
            available: *mut u32,
            left: *mut u32,
        ) -> i32;
    }
    let deadline = Instant::now() + timeout;
    loop {
        let mut available = 0u32;
        // SAFETY: the handle is owned by the caller for the whole call; every
        // out-pointer is null or points at a live local.
        let ok = unsafe {
            PeekNamedPipe(
                pipe.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || available > 0 {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(not(any(unix, windows)))]
fn readable<P>(_: &P, _: Duration) -> io::Result<bool> {
    Ok(true)
}

struct Worker<T> {
    state: Arc<Mutex<T>>,
    cancelled: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl<T: Send + 'static> Worker<T> {
    fn start(
        state: T,
        permit: Arc<SpawnPermit<'static>>,
        work: impl FnOnce(&AtomicBool, &Mutex<T>) + Send + 'static,
    ) -> io::Result<Self> {
        let state = Arc::new(Mutex::new(state));
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_state = state.clone();
        let worker_cancelled = cancelled.clone();
        let handle = thread::Builder::new()
            .name("gitpulse-pipe".into())
            .spawn(move || {
                // Declared first so it drops last: `work` owns the descriptor
                // and closes it before the permit is released.
                let _permit = permit;
                work(&worker_cancelled, &worker_state);
            })?;
        Ok(Self {
            state,
            cancelled,
            handle: Some(handle),
        })
    }
}

impl<T> Worker<T> {
    fn finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        #[cfg(windows)]
        if let Some(handle) = &self.handle {
            use std::os::windows::io::AsRawHandle;
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn CancelSynchronousIo(thread: *mut std::ffi::c_void) -> i32;
            }
            // SAFETY: the owned JoinHandle keeps this thread handle valid.
            // Cancellation can race a new read/write; repeat it during the
            // bounded settle loop. Failure never masquerades as completion:
            // finish reports incomplete and the worker retains its permit.
            unsafe {
                CancelSynchronousIo(handle.as_raw_handle());
            }
        }
    }

    fn cancel_until(&self, deadline: Instant) {
        while !self.finished() {
            self.cancel();
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}

impl<T: Default> Worker<T> {
    fn take(mut self) -> (T, bool) {
        let finished = self.finished();
        let joined = if finished {
            self.handle
                .take()
                .is_none_or(|handle| handle.join().is_ok())
        } else {
            false
        };
        let state = std::mem::take(
            &mut *self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        (state, finished && joined)
    }
}

impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        self.cancel();
        // Never join a blocked worker. Its closure retains the admission
        // permit, bounding total retained descriptors, payloads and threads.
        if self.finished() {
            if let Some(handle) = self.handle.take() {
                if handle.join().is_err() {
                    log::warn!(target: "git_cli", "pipe worker panicked during cleanup");
                }
            }
        }
    }
}

fn reader<R: Read + Send + 'static>(
    pipe: Option<R>,
    cap: usize,
    permit: Arc<SpawnPermit<'static>>,
    readable: fn(&R, Duration) -> io::Result<bool>,
) -> io::Result<Worker<Drained>> {
    Worker::start(Drained::default(), permit, move |cancelled, state| {
        let Some(mut pipe) = pipe else { return };
        let mut buffer = [0; 16_384];
        loop {
            if cancelled.load(Ordering::Acquire) {
                state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .stop = Some(Stop::Undelivered("output read cancelled before EOF".into()));
                break;
            }
            match readable(&pipe, READ_SLICE) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .stop = Some(Stop::Broken(e.to_string()));
                    break;
                }
            }
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .append(&buffer[..n], cap),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let stop = if cancelled.load(Ordering::Acquire) {
                        Stop::Undelivered(format!("output read cancelled: {e}"))
                    } else {
                        Stop::Broken(e.to_string())
                    };
                    state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .stop = Some(stop);
                    break;
                }
            }
        }
    })
}

pub(super) struct OutputDrains {
    stdout: Worker<Drained>,
    stderr: Worker<Drained>,
}

impl OutputDrains {
    pub(super) fn new(
        stdout: Option<ChildStdout>,
        stderr: Option<ChildStderr>,
        cap: usize,
        permit: Arc<SpawnPermit<'static>>,
    ) -> io::Result<Self> {
        Ok(Self {
            stdout: reader(stdout, cap, permit.clone(), readable)?,
            stderr: reader(stderr, 4 * 1024 * 1024, permit, readable)?,
        })
    }

    pub(super) fn observe(&self, observer: &mut dyn ProcessObserver, cursors: &mut [usize; 2]) {
        let mut stdout = self
            .stdout
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stderr = self
            .stderr
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        super::observe_output(observer, cursors, &stdout.bytes, &stderr.bytes);
        if observer.drains_stdout() {
            stdout.discard_observed();
            cursors[0] = 0;
        }
    }

    pub(super) fn finish(self, deadline: Instant) -> (Drained, Drained) {
        while (!self.stdout.finished() || !self.stderr.finished()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        self.stdout.cancel();
        self.stderr.cancel();
        let settle = Instant::now() + CANCEL_GRACE;
        self.stdout.cancel_until(settle);
        self.stderr.cancel_until(settle);
        let finish = |worker: Worker<Drained>| {
            let (mut drained, finished) = worker.take();
            if !finished {
                drained.stop = Some(Stop::Undelivered(
                    "output worker did not finish; resource slot retained until it exits".into(),
                ));
            }
            drained
        };
        (finish(self.stdout), finish(self.stderr))
    }
}

#[derive(Default)]
struct InputState {
    written: usize,
    total: usize,
    error: Option<String>,
}

pub(super) struct InputFeed(Option<Worker<InputState>>);

impl InputFeed {
    pub(super) fn new(
        pipe: Option<ChildStdin>,
        bytes: &[u8],
        permit: Arc<SpawnPermit<'static>>,
    ) -> io::Result<Self> {
        if bytes.is_empty() {
            return Ok(Self(None));
        }
        let bytes = bytes.to_vec();
        let worker = Worker::start(
            InputState {
                total: bytes.len(),
                ..InputState::default()
            },
            permit,
            move |cancelled, state| {
                let Some(mut pipe) = pipe else { return };
                let mut written = 0;
                while written < bytes.len() && !cancelled.load(Ordering::Acquire) {
                    match pipe.write(&bytes[written..bytes.len().min(written + 16_384)]) {
                        Ok(0) => {
                            state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .error = Some("stdin write made no progress".into());
                            break;
                        }
                        Ok(n) => {
                            written += n;
                            state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .written = written;
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .error = Some(e.to_string());
                            break;
                        }
                    }
                }
            },
        )?;
        Ok(Self(Some(worker)))
    }

    pub(super) fn finish(self) -> Result<(), String> {
        let Some(worker) = self.0 else { return Ok(()) };
        worker.cancel_until(Instant::now() + CANCEL_GRACE);
        let (state, finished) = worker.take();
        if finished && state.written == state.total && state.error.is_none() {
            return Ok(());
        }
        Err(format!(
            "stdin incomplete: wrote {} of {} bytes ({})",
            state.written,
            state.total,
            state
                .error
                .as_deref()
                .unwrap_or("child exited before input worker finished")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{reader, Worker};
    #[cfg(unix)]
    use super::{InputFeed, OutputDrains};
    use crate::engine::git_cli::SpawnPermit;
    #[cfg(unix)]
    use crate::procguard::LockedSpawn;
    use std::io;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    #[cfg(unix)]
    fn fallback_handles_simultaneous_input_stdout_and_stderr() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "printf diagnosis >&2; cat"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn_locked()
            .unwrap();
        let permit = permit();
        let output = OutputDrains::new(
            child.stdout.take(),
            child.stderr.take(),
            1024,
            permit.clone(),
        )
        .unwrap();
        let input = InputFeed::new(child.stdin.take(), &vec![b'x'; 1024 * 1024], permit).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("fallback deadlocked");
            }
            thread::sleep(Duration::from_millis(1));
        }
        input.finish().unwrap();
        output.observe(&mut (), &mut [0; 2]);
        let (stdout, stderr) = output.finish(deadline);
        assert_eq!(stdout.bytes, vec![b'x'; 1024]);
        assert!(stdout.truncated && stdout.stop.is_none());
        assert_eq!(stderr.bytes, b"diagnosis");
    }

    fn permit() -> Arc<SpawnPermit<'static>> {
        Arc::new(
            super::super::spawn_gate()
                .acquire(Instant::now() + Duration::from_secs(10))
                .unwrap(),
        )
    }

    #[cfg(unix)]
    fn holder(seconds: &str) -> std::process::Child {
        std::process::Command::new("sleep")
            .arg(seconds)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn_locked()
            .unwrap()
    }

    #[cfg(unix)]
    fn in_flight(gate: &super::super::SpawnGate) -> usize {
        gate.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .in_flight
    }

    /// Polls for the gate to empty. On timeout the holders are killed first so
    /// a failing run never leaves processes or blocked threads behind.
    #[cfg(unix)]
    fn settle_or_report(
        gate: &super::super::SpawnGate,
        children: &mut [std::process::Child],
        why: &str,
    ) {
        let deadline =
            Instant::now() + crate::test_support::coverage_relaxed(Duration::from_secs(5));
        while in_flight(gate) > 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let left = in_flight(gate);
        for child in children.iter_mut() {
            child.kill().ok();
            child.wait().ok();
        }
        assert_eq!(left, 0, "{why}: spawn slots still held after the settle");
    }

    #[cfg(unix)]
    fn abandon(
        child: &mut std::process::Child,
        gate: &'static super::super::SpawnGate,
    ) -> (super::Drained, super::Drained) {
        let permit = Arc::new(
            gate.acquire(Instant::now() + Duration::from_secs(5))
                .unwrap(),
        );
        let drains =
            OutputDrains::new(child.stdout.take(), child.stderr.take(), 64, permit).unwrap();
        // Let both workers reach their blocked read first; an early cancel is
        // seen before the first read and proves nothing.
        thread::sleep(Duration::from_millis(150));
        drains.finish(Instant::now())
    }

    #[test]
    #[cfg(unix)]
    fn abandoned_reader_behind_a_live_pipe_holder_releases_its_slot() {
        let gate: &'static super::super::SpawnGate =
            Box::leak(Box::new(super::super::SpawnGate::new(4)));
        let mut child = holder("60");
        let (stdout, _) = abandon(&mut child, gate);
        assert!(matches!(stdout.stop, Some(super::Stop::Undelivered(_))));
        settle_or_report(
            gate,
            std::slice::from_mut(&mut child),
            "live holder keeps the pipe open",
        );
    }

    #[test]
    #[cfg(unix)]
    fn abandoned_reader_releases_its_slot_when_the_write_end_closes() {
        let gate: &'static super::super::SpawnGate =
            Box::leak(Box::new(super::super::SpawnGate::new(4)));
        let mut child = holder("60");
        let _ = abandon(&mut child, gate);
        child.kill().unwrap();
        child.wait().unwrap();
        settle_or_report(gate, &mut [], "write end closed");
    }

    #[test]
    #[cfg(unix)]
    fn sixteen_abandoned_holders_cannot_pin_the_gate_at_its_limit() {
        let gate: &'static super::super::SpawnGate =
            Box::leak(Box::new(super::super::SpawnGate::new(16)));
        let mut children: Vec<_> = (0..16).map(|_| holder("60")).collect();
        for child in &mut children {
            let _ = abandon(child, gate);
        }
        let deadline =
            Instant::now() + crate::test_support::coverage_relaxed(Duration::from_secs(5));
        let mut admitted = None;
        while admitted.is_none() && Instant::now() < deadline {
            admitted = gate.acquire(Instant::now() + Duration::from_millis(50));
        }
        let held = in_flight(gate);
        let ok = admitted.is_some();
        drop(admitted);
        for child in &mut children {
            child.kill().ok();
            child.wait().ok();
        }
        assert!(ok, "gate pinned at {held} of 16 by abandoned readers");
    }

    #[test]
    #[cfg(unix)]
    fn a_worker_that_already_saw_eof_races_finish_cleanly() {
        let gate: &'static super::super::SpawnGate =
            Box::leak(Box::new(super::super::SpawnGate::new(4)));
        let mut child = std::process::Command::new("sh")
            .args(["-c", "printf abc"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn_locked()
            .unwrap();
        let permit = Arc::new(
            gate.acquire(Instant::now() + Duration::from_secs(5))
                .unwrap(),
        );
        let drains =
            OutputDrains::new(child.stdout.take(), child.stderr.take(), 64, permit).unwrap();
        child.wait().unwrap();
        let (stdout, stderr) = drains.finish(Instant::now() + Duration::from_secs(5));
        assert_eq!(stdout.bytes, b"abc");
        assert!(stdout.stop.is_none() && stderr.stop.is_none());
        settle_or_report(gate, &mut [], "clean EOF");
    }

    #[test]
    #[cfg(unix)]
    fn cap_reached_mid_read_still_releases_after_abandonment() {
        let gate: &'static super::super::SpawnGate =
            Box::leak(Box::new(super::super::SpawnGate::new(4)));
        let mut child = std::process::Command::new("sh")
            .args(["-c", "head -c 100000 /dev/zero; exec sleep 60"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn_locked()
            .unwrap();
        let permit = Arc::new(
            gate.acquire(Instant::now() + Duration::from_secs(5))
                .unwrap(),
        );
        let drains =
            OutputDrains::new(child.stdout.take(), child.stderr.take(), 10, permit).unwrap();
        thread::sleep(Duration::from_millis(200));
        let (stdout, _) = drains.finish(Instant::now());
        assert_eq!(stdout.bytes.len(), 10);
        assert!(stdout.truncated);
        settle_or_report(
            gate,
            std::slice::from_mut(&mut child),
            "cap reached then abandoned",
        );
    }

    #[test]
    #[cfg(unix)]
    fn dropping_the_drains_without_finish_releases_the_slot() {
        let gate: &'static super::super::SpawnGate =
            Box::leak(Box::new(super::super::SpawnGate::new(4)));
        let mut child = holder("60");
        let permit = Arc::new(
            gate.acquire(Instant::now() + Duration::from_secs(5))
                .unwrap(),
        );
        let drains =
            OutputDrains::new(child.stdout.take(), child.stderr.take(), 64, permit).unwrap();
        drop(drains);
        settle_or_report(
            gate,
            std::slice::from_mut(&mut child),
            "dropped without finish",
        );
    }

    #[test]
    fn worker_keeps_prefix_and_distinguishes_cap_from_read_failure() {
        let worker = reader(Some(io::Cursor::new(b"abcdef")), 3, permit(), |_, _| {
            Ok(true)
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !worker.finished() && Instant::now() < deadline {
            thread::yield_now();
        }
        let (drained, finished) = worker.take();
        assert!(finished);
        assert_eq!(drained.bytes, b"abc");
        assert!(drained.truncated);
        assert!(drained.stop.is_none());
    }

    #[test]
    fn unfinished_worker_never_releases_its_resource_permit_early() {
        let permit = permit();
        let weak = Arc::downgrade(&permit);
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = Worker::start((), permit, move |_, _| {
            rx.recv().unwrap();
        })
        .unwrap();
        let started = Instant::now();
        drop(worker);
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_millis(100))
        );
        assert!(
            weak.upgrade().is_some(),
            "blocked worker escaped admission budget"
        );
        tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while weak.upgrade().is_some() && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(weak.upgrade().is_none());
    }
}
