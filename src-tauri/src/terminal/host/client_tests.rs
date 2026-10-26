//! `Host::knows` against a terminal host that is hung, not yet adopted, or
//! has reported an exit. The host here is a fake on a real socket: the real
//! `server::serve` answers too promptly to be hung on demand.
//!
//! `knows` runs on a bridge worker that holds an intake slot, so it must never
//! wait on the host: a hung host would fill every slot and refuse every report.

use super::*;
use crate::terminal::host::proto::{decode, read_frame, write_frame};
use crate::terminal::{TerminalExitPayload, TerminalSessions};
use std::os::unix::net::UnixListener;
use std::sync::atomic::AtomicBool;
use tauri::Manager as _;

/// What a caller on a bridge worker can afford. The old path took about 10 s.
const PROMPT: Duration = Duration::from_secs(2);

type Writer = Arc<Mutex<UnixStream>>;

/// A terminal host that holds one live session, `term-live`, and can stop
/// answering on demand.
struct FakeHost {
    dir: tempfile::TempDir,
    /// Connections accepted.
    accepts: Arc<AtomicUsize>,
    /// Requests received, other than the fire-and-forget `Limit`, `Stall` and
    /// `Forget` that the app sends without waiting for an answer.
    calls: Arc<AtomicUsize>,
    hung: Arc<AtomicBool>,
    /// How many of the next lists are answered with something that is not one.
    garbage_lists: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    writers: Arc<Mutex<Vec<Writer>>>,
}

impl FakeHost {
    fn start() -> Self {
        // A socket path has a short limit: the system temp directory, not a
        // long scratch one.
        let dir = tempfile::Builder::new()
            .prefix("ph")
            .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
        let listener = UnixListener::bind(
            dir.path()
                .join(crate::terminal::host::socket_name(PROTOCOL)),
        )
        .unwrap();
        listener.set_nonblocking(true).unwrap();
        let fake = Self {
            dir,
            accepts: Arc::default(),
            calls: Arc::default(),
            hung: Arc::default(),
            garbage_lists: Arc::default(),
            stop: Arc::default(),
            writers: Arc::default(),
        };
        let (accepts, calls, hung, garbage, stop, writers) = (
            fake.accepts.clone(),
            fake.calls.clone(),
            fake.hung.clone(),
            fake.garbage_lists.clone(),
            fake.stop.clone(),
            fake.writers.clone(),
        );
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let Ok((stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                accepts.fetch_add(1, Ordering::AcqRel);
                let writer: Writer = Arc::new(Mutex::new(stream.try_clone().unwrap()));
                writers.lock().unwrap().push(writer.clone());
                let (calls, hung, garbage) = (calls.clone(), hung.clone(), garbage.clone());
                std::thread::spawn(move || serve(stream, writer, &calls, &hung, &garbage));
            }
        });
        fake
    }

    fn config(&self) -> HostConfig {
        HostConfig {
            dir: self.dir.path().to_path_buf(),
            binary: Some("/nonexistent/gitpulse-ptyd".into()),
            idle_exit: None,
            log_dir: None,
        }
    }

    /// From here on every request is read, counted and left unanswered.
    fn hang(&self) {
        self.hung.store(true, Ordering::Release);
    }

    fn accepts(&self) -> usize {
        self.accepts.load(Ordering::Acquire)
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }

    /// The host reports that `session` ended.
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
        for writer in self.writers.lock().unwrap().iter() {
            write_frame(&mut *writer.lock().unwrap(), &frame, &[]).unwrap();
        }
    }
}

impl Drop for FakeHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for writer in self.writers.lock().unwrap().iter() {
            let _ = writer.lock().unwrap().shutdown(std::net::Shutdown::Both);
        }
    }
}

fn serve(
    stream: UnixStream,
    writer: Writer,
    calls: &AtomicUsize,
    hung: &AtomicBool,
    garbage: &AtomicUsize,
) {
    let mut input = BufReader::new(stream);
    while let Ok(Some((header, _))) = read_frame(&mut input) {
        let Ok(Envelope { id, request }) = decode::<Envelope>(&header) else {
            return;
        };
        let ok = match request {
            Request::Limit { .. } | Request::Stall { .. } | Request::Forget { .. } => continue,
            Request::Hello { .. } => {
                calls.fetch_add(1, Ordering::AcqRel);
                serde_json::to_value(Hello {
                    protocol: PROTOCOL,
                    build: crate::updates::CURRENT_VERSION.to_owned(),
                    pid: std::process::id(),
                    sessions: 1,
                })
            }
            Request::List => {
                calls.fetch_add(1, Ordering::AcqRel);
                if garbage
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                    .is_ok()
                {
                    serde_json::to_value("not a list of sessions")
                } else {
                    serde_json::to_value([TerminalListing {
                        id: "term-live".into(),
                        shell: "sh".into(),
                        cwd: "/".into(),
                        repo: "/".into(),
                        launcher: None,
                        run_id: None,
                        detached: false,
                        ending: false,
                    }])
                }
            }
            Request::Receipts => {
                calls.fetch_add(1, Ordering::AcqRel);
                serde_json::to_value(Vec::<WireEnded>::new())
            }
            _ => {
                calls.fetch_add(1, Ordering::AcqRel);
                Ok(serde_json::Value::Null)
            }
        }
        .unwrap();
        if hung.load(Ordering::Acquire) {
            continue;
        }
        let reply = HostFrame::Reply {
            id,
            ok: Some(ok),
            error: None,
        };
        if write_frame(&mut *writer.lock().unwrap(), &reply, &[]).is_err() {
            return;
        }
    }
}

