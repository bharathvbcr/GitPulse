//! Unix PTY input uses nonblocking descriptors and one deadline for both lock
//! acquisition and delivery. A killed child does not necessarily wake a Linux
//! master write, so neither shutdown nor deadlines may depend on that wakeup.
use portable_pty::MasterPty;
use std::fs::File;
use std::io::{ErrorKind, Write};
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, TryLockError};
use std::time::{Duration, Instant};

/// Waits for the shared nonblocking master to become readable for at most
/// `timeout` (rounded up to a millisecond, which is poll's resolution). A
/// readiness notification, including hangup, only triggers another read:
/// buffered bytes must still be drained before the reader can declare EOF.
/// Whether the descriptor became ready: `false` is a timeout or an
/// interruption, both of which hand control back to the caller so it can run
/// its own shutdown checks.
///
/// This is the short, active wait (a stream's gap, the readiness fallback's
/// lap). A session with nothing to read sleeps in `readiness`, which ends on
/// the child's exit as well and has no lap.
pub(super) fn wait_for_output_within(
    fd: std::os::fd::RawFd,
    timeout: std::time::Duration,
) -> std::io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = timeout.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as libc::c_int;
    // SAFETY: descriptor is one initialized pollfd, and poll only borrows it.
    // The reader thread retains the master owning fd throughout this call.
    let result = unsafe { libc::poll(&mut descriptor, 1, millis) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != ErrorKind::Interrupted {
            return Err(error);
        }
        return Ok(false);
    }
    if fd < 0 || descriptor.revents & libc::POLLNVAL != 0 {
        return Err(std::io::Error::from_raw_os_error(libc::EBADF));
    }
    // Timeout and interruption return control to the caller, preserving its
    // shutdown checks. Never sleep after the descriptor has become ready.
    Ok(result > 0)
}

