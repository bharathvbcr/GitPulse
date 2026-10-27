//! Adversary tests for lane W3b item 1: graft's exchange on `DeadlineStream`
//! against the exchange it replaced.
//!
//! `old_converse` / `old_exchange` are the hand-rolled exchange of `2abaa1ba`
//! (`graft.rs` lines 247-338 there), copied with the one change that its
//! socket is reached through `crate::deadline::Timed` instead of the concrete
//! `UnixStream` timeout setters, so a scripted socket can stand in for a real
//! one. Every test compares the two on the same input.
use super::super::fake_server_tests::{FakeGraft, Reply};
use super::super::{converse, exchange, Outcome, MAX_ANSWER};
use crate::deadline::Timed;
use std::cell::RefCell;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

const BUDGET: Duration = Duration::from_millis(400);
const REQUEST: &[u8] = b"{\"op\":\"post\"}\n";

fn old_parse_answer(answer: &[u8]) -> Outcome {
    // The answer parser is untouched by the lane; reuse it.
    super::super::parse_answer(answer)
}

fn old_converse<S>(mut stream: S, request: &[u8], deadline: Instant) -> Outcome
where
    S: Timed + Read + Write,
{
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
    };
    let timed_out =
        |e: &std::io::Error| matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut);
    let bound = |result: std::io::Result<()>| match result {
        Err(e) if e.kind() != ErrorKind::InvalidInput => Err(()),
        _ => Ok(()),
    };
    let Some(left) = remaining() else {
        return Outcome::TimedOut;
    };
    if bound(stream.set_write(Some(left))).is_err() || bound(stream.set_read(Some(left))).is_err() {
        return Outcome::Io;
    }

    let mut sent = 0;
    while sent < request.len() {
        let Some(left) = remaining() else {
            return Outcome::TimedOut;
        };
        if bound(stream.set_write(Some(left))).is_err() {
            return Outcome::Io;
        }
        match stream.write(&request[sent..]) {
            Ok(0) => return Outcome::Io,
            Ok(n) => sent += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if timed_out(&e) => return Outcome::TimedOut,
            Err(_) => return Outcome::Io,
        }
    }

    let mut answer = Vec::with_capacity(64);
    let mut chunk = [0u8; 128];
    loop {
        let Some(left) = remaining() else {
            return Outcome::TimedOut;
        };
        if bound(stream.set_read(Some(left))).is_err() {
            return Outcome::Io;
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                answer.extend_from_slice(&chunk[..n]);
                if let Some(end) = answer.iter().position(|&b| b == b'\n') {
                    answer.truncate(end);
                    break;
                }
                if answer.len() > MAX_ANSWER {
                    return Outcome::Garbled;
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if timed_out(&e) => return Outcome::TimedOut,
            Err(_) => return Outcome::Io,
        }
    }
    old_parse_answer(&answer)
}

fn old_exchange(path: &Path, request: &[u8], budget: Duration) -> Outcome {
    let deadline = Instant::now() + budget;
    match UnixStream::connect(path) {
        Ok(stream) => old_converse(stream, request, deadline),
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            Outcome::Absent
        }
        Err(_) => Outcome::Io,
    }
}

/// Runs both exchanges against a fresh peer each and returns
/// `(old, new, old_elapsed, new_elapsed)`.
fn both_against(reply: impl Fn() -> Reply) -> (Outcome, Outcome, Duration, Duration) {
    let old_peer = FakeGraft::start(reply());
    let t = Instant::now();
    let old = old_exchange(old_peer.path(), REQUEST, BUDGET);
    let old_elapsed = t.elapsed();
    let new_peer = FakeGraft::start(reply());
    let t = Instant::now();
    let new = exchange(new_peer.path(), REQUEST, BUDGET);
    (old, new, old_elapsed, t.elapsed())
}

fn slack() -> Duration {
    crate::test_support::coverage_relaxed(Duration::from_millis(1500))
}

