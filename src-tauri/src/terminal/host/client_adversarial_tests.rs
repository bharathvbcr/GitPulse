//! Adversarial cases for ft-67f95e2d item 8: `Host::knows` answered from the
//! client's snapshot (`Inner.proxies`, `Inner.adopted`).
//!
//! The snapshot is only as good as the events that keep it current, so these
//! attack the ways it goes stale in the direction that admits a report: a host
//! that restarted, a reconciliation that fails half way, a listing older than
//! an exit, a client that gave up reconnecting or let the host go, a spawn that
//! never answers. They also attack the one property the change exists for,
//! that `knows` never waits, by holding the `proxies` lock the way `adopt` does.
//!
//! The host is a scripted fake on a real socket (the implementer's `FakeHost`
//! in `client_tests.rs` is private to that module, so this file has its own,
//! with the extra switches these cases need).
//!
//! Written without being run. Each case says what it expects of the code as
//! committed (`3a1c9f73`) and why; a case marked BREAK is predicted to fail.

use super::*;
use crate::terminal::host::proto::{decode, read_frame, write_frame};
use crate::terminal::{Place, TerminalExitPayload, TerminalSessions};
use std::os::unix::net::UnixListener;
use std::sync::atomic::AtomicBool;
use tauri::Manager as _;

/// What a caller on a bridge worker can afford.
const PROMPT: Duration = Duration::from_secs(2);

type Writer = Arc<Mutex<UnixStream>>;
type Held = Vec<(String, Option<String>)>;

/// Everything the fake host's connections share.
#[derive(Default)]
struct Shared {
    /// The sessions the host lists: id and run.
    sessions: Mutex<Held>,
    writers: Mutex<Vec<Writer>>,
    /// Connections accepted.
    accepts: AtomicUsize,
    /// Lists and receipts answered (or begun), across connections.
    lists: AtomicUsize,
    receipts: AtomicUsize,
    /// Only a spawn is left unanswered.
    hang_spawn: AtomicBool,
    /// The next receipts request is answered with an error.
    receipts_fail: AtomicBool,
    /// How many of the next lists are answered with something that is not a list.
    garbage_lists: AtomicUsize,
    /// How long a list waits before it is answered.
    list_delay_ms: AtomicU64,
    /// A list is answered from the sessions as they are when it is asked,
    /// this long afterwards, on its own thread: other requests are answered
    /// in the meantime, and the answer is older than they are.
    list_late_ms: AtomicU64,
    /// How long the hello waits before it is answered.
    hello_delay_ms: AtomicU64,
    /// A spawn is answered as a host would: the session is listed from then on.
    spawn_ok: AtomicBool,
    /// A spawn is refused, this long after it is asked.
    spawn_reject_ms: AtomicU64,
    /// Accepting; false drops the listener, so a connect is refused.
    listening: AtomicBool,
    stop: AtomicBool,
}

struct FakeHost {
    dir: tempfile::TempDir,
    shared: Arc<Shared>,
}

fn held(sessions: &[(&str, Option<&str>)]) -> Held {
    sessions
        .iter()
        .map(|(id, run)| ((*id).to_owned(), run.map(str::to_owned)))
        .collect()
}

impl FakeHost {
    fn start(sessions: &[(&str, Option<&str>)]) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("pa")
            .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
        let listener = UnixListener::bind(
            dir.path()
                .join(crate::terminal::host::socket_name(PROTOCOL)),
        )
        .unwrap();
        listener.set_nonblocking(true).unwrap();
        let shared = Arc::new(Shared::default());
        *shared.sessions.lock().unwrap() = held(sessions);
        shared.listening.store(true, Ordering::Release);
        let accepting = shared.clone();
        std::thread::spawn(move || {
            let mut listener = Some(listener);
            while !accepting.stop.load(Ordering::Acquire) {
                if !accepting.listening.load(Ordering::Acquire) {
                    // The socket file stays; nothing answers it.
                    listener = None;
                }
                let Some(open) = &listener else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                let Ok((stream, _)) = open.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                accepting.accepts.fetch_add(1, Ordering::AcqRel);
                let writer: Writer = Arc::new(Mutex::new(stream.try_clone().unwrap()));
                accepting.writers.lock().unwrap().push(writer.clone());
                let shared = accepting.clone();
                std::thread::spawn(move || serve(stream, writer, &shared));
            }
        });
        Self { dir, shared }
    }

    fn config(&self) -> HostConfig {
        HostConfig {
            dir: self.dir.path().to_path_buf(),
            binary: Some("/nonexistent/gitpulse-ptyd".into()),
            idle_exit: None,
            log_dir: None,
        }
    }

    fn accepts(&self) -> usize {
        self.shared.accepts.load(Ordering::Acquire)
    }

    fn receipts(&self) -> usize {
        self.shared.receipts.load(Ordering::Acquire)
    }

    fn set_sessions(&self, sessions: &[(&str, Option<&str>)]) {
        *self.shared.sessions.lock().unwrap() = held(sessions);
    }

    /// The host stops listing `session`, as it does once its exit is recorded.
    fn forget(&self, session: &str) {
        self.shared
            .sessions
            .lock()
            .unwrap()
            .retain(|(id, _)| id != session);
    }

    /// The host drops every connection; the app's reader sees the end.
    fn drop_connections(&self) {
        let writers = std::mem::take(&mut *self.shared.writers.lock().unwrap());
        for writer in writers {
            let _ = writer.lock().unwrap().shutdown(std::net::Shutdown::Both);
        }
    }

    /// The host is gone for good: nothing answers a new connection.
    fn go_away(&self) {
        self.shared.listening.store(false, Ordering::Release);
        // Past the accept loop's next look, so a reconnect cannot slip in.
        std::thread::sleep(Duration::from_millis(100));
        self.drop_connections();
    }

    /// The host reports that `session` ended. It keeps listing it unless
    /// [`Self::forget`] is also called, which is what a listing taken before
    /// the exit looks like.
    fn end(&self, session: &str) {
        let frame = HostFrame::Exit {
            ended: WireEnded {
                payload: TerminalExitPayload {
                    id: session.to_owned(),
                    exit_code: Some(0),
                    signal: String::new(),
                    error: None,
                    reaped: true,
                    ended_by_gitpulse: false,
                },
                ended_by_gitpulse: false,
                program: "sh".into(),
                cwd: "/".into(),
                run_id: None,
            },
        };
        for writer in self.shared.writers.lock().unwrap().iter() {
            let _ = write_frame(&mut *writer.lock().unwrap(), &frame, &[]);
        }
    }
}

