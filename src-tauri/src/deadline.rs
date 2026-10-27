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

    /// The wrapped stream, for what is not a read or a write (a half-close).
    pub fn get_ref(&self) -> &S {
        &self.stream
    }

    /// Puts both timeouts in force before the first read or write, so a socket
    /// that cannot take one fails here, with nothing sent, rather than after a
    /// request is already out. Applies the same tolerance as every operation, so
    /// a peer that has already closed is not a failure.
    pub fn arm(&self) -> io::Result<()> {
        let left = self.remaining()?;
        apply(self.stream.set_write(Some(left)))?;
        apply(self.stream.set_read(Some(left)))
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

/// How long one analyzer command may run, and — by the card's decision — how
/// long one whole dependency or coverage scan may run. The scan deadline is
/// this one number, not a second product budget.
pub(crate) const AUDIT_TIMEOUT: Duration = Duration::from_secs(90);

/// One wall-clock deadline for a scan made of many small pieces of work
/// (auditors, artifacts, source reads).
///
/// A per-piece timeout bounds each piece, so a scan's worst case is their
/// sum. A scan that checks one `ScanDeadline` before starting each piece, and
/// hands the piece what is left of it as its own timeout, is bounded by the
/// deadline plus the grace of whatever piece was already running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanDeadline {
    at: Instant,
}

impl ScanDeadline {
    /// A deadline `budget` from now.
    pub fn after(budget: Duration) -> Self {
        Self {
            at: Instant::now() + budget,
        }
    }

    /// A deadline of one [`AUDIT_TIMEOUT`] from now.
    pub fn audit() -> Self {
        Self::after(AUDIT_TIMEOUT)
    }

    /// What is left of the budget; zero once it has passed.
    pub fn remaining(&self) -> Duration {
        #[cfg(test)]
        if tests::forced_exhausted() {
            return Duration::ZERO;
        }
        self.at.saturating_duration_since(Instant::now())
    }

    /// True once the budget has passed. Checked before starting each piece of
    /// work, never after, so a scan that finishes in time is never reported
    /// as cut.
    pub fn expired(&self) -> bool {
        #[cfg(test)]
        if tests::forced_check() {
            return true;
        }
        self.at <= Instant::now()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        /// `Some(n)`: the next `n` `expired()` checks on this thread answer
        /// "no", every later one answers "yes" — whatever the clock says. Lets
        /// a test cut a scan between two exact pieces of work without a sleep.
        static FORCED_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    }

    pub(crate) fn forced_check() -> bool {
        FORCED_AFTER.with(|c| match c.get() {
            Some(0) => true,
            Some(n) => {
                c.set(Some(n - 1));
                false
            }
            None => false,
        })
    }

    pub(crate) fn forced_exhausted() -> bool {
        FORCED_AFTER.with(|c| c.get() == Some(0))
    }

    /// Restores real-clock behaviour when dropped.
    pub(crate) struct ExpireAfterChecks;

    impl ExpireAfterChecks {
        pub(crate) fn new(checks: usize) -> Self {
            FORCED_AFTER.with(|c| c.set(Some(checks)));
            Self
        }
    }

    impl Drop for ExpireAfterChecks {
        fn drop(&mut self) {
            FORCED_AFTER.with(|c| c.set(None));
        }
    }

    #[test]
    fn a_fresh_deadline_is_open_and_a_zero_budget_is_expired() {
        let open = ScanDeadline::after(Duration::from_secs(60));
        assert!(!open.expired());
        assert!(open.remaining() > Duration::from_secs(50));
        let spent = ScanDeadline::after(Duration::ZERO);
        assert!(spent.expired());
        assert_eq!(spent.remaining(), Duration::ZERO);
    }

    #[test]
    fn the_forced_countdown_expires_exactly_after_n_checks() {
        let open = ScanDeadline::after(Duration::from_secs(60));
        let _forced = ExpireAfterChecks::new(2);
        assert!(!open.expired());
        assert!(!open.expired());
        assert!(open.expired());
        assert_eq!(open.remaining(), Duration::ZERO);
    }

    #[test]
    fn the_audit_deadline_is_one_audit_timeout() {
        assert_eq!(AUDIT_TIMEOUT, Duration::from_secs(90));
        assert!(ScanDeadline::audit().remaining() <= AUDIT_TIMEOUT);
    }
}