/// A named peer behaviour and the outcome both exchanges must report for it.
type PeerCase = (&'static str, Box<dyn Fn() -> Reply>, Outcome);

#[test]
fn held_every_fake_peer_outcome_is_the_one_the_old_exchange_gave() {
    let cases: Vec<PeerCase> = vec![
        (
            "accepted",
            Box::new(|| Reply::Line("{\"ok\":true,\"sent\":true}".into())),
            Outcome::Accepted { sent: true },
        ),
        (
            "accepted not sent",
            Box::new(|| Reply::Line("{\"ok\":true}".into())),
            Outcome::Accepted { sent: false },
        ),
        (
            "garbled json",
            Box::new(|| Reply::Line("nope".into())),
            Outcome::Garbled,
        ),
        ("hangup", Box::new(|| Reply::Hangup), Outcome::Garbled),
        ("silent", Box::new(|| Reply::Silent), Outcome::TimedOut),
        ("dribble", Box::new(|| Reply::Drip), Outcome::TimedOut),
    ];
    for (name, reply, want) in cases {
        for round in 0..5 {
            let (old, new, old_elapsed, new_elapsed) = both_against(&reply);
            assert_eq!(old, want, "{name}: old exchange, round {round}");
            assert_eq!(new, old, "{name}: new differs from old, round {round}");
            assert!(
                new_elapsed <= old_elapsed.max(BUDGET) + slack(),
                "{name}: {new_elapsed:?}"
            );
        }
    }
}

#[test]
fn held_absent_and_unusable_socket_paths_map_as_before() {
    let dir = tempfile::tempdir().unwrap();
    // Nothing there.
    let missing = dir.path().join("none.sock");
    assert_eq!(old_exchange(&missing, REQUEST, BUDGET), Outcome::Absent);
    assert_eq!(exchange(&missing, REQUEST, BUDGET), Outcome::Absent);
    // A socket file nobody listens on any more.
    let dead = dir.path().join("dead.sock");
    drop(UnixListener::bind(&dead).unwrap());
    assert_eq!(
        old_exchange(&dead, REQUEST, BUDGET),
        exchange(&dead, REQUEST, BUDGET)
    );
    // A regular file, a directory, a path component that is a file.
    let file = dir.path().join("plain");
    std::fs::write(&file, b"x").unwrap();
    for path in [file.clone(), dir.path().to_path_buf(), file.join("below")] {
        assert_eq!(
            old_exchange(&path, REQUEST, BUDGET),
            exchange(&path, REQUEST, BUDGET),
            "{path:?}"
        );
    }
}

/// A peer that sends more than `MAX_ANSWER` bytes without a newline, then
/// holds the connection open: both give `Garbled` and neither waits it out.
#[test]
fn held_an_oversized_answer_is_garbled_without_waiting_for_the_close() {
    for exchange_fn in [
        old_exchange as fn(&Path, &[u8], Duration) -> Outcome,
        exchange,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let peer = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut line = Vec::new();
            let mut byte = [0u8; 1];
            while !line.ends_with(b"\n") {
                if s.read(&mut byte).unwrap_or(0) == 0 {
                    return;
                }
                line.push(byte[0]);
            }
            let _ = s.write_all(&vec![b'x'; MAX_ANSWER * 4]);
            std::thread::sleep(Duration::from_millis(1500));
        });
        let t = Instant::now();
        let out = exchange_fn(&path, REQUEST, Duration::from_secs(5));
        assert_eq!(out, Outcome::Garbled);
        assert!(
            t.elapsed() < Duration::from_millis(1200),
            "{:?}",
            t.elapsed()
        );
        peer.join().unwrap();
    }
}

/// Peer that reads nothing of a request far larger than the socket buffers, and
/// never closes: the write blocks, part of the request is already out, and the
/// deadline must end it as `TimedOut`, on one deadline.
fn pair_with_peer(
    peer: impl FnOnce(UnixStream) + Send + 'static,
) -> (UnixStream, std::thread::JoinHandle<()>) {
    let (mine, theirs) = UnixStream::pair().unwrap();
    (mine, std::thread::spawn(move || peer(theirs)))
}

