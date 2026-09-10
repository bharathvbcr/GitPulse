//! Bounded writes to inherited process streams. A host may stop reading a
//! pipe without closing it; moving the write off the caller is necessary even
//! when every I/O error is handled. There is at most one worker per stream,
//! with no replacement worker or retry after an uncertain partial write.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const QUEUED_WRITES: usize = 64;

struct Request {
    bytes: Vec<u8>,
    reply: mpsc::SyncSender<io::Result<()>>,
}

pub struct BoundedOutput {
    sender: mpsc::SyncSender<Request>,
    disabled: Arc<AtomicBool>,
    max_bytes: usize,
    timeout: Duration,
}

impl BoundedOutput {
    pub fn new(
        mut writer: impl Write + Send + 'static,
        name: &str,
        max_bytes: usize,
        timeout: Duration,
    ) -> io::Result<Self> {
        if max_bytes == 0 || timeout.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty output budget",
            ));
        }
        let (sender, receiver) = mpsc::sync_channel::<Request>(QUEUED_WRITES);
        let disabled = Arc::new(AtomicBool::new(false));
        let stopped = disabled.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let _ = ready_tx.send(());
                while let Ok(request) = receiver.recv() {
                    if stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let result = writer
                        .write_all(&request.bytes)
                        .and_then(|()| writer.flush());
                    let failed = result.is_err();
                    if failed {
                        stopped.store(true, Ordering::Release);
                    }
                    let _ = request.reply.send(result);
                    if failed {
                        break;
                    }
                }
            })?;
        // The write deadline is I/O time, not thread-start time. Windows CI
        // panic probes were emptying stderr because the first 100 ms write
        // timed out before this worker was scheduled, then disabled the sink.
        ready_rx.recv_timeout(Duration::from_secs(5)).map_err(|_| {
            io::Error::new(io::ErrorKind::TimedOut, "output worker failed to start")
        })?;
        Ok(Self {
            sender,
            disabled,
            max_bytes,
            timeout,
        })
    }

    /// Success means the complete record was written and flushed. A timeout
    /// means delivery is uncertain, never that the record was not written.
    /// No logger lock is held by the worker, and dropping this handle never
    /// joins a worker blocked in host-owned I/O.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let started = Instant::now();
        if bytes.len() > self.max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "output record exceeded its byte bound",
            ));
        }
        if self.disabled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "output is disabled",
            ));
        }
        let (reply, receipt) = mpsc::sync_channel(1);
        let result = match self.sender.try_send(Request {
            bytes: bytes.to_vec(),
            reply,
        }) {
            Ok(()) => match receipt.recv_timeout(self.timeout.saturating_sub(started.elapsed())) {
                Ok(result) => result,
                Err(mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "output deadline exceeded; delivery is uncertain",
                )),
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "output worker stopped",
                )),
            },
            Err(mpsc::TrySendError::Full(_)) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "output queue is full",
            )),
            Err(mpsc::TrySendError::Disconnected(_)) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "output worker stopped",
            )),
        };
        if result.is_err() {
            self.disabled.store(true, Ordering::Release);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::BoundedOutput;
    use std::io::{self, Write};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    struct PartialWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for PartialWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let count = bytes.len().min(3);
            self.0.lock().unwrap().extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn acknowledges_complete_records_and_refuses_oversized_records_before_writing() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let output = BoundedOutput::new(
            PartialWriter(bytes.clone()),
            "output-test",
            16,
            Duration::from_secs(1),
        )
        .unwrap();
        output.write(b"first record\n").unwrap();
        assert_eq!(*bytes.lock().unwrap(), b"first record\n");
        assert_eq!(
            output.write(&[0; 17]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        output.write(b"next\n").unwrap();
        assert_eq!(*bytes.lock().unwrap(), b"first record\nnext\n");
    }

    #[test]
    fn new_does_not_return_until_the_worker_is_receiving() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let started = Instant::now();
        let output = BoundedOutput::new(
            PartialWriter(bytes.clone()),
            "output-ready-test",
            16,
            Duration::from_millis(50),
        )
        .unwrap();
        output.write(b"ready\n").unwrap();
        assert_eq!(*bytes.lock().unwrap(), b"ready\n");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn a_closed_pipe_disables_the_sink_without_retrying() {
        use std::fs::File;
        use std::os::fd::FromRawFd;
        let mut descriptors = [-1; 2];
        // SAFETY: pipe initializes two owned descriptors on success.
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        let reader = unsafe { File::from_raw_fd(descriptors[0]) };
        let writer = unsafe { File::from_raw_fd(descriptors[1]) };
        drop(reader);
        let output =
            BoundedOutput::new(writer, "closed-pipe-test", 16, Duration::from_secs(1)).unwrap();
        assert_eq!(
            output.write(b"gone\n").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(
            output.write(b"never retry").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    struct StalledWriter(mpsc::Receiver<()>, Arc<Mutex<Vec<u8>>>);
    impl Write for StalledWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.recv().unwrap();
            self.1.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_stalled_write_times_out_once_and_never_retries_or_spawns_a_replacement() {
        let (release, blocked) = mpsc::channel();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let output = BoundedOutput::new(
            StalledWriter(blocked, bytes.clone()),
            "stalled-output-test",
            16,
            Duration::from_millis(50),
        )
        .unwrap();
        let started = Instant::now();
        assert_eq!(
            output.write(b"uncertain").unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        for _ in 0..1000 {
            assert_eq!(
                output.write(b"never retry").unwrap_err().kind(),
                io::ErrorKind::BrokenPipe
            );
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
        drop(output);
        // A detached worker is deliberately not joined by Drop. The release
        // above lets this test's worker finish without leaking a live thread.
    }
}
