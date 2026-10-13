//! The single-instance guard, attacked from the outside.
//!
//! The in-process cases pin each branch of `claim`. The multi-process cases
//! are the ones that matter: launches really racing, an owner really killed,
//! and a flood of real senders — `flock` and unix sockets behave differently
//! across processes than across descriptors, and only a process can die.

use super::*;
use crate::procguard::LockedSpawn as _;
use crate::test_support::{coverage_relaxed, isolated_libtest_command};
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;
use std::process::{Child, Stdio};
use std::sync::mpsc;

const CONTENDER: &str = "desktop::instance::tests::contender";
const ENV_DIR: &str = "GITPULSE_INSTANCE_TEST_DIR";
const ENV_MODE: &str = "GITPULSE_INSTANCE_TEST_MODE";
const ENV_EXPECT: &str = "GITPULSE_INSTANCE_TEST_EXPECT";
const ENV_GO: &str = "GITPULSE_INSTANCE_TEST_GO";
const ENV_PATH: &str = "GITPULSE_INSTANCE_TEST_PATH";

fn profile() -> tempfile::TempDir {
    // A short prefix: the socket path must fit in `sun_path`, and the per-user
    // temp dir on macOS is already ~50 bytes deep.
    let dir = tempfile::Builder::new()
        .prefix("gpi")
        .tempdir()
        .expect("profile dir");
    assert!(
        Paths::in_dir(dir.path()).socket_fits(),
        "fixture: {} is too deep for a socket",
        dir.path().display()
    );
    dir
}

/// A directory whose mailbox path is exactly `len` bytes long once resolved,
/// which is the path `claim` binds.
fn profile_with_socket_len(base: &Path, len: usize) -> PathBuf {
    let base = base.canonicalize().unwrap();
    let fixed = base.as_os_str().len() + 1 + 1 + SOCKET_FILE.len();
    assert!(len > fixed, "fixture: base too deep for a {len}-byte path");
    let dir = base.join("x".repeat(len - fixed));
    assert_eq!(Paths::in_dir(&dir).socket.as_os_str().len(), len);
    dir
}

fn primary(dir: &Path) -> Primary {
    match claim(dir, Duration::from_millis(200)) {
        Claim::Primary(primary) => {
            assert_eq!(primary.mailbox_error(), None);
            primary
        }
        other => panic!(
            "expected to own {}, got {}",
            dir.display(),
            describe(&other)
        ),
    }
}

fn secondary(dir: &Path) -> Secondary {
    match claim(dir, coverage_relaxed(CLAIM_DEADLINE)) {
        Claim::Secondary(secondary) => secondary,
        other => panic!(
            "expected an owner at {}, got {}",
            dir.display(),
            describe(&other)
        ),
    }
}

fn describe(claim: &Claim) -> String {
    match claim {
        Claim::Primary(p) => format!("Primary(mailbox_error={:?})", p.mailbox_error()),
        Claim::Secondary(s) => format!("Secondary(owner={})", s.owner_pid()),
        Claim::Blocked { pid, reason } => format!("Blocked(pid={pid:?}, {reason})"),
        Claim::Unavailable(reason) => format!("Unavailable({reason})"),
    }
}

fn recorder(primary: &Primary) -> mpsc::Receiver<Request> {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    primary.attach(move |request| {
        let _ = tx.lock().unwrap().send(request);
    });
    rx
}

/// A raw client that has read the greeting and can send anything.
fn raw(dir: &Path) -> (UnixStream, BufReader<UnixStream>) {
    let stream = UnixStream::connect(dir.join(SOCKET_FILE)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let hello = read_frame(&mut reader).unwrap().expect("greeting");
    assert!(hello.contains("\"v\":1"), "greeting: {hello}");
    (stream, reader)
}

fn ack_of(stream: &mut UnixStream, reader: &mut BufReader<UnixStream>, line: &[u8]) -> Ack {
    stream.write_all(line).unwrap();
    serde_json::from_str(&read_frame(reader).unwrap().expect("answer")).expect("ack json")
}

// ---------------------------------------------------------------------------
// Frames and requests
// ---------------------------------------------------------------------------

#[test]
fn requests_are_bounded_before_anything_acts_on_them() {
    let ok = [
        Request::Reveal,
        Request::Open {
            paths: vec!["/r".into()],
        },
        Request::Notification {
            id: "gitpulse-session-abc".into(),
            snooze: false,
        },
    ];
    for request in ok {
        assert_eq!(request.validate(), Ok(()), "{request:?}");
    }
    let refused = [
        Request::Open { paths: vec![] },
        Request::Open {
            paths: vec!["relative/repo".into()],
        },
        Request::Open {
            paths: vec![String::new()],
        },
        Request::Open {
            paths: vec!["/a\0b".into()],
        },
        Request::Open {
            paths: vec![format!("/{}", "x".repeat(MAX_PATH_BYTES))],
        },
        Request::Open {
            paths: vec!["/r".into(); MAX_PATHS + 1],
        },
        Request::Notification {
            id: String::new(),
            snooze: false,
        },
        Request::Notification {
            id: "a\nb".into(),
            snooze: true,
        },
        Request::Notification {
            id: "x".repeat(MAX_NATIVE_ID + 1),
            snooze: false,
        },
    ];
    for request in refused {
        assert!(request.validate().is_err(), "accepted {request:?}");
    }
}

#[test]
fn frames_are_bounded_terminated_and_utf8() {
    let read = |bytes: &[u8]| read_frame(&mut std::io::Cursor::new(bytes.to_vec()));
    assert_eq!(read(b""), Ok(None));
    assert_eq!(read(b"{}\n"), Ok(Some("{}".into())));
    assert!(read(b"{}").is_err(), "an unterminated frame was accepted");
    assert!(
        read(b"\xff\xfe\n").is_err(),
        "a non-UTF-8 frame was accepted"
    );
    let mut huge = vec![b'a'; MAX_FRAME + 10];
    huge.push(b'\n');
    assert!(read(&huge).is_err(), "an oversized frame was accepted");
    let mut exact = vec![b'a'; MAX_FRAME];
    exact.push(b'\n');
    assert!(read(&exact).is_ok(), "a frame at the limit was refused");
}

#[test]
fn only_this_user_may_hand_the_owner_work() {
    assert!(peer_allowed(Some(501), 501));
    assert!(!peer_allowed(Some(0), 501));
    assert!(!peer_allowed(Some(502), 501));
    assert!(!peer_allowed(None, 501), "an unreadable peer was admitted");
}

// ---------------------------------------------------------------------------
// Claim, in process
// ---------------------------------------------------------------------------

#[test]
fn the_first_launch_owns_and_the_next_one_forwards() {
    let dir = profile();
    let owner = primary(dir.path());
    let received = recorder(&owner);
    let launch = secondary(dir.path());
    assert_eq!(launch.owner_pid(), std::process::id());
    launch.forward(&Request::Reveal).expect("forward");
    assert_eq!(
        received.recv_timeout(Duration::from_secs(5)),
        Ok(Request::Reveal)
    );
    let mode = |path: PathBuf| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode(dir.path().join(SOCKET_FILE)),
        0o600,
        "mailbox is reachable by other users"
    );
    assert_eq!(
        mode(dir.path().to_path_buf()),
        0o700,
        "profile is open to other users"
    );
    assert_eq!(
        read_pid(&dir.path().join(LOCK_FILE)),
        Some(std::process::id())
    );
}