impl Drop for FakeHost {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.drop_connections();
    }
}

/// `frames` one-byte output frames for `session`, to every connection.
fn flood(shared: &Shared, session: &str, frames: usize) {
    let frame = HostFrame::Output {
        session: session.to_owned(),
    };
    for _ in 0..frames {
        for writer in shared.writers.lock().unwrap().iter() {
            let _ = write_frame(&mut *writer.lock().unwrap(), &frame, b"x");
        }
    }
}

fn serve(stream: UnixStream, writer: Writer, shared: &Shared) {
    let mut input = BufReader::new(stream);
    while let Ok(Some((header, _))) = read_frame(&mut input) {
        let Ok(Envelope { id, request }) = decode::<Envelope>(&header) else {
            return;
        };
        let answer: Result<serde_json::Value, String> = match request {
            Request::Limit { .. } | Request::Stall { .. } | Request::Forget { .. } => continue,
            Request::Hello { .. } => {
                let delay = shared.hello_delay_ms.load(Ordering::Acquire);
                if delay > 0 {
                    std::thread::sleep(Duration::from_millis(delay));
                }
                Ok(serde_json::to_value(Hello {
                    protocol: PROTOCOL,
                    build: crate::updates::CURRENT_VERSION.to_owned(),
                    pid: std::process::id(),
                    sessions: shared.sessions.lock().unwrap().len(),
                })
                .unwrap())
            }
            Request::List => {
                shared.lists.fetch_add(1, Ordering::AcqRel);
                let delay = shared.list_delay_ms.load(Ordering::Acquire);
                if delay > 0 {
                    std::thread::sleep(Duration::from_millis(delay));
                }
                let garbage = shared
                    .garbage_lists
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                    .is_ok();
                let answer = if garbage {
                    Ok(serde_json::json!("this is not a list of sessions"))
                } else {
                    let listed: Vec<TerminalListing> = shared
                        .sessions
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|(id, run)| TerminalListing {
                            id: id.clone(),
                            shell: "sh".into(),
                            cwd: "/".into(),
                            repo: "/".into(),
                            launcher: None,
                            run_id: run.clone(),
                            detached: false,
                            ending: false,
                        })
                        .collect();
                    Ok(serde_json::to_value(listed).unwrap())
                };
                let late = shared.list_late_ms.load(Ordering::Acquire);
                if let (true, Ok(value)) = (late > 0, &answer) {
                    let (writer, value) = (writer.clone(), value.clone());
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(late));
                        let reply = HostFrame::Reply {
                            id,
                            ok: Some(value),
                            error: None,
                        };
                        let _ = write_frame(&mut *writer.lock().unwrap(), &reply, &[]);
                    });
                    continue;
                }
                answer
            }
            Request::Receipts => {
                shared.receipts.fetch_add(1, Ordering::AcqRel);
                if shared.receipts_fail.load(Ordering::Acquire) {
                    Err("the host could not read its receipts".to_owned())
                } else {
                    Ok(serde_json::to_value(Vec::<WireEnded>::new()).unwrap())
                }
            }
            Request::Spawn { request } => {
                if shared.hang_spawn.load(Ordering::Acquire) {
                    continue;
                }
                let reject = shared.spawn_reject_ms.load(Ordering::Acquire);
                if reject > 0 {
                    std::thread::sleep(Duration::from_millis(reject));
                    Err("the host refused to start the session".to_owned())
                } else if shared.spawn_ok.load(Ordering::Acquire) {
                    shared
                        .sessions
                        .lock()
                        .unwrap()
                        .push((request.session_id.clone(), None));
                    Ok(serde_json::json!({
                        "spawned": {"id": request.session_id, "shell": "sh", "cwd": "/"},
                        "process": [4242, "born"],
                        "process_error": null,
                    }))
                } else {
                    Ok(serde_json::Value::Null)
                }
            }
            _ => Ok(serde_json::Value::Null),
        };
        let (ok, error) = match answer {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(error)),
        };
        let reply = HostFrame::Reply { id, ok, error };
        if write_frame(&mut *writer.lock().unwrap(), &reply, &[]).is_err() {
            return;
        }
    }
}

/// Session events that go nowhere: only the proxies are under test.
struct NullEvents;

impl SessionEvents for NullEvents {
    fn loss(&self, _: TerminalLossPayload) {}
    fn notice(&self, _: crate::alerts::Notice) {}
    fn scan_drops(&self, _: u64) {}
    fn exit(&self, _: SessionEnded) {}
}

/// The app's registry over `config`, with `knows` asked as the bridge asks it.
struct Fixture {
    sessions: TerminalSessions,
    host: Arc<dyn crate::alerts::Host>,
    _app: tauri::App<tauri::test::MockRuntime>,
}

fn fixture(config: HostConfig) -> Fixture {
    let app = tauri::test::mock_builder().build(crate::context()).unwrap();
    let sessions = TerminalSessions::with_host(config);
    sessions.bind_app(app.handle());
    app.manage(sessions.clone());
    let host = crate::workbench::notifications::session_host(app.handle());
    Fixture {
        sessions,
        host,
        _app: app,
    }
}

fn timed(host: &Arc<dyn crate::alerts::Host>, key: &str) -> (bool, Duration) {
    let started = Instant::now();
    let known = host.knows(key);
    (known, started.elapsed())
}

