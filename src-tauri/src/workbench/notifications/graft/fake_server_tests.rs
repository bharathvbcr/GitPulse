//! A stand-in for Graft's local submission socket, for tests only.
//!
//! It listens on a Unix socket in a private temp directory, records every
//! request line it is sent, and answers the way a test tells it to — including
//! the ways a real peer misbehaves: never answering, answering a byte at a
//! time, and hanging up without a word.
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

/// What the fake does once it has read a request line.
#[derive(Clone)]
pub(crate) enum Reply {
    /// Writes this text and a line feed, then closes.
    Line(String),
    /// Reads the request and then says nothing, holding the connection open.
    Silent,
    /// Writes one byte (never a line feed) every few milliseconds.
    Drip,
    /// Closes without writing anything.
    Hangup,
}

pub(crate) struct FakeGraft {
    _dir: tempfile::TempDir,
    path: PathBuf,
    received: mpsc::Receiver<Vec<u8>>,
    connections: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    accepting: Option<JoinHandle<()>>,
}

impl FakeGraft {
    pub(crate) fn start(reply: Reply) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("notify.sock");
        let listener = UnixListener::bind(&path).expect("bind the fake socket");
        let (sender, received) = mpsc::channel();
        let connections = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let accepting = {
            let (connections, stop) = (connections.clone(), stop.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    connections.fetch_add(1, Ordering::SeqCst);
                    let (reply, sender, stop) = (reply.clone(), sender.clone(), stop.clone());
                    std::thread::spawn(move || serve(stream, &reply, &sender, &stop));
                }
            })
        };
        Self {
            _dir: dir,
            path,
            received,
            connections,
            stop,
            accepting: Some(accepting),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The next request line received, LF included, or `None` if none arrives.
    pub(crate) fn next_request(&self, within: Duration) -> Option<Vec<u8>> {
        self.received.recv_timeout(within).ok()
    }

    /// How many connections have been accepted so far.
    pub(crate) fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

impl Drop for FakeGraft {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the blocking accept so the thread can see the flag.
        let _ = UnixStream::connect(&self.path);
        if let Some(thread) = self.accepting.take() {
            let _ = thread.join();
        }
    }
}

fn serve(
    mut stream: UnixStream,
    reply: &Reply,
    received: &mpsc::Sender<Vec<u8>>,
    stop: &AtomicBool,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let mut request = Vec::new();
    let mut chunk = [0u8; 1024];
    while !request.ends_with(b"\n") {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => request.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
    }
    if request.is_empty() {
        return;
    }
    let _ = received.send(request);
    match reply {
        Reply::Line(text) => {
            let _ = stream.write_all(text.as_bytes());
            let _ = stream.write_all(b"\n");
        }
        Reply::Silent => {
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        Reply::Drip => {
            while !stop.load(Ordering::SeqCst) {
                if stream.write_all(b" ").is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        }
        Reply::Hangup => {}
    }
}