#[test]
fn held_a_peer_that_reads_nothing_times_out_a_partial_write_once() {
    let big = vec![b'a'; 4 * 1024 * 1024];
    for new in [false, true] {
        let (stream, peer) = pair_with_peer(|s| {
            std::thread::sleep(Duration::from_millis(1500));
            drop(s);
        });
        let t = Instant::now();
        let deadline = t + Duration::from_millis(300);
        let out = if new {
            converse(stream, &big, deadline)
        } else {
            old_converse(stream, &big, deadline)
        };
        assert_eq!(out, Outcome::TimedOut, "new={new}");
        assert!(
            t.elapsed() < Duration::from_millis(300) + slack(),
            "new={new}: {:?}",
            t.elapsed()
        );
        peer.join().unwrap();
    }
}

#[test]
fn held_a_peer_that_reads_one_byte_at_a_time_ends_the_same_way_as_before() {
    // A request far larger than production's `MAX_REQUEST` (8 KiB) drained one
    // byte at a time. A single blocking `write(2)` that keeps making progress
    // restarts the kernel's send timeout, so BOTH the old exchange and the new
    // one run past the 300 ms deadline here (until the peer gives up at 2 s).
    // Not a regression, so this only pins that the two agree; at 8 KiB the
    // request fits the socket buffer and the case cannot arise.
    let big = vec![b'a'; 96 * 1024];
    let mut ran = Vec::new();
    for new in [false, true] {
        let (stream, peer) = pair_with_peer(|mut s| {
            let mut b = [0u8; 1];
            let end = Instant::now() + Duration::from_millis(2000);
            while Instant::now() < end {
                if s.read(&mut b).unwrap_or(0) == 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let t = Instant::now();
        let deadline = t + Duration::from_millis(300);
        let out = if new {
            converse(stream, &big, deadline)
        } else {
            old_converse(stream, &big, deadline)
        };
        ran.push((out, t.elapsed()));
        peer.join().unwrap();
    }
    eprintln!("old {:?}, new {:?}", ran[0], ran[1]);
    assert_eq!(ran[1].0, ran[0].0, "new differs from old");
    assert!(
        ran[1].1 <= ran[0].1 + slack(),
        "new ran longer than old: {ran:?}"
    );
}

/// A peer that has answered and closed before our request is even read: a race
/// between EPIPE/ECONNRESET on the write and the buffered answer. Both must stay
/// within {Accepted, Io}; a `TimedOut` or a hang would be a regression.
#[test]
fn held_a_peer_that_answers_and_closes_without_reading_stays_in_the_same_outcomes() {
    let mut old_seen = std::collections::BTreeSet::new();
    let mut new_seen = std::collections::BTreeSet::new();
    for new in [false, true] {
        for _ in 0..40 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("fast.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let peer = std::thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                let _ = s.write_all(b"{\"ok\":true,\"sent\":true}\n");
            });
            let out = if new {
                exchange(&path, REQUEST, BUDGET)
            } else {
                old_exchange(&path, REQUEST, BUDGET)
            };
            peer.join().unwrap();
            let seen = if new { &mut new_seen } else { &mut old_seen };
            seen.insert(format!("{out:?}"));
            assert!(
                matches!(out, Outcome::Accepted { sent: true } | Outcome::Io),
                "new={new}: {out:?}"
            );
        }
    }
    eprintln!("old saw {old_seen:?}, new saw {new_seen:?}");
}

/// macOS refuses a connect at once when the listener's backlog is full, so the
/// deadline never has to cover connect there. This holds the claim: a full
/// backlog is `Absent`, fast, for both. (On Linux a full backlog blocks the
/// connect in the old exchange and the new one alike; not exercised here.)
#[cfg(target_os = "macos")]
#[test]
fn held_a_full_backlog_is_absent_at_once_on_macos() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("full.sock");
    let _listener = UnixListener::bind(&path).unwrap();
    let mut filled = Vec::new();
    loop {
        match UnixStream::connect(&path) {
            Ok(s) => filled.push(s),
            Err(_) => break,
        }
        assert!(filled.len() < 10_000, "the backlog never filled");
    }
    for exchange_fn in [
        old_exchange as fn(&Path, &[u8], Duration) -> Outcome,
        exchange,
    ] {
        let t = Instant::now();
        assert_eq!(exchange_fn(&path, REQUEST, BUDGET), Outcome::Absent);
        assert!(
            t.elapsed() < Duration::from_millis(300),
            "{:?}",
            t.elapsed()
        );
    }
}

