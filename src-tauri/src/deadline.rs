//! One wall-clock deadline across every read and write of a socket.
//!
//! `set_read_timeout` bounds a single `read(2)`. A peer that sends one byte
//! just inside it resets it every time, so a loop of reads against a
//! per-operation timeout is bounded by the *size* of what the peer is willing
//! to drip, not by time: up to hours for an 8 KiB report. [`DeadlineStream`]
//! gives each operation only what is left of one deadline, and an operation
//! that would start with nothing left fails `TimedOut` without touching the
//! socket.
//!
//! Every socket this process serves or probes, and cannot trust to hurry,
//! goes through this one type rather than carrying its own copy of the shrink
//! loop: the Lappi client, the local-model HTTP client, the hook bridge's
//! accept loop and the devmap daemon probe.

use std::io::{self, Read, Write};
use std::net::TcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// A stream whose read and write timeouts can be set.
pub trait Timed {
    fn set_read(&self, timeout: Option<Duration>) -> io::Result<()>;
    fn set_write(&self, timeout: Option<Duration>) -> io::Result<()>;
}

impl Timed for TcpStream {
    fn set_read(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(timeout)
    }
    fn set_write(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.set_write_timeout(timeout)
    }
}

#[cfg(unix)]
impl Timed for UnixStream {
    fn set_read(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(timeout)
    }
    fn set_write(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.set_write_timeout(timeout)
    }
}

/// A stream whose every read and write is bounded by one deadline.
pub struct DeadlineStream<S> {
    stream: S,
    deadline: Instant,
}

impl<S: Timed> DeadlineStream<S> {
    pub fn new(stream: S, deadline: Instant) -> Self {
        Self { stream, deadline }
    }

    fn remaining(&self) -> io::Result<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(expired());
        }
        Ok(left)
    }
}

fn expired() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "the exchange ran past its deadline",
    )
}

/// macOS reports an expired socket timeout as `EAGAIN` (`WouldBlock`), Linux
/// as `TimedOut`; both mean the deadline passed.
fn timed_out(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::WouldBlock {
        expired()
    } else {
        error
    }
}

/// Apply a timeout, tolerating the one failure that is not a failure: macOS
/// refuses `SO_RCVTIMEO`/`SO_SNDTIMEO` with `EINVAL` (`InvalidInput`) on a
/// socket the peer has already shut down, which is exactly when a peer has
/// written its reply and closed. On such a socket a read returns the buffered
/// bytes or EOF at once and a write fails at once, so carrying on cannot
/// block, and the timeout already set by this exchange is never past the
/// deadline. Mirrors Lappi-decision `crates/qd-runtime/src/deadline.rs`.
fn apply(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
        other => other,
    }
}

impl<S: Timed + Read> Read for DeadlineStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.remaining()?;
        apply(self.stream.set_read(Some(left)))?;
        self.stream.read(buf).map_err(timed_out)
    }
}

impl<S: Timed + Write> Write for DeadlineStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let left = self.remaining()?;
        apply(self.stream.set_write(Some(left)))?;
        self.stream.write(buf).map_err(timed_out)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