#[test]
fn requests_before_the_window_exists_are_queued_in_order_and_bounded() {
    let dir = profile();
    let owner = primary(dir.path());
    for n in 0..MAX_QUEUED {
        secondary(dir.path())
            .forward(&Request::Open {
                paths: vec![format!("/r{n}")],
            })
            .expect("queued");
    }
    let overflow = secondary(dir.path()).forward(&Request::Reveal);
    assert!(overflow.is_err(), "the startup queue is unbounded");
    let received = recorder(&owner);
    for n in 0..MAX_QUEUED {
        assert_eq!(
            received.recv_timeout(Duration::from_secs(1)),
            Ok(Request::Open {
                paths: vec![format!("/r{n}")]
            })
        );
    }
    secondary(dir.path())
        .forward(&Request::Reveal)
        .expect("after attach");
    assert_eq!(
        received.recv_timeout(Duration::from_secs(5)),
        Ok(Request::Reveal)
    );
}

#[test]
fn malformed_frames_are_refused_and_the_owner_keeps_answering() {
    let dir = profile();
    let owner = primary(dir.path());
    let received = recorder(&owner);
    let refused: &[&[u8]] = &[
        b"not json\n",
        b"{\"v\":2,\"request\":{\"kind\":\"reveal\"}}\n",
        b"{\"v\":1,\"request\":{\"kind\":\"reveal\"},\"extra\":1}\n",
        b"{\"v\":1,\"request\":{\"kind\":\"launch_missiles\"}}\n",
        b"{\"v\":1,\"request\":{\"kind\":\"open\",\"paths\":[\"relative\"]}}\n",
        b"{\"v\":1,\"request\":{\"kind\":\"notification\",\"id\":\"\",\"snooze\":false}}\n",
    ];
    for line in refused {
        let (mut stream, mut reader) = raw(dir.path());
        let ack = ack_of(&mut stream, &mut reader, line);
        assert!(!ack.accepted, "accepted {}", String::from_utf8_lossy(line));
        assert!(ack.reason.is_some());
    }
    // Oversized: refused or cut off, never acted on.
    let (mut stream, mut reader) = raw(dir.path());
    let mut huge = vec![b'x'; MAX_FRAME * 2];
    huge.push(b'\n');
    let _ = stream.write_all(&huge);
    if let Ok(Some(line)) = read_frame(&mut reader) {
        let ack: Ack = serde_json::from_str(&line).unwrap();
        assert!(!ack.accepted);
    }
    assert!(
        received.try_recv().is_err(),
        "a refused frame reached the app"
    );
    secondary(dir.path())
        .forward(&Request::Reveal)
        .expect("still answering");
    assert_eq!(
        received.recv_timeout(Duration::from_secs(5)),
        Ok(Request::Reveal)
    );
}

/// An owner that greets and then records what it was sent.
fn listening_fake(dir: &Path) -> (File, std::thread::JoinHandle<Vec<u8>>) {
    let paths = Paths::in_dir(dir);
    let held = open_lock(&paths.lock).unwrap();
    assert!(try_lock(&held).unwrap());
    let listener =
        crate::procguard::with_inheritance_lock(|| UnixListener::bind(&paths.socket)).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"{\"v\":1,\"pid\":7}\n").unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let mut sent = Vec::new();
        let _ = stream.read_to_end(&mut sent);
        sent
    });
    (held, server)
}

#[test]
fn a_launch_never_sends_what_the_owner_would_refuse() {
    let invalid = [
        Request::Open {
            paths: vec!["relative".into()],
        },
        Request::Notification {
            id: String::new(),
            snooze: false,
        },
    ];
    for request in invalid {
        let dir = profile();
        let (_held, server) = listening_fake(dir.path());
        assert!(
            secondary(dir.path()).forward(&request).is_err(),
            "{request:?}"
        );
        assert!(server.join().unwrap().is_empty(), "sent {request:?} anyway");
    }
}

#[test]
fn the_largest_valid_request_fits_in_one_frame() {
    // Every path at its limit, every byte one JSON spells as six.
    let path = format!("/{}", "\u{1}".repeat(MAX_PATH_BYTES - 1));
    let request = Request::Open {
        paths: vec![path; MAX_PATHS],
    };
    assert_eq!(request.validate(), Ok(()));
    let dir = profile();
    let owner = primary(dir.path());
    let received = recorder(&owner);
    secondary(dir.path())
        .forward(&request)
        .expect("largest valid request");
    assert_eq!(received.recv_timeout(Duration::from_secs(5)), Ok(request));
}

