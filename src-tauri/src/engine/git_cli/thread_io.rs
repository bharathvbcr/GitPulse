//! Blocking-platform fallback. Workers retain the spawn permit until their
//! descriptors close, even if an OS refuses cancellation. Captured prefixes
//! live in shared bounded storage, not an all-or-nothing completion channel.

use super::{Drained, ProcessObserver, SpawnPermit, Stop};
use std::io::{self, Read, Write};
use std::process::{ChildStderr, ChildStdin, ChildStdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const CANCEL_GRACE: Duration = Duration::from_millis(50);

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
            stdout: reader(stdout, cap, permit.clone())?,
            stderr: reader(stderr, 4 * 1024 * 1024, permit)?,
        })
    }

    pub(super) fn observe(&self, observer: &mut dyn ProcessObserver, cursors: &mut [usize; 2]) {
        let stdout = self
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
            .spawn()
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

    #[test]
    fn worker_keeps_prefix_and_distinguishes_cap_from_read_failure() {
        let worker = reader(Some(io::Cursor::new(b"abcdef")), 3, permit()).unwrap();
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
        assert!(started.elapsed() < Duration::from_millis(100));
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