pub(super) fn open(master: &dyn MasterPty) -> Result<Box<dyn Write + Send>, String> {
    let fd = master
        .as_raw_fd()
        .ok_or("PTY has no Unix master descriptor")?;
    // SAFETY: the borrowed master stays live, and the duplicate is transferred
    // immediately to File. O_NONBLOCK affects all clones, including the reader,
    // whose WouldBlock branch must wait instead of treating it as EOF.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let file = unsafe { File::from_raw_fd(duplicate) };
    let flags = unsafe { libc::fcntl(duplicate, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(duplicate, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // portable-pty's Unix writer also writes EOF in Drop, which can block.
    // This app explicitly kills/reaps sessions; closing the owned fd suffices.
    Ok(Box::new(file))
}

pub(super) fn write(
    writer: &Mutex<Box<dyn Write + Send>>,
    dead: &AtomicBool,
    data: &[u8],
) -> Result<(), String> {
    write_with_timeout(writer, dead, data, Duration::from_secs(2))
}

fn write_with_timeout(
    writer: &Mutex<Box<dyn Write + Send>>,
    dead: &AtomicBool,
    data: &[u8],
    timeout: Duration,
) -> Result<(), String> {
    // A stall, not a budget for the whole write. A reader that keeps accepting
    // bytes (a large paste into a slow program) is making progress; a write
    // that stops advancing still fails after `timeout`. A single deadline from
    // the start failed a 60 KiB write on a loaded runner after 28 KiB had
    // already been accepted.
    let mut deadline = Instant::now() + timeout;
    let check = |written: usize, deadline: Instant| {
        if dead.load(Ordering::Acquire) {
            Err(format!(
                "Terminal closed after writing {written} of {} input bytes",
                data.len()
            ))
        } else if Instant::now() >= deadline {
            Err(format!("Terminal input timed out after writing {written} of {} bytes; input was not retried", data.len()))
        } else {
            Ok(())
        }
    };
    let mut writer = loop {
        check(0, deadline)?;
        match writer.try_lock() {
            Ok(writer) => break writer,
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(5)),
            // An unbuffered descriptor: a panic under the lock leaves nothing
            // half-written to refuse the session for.
            Err(TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
        }
    };
    let mut written = 0;
    // The delivery that gave up part-way leaves the terminal in whatever state
    // the delivered prefix set; the one state that outlives the write and
    // swallows what is typed next is an open bracketed paste.
    let give_up = |written: usize, error: String, writer: &mut Box<dyn Write + Send>| {
        if is_paste_open(&data[..written]) {
            close_paste(writer.as_mut());
        }
        Err(error)
    };
    while written < data.len() {
        if let Err(error) = check(written, deadline) {
            return give_up(written, error, &mut writer);
        }
        match writer.write(&data[written..]) {
            Ok(0) => {
                return give_up(
                    written,
                    format!(
                        "Terminal input made no progress after {written} of {} bytes",
                        data.len()
                    ),
                    &mut writer,
                )
            }
            Ok(n) => {
                written += n;
                deadline = Instant::now() + timeout;
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(e) => {
                return give_up(
                    written,
                    format!(
                        "Terminal input failed after {written} of {} bytes: {e}",
                        data.len()
                    ),
                    &mut writer,
                )
            }
        }
    }
    // The backend is an unbuffered File; success means every byte was accepted.
    Ok(())
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// How long closing a cut-off paste may take. It is a three-byte courtesy to a
/// terminal that was just too full to take the rest; it never extends a stall.
const PASTE_CLOSE_BUDGET: Duration = Duration::from_millis(250);

fn last_position(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

/// Whether `delivered` opened a bracketed paste it never closed.
fn is_paste_open(delivered: &[u8]) -> bool {
    match (
        last_position(delivered, PASTE_START),
        last_position(delivered, PASTE_END),
    ) {
        (Some(start), Some(end)) => start > end,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Sends the paste terminator, once, within [`PASTE_CLOSE_BUDGET`]. Best
/// effort: the write that brought us here already failed, so a failure here
/// is the same trouble and is not reported twice.
fn close_paste(writer: &mut (dyn Write + Send)) {
    let deadline = Instant::now() + PASTE_CLOSE_BUDGET;
    let mut sent = 0;
    while sent < PASTE_END.len() && Instant::now() < deadline {
        match writer.write(&PASTE_END[sent..]) {
            Ok(0) => return,
            Ok(n) => sent += n,
            Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_paste_open, wait_for_output_within, write_with_timeout};
    use std::io::{self, Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// One 100 ms lap, the longest the readiness fallback sleeps.
    fn wait_for_output(fd: std::os::fd::RawFd) -> io::Result<()> {
        wait_for_output_within(fd, Duration::from_millis(100)).map(|_| ())
    }

    /// A writer that accepts one byte and then blocks briefly is slow, not
    /// stuck. The timeout is how long delivery may go without progress.
    struct SlowByte {
        accepted: Arc<AtomicUsize>,
        gap: Duration,
        next: Instant,
    }

    impl Write for SlowByte {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            if data.is_empty() {
                return Ok(0);
            }
            if Instant::now() < self.next {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "full"));
            }
            self.accepted.fetch_add(1, Ordering::SeqCst);
            self.next = Instant::now() + self.gap;
            Ok(1)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_slow_reader_that_keeps_accepting_bytes_is_not_a_stuck_write() {
        let gap = Duration::from_millis(30);
        let timeout = Duration::from_millis(100);
        let accepted = Arc::new(AtomicUsize::new(0));
        let writer = SlowByte {
            accepted: accepted.clone(),
            gap,
            next: Instant::now(),
        };
        let held = Mutex::new(Box::new(writer) as Box<dyn Write + Send>);
        let dead = AtomicBool::new(false);
        let payload = vec![b'.'; 20];
        let started = Instant::now();
        write_with_timeout(&held, &dead, &payload, timeout).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(accepted.load(Ordering::SeqCst), payload.len());
        assert!(
            elapsed > timeout,
            "the write finished inside one stall window ({elapsed:?}), so this did not prove progress resets it"
        );
    }

    /// The defect: a poisoned writer lock answered "Terminal writer lock
    /// failed" for the rest of the session, so one panic under it made a live
    /// agent permanently untypeable. The writer is an unbuffered descriptor:
    /// nothing a panic interrupts leaves it half-updated.
    #[test]
    fn a_poisoned_writer_lock_does_not_make_the_terminal_untypeable() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        let writer: Mutex<Box<dyn Write + Send>> = Mutex::new(Box::new(writer));
        let _ = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _held = writer.lock().unwrap();
                    panic!("injected panic under the writer lock");
                })
                .join()
        });
        assert!(
            writer.is_poisoned(),
            "positive control: the lock is poisoned"
        );
        let dead = AtomicBool::new(false);
        write_with_timeout(&writer, &dead, b"typed", Duration::from_secs(1)).unwrap();
        let mut got = [0; 5];
        reader.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"typed");
    }

    #[test]
    fn output_wait_is_bounded_and_hangup_preserves_buffered_bytes() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let start = Instant::now();
        wait_for_output(reader.as_raw_fd()).unwrap();
        assert!(start.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(1)));
        assert_eq!(
            reader.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        writer.write_all(b"tail").unwrap();
        drop(writer);
        wait_for_output(reader.as_raw_fd()).unwrap();
        let mut tail = [0; 4];
        reader.read_exact(&mut tail).unwrap();
        assert_eq!(&tail, b"tail");
        wait_for_output(reader.as_raw_fd()).unwrap();
        assert_eq!(reader.read(&mut [0]).unwrap(), 0);
        assert_eq!(
            wait_for_output(-1).unwrap_err().raw_os_error(),
            Some(libc::EBADF)
        );
    }

    /// The reader's gather wait: a timeout is `false` and ends on time, readiness
    /// (data or hangup) is `true` at once, and a sub-millisecond ask still waits
    /// a whole poll tick instead of rounding to a busy loop.
    #[test]
    fn a_bounded_wait_reports_readiness_and_timeout_distinctly() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let fd = reader.as_raw_fd();
        let start = Instant::now();
        assert!(!super::wait_for_output_within(fd, Duration::from_millis(30)).unwrap());
        let waited = start.elapsed();
        assert!(waited >= Duration::from_millis(25), "{waited:?}");
        assert!(waited < crate::test_support::coverage_relaxed(Duration::from_secs(1)));
        writer.write_all(b"x").unwrap();
        let ready = Instant::now();
        assert!(super::wait_for_output_within(fd, Duration::from_secs(10)).unwrap());
        assert!(ready.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(1)));
        let mut one = [0; 1];
        reader.read_exact(&mut one).unwrap();
        let tick = Instant::now();
        assert!(!super::wait_for_output_within(fd, Duration::from_micros(1)).unwrap());
        assert!(tick.elapsed() >= Duration::from_micros(500));
        drop(writer);
        assert!(
            super::wait_for_output_within(fd, Duration::from_secs(10)).unwrap(),
            "a hangup is readiness: the reader must go read the EOF"
        );
    }

    struct PartialThenBlocked {
        remaining: usize,
        kind: io::ErrorKind,
    }
    impl Write for PartialThenBlocked {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(self.kind.into());
            }
            let accepted = self.remaining.min(bytes.len());
            self.remaining -= accepted;
            Ok(accepted)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_deadline_bounds_partial_and_interrupted_writes_without_retrying_delivered_bytes() {
        for kind in [io::ErrorKind::WouldBlock, io::ErrorKind::Interrupted] {
            let writer: Mutex<Box<dyn Write + Send>> =
                Mutex::new(Box::new(PartialThenBlocked { remaining: 3, kind }));
            let start = Instant::now();
            let error = write_with_timeout(
                &writer,
                &AtomicBool::new(false),
                b"abcdef",
                Duration::from_millis(20),
            )
            .unwrap_err();
            assert!(error.contains("3 of 6"), "{error}");
            assert!(error.contains("timed out"), "{error}");
            assert!(
                start.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(1))
            );
        }
    }

    #[test]
    fn closed_sessions_and_lock_contention_do_not_wait_without_a_bound() {
        let writer: Mutex<Box<dyn Write + Send>> = Mutex::new(Box::new(Vec::<u8>::new()));
        let _guard = writer.lock().unwrap();
        assert!(write_with_timeout(
            &writer,
            &AtomicBool::new(true),
            b"x",
            Duration::from_secs(1)
        )
        .unwrap_err()
        .contains("closed"));
        let start = Instant::now();
        assert!(write_with_timeout(
            &writer,
            &AtomicBool::new(false),
            b"x",
            Duration::from_millis(20)
        )
        .unwrap_err()
        .contains("0 of 1"));
        assert!(start.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(1)));
    }

    #[test]
    fn only_an_unclosed_bracketed_paste_counts_as_open() {
        assert!(is_paste_open(b"\x1b[200~half a paste"));
        assert!(!is_paste_open(b"\x1b[200~whole\x1b[201~"));
        assert!(is_paste_open(b"\x1b[200~a\x1b[201~ then \x1b[200~b"));
        assert!(!is_paste_open(b"plain typing"));
        assert!(!is_paste_open(b""));
    }

    /// Accepts `accept` bytes, then pushes back `blocked` times, then takes
    /// everything; every accepted byte is kept.
    struct Backpressure {
        accept: usize,
        blocked: usize,
        taken: std::sync::Arc<Mutex<Vec<u8>>>,
    }
    impl Write for Backpressure {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut taken = self.taken.lock().unwrap();
            if taken.len() < self.accept {
                let n = (self.accept - taken.len()).min(bytes.len());
                taken.extend_from_slice(&bytes[..n]);
                return Ok(n);
            }
            if self.blocked > 0 {
                self.blocked -= 1;
                return Err(io::ErrorKind::WouldBlock.into());
            }
            taken.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The defect: a paste whose write timed out part-way left the terminal in
    /// paste mode, so everything typed afterwards was swallowed as paste text.
    #[test]
    fn a_cut_off_bracketed_paste_is_closed() {
        let taken = std::sync::Arc::new(Mutex::new(Vec::new()));
        let writer: Mutex<Box<dyn Write + Send>> = Mutex::new(Box::new(Backpressure {
            accept: 12,
            blocked: 6,
            taken: taken.clone(),
        }));
        let error = write_with_timeout(
            &writer,
            &AtomicBool::new(false),
            b"\x1b[200~0123456789abcdef\x1b[201~",
            Duration::from_millis(20),
        )
        .unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        let taken = taken.lock().unwrap().clone();
        assert!(
            taken.ends_with(b"\x1b[201~"),
            "the paste was left open: {taken:?}"
        );
        // The next write goes through untouched.
        write_with_timeout(
            &writer,
            &AtomicBool::new(false),
            b"next",
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(taken.len() >= 12);
    }
}