#[test]
fn a_liveness_probe_that_sends_nothing_is_harmless() {
    let dir = profile();
    let owner = primary(dir.path());
    let received = recorder(&owner);
    for _ in 0..20 {
        drop(raw(dir.path()));
    }
    secondary(dir.path())
        .forward(&Request::Reveal)
        .expect("forward");
    assert_eq!(
        received.recv_timeout(Duration::from_secs(5)),
        Ok(Request::Reveal)
    );
    assert!(received.try_recv().is_err());
}

// ---------------------------------------------------------------------------
// How a conversation that ended early is classified (and so logged)
// ---------------------------------------------------------------------------

/// Runs one conversation over a socket pair: `peer` is the client end, handed
/// to the closure and dropped when it returns, then the owner answers it.
fn converse(peer: impl FnOnce(&mut UnixStream)) -> (Result<(), Ended>, Shared) {
    let (mut server, mut client) = UnixStream::pair().expect("socket pair");
    peer(&mut client);
    drop(client);
    // SAFETY: `getuid` has no preconditions.
    let shared = Shared::new(unsafe { libc::getuid() });
    (answer(&mut server, &shared), shared)
}

#[test]
fn a_peer_that_leaves_before_it_is_greeted_is_a_probe_logged_at_debug() {
    // What a hook's liveness check does: connect, close, never read or write.
    // The owner's greeting then meets a closed socket (EPIPE, or on macOS an
    // EINVAL setting the socket up) — 3,280 `warn` lines in the installed log.
    let (outcome, _) = converse(|_| {});
    let Err(ended) = outcome else {
        panic!("a peer that left cannot have been served")
    };
    assert!(matches!(ended, Ended::Probe(_)), "{ended:?}");
    let (level, line) = ended.log_line();
    assert_eq!(level, log::Level::Debug, "{line}");
}

#[test]
fn a_failed_greeting_is_a_probe_only_when_the_peer_is_gone_and_sent_nothing() {
    // The 3,033 installed-log lines: the greeting write failed with EPIPE. The
    // classifier is the same whichever step failed first, so it is checked on
    // its own against each state the peer can be in.
    let epipe = || "write: Broken pipe (os error 32)".to_string();

    let (server, client) = UnixStream::pair().unwrap();
    drop(client);
    assert_eq!(
        Ended::before_first_frame(&server, epipe()),
        Ended::Probe(epipe()),
        "gone, and it never wrote"
    );

    let (server, _held) = UnixStream::pair().unwrap();
    assert_eq!(
        Ended::before_first_frame(&server, epipe()),
        Ended::Refused(epipe()),
        "still connected: the failure is not explained by a probe leaving"
    );

    let (server, mut client) = UnixStream::pair().unwrap();
    client.write_all(b"{\"v\":1").unwrap();
    drop(client);
    assert_eq!(
        Ended::before_first_frame(&server, epipe()),
        Ended::Refused(epipe()),
        "gone, but it had started to say something"
    );
}

#[test]
fn a_peer_that_leaves_after_the_greeting_without_a_frame_ends_quietly() {
    let (mut server, client) = UnixStream::pair().unwrap();
    // SAFETY: `getuid` has no preconditions.
    let shared = Shared::new(unsafe { libc::getuid() });
    let reader = std::thread::spawn(move || {
        let mut greeting = BufReader::new(client);
        read_frame(&mut greeting).unwrap().expect("greeting");
    });
    // The client closes after reading the greeting: end of file, not an error.
    let outcome = answer(&mut server, &shared);
    reader.join().unwrap();
    assert_eq!(outcome, Ok(()));
}

#[test]
fn a_peer_that_sent_part_of_a_frame_is_refused_at_warn() {
    // Hello reaches the peer, which answers with half a request and goes.
    let (mut server, client) = UnixStream::pair().unwrap();
    // SAFETY: `getuid` has no preconditions.
    let shared = Shared::new(unsafe { libc::getuid() });
    let peer = std::thread::spawn(move || {
        let mut writer = client.try_clone().unwrap();
        let mut greeting = BufReader::new(client);
        read_frame(&mut greeting).unwrap().expect("greeting");
        writer
            .write_all(b"{\"v\":1,\"request\":{\"op\":\"reve")
            .unwrap();
    });
    let outcome = answer(&mut server, &shared);
    peer.join().unwrap();
    let Err(ended) = outcome else {
        panic!("half a frame cannot be served")
    };
    assert!(matches!(ended, Ended::Refused(_)), "{ended:?}");
    let (level, line) = ended.log_line();
    assert_eq!(level, log::Level::Warn);
    assert!(line.contains("refused a launch"), "{line}");
    assert!(line.contains("newline"), "{line}");
}

#[test]
fn a_peer_that_sent_bytes_and_left_before_the_greeting_is_still_refused() {
    // The greeting fails with the same EPIPE a probe causes, but this peer
    // asked for something: that it left is not a reason to stop saying so.
    let (outcome, _) = converse(|peer| {
        peer.write_all(b"{\"v\":1,\"request\":{\"op\":\"revea")
            .unwrap();
    });
    let Err(ended) = outcome else {
        panic!("a peer that left cannot have been served")
    };
    assert!(matches!(ended, Ended::Refused(_)), "{ended:?}");
    assert_eq!(ended.log_line().0, log::Level::Warn);
}

#[test]
fn a_broken_pipe_while_answering_a_real_request_is_refused_at_warn() {
    // A complete, valid request from a sender that has stopped listening: the
    // greeting reaches it, then it closes its read side before sending, so the
    // ack write is the EPIPE. That is after the first frame, so not a probe.
    let (mut server, client) = UnixStream::pair().unwrap();
    // SAFETY: `getuid` has no preconditions.
    let shared = Shared::new(unsafe { libc::getuid() });
    let line = serde_json::to_string(&Envelope {
        v: PROTOCOL,
        request: Request::Reveal,
    })
    .unwrap();
    let peer = std::thread::spawn(move || {
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client.try_clone().unwrap());
        read_frame(&mut reader).unwrap().expect("greeting");
        client.shutdown(std::net::Shutdown::Read).unwrap();
        writer.write_all(format!("{line}\n").as_bytes()).unwrap();
    });
    let outcome = answer(&mut server, &shared);
    peer.join().unwrap();
    let Err(ended) = outcome else {
        panic!("an unanswerable request is not a served one")
    };
    assert!(
        matches!(&ended, Ended::Refused(reason) if reason.starts_with("write:")),
        "{ended:?}"
    );
    assert_eq!(ended.log_line().0, log::Level::Warn);
}

