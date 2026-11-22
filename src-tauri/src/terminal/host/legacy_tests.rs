//! The legacy client against hosts that misbehave in every way a real one can,
//! and against a real protocol-4 host. `PROTOCOL` is 4 today, so "the current
//! protocol" is a parameter here: a socket named `ptyd-v4.sock` is the previous
//! host of an app that pretends to speak 5.

use super::*;
use crate::terminal::host::compat_v4::{
    read_frame, write_frame, HostFrame, Listing, TranscriptRead, TranscriptTail,
};
use serde_json::{json, Value};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// The protocol the app under test pretends to speak.
const PRETEND: u32 = 5;
/// A deadline short enough for a test to wait out and long enough not to flake.
const SHORT: Duration = Duration::from_millis(400);

fn short_tempdir() -> tempfile::TempDir {
    // A socket path has a short limit: the system temp directory, not a long
    // scratch one.
    tempfile::Builder::new()
        .prefix("lg")
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap()
}

/// What a fake host does with one request: the parsed header and the stream.
/// `false` hangs up.
type Handler = Arc<dyn Fn(&mut UnixStream, &Value) -> bool + Send + Sync>;

struct Fake {
    dir: tempfile::TempDir,
    accepts: Arc<AtomicUsize>,
    /// Every request header seen, in order.
    seen: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
}

impl Fake {
    fn on(protocol: u32, handler: Handler) -> Self {
        let dir = short_tempdir();
        let listener =
            UnixListener::bind(dir.path().join(super::super::socket_name(protocol))).unwrap();
        listener.set_nonblocking(true).unwrap();
        let fake = Self {
            dir,
            accepts: Arc::default(),
            seen: Arc::default(),
            stop: Arc::default(),
        };
        let (accepts, seen, stop) = (fake.accepts.clone(), fake.seen.clone(), fake.stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                accepts.fetch_add(1, Ordering::AcqRel);
                let (handler, seen, stop) = (handler.clone(), seen.clone(), stop.clone());
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Acquire) {
                        let Ok(Some((header, _))) = read_frame(&mut stream) else {
                            return;
                        };
                        let header: Value = serde_json::from_slice(&header).unwrap();
                        seen.lock().unwrap().push(header.clone());
                        if !handler(&mut stream, &header) {
                            return;
                        }
                    }
                });
            }
        });
        fake
    }

    fn v4(handler: Handler) -> Self {
        Self::on(4, handler)
    }

    fn dir(&self) -> &Path {
        self.dir.path()
    }

    fn ops(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|h| h["op"].as_str().unwrap_or("?").to_owned())
            .collect()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn reply(stream: &mut UnixStream, header: &Value, ok: Value) -> bool {
    let frame = HostFrame::Reply {
        id: header["id"].as_u64().unwrap(),
        ok: Some(ok),
        error: None,
    };
    write_frame(stream, &frame, b"").is_ok()
}

fn hello_ok() -> Value {
    json!({"protocol": 4, "build": "9.9.9", "pid": 4242, "sessions": 2})
}

fn listing(id: &str) -> Value {
    serde_json::to_value(Listing {
        id: id.into(),
        shell: "claude".into(),
        cwd: "/work".into(),
        repo: "/work".into(),
        launcher: Some("claude".into()),
        run_id: None,
        detached: true,
        ending: false,
        parent_session_id: None,
        title: Some("fix".into()),
        quiet_ms: Some(12),
    })
    .unwrap()
}

/// A host that is well behaved: hello, a two-session list, kills and
/// transcripts answered.
fn well_behaved() -> Handler {
    Arc::new(|stream, header| match header["op"].as_str().unwrap() {
        "hello" => reply(stream, header, hello_ok()),
        "list" => reply(
            stream,
            header,
            json!([listing("term-1-a"), listing("term-1-b")]),
        ),
        "kill" => reply(stream, header, Value::Null),
        "transcript" => reply(
            stream,
            header,
            serde_json::to_value(TranscriptRead::Found(TranscriptTail {
                data_b64: "aGk=".into(),
                bytes: 2,
                retained: 2,
                total_bytes: 2,
                truncated: false,
                rotated: false,
                clipped: false,
                gap_markers: 0,
                unrecorded_bytes: 0,
                redacted: false,
                live: true,
                settled: true,
                recording_error: None,
                run_id: None,
            }))
            .unwrap(),
        ),
        _ => false,
    })
}

