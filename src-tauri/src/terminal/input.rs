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
    let deadline = Instant::now() + timeout;
    let check = |written| {
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
        check(0)?;
        match writer.try_lock() {
            Ok(writer) => break writer,
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(5)),
            Err(TryLockError::Poisoned(_)) => return Err("Terminal writer lock failed".into()),
        }
    };
    let mut written = 0;
    while written < data.len() {
        check(written)?;
        match writer.write(&data[written..]) {
            Ok(0) => {
                return Err(format!(
                    "Terminal input made no progress after {written} of {} bytes",
                    data.len()
                ))
            }
            Ok(n) => written += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(e) => {
                return Err(format!(
                    "Terminal input failed after {written} of {} bytes: {e}",
                    data.len()
                ))
            }
        }
    }
    // The backend is an unbuffered File; success means every byte was accepted.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::write_with_timeout;
    use std::io::{self, Write};
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

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
            assert!(start.elapsed() < Duration::from_secs(1));
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
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