#[test]
fn a_valid_frame_is_handled_and_logs_nothing() {
    let (mut server, client) = UnixStream::pair().unwrap();
    // SAFETY: `getuid` has no preconditions.
    let shared = Shared::new(unsafe { libc::getuid() });
    let line = serde_json::to_string(&Envelope {
        v: PROTOCOL,
        request: Request::Reveal,
    })
    .unwrap();
    let peer = std::thread::spawn(move || {
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);
        read_frame(&mut reader).unwrap().expect("greeting");
        writer.write_all(format!("{line}\n").as_bytes()).unwrap();
        let ack: Ack =
            serde_json::from_str(&read_frame(&mut reader).unwrap().expect("ack")).unwrap();
        ack
    });
    let outcome = answer(&mut server, &shared);
    let ack = peer.join().unwrap();
    assert_eq!(outcome, Ok(()));
    assert!(ack.accepted, "ack: {:?}", ack.reason);
    assert_eq!(shared.inbox.lock().unwrap().queued.len(), 1);
}

#[test]
fn a_stranger_is_refused_at_warn_not_taken_for_a_probe() {
    // Held open: the peer is connected and silent, and it is not our user.
    let (mut server, _client) = UnixStream::pair().unwrap();
    // SAFETY: `getuid` has no preconditions.
    let own = unsafe { libc::getuid() };
    let shared = Shared::new(own.wrapping_add(1));
    let outcome = answer(&mut server, &shared);
    let Err(ended) = outcome else {
        panic!("another user's connection must not be served")
    };
    assert!(matches!(ended, Ended::Refused(_)), "{ended:?}");
    assert_eq!(ended.log_line().0, log::Level::Warn);
}

#[test]
fn senders_that_stall_cannot_starve_a_launch() {
    let dir = profile();
    let owner = primary(dir.path());
    let received = recorder(&owner);
    // Fill every handler with a connection that greets and then says nothing.
    let stalled: Vec<_> = (0..MAX_HANDLERS).map(|_| raw(dir.path())).collect();
    let started = Instant::now();
    secondary(dir.path())
        .forward(&Request::Reveal)
        .expect("forward past stalled senders");
    let waited = started.elapsed();
    assert!(
        waited < coverage_relaxed(CLAIM_DEADLINE),
        "waited {waited:?}, past the claim deadline"
    );
    assert_eq!(
        received.recv_timeout(Duration::from_secs(5)),
        Ok(Request::Reveal)
    );
    drop(stalled);
}

#[test]
fn an_owner_that_never_answers_blocks_the_launch_instead_of_doubling() {
    let dir = profile();
    let paths = Paths::in_dir(dir.path());
    let mut held = open_lock(&paths.lock).unwrap();
    assert!(try_lock(&held).unwrap());
    write!(held, "4242").unwrap();
    let deadline = Duration::from_millis(400);
    let started = Instant::now();
    match claim(dir.path(), deadline) {
        Claim::Blocked { pid, .. } => assert_eq!(pid, Some(4242)),
        other => panic!("expected Blocked, got {}", describe(&other)),
    }
    let waited = started.elapsed();
    assert!(waited >= deadline, "gave up early: {waited:?}");
    assert!(
        waited < deadline + coverage_relaxed(Duration::from_secs(1)),
        "overran: {waited:?}"
    );
}

#[test]
fn an_owner_still_starting_is_waited_for() {
    let dir = profile();
    // As `claim` would: the mailbox is bound on the resolved path.
    let paths = Paths::in_dir(&dir.path().canonicalize().unwrap());
    let held = open_lock(&paths.lock).unwrap();
    assert!(try_lock(&held).unwrap());
    // Lock taken, mailbox not yet bound: the window an owner's startup has.
    let starter = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        Primary::start(held, paths)
    });
    let launch = secondary(dir.path());
    let owner = starter.join().unwrap();
    let received = recorder(&owner);
    launch.forward(&Request::Reveal).expect("forward");
    assert_eq!(
        received.recv_timeout(Duration::from_secs(5)),
        Ok(Request::Reveal)
    );
}

#[test]
fn a_launch_during_a_quit_becomes_the_next_owner() {
    let dir = profile();
    let owner = primary(dir.path());
    owner.shutdown();
    assert!(
        !dir.path().join(SOCKET_FILE).exists(),
        "quit left the mailbox up"
    );
    let leaving = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        drop(owner);
    });
    let next = primary_within(dir.path(), coverage_relaxed(Duration::from_secs(3)));
    leaving.join().unwrap();
    let received = recorder(&next);
    secondary(dir.path())
        .forward(&Request::Reveal)
        .expect("the new owner answers");
    assert_eq!(
        received.recv_timeout(Duration::from_secs(5)),
        Ok(Request::Reveal)
    );
}

fn primary_within(dir: &Path, deadline: Duration) -> Primary {
    match claim(dir, deadline) {
        Claim::Primary(primary) => primary,
        other => panic!(
            "expected to own {}, got {}",
            dir.display(),
            describe(&other)
        ),
    }
}

#[test]
fn a_quitting_owner_refuses_work_it_could_not_do() {
    let dir = profile();
    let owner = primary(dir.path());
    let _received = recorder(&owner);
    let launch = secondary(dir.path());
    owner.shutdown();
    assert!(
        launch.forward(&Request::Reveal).is_err(),
        "a quitting owner accepted work"
    );
}