// ---------------------------------------------------------------------------
// Scripted differential: a deterministic generator drives both implementations
// through the same socket behaviour and compares what each reported and what
// each wrote.

#[derive(Clone, Debug)]
enum W {
    Take(usize),
    Fail(ErrorKind),
}

#[derive(Clone, Debug)]
enum R {
    Data(Vec<u8>),
    Eof,
    Fail(ErrorKind),
}

#[derive(Clone, Debug, Default)]
struct Script {
    set_read_error: Option<ErrorKind>,
    set_write_error: Option<ErrorKind>,
    writes: Vec<W>,
    reads: Vec<R>,
}

#[derive(Default)]
struct Log {
    written: Vec<u8>,
    write_calls: usize,
}

struct Fake {
    script: Script,
    log: Rc<RefCell<Log>>,
    next_write: usize,
    next_read: usize,
}

impl Fake {
    fn new(script: &Script) -> (Self, Rc<RefCell<Log>>) {
        let log = Rc::new(RefCell::new(Log::default()));
        (
            Self {
                script: script.clone(),
                log: log.clone(),
                next_write: 0,
                next_read: 0,
            },
            log,
        )
    }
}

impl Timed for Fake {
    fn set_read(&self, _: Option<Duration>) -> std::io::Result<()> {
        self.script
            .set_read_error
            .map_or(Ok(()), |k| Err(std::io::Error::from(k)))
    }
    fn set_write(&self, _: Option<Duration>) -> std::io::Result<()> {
        self.script
            .set_write_error
            .map_or(Ok(()), |k| Err(std::io::Error::from(k)))
    }
}