fn until(what: &str, within: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn spawn_request(session: &str) -> SpawnRequest {
    SpawnRequest {
        session_id: session.to_owned(),
        place: Place::Task { cwd: "/".into() },
        rows: 24,
        cols: 80,
        program: None,
        args: None,
        env: None,
        run_id: None,
    }
}

/// A host that restarted with new session ids, whose receipts then cannot be
/// read. `adopt` sets `adopted` and inserts the new ids (client.rs:985-1010),
/// then returns at the receipts call (client.rs:1012 `?`) before it ends the
/// proxies the host no longer holds (client.rs:1020-1041). `ensure_connected`
/// returns the error but leaves the connection in place, and the reconnect loop
/// asks `ensure_connected` again, which finds that connection and returns
/// (client.rs:842-844), so nothing ever retries the reconciliation.
///
/// Predicted: BREAK. The old id stays known for as long as the connection
/// lives, to a host that has never heard of it. The base asked the host.
#[test]
fn a_restarted_host_with_new_ids_leaves_no_old_id_known_when_receipts_fail() {
    let fake = FakeHost::start(&[("term-old", None)]);
    let app = fixture(fake.config());
    app.sessions.connect_host().unwrap();
    assert!(app.host.knows("term-old"));

    fake.set_sessions(&[("term-new", None)]);
    fake.shared.receipts_fail.store(true, Ordering::Release);
    fake.drop_connections();
    until(
        "the new host's session adopted",
        Duration::from_secs(10),
        || app.host.knows("term-new"),
    );
    std::thread::sleep(Duration::from_millis(300));

    assert!(
        !app.host.knows("term-old"),
        "a session the restarted host does not hold is still vouched for"
    );
}

/// The control for the case above: with the receipts readable the same restart
/// is reconciled, and the old id goes.
///
/// Predicted: pass.
#[test]
fn a_restarted_host_with_new_ids_is_reconciled_when_adoption_completes() {
    let fake = FakeHost::start(&[("term-old", None)]);
    let app = fixture(fake.config());
    app.sessions.connect_host().unwrap();

    fake.set_sessions(&[("term-new", None)]);
    fake.drop_connections();
    until(
        "the new host's session adopted",
        Duration::from_secs(10),
        || app.host.knows("term-new"),
    );
    until("the old id ended", Duration::from_secs(5), || {
        !app.host.knows("term-old")
    });
}

/// The first adoption fails at the list (it timed out, or came back unreadable).
/// `ensure_connected` has already stored the connection (client.rs:874) before
/// `adopt`, so every later call finds a live connection and returns without
/// adopting: `adopted` stays false for the life of that connection. The host
/// holds `term-live` and answers `list`, yet every `knows` is `None`, refused.
///
/// Predicted: BREAK. This is fail-closed, so not a widening, but it is a
/// permanent loss of every non-`hook-` report where the base would have asked
/// the host over the connection that is open and answered true.
#[test]
fn a_failed_first_adoption_is_not_left_refusing_for_the_life_of_the_connection() {
    let fake = FakeHost::start(&[("term-live", None)]);
    fake.shared.garbage_lists.store(1, Ordering::Release);
    let app = fixture(fake.config());
    assert!(
        app.sessions.connect_host().is_err(),
        "an unreadable list must fail the connect"
    );
    // What the app does next, on any call that needs the host.
    let _ = app.sessions.connect_host();
    let listed = crate::terminal::list_sessions(&app.sessions).unwrap();
    assert!(
        listed.iter().any(|s| s.id == "term-live"),
        "the host does hold the session"
    );

    assert!(
        app.host.knows("term-live"),
        "the host holds the session and answers, but the app never adopted it"
    );
}

/// The reconnect loop makes three attempts (client.rs:1274) and then stops;
/// nothing clears the proxies or `adopted`. The brief's window (b) is bounded
/// "by the reconnect loop"; after the loop gives up it is not bounded at all.
///
/// Predicted: BREAK. A host that never comes back is vouched for, for every
/// session it once held, until something else happens to connect.
#[test]
fn a_host_that_never_returns_is_not_vouched_for_after_the_reconnects_give_up() {
    let fake = FakeHost::start(&[("term-live", None)]);
    let app = fixture(fake.config());
    app.sessions.connect_host().unwrap();
    assert!(app.host.knows("term-live"));

    fake.go_away();
    // Three attempts, one second apart, each refused at once.
    std::thread::sleep(Duration::from_secs(5));

    assert!(
        !app.host.knows("term-live"),
        "a host that has been gone for 5 s and was given up on still has its sessions vouched for"
    );
}

/// `disconnect` (the app is quitting and leaving the sessions) closes the
/// connection and sets `leaving` (client.rs:366-371), which stops every
/// reconnect, and leaves `adopted` and the proxies as they were. From then on
/// nothing can ever update the snapshot. The base answered false here
/// (`ensure_connected` refuses while leaving).
///
/// Predicted: BREAK, low severity: it only matters for reports that arrive
/// during the quit.
#[test]
fn an_app_that_let_the_host_go_does_not_vouch_for_its_sessions() {
    let fake = FakeHost::start(&[("term-live", None)]);
    let app = fixture(fake.config());
    app.sessions.connect_host().unwrap();
    assert!(app.host.knows("term-live"));

    app.sessions.disconnect_host();

    assert!(
        !app.host.knows("term-live"),
        "after disconnect nothing keeps the snapshot current"
    );
}

/// A listing older than an exit this app has recorded. `adopt` inserts a proxy
/// for every listed id that has none (client.rs:996-1005) and never consults
/// `ended`. In production this is the race of a `List` reply taken before a
/// session exits against the exit's own frame, which `record_exit` handles on
/// its own thread and may finish before `adopt` takes the lock. Here it is
/// made deterministic: the host keeps listing a session whose exit it has
/// already reported.
///
/// Predicted: BREAK. The ended session is proxied again and `knows` returns
/// true for it until the next reconnect.
#[test]
fn a_listing_older_than_a_recorded_exit_does_not_bring_the_session_back() {
    let fake = FakeHost::start(&[("term-live", None)]);
    let app = fixture(fake.config());
    app.sessions.connect_host().unwrap();
    assert!(app.host.knows("term-live"));

    fake.end("term-live");
    let proxies = || app.sessions.host.as_ref().unwrap().known_sessions();
    until("the exit recorded", Duration::from_secs(10), || {
        proxies() == 0
    });
    assert!(!app.host.knows("term-live"));

    // A second connection whose list still carries the session.
    fake.drop_connections();
    until("the second connection", Duration::from_secs(10), || {
        fake.accepts() >= 2
    });
    until(
        "the second adoption read its receipts",
        Duration::from_secs(10),
        || fake.receipts() >= 2,
    );
    std::thread::sleep(Duration::from_millis(300));

    assert!(
        !app.host.knows("term-live"),
        "a session whose exit was recorded was proxied again from an older list"
    );
}

/// `knows` takes the `proxies` lock, and `adopt` holds it across
/// `adopt_observer` (client.rs:985-1005), which is the app's adopter: a store
/// read for every listed session that belongs to a run. On a reconnect
/// `adopted` is already true, so `knows` does not return early; it waits for
/// the adopter. The change's own doc says it "never waits".
///
/// Predicted: BREAK. `knows` takes about as long as the adopter (3 s here),
/// on a bridge worker holding an intake slot.
#[test]
fn knows_does_not_wait_on_an_adopt_that_is_reading_the_store() {
    let fake = FakeHost::start(&[("term-live", None)]);
    let app = fixture(fake.config());
    let entered = Arc::new(AtomicBool::new(false));
    let seen = entered.clone();
    app.sessions.set_adopter(Arc::new(
        move |_run: &str| -> Option<Arc<dyn SessionObserver>> {
            seen.store(true, Ordering::Release);
            std::thread::sleep(Duration::from_secs(3));
            None
        },
    ));
    app.sessions.connect_host().unwrap();
    assert!(app.host.knows("term-live"));
    assert!(
        !entered.load(Ordering::Acquire),
        "the first adoption had no run to adopt"
    );

    // The host comes back with a run's session among its sessions.
    fake.set_sessions(&[("term-live", None), ("term-run", Some("run-1"))]);
    fake.drop_connections();
    until("the adopter running", Duration::from_secs(10), || {
        entered.load(Ordering::Acquire)
    });

    let (known, took) = timed(&app.host, "term-live");
    assert!(known);
    assert!(
        took < PROMPT,
        "the check waited {took:?} on an adopt that holds the proxies lock"
    );
}

/// Before the first list finishes, `knows` is refused without touching the
/// lock or the host: the connect is mid-flight (the list is slow), and the
/// check returns at once, then agrees once the adoption is done.
///
/// Predicted: pass.
#[test]
fn knows_during_a_slow_first_adoption_refuses_without_waiting() {
    let fake = FakeHost::start(&[("term-live", None)]);
    fake.shared.list_delay_ms.store(1500, Ordering::Release);
    let app = fixture(fake.config());
    let sessions = app.sessions.clone();
    let connecting = std::thread::spawn(move || sessions.connect_host());
    until("the list requested", Duration::from_secs(5), || {
        fake.shared.lists.load(Ordering::Acquire) >= 1
    });

    let (known, took) = timed(&app.host, "term-live");
    assert!(!known, "a list not yet read cannot vouch");
    assert!(
        took < Duration::from_millis(500),
        "the check waited {took:?} on the connect"
    );

    connecting.join().unwrap().unwrap();
    assert!(app.host.knows("term-live"));
}

/// A spawn the host never answers. The proxy goes in before the request
/// (client.rs:400) and comes out only when the reply or the deadline ends the
/// call: `SPAWN_DEADLINE` is 30 s (client.rs:40). The brief calls this window
/// "open for milliseconds".
///
/// First half, predicted: pass. The starting session is known (window (a)).
/// Second half, predicted: BREAK. Two seconds on, with the host still silent,
/// the session is still vouched for.
#[test]
fn a_spawn_the_host_never_answers_is_vouched_for_only_briefly() {
    let fake = FakeHost::start(&[]);
    let app = fixture(fake.config());
    app.sessions.connect_host().unwrap();
    fake.shared.hang_spawn.store(true, Ordering::Release);

    let host = app.sessions.host.clone().unwrap();
    let spawner = std::thread::spawn(move || {
        host.spawn(
            Arc::new(NullEvents),
            spawn_request("term-7-a"),
            nowhere(),
            None,
        )
        .map(|_| ())
    });
    until("the starting proxy", Duration::from_secs(5), || {
        app.host.knows("term-7-a")
    });
    std::thread::sleep(Duration::from_millis(2200));
    let still = app.host.knows("term-7-a");

    fake.drop_connections();
    assert!(spawner.join().unwrap().is_err());
    assert!(
        !still,
        "a session the host has not confirmed in 2 s is still vouched for"
    );
}

/// The host reports the exit of a session whose spawn reply has not arrived.
/// `record_exit` parks it in `deferred_exit` and returns (client.rs:1099-1102),
/// leaving the proxy `starting` and in the map, so the session the host has
/// said is over is still known until the reply or the 30 s deadline.
///
/// Predicted: BREAK (low severity: the ordinary case is a few milliseconds).
#[test]
fn an_exit_that_overtakes_the_spawn_reply_is_not_vouched_for() {
    let fake = FakeHost::start(&[]);
    let app = fixture(fake.config());
    app.sessions.connect_host().unwrap();
    fake.shared.hang_spawn.store(true, Ordering::Release);

    let host = app.sessions.host.clone().unwrap();
    let spawner = std::thread::spawn(move || {
        host.spawn(
            Arc::new(NullEvents),
            spawn_request("term-7-b"),
            nowhere(),
            None,
        )
        .map(|_| ())
    });
    until("the starting proxy", Duration::from_secs(5), || {
        app.host.knows("term-7-b")
    });

    fake.end("term-7-b");
    std::thread::sleep(Duration::from_millis(500));
    let still = app.host.knows("term-7-b");

    fake.drop_connections();
    assert!(spawner.join().unwrap().is_err());
    assert!(
        !still,
        "a session the host reported ended is still vouched for"
    );
}

/// Keys that are not a session this registry holds, however they are dressed:
/// empty, blank, a prefix of a real id, a longer id, other case, a NUL, a
/// Unicode lookalike, a zero-width character, path and pattern characters, and
/// a megabyte. Each is a map lookup on the exact string.
///
/// Predicted: pass.
#[test]
fn keys_that_are_not_a_held_session_are_never_known() {
    let fake = FakeHost::start(&[("term-live", None)]);
    let app = fixture(fake.config());
    app.sessions.connect_host().unwrap();
    assert!(app.host.knows("term-live"), "the control");

    let long = "t".repeat(1 << 20);
    let repeated = "term-live".repeat(1000);
    for key in [
        "",
        " ",
        "term-live ",
        " term-live",
        "term-live\n",
        "TERM-LIVE",
        "Term-Live",
        "term-liv",
        "term-",
        "term",
        "term-live-",
        "term-live\0",
        "term-live\0x",
        "\0",
        "term-\u{FF4C}ive",
        "term\u{2010}live",
        "term-live\u{200B}",
        "../term-live",
        "term-live/..",
        "hook-term-live",
        "term-%",
        "term-_ive",
        "*",
        long.as_str(),
        repeated.as_str(),
    ] {
        let (known, took) = timed(&app.host, key);
        assert!(
            !known,
            "{:?} (len {}) was vouched for",
            key.chars().take(24).collect::<String>(),
            key.len()
        );
        assert!(took < PROMPT, "a key of {} bytes took {took:?}", key.len());
    }
}

/// A registry the app never managed: nothing to ask, so no.
///
/// Predicted: pass.
#[test]
fn a_registry_the_app_does_not_manage_refuses() {
    let app = tauri::test::mock_builder().build(crate::context()).unwrap();
    let host = crate::workbench::notifications::session_host(app.handle());
    assert!(!host.knows("term-live"));
    assert!(!host.knows(""));
}

/// The check is not a way to start a host. With no host listening and a binary
/// that would not start, a report's check must leave nothing behind: no
/// `ptyd.log`, which `start_host` creates before it spawns (client.rs:1307-1317),
/// and no recorded trouble. On the base this check calls `list`, which calls
/// `ensure_connected`, which calls `start_host`.
///
/// Predicted: pass (and it fails on `fa89a7e6`).
#[test]
fn the_check_does_not_start_a_host_or_leave_trouble_behind() {
    let dir = tempfile::Builder::new()
        .prefix("pn")
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap();
    let app = fixture(HostConfig {
        dir: dir.path().to_path_buf(),
        binary: Some("/nonexistent/gitpulse-ptyd".into()),
        idle_exit: None,
        log_dir: None,
    });

    let (known, took) = timed(&app.host, "term-live");
    assert!(!known);
    assert!(took < PROMPT, "the check took {took:?}");
    assert!(
        app.sessions.host_trouble().is_empty(),
        "the check tried to reach a host: {:?}",
        app.sessions.host_trouble()
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "the check created files in the host directory"
    );
}

/// 100 threads ask while the host is dropped and re-adopted over and over and
/// a session ends. No deadlock (every thread finishes), no call waits long, an
/// unknown key is never known, and once the session's exit is recorded and the
/// last reconnect has settled, no thread that begins afterwards is told it is
/// known.
///
/// Predicted: pass. `knows` takes one lock and nothing else; no path takes
/// `proxies` and then waits on something `knows` holds.
#[test]
fn a_hundred_threads_ask_while_the_host_churns_and_a_session_ends() {
    let fake = FakeHost::start(&[("term-live", None), ("term-b", None)]);
    let app = fixture(fake.config());
    app.sessions.connect_host().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let ended = Arc::new(AtomicBool::new(false));
    let worst = Arc::new(AtomicU64::new(0));
    let violations = Arc::new(AtomicUsize::new(0));
    let (finished, done) = mpsc::channel();
    for thread in 0..100usize {
        let (host, stop, ended, worst, violations, finished) = (
            app.host.clone(),
            stop.clone(),
            ended.clone(),
            worst.clone(),
            violations.clone(),
            finished.clone(),
        );
        std::thread::spawn(move || {
            let keys = ["term-live", "term-b", "term-404", "", "hook-x"];
            let mut next = thread;
            while !stop.load(Ordering::Acquire) {
                let key = keys[next % keys.len()];
                next += 1;
                let gone_before = ended.load(Ordering::Acquire);
                let started = Instant::now();
                let known = host.knows(key);
                worst.fetch_max(started.elapsed().as_micros() as u64, Ordering::AcqRel);
                let wrong = match key {
                    "term-404" | "" | "hook-x" => known,
                    "term-live" => gone_before && known,
                    _ => false,
                };
                if wrong {
                    violations.fetch_add(1, Ordering::AcqRel);
                }
                std::thread::yield_now();
            }
            let _ = finished.send(());
        });
    }

    for _ in 0..15 {
        fake.drop_connections();
        std::thread::sleep(Duration::from_millis(40));
    }
    // Let the last reconnect settle, then end the session for good.
    std::thread::sleep(Duration::from_millis(500));
    fake.forget("term-live");
    fake.end("term-live");
    let proxies = || app.sessions.host.as_ref().unwrap().known_sessions();
    until("the exit recorded", Duration::from_secs(10), || {
        proxies() == 1
    });
    ended.store(true, Ordering::Release);
    for _ in 0..5 {
        fake.drop_connections();
        std::thread::sleep(Duration::from_millis(40));
    }
    std::thread::sleep(Duration::from_millis(500));
    stop.store(true, Ordering::Release);

    for _ in 0..100 {
        done.recv_timeout(Duration::from_secs(30))
            .expect("a thread asking `knows` never finished: deadlock");
    }
    assert_eq!(violations.load(Ordering::Acquire), 0, "an answer was wrong");
    let worst = Duration::from_micros(worst.load(Ordering::Acquire));
    assert!(worst < PROMPT, "the slowest check took {worst:?}");
}

/// The host's output for a session floods the delivery thread, which takes the
/// `proxies` lock once per frame (client.rs:1169). The check must not be
/// starved behind it.
///
/// Predicted: pass.
#[test]
fn an_output_flood_does_not_stall_the_check() {
    let fake = FakeHost::start(&[("term-live", None)]);
    let app = fixture(fake.config());
    app.sessions.connect_host().unwrap();

    let shared = fake.shared.clone();
    let flooding = std::thread::spawn(move || flood(&shared, "term-live", 20_000));
    let askers: Vec<_> = (0..4)
        .map(|_| {
            let host = app.host.clone();
            std::thread::spawn(move || {
                let mut worst = Duration::ZERO;
                for _ in 0..2000 {
                    let (known, took) = timed(&host, "term-live");
                    assert!(known);
                    worst = worst.max(took);
                }
                worst
            })
        })
        .collect();
    flooding.join().unwrap();
    for asker in askers {
        let worst = asker.join().unwrap();
        assert!(worst < PROMPT, "a check waited {worst:?} behind the flood");
    }
}

// ---------------------------------------------------------------------------
// Round 2: the `Inner.vouched` design (16e46225). `knows` reads `vouched`
// alone; `proxies` is kept for everything else, so the two can disagree.
// ---------------------------------------------------------------------------

/// A host that is not listing at all, every time: the retry attempts fail too.
const ALWAYS: usize = usize::MAX / 2;

fn the_client(app: &Fixture) -> HostClient {
    app.sessions.host.clone().unwrap()
}

/// A spawn the host answers while the adoption's list is still on its way, as
/// it can on a connection that is already usable before it has listed: the
/// list was taken before the spawn, so it does not name the session. `adopt`
/// publishes that list over `held` (client.rs `publish_listing` assigns, it
/// does not merge), which drops the confirmed spawn; and its `gone` pass then
/// ends the proxy of a session the host holds, because it is not in the list.
///
/// Predicted: BREAK. The session is alive and was confirmed by the host, and
/// is never vouched for.
#[test]
fn a_spawn_confirmed_while_the_adoption_is_listing_is_still_vouched_for() {
    let fake = FakeHost::start(&[]);
    fake.shared.list_late_ms.store(1000, Ordering::Release);
    let app = fixture(fake.config());
    let sessions = app.sessions.clone();
    let connecting = std::thread::spawn(move || sessions.connect_host());
    until("the list requested", Duration::from_secs(5), || {
        fake.shared.lists.load(Ordering::Acquire) >= 1
    });
    fake.shared.spawn_ok.store(true, Ordering::Release);

    let host = the_client(&app);
    let spawned = host
        .spawn(
            Arc::new(NullEvents),
            spawn_request("term-8-a"),
            nowhere(),
            None,
        )
        .map(|_| ());
    assert!(spawned.is_ok(), "the spawn was answered: {spawned:?}");
    connecting.join().unwrap().unwrap();
    std::thread::sleep(Duration::from_millis(300));

    assert!(
        app.host.knows("term-8-a"),
        "a live session the host confirmed during the adoption is not vouched for"
    );
}

/// Session events that keep the ids of the exits they were handed.
#[derive(Default)]
struct ExitLog {
    exits: Mutex<Vec<(String, Option<String>)>>,
}

impl SessionEvents for ExitLog {
    fn loss(&self, _: TerminalLossPayload) {}
    fn notice(&self, _: crate::alerts::Notice) {}
    fn scan_drops(&self, _: u64) {}
    fn exit(&self, ended: SessionEnded) {
        self.exits
            .lock()
            .unwrap()
            .push((ended.payload.id, ended.payload.error));
    }
}

impl ExitLog {
    fn ended(&self, id: &str) -> bool {
        self.exits
            .lock()
            .unwrap()
            .iter()
            .any(|(ended, _)| ended == id)
    }
}

/// Makes `adopt` run `during` between its scan of the proxies and its read of
/// the spawns confirmed meanwhile, on the adopting thread, holding no lock.
fn at_the_seam(host: &HostClient, during: impl FnOnce() + Send + 'static) {
    *lock(&host.inner.adopt_seam) = Some(Box::new(during));
}

/// A spawn the host confirms exactly between `adopt`'s two reads of what it
/// has confirmed: the proxy scan and the copy of `vouched.late`. Whichever
/// comes second is the one that can miss it, so the order is the whole of the
/// guard: copying `late` first lets the spawn finish (its proxy no longer
/// `starting`) after the copy and before the scan, and the `gone` pass ends a
/// live session with `HOST_ENDED`; scanning first, the scan either sees it
/// `starting` or sees it done, and a proxy seen done was put in `late` first.
///
/// Reliable by construction: the spawn runs on the adopting thread at the seam.
#[test]
fn a_spawn_confirmed_between_the_two_reads_of_an_adoption_is_not_ended() {
    let fake = FakeHost::start(&[]);
    fake.shared.spawn_ok.store(true, Ordering::Release);
    let app = fixture(fake.config());
    let host = the_client(&app);
    let log = Arc::new(ExitLog::default());
    host.set_events(log.clone());
    let spawning = host.clone();
    let spawned_events = log.clone();
    at_the_seam(&host, move || {
        spawning
            .spawn(
                spawned_events,
                spawn_request("term-9-race"),
                nowhere(),
                None,
            )
            .unwrap();
    });

    app.sessions.connect_host().unwrap();

    assert!(
        lock(&host.inner.proxies).contains_key("term-9-race"),
        "the spawn the host confirmed during the adoption lost its proxy"
    );
    assert!(
        !log.ended("term-9-race"),
        "the adoption ended a live session the host had just confirmed: {:?}",
        log.exits.lock().unwrap()
    );
    assert!(app.host.knows("term-9-race"));
}

/// The same seam, on a reconnect that has a genuinely vanished session to end:
/// the spawn confirmed at the seam survives and the session the host no longer
/// holds is still ended as `HOST_ENDED`. The fix narrows the `gone` pass; it
/// does not switch it off.
#[test]
fn an_adoption_with_a_spawn_at_the_seam_still_ends_the_session_the_host_lost() {
    let fake = FakeHost::start(&[("term-9-old", None)]);
    fake.shared.spawn_ok.store(true, Ordering::Release);
    let app = fixture(fake.config());
    let host = the_client(&app);
    let log = Arc::new(ExitLog::default());
    host.set_events(log.clone());
    app.sessions.connect_host().unwrap();
    assert!(app.host.knows("term-9-old"), "the first adoption listed it");

    let spawning = host.clone();
    let spawned_events = log.clone();
    at_the_seam(&host, move || {
        spawning
            .spawn(spawned_events, spawn_request("term-9-new"), nowhere(), None)
            .unwrap();
    });
    fake.forget("term-9-old");
    fake.drop_connections();
    until("the lost session ended", Duration::from_secs(10), || {
        log.ended("term-9-old")
    });

    let exits = log.exits.lock().unwrap().clone();
    assert_eq!(
        exits
            .iter()
            .find(|(id, _)| id == "term-9-old")
            .and_then(|(_, error)| error.as_deref()),
        Some(crate::terminal::host::receipts::HOST_ENDED),
        "the session the host lost is ended as the host's loss"
    );
    assert!(
        !log.ended("term-9-new"),
        "the spawn confirmed at the seam was ended with it: {exits:?}"
    );
    assert!(app.host.knows("term-9-new"));
}

/// The app quits while a connect is in flight (the hello is slow). `disconnect`
/// finds no connection to close and revokes; the connect then stores its
/// connection (client.rs `connect` does not look at `leaving` again), lists,
/// and `publish_listing` sets `live`. From then on the quit's connection is
/// open and vouched for.
///
/// Predicted: BREAK. The same race leaves a connection open after the quit.
#[test]
fn a_quit_during_a_connect_is_not_undone_by_the_connect_finishing() {
    let fake = FakeHost::start(&[("term-live", None)]);
    fake.shared.hello_delay_ms.store(800, Ordering::Release);
    let app = fixture(fake.config());
    let sessions = app.sessions.clone();
    let connecting = std::thread::spawn(move || sessions.connect_host());
    until("the connection accepted", Duration::from_secs(5), || {
        fake.accepts() >= 1
    });
    std::thread::sleep(Duration::from_millis(100));

    app.sessions.disconnect_host();
    let _ = connecting.join().unwrap();
    std::thread::sleep(Duration::from_millis(200));

    assert!(
        !app.host.knows("term-live"),
        "the app let the host go, and a connect that was already under way vouched for it again"
    );
}

/// A host whose listing is never readable: the first attempt and three retries
/// (about 1, 3 and 7 s after the first), and then no more, however long it
/// waits. A fresh `connect_host` after it starts working again recovers.
///
/// Predicted: pass.
#[test]
fn a_failing_listing_is_retried_three_times_and_then_left_alone() {
    let fake = FakeHost::start(&[("term-live", None)]);
    fake.shared.garbage_lists.store(ALWAYS, Ordering::Release);
    let app = fixture(fake.config());
    assert!(app.sessions.connect_host().is_err());
    assert_eq!(fake.accepts(), 1);

    std::thread::sleep(Duration::from_secs(9));
    assert_eq!(
        fake.accepts(),
        4,
        "the first attempt and three retries, by 9 s"
    );
    assert!(!app.host.knows("term-live"));
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(fake.accepts(), 4, "the retries went on");
    assert!(
        !the_client(&app).inner.retrying.load(Ordering::Acquire),
        "the retry loop finished but still claims to be running, so no later failure is retried"
    );

    fake.shared.garbage_lists.store(0, Ordering::Release);
    app.sessions.connect_host().unwrap();
    assert!(app.host.knows("term-live"));
}

/// A caller reconnects while a retry is waiting, and its listing fails as well.
/// There is one retry loop, not two: the caller's failure does not start
/// another, and the one that runs finds the host working.
///
/// Predicted: pass.
#[test]
fn a_reconnect_by_a_caller_during_a_retry_does_not_double_the_retries() {
    let fake = FakeHost::start(&[("term-live", None)]);
    fake.shared.garbage_lists.store(2, Ordering::Release);
    let app = fixture(fake.config());
    assert!(app.sessions.connect_host().is_err());
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        app.sessions.connect_host().is_err(),
        "the second list is unreadable too"
    );

    until("the retry adopted", Duration::from_secs(5), || {
        app.host.knows("term-live")
    });
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(
        fake.accepts(),
        3,
        "the first attempt, the caller's, and one retry"
    );
    assert!(!the_client(&app).inner.retrying.load(Ordering::Acquire));
}