/// Answers `hello`, then does `then` to everything else.
fn hello_then(then: impl Fn(&mut UnixStream, &Value) -> bool + Send + Sync + 'static) -> Handler {
    Arc::new(move |stream, header| {
        if header["op"] == "hello" {
            reply(stream, header, hello_ok())
        } else {
            then(stream, header)
        }
    })
}

#[test]
fn a_host_is_listed_killed_and_read_through_the_frozen_dialect() {
    let fake = Fake::v4(well_behaved());
    let mut host = LegacyHost::connect(fake.dir(), 4).unwrap();
    assert_eq!(
        host.info(),
        &LegacyHostInfo {
            protocol: 4,
            build: "9.9.9".into(),
            pid: 4242,
            sessions: 2
        }
    );
    let sessions = host.list().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        sessions[0],
        LegacySession {
            id: "term-1-a".into(),
            program: "claude".into(),
            cwd: "/work".into(),
            repo: "/work".into(),
            launcher: Some("claude".into()),
            run_id: None,
            title: Some("fix".into()),
            parent_session_id: None,
            detached: true,
            ending: false,
            quiet_ms: Some(12),
        }
    );
    host.kill("term-1-a").unwrap();
    let crate::terminal::TranscriptRead::Found(tail) =
        host.transcript_tail("term-1-a", 4096, true).unwrap()
    else {
        panic!("not found");
    };
    assert_eq!(
        (tail.data_b64.as_str(), tail.bytes, tail.live),
        ("aGk=", 2, true)
    );
    assert_eq!(fake.ops(), ["hello", "list", "kill", "transcript"]);
    let seen = fake.seen.lock().unwrap();
    assert_eq!(
        seen[0],
        json!({"id": 1, "op": "hello", "protocol": 4, "build": crate::updates::CURRENT_VERSION})
    );
    assert_eq!(
        seen[2],
        json!({"id": 3, "op": "kill", "session": "term-1-a"})
    );
    assert_eq!(
        seen[3],
        json!({"id": 4, "op": "transcript", "session": "term-1-a", "max_bytes": 4096, "redact": true})
    );
}

#[test]
fn a_transcript_read_is_clamped_to_what_a_v4_host_answers() {
    let fake = Fake::v4(well_behaved());
    let mut host = LegacyHost::connect(fake.dir(), 4).unwrap();
    host.transcript_tail("s", usize::MAX, false).unwrap();
    host.transcript_tail("s", 0, false).unwrap();
    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen[1]["max_bytes"], 128 * 1024);
    assert_eq!(seen[2]["max_bytes"], 1);
}

#[test]
fn a_session_id_that_is_not_one_is_refused_before_anything_is_sent() {
    let fake = Fake::v4(well_behaved());
    let mut host = LegacyHost::connect(fake.dir(), 4).unwrap();
    for bad in ["", "a\nb", "a\0b", &"x".repeat(257)] {
        assert!(
            matches!(host.kill(bad), Err(LegacyError::Invalid(_))),
            "{bad:?}"
        );
        assert!(matches!(
            host.transcript_tail(bad, 10, false),
            Err(LegacyError::Invalid(_))
        ));
    }
    assert_eq!(
        fake.ops(),
        ["hello"],
        "nothing past the introduction was sent"
    );
    host.list()
        .expect("a refused argument leaves the handle usable");
}

#[test]
fn a_socket_with_no_host_behind_it_is_unreachable_not_empty() {
    let dir = short_tempdir();
    let path = dir.path().join(super::super::socket_name(4));
    drop(UnixListener::bind(&path).unwrap());
    assert!(path.exists(), "the dead host left its socket file");
    assert!(matches!(
        LegacyHost::connect(dir.path(), 4),
        Err(LegacyError::Unreachable(_))
    ));
    let scan = discover(dir.path(), PRETEND);
    assert_eq!(scan.hosts.len(), 1);
    assert!(matches!(
        scan.hosts[0].status,
        LegacyStatus::Unreachable { .. }
    ));
    // And no socket at all is the same answer.
    let empty = short_tempdir();
    assert!(matches!(
        LegacyHost::connect(empty.path(), 4),
        Err(LegacyError::Unreachable(_))
    ));
    assert!(discover(empty.path(), PRETEND).hosts.is_empty());
    assert!(discover(&empty.path().join("absent"), PRETEND)
        .hosts
        .is_empty());
}

