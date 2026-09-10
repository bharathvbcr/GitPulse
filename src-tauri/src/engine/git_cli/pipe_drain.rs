//! Unix output collection stays on the command's waiting thread. Each pass
//! drains a bounded amount from each pipe, so a noisy child cannot starve its
//! sibling pipe or the command deadline. A retained write end gets one shared
//! EOF grace window; captured bytes survive and read descriptors close on return.

use super::{Drained, Stop};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::process::{ChildStderr, ChildStdin, ChildStdout};
use std::time::{Duration, Instant};

fn nonblocking(pipe: &impl AsRawFd) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    // SAFETY: caller owns this descriptor throughout both calls. The child's
    // opposite pipe end has a separate file description and is unaffected.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Input shares the waiter with output: no cloned payload, detached writer,
/// or join that can block behind a descendant retaining stdin.
pub(super) struct InputFeed<'a> {
    pipe: Option<ChildStdin>,
    pending: &'a [u8],
    total: usize,
    failure: Option<String>,
}

impl<'a> InputFeed<'a> {
    pub(super) fn new(pipe: Option<ChildStdin>, bytes: &'a [u8]) -> io::Result<Self> {
        if let Some(pipe) = &pipe {
            nonblocking(pipe)?;
        }
        Ok(Self {
            pipe: pipe.filter(|_| !bytes.is_empty()),
            pending: bytes,
            total: bytes.len(),
            failure: None,
        })
    }

    pub(super) fn pump(&mut self) {
        let Some(pipe) = &mut self.pipe else { return };
        for _ in 0..16 {
            match pipe.write(&self.pending[..self.pending.len().min(16_384)]) {
                Ok(0) => {
                    self.failure = Some("stdin write made no progress".into());
                    break;
                }
                Ok(n) => {
                    self.pending = &self.pending[n..];
                    if self.pending.is_empty() {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) => {
                    self.failure = Some(error.to_string());
                    break;
                }
            }
        }
        if self.pending.is_empty() || self.failure.is_some() {
            self.pipe = None;
        }
    }

    pub(super) fn finish(self) -> Result<(), String> {
        if self.pending.is_empty() {
            return Ok(());
        }
        Err(format!(
            "stdin incomplete: wrote {} of {} bytes ({})",
            self.total - self.pending.len(),
            self.total,
            self.failure
                .as_deref()
                .unwrap_or("child exited before accepting all input")
        ))
    }

    fn descriptor(&self) -> libc::pollfd {
        libc::pollfd {
            fd: self.pipe.as_ref().map_or(-1, AsRawFd::as_raw_fd),
            events: libc::POLLOUT,
            revents: 0,
        }
    }
}

struct PipeDrain<R> {
    pipe: Option<R>,
    captured: Drained,
    cap: usize,
}

impl<R: Read + AsRawFd> PipeDrain<R> {
    fn new(pipe: Option<R>, cap: usize) -> io::Result<Self> {
        if let Some(pipe) = &pipe {
            nonblocking(pipe)?;
        }
        Ok(Self {
            pipe,
            captured: Drained::default(),
            cap,
        })
    }

    fn drain_ready(&mut self) {
        let Some(pipe) = &mut self.pipe else { return };
        let mut buffer = [0u8; 16_384];
        // Bound attempts as well as bytes: repeated EINTR must yield back to
        // the caller's deadline. Keep draining after the cap to unblock writers.
        for _ in 0..16 {
            match pipe.read(&mut buffer) {
                Ok(0) => {
                    self.pipe = None;
                    return;
                }
                Ok(n) => self.captured.append(&buffer[..n], self.cap),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) => {
                    self.captured.stop = Some(Stop::Broken(error.to_string()));
                    self.pipe = None;
                    return;
                }
            }
        }
    }

    fn descriptor(&self) -> libc::pollfd {
        libc::pollfd {
            fd: self.pipe.as_ref().map_or(-1, AsRawFd::as_raw_fd),
            events: libc::POLLIN,
            revents: 0,
        }
    }

    fn finish(mut self) -> Drained {
        if self.pipe.is_some() {
            self.captured.stop = Some(Stop::Undelivered(format!(
                "pipe did not reach EOF after child exit (captured {} bytes)",
                self.captured.bytes.len()
            )));
        }
        self.captured
    }
}

pub(super) struct OutputDrains {
    stdout: PipeDrain<ChildStdout>,
    stderr: PipeDrain<ChildStderr>,
}