/// The app quits while an adoption retry is waiting: the retry ends without
/// connecting, and nothing is vouched for.
///
/// Predicted: pass.
#[test]
fn a_quit_during_a_retry_stops_it() {
    let fake = FakeHost::start(&[("term-live", None)]);
    fake.shared.garbage_lists.store(ALWAYS, Ordering::Release);
    let app = fixture(fake.config());
    assert!(app.sessions.connect_host().is_err());
    std::thread::sleep(Duration::from_millis(300));

    app.sessions.disconnect_host();
    std::thread::sleep(Duration::from_secs(2));

    assert_eq!(fake.accepts(), 1, "a retry connected after the quit");
    assert!(!app.host.knows("term-live"));
    assert!(
        !the_client(&app).inner.retrying.load(Ordering::Acquire),
        "the retry never ended"
    );
}

/// A spawn the host refuses, after a delay shorter than the grace. While it is
/// pending the session is vouched for (the grace); the moment the refusal is
/// read it is not, and nothing keeps it.
///
/// Predicted: pass.
#[test]
fn a_spawn_the_host_refuses_is_not_vouched_for_once_it_is_refused() {
    let fake = FakeHost::start(&[]);
    let app = fixture(fake.config());
    app.sessions.connect_host().unwrap();
    fake.shared.spawn_reject_ms.store(700, Ordering::Release);

    let host = the_client(&app);
    let spawner = std::thread::spawn(move || {
        host.spawn(
            Arc::new(NullEvents),
            spawn_request("term-9-a"),
            nowhere(),
            None,
        )
        .map(|_| ())
    });
    until("the pending spawn", Duration::from_secs(5), || {
        app.host.knows("term-9-a")
    });
    assert!(spawner.join().unwrap().is_err(), "the host refused");

    assert!(
        !app.host.knows("term-9-a"),
        "a session the host refused is still vouched for"
    );
    std::thread::sleep(Duration::from_millis(1200));
    assert!(!app.host.knows("term-9-a"));
}