#[test]
fn a_protocol_without_a_dialect_is_reported_and_never_connected_to() {
    let fake = Fake::on(3, well_behaved());
    assert!(matches!(
        LegacyHost::connect(fake.dir(), 3),
        Err(LegacyError::NoCompat(3))
    ));
    let scan = discover(fake.dir(), PRETEND);
    assert_eq!(scan.hosts.len(), 1);
    assert_eq!(scan.hosts[0].protocol, 3);
    assert_eq!(scan.hosts[0].status, LegacyStatus::NoCompat);
    assert_eq!(fake.accepts.load(Ordering::Acquire), 0);
    assert!(!has_compat(PRETEND) && !has_compat(3) && has_compat(4));
}

#[test]
fn the_current_protocols_own_socket_is_not_a_legacy_host() {
    let fake = Fake::v4(well_behaved());
    assert!(discover(fake.dir(), 4).hosts.is_empty());
    assert_eq!(fake.accepts.load(Ordering::Acquire), 0);
    assert_eq!(discover(fake.dir(), PRETEND).hosts.len(), 1);
}

#[test]
fn discovery_lists_every_other_host_and_says_when_it_stopped_looking() {
    let fake = Fake::v4(well_behaved());
    // A socket for protocol 2 beside it, and files that are not sockets.
    let _two = UnixListener::bind(fake.dir().join(super::super::socket_name(2))).unwrap();
    std::fs::write(fake.dir().join("ptyd-v4.lock"), b"").unwrap();
    std::fs::write(fake.dir().join("ptyd.log"), b"").unwrap();
    let scan = discover(fake.dir(), PRETEND);
    assert!(!scan.truncated);
    let seen: Vec<(u32, bool)> = scan
        .hosts
        .iter()
        .map(|h| (h.protocol, matches!(h.status, LegacyStatus::Listed { .. })))
        .collect();
    assert_eq!(seen, [(2, false), (4, true)]);

    let crowded = short_tempdir();
    for protocol in 100..100 + MAX_SOCKETS as u32 + 3 {
        // Plain files: `discover` reads names, and a name is all a stale
        // socket leaves. Each is unreachable, none is a host.
        std::fs::write(
            crowded.path().join(super::super::socket_name(protocol)),
            b"",
        )
        .unwrap();
    }
    let scan = discover(crowded.path(), PRETEND);
    assert!(scan.truncated);
    assert_eq!(scan.hosts.len(), MAX_SOCKETS);
}

#[test]
fn a_host_that_answers_garbage_is_a_protocol_error() {
    for garbage in [
        b"GARBAGE-NOT-A-FRAME".to_vec(),
        // A header length far past the bound.
        u32::MAX.to_be_bytes().to_vec(),
        // A frame whose header is JSON that is not a frame.
        {
            let mut wire = Vec::new();
            write_frame(&mut wire, &json!({"unexpected": true}), b"").unwrap();
            wire
        },
        // Valid length and bytes, but not JSON.
        {
            let mut wire = 4u32.to_be_bytes().to_vec();
            wire.extend_from_slice(b"nope");
            wire.extend_from_slice(&0u32.to_be_bytes());
            wire
        },
    ] {
        let fake = Fake::v4(Arc::new(move |stream, _| {
            use std::io::Write;
            let _ = stream.write_all(&garbage);
            true
        }));
        let error = LegacyHost::connect_with(fake.dir(), 4, SHORT)
            .err()
            .unwrap();
        assert!(matches!(error, LegacyError::Protocol(_)), "{error:?}");
        assert!(matches!(
            inspect_with(fake.dir(), 4, SHORT),
            LegacyStatus::Failed { .. }
        ));
    }
}

#[test]
fn a_host_that_never_answers_is_waited_for_only_as_long_as_the_deadline() {
    let fake = Fake::v4(Arc::new(|_, _| true));
    let started = Instant::now();
    let error = LegacyHost::connect_with(fake.dir(), 4, SHORT)
        .err()
        .unwrap();
    assert!(matches!(error, LegacyError::TimedOut(_)), "{error:?}");
    assert!(started.elapsed() < SHORT * 5, "{:?}", started.elapsed());

    let fake = Fake::v4(hello_then(|_, _| true));
    let mut host = LegacyHost::connect_with(fake.dir(), 4, SHORT).unwrap();
    let started = Instant::now();
    assert!(matches!(host.list(), Err(LegacyError::TimedOut(_))));
    assert!(started.elapsed() < SHORT * 5);
    assert!(
        matches!(host.list(), Err(LegacyError::Broken)),
        "a late answer must not be taken for the next request's"
    );
}