impl Write for Fake {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let step = self
            .script
            .writes
            .get(self.next_write)
            .cloned()
            .unwrap_or(W::Take(usize::MAX));
        self.next_write += 1;
        self.log.borrow_mut().write_calls += 1;
        match step {
            W::Take(n) => {
                let n = n.min(buf.len());
                self.log.borrow_mut().written.extend_from_slice(&buf[..n]);
                Ok(n)
            }
            W::Fail(kind) => Err(std::io::Error::from(kind)),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for Fake {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let step = self
            .script
            .reads
            .get(self.next_read)
            .cloned()
            .unwrap_or(R::Eof);
        self.next_read += 1;
        match step {
            R::Data(mut bytes) => {
                bytes.truncate(buf.len());
                buf[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
            R::Eof => Ok(0),
            R::Fail(kind) => Err(std::io::Error::from(kind)),
        }
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const KINDS: [ErrorKind; 9] = [
    ErrorKind::Interrupted,
    ErrorKind::WouldBlock,
    ErrorKind::TimedOut,
    ErrorKind::InvalidInput,
    ErrorKind::BrokenPipe,
    ErrorKind::ConnectionReset,
    ErrorKind::PermissionDenied,
    ErrorKind::UnexpectedEof,
    ErrorKind::Other,
];

fn script(rng: &mut Rng, with_timeout_faults: bool) -> Script {
    let pick = |rng: &mut Rng| KINDS[rng.below(KINDS.len() as u64) as usize];
    let mut s = Script::default();
    if with_timeout_faults && rng.below(3) == 0 {
        s.set_read_error = Some(pick(rng));
        s.set_write_error = Some(pick(rng));
    } else if rng.below(4) == 0 {
        // The macOS case: every timeout refused with EINVAL.
        s.set_read_error = Some(ErrorKind::InvalidInput);
        s.set_write_error = Some(ErrorKind::InvalidInput);
    }
    for _ in 0..rng.below(5) {
        s.writes.push(match rng.below(4) {
            0 => W::Fail(pick(rng)),
            1 => W::Take(0),
            _ => W::Take(1 + rng.below(6) as usize),
        });
    }
    let answers: [&[u8]; 5] = [
        b"{\"ok\":true,\"sent\":true}\n",
        b"{\"ok\":false,\"err\":\"nope\"}\n",
        b"garbage\n",
        b"{\"ok\":true",
        b"",
    ];
    for _ in 0..rng.below(6) {
        s.reads.push(match rng.below(4) {
            0 => R::Fail(pick(rng)),
            1 => R::Eof,
            _ => {
                let whole = answers[rng.below(answers.len() as u64) as usize];
                let cut = 1 + rng.below(whole.len().max(1) as u64) as usize;
                R::Data(whole[..cut.min(whole.len())].to_vec())
            }
        });
    }
    s
}

fn run_both(script: &Script, deadline: Instant) -> ((Outcome, Vec<u8>), (Outcome, Vec<u8>)) {
    let (fake, log) = Fake::new(script);
    let old = old_converse(fake, REQUEST, deadline);
    let old_written = log.borrow().written.clone();
    let (fake, log) = Fake::new(script);
    let new = converse(fake, REQUEST, deadline);
    let new_written = log.borrow().written.clone();
    ((old, old_written), (new, new_written))
}

/// Timeouts that can be set (or are refused with EINVAL): every outcome and every
/// byte written must be identical, over 20 000 scripted sockets.
#[test]
fn held_scripted_sockets_with_no_timeout_fault_give_identical_outcomes_and_bytes() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let deadline = Instant::now() + Duration::from_secs(3600);
    for n in 0..20_000 {
        let s = script(&mut rng, false);
        let (old, new) = run_both(&s, deadline);
        assert_eq!(old, new, "case {n}: {s:?}");
    }
}

/// An expired deadline: both time out without writing a byte.
#[test]
fn held_an_expired_deadline_is_timed_out_before_any_write() {
    let mut rng = Rng(42);
    let past = Instant::now() - Duration::from_secs(1);
    for _ in 0..500 {
        let s = script(&mut rng, true);
        let (old, new) = run_both(&s, past);
        assert_eq!(old, (Outcome::TimedOut, vec![]));
        assert_eq!(new, (Outcome::TimedOut, vec![]));
    }
}

/// `set_*_timeout` refused with a kind that is not EINVAL: the old exchange
/// sets both timeouts before it sends anything, so it reported `Io` having sent
/// nothing. The new one sets the write timeout, sends, and only then finds the
/// read timeout cannot be set; it reports the same `Io` but the request is
/// already out. Graft may have shown the banner for an exchange the hook
/// reports as failed.
#[test]
fn broke_a_read_timeout_that_cannot_be_set_no_longer_stops_the_request_being_sent() {
    let s = Script {
        set_read_error: Some(ErrorKind::PermissionDenied),
        set_write_error: None,
        writes: vec![],
        reads: vec![R::Data(b"{\"ok\":true,\"sent\":true}\n".to_vec())],
    };
    let ((old_out, old_written), (new_out, new_written)) =
        run_both(&s, Instant::now() + Duration::from_secs(60));
    assert_eq!(old_out, Outcome::Io);
    assert_eq!(new_out, Outcome::Io);
    assert_eq!(old_written, Vec::<u8>::new(), "old sent nothing");
    assert_eq!(
        new_written, old_written,
        "new sent the request before finding the read timeout unusable"
    );
}

/// A timeout setter that fails with `TimedOut` (not a deadline: the setter
/// refused). Old: `Io`. New: `failed()` reads the kind and says `TimedOut`.
///
/// Accepted as a documented change (coordinator ruling, W3b rework round 1): the
/// outcome is a failure either way, nothing is sent (`arm` runs first), and a
/// real `setsockopt` does not return `ETIMEDOUT`, so this is not a practical
/// case. The test pins what now happens instead of the old mapping.
#[test]
fn a_timeout_setter_that_fails_with_timed_out_is_now_reported_timed_out_with_nothing_sent() {
    let s = Script {
        set_write_error: Some(ErrorKind::TimedOut),
        ..Script::default()
    };
    let ((old, _), (new, new_written)) = run_both(&s, Instant::now() + Duration::from_secs(60));
    assert_eq!(old, Outcome::Io, "the exchange of 2abaa1ba");
    assert_eq!(new, Outcome::TimedOut, "documented change, see above");
    assert_eq!(new_written, Vec::<u8>::new());
}