/// Thousands of exits, none of them a session this app ever held, and 50 live
/// sessions the host keeps: the record of ended sessions and the vouched set
/// stay bounded, and the live sessions are not lost to them.
///
/// Predicted: pass.
#[test]
fn thousands_of_exits_leave_the_records_bounded() {
    let live: Vec<String> = (0..50).map(|n| format!("term-1-{n:x}")).collect();
    let listed: Vec<(&str, Option<&str>)> = live.iter().map(|id| (id.as_str(), None)).collect();
    let fake = FakeHost::start(&listed);
    let app = fixture(fake.config());
    app.sessions.connect_host().unwrap();
    let client = the_client(&app);

    for n in 0..2500 {
        fake.end(&format!("term-2-{n:x}"));
    }
    until("the exits recorded", Duration::from_secs(60), || {
        lock(&client.inner.ended).len() >= REMEMBERED_EXITS
    });
    std::thread::sleep(Duration::from_secs(1));

    assert!(lock(&client.inner.ended).len() <= REMEMBERED_EXITS);
    assert!(lock(&client.inner.vouched).exited.len() <= REMEMBERED_EXITS);
    assert_eq!(lock(&client.inner.vouched).held.len(), 50);
    assert_eq!(client.known_sessions(), 50);
    assert!(app.host.knows(&live[0]));
}