#[test]
fn a_deleted_lock_file_does_not_crown_a_second_owner() {
    let dir = profile();
    let owner = primary(dir.path());
    let received = recorder(&owner);
    std::fs::remove_file(dir.path().join(LOCK_FILE)).unwrap();
    // This launch locks a brand-new file, so the lock alone says "yours". The
    // live mailbox must outrank it.
    secondary(dir.path())
        .forward(&Request::Reveal)
        .expect("forward");
    assert_eq!(
        received.recv_timeout(Duration::from_secs(5)),
        Ok(Request::Reveal)
    );
}

#[test]
fn an_owner_that_steps_down_leaves_no_mailbox_behind() {
    let dir = profile();
    let owner = primary(dir.path());
    assert!(dir.path().join(SOCKET_FILE).exists());
    drop(owner);
    assert!(
        !dir.path().join(SOCKET_FILE).exists(),
        "a dropped owner left its socket"
    );
    // And the profile is free for the next launch at once.
    drop(primary(dir.path()));
}

#[test]
fn a_file_squatting_on_the_mailbox_is_never_deleted() {
    let dir = profile();
    let squatter = dir.path().join(SOCKET_FILE);
    std::fs::write(&squatter, b"not ours").unwrap();
    let owner = match claim(dir.path(), Duration::from_millis(200)) {
        Claim::Primary(owner) => owner,
        other => panic!("expected Primary, got {}", describe(&other)),
    };
    let error = owner.mailbox_error().expect("bound over a regular file");
    assert!(error.contains("not a socket"), "{error}");
    drop(owner);
    assert_eq!(std::fs::read(&squatter).unwrap(), b"not ours");
}

#[test]
fn a_profile_reached_through_a_symlink_is_the_same_profile() {
    let base = profile();
    let real = base.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = base.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let owner = primary(&link);
    let received = recorder(&owner);
    // One launch names the profile through the link, the next directly.
    secondary(&real).forward(&Request::Reveal).expect("forward");
    assert_eq!(
        received.recv_timeout(Duration::from_secs(5)),
        Ok(Request::Reveal)
    );
}

#[test]
fn a_dead_owners_socket_is_replaced() {
    let dir = profile();
    let socket = dir.path().join(SOCKET_FILE);
    drop(crate::procguard::with_inheritance_lock(|| UnixListener::bind(&socket)).unwrap());
    assert!(socket.exists(), "fixture: stale socket file");
    let owner = primary(dir.path());
    let received = recorder(&owner);
    secondary(dir.path())
        .forward(&Request::Reveal)
        .expect("forward");
    assert_eq!(
        received.recv_timeout(Duration::from_secs(5)),
        Ok(Request::Reveal)
    );
}

#[test]
fn a_socket_path_too_long_to_bind_still_keeps_one_owner() {
    let base = profile();
    let deep = base.path().join("d".repeat(120));
    let owner = match claim(&deep, Duration::from_millis(200)) {
        Claim::Primary(primary) => primary,
        other => panic!("expected Primary, got {}", describe(&other)),
    };
    assert!(owner.mailbox_error().is_some());
    let started = Instant::now();
    match claim(&deep, CLAIM_DEADLINE) {
        Claim::Blocked { pid, .. } => assert_eq!(pid, Some(std::process::id())),
        other => panic!("expected Blocked, got {}", describe(&other)),
    }
    assert!(
        started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(1)),
        "waited for a mailbox that cannot exist"
    );
}

#[test]
fn the_longest_bindable_socket_path_is_used_and_one_longer_is_not() {
    let base = profile();
    let limit = crate::local_socket::max_path_bytes();
    let longest = profile_with_socket_len(base.path(), limit);
    let owner = primary(&longest);
    let received = recorder(&owner);
    secondary(&longest)
        .forward(&Request::Reveal)
        .expect("forward at the limit");
    assert_eq!(
        received.recv_timeout(Duration::from_secs(5)),
        Ok(Request::Reveal)
    );

    let over = profile_with_socket_len(base.path(), limit + 1);
    match claim(&over, Duration::from_millis(200)) {
        Claim::Primary(owner) => assert!(owner.mailbox_error().is_some()),
        other => panic!("expected Primary, got {}", describe(&other)),
    }
}

#[test]
fn an_unusable_profile_is_reported_not_assumed_alone() {
    let base = profile();
    let file = base.path().join("file");
    std::fs::write(&file, b"").unwrap();
    assert!(matches!(
        claim(&file.join("profile"), CLAIM_DEADLINE),
        Claim::Unavailable(_)
    ));
}

#[test]
fn an_owner_on_another_protocol_is_still_an_owner() {
    for hold_lock in [true, false] {
        let dir = profile();
        let paths = Paths::in_dir(dir.path());
        let held = open_lock(&paths.lock).unwrap();
        if hold_lock {
            assert!(try_lock(&held).unwrap());
        } else {
            drop(held);
        }
        let listener =
            crate::procguard::with_inheritance_lock(|| UnixListener::bind(&paths.socket)).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"{\"v\":99,\"pid\":7}\n").unwrap();
            std::thread::sleep(Duration::from_millis(200));
        });
        match claim(dir.path(), CLAIM_DEADLINE) {
            Claim::Blocked { pid, .. } => assert_eq!(pid, Some(7), "hold_lock={hold_lock}"),
            other => panic!(
                "hold_lock={hold_lock}: expected Blocked, got {}",
                describe(&other)
            ),
        }
        server.join().unwrap();
    }
}

// ---------------------------------------------------------------------------
// Across processes
// ---------------------------------------------------------------------------

