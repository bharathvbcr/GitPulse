//! ADVERSARY tests for lane W3a (ft-c113d00b), written against `ab931635`:
//! adopt reads `vouched.late` after its scan of `proxies`.
//!
//! Each case names what it attacks. A case that FAILS on the lane tip is a
//! break; a case that passes is kept only as coverage of a held attack. They
//! drive `adopt` through the lane's own `adopt_seam` and the private state of
//! `Inner`, as `client_adversarial_tests.rs` does.

use super::*;

fn exit_of(id: &str, exit_code: Option<i32>) -> WireEnded {
    WireEnded {
        payload: TerminalExitPayload {
            id: id.to_owned(),
            exit_code,
            signal: String::new(),
            error: None,
            reaped: true,
            ended_by_gitpulse: false,
        },
        ended_by_gitpulse: false,
        program: "sh".into(),
        cwd: "/".into(),
        run_id: None,
    }
}

/// A run's bookkeeping whose `finished` is slow: the first call waits for the
/// gate (bounded), every later call is counted and returns at once.
struct SlowFinish {
    started: AtomicUsize,
    open: AtomicBool,
}

impl SlowFinish {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            started: AtomicUsize::new(0),
            open: AtomicBool::new(false),
        })
    }
}

impl SessionObserver for SlowFinish {
    fn run_id(&self) -> &str {
        "run-w3a"
    }
    fn before_spawn(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn started(&self, _: &str, _: Result<(u32, String), String>) -> Result<(), String> {
        Ok(())
    }
    fn spawn_failed(&self, _: &str, _: &str) {}
    fn finished(&self, _: &TerminalExitPayload) {
        if self.started.fetch_add(1, Ordering::AcqRel) == 0 {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !self.open.load(Ordering::Acquire) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

/// ATTACK 1 (a path where `proxy.starting` clears without `late` written while
/// adopting). `spawn` does not write `late` for a session whose exit was
/// already reported (`vouched.exited`), yet it still clears `starting`, and
/// only afterwards records the deferred exit it took in that same `proxies`
/// critical section. An adoption that scans in that gap sees a proxy that is
/// not `starting`, not listed, and not in `late`: the claim "not starting in
/// the scan implies in `late`" is false there. The session did end, but the
/// adoption announces it as the host's loss and the real exit, recorded next,
/// is dropped as already recorded.
///
/// The gap is `client.rs` between releasing the `proxies` lock after
/// `proxy.starting = false` and `record_exit(deferred)`; there is no hook in
/// it, so the state it leaves is built from the same private calls: a proxy
/// that is not starting, `note_ended` (what the exit report did), and the
/// exit held in hand, recorded after the adoption as `spawn` would.
#[test]
fn adv_w3a_a_deferred_exit_still_in_hand_is_not_pre_empted_by_the_adoption() {
    let fake = FakeHost::start(&[]);
    let app = fixture(fake.config());
    let host = the_client(&app);
    let log = Arc::new(ExitLog::default());
    host.set_events(log.clone());
    lock(&host.inner.proxies).insert(
        "term-9-gap".into(),
        Proxy::new(nowhere(), None, None, false, false),
    );
    host.inner.note_ended("term-9-gap");
    let in_hand = exit_of("term-9-gap", Some(0));

    app.sessions.connect_host().unwrap();
    host.inner.record_exit(in_hand);

    let exits = log.exits.lock().unwrap().clone();
    let first = exits.iter().find(|(id, _)| id == "term-9-gap");
    assert_eq!(
        first,
        Some(&("term-9-gap".to_owned(), None)),
        "the first announcement of the exit must be the exit the host reported, not the host's loss: {exits:?}"
    );
}

/// ATTACK 5 (a new false negative or a regression from the reorder). The old
/// order copied `late` before the scan, so a session confirmed during the
/// adoption stayed exempt from the `gone` pass even when its exit was reported
/// afterwards. The new order reads `late` last, so an exit reported between
/// the scan and the read (`note_ended` removes the id from `late`) makes the
/// proxy a `gone` candidate while the real exit's recording is still under way
/// (the proxy is removed only after `observer.finished`). `record_exit`'s
/// `already` check is not atomic with the push, so the adoption's synthetic
/// `HOST_ENDED` exit and the real one are both recorded: the run's bookkeeping
/// runs twice and two `terminal-exit` events go out for one session.
///
/// Deterministic: the seam starts the real exit on its own thread and waits
/// until its bookkeeping is under way, then lets the adoption read `late`.
#[test]
fn adv_w3a_an_exit_being_recorded_when_adopt_reads_late_is_not_recorded_twice() {
    let fake = FakeHost::start(&[]);
    fake.shared.list_late_ms.store(1000, Ordering::Release);
    let app = fixture(fake.config());
    let host = the_client(&app);
    let log = Arc::new(ExitLog::default());
    host.set_events(log.clone());
    let observer = SlowFinish::new();
    let recording = Arc::new(Mutex::new(None));
    {
        let inner = host.inner.clone();
        let observer = observer.clone();
        let recording = recording.clone();
        at_the_seam(&host, move || {
            let ending = inner.clone();
            *lock(&recording) = Some(std::thread::spawn(move || {
                ending.record_exit(exit_of("term-9-slow", Some(0)));
            }));
            until(
                "the real exit's bookkeeping",
                Duration::from_secs(5),
                || observer.started.load(Ordering::Acquire) >= 1,
            );
        });
    }
    let sessions = app.sessions.clone();
    let connecting = std::thread::spawn(move || sessions.connect_host());
    until("the list requested", Duration::from_secs(5), || {
        fake.shared.lists.load(Ordering::Acquire) >= 1
    });
    fake.shared.spawn_ok.store(true, Ordering::Release);
    host.spawn(
        log.clone(),
        spawn_request("term-9-slow"),
        nowhere(),
        Some(observer.clone()),
    )
    .unwrap();
    connecting.join().unwrap().unwrap();
    observer.open.store(true, Ordering::Release);
    if let Some(recording) = lock(&recording).take() {
        recording.join().unwrap();
    }

    let exits = log.exits.lock().unwrap().clone();
    assert_eq!(
        observer.started.load(Ordering::Acquire),
        1,
        "the run's bookkeeping ran more than once for one session: {exits:?}"
    );
    assert_eq!(
        exits.iter().filter(|(id, _)| id == "term-9-slow").count(),
        1,
        "one session, one exit announcement: {exits:?}"
    );
}

/// ATTACK 5, the other half (coverage; held). A session spawned during an
/// adoption sits in `late` for that adoption only. When the host then loses it
/// without an exit report, the next adoption (a new window, `late` cleared)
/// ends it as the host's loss: the exemption never outlives its window.
#[test]
fn adv_w3a_a_late_spawn_the_host_then_loses_is_ended_by_the_next_adoption() {
    let fake = FakeHost::start(&[]);
    fake.shared.list_late_ms.store(1000, Ordering::Release);
    let app = fixture(fake.config());
    let host = the_client(&app);
    let log = Arc::new(ExitLog::default());
    host.set_events(log.clone());
    let sessions = app.sessions.clone();
    let connecting = std::thread::spawn(move || sessions.connect_host());
    until("the list requested", Duration::from_secs(5), || {
        fake.shared.lists.load(Ordering::Acquire) >= 1
    });
    fake.shared.spawn_ok.store(true, Ordering::Release);
    host.spawn(log.clone(), spawn_request("term-9-keep"), nowhere(), None)
        .unwrap();
    connecting.join().unwrap().unwrap();
    assert!(app.host.knows("term-9-keep"));
    assert!(!log.ended("term-9-keep"), "ended by the adoption it raced");

    fake.shared.list_late_ms.store(0, Ordering::Release);
    fake.forget("term-9-keep");
    fake.drop_connections();
    until("the lost session ended", Duration::from_secs(10), || {
        log.ended("term-9-keep")
    });
    let exits = log.exits.lock().unwrap().clone();
    assert_eq!(
        exits
            .iter()
            .find(|(id, _)| id == "term-9-keep")
            .and_then(|(_, error)| error.as_deref()),
        Some(crate::terminal::host::receipts::HOST_ENDED),
        "{exits:?}"
    );
}
