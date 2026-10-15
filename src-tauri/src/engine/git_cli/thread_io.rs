//! Blocking-platform fallback. Workers retain the spawn permit until their
//! descriptors close, even if an OS refuses cancellation. Captured prefixes
//! live in shared bounded storage, not an all-or-nothing completion channel.
//!
//! Every worker owns a sticky [`Cancel`] signal and parks in a wait that the
//! signal can end (overlapped I/O plus an event on Windows, `poll` on the pipe
//! and a signal pipe elsewhere). A detached worker whose pipe is still held by
//! a daemonized grandchild therefore observes the cancellation `Worker::drop`
//! leaves behind, abandons its pending I/O, closes its own handle and only
//! then drops the permit. The parent closes nothing a worker uses: closing a
//! descriptor does not wake a blocked read, and a recycled number could close
//! someone else's file.

use super::{Drained, ProcessObserver, SpawnPermit, Stop};
use std::io;
use std::process::{ChildStderr, ChildStdin, ChildStdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const CANCEL_GRACE: Duration = Duration::from_millis(50);

/// A pipe read that the worker's [`Cancel`] can end. A cancelled wait returns
/// `ErrorKind::Interrupted`; the caller re-checks the signal.
trait CancelRead {
    fn read_cancellable(&mut self, buf: &mut [u8], cancel: &Cancel) -> io::Result<usize>;
}

/// The write-side twin of [`CancelRead`].
trait CancelWrite {
    fn write_cancellable(&mut self, buf: &[u8], cancel: &Cancel) -> io::Result<usize>;
}

/// Sticky cancellation: a `set` before the worker reaches its wait is still
/// observed, so the wait can never be missed.
struct Cancel {
    flag: AtomicBool,
    signal: sys::Signal,
}

impl Cancel {
    fn new() -> io::Result<Self> {
        Ok(Self {
            flag: AtomicBool::new(false),
            signal: sys::Signal::new()?,
        })
    }

    fn is_set(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    fn set(&self) {
        self.flag.store(true, Ordering::Release);
        self.signal.raise();
    }
}

#[cfg(unix)]
mod sys {
    use super::{Cancel, CancelRead, CancelWrite};
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::process::{ChildStderr, ChildStdin, ChildStdout};

    /// A pipe whose read end becomes readable once raised, and stays so.
    pub(super) struct Signal {
        read: OwnedFd,
        write: OwnedFd,
    }

    impl Signal {
        pub(super) fn new() -> io::Result<Self> {
            let mut fds = [0; 2];
            // SAFETY: `fds` has room for the two descriptors `pipe` writes.
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `pipe` succeeded, so both are fresh descriptors that
            // nothing else owns.
            let (read, write) =
                unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
            for fd in [read.as_raw_fd(), write.as_raw_fd()] {
                set_flags(fd, libc::F_GETFD, libc::F_SETFD, libc::FD_CLOEXEC)?;
            }
            set_flags(
                write.as_raw_fd(),
                libc::F_GETFL,
                libc::F_SETFL,
                libc::O_NONBLOCK,
            )?;
            Ok(Self { read, write })
        }

        pub(super) fn raise(&self) {
            let byte = 1u8;
            // SAFETY: one byte from a live local to a descriptor we own.
            let wrote = unsafe { libc::write(self.write.as_raw_fd(), (&raw const byte).cast(), 1) };
            if wrote < 0 {
                let error = io::Error::last_os_error();
                // A full pipe is already raised; anything else is a fault.
                if error.kind() != io::ErrorKind::WouldBlock {
                    log::warn!(target: "git_cli", "pipe cancel signal failed: {error}");
                }
            }
        }
    }

    fn set_flags(fd: RawFd, get: i32, set: i32, flag: i32) -> io::Result<()> {
        // SAFETY: plain fcntl on a descriptor the caller keeps open.
        let current = unsafe { libc::fcntl(fd, get) };
        if current < 0 || unsafe { libc::fcntl(fd, set, current | flag) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Waits until `fd` has `events` (`Ok(true)`) or the worker is cancelled
    /// (`Ok(false)`). No timeout, no spinning: the kernel wakes the thread.
    fn wait(fd: RawFd, events: i16, cancel: &Cancel) -> io::Result<bool> {
        loop {
            if cancel.is_set() {
                return Ok(false);
            }
            let mut fds = [
                libc::pollfd {
                    fd,
                    events,
                    revents: 0,
                },
                libc::pollfd {
                    fd: cancel.signal.read.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: two valid pollfds that outlive the call.
            if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if fds[0].revents != 0 {
                return Ok(true);
            }
            if fds[1].revents != 0 {
                return Ok(false);
            }
        }
    }

    fn read_fd<T: Read + AsRawFd>(
        pipe: &mut T,
        buf: &mut [u8],
        cancel: &Cancel,
    ) -> io::Result<usize> {
        let fd = pipe.as_raw_fd();
        set_flags(fd, libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK)?;
        loop {
            if !wait(fd, libc::POLLIN, cancel)? {
                return Err(io::ErrorKind::Interrupted.into());
            }
            match pipe.read(buf) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                other => return other,
            }
        }
    }

    fn write_fd<T: Write + AsRawFd>(
        pipe: &mut T,
        buf: &[u8],
        cancel: &Cancel,
    ) -> io::Result<usize> {
        let fd = pipe.as_raw_fd();
        set_flags(fd, libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK)?;
        loop {
            if !wait(fd, libc::POLLOUT, cancel)? {
                return Err(io::ErrorKind::Interrupted.into());
            }
            match pipe.write(buf) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                other => return other,
            }
        }
    }

    impl CancelRead for ChildStdout {
        fn read_cancellable(&mut self, buf: &mut [u8], cancel: &Cancel) -> io::Result<usize> {
            read_fd(self, buf, cancel)
        }
    }

    impl CancelRead for ChildStderr {
        fn read_cancellable(&mut self, buf: &mut [u8], cancel: &Cancel) -> io::Result<usize> {
            read_fd(self, buf, cancel)
        }
    }

    impl CancelWrite for ChildStdin {
        fn write_cancellable(&mut self, buf: &[u8], cancel: &Cancel) -> io::Result<usize> {
            write_fd(self, buf, cancel)
        }
    }
}

#[cfg(windows)]
mod sys {
    use super::{Cancel, CancelRead, CancelWrite};
    use std::ffi::c_void;
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::process::{ChildStderr, ChildStdin, ChildStdout};
    use std::ptr::{null, null_mut};

    const ERROR_BROKEN_PIPE: u32 = 109;
    const ERROR_HANDLE_EOF: u32 = 38;
    const ERROR_IO_PENDING: u32 = 997;
    const ERROR_NO_DATA: u32 = 232;
    const ERROR_OPERATION_ABORTED: u32 = 995;
    const ERROR_PIPE_NOT_CONNECTED: u32 = 233;
    const WAIT_OBJECT_0: u32 = 0;
    const INFINITE: u32 = u32::MAX;

    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u64,
        event: *mut c_void,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateEventW(
            attributes: *mut c_void,
            manual_reset: i32,
            initial: i32,
            name: *const u16,
        ) -> *mut c_void;
        fn SetEvent(event: *mut c_void) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn ReadFile(
            file: *mut c_void,
            buffer: *mut c_void,
            len: u32,
            read: *mut u32,
            overlapped: *mut Overlapped,
        ) -> i32;
        fn WriteFile(
            file: *mut c_void,
            buffer: *const c_void,
            len: u32,
            written: *mut u32,
            overlapped: *mut Overlapped,
        ) -> i32;
        fn WaitForMultipleObjects(
            count: u32,
            handles: *const *mut c_void,
            wait_all: i32,
            millis: u32,
        ) -> u32;
        fn CancelIoEx(file: *mut c_void, overlapped: *mut Overlapped) -> i32;
        fn GetOverlappedResult(
            file: *mut c_void,
            overlapped: *mut Overlapped,
            transferred: *mut u32,
            wait: i32,
        ) -> i32;
    }

    /// A manual-reset event: once set it stays signalled.
    pub(super) struct Signal(usize);

    impl Signal {
        pub(super) fn new() -> io::Result<Self> {
            // SAFETY: null attributes and name are documented as valid.
            let event = unsafe { CreateEventW(null_mut(), 1, 0, null()) };
            if event.is_null() {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(event as usize))
        }

        fn handle(&self) -> *mut c_void {
            self.0 as *mut c_void
        }

        pub(super) fn raise(&self) {
            // SAFETY: the event stays open until this `Signal` drops.
            if unsafe { SetEvent(self.handle()) } == 0 {
                log::warn!(target: "git_cli", "pipe cancel event failed: {}", io::Error::last_os_error());
            }
        }
    }

    impl Drop for Signal {
        fn drop(&mut self) {
            // SAFETY: the event is owned here and closed exactly once, after
            // every `Arc<Cancel>` holder (parent and worker) is gone.
            unsafe { CloseHandle(self.handle()) };
        }
    }

    // SAFETY: an event handle may be signalled and waited on from any thread.
    unsafe impl Send for Signal {}
    unsafe impl Sync for Signal {}

    #[derive(Clone, Copy)]
    enum Op {
        Read,
        Write,
    }

    fn os_error() -> u32 {
        io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(0)
            .cast_unsigned()
    }

    /// One overlapped transfer of `data`, ended early by `cancel`.
    ///
    /// Once the kernel owns the request, `data` and the `OVERLAPPED` stay alive
    /// until `GetOverlappedResult(wait)` reports the request is finished. If
    /// that drain cannot be confirmed they are leaked, never freed, exactly as
    /// std's `AsyncPipe::drop` does: the kernel may still write into them.
    fn transfer(
        handle: *mut c_void,
        op: Op,
        mut data: Box<[u8]>,
        cancel: &Cancel,
    ) -> io::Result<(usize, Box<[u8]>)> {
        if cancel.is_set() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        // SAFETY: null attributes and name are documented as valid.
        let event = unsafe { CreateEventW(null_mut(), 1, 0, null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut overlapped = Box::new(Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            event,
        });
        let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
        // SAFETY: `data` and `overlapped` are heap allocations that outlive the
        // request (see the drain below); `handle` is open for this call.
        let started = unsafe {
            match op {
                Op::Read => ReadFile(
                    handle,
                    data.as_mut_ptr().cast(),
                    len,
                    null_mut(),
                    &mut *overlapped,
                ),
                Op::Write => WriteFile(
                    handle,
                    data.as_ptr().cast(),
                    len,
                    null_mut(),
                    &mut *overlapped,
                ),
            }
        };
        let mut wait_error = None;
        if started == 0 {
            let error = os_error();
            if error != ERROR_IO_PENDING {
                // SAFETY: the request never started, so nothing references
                // the buffers or the event.
                unsafe { CloseHandle(event) };
                return match (op, error) {
                    (Op::Read, ERROR_BROKEN_PIPE) => Ok((0, data)),
                    _ => Err(io::Error::from_raw_os_error(error.cast_signed())),
                };
            }
            let waits = [event, cancel.signal.handle()];
            // SAFETY: two valid handles that outlive the call.
            let woke = unsafe { WaitForMultipleObjects(2, waits.as_ptr(), 0, INFINITE) };
            if woke != WAIT_OBJECT_0 {
                if woke != WAIT_OBJECT_0 + 1 {
                    wait_error = Some(io::Error::last_os_error());
                }
                // Not found just means it completed first; the drain reports.
                // SAFETY: cancels exactly this request on this handle.
                unsafe { CancelIoEx(handle, &mut *overlapped) };
            }
        }
        let mut transferred = 0u32;
        // SAFETY: blocks until the request is no longer in flight.
        let ok = unsafe { GetOverlappedResult(handle, &mut *overlapped, &mut transferred, 1) };
        let error = if ok != 0 { 0 } else { os_error() };
        let drained = ok != 0
            || matches!(
                error,
                ERROR_OPERATION_ABORTED
                    | ERROR_BROKEN_PIPE
                    | ERROR_HANDLE_EOF
                    | ERROR_NO_DATA
                    | ERROR_PIPE_NOT_CONNECTED
            );
        if !drained {
            // The kernel may still own the buffers (and signal the event).
            Box::leak(data);
            Box::leak(overlapped);
            return Err(io::Error::from_raw_os_error(error.cast_signed()));
        }
        // SAFETY: the request finished, so the event is no longer referenced.
        unsafe { CloseHandle(event) };
        if ok != 0 {
            return Ok((transferred as usize, data));
        }
        match (op, error) {
            (Op::Read, ERROR_BROKEN_PIPE) => Ok((0, data)),
            (_, ERROR_OPERATION_ABORTED) if cancel.is_set() => {
                Err(io::ErrorKind::Interrupted.into())
            }
            _ => {
                Err(wait_error.unwrap_or_else(|| io::Error::from_raw_os_error(error.cast_signed())))
            }
        }
    }

    fn read_handle(handle: *mut c_void, buf: &mut [u8], cancel: &Cancel) -> io::Result<usize> {
        let (n, data) = transfer(
            handle,
            Op::Read,
            vec![0; buf.len()].into_boxed_slice(),
            cancel,
        )?;
        buf[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }

    fn write_handle(handle: *mut c_void, buf: &[u8], cancel: &Cancel) -> io::Result<usize> {
        transfer(handle, Op::Write, buf.into(), cancel).map(|(n, _)| n)
    }

    impl CancelRead for ChildStdout {
        fn read_cancellable(&mut self, buf: &mut [u8], cancel: &Cancel) -> io::Result<usize> {
            read_handle(self.as_raw_handle(), buf, cancel)
        }
    }

    impl CancelRead for ChildStderr {
        fn read_cancellable(&mut self, buf: &mut [u8], cancel: &Cancel) -> io::Result<usize> {
            read_handle(self.as_raw_handle(), buf, cancel)
        }
    }

    impl CancelWrite for ChildStdin {
        fn write_cancellable(&mut self, buf: &[u8], cancel: &Cancel) -> io::Result<usize> {
            write_handle(self.as_raw_handle(), buf, cancel)
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod sys {
    use super::{Cancel, CancelRead, CancelWrite};
    use std::io::{self, Read, Write};
    use std::process::{ChildStderr, ChildStdin, ChildStdout};

    /// No wait to interrupt on this platform: reads and writes block.
    pub(super) struct Signal;

    impl Signal {
        pub(super) fn new() -> io::Result<Self> {
            Ok(Self)
        }

        pub(super) fn raise(&self) {}
    }

    impl CancelRead for ChildStdout {
        fn read_cancellable(&mut self, buf: &mut [u8], _: &Cancel) -> io::Result<usize> {
            self.read(buf)
        }
    }

    impl CancelRead for ChildStderr {
        fn read_cancellable(&mut self, buf: &mut [u8], _: &Cancel) -> io::Result<usize> {
            self.read(buf)
        }
    }

    impl CancelWrite for ChildStdin {
        fn write_cancellable(&mut self, buf: &[u8], _: &Cancel) -> io::Result<usize> {
            self.write(buf)
        }
    }
}

/// Wakes `wait_until` callers when the worker thread has fully exited.
#[derive(Default)]
struct Done {
    finished: Mutex<bool>,
    changed: Condvar,
}

impl Done {
    fn is_set(&self) -> bool {
        *self.finished.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait_until(&self, deadline: Instant) -> bool {
        let mut finished = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
        while !*finished {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            finished = self
                .changed
                .wait_timeout(finished, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }
}

/// Marks the worker done on drop, so a panicking worker still wakes waiters.
struct DoneGuard(Arc<Done>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        *self
            .0
            .finished
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
        self.0.changed.notify_all();
    }
}

struct Worker<T> {
    state: Arc<Mutex<T>>,
    cancel: Arc<Cancel>,
    done: Arc<Done>,
    handle: Option<JoinHandle<()>>,
}

impl<T: Send + 'static> Worker<T> {
    fn start(
        state: T,
        permit: Arc<SpawnPermit<'static>>,
        work: impl FnOnce(&Cancel, &Mutex<T>) + Send + 'static,
    ) -> io::Result<Self> {
        let state = Arc::new(Mutex::new(state));
        let cancel = Arc::new(Cancel::new()?);
        let done = Arc::new(Done::default());
        let worker_state = state.clone();
        let worker_cancel = cancel.clone();
        let worker_done = done.clone();
        let handle = thread::Builder::new()
            .name("gitpulse-pipe".into())
            .spawn(move || {
                // Declaration order is drop order in reverse: `work` closes the
                // descriptor, then the permit is released, then waiters wake.
                let _done = DoneGuard(worker_done);
                let _permit = permit;
                work(&worker_cancel, &worker_state);
            })?;
        Ok(Self {
            state,
            cancel,
            done,
            handle: Some(handle),
        })
    }
}

impl<T> Worker<T> {
    fn finished(&self) -> bool {
        self.done.is_set()
    }

    fn cancel(&self) {
        self.cancel.set();
    }

    /// Waits for the worker to exit, up to `deadline`.
    fn wait_until(&self, deadline: Instant) -> bool {
        self.done.wait_until(deadline)
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
        let state = std::mem::take(&mut *self.state.lock().unwrap_or_else(PoisonError::into_inner));
        (state, finished && joined)
    }
}

impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        self.cancel();
        // Never join a blocked worker. Its closure retains the admission
        // permit, bounding total retained descriptors, payloads and threads;
        // the cancellation above is what lets it exit and release them.
        if self.finished() {
            if let Some(handle) = self.handle.take() {
                if handle.join().is_err() {
                    log::warn!(target: "git_cli", "pipe worker panicked during cleanup");
                }
            }
        }
    }
}

fn reader<R: CancelRead + Send + 'static>(
    pipe: Option<R>,
    cap: usize,
    permit: Arc<SpawnPermit<'static>>,
) -> io::Result<Worker<Drained>> {
    Worker::start(Drained::default(), permit, move |cancel, state| {
        let Some(mut pipe) = pipe else { return };
        let mut buffer = [0; 16_384];
        loop {
            if cancel.is_set() {
                state.lock().unwrap_or_else(PoisonError::into_inner).stop =
                    Some(Stop::Undelivered("output read cancelled before EOF".into()));
                break;
            }
            match pipe.read_cancellable(&mut buffer, cancel) {
                Ok(0) => break,
                Ok(n) => state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .append(&buffer[..n], cap),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let stop = if cancel.is_set() {
                        Stop::Undelivered(format!("output read cancelled: {e}"))
                    } else {
                        Stop::Broken(e.to_string())
                    };
                    state.lock().unwrap_or_else(PoisonError::into_inner).stop = Some(stop);
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
        let mut stdout = self
            .stdout
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let stderr = self
            .stderr
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        super::observe_output(observer, cursors, &stdout.bytes, &stderr.bytes);
        if observer.drains_stdout() {
            stdout.discard_observed();
            cursors[0] = 0;
        }
    }

    pub(super) fn finish(self, deadline: Instant) -> (Drained, Drained) {
        self.stdout.wait_until(deadline);
        self.stderr.wait_until(deadline);
        self.stdout.cancel();
        self.stderr.cancel();
        let settle = Instant::now() + CANCEL_GRACE;
        self.stdout.wait_until(settle);
        self.stderr.wait_until(settle);
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
            move |cancel, state| {
                let Some(mut pipe) = pipe else { return };
                let mut written = 0;
                while written < bytes.len() && !cancel.is_set() {
                    match pipe.write_cancellable(
                        &bytes[written..bytes.len().min(written + 16_384)],
                        cancel,
                    ) {
                        Ok(0) => {
                            state.lock().unwrap_or_else(PoisonError::into_inner).error =
                                Some("stdin write made no progress".into());
                            break;
                        }
                        Ok(n) => {
                            written += n;
                            state.lock().unwrap_or_else(PoisonError::into_inner).written = written;
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            state.lock().unwrap_or_else(PoisonError::into_inner).error =
                                Some(e.to_string());
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
        if !worker.finished() {
            worker.cancel();
            worker.wait_until(Instant::now() + CANCEL_GRACE);
        }
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
    use super::{reader, Cancel, CancelRead, Drained, InputFeed, OutputDrains, Stop, Worker};
    use crate::engine::git_cli::{SpawnGate, SpawnPermit};
    use crate::procguard::LockedSpawn;
    use std::io::{self, Read};
    use std::process::{Child, ChildStdout, Command, Stdio};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    impl<T: AsRef<[u8]>> CancelRead for io::Cursor<T> {
        fn read_cancellable(&mut self, buf: &mut [u8], _: &Cancel) -> io::Result<usize> {
            self.read(buf)
        }
    }

    /// A reader that parks before its first wait, so a test can cancel the
    /// worker deterministically before the worker has waited at all.
    struct Late {
        go: mpsc::Receiver<()>,
        inner: ChildStdout,
    }

    impl CancelRead for Late {
        fn read_cancellable(&mut self, buf: &mut [u8], cancel: &Cancel) -> io::Result<usize> {
            self.go.recv().ok();
            self.inner.read_cancellable(buf, cancel)
        }
    }

    #[test]
    #[cfg(unix)]
    fn fallback_handles_simultaneous_input_stdout_and_stderr() {
        let mut child = Command::new("sh")
            .args(["-c", "printf diagnosis >&2; cat"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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

    fn gate(limit: usize) -> &'static SpawnGate {
        Box::leak(Box::new(SpawnGate::new(limit)))
    }

    fn take_permit(gate: &'static SpawnGate) -> Arc<SpawnPermit<'static>> {
        Arc::new(
            gate.acquire(Instant::now() + Duration::from_secs(5))
                .unwrap(),
        )
    }

    /// A child that keeps every pipe open and never reads stdin.
    fn holder() -> Child {
        #[cfg(unix)]
        let mut command = {
            let mut command = Command::new("sleep");
            command.arg("60");
            command
        };
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("cmd");
            command.args(["/C", "ping -n 61 127.0.0.1 >nul"]);
            command
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn_locked()
            .unwrap()
    }

    fn in_flight(gate: &SpawnGate) -> usize {
        gate.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .in_flight
    }

    /// Polls for the gate to empty. On timeout the holders are killed first so
    /// a failing run never leaves processes or blocked threads behind.
    fn settle_or_report(gate: &SpawnGate, children: &mut [Child], why: &str) {
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

    fn abandon(child: &mut Child, gate: &'static SpawnGate) -> (Drained, Drained) {
        let drains = OutputDrains::new(
            child.stdout.take(),
            child.stderr.take(),
            64,
            take_permit(gate),
        )
        .unwrap();
        // Let both workers reach their blocked read first.
        thread::sleep(Duration::from_millis(150));
        drains.finish(Instant::now())
    }

    #[test]
    fn abandoned_reader_behind_a_live_pipe_holder_releases_its_slot() {
        let gate = gate(4);
        let mut child = holder();
        let (stdout, _) = abandon(&mut child, gate);
        assert!(matches!(stdout.stop, Some(Stop::Undelivered(_))));
        settle_or_report(
            gate,
            std::slice::from_mut(&mut child),
            "live holder keeps the pipe open",
        );
    }

    #[test]
    fn abandoned_reader_releases_its_slot_when_the_write_end_closes() {
        let gate = gate(4);
        let mut child = holder();
        let _ = abandon(&mut child, gate);
        child.kill().unwrap();
        child.wait().unwrap();
        settle_or_report(gate, &mut [], "write end closed");
    }

    #[test]
    fn sixteen_abandoned_holders_cannot_pin_the_gate_at_its_limit() {
        let gate = gate(16);
        let mut children: Vec<_> = (0..16).map(|_| holder()).collect();
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
    fn a_worker_that_already_saw_eof_races_finish_cleanly() {
        let gate = gate(4);
        let mut child = {
            #[cfg(unix)]
            let mut command = {
                let mut command = Command::new("sh");
                command.args(["-c", "printf abc"]);
                command
            };
            #[cfg(windows)]
            let mut command = {
                let mut command = Command::new("cmd");
                command.args(["/C", "<nul set /p =abc"]);
                command
            };
            command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn_locked()
                .unwrap()
        };
        let drains = OutputDrains::new(
            child.stdout.take(),
            child.stderr.take(),
            64,
            take_permit(gate),
        )
        .unwrap();
        child.wait().unwrap();
        let (stdout, stderr) = drains.finish(Instant::now() + Duration::from_secs(5));
        assert_eq!(stdout.bytes, b"abc");
        assert!(stdout.stop.is_none() && stderr.stop.is_none());
        settle_or_report(gate, &mut [], "clean EOF");
    }

    #[test]
    fn cap_reached_mid_read_still_releases_after_abandonment() {
        let gate = gate(4);
        // The holder never writes, so fill the capped prefix through a child
        // that emits then holds the pipe open.
        let mut child = {
            #[cfg(unix)]
            let mut command = {
                let mut command = Command::new("sh");
                command.args(["-c", "head -c 100000 /dev/zero; exec sleep 60"]);
                command
            };
            #[cfg(windows)]
            let mut command = {
                let mut command = Command::new("cmd");
                command.args(["/C", "echo 0123456789abcdef& ping -n 61 127.0.0.1 >nul"]);
                command
            };
            command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn_locked()
                .unwrap()
        };
        let drains = OutputDrains::new(
            child.stdout.take(),
            child.stderr.take(),
            10,
            take_permit(gate),
        )
        .unwrap();
        thread::sleep(Duration::from_millis(300));
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
    fn dropping_the_drains_without_finish_releases_the_slot() {
        let gate = gate(4);
        let mut child = holder();
        let drains = OutputDrains::new(
            child.stdout.take(),
            child.stderr.take(),
            64,
            take_permit(gate),
        )
        .unwrap();
        drop(drains);
        settle_or_report(
            gate,
            std::slice::from_mut(&mut child),
            "dropped without finish",
        );
    }

    #[test]
    fn cancel_before_the_workers_first_wait_is_still_observed() {
        let gate = gate(4);
        let mut child = holder();
        let (go, wake) = mpsc::channel();
        let worker = reader(
            Some(Late {
                go: wake,
                inner: child.stdout.take().unwrap(),
            }),
            64,
            take_permit(gate),
        )
        .unwrap();
        worker.cancel();
        go.send(()).unwrap();
        settle_or_report(
            gate,
            std::slice::from_mut(&mut child),
            "cancel set before the first wait",
        );
        drop(worker);
    }

    #[test]
    fn cancel_during_a_pending_read_releases_the_slot() {
        let gate = gate(4);
        let mut child = holder();
        let worker = reader(child.stdout.take(), 64, take_permit(gate)).unwrap();
        thread::sleep(Duration::from_millis(150));
        assert_eq!(in_flight(gate), 1, "the pending read must hold the slot");
        worker.cancel();
        settle_or_report(
            gate,
            std::slice::from_mut(&mut child),
            "cancel during a pending read",
        );
        let (drained, finished) = worker.take();
        assert!(finished);
        assert!(matches!(drained.stop, Some(Stop::Undelivered(_))));
    }

    #[test]
    fn a_writer_stuck_on_a_child_that_never_reads_releases_its_slot() {
        let gate = gate(4);
        let mut child = holder();
        let feed =
            InputFeed::new(child.stdin.take(), &vec![0; 1024 * 1024], take_permit(gate)).unwrap();
        thread::sleep(Duration::from_millis(150));
        assert!(feed.finish().is_err());
        settle_or_report(
            gate,
            std::slice::from_mut(&mut child),
            "stdin writer parked on a full pipe",
        );
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