/// The other side of the multi-process cases. Does nothing in a normal run.
#[test]
fn contender() {
    let (Some(dir), Some(mode)) = (std::env::var_os(ENV_DIR), std::env::var(ENV_MODE).ok()) else {
        return;
    };
    let dir = PathBuf::from(dir);
    if let Some(go) = std::env::var_os(ENV_GO) {
        let go = PathBuf::from(go);
        let until = Instant::now() + Duration::from_secs(30);
        while !go.exists() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let outcome = claim(&dir, coverage_relaxed(CLAIM_DEADLINE));
    match (mode.as_str(), outcome) {
        ("race", Claim::Primary(owner)) => {
            let expect: usize = std::env::var(ENV_EXPECT).unwrap().parse().unwrap();
            let received = recorder(&owner);
            let mut count = 0;
            let until = Instant::now() + coverage_relaxed(Duration::from_secs(30));
            while count < expect && Instant::now() < until {
                if received.recv_timeout(Duration::from_millis(100)).is_ok() {
                    count += 1;
                }
            }
            println!("ROLE primary {count}");
        }
        ("race" | "forward", Claim::Secondary(launch)) => {
            let request = match std::env::var(ENV_PATH) {
                Ok(path) => Request::Open { paths: vec![path] },
                Err(_) => Request::Reveal,
            };
            match launch.forward(&request) {
                Ok(()) => println!("ROLE secondary ok"),
                Err(error) => println!("ROLE secondary failed {error}"),
            }
        }
        ("hold", Claim::Primary(owner)) => {
            let _received = recorder(&owner);
            // A child that would keep the lock alive if it were inherited.
            let mut sleeper = std::process::Command::new("/bin/sleep")
                .arg("60")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn_locked()
                .expect("spawn sleeper");
            println!("SLEEPER {}", sleeper.id());
            println!("READY");
            // The parent kills this process long before the sleeper ends.
            let _ = sleeper.wait();
        }
        (mode, other) => println!("ROLE unexpected {mode} {}", describe(&other)),
    }
}

/// Contender processes, all run from one copy of this test harness: copying a
/// 100+ MiB image per child made every launch a first exec of a new binary,
/// which the kernel validates one at a time — dozens of them at once wedged at
/// `_dyld_start` under load, and cost gigabytes of disk.
struct Contenders {
    _harness: crate::test_support::IsolatedHarnessGuard,
    exe: PathBuf,
    children: Vec<Child>,
}

impl Contenders {
    fn new() -> Self {
        let (_command, harness) = isolated_libtest_command(CONTENDER);
        let exe = harness.path().to_path_buf();
        Self {
            _harness: harness,
            exe,
            children: Vec::new(),
        }
    }

    fn spawn(&mut self, dir: &Path, mode: &str, extra: &[(&str, String)]) -> &mut Child {
        let mut command = std::process::Command::new(&self.exe);
        command
            .args(["--exact", CONTENDER, "--nocapture"])
            .env(ENV_DIR, dir)
            .env(ENV_MODE, mode)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in extra {
            command.env(key, value);
        }
        self.children
            .push(command.spawn_locked().expect("spawn contender"));
        self.children.last_mut().unwrap()
    }

    /// Every child's `ROLE` lines. A child still running at `deadline` is
    /// killed and the test fails, rather than hanging the suite.
    fn roles(mut self, deadline: Duration) -> Vec<String> {
        let (tx, rx) = mpsc::channel();
        let pids: Vec<u32> = self.children.iter().map(Child::id).collect();
        let count = self.children.len();
        for child in self.children.drain(..) {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(child.wait_with_output());
            });
        }
        let until = Instant::now() + deadline;
        let mut roles = Vec::new();
        for _ in 0..count {
            let left = until.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(output) => roles.extend(
                    String::from_utf8_lossy(&output.expect("contender output").stdout)
                        .lines()
                        .filter(|line| line.starts_with("ROLE "))
                        .map(str::to_owned),
                ),
                Err(_) => {
                    for pid in &pids {
                        // SAFETY: signalling children this test spawned.
                        unsafe { libc::kill(*pid as i32, libc::SIGKILL) };
                    }
                    panic!("contenders still running after {deadline:?}; so far: {roles:#?}");
                }
            }
        }
        roles
    }
}

#[test]
fn racing_launches_elect_exactly_one_owner_and_lose_no_request() {
    const ROUNDS: usize = 8;
    const LAUNCHES: usize = 16;
    for round in 0..ROUNDS {
        let dir = profile();
        let go = dir.path().join("go");
        let mut contenders = Contenders::new();
        for _ in 0..LAUNCHES {
            contenders.spawn(
                dir.path(),
                "race",
                &[
                    (ENV_EXPECT, (LAUNCHES - 1).to_string()),
                    (ENV_GO, go.display().to_string()),
                ],
            );
        }
        std::fs::write(&go, b"").unwrap();
        let roles = contenders.roles(coverage_relaxed(Duration::from_secs(60)));
        let owners: Vec<_> = roles
            .iter()
            .filter(|r| r.starts_with("ROLE primary"))
            .collect();
        let forwarded = roles.iter().filter(|r| *r == "ROLE secondary ok").count();
        assert_eq!(owners.len(), 1, "round {round}: {roles:#?}");
        assert_eq!(forwarded, LAUNCHES - 1, "round {round}: {roles:#?}");
        assert_eq!(
            owners[0],
            &format!("ROLE primary {}", LAUNCHES - 1),
            "round {round}: {roles:#?}"
        );
    }
}

#[test]
fn a_flood_of_launches_reaches_the_owner_exactly_once_each() {
    const LAUNCHES: usize = 48;
    let dir = profile();
    let owner = primary(dir.path());
    let received = recorder(&owner);
    let go = dir.path().join("go");
    let mut contenders = Contenders::new();
    for n in 0..LAUNCHES {
        contenders.spawn(
            dir.path(),
            "forward",
            &[
                (ENV_PATH, format!("/flood/{n}")),
                (ENV_GO, go.display().to_string()),
            ],
        );
    }
    std::fs::write(&go, b"").unwrap();
    let roles = contenders.roles(coverage_relaxed(Duration::from_secs(60)));
    assert_eq!(
        roles.iter().filter(|r| *r == "ROLE secondary ok").count(),
        LAUNCHES,
        "{roles:#?}"
    );
    let mut seen = BTreeSet::new();
    while let Ok(request) = received.recv_timeout(Duration::from_millis(500)) {
        let Request::Open { paths } = request else {
            panic!("unexpected {request:?}")
        };
        assert!(
            seen.insert(paths[0].clone()),
            "delivered twice: {}",
            paths[0]
        );
    }
    assert_eq!(seen.len(), LAUNCHES);
}