/// "Ended ids are never resurrected" holds only as long as the id is
/// remembered: `exited` keeps the newest 1024. A session whose exit has since
/// been pushed out by 1100 others is brought back by a listing that still
/// names it.
///
/// Predicted: BREAK, low severity: the bound is the price of bounded memory,
/// and the listing must be older than 1100 exits to do it. It documents that
/// the guarantee is a window, not "never".
#[test]
fn an_exit_pushed_out_of_the_record_is_not_resurrected_by_an_old_listing() {
    let fake = FakeHost::start(&[("term-live", None)]);
    let app = fixture(fake.config());
    app.sessions.connect_host().unwrap();
    let client = the_client(&app);

    fake.end("term-live");
    until("the exit recorded", Duration::from_secs(10), || {
        !app.host.knows("term-live")
    });
    for n in 0..1100 {
        fake.end(&format!("term-3-{n:x}"));
    }
    until("the later exits recorded", Duration::from_secs(60), || {
        lock(&client.inner.ended).len() >= REMEMBERED_EXITS
    });
    std::thread::sleep(Duration::from_secs(1));

    // The host still lists it: a listing older than all of that.
    fake.drop_connections();
    until("the second connection", Duration::from_secs(10), || {
        fake.accepts() >= 2
    });
    until("the second adoption", Duration::from_secs(10), || {
        fake.receipts() >= 2
    });
    std::thread::sleep(Duration::from_millis(300));

    assert!(
        !app.host.knows("term-live"),
        "an ended session came back once its exit left the record"
    );
}