impl OutputDrains {
    pub(super) fn observe(
        &self,
        observer: &mut dyn super::ProcessObserver,
        cursors: &mut [usize; 2],
    ) {
        super::observe_output(
            observer,
            cursors,
            &self.stdout.captured.bytes,
            &self.stderr.captured.bytes,
        );
    }
    pub(super) fn new(
        stdout: Option<ChildStdout>,
        stderr: Option<ChildStderr>,
        cap: usize,
    ) -> io::Result<Self> {
        Ok(Self {
            stdout: PipeDrain::new(stdout, cap)?,
            stderr: PipeDrain::new(stderr, 4 * 1024 * 1024)?,
        })
    }

    pub(super) fn drain_ready(&mut self) {
        self.stdout.drain_ready();
        self.stderr.drain_ready();
    }

    pub(super) fn wait(&self, duration: Duration) -> io::Result<()> {
        self.wait_with_input(duration, None)
    }

    pub(super) fn wait_with_input(
        &self,
        duration: Duration,
        input: Option<&InputFeed<'_>>,
    ) -> io::Result<()> {
        let mut descriptors = [
            self.stdout.descriptor(),
            self.stderr.descriptor(),
            input.map_or(
                libc::pollfd {
                    fd: -1,
                    events: 0,
                    revents: 0,
                },
                InputFeed::descriptor,
            ),
        ];
        // Never convert a large duration into poll's negative/infinite timeout.
        let millis = i32::try_from(duration.as_millis()).unwrap_or(i32::MAX);
        let millis = if !duration.is_zero() && millis == 0 {
            1
        } else {
            millis
        };
        // SAFETY: both initialized descriptors remain owned by self during
        // this call; -1 marks closed pipes and is ignored by poll.
        let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), 3, millis) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        Ok(())
    }

    pub(super) fn finish(mut self, deadline: Instant) -> (Drained, Drained) {
        loop {
            // Read before checking time: a descheduled waiter must still
            // consume available bytes and observe EOF when it resumes.
            self.drain_ready();
            if self.stdout.pipe.is_none() && self.stderr.pipe.is_none() {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if let Err(error) = self.wait(remaining) {
                for captured in [&mut self.stdout.captured, &mut self.stderr.captured] {
                    if captured.stop.is_none() {
                        captured.stop = Some(Stop::Broken(error.to_string()));
                    }
                }
                // Preserve the polling error rather than overwriting it with
                // a missing-EOF reason, and close both descriptors now.
                self.stdout.pipe = None;
                self.stderr.pipe = None;
                break;
            }
        }
        (self.stdout.finish(), self.stderr.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::{OutputDrains, PipeDrain};
    use crate::engine::git_cli::Stop;
    use std::io::{self, Read, Write};
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    fn drain_fixture_to_eof<R: Read + AsRawFd>(drain: &mut PipeDrain<R>) {
        // Closing this process's writer does not close a concurrent fork's
        // temporary copy. Exercise the same retry-until-EOF contract for
        // every fixture that expects a complete read.
        let deadline = Instant::now() + Duration::from_secs(2);
        while drain.pipe.is_some() {
            drain.drain_ready();
            assert!(
                drain.pipe.is_none() || Instant::now() < deadline,
                "fixture did not reach EOF"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn temporarily_empty_pipe_resumes_and_distinguishes_exact_cap_from_overflow() {
        for (bytes, cap, truncated) in [
            (&b""[..], 0, false),
            (&b"x"[..], 0, true),
            (&b"abcd"[..], 4, false),
            (&b"abcde"[..], 4, true),
        ] {
            let (reader, mut writer) = std::io::pipe().expect("pipe");
            let mut drain = PipeDrain::new(Some(reader), cap).expect("drain");
            drain.drain_ready();
            assert!(drain.pipe.is_some(), "WouldBlock is not EOF");
            writer.write_all(bytes).expect("write");
            drop(writer);
            drain_fixture_to_eof(&mut drain);
            assert!(drain.pipe.is_none(), "EOF closes the reader");
            let out = drain.finish();
            assert_eq!(out.bytes, &bytes[..bytes.len().min(cap)]);
            assert_eq!(out.truncated, truncated);
            assert!(out.stop.is_none());
        }
    }

    #[test]
    fn unfinished_pipe_keeps_bytes_and_closes_its_read_descriptor() {
        struct OwnedReader {
            pipe: Option<std::io::PipeReader>,
            closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
        }
        impl Read for OwnedReader {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.pipe.as_mut().unwrap().read(bytes)
            }
        }
        impl AsRawFd for OwnedReader {
            fn as_raw_fd(&self) -> std::os::fd::RawFd {
                self.pipe.as_ref().unwrap().as_raw_fd()
            }
        }
        impl Drop for OwnedReader {
            fn drop(&mut self) {
                drop(self.pipe.take());
                self.closed
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        let closed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = OwnedReader {
            pipe: Some(reader),
            closed: closed.clone(),
        };
        let mut drain = PipeDrain::new(Some(reader), 32).expect("drain");
        writer.write_all(b"prefix").expect("write");
        drain.drain_ready();
        let out = drain.finish();
        assert_eq!(out.bytes, b"prefix");
        assert!(matches!(out.stop, Some(Stop::Undelivered(_))));
        assert!(
            closed.load(std::sync::atomic::Ordering::Acquire),
            "owner did not close its descriptor"
        );
        // Other tests fork concurrently. CLOEXEC closes their transient copy
        // at exec, not at fork; immediate EPIPE is therefore not guaranteed.
        super::nonblocking(&writer).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Err(error) = writer.write_all(b"late") {
                if error.kind() == io::ErrorKind::BrokenPipe {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "a detached reader survived closure"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn ready_output_is_read_even_when_the_waiter_resumes_after_its_deadline() {
        // This case requires all writer copies to be closed *before* the
        // expired collection starts. Isolate it from other tests' forks;
        // child exit alone cannot establish that precondition in a shared
        // process. The separate retained-pipe tests cover missing EOF.
        const CHILD: &str = "GITPULSE_EOF_FIXTURE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let (mut command, _harness) = crate::test_support::isolated_libtest_command(
                "engine::git_cli::pipe_drain::tests::ready_output_is_read_even_when_the_waiter_resumes_after_its_deadline",
            );
            command.arg("--test-threads=1");
            command.env(CHILD, "1");
            let run = crate::engine::git_cli::run_bounded_capped(
                command,
                "EOF fixture",
                Duration::from_secs(30),
                None,
                16 * 1024,
            )
            .unwrap();
            assert!(
                run.success,
                "{}\n{}",
                String::from_utf8_lossy(&run.stdout),
                String::from_utf8_lossy(&run.stderr)
            );
            assert!(
                String::from_utf8_lossy(&run.stdout).contains("1 passed"),
                "fixture was not collected"
            );
            return;
        }
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "printf complete; printf diagnostic >&2"]);
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let (mut child, guard) = crate::procguard::spawn(&mut command, "test").expect("spawn");
        let drains =
            OutputDrains::new(child.stdout.take(), child.stderr.take(), 64).expect("drains");
        // These tiny writes fit in the kernel pipes. Reap first to prove both
        // EOFs are available, and supply an already-expired handoff deadline.
        let status = guard.reap(|| child.wait()).expect("wait");
        assert!(status.success());
        let (stdout, stderr) = drains.finish(Instant::now() - Duration::from_secs(1));
        assert_eq!(stdout.bytes, b"complete");
        assert_eq!(stderr.bytes, b"diagnostic");
        assert!(stdout.stop.is_none() && stderr.stop.is_none());
    }

    #[test]
    fn chatty_pipes_yield_between_bounded_passes() {
        struct AlwaysReady(std::io::PipeReader);
        impl AsRawFd for AlwaysReady {
            fn as_raw_fd(&self) -> std::os::fd::RawFd {
                self.0.as_raw_fd()
            }
        }
        impl Read for AlwaysReady {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                bytes.fill(b'x');
                Ok(bytes.len())
            }
        }
        let (reader, _writer) = std::io::pipe().expect("pipe");
        let mut drain = PipeDrain::new(Some(AlwaysReady(reader)), 8).expect("drain");
        drain.drain_ready();
        assert_eq!(drain.captured.bytes, b"xxxxxxxx");
        assert!(drain.captured.truncated);
        assert!(
            drain.pipe.is_some(),
            "one pass yields before EOF on a hot pipe"
        );
    }

    #[test]
    fn interrupted_nonblocking_reads_resume_to_real_eof() {
        struct InterruptOnce(std::io::PipeReader, bool);
        impl AsRawFd for InterruptOnce {
            fn as_raw_fd(&self) -> std::os::fd::RawFd {
                self.0.as_raw_fd()
            }
        }
        impl Read for InterruptOnce {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                if !self.1 {
                    self.1 = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                self.0.read(bytes)
            }
        }
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        writer.write_all(b"complete").expect("write");
        drop(writer);
        let mut drain = PipeDrain::new(Some(InterruptOnce(reader, false)), 64).expect("drain");
        drain_fixture_to_eof(&mut drain);
        let out = drain.finish();
        assert_eq!(out.bytes, b"complete");
        assert!(out.stop.is_none());
    }
}