#[test]
fn a_killed_owner_frees_the_profile_even_if_its_child_lives_on() {
    let dir = profile();
    let mut contenders = Contenders::new();
    let owner = contenders.spawn(dir.path(), "hold", &[]);
    let mut lines = BufReader::new(owner.stdout.take().unwrap()).lines();
    let mut sleeper = None;
    for line in lines.by_ref() {
        let line = line.unwrap();
        if let Some(pid) = line.strip_prefix("SLEEPER ") {
            sleeper = Some(pid.parse::<i32>().unwrap());
        }
        if line == "READY" {
            break;
        }
    }
    let sleeper = sleeper.expect("owner reported its child");
    // While it lives, it owns.
    secondary(dir.path())
        .forward(&Request::Reveal)
        .expect("live owner answers");
    owner.kill().unwrap();
    owner.wait().unwrap();
    // SAFETY: signalling a pid this test spawned and still owns.
    let alive = unsafe { libc::kill(sleeper, 0) } == 0;
    let next = claim(dir.path(), coverage_relaxed(Duration::from_secs(3)));
    // SAFETY: as above; reap the sleeper whatever the claim said.
    unsafe { libc::kill(sleeper, libc::SIGKILL) };
    assert!(alive, "fixture: the owner's child should outlive it");
    match next {
        Claim::Primary(primary) => assert_eq!(primary.mailbox_error(), None),
        other => panic!("a dead owner still holds the profile: {}", describe(&other)),
    }
}

#[test]
fn a_panicking_mailbox_handler_releases_its_slot() {
    let shared = Arc::new(Shared::new(0));
    // More panics than there are slots: without release-on-drop the mailbox
    // would be full after MAX_HANDLERS of them and refuse every later launch.
    for _ in 0..MAX_HANDLERS * 2 {
        shared.active.fetch_add(1, Ordering::AcqRel);
        let slot = HandlerSlot(shared.clone());
        let panicked = std::thread::spawn(move || {
            let _slot = slot;
            panic!("handler panicked");
        })
        .join();
        assert!(panicked.is_err());
    }
    assert_eq!(shared.active.load(Ordering::Acquire), 0);
}