#[test]
fn a_host_that_answers_a_byte_at_a_time_cannot_stretch_the_deadline() {
    let fake = Fake::v4(Arc::new(|stream, header| {
        use std::io::Write;
        let mut wire = Vec::new();
        write_frame(
            &mut wire,
            &HostFrame::Reply {
                id: header["id"].as_u64().unwrap(),
                ok: Some(hello_ok()),
                error: None,
            },
            b"",
        )
        .unwrap();
        for byte in wire {
            if stream.write_all(&[byte]).is_err() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(60));
        }
        true
    }));
    let started = Instant::now();
    let error = LegacyHost::connect_with(fake.dir(), 4, SHORT)
        .err()
        .unwrap();
    assert!(matches!(error, LegacyError::TimedOut(_)), "{error:?}");
    assert!(started.elapsed() < SHORT * 4, "{:?}", started.elapsed());
}

#[test]
fn a_host_that_goes_down_mid_call_is_an_error_and_the_handle_is_spent() {
    let fake = Fake::v4(hello_then(|_, _| false));
    let mut host = LegacyHost::connect(fake.dir(), 4).unwrap();
    assert!(matches!(host.list(), Err(LegacyError::Protocol(_))));
    assert!(matches!(host.kill("term-1-a"), Err(LegacyError::Broken)));
    assert!(matches!(
        host.transcript_tail("term-1-a", 1, false),
        Err(LegacyError::Broken)
    ));
}

#[test]
fn a_host_that_hangs_up_before_it_introduces_itself_is_a_protocol_error() {
    let fake = Fake::v4(Arc::new(|_, _| false));
    assert!(matches!(
        LegacyHost::connect(fake.dir(), 4),
        Err(LegacyError::Protocol(_))
    ));
}

#[test]
fn another_app_holding_the_host_is_refused_not_reported_empty() {
    let fake = Fake::v4(Arc::new(|stream, header| {
        let frame = HostFrame::Reply {
            id: header["id"].as_u64().unwrap(),
            ok: None,
            error: Some("Another GitPulse window is connected to this terminal host.".into()),
        };
        let _ = write_frame(stream, &frame, b"");
        false
    }));
    let error = LegacyHost::connect(fake.dir(), 4).err().unwrap();
    assert!(
        matches!(&error, LegacyError::Refused(why) if why.contains("Another GitPulse window")),
        "{error:?}"
    );
    assert!(matches!(
        discover(fake.dir(), PRETEND).hosts[0].status,
        LegacyStatus::Refused { .. }
    ));
}

#[test]
fn a_host_that_speaks_another_protocol_than_its_socket_says_is_not_used() {
    let fake = Fake::v4(Arc::new(|stream, header| {
        reply(
            stream,
            header,
            json!({"protocol": 6, "build": "b", "pid": 1, "sessions": 0}),
        )
    }));
    let error = LegacyHost::connect(fake.dir(), 4).err().unwrap();
    assert!(
        matches!(&error, LegacyError::Protocol(why) if why.contains("speaks 6")),
        "{error:?}"
    );
}

#[test]
fn an_answer_to_another_request_is_never_taken_for_this_ones() {
    let fake = Fake::v4(hello_then(|stream, header| {
        let frame = HostFrame::Reply {
            id: header["id"].as_u64().unwrap() + 40,
            ok: Some(json!([])),
            error: None,
        };
        write_frame(stream, &frame, b"").is_ok()
    }));
    let mut host = LegacyHost::connect(fake.dir(), 4).unwrap();
    assert!(matches!(host.list(), Err(LegacyError::Protocol(_))));
    assert!(matches!(host.list(), Err(LegacyError::Broken)));
}