/// The app's registry against `fake`, with `knows` asked as the bridge asks it.
struct Fixture {
    sessions: TerminalSessions,
    host: Arc<dyn crate::alerts::Host>,
    _app: tauri::App<tauri::test::MockRuntime>,
}

fn fixture(fake: &FakeHost) -> Fixture {
    let app = tauri::test::mock_builder().build(crate::context()).unwrap();
    let sessions = TerminalSessions::with_host(fake.config());
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

/// AC: a hung terminal host cannot hold the check. After adoption the answer
/// comes from what this app already holds, with no request to the host: the
/// live session is known, a stranger is not, and neither waits.
#[test]
fn a_hung_host_does_not_hold_the_session_check() {
    let fake = FakeHost::start();
    let app = fixture(&fake);
    app.sessions.connect_host().unwrap();
    fake.hang();
    let before = fake.calls();

    let (live, took) = timed(&app.host, "term-live");
    assert!(live, "a session the host listed is known");
    assert!(took < PROMPT, "the check waited {took:?} on a hung host");
    let (stranger, took) = timed(&app.host, "term-404");
    assert!(!stranger, "a session the host never listed is not known");
    assert!(took < PROMPT, "the check waited {took:?} on a hung host");
    assert_eq!(
        fake.calls(),
        before,
        "the check sent the host a request it would never answer"
    );
}

/// AC: a check never connects. Before the app has read the host's sessions it
/// cannot tell, which is refused, and it does not start or reach a host to
/// find out: that is for the app's own start-up, not for a hook report.
#[test]
fn the_check_before_adoption_refuses_and_does_not_connect() {
    let fake = FakeHost::start();
    let app = fixture(&fake);

    let (known, took) = timed(&app.host, "term-live");
    assert!(!known, "a registry that was never asked cannot vouch");
    assert!(took < PROMPT, "the check took {took:?}");
    assert_eq!(fake.accepts(), 0, "the check connected to the host");
}

/// A session whose exit was recorded is no longer known, and the check still
/// asks nothing of the host.
#[test]
fn a_session_that_exited_is_no_longer_known() {
    let fake = FakeHost::start();
    let app = fixture(&fake);
    app.sessions.connect_host().unwrap();
    assert!(app.host.knows("term-live"));
    let before = fake.calls();

    fake.end("term-live");
    let held = || app.sessions.host.as_ref().unwrap().known_sessions();
    let deadline = Instant::now() + Duration::from_secs(10);
    while held() > 0 {
        assert!(Instant::now() < deadline, "the exit was never recorded");
        std::thread::sleep(Duration::from_millis(10));
    }

    let (known, took) = timed(&app.host, "term-live");
    assert!(!known, "an ended session is not known");
    assert!(took < PROMPT, "the check took {took:?}");
    assert_eq!(fake.calls(), before);
}

/// A listing that fails is tried again without anyone asking: until it
/// succeeds the check refuses, and it must not stay refusing for the life of a
/// connection that nothing re-lists on.
#[test]
fn a_failed_first_listing_is_retried_in_the_background() {
    let fake = FakeHost::start();
    fake.garbage_lists.store(1, Ordering::Release);
    let app = fixture(&fake);
    assert!(app.sessions.connect_host().is_err());
    assert!(!app.host.knows("term-live"), "nothing has been listed");

    let deadline = Instant::now() + Duration::from_secs(10);
    while !app.host.knows("term-live") {
        assert!(Instant::now() < deadline, "the listing was never retried");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(fake.accepts(), 2, "one failed connection, one retry");
}

/// With the sessions in this process there is no host to ask, and no connection
/// to wait on: the registry answers from its own map.
#[test]
fn an_in_process_registry_knows_only_what_it_holds() {
    let app = tauri::test::mock_builder().build(crate::context()).unwrap();
    app.manage(TerminalSessions::default());
    let host = crate::workbench::notifications::session_host(app.handle());
    assert!(!host.knows("term-live"));
    assert!(!host.knows(""));
}