/// The exit arrives while the adoption's list, which still names the session,
/// is on its way. Whichever the app handles first, the session ends up not
/// vouched for and stays so.
///
/// Predicted: pass.
#[test]
fn an_exit_racing_the_listing_never_leaves_the_session_vouched() {
    let fake = FakeHost::start(&[("term-live", None)]);
    fake.shared.list_late_ms.store(400, Ordering::Release);
    let app = fixture(fake.config());
    let sessions = app.sessions.clone();
    let connecting = std::thread::spawn(move || sessions.connect_host());
    until("the list requested", Duration::from_secs(5), || {
        fake.shared.lists.load(Ordering::Acquire) >= 1
    });

    fake.end("term-live");
    connecting.join().unwrap().unwrap();
    until(
        "the session not vouched for",
        Duration::from_secs(3),
        || !app.host.knows("term-live"),
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(!app.host.knows("term-live"));
}

/// The two sets `vouched` and `proxies` after a restart that is reconciled
/// completely: they name the same sessions.
///
/// Predicted: pass.
#[test]
fn vouched_and_proxies_name_the_same_sessions_after_a_clean_restart() {
    let fake = FakeHost::start(&[("term-a", None), ("term-b", None), ("term-c", None)]);
    let app = fixture(fake.config());
    app.sessions.connect_host().unwrap();
    let client = the_client(&app);

    fake.end("term-b");
    fake.forget("term-b");
    until("b recorded", Duration::from_secs(10), || {
        !app.host.knows("term-b")
    });
    fake.set_sessions(&[("term-a", None), ("term-c", None), ("term-d", None)]);
    fake.drop_connections();
    until("d adopted", Duration::from_secs(10), || {
        app.host.knows("term-d")
    });
    std::thread::sleep(Duration::from_millis(500));

    let mut proxies: Vec<String> = lock(&client.inner.proxies).keys().cloned().collect();
    proxies.sort();
    let mut vouched: Vec<String> = lock(&client.inner.vouched).held.iter().cloned().collect();
    vouched.sort();
    assert_eq!(vouched, ["term-a", "term-c", "term-d"]);
    assert_eq!(proxies, vouched);
}

/// The same restart with receipts that cannot be read. `knows` is right (the
/// listing alone feeds `vouched`), but `adopt` returned before it ended the
/// proxies the host no longer holds, so `proxies` keeps a session `vouched`
/// does not: `tracks_run` and `known_sessions` still count it.
///
/// Predicted: BREAK. The two sets diverge, and the stale one is the one that
/// decides whether a run is released.
#[test]
fn proxies_do_not_keep_a_session_vouched_does_not_hold() {
    let fake = FakeHost::start(&[("term-old", None)]);
    let app = fixture(fake.config());
    app.sessions.connect_host().unwrap();
    let client = the_client(&app);

    fake.set_sessions(&[("term-new", None)]);
    fake.shared.receipts_fail.store(true, Ordering::Release);
    fake.drop_connections();
    until("new adopted", Duration::from_secs(10), || {
        app.host.knows("term-new")
    });
    std::thread::sleep(Duration::from_millis(500));

    assert!(!app.host.knows("term-old"), "the answer `knows` gives");
    let mut proxies: Vec<String> = lock(&client.inner.proxies).keys().cloned().collect();
    proxies.sort();
    assert_eq!(
        proxies,
        ["term-new"],
        "a proxy for a session the host no longer holds stays, and counts for `tracks_run`"
    );
}

/// 100 threads ask while an adoption keeps failing and is retried, then
/// succeeds. No deadlock, no wrong answer while nothing
/// is vouched for, no call that waits, and the retry's success is seen.
///
/// Predicted: pass.
#[test]
fn a_hundred_threads_ask_through_failing_listings_and_their_retries() {
    let fake = FakeHost::start(&[("term-live", None), ("term-b", None)]);
    fake.shared.garbage_lists.store(3, Ordering::Release);
    let app = fixture(fake.config());

    let stop = Arc::new(AtomicBool::new(false));
    let worst = Arc::new(AtomicU64::new(0));
    let violations = Arc::new(AtomicUsize::new(0));
    let (finished, done) = mpsc::channel();
    for thread in 0..100usize {
        let (host, stop, worst, violations, finished) = (
            app.host.clone(),
            stop.clone(),
            worst.clone(),
            violations.clone(),
            finished.clone(),
        );
        std::thread::spawn(move || {
            let keys = ["term-live", "term-b", "term-404", "", "hook-x"];
            let mut next = thread;
            while !stop.load(Ordering::Acquire) {
                let key = keys[next % keys.len()];
                next += 1;
                let started = Instant::now();
                let known = host.knows(key);
                worst.fetch_max(started.elapsed().as_micros() as u64, Ordering::AcqRel);
                if known && matches!(key, "term-404" | "" | "hook-x") {
                    violations.fetch_add(1, Ordering::AcqRel);
                }
                std::thread::yield_now();
            }
            let _ = finished.send(());
        });
    }

    // Three unreadable lists: the first attempt and the retries at about 1 s
    // and 3 s; the retry at about 7 s reads the host.
    assert!(app.sessions.connect_host().is_err());
    until("the retry adopted", Duration::from_secs(15), || {
        app.host.knows("term-live")
    });
    std::thread::sleep(Duration::from_millis(500));
    stop.store(true, Ordering::Release);

    for _ in 0..100 {
        done.recv_timeout(Duration::from_secs(30))
            .expect("a thread asking `knows` never finished: deadlock");
    }
    assert_eq!(violations.load(Ordering::Acquire), 0, "an answer was wrong");
    let worst = Duration::from_micros(worst.load(Ordering::Acquire));
    assert!(worst < PROMPT, "the slowest check took {worst:?}");
}