#[test]
fn frames_that_are_not_the_answer_are_skipped_up_to_a_bound() {
    let exit = json!({"kind": "exit", "ended": {"payload": {"id": "term-9-z", "exit_code": 0,
        "signal": "", "error": null, "reaped": true, "ended_by_gitpulse": false},
        "ended_by_gitpulse": false, "program": "sh", "cwd": "/", "run_id": null}});
    let noisy = {
        let exit = exit.clone();
        Fake::v4(hello_then(move |stream, header| {
            write_frame(stream, &exit, b"").unwrap();
            write_frame(
                stream,
                &json!({"kind": "output", "session": "term-1-a"}),
                b"out",
            )
            .unwrap();
            reply(stream, header, json!([listing("term-1-a")]))
        }))
    };
    let mut host = LegacyHost::connect(noisy.dir(), 4).unwrap();
    assert_eq!(
        host.list().unwrap().len(),
        1,
        "an exit and output before the answer are skipped"
    );

    let flood = Fake::v4(hello_then(|stream, _| {
        for _ in 0..MAX_SKIPPED_FRAMES + 10 {
            if write_frame(stream, &json!({"kind": "end", "session": "s"}), b"").is_err() {
                break;
            }
        }
        true
    }));
    let mut host = LegacyHost::connect(flood.dir(), 4).unwrap();
    assert!(matches!(host.list(), Err(LegacyError::Protocol(_))));
    assert!(matches!(host.list(), Err(LegacyError::Broken)));
}

#[test]
fn a_host_that_says_no_is_an_error_and_the_handle_stays_usable() {
    let fake = Fake::v4(hello_then(|stream, header| {
        if header["op"] == "kill" {
            let frame = HostFrame::Reply {
                id: header["id"].as_u64().unwrap(),
                ok: None,
                error: Some("no such session".into()),
            };
            write_frame(stream, &frame, b"").is_ok()
        } else {
            reply(stream, header, json!([]))
        }
    }));
    let mut host = LegacyHost::connect(fake.dir(), 4).unwrap();
    assert_eq!(
        host.kill("term-9-z"),
        Err(LegacyError::Host("no such session".into()))
    );
    assert_eq!(host.list().unwrap(), []);
}

#[test]
fn an_unreadable_listing_is_a_protocol_error_and_spends_the_handle() {
    let fake = Fake::v4(hello_then(|stream, header| {
        reply(stream, header, json!("not a list of sessions"))
    }));
    let mut host = LegacyHost::connect(fake.dir(), 4).unwrap();
    assert!(matches!(host.list(), Err(LegacyError::Protocol(_))));
    assert!(matches!(host.list(), Err(LegacyError::Broken)));
}

// ---- a real protocol-4 host ------------------------------------------------

use crate::terminal::host::{proto, server};
use std::os::unix::net::UnixStream as Stream;

/// A real `server::serve` of this build (protocol 4) in a temp directory, with
/// the sessions a test started on it, stopped when it drops.
struct RealHost {
    dir: tempfile::TempDir,
    sessions: Vec<String>,
    thread: Option<std::thread::JoinHandle<i32>>,
}

impl RealHost {
    fn start() -> Self {
        let dir = short_tempdir();
        let config = server::ServeConfig {
            dir: dir.path().to_path_buf(),
            idle_exit: Duration::from_millis(300),
        };
        let thread = std::thread::spawn(move || {
            server::serve_for_unit_test(config, crate::terminal::Engine::default())
        });
        let socket = dir.path().join(super::super::socket_name(proto::PROTOCOL));
        let deadline = Instant::now() + Duration::from_secs(10);
        while Stream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "the test host never listened");
            std::thread::sleep(Duration::from_millis(10));
        }
        Self {
            dir,
            sessions: Vec::new(),
            thread: Some(thread),
        }
    }

    /// An app of this build's own protocol, connected to the host.
    fn app(&self) -> Stream {
        let stream = Stream::connect(
            self.dir
                .path()
                .join(super::super::socket_name(proto::PROTOCOL)),
        )
        .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
    }

    fn app_call(stream: &mut Stream, id: u64, request: proto::Request) -> Result<Value, String> {
        proto::write_frame(stream, &proto::Envelope { id, request }, b"").unwrap();
        loop {
            let (header, _) = proto::read_frame(stream).unwrap().expect("the host closed");
            if let proto::HostFrame::Reply { id: got, ok, error } = proto::decode(&header).unwrap()
            {
                assert_eq!(got, id);
                return match error {
                    Some(error) => Err(error),
                    None => Ok(ok.unwrap_or(Value::Null)),
                };
            }
        }
    }

    /// Starts `sh -c <script>` in `cwd` and leaves it running with no app
    /// connected: the situation a protocol bump creates.
    fn spawn(&mut self, id: &str, script: &str, cwd: &Path) {
        let mut app = self.app();
        Self::app_call(
            &mut app,
            1,
            proto::Request::Hello {
                protocol: proto::PROTOCOL,
                build: crate::updates::CURRENT_VERSION.to_owned(),
            },
        )
        .unwrap();
        let request = crate::terminal::SpawnRequest {
            session_id: id.to_owned(),
            place: crate::terminal::Place::Task {
                cwd: cwd.to_string_lossy().into_owned(),
            },
            rows: 24,
            cols: 80,
            program: Some("/bin/sh".into()),
            args: Some(vec!["-c".into(), script.to_owned()]),
            env: None,
            run_id: None,
            parent_session_id: None,
            child_launcher: None,
            child_title: None,
            token_hash_hex: None,
        };
        Self::app_call(&mut app, 2, proto::Request::Spawn { request })
            .expect("the test session started");
        self.sessions.push(id.to_owned());
        // Dropping `app` is the app going away; the session stays.
    }
}