#[test]
fn a_panicking_job_keeps_its_worker_and_gives_back_its_slot() {
    let shared = Arc::new(Shared::new(0));
    let pool = HandlerPool::new(shared.clone());
    shared.active.fetch_add(1, Ordering::AcqRel);
    let slot = HandlerSlot(shared.clone());
    pool.submit(Box::new(move || {
        let _slot = slot;
        panic!("handler panicked");
    }))
    .unwrap();
    // The job unwinds (and prints) on the worker; its slot comes back as it
    // does, and the worker goes back to waiting.
    let waited = Instant::now();
    while shared.active.load(Ordering::Acquire) != 0 || pool.lock().idle != 1 {
        assert!(
            waited.elapsed() < coverage_relaxed(Duration::from_secs(5)),
            "the panicking job never gave back its slot, or its worker ended"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let (done, finished) = mpsc::channel();
    pool.submit(Box::new(move || done.send(()).unwrap()))
        .unwrap();
    finished
        .recv_timeout(Duration::from_secs(5))
        .expect("the next job was never run");
    assert_eq!(
        shared.handler_threads.load(Ordering::Relaxed),
        1,
        "the surviving worker should have taken the next job"
    );
    pool.close();
}

#[test]
fn the_pool_never_starts_more_threads_than_the_mailbox_admits() {
    let shared = Arc::new(Shared::new(0));
    let pool = HandlerPool::new(shared.clone());
    let release = Arc::new(AtomicBool::new(false));
    let ran = Arc::new(AtomicUsize::new(0));
    let jobs = MAX_HANDLERS + MAX_GATE_HANDLERS + 6;
    for _ in 0..jobs {
        let (release, ran) = (release.clone(), ran.clone());
        pool.submit(Box::new(move || {
            while !release.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(2));
            }
            ran.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
    }
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        shared.handler_threads.load(Ordering::Relaxed),
        MAX_HANDLERS + MAX_GATE_HANDLERS
    );
    release.store(true, Ordering::Release);
    let waited = Instant::now();
    while ran.load(Ordering::SeqCst) < jobs {
        assert!(
            waited.elapsed() < coverage_relaxed(Duration::from_secs(5)),
            "queued jobs were not run"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    pool.close();
}

#[test]
fn closing_the_pool_ends_idle_workers() {
    let shared = Arc::new(Shared::new(0));
    let pool = HandlerPool::new(shared);
    let (done, finished) = mpsc::channel();
    pool.submit(Box::new(move || done.send(()).unwrap()))
        .unwrap();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(pool.lock().idle, 1, "the worker should be waiting");
    pool.close();
    let waited = Instant::now();
    while pool.lock().workers > 0 {
        assert!(
            waited.elapsed() < coverage_relaxed(Duration::from_secs(5)),
            "an idle worker outlived the pool"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

// ---------------------------------------------------------------------------
// What an idle mailbox costs
// ---------------------------------------------------------------------------

/// An owner nobody is talking to has nothing to accept, so its accept thread
/// must not wake to find that out. It used to poll its listener every 500 ms
/// only to notice a shutdown, which is two wakeups a second for as long as the
/// app was open.
#[test]
fn an_idle_mailbox_never_wakes_its_accept_thread() {
    let dir = profile();
    let owner = primary(dir.path());
    std::thread::sleep(Duration::from_millis(1300));
    let wakes = owner.shared.accept_wakes.load(Ordering::Relaxed);
    assert_eq!(wakes, 0, "the accept thread woke {wakes} times while idle");
}

/// The wait that no longer times out must still end when the owner steps down.
#[test]
fn shutdown_ends_a_blocked_accept_thread_promptly() {
    let dir = profile();
    let owner = primary(dir.path());
    let shared = owner.shared.clone();
    // Let the thread reach its wait.
    std::thread::sleep(Duration::from_millis(100));
    let stepped_down = Instant::now();
    owner.shutdown();
    while !shared.accept_exited.load(Ordering::Acquire) {
        assert!(
            stepped_down.elapsed() < coverage_relaxed(Duration::from_secs(5)),
            "the accept thread did not end after shutdown"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        stepped_down.elapsed() < coverage_relaxed(Duration::from_millis(100)),
        "the accept thread took {:?} to notice shutdown",
        stepped_down.elapsed()
    );
}

/// Answering a launch used to start a thread for it. The owner is asked about
/// as often as a hook fires, so those starts and exits were a steady drip; the
/// handlers are a pool that keeps its threads.
#[test]
fn successive_launches_are_answered_by_the_same_handler_threads() {
    let dir = profile();
    let owner = primary(dir.path());
    let received = recorder(&owner);
    for _ in 0..6 {
        secondary(dir.path())
            .forward(&Request::Reveal)
            .expect("forward");
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)),
            Ok(Request::Reveal)
        );
        // The handler gives its slot back just after it has answered.
        std::thread::sleep(Duration::from_millis(30));
    }
    let started = owner.shared.handler_threads.load(Ordering::Relaxed);
    assert!(
        started <= 2,
        "six launches started {started} handler threads"
    );
}

// ---------------------------------------------------------------------------
// Hook verdicts over the mailbox
// ---------------------------------------------------------------------------

fn gate_request(id: &str) -> Request {
    Request::GateCommand {
        id: id.into(),
        cwd: "/".into(),
        command: "ls".into(),
    }
}

#[test]
fn a_gate_request_is_bounded_before_anything_acts_on_it() {
    assert_eq!(gate_request("g-1").validate(), Ok(()));
    let refused = [
        Request::GateCommand {
            id: String::new(),
            cwd: "/".into(),
            command: "ls".into(),
        },
        Request::GateCommand {
            id: "g\n1".into(),
            cwd: "/".into(),
            command: "ls".into(),
        },
        Request::GateCommand {
            id: "g".into(),
            cwd: "relative".into(),
            command: "ls".into(),
        },
        Request::GateCommand {
            id: "g".into(),
            cwd: "/a\0b".into(),
            command: "ls".into(),
        },
        Request::GateCommand {
            id: "g".into(),
            cwd: "/".into(),
            command: String::new(),
        },
        Request::GateCommand {
            id: "g".into(),
            cwd: "/".into(),
            command: "x".repeat(MAX_GATE_COMMAND_BYTES + 1),
        },
    ];
    for request in refused {
        assert!(request.validate().is_err(), "{request:?}");
    }
}

#[test]
fn a_gate_request_is_refused_until_a_gate_is_attached() {
    let dir = profile();
    let owner = primary(dir.path());
    let socket = Paths::in_dir(&dir.path().canonicalize().unwrap()).socket;
    let secondary = Secondary::connect(&socket).ok().expect("reach the owner");
    let refused = secondary.gate(&gate_request("g-1"), Duration::from_secs(5));
    assert!(refused.is_err(), "answered with no gate attached");
    drop(owner);
}

#[test]
fn a_gate_answer_carries_the_sinks_verdict_and_the_requests_id() {
    let dir = profile();
    let owner = primary(dir.path());
    owner.attach_gate(Arc::new(|cwd, command| {
        Ok(serde_json::json!({"cwd": cwd, "command": command}))
    }));
    let socket = Paths::in_dir(&dir.path().canonicalize().unwrap()).socket;
    let reply = Secondary::connect(&socket)
        .ok()
        .expect("reach the owner")
        .gate(&gate_request("g-7"), Duration::from_secs(5))
        .expect("answered");
    assert_eq!(reply, serde_json::json!({"cwd": "/", "command": "ls"}));

    // A sink that errors is a refusal, never a reply.
    owner.attach_gate(Arc::new(|_, _| Err("no verdict".into())));
    let refused = Secondary::connect(&socket)
        .ok()
        .expect("reach the owner")
        .gate(&gate_request("g-8"), Duration::from_secs(5));
    assert_eq!(refused, Err("no verdict".to_string()));
}

#[test]
fn a_burst_of_hooks_is_bounded_and_never_starves_a_relaunch() {
    let dir = profile();
    let owner = primary(dir.path());
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let entered = Arc::new(AtomicUsize::new(0));
    {
        let release = release.clone();
        let entered = entered.clone();
        owner.attach_gate(Arc::new(move |_, _| {
            entered.fetch_add(1, Ordering::AcqRel);
            let (open, wake) = &*release;
            let mut open = open.lock().unwrap();
            while !*open {
                open = wake.wait(open).unwrap();
            }
            Ok(serde_json::json!({"ok": true}))
        }));
    }
    let socket = Paths::in_dir(&dir.path().canonicalize().unwrap()).socket;
    let held: Vec<_> = (0..MAX_GATE_HANDLERS)
        .map(|n| {
            let socket = socket.clone();
            std::thread::spawn(move || {
                Secondary::connect(&socket)
                    .ok()
                    .expect("reach the owner")
                    .gate(&gate_request(&format!("g-{n}")), Duration::from_secs(30))
            })
        })
        .collect();
    let started = Instant::now();
    while entered.load(Ordering::Acquire) < MAX_GATE_HANDLERS {
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(10)),
            "gates never filled"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    // One past the cap is told it is busy, at once.
    let over = Secondary::connect(&socket)
        .ok()
        .expect("reach the owner")
        .gate(&gate_request("g-over"), Duration::from_secs(5));
    assert_eq!(over, Err("the gate is busy".to_string()));
    // And a relaunch is still taken while every gate slot is held.
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    owner.attach(move |request| sink.lock().unwrap().push(request));
    Secondary::connect(&socket)
        .ok()
        .expect("reach the owner")
        .forward(&Request::Reveal)
        .expect("a relaunch was starved by hook traffic");

    {
        let (open, wake) = &*release;
        *open.lock().unwrap() = true;
        wake.notify_all();
    }
    for worker in held {
        assert!(worker.join().unwrap().is_ok());
    }
}