impl Drop for RealHost {
    fn drop(&mut self) {
        // Stop what is left, then let the host's idle timer end it.
        if !self.sessions.is_empty() {
            if let Ok(mut host) = LegacyHost::connect(self.dir.path(), 4) {
                for session in std::mem::take(&mut self.sessions) {
                    let _ = host.kill(&session);
                }
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
fn a_real_v4_host_is_discovered_read_and_stopped_by_an_app_of_a_later_protocol() {
    let work = crate::test_support::git_repo();
    // The host refuses a working directory that resolves elsewhere.
    let cwd = work.path().canonicalize().unwrap();
    let mut host = RealHost::start();
    host.spawn(
        "term-77-1",
        "echo legacy-output-marker; exec sleep 300",
        &cwd,
    );

    let scan = discover(host.dir.path(), PRETEND);
    assert_eq!(scan.hosts.len(), 1, "{scan:?}");
    let LegacyStatus::Listed { info, sessions } = &scan.hosts[0].status else {
        panic!("not listed: {:?}", scan.hosts[0]);
    };
    assert_eq!(info.protocol, 4);
    assert_eq!(info.sessions, 1);
    assert_eq!(info.pid, std::process::id(), "the in-process test host");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "term-77-1");
    assert_eq!(sessions[0].program, "/bin/sh");
    assert!(sessions[0].detached, "no app holds it");

    let mut legacy = LegacyHost::connect(host.dir.path(), 4).unwrap();
    // The saved output is read while the session lives, polled for because the
    // host records it asynchronously.
    let deadline = Instant::now() + Duration::from_secs(15);
    let tail = loop {
        match legacy.transcript_tail("term-77-1", 4096, false).unwrap() {
            crate::terminal::TranscriptRead::Found(tail) if tail.bytes > 0 => break tail,
            other => assert!(
                Instant::now() < deadline,
                "no output was recorded: {other:?}"
            ),
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(tail.live);
    use base64::Engine as _;
    let text = base64::engine::general_purpose::STANDARD
        .decode(&tail.data_b64)
        .unwrap();
    assert!(String::from_utf8_lossy(&text).contains("legacy-output-marker"));
    let stranger = legacy.transcript_tail("term-1-1", 64, false).unwrap();
    assert!(
        matches!(stranger, crate::terminal::TranscriptRead::Missing),
        "{stranger:?}"
    );

    legacy.kill("term-77-1").unwrap();
    assert!(legacy.list().unwrap().is_empty(), "the session is gone");
    // A v4 host's kill of a session that is already gone is not an error.
    legacy.kill("term-77-1").unwrap();
    host.sessions.clear();
}

#[test]
fn a_real_v4_host_whose_own_app_is_still_connected_refuses_the_legacy_client() {
    let host = RealHost::start();
    let mut app = host.app();
    RealHost::app_call(
        &mut app,
        1,
        proto::Request::Hello {
            protocol: proto::PROTOCOL,
            build: crate::updates::CURRENT_VERSION.to_owned(),
        },
    )
    .unwrap();
    let status = inspect_with(host.dir.path(), 4, Duration::from_secs(5));
    assert!(matches!(status, LegacyStatus::Refused { .. }), "{status:?}");
    drop(app);
}
